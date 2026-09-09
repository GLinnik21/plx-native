//! Playback orchestration from the app core's side: the HUD timers and visibility rule, the
//! scrub/seek/position accessors, the held-key and repeat gates, the HUD state, start/exit/finish
//! of a playback and the player-route key handlers. Moved out of `app.rs` verbatim in phase 1a
//! (a pure move; `pub(super)` widening only).

use super::*;

#[inline]
pub(super) fn hud_until() -> u32 {
    crate::player::TX.hud_until.load(Relaxed)
}
#[inline]
pub(super) fn set_hud(x: u32) {
    crate::player::TX.hud_until.store(x, Relaxed)
}
/// Raise the HUD to at least `now + ms`, never PULLING IN a deadline already further out.
///
/// `set_hud` stores an absolute instant, so an unconditional call SHORTENS whatever was there.
/// The headless capture path pins the HUD for `HUD_HEADLESS_MS` (60 s), and the marker prompts
/// below fire mid-playback — calling `set_hud` there cut that pin to the 4.5 s linger and the
/// transport vanished out from under a live Skip button (seen on device, not in review).
/// Comparison is plain `>`, matching `hud_shown`'s own non-wrapping `now < until`.
#[inline]
pub(super) fn extend_hud(now: u32, ms: u32) {
    let want = now.saturating_add(ms).max(1);
    if want > hud_until() {
        set_hud(want);
    }
}
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
pub(super) fn hud_visible(now: u32, until: u32, is_paused: bool, dismissed: bool) -> bool {
    ((now < until || is_paused) && !dismissed) || crate::player::loading()
}

/// The transport's visibility predicate, and the one state transition that has to OUTRANK it —
/// a fresh control-row offer. Almost nothing else in this file is host-testable — it is the SDL
/// event loop — but these two are, and between them they encode the bugs that cost the diagnostics
/// overlay its whole reason for existing and the Up Next tile its auto-advance.
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
        with_state(PlaybackState::Buffering, || {
            // the timer alone would say hidden — 9 s past the linger, nothing paused
            let (now, expired) = (10_000u32, 1_000u32);
            assert!(
                hud_visible(now, expired, false, false),
                "on screen, so keys must reach it"
            );
        });
    }

    /// While playing normally the two agree — an expired linger really does mean hidden, or the HUD
    /// would never auto-hide at all.
    #[test]
    fn a_healthy_playing_pipeline_still_auto_hides() {
        with_state(PlaybackState::Playing, || {
            assert!(!hud_visible(10_000, 1_000, false, false));
            assert!(hud_visible(500, 1_000, false, false), "inside the linger");
            assert!(hud_visible(10_000, 1_000, true, false), "paused pins it up");
        });
    }

    /// **A LEFT/RIGHT press that finds the HUD hidden is spent RAISING it** — the rule
    /// [`key_scrub`] is built around, and the one arm of that ladder no other test can reach (every
    /// other branch of it drives the player's globals from inside the SDL loop).
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
        // …but the two indexed rows are navigable whether or not the item has a duration
        assert_eq!(scrub_press(true, 1, false), ScrubPress::Row);
        assert_eq!(scrub_press(true, 2, false), ScrubPress::Tabs);
    }

    /// **A transport hidden BY HAND is hidden**, and the press that follows must raise it like any
    /// other — the hole [`HudState::note_fresh_press`] exists to close.
    ///
    /// UP-from-the-control-row hides the HUD without extending the linger, so for up to
    /// `HUD_LINGER_MS` afterwards the TIMER still says "on screen" while nothing is drawn. The
    /// dismissal is what tells them apart, and it is cleared at the top of every fresh press — so
    /// an arm that re-derived visibility for itself got `true` and drove geometry the user could
    /// not see. Three points of one loop iteration, in their real order.
    #[test]
    fn a_hand_hidden_hud_still_takes_the_press_that_wakes_it() {
        with_state(PlaybackState::Playing, || {
            let now = 10_000u32; // a literal tick, like every test here: the host links no SDL
            let saved = hud_until(); // `extend_hud` never pulls a deadline IN — reset on the way out
            let mut hud = HudState::IDLE;
            extend_hud(now, HUD_LINGER_MS); // …the HUD is up, its linger running
            assert!(
                now < hud_until(),
                "the linger IS still running — the case this is about"
            );

            hud.dismissed = true; // …UP from the control row: hidden, and the timer left alone
            hud.nav.focus = 0;

            hud.note_fresh_press(now); // …and a LEFT arrives
            assert!(
                !hud.visible_at_press,
                "it was NOT on screen, whatever the timer says"
            );
            assert!(
                !hud.dismissed,
                "…and the press has un-dismissed it, as every key does"
            );
            assert_eq!(
                scrub_press(hud.visible_at_press, hud.nav.focus, true),
                ScrubPress::Reveal,
                "so the press raises the transport instead of seeking behind it"
            );

            // …and the NEXT press, with the HUD genuinely up, is the one that hops
            hud.note_fresh_press(now);
            assert!(hud.visible_at_press);
            assert_eq!(
                scrub_press(hud.visible_at_press, hud.nav.focus, true),
                ScrubPress::Jump
            );
            set_hud(saved);
        });
    }

    /// `disengage` is what ends a gesture, and it must end EVERY part of one. `reveal` outliving a
    /// disengage would make the next tap release throw away a preview the user had really built:
    /// the release arm reads `hold` then `reveal`, so a stale `reveal` silently outranks a real
    /// tap commit. `commit_at` is the deliberate exception and stays untouched (see its doc).
    #[test]
    fn disengaging_ends_every_part_of_the_gesture() {
        let mut s = Scrub {
            dir: -1,
            hold: true,
            reveal: true,
            commit_at: 4_242,
            ..Scrub::IDLE
        };
        s.disengage();
        assert_eq!((s.dir, s.hold, s.reveal), (0, false, false));
        assert_eq!(
            s.commit_at, 4_242,
            "a pending tap commit is NOT this function's to cancel"
        );
    }

    /// An explicit dismiss (UP from the top row) still hides it while healthy — but must NOT be able
    /// to hide it while the pipeline is stalled, because that is the state the user needs to report
    /// and the read-out is pinned on screen there regardless.
    #[test]
    fn dismiss_wins_while_healthy_and_loses_while_stalled() {
        with_state(PlaybackState::Playing, || {
            assert!(
                !hud_visible(500, 1_000, false, true),
                "dismissed during playback"
            );
        });
        with_state(PlaybackState::Buffering, || {
            assert!(
                hud_visible(500, 1_000, false, true),
                "a stall outranks the dismiss"
            );
        });
    }

    /// THE Up Next regression: the credits offer has to reach the panel even when the user hid the
    /// transport BY HAND earlier in the episode.
    ///
    /// Three points of one loop iteration are compressed here, in their real order, because the
    /// failure lived in their COMPOSITION and not in any one of them: the offer edge raises the
    /// HUD, the auto-hide re-park a few lines below reads `hud_visible`, and the NEXT frame's
    /// steady-state cancel rule reads the ring. Raising the TIMER alone satisfied the first and
    /// lost the other two — `dismissed` outranks the timer, so `draw_hud` was never called, the
    /// invisible HUD's ring was reset the same frame, and `up_next::countdown_may_run` then read
    /// that as the user walking away and latched the countdown off for the whole segment. The tile
    /// appeared only if the HUD was raised by hand, with its auto-advance already dead.
    ///
    /// The on-device case (`marker_credits_up_next`) cannot see this: it never hides the HUD.
    #[test]
    fn a_fresh_offer_reaches_the_panel_through_an_earlier_up_hide() {
        with_state(PlaybackState::Playing, || {
            let saved = hud_until();
            let now = 10_000u32;
            set_hud(0); // the linger long expired…
                        // …and UP-from-the-control-row on top of it: dismissed, ring back on the scrubber
            let mut hud = HudState {
                dismissed: true,
                ..HudState::IDLE
            };
            assert!(
                !hud_visible(now, hud_until(), false, hud.dismissed),
                "the state the credits marker arrives into"
            );

            hud.raise_for_offer(now, crate::ui::up_next::PRIMARY_BTN);

            assert!(
                hud_visible(now, hud_until(), false, hud.dismissed),
                "a countdown behind a HUD nobody drew is a cut to the next episode out of nowhere"
            );
            assert_eq!(
                hud.nav.focus, 1,
                "on the control row, so the auto-hide re-park leaves it"
            );
            assert!(
                crate::ui::up_next::countdown_may_run(true, hud.nav.focus == 1, hud.nav.btn),
                "…and RESTING on the primary, which is what the next frame's cancel rule asks"
            );
            set_hud(saved);
        });
    }

    /// The resting-position clause, the other half of the same call: an offer never takes the ring
    /// off a control the user chose. It still puts the transport on screen — the segment is worth
    /// seeing either way — but for Up Next this is also how being busy elsewhere DECLINES the
    /// countdown, since the cancel rule reads the ring as a steady state rather than as an edge.
    #[test]
    fn an_offer_leaves_a_user_who_walked_off_the_scrubber_where_they_are() {
        with_state(PlaybackState::Playing, || {
            let saved = hud_until();
            let now = 10_000u32;
            set_hud(0);
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
            assert!(
                hud_visible(now, hud_until(), false, hud.dismissed),
                "the offer is still shown"
            );
            assert_eq!((hud.nav.focus, hud.nav.tab), (2, 1), "their spot is theirs");
            assert!(
                !crate::ui::up_next::countdown_may_run(true, hud.nav.focus == 1, hud.nav.btn),
                "engaging the transport is not consent to be pulled into the next episode"
            );
            set_hud(saved);
        });
    }
}
#[inline]
pub(super) fn scrub() -> i64 {
    crate::player::TX.scrub_ns.load(Relaxed)
}
#[inline]
pub(super) fn set_scrub(x: i64) {
    crate::player::TX.scrub_ns.store(x, Relaxed)
}
#[inline]
pub(super) fn resume_pend() -> bool {
    crate::player::TX.resume_pend.load(Relaxed)
}
#[inline]
pub(super) fn set_resume_pend(v: bool) {
    crate::player::TX.resume_pend.store(v, Relaxed)
}
#[inline]
pub(super) fn dur() -> i64 {
    crate::player::duration_ns()
}
#[inline]
pub(super) fn playpos() -> i64 {
    crate::player::playpos_ns()
}
/// The playhead the user INTENDED, which is not always the one being published. While a seek is
/// still resolving (request → reopen → prime → Play) `playpos()` keeps reporting the PRE-seek spot,
/// so anything that snapshots "where are we?" inside that window snapshots the position the user
/// just left. The rule — an in-flight seek target wins, else the published position — was open-coded
/// at each reader that remembered it (the scrub seed below; the HUD's frozen playhead in
/// `ui/player_hud.rs`) and simply MISSING at the one that did not: the OS-background save took a bare
/// `playpos()`, so backgrounding right after a seek stored the pre-seek spot and the foreground
/// restore replayed from there — and teardown clears the pending target, so nothing self-corrected.
/// Use this at every reader that means "where the user is"; keep the raw `playpos()` only where the
/// PUBLISHED position is the point (the re-pause gate, which is already behind `seek_pending() < 0`,
/// and the heartbeat's `pos=`, which the harness grades real playback progress from).
#[inline]
pub(super) fn intended_pos() -> i64 {
    crate::player::intended_pos_ns()
}
#[inline]
pub(super) fn frames() -> i32 {
    crate::player::frames()
}
#[inline]
pub(super) fn seek_pending() -> i64 {
    crate::player::seek_pending()
}
#[inline]
pub(super) fn request_seek(x: i64) {
    crate::player::request_seek(x)
}
/// Commit a scrub to `target` and clear the preview. If we were PAUSED, STAY logically paused: a
/// dedicated seek-preroll feed override lets the synchronized native clock decode one landed frame
/// without publishing a false viewer Resume. `resume_pend` asks the per-frame loop to close that
/// bounded override. `repause_at` is the landed-frame wait target.
pub(super) fn commit_seek(target: i64, repause_at: &mut i64) {
    crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
        feature: crate::diag::schema::Feature::Seek,
    });
    request_seek(target);
    set_scrub(-1);
    if paused() {
        *repause_at = target;
        set_resume_pend(true);
        crate::player::TX.begin_paused_seek();
    }
}
#[inline]
pub(super) fn is_started() -> bool {
    crate::player::is_started()
}

