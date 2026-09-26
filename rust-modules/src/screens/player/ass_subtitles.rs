//! Player-owned ASS texture cache. Native parsing/rasterization is the worker's job;
//! the frame thread submits its clock and uploads only a changed completed image.
//! Authored placement is retained even while the transport HUD is visible.
use crate::player::{ass, ass_source, sidecar};
use crate::ui::{
    consts::{SCR_H, SCR_W},
    Painter, Rect,
};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct AssSubtitles {
    frame: Option<Arc<ass::Frame>>,
    source: u64,
    clock: ass_source::Clock,
    textures: Vec<CachedTexture>,
    uploaded: u64,
}

impl AssSubtitles {
    pub(crate) fn update(&mut self, ps: &crate::route::PlaybackSession, now: u32) {
        let source = if crate::route::is_transcoding(ps) || crate::player::loading(ps) {
            None
        } else {
            sidecar::ass_source(false)
                .or_else(|| ass_source::selected(crate::player::desired_sub_idx()))
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
            crate::player::playpos_ns(),
            now,
            crate::player::is_playing(ps),
        );
        let clock = clock.saturating_sub(crate::player::subtitle_offset_ms());
        let (coded_w, coded_h) = crate::player::video_raster();
        if let Some(frame) =
            ass::request(source, clock, SCR_W as i32, SCR_H as i32, coded_w, coded_h)
        {
            self.frame = Some(frame);
        }
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        self.frame.as_ref().map_or(0, |f| f.serial)
    }

    pub(crate) fn draw(&mut self) {
        let Some(frame) = &self.frame else {
            if self.source == 0 {
                clear_textures(&mut self.textures);
                self.uploaded = 0;
            }
            return;
        };
        if self.uploaded != frame.serial {
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
                    let id = crate::gfx::upload_rgba(
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
                crate::gfx::delete_tex(unused.id);
            }
            self.textures = next.into_iter().flatten().collect();
            self.uploaded = frame.serial;
        }
        let ink = crate::ui::player_hud::subtitle_ink();
        for (texture, rect) in self.textures.iter().zip(&frame.rects) {
            Painter::root().tex(
                texture.id,
                Rect::new(
                    rect.x as f32,
                    rect.y as f32,
                    rect.width as f32,
                    rect.height as f32,
                ),
                0.0,
                ink,
            );
        }
    }

    pub(crate) fn error(&self) -> Option<&'static str> {
        self.frame.as_ref().and_then(|f| f.error)
    }

    pub(crate) fn release(&mut self) {
        clear_textures(&mut self.textures);
        self.uploaded = 0;
        self.frame = None;
        self.source = 0;
        ass::clear();
    }

    pub(crate) fn render_report(&self) -> crate::ui::frame::RenderReport {
        crate::ui::frame::RenderReport {
            textures: self.textures.iter().filter(|t| t.id != 0).count() as u32,
            bytes: self.textures.iter().map(|t| t.pixels.len()).sum(),
        }
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
        crate::gfx::delete_tex(texture.id);
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
