//! Startup order for the one backend attempt owner. This has no admission, IO or command IDs:
//! `BackendState` decides those. Only accepted, addressed effects advance this plan.
use plx_playback_contract::{AttemptId, BackendKind, CommandId, Fx, InitialPlayback,
    Observation, Operation, Playback, Requirements, TrackChoice, TrackKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Ready,
    Audio,
    WaitAudio(Option<CommandId>),
    Subtitle,
    WaitSubtitle(Option<CommandId>),
    Seek,
    WaitSeek(Option<CommandId>),
    Play,
    Done,
    Halted,
}

struct Plan {
    backend: BackendKind,
    attempt: AttemptId,
    requirements: Requirements,
    initial: InitialPlayback,
    stage: Stage,
    /// The first Play is mandatory only when this startup chose an unpaused goal.
    /// A later viewer Play, including the first Play of a deliberately paused session, is not.
    issuing_initial_play: bool,
    initial_play_command: Option<CommandId>,
}

pub(super) struct Startup { plan: Option<Plan>, foreground: bool }

impl Startup {
    pub(super) fn new() -> Self { Self { plan: None, foreground: true } }

    pub(super) fn begin(&mut self, backend: BackendKind, attempt: AttemptId,
        requirements: Requirements, initial: InitialPlayback) {
        self.plan = Some(Plan { backend, attempt, requirements, initial, stage: Stage::Ready,
            issuing_initial_play: false, initial_play_command: None });
    }

    pub(super) fn set_foreground(&mut self, foreground: bool) { self.foreground = foreground; }

    pub(super) fn foreground(&self) -> bool { self.foreground }

    /// The viewer's pending goal while startup still owns track/seek/initial Play ordering.
    /// The physical TX pause remains a separate hold until the native verb is admitted.
    pub(super) fn viewer_paused(&self, backend: BackendKind, attempt: AttemptId) -> Option<bool> {
        self.plan.as_ref().filter(|plan| (plan.backend, plan.attempt) == (backend, attempt)
            && (plan.stage != Stage::Halted
                && (plan.stage != Stage::Done || plan.initial_play_command.is_some())))
            .map(|plan| plan.initial.paused)
    }

    pub(super) fn abort(&mut self, backend: BackendKind, attempt: AttemptId) {
        if let Some(plan) = self.plan.as_mut() {
            if (plan.backend, plan.attempt) == (backend, attempt) {
                plan.stage = Stage::Halted;
            }
        }
    }

    /// A Play/Pause before the first Play changes the desired state. It cannot bypass initial
    /// track or seek confirmation. Other controls wait until the startup plan is complete.
    pub(super) fn intercept_control(&mut self, backend: BackendKind, attempt: AttemptId,
        operation: Operation) -> bool {
        let Some(plan) = self.plan.as_mut() else { return false };
        if (plan.backend, plan.attempt) != (backend, attempt)
            || plan.stage == Stage::Done { return false; }
        match operation {
            Operation::Play => plan.initial.paused = false,
            Operation::Pause => plan.initial.paused = true,
            _ => {}
        }
        true
    }

    pub(super) fn is_halted(&self, backend: BackendKind, attempt: AttemptId) -> bool {
        self.plan.as_ref().is_some_and(|plan| (plan.backend, plan.attempt) == (backend, attempt)
            && plan.stage == Stage::Halted)
    }

    pub(super) fn on_dispatch(&mut self, backend: BackendKind, attempt: AttemptId,
        command: CommandId, operation: Operation) {
        let Some(plan) = self.plan.as_mut() else { return };
        if (plan.backend, plan.attempt) != (backend, attempt) { return; }
        if plan.issuing_initial_play && operation == Operation::Play {
            plan.issuing_initial_play = false;
            plan.initial_play_command = Some(command);
        } else if plan.stage == Stage::Done && operation == Operation::Pause {
            // A viewer Pause supersedes an initial Play still reaching the native clock.
            plan.initial_play_command = None;
        }
        match (plan.stage, operation) {
            (Stage::WaitAudio(None), Operation::SelectTrack { kind: TrackKind::Audio, .. }) =>
                plan.stage = Stage::WaitAudio(Some(command)),
            (Stage::WaitSubtitle(None), Operation::SelectTrack { kind: TrackKind::Subtitle, .. }) =>
                plan.stage = Stage::WaitSubtitle(Some(command)),
            (Stage::WaitSeek(None), Operation::Seek(_)) => plan.stage = Stage::WaitSeek(Some(command)),
            _ => {}
        }
    }

