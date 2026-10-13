//! **The key-hold half of a hold-to-scrub gesture**, owned once for both transports that have one:
//! the player HUD's scrubber (`screens::player::input::Scrub`) and the trailer transport's
//! (`screens::detail::trailer::Transport`).
//!
//! The two screens cannot name each other (`ci/check-deps.sh`'s `sibling` gate), so before this
//! type each carried its own copy of the same seven fields and the same five rules. A rule changed
//! in one and not the other is a bug the compiler cannot see — the key-bounce debounce below was
//! the first change that would have had to be written twice. What stays with each screen is what
//! genuinely differs: the preview position, the tap debounce, the HUD's reveal-only press, the
//! pointer drag and the seek request they each make.
//!
//! This remote emits a HELD key as auto-repeat keydowns (~50 ms apart, the first ~500 ms after
//! the press) followed by ONE keyup. Two measured facts shape the rules:
//!
//! * **The driver bounces.** Mid-hold it delivers `Up` and then a fresh `Down` 0-17 ms later
//!   (television event log, 2026-10-13). A release is therefore held back for
//!   [`SCRUB_RELEASE_DEBOUNCE_MS`]; the same key coming back inside it is the same hold, and the
//!   re-press restarts the remote's repeat delay, so the lost-keyup net waits
//!   [`SCRUB_REPEAT_RESTART_MS`] until a repeat proves the key is down.
//! * **Keyups can be lost.** [`SCRUB_LOST_MS`] without a repeat is a release that never arrived.
//!
//! Pure: every method takes `now` (`SDL_GetTicks`), so the host suite grades it without a clock.

use crate::player_hud::{
    scrub_speed, SCRUB_LOST_MS, SCRUB_RELEASE_DEBOUNCE_MS, SCRUB_REPEAT_RESTART_MS,
};

/// A direction key's hold, as far as scrubbing cares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeldKey {
    /// -1 back / +1 forward / 0 = no gesture in progress.
    pub dir: i32,
    /// A repeat arrived: the continuous accelerating scrub is engaged.
    pub engaged: bool,
    /// When the hold engaged — the acceleration ramp is measured from here.
    pub since: u32,
    /// The last continuous-advance tick.
    t: u32,
    /// The last event that proved the key down (a press or a repeat).
    alive: u32,
    /// A held key's release is waiting out the bounce window and commits at this tick (`0` = none).
    release_at: u32,
    /// The key bounced and came back, so the repeat delay restarted and no repeat has arrived yet.
    rearmed: bool,
}

impl HeldKey {
    /// No gesture in progress — where the screen starts.
    pub const IDLE: HeldKey = HeldKey {
        dir: 0,
        engaged: false,
        since: 0,
        t: 0,
        alive: 0,
        release_at: 0,
        rearmed: false,
    };

    fn sign(fwd: bool) -> i32 {
        if fwd {
            1
        } else {
            -1
        }
    }

    /// Start a WHOLE gesture in `now`/`fwd` — every field, not the few a press happens to care
    /// about. A gesture can end without [`disengage`](Self::disengage) (a pointer drag's commit, a
    /// fresh press on top of one), so `since` must not be trusted from a previous one: the ramp is
    /// measured from it, and a stale value runs the first frame of a new press at the cap.
    pub fn begin(&mut self, now: u32, fwd: bool) {
        *self = HeldKey { dir: Self::sign(fwd), since: now, t: now, alive: now, ..Self::IDLE };
    }

    /// A fresh press that continues or starts a gesture the screen already decided to run: stamp
    /// the key alive and point it at `fwd`. From rest it also starts the clocks.
    pub fn press(&mut self, now: u32, fwd: bool) {
        if self.dir == 0 {
            self.t = now;
            self.since = now;
        }
        self.alive = now;
        self.dir = Self::sign(fwd);
    }

    /// Is a held key's release still waiting out the bounce window?
    pub fn release_pending(&self) -> bool {
        self.release_at != 0
    }

    /// Is a press of `fwd` at `now` the held key COMING BACK inside the bounce window?
    pub fn continues(&self, fwd: bool, now: u32) -> bool {
        self.release_at != 0
            && self.engaged
            && self.dir == Self::sign(fwd)
            && now.wrapping_sub(self.release_at) >= 0x8000_0000
    }

    /// The bounce: if this press is the held key coming back, cancel the pending release, note the
    /// restarted repeat delay and report `true` — the caller then does NOTHING else with the press
    /// (no hop, no commit, the ramp keeps its speed).
    pub fn bounce(&mut self, now: u32, fwd: bool) -> bool {
        if !self.continues(fwd, now) {
            return false;
        }
        self.release_at = 0;
        self.rearmed = true;
        self.alive = now;
        true
    }

    /// A hardware auto-repeat. Returns `true` on the one that ENGAGES the continuous scrub. A
    /// repeat proves the key is down, so a release still waiting out the bounce window was one, and
    /// the restarted repeat delay is over.
    pub fn repeat(&mut self, now: u32) -> bool {
        if self.dir == 0 {
            return false;
        }
        self.alive = now;
        self.release_at = 0;
        self.rearmed = false;
        if self.engaged {
            return false;
        }
        self.engaged = true;
        self.since = now;
        self.t = now;
        true
    }

