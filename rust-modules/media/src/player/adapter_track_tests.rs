//! Adapter integration tests for source-selected URI controls and route projection, over the
//! contract's scriptable `FakeUriDriver`: what the adapter does with the events a URI driver
//! publishes. The native `selectTrack` round trip (acknowledgement, `trackSelected`, request-id
//! retirement) is the product driver's and is covered through a real `UriBackend` in the
//! application's `run_uri_boot_tests`.
use super::*;
use plx_playback_contract::test_support::{FakeUriDriver, FakeUriHandle};
use plx_playback_contract::uri::NativeTrackIndex;
use plx_playback_contract::{CommandId, DriverEvent, EvidenceSource, Failure, Fidelity,
    InitialPlayback, Observation, Observed, Playback, ReleaseStatus, Requirements, SourceKind,
    TrackChoice, TrackKind, TrackSelection};

const CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false,
    packet_feed: false, pause: true, seek: true, audio_track: true, subtitle_track: true,
    audio_off: false, subtitle_off: false };

const AUDIO_42: TrackSelection = TrackSelection { kind: TrackKind::Audio, id: 42 };
const AUDIO_43: TrackSelection = TrackSelection { kind: TrackKind::Audio, id: 43 };
const SUBTITLE_77: TrackSelection = TrackSelection { kind: TrackKind::Subtitle, id: 77 };

/// The command ids the driver was handed a `SelectTrack` under, oldest first.
fn selects(handle: &FakeUriHandle) -> Vec<CommandId> {
    handle.dispatched().into_iter()
        .filter_map(|(_, command, operation)| matches!(operation,
            Operation::SelectTrack { .. }).then_some(command)).collect()
}

/// How many times the driver was handed `wanted`.
fn dispatches(handle: &FakeUriHandle, wanted: impl Fn(&Operation) -> bool) -> usize {
    handle.dispatched().iter().filter(|(_, _, operation)| wanted(operation)).count()
}

/// The driver confirms the exact track command: the selection report, then the settled command.
fn settled(handle: &FakeUriHandle, attempt: AttemptId, command: CommandId, track: TrackSelection) {
    handle.queue([
        DriverEvent::Observation { attempt, observation: Observation::TrackSelected(Observed {
            value: track, from: EvidenceSource::BackendReport }) },
        DriverEvent::TrackSettled { attempt, command, kind: track.kind,
            choice: TrackChoice::Selected(track.id) },
    ]);
}

/// The driver refuses the exact track command at its boundary.
fn refused(handle: &FakeUriHandle, attempt: AttemptId, command: CommandId, track: TrackSelection) {
    handle.queue([DriverEvent::ControlRefused { attempt, command,
        operation: Operation::SelectTrack { kind: track.kind,
            choice: TrackChoice::Selected(track.id) }, failure: Failure::Refused }]);
}

fn poll(pa: &mut PlayerAdapter, ps: &mut crate::route::PlaybackSession) {
    let at = pa.backend_tick();
    let _ = pa.backend_poll_session_at(ps, at);
}

fn opened() -> (crate::route::PlaybackSession, PlayerAdapter, FakeUriHandle,
    crate::route::RouteStartAttempt) {
    let mut ps = crate::route::PlaybackSession::IDLE;
    crate::route::reset_player_control_for_test(&ps);
    let ticket = crate::route::begin_route_start().expect("first real route transaction");
    assert!(crate::route::prepare_route_start(ticket));
    let route_attempt = crate::route::claim_route_start_attempt(ticket)
        .expect("first physical route attempt");
    ps.begin_attempt_view(route_attempt, super::super::attempt_view::Backend::Uri);
    let mut pa = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
    let (driver, handle) = FakeUriDriver::new(CAPS);
    for track in [AUDIO_42, AUDIO_43, SUBTITLE_77] { handle.map_track(track); }
    assert!(pa.install_uri_driver(Box::new(driver)).is_ok());
    let attempt = route_attempt.contract_id();
    let source = pa.stage_uri_source(attempt, UriSource::new(
        "https://example.invalid/selected.mkv".into(), vec![
            NativeTrackIndex { selection: AUDIO_42, index: 3 },
            NativeTrackIndex { selection: AUDIO_43, index: 4 },
            NativeTrackIndex { selection: SUBTITLE_77, index: 5 },
        ])).expect("staged source");
    let requirements = Requirements { source: SourceKind::ProgressiveUri,
        video: Fidelity::Exact, audio: Fidelity::Exact,
        audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified };
    let at = pa.backend_tick();
    let started = pa.backend_send_at(&mut ps, Intent::Start { backend: BackendKind::Uri, attempt, source,
        requirements, initial: InitialPlayback { position: None, paused: true } },
        super::super::attempt_metrics::Origin::Fresh, at);
    assert!(!started.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { .. })));
    pa.bind_uri_attempt(route_attempt, crate::route::backend_policy::OfferId::URI_ORIGINAL);
    handle.queue([DriverEvent::Observation { attempt, observation: Observation::Playback(Observed {
        value: Playback::ReadyAwaitPlay, from: EvidenceSource::BackendReport }) }]);
    poll(&mut pa, &mut ps);
    assert_eq!(pa.backend_snapshot().phase, plx_playback_contract::Phase::Active);
    // ReadyAwaitPlay settled the real transaction through the adapter's addressed path.
    (ps, pa, handle, route_attempt)
}