    pub(super) fn on_notice(&mut self, notice: &Fx) {
        let Some(plan) = self.plan.as_mut() else { return };
        let (backend, attempt) = match *notice {
            Fx::Rejected { backend, attempt, .. }
            | Fx::CommandRefused { backend, attempt, .. }
            | Fx::AcceptedControl { backend, attempt, .. }
            | Fx::StartupFailed { backend, attempt, .. }
            | Fx::AcceptedObservation { backend, attempt, .. }
            | Fx::AcceptedTrackSettled { backend, attempt, .. }
            | Fx::AcceptedSeekLanded { backend, attempt, .. }
            | Fx::ReleaseOutcome { backend, attempt, .. } => (backend, attempt),
            Fx::Dispatch { .. } | Fx::DiscardStaged { .. } => return,
        };
        if (plan.backend, plan.attempt) != (backend, attempt) { return; }
        match *notice {
            Fx::AcceptedControl { command, operation: Operation::Play, .. }
                if plan.initial_play_command == Some(command) => {
                plan.initial_play_command = None;
            }
            Fx::AcceptedObservation { observation: Observation::Playback(value), .. }
                if value.value == Playback::ReadyAwaitPlay && plan.stage == Stage::Ready => {
                    plan.stage = Stage::Audio;
                }
            Fx::AcceptedTrackSettled { command, kind: TrackKind::Audio, choice, .. }
                if plan.stage == Stage::WaitAudio(Some(command))
                    && choice == plan.requirements.audio_track => {
                    plan.stage = Stage::Subtitle;
                }
            Fx::AcceptedTrackSettled { command, kind: TrackKind::Subtitle, choice, .. }
                if plan.stage == Stage::WaitSubtitle(Some(command))
                    && choice == plan.requirements.subtitle_track => {
                    plan.stage = Stage::Seek;
                }
            Fx::AcceptedSeekLanded { command, .. }
                if plan.stage == Stage::WaitSeek(Some(command)) => {
                    plan.stage = Stage::Play;
                }
            Fx::AcceptedObservation { observation: Observation::Failed(_), .. }
            | Fx::AcceptedObservation { observation: Observation::Playback(
                plx_playback_contract::Observed { value: Playback::Ended, .. }), .. } =>
                plan.stage = Stage::Halted,
            Fx::CommandRefused { .. } if plan.stage != Stage::Done => plan.stage = Stage::Halted,
            Fx::CommandRefused { command, .. } if plan.initial_play_command == Some(command) => {
                plan.initial_play_command = None;
                plan.stage = Stage::Halted;
            }
            Fx::Rejected { .. } if plan.stage != Stage::Done || plan.issuing_initial_play => {
                plan.issuing_initial_play = false;
                plan.stage = Stage::Halted;
            }
            Fx::ReleaseOutcome { .. } => plan.stage = Stage::Halted,
            Fx::StartupFailed { .. } => plan.stage = Stage::Halted,
            _ => {}
        }
        if matches!(*notice, Fx::ReleaseOutcome { .. }) { self.plan = None; }
    }

    /// Return one operation and mark it pending before the runner dispatches it recursively.
    pub(super) fn next(&mut self) -> Option<(BackendKind, AttemptId, Operation)> {
        let plan = self.plan.as_mut()?;
        loop {
            let operation = match plan.stage {
                Stage::Audio => match plan.requirements.audio_track {
                    TrackChoice::Unspecified => { plan.stage = Stage::Subtitle; continue; }
                    choice => {
                        plan.stage = Stage::WaitAudio(None);
                        Operation::SelectTrack { kind: TrackKind::Audio, choice }
                    }
                },
                Stage::Subtitle => match plan.requirements.subtitle_track {
                    TrackChoice::Unspecified => { plan.stage = Stage::Seek; continue; }
                    choice => {
                        plan.stage = Stage::WaitSubtitle(None);
                        Operation::SelectTrack { kind: TrackKind::Subtitle, choice }
                    }
                },
                Stage::Seek => match plan.initial.position {
                    None => { plan.stage = Stage::Play; continue; }
                    Some(time) => { plan.stage = Stage::WaitSeek(None); Operation::Seek(time) }
                },
                Stage::Play if plan.initial.paused => {
                    plan.stage = Stage::Done;
                    return None;
                }
                Stage::Play if self.foreground => {
                    plan.stage = Stage::Done;
                    plan.issuing_initial_play = true;
                    Operation::Play
                }
                _ => return None,
            };
            return Some((plan.backend, plan.attempt, operation));
        }
    }

    pub(super) fn is_mandatory_refusal(&self, notice: &Fx) -> bool {
        let Some(plan) = self.plan.as_ref() else { return false };
        let Fx::CommandRefused { backend, attempt, command, operation, .. } = *notice else {
            return false;
        };
        if (backend, attempt) != (plan.backend, plan.attempt) { return false; }
        match (plan.stage, operation) {
            (Stage::WaitAudio(Some(expected)), Operation::SelectTrack {
                kind: TrackKind::Audio, .. })
            | (Stage::WaitSubtitle(Some(expected)), Operation::SelectTrack {
                kind: TrackKind::Subtitle, .. })
            | (Stage::WaitSeek(Some(expected)), Operation::Seek(_)) => command == expected,
            (Stage::Done, Operation::Play) => plan.initial_play_command == Some(command),
            _ => false,
        }
    }
}
