//! `Popover` — the ONE open/appear choreography every modal panel shares (track menu, Info card,
//! Chapters strip, profile menu): an OPEN flag + a critically-damped 0→1 appear spring driving a
//! fade + slide-into-place, with an optional full-screen scrim. Each panel used to hand-wire its
//! own `static OPEN + APPEAR` pair, so any motion change was a four-file edit.
//!
//! ## The host page is part of the choreography, and since 2026-09-02 it is part of THIS module
//!
//! A modal stands on a page that is not moving, and on a tile-based GPU redrawing that page is the
//! whole cost of having the modal up. Measured on the television at commit `c75d50ae`: the detail
//! page under an open track panel presented every frame at `draw≈75 ms` — `loop=30` while paging —
//! against 60 fps and `worstframe≈29 ms` for the same page with no panel. The class bisect
//! (`/tmp/plxnative-drawmask`) priced it as fill and nothing else: `all` 17 ms (the compositor
//! floor), `glass` 50, `rect` 51, `grad` 50, `card` 73, `text` 75 — i.e. the page's own full-screen
//! ground, the modal scrim and the panel's glass each cost a vsync slot and they overlap.
//!
//! The profile menu had solved this for itself in 2026-08 with a private `gfx::FrameCache`, and no
//! other popover could reach it. [`host`] is that mechanism generalised, and the generalisation is
//! what makes it apply to panels the profile menu's shape could not: a popover drawn from INSIDE
//! its page (`tracks_panel`, `about_panel`, `alt_sources`, `person_bio` all draw at the tail of
//! `detail::draw` / `person::draw`) cannot be served by "skip the page and draw the panel after
//! it". So the freeze is a REFUSAL at the renderer's one shared gate rather than a skipped call
//! tree: the page's draw still runs and still records its layout and hit rects, the fill is what
//! goes away, and each popover lifts the freeze around its own drawing with [`host::live`].
//!
//! What the mechanism guarantees, in the order the code establishes it:
//!
//! - **The snapshot is the UNDIMMED host page.** [`host::live`] captures on its first call of a
//!   frame, which is by construction the moment before the first popover draws anything — its
//!   scrim included. The scrim and the [`Opener`] lift stay LIVE above the quad, which they must:
//!   the scrim ramps with the appear spring, and a lift baked into the snapshot would then be
//!   dimmed by the live scrim drawn over it, which is the exact bug the lift exists to fix.
//! - **A cached-glass snapshot is still taken from the completed dimmed page.** Nothing about the
//!   glass changes: `Glass::CACHED` grabs framebuffer 0 after the page scrim is on it, and the page
//!   scrim is drawn live over the cached quad, so the composite it samples is identical.
//! - **HOST DAMAGE refreshes the snapshot; the popover's OWN activity does not.** That distinction
//!   was `person_bio`'s private `OWN_DAMAGE` ledger and is now [`own_motion`] / [`host::live`] /
//!   [`host::input_scope`] / [`host::page_pass`] for the host cache (and [`note_own_damage`] for
//!   the glass ledger), shared — attributed at the source and counted
//!   (`idle::take_page_damage`), with [`host_refresh`] as the host cache's decision and
//!   [`glass_refresh`] as the dynamic backdrop's.
use crate::ui::widgets::{Glass, GlassState};
use crate::ui::{theme, Painter, Rect, Spring};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

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

/// How many OPEN popovers have asked for a cached host — see [`Popover::caching_host`].
///
/// A separate counter from [`OPEN_COUNT`] and not a filter over it, for the same reason that one is
/// a counter: the popovers live in seven modules and two of them are routes, so nothing can
/// enumerate them. Reaching zero is what puts the page back on the live path.
static mut HOST_USERS: u32 = 0;
/// How many of [`HOST_USERS`] are panels on their way OUT — dismissed, fading, still holding the
/// freeze (see `Popover::release_host`). When every user is one of these the page under the
/// snapshot is live to input again, and its MOTION becomes a reason to redraw it
/// ([`host_refresh`]) — under an open panel only its damage is.
static mut HOST_CLOSING: u32 = 0;

/// Damage this frame that a POPOVER caused, rather than the page underneath it.
///
/// The process-wide dirty/motion flags cannot tell the two apart — a panel's page turn calls
/// `idle::invalidate` (it must: the panel really did change) and a panel's scroll spring reports
/// through `note_spring` exactly like the page's would. So the panels keep this ledger of the
/// damage they are themselves the reason for, and [`glass_refresh`] subtracts it.
///
/// It was `person_bio`'s private static and is shared for one blunt reason: the bug it fixes is a
/// property of every popover with a scroll or a selection in it, and that module had it only
/// because it was the first one whose FPS was measured. Set through [`note_own_damage`] /
/// [`own_motion`], taken once a frame by [`host::begin_frame`].
static OWN_DAMAGE: AtomicBool = AtomicBool::new(false);
/// The panel's own SPRINGS moved this frame ([`own_motion`]). Both bits feed ONE consumer now —
/// the dynamic backdrop's cadence, through [`host::own_damage_this_frame`] — merged; the host
/// cache no longer reads either, its attribution being by scope and by count
/// (`idle::take_page_damage`, [`host::input_scope`]).
static OWN_MOTION: AtomicBool = AtomicBool::new(false);

/// Record that a popover's OWN state changed this frame — a page turn, a selection move, a row
/// commit. Call it beside the `idle::invalidate` such a change already owes: the two are different
/// questions ("something must repaint" vs "the page underneath did NOT change"), and answering only
/// the first is what made every keypress re-source a backdrop.
#[inline]
pub(crate) fn note_own_damage() {
    OWN_DAMAGE.store(true, Relaxed);
}

/// Attribute the spring motion of a popover's own `update` to the POPOVER rather than to the page
/// it stands on — one line at the top of that `update`, held for the body.
///
/// This is [`crate::ui::idle::MotionScope`] with the result routed into [`OWN_MOTION`], plus an
/// [`crate::ui::idle::OwnScope`] so that every `idle::invalidate` the update raises (the appear
/// spring's, a marquee's) is the panel's own damage rather than the page's: the same shape `app.rs`
/// already uses to keep Home's springs out of the account popover's backdrop, said once here so
/// every panel's `update` is one line rather than six, and so that a panel added tomorrow inherits
/// it.
///
/// **The scope merges back**, so a popover's motion still keeps the present gate awake — this
/// changes who the motion is ATTRIBUTED to, never whether it counts as motion.
#[must_use = "the scope is only open for this guard's lifetime"]
pub(crate) struct OwnMotion(
    Option<crate::ui::idle::MotionScope>,
    #[allow(dead_code)] crate::ui::idle::OwnScope,
);

/// See [`OwnMotion`].
pub(crate) fn own_motion() -> OwnMotion {
    OwnMotion(
        Some(crate::ui::idle::MotionScope::open()),
        crate::ui::idle::OwnScope::open(),
    )
}

impl Drop for OwnMotion {
    fn drop(&mut self) {
        if let Some(scope) = self.0.take() {
            if scope.close() {
                OWN_MOTION.store(true, Relaxed);
            }
        }
    }
}

