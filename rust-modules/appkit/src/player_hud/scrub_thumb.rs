//! The scrubbing thumbnail (FinNative): the picture of the moment being scrubbed to, above the
//! playhead, from the server's trickplay sheets (`scrub_thumbs.rs`). It is drawn as upstream's scrub
//! preview card ([`crate::scrub_preview`]: its size, place, shadow and rim), over the bar the layout
//! reports ([`HudGeometry::playbar`](super::geometry::HudGeometry::playbar)).
use super::*;
use crate::scrub_preview::{card_rect, card_size, draw};
use std::sync::atomic::Ordering::Relaxed;

/// **The scrubbing thumbnail on screen, and the worker that cuts it** — a RENDER resource of the
/// player's instance (`PlayerScreen::render`), like [`SubtitleBitmaps`]: the GL name is OWNED, and
/// [`Self::release`] deletes it and forgets the item when the screen is unmounted.
pub struct ScrubThumb {
    source: std::sync::Arc<crate::scrub_thumbs::Trickplays<crate::scrub_thumbs::ServerSource>>,
    tex: c_uint,
    /// The picture in `tex` (`None` until one arrives for this scrub), and its size.
    shown: Option<(u32, u32, u32)>,
    scrubbing: bool,
}

impl Default for ScrubThumb {
    fn default() -> Self {
        Self {
            source: crate::scrub_thumbs::Trickplays::new(crate::scrub_thumbs::ServerSource),
            tex: 0,
            shown: None,
            scrubbing: false,
        }
    }
}

impl ScrubThumb {
    pub fn release(&mut self) {
        delete_tex(self.tex);
        self.tex = 0;
        self.shown = None;
        self.scrubbing = false;
        self.source.forget();
    }
}

/// Where the card of a `w`×`h` picture sits for a scrub at `frac` of `bar`: the scrub preview's own
/// card ([`card_size`], [`card_rect`]) over the playhead.
pub(super) fn scrub_thumb_rect(frac: f64, w: u32, h: u32, bar: Rect) -> Rect {
    let knob_x = bar.x + (bar.w as f64 * frac.clamp(0.0, 1.0)) as f32;
    card_rect(knob_x, bar.x, bar.w, bar.y, card_size(w as f32, h as f32).0)
}

/// The picture of the moment being scrubbed to, above the playhead, while the viewer scrubs with
/// the transport up. Asks the worker for the scrub position every frame (a want for the picture
/// already shown is free) and uploads a newly cut picture into its one texture. When scrubbing
/// ends the worker drops its decoded sheet. Draws nothing for an item without thumbnails, and
/// nothing until the first picture of a scrub arrives. Main-thread only (GL).
pub fn draw_scrub_thumb(t: &mut ScrubThumb, ps: &plx_media::route::PlaybackSession, transport: bool, bar: Rect) {
    let scrub = plx_media::player::TX.scrub_ns.load(Relaxed);
    if !transport || scrub < 0 {
        if t.scrubbing {
            t.scrubbing = false;
            t.shown = None;
            t.source.release();
        }
        return;
    }
    t.scrubbing = true;
    let rk = plx_media::route::cur_rk(ps);
    let sid = plx_media::route::item_sid(plx_media::route::cur_sid(ps));
    t.source.want(sid, &rk, plx_media::route::cur_part_id(ps), scrub / 1_000_000);
    if let Some(tile) = t.source.take_ready() {
        t.tex = upload_rgba(t.tex, tile.width as c_int, tile.height as c_int, tile.rgba.as_ptr());
        t.shown = Some((tile.index, tile.width, tile.height));
    }
    let (Some((_, w, h)), true) = (t.shown, t.tex != 0) else { return };
    let dur = plx_media::player::duration_ns();
    let frac = if dur > 0 { scrub as f64 / dur as f64 } else { 0.0 };
    let r = scrub_thumb_rect(frac, w, h, bar);
    draw(Painter::root(), r, t.tex, w as f32, h as f32, None);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scrub_thumbnail_is_the_scrub_preview_card_over_the_playhead() {
        use super::scrub_thumb_rect;
        use crate::scrub_preview::{card_rect, card_size};
        let bar = super::super::audit_geometry().playbar;
        let mid = scrub_thumb_rect(0.5, 320, 180, bar);
        assert_eq!(mid, card_rect(bar.x + bar.w * 0.5, bar.x, bar.w, bar.y, card_size(320.0, 180.0).0));
        assert_eq!(mid.x + mid.w * 0.5, bar.x + bar.w * 0.5, "centred on the playhead");
        assert_eq!(scrub_thumb_rect(0.0, 320, 134, bar).x, bar.x, "held inside the bar's start");
        let end = scrub_thumb_rect(1.0, 320, 134, bar);
        assert_eq!(end.x + end.w, bar.x + bar.w, "and its end");
    }
}