// ---- the route vocabulary, and the pure questions asked ABOUT a route -------------------------
//
// These are pure functions of a `Route` that read and write no app state, which is what lets
// `route_tests` at the bottom of this file grade them — and grading them is the point, because they
// decide things that have shipped wrong (the teardown rule below, twice), and a `Route` that only
// exists inside the run loop's body is a decision no host test can reach. The loop still owns every
// VALUE — `route` is a local, the trail is a local.

/// WHICH key the remote is holding down, as one value: the sym the client-side repeat timer
/// is driving, the two instants that timer reads, the hardware heartbeat that catches a
/// dropped key-up, and the sym we watched go physically down.
///
/// They are bundled because the two per-frame rules at the bottom of the loop each read
/// three of the five together — the lost-keyup net tests `sym`, `since` and `alive`, and the
/// repeat itself tests `sym`, `since` and `last_rep` — while every arm that arms a hold
/// writes the same three fields in the same order.
pub(super) struct HeldKey {
    pub(super) sym: u32,      // the key the client-side repeat is driving; 0 = nothing held
    pub(super) since: u32,    // when it was armed — the repeat's initial delay is measured from here
    pub(super) last_rep: u32, // when that repeat last fired
    pub(super) alive: u32,    // last hardware 0x101 for the held key — a lost-keyup liveness net
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
    pub(super) down_sym: u32,
}
impl HeldKey {
    /// Nothing held, no hold-repeat pending — where the loop starts.
    pub(super) const IDLE: HeldKey = HeldKey {
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
    pub(super) fn arm(&mut self, sym: u32, now: u32) {
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
pub(super) struct RepeatGate {
    /// The tick of the last step this gate admitted; `None` before the first one.
    pub(super) last: Option<u32>,
}
impl RepeatGate {
    /// Minimum time between two repeat-driven steps this gate allows. Slower than the discrete
    /// focus-list repeat (110ms, `HeldKey`'s own client-side timer) on purpose — a home-grid card
    /// is a glance, a settings row or a line of reading text is not.
    pub(super) const STEP_MS: u32 = 160;
    pub(super) const IDLE: RepeatGate = RepeatGate { last: None };
    /// True at most once per [`Self::STEP_MS`]; always true the first call, or after a gap at
    /// least that long (which is also what makes a long-idle gate behave like a fresh one).
    pub(super) fn ready(&mut self, now: u32) -> bool {
        let due = match self.last {
            None => true,
            Some(last) => now.wrapping_sub(last) >= Self::STEP_MS,
        };
        if due {
            self.last = Some(now);
        }
        due
    }
}

#[cfg(test)]
mod repeat_gate_tests {
    use super::RepeatGate;

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
}

/// Scrub-seek gesture state. This Magic Remote emits a HELD key as auto-repeat keydowns
/// (state 0x101, ~50ms apart) followed by ONE keyup on release; a TAP is a lone
/// keydown(0x001)+keyup(0x000). So: a fresh press does the fixed jump; the 0x101 repeats
/// engage the continuous scrub; the keyup is a reliable release. Taps commit on a short
/// debounce so quick taps accumulate.
///
/// The preview POSITION is not here — it lives in `player::TX` behind `scrub()`/`set_scrub`,
/// because the draw path reads it too.
pub(super) struct Scrub {
    pub(super) t: u32,          // last continuous-advance tick
    pub(super) dir: i32,        // -1 back / +1 forward / 0 = no scrub in progress
    pub(super) hold: bool,      // a 0x101 repeat arrived → continuous accelerating scrub engaged
    pub(super) hold_since: u32, // when that hold engaged — the acceleration ramp is measured from here
    pub(super) alive: u32,      // last held (0x101) event — for the lost-keyup safety commit
    pub(super) commit_at: u32,  // tap released → commit at this tick (0 = none; a new press cancels)
    /// This gesture began on a HIDDEN HUD, so its press was spent raising the transport
    /// ([`ScrubPress::Reveal`]) rather than hopping. It is still a fully armed scrub — a user who
    /// keeps holding gets the ordinary continuous rewind, and `hold` engaging clears this — but if
    /// it turns out to have been a TAP, the release must throw the preview away instead of
    /// committing it: the preview sits on the seed, i.e. exactly where playback already is, and
    /// committing that is a full reopen+prime to no effect.
    pub(super) reveal: bool,
}
impl Scrub {
    /// No scrub in progress and no tap commit pending — where the loop starts.
    pub(super) const IDLE: Scrub = Scrub {
        t: 0,
        dir: 0,
        hold: false,
        hold_since: 0,
        alive: 0,
        commit_at: 0,
        reveal: false,
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
    pub(super) fn begin(&mut self, now: u32, fwd: bool) {
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
    pub(super) fn disengage(&mut self) {
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
pub(super) struct HudNav {
    pub(super) focus: i32, // 0 = scrubber, 1 = right buttons (Subtitles/Audio/More), 2 = bottom tabs
    pub(super) btn: i32,   // 0 = Subtitles, 1 = Audio, 2 = More (within the buttons row)
    pub(super) tab: i32,   // 0 = Info, 1 = Chapters (within the tabs row)
}
impl HudNav {
    /// Focus parked on the scrubber, both indexed rows on their first item — where a fresh
    /// session starts and where an auto-hidden HUD is re-parked.
    pub(super) const HOME: HudNav = HudNav {
        focus: 0,
        btn: 0,
        tab: 0,
    };
}
/// Everything the loop remembers ABOUT the transport HUD between frames: where its focus
/// cursor is parked, whether the user dismissed it, and the two control-row edges the
/// per-frame block near the bottom of the loop compares against this frame's slot.
///
/// The cursor keeps its own type ([`HudNav`]) rather than dissolving into fields here: the
/// helpers below take it as `&mut HudNav` — `grep 'hud_nav: &mut HudNav'` for the list, which
/// this doc used to carry as a count and which grew the moment the key ladder's arms became
/// functions — and one of them (`start_playback`) is where the per-session reset happens.
///
/// Named `HudState` and not `Hud` so it does not read as `focusprobe::Hud`, which is that
/// module's own snapshot of the cursor plus a computed `visible`, built beside this one at
/// the tail of the loop.
pub(super) struct HudState {
    /// the focus cursor, reset per session by `start_playback`
    pub(super) nav: HudNav,
    /// UP-from-the-top explicitly dismisses the HUD even while paused; any other player
    /// input clears it. Without this, paused() would force the HUD permanently visible.
    pub(super) dismissed: bool,
    /// Was the transport ON SCREEN when the key being handled arrived? Sampled by
    /// [`begin_fresh_press`] at the top of every fresh press, and the ONLY honest answer to that
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
    pub(super) visible_at_press: bool,
    /// The last SEGMENT the control row offered. Sticky: it is never cleared back to None,
    /// so each segment raises the HUD exactly once per playback however often the row
    /// flickers.
    pub(super) last_offer: Option<(crate::metadata::MarkerKind, i64)>,
    /// Did a stand-in own the control row last frame? The reset below is the EDGE of a
    /// stand-in vanishing under the focus ring — see `player_hud::standin_left_the_ring`,
    /// which is where that rule is written down and tested.
    pub(super) was_standin: bool,
}
impl HudState {
    /// Focus at rest, nothing dismissed, no segment seen yet, discs in the control row.
    pub(super) const IDLE: HudState = HudState {
        nav: HudNav::HOME,
        dismissed: false,
        visible_at_press: false,
        last_offer: None,
        was_standin: false,
    };

    /// A FRESH segment offer takes the control row: put the HUD ON SCREEN and, from rest, park the
    /// ring on the row's primary so a bare OK acts on the offer in one press instead of
    /// raise-HUD → navigate → OK.
    ///
    /// **Clearing the dismissal is half of "on screen", and leaving it out was the bug.**
    /// [`extend_hud`] moves only the TIMER, and `dismissed` outranks the timer outright inside
    /// [`hud_visible`] — so a user who UP-hid the transport mid-episode and then touched nothing
    /// carried that dismissal into the credits, and the Up Next tile was offered to a HUD that
    /// `draw_hud` was never called for. Being invisible it then lost its ring to the auto-hide
    /// re-park at the bottom of the same block, which the NEXT frame's steady-state cancel rule
    /// ([`crate::ui::up_next::countdown_may_run`]) read as the user walking away — latching the
    /// countdown off for the whole segment. The tile appeared only if the HUD was raised by hand,
    /// with its auto-advance already dead: exactly as reported. A dismissal is a "not now" that any
    /// key the app BINDS clears (an unsupported one clears nothing — `note_global_press`); a segment
    /// beginning is that same kind of event, arriving from the player instead of the remote, and it
    /// must clear it too — which is also what makes an offer behave the same
    /// whether the HUD auto-hid or was hidden on purpose.
    ///
    /// Parking is only ever from REST: a user who walked to the Subtitles disc or an Info tab keeps
    /// their spot (and for Up Next thereby declines the countdown — the same one rule, read as a
    /// steady state one block below). `primary` is the occupant's own
    /// ([`crate::ui::player_hud::ControlSlot::primary_btn`]) — item 0 for a Skip pill, the
    /// RIGHT-hand one for Up Next, where parking on item 0 would disarm the timer on the frame
    /// after it armed.
    /// A fresh key the app BINDS has arrived: record what the transport LOOKED like to the user,
    /// then clear the dismissal it may have been carrying.
    ///
    /// The two are one operation and the ORDER is the whole point — `dismissed` outranks the timer
    /// inside [`hud_visible`], so sampling after the clear reports a hand-hidden transport as being
    /// on screen (see [`visible_at_press`](Self::visible_at_press)). Written down as one function
    /// rather than two lines in its caller so that the order is a thing a test can hold still,
    /// instead of a convention a later edit can quietly transpose.
    ///
    /// That caller is [`note_global_press`], NOT [`begin_fresh_press`] as it once was, and the
    /// difference is the point of the split: an unsupported key never gets here at all.
    pub(super) fn note_fresh_press(&mut self, now: u32) {
        self.visible_at_press = hud_visible(now, hud_until(), paused(), self.dismissed);
        // Any BOUND fresh key un-dismisses the HUD (UP-hide re-sets it). "Bound" and not "any" is
        // the whole of `note_global_press`, the ONLY caller: an unsupported press never reaches
        // here, so a colour button over a film no longer raises the transport.
        self.dismissed = false;
    }

    pub(super) fn raise_for_offer(&mut self, now: u32, primary: c_int) {
        extend_hud(now, HUD_LINGER_MS);
        self.dismissed = false;
        if self.nav.focus == 0 {
            self.nav.focus = 1;
            self.nav.btn = primary;
        }
    }
}
// scrub tuning: a press jumps SCRUB_STEP_NS; holding engages a continuous scrub ramping
// SCRUB_BASE→SCRUB_MAX (playback-seconds per real-second).
pub(super) const SCRUB_STEP_NS: i64 = 10_000_000_000; // 10s per press
pub(super) const SCRUB_BASE: f32 = 10.0;
pub(super) const SCRUB_ACCEL: f32 = 45.0; // added per second of hold
pub(super) const SCRUB_MAX: f32 = 140.0;
// tap released → commit after this (further taps accumulate). Long enough that a rapid
// ±10s tap burst coalesces into ONE seek — each separate commit is a full reopen+prime on
// the engine, and back-to-back in-flight seeks are what race the demux (the stale-audio
// silence incident); short enough that a single tap still feels immediate.
pub(super) const TAP_COMMIT_MS: u32 = 450;
pub(super) const SCRUB_LOST_MS: u32 = 400; // holding but no repeat this long → lost keyup → commit
                                // HUD auto-hide: how long the HUD lingers after the input that raised it.
pub(super) const HUD_LINGER_MS: u32 = 4500; // plain transport/nav input
pub(super) const HUD_MENU_MS: u32 = 8000; // a modal menu is up (track/chapter nav) — longer read time
pub(super) const HUD_HEADLESS_MS: u32 = 60_000; // autoplay/headless runs pin the HUD up for capture
/// Perform what the `…` popover reported. Shared by the OK key and the pointer click, so
/// the two paths can never come to disagree about what a row does.
pub(super) fn apply_more_action(mt: &crate::task::MainThread, a: crate::ui::more_menu::Action) {
    match a {
        crate::ui::more_menu::Action::ToggleStats => crate::ui::stats::toggle(),
        // A rung of the playback-quality ladder — a routing POLICY, not a number handed to a
        // running stream. Not deferred either: `route::set_quality` re-asks the routing question
        // for the playback on screen and reloads only when the answer changed.
        crate::ui::more_menu::Action::SetQuality(q) => {
            // A terminal Engine never reaches pump's pending-retranscode arm, and a `/decision`
            // refusal has no Engine at all.  Persist the pick first, then make a fresh playback
            // request at the same user-visible position.  Selecting the already-active rung is
            // therefore the promised plain Retry.
            let failed = matches!(crate::player::state(), crate::player::PlaybackState::Error);
            if failed {
                crate::route::set_quality_for_retry(q);
                retry_failed_playback(mt);
            } else {
                crate::route::set_quality(q);
            }
        }
        // Lab builds only. Nothing about playback changes: the snapshot is taken and the toast
        // reports, over whatever the player is doing.
        crate::ui::more_menu::Action::SendDiagnostics => crate::lab::request_upload("menu"),
        crate::ui::more_menu::Action::None => {}
    }
}

/// Replace a terminal attempt with a new resolve of the same Plex item.
///
/// This is a REAL stop followed by a new request, not an Engine reload: it covers the pre-flight
/// refusal which never created an Engine, retires a failed server transcode when there was one,
/// and gives telemetry two honest attempts.  The descriptor lives in `route`; the app owns only
/// the current playhead and the Engine lifecycle.
pub(super) fn retry_failed_playback(mt: &crate::task::MainThread) -> bool {
    // URL/dev-trigger playback has no Plex descriptor.  Check BEFORE teardown: extinguishing its
    // Error Engine and only then discovering it cannot be rebuilt would replace an actionable
    // read-out with an idle black frame.
    if !crate::route::can_retry_current_play() {
        log("playback retry: current source has no reusable Plex request");
        return false;
    }
    // A terminal error can race a seek whose requested target has not landed.  Resume what the
    // viewer asked for, not the last frame the dying Engine happened to publish.  If an earlier
    // retry was refused before presenting anything, retain its target too: the stopped Engine now
    // reports zero and must not send a second quality attempt back to the beginning.
    let resume_ns = intended_pos()
        .max(crate::route::unpresented_resume_ns())
        .max(0);
    crate::player::stop_bufferfeed(mt);
    if crate::route::retry_current_play(resume_ns) {
        crate::ui::idle::invalidate();
        true
    } else {
        log("playback retry: current source cannot be resolved again");
        false
    }
}

/// The ONE start-playback ritual (detail OK, home episode OK, and the plxnative-autoplay/
/// -detailplay/-play dev triggers all share it): arm the resume point BEFORE the first
/// Load (direct-play av_seek / transcode &offset restart), start the engine, record the
/// Stop/BACK/EOS return target, reset the HUD focus cursor, and show the HUD. A missed step
/// here used to silently fork behavior between the interactive and headless paths.
pub(super) fn start_playback(
    mt: &crate::task::MainThread,
    resume_ns: i64,
    from: Origin,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    hud_nav: &mut HudNav,
) {
    // A resolve in flight means the route statics are NOT installed yet. Applying the
    // resume now would read a stale/empty TSESSION, so `resume_at` would take its
    // DIRECT-PLAY branch and arm_seek() a transcode — and pump.rs's feed gate requires
    // `seek_to_ns < 0`, so that stray armed seek blocks feeding forever: no frames, no
    // ACB bind, timeline frozen at the resume point. (Exactly what broke
    // transcode_av1_no_dp_audio. Direct-play never noticed because arm_seek is what the
    // correct branch does anyway.) Defer it to `pump_play`, after apply_plan.
    let pending = crate::route::play_pending();
    let resume_prepared = pending
        || resume_ns <= 0
        || matches!(
            crate::player::resume_at(resume_ns),
            crate::player::ResumeOutcome::Prepared
        );
    if !resume_prepared {
        if let Some(transaction) = crate::route::pending_route_start() {
            let _ = crate::route::reject_route_start_preparation(transaction);
        }
    }
    // Flip to the player NOW so the HUD draws its Resolving state this frame; `pump_play`
    // below starts the engine when the plan lands. With nothing pending this is the old
    // synchronous behaviour, byte for byte.
    let entering = if pending {
        crate::route::arm_play_resume(resume_ns);
        true
    } else if resume_prepared {
        crate::player::start_bufferfeed(mt)
    } else {
        false
    };
    if entering {
        set_origin(play_from, from);
        *route = Route::Player {
            overlay: Overlay::None,
        };
    }
    // A NEW session starts on the scrubber. The cursor is per-session state that nothing
    // else clears: the auto-hide re-park later in the loop only runs while the route is
    // already Player, and the exit paths leave the player entirely — so leaving a movie
    // with the Subtitles button focused used to carry `focus == 1` into the next one,
    // where the first OK opened the track menu instead of pausing. Unconditional, like the
    // `set_paused`/`set_hud` below it: the HUD that is about to be drawn belongs to THIS
    // attempt either way.
    *hud_nav = HudNav::HOME;
    // Per-session: an auto-advance chain (episode → episode → …) re-enters here without
    // ever passing through `exit_player`, so the finished episode's countdown state must
    // not carry into the next one.
    crate::ui::up_next::reset();
    set_paused(false);
    // Stamp the HUD deadline HERE, from NOW — not from the keypress. Callers used to pass
    // `last_input + HUD_LINGER_MS`, a timestamp taken BEFORE the blocking resolve above, so
    // a load longer than the 4.5 s linger expired the HUD before it was ever drawn and the
    // user got a blank screen instead of a transport. Taking a duration makes that
    // unrepresentable, and keeps the headless 60 s case working.
    set_hud(clock::now().wrapping_add(hud_ms).max(1));
}

/// Resume if a seek landed while paused — the twin of `commit_seek`, which is the
/// stay-paused variant. Written out four separate times in this file before it had a name.
pub(super) fn resume_if_paused(mt: &crate::task::MainThread) {
    if paused() {
        set_transport_paused(mt, false);
    }
}

/// Legacy card launches resolve media data without creating an invisible Detail screen.
pub(super) fn request_loaded_hero() -> Option<i64> {
    let d = crate::metadata::current()?;
    if d.kind == "show" || !d.seasons.is_empty() {
        let started = d.on_deck.as_ref().is_some_and(|e| e.resume_ms > 0)
            || d.seasons.iter().any(|s| s.viewed_leaf_count > 0);
        let ep = (if started { d.on_deck.as_ref() } else { None })
            .or_else(|| d.episodes.first())?;
        request_episode(d, ep).then(|| crate::metadata::resume_ns(ep.resume_ms, ep.dur_ms))
    } else {
        crate::route::request_play(crate::route::item_sid(d.sid), &d.rk, &d.part,
            &d.vcodec, &d.acodec, &d.title, "")
            .then(|| crate::metadata::resume_ns(d.resume_ms, d.dur_ms))
    }
}

pub(super) fn request_loaded_episode(rk: &str) -> bool {
    let Some(d) = crate::metadata::current() else { return false };
    d.episodes.iter().find(|e| e.rk == rk).is_some_and(|ep| request_episode(d, ep))
}

fn request_episode(d: &crate::metadata::Detail, ep: &crate::metadata::Episode) -> bool {
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(Some(crate::metadata::NowPlaying {
        is_episode: true, title: d.title.clone(), ep_title: ep.title.clone(),
        season: ep.season, index: ep.index, summary: ep.summary.clone(),
        year: ep.aired.get(..4).and_then(|s| s.parse().ok()).unwrap_or(0),
        dur_ms: ep.dur_ms, rating: ep.rating.clone(), thumb: ep.thumb.clone(), detail_rk: d.rk.clone(),
    })));
    let title = if ep.title.is_empty() { &d.title } else { &ep.title };
    let context = format!("{}  ·  S{} E{}", d.title, ep.season, ep.index);
    crate::route::request_play(crate::route::item_sid(d.sid), &ep.rk, &ep.part,
        &ep.vcodec, &ep.acodec, title, &context)
}

/// Leaving playback (Stop / BACK / EOS / Info's jump-to-detail): close every in-player
/// overlay so no stale popover OPEN flag survives into the next session — the route flip
/// alone hides them but leaves the module state set (the EOS path once forgot the menu).
pub(super) fn close_player_overlays() {
    crate::ui::track_menu::close();
    crate::ui::info_panel::close();
    crate::ui::chapters_panel::close();
    crate::ui::more_menu::close();
    crate::ui::stats::close(); // a diagnostics panel must not survive into the next session
    crate::ui::up_next::cancel(); // disarm the auto-advance countdown
}

/// Returning to a detail page after an EPISODE lands on its SHOW at the episode that played, not
/// on the episode's own page. Reports whether it mounted anything.
///
/// The play paths load the played LEAF's detail (that is where the HUD caption and Info card come
/// from), so an exit that only flipped the route stranded the user on an episode hero page — even
/// when they had started from the show page, and even after an auto-advance chain had moved them
/// several episodes along. `detail_rk` is already the "Go to Show" target.
///
/// **Gated on the page actually BEING that show**, which the `played_from_detail` bool could not
/// express: a *Play from Start* on a Related tile is an item the page says nothing about, and if
/// it happens to be an episode then revealing its show here would navigate the user to a page they
/// never asked for instead of back to the one they were standing on.
pub(super) fn reveal_played_episode(from: &Node) -> bool {
    let Node::Detail { sid, rk, .. } = from else {
        return false;
    };
    // The played leaf's own server — `metadata::playing()` is the store the playback was resolved
    // from, so it names the machine the show is on. `plex::current_server()` would be the wrong
    // answer for anything played off a share.
    let psid = crate::metadata::playing()
        .map(|p| p.sid)
        .unwrap_or_else(crate::plex::current_server);
    let Some((show_rk, season)) = crate::metadata::now_playing()
        .filter(|n| n.is_episode && !n.detail_rk.is_empty())
        .map(|n| (n.detail_rk.clone(), n.season))
    else {
        return false;
    };
    if !crate::plex::same_item((psid, &show_rk), (*sid, rk)) {
        return false;
    }
    let _ = season; // The addressed reveal is applied after the origin entry is uncovered.
    true
}

/// The ONE leave-playback ritual (Stop key, BACK, EOS): close the overlays, stop the
/// engine, put the page the session was LAUNCHED FROM back on screen, and arm the deferred
/// hub refresh so Continue Watching reflects the session that just ended. A new exit path
/// that skips this quietly re-introduces the stale-CW bug.
///
/// `from` is [`Origin`]'s payload — the page that was mounted when playback started. Re-entry is
/// [`enter_node`], the same ritual every BACK pop and every forward `Nav::Open` uses, so a player
/// exit cannot mount a page in a way nothing else does.
pub(super) fn exit_player(
    mt: &crate::task::MainThread,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
) {
    crate::route::cancel_play(); // BACK during a load: supersede, drop the landing
    close_player_overlays();
    crate::player::stop_bufferfeed(mt);
    // `stop_bufferfeed` reports/clears a real engine through `report::ended`, but a refusal or a
    // BACK during resolve has no engine for teardown to take. The exit ritual still ends that
    // attempt, so retire its in-memory trace here as the common backstop.
    crate::player::report::clear_error_trace();
    if reveal_played_episode(play_from) {
        // …the reveal IS the mount, so `enter_node`'s would be a second, competing one.
        *route = Route::Detail;
    } else {
        enter_node(play_from, route);
    }
    // The player is NOT a trail node — it returns to the page it was started from, and that page
    // may be one nothing ever pushed (a dev trigger, or `home_activate` opening a detail page
    // under the hood purely to fire its Play). `ensure` makes the trail agree with where we
    // landed: a no-op in the ordinary case (the page IS still the top, because playing never
    // moved the trail), the root for a return to Home, a push otherwise.
    trail.ensure(play_from);
    *refresh_hubs_at = clock::now().wrapping_add(800).max(1);
}

/// The episode is OVER — drained to EOS, or the user skipped a `final` credits marker.
/// Starts the queued episode when the show has one, else leaves the player exactly as
/// `exit_player` would. There is no interstitial: "always the next episode".
pub(super) fn finish_playback(
    mt: &crate::task::MainThread,
    route: &mut Route,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
    hud_nav: &mut HudNav,
    trail: &mut Trail,
) {
    if play_up_next(mt, HUD_LINGER_MS, route, play_from, hud_nav) {
        return;
    }
    exit_player(mt, route, play_from, refresh_hubs_at, trail);
    hud_nav.focus = 0;
}

/// Activate whatever occupies the control row. ONE dispatch for both the OK key and the
/// pointer — they used to hold byte-identical copies of this `match`, and had already
/// drifted (the key path cleared the held key, the pointer path did not). Returns true when
/// the route flipped, which is the only thing the two callers still handle differently.
pub(super) fn activate_ctrl_row(
    mt: &crate::task::MainThread,
    slot: crate::ui::player_hud::ControlSlot,
    route: &mut Route,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
    hud_nav: &mut HudNav,
    trail: &mut Trail,
) -> bool {
    use crate::ui::player_hud::ControlSlot;
    use crate::ui::skip_pill::SkipAction;
    match slot {
        // The row's two items, off the cursor the caller already parked (a click sets it
        // from the hit-test, a key press moved it). *Next Episode* starts the successor;
        // *Watch Credits* does nothing beyond the cancel the frame block below performs
        // for it — the button exists so that "let it run" is a THING YOU CAN PRESS rather
        // than an absence, which on a countdown is the difference between choosing and
        // being caught out.
        ControlSlot::UpNext(_) => {
            if hud_nav.btn == crate::ui::up_next::BTN_NEXT {
                play_up_next(mt, HUD_LINGER_MS, route, play_from, hud_nav)
            } else {
                crate::ui::up_next::cancel();
                false
            }
        }
        ControlSlot::Skip(pr) => {
            crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                feature: match pr.kind {
                    crate::metadata::MarkerKind::Intro => crate::diag::schema::Feature::SkipIntro,
                    crate::metadata::MarkerKind::Credits => {
                        crate::diag::schema::Feature::SkipCredits
                    }
                },
            });
            match pr.action {
                SkipAction::Seek(ns) => {
                    // Retire the segment FIRST: the seek lands on the preceding keyframe, which
                    // is usually still inside it, so without this the button comes straight back
                    // (see `metadata::mark_skipped`).
                    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::MarkSkipped(pr.marker));
                    request_seek(ns);
                    resume_if_paused(mt);
                    false
                }
                // a `final` credits segment: skipping it IS finishing the item
                SkipAction::Finish => {
                    finish_playback(mt, route, play_from, refresh_hubs_at, hud_nav, trail);
                    true
                }
            }
        }
        ControlSlot::Discs => false,
    }
}

/// Start the queued episode. Returns false when there is nothing queued (a movie, or the
/// last episode), which is the caller's cue to leave the player.
///
/// It stops the outgoing session ITSELF rather than trusting each call site to: three
/// paths reach here (EOS, Skip Credits on a `final` marker, and OK on the HUD tile while
/// the credits are still rolling) and in all three an Engine is live — `start_bufferfeed`
/// no-ops while one is, so skipping the stop would silently fail to advance. The stop is
/// also what posts the `state=stopped` timeline that commits the watched state, and it
/// must happen BEFORE `request_play_up_next`: teardown reads the outgoing item's session
/// ids and clears the URL, both of which the new plan is about to overwrite.
pub(super) fn play_up_next(
    mt: &crate::task::MainThread,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    hud_nav: &mut HudNav,
) -> bool {
    // clone off the `&'static` store BEFORE anything can replace it (see up_next::take)
    let Some(u) = crate::ui::up_next::take() else {
        return false;
    };
    // The ratingKey, not the episode title: `rk` is the handle every other line and every harness
    // assertion already uses, and the title is LG's "Content Viewing Information" — the one
    // category this app's Data Safety declaration answers "Not collected" to. `diag::scrub` is a
    // backstop for shapes like this; not writing it is the mechanism.
    log(&format!("up next: S{}E{} rk={}", u.season, u.index, u.rk));
    let (rk, resume) = (
        u.rk.clone(),
        crate::metadata::resume_ns(u.resume_ms, u.dur_ms),
    );
    close_player_overlays();
    crate::player::stop_bufferfeed(mt);
    if !crate::route::request_play_up_next(u) {
        return false;
    }
    // Same ritual as `play_item_now`: retire the finished episode's descriptor so the HUD
    // caption and Info card don't label the new playback with the old one's title for the
    // whole pre-roll, and fetch the new leaf off the loop.
    // Read BEFORE `retire_playing` drops the store: the successor is a row of the queue
    // the finished episode created, so it lives on that episode's server.
    let sid = crate::metadata::playing()
        .map(|p| p.sid)
        .unwrap_or_else(crate::plex::current_server);
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RetirePlaying);
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid: sid, rk: rk.to_string() });
    start_playback(
        mt,
        resume,
        Origin::Unchanged,
        hud_ms,
        route,
        play_from,
        hud_nav,
    );
    true
}

