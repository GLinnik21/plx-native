//! Subtitle-stream decode for one `AVFormatContext`, split out of `ff.rs` so a second demux
//! context (one that reads only a subtitle stream of the original Part) can share it with the
//! main demuxer instead of duplicating it.
//!
//! [`SubTracks`] owns the per-subtitle-stream decoder state: the stream list in FILE order (so a
//! track's 0-based position is the track menu's `desired_sub_idx`), each stream's [`SubKind`], and
//! the software decoder of every image-subtitle stream. [`SubTracks::decode`] turns one packet of
//! one track into at most one [`SubCue`]; [`SubCue::push`] hands it to the render store. Timing is
//! in the stream's own clock as `pts_ns` rebases it (stream time base -> nanoseconds, `pts` else
//! `dts`, 0 when neither); NO other rebasing happens here, so the caller owns any offset between
//! this context's clock and the playhead.

use super::*;
/// How a subtitle stream's payload turns into displayable text — classified by the codec's
/// name (avcodec_get_name is already linked, so we avoid hardcoding the n3.3 subtitle codec-id
/// block, which the ABI probe never verified). Bitmap subs (PGS/VobSub/DVB/teletext) carry no
/// text and can't be client-rendered, but still occupy their file-order slot so the track
/// menu's desired_sub_idx stays aligned with the metadata subs list.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum SubKind {
    Plain,   // SRT / subrip / text / webvtt: packet payload is UTF-8 text
    Ass,     // ASS / SSA: dialogue line; text is the field after the 8th comma
    MovText, // mp4 tx3g: 2-byte big-endian text-length prefix, then UTF-8 text
    Bitmap,  // PGS / VobSub / DVB / teletext: image subtitle, not renderable here
}

pub(super) unsafe fn sub_kind(codec_id: c_int) -> SubKind {
    let name = std::ffi::CStr::from_ptr(avcodec_get_name(codec_id)).to_string_lossy();
    match name.as_ref() {
        "ass" | "ssa" => SubKind::Ass,
        "mov_text" => SubKind::MovText,
        "subrip" | "srt" | "text" | "webvtt" | "vplayer" | "pjs" | "jacosub" | "microdvd"
        | "sami" | "realtext" | "subviewer" | "subviewer1" | "stl" | "mpl2" => SubKind::Plain,
        _ => SubKind::Bitmap,
    }
}

/// Open a software decoder for an image-subtitle stream (PGS/VobSub/DVB). Returns a
/// lib-allocated AVCodecContext (free with avcodec_free_context) or null if the build
/// lacks the decoder / open fails. parameters_to_context carries extradata — dvdsub needs
/// the palette from it, so this must run before open2.
pub(super) unsafe fn open_sub_decoder(cp: *const AVCodecParameters) -> *mut AVCodecContext {
    let codec = avcodec_find_decoder((*cp).codec_id);
    if codec.is_null() {
        crate::player::log(&format!(
            "ff: no image-sub decoder for codec_id={}",
            (*cp).codec_id
        ));
        return std::ptr::null_mut();
    }
    let ctx = avcodec_alloc_context3(codec);
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    if avcodec_parameters_to_context(ctx, cp) < 0
        || avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0
    {
        let mut c = ctx;
        avcodec_free_context(&mut c);
        return std::ptr::null_mut();
    }
    ctx
}

