//! `nav` — the ROUTE-level page transition: the cross-fade a whole SCREEN change rides, and the
//! rule that decides which chrome rides with it.
//!
//! Third sibling of [`xfade`](crate::ui::xfade) (a screen's CONTENT being replaced under it) and
//! [`popover`](crate::ui::popover) (a modal appearing OVER it): this is the page ITSELF being
//! replaced. It is built ON an [`Xfade`], not beside one — same scheduled Out/Hold/In, same
//! 70/140 ms — because a route change and a content reload are the same statement at two altitudes,
//! and two dissolve speeds in one app would read as two products.
//!
//! **It is a DIP, not a cross-dissolve.** Every screen's draw opens with `gfx::frame_clear`, so two
//! pages can never be on the panel at once; the outgoing page fades to the app ground, the route
//! flips at the floor, the incoming page fades up off it. `theme::CLEAR_RGB` IS
//! `theme::SURFACE_APP`, so the trough colour and the clear are the same pixel and the swap has no
//! seam. It is also why it costs no second pass: the alpha is a value in a cascade every primitive
//! already multiplies through (`Painter::c`), not an extra full-screen quad.
//!
//! **Why the route flip is DEFERRED and the chrome is not.** [`Xfade`]'s doc has the general
//! argument — a control must acknowledge a press on the press frame while its data swaps later.
//! Here the control is the shared top tab bar and its acknowledgement is the capsule TRAVELLING to
//! the pressed pill, so the pending SELECTION lives here ([`view_tab`]) and is read inside
//! `widgets::tab_row_update`, the one function both screens go through. That is exactly the shape
//! `library::view_section` already has for that screen's own chrome, extended across the route
//! boundary rather than forked into a second mechanism. Before this existed, the strip only learned
//! the new selection on the first frame the DESTINATION drew — i.e. after the page had already been
//! replaced — which is why the highlight visibly chased the swap.
//!
//! **Why the outgoing page's TEARDOWN is queued here too** ([`begin`]'s `leave`, spent by
//! [`spend_leave`]). The route flip is not the only thing a transition defers. A page that is being
//! left for good has to be torn down — `detail::close` drops the loaded item (`metadata::clear`),
//! `person::leave` drops the person — and running that on the PRESS frame blanks the page *during
//! its own fade-out*: the detail hero falls back to the catalog row, every section below it empties,
//! and a spinner appears, all while the user is still looking at it. So the teardown rides the
//! request and runs at the floor, with the incoming page's entry, where nothing is on the panel to
//! see it. It is a payload rather than a special case bolted onto the BACK arm because BOTH stacking
//! screens have this shape and the next one will too — and because it is only liftable into a value
//! that a host test can drive.
//!
//! A plain `fn()`, never a boxed closure: it is a code pointer, `Copy`, allocates nothing, and so
//! carries none of the objection the module doc raises against a `Box<dyn FnOnce>` behind a
//! `static mut`. What it deliberately CANNOT express is a teardown that needs arguments — that is
//! the caller's own pending value, exactly as the route is.
//!
//! It owns MOTION and the chrome rule only. The pending ROUTE is `app.rs`'s typed value, because
//! `Route` is a `plex_run`-local enum and because a generic queue here would be a
//! `Box<dyn FnOnce>` behind a `static mut` — the same division `xfade.rs` already draws.
//!
//! Main-thread only; `static mut` with the screens' discipline.