/// Direct-play a LEAF catalog item (movie or episode) — the hero-pill / Continue-Watching
/// "play now" ritual: route cfg + streams metadata + the shared start ritual.
/// `from_start` ignores the item's resume point — the item menu's "Play from Start", which is
/// the ONLY difference between restarting a Continue Watching tile and resuming it. Taking it
/// as a flag (rather than a resume_ns the caller computes) keeps Plex's resume rule
/// (`metadata::resume_ns`, which also refuses to resume the last few percent) in one place.
pub(super) unsafe fn play_item_now(
    mt: &crate::task::MainThread,
    mm: &crate::pms::PmsMovie,
    from_start: bool,
    from: Origin,
    hud_ms: u32,
    route: &mut Route,
    play_from: &mut Node,
    hud_nav: &mut HudNav,
) {
    if mm.rk.is_empty() {
        return;
    }
    if !crate::route::request_play_movie(mm) {
        return;
    }
    // resolve OFF the SDL loop — pump_play starts it
    // Fetch OFF the loop too — pump_detail lands it. Nothing here reads current(): every
    // start_playback argument comes from `mm` (the catalog row), and the in-player track
    // menu reads metadata::playing(), which the resolve worker installs. The one consumer
    // is sync_now_playing()'s descriptor for the HUD caption and Info card, so a landing a
    // beat later costs a few frames of missing caption, never a wrong play.
    // Retire the old descriptor first: it describes the PREVIOUSLY played item, and the
    // HUD caption + Info card read it every frame — leaving it up would label this
    // playback with the last one's title for the whole pre-roll. None is honest (the
    // route's own TITLE/CTXLINE, set synchronously by request_play_movie, still carry
    // this item), and the landing refills it via sync_now_playing.
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid: mm.sid, rk: mm.rk.to_string() });
    start_playback(
        mt,
        if from_start {
            0
        } else {
            crate::metadata::resume_ns(mm.resume_ms, mm.dur_ns / 1_000_000)
        },
        from,
        hud_ms,
        route,
        play_from,
        hud_nav,
    );
}

