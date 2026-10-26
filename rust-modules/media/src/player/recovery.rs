//! The failed-open recovery ladder: what a play request does when the pipeline (or the source)
//! fails it BEFORE the first picture.
//!
//! `OpenRecovery` is a tested `State + RecoveryEvent -> Effects` machine in the shape of
//! [`super::lifecycle::ForegroundLifecycle`]: its state is private and its one writer is
//! [`step`](OpenRecovery::step). It reads no global and performs no IO. The facts a decision needs
//! (an HLS->Original trial holding a live encoder, a cold Auto Original, Forced Direct Play, no
//! decoded frame yet, the audio the pipeline was fed, the natively fed audio a refusal names) arrive
//! as a value on the event ([`OpenFacts`]); the work a decision asks for (the learner's write, a
//! recovery flight, the failure read-out) leaves as a [`RecoveryEffect`] which the pump
//! (`super::pump`, the driver) executes through the route's own dispatches, and every answer
//! returns as the next event in the same drain.
//!
//! **The owner is the request.** [`PlaybackSession`](crate::route::PlaybackSession) holds one
//! `Option<OpenRecovery>`, created by `begin_attempt` with the capability value the app's play start
//! read, and replaced by the next request's. The machine also owns that value: the attempt's
//! capabilities are `start` narrowed by the one facet the facet rung took out of this request (or
//! the native audio a refusal named while a pending rollback was chosen over the facet rung, so the
//! restored route does not offer it again) ([`Caps::without`]); [`attempt`](OpenRecovery::attempt) is the only reader the route plans from,
//! and `attempt` is derived from `start` on each rung's entry, never narrowed in place.
//!
//! **The ladder** is the order of [`decide`](OpenRecovery::decide): a pending HLS->Original
//! rollback first (its HLS route is the one a failed open owes the viewer); under Forced Direct
//! Play the read-out (a forced play is never converted); else, for a refusal that names the
//! pipeline, the conversion of the audio the pipeline was fed when [`suspect`] names it (the facet
//! rung, once per request, [`RecoveryEffect::ConvertFacet`]); else the HLS contingency of a cold
//! Auto Original (the both-facets rung); else the failure read-out. The rungs which start a flight
//! (the facet and the both-facets rung) are bounded by [`MAX_RECOVERY_FLIGHTS`] per request.
//!
//! **A refusal of natively fed audio** first asks the driver to confirm it
//! ([`RecoveryEffect::ConfirmRefusal`]: the install's learner is IO and persists the combination).
//! The confirmation is persistence only: the answer never selects a rung, the facet rung is
//! selected by [`suspect`] and by the loop guard alone (a facet `start` already lacks narrows
//! nothing and gets no flight, so a stored refusal is never converted twice).
//!
//! The state the machine does not own: the flight bookkeeping stays in `route::decision`
//! (`original_recovery_pending`, the recovery flight slot), and it reaches the machine as a fact
//! on the event, not as a field.

use std::sync::Arc;

use plx_machine::machine::Handled;
use plx_platform::devcaps::Caps;
use plx_platform::devcaps::Facet;

/// The most flights one play request starts through the facet rung and the both-facets rung (and a
/// failed source's HLS contingency): the facet's conversion, then the both-facets rung. The
/// Original rollback is not counted.
pub const MAX_RECOVERY_FLIGHTS: u32 = 2;

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

impl LoadRefusal {
    /// Whether the refusal is the pipeline's, so the stream it was fed is a suspect. A timeout
    /// names no facet (the pipeline said nothing) and a source failure is not the pipeline's.
    pub fn names_pipeline(self) -> bool {
        matches!(self, Self::Refused { .. } | Self::Stalled)
    }
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

/// The audio the pipeline was fed: the stream's declared codec (`route::stream_acodec`) and the
/// carried track's channel count (`route::stream_achannels`, 0 when unknown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamAudio {
    /// The codec in any spelling [`plx_platform::devcaps::audio_codec_id`] knows (`dca` for DTS).
    pub codec: String,
    pub channels: u32,
}

/// The stream class a pipeline refusal puts under suspicion: audio outside the codecs every set
/// decodes (`aac`, `ac3`, `eac3`). `channels == 0` (unknown) names the codec at no width, which
/// [`Caps::without`] reads as "not at all", the conservative choice. Video is S1b's.
pub fn suspect(audio: Option<&StreamAudio>) -> Option<Facet> {
    let a = audio?;
    let id = plx_platform::devcaps::audio_codec_id(&a.codec);
    (!matches!(&*id, "aac" | "ac3" | "eac3")).then(|| Facet::Audio { codec: id.into_owned(), channels: a.channels })
}

/// What the driver read, at the moment of a failure, that the ladder decides on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenFacts {
    /// An HLS->Original trial holds a live encoder for exactly this outcome
    /// (`route::original_recovery_pending`).
    pub pending_original: bool,
    /// The playing item is a cold Auto Original (`route::auto_original_watch(..)` is some).
    pub auto_original: bool,
    /// No picture has decoded yet in this Engine (`frames == 0`).
    pub no_frames: bool,
    /// The natively fed audio this failure names: the pipeline refused it (`type=18 num=200`) or
    /// swallowed it (the stall). `None` when the failure does not name audio. It asks for the
    /// learner's confirmation ([`RecoveryEffect::ConfirmRefusal`]) and selects no rung.
    pub native_audio: Option<Facet>,
    /// Forced Direct Play (`route::forced_direct_play`): the viewer asked not to convert.
    pub forced: bool,
    /// What the pipeline was fed (`route::stream_audio`), for [`suspect`].
    pub audio: Option<StreamAudio>,
}