use crate::ui::xfade::Xfade;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// THE page fade — one for the app, because one page is mounted at a time and a second route
/// change SUPERSEDES the first rather than stacking beside it.
static mut PAGE: Xfade = Xfade::new();
/// Is the shared top tab bar present on BOTH sides of the transition in flight? Only meaningful
/// while one is (see [`chrome_alpha`]); at rest both alphas are 1, so a stale value cannot be seen.
static mut CONTINUOUS: bool = false;
/// The tab pill the destination SELECTS, or -1 for "no pending selection" (nothing in flight, or a
/// destination with no tab bar). -1 rather than `Option<usize>` so it drops straight into the
/// `c_int` the strip is placed from.
///
/// A PILL, and since the strip became a projection of the section table (`browse::tabs`) that is
/// not the destination's section index plus one: several libraries can share one pill, so
/// `app.rs`'s `Nav::Library` carries the TAB the press named and the `+1` here is only the Home
/// pill leading the row.
static mut TAB: c_int = -1;
/// The OUTGOING page's teardown, waiting for the floor — see the module doc. `None` for a FORWARD
/// navigation, which leaves the page it came from standing behind the destination (that is what the
/// BACK trail is for); only a page being left for good has anything to tear down.
static mut LEAVE: Option<fn()> = None;

fn page() -> &'static mut Xfade {
    unsafe { &mut *addr_of_mut!(PAGE) }
}

/// Start a page transition. `continuous` = both routes draw the shared top bar; `tab` = the pill
/// the destination selects, from THIS frame on; `leave` = the outgoing page's teardown, run at the
/// floor by [`spend_leave`].
///
/// A second call while one is running RETARGETS it (newest wins — [`Xfade::reload`] continues the
/// ramp instead of restarting it), with one asymmetry: `continuous` is sticky-false for the
/// duration. A transition that has already begun hiding the bar must not un-hide it mid-fade — the
/// bar popping back to full and then away again is a blink, and the honest reading of "the user
/// changed their mind" is that the chrome finishes the fade it is already in.
///
/// `leave` follows the plain newest-wins rule, because it belongs to the REQUEST rather than to the
/// fade: the newest request is the one that will be applied, so its teardown is the one that must
/// run. That is not merely consistent, it is the right answer for the case it decides — a BACK off a
/// detail page (teardown queued) retargeted inside the window to a Related tile on that same page
/// (no teardown, the page stays on the trail) must NOT close the page the user has just chosen to
/// navigate deeper from.
pub(crate) fn begin(continuous: bool, tab: Option<usize>, leave: Option<fn()>) {
    let running = page().is_swapping();
    unsafe {
        CONTINUOUS = if running { addr_of!(CONTINUOUS).read() && continuous } else { continuous };
        TAB = tab.map(|t| t as c_int).unwrap_or(-1);
        LEAVE = leave;
    }
    page().reload();
}

/// Withdraw a transition that has not committed yet. Returns whether there WAS one, so the caller
/// drops its pending value on `true` and handles the input normally on `false` — a BACK inside the
/// fade window is never silently eaten.
pub(crate) fn cancel() -> bool {
    let did = page().cancel();
    if did {
        // the capsule springs back to whatever the screen we never left says is selected
        unsafe { TAB = -1 };
        // …and the page we never left is not torn down. This is the half that makes a withdrawal
        // free: nothing has been committed, so there is nothing to un-commit.
        unsafe { LEAVE = None };
    }
    did
}

/// Spend the queued teardown at the fade floor: run it if `alive` — the CALLER's supersede test,
/// the one thing this module cannot make (`Route` is `app.rs`'s) — and drop it either way.
///
/// One unconditional call with the condition as an ARGUMENT, deliberately, rather than a
/// `take_leave()` the caller decides whether to invoke. That shape makes both failures
/// unrepresentable: a teardown cannot be forgotten (leaking into whatever transition comes next,
/// where it would tear down a page nobody asked to leave) and it cannot run for a request that was
/// dropped (something else moved the app while this was fading, and the page this would close is
/// no longer the one it names).
///
/// Called on the commit frame — the one frame [`tick`] returns `true` — and nowhere else.
pub(crate) fn spend_leave(alive: bool) {
    let f = unsafe { std::ptr::replace(addr_of_mut!(LEAVE), None) };
    if let Some(f) = f.filter(|_| alive) {
        f();
    }
}

