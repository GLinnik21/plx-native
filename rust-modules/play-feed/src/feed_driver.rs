//! Persistent physical feed driver behind the common attempt runner. The route owner stages
//! one prepared native source under its canonical attempt and a private source key.

use crate::feed_pump::picture_on_panel;
use crate::feed_resources::FeedResources;
use crate::native_executor::{launch_native_open, play_initial_now, NativePrepared};
use crate::shared::{Stage, FEED_TX};
use plx_base::task::MainThread;
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, ContentTime, DispatchResult,
    DriverEvent, EvidenceSource, ExitDisposition, Failure, Observation, Observed, Operation,
    Playback, ReleaseStatus, SourceKey,
};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering::Acquire;

pub use plx_playback_contract::feed::FeedTransportAction;

pub struct FeedDriver {
    staged: HashMap<(AttemptId, SourceKey), NativePrepared>,
    active: Option<AttemptId>,
    ready_emitted: bool,
    /// `Observation::Presented` was published for the active attempt (once per attempt).
    presented_emitted: bool,
    startup_acceptance_emitted: bool,
    startup_play_command: Option<CommandId>,
    deferred_transport_play: Option<(AttemptId, CommandId)>,
    last_position_tick: u32,
    transport_actions: VecDeque<FeedTransportAction>,
    events: VecDeque<BackendEvent>,
}

