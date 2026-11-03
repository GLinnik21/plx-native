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
use crate::player::text_ass::TextDialect;
use plx_media_codecs::subtitle_extract::ImageDecoders;
#[cfg(test)]
pub(super) use plx_media_codecs::subtitle::open_sub_decoder;
/// How a subtitle stream's payload turns into displayable text — classified by the codec's
/// name (avcodec_get_name is already linked, so we avoid hardcoding the n3.3 subtitle codec-id
/// block, which the ABI probe never verified). Bitmap subs (PGS/VobSub/DVB/teletext) carry no
/// text and can't be client-rendered, but still occupy their file-order slot so the track
/// menu's desired_sub_idx stays aligned with the metadata subs list.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum SubKind {
    /// SRT / subrip / webvtt / sami / text / mov_text ...: the packet payload is text, UTF-8 or (see
    /// `sub_charset`) a legacy encoding, converted to ASS by the dialect (`player::text_ass`) and
    /// rendered by libass like an authored track. mov_text (mp4 tx3g) also carries a 2-byte
    /// big-endian text-length prefix before the text.
    Text(TextDialect),
    Ass,    // ASS / SSA: dialogue line; text is the field after the 8th comma
    Bitmap, // PGS / VobSub / DVB / teletext: image subtitle, not renderable here
}

pub(super) unsafe fn sub_kind(codec_id: c_int) -> SubKind {
    let name = std::ffi::CStr::from_ptr(avcodec_get_name(codec_id)).to_string_lossy();
    if matches!(name.as_ref(), "ass" | "ssa") {
        return SubKind::Ass;
    }
    TextDialect::from_codec_name(&name).map_or(SubKind::Bitmap, SubKind::Text)
}

/// Adapt the codec's neutral bitmap output to the player store's cue type.
unsafe fn decode_bitmap(
    dec: *mut AVCodecContext,
    pkt: *mut AVPacket,
    track: c_int,
    st: *mut AVStream,
) -> Option<SubCue<'static>> {
    let display = plx_media_codecs::subtitle::decode_bitmap(dec, pkt, crate::player::SUB_BITMAP_BUDGET)?;
    let pts = pts_ns(pkt, st);
    Some(bitmap_cue(display, track, pts))
}

fn bitmap_cue(display: plx_media_codecs::subtitle::BitmapDisplay, track: c_int, pts: i64) -> SubCue<'static> {
    match display {
        plx_media_codecs::subtitle::BitmapDisplay::Clear => SubCue::BitmapClear { track, pts },
        plx_media_codecs::subtitle::BitmapDisplay::Set { cw, ch, rects, total_rects } => {
            SubCue::BitmapSet {
                track, pts, cw, ch,
                rects: rects.into_iter().map(|r| crate::player::SubRect {
                    x: r.x, y: r.y, w: r.w, h: r.h, index: r.index, palette: r.palette,
                }).collect(),
                total_rects,
            }
        }
    }
}

/// Player-store adapter for the shared, generic byte-budget selector.
fn rects_within_budget(
    num_rects: usize,
    cost_at: impl FnMut(usize) -> Option<usize>,
    rect_at: impl FnMut(usize) -> Option<crate::player::SubRect>,
    budget: usize,
) -> Vec<crate::player::SubRect> {
    plx_media_codecs::subtitle::rects_within_budget(num_rects, cost_at, rect_at, |r| r.bytes(), budget)
}