#[test]
fn external_sidecar_with_positive_native_id_never_dispatches_a_uri_track() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, handle, _) = opened();
    assert!(!pa.commit_subtitle_track(&mut ps, 2, 77,
        Some("/library/parts/sidecar".into()), "srt".into(), None));
    assert!(selects(&handle).is_empty());
    assert_eq!(crate::route::cur_sub_sid(&ps), 0);
    assert!(pa.pending_uri_subtitle.is_none());
    crate::route::reset_player_control_for_test(&ps);
}

#[test]
fn queued_and_settled_uri_picks_keep_the_prior_route_until_exact_confirmation() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, handle, route) = opened();
    let attempt = route.contract_id();
    assert!(pa.commit_audio_track(&mut ps, crate::route::CarriedAudio::named(42, 3), None));
    let first = pa.pending_uri_audio.as_ref().expect("queued audio command").command;
    assert_eq!(selects(&handle), [first], "the exact command reached the driver once");
    assert_eq!(crate::route::cur_audio_sid(&ps), 0, "queue success is not selection");

    // A second same-kind request is refused (the product driver does so while the first native
    // request is unresolved): the pending pick stays the first and the route does not move.
    handle.refuse_next_track(Failure::Busy);
    assert!(!pa.commit_audio_track(&mut ps, crate::route::CarriedAudio::named(43, 4), None));
    assert_eq!(pa.pending_uri_audio.as_ref().map(|pick| pick.command), Some(first));
    assert_eq!(selects(&handle).len(), 2, "the refused request was dispatched and answered");
    assert_eq!(crate::route::cur_audio_sid(&ps), 0);

    // A selection report which settles no command is evidence only, never a commit.
    handle.queue([DriverEvent::Observation { attempt, observation: Observation::TrackSelected(
        Observed { value: AUDIO_42, from: EvidenceSource::BackendReport }) }]);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_audio_sid(&ps), 0, "a selected-track report alone is not a commit");
    settled(&handle, attempt, first, AUDIO_42);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_audio_sid(&ps), 42);
    assert!(pa.pending_uri_audio.is_none());

    assert!(pa.commit_audio_track(&mut ps, crate::route::CarriedAudio::named(43, 4), None));
    let second = pa.pending_uri_audio.as_ref().expect("successor command").command;
    assert_ne!(first, second);
    settled(&handle, attempt, first, AUDIO_42);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_audio_sid(&ps), 42,
        "late old selection cannot commit the pending successor");
    assert_eq!(pa.pending_uri_audio.as_ref().map(|pick| pick.command), Some(second));
    refused(&handle, attempt, first, AUDIO_42);
    poll(&mut pa, &mut ps);
    assert!(pa.refused_uri_audio.is_none(),
        "a retired native request cannot schedule an old source reconfiguration");
    refused(&handle, attempt, second, AUDIO_43);
    poll(&mut pa, &mut ps);
    assert!(pa.pending_uri_audio.is_none(), "exact refusal retires only the pending command");
    assert_eq!(crate::route::cur_audio_sid(&ps), 42);
    assert!(matches!(pa.take_uri_track_fallback(), Some(UriTrackFallback::Audio { audio, .. })
        if audio.sid == 43), "only the latest refused command can ask the resolver");
    assert!(pa.commit_subtitle_track(&mut ps, 5, 77, None, String::new(), None),
        "a later interactive refusal cannot poison another selected track");
    let subtitle = *selects(&handle).last().expect("the subtitle command");
    handle.queue([DriverEvent::Observation { attempt, observation: Observation::TrackSelected(
        Observed { value: SUBTITLE_77, from: EvidenceSource::BackendReport }) }]);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_sub_sid(&ps), 0, "uncorrelated native report is not a commit");
    settled(&handle, attempt, subtitle, SUBTITLE_77);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_sub_sid(&ps), 77);
    let at = pa.backend_tick();
    let attempt = pa.active_uri_attempt().expect("URI attempt").contract_id();
    let resumed = pa.backend_send_at(&mut ps, Intent::Control { backend: BackendKind::Uri,
        attempt,
        operation: Operation::Play }, super::super::attempt_metrics::Origin::Fresh, at);
    assert!(!resumed.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { .. } | plx_playback_contract::Fx::CommandRefused { .. })));
    assert_eq!(dispatches(&handle, |op| *op == Operation::Play), 1,
        "the paused viewer may resume after an interactive refusal");
    crate::route::reset_player_control_for_test(&ps);
}

