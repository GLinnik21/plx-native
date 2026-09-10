//! **The player's INPUT state, owned by [`crate::screens::player::PlayerScreen`]** (restructure
//! spec §9, phase 9): the held-key timer, the repeat gate, the scrub gesture, the HUD's focus
//! cursor and the HUD's own timer/dismissal state.
//!
//! Every type here was a `plex_run` loop local through phase 1b-i and an `App` field from 1b-ii
//! on; phase 9 moves them into the player's `Screen` instance unchanged, which is what §2.2 means
//! by "HUD/scrub/held/up_next/overlays belong to `PlayerScreen`". Two fields are NEW here and were
//! `player::TX` atomics before — [`HudState::until`] and [`Scrub::ns`] — because a decision read
//! from another thread is a PUBLISHED snapshot of state its owner holds, not state that lives in
//! the atomic (§2.3). [`crate::screens::player::PlayerScreen::publish`] writes both into `TX`
//! after every step, and is the only writer.
//!
//! The predicates are pure and take `now` rather than calling `SDL_GetTicks`, so the host suite
//! can grade them: `hud_visibility_tests` and `repeat_gate_tests` moved here under their own names
//! from `app/playback.rs` (§11's test-module homes).

use std::os::raw::c_int;

/// Is the transport HUD on screen? Its timer is live, OR playback is paused, OR the pipeline is
/// BUSY — unless the user explicitly dismissed it (UP from the top row), which holds until the
/// next key but cannot hide a stalled pipeline's read-out.
///
/// **The `loading()` term is load-bearing and there must be exactly ONE predicate.** The draw path
/// and the pointer path used to spell it out inline while the three KEY sites and the focus PARKER
/// did not, and the divergence was worst in the one state this app most needs a user to report:
/// stuck in `Buffering` with the 4.5 s linger expired, the transport is drawn, but every key site
/// believed it hidden — so the parker reset `hud.nav` to the scrubber on EVERY frame, UP was eaten
/// as a "reveal", and focus could not reach the control row at all. The `…` disc, and the
/// diagnostics read-out behind it, were unreachable in exactly the stall they explain.
///
/// It was briefly TWO functions, a timer-only `hud_shown` wrapped by this one. That is the same
/// trap with a friendlier name on it — seven call sites, no compiler help, and "shown" is the
/// obvious one to reach for. One predicate, no wrong choice.
#[inline]
pub(crate) fn hud_visible(ps: &crate::route::PlaybackSession, now: u32, until: u32, is_paused: bool, dismissed: bool) -> bool {
    ((now < until || is_paused) && !dismissed) || crate::player::loading(ps)
}

/// WHICH key the remote is holding down, as one value: the sym the client-side repeat timer
/// is driving, the two instants that timer reads, the hardware heartbeat that catches a
/// dropped key-up, and the sym we watched go physically down.
///
/// They are bundled because the two per-frame rules at the bottom of the loop each read
/// three of the five together — the lost-keyup net tests `sym`, `since` and `alive`, and the
/// repeat itself tests `sym`, `since` and `last_rep` — while every arm that arms a hold
/// writes the same three fields in the same order.
pub(crate) struct HeldKey {
    pub(crate) sym: u32,      // the key the client-side repeat is driving; 0 = nothing held
    pub(crate) since: u32,    // when it was armed — the repeat's initial delay is measured from here
    pub(crate) last_rep: u32, // when that repeat last fired
    pub(crate) alive: u32,    // last hardware 0x101 for the held key — a lost-keyup liveness net
    /// The sym we believe is PHYSICALLY DOWN right now — set by a fresh key-down, cleared by
    /// its key-up. It exists to tell a real hardware auto-repeat from a PHANTOM one, which
    /// this TV emits routinely and which the repeat guard below would otherwise swallow.
    ///
    /// Device-measured 2026-08-15, over the system keyboard: the panel does not deliver a
    /// key-up for the press that raised it (`RETURN` down at t=326491 with no up until the
    /// panel's own session ends), so LG's key driver still believes OK is held and stamps
    /// the NEXT press with `state & 0x100`. The guard read that as a repeat and dropped it,
    /// so the first OK after every keyboard session did nothing and the user pressed twice —
    /// reported as "I have to click the search field twice for the keyboard to appear" and
    /// "Enter twice dismisses it". Both are this one field. A repeat for a key we never saw
    /// pressed is not a repeat.
    pub(crate) down_sym: u32,
}
impl HeldKey {
    /// Nothing held, no hold-repeat pending — where the loop starts.
    pub(crate) const IDLE: HeldKey = HeldKey {
        sym: 0,
        since: 0,
        last_rep: 0,
        alive: 0,
        down_sym: 0,
    };
    /// Arm the client-side hold-repeat for `sym` at `now` — the trio every fresh-press arm
    /// writes together. `alive` and `down_sym` are the hardware's own bookkeeping and are
    /// deliberately untouched here: `alive` is stamped by the 0x101 repeat arm, `down_sym`
    /// by the key-down and key-up edges.
    pub(crate) fn arm(&mut self, sym: u32, now: u32) {
        self.sym = sym;
        self.since = now;
        self.last_rep = now;
    }
}