    /// The key went up. A held key's release is deferred (see the module doc) and this returns
    /// `true`; anything else is a tap, which the screen debounces its own way, and returns `false`.
    pub fn release(&mut self, now: u32) -> bool {
        if !self.engaged || self.dir == 0 {
            return false;
        }
        self.release_at = now.wrapping_add(SCRUB_RELEASE_DEBOUNCE_MS).max(1);
        true
    }

    /// Has the pending release survived the bounce window? Then it was the real one.
    pub fn release_due(&self, now: u32) -> bool {
        self.release_at != 0
            && self.engaged
            && self.dir != 0
            && now.wrapping_sub(self.release_at) < 0x8000_0000
    }

    /// The preview's travel since the last call, in signed ns: the accelerating ramp at the
    /// speed this hold has reached. A stalled frame cannot teleport the preview — one advance
    /// covers at most 100 ms. `0` unless the continuous scrub is engaged.
    pub fn advance_ns(&mut self, now: u32) -> i64 {
        if self.dir == 0 || !self.engaged {
            return 0;
        }
        let speed = scrub_speed(now.wrapping_sub(self.since));
        let sdt = (now.wrapping_sub(self.t) as f32 / 1000.0).min(0.1);
        self.t = now;
        (self.dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64
    }

    /// Has the key gone quiet for longer than a held key can — a keyup that never arrived? After
    /// a bounce the quiet is the restarted repeat delay, which is longer.
    pub fn lost(&self, now: u32) -> bool {
        let after = if self.rearmed { SCRUB_REPEAT_RESTART_MS } else { SCRUB_LOST_MS };
        now.wrapping_sub(self.alive) > after
    }

    /// End the gesture: no direction, no continuous hold, nothing pending.
    pub fn disengage(&mut self) {
        *self = Self::IDLE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(now: u32) -> HeldKey {
        let mut k = HeldKey::IDLE;
        k.press(now, true);
        assert!(k.repeat(now + 500), "the first repeat engages");
        k
    }

    #[test]
    fn only_the_first_repeat_engages_and_a_stray_one_is_ignored() {
        let mut k = HeldKey::IDLE;
        assert!(!k.repeat(10), "no gesture, nothing to engage");
        k.press(100, false);
        assert!(k.repeat(600));
        assert!(!k.repeat(650), "already engaged");
        assert_eq!((k.dir, k.engaged, k.since), (-1, true, 600));
    }

    #[test]
    fn a_release_waits_out_the_window_and_a_returning_key_cancels_it() {
        let mut k = held(1_000);
        assert!(k.release(2_000));
        assert!(!k.release_due(2_000 + SCRUB_RELEASE_DEBOUNCE_MS - 1));
        assert!(k.release_due(2_000 + SCRUB_RELEASE_DEBOUNCE_MS));

        let mut k = held(1_000);
        k.release(2_000);
        assert!(!k.continues(false, 2_001), "the other direction is not the same key");
        assert!(k.bounce(2_001, true));
        assert!(!k.release_pending());
        assert!(!k.bounce(2_002, true), "nothing left to cancel");
        assert!(k.engaged, "the hold survived");
    }

    #[test]
    fn a_press_after_the_window_is_a_new_gesture_not_a_bounce() {
        let mut k = held(1_000);
        k.release(2_000);
        assert!(!k.continues(true, 2_000 + SCRUB_RELEASE_DEBOUNCE_MS + 1));
    }

    #[test]
    fn the_lost_keyup_net_waits_longer_after_a_bounce_until_a_repeat_arrives() {
        let mut k = held(1_000);
        k.repeat(1_950);
        assert!(!k.lost(1_950 + SCRUB_LOST_MS));
        assert!(k.lost(1_950 + SCRUB_LOST_MS + 1));

        k.release(2_000);
        assert!(k.bounce(2_001, true));
        assert!(!k.lost(2_001 + SCRUB_LOST_MS + 100), "inside the restarted repeat delay");
        assert!(k.lost(2_001 + SCRUB_REPEAT_RESTART_MS + 1));
        k.repeat(2_560);
        assert!(k.lost(2_560 + SCRUB_LOST_MS + 1), "a repeat puts the short net back");
    }

    #[test]
    fn the_advance_follows_the_ramp_and_never_covers_more_than_a_tenth_of_a_second() {
        let mut k = held(1_000); // engaged at 1_500
        let first = k.advance_ns(1_516);
        assert!(first > 0 && first < 2_000_000_000, "{first}");
        let stalled = k.advance_ns(1_516 + 5_000);
        let cap = (scrub_speed(5_016) as f64 * 0.1 * 1e9) as i64;
        assert!((stalled - cap).abs() < 10_000, "a 5 s frame advances one tenth of a second's worth: {stalled} vs {cap}");

        let mut back = HeldKey::IDLE;
        back.press(0, false);
        back.repeat(500);
        assert!(back.advance_ns(600) < 0, "backwards travels negative");
        let mut idle = HeldKey::IDLE;
        assert_eq!(idle.advance_ns(10), 0, "idle never moves");
    }

    #[test]
    fn begin_forgets_a_previous_gestures_clocks() {
        let mut k = held(1_000);
        k.release(2_000);
        k.begin(900_000, false);
        assert_eq!((k.dir, k.engaged, k.since), (-1, false, 900_000));
        assert!(!k.release_pending());
        assert_eq!(k.advance_ns(900_050), 0, "not engaged: a tap slews nothing");
    }
}