/// What moves the ladder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEvent {
    /// The open failed. Consumed in `Opening` and `Playing` (a failure after a picture runs the same
    /// ladder, whose `no_frames` fact then stops it at the failure read-out); a failure while a
    /// flight, a confirmation or the read-out is already outstanding is not.
    Refused { refusal: LoadRefusal, facts: OpenFacts },
    /// The answer to [`RecoveryEffect::ConfirmRefusal`]: `newly` is true when the combination was new
    /// to the install (the learner recorded it). It selects no rung; the driver words its log line
    /// with it.
    RefusalConfirmed { newly: bool },
    /// The recovery flight installed its replacement route; the driver reloads onto it.
    FlightLanded,
    /// The recovery flight could not start, or came back refused.
    FlightRefused,
    /// The first decoded picture of the open.
    FirstPicture,
    /// A Load started on this request's route (`engine::start_bufferfeed_for`, which every Engine
    /// start passes: a fresh play, a reload, the foreground restore's Load). A start the machine
    /// asked for has already moved it (`FlightLanded` -> `Opening`), so this changes nothing there;
    /// a start it did not ask for (a park and restore, or any reload, that crossed an outstanding
    /// flight or the read-out) leaves `Flight` or `Failed` with a new open to rule on.
    LoadStarted,
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
    /// Fly today's route rebuilt from [`OpenRecovery::attempt`], the facet excluded: the server
    /// converts the stream the attempt no longer feeds (`dispatch_refusal_conversion`). `facet` is
    /// the stream class taken out of the attempt and `flight` the ordinal of this flight among the
    /// request's ([`MAX_RECOVERY_FLIGHTS`]), for the driver's log line.
    ConvertFacet { facet: Facet, flight: u32 },
    /// Fly the HLS contingency a cold Auto Original retained (`dispatch_unopened_auto_hls`).
    StartAutoHls,
    /// Fail the Engine for good and show the failure read-out.
    ShowError { refusal: LoadRefusal },
}

/// Which rung a flight is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rung {
    /// The Original trial's HLS rollback.
    Rollback,
    /// The facet rung: the suspect stream converted.
    Facet,
    /// The both-facets rung: the HLS contingency, from the attempt the facet rung left.
    Both,
}

#[derive(Clone, Debug)]
enum State {
    /// The open is under way (the initial state, and the state a landed flight's reload re-enters).
    Opening,
    /// A native-audio refusal waits on the learner's answer.
    Confirming { refusal: LoadRefusal, facts: OpenFacts },
    /// A recovery flight is outstanding; `cause` is the refusal that started it and `facts` are what
    /// the next rung decides on if the flight comes back refused.
    Flight { rung: Rung, cause: LoadRefusal, facts: OpenFacts },
    /// A picture decoded in this request. The ladder is not over: a reload starts a new Engine
    /// whose own open has no picture, and the facts (`no_frames`) say which Engine a refusal is of.
    Playing,
    /// The ladder is exhausted and the read-out was asked for.
    Failed,
}

/// The ladder for one play request. See the module doc.
#[derive(Clone, Debug)]
pub struct OpenRecovery {
    /// What the app's play start read; never changed.
    start: Arc<Caps>,
    /// `start` narrowed by `excluded`: the value the route plans and profiles from. Assigned only
    /// on a rung's entry, always from `start` ([`attempt_for`]).
    attempt: Arc<Caps>,
    /// The facet the facet rung took out. One facet rung per request, so at most one; carried into
    /// the both-facets rung.
    excluded: Option<Facet>,
    /// Flights started by the facet and both-facets rungs (and a failed source's HLS contingency).
    /// The rollback is not counted.
    flights: u32,
    state: State,
}

/// `start` less `excluded`. Never takes a previous attempt as input, so the value is the same
/// whichever rungs ran.
fn attempt_for(start: &Caps, excluded: Option<&Facet>) -> Caps {
    excluded.map_or_else(|| start.clone(), |facet| start.without(facet))
}

impl OpenRecovery {
    /// A fresh request on `start`.
    pub fn new(start: Arc<Caps>) -> Self {
        Self { attempt: start.clone(), start, excluded: None, flights: 0, state: State::Opening }
    }

    /// The capability value this request plans and profiles from: `start` less the facet the facet
    /// rung took out. A cheap `Arc` clone, so a worker or a flight can own it.
    pub fn attempt(&self) -> Arc<Caps> {
        self.attempt.clone()
    }

    /// Re-derive the attempt from `start` and `excluded`; a rung's entry is the only caller.
    fn narrow(&mut self) {
        self.attempt = Arc::new(attempt_for(&self.start, self.excluded.as_ref()));
    }

