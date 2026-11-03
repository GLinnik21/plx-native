//! Persistent physical feed driver behind the common attempt runner. The route owner stages
//! one prepared native source under its canonical attempt and a private source key.

use super::{engine, feed_resources::FeedResources};
use plx_base::task::MainThread;
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, CommandId, DispatchResult,
    DriverEvent, EvidenceSource, ExitDisposition, Failure, Observation, Observed, Operation,
    Playback, ReleaseStatus, SourceKey, StopReason,
};
use std::collections::{HashMap, VecDeque};
#[cfg(feature = "play-feed")]
use plx_playback_contract::Capabilities;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FeedTransportAction {
    Play { attempt: AttemptId, command: CommandId },
    Pause { attempt: AttemptId, command: CommandId },
}

pub(super) struct FeedDriver {
    staged: HashMap<(AttemptId, SourceKey), engine::NativePrepared>,
    active: Option<AttemptId>,
    ready_emitted: bool,
    playing_emitted: bool,
    startup_play_command: Option<CommandId>,
    stop_requested: Option<(AttemptId, StopReason)>,
    transport_actions: VecDeque<FeedTransportAction>,
    events: VecDeque<BackendEvent>,
}

impl FeedDriver {
    pub(super) fn new() -> Self {
        Self {
            staged: HashMap::new(), active: None, ready_emitted: false,
            playing_emitted: false, startup_play_command: None, stop_requested: None,
            transport_actions: VecDeque::new(), events: VecDeque::new(),
        }
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn capabilities(&self) -> Capabilities {
        Capabilities {
            progressive_uri: false, hls_uri: false, packet_feed: true,
            pause: true, seek: false, audio_track: false, subtitle_track: false,
            audio_off: false, subtitle_off: false,
        }
    }

    pub(super) fn stage(&mut self, attempt: AttemptId, key: SourceKey,
        prepared: engine::NativePrepared) -> Result<(), Failure> {
        if self.staged.contains_key(&(attempt, key)) { return Err(Failure::Busy); }
        self.staged.insert((attempt, key), prepared);
        Ok(())
    }

    pub(super) fn dispatch(&mut self, feed: &mut FeedResources, mt: &MainThread,
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
                match engine::launch_native_open(feed, attempt, prepared) {
                    Ok(()) => {
                        self.active = Some(attempt);
                        self.ready_emitted = false;
                        self.playing_emitted = false;
                        self.startup_play_command = None;
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
                let Some(eng) = feed.engine() else { return DispatchResult::Refused(Failure::Stale); };
                if !eng.startup_controlled { return DispatchResult::Refused(Failure::Stale); }
                if !eng.startup_play_commanded {
                    eng.startup_play_commanded = true;
                    self.startup_play_command = Some(command);
                    if eng.startup_immediate_play {
                        if engine::play_initial_now(mt, eng) == Some(false) {
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
                self.transport_actions.push_back(FeedTransportAction::Pause { attempt, command });
                DispatchResult::Queued
            }
            Operation::Stop(reason) if self.active == Some(attempt) => {
                self.stop_requested = Some((attempt, reason));
                DispatchResult::Queued
            }
            Operation::Seek(_) | Operation::SelectTrack { .. } =>
                DispatchResult::Refused(Failure::Unsupported),
            _ => DispatchResult::Refused(Failure::Stale),
        }
    }

    pub(super) fn note_load_outcome(&mut self, attempt: AttemptId, ok: bool) {
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

    pub(super) fn poll(&mut self, feed: &mut FeedResources) -> Vec<BackendEvent> {
        if let Some((attempt, status)) = feed.take_reaped_release() {
            self.note_release(attempt, status);
        }
        if let Some(attempt) = self.active {
            if let Some(eng) = feed.engine().filter(|eng| eng.attempt == attempt) {
                if eng.startup_controlled && eng.stage >= super::shared::Stage::Playing
                    && !eng.preview_abandon && !self.ready_emitted {
                    self.ready_emitted = true;
                    self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                        event: DriverEvent::Observation { attempt,
                            observation: Observation::Playback(Observed {
                                value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport,
                            }) } });
                }
                if eng.startup_controlled && eng.startup_play_commanded && !eng.prime_play
                    && !self.playing_emitted {
                    self.playing_emitted = true;
                    self.startup_play_command = None;
                    eng.startup_first_play_done = true;
                    self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                        event: DriverEvent::Observation { attempt,
                            observation: Observation::Playback(Observed {
                                value: Playback::Playing, from: EvidenceSource::BackendReport,
                            }) } });
                }
                if eng.startup_play_refused {
                    eng.startup_play_refused = false;
                    if let Some(command) = self.startup_play_command.take() {
                        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                            event: DriverEvent::ControlRefused { attempt, command,
                                operation: Operation::Play, failure: Failure::Refused } });
                    }
                }
            }
        }
        self.events.drain(..).collect()
    }

    pub(super) fn take_stop_request(&mut self) -> Option<(AttemptId, StopReason)> {
        self.stop_requested.take()
    }

    pub(super) fn take_transport_action(&mut self) -> Option<FeedTransportAction> {
        self.transport_actions.pop_front()
    }

    pub(super) fn note_transport_result(&mut self, action: FeedTransportAction, accepted: bool) {
        let (attempt, command, operation, value) = match action {
            FeedTransportAction::Play { attempt, command } =>
                (attempt, command, Operation::Play, Playback::Playing),
            FeedTransportAction::Pause { attempt, command } =>
                (attempt, command, Operation::Pause, Playback::Paused),
        };
        if self.active != Some(attempt) { return; }
        if !accepted {
            self.events.push_back(BackendEvent { backend: BackendKind::Feed,
                event: DriverEvent::ControlRefused { attempt, command, operation,
                    failure: Failure::Refused } });
            return;
        }
        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::Observation { attempt,
                observation: Observation::Playback(Observed {
                    value, from: EvidenceSource::BackendReport,
                }) } });
    }

    pub(super) fn note_release(&mut self, attempt: AttemptId, status: ReleaseStatus) {
        if self.active != Some(attempt) { return; }
        self.events.push_back(BackendEvent { backend: BackendKind::Feed,
            event: DriverEvent::Release { attempt, status } });
        if status == ReleaseStatus::Released {
            self.active = None;
            self.stop_requested = None;
            self.startup_play_command = None;
        }
    }

    pub(super) fn discard_staged(&mut self, attempt: AttemptId, key: SourceKey) {
        self.staged.remove(&(attempt, key));
    }

    pub(super) fn exit_disposition(&self, feed: &FeedResources) -> ExitDisposition {
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
