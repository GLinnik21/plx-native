//! `press` — the shared "click" press interaction (tvOS-style), the animated counterpart to a plain
//! activation. OK-**down** dips the focused control inward (press-in); OK-**up** releases it back with
//! an overshoot bounce and, a beat later (so the bounce is actually on screen), the caller commits the
//! activation. It is genuinely event-driven — the dip persists for as long as the button is physically
//! held — so a HELD OK is a measurable long-press ([`held_ms`]/[`is_long`]), not a tap. The design
//! (`Home Screen.dc.html`) fakes down/up with a fixed `setTimeout`; the real remote gives us both
//! edges, so we use them. That long press is what opens the **item context menu** on a home shelf
//! card and on the detail page's episode still (`ui/item_menu.rs`) — see [`LONG_MS`].
//!
//! ONE control is pressed at a time (always the currently focused one), so a single global suffices —
//! the renderer multiplies the focused tile's scale by [`scale`] while [`is_active`]. Focus can't move
//! mid-press (navigation [`cancel`]s the press), so "the focused tile" is unambiguous the whole time.
//!
//! **Two things take this press, not one.** A CARD ([`begin`]) and a CONTROL FACE ([`begin_ctl`]) —
//! the design system's `Button` / `CircleButton` / `TransportButton`, whose dip arrives through their
//! row's [`CtlPop::scale`](crate::ui::widgets::CtlPop::scale). They differ only in whether a HOLD
//! means anything: a card grows a context menu out of one, a control face has nothing to grow, so
//! `begin_ctl` does not arm the [`LONG_MS`] latch and a slow press on a Play pill still plays.
//! Until 2026-08-22 only cards armed it at all, so every control in the app activated on the key-DOWN
//! and the dip the design system specifies (`tokens/motion.css`, `--press-dip`) had no way to appear —
//! `CtlPop` was already folding a factor that was permanently 1.0.
//!
//! Reliability mirrors the scrub commit in `app.rs`: the Magic Remote occasionally drops a key-up, so
//! [`tick`] resolves a stuck press three ways — a real release (after a minimum visible dip), a stale
//! heartbeat (dropped key-up), or a hard hold cap — and a press therefore always commits or cancels.
use crate::ui::Spring;
use std::ptr::addr_of_mut;