/// The subtitle stream's AUTHORING CANVAS (the coordinate space the decoded rects' x/y/w/h are
/// expressed in), or (0,0) if this decoder never declared one. 1920×1080 for Blu-ray PGS,
/// 720×480/576 for a DVD VobSub rip, 3840×2160 for some 4K PGS — assuming 1080p unconditionally
/// is what made VobSub land as a postage stamp in the corner.
///
/// Read WITHOUT a raw struct poke: `avcodec_parameters_from_context` copies the decoder's
/// width/height into the AVCodecParameters this crate already models (and whose width/height at
/// +48/+52 the video path has used on-device since the demuxer landed), so the whole read runs
/// inside the library's own code and needs no new ABI offset.
///
/// ABI proof (device's own `libavcodec.so.57.89.100`, disassembled 2026-07-29 — the build ships
/// stripped, so this is the primary evidence, not a header):
/// `avcodec_parameters_from_context+0x88` is `cmp r3,#3 / beq +0x13c` (AVMEDIA_TYPE_SUBTITLE ==
/// 3) and `+0x13c` is exactly `ldr r2,[r5,#124] / ldr r3,[r5,#128] / str r2,[r4,#48] /
/// str r3,[r4,#52]` — so THIS build does carry the subtitle case, and it corroborates
/// `OFF_CTX_WIDTH`/`OFF_CTX_HEIGHT` (124/128) and AVCodecParameters.width/height (48/52) at the
/// same time. The prologue's `memset(par, 0, 136)` likewise confirms the modeled sizeof = 136.
///
/// Must be called AFTER a decode: PGS carries the canvas in the presentation composition segment,
/// so pgssubdec only sets it while decoding (dvdsub sets it at open, from the .idx `size:` line).
unsafe fn sub_canvas(dec: *mut AVCodecContext) -> (i32, i32) {
    let par = avcodec_parameters_alloc();
    if par.is_null() {
        return (0, 0);
    }
    let wh = if avcodec_parameters_from_context(par, dec) >= 0 {
        ((*par).width, (*par).height)
    } else {
        (0, 0)
    };
    let mut p = par;
    avcodec_parameters_free(&mut p);
    // A canvas we cannot make sense of is worse than none: report unknown and let the renderer
    // fall back to 1:1 rather than scale the cue by a garbage ratio. The window spans every real
    // authoring canvas with room to spare (the smallest in the wild is DVD's 720×480) and rejects
    // a decoder that reports a rect size, a zero, or an uninitialised field.
    const MIN: c_int = 160;
    const MAX: c_int = 8192;
    if wh.0 < MIN || wh.1 < MIN || wh.0 > MAX || wh.1 > MAX {
        (0, 0)
    } else {
        wh
    }
}

/// Copy one decoded PAL8 subtitle rect into the store's indexed form — its `w*h` palette indices
/// (the decoder's rows, stride dropped) and its 256-entry palette as straight-alpha RGBA (palette
/// entries are 0xAARRGGBB) — or None if the decoder left it unusable. Indexed rather than
/// expanded: a quarter of the bytes, which is what `player::SUB_BITMAP_BUDGET` is sized on; the
/// renderer expands a set once, when it uploads it (`SubRect::to_rgba`). Coords are passed
/// through in the stream's own authoring canvas — the renderer scales, not us.
///
/// Every field here is unvalidated data from a decoder fed by the network, and `usize` is 32
/// bits on this target, so the size is bounded BEFORE it is multiplied: `w*h` for a rect the
/// decoder claimed was 40000×40000 is refused before any allocation, and the copy loop can never
/// run off the end of what was allocated (a panic on the demux thread, which is outside
/// `ui::guard`). The palette is the decoders' fixed `AVPALETTE_SIZE` (256 entries, which pgssub
/// and dvdsub allocate whole), so any index byte stays inside it.
unsafe fn rect_to_indexed(r: *const AVSubtitleRect) -> Option<crate::player::SubRect> {
    if r.is_null() {
        return None;
    }
    let (x, y, w, h, stride) = ((*r).x, (*r).y, (*r).w, (*r).h, (*r).linesize[0]);
    let idx = (*r).data[0];
    let pal = (*r).data[1] as *const u32;
    // no real subtitle bitmap approaches 8192 on a side — the largest authoring canvas in the
    // wild is 4K, and a rect cannot usefully exceed its own canvas
    const MAX_SIDE: c_int = 8192;
    if idx.is_null()
        || pal.is_null()
        || w <= 0
        || h <= 0
        || stride < w
        || w > MAX_SIDE
        || h > MAX_SIDE
    {
        return None;
    }
    let (wu, hu, su) = (w as usize, h as usize, stride as usize);
    // the 4x headroom keeps `to_rgba`'s expansion of this rect inside `usize` too
    let pixels = wu.checked_mul(hu).filter(|n| n.checked_mul(4).is_some())?;
    let mut index = Vec::with_capacity(pixels);
    for row in 0..hu {
        index.extend_from_slice(std::slice::from_raw_parts(idx.add(row * su), wu));
    }
    let mut palette = Box::new([[0u8; 4]; 256]);
    for (i, entry) in palette.iter_mut().enumerate() {
        let p = *pal.add(i); // 0xAARRGGBB (native u32)
        *entry = [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8];
    }
    Some(crate::player::SubRect { x, y, w, h, index, palette })
}

