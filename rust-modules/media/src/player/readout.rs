//! The player's own readout: the members only the player writes, and the last facts the feed
//! sampled, as one `Copy` value with one writer.
//!
//! They lived in the feed's process-global mailbox (`FEED_SHARED`: `pb_state`, `ended`,
//! `start_queued`, `speed_unsupported`, `rate_reload`, `play_rate_permille`), although the feed
//! never wrote any of them. They moved here (D2 v2, 1.2 and 3.3) so a reader asks the session,
//! and a test and a replay drive the same events the product does. A writer is an event; the
//! session applies it through [`PlayerReadout::step`] and nothing else can change a field, and the
//! mailbox statics are gone.
//!
//! The machine is pure: no clock, no registry, no thread. It names no feed type; the facts it
//! holds are the contract's [`FeedRaw`], so it builds whatever backends a composition installs.
//! It has no effects yet: the intents that also yield a feed command (seek, pause, audio) are
//! L3b's, and join as events beside these. The pump feeds it: the session's one ingestion
//! (`ingest_feed_facts`) takes the raw facts, and `present` derives the state and the end of
//! stream at the pump's derivation point, both addressed by attempt. `state` and `ended` are read
//! from here alone (`player::state`, `player::ended`), and a `SessionReset` at every real call of
//! the Feed's session reset retires them.
//!
//! The speed members follow their writers. `play_rate_permille` is the OFFERED rate: what the next
//! Load plays at, which `session_rate_for` lowers to 1x when this playback does not offer the
//! viewer's pick. It is not the viewer's intent, which is the session's `requested_rate_permille`
//! and survives that downgrade. `rate_reload` is the one-shot the pump takes with the seek that
//! carries a speed change (read it, then step `RateReloadTaken`). `start_queued` is the release
//! machine's answer, stated by the runners of its effects after a step
//! (`PlayerAdapter::publish_release_queue`).

use plx_playback_contract::feed::{FeedRaw, PresentationTick, Stage};
use plx_playback_contract::RATE_NORMAL;

use super::attempt_view::PlaybackState;

/// Everything the player knows about the playback on screen that the feed does not own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerReadout {
    /// The pump's derived UI state, published once a frame.
    state: PlaybackState,
    /// The pipeline drained to its end of stream and the pump has seen it.
    ended: bool,
    /// A native start is queued behind the previous object's still-running `Unload`.
    start_queued: bool,
    /// This television did not take a speed session's PCM; latched until the app restarts.
    speed_unsupported: bool,
    /// The next direct-play seek must be a full reload, because it carries a speed change.
    rate_reload: bool,
    /// The viewer's playback speed in thousandths (1000 = 1x).
    play_rate_permille: i32,
    /// The feed's raw observations from the last ingestion.
    raw: FeedRaw,
}

/// How close to the container's duration the playhead must be, after the EOS, to count as the end.
pub(super) const EOS_TAIL_NS: i64 = 1_000_000_000;

/// Whether a stream whose EOS has been pushed has ended: the pipeline said so
/// (`Shared::pipeline_eos`), or the playhead is within [`EOS_TAIL_NS`] of the duration. The
/// duration is the longest track's, so a film whose audio outlasts its video by more than the
/// tail never gets there on the arithmetic alone.
pub(super) fn stream_ended(pos_ns: i64, duration_ns: i64, pipeline_eos: bool) -> bool {
    pipeline_eos || (duration_ns > 0 && pos_ns >= duration_ns - EOS_TAIL_NS)
}