/// A playback FAILURE owns the whole frame (`player_hud::transport_hidden`): `draw_hud` returns
/// before painting anything and the overlay panels below are gated the same way, so the scrubber,
/// the control row, the bottom tabs and any panel that happened to be open are all absent from the
/// picture. Nothing that is not drawn may be driven — the rule `ControlSlot::UpNext` states and
/// `up_next::card_active` already keeps for the post-play card.
///
/// Two exceptions are painted on the read-out: BACK returns, while OK opens the shared quality
/// ladder on its current rung.  Selecting that rung is a plain retry; selecting another starts the
/// same item under the new policy.  A failure is therefore terminal for the Engine, not a trap for
/// the viewer.
///
/// This arm's guard still swallows Menu / Info / Chapters while the transport is absent.  It
/// explicitly exempts the More route it opened, so only that visible recovery panel reaches the
/// ordinary modal key arm beneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailedKeyAction {
    ChooseQuality,
    Return,
    Ignore,
}

pub(super) fn failed_key_action(ok: bool, back: bool) -> FailedKeyAction {
    if ok {
        FailedKeyAction::ChooseQuality
    } else if back {
        FailedKeyAction::Return
    } else {
        FailedKeyAction::Ignore
    }
}

pub(super) fn key_player_failed(
    mt: &crate::task::MainThread,
    sym: c_uint,
    wcode: c_uint,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
) {
    match failed_key_action(is_ok(sym), is_back(sym, wcode)) {
        FailedKeyAction::ChooseQuality => {
            crate::ui::more_menu::open_quality();
            *route = Route::Player {
                overlay: Overlay::More,
            };
        }
        FailedKeyAction::Return => {
            if matches!(modal_of(*route), Modal::None) {
                exit_player(mt, route, play_from, refresh_hubs_at, trail);
            } else {
                close_player_overlays();
                *route = Route::Player {
                    overlay: Overlay::None,
                };
            }
        }
        FailedKeyAction::Ignore => {}
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

/// Whether an open player overlay swallows this key rather than letting it fall through, past its
/// own `if...continue` arm, to the ordinary dispatch further down the event loop —
/// `key_pause`/`key_play`/the `Key::PlayPause` arm, none of which carry an overlay term of their
/// own, so falling through reaches the exact toggle the HUD uses with no overlay open and leaves
/// `route` — and therefore the open panel — untouched.
///
/// Tracks (`Overlay::Menu`) / Info / Chapters are each modal and swallow almost everything by
/// design (see their own `key_track_menu`/`key_info_panel`/`key_chapters` doc comments), but
/// transport keys are never overlay-scoped: a viewer holding one of those three open still expects
/// PAUSE/PLAY to work. `Overlay::More` (the `…` options popover) keeps the old swallow-everything
/// answer — the transport exception was reported and reproduced against Info/Chapters/Tracks, not
/// this one, and its own call site never consults this function at all, always dispatching to
/// `key_more_menu` unconditionally; this arm exists so the predicate still answers truthfully for
/// it rather than silently claiming nothing is swallowed. `Overlay::None` and every non-Player
/// route swallow nothing HERE for the opposite reason: none of them has an overlay arm above the
/// ordinary dispatch for this function to be asked about in the first place.
pub(super) fn overlay_swallows_key(route: Route, key: Key) -> bool {
    match route {
        Route::Player {
            overlay: Overlay::Menu | Overlay::Info | Overlay::Chapters,
        } => !matches!(key, Key::Pause | Key::Play | Key::PlayPause),
        Route::Player {
            overlay: Overlay::More,
        } => true,
        _ => false,
    }
}

#[cfg(test)]
mod overlay_transport_key_tests {
    //! Reproduces issue 28 as a pure decision, the way `route_tests` grades the route-classifying
    //! functions elsewhere in this file: nothing here runs the SDL loop or touches a global, so
    //! this cannot say whether the panel visually stays up (a device check does that) — only
    //! whether the DISPATCH itself would have reached the overlay handler or fallen through to the
    //! ordinary transport-key arms. Watched red against the pre-fix body (the function returned
    //! `true` unconditionally for the three overlays, i.e. it had no `key` term at all).
    use super::*;

    /// `Overlay` derives no `Debug` (like `Route` beside it), so these name their own labels
    /// rather than reaching for `{:?}`.
    const MODAL_OVERLAYS: [(Overlay, &str); 3] = [
        (Overlay::Menu, "Menu (tracks)"),
        (Overlay::Info, "Info"),
        (Overlay::Chapters, "Chapters"),
    ];

    #[test]
    fn pause_falls_through_a_modal_player_overlay() {
        for (overlay, name) in MODAL_OVERLAYS {
            let route = Route::Player { overlay };
            assert!(
                !overlay_swallows_key(route, Key::Pause),
                "PAUSE must reach the toggle with {name} open",
            );
            assert!(
                !overlay_swallows_key(route, Key::Play),
                "PLAY must reach the toggle with {name} open",
            );
            assert!(
                !overlay_swallows_key(route, Key::PlayPause),
                "PLAYPAUSE must reach the toggle with {name} open",
            );
        }
    }

    #[test]
    fn every_other_key_still_stays_swallowed_by_those_three() {
        for (overlay, name) in MODAL_OVERLAYS {
            let route = Route::Player { overlay };
            for key in [Key::Ok, Key::Back, Key::Up, Key::Down, Key::Stop] {
                assert!(
                    overlay_swallows_key(route, key),
                    "{key:?} must still be swallowed by {name}",
                );
            }
        }
    }

    #[test]
    fn the_options_popover_keeps_the_old_swallow_everything_behaviour() {
        let route = Route::Player {
            overlay: Overlay::More,
        };
        for key in [Key::Pause, Key::Play, Key::PlayPause, Key::Ok, Key::Back] {
            assert!(
                overlay_swallows_key(route, key),
                "More is deliberately excluded from the transport exception ({key:?})",
            );
        }
    }

    #[test]
    fn a_route_with_no_overlay_arm_has_nothing_to_swallow() {
        // Not because these routes let transport keys through some OTHER mechanism — the
        // ordinary dispatch just never asks this function about them, since none of them has an
        // `if...continue` overlay arm above it. `false` here documents that, not "always works".
        assert!(!overlay_swallows_key(
            Route::Player {
                overlay: Overlay::None
            },
            Key::Ok
        ));
        assert!(!overlay_swallows_key(Route::Home, Key::Pause));
    }
}

/// The in-player track menu is modal — it swallows every key while open, EXCEPT the transport keys
/// (Pause/Play/PlayPause): see [`overlay_swallows_key`], consulted by the caller before reaching
/// this function at all.
pub(super) fn key_track_menu(sym: c_uint, wcode: c_uint, now: u32, route: &mut Route, held: &mut HeldKey) {
    if sym == SDLK_LEFT || sym == SDLK_RIGHT || sym == SDLK_UP || sym == SDLK_DOWN {
        // move once on the fresh press; holding repeats via the client-side timer
        crate::ui::track_menu::move_focus(sym as c_int);
        held.arm(sym, now);
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        crate::ui::track_menu::on_ok();
        *route = Route::Player {
            overlay: Overlay::None,
        };
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::track_menu::close();
        *route = Route::Player {
            overlay: Overlay::None,
        };
    }
}

/// The `…` overflow popover is modal too, and has ONE column — so LEFT/RIGHT are swallowed without
/// moving anything, rather than falling through to the scrubber.
pub(super) fn key_more_menu(
    mt: &crate::task::MainThread,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    held: &mut HeldKey,
) {
    if sym == SDLK_UP || sym == SDLK_DOWN {
        crate::ui::more_menu::move_focus(sym as c_int);
        held.arm(sym, now);
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        apply_more_action(mt, crate::ui::more_menu::on_ok());
        *route = Route::Player {
            overlay: Overlay::None,
        };
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::more_menu::close();
        *route = Route::Player {
            overlay: Overlay::None,
        };
    }
}

/// The Info card is modal too — it swallows every key while open, EXCEPT the transport keys
/// (Pause/Play/PlayPause): see [`overlay_swallows_key`], consulted by the caller before reaching
/// this function at all.
pub(super) fn key_info_panel(
    mt: &crate::task::MainThread,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
    hud_nav: &mut HudNav,
    held: &mut HeldKey,
    ok_armed: &mut bool,
    press: &mut crate::ui::press::Press,
) {
    if sym == SDLK_DOWN && crate::ui::info_panel::at_last() {
        // past the bottom of the card → drop focus back onto the tabs
        crate::ui::info_panel::close();
        *route = Route::Player {
            overlay: Overlay::None,
        };
        hud_nav.focus = 2;
        extend_hud(now, HUD_LINGER_MS);
    } else if sym == SDLK_UP || sym == SDLK_DOWN {
        crate::ui::info_panel::move_focus(sym as c_int);
        held.arm(sym, now); // holding repeats via the client-side timer
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) && crate::ui::info_panel::focus_is_ctl() {
        // The card's two actions are control faces with a pop of their own, so OK takes the tvOS
        // press and `commit_info_panel` spends it on the spring-back. The card stays up through the
        // dip — `info_panel::on_ok` is what takes it down — so the whole animation is on screen.
        press.begin_ctl(now);
        *ok_armed = true;
    } else if is_ok(sym) {
        commit_info_panel(mt, now, route, play_from, refresh_hubs_at, trail);
    } else if is_back(sym, wcode) {
        crate::ui::info_panel::close();
        *route = Route::Player {
            overlay: Overlay::None,
        };
        extend_hud(now, HUD_LINGER_MS);
    }
}

/// Activate the Info card's focused action — the deferred half of [`key_info_panel`]'s OK arm, run
/// from the per-frame loop on the press spring-back (and directly for a focus that is on the TABS
/// above the column, which has no face to dip).
pub(super) fn commit_info_panel(
    mt: &crate::task::MainThread,
    now: u32,
    route: &mut Route,
    play_from: &Node,
    refresh_hubs_at: &mut u32,
    trail: &mut Trail,
) {
    match crate::ui::info_panel::on_ok() {
        crate::ui::info_panel::InfoAction::FromBeginning => {
            request_seek(0);
            if paused() {
                set_transport_paused(mt, false);
            }
        }
        crate::ui::info_panel::InfoAction::GoToDetail(rk) => {
            // Leave playback through THE exit ritual, then override where
            // it landed. This arm used to hand-roll the exit — overlays +
            // stop_bufferfeed — which is three quarters of `exit_player`
            // and silently dropped the other quarter: `route::cancel_play()`
            // (a jump taken while a play resolve was still in flight left
            // it to land later on Detail, starting audio the user cannot
            // reach) and the armed hub refresh (Continue Watching kept the
            // resume point from BEFORE this session — exactly the stale-CW
            // bug `exit_player`'s doc warns a new exit path re-introduces).
            // The override is the one real difference: the Info card's
            // "Go to Show/Movie" always lands on THIS rk's page, whatever
            // origin route the ritual would otherwise have chosen.
            if !rk.is_empty() {
                // The played leaf's server, read BEFORE the exit ritual —
                // `detail_rk` is that item's own show, so it is on the same
                // machine, and the store this reads is torn down below.
                let sid = crate::metadata::playing()
                    .map(|p| p.sid)
                    .unwrap_or_else(crate::plex::current_server);
                exit_player(mt, route, play_from, refresh_hubs_at, trail);
                // A LANDING, not a navigation, so the trail is made to agree
                // rather than pushed blindly: the exit above has usually
                // already put this very page on top (the show playback
                // started from), and `ensure_detail` is a no-op there. It is
                // also strictly better than the flag it replaces — a
                // Library → detail → play → "Go to Show" now returns to the
                // Library instead of to Home.
                trail.ensure(&to_detail(sid, &rk));
                *route = Route::Detail;
            }
        }
        crate::ui::info_panel::InfoAction::None => {}
    }
    // guarded: the GoToDetail arm above set Route::Detail — don't resurrect Player over it
    if matches!(*route, Route::Player { .. }) {
        *route = Route::Player {
            overlay: Overlay::None,
        };
    }
    extend_hud(now, HUD_LINGER_MS);
}

/// The Chapters strip is modal too — LEFT/RIGHT pick, OK seeks, BACK closes — EXCEPT the transport
/// keys (Pause/Play/PlayPause): see [`overlay_swallows_key`], consulted by the caller before
/// reaching this function at all.
pub(super) fn key_chapters(
    mt: &crate::task::MainThread,
    key: Key,
    sym: c_uint,
    wcode: c_uint,
    now: u32,
    route: &mut Route,
    hud_nav: &mut HudNav,
    held: &mut HeldKey,
) {
    if matches!(key, Key::Left { .. } | Key::Right { .. }) {
        let dir_sym = if matches!(key, Key::Left { .. }) {
            SDLK_LEFT
        } else {
            SDLK_RIGHT
        };
        crate::ui::chapters_panel::move_focus(dir_sym as c_int);
        // hold-repeat via the client-side timer, but only when the direction
        // arrived as the plain sym (keyup clears held_key.sym by matching sym;
        // arming it with a normalized key for the alt-d-pad wcodes would stick
        // on release).
        if matches!(key, Key::Left { alt: false } | Key::Right { alt: false }) {
            held.arm(sym, now);
        }
        extend_hud(now, HUD_MENU_MS);
    } else if is_ok(sym) {
        let ns = crate::ui::chapters_panel::on_ok();
        if ns >= 0 {
            request_seek(ns);
            if paused() {
                set_transport_paused(mt, false);
            }
        }
        *route = Route::Player {
            overlay: Overlay::None,
        };
        extend_hud(now, HUD_LINGER_MS);
    } else if matches!(key, Key::Down) {
        // drop focus back onto the tabs below the strip
        crate::ui::chapters_panel::close();
        *route = Route::Player {
            overlay: Overlay::None,
        };
        hud_nav.focus = 2;
        extend_hud(now, HUD_LINGER_MS);
    } else if is_back(sym, wcode) {
        crate::ui::chapters_panel::close();
        *route = Route::Player {
            overlay: Overlay::None,
        };
        extend_hud(now, HUD_LINGER_MS);
    }
}

/// Playing: UP/DOWN move the HUD focus (scrubber ↔ buttons ↔ tabs). The first press on a hidden HUD
/// just reveals it (focused on the scrubber); pressing UP with nothing focusable above (the buttons
/// row) hides the HUD again.
pub(super) fn key_player_updown(key: Key, now: u32, hud: &mut HudState, scrubber: &mut Scrub) {
    // the pre-press sample, not a fresh one: `begin_fresh_press` has already cleared `dismissed`,
    // so re-asking would call a hand-hidden transport visible (`HudState::visible_at_press`)
    let vis = hud.visible_at_press;
    let mut hide = false;
    if !vis {
        hud.nav.focus = 0; // reveal, on the scrubber
    } else if matches!(key, Key::Up) {
        // vertical stack, top → bottom: control row, scrubber, tabs. Both
        // marker stand-ins live IN the control row, so the ring is unchanged.
        match hud.nav.focus {
            0 => hud.nav.focus = 1, // scrubber → control row
            2 => hud.nav.focus = 0, // tabs → scrubber
            _ => {
                hide = true; // control row: nothing above → hide the HUD
                hud.nav.focus = 0;
            }
        }
    } else {
        match hud.nav.focus {
            0 => hud.nav.focus = 2, // scrubber → tabs
            1 => hud.nav.focus = 0, // buttons → scrubber
            _ => {}                 // tabs: nothing below → stay
        }
    }
    if hud.nav.focus != 0 || hide {
        // leaving the bar cancels any in-progress scrub preview
        if scrub() >= 0 {
            set_scrub(-1);
        }
        scrubber.disengage();
    }
    if hide {
        hud.dismissed = true; // stays hidden even while paused, until the next key
    } else {
        extend_hud(now, HUD_LINGER_MS);
    }
}

/// OK, on every screen that has not already `continue`d above.
/// Activate the player transport's focused CONTROL ROW item — the deferred half of [`key_ok`]'s
/// player arm, run from the per-frame loop once the press spring-back has played.
///
/// Two arms, in the order they were written in `key_ok`: a STAND-IN owns the row (Skip, Up Next) and
/// performs its own action, or the row holds the three discs and OK opens that disc's panel. `ctrl`
/// is re-resolved by the caller on the committing frame rather than captured at the press, so the
/// activation acts on the row that is DRAWN — the slot is resolved once per loop iteration for input,
/// update and draw alike (see the `let ctrl` at the top of the loop), and an offer that arrived
/// mid-press has already changed what the user is looking at.
pub(super) unsafe fn activate_player_row(
    mt: &crate::task::MainThread,
    ctrl: crate::ui::player_hud::ControlSlot,
    now: u32,
    route: &mut Route,
    hud: &mut HudState,
    held: &mut HeldKey,
    trail: &mut Trail,
    play_from: &mut Node,
    refresh_hubs_at: &mut u32,
) {
    if !ctrl.is_discs() {
        // A stand-in owns row 1 — activate it. Same value the draw used.
        if activate_ctrl_row(
            mt,
            ctrl,
            route,
            play_from,
            refresh_hubs_at,
            &mut hud.nav,
            trail,
        ) {
            held.sym = 0; // async route flip: don't repeat a held key into the next screen
        }
    } else if hud.nav.btn == crate::ui::player_hud::BTN_MORE {
        // …so the discs are what row 1 holds — the complement of the arm above, and the row's only
        // other occupant. OK on a control disc opens its panel (Subtitles / Audio / More).
        crate::ui::more_menu::open();
        *route = Route::Player {
            overlay: Overlay::More,
        };
    } else {
        crate::ui::track_menu::open_tab(if hud.nav.btn == 0 { 1 } else { 0 });
        *route = Route::Player {
            overlay: Overlay::Menu,
        };
    }
    extend_hud(now, HUD_LINGER_MS);
}

/// PAUSE — the dedicated transport key, which only ever pauses (PLAY is its other half).
pub(super) fn key_pause(mt: &crate::task::MainThread, route: Route, now: u32) {
    if matches!(route, Route::Player { .. }) && !paused() {
        if set_transport_paused(mt, true) {
            crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                feature: crate::diag::schema::Feature::Pause,
            });
        }
    }
    extend_hud(now, HUD_LINGER_MS);
}