/// Rate-limits a REPEAT-DRIVEN discrete step — a forwarded hardware auto-repeat, or one tick of a
/// scroll-wheel gesture — to a couch-comfortable cadence, independent of the SOURCE's own cadence.
/// A held hardware key repeats roughly every 50ms; a wheel gesture can deliver several ticks in one
/// pass. Settings, Consent and Legal move a whole table row — or, inside a document, a full page of
/// reading text — per step, so letting either source drive `on_updown` at its own rate reads as a
/// blur rather than a scroll: item 13's whole ask.
///
/// Pure and host-testable — no `SDL_GetTicks` inside; `now` is threaded in by the caller, the same
/// shape `HeldKey`'s own `wrapping_sub` timing takes, so it survives the tick wrap the same way.
pub(crate) struct RepeatGate {
    /// The tick of the last step this gate admitted; `None` before the first one.
    pub(crate) last: Option<u32>,
}
impl RepeatGate {
    /// Minimum time between two repeat-driven steps this gate allows. Slower than the discrete
    /// focus-list repeat (110ms, [`PANEL_REPEAT_MS`]) on purpose — a home-grid card is a glance, a
    /// settings row or a line of reading text is not.
    pub(crate) const STEP_MS: u32 = 160;
    pub(crate) const IDLE: RepeatGate = RepeatGate { last: None };
    /// True at most once per [`Self::STEP_MS`]; always true the first call, or after a gap at
    /// least that long (which is also what makes a long-idle gate behave like a fresh one).
    pub(crate) fn ready(&mut self, now: u32) -> bool {
        self.ready_every(now, Self::STEP_MS)
    }
    /// The same gate at a caller-chosen cadence. The player's four overlay surfaces take
    /// [`PANEL_REPEAT_MS`] through this, which is what preserves the exact hold-to-move feel the
    /// loop's own client-side repeat timer gave them before phase 9 moved their input onto the
    /// dispatcher: the remote streams `Edge::Repeat` at roughly 50 ms, and a list walked at that
    /// rate reads as a blur (see [`PANEL_REPEAT_MS`]).
    pub(crate) fn ready_every(&mut self, now: u32, step_ms: u32) -> bool {
        let due = match self.last {
            None => true,
            Some(last) => now.wrapping_sub(last) >= step_ms,
        };
        if due {
            self.last = Some(now);
        }
        due
    }
    /// **A fresh press restarts the cadence from itself.** The press is acted on unconditionally
    /// by its own arm — a fresh press is never swallowed by the cadence of the press before it —
    /// and this records it as the step it is, so the FIRST hardware repeat of the new hold waits a
    /// full [`PANEL_REPEAT_MS`] rather than landing on top of it.
    ///
    /// It cleared `last` instead until the surface tests were written, which was wrong in exactly
    /// the direction that is invisible on a fresh gate: the remote streams `Edge::Repeat` at ~50 ms
    /// and the first of them arrives with `last: None`, so a held key stepped TWICE before the
    /// cadence engaged, once for the press and once for the repeat behind it.
    pub(crate) fn rearm(&mut self, now: u32) {
        self.last = Some(now);
    }
}

/// **The cadence a held direction walks a player panel's list at**, in ms.
///
/// 110 ms, which is the number `app/run.rs`'s client-side repeat timer used for exactly these four
/// panels (the track menu, the `…` popover, the Info card and the Chapters strip) while their keys
/// went through the loop's own ladder. That timer existed because the Magic Remote streams
/// hardware auto-repeat at ~50 ms and a list walked at that rate is unusable; phase 9 put the
/// panels' input on the dispatcher, where `Edge::Repeat` arrives at the hardware's rate, so the
/// cadence has to be applied by the surface that receives it. Same number, same feel, one owner.
pub(crate) const PANEL_REPEAT_MS: u32 = 110;

