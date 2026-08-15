//! **Whole-frame present gating** — "nothing on screen changed, so don't repaint it".
//!
//! This is *not* the dirty-rectangle tracking [`mod.rs`](super)'s renderer doc rejects, and the
//! distinction is the whole reason this module is allowed to exist. That rule is about what
//! happens *inside* a frame: the renderer stays immediate-mode, every frame still clears and
//! rebuilds the entire view tree, and the way to not draw something is still to CULL it. This
//! module only decides whether to run that frame **at all**. When it says yes, nothing below it
//! changes; when it says no, the loop skips `glViewport`…`SDL_GL_SwapWindow` wholesale. No screen,
//! widget or painter learns anything new, and nothing tracks sub-frame damage.
//!
//! # Why it is worth having
//!
//! Measured on the device (2026-07-31, 60 s windows, `/proc` jiffy deltas): a **still** Home grid
//! cost 16.0% of one Cortex-A53 core in this process — and, because the compositor must blend our
//! 1080p surface every time we present, another ~19.4 points inside `surface-manager` (23.8% with
//! us up vs 4.4% with the app closed). That second charge is **content-independent** — it measured
//! the same on the flat profile picker, the Home grid and the hero billboard — so it is a per-
//! *present* charge, not a per-pixel one. Roughly 35 points of one core, indefinitely, to re-send
//! a picture that has not changed. A TV sits on Home for hours and the SoC is fan-less.
//!
//! # The model: exact motion, plus explicit invalidation
//!
//! Two independent signals, deliberately not one heuristic:
//!
//! 1. **Motion is detected exactly, not guessed.** Every animation in the app runs through
//!    [`gfx::spring`](crate::gfx::spring) or [`gfx::spring_zeta`](crate::gfx::spring_zeta) — those
//!    two functions are the only spring integrators there are — so they call [`note_spring`] and
//!    the gate *knows* whether any of the ~439 springs a settled Home grid steps per frame is
//!    still in flight. There is no settle-window guess to tune, and no animation can be truncated
//!    mid-flight by a timer that expired early.
//!
//! 2. **Discrete changes call [`invalidate`].** A spring is not involved when a poster texture
//!    lands, a hub refetch commits, or a keypress swaps a label. Those sites say so explicitly.
//!    The full set is listed on [`invalidate`]; adding a new async landing means adding a call
//!    there, and the [`KEEPALIVE_MS`] backstop below bounds the damage if someone forgets.
//!
//! **Springs are not the only clock, and that is the standing hazard.** Anything that animates
//! from raw time — a millisecond ramp, a phase accumulator, a countdown — is invisible to (1) by
//! construction and must report through (2) itself. Two did not, and both froze in the product:
//! [`Xfade`](crate::ui::xfade) (every route dip) and [`Spinner`](crate::ui::widgets::Spinner)
//! (every loading read-out). They report from their own advance and draw respectively; the reasons
//! those two sides differ are on each call. `docs/retui-invalidation-design.md` is the accepted
//! plan for closing the class properly, by making `dt` a capability rather than an `f32`.
//!
//! The asymmetry is deliberate: a false "something moved" costs one wasted frame, a false
//! "nothing moved" freezes the screen. Both the rest threshold and the backstop are therefore
//! biased toward presenting.
//!
//! # What this module does NOT do
//!
//! It does not slow the **loop** — only the **present**. Input polling, the remote FIFO drain,
//! `ls2_pump`, `route::pump_play`, `metadata::pump_detail`, `posters::poster_pump` and every
//! screen's `*_update` keep running at full rate, so key latency is unchanged, timers still fire
//! (the hero billboard's 8 s auto-flip still flips), and async work still lands on schedule. Those
//! cost ~0.3% of a core between them; the 16% was the draw.
//!
//! It is also **not applied to the player route** — see [`should_present`]'s caller in `app.rs`.
//! `system.rs`'s `clear_opaque_region` documents the hardware video plane as *slaved* to our
//! wayland surface, and "we stop presenting for seconds while a plane is slaved to it" is a claim
//! about this compositor that no amount of reading settles. Home has no video plane active, which
//! is what makes it the safe place to prove the mechanism.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