/// PLAY — off the player route it starts the buffer-feed and enters the player; on it, it un-pauses.
pub(super) unsafe fn key_play(
    mt: &crate::task::MainThread,
    now: u32,
    foreground: &mut ForegroundLifecycle,
    repause_at: &mut i64,
    route: &mut Route,
    play_from: &mut Node,
    ptr: &mut Pointer,
    trail: &Trail,
) {
    let was_off_player = !matches!(*route, Route::Player { .. });
    if was_off_player {
        foreground.discard_started_state();
    }
    let activation = drive_foreground(
        foreground,
        ForegroundInput::PlayKey,
        &mut PlayerForegroundActuator { mt, repause_at },
    );
    if matches!(activation, ForegroundActivation::Launched) {
        // A suspended session KEEPS its origin. The lifecycle arm forced its route to Home, but
        // that temporary screen is not where Stop/BACK should return.
        *route = Route::Player {
            overlay: Overlay::None,
        };
    } else if matches!(activation, ForegroundActivation::Ordinary) {
        if !matches!(*route, Route::Player { .. }) {
            if crate::player::start_bufferfeed(mt) {
                if let Origin::From(n) = origin_here(*route, trail) {
                    *play_from = n;
                }
                *route = Route::Player {
                    overlay: Overlay::None,
                };
                // Keep the ordinary off-route start's existing stale-Pause defense. A foreground
                // transition applies its explicit clock intent through the lifecycle actuator.
                if paused() {
                    set_transport_paused(mt, false);
                }
            }
        } else if paused() {
            set_transport_paused(mt, false);
        }
    }
    if was_off_player && !ptr.dpad_mode {
        hide_cursor();
        ptr.dpad_mode = true;
        ptr.cur_hidden = true;
    }
    extend_hud(now, HUD_LINGER_MS);
}

