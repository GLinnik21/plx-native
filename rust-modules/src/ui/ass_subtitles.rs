//! Player-owned ASS texture cache. Native parsing/rasterization is the worker's job;
//! the frame thread submits its clock and uploads only a changed completed image.
//! Authored placement is retained even while the transport HUD is visible.
use super::{
    consts::{SCR_H, SCR_W},
    Painter, Rect,
};
use crate::player::{ass, ass_source, sidecar};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct AssSubtitles {
    frame: Option<Arc<ass::Frame>>,
    source: u64,
    clock: ass_source::Clock,
    texture: u32,
    uploaded: u64,
    bytes: usize,
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
        if let Some(frame) = ass::request(source, clock, SCR_W as i32, SCR_H as i32, coded_w, coded_h) {
            self.frame = Some(frame);
        }
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        self.frame.as_ref().map_or(0, |f| f.serial)
    }

    pub(crate) fn draw(&mut self) {
        let Some(frame) = &self.frame else {
            if self.source == 0 {
                crate::gfx::delete_tex(self.texture);
                self.texture = 0;
                self.uploaded = 0;
                self.bytes = 0;
            }
            return;
        };
        let Some(rect) = &frame.rect else {
            return;
        };
        if self.uploaded != frame.serial {
            self.texture =
                crate::gfx::upload_rgba(self.texture, rect.width, rect.height, rect.rgba.as_ptr());
            self.uploaded = frame.serial;
            self.bytes = rect.rgba.len();
        }
        let ink = super::player_hud::subtitle_ink();
        Painter::root().tex(
            self.texture,
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

    pub(crate) fn error(&self) -> Option<&'static str> {
        self.frame.as_ref().and_then(|f| f.error)
    }

    pub(crate) fn release(&mut self) {
        crate::gfx::delete_tex(self.texture);
        self.texture = 0;
        self.uploaded = 0;
        self.bytes = 0;
        self.frame = None;
        self.source = 0;
        ass::clear();
    }

    pub(crate) fn render_report(&self) -> super::frame::RenderReport {
        super::frame::RenderReport {
            textures: u32::from(self.texture != 0),
            bytes: self.bytes,
        }
    }
}