/// What one pump frame hands the derivation of the UI state and the end of stream: the physical
/// tick, the two sampled facts that have no [`FeedRaw`] field (a runtime rebuffer, the container's
/// duration), and the inputs whose writer is still the player's own mailbox (`seeking`,
/// `pipeline_eos`, the pause transport behind `paused_and_idle`; L3b moves each into the readout
/// beside the intent that writes it, and this struct loses the field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub tick: PresentationTick,
    /// The HLS runtime rebuffer, distinct from the initial Load and from seek priming.
    pub rebuffering: bool,
    /// The container's duration, 0 while unknown.
    pub duration_ns: i64,
    /// A seek is in flight: its target, not the stale frames on the panel, is what the HUD shows.
    pub seeking: bool,
    /// The viewer is paused and the pump is doing no work for a picture.
    pub paused_and_idle: bool,
    /// The pipeline reported its own end of stream.
    pub pipeline_eos: bool,
}

/// The inputs of the derivation whose writer is still the player's own mailbox: what the pump hands
/// [`super::super::route::PlaybackSession::present`] beside the post-tick sample. L3b moves each into
/// the readout beside the intent that writes it and this struct shrinks to nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentInputs {
    pub seeking: bool,
    pub paused_and_idle: bool,
    pub pipeline_eos: bool,
}

/// What can happen to the readout. Intent-named: a writer states what occurred, never a field
/// value to store, so a transition that touches several members is one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadoutEvent {
    /// The pump derived this frame's state.
    StateDerived(PlaybackState),
    /// The pipeline drained to its end of stream.
    StreamEnded,
    /// A seek was requested, so a drained stream is no longer the end.
    EndSuperseded,
    /// One session's facts are gone (its engine was torn down): back to Idle, nothing ended, no
    /// raw facts. The viewer's speed, the speed latch and the release queue outlive a session.
    SessionReset,
    /// A new item: the speed is a choice about what is playing, so it returns to 1x.
    NewItem,
    /// The release machine's queue changed: whether a start waits behind an `Unload`.
    ReleaseQueue { queued: bool },
    /// The viewer picked a speed, or the session's offer forced it to another.
    RateSelected { permille: i32 },
    /// The next direct-play seek is to carry a speed change as a full reload.
    RateReloadArmed,
    /// The pump took that one-shot reload. Read [`PlayerReadout::rate_reload`] first.
    RateReloadTaken,
    /// The television refused a speed session's PCM: speed is off for the life of the process, the
    /// viewer is back at 1x, and the reload that lands there is armed. One decision, one event.
    SpeedRefused,
    /// The feed's raw observations, sampled for the live attempt.
    FactsIngested(FeedRaw),
    /// The pump presented a frame: derive the UI state, and the end of stream, from the facts last
    /// ingested and what the pump saw. The session steps it only for the attempt that is live
    /// and only after the post-tick sample, so a frame that returned early derives nothing.
    Presented(Presentation),
}

impl PlayerReadout {
    /// No playback: what a session holds before the first play.
    pub const IDLE: Self = Self {
        state: PlaybackState::Idle,
        ended: false,
        start_queued: false,
        speed_unsupported: false,
        rate_reload: false,
        play_rate_permille: RATE_NORMAL,
        raw: FeedRaw::IDLE,
    };

    pub fn state(&self) -> PlaybackState { self.state }
    pub fn ended(&self) -> bool { self.ended }
    pub fn start_queued(&self) -> bool { self.start_queued }
    pub fn speed_unsupported(&self) -> bool { self.speed_unsupported }
    pub fn rate_reload(&self) -> bool { self.rate_reload }
    pub fn play_rate_permille(&self) -> i32 { self.play_rate_permille }
    pub fn raw(&self) -> &FeedRaw { &self.raw }