/// What a LEFT/RIGHT press on the player DOES — the decision alone, with nothing done yet.
///
/// It is a value rather than a ladder inside [`key_scrub`] because the interesting arm is the one
/// that acts on nothing the user can see, and that arm is unreachable from a host test: the ladder
/// lives inside the SDL event loop and every other branch reaches the player's globals. Deciding
/// first and acting second is what makes the rule itself testable (`a_hidden_hud_spends_the_press`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ScrubPress {
    /// The HUD is not on screen, so the press is spent RAISING it and the playhead does not move.
    /// See [`key_scrub`] for why a control the user cannot see must not be driven blind.
    Reveal,
    /// the control row (Subtitles / Audio / More, or whichever stand-in owns it) — move its cursor
    Row,
    /// the bottom tabs (Info / Chapters) — move theirs
    Tabs,
    /// the scrubber: jump the preview by [`SCRUB_STEP_NS`]
    Jump,
    /// the scrubber, on something with no duration to move through — a live or still-loading item
    Nothing,
}

/// `vis` is [`hud_visible`] sampled BEFORE this press re-arms the timer; `focus` is
/// [`HudNav::focus`]; `seekable` is `dur() > 0`.
pub(super) fn scrub_press(vis: bool, focus: i32, seekable: bool) -> ScrubPress {
    if !vis {
        return ScrubPress::Reveal;
    }
    match focus {
        1 => ScrubPress::Row,
        2 => ScrubPress::Tabs,
        _ if seekable => ScrubPress::Jump,
        _ => ScrubPress::Nothing,
    }
}

