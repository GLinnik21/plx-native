//! Player-owned ASS texture cache. Native parsing/rasterization is the worker's job;
//! the frame thread submits its clock and uploads only a changed completed image.
//! Authored placement is retained even while the transport HUD is visible; a text subtitle's
//! bottom-anchored regions rise clear of it (plan `libass-text-subs` §2.4).
use plx_media::player::{
    ass, ass_source, sidecar,
    text_style::{self, HudBand},
    video_geometry::Viewport,
};
use plx_ui::{
    consts::{SCR_H, SCR_W},
    Painter, Rect,
};
use std::sync::Arc;

#[derive(Default)]
pub struct AssSubtitles {
    frame: Option<Arc<ass::Frame>>,
    source: u64,
    clock: ass_source::Clock,
    textures: Vec<CachedTexture>,
    uploaded: u64,
    viewport: Viewport,
    /// The origin `frame` was requested under. The source's current origin can already differ
    /// (a restyle bumps the revision when its worker publishes; the libass worker's re-render
    /// follows), and the lift is
    /// decided by the frame on screen (plan `libass-text-subs` §2.4).
    frame_origin: ass::Origin,
    /// The alignment classes of the cues in one frame, by its serial (Text origin only).
    aligns: Option<(u64, ass::AlignSet)>,
    /// Reused each draw so the 60 fps path allocates nothing.
    spans: Vec<(i32, i32)>,
    dys: Vec<f32>,
}