/// One frame. Returns `true` on exactly ONE frame — the floor — where the caller applies its route
/// change.
///
/// `ready` is unconditionally TRUE because this fader gates a ROUTE, not a fetch. Every screen
/// already owns its own data wait (Library's grid [`Xfade`], Home's hub read-out, detail's load
/// state), and parking the PAGE behind a slow request would hold the whole app — chrome included —
/// at alpha 0 for as long as a server takes to answer. So `Hold` lasts exactly one frame and a
/// transition can never wedge, whatever the network does.
pub(crate) fn tick(dt: f32) -> bool {
    let commit = page().tick(dt, true);
    if commit {
        // from here the destination is mounted and answers for its own chrome
        unsafe { TAB = -1 };
    }
    commit
}

/// The cascade alpha for PAGE CONTENT — everything a screen draws that does not survive the swap.
pub(crate) fn page_alpha() -> f32 {
    page_ref().alpha()
}

fn page_ref() -> &'static Xfade {
    unsafe { &*addr_of!(PAGE) }
}

/// The cascade alpha for CONTINUOUS CHROME — the shared top band (tab row + profile chip), which is
/// the same object on Home and on the Library and must NOT blink when one replaces the other. Full
/// while the bar exists on both sides of the transition; otherwise it rides the page fade, because
/// a destination with no tab bar has nothing for it to be continuous WITH.
pub(crate) fn chrome_alpha() -> f32 {
    if unsafe { addr_of!(CONTINUOUS).read() } {
        1.0
    } else {
        page_alpha()
    }
}

