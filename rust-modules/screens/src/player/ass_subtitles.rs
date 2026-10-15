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
use std::sync::Arc;

#[derive(Default)]
pub struct AssSubtitles {
    frame: Option<Arc<ass::Frame>>,
    source: u64,
    clock: ass_source::Clock,
    textures: Vec<CachedTexture>,
    uploaded: u64,
    viewport: Viewport,
    origin: ass::Origin,
    /// The alignment classes of the cues in `aligns_serial`'s frame (Text origin only).
    aligns: ass::AlignSet,
    aligns_serial: u64,
}

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
        self.origin = source.origin;
        if self.viewport != viewport {
            self.viewport = viewport;
            self.frame = None;
        }
        if let Some(frame) =
            ass::request(source, clock, viewport.width, viewport.height, coded_w, coded_h)
        {
            self.frame = Some(frame);
        }
    }

    /// The classes of the cues in the frame about to be drawn. The mailbox may be busy this tick;
    /// then the last answer stands, and before any answer every cue is taken to be a bottom one,
    /// the normal case.
    fn refresh_aligns(&mut self, serial: u64) {
        if self.aligns_serial == serial {
            return;
        }
        match ass::published_aligns(serial) {
            Some(set) => {
                self.aligns = set;
                self.aligns_serial = serial;
            }
            None if self.aligns_serial == 0 => {
                self.aligns = ass::AlignSet { bottom: true, middle: false, top: false };
            }
            None => {}
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
        let serial = frame.serial;
        if lift && self.origin.is_text() {
            self.refresh_aligns(serial);
        }
        let spans: Vec<(i32, i32)> = frame.rects.iter().map(|r| (r.y, r.height)).collect();
        let dys = lift_dys(self.origin, lift, &spans, self.aligns);
        let ink = plx_appkit::player_hud::subtitle_ink();
        for (i, (texture, rect)) in self.textures.iter().zip(&frame.rects).enumerate() {
            let dy = dys[i];
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
/// - every active cue bottom-aligned: all regions move by the fixed `HUD_LIFT`;
/// - no bottom cue: nothing moves;
/// - mixed: a region moves when its bottom edge is at or below `anchor - pitch / 2`, unless such a
///   region also starts above the frame's midline (a merged top+bottom region), in which case
///   nothing moves, erring towards not lifting a top cue.
///
/// The moved group is clamped once so its top stays on the screen.
fn lift_dys(
    origin: ass::Origin,
    lift: bool,
    regions: &[(i32, i32)],
    aligns: ass::AlignSet,
) -> Vec<f32> {
    let mut dys = vec![0.0; regions.len()];
    let ass::Origin::Text(style) = origin else { return dys };
    if !lift || !aligns.bottom {
        return dys;
    }
    let moves: Vec<bool> = if aligns.middle || aligns.top {
        let band = text_style::bottom_anchor_y(style.position)
            - text_style::CAPTION_LINE_PITCH * style.scale() / 2.0;
        let in_band: Vec<bool> =
            regions.iter().map(|&(y, h)| (y + h) as f32 >= band).collect();
        let merged = regions
            .iter()
            .zip(&in_band)
            .any(|(&(y, _), &hit)| hit && (y as f32) < text_style::SCR_H / 2.0);
        if merged {
            return dys;
        }
        in_band
    } else {
        vec![true; regions.len()]
    };
    let top = regions
        .iter()
        .zip(&moves)
        .filter(|(_, &m)| m)
        .map(|(&(y, _), _)| y)
        .min();
    let Some(top) = top else { return dys };
    let lift = text_style::HUD_LIFT.min(top.max(0) as f32);
    for (dy, &m) in dys.iter_mut().zip(&moves) {
        if m {
            *dy = -lift;
        }
    }
    dys
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
                let dys = lift_dys(ass::Origin::Text(style), true, &[plain, deep], BOTTOM);
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
                lift_dys(ass::Origin::Authored, true, &regions, aligns),
                [0.0, 0.0, 0.0]
            );
        }
    }

    #[test]
    fn a_text_frame_is_unmoved_with_the_hud_down() {
        let style = TextStyle::default();
        assert_eq!(lift_dys(text(style), false, &[line_at(style, 0)], BOTTOM), [0.0]);
    }

    #[test]
    fn a_four_line_block_moves_as_one() {
        let style = style(SubtitleSize::ExtraLarge, SubtitlePosition::High);
        let pitch = 72;
        let bottom = text_style::bottom_anchor_y(style.position) as i32;
        let region = (bottom - 4 * pitch, 4 * pitch);
        assert_eq!(lift_dys(ass::Origin::Text(style), true, &[region], BOTTOM), [-200.0]);
    }

    #[test]
    fn nothing_moves_without_a_bottom_cue() {
        let style = TextStyle::default();
        let middle = ass::AlignSet { bottom: false, middle: true, top: false };
        let top = ass::AlignSet { bottom: false, middle: false, top: true };
        let r = [line_at(style, 0), (500, 48)];
        assert_eq!(lift_dys(text(style), true, &r, middle), [0.0, 0.0]);
        assert_eq!(lift_dys(text(style), true, &r, top), [0.0, 0.0]);
        assert_eq!(lift_dys(text(style), true, &r, ass::AlignSet::default()), [0.0, 0.0]);
    }

    #[test]
    fn only_the_bottom_cue_moves_beside_a_top_one() {
        let style = TextStyle::default();
        let top_cue = (100, 48);
        let bottom_cue = line_at(style, 12);
        assert_eq!(lift_dys(text(style), true, &[top_cue, bottom_cue], TOP_AND_BOTTOM), [0.0, -200.0]);
    }

    #[test]
    fn a_merged_top_and_bottom_region_is_not_lifted() {
        let style = TextStyle::default();
        let bottom = text_style::bottom_anchor_y(style.position) as i32;
        let merged = (100, bottom - 100);
        assert_eq!(lift_dys(text(style), true, &[merged], TOP_AND_BOTTOM), [0.0]);
    }

    #[test]
    fn the_lifted_group_stays_on_the_screen() {
        let style = TextStyle::default();
        // A tall block whose top is 60 px down moves only 60.
        assert_eq!(lift_dys(text(style), true, &[(60, 900)], BOTTOM), [-60.0]);
        assert_eq!(lift_dys(text(style), true, &[], BOTTOM), Vec::<f32>::new());
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
