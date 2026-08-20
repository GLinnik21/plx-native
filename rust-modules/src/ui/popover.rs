//! `Popover` — the ONE open/appear choreography every modal panel shares (track menu, Info card,
//! Chapters strip, profile menu): an OPEN flag + a critically-damped 0→1 appear spring driving a
//! fade + slide-into-place, with an optional full-screen scrim. Each panel used to hand-wire its
//! own `static OPEN + APPEAR` pair, so any motion change was a four-file edit.
use crate::ui::{theme, Painter, Rect, Spring};
use crate::ui::widgets::{Glass, GlassState};

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
/// Its user is the glass tab bar. A modal disables that bar because the two far-apart glass regions
/// would union into most of the frame; the popover's own material is the only glass worth drawing
/// while it is up. This remains true for both cached and dynamic popovers.
pub(crate) fn any_open() -> bool {
    unsafe { *std::ptr::addr_of!(OPEN_COUNT) > 0 }
}

pub(crate) struct Popover {
    open: bool,
    appear: Spring,
    glass: Glass,
    glass_state: GlassState,
    /// The `rise` the last [`painter`](Self::painter) was asked for, so [`panel`](Self::panel) can
    /// work out how far this frame is from the panel's resting position. Stashed rather than passed
    /// again because the painter and panel belong to one draw choreography, and a second copy of the
    /// number is a second place for it to be wrong. A `Cell` because both calls take `&self`, and
    /// this is main-thread draw state like everything else here.
    rise: std::cell::Cell<f32>,
}
impl Popover {
    pub(crate) const fn new() -> Self {
        Self::with_glass(Glass::CACHED)
    }
    /// Opt this popover into a reusable glass policy. Existing callers stay cached through
    /// [`new`](Self::new); a moving underlay must choose the dynamic policy explicitly.
    pub(crate) const fn with_glass(glass: Glass) -> Self {
        Popover {
            open: false,
            appear: Spring::at(0.0),
            glass,
            glass_state: GlassState::new(),
            rise: std::cell::Cell::new(0.0),
        }
    }
    /// (re)open: restart the fade+slide from 0.
    ///
    /// Also starts this popover's glass lifetime. Cached glass takes one snapshot; dynamic glass
    /// anchors its every-third-present cadence here. Anything outside that policy which changes the
    /// underlay still owes `gfx::blur_invalidate` a call of its own.
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
        self.glass.activate(&mut self.glass_state);
    }
    /// Close is an instant hide (no exit animation — matches the panels' historical behavior).
    pub(crate) fn close(&mut self) {
        if self.open {
            unsafe { *std::ptr::addr_of_mut!(OPEN_COUNT) -= 1 };
        }
        self.open = false;
        self.glass_state.deactivate();
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

    /// Resolve this popover's glass cadence BEFORE its host page draws. `underlay_changed`
    /// describes that page, not this popover's own springs. Capture is still deferred to [`panel`],
    /// after the host page is complete; this resolves only cadence invalidation.
    pub(crate) fn prepare_present(&mut self, underlay_changed: bool) {
        if self.open {
            self.glass.prepare(&mut self.glass_state, underlay_changed);
        }
    }

    /// The panel painter without a full-screen scrim. Pair with [`scrim`](Self::scrim), for a
    /// caller that draws the dim as part of its host page — see that method for why one does.
    pub(crate) fn content_painter(&self, rise: f32) -> Painter {
        let a = self.appear();
        self.rise.set(rise);
        Painter::root()
            .alpha(a * crate::ui::nav::page_alpha())
            .translate(0.0, rise * (1.0 - a))
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
        // A REFRESHING policy's scrim belongs to the host page — see [`Glass::needs_page_scrim`].
        // Reaching here with one means the panel's own backdrop will be sampled from an undimmed
        // page, which on a television reads as a panel brighter than the screen around it and is
        // very hard to attribute. On the host it is now this line.
        debug_assert!(
            !self.glass.needs_page_scrim() || scrim_a <= 0.0,
            "a refreshing popover's scrim belongs to the page: use Popover::scrim + content_painter"
        );
        self.scrim(scrim_a);
        self.content_painter(rise)
    }

    /// Draw the modal scrim ALONE, for a caller that needs it earlier in the frame than the panel.
    ///
    /// **A dynamic backdrop needs this.** The scrim sits between the page and the panel, so it is
    /// part of what the panel's glass looks through — and the capture path gets that for free, since
    /// it grabs framebuffer 0 after the scrim is already on it. The DIRECT path does not: it
    /// re-renders the page closure into a small target before any popover draws, so a panel served
    /// by it sampled an undimmed page and came out brighter than the dimmed surroundings it sat in.
    /// Drawing the scrim as part of the page closure puts it back on both paths, in one place, at
    /// the same point in draw order it always occupied.
    ///
    /// Pair with [`content_painter`](Self::content_painter), which is the same panel painter with
    /// the scrim left out — calling [`painter`](Self::painter) as well would dim twice.
    pub(crate) fn scrim(&self, scrim_a: f32) {
        if scrim_a > 0.0 {
            let dim = theme::scrim_black(scrim_a * self.appear() * crate::ui::nav::page_alpha());
            Painter::root().rect(Rect::FULL, 0.0, dim, dim, 0.0);
        }
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
    /// place. Cached popovers capture the scrim at their first draw; a source-dimmed dynamic policy
    /// refreshes the moving underlay on its own successful-present cadence.
    ///
    /// **Not for the player's panels.** Behind those is punch-through alpha to the hardware video
    /// plane, which GL cannot read; they keep `p.rect(…, PANEL_TOP, PANEL_BOT, …)` on purpose.
    pub(crate) fn panel(&self, p: Painter, r: Rect, rad: f32) {
        // How far below (or above) its resting place this frame draws the panel — exactly the
        // translate `painter` just applied. The snapshot is grabbed around the REST rect, so the
        // slide itself never forces another capture; a dynamic refresh policy may still do so.
        let slide = self.rise.get() * (1.0 - self.appear());
        self.glass.panel(p, r, slide, rad);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed popover prepares NOTHING, whichever policy it carries — no activation, no
    /// invalidation, no snapshot scheduled. That is what lets every popover in the app call
    /// `prepare_present` unconditionally from its route arm.
    /// The rule `painter` asserts on, graded as a pure property of the two policies: a refreshing
    /// backdrop is re-sourced from the page closure, so its dim has to be IN that closure.
    #[test]
    fn only_a_refreshing_policy_owes_its_scrim_to_the_page() {
        assert!(!Glass::CACHED.needs_page_scrim(), "a cached grab already contains its own scrim");
        assert!(
            Glass::DYNAMIC_BACKDROP.needs_page_scrim(),
            "a refreshing backdrop re-renders the page, which must therefore carry the dim"
        );
    }

    #[test]
    fn cached_is_the_default_and_dynamic_glass_requires_an_explicit_opt_in() {
        let mut cached = Popover::new();
        let mut dynamic = Popover::with_glass(Glass::DYNAMIC_BACKDROP);
        assert_eq!(cached.glass, Glass::CACHED);
        assert_eq!(dynamic.glass, Glass::DYNAMIC_BACKDROP);
        cached.prepare_present(false);
        dynamic.prepare_present(false);
        assert!(!cached.glass_state.is_active() && !dynamic.glass_state.is_active());
    }

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
