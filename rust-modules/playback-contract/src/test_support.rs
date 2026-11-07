//! Test doubles for the contract's driver traits, compiled only for this crate's own tests and for
//! dependents that name the `test-support` feature from their `[dev-dependencies]`.

use crate::uri::{UriDriver, UriSource};
use crate::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult,
    DriverEvent, DriverSet, ExitDisposition, Failure, Observation, Operation, ReleaseStatus,
    SourceKey, TrackSelection};
use plx_base::task::MainThread;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Script {
    staged: Vec<(AttemptId, SourceKey)>,
    cancelled: Vec<AttemptId>,
    discarded: Vec<(AttemptId, SourceKey)>,
    dispatched: Vec<(AttemptId, CommandId, Operation)>,
    events: VecDeque<DriverEvent>,
    plane_owned: bool,
    mapped: Vec<TrackSelection>,
    fail_open: Option<Failure>,
    refuse_start: Option<Failure>,
    refuse_track: Option<Failure>,
    exit_preserved: bool,
}

/// A scriptable [`UriDriver`]. It answers [`DriverSet::capabilities`] with the set it was built
/// with, records every call it receives, and returns only the events a test queued (plus, when
/// asked, the failure and release a real driver reports when its native open is refused). The
/// test keeps the paired [`FakeUriHandle`]: the driver itself is boxed into the adapter.
pub struct FakeUriDriver {
    caps: Capabilities,
    script: Arc<Mutex<Script>>,
}

/// The test's side of a [`FakeUriDriver`]: what it was asked, and what it should say next.
#[derive(Clone)]
pub struct FakeUriHandle(Arc<Mutex<Script>>);

impl FakeUriDriver {
    /// A driver answering `caps`, and the handle that scripts and inspects it.
    pub fn new(caps: Capabilities) -> (Self, FakeUriHandle) {
        let script = Arc::new(Mutex::new(Script::default()));
        (Self { caps, script: Arc::clone(&script) }, FakeUriHandle(script))
    }
}

impl FakeUriHandle {
    /// Every `(attempt, key)` a stage was accepted for, in order.
    pub fn staged(&self) -> Vec<(AttemptId, SourceKey)> { self.0.lock().unwrap().staged.clone() }
    /// Every attempt whose unconsumed source was cancelled.
    pub fn cancelled(&self) -> Vec<AttemptId> { self.0.lock().unwrap().cancelled.clone() }
    /// Every `(attempt, key)` the runner discarded.
    pub fn discarded(&self) -> Vec<(AttemptId, SourceKey)> { self.0.lock().unwrap().discarded.clone() }
    /// Every command the runner dispatched to the driver.
    pub fn dispatched(&self) -> Vec<(AttemptId, CommandId, Operation)> {
        self.0.lock().unwrap().dispatched.clone()
    }
    /// Events the next poll returns, in order.
    pub fn queue(&self, events: impl IntoIterator<Item = DriverEvent>) {
        self.0.lock().unwrap().events.extend(events);
    }
    /// After a Start is queued, the driver's open is refused: the next poll reports the failure
    /// and then the release of the native slot, as a real driver does.
    pub fn fail_open_after_start(&self, failure: Failure) {
        self.0.lock().unwrap().fail_open = Some(failure);
    }
    /// Refuse a Start at dispatch.
    pub fn refuse_start(&self, failure: Failure) { self.0.lock().unwrap().refuse_start = Some(failure); }
    /// Refuse the next `SelectTrack` at dispatch, as the product driver does while an earlier
    /// pick of the same kind is unresolved. One-shot: later picks queue again.
    pub fn refuse_next_track(&self, failure: Failure) {
        self.0.lock().unwrap().refuse_track = Some(failure);
    }
    /// Whether the driver claims the plane.
    pub fn set_plane_owned(&self, owned: bool) { self.0.lock().unwrap().plane_owned = owned; }
    /// Declare that the active source maps this selected track.
    pub fn map_track(&self, selection: TrackSelection) {
        self.0.lock().unwrap().mapped.push(selection);
    }
    /// Whether native ownership is still held at exit.
    pub fn set_exit_preserved(&self, preserved: bool) {
        self.0.lock().unwrap().exit_preserved = preserved;
    }
}

impl DriverSet for FakeUriDriver {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        (backend == BackendKind::Uri).then_some(self.caps)
    }

    fn dispatch(&mut self, backend: BackendKind, _: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if backend != BackendKind::Uri { return DispatchResult::Refused(Failure::Unavailable); }
        let mut script = self.script.lock().unwrap();
        script.dispatched.push((attempt, command, operation));
        if let Operation::Start { .. } = operation {
            if let Some(failure) = script.refuse_start { return DispatchResult::Refused(failure); }
            if let Some(failure) = script.fail_open {
                script.events.push_back(DriverEvent::Observation { attempt,
                    observation: Observation::Failed(failure) });
                script.events.push_back(DriverEvent::Release { attempt,
                    status: ReleaseStatus::Released });
            }
        }
        if let Operation::SelectTrack { .. } = operation {
            if let Some(failure) = script.refuse_track.take() { return DispatchResult::Refused(failure); }
        }
        DispatchResult::Queued
    }

    fn poll(&mut self, _: &MainThread) -> Vec<BackendEvent> {
        let mut script = self.script.lock().unwrap();
        script.events.drain(..).map(|event| BackendEvent { backend: BackendKind::Uri, event })
            .collect()
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        if backend == BackendKind::Uri {
            self.script.lock().unwrap().discarded.push((attempt, key));
        }
    }

    fn exit_disposition(&self) -> ExitDisposition {
        if self.script.lock().unwrap().exit_preserved {
            ExitDisposition::PreserveNativeOwnership
        } else {
            ExitDisposition::Clean
        }
    }
}

