//! Persistent physical feed driver behind the common attempt runner. The route owner stages
//! one prepared native source under its canonical attempt and a private source key.

use crate::feed_resources::FeedResources;
use crate::native_executor::{launch_native_open, play_initial_now, NativePrepared};
use crate::shared::{Stage, FEED_TX};
use plx_base::task::MainThread;
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, ContentTime, DispatchResult,
    DriverEvent, EvidenceSource, ExitDisposition, Failure, Observation, Observed, Operation,
    Playback, ReleaseStatus, SourceKey, StopReason,
};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering::Acquire;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedTransportAction {
    /// Release the captured viewer hold before the initial physical prime may Play.
    ResumeInitial { attempt: AttemptId, command: CommandId },
    Play { attempt: AttemptId, command: CommandId },
    Pause { attempt: AttemptId, command: CommandId },
}

pub struct FeedDriver {
    staged: HashMap<(AttemptId, SourceKey), NativePrepared>,
    active: Option<AttemptId>,
    ready_emitted: bool,
    startup_acceptance_emitted: bool,
    startup_play_command: Option<CommandId>,
    deferred_transport_play: Option<(AttemptId, CommandId)>,
    last_position_tick: u32,
    stop_requested: Option<(AttemptId, StopReason)>,
    transport_actions: VecDeque<FeedTransportAction>,
    events: VecDeque<BackendEvent>,
}

impl FeedDriver {
    pub fn new() -> Self {
        Self {
            staged: HashMap::new(), active: None, ready_emitted: false,
            startup_acceptance_emitted: false, startup_play_command: None,
            deferred_transport_play: None,
            last_position_tick: 0, stop_requested: None,
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
                        self.active = Some(attempt);
                        self.ready_emitted = false;
                        self.startup_acceptance_emitted = false;
                        self.startup_play_command = None;
                        self.deferred_transport_play = None;
                        self.last_position_tick = 0;
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
            Operation::Stop(reason) if self.active == Some(attempt) => {
                // A transport request queued earlier on this frame must not run after Stop.
                self.transport_actions.clear();
                self.deferred_transport_play = None;
                self.stop_requested = Some((attempt, reason));
                DispatchResult::Queued
            }
            Operation::Seek(_) | Operation::SelectTrack { .. } =>
                DispatchResult::Refused(Failure::Unsupported),
            _ => DispatchResult::Refused(Failure::Stale),
        }
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

    pub fn take_stop_request(&mut self) -> Option<(AttemptId, StopReason)> {
        self.stop_requested.take()
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
            self.stop_requested = None;
            self.startup_play_command = None;
        }
    }

    pub fn discard_staged(&mut self, attempt: AttemptId, key: SourceKey) {
        self.staged.remove(&(attempt, key));
    }

    pub fn exit_disposition(&self, feed: &FeedResources) -> ExitDisposition {
        if self.active.is_none() && self.staged.is_empty() && self.stop_requested.is_none()
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