    /// The rung `facts` select. The order is the proof that no rung of main is reordered: pending
    /// rollback, Forced's read-out, the facet rung, the both-facets rung, the read-out. The facet
    /// rung is for an Engine that has shown no picture (`facts.no_frames`: the picture is per
    /// Engine, so a reload after the request's first picture owes its own open the same rung).
    fn decide(&mut self, refusal: LoadRefusal, facts: &OpenFacts, fx: &mut Vec<RecoveryEffect>) {
        let flight = |rung| State::Flight { rung, cause: refusal, facts: facts.clone() };
        if facts.pending_original {
            // A refusal of native audio the learner just recorded narrows the rollback's profile
            // too: the restored HLS route must not offer it natively again.
            if let Some(facet) = facts.native_audio.as_ref().filter(|f| self.start.without(f) != *self.start) {
                self.excluded = Some(facet.clone());
                self.narrow();
            }
            fx.push(RecoveryEffect::Rollback);
            self.state = flight(Rung::Rollback);
            return;
        }
        if facts.forced {
            fx.push(RecoveryEffect::ShowError { refusal });
            self.state = State::Failed;
            return;
        }
        if refusal.names_pipeline() && facts.no_frames && self.flights == 0 {
            if let Some(facet) = suspect(facts.audio.as_ref()).filter(|f| self.start.without(f) != *self.start) {
                self.excluded = Some(facet.clone());
                self.narrow();
                self.flights += 1;
                fx.push(RecoveryEffect::ConvertFacet { facet, flight: self.flights });
                self.state = flight(Rung::Facet);
                return;
            }
        }
        if facts.auto_original && facts.no_frames && self.flights < MAX_RECOVERY_FLIGHTS {
            self.narrow();
            self.flights += 1;
            fx.push(RecoveryEffect::StartAutoHls);
            self.state = flight(Rung::Both);
            return;
        }
        fx.push(RecoveryEffect::ShowError { refusal });
        self.state = State::Failed;
    }