/// Rest factor: the focused card sits at its full focus scale (press is a *multiplier* on top).
const REST: f32 = 1.0;
/// Full-press dip factor. The design dips a `scale(1.09)` focused card to `scale(1.0)`, i.e. `1/1.09`
/// — an ~8% inward press. Applied as a factor so it reads as a consistent "press" at any focus scale.
const DIP: f32 = 0.918;
/// Press-in stiffness — critically damped ([`Spring::step`]) so the dip is quick and does NOT bounce.
const K_DOWN: f32 = 620.0;
/// Spring-back stiffness, paired with [`ZETA_UP`] for the release overshoot (design's
/// `cubic-bezier(.2,1.5,.35,1)` — the `1.5` is the pop past the endpoint).
const K_UP: f32 = 340.0;
/// Spring-back damping ratio (`< 1` ⇒ overshoots/rings = the tvOS click pop).
const ZETA_UP: f32 = 0.55;
// The control FOCUS POP used to be exported from here as `K_POP`/`ZETA_POP` — this module's own
// release spring, lent to `widgets::CtlPop` under the claim that the design system named two
// bouncing things, the click and a control arriving at focus. It names ONE. `tokens/motion.css`
// opens by saying so ("focus ARRIVING is a calm grow, the CLICK is what rings") and its
// `--ease-bounce` token says to use that curve "for the press spring-back and NOTHING else — never
// a focus pop". So the pop is critically damped on the TILE's `consts::K_SCALE` now, and the only
// underdamped spring left in the app is the one below. Removed rather than deprecated: the whole
// point was that the two moved together, and they must not.
/// Minimum time the dip is shown before the release bounce may start, so even a flash-quick tap still
/// registers a visible press-in (the design holds the dip a fixed 120 ms; we enforce a floor).
const MIN_DIP_MS: u32 = 90;
/// Delay from release to committing the activation, so the spring-back bounce is on screen first.
const COMMIT_MS: u32 = 120;
/// Heartbeats were arriving and then stopped for this long ⇒ the key-up was dropped ⇒ auto-release
/// (twin of `SCRUB_LOST_MS`). Only consulted once a heartbeat has actually been seen (`got_beat`) —
/// THIS remote's OK sends no auto-repeat, so without that gate a plain hold looked like a lost up.
const LOST_MS: u32 = 350;
/// Absolute hold ceiling — the last-resort dropped-key-up safety when no heartbeat ever arrives (so a
/// hold shorter than this always waits for the real release). Also the long-press ceiling.
///
/// It is the one place the two press kinds visibly part. A CARD press has already latched long by
/// here ([`LONG_MS`] is half this) and so springs back without activating; a CONTROL press never
/// latches, so this is what finally commits it — a button held down forever fires once, at ~1.1 s,
/// rather than waiting for a release that may never be delivered.
const MAX_HOLD_MS: u32 = 1000;
/// A hold at least this long is a long press: it is NO LONGER a tap, so the normal activation is
/// cancelled ([`tick`]'s latch) and the press just holds + springs back without activating.
///
/// This is the threshold the **item context menu** opens on (`ui/item_menu.rs`, via `app.rs`'s
/// per-frame press block reading [`is_long`] while the key is still DOWN) — on a home shelf card and
/// on the detail page's episode still. On a screen with no hold action the latch still fires and the
/// long press stays a deliberate no-op, which is why the cancellation lives here rather than at the
/// call sites that act on it.
pub const LONG_MS: u32 = 500;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Down, // held: dipping toward DIP
    Up,   // released/cancelled: springing back toward REST
}

struct State {
    sp: Spring,
    phase: Phase,
    down_at: u32,      // tick at press-down (long-press timing)
    alive: u32,        // last liveness tick (press-down + OK auto-repeats) — the dropped-key-up net
    got_beat: bool, // an auto-repeat heartbeat has arrived → `alive` is meaningful for LOST detection
    release_at: u32, // tick of the real key-up (0 = still held)
    commit_at: u32, // tick at which the activation may fire (0 = none scheduled)
    want_commit: bool, // false after a cancel or the long-press latch — spring back, do NOT activate
    long: bool,        // the hold crossed LONG_MS → a press-and-hold, not a tap (see `was_long`)
    took: bool,        // the caller already consumed the commit
    holdable: bool,    // a HOLD is a distinct gesture here (a card) — see `begin` vs `begin_ctl`
}

static mut S: State = State {
    sp: Spring::at(REST),
    phase: Phase::Idle,
    down_at: 0,
    alive: 0,
    got_beat: false,
    release_at: 0,
    commit_at: 0,
    want_commit: false,
    long: false,
    took: false,
    holdable: true,
};

#[inline]
fn st() -> &'static mut State {
    unsafe { &mut *addr_of_mut!(S) }
}

/// Monotone tick compare that tolerates u32 wrap: `now` has reached `t` (and `t` is armed).
#[inline]
fn reached(now: u32, t: u32) -> bool {
    t != 0 && now.wrapping_sub(t) < 0x8000_0000
}

/// OK-down on the focused CARD: begin (or restart) the press-in dip. A hold is a second gesture
/// here — past [`LONG_MS`] the activation is cancelled and the caller opens the item context menu
/// instead ([`is_long`]).
pub fn begin(now: u32) {
    arm(now, true);
}