impl FeedDriver {
    pub fn new() -> Self {
        Self {
            staged: HashMap::new(), active: None, ready_emitted: false,
            presented_emitted: false,
            startup_acceptance_emitted: false, startup_play_command: None,
            deferred_transport_play: None,
            last_position_tick: 0,
            transport_actions: VecDeque::new(), events: VecDeque::new(),
        }
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            progressive_uri: false, hls_uri: false, packet_feed: true,
            pause: true, seek: false, audio_track: false, subtitle_track: false,
            audio_off: false, subtitle_off: false,
        }
    }

    pub fn stage(&mut self, attempt: AttemptId, key: SourceKey,
        prepared: NativePrepared) -> Result<(), Failure> {
        if self.staged.contains_key(&(attempt, key)) { return Err(Failure::Busy); }
        self.staged.insert((attempt, key), prepared);
        Ok(())
    }

    pub fn dispatch(&mut self, feed: &mut FeedResources, mt: &MainThread,
        attempt: AttemptId, command: CommandId, operation: Operation) -> DispatchResult {
        match operation {
            Operation::Start { source, .. } => {
                let Some(prepared) = self.staged.remove(&(attempt, source)) else {
                    self.release_refused_start(attempt);
                    return DispatchResult::Refused(Failure::Stale);
                };
                if self.active.is_some() || feed.is_live() {
                    self.release_refused_start(attempt);
                    return DispatchResult::Refused(Failure::Busy);
                }
                let hooks = feed.hooks();
                match launch_native_open(
                    feed, &FEED_TX, attempt, prepared, &hooks.launch) {
                    Ok(()) => {
                        self.arm_attempt(attempt);
                        DispatchResult::Queued
                    }
                    Err(_) => {
                        // The native start guard retired its own epoch on the failed launch.
                        self.release_refused_start(attempt);
                        DispatchResult::Refused(Failure::Refused)
                    }
                }
            }
            Operation::Play if self.active == Some(attempt) => {
                self.deferred_transport_play = None;
                let hooks = feed.hooks();
                let Some(eng) = feed.engine() else { return DispatchResult::Refused(Failure::Stale); };
                if !eng.startup_controlled { return DispatchResult::Refused(Failure::Stale); }
                if !eng.startup_play_commanded {
                    eng.startup_play_commanded = true;
                    self.startup_play_command = Some(command);
                    if eng.startup_user_held {
                        self.transport_actions.push_back(FeedTransportAction::ResumeInitial {
                            attempt, command });
                    } else if eng.startup_immediate_play {
                        if play_initial_now(mt, eng, &hooks.feed) == Some(false) {
                            eng.startup_play_commanded = false;
                            self.startup_play_command = None;
                            return DispatchResult::Refused(Failure::Refused);
                        }
                    }
                } else if !eng.prime_play {
                    self.transport_actions.push_back(FeedTransportAction::Play { attempt, command });
                }
                DispatchResult::Queued
            }
            Operation::Pause if self.active == Some(attempt) => {
                self.deferred_transport_play = None;
                self.transport_actions.push_back(FeedTransportAction::Pause { attempt, command });
                DispatchResult::Queued
            }
            Operation::Stop(_) if self.active == Some(attempt) => {
                // A transport request queued earlier on this frame must not run after Stop.
                self.transport_actions.clear();
                self.deferred_transport_play = None;
                DispatchResult::Queued
            }
            Operation::Seek(_) | Operation::SelectTrack { .. } =>
                DispatchResult::Refused(Failure::Unsupported),
            _ => DispatchResult::Refused(Failure::Stale),
        }
    }

    /// A native open is under way for `attempt`: everything the previous attempt latched is
    /// forgotten, so each attempt publishes its own ready and picture exactly once.
    fn arm_attempt(&mut self, attempt: AttemptId) {
        self.active = Some(attempt);
        self.ready_emitted = false;
        self.presented_emitted = false;
        self.startup_acceptance_emitted = false;
        self.startup_play_command = None;
        self.deferred_transport_play = None;
        self.last_position_tick = 0;
    }

    /// Publish `Observation::Presented` once per attempt, when [`picture_on_panel`] holds for the
    /// facts of its Engine. The evidence is necessary-condition (plane bound, sink counted
    /// displayed frames), never a position tick and never photon proof.
    fn note_picture(&mut self, attempt: AttemptId, shared: &crate::shared::Shared,
        acb_plane: bool, stage: Stage, bound_at_frames: Option<i32>) {
        if self.presented_emitted || !picture_on_panel(shared, acb_plane, stage, bound_at_frames) {
            return;
        }
        self.presented_emitted = true;
        // The one line that says a device run reached the rule; the evidence lines before it are
        // `setMediaVideoData ... window+PLAYING` (the bind) and `sink: displayed=N` (the counter).
        plx_base::eventlog::log(&format!(
            "feed: presented (plane shows the picture; displayed-frame callbacks so far: {})",
            shared.dg_sink_displayed_cb.load(std::sync::atomic::Ordering::Relaxed)));
        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::Observation { attempt, observation: Observation::Presented } });
    }

    pub fn note_load_outcome(&mut self, attempt: AttemptId, ok: bool) {
        if self.active != Some(attempt) { return; }
        if !ok {
            self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                event: DriverEvent::Observation { attempt,
                    observation: Observation::Failed(Failure::Backend) } });
        }
    }

    fn release_refused_start(&mut self, attempt: AttemptId) {
        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::Release { attempt, status: ReleaseStatus::Released } });
    }

    pub fn poll(&mut self, feed: &mut FeedResources) -> Vec<BackendEvent> {
        if let Some((attempt, status)) = feed.take_reaped_release() {
            self.note_release(attempt, status);
        }
        let hooks = feed.hooks();
        if let Some(attempt) = self.active {
            if let Some(eng) = feed.engine().filter(|eng| eng.attempt == attempt) {
                if eng.startup_controlled && eng.stage >= Stage::Playing
                    && !eng.preview_abandon && !self.ready_emitted {
                    self.ready_emitted = true;
                    self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                        event: DriverEvent::Observation { attempt,
                            observation: Observation::Playback(Observed {
                                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport,
                            }) } });
                }
                if !eng.preview_abandon {
                    self.note_picture(attempt, eng.shared, hooks.release.plane.handle(),
                        eng.stage, eng.bound_at_frames);
                }
                if eng.startup_controlled && eng.startup_play_commanded && !eng.prime_play
                    && !self.startup_acceptance_emitted {
                    self.startup_acceptance_emitted = true;
                    eng.startup_first_play_done = true;
                    if let Some(command) = self.startup_play_command.take() {
                        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                            event: DriverEvent::ControlAccepted { attempt, command,
                                operation: Operation::Play, native_acknowledged: true } });
                    }
                }
                if eng.startup_play_refused {
                    eng.startup_play_refused = false;
                    if let Some(command) = self.startup_play_command.take() {
                        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                            event: DriverEvent::ControlRefused { attempt, command,
                                operation: Operation::Play, failure: Failure::Refused } });
                    }
                }
                if !eng.prime_play {
                    if let Some((pending_attempt, command)) = self.deferred_transport_play.take() {
                        if pending_attempt == attempt {
                            self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                                event: DriverEvent::ControlAccepted { attempt, command,
                                    operation: Operation::Play, native_acknowledged: true } });
                        }
                    }
                }
                // The callback's type=0 position report is a real native clock sample, not a
                // presented-frame proof. Its release-published sequence excludes seek/HUD writes.
                let tick = eng.shared.dg_pos_ticks.load(Acquire);
                if tick != 0 && tick != self.last_position_tick {
                    self.last_position_tick = tick;
                    let ns = eng.shared.reported_playpos_ns.load(Acquire).max(0) as u64;
                    self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                        event: DriverEvent::Observation { attempt,
                            observation: Observation::Position(Observed {
                                value: ContentTime::from_millis(ns / 1_000_000),
                                from: EvidenceSource::BackendReport,
                            }) } });
                }
            }
        }
        self.events.drain(..).collect()
    }

    pub fn take_transport_action(&mut self) -> Option<FeedTransportAction> {
        self.transport_actions.pop_front()
    }

    pub fn note_transport_result(&mut self, feed: &mut FeedResources,
        mt: &MainThread, action: FeedTransportAction, accepted: bool,
        native_acknowledged: bool) {
        if let FeedTransportAction::ResumeInitial { attempt, command } = action {
            if self.active != Some(attempt) { return; }
            let hooks = feed.hooks();
            let Some(eng) = feed.engine().filter(|eng| eng.attempt == attempt) else { return };
            if !accepted {
                eng.startup_play_commanded = false;
                self.startup_play_command = None;
                self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                    event: DriverEvent::ControlRefused { attempt, command,
                        operation: Operation::Play, failure: Failure::Refused } });
                return;
            }
            eng.startup_user_held = false;
            if eng.startup_immediate_play && play_initial_now(mt, eng, &hooks.feed) == Some(false) {
                eng.startup_play_commanded = false;
                self.startup_play_command = None;
                self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                    event: DriverEvent::ControlRefused { attempt, command,
                        operation: Operation::Play, failure: Failure::Refused } });
            }
            return;
        }
        let (attempt, command, operation) = match action {
            FeedTransportAction::ResumeInitial { .. } => unreachable!("handled above"),
            FeedTransportAction::Play { attempt, command } =>
                (attempt, command, Operation::Play),
            FeedTransportAction::Pause { attempt, command } =>
                (attempt, command, Operation::Pause),
        };
        if self.active != Some(attempt) { return; }
        if !accepted {
            self.deferred_transport_play = None;
            self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                event: DriverEvent::ControlRefused { attempt, command, operation,
                    failure: Failure::Refused } });
            return;
        }
        if operation == Operation::Play && !native_acknowledged
            && feed.engine().is_some_and(|eng| eng.attempt == attempt && eng.prime_play) {
            self.deferred_transport_play = Some((attempt, command));
        }
        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::ControlAccepted { attempt, command, operation,
                native_acknowledged } });
    }

    pub fn note_release(&mut self, attempt: AttemptId, status: ReleaseStatus) {
        if self.active != Some(attempt) { return; }
        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::Release { attempt, status } });
        self.transport_actions.clear();
        self.deferred_transport_play = None;
        if status == ReleaseStatus::Released {
            self.active = None;
            self.startup_play_command = None;
        }
    }

    pub fn discard_staged(&mut self, attempt: AttemptId, key: SourceKey) {
        self.staged.remove(&(attempt, key));
    }

    pub fn exit_disposition(&self, feed: &FeedResources) -> ExitDisposition {
        if self.active.is_none() && self.staged.is_empty()
            && !feed.is_live() && !feed.has_pending_release() && !feed.has_abandoned_load()
            && !feed.has_reaped_release() {
            ExitDisposition::Clean
        } else {
            ExitDisposition::PreserveNativeOwnership
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf_event::note_sink_counter;
    use crate::shared::Shared;
    use std::sync::atomic::Ordering::Relaxed;

    const A: AttemptId = AttemptId::new(7, 1);
    const B: AttemptId = AttemptId::new(7, 2);
    /// The numbering of the sets the displayed counter was measured on, and of webOS 5+.
    const WEBOS4: u32 = 4;
    const WEBOS10: u32 = 10;

    fn presented(driver: &FeedDriver) -> usize {
        driver.events.iter().filter(|event| matches!(event.event, DriverEvent::Observation {
            observation: Observation::Presented, .. })).count()
    }
    /// One `type=0` position tick: all `sf_on_event_inner` does to `frames`.
    fn position_tick(shared: &Shared) { shared.frames.fetch_add(1, Relaxed); }
    /// What the pump does when the ACB bind completes.
    fn bind(shared: &Shared) -> Option<i32> {
        let at = shared.frames.load(Relaxed);
        shared.forget_displayed_before_bind();
        Some(at)
    }
    /// An ACB-route driver for attempt A (webOS 4: the handle exists, so the bind is awaited).
    fn acb_driver() -> FeedDriver {
        let mut driver = FeedDriver::new();
        driver.arm_attempt(A);
        driver
    }

    #[test]
    fn acb_a_position_tick_never_presents_but_a_counted_frame_after_the_bind_does() {
        let (shared, mut driver) = (Shared::new(), acb_driver());
        position_tick(&shared);
        driver.note_picture(A, &shared, true, Stage::Playing, None);
        assert_eq!(presented(&driver), 0, "a tick before the plane bind is not a picture");
        let bound = bind(&shared);
        position_tick(&shared);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 0, "a tick after the bind is a clock sample, not a frame");
        note_sink_counter(&shared, 47, 24, WEBOS4);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 1, "the counted frame presents, once");
    }

    #[test]
    fn acb_a_zero_count_is_not_a_displayed_frame() {
        let (shared, mut driver) = (Shared::new(), acb_driver());
        let bound = bind(&shared);
        position_tick(&shared);
        note_sink_counter(&shared, 47, 0, WEBOS4);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 0, "the sink reports 0 until it has shown something");
        note_sink_counter(&shared, 47, 12, WEBOS4);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 1);
    }

    #[test]
    fn acb_a_count_taken_before_the_bind_waits_for_the_next_one_after_it() {
        let (shared, mut driver) = (Shared::new(), acb_driver());
        note_sink_counter(&shared, 47, 24, WEBOS4);
        driver.note_picture(A, &shared, true, Stage::Playing, None);
        let bound = bind(&shared);
        position_tick(&shared);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 0, "the frame counted before the bind was not on the panel");
        note_sink_counter(&shared, 47, 48, WEBOS4);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 1);
    }

    #[test]
    fn acb_a_counted_frame_waits_for_the_tick_that_follows_the_bind() {
        let (shared, mut driver) = (Shared::new(), acb_driver());
        let bound = bind(&shared);
        note_sink_counter(&shared, 47, 24, WEBOS4);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 0, "`plane_shows_the_picture` still owes its tick");
        position_tick(&shared);
        driver.note_picture(A, &shared, true, Stage::Streaming, bound);
        assert_eq!(presented(&driver), 1);
    }

    #[test]
    fn without_a_bind_only_the_displayed_counter_of_the_new_numbering_presents() {
        let (shared, mut driver) = (Shared::new(), acb_driver());
        position_tick(&shared);
        note_sink_counter(&shared, 47, 30, WEBOS10); // 47 is something else on webOS 5+
        note_sink_counter(&shared, 48, 30, WEBOS10); // the dropped-frame counter
        note_sink_counter(&shared, 49, 0, WEBOS10);
        driver.note_picture(A, &shared, false, Stage::Playing, None);
        assert_eq!(presented(&driver), 0);
        note_sink_counter(&shared, 49, 6, WEBOS10);
        driver.note_picture(A, &shared, false, Stage::Playing, None);
        assert_eq!(presented(&driver), 1, "no bind to wait for: the first positive count presents");
    }

    #[test]
    fn a_new_attempt_and_a_session_reset_forget_the_previous_pictures_evidence() {
        let (shared, mut driver) = (Shared::new(), acb_driver());
        note_sink_counter(&shared, 49, 6, WEBOS10);
        driver.note_picture(A, &shared, false, Stage::Playing, None);
        assert_eq!(presented(&driver), 1);
        driver.events.clear();
        driver.arm_attempt(B);
        shared.reset_session();
        driver.note_picture(B, &shared, false, Stage::Playing, None);
        assert_eq!(presented(&driver), 0, "the counter of attempt A is not attempt B's picture");
        note_sink_counter(&shared, 49, 2, WEBOS10);
        driver.note_picture(B, &shared, false, Stage::Playing, None);
        assert_eq!(driver.events, [BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::Observation { attempt: B, observation: Observation::Presented } }]);
    }

    #[test]
    fn stale_release_cannot_change_the_current_attempt() {
        let current = AttemptId::new(7, 1);
        let old = AttemptId::new(6, 1);
        let mut driver = FeedDriver::new();
        driver.active = Some(current);
        driver.note_release(old, ReleaseStatus::Released);
        assert_eq!(driver.active, Some(current));
        assert!(driver.events.is_empty());

        driver.note_release(current, ReleaseStatus::Closing);
        assert_eq!(driver.active, Some(current));
        driver.note_release(current, ReleaseStatus::Released);
        assert_eq!(driver.active, None);
        assert_eq!(driver.events.len(), 2);
    }
}
