//! Physical subtitle packet decode for one FFmpeg format context. The player
//! session owns dialect, charset, selection and render-store publication.

use super::*;
use plx_media_codecs::subtitle_extract::ImageDecoders;
use plx_playback_contract::subtitle::{DecodeKind, RawCue, TrackInfo};

/// File-order subtitle streams and their image decoders. The player supplies
/// one decode plan entry per inspected stream before any decoder is opened.
pub(super) struct SubTracks {
    infos: Vec<TrackInfo>,
    plan: Vec<DecodeKind>,
    decoders: ImageDecoders,
}

impl SubTracks {
    pub(super) unsafe fn inspect(fmt: *mut AVFormatContext, streams: *mut *mut AVStream) -> Vec<TrackInfo> {
        if fmt.is_null() || streams.is_null() { return Vec::new(); }
        let mut infos = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let st = *streams.add(i as usize);
            let cp = stream_codecpar(st);
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                infos.push(TrackInfo {
                    stream_index: i as c_int,
                    codec_name: std::ffi::CStr::from_ptr(avcodec_get_name((*cp).codec_id))
                        .to_string_lossy().into_owned(),
                    language: stream_language(st),
                });
            }
        }
        infos
    }

    pub(super) unsafe fn open(streams: *mut *mut AVStream, infos: Vec<TrackInfo>, plan: Vec<DecodeKind>) -> Self {
        assert_eq!(infos.len(), plan.len(), "subtitle plan must cover every file-order track");
        let specs: Vec<_> = infos.iter().zip(&plan)
            .map(|(info, kind)| (info.stream_index, matches!(kind, DecodeKind::Bitmap)))
            .collect();
        let decoders = ImageDecoders::open(streams, &specs);
        Self { infos, plan, decoders }
    }

    pub(super) fn empty() -> Self {
        Self { infos: Vec::new(), plan: Vec::new(), decoders: ImageDecoders::empty() }
    }

    pub(super) fn entries(&self) -> &[TrackInfo] { &self.infos }
    pub(super) fn plan(&self) -> &[DecodeKind] { &self.plan }
    pub(super) fn position(&self, stream_index: c_int) -> Option<usize> {
        self.infos.iter().position(|info| info.stream_index == stream_index)
    }

    /// Every text track is observed; image tracks are decoded while subtitles
    /// are enabled, matching live track-switch behavior.
    pub(super) unsafe fn decode(
        &self, pos: usize, pkt: *mut AVPacket, st: *mut AVStream,
        decode_image: bool, bitmap_budget: usize,
    ) -> Option<RawCue> {
        match self.plan.get(pos)? {
            DecodeKind::Bitmap if decode_image => self.decoders.decode(pos, pkt, bitmap_budget)
                .map(|display| RawCue::Bitmap { track: pos, pts: pts_ns(pkt, st), display }),
            DecodeKind::Text { max_bytes } | DecodeKind::Ass { max_bytes } => {
                let size = (*pkt).size.max(0) as usize;
                if size == 0 || size > *max_bytes || (*pkt).data.is_null() { return None; }
                let start = pts_ns(pkt, st);
                let duration = (*pkt).duration;
                let end = if duration > 0 {
                    start + av_rescale_q(duration, stream_time_base(st), NS_TB)
                } else { start + 4_000_000_000 };
                let payload = std::slice::from_raw_parts((*pkt).data, size).to_vec();
                Some(RawCue::Bytes { track: pos, start, end, payload })
            }
            _ => None,
        }
    }

    pub(super) unsafe fn close(&mut self) { self.decoders.close(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::subtitle_cue::SubCue;
    const PALETTE_BYTES: usize = plx_media_codecs::subtitle::PALETTE_BYTES;

    fn rects_within_budget(
        num_rects: usize,
        cost_at: impl FnMut(usize) -> Option<usize>,
        rect_at: impl FnMut(usize) -> Option<crate::player::SubRect>,
        budget: usize,
    ) -> Vec<crate::player::SubRect> {
        plx_media_codecs::subtitle::rects_within_budget(
            num_rects, cost_at, rect_at, |rect| rect.bytes(), budget,
        )
    }

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
        match (SubCue::Ass { track: 2, start: 5_000_000_000, end: 6_000_000_000, payload: b"y"[..].into() }).shifted(by) {
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