/// OK-down on the focused CONTROL FACE — a `Button`, `CircleButton` or `TransportButton`
/// (`ui::widgets`), whose dip is folded in by its row's [`CtlPop::scale`](crate::ui::widgets::CtlPop::scale).
///
/// Identical to [`begin`] in everything the eye can see, and different in one thing it cannot: a
/// control face has **no hold gesture**. Nothing in this app grows a context menu out of a Play pill
/// or a transport disc, so the [`LONG_MS`] latch is not armed and a slow press still activates on
/// release — where [`begin`]'s would be swallowed. A control that ate a deliberate, firmly-held OK
/// and did nothing would read as a dropped keypress, which is exactly the fault a press animation is
/// meant to rule out.
///
/// [`is_long`] therefore answers `false` for the whole of such a press, which also short-circuits
/// app.rs's held-menu chain rather than leaving each of its arms to decline one at a time.
pub fn begin_ctl(now: u32) {
    arm(now, false);
}

fn arm(now: u32, holdable: bool) {
    let s = st();
    s.phase = Phase::Down;
    s.down_at = now;
    s.alive = now;
    s.got_beat = false;
    s.release_at = 0;
    s.commit_at = 0;
    s.want_commit = true;
    s.long = false;
    s.took = false;
    s.holdable = holdable;
}

/// A held-key heartbeat (OK 0x101 auto-repeat) — keeps [`LOST_MS`] from firing on a genuine hold.
pub fn note_alive(now: u32) {
    let s = st();
    if s.phase == Phase::Down {
        s.alive = now;
        s.got_beat = true;
    }
}

/// OK-up: record the release. The bounce starts (respecting [`MIN_DIP_MS`]) and the activation commits
/// a [`COMMIT_MS`] beat later — poll [`take_commit`].
pub fn release(now: u32) {
    let s = st();
    if s.phase == Phase::Down && s.release_at == 0 {
        s.release_at = now;
    }
}

/// Abort the in-flight press (navigation / BACK arrived): spring back WITHOUT committing.
pub fn cancel() {
    let s = st();
    if s.phase != Phase::Idle {
        s.phase = Phase::Up;
        s.want_commit = false;
        s.commit_at = 0;
        s.release_at = 0;
    }
}

/// True while a press is dipping or springing back — the renderer applies [`scale`] only then.
#[inline]
pub fn is_active() -> bool {
    st().phase != Phase::Idle
}

/// The focused CARD has been held down at least [`LONG_MS`] RIGHT NOW (still in the press). Always
/// `false` inside a [`begin_ctl`] press — a control face has no hold gesture, so the caller's whole
/// held-menu chain short-circuits on this one test instead of each of its arms declining in turn.
/// **This is the one the hold menu opens on** (`app.rs`'s press block → `ui::item_menu`): firing
/// while the key is still down is what makes it read as a hold rather than a delayed tap.
pub fn is_long(now: u32) -> bool {
    let s = st();
    s.holdable && s.phase == Phase::Down && now.wrapping_sub(s.down_at) >= LONG_MS
}

/// The current / most-recent press crossed into a press-and-hold (latched at [`LONG_MS`]; stays true
/// until the next [`begin`]) — the AFTER-THE-FACT form of [`is_long`], for a caller that wants to
/// branch tap-vs-hold on the release rather than act the instant the threshold is crossed.
///
/// Nothing reads it today: the item menu deliberately opens on the live [`is_long`] instead, so the
/// panel is up while the finger is still down. Kept because the latch it reports is what makes the
/// distinction observable at all, and a screen whose hold action can only run on release (one that
/// must not fire mid-press) needs exactly this.
pub fn was_long() -> bool {
    st().long
}

/// Current press scale-factor to multiply the focused tile's scale by (`1.0` when idle).
#[inline]
pub fn scale() -> f32 {
    st().sp.pos
}