#[cfg(test)]
mod repeat_gate_tests {
    use super::{RepeatGate, PANEL_REPEAT_MS};

    #[test]
    fn a_gate_admits_the_first_step_then_holds_the_cadence() {
        let mut gate = RepeatGate::IDLE;
        assert!(gate.ready(1_000), "nothing has fired yet");
        assert!(!gate.ready(1_050), "too soon");
        assert!(!gate.ready(1_159), "still short of the step");
        assert!(gate.ready(1_160), "exactly one step later");
        assert!(!gate.ready(1_161));
    }

    /// SDL ticks wrap at 2^32ms; the same arithmetic `HeldKey`'s lost-keyup net and client-side
    /// repeat already rely on, so this gate must survive it the same way.
    #[test]
    fn the_gate_survives_the_tick_wrap() {
        let mut gate = RepeatGate::IDLE;
        let at = u32::MAX - 50;
        assert!(gate.ready(at));
        assert!(!gate.ready(at.wrapping_add(100)));
        assert!(gate.ready(at.wrapping_add(160)));
    }

    /// The player panels' own cadence, and the reason `ready_every` exists: the remote's ~50 ms
    /// hardware repeat is admitted at 110 ms, not at 160 (a settings row) and not at 50.
    #[test]
    fn a_player_panel_walks_a_held_direction_at_its_own_cadence() {
        let mut gate = RepeatGate::IDLE;
        assert!(gate.ready_every(1_000, PANEL_REPEAT_MS));
        assert!(!gate.ready_every(1_050, PANEL_REPEAT_MS), "the hardware's own rate is too fast");
        assert!(gate.ready_every(1_110, PANEL_REPEAT_MS));
        // …and a FRESH press restarts the cadence FROM ITSELF. The press is acted on by its own
        // arm without consulting the gate, so it is never swallowed; what this pins is the other
        // half, which was wrong until the surface tests caught it — the first hardware repeat of
        // the new hold must wait a full cadence rather than landing on top of the press.
        gate.rearm(1_115);
        assert!(!gate.ready_every(1_165, PANEL_REPEAT_MS), "the repeat behind a fresh press waits");
        assert!(gate.ready_every(1_225, PANEL_REPEAT_MS));
    }
}