    /// The one writer. Applies `event`; there are no effects yet. The session addresses the
    /// events that carry an attempt's facts before it steps them.
    pub fn step(&mut self, event: ReadoutEvent) {
        match event {
            ReadoutEvent::StateDerived(state) => self.state = state,
            ReadoutEvent::StreamEnded => self.ended = true,
            ReadoutEvent::EndSuperseded => self.ended = false,
            ReadoutEvent::SessionReset => {
                self.state = PlaybackState::Idle;
                self.ended = false;
                self.raw = FeedRaw::IDLE;
            }
            ReadoutEvent::NewItem => {
                self.play_rate_permille = RATE_NORMAL;
                self.rate_reload = false;
            }
            ReadoutEvent::ReleaseQueue { queued } => self.start_queued = queued,
            ReadoutEvent::RateSelected { permille } => self.play_rate_permille = permille,
            ReadoutEvent::RateReloadArmed => self.rate_reload = true,
            ReadoutEvent::RateReloadTaken => self.rate_reload = false,
            ReadoutEvent::SpeedRefused => {
                self.speed_unsupported = true;
                self.play_rate_permille = RATE_NORMAL;
                self.rate_reload = true;
            }
            ReadoutEvent::FactsIngested(raw) => self.raw = raw,
            ReadoutEvent::Presented(p) => {
                // Order matters: a seek in flight outranks "we have frames", because the frames
                // on the panel are the PRE-seek ones and the HUD must show the target, not them.
                self.state = if p.seeking {
                    PlaybackState::Seeking
                } else if p.tick.stage == Stage::Loading {
                    PlaybackState::Connecting
                } else if p.rebuffering {
                    PlaybackState::Buffering
                } else if self.raw.frames == 0 {
                    // No picture yet. Work pending for one is Buffering; a paused player with
                    // nothing in flight is waiting on the viewer, which is neither a spinner nor
                    // `Playing` (no frame is presented, so `is_playing` stays false).
                    if p.paused_and_idle { PlaybackState::Paused } else { PlaybackState::Buffering }
                } else {
                    PlaybackState::Playing
                };
                if p.tick.eos_pushed && !self.ended
                    && stream_ended(self.raw.playpos_ns, p.duration_ns, p.pipeline_eos) {
                    self.ended = true;
                }
            }
        }
    }
}

