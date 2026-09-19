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
//! its page (`about_panel` and `person_bio` did, at the tail of `detail::draw` / `person::draw`,
//! until phase 10 made both surfaces; `decision_alert` still does) cannot be served by "skip the
//! page and draw the panel after it". So the freeze is a REFUSAL at the renderer's one shared gate rather than a skipped call
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
//! - **A panel's ground reads no framebuffer at all.** It is the latched underlay field (the one
//!   the modal dim draws, `ui::underlay`) sampled at the panel's own rect under the frost — see
//!   [`crate::ui::widgets::panel_ground`] — so nothing about the snapshot constrains it, and nothing
//!   about it constrains the snapshot.
//! - **HOST DAMAGE refreshes the snapshot; the popover's OWN activity does not.** That distinction
//!   was the bio panel's private `OWN_DAMAGE` ledger and is now [`own_motion`] / [`host::live`] /
//!   [`host::input_scope`] / [`host::page_pass`], shared — attributed at the source and counted
//!   (`idle::take_page_damage`), with [`host_refresh`] as the decision.
use crate::ui::{theme, Painter, Rect, Spring};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// Stiffness of the appear spring — the panels' shared open-motion constant, and the stiffness every
/// other fade-into-place in the UI matches (the tab capsules' alpha, [`crate::ui::widgets::TabStrip`]).
/// `pub(crate)` for exactly that reason: a fade that is not a panel still belongs to the same family,
/// and re-typing 300 somewhere else is how two "shared" motions drift apart.
pub(crate) const K_APPEAR: f32 = 300.0;

/// How many panels are open right now, across the whole app — see [`any_open`].
///
/// A safe atomic rather than the `static mut` + raw-pointer form this counter used until phase 12
/// (`ci/allow/statics-migration.txt`'s "the modal phase's last legacy owner"): every writer here is
/// already a narrow, audited door — [`surface_held`]/[`surface_closing`]/[`surface_released`] (fed
/// by `app/bridge.rs`, which is itself reading every `ModalStack`'s own phase — there is no SINGLE
/// `ModalStack` instance this could become a field of, since the container tree holds one per
/// screen) and [`Popover`]'s own `open_inner`/`release`/`hold_host`/`release_host`/`enter_closing`/
/// `leave_closing`, the last direct caller being `ui::decision_alert`'s embedded panel. `Relaxed` is
/// exact: every access is from the main render thread (or, in a host test, serialized behind
/// `testlock::serial()`), so this buys memory safety over the old unsafe pointer arithmetic with no
/// behaviour change — same counter, same call sites, same values.
static OPEN_COUNT: AtomicU32 = AtomicU32::new(0);

/// Is ANY modal panel up? The one question a screen's CHROME has to ask, and the reason it is a
/// counter here rather than a bool threaded through three `draw_tab_row` call sites: the panels
/// that cover the top bar belong to four different modules and two of them are ROUTES
/// (the two menus, both `ModalStack` surfaces since phase 10), so no screen can enumerate the ones
/// drawn over it. Every panel in
/// the app goes through [`Popover::open`]/[`Popover::close`], so registering there is exact, costs
/// nothing, and a panel added tomorrow is counted without touching this.
///
/// Its user is the glass tab bar. A modal disables that bar's backdrop blur: the bar sits under the
/// dim, and a live blur of a page the modal has frozen is a full-screen source pass bought for a
/// strip nobody is looking at.
pub(crate) fn any_open() -> bool {
    OPEN_COUNT.load(Relaxed) > 0
}

/// **A dispatcher-owned surface's share of the counters** (restructure phase 5b). A surface on
/// the container tree has no `Popover` — its PHASE is the `ModalStack`'s — but the page under it
/// is still the legacy frame's, and these three counters are what that frame reads to freeze,
/// cache and un-glass it. The bridge (`app/bridge.rs`) drives them from the surface phases it
/// observes after every dispatcher frame: `Opening|Open` → held, `Closing` → held and closing,
/// gone → released. Exactly [`Popover::open`]/`dismiss`/`release_host`'s arithmetic, exposed
/// for a caller that keeps its phase elsewhere.
///
/// `cached` is **`modal::style_caches_host`** — the container's own policy table, asked rather
/// than restated. This line used to name the styles instead ("`Style::Sheet`/`Opaque { snapshot:
/// true }`, a Compact surface holds no snapshot"), and the bridge's `matches!` agreed with it and
/// disagreed with `surface_policy`, whose `(Style::Compact, _) => (U::Live, R::Cached)` had been
/// there all along. What that cost is in `style_caches_host`'s doc.
pub(crate) fn surface_held(cached: bool) {
    OPEN_COUNT.fetch_add(1, Relaxed);
    if cached {
        HOST_USERS.fetch_add(1, Relaxed);
    }
    host::invalidate();
}