    /// The one writer. The table (a state not listed answers [`Handled::No`] and changes nothing):
    ///
    /// | state | event | effects, next state |
    /// |---|---|---|
    /// | `Opening` | `Refused`, facts name native audio | `ConfirmRefusal`; `Confirming` |
    /// | `Opening` | `Refused`, otherwise | the ladder below |
    /// | `Playing` | `Refused`, facts name native audio | `ConfirmRefusal`; `Confirming` |
    /// | `Playing` | `Refused`, otherwise | the ladder below (`Playing` and `Opening` rule alike: the facts say which Engine failed) |
    /// | `Confirming` | `RefusalConfirmed{newly}` | the ladder below; `newly` selects nothing |
    /// | `Flight` | `FlightLanded` | none; `Opening` (the reload re-enters the open) |
    /// | `Flight{Facet}` | `FlightRefused` | the ladder below (one flight spent, so the facet rung is closed): `StartAutoHls` or `ShowError` |
    /// | `Flight{Rollback, Both}` | `FlightRefused` | `ShowError`; `Failed` (a refused rollback stays terminal) |
    /// | `Opening` | `FirstPicture` | none; `Playing` |
    /// | `Flight`, `Failed` | `LoadStarted` | none; `Opening`, `flights` and the excluded facet kept |
    ///
    /// The ladder: `pending_original` -> `Rollback`; else `forced` -> `ShowError`; else a pipeline
    /// refusal, of an Engine with no picture (`no_frames`), of a [`suspect`] stream which narrows the
    /// attempt, with no flight spent -> `ConvertFacet`; else `auto_original && no_frames` with a
    /// flight left -> `StartAutoHls` (each: `Flight`); else `ShowError`; `Failed`.
    ///
    /// `Refused` is not handled in `Confirming`, `Flight` or `Failed`: a confirmation or a flight is
    /// outstanding (the pump runs no failure arm while a flight is), or the read-out is already up
    /// (the pump meets the same Engine's failed flags every frame until the Engine is released, and
    /// re-asserts the read-out itself). The driver treats an unhandled `Refused` as the Engine failing
    /// for good, never as silence.
    pub fn step(&mut self, ev: &RecoveryEvent, fx: &mut Vec<RecoveryEffect>) -> Handled {
        match ev {
            RecoveryEvent::Refused { refusal, facts } => {
                if !matches!(self.state, State::Opening | State::Playing) {
                    return Handled::No;
                }
                if facts.native_audio.is_some() {
                    fx.push(RecoveryEffect::ConfirmRefusal);
                    self.state = State::Confirming { refusal: *refusal, facts: facts.clone() };
                } else {
                    self.decide(*refusal, facts, fx);
                }
                Handled::Yes
            }
            RecoveryEvent::RefusalConfirmed { .. } => {
                let State::Confirming { refusal, facts } = &self.state else {
                    return Handled::No;
                };
                let (refusal, facts) = (*refusal, facts.clone());
                self.decide(refusal, &facts, fx);
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
                let State::Flight { rung, cause, facts } = &self.state else {
                    return Handled::No;
                };
                let (rung, cause, facts) = (*rung, *cause, facts.clone());
                if rung == Rung::Facet {
                    self.decide(cause, &facts, fx);
                } else {
                    fx.push(RecoveryEffect::ShowError { refusal: cause });
                    self.state = State::Failed;
                }
                Handled::Yes
            }
            RecoveryEvent::FirstPicture => {
                if !matches!(self.state, State::Opening) {
                    return Handled::No;
                }
                self.state = State::Playing;
                Handled::Yes
            }
            RecoveryEvent::LoadStarted => {
                if !matches!(self.state, State::Flight { .. } | State::Failed) {
                    return Handled::No;
                }
                // The flight (if any) is gone with the Engine that waited on it, and the read-out
                // belongs to the Engine that failed: the new Load is a new open, ruled on by the
                // same ladder with what this request has already spent (`flights`, `excluded`).
                self.state = State::Opening;
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

    fn dts6() -> Facet {
        Facet::Audio { codec: "dts".into(), channels: 6 }
    }

    fn opus6() -> Facet {
        Facet::Audio { codec: "opus".into(), channels: 6 }
    }

    fn audio(codec: &str, channels: u32) -> Option<StreamAudio> {
        Some(StreamAudio { codec: codec.into(), channels })
    }

    /// A set whose table lists DTS at six channels (the shape of `plan_track_selection_tests.rs`'
    /// `dts_row_caps`): taking `dts6()` out of it is a real narrowing.
    fn dts_start() -> Arc<Caps> {
        Arc::new(Caps {
            audio: "aac,ac3,eac3,dts".into(),
            audio_channels: [("dts".into(), 6)].into(),
            ..Caps::assumed()
        })
    }

    /// A set whose pipeline announces Opus at any channel count.
    fn opus_start() -> Arc<Caps> {
        Arc::new(Caps { audio: "aac,ac3,eac3,opus".into(), ..Caps::assumed() })
    }

    fn machine() -> OpenRecovery {
        OpenRecovery::new(dts_start())
    }

    fn facts(pending_original: bool, auto_original: bool, no_frames: bool) -> OpenFacts {
        OpenFacts { pending_original, auto_original, no_frames, ..OpenFacts::default() }
    }

    /// `f` with the DTS 5.1 track the pipeline was fed.
    fn on_dts(f: OpenFacts) -> OpenFacts {
        OpenFacts { audio: audio("dca", 6), ..f }
    }

    /// `f` with the Opus 5.1 track the pipeline was fed natively, which the refusal names.
    fn names_opus(f: OpenFacts) -> OpenFacts {
        OpenFacts { native_audio: Some(opus6()), audio: audio("opus", 6), ..f }
    }

    fn run(m: &mut OpenRecovery, ev: RecoveryEvent) -> (Handled, Vec<RecoveryEffect>) {
        let mut fx = Vec::new();
        let handled = m.step(&ev, &mut fx);
        (handled, fx)
    }

    const SHUT: LoadRefusal = LoadRefusal::Refused { num: 601 };
    const NATIVE: LoadRefusal = LoadRefusal::Refused { num: 200 };
    const ERROR: [RecoveryEffect; 1] = [RecoveryEffect::ShowError { refusal: SHUT }];

    fn refused(refusal: LoadRefusal, facts: OpenFacts) -> RecoveryEvent {
        RecoveryEvent::Refused { refusal, facts }
    }

    /// The ladder by facts, through the machine: the rung a `601` lands on, with (`suspect`) or
    /// without a DTS 5.1 track fed.
    fn rung(pending: bool, suspect: bool, auto: bool, no_frames: bool) -> Vec<RecoveryEffect> {
        let f = facts(pending, auto, no_frames);
        run(&mut machine(), refused(SHUT, if suspect { on_dts(f) } else { f })).1
    }

    #[test]
    fn suspect_names_audio_outside_aac_ac3_eac3_only() {
        let s = |codec: &str, channels| suspect(audio(codec, channels).as_ref());
        assert_eq!(s("dca", 6), Some(dts6()));
        assert_eq!(s("opus", 6), Some(opus6()));
        for known in ["aac", "ac3", "eac3"] {
            assert_eq!(s(known, 6), None, "{known}");
        }
        assert_eq!(suspect(None), None);
        assert_eq!(s("dca", 0), Some(Facet::Audio { codec: "dts".into(), channels: 0 }), "unknown width names the codec");
    }

    #[test]
    fn a_pipeline_refusal_with_an_audio_suspect_converts_that_facet_once() {
        let start = dts_start();
        assert_ne!(start.without(&dts6()), *start, "the fixture must exercise a real narrowing");
        let mut m = OpenRecovery::new(start.clone());
        let (handled, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }], Phase::Flight));
        assert_eq!(*m.attempt(), start.without(&dts6()));
    }