/// Advance the press spring + phase machine one frame. Poll [`take_commit`] afterwards for the
/// deferred activation.
pub fn tick(now: u32, dt: f32) {
    let s = st();
    match s.phase {
        Phase::Idle => {}
        Phase::Down => {
            s.sp.step(DIP, K_DOWN, dt); // fast, non-bouncy dip
                                        // Long-press latch: once held past LONG_MS this is a press-and-hold, NOT a tap — cancel
                                        // the normal activation so it can never launch (the hard cap below would otherwise fire
                                        // it). The press then just holds the dip and springs back; whether anything HAPPENS is
                                        // the caller's business, read off `is_long` (Home and the detail page's episode
                                        // filmstrip open the item context menu there; every other screen leaves a hold as a
                                        // deliberate no-op).
            if s.holdable && s.want_commit && now.wrapping_sub(s.down_at) >= LONG_MS {
                s.want_commit = false;
                s.long = true;
            }
            // Resolve the hold. The PRIMARY trigger is the real key-up (once the dip has shown for
            // ≥ MIN_DIP_MS): the activation WAITS for the physical release. The other two are
            // dropped-key-up SAFETY only — `lost` fires when auto-repeat heartbeats were arriving and
            // then stopped (gated on `got_beat`, so THIS remote's OK, which never repeats, is not
            // mistaken for a lost release — the "launches before release" bug), and `capped` is the
            // last-resort ceiling when no heartbeat ever arrives.
            let released = s.release_at != 0 && reached(now, s.down_at.wrapping_add(MIN_DIP_MS));
            let lost = s.got_beat && now.wrapping_sub(s.alive) > LOST_MS;
            let capped = now.wrapping_sub(s.down_at) > MAX_HOLD_MS;
            if released || lost || capped {
                s.phase = Phase::Up;
                if s.want_commit {
                    s.commit_at = now.wrapping_add(COMMIT_MS).max(1);
                }
            }
        }
        Phase::Up => {
            s.sp.step_zeta(REST, K_UP, ZETA_UP, dt); // underdamped overshoot back to rest
            if (s.sp.pos - REST).abs() < 0.002 && s.sp.vel.abs() < 0.01 {
                s.sp.jump(REST);
                s.phase = Phase::Idle;
            }
        }
    }
}