/// LEFT/RIGHT while playing: move the focused HUD row's cursor, or — on the scrubber — jump the
/// scrub preview. A fresh press (0x001) is the fixed 10s jump; a held key's 0x101 repeats
/// ([`on_auto_repeat`]) then engage the continuous scrub and the keyup commits.
///
/// **A press that finds the HUD hidden is spent RAISING it, and moves nothing.** The transport is
/// on a 4.5 s timer over full-screen video, so "where am I" and "take me back ten seconds" are two
/// different intentions and the remote has no way to tell them apart — the old ladder read every
/// LEFT as the second, and a viewer glancing at the clock lost their place to it. It is the rule
/// UP/DOWN has always had one arm over ([`key_player_updown`]) and the rule the CLICK path already
/// enforced on this very band (`hud_vis` there is sampled before the click re-arms the timer,
/// after "a click in the invisible timed-out scrub band committed a blind seek"); the key path was
/// the last way in that still acted on geometry nobody could see.
///
/// **A HOLD is not a tap and keeps working.** The reveal still arms `dir` and seeds the preview, so
/// a user who holds LEFT to rewind gets the HUD on screen and then the ordinary continuous scrub as
/// the 0x101 repeats arrive — by which point the band they are dragging IS on screen. Only the
/// discrete 10 s hop waits for a second press, which is what `Scrub::reveal` marks: without it the
/// tap release would commit a seek to the seed, i.e. a full reopen+prime to the spot we are
/// already sitting on.
pub(super) unsafe fn key_scrub(
    key: Key,
    now: u32,
    ctrl: crate::ui::player_hud::ControlSlot,
    hud: &mut HudState,
    ptr: &mut Pointer,
    scrubber: &mut Scrub,
) {
    if !ptr.cur_hidden {
        hide_cursor();
        ptr.cur_hidden = true;
    }
    if ptr.drag {
        ptr.drag = false;
        set_scrub(-1);
    }
    let fwd = matches!(key, Key::Right { .. });
    // the pre-press sample — see `key_player_updown`'s note and `HudState::visible_at_press`
    let act = scrub_press(hud.visible_at_press, hud.nav.focus, dur() > 0);
    extend_hud(now, HUD_LINGER_MS);
    match act {
        ScrubPress::Reveal => {
            hud.nav.focus = 0; // the HUD comes up on the scrubber, ready for the press after this one
                               // …and the gesture is armed but not spent, so a user who keeps HOLDING gets the
                               // ordinary continuous scrub the moment the repeats arrive. Only on something with a
                               // duration: arming a scrub over an item there is nothing to move through would put a
                               // preview on a band that cannot answer for it.
            if dur() > 0 {
                scrubber.begin(now, fwd);
                scrubber.reveal = true; // …but this press hops nothing; only a HOLD grows out of it
                seed_scrub();
            }
        }
        ScrubPress::Row => {
            // the row's occupant says how many items it has — no magic pin
            hud.nav.btn = (hud.nav.btn + if fwd { 1 } else { -1 }).clamp(0, ctrl.items() - 1);
        }
        ScrubPress::Tabs => {
            let max_tab = if crate::ui::chapters_panel::has_chapters() {
                1
            } else {
                0
            };
            hud.nav.tab = (hud.nav.tab + if fwd { 1 } else { -1 }).clamp(0, max_tab);
        }
        ScrubPress::Jump => {
            // scrubber focus, FRESH press (0x001): the fixed 10s jump. A held key's
            // 0x101 repeats (handled above) then engage the continuous scrub; the
            // keyup commits. Quick re-taps before scrubber.commit_at accumulate.
            let cap = dur() - 3 * 1_000_000_000;
            scrubber.commit_at = 0; // more input → cancel a pending tap commit
            scrubber.alive = now;
            scrubber.reveal = false; // a visible press is a real gesture whatever raised the HUD
            if scrubber.dir == 0 && scrub() < 0 {
                seed_scrub();
            }
            if !scrubber.hold {
                let mut s = scrub().max(0) + if fwd { SCRUB_STEP_NS } else { -SCRUB_STEP_NS };
                if s < 0 {
                    s = 0;
                }
                if cap > 0 && s > cap {
                    s = cap;
                }
                set_scrub(s);
            }
            scrubber.dir = if fwd { 1 } else { -1 };
        }
        ScrubPress::Nothing => {}
    }
}