/// The surface's fade-out began (`Phase::Closing`): the page under it is live to input again.
pub(crate) fn surface_closing(cached: bool) {
    if OPEN_COUNT.load(Relaxed) > 0 {
        OPEN_COUNT.fetch_sub(1, Relaxed);
    }
    if cached {
        HOST_CLOSING.fetch_add(1, Relaxed);
    }
    crate::ui::idle::invalidate();
}

/// The surface left for good: release its host user (and its closing mark, if it was fading).
pub(crate) fn surface_released(cached: bool, was_closing: bool) {
    if !was_closing && OPEN_COUNT.load(Relaxed) > 0 {
        OPEN_COUNT.fetch_sub(1, Relaxed);
    }
    if cached {
        if was_closing && HOST_CLOSING.load(Relaxed) > 0 {
            HOST_CLOSING.fetch_sub(1, Relaxed);
        }
        if HOST_USERS.load(Relaxed) > 0 {
            HOST_USERS.fetch_sub(1, Relaxed);
        }
    }
    host::invalidate();
}

/// [`HOST_USERS`], for a test that has to grade the counter from another module — `app/bridge.rs`,
/// whose `sync_host` is the only other caller that takes and releases one. Read as a DELTA against
/// a base taken at the top of the test: the counter is process-wide and `testlock::serial()` bounds
/// interleaving, not what a previous test in the same process left behind.
#[cfg(test)]
pub(crate) fn host_users_for_test() -> u32 {
    HOST_USERS.load(Relaxed)
}

/// How many OPEN popovers have asked for a cached host — see [`Popover::caching_host`].
///
/// A separate counter from [`OPEN_COUNT`] and not a filter over it, for the same reason that one is
/// a counter: the popovers live in seven modules and two of them are routes, so nothing can
/// enumerate them. Reaching zero is what puts the page back on the live path.
static HOST_USERS: AtomicU32 = AtomicU32::new(0);
/// How many of [`HOST_USERS`] are panels on their way OUT — dismissed, fading, still holding the
/// freeze (see `Popover::release_host`). When every user is one of these the page under the
/// snapshot is live to input again, and its MOTION becomes a reason to redraw it
/// ([`host_refresh`]) — under an open panel only its damage is.
static HOST_CLOSING: AtomicU32 = AtomicU32::new(0);

/// Attribute the spring motion of a popover's own `update` to the POPOVER rather than to the page
/// it stands on — one line at the top of that `update`, held for the body.
///
/// This is [`crate::ui::idle::MotionScope`], so the panel's springs never read as the PAGE's
/// (`idle::PAGE_MOVING`, which [`host_refresh`] asks for a fading panel), plus an
/// [`crate::ui::idle::OwnScope`] so that every `idle::invalidate` the update raises (the appear
/// spring's, a marquee's) is the panel's own damage rather than the page's. Said once here so
/// every panel's `update` is one line rather than six, and so that a panel added tomorrow inherits
/// it.
///
/// **The scope merges back** on drop, so a popover's motion still keeps the present gate awake —
/// this changes who the motion is ATTRIBUTED to, never whether it counts as motion.
#[must_use = "the scope is only open for this guard's lifetime"]
pub(crate) struct OwnMotion(
    #[allow(dead_code)] crate::ui::idle::MotionScope,
    #[allow(dead_code)] crate::ui::idle::OwnScope,
);