/// A spring is in flight while the value it will DRAW this frame differs **visibly** from the one
/// it drew last frame. Two terms, and the second one is where the cost was.
///
/// The first version of this module used one absolute `1e-3` for both the distance and the
/// velocity, on the argument that 1e-3 is invisible whether the spring is in pixels or in 0..1.
/// That is true of the DISTANCE term and false of the VELOCITY term, because velocity is in units
/// **per second**: `1e-3` on a pixel-valued spring means "still moving at a thousandth of a pixel
/// per second", which a critically-damped spring satisfies for a very long time after it has
/// visually arrived. Simulated against the real constants, that tail was **36–52% of every
/// interaction's presents** — 43 of the 83 frames a 280 px shelf scroll costs, all of them
/// sub-pixel. Those are whole presents, so each one also carried the compositor's per-present
/// charge, which is the larger half.
///
/// So: scale the threshold to the spring's own magnitude (`REST_REL`), cap it below one pixel so a
/// large-magnitude spring can never stop somewhere visible (`REST_CAP`), and compare **velocity
/// times dt** — the distance this frame — rather than velocity itself.
///
/// Erring small is still the safe direction: too large freezes a moving screen, too small costs a
/// few frames at the tail. `REST_CAP` is the guarantee that "too large" can never exceed a quarter
/// of a pixel, whatever the units turn out to be.
const REST_REL: f32 = 1e-3;

/// Ceiling on the rest threshold, in the units of the largest springs (pixels). A quarter pixel is
/// below anything the panel can show, so this bounds the worst case for a `scroll_y` running to
/// four figures while leaving 0..1 springs entirely to [`REST_REL`] (for them the relative term is
/// ~1e-3 and this cap never binds).
const REST_CAP: f32 = 0.25;

/// Present at least this often even when the gate sees no reason to. This is **insurance, not
/// pacing**: it bounds how long a screen can be stale if some future async landing forgets to call
/// [`invalidate`], and it keeps a commit flowing to the compositor rather than leaving the surface
/// silent for minutes on end — a state this wayland stack has never been asked to hold.
///
/// At 2 s this is 0.5 fps, i.e. it gives back ~0.8% of the 60 fps cost while capping worst-case
/// staleness at two seconds. Set it to 0 to disable once the invalidation set has been proven
/// complete on device over a long soak.
const KEEPALIVE_MS: u32 = 2000;

/// How long the loop sleeps on a frame it decided not to present.
///
/// **The swap is the loop's only blocking call** — there is no `SDL_Delay`, `nanosleep` or frame
/// budget anywhere else in `app.rs` — so skipping it without sleeping turns a 16%-of-a-core app
/// into a 100% spinner, which is strictly worse than the problem this module exists to solve.
/// One frame period keeps the input poll rate (and therefore key latency) exactly where it is
/// today; the saving being chased is the GPU and the compositor, not these few CPU percent.
pub(crate) const IDLE_POLL_MS: u32 = 16;