/// Scrub-seek gesture state. This Magic Remote emits a HELD key as auto-repeat keydowns
/// (state 0x101, ~50ms apart) followed by ONE keyup on release; a TAP is a lone
/// keydown(0x001)+keyup(0x000). So: a fresh press does the fixed jump; the 0x101 repeats
/// engage the continuous scrub; the keyup is a reliable release. Taps commit on a short
/// debounce so quick taps accumulate.
pub(crate) struct Scrub {
    pub(crate) t: u32,          // last continuous-advance tick
    pub(crate) dir: i32,        // -1 back / +1 forward / 0 = no scrub in progress
    pub(crate) hold: bool,      // a 0x101 repeat arrived → continuous accelerating scrub engaged
    pub(crate) hold_since: u32, // when that hold engaged — the acceleration ramp is measured from here
    pub(crate) alive: u32,      // last held (0x101) event — for the lost-keyup safety commit
    pub(crate) commit_at: u32,  // tap released → commit at this tick (0 = none; a new press cancels)
    /// This gesture began on a HIDDEN HUD, so its press was spent raising the transport
    /// (`ScrubPress::Reveal`) rather than hopping. It is still a fully armed scrub — a user who
    /// keeps holding gets the ordinary continuous rewind, and `hold` engaging clears this — but if
    /// it turns out to have been a TAP, the release must throw the preview away instead of
    /// committing it: the preview sits on the seed, i.e. exactly where playback already is, and
    /// committing that is a full reopen+prime to no effect.
    pub(crate) reveal: bool,
    /// **The PREVIEW position, in ns; `-1` = not scrubbing.**
    ///
    /// This was `player::TX.scrub_ns` and is now the gesture's own field, published back into that
    /// atomic by `PlayerScreen::publish` (§2.3, and the module doc). It sits here rather than
    /// beside the HUD because it IS the gesture — `begin`/`disengage` and the per-frame continuous
    /// advance are the only things that move it, and the draw path reads the published copy.
    pub(crate) ns: i64,
}
impl Scrub {
    /// No scrub in progress and no tap commit pending — where the loop starts.
    pub(crate) const IDLE: Scrub = Scrub {
        t: 0,
        dir: 0,
        hold: false,
        hold_since: 0,
        alive: 0,
        commit_at: 0,
        reveal: false,
        ns: -1,
    };
    /// Start a WHOLE gesture in `now`/`fwd` — every field, not the four a press happens to care
    /// about.
    ///
    /// Two sites end a scrub without [`disengage`](Self::disengage) — the pointer drag's mouse-up
    /// commit, and `key_scrub`'s own drag cancel — and `exit_player` never touches this at all, so
    /// `hold`/`hold_since` can outlive the gesture that set them and even the playback session.
    /// Arming only `dir`/`alive` on top of that leaves the per-frame advance reading a `hold_since`
    /// from minutes ago: its acceleration ramp is measured from there, so the first frame of a
    /// brand-new press runs at `SCRUB_MAX` and one tap slews the preview tens of seconds.
    pub(crate) fn begin(&mut self, now: u32, fwd: bool) {
        self.dir = if fwd { 1 } else { -1 };
        self.hold = false;
        self.hold_since = now;
        self.t = now;
        self.alive = now;
        self.commit_at = 0; // more input → cancel a pending tap commit
        self.reveal = false;
    }
    /// End the gesture: no direction, no continuous hold, and no reveal pending. `commit_at` is
    /// deliberately NOT cleared — four of the five call sites leave a pending tap commit alone, and
    /// the fifth IS that commit and clears the field itself right after calling this.
    pub(crate) fn disengage(&mut self) {
        self.dir = 0;
        self.hold = false;
        self.reveal = false;
    }
}
/// The player HUD's focus cursor: WHICH row owns focus, plus the index WITHIN each of the
/// two indexed rows. One cursor, not three settings — the three are drawn together every
/// frame (`draw_hud`), moved together by UP/DOWN, and, the reason they are bundled here,
/// must be RESET together when a new playback session begins.
///
/// As three loose `plex_run` locals they were never reset at all: `start_playback` sets the
/// route, the resume point and the HUD timer, but the focus cursor survived from the
/// PREVIOUS session — leave one movie with the Subtitles button focused (`focus == 1`),
/// start another, and the first OK opened the track menu instead of pausing. Bundling makes
/// "reset the HUD focus" one assignment that `start_playback` cannot half-do.
#[derive(Clone, Copy)]
pub(crate) struct HudNav {
    pub(crate) focus: i32, // 0 = scrubber, 1 = right buttons (Subtitles/Audio/More), 2 = bottom tabs
    pub(crate) btn: i32,   // 0 = Subtitles, 1 = Audio, 2 = More (within the buttons row)
    pub(crate) tab: i32,   // 0 = Info, 1 = Chapters (within the tabs row)
}
impl HudNav {
    /// Focus parked on the scrubber, both indexed rows on their first item — where a fresh
    /// session starts and where an auto-hidden HUD is re-parked.
    pub(crate) const HOME: HudNav = HudNav {
        focus: 0,
        btn: 0,
        tab: 0,
    };
}
/// Everything the player remembers ABOUT the transport HUD between frames: its auto-hide deadline,
/// where its focus cursor is parked, whether the user dismissed it, and the two control-row edges
/// the per-frame block near the bottom of the loop compares against this frame's slot.
///
/// The cursor keeps its own type ([`HudNav`]) rather than dissolving into fields here: the
/// helpers take it as `&mut HudNav`, and one of them (`start_playback`) is where the per-session
/// reset happens.
///
/// Named `HudState` and not `Hud` so it does not read as `focusprobe::Hud`, which is that
/// module's own snapshot of the cursor plus a computed `visible`, built beside this one at
/// the tail of the loop.
pub(crate) struct HudState {
    /// the focus cursor, reset per session by `start_playback`
    pub(crate) nav: HudNav,
    /// **The auto-hide deadline, as an absolute `SDL_GetTicks` instant** — `0` = expired.
    ///
    /// It was `player::TX.hud_until` and is now this screen's own field (§2.3): the HUD's timer is
    /// a UI decision, made on the main thread, that the draw path also reads — so the owner holds
    /// it and `PlayerScreen::publish` mirrors it into the atomic for that reader. `TX::reset` and
    /// `TX::reset_for_reload` used to clear it from underneath, which is the cross-owner write the
    /// spec's rule exists to remove; a teardown now DELIVERS the reset and the screen answers it.
    pub(crate) until: u32,
    /// UP-from-the-top explicitly dismisses the HUD even while paused; any other player
    /// input clears it. Without this, paused() would force the HUD permanently visible.
    pub(crate) dismissed: bool,
    /// Was the transport ON SCREEN when the key being handled arrived? Sampled by
    /// `begin_fresh_press` at the top of every fresh press, and the ONLY honest answer to that
    /// question by the time an arm runs.
    ///
    /// The arm cannot re-derive it, because the same function clears [`dismissed`] one line later —
    /// and `dismissed` OUTRANKS the timer inside [`hud_visible`]. So a user who hid the transport
    /// by hand (UP from the control row, which deliberately does not extend the timer) and pressed
    /// again inside the remaining linger produced `hud_visible == true` for a HUD that was not on
    /// screen: the press then drove geometry nobody could see — the very thing the two arms below
    /// refuse to do. The pointer path has always sampled BEFORE re-arming for this reason (see the
    /// click arm's `hud_vis`); this is the key path's version of that sample, taken once so the two
    /// arms that need it cannot answer the question differently.
    pub(crate) visible_at_press: bool,
    /// The last SEGMENT the control row offered. Sticky: it is never cleared back to None,
    /// so each segment raises the HUD exactly once per playback however often the row
    /// flickers.
    pub(crate) last_offer: Option<(crate::metadata::MarkerKind, i64)>,
    /// Did a stand-in own the control row last frame? The reset below is the EDGE of a
    /// stand-in vanishing under the focus ring — see `player_hud::standin_left_the_ring`,
    /// which is where that rule is written down and tested.
    pub(crate) was_standin: bool,
}
impl HudState {
    /// Focus at rest, no timer, nothing dismissed, no segment seen yet, discs in the control row.
    pub(crate) const IDLE: HudState = HudState {
        nav: HudNav::HOME,
        until: 0,
        dismissed: false,
        visible_at_press: false,
        last_offer: None,
        was_standin: false,
    };