/// Decode one image-subtitle packet (the demux loop calls this for EVERY image track while
/// subtitles are on) and push it to the render store.
/// A CLEAR (num_rects==0) closes the open cue; otherwise EVERY rect of the display set is
/// copied in indexed form (`rect_to_indexed`) and pushed as one cue with start = packet pts (the end is
/// set later by the next CLEAR or superseding set). Two-line dialogue and sign-plus-dialogue are
/// authored as separate rects of the SAME display set, so dropping all but rect 0 (what this did
/// before) silently lost half the line. The set's canvas comes from `sub_canvas`.
unsafe fn decode_bitmap(
    dec: *mut AVCodecContext,
    pkt: *mut AVPacket,
    track: c_int,
    st: *mut AVStream,
) -> Option<SubCue<'static>> {
    let mut sub: AVSubtitle = std::mem::zeroed();
    let mut got: c_int = 0;
    if avcodec_decode_subtitle2(dec, &mut sub, &mut got, pkt) < 0 || got == 0 {
        return None;
    }
    let pts = pts_ns(pkt, st);
    if sub.num_rects == 0 {
        avsubtitle_free(&mut sub);
        return Some(SubCue::BitmapClear { track, pts });
    }
    // A pathological display set cannot be allowed to bloat the 24 MiB store or the renderer's
    // texture set; DVB regions are the realistic source of many rects, PGS allows at most 2.
    let n = (sub.num_rects as usize).min(MAX_RECTS);
    let mut rects = Vec::with_capacity(n);
    for i in 0..n {
        if let Some(r) = rect_to_indexed(*sub.rects.add(i)) {
            rects.push(r);
        }
    }
    let cue = if rects.is_empty() {
        None
    } else {
        let (cw, ch) = sub_canvas(dec);
        Some(SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects: sub.num_rects as usize })
    };
    avsubtitle_free(&mut sub);
    cue
}

const MAX_RECTS: usize = 8;

/// [`decode_bitmap`] then [`SubCue::push`]: the decode-and-store step the demux loop runs for an
/// image-subtitle packet.
pub(super) unsafe fn decode_bitmap_cue(
    dec: *mut AVCodecContext,
    pkt: *mut AVPacket,
    track: c_int,
    st: *mut AVStream,
) {
    if let Some(cue) = decode_bitmap(dec, pkt, track, st) {
        cue.push(0);
    }
}

/// One cue produced from one subtitle packet, in the stream's clock (`pts_ns`), not yet delivered.
pub(super) enum SubCue<'a> {
    /// SRT / WebVTT / mov_text payload (mov_text's length prefix already dropped) for the text store.
    Text { track: i32, start: i64, end: i64, payload: &'a [u8] },
    /// An ASS/SSA dialogue line for `ass_source`.
    Ass { track: i32, start: i64, end: i64, payload: &'a [u8] },
    /// A decoded image display set (every rect, indexed) on its authoring canvas.
    BitmapSet {
        track: c_int,
        pts: i64,
        cw: i32,
        ch: i32,
        rects: Vec<crate::player::SubRect>,
        /// Rects the decoder produced, before the `MAX_RECTS` cap (for the cap's log line).
        total_rects: usize,
    },
    /// An image-subtitle CLEAR: closes the track's open bitmap cue at `pts`.
    BitmapClear { track: c_int, pts: i64 },
}

impl<'a> SubCue<'a> {
    /// The cue with every time moved by `by_ns` (negative = earlier). The side reader's clock
    /// mapping: a cue is stamped in the original Part's time and drawn against the remux's playhead,
    /// so each one goes through this with `-delta` before [`Self::push`].
    pub(super) fn shifted(self, by_ns: i64) -> SubCue<'a> {
        match self {
            SubCue::Text { track, start, end, payload } => {
                SubCue::Text { track, start: start.saturating_add(by_ns), end: end.saturating_add(by_ns), payload }
            }
            SubCue::Ass { track, start, end, payload } => {
                SubCue::Ass { track, start: start.saturating_add(by_ns), end: end.saturating_add(by_ns), payload }
            }
            SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects } => {
                SubCue::BitmapSet { track, pts: pts.saturating_add(by_ns), cw, ch, rects, total_rects }
            }
            SubCue::BitmapClear { track, pts } => SubCue::BitmapClear { track, pts: pts.saturating_add(by_ns) },
        }
    }
}

