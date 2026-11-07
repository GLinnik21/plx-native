//! Bitmap subtitle decode against the bundled FFmpeg, with neutral owned output.
use crate::ffi::*;
use std::os::raw::c_int;

pub use plx_playback_contract::subtitle::{IndexedRect, BitmapDisplay};

/// Open a software decoder for an image-subtitle stream (PGS/VobSub/DVB). Returns a
/// lib-allocated AVCodecContext (free with avcodec_free_context) or null if the build
/// lacks the decoder / open fails. parameters_to_context carries extradata — dvdsub needs
/// the palette from it, so this must run before open2.
pub unsafe fn open_sub_decoder(cp: *const AVCodecParameters) -> *mut AVCodecContext {
    let codec = avcodec_find_decoder((*cp).codec_id);
    if codec.is_null() {
        plx_base::eventlog::log(&format!(
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
/// The bundled FFmpeg layout is asserted against its headers in `ci/ffabi-assert.c`.
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
/// expanded: a quarter of the bytes. The renderer expands a set once, when it uploads it.
/// Coords are passed
/// through in the stream's own authoring canvas — the renderer scales, not us.
///
/// Every field here is unvalidated data from a decoder fed by the network, and `usize` is 32
/// bits on this target, so the size is bounded BEFORE it is multiplied: `w*h` for a rect the
/// decoder claimed was 40000×40000 is refused before any allocation, and the copy loop can never
/// run off the end of what was allocated (a panic on the demux thread, which is outside
/// `ui::guard`). The palette is the decoders' fixed `AVPALETTE_SIZE` (256 entries, which pgssub
/// and dvdsub allocate whole), so any index byte stays inside it.
pub unsafe fn rect_to_indexed(r: *const AVSubtitleRect) -> Option<IndexedRect> {
    let pixels = rect_pixels(r)?;
    let (x, y, w, h, stride) = ((*r).x, (*r).y, (*r).w, (*r).h, (*r).linesize[0]);
    let idx = (*r).data[0];
    let pal = (*r).data[1] as *const u32;
    let (hu, su, wu) = (h as usize, stride as usize, w as usize);
    let mut index = Vec::with_capacity(pixels);
    for row in 0..hu {
        index.extend_from_slice(std::slice::from_raw_parts(idx.add(row * su), wu));
    }
    let mut palette = Box::new([[0u8; 4]; 256]);
    for (i, entry) in palette.iter_mut().enumerate() {
        let p = *pal.add(i); // 0xAARRGGBB (native u32)
        *entry = [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8];
    }
    Some(IndexedRect { x, y, w, h, index, palette })
}

/// The pixel count (`w*h`) of a rect that [`rect_to_indexed`] would copy, read from the rect's own
/// fields alone — nothing is allocated — or None if the decoder left it unusable. This is the
/// whole validation, so the cost of a copy is known before it is made
/// ([`rects_within_budget`]).
pub unsafe fn rect_pixels(r: *const AVSubtitleRect) -> Option<usize> {
    if r.is_null() {
        return None;
    }
    let (w, h, stride) = ((*r).w, (*r).h, (*r).linesize[0]);
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
    // the 4x headroom keeps `to_rgba`'s expansion of this rect inside `usize` too
    (w as usize).checked_mul(h as usize).filter(|n| n.checked_mul(4).is_some())
}

/// The rects of one decoded display set that are kept, in the decoder's order: every usable rect
/// until the next one would take the set past `budget` bytes (the caller's `bytes_of` measure,
/// the measure the store's ceiling counts). A rect the decoder left unusable is skipped and costs
/// nothing. The first rect that does not fit ends the set, so the kept rects are always a prefix of
/// the decoder's order. The FIRST usable rect is kept whatever it costs, even past the budget: a
/// cue the viewer should see is never dropped for its size (`MAX_SIDE` in [`rect_to_indexed`]
/// bounds one rect), and the store evicts OTHER cues to make room for it.
///
/// `cost_at` is a rect's store cost read from the decoder's own
/// fields, or None for an unusable rect; `rect_at` is the copy. A rect the budget will refuse is
/// never copied: the copy of one the decoder claims at 8192x8192 is 64 MiB, made only to be
/// dropped on the next line.
pub fn rects_within_budget<T>(
    num_rects: usize,
    mut cost_at: impl FnMut(usize) -> Option<usize>,
    mut rect_at: impl FnMut(usize) -> Option<T>,
    bytes_of: impl Fn(&T) -> usize,
    budget: usize,
) -> Vec<T> {
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for i in 0..num_rects {
        let Some(cost) = cost_at(i) else { continue };
        if !kept.is_empty() && bytes.saturating_add(cost) > budget {
            break;
        }
        let Some(r) = rect_at(i) else { continue };
        bytes += bytes_of(&r);
        kept.push(r);
    }
    kept
}

/// The palette's share of a rect's store cost (`SubRect::bytes`).
pub const PALETTE_BYTES: usize = std::mem::size_of::<[[u8; 4]; 256]>();

/// Decode one image-subtitle packet into owned data for the media layer to publish.
/// A CLEAR (num_rects==0) closes the open cue; otherwise EVERY rect of the display set is
/// copied in indexed form (`rect_to_indexed`). Two-line dialogue and sign-plus-dialogue are
/// authored as separate rects of the SAME display set, so dropping all but rect 0 (what this did
/// before) silently lost half the line. The set's canvas comes from `sub_canvas`.
pub unsafe fn decode_bitmap(
    dec: *mut AVCodecContext,
    pkt: *mut AVPacket,
    budget: usize,
) -> Option<BitmapDisplay> {
    let mut sub: AVSubtitle = std::mem::zeroed();
    let mut got: c_int = 0;
    if avcodec_decode_subtitle2(dec, &mut sub, &mut got, pkt) < 0 || got == 0 {
        return None;
    }
    if sub.num_rects == 0 {
        avsubtitle_free(&mut sub);
        return Some(BitmapDisplay::Clear);
    }
    let rects = rects_within_budget(
        sub.num_rects as usize,
        |i| rect_pixels(*sub.rects.add(i)).map(|pixels| pixels + PALETTE_BYTES),
        |i| rect_to_indexed(*sub.rects.add(i)),
        |r| r.bytes(),
        budget,
    );
    let cue = if rects.is_empty() {
        None
    } else {
        let (cw, ch) = sub_canvas(dec);
        Some(BitmapDisplay::Set { cw, ch, rects, total_rects: sub.num_rects as usize })
    };
    avsubtitle_free(&mut sub);
    cue
}
