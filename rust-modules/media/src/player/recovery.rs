//! The failed-open recovery ladder: what a play request does when the pipeline (or the source)
//! fails it BEFORE the first picture.
//!
//! `OpenRecovery` is a tested `State + RecoveryEvent -> Effects` machine in the shape of
//! [`super::lifecycle::ForegroundLifecycle`]: its state is private and its one writer is
//! [`step`](OpenRecovery::step). It reads no global and performs no IO. The facts a decision needs
//! (an HLS->Original trial holding a live encoder, a cold Auto Original, no decoded frame yet, the
//! natively fed audio a refusal names) arrive as a value on the event ([`OpenFacts`]); the work a
//! decision asks for (the learner's write, a recovery flight, the failure read-out) leaves as an
//! [`RecoveryEffect`] which the pump (`super::pump`, the driver) executes through the route's own dispatches,
//! and every answer returns as the next event in the same drain.
//!
//! **The owner is the request.** [`PlaybackSession`](crate::route::PlaybackSession) holds one
//! `Option<OpenRecovery>`, created by `begin_attempt` with the capability value the app's play start
//! read, and replaced by the next request's. The machine also owns that value: the attempt's
//! capabilities are `start` narrowed by every audio facet the install just learned the pipeline
//! refuses ([`Caps::without`]); [`attempt`](OpenRecovery::attempt) is the only reader the route
//! plans from, and `attempt` is derived from `start` each time a facet is added, never narrowed in
//! place.
//!
//! **The ladder** is the order main's `open_failure_action` fixed, unchanged (the table is on
//! [`OpenRecovery::step`]): a pending HLS->Original rollback first (its HLS route is the one a
//! failed open owes the viewer), else the conversion of native audio the install just learned the
//! pipeline refuses, else the HLS contingency of a cold Auto Original, else the failure read-out.
//! A refusal of natively fed audio first asks the driver to confirm it ([`RecoveryEffect::ConfirmRefusal`]:
//! the install's learner is IO and answers whether the combination was NEW); the answer picks the
//! rung. The conversion is therefore rung-selected exactly as it was when the pump learned first.
//!
//! The state the machine does not own: the flight bookkeeping stays in `route::decision`
//! (`original_recovery_pending`, the recovery flight slot), and it reaches the machine as a fact
//! on the event, not as a field.

use std::sync::Arc;

use plx_machine::machine::Handled;
use plx_platform::devcaps::Caps;
use plx_platform::devcaps::Facet;

/// How the open failed, as the pump's failure branches see it. The event payload; no field of the
/// session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadRefusal {
    /// The pipeline's `type=18` error callback, with its number (`200` Audio Codec Not Supported,
    /// `601` Resource Allocation Error, ...). Also the synchronous `sf_load == 0` refusal, which
    /// publishes the same flag.
    Refused { num: i32 },
    /// The Load budget ran out with the pipeline silent.
    TimedOut,
    /// The surround native-audio watchdog fired: the pipeline took the Load and a few access units,
    /// then swallowed the feed with no picture ([`super::native_stall`]).
    Stalled,
    /// Not the pipeline: the demuxer or the transport died before a picture.
    SourceFailed,
}

impl std::fmt::Display for LoadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { num } => write!(f, "type=18 num={num}"),
            Self::TimedOut => f.write_str("Load timed out"),
            Self::Stalled => f.write_str("native audio stall"),
            Self::SourceFailed => f.write_str("source failed"),
        }
    }
}

/// What the driver read, at the moment of a failure, that the ladder decides on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFacts {
    /// An HLS->Original trial holds a live encoder for exactly this outcome
    /// (`route::original_recovery_pending`).
    pub pending_original: bool,
    /// The playing item is a cold Auto Original (`route::auto_original_watch(..)` is some).
    pub auto_original: bool,
    /// No picture has decoded yet in this Engine (`frames == 0`).
    pub no_frames: bool,
    /// The natively fed audio this failure names: the pipeline refused it (`type=18 num=200`) or
    /// swallowed it (the stall). `None` when the failure does not name audio.
    pub native_audio: Option<Facet>,
}