impl SubCue<'_> {
    /// Hand the cue to the render store. `ass_generation` is the `ass_source::begin` token an
    /// `Ass` cue is pushed under; the other kinds ignore it.
    pub(super) fn push(self, ass_generation: u64) {
        match self {
            SubCue::Text { track, start, end, payload } => {
                crate::player::push_subtitle_cue(track, start, end, payload);
            }
            SubCue::Ass { track, start, end, payload } => {
                crate::player::ass_source::push(ass_generation, track, start, end, payload);
            }
            SubCue::BitmapClear { track, pts } => crate::player::close_subtitle_bitmap(track, pts),
            SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects } => {
                if track == crate::player::desired_sub_idx() {
                    let r0 = &rects[0];
                    crate::player::log(&format!(
                        "image cue [{}ms] {}x{} at {},{} rects={} canvas={cw}x{ch}",
                        pts / 1_000_000,
                        r0.w,
                        r0.h,
                        r0.x,
                        r0.y,
                        rects.len()
                    ));
                }
                crate::player::push_subtitle_bitmap(track, pts, cw, ch, rects);
                if total_rects > MAX_RECTS {
                    crate::player::log(&format!(
                        "ff: image-sub track#{track} {total_rects} rects (capped at {MAX_RECTS})"
                    ));
                }
            }
        }
    }
}

/// The subtitle streams of one `AVFormatContext`, in FILE order, with the decoder of each image
/// stream. A track's 0-based position here is its `desired_sub_idx` / store key.
pub(super) struct SubTracks {
    /// (ffmpeg stream index, kind, decoder ctx). The decoder is non-null only for Bitmap tracks
    /// (PGS/VobSub/DVB), which are software-decoded to pixels; text tracks carry a null ctx.
    streams: Vec<(c_int, SubKind, *mut AVCodecContext)>,
}