impl Default for PlayerReadout {
    fn default() -> Self { Self::IDLE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::attempt_view::Backend;
    use crate::route::{PlaybackSession, RouteStartAttempt};
    use plx_playback_contract::feed::{FeedFacts, FeedLoad};
    use plx_playback_contract::feed_fake::FakeFeed;

    fn stepped(events: &[ReadoutEvent]) -> PlayerReadout {
        let mut readout = PlayerReadout::IDLE;
        for event in events { readout.step(*event); }
        readout
    }

    #[test]
    fn a_readout_starts_idle_at_normal_speed() {
        let r = PlayerReadout::IDLE;
        assert_eq!(r.state(), PlaybackState::Idle);
        assert!(!r.ended() && !r.start_queued() && !r.speed_unsupported() && !r.rate_reload());
        assert_eq!(r.play_rate_permille(), RATE_NORMAL);
        assert_eq!(*r.raw(), FeedRaw::IDLE);
    }

    #[test]
    fn the_derived_state_and_the_end_of_stream_follow_their_events() {
        let r = stepped(&[ReadoutEvent::StateDerived(PlaybackState::Playing), ReadoutEvent::StreamEnded]);
        assert_eq!(r.state(), PlaybackState::Playing);
        assert!(r.ended());
        let r = stepped(&[ReadoutEvent::StreamEnded, ReadoutEvent::EndSuperseded]);
        assert!(!r.ended(), "a seek supersedes the end");
    }

    #[test]
    fn a_session_reset_forgets_the_session_but_not_the_viewers_choices() {
        let mut facts = FeedRaw::IDLE;
        facts.frames = 90;
        facts.flags.demux_failed = true;
        let r = stepped(&[
            ReadoutEvent::RateSelected { permille: 1500 },
            ReadoutEvent::ReleaseQueue { queued: true },
            ReadoutEvent::SpeedRefused,
            ReadoutEvent::StateDerived(PlaybackState::Error),
            ReadoutEvent::StreamEnded,
            ReadoutEvent::FactsIngested(facts),
            ReadoutEvent::SessionReset,
        ]);
        assert_eq!(r.state(), PlaybackState::Idle);
        assert!(!r.ended());
        assert_eq!(*r.raw(), FeedRaw::IDLE, "one session's facts do not outlive it");
        assert!(r.speed_unsupported(), "the latch is for the life of the process");
        assert!(r.start_queued(), "the queue outlives a session reset exactly as the machine does");
        assert!(r.rate_reload(), "a pending reload survives the reset that carries it");
    }

    #[test]
    fn a_new_item_returns_the_speed_to_normal_and_drops_a_pending_reload() {
        let r = stepped(&[
            ReadoutEvent::RateSelected { permille: 1500 },
            ReadoutEvent::RateReloadArmed,
            ReadoutEvent::NewItem,
        ]);
        assert_eq!(r.play_rate_permille(), RATE_NORMAL);
        assert!(!r.rate_reload());
    }

    #[test]
    fn a_speed_change_arms_one_reload_and_the_pump_takes_it_once() {
        let mut r = stepped(&[ReadoutEvent::RateSelected { permille: 1250 }, ReadoutEvent::RateReloadArmed]);
        assert_eq!(r.play_rate_permille(), 1250);
        assert!(r.rate_reload());
        r.step(ReadoutEvent::RateReloadTaken);
        assert!(!r.rate_reload());
        assert_eq!(r.play_rate_permille(), 1250, "taking the reload keeps the speed");
    }

    #[test]
    fn refusing_the_speed_session_latches_and_returns_to_normal_in_one_event() {
        let r = stepped(&[ReadoutEvent::RateSelected { permille: 2000 }, ReadoutEvent::SpeedRefused]);
        assert!(r.speed_unsupported());
        assert_eq!(r.play_rate_permille(), RATE_NORMAL);
        assert!(r.rate_reload(), "the reload that lands at 1x is armed");
    }

    #[test]
    fn the_last_ingested_facts_replace_the_previous_ones() {
        let mut first = FeedRaw::IDLE;
        first.playpos_ns = 5;
        let mut second = FeedRaw::IDLE;
        second.frames = 3;
        let r = stepped(&[ReadoutEvent::FactsIngested(first), ReadoutEvent::FactsIngested(second)]);
        assert_eq!(*r.raw(), second, "a sample replaces, it does not merge");
    }

    #[test]
    fn the_release_queue_follows_the_machine() {
        let r = stepped(&[ReadoutEvent::ReleaseQueue { queued: true }]);
        assert!(r.start_queued());
        let r = stepped(&[ReadoutEvent::ReleaseQueue { queued: true }, ReadoutEvent::ReleaseQueue { queued: false }]);
        assert!(!r.start_queued());
    }

    // ---- the session's ingestion and the addressed `Presented` event (L3a-2 step 3) ----

    fn facts(frames: i32, duration_ns: Option<i64>) -> FeedFacts {
        let mut facts = FeedFacts::default();
        facts.raw.frames = frames;
        facts.raw.playpos_ns = 7;
        facts.attempt.duration_ns = duration_ns;
        facts
    }

    fn tick(stage: Stage, eos_pushed: bool) -> PresentationTick {
        PresentationTick { stage, eos_pushed }
    }

    const QUIET: PresentInputs =
        PresentInputs { seeking: false, paused_and_idle: false, pipeline_eos: false };

    /// One sample splits between the attempt view and the readout, and only the attempt the view
    /// belongs to can deliver it: a late worker of a previous attempt changes neither half.
    #[test]
    fn a_feed_sample_reaches_both_halves_addressed_by_its_attempt() {
        let _serial = plx_base::testlock::serial();
        let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_feed_facts(a, facts(5, Some(100))));
        assert_eq!(ps.readout().raw().frames, 5, "the raw half reaches the readout");
        assert_eq!(ps.attempt_view().facts().duration_ns, Some(100), "the mapped half reaches the view");

        ps.begin_attempt_view(b, Backend::Feed);
        assert!(!ps.ingest_feed_facts(a, facts(9, Some(900))), "a stale attempt is refused");
        assert_eq!(ps.readout().raw().frames, 5, "its raw facts did not reach the readout");
        assert_eq!(ps.attempt_view().facts().duration_ns, None, "nor the view");
        assert!(ps.ingest_feed_facts(b, facts(9, Some(900))));
        assert_eq!(ps.readout().raw().frames, 9);
    }

