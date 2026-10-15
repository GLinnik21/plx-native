//! Player-owned ASS texture cache. Native parsing/rasterization is the worker's job;
//! the frame thread submits its clock and uploads only a changed completed image.
//! Authored placement is retained even while the transport HUD is visible; a text subtitle's
//! bottom-anchored regions rise clear of it (plan `libass-text-subs` §2.4).
use plx_media::player::{
    ass, ass_source, sidecar,
    text_style,
    video_geometry::Viewport,
};
use plx_ui::{
    consts::{SCR_H, SCR_W},
    Painter, Rect,
};
use plx_gfx::gfx::{bucket_dim, BucketTex};
use std::sync::Arc;

#[derive(Default)]
pub struct AssSubtitles {
    frame: Option<Arc<ass::Frame>>,
    source: u64,
    clock: ass_source::Clock,
    textures: Vec<Slot>,
    uploaded: u64,
    viewport: Viewport,
    /// The origin `frame` was requested under. The source's current origin can already differ
    /// (a restyle bumps the revision at once; the worker's re-render follows), and the lift is
    /// decided by the frame on screen (plan `libass-text-subs` §2.4).
    frame_origin: ass::Origin,
    /// The alignment classes of the cues in one frame, by its serial (Text origin only).
    aligns: Option<(u64, ass::AlignSet)>,
    /// Reused each draw so the 60 fps path allocates nothing.
    spans: Vec<(i32, i32)>,
    dys: Vec<f32>,
}

/// Every class present: the rule for a frame whose own classes are not known. It never lifts a
/// top or middle cue and still lifts a normal bottom block by the full `HUD_LIFT`.
const UNKNOWN_ALIGNS: ass::AlignSet = ass::AlignSet { bottom: true, middle: true, top: true };

impl AssSubtitles {
    pub fn update(&mut self, ps: &plx_media::route::PlaybackSession, now: u32) {
        let source = if plx_media::route::subtitles_burned(ps) || plx_media::player::loading(ps) {
            None
        } else {
            sidecar::ass_source(false)
                .or_else(|| ass_source::selected(plx_media::player::desired_sub_idx()))
        };
        let Some(source) = source else {
            if self.source != 0 {
                ass::clear();
            }
            self.source = 0;
            self.frame = None;
            return;
        };
        if self.source != source.id {
            self.source = source.id;
            self.clock = ass_source::Clock::default();
            self.frame = None;
            self.aligns = None;
        }
        let clock = self.clock.sample(
            plx_media::player::playpos_ns(),
            now,
            plx_media::player::is_playing(ps),
        );
        let clock = clock.saturating_sub(plx_media::player::subtitle_offset_ms());
        let (viewport, (coded_w, coded_h)) = frame_geometry(
            source.origin,
            plx_media::player::video_viewport(SCR_W as i32, SCR_H as i32),
            plx_media::player::video_raster(),
        );
        if self.viewport != viewport {
            self.viewport = viewport;
            self.frame = None;
        }
        let origin = source.origin;
        if let Some(frame) =
            ass::request(source, clock, viewport.width, viewport.height, coded_w, coded_h)
        {
            self.frame = Some(frame);
            self.frame_origin = origin;
        }
    }

    pub fn fingerprint(&self) -> u64 {
        self.frame.as_ref().map_or(0, |f| f.serial)
    }