impl UriDriver for FakeUriDriver {
    fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, source: UriSource)
        -> Result<(), Failure> {
        // The checks the product driver makes before it escrows a source.
        if !source.has_url() { return Err(Failure::Stale); }
        if !source.valid_track_map() { return Err(Failure::Unsupported); }
        self.script.lock().unwrap().staged.push((attempt, key));
        Ok(())
    }

    fn cancel_staged(&mut self, attempt: AttemptId) {
        self.script.lock().unwrap().cancelled.push(attempt);
    }

    fn plane_owned(&self) -> bool { self.script.lock().unwrap().plane_owned }

    fn maps_track(&self, selection: TrackSelection) -> bool {
        self.script.lock().unwrap().mapped.contains(&selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri::NativeTrackIndex;
    use crate::{TrackKind, Requirements, SourceKind, Fidelity, TrackChoice};

    const CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false,
        packet_feed: false, pause: true, seek: true, audio_track: true, subtitle_track: false,
        audio_off: false, subtitle_off: false };

    fn boxed() -> (Box<dyn UriDriver<Context = MainThread>>, FakeUriHandle) {
        let (driver, handle) = FakeUriDriver::new(CAPS);
        (Box::new(driver), handle)
    }

    fn mt() -> MainThread { unsafe { MainThread::assume() } }

    fn attempt(n: u64) -> AttemptId { AttemptId::new(1, n) }

    #[test]
    fn a_staged_source_and_its_cancel_round_trip_through_the_boxed_driver() {
        let (mut driver, handle) = boxed();
        let key = SourceKey::new(7);
        let source = UriSource::new("https://example.invalid/a.mkv".into(), vec![]);
        assert_eq!(driver.stage_source(attempt(1), key, source), Ok(()));
        assert_eq!(handle.staged(), [(attempt(1), key)]);
        driver.cancel_staged(attempt(1));
        assert_eq!(handle.cancelled(), [attempt(1)]);
        driver.discard_staged(BackendKind::Uri, attempt(2), key);
        assert_eq!(handle.discarded(), [(attempt(2), key)]);
    }

    #[test]
    fn a_source_the_product_driver_would_refuse_is_refused_and_not_recorded() {
        let (mut driver, handle) = boxed();
        let key = SourceKey::new(1);
        assert_eq!(driver.stage_source(attempt(1), key, UriSource::new(String::new(), vec![])),
            Err(Failure::Stale));
        let twice = |index| NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 4 }, index };
        assert_eq!(driver.stage_source(attempt(1), key,
            UriSource::new("https://example.invalid/a.mkv".into(), vec![twice(1), twice(2)])),
            Err(Failure::Unsupported));
        assert!(handle.staged().is_empty());
    }

    #[test]
    fn ownership_facts_and_capabilities_are_what_the_test_scripted() {
        let (driver, handle) = boxed();
        let audio = TrackSelection { kind: TrackKind::Audio, id: 42 };
        assert_eq!(driver.capabilities(BackendKind::Uri), Some(CAPS));
        assert_eq!(driver.capabilities(BackendKind::Feed), None);
        assert!(!driver.plane_owned() && !driver.maps_track(audio));
        assert_eq!(driver.exit_disposition(), ExitDisposition::Clean);
        handle.set_plane_owned(true);
        handle.map_track(audio);
        handle.set_exit_preserved(true);
        assert!(driver.plane_owned() && driver.maps_track(audio));
        assert!(!driver.maps_track(TrackSelection { kind: TrackKind::Subtitle, id: 42 }));
        assert_eq!(driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
    }

    #[test]
    fn a_track_pick_is_refused_once_when_scripted_and_queued_after() {
        let (mut driver, handle) = boxed();
        handle.refuse_next_track(Failure::Busy);
        let pick = |id| Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(id) };
        assert_eq!(driver.dispatch(BackendKind::Uri, &mt(), attempt(3), CommandId(1), pick(4)),
            DispatchResult::Refused(Failure::Busy));
        assert_eq!(driver.dispatch(BackendKind::Uri, &mt(), attempt(3), CommandId(2), pick(4)),
            DispatchResult::Queued);
        assert_eq!(handle.dispatched().len(), 2, "a refused command was still dispatched");
    }

    #[test]
    fn a_refused_open_is_reported_after_the_start_is_queued() {
        let (mut driver, handle) = boxed();
        handle.fail_open_after_start(Failure::Backend);
        let requirements = Requirements { source: SourceKind::ProgressiveUri, video: Fidelity::Exact,
            audio: Fidelity::Exact, audio_track: TrackChoice::Unspecified,
            subtitle_track: TrackChoice::Unspecified };
        let start = Operation::Start { source: SourceKey::new(1), requirements };
        assert_eq!(driver.dispatch(BackendKind::Uri, &mt(), attempt(3), CommandId(9), start),
            DispatchResult::Queued);
        assert_eq!(handle.dispatched(), [(attempt(3), CommandId(9), start)]);
        assert_eq!(driver.poll(&mt()), [
            BackendEvent { backend: BackendKind::Uri, event: DriverEvent::Observation {
                attempt: attempt(3), observation: Observation::Failed(Failure::Backend) } },
            BackendEvent { backend: BackendKind::Uri, event: DriverEvent::Release {
                attempt: attempt(3), status: ReleaseStatus::Released } },
        ]);
        assert!(driver.poll(&mt()).is_empty(), "events are delivered once");
    }
}