/// **Should a popover re-source its dynamic BACKDROP this present?** The decision behind the
/// dynamic glass cadence. (The host snapshot's lifetime used to be resolved from it too; that is
/// [`host_refresh`] over `idle::take_page_damage` now.)
///
/// Lifted verbatim out of `person_bio`, where it was written after a measured FPS regression and
/// two review passes; the reasoning that produced it is general and the file it lived in was not.
///
/// - `underlay_changed` is what the caller believes about the page. It cannot be trusted alone:
///   `app.rs` folds `idle::present_dirty()` into it, and every key press sets that — including the
///   ones this panel swallowed.
/// - `own_damage` is [`OWN_DAMAGE`], the panel's own ledger, subtracted from it.
/// - `appear_settled` keeps the ramp honest: while the panel is fading in, the SCRIM under it is
///   still darkening, so a backdrop sampled once at open would frost an undimmed page for the rest
///   of the session.
///
/// A settled panel over a page that nothing but the panel touched never re-sources; one over a page
/// that just grew a shelf, or landed a poster texture, re-sources once.
pub(crate) fn glass_refresh(underlay_changed: bool, appear_settled: bool, own_damage: bool) -> bool {
    !appear_settled || (underlay_changed && !own_damage)
}

/// **Does the host snapshot survive this frame?** Two questions about the PAGE, and only the page.
///
/// `page_dirty` — did it CHANGE: `idle::take_page_damage`, every invalidation this frame that no
/// popover claimed as its own, by count, so a landing beside a key press or during the panel's
/// own motion is still seen. That is a reason to redraw whether the panels are open or fading.
///
/// `page_moving` — is it MOVING: `app.rs`'s `underlay_moving` (Home, the Library and Search step
/// their springs inside a `scoped_motion`) OR'd with the gate's unscoped-motion bit. **That is a
/// reason to redraw only while every holder is FADING** (`fading_only`: dismissed, input back on
/// the page, the page free to scroll under the fade). With a panel OPEN the page cannot be driven,
/// and what still moves on it is decoration — a focused card's title marquee, the hero's slow
/// drift — which the frozen picture is MEANT to pause (the module doc's "a decoration on a page
/// the user cannot reach"). Read as a refresh reason, that decoration re-rendered Home under the
/// account menu on every frame: 25M GPU cycles and 26 fps on the set, 2026-09-04, against the
/// 11M and 50+ the freeze exists for. The one thing this leaves out is a page still SETTLING on
/// the frame a panel opens (the item menu over a grid whose focus spring has not landed): the
/// snapshot holds that frame and the page finishes its last few pixels when the panel lets go.
/// Accepted — it is the account menu's behaviour since the cache existed.
///
/// The panel's own motion and damage never reach either term: they are attributed at the source
/// (`own_motion`, `host::live`, `host::input_scope`, `host::page_pass`). Earlier shapes subtracted a merged per-frame
/// "own" bit from `dirty || moving`, and each had a case where the page's change was consumed
/// unseen (Codex review, three rounds, 2026-09-04).
pub(crate) fn host_refresh(fading_only: bool, page_dirty: bool, page_moving: bool) -> bool {
    page_dirty || (fading_only && page_moving)
}

/// The element a popover was opened FROM — where it is, and how to put it back on screen.
///
/// A modal scrim is ONE full-screen quad, so it dims the whole page INCLUDING the card or chip the
/// menu is about: the one thing on screen the panel is talking about recedes with everything else.
/// Fixing that needs no new machinery — the renderer is immediate-mode and z-order IS call order,
/// so "above the scrim" literally means "drawn after the scrim call".
///
/// **Both halves come from the HOST, which is why they travel together.** Only the screen that drew
/// the element knows where it landed (a shelf's scroll spring and the cell's own focus pop are
/// invisible from here) and only that screen can draw it again. A bare `fn()` rather than a closure,
/// because this is stored beside the open panel in a `static mut` — the same shape `ui::nav` carries
/// an outgoing page's teardown as.
#[derive(Clone, Copy)]
pub(crate) struct Opener {
    /// The element's drawn rect in screen coords — what the panel is anchored beside. `None` when
    /// the host has no element to point at (the hero view has no card; a headless trigger opens the
    /// menu with nothing focused), which is a placement fallback and never an error.
    pub(crate) rect: Option<Rect>,
    /// Re-draw that element exactly as its page drew it, ABOVE the scrim. It builds its own root
    /// painter, because which alpha it belongs on is the host's business: a page element rides
    /// `nav::page_alpha`, a piece of the shared top bar rides `nav::chrome_alpha`.
    pub(crate) redraw: fn(),
}

/// The [`Opener::redraw`] of a popover with nothing to lift — a named fn rather than a closure, so
/// [`Opener::NONE`] can be a `const`.
fn draw_nothing() {}

impl Opener {
    /// No element at all: the panel falls back to its own placement and the scrim covers everything,
    /// which is exactly what every popover did before this existed.
    pub(crate) const NONE: Opener = Opener {
        rect: None,
        redraw: draw_nothing,
    };

    /// An opener that is only a DRAW — for a panel whose placement is its own. The Library's chip
    /// menus are anchored on the toolbar rather than on the chip's measured rect, so they have a
    /// thing to lift and no rect to hand over.
    pub(crate) const fn drawn(redraw: fn()) -> Opener {
        Opener { rect: None, redraw }
    }
}

/// Does this popover freeze the page it stands on into the shared [`host`] snapshot?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostPolicy {
    /// The page keeps drawing underneath. The right answer whenever the popover does not cover
    /// enough of it to matter, or whenever the host is not a page at all — the player route, where
    /// what is behind the panel is punch-through alpha to a hardware plane that GL cannot read.
    /// (The Settings family was listed here too, as "replaces its host with an opaque ground and so
    /// has nothing to cache" — since 2026-09-03 it caches its host like every other page popover,
    /// and once its ground is ready it skips the page pass altogether; see `settings.rs`.)
    Live,
    /// The page is drawn once into the shared snapshot and served from it, refreshed on host
    /// damage. See the module doc.
    Cached,
}