    /// `lift` is the transport HUD being up. It moves a text subtitle's bottom regions and nothing
    /// else: an authored script keeps its own placement.
    pub fn draw(&mut self, lift: bool) {
        let Some(frame) = self.frame.clone() else {
            if self.source == 0 {
                clear_textures(&mut self.textures);
                self.uploaded = 0;
            }
            return;
        };
        if self.uploaded != frame.serial {
            plx_base::diag::spans::span("assup", || {
                // Reserve all unchanged regions first. A changing region must not steal
                // a texture that a later static sign still needs in this same frame.
                let matches = retained_regions(&self.textures, &frame.rects);
                let mut old: Vec<_> = std::mem::take(&mut self.textures)
                    .into_iter()
                    .map(Some)
                    .collect();
                let mut next: Vec<_> = matches
                    .into_iter()
                    .map(|index| index.and_then(|i| old[i].take()))
                    .collect();
                for (slot, rect) in next.iter_mut().zip(&frame.rects) {
                    if slot.is_none() {
                        let i = pick_slot(old.iter().map(Option::as_ref), rect.width, rect.height);
                        let mut recycled = i.and_then(|i| old[i].take()).unwrap_or_default();
                        plx_media::player::ass_perf::note_upload(rect.rgba.len());
                        recycled.upload(rect);
                        *slot = Some(recycled);
                    }
                }
                for unused in old.into_iter().flatten() {
                    unused.delete();
                }
                self.textures = next.into_iter().flatten().collect();
                self.uploaded = frame.serial;
            });
        }
        let origin = self.frame_origin;
        let aligns = if lift && origin.is_text() {
            aligns_for(&mut self.aligns, frame.serial, || ass::published_aligns(frame.serial))
        } else {
            UNKNOWN_ALIGNS
        };
        self.spans.clear();
        self.spans.extend(frame.rects.iter().map(|r| (r.y, r.height)));
        lift_dys(origin, lift, &self.spans, aligns, &mut self.dys);
        let ink = plx_appkit::player_hud::subtitle_ink();
        for (i, (slot, rect)) in self.textures.iter().zip(&frame.rects).enumerate() {
            let dy = self.dys[i];
            let shown = slot.shown();
            if shown.used_w == 0 || shown.used_h == 0 {
                continue;
            }
            Painter::root().tex_uv(
                shown.tex,
                shown.uv(),
                Rect::new(
                    (self.viewport.x + rect.x) as f32,
                    (self.viewport.y + rect.y) as f32 + dy,
                    rect.width as f32,
                    rect.height as f32,
                ),
                0.0,
                ink,
            );
        }
    }

    pub fn error(&self) -> Option<&'static str> {
        self.frame.as_ref().and_then(|f| f.error).map(plx_media::player::ass::Fault::message)
    }

    pub fn release(&mut self) {
        clear_textures(&mut self.textures);
        self.uploaded = 0;
        self.frame = None;
        self.aligns = None;
        self.source = 0;
        ass::clear();
    }

    pub fn render_report(&self) -> plx_ui::frame::RenderReport {
        plx_ui::frame::RenderReport {
            textures: self.textures.iter().map(Slot::texture_count).sum(),
            bytes: self.textures.iter().map(Slot::allocated_bytes).sum(),
        }
    }
}

/// The viewport a frame is rendered into and the storage raster it is sized against. An authored
/// script keeps the video's; a text subtitle is placed in screen space, so it gets the whole
/// logical screen whatever the video's aspect (plan §2.1).
fn frame_geometry(
    origin: ass::Origin,
    video: Viewport,
    coded: (i32, i32),
) -> (Viewport, (i32, i32)) {
    if origin.is_text() {
        let (w, h) = (text_style::SCR_W as i32, text_style::SCR_H as i32);
        (Viewport { x: 0, y: 0, width: w, height: h }, (w, h))
    } else {
        (video, coded)
    }
}

