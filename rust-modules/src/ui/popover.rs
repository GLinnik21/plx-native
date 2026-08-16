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

/// How many panels are open right now, across the whole app — see [`any_open`].
static mut OPEN_COUNT: u32 = 0;

/// Is ANY modal panel up? The one question a screen's CHROME has to ask, and the reason it is a
/// counter here rather than a bool threaded through three `draw_tab_row` call sites: the panels
/// that cover the top bar belong to four different modules and two of them are ROUTES
/// (`account_menu`, `item_menu`), so no screen can enumerate the ones drawn over it. Every panel in
/// the app goes through [`Popover::open`]/[`Popover::close`], so registering there is exact, costs
/// nothing, and a panel added tomorrow is counted without touching this.
///
/// Its user is the glass tab bar, and what it buys is stated at that call site: a popover FREEZES
/// the page under it — that is the premise `open`'s own doc rests on — so a bar re-snapshotting the
/// backdrop every frame while a menu is up is paying a full blur chain, under a modal scrim, for a
/// picture that cannot have changed.
pub(crate) fn any_open() -> bool {
    unsafe { *std::ptr::addr_of!(OPEN_COUNT) > 0 }
}

pub(crate) struct Popover {
    open: bool,
    appear: Spring,
    /// The `rise` the last [`painter`](Self::painter) was asked for, so [`panel`](Self::panel) can
    /// work out how far this frame is from the panel's resting position. Stashed rather than passed
    /// again because the two calls are one statement — every caller does `panel(painter(…), …)` —
    /// and a second copy of the number is a second place for it to be wrong. A `Cell` because both
    /// of those take `&self`, and this is main-thread draw state like everything else here.
    rise: std::cell::Cell<f32>,
}
impl Popover {
    pub(crate) const fn new() -> Self {
        Popover { open: false, appear: Spring::at(0.0), rise: std::cell::Cell::new(0.0) }
    }
    /// (re)open: restart the fade+slide from 0.
    ///
    /// Also drops any cached backdrop-blur snapshot, which is the WHOLE invalidation rule for
    /// [`panel`](Self::panel): a popover opens over a page that is not moving and closes before it
    /// moves again, so one capture per opening is exactly enough. Anything that starts animating
    /// underneath an open panel owes `gfx::blur_invalidate` a call of its own.
    pub(crate) fn open(&mut self) {
        self.appear = Spring::at(0.0);
        // Both transitions are guarded on the flag actually CHANGING, not on the call: `open` is a
        // re-open (a second press on the same chip restarts the motion) and `close` is called
        // defensively by arms that do not know whether anything was up. An unguarded pair leaks the
        // count in both directions, and a leaked count silently freezes the tab bar's backdrop for
        // the rest of the session.
        if !self.open {
            unsafe { *std::ptr::addr_of_mut!(OPEN_COUNT) += 1 };
        }
        self.open = true;
        crate::gfx::blur_invalidate();
    }
    /// Close is an instant hide (no exit animation — matches the panels' historical behavior).
    pub(crate) fn close(&mut self) {
        if self.open {
            unsafe { *std::ptr::addr_of_mut!(OPEN_COUNT) -= 1 };
        }
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
        self.rise.set(rise);
        let page = crate::ui::nav::page_alpha();
        if scrim_a > 0.0 {
            let dim = theme::scrim_black(scrim_a * a * page);
            Painter::root().rect(Rect::FULL, 0.0, dim, dim, 0.0);
        }
        Painter::root().alpha(a * page).translate(0.0, rise * (1.0 - a))
    }

    /// Draw the panel's GROUND at `r` with corner `rad` — frosted over a live backdrop blur where
    /// the platform allows one, and the near-opaque sheet where it does not.
    ///
    /// **The pair is the point.** `theme::PANEL_FROST_*` is only legal on top of a blur, and the
    /// blur latches itself off on a driver that cannot give it a render target — so a screen that
    /// picked the frosted tokens itself would draw a translucent hole on exactly the devices nobody
    /// here owns. One function owns both halves and no screen carries the fallback.
    ///
    /// Call it as the FIRST thing drawn through the content painter: `gfx::draw_blur_backdrop`
    /// samples the default framebuffer as it stands, so everything meant to show through must
    /// already be on it and nothing that sits on the panel may be yet.
    ///
    /// Two consequences worth knowing. The window follows the panel through its entry SLIDE, which
    /// is what real glass does — the snapshot is of the page, not of the panel's final resting
    /// place. And the snapshot is taken while the modal scrim is still ramping up, so the glass
    /// stays a little brighter than the page dimming around it; that reads as the panel being lit
    /// rather than as a mismatch, and chasing it would mean re-capturing every frame of the appear.
    ///
    /// **Not for the player's panels.** Behind those is punch-through alpha to the hardware video
    /// plane, which GL cannot read; they keep `p.rect(…, PANEL_TOP, PANEL_BOT, …)` on purpose.
    pub(crate) fn panel(&self, p: Painter, r: Rect, rad: f32) {
        // How far below (or above) its resting place this frame draws the panel — exactly the
        // translate `painter` just applied. The snapshot is grabbed around the REST rect, so the
        // whole appear animation is served by one capture instead of ~29.
        let slide = self.rise.get() * (1.0 - self.appear());
        if p.backdrop_blur(r, slide, rad, [1.0, 1.0, 1.0, 1.0]) {
            p.rect(r, rad, theme::PANEL_FROST_TOP, theme::PANEL_FROST_BOT, 0.0);
        } else {
            p.rect(r, rad, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counter behind [`any_open`], driven through the four sequences that leak it if either
    /// transition is unguarded: a re-open (the same chip pressed twice), a redundant close (an arm
    /// that closes whatever might be up), two panels overlapping, and the return to rest.
    ///
    /// A leak in either direction is silent and lasts the session — over-count freezes the glass
    /// tab bar's backdrop on a stale picture, under-count spends a blur chain a frame — so the
    /// property worth pinning is the whole round trip, not a single call.
    #[test]
    fn the_open_count_survives_re_opens_redundant_closes_and_overlap() {
        let _g = crate::testlock::serial();
        // Whatever the process arrived with (a static, and other tests may have moved it), the
        // assertions below are all RELATIVE to it — the invariant is the round trip, not zero.
        let base = unsafe { *std::ptr::addr_of!(OPEN_COUNT) };
        let count = || unsafe { *std::ptr::addr_of!(OPEN_COUNT) } - base;

        let mut a = Popover::new();
        let mut b = Popover::new();
        assert_eq!(count(), 0, "two fresh panels register nothing");

        a.open();
        assert!(any_open());
        a.open(); // re-open: restarts the motion, does not open a second panel
        assert_eq!(count(), 1, "a re-open must not count twice");

        b.open();
        assert_eq!(count(), 2, "overlapping panels each count once");

        b.close();
        b.close(); // the defensive close every dismissal arm makes
        assert_eq!(count(), 1, "closing an already-closed panel must not decrement");
        assert!(any_open(), "the other panel is still up");

        a.close();
        assert_eq!(count(), 0);
        assert!(!any_open(), "back to rest — the tab bar resumes retaking its backdrop");
    }
}