impl SubTracks {
    /// Enumerate `fmt`'s subtitle streams and open a decoder for each image one.
    pub(super) unsafe fn open(fmt: *mut AVFormatContext, streams: *mut *mut AVStream) -> Self {
        let mut out: Vec<(c_int, SubKind, *mut AVCodecContext)> = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let cp = stream_codecpar(*streams.add(i as usize));
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                let k = sub_kind((*cp).codec_id);
                let dec = if k == SubKind::Bitmap {
                    open_sub_decoder(cp)
                } else {
                    std::ptr::null_mut()
                };
                out.push((i as c_int, k, dec));
            }
        }
        SubTracks { streams: out }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// The (stream index, kind, decoder) list, in file order.
    pub(super) fn entries(&self) -> &[(c_int, SubKind, *mut AVCodecContext)] {
        &self.streams
    }

    /// The track position of ffmpeg stream `stream_index`, if it is a subtitle stream.
    pub(super) fn position(&self, stream_index: c_int) -> Option<usize> {
        self.streams.iter().position(|(sidx, _, _)| *sidx == stream_index)
    }

    /// The `ff: sub tracks=[..] selected=N` line.
    pub(super) fn log_tracks(&self) {
        if self.streams.is_empty() {
            return;
        }
        let desc: Vec<String> = self
            .streams
            .iter()
            .map(|(si, k, _)| {
                let kn = match k {
                    SubKind::Ass => "ass",
                    SubKind::MovText => "mov_text",
                    SubKind::Plain => "text",
                    SubKind::Bitmap => "image",
                };
                format!("#{si}:{kn}")
            })
            .collect();
        crate::player::log(&format!(
            "ff: sub tracks=[{}] selected={}",
            desc.join(","),
            crate::player::desired_sub_idx()
        ));
    }

    /// Decode one packet of track `pos` (an index from [`Self::position`]; `st` is its stream).
    ///
    /// Text kinds yield a cue for EVERY track, selected or not, so a mid-play switch is instant;
    /// `end` is `pkt.duration` or, absent, `start + 4 s`. Image kinds are decoded only while
    /// subtitles are on at all (`desired_sub_idx() >= 0`), on every image track, so a switch
    /// between two image tracks stays instant. Reads the live selection; the packet is not unref'd.
    pub(super) unsafe fn decode<'p>(
        &self,
        pos: usize,
        pkt: *mut AVPacket,
        st: *mut AVStream,
    ) -> Option<SubCue<'p>> {
        let (_, kind, dec) = self.streams[pos];
        if kind == SubKind::Bitmap {
            if crate::player::desired_sub_idx() >= 0 && !dec.is_null() {
                return decode_bitmap(dec, pkt, pos as c_int, st);
            }
            return None;
        }
        let start = pts_ns(pkt, st);
        let dur = (*pkt).duration;
        let end = if dur > 0 {
            start + av_rescale_q(dur, stream_time_base(st), NS_TB)
        } else {
            start + 4_000_000_000
        };
        let sz = (*pkt).size.max(0) as usize;
        if (*pkt).data.is_null() || sz == 0 {
            return None;
        }
        let raw: &'p [u8] = std::slice::from_raw_parts((*pkt).data, sz);
        // mp4 tx3g: drop the 2-byte big-endian text-length prefix.
        let payload: &[u8] = if kind == SubKind::MovText && sz >= 2 {
            let tl = ((raw[0] as usize) << 8) | raw[1] as usize;
            &raw[2..2 + tl.min(sz - 2)]
        } else {
            raw
        };
        let track = pos as i32;
        Some(if kind == SubKind::Ass {
            SubCue::Ass { track, start, end, payload }
        } else {
            SubCue::Text { track, start, end, payload }
        })
    }

    /// Free every image decoder (they are reopened fresh by the next [`Self::open`]).
    pub(super) unsafe fn close(&mut self) {
        for (_, _, dec) in self.streams.iter_mut() {
            if !dec.is_null() {
                avcodec_free_context(dec); // frees + nulls
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_moves_every_variant() {
        let by = -2_500_000_000;
        match (SubCue::Text { track: 1, start: 10_000_000_000, end: 12_000_000_000, payload: b"x" }).shifted(by) {
            SubCue::Text { track, start, end, payload } => {
                assert_eq!((track, start, end, payload), (1, 7_500_000_000, 9_500_000_000, &b"x"[..]));
            }
            _ => panic!("a text cue stays a text cue"),
        }
        match (SubCue::Ass { track: 2, start: 5_000_000_000, end: 6_000_000_000, payload: b"y" }).shifted(by) {
            SubCue::Ass { track, start, end, .. } => assert_eq!((track, start, end), (2, 2_500_000_000, 3_500_000_000)),
            _ => panic!("an ASS cue stays an ASS cue"),
        }
        match (SubCue::BitmapSet { track: 3, pts: 9_000_000_000, cw: 1920, ch: 1080, rects: Vec::new(), total_rects: 4 })
            .shifted(by)
        {
            SubCue::BitmapSet { track, pts, cw, ch, total_rects, .. } => {
                assert_eq!((track, pts, cw, ch, total_rects), (3, 6_500_000_000, 1920, 1080, 4));
            }
            _ => panic!("a bitmap set stays a bitmap set"),
        }
        match (SubCue::BitmapClear { track: 3, pts: 1_000_000_000 }).shifted(by) {
            SubCue::BitmapClear { track, pts } => assert_eq!((track, pts), (3, -1_500_000_000)),
            _ => panic!("a clear stays a clear"),
        }
        // a huge time saturates rather than wrapping
        match (SubCue::BitmapClear { track: 0, pts: i64::MAX }).shifted(5) {
            SubCue::BitmapClear { pts, .. } => assert_eq!(pts, i64::MAX),
            _ => unreachable!(),
        }
    }
}