    /// Is the transport on screen right now, by this HUD's own state?
    #[inline]
    pub(crate) fn visible(&self, ps: &crate::route::PlaybackSession, now: u32, is_paused: bool) -> bool {
        hud_visible(ps, now, self.until, is_paused, self.dismissed)
    }

    /// Raise the HUD to at least `now + ms`, never PULLING IN a deadline already further out.
    ///
    /// [`until`](Self::until) is an absolute instant, so an unconditional write SHORTENS whatever
    /// was there. The headless capture path pins the HUD for `HUD_HEADLESS_MS` (60 s), and the
    /// marker prompts fire mid-playback — writing unconditionally there cut that pin to the 4.5 s
    /// linger and the transport vanished out from under a live Skip button (seen on device, not in
    /// review). Comparison is plain `>`, matching [`hud_visible`]'s own non-wrapping `now < until`.
    #[inline]
    pub(crate) fn extend(&mut self, now: u32, ms: u32) {
        let want = now.saturating_add(ms).max(1);
        if want > self.until {
            self.until = want;
        }
    }

    /// A fresh key the app BINDS has arrived: record what the transport LOOKED like to the user,
    /// then clear the dismissal it may have been carrying.
    ///
    /// The two are one operation and the ORDER is the whole point — `dismissed` outranks the timer
    /// inside [`hud_visible`], so sampling after the clear reports a hand-hidden transport as being
    /// on screen (see [`visible_at_press`](Self::visible_at_press)). Written down as one function
    /// rather than two lines in its caller so that the order is a thing a test can hold still,
    /// instead of a convention a later edit can quietly transpose.
    ///
    /// That caller is `note_global_press`, NOT `begin_fresh_press` as it once was, and the
    /// difference is the point of the split: an unsupported key never gets here at all.
    pub(crate) fn note_fresh_press(&mut self, ps: &crate::route::PlaybackSession, now: u32, is_paused: bool) {
        self.visible_at_press = self.visible(ps, now, is_paused);
        // Any BOUND fresh key un-dismisses the HUD (UP-hide re-sets it). "Bound" and not "any" is
        // the whole of `note_global_press`, the ONLY caller: an unsupported press never reaches
        // here, so a colour button over a film no longer raises the transport.
        self.dismissed = false;
    }