/// The tab pill the shared row must read as SELECTED this frame: the queued destination's while a
/// route change is in flight, the caller's own otherwise. The cross-route twin of
/// `library::view_section`, and the one thing that makes the capsule leave on the PRESS frame.
pub(crate) fn view_tab(own: c_int) -> c_int {
    let t = unsafe { addr_of!(TAB).read() };
    if t >= 0 {
        t
    } else {
        own
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! `PAGE`/`CONTINUOUS`/`TAB` are module `static mut`s, so every test here holds the module's own
    //! [`NAV`] mutex for its whole body — the `home.rs` `FOCUS` / `library.rs` `PEND` precedent.
    //! Nothing here draws, so what these CANNOT say is whether the dip reads right on the panel or
    //! whether the tab bar visibly holds still through a swap; those are device captures.
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::sync::Mutex;

    static NAV: Mutex<()> = Mutex::new(());

    /// One 60 Hz frame.
    const DT: f32 = 1.0 / 60.0;

    /// Stand-in for `detail::close` / `person::leave`: the teardown is a bare `fn()` here precisely
    /// so a test can hand in one whose only effect is countable. Guarded by [`NAV`] like every
    /// other static in this module.
    static TORN: AtomicUsize = AtomicUsize::new(0);
    fn tear_down() {
        TORN.fetch_add(1, Relaxed);
    }

    /// Put the module back the way a fresh boot has it, so ordering between tests cannot matter.
    fn clear() {
        unsafe {
            PAGE = Xfade::new();
            CONTINUOUS = false;
            TAB = -1;
            LEAVE = None;
        }
        TORN.store(0, Relaxed);
    }

    /// Drive `n` frames, returning (commit count, page alpha per frame, chrome alpha per frame).
    fn run(n: usize) -> (usize, Vec<f32>, Vec<f32>) {
        run_as(n, true)
    }

    /// [`run`] with `app.rs`'s supersede answer made explicit. The commit frame ALWAYS spends the
    /// teardown, exactly as the loop does — `alive` only decides whether it runs — so these tests
    /// exercise the same call shape the app does rather than a friendlier one.
    fn run_as(n: usize, alive: bool) -> (usize, Vec<f32>, Vec<f32>) {
        let mut commits = 0;
        let (mut pa, mut ca) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for _ in 0..n {
            if tick(DT) {
                commits += 1;
                spend_leave(alive);
            }
            pa.push(page_alpha());
            ca.push(chrome_alpha());
        }
        (commits, pa, ca)
    }

    /// The direct regression test for the reported bug: pressing `Movies` on Home moved the
    /// highlight only AFTER the page had swapped. The queued pill must be what the shared row reads
    /// from the press frame until the destination takes over answering for itself.
    #[test]
    fn the_capsule_reads_the_queued_tab_from_the_press_frame_until_the_commit() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(true, Some(2), None);
        assert_eq!(view_tab(0), 2, "the capsule leaves on the PRESS frame, before any tick");

        let mut committed = false;
        for f in 0..30 {
            if !committed {
                assert_eq!(view_tab(0), 2, "frame {f}: the queued pill still owns the row");
            }
            if tick(DT) {
                committed = true;
            }
            if committed {
                assert_eq!(view_tab(0), 0, "past the commit the screen answers for its own chrome");
            }
        }
        assert!(committed);
        clear();
    }

    /// Requirement: the tab bar is CONTINUOUS chrome across Home↔Library, so it must not fade with
    /// the page — the whole screen blinking is what the transition is supposed to replace. The
    /// second half proves the flag actually does something: with no bar on the far side, the chrome
    /// rides the page exactly.
    #[test]
    fn continuous_chrome_never_dips_while_the_page_does() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(true, Some(1), None);
        let (commits, pa, ca) = run(40);
        assert_eq!(commits, 1);
        assert!(ca.iter().all(|a| *a == 1.0), "the shared bar held still: {ca:?}");
        assert!(pa.iter().any(|a| *a < 0.05), "…and the assertion is not vacuous — the page DID dip");

        clear();
        begin(false, None, None);
        let (commits, pa, ca) = run(40);
        assert_eq!(commits, 1);
        assert_eq!(pa, ca, "with no bar on the far side there is nothing to be continuous with");
        clear();
    }

    /// OK on the pill of the screen you are already on, or BACK inside the 70 ms window: the row
    /// must hand the selection straight back, and nothing may commit.
    #[test]
    fn a_withdrawn_transition_hands_the_row_straight_back() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(true, Some(3), None);
        run(2);
        assert!(cancel(), "two frames in, it is still withdrawable");
        assert_eq!(view_tab(0), 0, "the row is the screen's own again on the very same frame");
        let (commits, _, _) = run(30);
        assert_eq!(commits, 0, "a withdrawn transition must never apply a route change");
        assert_eq!(page_alpha(), 1.0);
        assert!(!cancel(), "and there is nothing left to withdraw twice");
        clear();
    }

    /// The sticky-false half of [`begin`]'s retarget rule. Without it, a Home→Detail-shaped fade
    /// (bar going away) retargeted to a Home→Library one would pop the bar back to full for the
    /// rest of the fade and then never hide it — a blink in the middle of a transition.
    #[test]
    fn a_superseding_transition_cannot_un_hide_chrome_it_has_already_started_hiding() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(false, None, None);
        run(2);
        assert!(chrome_alpha() < 1.0, "the bar is already on its way out");
        begin(true, Some(1), None);
        let (_, pa, ca) = run(40);
        assert_eq!(pa, ca, "the chrome finishes the fade it is in");
        assert_eq!(view_tab(0), 0, "the retarget still moved the capsule");
        clear();

        // …and the stickiness is scoped to the transition: the next one starts clean.
        begin(true, Some(1), None);
        assert_eq!(chrome_alpha(), 1.0);
        clear();
    }

    /// The route-level twin of `xfade`'s `the_fade_never_wedges_at_zero_…`, and the reason `ready`
    /// is a literal `true`: a page must come back whatever the network is doing, because the tab bar
    /// and every escape from the screen are inside the thing being faded.
    #[test]
    fn the_page_can_never_wedge_at_zero() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(true, Some(1), None);
        let (commits, pa, _) = run(600);
        assert_eq!(commits, 1, "exactly one route change per request");
        assert!(pa.iter().all(|a| (0.0..=1.0).contains(a)), "alpha never escaped 0..1");
        assert_eq!(page_alpha(), 1.0);
        assert_eq!(chrome_alpha(), 1.0);
        clear();
    }

    // ---- the LEAVE payload -------------------------------------------------------------------
    // The whole point of the payload is WHEN it runs, so every test below grades the frame, not
    // just the count: a teardown one frame early is the bug it exists to prevent (the page blanks
    // during its own fade-out), and one frame late is a page torn down after the next one is up.

    /// `Detail → Home`, the arm this was built for: `detail::close` must run at the FLOOR, exactly
    /// once, on the same frame the route flips — never on the press frame, where the user is still
    /// looking at the page it empties.
    #[test]
    fn the_teardown_runs_at_the_floor_and_not_one_frame_before_it() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(false, None, Some(tear_down as fn()));
        assert_eq!(TORN.load(Relaxed), 0, "the press frame tears nothing down");

        let mut committed = false;
        for f in 0..40 {
            let commit = tick(DT);
            if !commit {
                assert_eq!(TORN.load(Relaxed), committed as usize, "frame {f}: teardown off the floor");
            }
            if commit {
                assert!(!committed, "exactly one floor per transition");
                assert_eq!(page_alpha(), 0.0, "the floor IS alpha 0 — nothing is on the panel to see");
                spend_leave(true);
                committed = true;
                assert_eq!(TORN.load(Relaxed), 1, "…and the teardown is what the floor is for");
            }
        }
        assert!(committed);
        assert_eq!(TORN.load(Relaxed), 1, "and it is spent — a fade-in must not re-run it");
        clear();
    }

    /// **BACK during a `Detail → Home` fade.** The press is at most four frames old, so it is
    /// withdrawn — and a withdrawal that still closed the page would be strictly worse than no
    /// transition at all: the user would be left standing on a detail page with no item loaded.
    #[test]
    fn a_back_inside_the_window_withdraws_the_teardown_with_the_route() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(false, None, Some(tear_down as fn()));
        run(2);
        assert!(cancel(), "two frames in, the BACK is still withdrawable");
        let (commits, _, _) = run(60);
        assert_eq!(commits, 0);
        assert_eq!(TORN.load(Relaxed), 0, "the page we never left must still be loaded");
        assert_eq!(page_alpha(), 1.0, "…and fully back on screen");
        clear();
    }

    /// A request the commit DROPS (something else moved the app while it was fading) must drop its
    /// teardown with it — closing `req.from` then would tear down a page the user is no longer on,
    /// and, for `detail::close`, wipe the rk the player's exit reads back.
    ///
    /// The second half is the reason `spend_leave` takes the answer instead of handing the payload
    /// out: the drop must SPEND it, or the next transition would inherit a teardown nobody queued.
    #[test]
    fn a_superseded_request_drops_its_teardown_and_cannot_leak_it_forward() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(false, None, Some(tear_down as fn()));
        let (commits, _, _) = run_as(40, false); // app.rs: `route != req.from`
        assert_eq!(commits, 1, "the fade still completes — the page the user HAS comes back");
        assert_eq!(TORN.load(Relaxed), 0, "a dropped request tears nothing down");

        // the next transition carries no teardown of its own, and must not find one lying around
        begin(true, Some(1), None);
        run(40);
        assert_eq!(TORN.load(Relaxed), 0, "a forward navigation closed the page behind it");
        clear();
    }

    /// The retarget rule for the payload, stated as behaviour: a BACK off a detail page, taken back
    /// inside the window by an OK on that page's Related shelf, must leave the page STANDING — it is
    /// the page the new destination is stacking on top of, and the trail needs it there.
    #[test]
    fn retargeting_replaces_the_teardown_rather_than_keeping_the_old_one() {
        let _g = NAV.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        begin(false, None, Some(tear_down as fn())); // BACK: close the page
        run(2);
        begin(false, None, None); // …no, open the Related item instead: the page stays
        let (commits, _, _) = run(40);
        assert_eq!(commits, 1);
        assert_eq!(TORN.load(Relaxed), 0, "the page the new page stacks on must not be closed");

        // and the other way round: a forward request retargeted to a BACK does tear down
        clear();
        begin(false, None, None);
        run(2);
        begin(false, None, Some(tear_down as fn()));
        run(40);
        assert_eq!(TORN.load(Relaxed), 1);
        clear();
    }
}