/// How far (vertical px, negative is up) each region of a Text-origin frame moves while the HUD is
/// up (`lift`); an authored frame never moves. `regions` are `(y, height)` in frame pixels. Plan §2.4:
/// - every active cue bottom-aligned: all regions move by the fixed `HUD_LIFT`;
/// - no bottom cue: nothing moves;
/// - mixed: a region moves when its bottom edge is at or below `anchor - pitch / 2`, unless such a
///   region also starts above the frame's midline (a merged top+bottom region), in which case
///   nothing moves, erring towards not lifting a top cue. A region right above the moving group
///   (a block whose lines are separate regions) joins it.
///
/// The moved group is clamped once so its top stays on the screen.
fn lift_dys(
    origin: ass::Origin,
    lift: bool,
    regions: &[(i32, i32)],
    aligns: ass::AlignSet,
    dys: &mut Vec<f32>,
) {
    dys.clear();
    dys.resize(regions.len(), 0.0);
    let ass::Origin::Text(style) = origin else { return };
    if !lift || !aligns.bottom {
        return;
    }
    let pitch = text_style::CAPTION_LINE_PITCH * style.scale();
    // `dys` doubles as the "moves" mark (-1) until the lift is known.
    if aligns.middle || aligns.top {
        let band = text_style::bottom_anchor_y(style.position) - pitch / 2.0;
        let mut merged = false;
        for (dy, &(y, h)) in dys.iter_mut().zip(regions) {
            if (y + h) as f32 >= band {
                *dy = -1.0;
                merged |= (y as f32) < text_style::SCR_H / 2.0;
            }
        }
        if merged {
            dys.fill(0.0);
            return;
        }
        // A block whose lines are separate regions rises as one: a region sitting right above
        // the moving group (a gap under half a pitch) joins it, up to a fixed point.
        while let Some(top) = moving_top(regions, dys) {
            let mut grew = false;
            for (dy, &(y, h)) in dys.iter_mut().zip(regions) {
                if *dy == 0.0 && y < top && ((y + h) as f32) >= top as f32 - pitch / 2.0 {
                    *dy = -1.0;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
    } else {
        dys.fill(-1.0);
    }
    let Some(top) = moving_top(regions, dys) else { return };
    let lift = text_style::HUD_LIFT.min(top.max(0) as f32);
    for dy in dys.iter_mut().filter(|d| **d != 0.0) {
        *dy = -lift;
    }
}

/// The top edge of the regions marked as moving (`dys` entry non-zero).
fn moving_top(regions: &[(i32, i32)], dys: &[f32]) -> Option<i32> {
    regions.iter().zip(dys).filter(|(_, &d)| d != 0.0).map(|(r, _)| r.0).min()
}

/// The classes of the frame with `serial`: the cached answer if it is for this frame, else ask
/// (`ask` may find the mailbox busy, or the frame no longer published) and cache a hit. With no
/// answer for THIS frame the classes are unknown, never another frame's (`UNKNOWN_ALIGNS`).
fn aligns_for(
    cache: &mut Option<(u64, ass::AlignSet)>,
    serial: u64,
    ask: impl FnOnce() -> Option<ass::AlignSet>,
) -> ass::AlignSet {
    if let Some((s, set)) = *cache {
        if s == serial {
            return set;
        }
    }
    match ask() {
        Some(set) => {
            *cache = Some((serial, set));
            set
        }
        None => UNKNOWN_ALIGNS,
    }
}

/// A region's two textures. A changed region uploads into the one NOT drawn last, so a partial
/// `glTexSubImage2D` never lands on a texture the in-flight frames still sample (Mali copies the
/// whole texture to keep them intact, resource ghosting); at the subtitle change rate the spare was
/// last drawn many frames ago.
#[derive(Default)]
struct Slot {
    bufs: [BucketTex; 2],
    /// Index of the buffer drawn last.
    shown: usize,
    width: i32,
    height: i32,
    /// Identity of the pixels in `bufs[shown]`, for [`retained_regions`].
    pixels: Option<Arc<[u8]>>,
}

/// What an upload does to the spare buffer, by how the needed bucket compares with its storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpareUse {
    /// It fits and is not wasteful: copy in.
    Reuse,
    /// Nothing there yet, or too small: allocate (growing only goes up).
    Grow,
    /// Much larger than needed: free it and start over so the allocation shrinks.
    Reset,
}

/// A buffer whose area exceeds this multiple of the needed bucket's is shrunk. Growth takes the
/// per-axis maximum, so without it a tall-then-wide sequence only ever ratchets the storage up.
const SHRINK_FACTOR: i64 = 4;

fn spare_use(spare: BucketTex, w: i32, h: i32) -> SpareUse {
    if spare.tex == 0 {
        return SpareUse::Grow;
    }
    let need = i64::from(bucket_dim(w)) * i64::from(bucket_dim(h));
    if i64::from(spare.alloc_w) * i64::from(spare.alloc_h) > need * SHRINK_FACTOR {
        SpareUse::Reset
    } else if spare.fits(w, h) {
        SpareUse::Reuse
    } else {
        SpareUse::Grow
    }
}

/// Which unmatched old slot to recycle for a `w`x`h` region: the first whose spare can take it as
/// is, else the first that would grow, and an oversized one (to be reset) last. `None` when there
/// is none.
fn pick_slot<'a>(
    olds: impl Iterator<Item = Option<&'a Slot>>,
    w: i32,
    h: i32,
) -> Option<usize> {
    olds.enumerate()
        .filter_map(|(i, s)| s.map(|s| (i, spare_use(s.bufs[1 - s.shown], w, h))))
        .min_by_key(|&(i, u)| (u != SpareUse::Reuse, u == SpareUse::Reset, i))
        .map(|(i, _)| i)
}

impl Slot {
    fn shown(&self) -> BucketTex {
        self.bufs[self.shown]
    }

    /// Put `rect` into the spare buffer and make it the shown one.
    fn upload(&mut self, rect: &ass::Rect) {
        let spare = 1 - self.shown;
        let mut prev = self.bufs[spare];
        if spare_use(prev, rect.width, rect.height) == SpareUse::Reset {
            plx_gfx::gfx::delete_tex(prev.tex);
            prev = BucketTex::default();
        }
        self.bufs[spare] =
            plx_gfx::gfx::upload_rgba_bucketed(prev, rect.width, rect.height, rect.rgba.as_ptr());
        self.shown = spare;
        self.width = rect.width;
        self.height = rect.height;
        self.pixels = Some(rect.rgba.clone());
    }

    fn texture_count(&self) -> u32 {
        self.bufs.iter().filter(|b| b.tex != 0).count() as u32
    }

    /// Storage the driver holds, not the part drawn: RGBA8 at the bucketed size.
    fn allocated_bytes(&self) -> usize {
        self.bufs.iter().map(|b| b.alloc_w as usize * b.alloc_h as usize * 4).sum()
    }

    fn delete(self) {
        for b in self.bufs {
            plx_gfx::gfx::delete_tex(b.tex);
        }
    }
}

fn clear_textures(textures: &mut Vec<Slot>) {
    for slot in textures.drain(..) {
        slot.delete();
    }
}

/// The worker interns unchanged RGBA. Pixel identity can therefore retain an
/// upload even if a sign moved or libass reordered its disjoint output regions.
fn retained_regions(textures: &[Slot], rects: &[ass::Rect]) -> Vec<Option<usize>> {
    let mut used = vec![false; textures.len()];
    rects
        .iter()
        .map(|rect| {
            let found = textures.iter().enumerate().position(|(i, texture)| {
                !used[i]
                    && texture.width == rect.width
                    && texture.height == rect.height
                    && texture.pixels.as_ref().is_some_and(|p| Arc::ptr_eq(p, &rect.rgba))
            });
            if let Some(i) = found {
                used[i] = true;
            }
            found
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_media::player::text_style::TextStyle;
    use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};

    const BOTTOM: ass::AlignSet = ass::AlignSet { bottom: true, middle: false, top: false };
    const TOP_AND_BOTTOM: ass::AlignSet = ass::AlignSet { bottom: true, middle: false, top: true };

    fn dys_of(
        origin: ass::Origin,
        lift: bool,
        regions: &[(i32, i32)],
        aligns: ass::AlignSet,
    ) -> Vec<f32> {
        let mut dys = Vec::new();
        lift_dys(origin, lift, regions, aligns, &mut dys);
        dys
    }

    fn text(style: TextStyle) -> ass::Origin {
        ass::Origin::Text(style)
    }

    fn style(size: SubtitleSize, position: SubtitlePosition) -> TextStyle {
        TextStyle { size, position }
    }

    /// A one-line region whose bottom edge sits on the style's bottom anchor, `extra` px below
    /// the line (a descender).
    fn line_at(style: TextStyle, extra: i32) -> (i32, i32) {
        let pitch = (text_style::CAPTION_LINE_PITCH * style.scale()) as i32;
        let bottom = text_style::bottom_anchor_y(style.position) as i32 + extra;
        (bottom - pitch, pitch)
    }

    #[test]
    fn text_origin_renders_into_the_whole_screen() {
        let video = Viewport { x: 0, y: 140, width: 1920, height: 800 };
        let (viewport, storage) = frame_geometry(
            ass::Origin::Text(TextStyle::default()),
            video,
            (3840, 1600),
        );
        assert_eq!(viewport, Viewport { x: 0, y: 0, width: 1920, height: 1080 });
        assert_eq!(storage, (1920, 1080));
        let (viewport, storage) = frame_geometry(ass::Origin::Authored, video, (3840, 1600));
        assert_eq!((viewport, storage), (video, (3840, 1600)));
    }

    #[test]
    fn bottom_cues_rise_exactly_the_hud_lift_at_every_position_and_size() {
        for position in SubtitlePosition::LADDER {
            for size in SubtitleSize::LADDER {
                let style = style(size, position);
                // With and without descenders: the same motion, the descender does not jitter it.
                let plain = line_at(style, 0);
                let deep = line_at(style, 12);
                let dys = dys_of(ass::Origin::Text(style), true, &[plain, deep], BOTTOM);
                assert_eq!(dys, [-200.0, -200.0], "{position:?} {size:?}");
                assert_eq!(text_style::HUD_LIFT, 200.0);
            }
        }
    }

    #[test]
    fn an_authored_frame_is_unmoved_with_the_hud_up() {
        let style = TextStyle::default();
        let regions = [line_at(style, 0), (100, 48), line_at(style, 12)];
        for aligns in [BOTTOM, TOP_AND_BOTTOM, ass::AlignSet::default()] {
            assert_eq!(
                dys_of(ass::Origin::Authored, true, &regions, aligns),
                [0.0, 0.0, 0.0]
            );
        }
    }

    #[test]
    fn a_text_frame_is_unmoved_with_the_hud_down() {
        let style = TextStyle::default();
        assert_eq!(dys_of(text(style), false, &[line_at(style, 0)], BOTTOM), [0.0]);
    }

    #[test]
    fn a_four_line_block_moves_as_one() {
        let style = style(SubtitleSize::ExtraLarge, SubtitlePosition::High);
        let pitch = 72;
        let bottom = text_style::bottom_anchor_y(style.position) as i32;
        let region = (bottom - 4 * pitch, 4 * pitch);
        assert_eq!(dys_of(ass::Origin::Text(style), true, &[region], BOTTOM), [-200.0]);
    }

    #[test]
    fn nothing_moves_without_a_bottom_cue() {
        let style = TextStyle::default();
        let middle = ass::AlignSet { bottom: false, middle: true, top: false };
        let top = ass::AlignSet { bottom: false, middle: false, top: true };
        let r = [line_at(style, 0), (500, 48)];
        assert_eq!(dys_of(text(style), true, &r, middle), [0.0, 0.0]);
        assert_eq!(dys_of(text(style), true, &r, top), [0.0, 0.0]);
        assert_eq!(dys_of(text(style), true, &r, ass::AlignSet::default()), [0.0, 0.0]);
    }

    #[test]
    fn only_the_bottom_cue_moves_beside_a_top_one() {
        for position in SubtitlePosition::LADDER {
            for size in SubtitleSize::LADDER {
                let style = style(size, position);
                let top_cue = (100, 48);
                let bottom_cue = line_at(style, 12);
                assert_eq!(
                    dys_of(text(style), true, &[top_cue, bottom_cue], TOP_AND_BOTTOM),
                    [0.0, -200.0],
                    "{position:?} {size:?}"
                );
            }
        }
    }

    #[test]
    fn a_merged_top_and_bottom_region_is_not_lifted() {
        for position in SubtitlePosition::LADDER {
            for size in SubtitleSize::LADDER {
                let style = style(size, position);
                let bottom = text_style::bottom_anchor_y(style.position) as i32;
                let merged = (100, bottom - 100);
                assert_eq!(
                    dys_of(text(style), true, &[merged], TOP_AND_BOTTOM),
                    [0.0],
                    "{position:?} {size:?}"
                );
            }
        }
    }

    #[test]
    fn a_four_line_block_as_separate_regions_moves_together_beside_a_top_cue() {
        let style = style(SubtitleSize::ExtraLarge, SubtitlePosition::High);
        let pitch = (text_style::CAPTION_LINE_PITCH * style.scale()) as i32;
        let bottom = text_style::bottom_anchor_y(style.position) as i32;
        let mut regions = vec![(100, 48)];
        regions.extend((1..=4).rev().map(|n| (bottom - n * pitch, pitch)));
        assert_eq!(
            dys_of(text(style), true, &regions, TOP_AND_BOTTOM),
            [0.0, -200.0, -200.0, -200.0, -200.0]
        );
    }

    #[test]
    fn unknown_classes_never_lift_a_top_cue_but_lift_a_normal_bottom_block() {
        for position in SubtitlePosition::LADDER {
            for size in SubtitleSize::LADDER {
                let style = style(size, position);
                let dys = dys_of(
                    text(style),
                    true,
                    &[(100, 48), (500, 48), line_at(style, 12)],
                    UNKNOWN_ALIGNS,
                );
                assert_eq!(dys, [0.0, 0.0, -200.0], "{position:?} {size:?}");
            }
        }
    }

    #[test]
    fn a_frame_without_an_answer_does_not_borrow_another_frames_classes() {
        let mut cache = None;
        assert_eq!(aligns_for(&mut cache, 7, || Some(BOTTOM)), BOTTOM);
        // Cached for this serial: no ask.
        assert_eq!(aligns_for(&mut cache, 7, || panic!("asked again")), BOTTOM);
        // Another frame, mailbox busy or frame gone: unknown, not the bottom-only set.
        assert_eq!(aligns_for(&mut cache, 8, || None), UNKNOWN_ALIGNS);
        assert_eq!(aligns_for(&mut cache, 8, || Some(TOP_AND_BOTTOM)), TOP_AND_BOTTOM);
        // The top region does not move under the unknown set.
        let style = TextStyle::default();
        let regions = [(100, 48), line_at(style, 0)];
        assert_eq!(dys_of(text(style), true, &regions, UNKNOWN_ALIGNS), [0.0, -200.0]);
    }

    #[test]
    fn the_lifted_group_stays_on_the_screen() {
        let style = TextStyle::default();
        // A tall block whose top is 60 px down moves only 60.
        assert_eq!(dys_of(text(style), true, &[(60, 900)], BOTTOM), [-60.0]);
        assert_eq!(dys_of(text(style), true, &[], BOTTOM), Vec::<f32>::new());
    }

    #[test]
    fn moving_and_reordered_regions_retain_uploads_before_recycling_changed_ones() {
        let a: Arc<[u8]> = Arc::from([255, 0, 0, 255]);
        let b: Arc<[u8]> = Arc::from([0, 0, 255, 255]);
        let old = vec![slot_of(11, a.clone()), slot_of(12, b.clone())];
        let rect = |x, rgba| ass::Rect {
            x,
            y: 0,
            width: 1,
            height: 1,
            rgba,
        };
        let changed: Arc<[u8]> = Arc::from([0, 255, 0, 255]);
        let next = vec![rect(40, b), rect(90, changed), rect(120, a.clone())];
        assert_eq!(retained_regions(&old, &next), [Some(1), None, Some(0)]);
        assert_eq!(
            retained_regions(&old, &[rect(0, a.clone()), rect(20, a)]),
            [Some(0), None]
        );
        assert!(retained_regions(&old, &[]).is_empty());
    }

    fn slot_of(tex: u32, pixels: Arc<[u8]>) -> Slot {
        Slot {
            bufs: [BucketTex { tex, alloc_w: 64, alloc_h: 64, used_w: 1, used_h: 1 }, BucketTex::default()],
            shown: 0,
            width: 1,
            height: 1,
            pixels: Some(pixels),
        }
    }

    fn buf(tex: u32, alloc_w: i32, alloc_h: i32) -> BucketTex {
        BucketTex { tex, alloc_w, alloc_h, used_w: alloc_w, used_h: alloc_h }
    }

    fn slot_with(bufs: [BucketTex; 2], shown: usize) -> Slot {
        Slot { bufs, shown, ..Slot::default() }
    }

    #[test]
    fn the_spare_is_the_buffer_not_drawn_last() {
        let s = slot_with([buf(1, 64, 64), buf(2, 64, 64)], 0);
        assert_eq!(s.shown().tex, 1);
        assert_eq!(s.bufs[1 - s.shown].tex, 2);
        let s = slot_with([buf(1, 64, 64), buf(2, 64, 64)], 1);
        assert_eq!(s.shown().tex, 2);
        assert_eq!(s.bufs[1 - s.shown].tex, 1);
    }

    #[test]
    fn a_spare_that_fits_is_reused_and_a_missing_or_small_one_grows() {
        assert_eq!(spare_use(buf(2, 256, 128), 200, 100), SpareUse::Reuse);
        assert_eq!(spare_use(buf(2, 256, 128), 256, 128), SpareUse::Reuse);
        assert_eq!(spare_use(BucketTex::default(), 10, 10), SpareUse::Grow);
        assert_eq!(spare_use(buf(2, 64, 64), 65, 10), SpareUse::Grow);
        assert_eq!(spare_use(buf(2, 64, 64), 10, 65), SpareUse::Grow);
    }

    #[test]
    fn a_spare_over_four_times_the_needed_bucket_is_reset() {
        // Needed bucket 64x64 = 4096; 128x128 = 16384 is exactly 4x (kept), 192x128 is over.
        assert_eq!(spare_use(buf(2, 128, 128), 10, 10), SpareUse::Reuse);
        assert_eq!(spare_use(buf(2, 192, 128), 10, 10), SpareUse::Reset);
        // Per-axis growth mixes shapes: a wide-then-tall region leaves a big square behind.
        assert_eq!(spare_use(buf(2, 1920, 1088), 1900, 70), SpareUse::Reset);
        assert_eq!(spare_use(buf(2, 1920, 1088), 1900, 1000), SpareUse::Reuse);
    }

    #[test]
    fn the_recycled_slot_is_the_one_whose_spare_fits() {
        let small = slot_with([buf(1, 64, 64), buf(2, 64, 64)], 0);
        let big = slot_with([buf(3, 512, 128), buf(4, 512, 128)], 0);
        let none = Slot::default();
        let pick = |slots: &[Option<Slot>], w, h| {
            pick_slot(slots.iter().map(Option::as_ref), w, h)
        };
        let slots = [Some(small), Some(big), Some(none)];
        assert_eq!(pick(&slots, 500, 100), Some(1));
        assert_eq!(pick(&slots, 50, 50), Some(0));
        // Nothing fits any spare: every slot would grow, so the first is taken.
        assert_eq!(pick(&slots, 600, 100), Some(0));
        assert_eq!(pick(&[None, None], 10, 10), None);
        let taken = [None, Some(Slot::default())];
        assert_eq!(pick(&taken, 10, 10), Some(1));
    }

    #[test]
    fn reports_allocated_bytes_and_every_texture_of_every_buffer() {
        let s = slot_with([buf(1, 128, 64), buf(2, 64, 64)], 0);
        assert_eq!(s.texture_count(), 2);
        assert_eq!(s.allocated_bytes(), (128 * 64 + 64 * 64) * 4);
        // Used is smaller than allocated: the report follows the allocation.
        let mut used = buf(1, 128, 64);
        used.used_w = 3;
        used.used_h = 3;
        let s = slot_with([used, BucketTex::default()], 0);
        assert_eq!(s.texture_count(), 1);
        assert_eq!(s.allocated_bytes(), 128 * 64 * 4);
        assert_eq!(Slot::default().texture_count(), 0);
        assert_eq!(Slot::default().allocated_bytes(), 0);
    }
}