    /// A FRESH segment offer takes the control row: put the HUD ON SCREEN and, from rest, park the
    /// ring on the row's primary so a bare OK acts on the offer in one press instead of
    /// raise-HUD → navigate → OK.
    ///
    /// **Clearing the dismissal is half of "on screen", and leaving it out was the bug.**
    /// [`extend`](Self::extend) moves only the TIMER, and `dismissed` outranks the timer outright
    /// inside [`hud_visible`] — so a user who UP-hid the transport mid-episode and then touched
    /// nothing carried that dismissal into the credits, and the Up Next tile was offered to a HUD
    /// that `draw_hud` was never called for. Being invisible it then lost its ring to the auto-hide
    /// re-park at the bottom of the same block, which the NEXT frame's steady-state cancel rule
    /// (`crate::ui::up_next::countdown_may_run`) read as the user walking away — latching the
    /// countdown off for the whole segment. The tile appeared only if the HUD was raised by hand,
    /// with its auto-advance already dead: exactly as reported. A dismissal is a "not now" that any
    /// key the app BINDS clears (an unsupported one clears nothing — `note_global_press`); a
    /// segment beginning is that same kind of event, arriving from the player instead of the
    /// remote, and it must clear it too — which is also what makes an offer behave the same
    /// whether the HUD auto-hid or was hidden on purpose.
    ///
    /// Parking is only ever from REST: a user who walked to the Subtitles disc or an Info tab keeps
    /// their spot (and for Up Next thereby declines the countdown — the same one rule, read as a
    /// steady state one block below). `primary` is the occupant's own
    /// ([`crate::ui::player_hud::ControlSlot::primary_btn`]) — item 0 for a Skip pill, the
    /// RIGHT-hand one for Up Next, where parking on item 0 would disarm the timer on the frame
    /// after it armed.
    pub(crate) fn raise_for_offer(&mut self, now: u32, primary: c_int) {
        self.extend(now, HUD_LINGER_MS);
        self.dismissed = false;
        if self.nav.focus == 0 {
            self.nav.focus = 1;
            self.nav.btn = primary;
        }
    }
}
// scrub tuning: a press jumps SCRUB_STEP_NS; holding engages a continuous scrub ramping
// SCRUB_BASE→SCRUB_MAX (playback-seconds per real-second).
pub(crate) const SCRUB_STEP_NS: i64 = 10_000_000_000; // 10s per press
pub(crate) const SCRUB_BASE: f32 = 10.0;
pub(crate) const SCRUB_ACCEL: f32 = 45.0; // added per second of hold
pub(crate) const SCRUB_MAX: f32 = 140.0;
// tap released → commit after this (further taps accumulate). Long enough that a rapid
// ±10s tap burst coalesces into ONE seek — each separate commit is a full reopen+prime on
// the engine, and back-to-back in-flight seeks are what race the demux (the stale-audio
// silence incident); short enough that a single tap still feels immediate.
pub(crate) const TAP_COMMIT_MS: u32 = 450;
pub(crate) const SCRUB_LOST_MS: u32 = 400; // holding but no repeat this long → lost keyup → commit
                                // HUD auto-hide: how long the HUD lingers after the input that raised it.
pub(crate) const HUD_LINGER_MS: u32 = 4500; // plain transport/nav input
pub(crate) const HUD_MENU_MS: u32 = 8000; // a modal menu is up (track/chapter nav) — longer read time
pub(crate) const HUD_HEADLESS_MS: u32 = 60_000; // autoplay/headless runs pin the HUD up for capture

/// What a LEFT/RIGHT press on the player route is spent on, given the transport's visibility and
/// where the HUD's ring is parked. Pure, and the only arm of `key_scrub` a host test can reach.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScrubPress {
    /// The transport was not on screen: the press raises it and moves nothing.
    Reveal,
    /// A scrub-seek hop/gesture on the bar.
    Jump,
    /// Walk the control row.
    Row,
    /// Walk the bottom tabs.
    Tabs,
    /// On the bar with nothing to seek through.
    Nothing,
}