/// What moves the ladder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEvent {
    /// The open failed. Consumed in `Opening` and `Playing` (a failure after a picture runs the same
    /// ladder, whose `no_frames` fact then stops it at the failure read-out); a failure while a
    /// flight, a confirmation or the read-out is already outstanding is not.
    Refused { refusal: LoadRefusal, facts: OpenFacts },
    /// The answer to [`RecoveryEffect::ConfirmRefusal`]: `newly` is true when the combination was new to
    /// the install (the learner recorded it).
    RefusalConfirmed { newly: bool },
    /// The recovery flight installed its replacement route; the driver reloads onto it.
    FlightLanded,
    /// The recovery flight could not start, or came back refused.
    FlightRefused,
    /// The first decoded picture of the open.
    FirstPicture,
}

/// What the driver executes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEffect {
    /// Learn the natively fed audio the facts named as refused on this firmware (the install's
    /// learner, which the driver runs against the audio it fed) and answer
    /// [`RecoveryEvent::RefusalConfirmed`].
    ConfirmRefusal,
    /// Fly the HLS rollback of the failed Original trial (`dispatch_rollback_rebase`).
    Rollback,
    /// Fly the same route rebuilt with the server converting the audio the attempt no longer feeds
    /// natively (`dispatch_native_audio_conversion`; it plans from [`OpenRecovery::attempt`]).
    StartFlight,
    /// Fly the HLS contingency a cold Auto Original retained (`dispatch_unopened_auto_hls`).
    StartAutoHls,
    /// Fail the Engine for good and show the failure read-out.
    ShowError { refusal: LoadRefusal },
}

#[derive(Clone, Debug)]
enum State {
    /// The open is under way (the initial state, and the state a landed flight's reload re-enters).
    Opening,
    /// A native-audio refusal waits on the learner's answer.
    Confirming { refusal: LoadRefusal, facts: OpenFacts },
    /// A recovery flight is outstanding; `cause` is the refusal that started it.
    Flight { cause: LoadRefusal },
    /// A picture decoded: the ladder is over.
    Playing,
    /// The ladder is exhausted and the read-out was asked for.
    Failed,
}

/// The ladder for one play request. See the module doc.
#[derive(Clone, Debug)]
pub struct OpenRecovery {
    /// What the app's play start read; never changed.
    start: Arc<Caps>,
    /// `start` narrowed by `excluded`: the value the route plans and profiles from. Assigned only by
    /// [`exclude`](Self::exclude), always from `start`.
    attempt: Arc<Caps>,
    /// The audio facets the install just learned the pipeline refuses, in the order learned.
    excluded: Vec<Facet>,
    state: State,
}

impl OpenRecovery {
    /// A fresh request on `start`.
    pub fn new(start: Arc<Caps>) -> Self {
        Self { attempt: start.clone(), start, excluded: Vec::new(), state: State::Opening }
    }

    /// The capability value this request plans and profiles from: `start` less every learned
    /// refusal. A cheap `Arc` clone, so a worker or a flight can own it.
    pub fn attempt(&self) -> Arc<Caps> {
        self.attempt.clone()
    }

    /// Take `facet` out of the attempt. Derived from `start`, so the value is the same whatever the
    /// order the facets were learned in.
    fn exclude(&mut self, facet: Facet) {
        self.excluded.push(facet);
        let narrowed = self.excluded.iter().fold((*self.start).clone(), |caps, facet| caps.without(facet));
        self.attempt = Arc::new(narrowed);
    }

    /// The rung `facts` select, once the learner's answer (`learned`: a native-audio refusal NEW to
    /// the install, which the attempt was just narrowed by) is known. The order is main's
    /// `open_failure_action`.
    fn decide(&mut self, refusal: LoadRefusal, facts: &OpenFacts, learned: bool, fx: &mut Vec<RecoveryEffect>) {
        if facts.pending_original {
            fx.push(RecoveryEffect::Rollback);
            self.state = State::Flight { cause: refusal };
        } else if learned {
            fx.push(RecoveryEffect::StartFlight);
            self.state = State::Flight { cause: refusal };
        } else if facts.auto_original && facts.no_frames {
            fx.push(RecoveryEffect::StartAutoHls);
            self.state = State::Flight { cause: refusal };
        } else {
            fx.push(RecoveryEffect::ShowError { refusal });
            self.state = State::Failed;
        }
    }

