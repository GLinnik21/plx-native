//! The player's own readout: the members only the player writes, and the last facts the feed
//! sampled, as one `Copy` value with one writer.
//!
//! Today these live in the feed's process-global mailbox (`FEED_SHARED`: `pb_state`, `ended`,
//! `start_queued`, `speed_unsupported`, `rate_reload`, `play_rate_permille`), although the feed
//! never writes any of them. They move here (D2 v2, 1.2 and 3.3) so a reader asks the session,
//! and a test and a replay drive the same events the product does. A writer is an event; the
//! session applies it through [`PlayerReadout::step`] and nothing else can change a field.
//!
//! The machine is pure: no clock, no registry, no thread. It names no feed type; the facts it
//! holds are the contract's [`FeedRaw`], so it builds whatever backends a composition installs.
//! It has no effects yet: the intents that also yield a feed command (seek, pause, audio) are
//! L3b's, and join as events beside these. Not yet fed by the pump (L3a-2 steps 3 and 4), so no
//! production reader exists; the type is public like [`super::attempt_metrics`] so that is not dead
//! code.

use plx_playback_contract::feed::FeedRaw;
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

    /// The one writer. Applies `event`; there are no effects yet.
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
        }
    }
}

impl Default for PlayerReadout {
    fn default() -> Self { Self::IDLE }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