/// See [`OwnMotion`].
pub(crate) fn own_motion() -> OwnMotion {
    OwnMotion(
        crate::ui::idle::MotionScope::open(),
        crate::ui::idle::OwnScope::open(),
    )
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
}
impl Popover {
    pub(crate) const fn new() -> Self {
        Popover {
            open: false,
            closing: false,
            appear: Spring::at(0.0),
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
            OPEN_COUNT.fetch_add(1, Relaxed);
            self.hold_host();
        }
        self.open = true;
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
    /// The bookkeeping half of a close: input modality ends NOW, whether the
    /// panel then vanishes or fades. The host freeze is NOT in here — see
    /// [`release_host`](Self::release_host) for why it outlives a dismiss.
    fn release(&mut self) {
        if self.open {
            OPEN_COUNT.fetch_sub(1, Relaxed);
        }
        self.open = false;
        host::invalidate();
    }
    /// Register this popover's frozen-host user — once, however many times it is (re)opened, and
    /// not at all under a `Live` policy. A re-open DURING the fade-out finds the user still held
    /// and must not count it twice.
    fn hold_host(&mut self) {
        if self.host == HostPolicy::Cached && !self.host_held {
            HOST_USERS.fetch_add(1, Relaxed);
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
            HOST_USERS.fetch_sub(1, Relaxed);
            self.host_held = false;
            host::invalidate();
        }
    }
    /// Count this held user among the fading ones — see [`HOST_CLOSING`]. A no-op for a panel that
    /// holds no user, or is already counted.
    fn enter_closing(&mut self) {
        if self.host_held && !self.host_closing {
            HOST_CLOSING.fetch_add(1, Relaxed);
            self.host_closing = true;
        }
    }
    fn leave_closing(&mut self) {
        if self.host_closing {
            HOST_CLOSING.fetch_sub(1, Relaxed);
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
    pub(crate) fn update(&mut self, dt: f32) {
        if self.open {
            self.step_appear(1.0, dt);
        } else if self.closing {
            self.step_appear(0.0, dt);
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
    /// Its callers: [`ground_done`](Self::ground_done), which captures the host's GROUND stage only
    /// once the ramp is over, and `DecisionAlert::settled`, which gates pointer hits on it.
    pub(crate) fn appear_settled(&self) -> bool {
        self.appear.pos >= 0.999
    }

    /// The panel painter without a full-screen scrim. Pair with [`scrim`](Self::scrim), for a
    /// caller that draws the dim as part of its host page — see that method for why one does.
    pub(crate) fn content_painter(&self, rise: f32) -> Painter {
        let a = self.appear();
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
    /// 20 is what *Also available* has always drawn. It is written down because the four alert panels
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

    /// Draw the panel's GROUND at `r` with corner `rad` through [`widgets::panel_ground`](crate::ui::widgets::panel_ground)
    /// — the latched underlay field under the frost where `field` has one, the flat panel
    /// material where it does not — then report the ground done to [`host`].
    ///
    /// Call it as the FIRST thing drawn through the content painter: everything after it is the
    /// popover's moving foreground, which is what [`ground_done`](Self::ground_done) marks.
    ///
    /// **Not for the player's panels.** Behind those is punch-through alpha to the hardware video
    /// plane, which GL cannot read; they keep `p.rect(…, PANEL_TOP, PANEL_BOT, …)` on purpose.
    pub(crate) fn panel(
        &self,
        p: Painter,
        r: Rect,
        rad: f32,
        field: Option<&crate::ui::underlay::UnderlayField>,
    ) {
        crate::ui::widgets::panel_ground(p, r, rad, field);
        self.ground_done();
    }

    /// The boundary between this popover's GROUND and its FOREGROUND, reported to [`host`].
    ///
    /// It hangs off [`panel`](Self::panel) rather than being a call every screen has to remember,
    /// because that method IS the boundary: everything after it is the moving part.
    ///
    /// Inert for a `Live` host: a popover that does not freeze its page has no ground stage, and
    /// the player's overlays must never reach one — behind them is punch-through alpha to a
    /// hardware plane, which GL cannot read back at all.
    fn ground_done(&self) {
        if self.host == HostPolicy::Cached {
            host::ground_drawn(self.appear_settled());
        }
    }

}

/// **Give back the three global counters, for a panel that is dropped while still up.**
///
/// [`OPEN_COUNT`], [`HOST_USERS`] and [`HOST_CLOSING`] are process-globals that a `Popover`
/// INCREMENTS on `open` and owes back on `close`. For most of this app's life nothing could
/// default on that debt, because every popover was a `static mut` that lives as long as the
/// process — which is why this impl did not exist and was not missed.
///
/// **Restructure phase 5b made popovers droppable, and with them this leak reachable.** An owned
/// screen (`screens::consent`'s `ConsentPage`, which holds a `DecisionAlert`, which holds one of
/// these) lives inside a `ModalStack` entry and is DROPPED when its surface is torn down. So:
/// open Privacy & data, press "Delete all local data?", then dismiss the whole Settings surface
/// while that alert is up — BACK at the family's root, or the loop's `dismiss_surfaces_now` on an
/// app-switch — and the page is dropped with `open == true`. `OPEN_COUNT` never returns to zero,
/// and since its one consumer is [`any_open`], the glass tab bar concludes a modal is up and stops
/// drawing its backdrop **for the rest of the session**, with nothing in any log.
///
/// `close` rather than `dismiss` is the right call here, and deliberately: a dismiss starts a FADE,
/// and there is nothing left to fade — the object is going away this instant. `close` is already
/// the "release everything now" path and is a no-op on a panel that holds nothing, so this is
/// exact for the common case of a closed popover being dropped.
///
/// The host suite found it as cross-test pollution three modules away: `screens::consent`'s two
/// alert tests end with the alert open, and `ui::popover`'s counter tests then failed on
/// `!any_open()` — passing alone, failing in a full run.
impl Drop for Popover {
    fn drop(&mut self) {
        self.close();
    }
}

/// **The shared host snapshot** — one page-sized texture, one lifetime, every popover that asked
/// for it. See this module's own doc for the measurement and the design; this is the protocol.
///
/// Four calls, in this order, once per drawn frame:
///
/// 1. [`begin_frame`] — before anything on the route draws. Takes the page's damage count and
///    decides whether the snapshot still describes what is under the popover.
/// 2. [`page_pass`] — first thing inside the page-drawing closure, on BOTH passes (the visible one
///    and the direct blur source one). An RAII guard: it draws or arms as the snapshot allows, and
///    on drop takes the capture nobody else took.
/// 3. [`live`] — an RAII guard at the top of each popover's `draw` and `draw_scrim`.
/// 4. [`ground_drawn`] — `Popover::panel`, the moment the popover's own GROUND is down.
///
/// A popover drawn from inside its page (`decision_alert`; `about_panel` and `person_bio` were the
/// other two until phase 10) and one drawn after it (the container's surfaces) both work, and
/// neither the page nor `app.rs` has to know which is which.
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
    use super::HOST_USERS;
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

    /// The page being drawn straight INTO [`CACHE`] this frame (`gfx::FrameCache::render_into`),
    /// opened by [`page_pass`] when it owes a capture and closed by [`capture_now`] at the instant
    /// the capture would otherwise have been copied. `None` on every other frame.
    static mut TARGET: Option<crate::surface::PageTarget> = None;

    /// Has the ground quad already gone down this frame? A popover reaches [`live`] twice (its
    /// scrim and its panel), and the quad is one full-screen draw that belongs to the frame rather
    /// than to either call.
    static GROUND_DRAWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Is a capture owed before the next popover draws? Set by [`page_pass`] when it finds no
    /// snapshot, cleared by whoever takes it.
    static CAPTURE_OWED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// The page is MOVING under a panel that is fading out, so [`host_refresh`](super::host_refresh)
    /// will drop whatever is captured this frame before anything draws from it. Set by
    /// [`begin_frame`], read by [`capture_now`]: a 1080p `glCopyTexSubImage2D` per frame that
    /// nothing ever reads was the larger half of the Settings exit fade (device-measured 2026-09-05:
    /// ~50 ms a frame with it, ~32 ms without, Home re-rendered live under the fade either way).
    static CAPTURE_POINTLESS: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Take the page snapshot off the framebuffer and HOLD it (`Held::Page`). The one place that
    /// pairs the copy with the stage, because the two were apart: [`PagePass`]'s drop captured
    /// without ever setting the stage, so a popover that draws its scrim without [`live`]
    /// (Settings, whose opaque ground needs no lift) re-rendered its host AND re-copied it on
    /// every ramp frame — 67 ms a frame on the entry fade, 33 ms once the copy is held.
    fn capture_now() -> bool {
        // The page went straight into the snapshot (`page_pass` redirected it): close that pass and
        // put the page on the frame from the texture. No copy, and no pointless-capture question —
        // the redirect is only ever opened when the capture is wanted.
        if let Some(target) = unsafe { (*std::ptr::addr_of_mut!(TARGET)).take() } {
            unsafe { (*std::ptr::addr_of_mut!(CACHE)).rendered(target) };
            unsafe { HELD = Held::Page };
            PAGE_EPOCH.fetch_add(1, Relaxed);
            return true;
        }
        if CAPTURE_POINTLESS.load(Relaxed) {
            return false;
        }
        if unsafe { (*std::ptr::addr_of_mut!(CACHE)).capture() } {
            unsafe { HELD = Held::Page };
            PAGE_EPOCH.fetch_add(1, Relaxed);
            true
        } else {
            false
        }
    }

    /// Bumped by every successful [`capture_now`] — i.e. every time the one snapshot starts to
    /// describe a NEW undimmed host page. Only the `Held::Page` stage counts: the ground capture in
    /// [`ground_drawn`] contains the scrim, and nothing may ever key an undimmed reading off it.
    static PAGE_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// **Which host page the snapshot holds**, as a number that changes exactly when it is re-taken.
    ///
    /// `containers::modal::ModalUnderlay` keys its field's latch on it: the dim a surface asks its
    /// host for inherits the host's own light, and that light has to be re-read when — and only
    /// when — the page under the surface has been re-captured. It reads it at the head of
    /// `ModalStack::draw_scrims`, which runs inside the frame's first [`live`], so a capture owed
    /// this frame has already been taken and counted by then.
    pub(crate) fn page_epoch() -> u32 {
        PAGE_EPOCH.load(Relaxed)
    }

    /// **The page snapshot's texture, while the snapshot IS the undimmed page** (`Held::Page`) —
    /// `None` in every other stage, the ground stage above all, whose quad has the scrim baked in.
    ///
    /// `containers::modal::ModalUnderlay` reduces its field from it: the field is asked for at the
    /// instant this snapshot is taken ([`page_epoch`]'s doc), so a second full-screen copy of the
    /// framebuffer for the field's own chain would be a copy of the same pixels.
    pub(crate) fn page_tex() -> Option<std::ffi::c_uint> {
        if held() != Held::Page {
            return None;
        }
        unsafe { (*std::ptr::addr_of!(CACHE)).tex() }
    }

    /// Hold off the GROUND stage for this frame. Set by `containers::modal::ModalUnderlay` while its
    /// field is still being read off the GPU: the ground quad bakes the dim in, so a ground taken
    /// now would keep the dim of the field BEFORE the read lands for as long as the panel stays
    /// up. Re-armed every frame by the underlay and cleared by [`begin_frame`], so it cannot outlive
    /// the stack that set it.
    static GROUND_DEFERRED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// See [`GROUND_DEFERRED`].
    pub(crate) fn defer_ground(defer: bool) {
        GROUND_DEFERRED.store(defer, Relaxed);
    }

    /// How many open popovers want a frozen host.
    fn users() -> u32 {
        HOST_USERS.load(Relaxed)
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
        n > 0 && super::HOST_CLOSING.load(Relaxed) == n
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

    /// Open the frame: take the page's damage count and decide whether the snapshot survives them.
    ///
    /// **The host-damage test is route-agnostic on purpose.** It is `idle::take_page_damage` —
    /// every invalidation this frame that was not raised inside a popover's own scope
    /// (`own_motion`, [`live`], [`input_scope`]) — plus, once every holder is fading, the page's
    /// motion (`app.rs`'s `underlay_moving`, which is threaded in as the one term this module
    /// cannot derive). Derived here rather than per screen, it covers the detail and person pages
    /// too, and it cannot go stale when a seventh screen learns to host a popover.
    ///
    /// **Ramping open is deliberately NOT a reason to re-capture**: the SCRIM over the page is still
    /// darkening, but the scrim is drawn live above this snapshot rather than into it. The page
    /// itself is not moving.
    pub(crate) fn begin_frame(page_moving: bool) {
        // §9: there is no host to snapshot on a video-plane frame — what is behind these panels is
        // a hardware plane GL cannot read back, so a capture is a photograph of the punch-through
        // hole. The player path already did not call this (`app/run.rs`'s player branch says so);
        // this is the same rule stated where it can be BROKEN rather than where it happens to be
        // obeyed, and keyed on the plane being bound rather than on the route.
        if crate::gfx::video_plane_refuses("popover::host::begin_frame") {
            return;
        }
        // This frame's page verdict — the scoped one app.rs threads in (Home, the Library, Search,
        // the press dip) OR'd with the unscoped one (Detail updates outside `scoped_motion` and
        // reports through `idle::page_moving`) — is the host cache's business and nothing else's;
        // `moving` below is where it is taken. It used to be PUBLISHED here as well, for
        // `gfx::page_wash_dither` to read, until 2026-09-19 proved a page-wide verdict is the wrong
        // question for a wash: the wash's own dissolve is in it. (That function is gone too: every
        // wash dithers on every frame now — `gfx::draw_ambient`.)
        // Taken every drawn frame, holder or not, so the count never carries over into the first
        // frame of the next panel to open.
        let page_dirty = crate::ui::idle::take_page_damage();
        CAPTURE_OWED.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
        GROUND_DEFERRED.store(false, Relaxed);
        if users() == 0 {
            invalidate();
            return;
        }
        let moving = crate::ui::idle::page_moving() || page_moving;
        CAPTURE_POINTLESS.store(fading_only() && moving, Relaxed);
        if super::host_refresh(fading_only(), page_dirty, moving) {
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
            // Draw this page into the snapshot rather than onto the frame and copy it out after:
            // see `FrameCache::render_into` for what the copy cost. Not in a blur source pass (the
            // capture is refused there anyway) and not when the capture would be thrown away.
            if !crate::gfx::blur_source_pass() && !CAPTURE_POINTLESS.load(Relaxed) {
                unsafe {
                    if (*std::ptr::addr_of!(TARGET)).is_none() {
                        TARGET = (*std::ptr::addr_of_mut!(CACHE)).render_into();
                    }
                }
            }
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
            // Nobody lifted: this page's popovers draw AFTER the closure (the two menus,
            // `account_menu`). The framebuffer holds the completed undimmed page, which is exactly
            // what the snapshot is.
            let redirected = unsafe { (*std::ptr::addr_of!(TARGET)).is_some() };
            if CAPTURE_OWED.swap(false, Relaxed) || redirected {
                capture_now();
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
            if !capture_now() {
                CAPTURE_OWED.store(true, Relaxed);
            }
        }
        Live {
            was_frozen: crate::gfx::set_page_frozen(false),
            own: crate::ui::idle::OwnScope::open(),
        }
    }

    /// **The popover's own GROUND is now on the framebuffer** — called by `Popover::panel`, the one
    /// line a `Popover` draws between its ground and its foreground.
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
        if settled
            && held() == Held::Page
            && !crate::gfx::blur_source_pass()
            && !GROUND_DEFERRED.load(Relaxed)
        {
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

    /// **A fresh open is not settled, and stepping the appear spring to rest is.** The host
    /// snapshot's GROUND stage keys on it (`ground_done`): a popover whose spring never reaches this
    /// true would never let the ground be captured, and would redraw its scrim and frost forever.
    #[test]
    fn appear_settled_is_false_on_open_and_true_once_the_spring_arrives() {
        // `open`/`close` touch the shared `OPEN_COUNT` static — the same reason the round-trip
        // test below takes this lock, and for the same reason this one must not skip it: two
        // popovers opening at once on different threads would otherwise race that counter.
        let _g = crate::testlock::serial();
        let mut pop = Popover::new();
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
        let base = HOST_USERS.load(Relaxed);
        let users = || HOST_USERS.load(Relaxed) - base;

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
        let base = OPEN_COUNT.load(Relaxed);
        let count = || OPEN_COUNT.load(Relaxed) - base;

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
        let base = HOST_USERS.load(Relaxed);
        let closing_base = HOST_CLOSING.load(Relaxed);
        let users = || HOST_USERS.load(Relaxed) - base;
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
        let closing = || HOST_CLOSING.load(Relaxed) - closing_base;
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

    // (`a_self_gated_popover_is_not_route_gated_on_update` stood here. Reported off a television
    // on 2026-09-03 as "popover menus do not hide and may stack up": `Popover::dismiss` ends the
    // OPEN state on the press frame and `Popover::update` is the only place `closing` is ever
    // cleared, so a panel drawn on `visible()` but UPDATED behind `matches!(route, …)` never
    // finished its fade once the dismissal flipped the route back to its host — it stayed visible
    // at full opacity for the rest of the session and every later panel piled on top of it. The
    // test read `app/run.rs` and refused any `ui::<m>::update(` whose preceding guard named a
    // route.
    //
    // Its subject list was five, then two, then one, and each shrink recorded a module leaving the
    // frame loop rather than the property weakening: 5b took `settings`, `legal` and `consent`,
    // phase 10's item 2 took `account_menu` and item 3 took `item_menu`, the last of them. There
    // is no `ui::<m>::update(` call in `app/run.rs` for the scan to find, and its own doc said
    // what to do about that — "do not restore the retired names: with the legacy modules gone
    // there is nothing behind them, and the scan would fail forever."
    //
    // The DEFECT cannot be spelled in the container world at all, which is why nothing replaces
    // it: `ModalStack` advances every live entry's motion each frame with no route term anywhere
    // in reach, and a surface's dismissal is a phase on the entry rather than a flag a second
    // gate has to agree with.
}