    /// The Feed's sample is the Feed's: an attempt the view began for the other backend refuses it.
    #[test]
    fn a_feed_sample_is_refused_for_an_attempt_of_the_other_backend() {
        let _serial = plx_base::testlock::serial();
        let c = RouteStartAttempt::fixture_n(3);
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(c, Backend::Uri);
        assert!(!ps.ingest_feed_facts(c, facts(5, None)));
        assert_eq!(*ps.readout().raw(), FeedRaw::IDLE);
        assert!(!ps.present(c, tick(Stage::Playing, false), facts(5, None), QUIET));
        assert_eq!(ps.readout().state(), PlaybackState::Idle);
    }

    /// `Presented` is addressed like the ingestion: a frame the pump presented for an attempt that
    /// has since been replaced derives nothing and ingests nothing.
    #[test]
    fn a_presented_frame_for_a_replaced_attempt_changes_nothing() {
        let _serial = plx_base::testlock::serial();
        let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        ps.begin_attempt_view(b, Backend::Feed);
        assert!(!ps.present(a, tick(Stage::Streaming, true), facts(30, Some(10)), QUIET));
        assert_eq!(ps.readout().state(), PlaybackState::Idle);
        assert!(!ps.readout().ended());
        assert_eq!(*ps.readout().raw(), FeedRaw::IDLE);
        assert!(ps.present(b, tick(Stage::Streaming, false), facts(30, Some(10)), QUIET));
        assert_eq!(ps.readout().state(), PlaybackState::Playing);
        assert_eq!(ps.readout().raw().frames, 30, "the post-tick sample is the readout's raw facts");
    }