const PALETTE_BYTES: usize = plx_media_codecs::subtitle::PALETTE_BYTES;

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
    /// SRT / WebVTT / mov_text ... payload (mov_text's length prefix already dropped), converted to
    /// ASS by the track's dialect in `ass_source::push_text`.
    /// `payload` is always UTF-8 here: [`SubTracks::decode`] ran the track's charset step.
    /// `identity` is `ass_source::packet_identity` of the packet bytes BEFORE the charset step: the
    /// dedup key of a re-read, which must not depend on how the detector decoded it this time.
    Text { track: i32, start: i64, end: i64, payload: std::borrow::Cow<'a, [u8]>, identity: u64 },
    /// An ASS/SSA dialogue line for `ass_source`.
    Ass { track: i32, start: i64, end: i64, payload: &'a [u8] },
    /// A decoded image display set (every rect, indexed) on its authoring canvas.
    BitmapSet {
        track: c_int,
        pts: i64,
        cw: i32,
        ch: i32,
        rects: Vec<crate::player::SubRect>,
        /// Rects the decoder produced, before the byte budget and the unusable-rect skip (for the
        /// log line that reports a set the store had to thin).
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
            SubCue::Text { track, start, end, payload, identity } => {
                SubCue::Text { track, start: start.saturating_add(by_ns), end: end.saturating_add(by_ns), payload, identity }
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
    /// Hand the cue to the render store. `ass_generation` is the `ass_source::begin` token a
    /// `Text` or `Ass` cue is pushed under; the image kinds ignore it.
    pub(super) fn push(self, ass_generation: u64) {
        match self {
            SubCue::Text { track, start, end, payload, identity } => {
                let text = String::from_utf8_lossy(&payload);
                crate::player::ass_source::push_text(ass_generation, track, start, end, &text, identity);
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
                let rects_kept = rects.len();
                crate::player::push_subtitle_bitmap(track, pts, cw, ch, rects);
                if rects_kept < total_rects {
                    crate::player::log(&format!(
                        "ff: image-sub track#{track} kept {rects_kept} of {total_rects} rects"
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
    decoders: ImageDecoders,
    /// Per track (same order as `streams`): the language-derived text charset, fixed by the first
    /// non-UTF-8 packet (`player::sub_charset`).
    charsets: Vec<crate::player::sub_charset::TrackCharset>,
}

impl SubTracks {
    /// Enumerate `fmt`'s subtitle streams and open a decoder for each image one.
    pub(super) unsafe fn open(fmt: *mut AVFormatContext, streams: *mut *mut AVStream) -> Self {
        if fmt.is_null() || streams.is_null() {
            return Self::empty();
        }
        let mut kinds: Vec<(c_int, SubKind)> = Vec::new();
        let mut charsets = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let cp = stream_codecpar(*streams.add(i as usize));
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                let k = sub_kind((*cp).codec_id);
                kinds.push((i as c_int, k));
                charsets.push(crate::player::sub_charset::TrackCharset::new(
                    stream_language(*streams.add(i as usize)).as_deref(),
                ));
            }
        }
        let specs: Vec<(c_int, bool)> = kinds.iter().map(|(si, k)| (*si, *k == SubKind::Bitmap)).collect();
        let decoders = ImageDecoders::open(streams, &specs);
        let streams = kinds.into_iter().enumerate().map(|(pos, (si, kind))| (si, kind, decoders.context(pos))).collect();
        SubTracks { streams, decoders, charsets }
    }

    /// No tracks: what the main demuxer holds while a side reader owns the subtitles.
    pub(super) fn empty() -> Self {
        SubTracks { streams: Vec::new(), decoders: ImageDecoders::empty(), charsets: Vec::new() }
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
                    SubKind::Text(TextDialect::MovText) => "mov_text",
                    SubKind::Text(_) => "text",
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
        let (_, kind, _) = self.streams[pos];
        if kind == SubKind::Bitmap {
            if crate::player::desired_sub_idx() >= 0 {
                let display = self.decoders.decode(pos, pkt, crate::player::SUB_BITMAP_BUDGET)?;
                return Some(bitmap_cue(display, pos as c_int, pts_ns(pkt, st)));
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
        let payload: &[u8] = if kind == SubKind::Text(TextDialect::MovText) && sz >= 2 {
            let tl = ((raw[0] as usize) << 8) | raw[1] as usize;
            &raw[2..2 + tl.min(sz - 2)]
        } else {
            raw
        };
        let track = pos as i32;
        Some(if kind == SubKind::Ass {
            SubCue::Ass { track, start, end, payload }
        } else {
            // Text is UTF-8 by the spec and legacy-encoded (GBK, Big5, ...) in practice (#628).
            let identity = crate::player::ass_source::packet_identity(payload);
            let payload = match crate::player::sub_charset::subtitle_text(payload, &self.charsets[pos]) {
                std::borrow::Cow::Borrowed(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
                std::borrow::Cow::Owned(text) => std::borrow::Cow::Owned(text.into_bytes()),
            };
            SubCue::Text { track, start, end, payload, identity }
        })
    }

    /// Free every image decoder (they are reopened fresh by the next [`Self::open`]).
    pub(super) unsafe fn close(&mut self) {
        self.decoders.close();
        for (_, _, dec) in self.streams.iter_mut() { *dec = std::ptr::null_mut(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rect whose store cost is exactly `pixels + 1024` (its indices plus the palette), so a
    /// budget can be written as a count of rects.
    fn rect_of_pixels(pixels: usize, x: i32) -> crate::player::SubRect {
        crate::player::SubRect { x, y: 0, w: pixels as i32, h: 1, index: vec![0; pixels], palette: Box::new([[0; 4]; 256]) }
    }

    /// A display set of `n` rects, each `pixels` wide, through the same helper the decoder uses.
    fn set_of(n: usize, pixels: usize, budget: usize) -> Vec<crate::player::SubRect> {
        rects_within_budget(n, |_| Some(pixels + PALETTE_BYTES), |i| Some(rect_of_pixels(pixels, i as i32)), budget)
    }

    /// PGS can carry more than eight objects in one display set, and a dense set (a two-line
    /// dialogue plus a sign plus a logo, from DVB regions) is authored that way. The eighth-rect
    /// cap silently left rects 9 onward undrawn, so all twelve must survive, in the decoder's order.
    #[test]
    fn a_display_set_of_twelve_rects_keeps_all_twelve() {
        let kept = set_of(12, 16, crate::player::SUB_BITMAP_BUDGET);
        assert_eq!(kept.len(), 12, "every rect of the decoded set is kept, not the first eight");
        let xs: Vec<i32> = kept.iter().map(|r| r.x).collect();
        assert_eq!(xs, (0..12).collect::<Vec<i32>>(), "decoder order is preserved");
    }

    /// The store's byte ceiling still bounds one cue's pixels: a rect that does not fit ends the set,
    /// the rects before it are kept, and nothing panics. Each rect here costs 2024 bytes, so a
    /// 10_000-byte budget holds four.
    #[test]
    fn a_display_set_over_the_byte_budget_keeps_the_prefix_that_fits() {
        let kept = set_of(12, 1000, 10_000);
        assert_eq!(kept.len(), 4, "four rects of 2024 bytes fit in 10_000");
        assert!(kept.iter().map(|r| r.bytes()).sum::<usize>() <= 10_000);
        assert_eq!(kept.last().map(|r| r.x), Some(3), "the kept rects are the first four, in order");
    }

    /// **A cue that alone exceeds the budget is still shown.** The budget bounds the cache of OTHER
    /// cues, never the one the viewer is meant to read: a set's first usable rect is always kept,
    /// and only the rects after it are held to what is left.
    #[test]
    fn a_rect_larger_than_the_budget_is_kept_as_the_cue_itself() {
        let kept = set_of(3, 5000, 4000);
        assert_eq!(kept.len(), 1, "the oversized first rect is the cue; nothing after it fits");
        assert_eq!(kept[0].x, 0);
        // an unusable rect in front does not use up the exemption
        let kept = rects_within_budget(
            3,
            |i| (i != 0).then_some(5000 + PALETTE_BYTES),
            |i| (i != 0).then(|| rect_of_pixels(5000, i as i32)),
            4000,
        );
        assert_eq!(kept.iter().map(|r| r.x).collect::<Vec<_>>(), [1]);
    }

    /// A rect the decoder left unusable is skipped and does not stop the set, and does not spend
    /// budget.
    #[test]
    fn an_unusable_rect_is_skipped_and_costs_no_budget() {
        let kept = rects_within_budget(
            3,
            |i| (i != 1).then_some(100 + PALETTE_BYTES),
            |i| (i != 1).then(|| rect_of_pixels(100, i as i32)),
            2 * (100 + 1024),
        );
        assert_eq!(kept.iter().map(|r| r.x).collect::<Vec<_>>(), [0, 2]);
    }

    /// **A rect the budget will refuse is never copied.** Rect 1 is far over what is left, so the set
    /// ends there; the counter proves its pixels were never allocated (only rect 0 was).
    #[test]
    fn a_rect_the_budget_rejects_is_never_allocated() {
        let copies = std::cell::RefCell::new(Vec::new());
        let kept = rects_within_budget(
            3,
            |i| Some(if i == 1 { 8192 * 8192 + PALETTE_BYTES } else { 100 + PALETTE_BYTES }),
            |i| {
                copies.borrow_mut().push(i);
                Some(rect_of_pixels(100, i as i32))
            },
            crate::player::SUB_BITMAP_BUDGET,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(*copies.borrow(), [0], "only the rect that fits was copied");
    }

    /// A set of one to eight rects is kept exactly as it always was: every rect, under the budget.
    #[test]
    fn a_display_set_of_one_to_eight_rects_is_unchanged() {
        for n in 1..=8 {
            assert_eq!(set_of(n, 16, crate::player::SUB_BITMAP_BUDGET).len(), n, "{n} rects");
        }
    }

    #[test]
    fn shifted_moves_every_variant() {
        let by = -2_500_000_000;
        match (SubCue::Text { track: 1, start: 10_000_000_000, end: 12_000_000_000, payload: b"x"[..].into(), identity: 7 }).shifted(by) {
            SubCue::Text { track, start, end, payload, .. } => {
                assert_eq!((track, start, end, &payload[..]), (1, 7_500_000_000, 9_500_000_000, &b"x"[..]));
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
