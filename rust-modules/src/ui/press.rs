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
/// The CONTROL FOCUS POP's spring — [`widgets::CtlPop`](crate::ui::widgets::CtlPop) — which is
/// deliberately this module's release spring and not one of its own.
///
/// The design system names exactly two things in the app that bounce: the press release, and a
/// control face arriving at focus (`tokens/motion.css`, `--ease-bounce`). Two underdamped springs
/// tuned separately would be two rates for one gesture — the pop and the click land on the same
/// object, often within a few hundred milliseconds of each other — so the pop borrows the numbers
/// rather than restating them, and moving the click moves both.
pub(crate) const K_POP: f32 = K_UP;
pub(crate) const ZETA_POP: f32 = ZETA_UP;
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
    down_at: u32,    // tick at press-down (long-press timing)
    alive: u32,      // last liveness tick (press-down + OK auto-repeats) — the dropped-key-up net
    got_beat: bool,  // an auto-repeat heartbeat has arrived → `alive` is meaningful for LOST detection
    release_at: u32, // tick of the real key-up (0 = still held)
    commit_at: u32,  // tick at which the activation may fire (0 = none scheduled)
    want_commit: bool, // false after a cancel or the long-press latch — spring back, do NOT activate
    long: bool,      // the hold crossed LONG_MS → a press-and-hold, not a tap (see `was_long`)
    took: bool,      // the caller already consumed the commit
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

/// OK-down on the focused control: begin (or restart) the press-in dip.
pub fn begin(now: u32) {
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

/// The focused control has been held down at least [`LONG_MS`] RIGHT NOW (still in the press).
/// **This is the one the hold menu opens on** (`app.rs`'s press block → `ui::item_menu`): firing
/// while the key is still down is what makes it read as a hold rather than a delayed tap.
pub fn is_long(now: u32) -> bool {
    let s = st();
    s.phase == Phase::Down && now.wrapping_sub(s.down_at) >= LONG_MS
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
            if s.want_commit && now.wrapping_sub(s.down_at) >= LONG_MS {
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