/// The decision above, written once so the key arm and its test cannot disagree.
pub(crate) fn scrub_press(hud_vis: bool, focus: i32, seekable: bool) -> ScrubPress {
    if !hud_vis {
        return ScrubPress::Reveal;
    }
    match focus {
        1 => ScrubPress::Row,
        2 => ScrubPress::Tabs,
        _ if seekable => ScrubPress::Jump,
        _ => ScrubPress::Nothing,
    }
}

/// What a key does while the player is showing its terminal failure read-out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FailedKeyAction {
    /// OK opens the shared quality picker — the read-out's own recovery path.
    ChooseQuality,
    /// BACK leaves the failed playback (or closes whatever panel is over it).
    Return,
    /// Everything else is swallowed by the read-out.
    Ignore,
}

/// The failure read-out's key policy, pure so it can be graded without a player.
pub(crate) fn failed_key_action(ok: bool, back: bool) -> FailedKeyAction {
    if ok {
        FailedKeyAction::ChooseQuality
    } else if back {
        FailedKeyAction::Return
    } else {
        FailedKeyAction::Ignore
    }
}

#[cfg(test)]
mod failed_player_input_tests {
    use super::{failed_key_action, FailedKeyAction};

    #[test]
    fn a_terminal_failure_has_a_forward_escape_and_a_back_escape() {
        assert_eq!(
            failed_key_action(true, false),
            FailedKeyAction::ChooseQuality
        );
        assert_eq!(failed_key_action(false, true), FailedKeyAction::Return);
        assert_eq!(failed_key_action(false, false), FailedKeyAction::Ignore);
    }
}

/// The transport's visibility predicate, and the one state transition that has to OUTRANK it —
/// a fresh control-row offer. Almost nothing else on the player's input path is host-testable — it
/// is the SDL event loop — but these two are, and between them they encode the bugs that cost the
/// diagnostics overlay its whole reason for existing and the Up Next tile its auto-advance.
///
/// Moved here from `app/playback.rs` with its name in phase 9 (§11). Its `with_state` helper still
/// holds `testlock::serial()`: `hud_visible`'s `loading()` term reads the crate-global playback
/// state, which is what these cases drive — the HUD's own timer is a screen field now and needs no
/// lock, but that global has not moved and will not until the `Player` machine lands.
#[cfg(test)]
mod hud_visibility_tests {
    use super::*;
    use crate::player::PlaybackState;

    /// Drive the derived playback state through the field the pump owns. Crate-global, so the whole
    /// body holds `testlock::serial()` — `state()` is read by other modules' tests too.
    fn with_state<T>(s: PlaybackState, f: impl FnOnce() -> T) -> T {
        let _g = crate::testlock::serial();
        let prev = crate::player::swap_state_for_test(s);
        let out = f();
        crate::player::restore_state_for_test(prev);
        out
    }

    /// THE regression. Stuck in `Buffering` with the linger long expired and nothing paused, the
    /// timer predicate says hidden while the transport is in fact drawn — so every key site and the
    /// focus parker must use the STATE-aware one, or focus is reset to the scrubber every frame and
    /// the `…` disc cannot be reached in the one state worth reporting.
    #[test]
    fn a_stalled_pipeline_keeps_the_transport_reachable_after_the_linger_expires() {
        let ps = crate::route::PlaybackSession::IDLE;
        with_state(PlaybackState::Buffering, || {
            // the timer alone would say hidden — 9 s past the linger, nothing paused
            let (now, expired) = (10_000u32, 1_000u32);
            assert!(
                hud_visible(&ps, now, expired, false, false),
                "on screen, so keys must reach it"
            );
        });
    }

    /// While playing normally the two agree — an expired linger really does mean hidden, or the HUD
    /// would never auto-hide at all.
    #[test]
    fn a_healthy_playing_pipeline_still_auto_hides() {
        let ps = crate::route::PlaybackSession::IDLE;
        with_state(PlaybackState::Playing, || {
            assert!(!hud_visible(&ps, 10_000, 1_000, false, false));
            assert!(hud_visible(&ps, 500, 1_000, false, false), "inside the linger");
            assert!(hud_visible(&ps, 10_000, 1_000, true, false), "paused pins it up");
        });
    }

