//! The lab upload's read-out: **"Uploading diagnostics…" → "Diagnostics uploaded" or a reason**.
//!
//! Lab builds only (`crate::lab`), and it is the only thing this feature ever puts on screen. Its
//! whole job is to answer, in a rented Cloud Test Lab hour with no console and no log, the one
//! question a tester has after pressing the button: *did anything happen*. A silent trigger and a
//! trigger that is not delivered at all look identical, and one of those is a bug in this feature
//! while the other is the colour-button question (`docs/lab-diagnostics.md` §7) — so the read-out
//! appears the moment the press is TAKEN, before the network is involved, and then changes.
//!
//! # Where it sits, and why not where the other read-out sits
//!
//! Top RIGHT. `ui::stats` owns the top left, and the two are shown together on purpose during a
//! lab session: the panel is the state, the toast is whether the state got out. It is inside the
//! overscan frame (`consts::MARGIN_X`) for the same reason `stats` is — a lab set is one nobody
//! here can measure, so nothing may rely on the panel showing every pixel.
//!
//! It takes NO KEYS: it is not a route, not a modal and has no dismiss. Every key keeps doing what
//! it did, which matters when the upload is triggered from the playback failure read-out — the
//! screen a tester most wants a snapshot of, and one whose own key arm swallows everything.
use crate::lab::upload;
use crate::ui::consts::{MARGIN_X, SCR_W};
use crate::ui::label::Label;
use crate::ui::{theme, Painter, Rect};
use std::ffi::CString;

const W: f32 = 560.0;
const H: f32 = 104.0;
const PAD: f32 = 24.0;
/// Clear of the top edge by the same margin the safe area uses on the sides.
const TOP: f32 = 60.0;

fn frame() -> Rect {
    Rect::new(SCR_W - MARGIN_X - W, TOP, W, H)
}

/// Retire an expired toast. One call per frame from the update block; a no-op when nothing is up.
pub(crate) fn update(now: u32) {
    upload::update(now);
}

pub(crate) fn draw() {
    if !upload::showing() {
        return;
    }
    let (title, ink) = match upload::phase() {
        upload::PHASE_SENDING => ("Uploading diagnostics…", theme::TEXT_PRIMARY),
        upload::PHASE_OK => ("Diagnostics uploaded", theme::TEXT_PRIMARY),
        _ => ("Diagnostics upload failed", theme::DANGER),
    };
    let r = frame();
    let p = Painter::root();
    // Its own opaque ground, like `stats`: on the player route the UI plane is cleared fully
    // transparent, so a scrim would leave the picture showing through the text — and this read-out
    // has to survive being looked at over a bright frame of video.
    p.rect(r, 20.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
    if let Ok(cs) = CString::new(title) {
        Label::new(cs.as_ptr(), theme::size::BODY, ink)
            .bold()
            .draw(p, Rect::new(r.x + PAD, r.y + theme::space::SM, r.w - 2.0 * PAD, 34.0));
    }
    // The second line is the only variable text: a byte count, or why it failed. Never a URL, a
    // host or a session secret — `lab::upload::send` composes it and this draws whatever it says.
    let detail = upload::detail();
    if !detail.is_empty() {
        if let Ok(cs) = CString::new(detail) {
            Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
                .draw(p, Rect::new(r.x + PAD, r.y + 56.0, r.w - 2.0 * PAD, 28.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::SAFE;

    /// The read-out must sit inside the overscan frame: a lab television is one nobody here can
    /// look at, so nothing about this may depend on the panel showing its outermost pixels.
    #[test]
    fn the_toast_stays_inside_the_safe_area() {
        let r = frame();
        assert!(r.x >= SAFE.x, "left edge");
        assert!(r.x + r.w <= SAFE.x + SAFE.w, "right edge");
        assert!(r.y >= SAFE.y, "top edge");
    }

    /// …and clear of the top-left panel it is deliberately shown beside.
    #[test]
    fn it_does_not_overlap_the_stats_read_out() {
        let mut rects = Vec::new();
        crate::ui::stats::overscan_rects(&mut rects);
        let r = frame();
        for (name, s) in rects {
            assert!(r.x >= s.x + s.w || s.x >= r.x + r.w, "overlaps {name}");
        }
    }
}