#[test]
fn uri_subtitle_pick_commits_only_after_the_driver_settles_that_command() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, handle, route) = opened();
    let attempt = route.contract_id();
    assert!(pa.commit_subtitle_track(&mut ps, 5, 77, None, String::new(), None));
    let command = pa.pending_uri_subtitle.as_ref().expect("queued subtitle command").command;
    assert_eq!(selects(&handle), [command]);
    assert_eq!(crate::route::cur_sub_sid(&ps), 0);
    handle.queue([DriverEvent::Observation { attempt, observation: Observation::TrackSelected(
        Observed { value: SUBTITLE_77, from: EvidenceSource::BackendReport }) }]);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_sub_sid(&ps), 0, "uncorrelated native report is not a commit");
    settled(&handle, attempt, command, SUBTITLE_77);
    poll(&mut pa, &mut ps);
    assert_eq!(crate::route::cur_sub_sid(&ps), 77);
    assert!(pa.pending_uri_subtitle.is_none());
    crate::route::reset_player_control_for_test(&ps);
}

/// A URI registration that was asked to stop and has not reported its release, with a qualified
/// successor deferred behind it in the release machine. Returns the closing attempt.
fn closing_with_a_deferred_successor() -> (crate::route::PlaybackSession, PlayerAdapter,
    FakeUriHandle, crate::route::RouteStartAttempt) {
    let (mut ps, mut pa, handle, first) = opened();
    // The driver is asked to stop and never reports the release: the registration stays closing.
    let at = pa.backend_tick();
    let _ = pa.backend_send_at(&mut ps, Intent::Stop { backend: BackendKind::Uri,
        attempt: first.contract_id(), reason: plx_playback_contract::StopReason::Replace },
        super::super::attempt_metrics::Origin::Fresh, at);
    poll(&mut pa, &mut ps);
    assert!(pa.release_busy());

    crate::route::set_url(&mut ps, "https://example.invalid/successor.mkv");
    let ticket = crate::route::begin_route_start().expect("successor route transaction");
    let offer = QualifiedUriOffer::new(
        crate::route::backend_policy::OfferId::URI_ORIGINAL,
        UriSourceRecipe::OriginalPart, ticket, ps.uri_selection(),
        UriSource::new("https://example.invalid/successor.mkv".into(), Vec::new()),
        Requirements { source: SourceKind::ProgressiveUri,
            video: Fidelity::Exact, audio: Fidelity::Exact,
            audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified },
        InitialPlayback { position: None, paused: true }, 60_000,
        std::time::Instant::now() + std::time::Duration::from_secs(5))
        .expect("exact qualification");
    assert!(pa.offer_qualified_uri(&mut ps, offer).is_ok());
    assert_eq!(super::super::route_start::start_bufferfeed_for(&mut ps, &mut pa, ticket),
        super::super::route_start::BufferfeedStartOutcome::Deferred);
    pa.watch_queued_start();
    (ps, pa, handle, first)
}

#[test]
fn a_qualified_uri_successor_waits_for_the_prior_registration_to_release() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, handle, first) = closing_with_a_deferred_successor();
    let starts = |handle: &FakeUriHandle| dispatches(handle, |op| matches!(op, Operation::Start { .. }));
    assert_eq!(starts(&handle), 1, "no second native registration while closing");
    assert!(pa.qualified_uri.is_some(), "the pending URL stays in one-shot escrow");

    handle.queue([DriverEvent::Release { attempt: first.contract_id(),
        status: ReleaseStatus::Released }]);
    poll(&mut pa, &mut ps);
    assert!(matches!(pa.take_queued_landing(), QueuedLanding::Launched(_)));
    assert!(pa.qualified_uri.is_none(), "the exact Start consumed its authorization once");
    assert_eq!(starts(&handle), 2, "exact release permits one successor native registration");
    assert_eq!(handle.staged().len(), 2, "the successor's source was staged exactly once");
    crate::route::reset_player_control_for_test(&ps);
}

/// The release machine's step is where the readout hears a URI `Released`: a raw backend poll
/// consumes the queue into the successor's `Start` effect and the readout already says no start is
/// queued, although nothing has run that effect (the session poll does).
#[test]
fn a_uri_released_verdict_reaches_the_readout_at_the_step_before_its_start_runs() {
    let _serial = plx_base::testlock::serial();
    let (mut ps, mut pa, handle, first) = closing_with_a_deferred_successor();
    let starts = |handle: &FakeUriHandle| dispatches(handle, |op| matches!(op, Operation::Start { .. }));
    assert!(ps.readout().start_queued(), "the deferred start reached the readout at its own step");
    handle.queue([DriverEvent::Release { attempt: first.contract_id(),
        status: ReleaseStatus::Released }]);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_at(&mut ps, at);
    assert!(!pa.release_has_queued_start(), "the verdict took the start out of the machine's queue");
    assert!(!ps.readout().start_queued(),
        "and the readout heard it at that step, not when the start's effect runs");
    let effects = pa.take_release_effects();
    assert!(matches!(effects.as_slice(), [super::super::release::ReleaseEffect::Start(_)]),
        "the verdict's one effect is the successor's Start, not yet run");
    assert_eq!(starts(&handle), 1, "no second native registration: nothing ran the effect");
    crate::route::reset_player_control_for_test(&ps);
}