    /// The one writer. The table (a state not listed answers [`Handled::No`] and changes nothing):
    ///
    /// | state | event | effects, next state |
    /// |---|---|---|
    /// | `Opening`, `Playing` | `Refused`, facts name native audio | `ConfirmRefusal`; `Confirming` |
    /// | `Opening`, `Playing` | `Refused`, otherwise | the ladder below, `learned = false` |
    /// | `Confirming` | `RefusalConfirmed{newly}` | `newly`: the audio is excluded from the attempt; then the ladder with `learned = newly`. A `Stalled` refusal of a combination the install already knew only logs (main's behavior, unchanged here): no effect, `Opening` |
    /// | `Flight` | `FlightLanded` | none; `Opening` (the reload re-enters the open) |
    /// | `Flight` | `FlightRefused` | `ShowError`; `Failed` |
    /// | `Opening` | `FirstPicture` | none; `Playing` |
    ///
    /// The ladder: `pending_original` -> `Rollback`; else `learned` -> `StartFlight`; else
    /// `auto_original && no_frames` -> `StartAutoHls` (each: `Flight`); else `ShowError`; `Failed`.
    pub fn step(&mut self, ev: &RecoveryEvent, fx: &mut Vec<RecoveryEffect>) -> Handled {
        match ev {
            RecoveryEvent::Refused { refusal, facts } => {
                if !matches!(self.state, State::Opening | State::Playing) {
                    return Handled::No;
                }
                match &facts.native_audio {
                    Some(_) => {
                        fx.push(RecoveryEffect::ConfirmRefusal);
                        self.state = State::Confirming { refusal: *refusal, facts: facts.clone() };
                    }
                    None => self.decide(*refusal, facts, false, fx),
                }
                Handled::Yes
            }
            RecoveryEvent::RefusalConfirmed { newly } => {
                let State::Confirming { refusal, facts } = &self.state else {
                    return Handled::No;
                };
                let (refusal, facts) = (*refusal, facts.clone());
                if *newly {
                    if let Some(facet) = &facts.native_audio {
                        self.exclude(facet.clone());
                    }
                }
                if refusal == LoadRefusal::Stalled && !*newly {
                    self.state = State::Opening;
                } else {
                    self.decide(refusal, &facts, *newly, fx);
                }
                Handled::Yes
            }
            RecoveryEvent::FlightLanded => {
                if !matches!(self.state, State::Flight { .. }) {
                    return Handled::No;
                }
                self.state = State::Opening;
                Handled::Yes
            }
            RecoveryEvent::FlightRefused => {
                let State::Flight { cause } = self.state else {
                    return Handled::No;
                };
                fx.push(RecoveryEffect::ShowError { refusal: cause });
                self.state = State::Failed;
                Handled::Yes
            }
            RecoveryEvent::FirstPicture => {
                if !matches!(self.state, State::Opening) {
                    return Handled::No;
                }
                self.state = State::Playing;
                Handled::Yes
            }
        }
    }

    /// Read-only view of the machine for tests; every write goes through [`step`](Self::step).
    #[cfg(any(test, feature = "test-support"))]
    pub fn phase(&self) -> Phase {
        match self.state {
            State::Opening => Phase::Opening,
            State::Confirming { .. } => Phase::Confirming,
            State::Flight { .. } => Phase::Flight,
            State::Playing => Phase::Playing,
            State::Failed => Phase::Failed,
        }
    }
}