    /// A test scripts a `FakeFeed`'s sample and delivers it through the real ingestion: the helper
    /// writes no readout field.
    #[test]
    fn a_scripted_fake_feed_sample_is_delivered_through_the_real_ingestion() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let (fake, handle) = FakeFeed::new();
        handle.script(|s| s.facts = facts(12, Some(4)));
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert!(ps.ingest_feed_facts(a, fake.sample()));
        assert_eq!(ps.readout().raw().frames, 12);
        assert_eq!(handle.calls(), vec![plx_playback_contract::feed_fake::FeedCall::Sample]);
    }

    fn presented(raw_frames: i32, p: Presentation) -> PlayerReadout {
        let mut raw = FeedRaw::IDLE;
        raw.frames = raw_frames;
        raw.playpos_ns = 99_500_000_000;
        stepped(&[ReadoutEvent::FactsIngested(raw), ReadoutEvent::Presented(p)])
    }

    fn pres(stage: Stage) -> Presentation {
        Presentation {
            tick: tick(stage, false), rebuffering: false, duration_ns: 100_000_000_000,
            seeking: false, paused_and_idle: false, pipeline_eos: false,
        }
    }

    /// The ladder's order: a seek outranks everything, then a Load still opening, then a rebuffer,
    /// then "no picture yet" (Paused when idle, else Buffering), else Playing.
    #[test]
    fn the_presented_frame_derives_the_state_in_the_ladders_order() {
        assert_eq!(presented(30, Presentation { seeking: true, ..pres(Stage::Loading) }).state(),
            PlaybackState::Seeking, "a seek outranks the frames on the panel");
        assert_eq!(presented(30, pres(Stage::Loading)).state(), PlaybackState::Connecting);
        assert_eq!(presented(30, Presentation { rebuffering: true, ..pres(Stage::Streaming) }).state(),
            PlaybackState::Buffering);
        assert_eq!(presented(0, pres(Stage::Streaming)).state(), PlaybackState::Buffering,
            "no picture yet, work pending");
        assert_eq!(presented(0, Presentation { paused_and_idle: true, ..pres(Stage::Streaming) }).state(),
            PlaybackState::Paused, "no picture, nothing in flight: waiting on the viewer");
        assert_eq!(presented(30, Presentation { paused_and_idle: true, ..pres(Stage::Streaming) }).state(),
            PlaybackState::Playing, "paused-and-idle only matters before the first picture");
        assert_eq!(presented(30, pres(Stage::Streaming)).state(), PlaybackState::Playing);
    }

    /// The end of stream needs the EOS pushed, then the pipeline's own word or a playhead inside the
    /// tail of a known duration; it is latched until a seek supersedes it.
    #[test]
    fn the_presented_frame_ends_the_stream_only_after_the_eos_and_the_tail() {
        let pushed = |p: Presentation| Presentation { tick: tick(Stage::Streaming, true), ..p };
        assert!(!presented(30, pres(Stage::Streaming)).ended(), "the EOS is not pushed yet");
        assert!(presented(30, pushed(pres(Stage::Streaming))).ended(), "within the tail of the duration");
        assert!(!presented(30, pushed(Presentation { duration_ns: 0, ..pres(Stage::Streaming) })).ended(),
            "no duration, no arithmetic");
        assert!(presented(30, pushed(Presentation { duration_ns: 0, pipeline_eos: true, ..pres(Stage::Streaming) })).ended(),
            "the pipeline said so");
        let mut r = presented(30, pushed(pres(Stage::Streaming)));
        r.step(ReadoutEvent::Presented(pres(Stage::Streaming)));
        assert!(r.ended(), "a later frame without the EOS does not un-end it");
        r.step(ReadoutEvent::EndSuperseded);
        assert!(!r.ended());
    }

    // ---- the readout is the READER of `state` and `ended` (L3a-2 step 4a) ----

    /// What `player::state` and `player::ended` say follows the readout: a presented end of stream
    /// is what the app's end-of-film rule polls, and a real stop's `SessionReset` clears it, so a
    /// replay does not find the finished play's end.
    #[test]
    fn the_readers_follow_the_readout_after_a_presented_eos_and_a_real_stop_clears_it() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Feed);
        assert_eq!(crate::player::ended(&ps), Some(false), "a live feed attempt has not ended");
        assert!(ps.present(a, tick(Stage::Streaming, true), facts(30, Some(100)), QUIET));
        assert_eq!(crate::player::state(&ps), PlaybackState::Playing, "the reader is the readout's state");
        assert_eq!(crate::player::ended(&ps), Some(true), "the reader is the readout's end of stream");
        ps.step_readout(ReadoutEvent::SessionReset);
        assert_eq!(crate::player::ended(&ps), Some(false), "a real stop retires the end");
        assert_eq!(crate::player::state(&ps), PlaybackState::Idle);
    }

    /// The URI backend keeps its own fact: its end of stream is an `AttemptFacts` value its driver
    /// writes, and the Feed's readout does not speak for it.
    #[test]
    fn the_uri_backends_end_of_stream_is_still_its_own_fact() {
        let _serial = plx_base::testlock::serial();
        let a = RouteStartAttempt::fixture();
        let mut ps = PlaybackSession::IDLE;
        ps.begin_attempt_view(a, Backend::Uri);
        let mut f = *ps.attempt_view().facts();
        f.ended = Some(true);
        assert!(ps.ingest_attempt_facts(a, Backend::Uri, f));
        assert_eq!(crate::player::ended(&ps), Some(true));
        ps.step_readout(ReadoutEvent::SessionReset);
        assert_eq!(crate::player::ended(&ps), Some(true), "a Feed reset leaves a URI attempt's fact alone");
    }
}