thread_local! {
    /// Some spring moved during this frame's update phase. Cleared by [`frame_begin`].
    ///
    /// **Thread-local, and that is the correct model, not a test workaround.** Springs are stepped
    /// by the render loop; a spring stepped on some other thread has, by definition, nothing to do
    /// with whether the panel needs repainting. Making it per-thread also makes the host suite
    /// hermetic for free: `card_row`'s tests drive a local `CardRow` and are documented as ordinary
    /// and parallel, so they step springs on other threads WITHOUT `testlock` — with a
    /// process-global flag they would intermittently mark this module's "settled screen"
    /// assertions as moving.
    static MOVING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// This frame's `dt`, stamped once by [`frame_begin`]. [`note_spring`] needs it to convert a
    /// velocity into "distance this frame", which is the only form in which a velocity can be
    /// judged visible. Same thread-local rationale as `MOVING`.
    static DT: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0 / 60.0) };
}
/// A discrete change happened; present once more. Taken-and-cleared by [`should_present`] — NOT by
/// [`note_present`], so that a report raised during a draw survives to the frame it belongs to.
static DIRTY: AtomicBool = AtomicBool::new(true);
/// `SDL_GetTicks` of the last present, for the [`KEEPALIVE_MS`] backstop.
static LAST_PRESENT: AtomicU32 = AtomicU32::new(0);
/// Presents since the last [`take_presents`] — the heartbeat's `fps=` field.
static PRESENTS: AtomicU32 = AtomicU32::new(0);
/// Kill switch (`/tmp/plxnative-noidle`), so a device A/B is one file apart and a bad frame on the
/// panel is one `rm` from being ruled out as this feature's fault.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Turn the gate off for this boot. Read once at startup from `/tmp/plxnative-noidle`.
pub(crate) fn set_enabled(on: bool) {
    ENABLED.store(on, Relaxed);
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

/// Report one spring step. Called from the two integrators in `gfx`, so it sees every animation
/// in the app without any screen having to opt in.
///
/// Takes the post-step state by value: at rest the analytic form lands `pos` bit-identically on
/// `target` with `vel` decayed to zero, so a settled screen does not even reach the store.
#[inline]
pub(crate) fn note_spring(pos: f32, target: f32, vel: f32) {
    let t = (REST_REL * (1.0 + pos.abs().max(target.abs()))).min(REST_CAP);
    // `vel * dt` is the travel this frame — the only form in which a velocity is comparable to a
    // distance, and the whole reason the tail of every scroll used to keep the panel awake.
    if (pos - target).abs() > t || (vel * DT.with(|d| d.get())).abs() > t {
        MOVING.with(|m| m.set(true));
    }
}

/// A [`Spring::jump`](crate::ui::Spring::jump) teleported a value that was not already there.
///
/// This is [`invalidate`], not a motion report, and the distinction is load-bearing: a jump can
/// happen inside an event handler, which runs BEFORE [`frame_begin`] clears the motion flag — so a
/// motion report would be wiped before the gate ever read it. A jump is a discrete change and
/// belongs on the sticky flag.
#[inline]
pub(crate) fn note_jump(changed: bool) {
    if changed {
        invalidate();
    }
}

/// Something changed that no spring will report: a texture landing, new data from the server, a
/// keypress, a route change.
///
/// Call sites today — keep this list current, it is the module's correctness argument:
/// - any SDL event dequeued, and any remote-FIFO token drained (`app.rs`)
/// - `metadata::pump_detail` landing (`app.rs`)
/// - **any** play plan landing (`route::apply_plan`) — including one that carries the server's
///   REFUSAL, which `app.rs` cannot see: `pump_play` returns false for a plan with no URL, so the
///   caller's invalidate is skipped for exactly the landing that flips the player from Resolving
///   to Error. It repainted anyway only because the player route bypasses this gate outright
/// - a poster texture uploaded (`posters::poster_pump`)
/// - a hub refetch committing (`pms::pump`)
/// - a view-state write's OPTIMISTIC edit (`viewstate::request` / `pms::edit_item`) and the refresh
///   its landing kicks (`viewstate::pump`) — a watched tick, a corner veil and a resume bar all
///   change with no spring behind any of them, and the press is the only thing that moved
/// - a browse page landing (`browse::pump`)
/// - a server's self-description landing (`plex::serverinfo::store`) — its version and Plex Pass
///   tristate are what the stats panel's Server row, the detail hero's "hardware conversion
///   needs [PLEX PASS]" note and the failure read-out's capsule are drawn from, and it lands on a
///   worker seconds after the screen that shows them has settled
/// - a `Spring::jump` that actually teleported something (via [`note_jump`])
/// - an `Xfade` ramp mid-flight, and a `Spinner` being drawn — the two time-driven animators
///   `note_spring` cannot see
#[inline]
pub(crate) fn invalidate() {
    DIRTY.store(true, Relaxed);
}

/// Start of an iteration: forget last frame's motion (the update phase is about to re-derive it)
/// and stamp this frame's `dt` for [`note_spring`].
#[inline]
pub(crate) fn frame_begin(dt: f32) {
    MOVING.with(|m| m.set(false));
    DT.with(|d| d.set(dt));
}

/// Should this iteration draw and swap?
///
/// Call AFTER the update phase (so this frame's spring steps have been seen) and before
/// `glViewport`. The caller owns the route policy — this answers only "has anything changed".
/// Should this iteration draw and swap?
///
/// **TAKES-AND-CLEARS the discrete flag**, which is why it must be called exactly once per
/// iteration and why the caller's route check must be on the RIGHT of the `||` — see `app.rs`.
/// Clearing here rather than after the draw is what lets a report raised DURING a draw survive:
/// `Spinner::draw` is the first such reporter, and with the flag cleared post-draw its report was
/// destroyed on the frame it was raised, so the spinner would freeze on its very first link.
pub(crate) fn should_present(now: u32) -> bool {
    if !ENABLED.load(Relaxed) {
        return true;
    }
    let dirty = DIRTY.swap(false, Relaxed);
    if MOVING.with(|m| m.get()) || dirty {
        return true;
    }
    KEEPALIVE_MS != 0 && now.wrapping_sub(LAST_PRESENT.load(Relaxed)) >= KEEPALIVE_MS
}

/// Record that a frame was presented. Deliberately does NOT clear the discrete flag — a report
/// raised by the draw this call follows belongs to the NEXT frame, and [`should_present`] already
/// consumed the one that justified this one.
#[inline]
pub(crate) fn note_present(now: u32) {
    LAST_PRESENT.store(now, Relaxed);
    PRESENTS.fetch_add(1, Relaxed);
}

/// Presents since the last call — drained once a second into the heartbeat as `fps=`, which is the
/// only field there that is a frame rate.
///
/// Its neighbour `loop=` deliberately counts LOOP iterations: it is the app's liveness signal and
/// `tests/run.py` anchors `pos=` to it, so it must not read 0 on a screen that is merely idle.
/// `fps=` is the number this module actually moves, and `fps=0` beside a healthy `loop=` is this
/// module working, not a fault.
pub(crate) fn take_presents() -> u32 {
    PRESENTS.swap(0, Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's statics are reached from `gfx::spring`, which every other module's spring tests
    /// also drive — so this contends across modules, not just within this file. `testlock`, not a
    /// module-local mutex (see `lib.rs::testlock`).
    fn fresh() -> std::sync::MutexGuard<'static, ()> {
        let g = crate::testlock::serial();
        set_enabled(true);
        frame_begin(1.0 / 60.0);
        DIRTY.store(false, Relaxed);
        LAST_PRESENT.store(0, Relaxed);
        PRESENTS.store(0, Relaxed);
        g
    }

    #[test]
    fn settled_screen_stops_presenting() {
        let _g = fresh();
        // a spring sitting exactly on target reports nothing
        note_spring(1.0, 1.0, 0.0);
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(1.0, 1.0, 0.0);
        assert!(!should_present(10_016), "a settled screen must not repaint");
    }

    #[test]
    fn motion_forces_a_present() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(0.5, 1.0, 0.0); // far from target
        assert!(should_present(10_016));

        frame_begin(1.0 / 60.0);
        note_spring(1.0, 1.0, 0.4); // on target but still carrying velocity
        assert!(should_present(10_016));
    }

    /// A 0..1 spring is judged by the RELATIVE term: its threshold is ~1e-3 and the sub-pixel cap
    /// never binds for it.
    #[test]
    fn sub_epsilon_residue_is_at_rest() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(1.0 + REST_REL / 4.0, 1.0, REST_REL / 4.0);
        assert!(!should_present(10_016));
    }

    /// THE REGRESSION THIS PREDICATE EXISTS FOR. A 280 px shelf scroll that has arrived to within a
    /// twentieth of a pixel, still carrying 5 px/s — 0.08 px of travel in the next frame — used to
    /// read as moving, because 5.0 > 1e-3. That tail was 43 of the scroll's 83 presents.
    #[test]
    fn a_scroll_that_has_visually_arrived_is_at_rest() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(280.0 - 0.05, 280.0, 5.0);
        assert!(!should_present(10_016), "sub-pixel travel must not hold the panel awake");
    }

    /// ...but the same spring one pixel out, or moving a pixel per frame, still is.
    #[test]
    fn a_scroll_still_visibly_moving_presents() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(279.0, 280.0, 0.0);
        assert!(should_present(10_016), "a whole pixel out is visible");

        frame_begin(1.0 / 60.0);
        note_spring(280.0, 280.0, 60.0); // 1 px this frame
        assert!(should_present(10_016), "a pixel of travel per frame is visible");
    }

    /// The cap is what stops the relative term from growing past visibility on a deep scroll: at
    /// pos 1500 the relative threshold would be 1.5 px, which is a visible stop.
    #[test]
    fn the_threshold_never_exceeds_a_quarter_pixel() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(1500.0 - 0.3, 1500.0, 0.0);
        assert!(should_present(10_016), "0.3px out must still repaint however large the value");
    }

    /// A jump reports only when it actually moved — `home.rs` jumps to the same value every frame
    /// while the hub list is empty, and an unguarded report would pin 60fps on that exact screen.
    #[test]
    fn a_jump_reports_only_when_it_changes_something() {
        let _g = fresh();
        let mut s = crate::ui::Spring::at(1.0);
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        s.jump(1.0); // already there
        assert!(!should_present(10_016));

        frame_begin(1.0 / 60.0);
        s.jump(2.0); // teleported
        assert!(should_present(10_032));
    }

    /// The ordering fix: a report raised DURING the draw (a spinner) must survive to the next
    /// frame. `should_present` takes-and-clears, so `note_present` after the draw cannot eat it.
    #[test]
    fn a_report_raised_during_a_draw_survives_to_the_next_frame() {
        let _g = fresh();
        frame_begin(1.0 / 60.0);
        assert!(should_present(10_000));
        note_present(10_000); // ... the draw runs here, and the spinner reports:
        invalidate();
        frame_begin(1.0 / 60.0);
        assert!(should_present(10_016), "the spinner's own chain must not break on its first link");
    }

    #[test]
    fn invalidate_buys_exactly_one_frame() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        invalidate();
        assert!(should_present(10_016));
        note_present(10_016); // the frame it bought
        frame_begin(1.0 / 60.0);
        assert!(!should_present(10_032), "one landing must not pin the loop on");
    }

    #[test]
    fn keepalive_bounds_staleness() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        assert!(!should_present(10_000 + KEEPALIVE_MS - 1));
        assert!(should_present(10_000 + KEEPALIVE_MS));
    }

    /// The tick counter wraps every ~49 days, and the keepalive is the one place the gate does
    /// arithmetic on it. Straight subtraction would read ~4 billion ms across the wrap and pin the
    /// loop presenting forever; the reverse case must err toward presenting, not stall.
    #[test]
    fn tick_wrap_is_arithmetically_sound() {
        let _g = fresh();
        note_present(u32::MAX - 10);
        frame_begin(1.0 / 60.0);
        // 16 ms past the wrap: elapsed is 16, not 4e9 — the keepalive is not due
        assert!(!should_present(5));
        // and it still comes due on time across that same wrap
        assert!(should_present((u32::MAX - 10).wrapping_add(KEEPALIVE_MS)));
        // a tick BEFORE the last present is only reachable at a wrap — repaint, never stall
        LAST_PRESENT.store(5, Relaxed);
        frame_begin(1.0 / 60.0);
        assert!(should_present(u32::MAX - 10));
    }

    #[test]
    fn kill_switch_always_presents() {
        let _g = fresh();
        set_enabled(false);
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        assert!(should_present(10_016));
        set_enabled(true);
    }

    /// OVER-REPORTING IS THE SILENT FAILURE. A gate that presents too often loses the entire
    /// saving while every floor-style assertion still passes, so a whole settled tree — the ~439
    /// springs a Home grid steps per frame, all at rest — must produce exactly nothing.
    #[test]
    fn a_whole_settled_tree_asks_for_nothing() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        for i in 0..439 {
            let v = 1.0 + (i as f32) * 0.001; // a spread of magnitudes, every one AT its target
            note_spring(v, v, 0.0);
        }
        assert!(!should_present(10_016), "439 springs at rest must not repaint");
    }

    /// One moving spring anywhere in that tree is enough. (The asymmetry is the design: a false
    /// "moved" costs one frame, a false "still" freezes the screen.)
    #[test]
    fn one_mover_in_a_settled_tree_is_enough() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        for _ in 0..438 {
            note_spring(1.0, 1.0, 0.0);
        }
        note_spring(1.0, 1.5, 0.0);
        assert!(should_present(10_016));
    }

    /// Motion is re-derived every frame, never remembered: last frame's movement must not leak
    /// into this frame's decision, or a single animation would pin the loop permanently.
    #[test]
    fn motion_does_not_survive_the_frame_that_had_it() {
        let _g = fresh();
        frame_begin(1.0 / 60.0);
        note_spring(0.0, 1.0, 0.0); // moving
        assert!(should_present(10_000));
        note_present(10_000);
        frame_begin(1.0 / 60.0); // next frame: nothing steps
        assert!(!should_present(10_016), "yesterday's motion must not repaint today");
    }

    /// `dt` scales the velocity term, so a long frame counts as more travel. A spring drifting at
    /// 12 px/s is at rest in a 16 ms frame (0.2 px) and moving in a 50 ms one (0.6 px) — the dt
    /// clamp's worst case, and the reason the velocity test cannot be a bare constant.
    #[test]
    fn a_longer_frame_covers_more_travel() {
        let _g = fresh();
        note_present(10_000);
        frame_begin(1.0 / 60.0);
        note_spring(300.0, 300.0, 12.0);
        assert!(!should_present(10_016), "0.2px in a 60Hz frame is not visible");

        frame_begin(0.05); // app.rs's dt clamp
        note_spring(300.0, 300.0, 12.0);
        assert!(should_present(10_032), "0.6px in a long frame is");
    }

    /// The kill switch must not silently consume the dirty flag — `/tmp/plxnative-noidle` is the
    /// A/B instrument, so arming it has to change the frame RATE and nothing else.
    #[test]
    fn the_kill_switch_leaves_the_dirty_flag_alone() {
        let _g = fresh();
        note_present(10_000); // park the keepalive; this test is about the flag, not the backstop
        set_enabled(false);
        invalidate();
        assert!(should_present(10_000));
        set_enabled(true);
        assert!(should_present(10_000), "the flag must still be there to consume");
        assert!(!should_present(10_000), "...and consumed exactly once");
    }

    #[test]
    fn presents_drain_once() {
        let _g = fresh();
        note_present(1);
        note_present(2);
        assert_eq!(take_presents(), 2);
        assert_eq!(take_presents(), 0);
    }
}