/// Seed a new scrub at the INTENDED playhead ([`intended_pos`]). If a prior commit's seek is still
/// landing, `playpos()` is stale (it still reports the pre-seek spot), so a quick re-press would
/// jump back to where we started and resume there — interrupting the scrub. The divergence IS "a
/// seek is in flight", so log it when the two disagree rather than re-deriving the condition here.
pub(super) unsafe fn seed_scrub() {
    let seed = intended_pos();
    let live = playpos();
    if seed != live {
        log(&format!(
            "scrub: seed at in-flight target {}s (playpos {}s stale)",
            seed / 1_000_000_000,
            live / 1_000_000_000
        ));
    }
    set_scrub(seed);
}

/// Run a play-plan landing and then observe the derived player state in the same frame. This tiny
/// seam is explicit because a refused `/decision` publishes `Error` inside the landing, after the
/// loop's ordinary report tick; BACK on the next frame can otherwise erase the only observation.
pub(super) fn land_play_then_observe(land: impl FnOnce(), observe: impl FnOnce()) {
    land();
    observe();
}

#[cfg(test)]
mod play_landing_order_tests {
    use super::land_play_then_observe;
    use std::cell::RefCell;

    #[test]
    fn the_landing_seam_runs_publication_before_observation() {
        let order = RefCell::new(Vec::new());
        land_play_then_observe(
            || order.borrow_mut().push("landing"),
            || order.borrow_mut().push("observation"),
        );
        assert_eq!(*order.borrow(), ["landing", "observation"]);
    }
}