/// Every class present: the rule for a frame whose own classes are not known. It never lifts a
/// top or middle cue and still lifts a normal bottom block by the band's full `lift`.
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

    /// Present a frame with `serial` and no regions, as the worker's answer to a cue change would:
    /// what the player's frame-fingerprint test moves.
    #[cfg(test)]
    pub(super) fn set_frame_serial_for_test(&mut self, serial: u64) {
        self.frame = Some(Arc::new(ass::Frame {
            source_id: self.source,
            serial,
            width: 0,
            height: 0,
            rects: Vec::new(),
            error: None,
        }));
    }

    /// `lift` is the transport HUD being up, and `band` the HUD's, as its layout reports it: the
    /// distance a bottom region moves is the band's `lift`. It moves a text subtitle's bottom regions
    /// and nothing else: an authored script keeps its own placement. `model` is the frame's HUD
    /// snapshot, which the caption ink's tone is read from.
    pub fn draw(&mut self, lift: bool, band: HudBand, model: &plx_appkit::player_hud::model::HudModel) {
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
                        let previous = old.iter_mut().find_map(Option::take);
                        plx_media::player::ass_perf::note_upload(rect.rgba.len());
                        let id = plx_gfx::gfx::upload_rgba(
                            previous.as_ref().map_or(0, |t| t.id),
                            rect.width,
                            rect.height,
                            rect.rgba.as_ptr(),
                        );
                        *slot = Some(CachedTexture {
                            id,
                            width: rect.width,
                            height: rect.height,
                            pixels: rect.rgba.clone(),
                        });
                    }
                }
                for unused in old.into_iter().flatten() {
                    plx_gfx::gfx::delete_tex(unused.id);
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
        lift_dys(origin, lift, band, &self.spans, aligns, &mut self.dys);
        let ink = plx_appkit::player_hud::subtitle_ink(model);
        for (i, (texture, rect)) in self.textures.iter().zip(&frame.rects).enumerate() {
            let dy = self.dys[i];
            Painter::root().tex(
                texture.id,
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
            textures: self.textures.iter().filter(|t| t.id != 0).count() as u32,
            bytes: self.textures.iter().map(|t| t.pixels.len()).sum(),
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
/// - every active cue bottom-aligned: all regions move by the band's fixed `lift`;
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
    band: HudBand,
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
        let bottom_edge = text_style::bottom_anchor_y(style.position) - pitch / 2.0;
        let mut merged = false;
        for (dy, &(y, h)) in dys.iter_mut().zip(regions) {
            if (y + h) as f32 >= bottom_edge {
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
    let lift = band.lift.min(top.max(0) as f32);
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

struct CachedTexture {
    id: u32,
    width: i32,
    height: i32,
    pixels: Arc<[u8]>,
}

fn clear_textures(textures: &mut Vec<CachedTexture>) {
    for texture in textures.drain(..) {
        plx_gfx::gfx::delete_tex(texture.id);
    }
}

/// The worker interns unchanged RGBA. Pixel identity can therefore retain an
/// upload even if a sign moved or libass reordered its disjoint output regions.
fn retained_regions(textures: &[CachedTexture], rects: &[ass::Rect]) -> Vec<Option<usize>> {
    let mut used = vec![false; textures.len()];
    rects
        .iter()
        .map(|rect| {
            let found = textures.iter().enumerate().position(|(i, texture)| {
                !used[i]
                    && texture.width == rect.width
                    && texture.height == rect.height
                    && Arc::ptr_eq(&texture.pixels, &rect.rgba)
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

    fn dys_in(
        band: HudBand,
        origin: ass::Origin,
        lift: bool,
        regions: &[(i32, i32)],
        aligns: ass::AlignSet,
    ) -> Vec<f32> {
        let mut dys = Vec::new();
        lift_dys(origin, lift, band, regions, aligns, &mut dys);
        dys
    }

    fn dys_of(
        origin: ass::Origin,
        lift: bool,
        regions: &[(i32, i32)],
        aligns: ass::AlignSet,
    ) -> Vec<f32> {
        dys_in(HudBand::CLASSIC, origin, lift, regions, aligns)
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
                assert_eq!(HudBand::CLASSIC.lift, 200.0);
            }
        }
    }

    /// A band that is not classic's lifts a bottom block by ITS lift: exactly the band's
    /// difference more than classic's, at every position and size.
    #[test]
    fn bottom_cues_rise_exactly_the_bands_lift_for_a_non_classic_band() {
        let band = HudBand::from_controls_top(700.0);
        let shift = HudBand::CLASSIC.controls_top_y - band.controls_top_y;
        for position in SubtitlePosition::LADDER {
            for size in SubtitleSize::LADDER {
                let style = style(size, position);
                let regions = [line_at(style, 0), line_at(style, 12)];
                let classic = dys_in(HudBand::CLASSIC, text(style), true, &regions, BOTTOM);
                let moved = dys_in(band, text(style), true, &regions, BOTTOM);
                assert_eq!(moved, [-band.lift, -band.lift], "{position:?} {size:?}");
                assert_eq!(classic[0] - moved[0], shift, "{position:?} {size:?}");
            }
        }
    }

    /// The largest block at the highest position, three lines, as libass places it, lifted by the
    /// band's `lift`: where its top starts and where it ends up.
    fn lifted_libass_top(band: HudBand) -> (f32, f32) {
        let style = style(SubtitleSize::Huge, SubtitlePosition::High);
        let pitch = text_style::CAPTION_LINE_PITCH * style.scale();
        let height = text_style::CAPTION_ANCHOR_LINES as f32 * pitch;
        let top = text_style::bottom_anchor_y(style.position) - height;
        let dys = dys_in(band, text(style), true, &[(top as i32, height as i32)], BOTTOM);
        (top, top + dys[0])
    }

    /// `min_controls_top_y` through the libass lift. Two claims, kept apart:
    /// - the lift IS the band's (the block rises by exactly `band.lift`, not a fixed 200), which is
    ///   what makes the largest block's lifted top depend on the band at the bound;
    /// - the block is then inside `SAFE` at the bound and one pixel past it (the libass block sits
    ///   `INK_TRIM` below the bespoke one, so the bound is set by the stricter bespoke draw), and a
    ///   band far past the bound clamps the lift so the block stays on the screen.
    #[test]
    fn the_libass_lift_keeps_the_largest_block_on_screen_at_the_bound() {
        let bound = plx_appkit::player_hud::min_controls_top_y();
        for band in [HudBand::from_controls_top(bound), HudBand::from_controls_top(bound - 1.0)] {
            let (top, lifted) = lifted_libass_top(band);
            assert_eq!(lifted, top - band.lift, "{band:?}: the rise is the band's lift");
            assert_ne!(band.lift, HudBand::CLASSIC.lift);
            assert!(lifted >= plx_ui::consts::SAFE.y, "{band:?}: lifted top {lifted}");
        }
        // The clamp, not the bound: far past it the lift is cut to keep the block on the screen.
        let (_, lifted) = lifted_libass_top(HudBand::from_controls_top(300.0));
        assert!(lifted >= 0.0);
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
                // The middle cue sits more than half a Huge pitch (43 px) above the High block's
                // line, so the lift cannot take it for one of that block's lines.
                let dys = dys_of(
                    text(style),
                    true,
                    &[(100, 48), (450, 48), line_at(style, 12)],
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
        let old = vec![
            CachedTexture {
                id: 11,
                width: 1,
                height: 1,
                pixels: a.clone(),
            },
            CachedTexture {
                id: 12,
                width: 1,
                height: 1,
                pixels: b.clone(),
            },
        ];
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
}