    #[test]
    fn an_auto_original_with_an_audio_suspect_converts_before_hls() {
        let start = dts_start();
        assert_ne!(start.without(&dts6()), *start);
        let mut m = OpenRecovery::new(start.clone());
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }], "the conversion keeps the Original route; HLS would throw it away");
        assert_eq!(*m.attempt(), start.without(&dts6()));
    }

    #[test]
    fn a_refusal_after_the_facet_flight_takes_the_both_facets_rung_then_the_read_out() {
        let start = dts_start();
        assert_ne!(start.without(&dts6()), *start);
        // Cold Auto Original: the facet, then the both-facets rung (HLS), then the read-out.
        let mut m = OpenRecovery::new(start.clone());
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        run(&mut m, RecoveryEvent::FlightLanded);
        // The converted route feeds AAC now: no suspect, so only the HLS rung is left.
        let converted = OpenFacts { audio: audio("aac", 2), ..facts(false, true, true) };
        let (_, fx) = run(&mut m, refused(SHUT, converted.clone()));
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartAutoHls], Phase::Flight));
        assert_eq!(*m.attempt(), start.without(&dts6()), "derived from start, the facet carried");
        run(&mut m, RecoveryEvent::FlightLanded);
        let (_, fx) = run(&mut m, refused(SHUT, converted));
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed));
        // Manual Original: the facet, then the read-out (the manual both-facets rung is S1b's).
        let mut m = OpenRecovery::new(start);
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, RecoveryEvent::FlightLanded);
        let (_, fx) = run(&mut m, refused(SHUT, OpenFacts { audio: audio("aac", 2), ..facts(false, false, true) }));
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed));
    }

    /// **Every reachable state of one request**, not a depth of paths: the walk runs to a fixpoint
    /// over (state, flights, excluded facet), which is everything the machine's future depends on,
    /// from every event the driver can raise (a refusal of an Engine with and without a picture, a
    /// confirmation or an answer nobody asked for, the first picture, a Load nobody asked for). On
    /// every path the flights a request starts never exceed the cap, and a facet conversion is only
    /// ever the request's first flight. `Playing` is inside the proof: the first picture is an event.
    #[test]
    fn at_most_two_flights_follow_one_request() {
        for auto in [true, false] {
            let base = OpenFacts { auto_original: auto, no_frames: true, audio: audio("dca", 6), ..OpenFacts::default() };
            let stall = OpenFacts { native_audio: Some(opus6()), ..base.clone() };
            let with_picture = OpenFacts { no_frames: false, ..base.clone() };
            let events = [
                refused(SHUT, base.clone()),
                refused(SHUT, with_picture),
                refused(LoadRefusal::TimedOut, base.clone()),
                refused(LoadRefusal::Stalled, stall),
                refused(LoadRefusal::SourceFailed, base),
                RecoveryEvent::RefusalConfirmed { newly: true },
                RecoveryEvent::FlightLanded,
                RecoveryEvent::FlightRefused,
                RecoveryEvent::FirstPicture,
                RecoveryEvent::LoadStarted,
            ];
            let key = |m: &OpenRecovery| format!("{:?}|{}|{:?}", m.state, m.flights, m.excluded);
            let mut seen = std::collections::HashSet::from([key(&machine())]);
            let mut frontier = vec![machine()];
            while let Some(m) = frontier.pop() {
                for ev in &events {
                    let mut next = m.clone();
                    let mut fx = Vec::new();
                    next.step(ev, &mut fx);
                    // The driver answers a confirmation with the learner's verdict, in the same drain.
                    let mut at = 0;
                    while at < fx.len() {
                        if fx[at] == RecoveryEffect::ConfirmRefusal {
                            next.step(&RecoveryEvent::RefusalConfirmed { newly: true }, &mut fx);
                        }
                        at += 1;
                    }
                    let started = fx.iter().filter(|e| matches!(e, RecoveryEffect::ConvertFacet { .. } | RecoveryEffect::StartAutoHls)).count();
                    if fx.iter().any(|e| matches!(e, RecoveryEffect::ConvertFacet { .. })) {
                        assert_eq!(m.flights, 0, "a facet conversion after another flight: {ev:?} in {:?}", m.state);
                    }
                    assert_eq!(next.flights as usize, m.flights as usize + started, "flights count what started: {ev:?}");
                    assert!(next.flights <= MAX_RECOVERY_FLIGHTS, "{} flights started on one request", next.flights);
                    if seen.insert(key(&next)) {
                        frontier.push(next);
                    }
                }
            }
            assert!(seen.len() > 5, "the walk reached {} states", seen.len());
        }
    }

    /// **Every state x event pair has a pinned outcome**: whether it is handled, and where it goes.
    /// An unhandled pair changes nothing and asks for nothing. (`Refused` is a DTS 5.1 refusal of an
    /// Engine with no picture on a manual Original; `RefusalConfirmed` is the learner's answer.)
    #[test]
    fn every_state_and_event_pair_has_a_pinned_outcome() {
        use Handled::{No, Yes};
        use Phase::{Confirming, Failed, Flight, Opening, Playing};
        let refusal = || refused(SHUT, on_dts(facts(false, false, true)));
        let confirmed = || RecoveryEvent::RefusalConfirmed { newly: false };
        // (state, event name, event, handled, next state)
        #[allow(clippy::type_complexity)]
        let table: Vec<(Phase, &str, RecoveryEvent, Handled, Phase)> = vec![
            (Opening, "Refused", refusal(), Yes, Flight),
            (Opening, "RefusalConfirmed", confirmed(), No, Opening),
            (Opening, "FlightLanded", RecoveryEvent::FlightLanded, No, Opening),
            (Opening, "FlightRefused", RecoveryEvent::FlightRefused, No, Opening),
            (Opening, "FirstPicture", RecoveryEvent::FirstPicture, Yes, Playing),
            (Opening, "LoadStarted", RecoveryEvent::LoadStarted, No, Opening),
            (Confirming, "Refused", refusal(), No, Confirming),
            (Confirming, "RefusalConfirmed", confirmed(), Yes, Failed),
            (Confirming, "FlightLanded", RecoveryEvent::FlightLanded, No, Confirming),
            (Confirming, "FlightRefused", RecoveryEvent::FlightRefused, No, Confirming),
            (Confirming, "FirstPicture", RecoveryEvent::FirstPicture, No, Confirming),
            (Confirming, "LoadStarted", RecoveryEvent::LoadStarted, No, Confirming),
            (Flight, "Refused", refusal(), No, Flight),
            (Flight, "RefusalConfirmed", confirmed(), No, Flight),
            (Flight, "FlightLanded", RecoveryEvent::FlightLanded, Yes, Opening),
            (Flight, "FlightRefused", RecoveryEvent::FlightRefused, Yes, Failed),
            (Flight, "FirstPicture", RecoveryEvent::FirstPicture, No, Flight),
            (Flight, "LoadStarted", RecoveryEvent::LoadStarted, Yes, Opening),
            (Playing, "Refused", refusal(), Yes, Flight),
            (Playing, "RefusalConfirmed", confirmed(), No, Playing),
            (Playing, "FlightLanded", RecoveryEvent::FlightLanded, No, Playing),
            (Playing, "FlightRefused", RecoveryEvent::FlightRefused, No, Playing),
            (Playing, "FirstPicture", RecoveryEvent::FirstPicture, No, Playing),
            (Playing, "LoadStarted", RecoveryEvent::LoadStarted, No, Playing),
            (Failed, "Refused", refusal(), No, Failed),
            (Failed, "RefusalConfirmed", confirmed(), No, Failed),
            (Failed, "FlightLanded", RecoveryEvent::FlightLanded, No, Failed),
            (Failed, "FlightRefused", RecoveryEvent::FlightRefused, No, Failed),
            (Failed, "FirstPicture", RecoveryEvent::FirstPicture, No, Failed),
            (Failed, "LoadStarted", RecoveryEvent::LoadStarted, Yes, Opening),
        ];
        assert_eq!(table.len(), 5 * 6, "every pair of five states and six events");
        for (state, name, ev, handled, next) in table {
            let mut m = in_phase(state);
            let (got, fx) = run(&mut m, ev);
            assert_eq!((got, m.phase()), (handled, next), "{state:?} x {name}");
            if handled == No {
                assert!(fx.is_empty(), "{state:?} x {name}: an unhandled event asks for {fx:?}");
            }
        }
    }

    #[test]
    fn a_refused_facet_flight_takes_the_next_rung_but_a_refused_rollback_ends() {
        let facet = |auto: bool| {
            let mut m = machine();
            run(&mut m, refused(SHUT, on_dts(facts(false, auto, true))));
            assert_eq!(m.phase(), Phase::Flight);
            run(&mut m, RecoveryEvent::FlightRefused)
        };
        assert_eq!(facet(true).1, [RecoveryEffect::StartAutoHls], "a refused conversion dispatch still has the HLS contingency");
        assert_eq!(facet(false).1, ERROR);
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(true, true, true))));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed), "a refused rollback stays terminal");
    }

    #[test]
    fn a_timeout_names_no_facet() {
        let timed_out = |auto| run(&mut machine(), refused(LoadRefusal::TimedOut, on_dts(facts(false, auto, true)))).1;
        assert_eq!(timed_out(true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(timed_out(false), [RecoveryEffect::ShowError { refusal: LoadRefusal::TimedOut }]);
    }

    #[test]
    fn forced_ends_on_the_read_out_after_rollback_only() {
        let forced = |pending, refusal| {
            run(&mut machine(), refused(refusal, OpenFacts { forced: true, ..on_dts(facts(pending, false, true)) })).1
        };
        for refusal in [SHUT, LoadRefusal::TimedOut, LoadRefusal::Stalled, LoadRefusal::SourceFailed] {
            assert_eq!(forced(true, refusal), [RecoveryEffect::Rollback], "{refusal}");
            assert_eq!(forced(false, refusal), [RecoveryEffect::ShowError { refusal }], "{refusal}");
        }
    }

    #[test]
    fn rollback_comes_first_and_is_not_counted() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(true, false, true))));
        assert_eq!(fx, [RecoveryEffect::Rollback]);
        run(&mut m, RecoveryEvent::FlightLanded);
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }], "the rollback spent no flight");
    }

    #[test]
    fn a_source_failure_keeps_mains_two_arms() {
        let failed = |m: &mut OpenRecovery, auto| {
            run(m, refused(LoadRefusal::SourceFailed, on_dts(facts(false, auto, true)))).1
        };
        assert_eq!(failed(&mut machine(), true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(failed(&mut machine(), false), [RecoveryEffect::ShowError { refusal: LoadRefusal::SourceFailed }]);
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        run(&mut m, RecoveryEvent::FlightLanded);
        assert_eq!(failed(&mut m, true), [RecoveryEffect::StartAutoHls]);
    }

    #[test]
    fn a_facet_the_attempt_already_lacks_is_no_rung() {
        let start = Arc::new(opus_start().without(&opus6()));
        assert_eq!(start.without(&opus6()), *start, "the fixture: the attempt already lacks the facet");
        let stalled = |auto| {
            let mut m = OpenRecovery::new(start.clone());
            let (_, fx) = run(&mut m, refused(LoadRefusal::Stalled, names_opus(facts(false, auto, true))));
            assert_eq!(fx, [RecoveryEffect::ConfirmRefusal]);
            run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false }).1
        };
        assert_eq!(stalled(false), [RecoveryEffect::ShowError { refusal: LoadRefusal::Stalled }]);
        assert_eq!(stalled(true), [RecoveryEffect::StartAutoHls]);
    }

    #[test]
    fn newly_does_not_pick_the_rung() {
        let start = opus_start();
        assert_ne!(start.without(&opus6()), *start);
        for newly in [true, false] {
            let mut m = OpenRecovery::new(start.clone());
            run(&mut m, refused(NATIVE, names_opus(facts(false, false, true))));
            assert_eq!(*m.attempt(), *start, "nothing is narrowed before the rung is entered");
            let (_, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly });
            assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: opus6(), flight: 1 }], "newly={newly}");
            assert_eq!(*m.attempt(), start.without(&opus6()), "newly={newly}");
        }
    }

    #[test]
    fn a_failed_original_open_prefers_rollback_then_cold_auto_before_error() {
        assert_eq!(rung(true, false, true, true), [RecoveryEffect::Rollback]);
        assert_eq!(rung(false, false, true, true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(rung(false, false, true, false), ERROR, "a post-frame failure is not a cold open");
        assert_eq!(rung(false, false, false, true), ERROR, "manual Original stays the user's fixed choice");
        // A suspect audio track converts first, on a cold Auto Original too (the full HLS fallback
        // would also convert it, but throws the route away), and on a manual Original.
        assert_eq!(rung(false, true, true, true), [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        assert_eq!(rung(false, true, false, true), [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        // The rollback of a pending HLS to Original still comes first: its HLS route is the one a
        // failed open owes the viewer, and its audio is the server's.
        assert_eq!(rung(true, true, true, true), [RecoveryEffect::Rollback]);
    }

    #[test]
    fn a_refusal_that_names_native_audio_is_confirmed_before_the_rung_is_picked() {
        let mut m = machine();
        let (handled, fx) = run(&mut m, refused(NATIVE, OpenFacts { native_audio: Some(opus6()), ..facts(false, false, true) }));
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Confirming));
        assert_eq!(fx, [RecoveryEffect::ConfirmRefusal]);
        // Nothing suspect was fed (the facts name no stream audio): the ladder for a manual
        // Original with nothing pending is the read-out.
        let (handled, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: NATIVE }]);
    }

    #[test]
    fn a_known_combination_still_takes_the_auto_originals_hls_contingency() {
        let mut m = machine();
        run(&mut m, refused(NATIVE, OpenFacts { native_audio: Some(opus6()), ..facts(false, true, true) }));
        let (_, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartAutoHls], Phase::Flight));
    }

    #[test]
    fn a_flight_ends_in_a_reload_or_in_the_read_out() {
        let mut m = machine();
        run(&mut m, refused(SHUT, facts(true, false, true)));
        assert_eq!(m.phase(), Phase::Flight);
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightLanded);
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![], Phase::Opening));

        let mut m = machine();
        run(&mut m, refused(SHUT, facts(false, true, true)));
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, ERROR, "the read-out names the refusal that started the flight");
    }

    #[test]
    fn a_failure_while_a_flight_or_the_read_out_is_outstanding_is_not_handled() {
        let mut m = machine();
        run(&mut m, refused(SHUT, facts(true, false, true)));
        assert_eq!(run(&mut m, refused(SHUT, facts(false, false, true))), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Flight);
        run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(run(&mut m, refused(SHUT, facts(true, false, true))), (Handled::No, vec![]));
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
        // A failure of an Engine that HAS a picture (`no_frames` false) runs the same ladder, whose
        // facts stop it at the read-out: neither the facet rung nor the HLS contingency applies to a
        // stream that was playing (the suspect is not the reason a playing stream failed).
        let (handled, fx) = run(&mut m, refused(LoadRefusal::SourceFailed, facts(false, true, false)));
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: LoadRefusal::SourceFailed }]);
        let mut m = machine();
        run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(run(&mut m, refused(SHUT, on_dts(facts(false, false, false)))).1, ERROR);
    }

    /// The machine lives per REQUEST but a picture is per ENGINE (`frames` and `seen_frame` are
    /// cleared by every reload): a request that showed a picture and then reloads (a track pick, a
    /// seek that escalates) owes the new Engine the same open ladder, so a refusal of THAT Engine,
    /// before ITS picture (`no_frames`), converts the suspect facet. The facet rung is gated on the
    /// refusal's own Engine having no picture, not on the request's history.
    #[test]
    fn a_reload_with_no_picture_is_pre_picture_for_its_own_engine() {
        let start = opus_start();
        assert_ne!(start.without(&opus6()), *start);
        let mut m = OpenRecovery::new(start.clone());
        run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(m.phase(), Phase::Playing);
        let reloaded = names_opus(facts(false, false, true));
        let (handled, fx) = run(&mut m, refused(NATIVE, reloaded.clone()));
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![RecoveryEffect::ConfirmRefusal], Phase::Confirming));
        let (_, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ConvertFacet { facet: opus6(), flight: 1 }], Phase::Flight));
        assert_eq!(*m.attempt(), start.without(&opus6()));
        // The same refusal of an Engine that HAS a picture converts nothing.
        let mut m = OpenRecovery::new(start);
        run(&mut m, RecoveryEvent::FirstPicture);
        run(&mut m, refused(NATIVE, names_opus(facts(false, false, false))));
        let (_, fx) = run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false });
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ShowError { refusal: NATIVE }], Phase::Failed));
    }

    #[test]
    fn a_stall_is_a_refusal_like_any_other_even_when_the_install_already_knew_the_combination() {
        // A known combination converts nothing (it is in `start` already, test above), so a manual
        // Original ends in the read-out and the viewer never waits on a spinner for ever. The rungs
        // above the read-out still apply: a pending Original trial's rollback, a cold Auto
        // Original's HLS contingency.
        let stalled = |pending, auto| {
            let mut m = OpenRecovery::new(Arc::new(opus_start().without(&opus6())));
            run(&mut m, refused(LoadRefusal::Stalled, names_opus(facts(pending, auto, true))));
            run(&mut m, RecoveryEvent::RefusalConfirmed { newly: false }).1
        };
        assert_eq!(stalled(false, false), [RecoveryEffect::ShowError { refusal: LoadRefusal::Stalled }]);
        assert_eq!(stalled(true, false), [RecoveryEffect::Rollback]);
        assert_eq!(stalled(false, true), [RecoveryEffect::StartAutoHls]);
    }

    /// A machine in each state, built through its own events: `Opening`, `Confirming`, `Flight`
    /// (the facet rung; a DTS 5.1 refused), `Playing` and `Failed` (the facet flight refused on a
    /// manual Original).
    fn in_phase(phase: Phase) -> OpenRecovery {
        let mut m = machine();
        match phase {
            Phase::Opening => {}
            Phase::Confirming => {
                run(&mut m, refused(NATIVE, OpenFacts { native_audio: Some(opus6()), ..facts(false, false, true) }));
            }
            Phase::Flight => {
                run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
            }
            Phase::Playing => {
                run(&mut m, RecoveryEvent::FirstPicture);
            }
            Phase::Failed => {
                run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
                run(&mut m, RecoveryEvent::FlightRefused);
            }
        }
        assert_eq!(m.phase(), phase, "the fixture");
        m
    }

    /// **A Load the machine did not ask for re-opens the ladder.** Home during a facet flight
    /// clears the flight (`begin_engine_teardown`), and the foreground restore Loads the same
    /// refused route: the machine was left in `Flight` (its `Refused` is `Handled::No`, the Engine
    /// spun on Connecting for good). The same restore after the read-out was up left `Failed`.
    /// `LoadStarted` moves both back to `Opening`, keeping what the request spent: its flights and
    /// the facet it took out.
    #[test]
    fn a_load_the_machine_did_not_ask_for_reopens_the_ladder_keeping_the_spent_rungs() {
        let start = dts_start();
        for phase in [Phase::Flight, Phase::Failed] {
            let mut m = in_phase(phase);
            assert_eq!(*m.attempt(), start.without(&dts6()), "{phase:?}: the facet rung narrowed the attempt");
            assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (Handled::Yes, vec![]), "{phase:?}");
            assert_eq!(m.phase(), Phase::Opening, "{phase:?}");
            assert_eq!(*m.attempt(), start.without(&dts6()), "{phase:?}: the excluded facet is kept");
            // The restored Load is refused again: the facet rung is spent (one flight), so a manual
            // Original ends on the read-out and a cold Auto Original takes its HLS contingency.
            let refused_again = |auto| refused(SHUT, on_dts(facts(false, auto, true)));
            let (handled, fx) = run(&mut m.clone(), refused_again(false));
            assert_eq!((handled, fx), (Handled::Yes, ERROR.to_vec()), "{phase:?}: no second conversion");
            let (handled, fx) = run(&mut m, refused_again(true));
            assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![RecoveryEffect::StartAutoHls], Phase::Flight), "{phase:?}");
        }
    }

    /// `LoadStarted` is a pinned outcome in every state, and only `Flight` and `Failed` move.
    #[test]
    fn load_started_changes_only_a_flight_and_the_read_out() {
        for (phase, handled, next) in [
            (Phase::Opening, Handled::No, Phase::Opening),
            (Phase::Confirming, Handled::No, Phase::Confirming),
            (Phase::Flight, Handled::Yes, Phase::Opening),
            (Phase::Playing, Handled::No, Phase::Playing),
            (Phase::Failed, Handled::Yes, Phase::Opening),
        ] {
            let mut m = in_phase(phase);
            assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (handled, vec![]), "{phase:?}");
            assert_eq!(m.phase(), next, "{phase:?}");
        }
    }

    /// The landing's own reload passes the same funnel: `FlightLanded` has moved the machine to
    /// `Opening` first, so the `LoadStarted` it raises changes nothing and the flight stays spent.
    #[test]
    fn the_flights_own_reload_changes_nothing() {
        let mut m = in_phase(Phase::Flight);
        run(&mut m, RecoveryEvent::FlightLanded);
        assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, ERROR, "one conversion per request, whichever way the second open began");
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
