//! `Popover` — the ONE open/appear choreography every modal panel shares (track menu, Info card,
//! Chapters strip, profile menu): an OPEN flag + a critically-damped 0→1 appear spring driving a
//! fade + slide-into-place, with an optional full-screen scrim. Each panel used to hand-wire its
//! own `static OPEN + APPEAR` pair, so any motion change was a four-file edit.
use crate::ui::{theme, Painter, Rect, Spring};

/// Stiffness of the appear spring — the panels' shared open-motion constant, and the stiffness every
/// other fade-into-place in the UI matches (the tab capsules' alpha, [`crate::ui::widgets::TabStrip`]).
/// `pub(crate)` for exactly that reason: a fade that is not a panel still belongs to the same family,
/// and re-typing 300 somewhere else is how two "shared" motions drift apart.
pub(crate) const K_APPEAR: f32 = 300.0;

pub(crate) struct Popover {
    open: bool,
    appear: Spring,
}
impl Popover {
    pub(crate) const fn new() -> Self {
        Popover { open: false, appear: Spring::at(0.0) }
    }
    /// (re)open: restart the fade+slide from 0.
    pub(crate) fn open(&mut self) {
        self.appear = Spring::at(0.0);
        self.open = true;
    }
    /// Close is an instant hide (no exit animation — matches the panels' historical behavior).
    pub(crate) fn close(&mut self) {
        self.open = false;
    }
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
    /// step the appear spring; no-op when closed.
    pub(crate) fn update(&mut self, dt: f32) {
        if self.open {
            self.appear.step(1.0, K_APPEAR, dt);
        }
    }
    /// the appear fraction 0..1, for anything else keyed to the open motion.
    pub(crate) fn appear(&self) -> f32 {
        self.appear.pos.clamp(0.0, 1.0)
    }
    /// Draw the optional modal scrim (peak alpha `scrim_a`; 0 = none) and return the content
    /// painter: faded by the appear state and sliding from `rise` px below (+, rises up into
    /// place) or above (−, drops down) to its rest position. The scrim draws on its OWN root
    /// painter so the full-screen dim doesn't compound with the content fade.
    ///
    /// Both roots ride [`nav::page_alpha`](crate::ui::nav::page_alpha), because a popover is drawn
    /// ON a page and a route change replaces the page: when the screen underneath dips to the app
    /// ground, a panel left sitting at full strength over it is the one thing on the panel that
    /// says the transition is not happening. It is one multiply into a cascade every primitive
    /// already goes through, and it is 1.0 whenever no route change is in flight — which is every
    /// frame of the in-player panels, since playback has no page transition.
    pub(crate) fn painter(&self, scrim_a: f32, rise: f32) -> Painter {
        let a = self.appear();
        let page = crate::ui::nav::page_alpha();
        if scrim_a > 0.0 {
            let dim = theme::scrim_black(scrim_a * a * page);
            Painter::root().rect(Rect::FULL, 0.0, dim, dim, 0.0);
        }
        Painter::root().alpha(a * page).translate(0.0, rise * (1.0 - a))
    }
}