/// One-shot: `true` exactly once, when a released press's bounce has played long enough to commit the
/// activation. A cancelled press never returns `true`.
pub fn take_commit(now: u32) -> bool {
    let s = st();
    if s.want_commit && !s.took && reached(now, s.commit_at) {
        s.took = true;
        s.want_commit = false;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    //! **The two press KINDS**, which is the whole of what [`begin_ctl`] added: a card's hold is a
    //! second gesture and swallows the tap, a control face's hold is nothing and must not.
    //!
    //! Every one of these drives the real module, and the module is a crate GLOBAL — `app.rs`'s
    //! `exit_alert_tests` now drive it too, from another file — so each takes `testlock::serial()`
    //! for its whole body and leaves the spring back at rest on the way out.
    use super::*;

    /// Tick the machine forward `ms` from `now` at ~60 Hz, reporting whether the activation
    /// committed anywhere in that span. The loop is the per-frame one in `app.rs`, minus the route
    /// dispatch: `tick` then `take_commit`, in that order, every frame.
    fn run(now: &mut u32, ms: u32) -> bool {
        let end = now.wrapping_add(ms);
        let mut committed = false;
        while now.wrapping_sub(end) >= 0x8000_0000 {
            *now = now.wrapping_add(16);
            tick(*now, 0.016);
            committed |= take_commit(*now);
        }
        committed
    }

    /// Put the global back at rest, whatever state a test left it in.
    fn rest(now: &mut u32) {
        cancel();
        run(now, 2000);
        assert!(
            !is_active(),
            "the spring must settle, or the next test starts mid-dip"
        );
    }

    /// **The one the design system asks for**: a control face dips inward on the press, and the dip
    /// is a factor *below* rest that the renderer multiplies the focus scale by (`--press-dip`).
    #[test]
    fn a_control_press_dips_inward_and_rings_back_past_rest() {
        let _g = crate::testlock::serial();
        let mut now = 1000;
        begin_ctl(now);
        run(&mut now, 100);
        let dipped = scale();
        assert!(
            dipped < 0.99,
            "the press must be visible as a dip, got {dipped}"
        );
        assert!(
            dipped >= DIP - 0.001,
            "…and must not go past the dip it is aiming at, got {dipped}"
        );
        release(now);
        // the RING: the release is underdamped, so somewhere in the spring-back the face is larger
        // than it rests at. This is the half `--ease-bounce` names and the only bounce in the app.
        let mut over = false;
        for _ in 0..40 {
            now = now.wrapping_add(16);
            tick(now, 0.016);
            let _ = take_commit(now);
            over |= scale() > REST + 0.005;
        }
        assert!(
            over,
            "the release must overshoot — a critically damped one would not ring"
        );
        rest(&mut now);
    }

    /// A control face has no hold gesture, so a firmly-held OK still activates on the release. The
    /// same hold on a CARD is a press-and-hold and activates nothing — that asymmetry IS the
    /// difference between the two entry points, and it is why buttons could not simply call
    /// [`begin`].
    #[test]
    fn a_held_control_still_activates_where_a_held_card_would_not() {
        let _g = crate::testlock::serial();
        let mut now = 1000;

        begin_ctl(now);
        assert!(
            !run(&mut now, LONG_MS + 100),
            "nothing commits while the key is still down"
        );
        assert!(!is_long(now), "a control press is never a long press");
        release(now);
        assert!(
            run(&mut now, 400),
            "a control held past LONG_MS must still activate on release"
        );
        rest(&mut now);

        begin(now);
        run(&mut now, LONG_MS + 100);
        assert!(is_long(now), "the same hold on a card IS a long press…");
        release(now);
        assert!(!run(&mut now, 400), "…and a long press activates nothing");
        rest(&mut now);
    }

    /// The dropped-key-up net, which is the one place the two kinds visibly part. A card has
    /// latched long by [`MAX_HOLD_MS`] and springs back inert; a control never latches, so the
    /// ceiling is what finally fires it — a button whose release never arrives acts once rather
    /// than never.
    #[test]
    fn a_control_press_whose_release_never_arrives_commits_at_the_ceiling() {
        let _g = crate::testlock::serial();
        let mut now = 1000;
        begin_ctl(now);
        assert!(
            !run(&mut now, MAX_HOLD_MS - 100),
            "…but not before the ceiling"
        );
        assert!(
            run(&mut now, 400),
            "the ceiling must resolve a control press as an activation"
        );
        rest(&mut now);

        begin(now);
        assert!(
            !run(&mut now, MAX_HOLD_MS + 400),
            "the same on a card is a hold, and commits nothing"
        );
        rest(&mut now);
    }

    /// Navigation (or a fresh click) aborts the press: the face springs back and the activation
    /// never runs. Identical for both kinds — "you slid off the control".
    #[test]
    fn a_cancelled_control_press_springs_back_without_activating() {
        let _g = crate::testlock::serial();
        let mut now = 1000;
        begin_ctl(now);
        run(&mut now, 100);
        cancel();
        assert!(!run(&mut now, 600), "a cancelled press must never commit");
        assert!(!is_active(), "and it must reach rest on its own");
        assert!((scale() - REST).abs() < 0.001);
    }

    /// A tap shorter than [`MIN_DIP_MS`] still shows its dip: the release waits out the floor
    /// rather than cutting the animation off, which is what keeps a flash-quick press from being
    /// invisible on a control that stays on screen.
    #[test]
    fn a_flash_quick_tap_still_shows_the_dip_before_it_rings() {
        let _g = crate::testlock::serial();
        let mut now = 1000;
        begin_ctl(now);
        run(&mut now, 16);
        release(now); // released almost immediately — well inside MIN_DIP_MS
        run(&mut now, MIN_DIP_MS - 32);
        assert!(
            scale() < REST - 0.01,
            "the dip must still be on screen at the floor"
        );
        assert!(
            run(&mut now, 500),
            "…and the activation still commits after it"
        );
        rest(&mut now);
    }
}