/// [`OpenRecovery`]'s state, without its payloads, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Opening,
    Confirming,
    Flight,
    Playing,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A facet the assumed device caps announce, so excluding it is a real narrowing.
    fn ac3_6() -> Facet {
        Facet::Audio { codec: "ac3".into(), channels: 6 }
    }

    fn machine() -> OpenRecovery {
        OpenRecovery::new(Arc::new(Caps::assumed()))
    }

    fn facts(pending_original: bool, auto_original: bool, no_frames: bool, native_audio: bool) -> OpenFacts {
        OpenFacts { pending_original, auto_original, no_frames, native_audio: native_audio.then(ac3_6) }
    }

    fn run(m: &mut OpenRecovery, ev: RecoveryEvent) -> (Handled, Vec<RecoveryEffect>) {
        let mut fx = Vec::new();
        let handled = m.step(&ev, &mut fx);
        (handled, fx)
    }

    const SHUT: LoadRefusal = LoadRefusal::Refused { num: 601 };

    fn refused(refusal: LoadRefusal, facts: OpenFacts) -> RecoveryEvent {
        RecoveryEvent::Refused { refusal, facts }
    }

    /// The ladder `open_failure_action` fixed, by facts and the learner's answer (`learned`), through
    /// the machine: the rung a refusal lands on.
    fn rung(pending: bool, learned: bool, auto: bool, no_frames: bool) -> Vec<RecoveryEffect> {
        let mut m = machine();
        if learned {
            let (_, fx) = run(&mut m, refused(LoadRefusal::Refused { num: 200 }, facts(pending, auto, no_frames, true)));
            assert_eq!(fx, [RecoveryEffect::ConfirmRefusal]);
            run(&mut m, RecoveryEvent::RefusalConfirmed { newly: true }).1
        } else {
            run(&mut m, refused(SHUT, facts(pending, auto, no_frames, false))).1
        }
    }

    #[test]
    fn a_failed_original_open_prefers_rollback_then_cold_auto_before_error() {
        let error = [RecoveryEffect::ShowError { refusal: SHUT }];
        assert_eq!(rung(true, false, true, true), [RecoveryEffect::Rollback]);
        assert_eq!(rung(false, false, true, true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(rung(false, false, true, false), error, "a post-frame failure is not a cold open");
        assert_eq!(rung(false, false, false, true), error, "manual Original stays the user's fixed choice");
        // A native audio refusal the install just learned rebuilds the same route converted, on a
        // cold Auto Original too (the full HLS fallback would also convert it, but throws the
        // route away), and on a manual Original (no other recovery owns it).
        assert_eq!(rung(false, true, true, true), [RecoveryEffect::StartFlight]);
        assert_eq!(rung(false, true, false, true), [RecoveryEffect::StartFlight]);
        // The rollback of a pending HLS to Original still comes first: its HLS route is the one a
        // failed open owes the viewer, and its audio is the server's.
        assert_eq!(rung(true, true, true, true), [RecoveryEffect::Rollback]);
    }

    #[test]
    fn a_refusal_that_names_native_audio_is_confirmed_before_the_rung_is_picked() {
        let mut m = machine();
        let (handled, fx) = run(&mut m, refused(LoadRefusal::Refused { num: 200 }, facts(false, false, true, true)));
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Confirming));
        assert_eq!(fx, [RecoveryEffect::ConfirmRefusal]);
        // A combination the install already knew: the ladder with `learned = false`, which for a
        // manual Original with nothing pending is the read-out.
        let (handled, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: LoadRefusal::Refused { num: 200 } }]);
    }

    #[test]
    fn a_known_combination_still_takes_the_auto_originals_hls_contingency() {
        let mut m = machine();
        run(&mut m, refused(LoadRefusal::Refused { num: 200 }, facts(false, true, true, true)));
        let (_, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartAutoHls], Phase::Flight));
    }

    #[test]
    fn a_newly_learned_refusal_narrows_the_attempt_on_every_rung_and_only_then() {
        let start = Arc::new(Caps::assumed());
        let narrowed = Arc::new(start.without(&ac3_6()));
        assert_ne!(*start, *narrowed, "the fixture must exercise a real narrowing");
        for (pending, auto) in [(true, true), (false, true), (false, false)] {
            let mut m = OpenRecovery::new(start.clone());
            run(&mut m, refused(LoadRefusal::Refused { num: 200 }, facts(pending, auto, true, true)));
            assert_eq!(*m.attempt(), *start, "nothing is excluded before the learner has answered");
            run(&mut m, RecoveryEvent::RefusalConfirmed { newly: true });
            assert_eq!(*m.attempt(), *narrowed, "pending={pending} auto={auto}: the rollback is narrowed too");
        }
        let mut m = OpenRecovery::new(start.clone());
        run(&mut m, refused(LoadRefusal::Refused { num: 200 }, facts(false, false, true, true)));
        run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!(*m.attempt(), *start, "a combination the install already knew is in `start` already");
    }

    #[test]
    fn the_attempt_is_derived_from_start_whatever_order_facets_are_learned_in() {
        let start = Arc::new(Caps::assumed());
        let stereo = Facet::Audio { codec: "eac3".into(), channels: 2 };
        let ac3 = ac3_6();
        let learn = |order: [&Facet; 2]| {
            let mut m = OpenRecovery::new(start.clone());
            for facet in order {
                let native_audio = Some(facet.clone());
                run(&mut m, refused(LoadRefusal::Refused { num: 200 }, OpenFacts { native_audio, ..facts(false, false, true, false) }));
                run(&mut m, RecoveryEvent::RefusalConfirmed { newly: true });
                // The flight lands and the reload is refused again: the next learned facet narrows
                // the same `start`.
                run(&mut m, RecoveryEvent::FlightLanded);
            }
            m.attempt()
        };
        assert_eq!(*learn([&stereo, &ac3]), *learn([&ac3, &stereo]));
        assert_eq!(*learn([&stereo, &ac3]), start.without(&stereo).without(&ac3));
        assert_ne!(*learn([&stereo, &ac3]), *start, "the fixture must exercise a real narrowing");
    }

    #[test]
    fn a_flight_ends_in_a_reload_or_in_the_read_out() {
        let mut m = machine();
        run(&mut m, refused(SHUT, facts(true, false, true, false)));
        assert_eq!(m.phase(), Phase::Flight);
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightLanded);
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![], Phase::Opening));

        let mut m = machine();
        run(&mut m, refused(SHUT, facts(false, true, true, false)));
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: SHUT }], "the read-out names the refusal that started the flight");
    }

    #[test]
    fn a_failure_while_a_flight_or_the_read_out_is_outstanding_is_not_handled() {
        let mut m = machine();
        run(&mut m, refused(SHUT, facts(true, false, true, false)));
        assert_eq!(run(&mut m, refused(SHUT, facts(false, false, true, false))), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Flight);
        run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(run(&mut m, refused(SHUT, facts(true, false, true, false))), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Failed);
    }

    #[test]
    fn answers_with_no_question_outstanding_are_not_handled() {
        let mut m = machine();
        for ev in [RecoveryEvent::RefusalConfirmed { newly: true }, RecoveryEvent::FlightLanded, RecoveryEvent::FlightRefused] {
            assert_eq!(run(&mut m, ev), (Handled::No, vec![]));
        }
        assert_eq!(m.phase(), Phase::Opening);
    }

    #[test]
    fn the_first_picture_ends_the_ladder_once() {
        let mut m = machine();
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::Yes, vec![]));
        assert_eq!(m.phase(), Phase::Playing);
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::No, vec![]), "idempotent");
        // A failure after a picture runs the same ladder, whose facts stop it at the read-out.
        let (handled, fx) = run(&mut m, refused(LoadRefusal::SourceFailed, facts(false, true, false, false)));
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: LoadRefusal::SourceFailed }]);
    }

    #[test]
    fn a_stall_of_a_combination_the_install_already_knew_only_logs() {
        // Main's behavior, characterized: the watchdog's `native_audio_stall_fired` returned false for
        // a known combination, so the pump went on and the viewer stayed on the buffering spinner.
        let mut m = machine();
        run(&mut m, refused(LoadRefusal::Stalled, facts(false, false, true, true)));
        let (handled, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![], Phase::Opening));
        // A new one takes the conversion.
        let mut m = machine();
        run(&mut m, refused(LoadRefusal::Stalled, facts(false, false, true, true)));
        let (_, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: true });
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartFlight], Phase::Flight));
    }

    #[test]
    fn a_request_starts_on_the_value_it_was_given() {
        let start = Arc::new(Caps::assumed());
        let m = OpenRecovery::new(start.clone());
        assert!(Arc::ptr_eq(&m.attempt(), &start), "no narrowing, no copy");
        assert_eq!(m.phase(), Phase::Opening);
    }

    #[test]
    fn a_refusal_prints_its_number() {
        assert_eq!(LoadRefusal::Refused { num: 601 }.to_string(), "type=18 num=601");
    }
}