pub(crate) struct Popover {
    open: bool,
    /// [`dismiss`](Self::dismiss) ran and the appear spring is still on its way back to 0: the
    /// panel is no longer OPEN (input has returned to the page) but it is still VISIBLE.
    closing: bool,
    appear: Spring,
    glass: Glass,
    glass_state: GlassState,
    /// See [`HostPolicy`]. Opt-in through [`Popover::caching_host`], so the set of popovers that
    /// freeze their page is a list a reviewer can read off the constructors.
    host: HostPolicy,
    /// Is this popover currently counted in [`HOST_USERS`]? Held from [`open`](Self::open) until
    /// the panel has GONE — which for a [`dismiss`](Self::dismiss) is the end of its fade, not the
    /// press frame (see [`release_host`](Self::release_host)). A flag of its own rather than a
    /// reading of `open`, because the two disagree for exactly that stretch.
    host_held: bool,
    /// Counted in [`HOST_CLOSING`]: held AND dismissed. Cleared by a re-open or by the release.
    host_closing: bool,
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
            closing: false,
            appear: Spring::at(0.0),
            glass,
            glass_state: GlassState::new(),
            rise: std::cell::Cell::new(0.0),
            host: HostPolicy::Live,
            host_held: false,
            host_closing: false,
        }
    }

    /// Freeze this popover's host page into the shared snapshot while it is open — see [`host`].
    ///
    /// Opt-IN rather than the default, and the default is the one that draws more, because the two
    /// failures are not comparable. A popover that could have frozen its page and does not costs
    /// frames; one that freezes a page it should not have shows a stale picture, and the classes
    /// where that is true are structural rather than a matter of taste (the player's overlays stand
    /// on a hardware video plane GL cannot read back at all). A `const fn` builder so the popovers
    /// stay `static`s with no initialiser to run.
    pub(crate) const fn caching_host(mut self) -> Self {
        self.host = HostPolicy::Cached;
        self
    }
    /// (re)open: restart the fade+slide from 0.
    ///
    /// Also starts this popover's glass lifetime. Cached glass takes one snapshot; dynamic glass
    /// anchors its every-changed-present cadence here. Anything outside that policy which changes
    /// the underlay still owes `gfx::blur_invalidate` a call of its own.
    pub(crate) fn open(&mut self) {
        self.appear = Spring::at(0.0);
        self.closing = false;
        // A re-open DURING the fade-out: the held user stops being a closing one.
        self.leave_closing();
        // Both transitions are guarded on the flag actually CHANGING, not on the call: `open` is a
        // re-open (a second press on the same chip restarts the motion) and `close` is called
        // defensively by arms that do not know whether anything was up. An unguarded pair leaks the
        // count in both directions, and a leaked count silently freezes the tab bar's backdrop for
        // the rest of the session.
        if !self.open {
            unsafe { *std::ptr::addr_of_mut!(OPEN_COUNT) += 1 };
            self.hold_host();
        }
        self.open = true;
        self.glass.activate(&mut self.glass_state);
        // A re-open restarts the appear motion over a page that may have moved since, and the very
        // first frame of a first open has no snapshot at all. Both are "the cache does not describe
        // what is behind me", which is what this call means.
        host::invalidate();
    }
    /// Close is an INSTANT hide — for a panel whose subject just went away (the item it was about
    /// was replaced under it) or a boot-time reset. An interactive BACK/OK wants [`dismiss`]: the
    /// appear choreography run in reverse, which every modal in the app shares for the same reason
    /// it shares the entry (`RISE`'s doc) — one panel leaving differently from its neighbour reads
    /// as a different kind of object. Settings' exit was the report (2026-09-02); it is the
    /// mechanism, not that one screen, that got the fade.
    pub(crate) fn close(&mut self) {
        self.release();
        self.release_host();
        self.closing = false;
        self.appear = Spring::at(0.0);
    }
    /// The bookkeeping half of a close: input modality and the glass lifetime end NOW, whether the
    /// panel then vanishes or fades. The host freeze is NOT in here — see
    /// [`release_host`](Self::release_host) for why it outlives a dismiss.
    fn release(&mut self) {
        if self.open {
            unsafe { *std::ptr::addr_of_mut!(OPEN_COUNT) -= 1 };
        }
        self.open = false;
        self.glass_state.deactivate();
        host::invalidate();
    }
    /// Register this popover's frozen-host user — once, however many times it is (re)opened, and
    /// not at all under a `Live` policy. A re-open DURING the fade-out finds the user still held
    /// and must not count it twice.
    fn hold_host(&mut self) {
        if self.host == HostPolicy::Cached && !self.host_held {
            unsafe { *std::ptr::addr_of_mut!(HOST_USERS) += 1 };
            self.host_held = true;
        }
    }
    /// The other half, and deliberately NOT part of [`release`](Self::release): **a dismissed panel
    /// keeps its page frozen for the length of its fade-out.** The page has not changed — input
    /// went back to it on the press frame, but nothing it does in the next 300 ms is a reason to
    /// redraw a hero and three shelves under a panel that is on its way out — and the fade is
    /// drawn over it up to sixty times. Releasing on the press frame put every one of those frames
    /// on the live path, which is the laggy DISMISSAL every cached panel had (2026-09-03: the About
    /// sheet, 60 fps while scrolling, fell to ~20 while fading; Settings' exit ramp showed its grey
    /// ground for a second over a Home that was being fully re-rendered under it). `close` releases
    /// at once; `update` releases when the fade lands. Real page damage during the fade still
    /// re-captures through `host::begin_frame`, exactly as it does while the panel is open.
    fn release_host(&mut self) {
        self.leave_closing();
        if self.host_held {
            unsafe { *std::ptr::addr_of_mut!(HOST_USERS) -= 1 };
            self.host_held = false;
            host::invalidate();
        }
    }
    /// Count this held user among the fading ones — see [`HOST_CLOSING`]. A no-op for a panel that
    /// holds no user, or is already counted.
    fn enter_closing(&mut self) {
        if self.host_held && !self.host_closing {
            unsafe { *std::ptr::addr_of_mut!(HOST_CLOSING) += 1 };
            self.host_closing = true;
        }
    }
    fn leave_closing(&mut self) {
        if self.host_closing {
            unsafe { *std::ptr::addr_of_mut!(HOST_CLOSING) -= 1 };
            self.host_closing = false;
        }
    }
    /// Step the appear spring inside an `idle::MotionScope`, so the panel's own motion is never
    /// read as the PAGE's — the distinction [`host_refresh`] rests on for a fading panel. Reports
    /// whether the spring moved.
    fn step_appear(&mut self, target: f32, dt: f32) -> bool {
        let scope = crate::ui::idle::MotionScope::open();
        self.appear.step(target, K_APPEAR, dt);
        scope.close()
    }
    /// Close with the appear choreography played backwards: the panel stops being OPEN this frame
    /// (keys go to the page again, its glass lifetime ends) and stays VISIBLE while `appear`
    /// springs back to 0 under [`update`](Self::update). A CACHING panel keeps its host frozen for
    /// the length of that fade and lets go at the end ([`Live`]'s doc,
    /// `a_cached_popover_holds_its_frozen_host_through_the_fade_and_releases_at_the_end`) — the
    /// page under a fading panel has not changed. Draw sites gate on
    /// [`visible`](Self::visible) so the fade is actually drawn; input sites keep gating on
    /// [`is_open`](Self::is_open). A no-op on a panel that is not open.
    pub(crate) fn dismiss(&mut self) {
        if !self.open {
            return;
        }
        self.release();
        self.closing = true;
        self.enter_closing();
        crate::ui::idle::invalidate();
    }
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
    /// Open, or still fading out after [`dismiss`](Self::dismiss). The gate for DRAWING a panel;
    /// never for taking its input.
    pub(crate) fn visible(&self) -> bool {
        self.open || self.closing
    }
    /// step the appear spring; no-op when closed.
    ///
    /// The ramp is booked as the POPOVER's own damage, not the page's: `Spring::step` reports to
    /// `ui::idle` like every other spring, and without this the frame-wide motion bit would say
    /// "the underlay is moving" for the whole of every panel's entry animation — the one stretch
    /// where nothing behind it moves at all.
    pub(crate) fn update(&mut self, dt: f32) {
        if self.open {
            self.step_appear(1.0, dt);
            if !self.appear_settled() {
                note_own_damage();
            }
        } else if self.closing {
            self.step_appear(0.0, dt);
            note_own_damage();
            // the same visual-arrival tolerance as `appear_settled`, at the other end
            if self.appear.pos <= 0.001 {
                self.closing = false;
                self.appear = Spring::at(0.0);
                self.release_host();
            }
        }
    }
    /// the appear fraction 0..1, for anything else keyed to the open motion.
    pub(crate) fn appear(&self) -> f32 {
        self.appear.pos.clamp(0.0, 1.0)
    }

    /// **Has the appear choreography finished ramping?** True once the fade+slide has visibly
    /// arrived — `pos` need not have decayed to the exact analytic rest [`Spring::step`] reports
    /// through `note_spring` (a fraction of a percent short of 1 is not a frame anybody can see),
    /// so this is a coarser, visual-arrival test rather than that motion-detector's own tolerance.
    ///
    /// The one caller today is a `DYNAMIC_BACKDROP` popover deciding whether ITS OWN opening
    /// motion is still a reason to keep resampling the host page — see
    /// `person_bio::prepare_present`'s module note. A popover with no reason to ask this (a cached
    /// one, or one with no backdrop at all) simply never calls it.
    pub(crate) fn appear_settled(&self) -> bool {
        self.appear.pos >= 0.999
    }

    /// Resolve this popover's glass cadence BEFORE its host page draws. `underlay_changed`
    /// describes that page, not this popover's own springs. Capture is still deferred to [`panel`],
    /// after the host page is complete; this resolves only cadence invalidation.
    ///
    /// The caller's flag is not passed straight through: it goes through [`glass_refresh`] together
    /// with this popover's own appear state and the shared [`OWN_DAMAGE`] ledger, which is where
    /// "the page changed" is separated from "I changed". `person_bio` did that folding privately
    /// and every other panel did not; doing it here is what makes the second one impossible to
    /// forget.
    pub(crate) fn prepare_present(&mut self, underlay_changed: bool) {
        if self.open {
            let refresh = glass_refresh(
                underlay_changed,
                self.appear_settled(),
                host::own_damage_this_frame(),
            );
            self.glass.prepare(&mut self.glass_state, refresh);
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
    /// **The app's entry-slide distance** — how far a panel rises into place on open, in px, for
    /// [`painter`](Self::painter) and [`content_painter`](Self::content_painter).
    ///
    /// It is published here because this type owns the appear choreography and the value is a
    /// property of THAT, not of any one panel: every modal in the app is meant to arrive the same
    /// way, and a panel sliding a different distance from its neighbour reads as a different kind
    /// of object. `rise` stays a parameter rather than becoming this constant outright, because a
    /// panel anchored to the BOTTOM of the frame passes it negative to drop down instead.
    ///
    /// 20 is what `alt_sources` has always drawn. It is written down because the four alert panels
    /// arrived with 24 / 20 / 20 / 18 — each documented in its own file as "the shared entry
    /// distance", "matching every other popover in the app", "the shape every panel in the app
    /// appears with". Four files claiming to match each other, and no two of them agreeing, is the
    /// exact failure a token exists to prevent; it is the same disease as the four names the 32px
    /// corner shipped under.
    pub(crate) const RISE: f32 = 20.0;

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

    /// Draw the modal scrim, then LIFT `opener` back out of it: the element the popover was opened
    /// FROM, re-drawn on top of the dim, so the one thing the panel is about stays at full strength
    /// while the page around it recedes.
    ///
    /// **Both calls belong to the HOST PAGE**, for the reason [`scrim`](Self::scrim) already gives
    /// and one more. The direct blur source path re-renders the page closure into a small target
    /// before any popover draws, so a lift drawn WITH the panel would reach the visible frame and
    /// never the snapshot — and a glass panel frosting a *dimmed* copy of the very card it is about
    /// is exactly the class of artefact the scrim's own placement rule exists to prevent.
    ///
    /// **The lift is a SECOND DRAW, not a cut-out, and that is a visual decision rather than an
    /// implementation detail.** Everything the element paints OPAQUELY comes out at exactly its own
    /// colour, which is the point. Everything it paints with ALPHA — its soft shadow, its focus
    /// glow, the anti-aliased edges of its label — composites twice, once dimmed under the scrim
    /// and once over it, so those read heavier than the same element does anywhere else on the
    /// screen. The shadow reading twice is what a raised object would really cast; the label's AA
    /// edges thickening is the price, and it is paid from the first frame the panel is up, not
    /// only once the scrim has ramped in. The alternative — scrim as four quads around the rect —
    /// has neither cost and cannot follow an element whose glow and caption run outside its own
    /// frame, which every card surface here has.
    pub(crate) fn scrim_lifting(&self, scrim_a: f32, opener: &Opener) {
        self.scrim(scrim_a);
        if scrim_a > 0.0 {
            (opener.redraw)();
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
    /// place. Cached popovers capture the page-drawn scrim at their first draw; a dynamic policy
    /// refreshes that same composed underlay on every changed present.
    ///
    /// **Not for the player's panels.** Behind those is punch-through alpha to the hardware video
    /// plane, which GL cannot read; they keep `p.rect(…, PANEL_TOP, PANEL_BOT, …)` on purpose.
    pub(crate) fn panel(&self, p: Painter, r: Rect, rad: f32) {
        // How far below (or above) its resting place this frame draws the panel — exactly the
        // translate `painter` just applied. The snapshot is grabbed around the REST rect, so the
        // slide itself never forces another capture; a dynamic refresh policy may still do so.
        let slide = self.rise.get() * (1.0 - self.appear());
        self.glass.panel(p, r, slide, rad);
        self.ground_done();
    }

    /// The large-modal counterpart of [`panel`](Self::panel). It keeps the same cached/dynamic
    /// lifetime policy and entry motion, but asks [`Glass::sheet`] for the design-system sheet
    /// material instead of spreading the compact-menu material over a near-full-screen surface.
    pub(crate) fn sheet(&self, p: Painter, r: Rect, rad: f32) {
        let slide = self.rise.get() * (1.0 - self.appear());
        self.glass.sheet(p, r, slide, rad);
        self.ground_done();
    }

    /// The boundary between this popover's GROUND and its FOREGROUND, reported to [`host`].
    ///
    /// It hangs off [`panel`](Self::panel)/[`sheet`](Self::sheet) rather than being a call every
    /// screen has to remember, because those two ARE that boundary: their doc has always said
    /// "call it as the FIRST thing drawn through the content painter", so everything after one of
    /// them is the moving part. A panel that grew a second ground call would break this, which is
    /// why the ground is drawn by exactly these two methods and not by the screens.
    ///
    /// Inert for a `Live` host: a popover that does not freeze its page has no ground stage, and
    /// the player's overlays must never reach one — behind them is punch-through alpha to a
    /// hardware plane, which GL cannot read back at all.
    fn ground_done(&self) {
        if self.host == HostPolicy::Cached {
            host::ground_drawn(self.appear_settled());
        }
    }

    /// Paint the shared, full-screen Settings-family ground through this popover's glass policy.
    /// The screen deliberately cannot reach into `glass`: keeping that field private preserves the
    /// activation/snapshot lifetime owned by `Popover`.
    pub(crate) fn modal_ground(&self, p: Painter, r: Rect) {
        self.glass.modal_ground(p, r);
    }
}

/// **The shared host snapshot** — one page-sized texture, one lifetime, every popover that asked
/// for it. See this module's own doc for the measurement and the design; this is the protocol.
///
/// Four calls, in this order, once per drawn frame:
///
/// 1. [`begin_frame`] — before anything on the route draws. Takes the frame's [`OWN_DAMAGE`] and
///    decides whether the snapshot still describes what is under the popover.
/// 2. [`page_pass`] — first thing inside the page-drawing closure, on BOTH passes (the visible one
///    and the direct blur source one). An RAII guard: it draws or arms as the snapshot allows, and
///    on drop takes the capture nobody else took.
/// 3. [`live`] — an RAII guard at the top of each popover's `draw` and `draw_scrim`.
/// 4. [`ground_drawn`] — `Popover::panel`/`sheet`, the moment the popover's own GROUND is down.
///
/// A popover drawn from inside its page (`tracks_panel`, `about_panel`, `alt_sources`,
/// `person_bio`) and one drawn after it (`item_menu`, `account_menu`) both work, and neither the
/// page nor `app.rs` has to know which is which.
///
/// ## The snapshot has TWO stages, and the second one is where the frames are
///
/// Freezing the page alone took the detail page under an open track panel from `draw≈75 ms` to
/// `draw≈58 ms` — real, and nowhere near a vsync slot. The class bisect on THAT build said why:
/// `drawmask=glass` 33 ms, `drawmask=rect` 33 ms. Two things left, ~25 ms each, and neither of them
/// is the page. They are the popover's own **full-screen modal scrim** (2 MP of SDF fill) and its
/// own **glass composite + frost** — redrawn on every presented frame, although once the appear
/// spring has settled NEITHER CHANGES. Only the popover's foreground does: the rows, the paged
/// body, the scroll rail.
///
/// So the snapshot grows to the whole composite below that foreground, and [`Held`] is which of the
/// two it holds:
///
/// - **[`Held::Page`]** — the undimmed host page, taken at the first [`live`] of a frame. Serves
///   the entry ramp (where the scrim really is changing, frame by frame) and the blur source pass.
/// - **[`Held::Ground`]** — page + scrim + the lifted [`Opener`](super::Opener) + the popover's own
///   ground, taken by [`ground_drawn`] on the first SETTLED frame. Serves every frame after that,
///   as one textured quad, with the freeze held right through the scrim and the panel ground and
///   lifted only for the foreground.
///
/// **One texture, re-taken, rather than two.** A second full-screen RGBA cache is 8.3 MB against a
/// `requiredMemory` budget two installs share, and it would buy nothing: the only thing that can
/// invalidate the ground is damage that invalidates the page too, at which point the page is
/// redrawn for real anyway.
///
/// **A blur source pass never sees [`Held::Ground`]**, and that is not a nicety: the ground quad
/// contains the panel's own frost, so a dynamic backdrop re-sourced from it would frost a picture
/// of itself, one refresh stale, forever. [`live`] and [`page_pass`] both fall back to the page
/// stage inside such a pass, and entering one drops the ground.
pub(crate) mod host {
    use super::{HOST_USERS, OWN_DAMAGE, OWN_MOTION};
    use std::sync::atomic::Ordering::Relaxed;

    /// What the one snapshot currently holds — see the module doc's two stages.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Held {
        /// Nothing usable: the next page draw is a real one.
        Nothing,
        /// The undimmed host page.
        Page,
        /// The host page, the scrim, the lifted opener and the popover's own ground.
        Ground,
    }

    /// The one snapshot. Main-render-thread only, like the `gfx` resource it holds.
    ///
    /// **One, shared, rather than one per popover**, and the memory is the smaller half of the
    /// argument: a full-screen RGBA texture is 8.3 MB on this panel and six of them would be a
    /// meaningful bite out of the app's `requiredMemory` budget. The larger half is that only one
    /// modal is ever up over one page, so a second cache could only ever hold a stale copy of a
    /// page some OTHER popover had already frozen — a second answer to a question with one answer.
    static mut CACHE: crate::gfx::FrameCache = crate::gfx::FrameCache::new();

    /// Which stage [`CACHE`] holds.
    static mut HELD: Held = Held::Nothing;

    /// Has the ground quad already gone down this frame? A popover reaches [`live`] twice (its
    /// scrim and its panel), and the quad is one full-screen draw that belongs to the frame rather
    /// than to either call.
    static GROUND_DRAWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// [`OWN_DAMAGE`] as taken by [`begin_frame`], readable for the rest of the frame.
    ///
    /// Taken ONCE and stored rather than swapped at each reader: the frame has two of them (the
    /// glass cadence through `Popover::prepare_present`, and this module's own invalidation) and a
    /// second `swap` would hand the second reader `false` for damage that really happened.
    static OWN_THIS_FRAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Is a capture owed before the next popover draws? Set by [`page_pass`] when it finds no
    /// snapshot, cleared by whoever takes it.
    static CAPTURE_OWED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// How many open popovers want a frozen host.
    fn users() -> u32 {
        unsafe { *std::ptr::addr_of!(HOST_USERS) }
    }
    /// **Input while a panel holds the page frozen belongs to the panel.** Opened by `app.rs`
    /// around every INPUT event (key, text, pointer, wheel — `app::is_input_event`) and every
    /// remote token: the per-event `idle::invalidate` and whatever the handler raises are then the
    /// panel's own damage rather than the page's, so a key-up under the account menu does not
    /// re-render Home behind it. NOT around lifecycle or window events, which are the app's and
    /// may change the page under the panel. `None` — no scope — once every holder is fading, when
    /// input has gone back to the page and its damage is the page's.
    pub(crate) fn input_scope() -> Option<crate::ui::idle::OwnScope> {
        (users() > 0 && !fading_only()).then(crate::ui::idle::OwnScope::open)
    }

    /// Is every one of them a dismissed panel still fading out? See [`super::host_refresh`].
    fn fading_only() -> bool {
        let n = users();
        n > 0 && unsafe { *std::ptr::addr_of!(super::HOST_CLOSING) } == n
    }

    /// Throw the snapshot away; the next page pass will draw the real page and take a new one.
    pub(crate) fn invalidate() {
        unsafe {
            (*std::ptr::addr_of_mut!(CACHE)).invalidate();
            HELD = Held::Nothing;
        }
    }

    /// Drop only the GROUND stage, keeping a page snapshot if there is one.
    ///
    /// Called when the popover's own ground is about to look different but the page under it has
    /// not moved: a blur source pass is running (the frost is being remade from a fresh sample), or
    /// `gfx::blur_invalidate` has decided the blur chain must be retaken. Without it the ground quad
    /// would keep showing the frost the panel had when it settled, which is invisible on a still
    /// page and wrong the moment anything re-blurs.
    pub(crate) fn ground_invalidate() {
        unsafe {
            if HELD == Held::Ground {
                (*std::ptr::addr_of_mut!(CACHE)).invalidate();
                HELD = Held::Nothing;
            }
        }
    }

    fn held() -> Held {
        unsafe { HELD }
    }

    /// The damage the open popovers caused this frame, as taken by [`begin_frame`].
    pub(crate) fn own_damage_this_frame() -> bool {
        OWN_THIS_FRAME.load(Relaxed)
    }

    /// Open the frame: take the popovers' own-damage ledger (for the glass) and the page's damage
    /// count (for the host), and decide whether the snapshot survives them.
    ///
    /// **The host-damage test is route-agnostic on purpose.** It is `idle::take_page_damage` —
    /// every invalidation this frame that was not raised inside a popover's own scope
    /// (`own_motion`, [`live`], [`input_scope`]) — plus, once every holder is fading, the page's
    /// motion (`app.rs`'s `underlay_moving`, which is threaded in as the one term this module
    /// cannot derive). Derived here rather than per screen, it covers the detail and person pages
    /// too, and it cannot go stale when a seventh screen learns to host a popover.
    ///
    /// **Ramping open is deliberately NOT a reason to re-capture**, although it is one for the
    /// dynamic backdrop (see [`glass_refresh`]). The two are asking about different pictures: the
    /// backdrop re-sources because the SCRIM over the page is still darkening, and the scrim is
    /// drawn live above this snapshot rather than into it. The page itself is not moving.
    pub(crate) fn begin_frame(page_moving: bool) {
        // Published for the renderer before anything draws: `gfx::page_wash_dither` reads it, so a
        // popover's own appear spring cannot strip the dither off the page snapshot under it. The
        // same OR `host_refresh` reads below: the scoped verdict app.rs threads in (Home, the
        // Library, Search, the press dip) plus the unscoped one (Detail updates outside
        // `scoped_motion` and reports through `idle::page_moving`) — Codex review, 2026-09-04.
        crate::ui::idle::note_underlay_motion(page_moving || crate::ui::idle::page_moving());
        let own = OWN_DAMAGE.swap(false, Relaxed);
        let own_motion = OWN_MOTION.swap(false, Relaxed);
        // The glass backdrop's ledger stays MERGED: re-sourcing a blur one frame late self-heals on
        // the next damage, and the dynamic cadence reads it through `Popover::prepare_present`.
        OWN_THIS_FRAME.store(own || own_motion, Relaxed);
        // Taken every drawn frame, holder or not, so the count never carries over into the first
        // frame of the next panel to open.
        let page_dirty = crate::ui::idle::take_page_damage();
        CAPTURE_OWED.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
        if users() == 0 {
            invalidate();
            return;
        }
        if super::host_refresh(
            fading_only(),
            page_dirty,
            crate::ui::idle::page_moving() || page_moving,
        ) {
            invalidate();
        }
    }

    /// One draw of the host page, visible or blur-source. Hold the guard for the length of the
    /// page closure; dropping it lifts the freeze and takes the capture nobody else took.
    pub(crate) struct PagePass {
        was_frozen: bool,
        /// **Damage the page raises while being drawn into (or under) the snapshot is not new
        /// damage.** A spinner on the host reports through `idle::invalidate` from its own `draw`
        /// to keep itself animating; on a frame the page is drawn for real (the refresh frame,
        /// with the freeze off) that report would otherwise count as page damage and buy another
        /// refresh, and the next, for as long as the spinner is on screen — the account menu over
        /// a Home still waiting on a hub re-rendered Home on every frame (26 fps, 2026-09-04). The
        /// picture the spinner reported from IS the snapshot being taken, so the snapshot pauses
        /// it, which is what the freeze has always done to a decoration nobody can reach.
        #[allow(dead_code)]
        own: Option<crate::ui::idle::OwnScope>,
    }

    /// Begin a host-page draw. Draws the cached quad and arms the freeze when there is a snapshot;
    /// otherwise leaves the page to draw itself and books the capture that must follow it.
    pub(crate) fn page_pass() -> PagePass {
        if users() == 0 {
            return PagePass {
                was_frozen: crate::gfx::page_frozen(),
                own: None,
            };
        }
        // A source pass may never be served the GROUND stage — it would hand a dynamic backdrop a
        // picture of its own frost. Drop it and let the page draw for real; the visible pass will
        // take a new one.
        if crate::gfx::blur_source_pass() {
            ground_invalidate();
        }
        let served = match held() {
            // The ground quad is one draw for the whole frame and belongs to the first `live`,
            // which is also where the freeze has to still be armed. Nothing is drawn here.
            Held::Ground => true,
            // `FrameCache::draw` lifts the freeze around its own quad, so this is correct whether
            // or not an outer pass has one armed.
            Held::Page => unsafe { (*std::ptr::addr_of!(CACHE)).draw() },
            Held::Nothing => false,
        };
        if !served {
            CAPTURE_OWED.store(true, Relaxed);
        }
        PagePass {
            was_frozen: crate::gfx::set_page_frozen(served),
            own: Some(crate::ui::idle::OwnScope::open()),
        }
    }

    impl Drop for PagePass {
        /// **A `Drop` guard and not straight-line code**, for `gfx::DirectPass`'s reason: `home_draw`
        /// opens with `ui::guard`, which CATCHES a panic and returns normally. A page that panicked
        /// with the freeze armed would otherwise leave every later frame refusing every quad — a
        /// frozen picture with no crash, no log line and no way back.
        fn drop(&mut self) {
            crate::gfx::set_page_frozen(self.was_frozen);
            // Nobody lifted: this page's popovers draw AFTER the closure (`item_menu`,
            // `account_menu`). The framebuffer holds the completed undimmed page, which is exactly
            // what the snapshot is.
            if CAPTURE_OWED.swap(false, Relaxed) {
                unsafe { (*std::ptr::addr_of_mut!(CACHE)).capture() };
            }
        }
    }

    /// Everything drawn while this guard is alive is LIVE over the frozen host — one line at the
    /// top of every popover's `draw` and `draw_scrim`, held for the body.
    ///
    /// **Constructing it is also what defines the snapshot.** On the first one of a frame in which
    /// a capture is owed, it takes that capture before lifting anything, and that instant is exactly
    /// "the host page is complete and no popover has put a pixel on the framebuffer yet" — so what
    /// lands in the texture is the undimmed page and nothing else. The scrim the guard's owner is
    /// about to draw, and the [`Opener`](super::Opener) lift that rides with it, therefore stay LIVE
    /// above the quad on every later frame. They must: the scrim ramps with the appear spring, and a
    /// lift baked into the snapshot would be dimmed by the live scrim drawn over it — which is the
    /// very bug the lift exists to fix.
    ///
    /// A guard rather than a `FnOnce` for two reasons: a panel's draw body is a hundred lines with
    /// several early returns, and `ui::guard` catches panics, so an unwind that skipped the restore
    /// would leave every later frame refusing every quad.
    ///
    /// Cheap and correct when nothing is frozen, so a panel constructs it unconditionally.
    #[must_use = "the freeze is lifted only for this guard's lifetime"]
    pub(crate) struct Live {
        was_frozen: bool,
        /// Everything drawn under this guard is the panel's own: an `idle::invalidate` raised by
        /// its marquee or spinner must not read as page damage to `begin_frame`.
        #[allow(dead_code)]
        own: crate::ui::idle::OwnScope,
    }

    /// See [`Live`].
    pub(crate) fn live() -> Live {
        if crate::gfx::blur_source_pass() {
            ground_invalidate();
        }
        if held() == Held::Ground {
            // STAGE TWO: the scrim, the lifted opener and the panel's own ground are all in this
            // one quad. Stay FROZEN through them — `Popover::panel` lifts for the foreground.
            if !GROUND_DRAWN.swap(true, Relaxed) {
                unsafe { (*std::ptr::addr_of!(CACHE)).draw() };
            }
            return Live {
                was_frozen: crate::gfx::set_page_frozen(true),
                own: crate::ui::idle::OwnScope::open(),
            };
        }
        if CAPTURE_OWED.swap(false, Relaxed) {
            // Refused during a blur source pass (the framebuffer is a small FBO, not the page).
            // `page_pass` books it again on the visible pass, so nothing is lost.
            if unsafe { (*std::ptr::addr_of_mut!(CACHE)).capture() } {
                unsafe { HELD = Held::Page };
            } else {
                CAPTURE_OWED.store(true, Relaxed);
            }
        }
        Live {
            was_frozen: crate::gfx::set_page_frozen(false),
            own: crate::ui::idle::OwnScope::open(),
        }
    }

    /// **The popover's own GROUND is now on the framebuffer** — called by `Popover::panel` and
    /// `Popover::sheet`, which is the one line every panel in the app draws between its ground and
    /// its foreground.
    ///
    /// Two jobs, exactly one of which runs:
    ///
    /// - serving the ground stage, the freeze is still armed (the scrim and the ground it covered
    ///   drew nothing) — LIFT it, because everything after this point is the popover's live
    ///   foreground;
    /// - not serving it yet, and this popover has SETTLED — take the ground snapshot. The
    ///   framebuffer at this instant is exactly page + scrim + lifted opener + this ground, which
    ///   is the composite every later frame can be served from.
    ///
    /// `settled` is the caller's `appear_settled`: capturing during the entry ramp would freeze a
    /// half-faded panel over a half-dimmed page for the rest of the session.
    pub(crate) fn ground_drawn(settled: bool) {
        if crate::gfx::page_frozen() {
            crate::gfx::set_page_frozen(false);
            return;
        }
        if settled && held() == Held::Page && !crate::gfx::blur_source_pass() {
            if unsafe { (*std::ptr::addr_of_mut!(CACHE)).capture() } {
                unsafe { HELD = Held::Ground };
            }
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            crate::gfx::set_page_frozen(self.was_frozen);
        }
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
        assert!(
            !Glass::CACHED.needs_page_scrim(),
            "a cached grab already contains its own scrim"
        );
        assert!(
            Glass::DYNAMIC_BACKDROP.needs_page_scrim(),
            "a refreshing backdrop re-renders the page, which must therefore carry the dim"
        );
    }

    /// **The FPS regression, as a pure decision.** Moved here from `person_bio` on 2026-09-02 with
    /// the function it grades, unchanged; it was confirmed red against the code it replaced before
    /// that fix landed, which is a claim about the ORIGINAL commit and not about this move.
    ///
    /// The bug: `prepare_present` used to hand `underlay_changed` straight to the glass policy, so
    /// a settled panel with `underlay_changed == true` — the panel's OWN page turn, misread as page
    /// motion by the process-wide `present_dirty` flag — forced a re-source on every such keypress.
    /// [`glass_refresh`] subtracts the panel's own activity from the caller's flag once the appear
    /// ramp has finished, and still refreshes for damage the panel did NOT cause (a shelf landing,
    /// a poster texture) rather than freezing the frost at its open frame.
    ///
    /// It now grades the HOST CACHE as well, which reads the same predicate with `appear_settled`
    /// pinned true — see `host::begin_frame`.
    #[test]
    fn the_backdrop_refreshes_while_ramping_or_for_host_damage_never_for_the_panels_own_scroll() {
        assert!(
            glass_refresh(false, false, false),
            "still ramping open — must refresh even with nothing reported changed"
        );
        assert!(
            glass_refresh(true, false, true),
            "ramping AND the page changed — still a refresh, whoever caused it"
        );
        assert!(
            !glass_refresh(true, true, true),
            "settled — the caller's flag carries this panel's own scroll or page turn, so it must not refresh"
        );
        assert!(
            !glass_refresh(false, true, false),
            "settled and nothing reported — no refresh"
        );
        assert!(
            glass_refresh(true, true, false),
            "settled and the HOST changed (a shelf landed, a poster arrived) — the frost must follow it"
        );
    }

    /// **The host snapshot's lifetime, as the same pure decision at `appear_settled = true`.**
    ///
    /// Split out from the case above because the two consumers differ on exactly one input and the
    /// difference is deliberate: a panel still RAMPING open must re-source its backdrop (the scrim
    /// beneath it is still darkening) but must NOT re-capture its host (the page is not moving, and
    /// the scrim is drawn live above the snapshot rather than into it). Getting this wrong is a
    /// full page redraw for every frame of every panel's entry animation, which is invisible in a
    /// screenshot and costs exactly the frames this mechanism exists to save.
    #[test]
    fn the_host_snapshot_survives_the_entry_ramp_and_the_panels_own_paging() {
        let held = |changed, own| glass_refresh(changed, true, own);
        assert!(
            !held(false, false),
            "settled page, nothing reported — the snapshot stands"
        );
        assert!(
            !held(true, true),
            "the panel's own page turn set the process-wide flag — not a reason to re-capture"
        );
        assert!(
            held(true, false),
            "the HOST changed — the snapshot is stale and must be retaken"
        );
    }

    /// **A fresh open is not settled, and stepping the appear spring to rest is.** The one caller
    /// (`Popover::prepare_present`) uses this to tell "still ramping open" from "at rest with
    /// only my own foreground moving" — a popover whose spring never reaches this true would keep
    /// re-sourcing its backdrop forever, which is the FPS regression this predicate exists to end.
    #[test]
    fn appear_settled_is_false_on_open_and_true_once_the_spring_arrives() {
        // `open`/`close` touch the shared `OPEN_COUNT` static — the same reason the round-trip
        // test below takes this lock, and for the same reason this one must not skip it: two
        // popovers opening at once on different threads would otherwise race that counter.
        let _g = crate::testlock::serial();
        let mut pop = Popover::with_glass(Glass::DYNAMIC_BACKDROP);
        pop.open();
        assert!(!pop.appear_settled(), "a fresh open has not ramped in yet");
        for _ in 0..240 {
            pop.update(1.0 / 60.0);
        }
        assert!(
            pop.appear_settled(),
            "four seconds at K_APPEAR must have settled the spring"
        );
        pop.close();
    }

    /// **The exit is the entry played backwards, and it is drawn.** `dismiss` ends the panel's
    /// OPEN state on the press frame — keys return to the page; a caching panel's host stays frozen
    /// through the fade, see the test two below — but the panel stays
    /// VISIBLE while the appear spring runs back to 0, reporting every frame of it as its own
    /// damage so the present gate keeps presenting. `close` stays the instant hide. Settings' exit
    /// was the report (2026-09-02); every popover took the mechanism.
    #[test]
    fn dismiss_fades_out_over_frames_while_close_hides_at_once() {
        let _g = crate::testlock::serial();
        let mut pop = Popover::new();
        pop.open();
        for _ in 0..240 {
            pop.update(1.0 / 60.0);
        }
        assert!(pop.appear_settled());
        pop.dismiss();
        assert!(!pop.is_open(), "input modality ends on the press frame");
        assert!(pop.visible(), "…but the panel is still drawn");
        assert!(pop.appear() > 0.99, "and starts its fade from where it stood");
        let mut frames = 0;
        let mut last = pop.appear();
        while pop.visible() {
            pop.update(1.0 / 60.0);
            frames += 1;
            assert!(pop.appear() <= last + 1e-4, "the fade is monotone");
            last = pop.appear();
            assert!(frames < 240, "a dismiss must settle within four seconds");
        }
        assert!(frames >= 3, "a dismiss is a fade, not a cut: {frames} frame(s)");
        assert_eq!(pop.appear(), 0.0);
        assert!(!pop.visible() && !pop.is_open());
        assert!(!any_open(), "the open count was released on the press frame, not at the end");

        // the instant hide, for a panel whose subject just vanished
        pop.open();
        pop.close();
        assert!(!pop.visible() && !pop.is_open());
        assert_eq!(pop.appear(), 0.0);
        // and a dismiss on a closed panel is a no-op rather than a second decrement
        pop.dismiss();
        assert!(!pop.visible() && !any_open());
    }

    /// **The host-cache registry, through the four sequences that leak it** — the same round trip
    /// [`any_open`]'s counter is pinned through, and for a worse failure: an over-count leaves the
    /// page frozen after every popover has closed, i.e. a screen that stops repainting and answers
    /// no keys visibly, with nothing in any log.
    ///
    /// It also pins the half that is easy to get wrong the other way: a `Live` popover must not
    /// register at all, however many times it is opened.
    #[test]
    fn only_a_caching_popover_registers_a_frozen_host_and_the_count_round_trips() {
        let _g = crate::testlock::serial();
        let base = unsafe { *std::ptr::addr_of!(HOST_USERS) };
        let users = || unsafe { *std::ptr::addr_of!(HOST_USERS) } - base;

        let mut live = Popover::new();
        let mut cached = Popover::new().caching_host();
        assert_eq!(live.host, HostPolicy::Live, "the default draws its page");
        assert_eq!(cached.host, HostPolicy::Cached);

        live.open();
        live.open();
        assert_eq!(users(), 0, "a Live popover never freezes anything");

        cached.open();
        cached.open(); // a re-open restarts the motion, it does not open a second panel
        assert_eq!(users(), 1, "a re-open must not count twice");

        cached.close();
        cached.close(); // the defensive close every dismissal arm makes
        assert_eq!(users(), 0, "closing an already-closed panel must not decrement");

        live.close();
        assert_eq!(users(), 0);
    }

    #[test]
    fn cached_is_the_default_and_dynamic_glass_requires_an_explicit_opt_in() {
        let _g = crate::testlock::serial();
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
        assert_eq!(
            count(),
            1,
            "closing an already-closed panel must not decrement"
        );
        assert!(any_open(), "the other panel is still up");

        a.close();
        assert_eq!(count(), 0);
        assert!(
            !any_open(),
            "back to rest — the tab bar resumes retaking its backdrop"
        );
    }

    /// **A dismissed CACHED panel keeps its page frozen for the length of its fade and lets go when
    /// the fade lands; `close` lets go at once; neither path can leak the count.** The fade is drawn
    /// over a page that has not changed, so releasing on the press frame put every frame of it on
    /// the live path — the laggy dismissal every cached panel had.
    #[test]
    fn a_cached_popover_holds_its_frozen_host_through_the_fade_and_releases_at_the_end() {
        let _g = crate::testlock::serial();
        let base = unsafe { *std::ptr::addr_of!(HOST_USERS) };
        let closing_base = unsafe { *std::ptr::addr_of!(HOST_CLOSING) };
        let users = || unsafe { *std::ptr::addr_of!(HOST_USERS) } - base;
        let mut pop = Popover::new().caching_host();
        pop.open();
        for _ in 0..240 {
            pop.update(1.0 / 60.0);
        }
        assert_eq!(users(), 1);
        pop.dismiss();
        assert!(!pop.is_open() && pop.visible());
        assert_eq!(users(), 1, "the page under a fading panel has not changed: keep it frozen");
        let mut frames = 0;
        while pop.visible() {
            assert_eq!(users(), 1, "held for the whole fade (frame {frames})");
            pop.update(1.0 / 60.0);
            frames += 1;
            assert!(frames < 240, "a dismiss must settle within four seconds");
        }
        assert_eq!(users(), 0, "the fade landed: the page is live again");

        // a re-open DURING its own fade must not count twice — settle first, because a dismiss
        // straight after an open has a zero-length fade (the spring is still at 0)
        let settle = |pop: &mut Popover| {
            for _ in 0..240 {
                pop.update(1.0 / 60.0);
            }
        };
        let closing = || unsafe { *std::ptr::addr_of!(HOST_CLOSING) } - closing_base;
        pop.open();
        settle(&mut pop);
        assert_eq!((users(), closing()), (1, 0));
        pop.dismiss();
        pop.update(1.0 / 60.0);
        assert_eq!((users(), closing()), (1, 1), "a fading holder is counted as one");
        pop.open();
        assert_eq!(
            (users(), closing()),
            (1, 0),
            "re-opened mid-fade: still one user, and no longer a fading one"
        );
        // …and the instant hide releases at once, from the open state and from mid-fade
        pop.close();
        assert_eq!((users(), closing()), (0, 0));
        pop.open();
        settle(&mut pop);
        pop.dismiss();
        pop.update(1.0 / 60.0);
        assert_eq!((users(), closing()), (1, 1));
        pop.close();
        assert_eq!(
            (users(), closing()),
            (0, 0),
            "close during a fade releases the held user and its closing count"
        );
        pop.close();
        pop.dismiss();
        assert_eq!((users(), closing()), (0, 0), "redundant close/dismiss decrement nothing");
        // the fade running out releases both counts too
        pop.open();
        settle(&mut pop);
        pop.dismiss();
        while pop.visible() {
            pop.update(1.0 / 60.0);
        }
        assert_eq!((users(), closing()), (0, 0));

        // a Live panel never registers, through the same sequence
        let mut live = Popover::new();
        live.open();
        settle(&mut live);
        live.dismiss();
        live.update(1.0 / 60.0);
        live.close();
        assert_eq!((users(), closing()), (0, 0));
    }

    /// [`host_refresh`]'s truth table: page DAMAGE redraws the host in either state; page MOTION
    /// only once every holder is fading (under an open panel it is decoration, meant to pause —
    /// read as a reason it cost the account menu 26 fps). The panel's own motion and damage never
    /// reach it: `idle::page_damage_is_what_no_popover_claimed` grades that attribution.
    #[test]
    fn the_host_redraws_on_page_damage_and_on_page_motion_only_under_a_fade() {
        assert!(!host_refresh(false, false, false));
        assert!(host_refresh(false, true, false), "a page event redraws the host");
        assert!(
            !host_refresh(false, false, true),
            "page motion under an OPEN panel is a decoration the freeze pauses"
        );
        assert!(host_refresh(true, false, true), "page motion under a fade is the user scrolling");
        assert!(host_refresh(true, true, false));
        assert!(!host_refresh(true, false, false));
    }

    /// **A popover drawn on `visible()` must be updated on `visible()` too.** Reported off a
    /// television on 2026-09-03 as "popover menus do not hide and may stack up".
    ///
    /// [`Popover::dismiss`] ends the OPEN state on the press frame and leaves `closing` set, and
    /// [`Popover::update`] is the only place `closing` is ever cleared. So a panel that stops being
    /// updated the moment it stops being OPEN never finishes its fade: `visible()` stays true for
    /// the rest of the session, the draw site keeps drawing it at full opacity, and every panel
    /// opened afterwards piles on top of it.
    ///
    /// `account_menu` and `item_menu` are the two popovers that are also ROUTES, so dismissing one
    /// flips `route` back to its host page on the same frame — and `app.rs` guarded their `update`
    /// with `matches!(route, …)` while drawing them self-gated. The draw site already carried the
    /// rule in a comment ("Both self-gated on `Popover::visible`, not on the route: a dismissed
    /// menu's route flips back to its host on the press frame while the panel is still fading out
    /// over it"); the update site did not obey it. Both modules already return early unless
    /// `visible()`, so the route guard bought nothing and cost the fade.
    ///
    /// **The player's four overlays are deliberately not in this list.** They gate DRAW and UPDATE
    /// on the same `Route::Player { overlay }` term, which is coherent: a stuck one could never be
    /// drawn off its own overlay route, so it can neither linger nor stack. The defect is the
    /// MISMATCH between the two gates, not a route gate as such — which is why this test lists the
    /// popovers whose draw is self-gated rather than every popover in the app.
    ///
    /// Asserted by reading `app.rs`, because the frame loop is reachable from no unit test — the
    /// same reason `diag::scrub` pins its call-site property by grepping the tree.
    #[test]
    fn a_self_gated_popover_is_not_route_gated_on_update() {
        let _g = crate::testlock::serial();
        let app = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app.rs");
        let text = std::fs::read_to_string(&app).expect("app.rs is readable");
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.len() > 1000,
            "read only {} lines of app.rs — not reading the frame loop",
            lines.len()
        );

        // The popovers `app.rs` draws WITHOUT a route term, gated only on `Popover::visible`.
        const SELF_GATED: &[&str] = &[
            "account_menu",
            "item_menu",
            "legal",
            "consent",
            "settings",
        ];

        let mut offences: Vec<String> = Vec::new();
        let mut seen = 0usize;
        for (n, line) in lines.iter().enumerate() {
            let Some(m) = SELF_GATED
                .iter()
                .find(|m| line.contains(&format!("ui::{m}::update(")))
            else {
                continue;
            };
            seen += 1;
            // Join the preceding non-comment lines so a `matches!` split across several of them —
            // which is how the player's overlay guards are written — is still seen as one guard.
            let preceding: String = lines[..n]
                .iter()
                .rev()
                .take(8)
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with("//"))
                .collect::<Vec<_>>()
                .join(" ");
            if preceding.contains("matches!(") && preceding.contains("route") {
                offences.push(format!(
                    "app.rs:{}: {m}::update is gated on a route, but {m}::draw is not\n    {}",
                    n + 1,
                    line.trim()
                ));
            }
        }
        assert!(
            seen >= SELF_GATED.len(),
            "found only {seen} of {} self-gated popover update call sites — the scan is not \
             finding them",
            SELF_GATED.len()
        );
        assert!(
            offences.is_empty(),
            "a dismissed popover stops being updated when its route flips, so its fade never \
             finishes and it never hides:\n{}",
            offences.join("\n")
        );
    }
}