    /// **A LEFT/RIGHT press that finds the HUD hidden is spent RAISING it** — the rule `key_scrub`
    /// is built around, and the one arm of that ladder no other test can reach (every other branch
    /// of it drives the player's globals from inside the SDL loop).
    ///
    /// The pairing is the point: whatever the cursor is parked on, an invisible transport takes the
    /// press for itself, and the SAME cursor acts normally the moment the transport is on screen.
    /// Focus survives an auto-hide (`HudNav::HOME` is re-parked one block down in the loop), so
    /// "hidden but focus == 1" is an ordinary state, not a corner.
    #[test]
    fn a_hidden_hud_spends_the_press_on_itself() {
        for focus in [0, 1, 2] {
            for seekable in [false, true] {
                assert_eq!(
                    scrub_press(false, focus, seekable),
                    ScrubPress::Reveal,
                    "hidden HUD, focus {focus}: the press raises it and moves nothing"
                );
            }
        }
        // …and visible, the same three cursors act — this is what the reveal DEFERS to, one press later
        assert_eq!(scrub_press(true, 0, true), ScrubPress::Jump);
        assert_eq!(scrub_press(true, 1, true), ScrubPress::Row);
        assert_eq!(scrub_press(true, 2, true), ScrubPress::Tabs);
        // the scrubber with nothing to move through is still not a Jump
        assert_eq!(scrub_press(true, 0, false), ScrubPress::Nothing);
    }

    /// The HUD's own deadline arithmetic: `extend` never pulls a longer pin in, which is the rule
    /// the headless 60 s capture pin and the mid-playback marker prompts both depend on.
    #[test]
    fn extending_the_hud_never_shortens_a_longer_pin() {
        let mut hud = HudState::IDLE;
        hud.extend(1_000, HUD_HEADLESS_MS);
        let pinned = hud.until;
        hud.extend(1_000, HUD_LINGER_MS);
        assert_eq!(hud.until, pinned, "the 4.5 s linger must not cut the 60 s pin");
        hud.extend(1_000 + HUD_HEADLESS_MS, HUD_LINGER_MS);
        assert!(hud.until > pinned, "…but a later deadline still extends it");
    }

    /// A fresh offer takes the control row FROM REST — and clearing the dismissal is half of "on
    /// screen", which is the half that was missing.
    #[test]
    fn a_fresh_offer_puts_the_transport_up_and_parks_the_ring_on_the_primary() {
        let ps = crate::route::PlaybackSession::IDLE;
        with_state(PlaybackState::Playing, || {
            let now = 10_000u32;
            let mut hud = HudState::IDLE;
            hud.dismissed = true; // UP-hidden mid-episode, exactly as reported
            hud.raise_for_offer(now, crate::ui::up_next::PRIMARY_BTN);
            assert!(hud.visible(&ps, now, false), "an offer clears a hand dismissal");
            assert_eq!(
                hud.nav.focus, 1,
                "on the control row, so the auto-hide re-park leaves it"
            );
            assert!(
                crate::ui::up_next::countdown_may_run(true, hud.nav.focus == 1, hud.nav.btn),
                "…and RESTING on the primary, which is what the next frame's cancel rule asks"
            );
        });
    }

    /// The resting-position clause, the other half of the same call: an offer never takes the ring
    /// off a control the user chose. It still puts the transport on screen — the segment is worth
    /// seeing either way — but for Up Next this is also how being busy elsewhere DECLINES the
    /// countdown, since the cancel rule reads the ring as a steady state rather than as an edge.
    #[test]
    fn an_offer_leaves_a_user_who_walked_off_the_scrubber_where_they_are() {
        let ps = crate::route::PlaybackSession::IDLE;
        with_state(PlaybackState::Playing, || {
            let now = 10_000u32;
            // parked on the Chapters tab, which is only reachable by pressing DOWN twice
            let mut hud = HudState {
                nav: HudNav {
                    focus: 2,
                    btn: 0,
                    tab: 1,
                },
                ..HudState::IDLE
            };
            hud.raise_for_offer(now, crate::ui::up_next::PRIMARY_BTN);
            assert!(hud.visible(&ps, now, false), "the offer is still shown");
            assert_eq!((hud.nav.focus, hud.nav.tab), (2, 1), "their spot is theirs");
            assert!(
                !crate::ui::up_next::countdown_may_run(true, hud.nav.focus == 1, hud.nav.btn),
                "engaging the transport is not consent to be pulled into the next episode"
            );
        });
    }
}
