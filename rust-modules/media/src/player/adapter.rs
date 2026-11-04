//! **The Player ADAPTER** — the main-thread token and the owned backend resources it confines
//! (restructure spec §2.2, phase 9).
//!
//! The adapter owns the one backend attempt runner beside its OS/FFI resources; route and viewer
//! decisions live one field away in [`crate::player::machine::Player`]. Buffer-feed ownership is grouped in [`FeedResources`]:
//! the [`Engine`] slot, native Load receipts, abandoned Load and parked release. The adapter
//! retains the [`MainThread`] token, periodic Plex reporter, release policy, and URI admission.
//!
//! # Why the slot moved here, and what replaced the token argument
//!
//! Until phase 9 the slot was `static mut ENGINE: Option<Engine>`, reachable only through four
//! accessors that each took a `&MainThread`. That argument was the enforcement: the accessor
//! handed out a `&'static mut` to a `static mut`, a second caller on another thread is instant UB,
//! and `static mut` carries no `Sync` bound to stop one (verified by compiling the counterexample
//! — see `docs/async-model-decision.md`).
//!
//! The reasoning is unchanged and the enforcement is stronger. **The token is now CONSUMED at
//! construction** ([`PlayerAdapter::new`] takes it by value, and `plex_run` is the only place that
//! can mint one), so holding a `&mut PlayerAdapter` IS the proof the old argument stood in for —
//! and it is a proof the borrow checker keeps rather than one a caller could satisfy twice. Two
//! live `&mut` to the engine no longer requires a convention: it does not compile. The native
//! feed owns that borrow; common callers receive addressed facts or coarse physical actions.
//!
//! The rest of `player/engine.rs` keeps its `mt: &MainThread` parameters unchanged. They gate a
//! DIFFERENT thing — the ACB/Starfish seam (`tv::sink::VideoSink`'s methods, whose bind order is a
//! sequence of calls with no locking behind it) — and that surface is not this module's.

#[cfg(all(test, feature = "play-feed"))]
use super::engine::Engine;
#[cfg(feature = "play-feed")]
use super::feed_resources::FeedResources;
#[cfg(any(feature = "play-feed", feature = "play-uri"))]
use super::timeline;
use super::backend;
#[cfg(feature = "play-feed")]
use super::feed_driver::{FeedDriver, FeedTransportAction};
#[cfg(all(test, feature = "play-feed"))]
use super::threads;
use plx_base::task::MainThread;
use std::collections::VecDeque;

const MAX_METRIC_NOTICES: usize = 32;
use plx_playback_contract::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId,
    DispatchResult, DriverSet, ExitDisposition, Failure, Intent, Operation,
    SourceKey};
#[cfg(any(feature = "play-feed", feature = "play-uri"))]
use plx_playback_contract::ReleaseStatus;
#[cfg(feature = "play-feed")]
use plx_playback_contract::StopReason;
#[cfg(feature = "play-uri")]
use plx_playback_contract::Playback;
#[cfg(feature = "play-uri")]
use plx_play_uri::backend::{Binding as UriBinding, Config as UriConfig,
    MonotonicClock, UriBackend, UriSource};

#[cfg(feature = "play-uri")]
struct PendingUriAudio {
    attempt: AttemptId,
    command: CommandId,
    audio: crate::route::CarriedAudio,
    memo: Option<plx_plex::plex::session::prefs::AudioMemo>,
}

#[cfg(all(test, feature = "play-uri"))]
#[path = "adapter_track_tests.rs"]
mod track_tests;

#[cfg(feature = "play-uri")]
struct PendingUriSubtitle {
    attempt: AttemptId,
    command: CommandId,
    ordinal: i32,
    stream_id: i64,
    memo: Option<plx_plex::plex::session::prefs::SubtitleMemo>,
}

/// Explicit product URI settings captured with the resolve port's certification evidence.
/// Platform transport and display are supplied at the IO composition boundary.
#[cfg(feature = "play-uri")]
pub struct UriDriverSettings {
    pub clock: Box<dyn MonotonicClock>,
    pub app_id: String,
    pub binding: UriBinding,
    pub capabilities: Capabilities,
    pub startup_timeout_ms: u64,
    pub release_allowance_ms: u64,
    pub source_ttl_ms: u64,
}

/// Product clock lives at the player IO boundary; the URI reducer receives elapsed values.
#[cfg(feature = "play-uri")]
struct ProductUriClock(std::time::Instant);

/// Expiry for an off-frame source qualification, read at the player IO boundary. A qualifier
/// owns the returned deadline with its source; the route only compares the supplied value.
#[cfg(feature = "play-uri")]
pub fn uri_source_expiry_after(ttl: std::time::Duration) -> std::time::Instant {
    std::time::Instant::now() + ttl
}

#[cfg(feature = "play-uri")]
impl MonotonicClock for ProductUriClock {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

/// An external IO qualifier's verdict for one resolved Part and its exact selected streams.
/// The owned URL and native track map never enter the route machine or a Debug value. No
/// production qualifier currently supplies this value; absence preserves the Feed choice.
#[cfg(feature = "play-uri")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriSourceRecipe {
    OriginalPart,
    PmsRemux { applied_audio: plx_plex::plex::AudioEnhancements },
    PmsEncode { applied_audio: plx_plex::plex::AudioEnhancements },
    PmsHls { applied_audio: plx_plex::plex::AudioEnhancements },
}

#[cfg(feature = "play-uri")]
impl UriSourceRecipe {
    fn offer_id(self) -> crate::route::backend_policy::OfferId {
        use crate::route::backend_policy::OfferId;
        match self {
            Self::OriginalPart => OfferId::URI_ORIGINAL,
            Self::PmsRemux { .. } => OfferId::URI_PMS_REMUX,
            Self::PmsEncode { .. } => OfferId::URI_PMS_ENCODE,
            Self::PmsHls { .. } => OfferId::URI_PMS_HLS,
        }
    }

    fn audio(self) -> plx_plex::plex::AudioEnhancements {
        match self {
            Self::OriginalPart => plx_plex::plex::AudioEnhancements::NONE,
            Self::PmsRemux { applied_audio } | Self::PmsEncode { applied_audio }
            | Self::PmsHls { applied_audio } => applied_audio,
        }
    }
}

#[cfg(feature = "play-uri")]
pub struct QualifiedUriOffer {
    pub(super) offer_id: crate::route::backend_policy::OfferId,
    pub(super) recipe: UriSourceRecipe,
    pub(super) route_start: crate::route::RouteStartTransaction,
    pub(super) selection: crate::route::UriSelection,
    pub(super) source: UriSource,
    pub(super) requirements: plx_playback_contract::Requirements,
    pub(super) initial: plx_playback_contract::InitialPlayback,
    /// The resolved item's PMS metadata duration, supplied with this source-specific offer.
    pub(super) pms_duration_ns: i64,
    pub(super) expires_at: std::time::Instant,
}

/// One physical URI failure awaiting an exact native release verdict before a Feed resolve.
/// This records evidence about the canonical attempt; it does not queue or admit a successor.
#[cfg(feature = "play-uri")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UriRecoveryNotice {
    pub attempt: crate::route::RouteStartAttempt,
    pub offer_id: crate::route::backend_policy::OfferId,
    pub phase: crate::route::UriFailurePhase,
    pub known_position_ns: Option<i64>,
    pub viewer_paused: bool,
}

#[cfg(feature = "play-uri")]
#[derive(Clone, Copy)]
struct PendingUriRecovery { notice: UriRecoveryNotice, released: bool }

#[cfg(all(test, feature = "play-uri"))]
mod qualified_uri_offer_tests {
    use super::*;
    use plx_playback_contract::{Fidelity, InitialPlayback, Requirements, SourceKind, TrackChoice};
    use plx_play_uri::backend::{Binding, MonotonicClock};

    struct Clock;
    impl MonotonicClock for Clock { fn now_ms(&self) -> u64 { 0 } }

    fn requirements(audio: TrackChoice) -> Requirements {
        Requirements { source: SourceKind::ProgressiveUri,
            video: Fidelity::Exact, audio: Fidelity::Exact,
            audio_track: audio, subtitle_track: TrackChoice::Unspecified }
    }

    #[test]
    fn qualified_source_is_bound_to_resolved_selection_and_never_inferred_from_feed() {
        let _serial = plx_base::testlock::serial();
        let mut session = crate::route::PlaybackSession::IDLE;
        crate::route::reset_player_control_for_test(&session);
        crate::route::set_url(&mut session, "https://example.invalid/original.mkv");
        let ticket = crate::route::begin_route_start().expect("route transaction");
        if cfg!(feature = "play-feed") {
            assert!(session.qualify_resolved_feed_offer(
                crate::route::backend_policy::OfferId::FEED_ORIGINAL,
                crate::route::CandidatePurpose::NewCandidate));
            assert!(matches!(session.backend_offer(),
                crate::route::backend_policy::Selection::Open(offer)
                    if offer.backend == BackendKind::Feed));
        }
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        let config = UriConfig {
            transport: &plx_platform::tv::uri::NoUriMedia,
            display: &plx_platform::tv::sink::NoSink,
            clock: Box::new(Clock), app_id: "fixture".to_owned(),
            binding: Binding::Unbound,
            capabilities: Capabilities { progressive_uri: true, hls_uri: false,
                packet_feed: false, pause: true, seek: true, audio_track: true,
                subtitle_track: true, audio_off: false, subtitle_off: false },
            startup_timeout_ms: 100, release_allowance_ms: 100, source_ttl_ms: 100,
        };
        assert!(adapter.install_uri_backend_config(config).is_ok());
        let selection = session.uri_selection();
        let mut stale = selection;
        stale.part_id += 1;
        let initial = InitialPlayback { position: None, paused: false };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stale_offer = QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_ORIGINAL,
            UriSourceRecipe::OriginalPart, ticket, stale,
            UriSource::new("https://example.invalid/wrong.mkv".to_owned(), vec![]),
            requirements(TrackChoice::Unspecified), initial, 60_000, deadline).expect("typed offer");
        assert!(matches!(adapter.offer_qualified_uri(&mut session, stale_offer), Err(Failure::Stale)));

        #[cfg(feature = "play-feed")]
        {
        session.accept_requested_rate(1500);
        let wrong_speed = QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_ORIGINAL,
            UriSourceRecipe::OriginalPart, ticket, selection,
            UriSource::new("https://example.invalid/original.mkv".to_owned(), vec![]),
            requirements(TrackChoice::Unspecified), initial, 60_000, deadline)
            .expect("typed source");
        assert!(matches!(adapter.offer_qualified_uri(&mut session, wrong_speed),
            Err(Failure::Unsupported)), "URI has no implicit speed transform");
        session.accept_requested_rate(plx_playback_contract::RATE_NORMAL);
        }

        let offered = QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_ORIGINAL,
            UriSourceRecipe::OriginalPart, ticket, selection,
            UriSource::new("https://example.invalid/original.mkv".to_owned(), vec![]),
            requirements(TrackChoice::Unspecified), initial, 60_000, deadline).expect("typed offer");
        assert!(adapter.offer_qualified_uri(&mut session, offered).is_ok());
        assert!(matches!(session.backend_offer(),
            crate::route::backend_policy::Selection::Open(offer)
                if offer.backend == BackendKind::Uri
                    && offer.id == crate::route::backend_policy::OfferId::URI_ORIGINAL));
        let started = super::super::route_start::start_bufferfeed_for(
            &mut session, &mut adapter, ticket);
        assert!(matches!(started, super::super::route_start::BufferfeedStartOutcome::Launched(_)),
            "queued URI Open is distinct from the fake transport's later refusal");
        assert!(!adapter.request_seek(&mut session, 3_000_000_000),
            "a URI still opening refuses runtime seek through the common door");
        assert!(adapter.qualified_uri.is_none(), "failed Start consumed the source authorization");
        let at = adapter.backend_tick();
        let _ = adapter.backend_poll_session_at(&mut session, at);
        assert_eq!(crate::route::route_start_status(
            match started { super::super::route_start::BufferfeedStartOutcome::Launched(a) => a,
                _ => unreachable!() }), crate::route::RouteStartStatus::Failed);
        assert_eq!(crate::player::state(&session),
            plx_playback_contract::readout::PlaybackState::Error,
            "a native Open refusal cannot leave a URI-only session spinning forever");
        assert!(adapter.take_qualified_uri(&mut session, ticket).is_none(), "the URL is owned once");
        if cfg!(feature = "play-feed") {
            assert!(matches!(session.backend_offer(),
                crate::route::backend_policy::Selection::Open(offer)
                    if offer.backend == BackendKind::Feed),
                "URI refusal retains the qualified Feed choice");
        } else {
            assert_eq!(session.backend_offer(), crate::route::backend_policy::Selection::Unqualified);
        }
        crate::route::reset_player_control_for_test(&session);
    }

    #[test]
    fn original_selected_track_cannot_be_silently_changed_by_a_uri_offer() {
        let selection = crate::route::UriSelection { server: plx_plex::plex::ServerId::UNSET,
            part_id: 17, audio_sid: 41, subtitle_sid: 0,
            audio_contract: plx_plex::plex::AudioEnhancements::NONE };
        let ticket = crate::route::RouteStartTransaction::fixture(17);
        let source = || UriSource::new("https://example.invalid/track.mkv".to_owned(), vec![]);
        let initial = InitialPlayback { position: None, paused: true };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        assert!(matches!(QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_ORIGINAL,
            UriSourceRecipe::OriginalPart, ticket, selection, source(),
            requirements(TrackChoice::Selected(42)), initial, 60_000, deadline), Err(Failure::Unsupported)));
        let enhanced = crate::route::UriSelection {
            audio_contract: plx_plex::plex::AudioEnhancements {
                boost_dialog: true, normalize_loudness: false,
            }, ..selection
        };
        assert!(matches!(QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_ORIGINAL,
            UriSourceRecipe::OriginalPart, ticket, enhanced, source(),
            requirements(TrackChoice::Selected(41)), initial, 60_000, deadline),
            Err(Failure::Unsupported)),
            "original native URI cannot silently drop a viewer audio enhancement");
        assert!(QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_ORIGINAL,
            UriSourceRecipe::OriginalPart, ticket, selection, source(),
            requirements(TrackChoice::Selected(41)), initial, 60_000, deadline).is_ok());
        let converted = Requirements { source: SourceKind::HlsUri,
            video: Fidelity::Compatible, audio: Fidelity::Compatible,
            audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified };
        assert!(QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_PMS_HLS,
            UriSourceRecipe::PmsHls { applied_audio: plx_plex::plex::AudioEnhancements::NONE },
            ticket, selection, source(),
            converted, initial, 60_000, deadline).is_ok(),
            "PMS has already selected the original source streams before its HLS output is qualified");
        assert!(matches!(QualifiedUriOffer::new(
            crate::route::backend_policy::OfferId::URI_PMS_HLS,
            UriSourceRecipe::PmsHls { applied_audio: plx_plex::plex::AudioEnhancements::NONE },
            ticket, selection, source(),
            converted, initial, 0, deadline), Err(Failure::Unsupported)),
            "a URI report needs an identified item duration for the common timeline");
    }
}

#[cfg(feature = "play-uri")]
impl QualifiedUriOffer {
    pub fn new(offer_id: crate::route::backend_policy::OfferId,
        recipe: UriSourceRecipe,
        route_start: crate::route::RouteStartTransaction,
        selection: crate::route::UriSelection, source: UriSource,
        requirements: plx_playback_contract::Requirements,
        initial: plx_playback_contract::InitialPlayback,
        pms_duration_ms: u64,
        expires_at: std::time::Instant) -> Result<Self, Failure> {
        use crate::route::backend_policy::OfferId;
        let required_fidelity = match offer_id {
            OfferId::URI_ORIGINAL => plx_playback_contract::Fidelity::Exact,
            OfferId::URI_PMS_REMUX | OfferId::URI_PMS_ENCODE | OfferId::URI_PMS_HLS =>
                plx_playback_contract::Fidelity::Compatible,
            _ => return Err(Failure::Unsupported),
        };
        if recipe.offer_id() != offer_id || recipe.audio() != selection.audio_contract
            || requirements.source == plx_playback_contract::SourceKind::PacketFeed
            || requirements.video != required_fidelity
            || requirements.audio != required_fidelity
            || (offer_id == OfferId::URI_PMS_HLS
                && requirements.source != plx_playback_contract::SourceKind::HlsUri)
            || (matches!(offer_id, OfferId::URI_PMS_REMUX | OfferId::URI_PMS_ENCODE)
                && requirements.source != plx_playback_contract::SourceKind::ProgressiveUri) {
            return Err(Failure::Unsupported);
        }
        let pms_duration_ns = pms_duration_ms.checked_mul(1_000_000)
            .and_then(|ns| i64::try_from(ns).ok()).filter(|ns| *ns > 0)
            .ok_or(Failure::Unsupported)?;
        if initial.position.is_some_and(|position| position.as_millis()
            .checked_mul(1_000_000)
            .and_then(|ns| i64::try_from(ns).ok())
            .is_none_or(|ns| ns > pms_duration_ns)) {
            return Err(Failure::Unsupported);
        }
        let selected_audio = (selection.audio_sid > 0).then(||
            plx_playback_contract::TrackChoice::Selected(selection.audio_sid as u64));
        let selected_subtitle = (selection.subtitle_sid > 0).then(||
            plx_playback_contract::TrackChoice::Selected(selection.subtitle_sid as u64));
        if (offer_id == OfferId::URI_ORIGINAL
                && (selected_audio.is_some_and(|choice| requirements.audio_track != choice)
                    || selected_subtitle.is_some_and(|choice| requirements.subtitle_track != choice)))
            || selected_audio.is_some_and(|choice| matches!(requirements.audio_track,
                plx_playback_contract::TrackChoice::Selected(_) if requirements.audio_track != choice))
            || selected_subtitle.is_some_and(|choice| matches!(requirements.subtitle_track,
                plx_playback_contract::TrackChoice::Selected(_) if requirements.subtitle_track != choice))
            || (selection.audio_sid <= 0 && matches!(requirements.audio_track,
                plx_playback_contract::TrackChoice::Selected(_)))
            || (selection.subtitle_sid <= 0 && matches!(requirements.subtitle_track,
                plx_playback_contract::TrackChoice::Selected(_))) {
            return Err(Failure::Unsupported);
        }
        Ok(Self { offer_id, recipe, route_start, selection, source, requirements, initial,
            pms_duration_ns, expires_at })
    }
}

#[cfg(feature = "play-uri")]
fn uri_offer_executable(ps: &crate::route::PlaybackSession, offer: &QualifiedUriOffer) -> bool {
    // The URI driver has no speed transform. The server output, when it claims an enhancement,
    // must already have it applied; an app-only/pending ask cannot be silently dropped.
    ps.requested_rate_permille() == plx_playback_contract::RATE_NORMAL
        && crate::route::displayed_audio_enhancements(ps) == offer.recipe.audio()
        && (offer.recipe.audio().is_none()
            || crate::route::applied_audio_enhancements(ps) == offer.recipe.audio())
}

/// Temporary borrow of the adapter's persistent physical resources. The runner remains the
/// single admission owner; each driver only executes addressed commands and reports evidence.
struct PhysicalDrivers<'a> {
    marker: std::marker::PhantomData<&'a ()>,
    #[cfg(feature = "play-feed")]
    feed: &'a mut FeedResources,
    #[cfg(feature = "play-feed")]
    feed_driver: &'a mut FeedDriver,
    #[cfg(feature = "play-uri")]
    uri: &'a mut Option<UriBackend>,
}

impl DriverSet for PhysicalDrivers<'_> {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        match backend {
            BackendKind::Feed => {
                #[cfg(feature = "play-feed")]
                { Some(self.feed_driver.capabilities()) }
                #[cfg(not(feature = "play-feed"))]
                { None }
            }
            BackendKind::Uri => {
                #[cfg(feature = "play-uri")]
                { self.uri.as_ref().and_then(|driver| driver.capabilities(BackendKind::Uri)) }
                #[cfg(not(feature = "play-uri"))]
                { None }
            }
        }
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        match backend {
            BackendKind::Feed => {
                #[cfg(feature = "play-feed")]
                { self.feed_driver.dispatch(self.feed, mt, attempt, command, operation) }
                #[cfg(not(feature = "play-feed"))]
                { let _ = (mt, attempt, command, operation);
                    DispatchResult::Refused(Failure::Unavailable) }
            }
            BackendKind::Uri => {
                #[cfg(feature = "play-uri")]
                { self.uri.as_mut().map_or(DispatchResult::Refused(Failure::Unavailable),
                    |driver| driver.dispatch(BackendKind::Uri, mt, attempt, command, operation)) }
                #[cfg(not(feature = "play-uri"))]
                { let _ = (mt, attempt, command, operation); DispatchResult::Refused(Failure::Unavailable) }
            }
        }
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<BackendEvent> {
        #[cfg(all(feature = "play-feed", feature = "play-uri"))]
        let mut events = self.feed_driver.poll(self.feed);
        #[cfg(all(feature = "play-feed", not(feature = "play-uri")))]
        let events = self.feed_driver.poll(self.feed);
        #[cfg(all(not(feature = "play-feed"), feature = "play-uri"))]
        let mut events = Vec::new();
        #[cfg(not(any(feature = "play-feed", feature = "play-uri")))]
        let events = Vec::new();
        #[cfg(feature = "play-uri")]
        if let Some(uri) = self.uri.as_mut() { events.extend(uri.poll(mt)); }
        #[cfg(not(feature = "play-uri"))]
        let _ = mt;
        events
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        match backend {
            BackendKind::Feed => {
                #[cfg(feature = "play-feed")]
                self.feed_driver.discard_staged(attempt, key);
                #[cfg(not(feature = "play-feed"))]
                let _ = (attempt, key);
            }
            BackendKind::Uri => {
                #[cfg(feature = "play-uri")]
                if let Some(uri) = self.uri.as_mut() {
                    uri.discard_staged(BackendKind::Uri, attempt, key);
                }
                #[cfg(not(feature = "play-uri"))]
                let _ = (attempt, key);
            }
        }
    }

    fn exit_disposition(&self) -> ExitDisposition {
        #[cfg(feature = "play-feed")]
        if self.feed_driver.exit_disposition(self.feed) == ExitDisposition::PreserveNativeOwnership {
            return ExitDisposition::PreserveNativeOwnership;
        }
        #[cfg(feature = "play-uri")]
        if self.uri.as_ref().is_some_and(|driver| driver.exit_disposition() == ExitDisposition::PreserveNativeOwnership) {
            return ExitDisposition::PreserveNativeOwnership;
        }
        ExitDisposition::Clean
    }
}

/// Common coordinator and main-thread proof, as one owned value (`App.adapters.player`).
pub struct PlayerAdapter {
    /// Proof, held rather than passed: see the module doc. It is a ZST, so this costs nothing and
    /// is what makes `&mut PlayerAdapter` mean "the main thread, exclusively".
    mt: MainThread,
    /// The native feed slot and its Load/release worker resources.
    #[cfg(feature = "play-feed")]
    pub(super) feed: FeedResources,
    /// The one neutral attempt owner, with its physical feed and optional URI executors below.
    backend: backend::BackendRunner,
    /// Recent typed measurements for local diagnostics. Older notices are evicted; the
    /// reducer's aggregate summary keeps the logical request's counts and durations.
    metric_notices: VecDeque<backend::MetricNotice>,
    #[cfg(feature = "play-feed")]
    feed_driver: FeedDriver,
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    next_source_key: u64,
    backend_epoch: std::time::Instant,
    #[cfg(feature = "play-uri")]
    uri_backend: Option<UriBackend>,
    #[cfg(feature = "play-uri")]
    qualified_uri: Option<QualifiedUriOffer>,
    #[cfg(feature = "play-uri")]
    uri_route_attempt: Option<crate::route::RouteStartAttempt>,
    #[cfg(feature = "play-uri")]
    uri_offer_id: Option<crate::route::backend_policy::OfferId>,
    #[cfg(feature = "play-uri")]
    uri_recovery: Option<PendingUriRecovery>,
    #[cfg(feature = "play-uri")]
    uri_route_settled: bool,
    #[cfg(feature = "play-uri")]
    uri_played_once: bool,
    #[cfg(feature = "play-uri")]
    uri_stop_requested: bool,
    #[cfg(feature = "play-uri")]
    uri_finalized: bool,
    #[cfg(feature = "play-uri")]
    pending_uri_audio: Option<PendingUriAudio>,
    #[cfg(feature = "play-uri")]
    pending_uri_subtitle: Option<PendingUriSubtitle>,
    /// Route identity and product trace for the installed physical attempt. The feed itself
    /// receives only the canonical contract ID and native epoch.
    #[cfg(feature = "play-feed")]
    feed_attempt: Option<(crate::route::RouteStartAttempt, u32)>,
    /// Backend-independent periodic reporting resource for the current physical attempt.
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    timeline: Option<timeline::TimelineRunner>,
    repair: Option<(u64, std::sync::mpsc::Receiver<Result<(), plx_platform::tv::sandbox::Failure>>)>,
    release: super::release::NativeRelease,
    /// Effects produced by an addressed URI release while the backend poll has no session
    /// borrow. The release machine owns the sole queued transaction; these are only its IO work.
    #[cfg(feature = "play-uri")]
    release_fx: Vec<super::release::ReleaseEffect>,
    /// What became of the start the FOREGROUND restore queued behind native release
    /// ([`watch_queued_start`](Self::watch_queued_start)); the lifecycle machine polls it.
    queued_watch: QueuedWatch,
    /// A hero PREVIEW's start was queued behind Feed release: whether its slot counts as admitted
    /// once the Load actually launches (`None`: no preview start is queued). Admission is taken
    /// when the queued start runs, never when it is merely queued.
    #[cfg(feature = "play-feed")]
    preview_admission: Option<bool>,
    /// The URI-mode probe's session (issue #1064, dev trigger `plxnative-uriprobe`): the probe plays
    /// through `com.webos.media` instead of an [`Engine`], so it lives BESIDE the engine slot and
    /// never in it. [`is_live`](Self::is_live) stays Engine-only: the Engine paths (`pump`, the
    /// HUD, the stop ritual's `take`) must never see a probe. Owned here, not by a static, for the
    /// same reason the Engine is: ending it calls the window verbs of the sink, which only the
    /// main thread may. Absent from a release build.
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    uri: Option<super::uriprobe_drive::UriSession>,
}

/// The foreground restore's view of its own queued start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuedWatch {
    /// Nobody follows the queued start (an ordinary play lands through `finish_landed_play`).
    Unwatched,
    /// The restore's start is queued; it has not run.
    Watching,
    /// The queued start ran: the Load was launched ...
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    Launched(crate::route::RouteStartAttempt),
    /// ... or ended without one (refused at the deadline, or the start itself failed).
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    Failed,
}

/// What polling the restore's queued start found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedLanding {
    Waiting,
    /// Cancelled or superseded: nothing to follow.
    Stale,
    Launched(crate::route::RouteStartAttempt),
    Failed,
}

impl PlayerAdapter {
    /// Take the token. `app::boot` calls this once, with the token `plex_run` minted.
    pub fn new(mt: MainThread) -> Self {
        Self {
            mt,
            #[cfg(feature = "play-feed")]
            feed: FeedResources::new(&super::SHARED.feed),
            backend: backend::BackendRunner::new(),
            metric_notices: VecDeque::new(),
            #[cfg(feature = "play-feed")]
            feed_driver: FeedDriver::new(),
            #[cfg(any(feature = "play-feed", feature = "play-uri"))]
            next_source_key: 0,
            backend_epoch: std::time::Instant::now(),
            #[cfg(feature = "play-uri")]
            uri_backend: None,
            #[cfg(feature = "play-uri")]
            qualified_uri: None,
            #[cfg(feature = "play-uri")]
            uri_route_attempt: None,
            #[cfg(feature = "play-uri")]
            uri_offer_id: None,
            #[cfg(feature = "play-uri")]
            uri_recovery: None,
            #[cfg(feature = "play-uri")]
            uri_route_settled: false,
            #[cfg(feature = "play-uri")]
            uri_played_once: false,
            #[cfg(feature = "play-uri")]
            uri_stop_requested: false,
            #[cfg(feature = "play-uri")]
            uri_finalized: false,
            #[cfg(feature = "play-uri")]
            pending_uri_audio: None,
            #[cfg(feature = "play-uri")]
            pending_uri_subtitle: None,
            #[cfg(feature = "play-feed")]
            feed_attempt: None,
            #[cfg(any(feature = "play-feed", feature = "play-uri"))]
            timeline: None,
            repair: None,
            release: super::release::NativeRelease::new(),
            #[cfg(feature = "play-uri")]
            release_fx: Vec::new(),
            queued_watch: QueuedWatch::Unwatched,
            #[cfg(feature = "play-feed")]
            preview_admission: None,
            #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
            uri: None,
        }
    }

    /// The adapter reads monotonic time; the pure backend and metrics reducers receive a value.
    pub fn backend_tick(&self) -> super::attempt_metrics::Tick {
        super::attempt_metrics::Tick::from_millis(
            self.backend_epoch.elapsed().as_millis().min(u64::MAX as u128) as u64,
        )
    }

    /// Drain route facts before any physical Start. Acceptance happened in a pure route step,
    /// where this monotonic clock is unavailable; the request's exact start tick remains Unknown.
    /// Subsequent physical observations use this adapter's single clock domain.
    pub fn sync_request_metrics(&mut self, ps: &mut crate::route::PlaybackSession) {
        for effect in ps.take_metric_lifecycle() {
            let result = match effect {
                backend::RequestLifecycle::Begin { request, origin } =>
                    self.backend.begin_request_at(request, origin,
                        super::attempt_metrics::Tick::Unknown),
                backend::RequestLifecycle::Finish { request, outcome } =>
                    self.backend.finish_request_at(request, outcome, self.backend_tick()),
            };
            self.record_backend_metrics(&result.metrics);
        }
    }

    /// The common recovery owner has accepted an addressed automatic switch while the failed
    /// physical attempt still owns release. Start its visible gap before Closing and resolve;
    /// the future Feed attempt is not claimed yet. Stale request/attempt pairs are ignored by
    /// the reducer before its clock can affect the current request.
    pub fn begin_automatic_handoff(&mut self, request: backend::LogicalRequestId,
        from: AttemptId) {
        let result = self.backend.handoff_begun_at(request, from, self.backend_tick());
        self.record_backend_metrics(&result.metrics);
    }

    /// Project the addressed common readout into intentional/eligible time for the active
    /// physical attempt. An old session view cannot classify its successor's interval.
    pub fn observe_backend_condition(&mut self, ps: &crate::route::PlaybackSession,
        foreground: bool) {
        let snapshot = self.backend.snapshot();
        let (Some(backend), Some(attempt)) = (snapshot.backend, snapshot.attempt) else {
            return;
        };
        if ps.attempt_view().attempt().map(|a| a.contract_id()) != Some(attempt) {
            return;
        }
        use super::attempt_metrics::Condition;
        let condition = if !foreground {
            Condition::Held
        } else if self.backend.viewer_paused(backend, attempt) == Some(true) {
            Condition::Paused
        } else {
            match super::state(ps) {
                super::PlaybackState::Seeking => Condition::Seeking,
                super::PlaybackState::Playing => Condition::Playing,
                super::PlaybackState::Buffering
                    if super::rebuffering(ps) == Some(true) => Condition::Rebuffering,
                _ => Condition::Unknown,
            }
        };
        let result = self.backend.condition_at(backend, attempt, condition, self.backend_tick());
        self.record_backend_metrics(&result.metrics);
    }

    /// Retain every runner result in a bounded local typed window. Notices contain only numeric
    /// IDs, enums and durations; the ordinary event log is not a bounded default-build sink.
    fn record_backend_metrics(&mut self, notices: &[backend::MetricNotice]) {
        for notice in notices {
            if self.metric_notices.len() == MAX_METRIC_NOTICES {
                self.metric_notices.pop_front();
            }
            self.metric_notices.push_back(notice.clone());
        }
    }

    /// Recent local measurement effects; older details have been folded into the reducer's
    /// bounded request summary and are intentionally no longer retained here.
    pub fn backend_metric_notices(&self) -> impl Iterator<Item = &backend::MetricNotice> {
        self.metric_notices.iter()
    }

    pub fn backend_snapshot(&self) -> &plx_playback_contract::Snapshot {
        self.backend.snapshot()
    }

    /// One bounded local summary, with no URL, title, token or server identity.
    pub fn backend_metrics_snapshot(&self) -> Option<&super::attempt_metrics::RequestSummary<
        backend::LogicalRequestId, AttemptId>> {
        self.backend.metrics_snapshot()
    }

    /// The same fresh offer decision the start door will use. IO retires an expired URI
    /// authorization before the app arms any backend-specific foreground transport hold.
    pub fn planned_start_backend(&mut self, ps: &mut crate::route::PlaybackSession)
        -> Option<BackendKind> {
        #[cfg(feature = "play-uri")]
        self.refresh_uri_qualification(ps);
        ps.planned_start_backend()
    }

    /// Product URI plane ownership is separate from the diagnostic probe and panel evidence.
    #[cfg(feature = "play-uri")]
    pub fn uri_product_plane_owned(&self) -> bool {
        self.uri_backend.as_ref().is_some_and(UriBackend::plane_owned)
    }

    /// A mounted page is not the owner of a physical attempt. A URI EOS may have already
    /// released its native session while its final Plex stop still belongs to the route.
    pub fn playback_needs_teardown(&self, ps: &crate::route::PlaybackSession) -> bool {
        #[cfg(feature = "play-feed")]
        if self.is_live() { return true; }
        #[cfg(feature = "play-uri")]
        if self.uri_route_attempt.is_some()
            || (!self.uri_finalized
                && ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri)
                && ps.attempt_view().facts().ended == Some(true)) {
            return true;
        }
        let _ = ps;
        false
    }

    /// A pending first Play keeps the native pause hold until its exact startup command is
    /// accepted. Viewer toggles read the common pending goal during that interval.
    pub fn backend_viewer_paused(&self) -> Option<bool> {
        let snapshot = self.backend.snapshot();
        let (Some(backend), Some(attempt)) = (snapshot.backend, snapshot.attempt) else {
            return None;
        };
        self.backend.viewer_paused(backend, attempt)
    }

    /// Route a viewer transport change through the active common attempt. An absent attempt
    /// leaves legacy no-engine transport handling to the caller.
    pub fn backend_transport_control(&mut self, paused: bool) -> Option<bool> {
        let snapshot = self.backend.snapshot();
        let (Some(backend), Some(attempt)) = (snapshot.backend, snapshot.attempt) else {
            return None;
        };
        let operation = if paused { Operation::Pause } else { Operation::Play };
        let at = self.backend_tick();
        let result = self.backend_send_at(Intent::Control { backend, attempt, operation },
            super::attempt_metrics::Origin::Fresh, at);
        Some(!result.playback.iter().any(|fx| matches!(fx,
            plx_playback_contract::Fx::Rejected { .. }
                | plx_playback_contract::Fx::CommandRefused { .. }
        )))
    }

    /// The app's one seek door. URI uses the addressed runner; Feed retains its existing
    /// coalescing/reload planner. An unavailable physical backend refuses without global writes.
    pub fn request_seek(&mut self, ps: &mut crate::route::PlaybackSession, target_ns: i64) -> bool {
        #[cfg(feature = "play-uri")]
        if let Some(accepted) = self.seek_uri(ps, target_ns) { return accepted; }
        #[cfg(feature = "play-feed")]
        {
            let _ = ps;
            super::request_seek(target_ns);
            return true;
        }
        #[cfg(not(feature = "play-feed"))]
        { let _ = (ps, target_ns); false }
    }

    /// Playback-rate conversion is a Feed capability. A URI attempt never mutates the Feed
    /// rate or requests its reload as a substitute for a missing native URI operation.
    pub fn set_play_rate(&mut self, ps: &mut crate::route::PlaybackSession, permille: i32) -> bool {
        if ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri) {
            return false;
        }
        #[cfg(feature = "play-feed")]
        { super::set_play_rate(ps, permille) }
        #[cfg(not(feature = "play-feed"))]
        { let _ = (ps, permille); false }
    }

    /// `None` means the active route is Feed. `Some(false)` is an explicit URI refusal, so a
    /// caller must never fall through to Feed track or sidecar controls for this route.
    #[cfg(feature = "play-uri")]
    pub fn select_uri_audio(&mut self, ps: &crate::route::PlaybackSession,
        audio: crate::route::CarriedAudio,
        memo: Option<plx_plex::plex::session::prefs::AudioMemo>) -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) { return None; }
        if crate::route::cur_sub_mode(ps) == crate::route::SubMode::ForcedOnly {
            return Some(false);
        }
        let Some(route_attempt) = self.uri_route_attempt else { return Some(false); };
        let Ok(id) = u64::try_from(audio.sid) else { return Some(false); };
        let attempt = route_attempt.contract_id();
        let operation = Operation::SelectTrack { kind: plx_playback_contract::TrackKind::Audio,
            choice: plx_playback_contract::TrackChoice::Selected(id) };
        let result = self.backend_send_at(Intent::Control { backend: BackendKind::Uri, attempt,
            operation },
            super::attempt_metrics::Origin::Fresh, self.backend_tick());
        if result.playback.iter().any(|fx| matches!(fx,
            plx_playback_contract::Fx::Rejected { .. }
                | plx_playback_contract::Fx::CommandRefused { .. })) { return Some(false); }
        let Some(command) = result.controls.iter().find(|sent|
            (sent.backend, sent.attempt, sent.operation) == (BackendKind::Uri, attempt, operation))
            .map(|sent| sent.command) else { return Some(false); };
        self.pending_uri_audio = Some(PendingUriAudio { attempt, command, audio, memo });
        Some(true)
    }

    #[cfg(feature = "play-uri")]
    pub fn select_uri_subtitle(&mut self, ps: &crate::route::PlaybackSession,
        ordinal: i32, stream_id: i64, has_sidecar: bool,
        memo: Option<plx_plex::plex::session::prefs::SubtitleMemo>) -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) { return None; }
        // Off and external sidecars have no confirmed native operation in this transport.
        let Ok(id) = u64::try_from(stream_id) else { return Some(false); };
        if id == 0 || ordinal < 0 || has_sidecar { return Some(false); }
        let Some(route_attempt) = self.uri_route_attempt else { return Some(false); };
        let attempt = route_attempt.contract_id();
        let operation = Operation::SelectTrack { kind: plx_playback_contract::TrackKind::Subtitle,
            choice: plx_playback_contract::TrackChoice::Selected(id) };
        let result = self.backend_send_at(Intent::Control { backend: BackendKind::Uri, attempt,
            operation },
            super::attempt_metrics::Origin::Fresh, self.backend_tick());
        if result.playback.iter().any(|fx| matches!(fx,
            plx_playback_contract::Fx::Rejected { .. }
                | plx_playback_contract::Fx::CommandRefused { .. })) { return Some(false); }
        let Some(command) = result.controls.iter().find(|sent|
            (sent.backend, sent.attempt, sent.operation) == (BackendKind::Uri, attempt, operation))
            .map(|sent| sent.command) else { return Some(false); };
        self.pending_uri_subtitle = Some(PendingUriSubtitle {
            attempt, command, ordinal, stream_id, memo,
        });
        Some(true)
    }

    pub fn commit_audio_track(&mut self, ps: &mut crate::route::PlaybackSession,
        audio: crate::route::CarriedAudio,
        memo: Option<plx_plex::plex::session::prefs::AudioMemo>) -> bool {
        #[cfg(feature = "play-uri")]
        if let Some(accepted) = self.select_uri_audio(ps, audio.clone(), memo.clone()) {
            return accepted;
        }
        #[cfg(feature = "play-feed")]
        { crate::route::commit_audio_pick(ps, audio, memo) }
        #[cfg(not(feature = "play-feed"))]
        { let _ = (ps, audio, memo); false }
    }

    pub fn commit_subtitle_track(&mut self, ps: &mut crate::route::PlaybackSession,
        ordinal: i32, stream_id: i64, sidecar_key: Option<String>, sidecar_codec: String,
        memo: Option<plx_plex::plex::session::prefs::SubtitleMemo>) -> bool {
        #[cfg(feature = "play-uri")]
        if let Some(accepted) = self.select_uri_subtitle(ps, ordinal, stream_id,
            sidecar_key.is_some(), memo.clone()) {
            return accepted;
        }
        #[cfg(feature = "play-feed")]
        {
            crate::route::commit_subtitle_and_sidecar(ps, ordinal, stream_id,
                sidecar_key, sidecar_codec, memo);
            true
        }
        #[cfg(not(feature = "play-feed"))]
        { let _ = (ps, ordinal, stream_id, sidecar_key, sidecar_codec, memo); false }
    }

    pub fn commit_forced_only_track(&mut self, ps: &mut crate::route::PlaybackSession,
        ordinal: i32, stream_id: i64, lang: &str, changed: bool) -> bool {
        if ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri) {
            return false;
        }
        #[cfg(feature = "play-feed")]
        {
            crate::route::commit_forced_only_and_sidecar(ps, ordinal, stream_id, lang, changed);
            true
        }
        #[cfg(not(feature = "play-feed"))]
        { let _ = (ps, ordinal, stream_id, lang, changed); false }
    }

    pub fn commit_audio_enhancement(&mut self, ps: &mut crate::route::PlaybackSession,
        enhancement: plx_plex::plex::AudioEnhancements) -> bool {
        if ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri) {
            return false;
        }
        #[cfg(feature = "play-feed")]
        { super::request_audio_enhancement(ps, enhancement); true }
        #[cfg(not(feature = "play-feed"))]
        { let _ = (ps, enhancement); false }
    }

    /// Address a seek to the active URI attempt. A refused command changes no Feed mailbox or
    /// route selection; the visible target is held until that command's native landing report.
    #[cfg(feature = "play-uri")]
    pub fn seek_uri(&mut self, ps: &mut crate::route::PlaybackSession, target_ns: i64)
        -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) {
            return None;
        }
        let Some(route_attempt) = self.uri_route_attempt else { return Some(false); };
        let attempt = route_attempt.contract_id();
        let target_ms = u64::try_from(target_ns.max(0) / 1_000_000).ok()?;
        let result = self.backend_send_at(Intent::Control { backend: BackendKind::Uri,
            attempt, operation: Operation::Seek(plx_playback_contract::ContentTime::from_millis(target_ms)) },
            super::attempt_metrics::Origin::Fresh, self.backend_tick());
        if result.playback.iter().any(|fx| matches!(fx,
            plx_playback_contract::Fx::Rejected { .. }
                | plx_playback_contract::Fx::CommandRefused { .. })) {
            return Some(false);
        }
        let mut facts = *ps.attempt_view().facts();
        facts.ended = Some(false);
        facts.seek_pending_ns = Some(target_ns.max(0));
        facts.seek_display_ns = Some(target_ns.max(0));
        facts.status = Some(plx_playback_contract::readout::PlaybackState::Seeking);
        let _ = ps.ingest_attempt_facts(route_attempt, super::attempt_view::Backend::Uri, facts);
        Some(true)
    }

    /// Execute one route-owned intent through the sole backend admission state. The caller
    /// supplies the physical attempt's origin and a monotonic tick for local measurements.
    pub fn backend_send_at(&mut self, intent: Intent, origin: super::attempt_metrics::Origin,
        at: super::attempt_metrics::Tick) -> backend::RunResult {
        let mut drivers = PhysicalDrivers {
            marker: std::marker::PhantomData,
            #[cfg(feature = "play-feed")]
            feed: &mut self.feed,
            #[cfg(feature = "play-feed")]
            feed_driver: &mut self.feed_driver,
            #[cfg(feature = "play-uri")]
            uri: &mut self.uri_backend,
        };
        let result = self.backend.send_at(&mut drivers, &self.mt, intent, origin, at);
        let result = self.finish_backend_actions(result, at);
        #[cfg(feature = "play-uri")]
        {
            if let Intent::Stop { backend: BackendKind::Uri, attempt, .. } = intent {
                let refused = result.playback.iter().any(|notice| matches!(notice,
                    plx_playback_contract::Fx::Rejected {
                        backend: BackendKind::Uri, attempt: rejected, ..
                    } | plx_playback_contract::Fx::CommandRefused {
                        backend: BackendKind::Uri, attempt: rejected,
                        operation: Operation::Stop(_), ..
                    } if *rejected == attempt));
                if !refused && result.controls.iter().any(|sent| sent.backend == BackendKind::Uri
                    && sent.attempt == attempt && matches!(sent.operation, Operation::Stop(_))) {
                    if self.uri_route_attempt.is_some_and(|route| route.contract_id() == attempt) {
                        self.uri_stop_requested = true;
                    }
                    self.release_step(&super::release::ReleaseEvent::UriReleaseStarted {
                        attempt, at: std::time::Instant::now(),
                    }, &mut Vec::new());
                }
            }
            self.capture_uri_release(&result.playback);
        }
        self.record_backend_metrics(&result.metrics);
        result
    }

    /// Poll both physical executors even when no player page is mounted. A terminal observation
    /// and a same-poll release retain their order in the returned addressed results.
    pub fn backend_poll_at(&mut self, at: super::attempt_metrics::Tick) -> backend::RunResult {
        let mut drivers = PhysicalDrivers {
            marker: std::marker::PhantomData,
            #[cfg(feature = "play-feed")]
            feed: &mut self.feed,
            #[cfg(feature = "play-feed")]
            feed_driver: &mut self.feed_driver,
            #[cfg(feature = "play-uri")]
            uri: &mut self.uri_backend,
        };
        let result = self.backend.poll_at(&mut drivers, &self.mt, at);
        let result = self.finish_backend_actions(result, at);
        #[cfg(feature = "play-uri")]
        self.capture_uri_release(&result.playback);
        self.record_backend_metrics(&result.metrics);
        result
    }

    #[cfg(feature = "play-uri")]
    fn capture_uri_release(&mut self, notices: &[plx_playback_contract::Fx]) {
        for notice in notices {
            if let plx_playback_contract::Fx::ReleaseOutcome {
                backend: BackendKind::Uri, attempt, status,
            } = *notice {
                let mut fx = Vec::new();
                self.release_step(&match status {
                    ReleaseStatus::Closing | ReleaseStatus::Quarantined =>
                        super::release::ReleaseEvent::UriReleaseStarted {
                            attempt, at: std::time::Instant::now(),
                        },
                    ReleaseStatus::Released =>
                        super::release::ReleaseEvent::UriReleased { attempt },
                }, &mut fx);
                self.release_fx.extend(fx);
            }
        }
    }

    pub fn backend_set_foreground_at(&mut self, foreground: bool,
        at: super::attempt_metrics::Tick) -> backend::RunResult {
        let mut drivers = PhysicalDrivers {
            marker: std::marker::PhantomData,
            #[cfg(feature = "play-feed")]
            feed: &mut self.feed,
            #[cfg(feature = "play-feed")]
            feed_driver: &mut self.feed_driver,
            #[cfg(feature = "play-uri")]
            uri: &mut self.uri_backend,
        };
        let result = self.backend.set_foreground_at(&mut drivers, &self.mt, foreground, at);
        let result = self.finish_backend_actions(result, at);
        self.record_backend_metrics(&result.metrics);
        result
    }

    /// The native user transport door owns TX, Starfish and ACB together. A queued Feed control
    /// is executed only after the runner's temporary driver borrow ends, then its addressed
    /// acceptance is returned through the same runner before another command can advance.
    fn finish_backend_actions(&mut self, result: backend::RunResult,
        at: super::attempt_metrics::Tick) -> backend::RunResult {
        #[cfg(feature = "play-feed")]
        let mut result = result;
        #[cfg(feature = "play-feed")]
        while let Some(action) = self.feed_driver.take_transport_action() {
            let transport = match action {
                FeedTransportAction::Play { .. } | FeedTransportAction::ResumeInitial { .. } =>
                    super::resume_result(self),
                FeedTransportAction::Pause { .. } => super::pause_result(self),
            };
            self.feed_driver.note_transport_result(&mut self.feed, &self.mt, action,
                transport.accepted, transport.native_acknowledged);
            let mut drivers = PhysicalDrivers {
                marker: std::marker::PhantomData,
                #[cfg(feature = "play-feed")]
                feed: &mut self.feed,
                #[cfg(feature = "play-feed")]
                feed_driver: &mut self.feed_driver,
                #[cfg(feature = "play-uri")]
                uri: &mut self.uri_backend,
            };
            let next = self.backend.poll_at(&mut drivers, &self.mt, at);
            result.playback.extend(next.playback);
            result.metrics.extend(next.metrics);
            result.controls.extend(next.controls);
        }
        #[cfg(not(feature = "play-feed"))]
        let _ = at;
        result
    }

    pub fn backend_exit_disposition(&self) -> ExitDisposition {
        if self.backend.snapshot().phase != plx_playback_contract::Phase::Vacant
            || {
                #[cfg(feature = "play-feed")]
                { self.feed_driver.exit_disposition(&self.feed) == ExitDisposition::PreserveNativeOwnership }
                #[cfg(not(feature = "play-feed"))]
                { false }
            } {
            return ExitDisposition::PreserveNativeOwnership;
        }
        #[cfg(feature = "play-uri")]
        if self.uri_backend.as_ref().is_some_and(|driver|
            driver.exit_disposition() == ExitDisposition::PreserveNativeOwnership) {
            return ExitDisposition::PreserveNativeOwnership;
        }
        ExitDisposition::Clean
    }

    /// Capture the installed display and URI bus at the playback IO boundary. Current product
    /// evidence remains unqualified; returning a port never certifies a source by itself.
    #[cfg(feature = "play-uri")]
    pub fn install_uri_product(&mut self, session: &mut crate::route::PlaybackSession) -> bool {
        let display = plx_platform::tv::sink::display_installed();
        let evidence = crate::route::uri_preflight::ProductEvidence::unqualified(
            display.window_mode());
        let port = crate::route::uri_preflight::ResolvePort::new(evidence,
            std::sync::Arc::new(crate::route::uri_preflight::ProductQualifier));
        self.install_uri_product_with_ports(session, port,
            plx_platform::tv::uri::installed(), display)
    }

    /// One boot composition for both real and injected ports. The exact evidence configures the
    /// driver and is handed to the resolver only after physical installation succeeds.
    #[cfg(feature = "play-uri")]
    pub fn install_uri_product_with_ports(&mut self,
        session: &mut crate::route::PlaybackSession,
        port: crate::route::uri_preflight::ResolvePort,
        transport: &'static dyn plx_platform::tv::uri::UriMedia,
        display: &'static dyn plx_platform::tv::sink::DisplaySink) -> bool {
        let evidence = port.evidence();
        let settings = UriDriverSettings {
            clock: Box::new(ProductUriClock(std::time::Instant::now())),
            app_id: plx_base::paths::app_id().to_owned(),
            binding: evidence.binding,
            capabilities: evidence.certified,
            startup_timeout_ms: 15_000,
            release_allowance_ms: 10_000,
            source_ttl_ms: 30_000,
        };
        if self.install_uri_backend_with_ports(settings, transport, display).is_err() {
            return false;
        }
        session.install_uri_preflight_port(port);
        true
    }

    /// Install only an explicitly configured URI transport/display pair while no physical
    /// attempt owns the player. Installation supplies no source-specific qualification and
    /// therefore does not change the route's default Feed choice.
    #[cfg(feature = "play-uri")]
    pub fn install_uri_backend(&mut self, settings: UriDriverSettings)
        -> Result<(), UriDriverSettings> {
        self.install_uri_backend_with_ports(settings,
            plx_platform::tv::uri::installed(),
            plx_platform::tv::sink::display_installed())
    }

    /// IO composition may pass captured platform ports directly. Product boot supplies the
    /// installed pair; a host rig can supply the same interfaces without a process-wide port.
    #[cfg(feature = "play-uri")]
    pub fn install_uri_backend_with_ports(&mut self, settings: UriDriverSettings,
        transport: &'static dyn plx_platform::tv::uri::UriMedia,
        display: &'static dyn plx_platform::tv::sink::DisplaySink)
        -> Result<(), UriDriverSettings> {
        if self.uri_backend.is_some() || self.backend_exit_disposition() != ExitDisposition::Clean
            || settings.capabilities.packet_feed || settings.capabilities.audio_off
            || settings.capabilities.subtitle_off {
            return Err(settings);
        }
        let config = UriConfig {
            transport,
            display,
            clock: settings.clock,
            app_id: settings.app_id,
            binding: settings.binding,
            capabilities: settings.capabilities,
            startup_timeout_ms: settings.startup_timeout_ms,
            release_allowance_ms: settings.release_allowance_ms,
            source_ttl_ms: settings.source_ttl_ms,
        };
        match self.install_uri_backend_config(config) {
            Ok(()) => Ok(()),
            Err(config) => Err(UriDriverSettings {
                clock: config.clock, app_id: config.app_id, binding: config.binding,
                capabilities: config.capabilities,
                startup_timeout_ms: config.startup_timeout_ms,
                release_allowance_ms: config.release_allowance_ms,
                source_ttl_ms: config.source_ttl_ms,
            }),
        }
    }

    /// The same production installation seam with injected IO ports for deterministic tests.
    #[cfg(feature = "play-uri")]
    fn install_uri_backend_config(&mut self, config: UriConfig) -> Result<(), UriConfig> {
        if self.uri_backend.is_some()
            || self.backend_exit_disposition() != ExitDisposition::Clean {
            return Err(config);
        }
        self.uri_backend = Some(UriBackend::new(config));
        Ok(())
    }

    /// Escrow an exact resolved URL and its source-specific native track map. The route owner
    /// still has to supply a qualified offer and send an addressed Start through the runner.
    #[cfg(feature = "play-uri")]
    pub fn stage_uri_source(&mut self, attempt: AttemptId, source: UriSource)
        -> Result<SourceKey, Failure> {
        let key = self.next_source_key();
        self.uri_backend.as_mut().ok_or(Failure::Unavailable)?
            .stage_source(attempt, key, source)?;
        Ok(key)
    }

    /// Accept source-specific qualification only for this session's current Part and selected
    /// streams. The external qualifier supplies an expiry; a deferred native release cannot
    /// retain an authorization past that deadline.
    #[cfg(feature = "play-uri")]
    pub fn offer_qualified_uri(&mut self, ps: &mut crate::route::PlaybackSession,
        offer: QualifiedUriOffer) -> Result<(), Failure> {
        if self.uri_backend.is_none() { return Err(Failure::Unavailable); }
        if crate::route::is_preview(ps) { return Err(Failure::Unsupported); }
        if offer.selection != ps.uri_selection()
            || crate::route::pending_route_start() != Some(offer.route_start)
            || offer.expires_at <= std::time::Instant::now() {
            return Err(Failure::Stale);
        }
        if !uri_offer_executable(ps, &offer) { return Err(Failure::Unsupported); }
        if !ps.ensure_backend_offers() { return Err(Failure::Stale); }
        let offer_id = offer.offer_id;
        if let Some(previous) = self.qualified_uri.replace(offer) {
            ps.qualify_backend_offer(BackendKind::Uri, previous.offer_id,
                crate::route::backend_policy::Qualification::Unknown);
        }
        ps.qualify_backend_offer(BackendKind::Uri,
            offer_id,
            crate::route::backend_policy::Qualification::Qualified);
        Ok(())
    }

    /// Consume only the exact still-authorized source after the route has won its native claim.
    #[cfg(feature = "play-uri")]
    pub(super) fn take_qualified_uri(&mut self, ps: &mut crate::route::PlaybackSession,
        route_start: crate::route::RouteStartTransaction)
        -> Option<QualifiedUriOffer> {
        let offer = self.qualified_uri.take()?;
        if offer.route_start == route_start && offer.selection == ps.uri_selection()
            && offer.expires_at > std::time::Instant::now()
            && uri_offer_executable(ps, &offer) {
            Some(offer)
        } else {
            ps.qualify_backend_offer(BackendKind::Uri,
                offer.offer_id,
                crate::route::backend_policy::Qualification::Unknown);
            None
        }
    }

    /// A cancelled queued start has no future claim boundary. Drop only the authorization
    /// attached to that route ticket; a newer replacement's escrow stays intact.
    #[cfg(feature = "play-uri")]
    pub(super) fn discard_qualified_uri_for(&mut self, ps: &mut crate::route::PlaybackSession,
        route_start: crate::route::RouteStartTransaction) {
        if self.qualified_uri.as_ref().is_some_and(|offer| offer.route_start == route_start) {
            if let Some(offer) = self.qualified_uri.take() {
                ps.qualify_backend_offer(BackendKind::Uri, offer.offer_id,
                    crate::route::backend_policy::Qualification::Unknown);
            }
        }
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn drop_qualified_uri_for_exit(&mut self,
        route_start: crate::route::RouteStartTransaction) {
        if self.qualified_uri.as_ref().is_some_and(|offer| offer.route_start == route_start) {
            self.qualified_uri = None;
        }
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn take_release_effects(&mut self) -> Vec<super::release::ReleaseEffect> {
        std::mem::take(&mut self.release_fx)
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn refresh_uri_qualification(&mut self, ps: &mut crate::route::PlaybackSession) {
        if self.qualified_uri.as_ref().is_some_and(|offer|
            offer.expires_at <= std::time::Instant::now()
                || offer.selection != ps.uri_selection()
                || !uri_offer_executable(ps, offer)
                || crate::route::pending_route_start() != Some(offer.route_start)) {
            if let Some(expired) = self.qualified_uri.take() {
                ps.qualify_backend_offer(BackendKind::Uri, expired.offer_id,
                    crate::route::backend_policy::Qualification::Unknown);
            }
        }
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn bind_uri_attempt(&mut self, attempt: crate::route::RouteStartAttempt,
        offer_id: crate::route::backend_policy::OfferId) {
        self.uri_route_attempt = Some(attempt);
        self.uri_offer_id = Some(offer_id);
        self.uri_recovery = None;
        self.uri_route_settled = false;
        self.uri_played_once = false;
        self.uri_stop_requested = false;
        self.uri_finalized = false;
        self.pending_uri_audio = None;
        self.pending_uri_subtitle = None;
    }

    #[cfg(feature = "play-uri")]
    pub fn take_released_uri_recovery(&mut self) -> Option<UriRecoveryNotice> {
        if self.uri_recovery.is_some_and(|pending| pending.released) {
            self.uri_recovery.take().map(|pending| pending.notice)
        } else { None }
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn cancel_uri_recovery(&mut self, ps: &mut crate::route::PlaybackSession) {
        if let Some(pending) = self.uri_recovery.take() {
            ps.clear_uri_recovery_pending(pending.notice.attempt);
        }
    }

    #[cfg(feature = "play-uri")]
    fn note_uri_failure(&mut self, ps: &mut crate::route::PlaybackSession,
        attempt: AttemptId, viewer_paused: Option<bool>) {
        let Some(route_attempt) = self.uri_route_attempt.filter(|route|
            route.contract_id() == attempt) else { return; };
        if self.uri_recovery.as_ref().is_some_and(|pending|
            pending.notice.attempt == route_attempt) { return; }
        let Some(offer_id) = self.uri_offer_id else { return; };
        let phase = if self.uri_played_once { crate::route::UriFailurePhase::Runtime }
            else { crate::route::UriFailurePhase::Startup };
        let known_position_ns = super::intended_pos_known_ns(ps);
        if !ps.can_retry_uri_as_feed(route_attempt, offer_id, phase, known_position_ns) { return; }
        let Some(viewer_paused) = viewer_paused else {
            return;
        };
        if !ps.mark_uri_recovery_pending(route_attempt) { return; }
        if let Some(request) = ps.metric_request() {
            self.begin_automatic_handoff(request, attempt);
        }
        self.uri_recovery = Some(PendingUriRecovery { notice: UriRecoveryNotice {
            attempt: route_attempt, offer_id, phase, known_position_ns, viewer_paused,
        }, released: false });
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn finalize_uri_once(&mut self) -> bool {
        if self.uri_finalized { false } else {
            self.uri_finalized = true;
            true
        }
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn active_uri_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        self.uri_route_attempt
    }

    #[cfg(feature = "play-uri")]
    pub(super) fn uri_backend_exit_pending(&self) -> bool {
        self.uri_backend.as_ref().is_some_and(|driver|
            driver.exit_disposition() == ExitDisposition::PreserveNativeOwnership)
    }

    /// A quarantined native registration must survive the stack unwind before `_exit`.
    #[cfg(feature = "play-uri")]
    pub(super) fn park_uri_backend_for_exit(&mut self) {
        if let Some(driver) = self.uri_backend.take() { std::mem::forget(driver); }
    }

    /// Land URI readiness/failure against the same route transaction as a Feed Load receipt.
    /// A generic queue answer or clock observation cannot settle the start.
    pub fn backend_poll_session_at(&mut self, ps: &mut crate::route::PlaybackSession,
        at: super::attempt_metrics::Tick) -> backend::RunResult {
        #[cfg(feature = "play-uri")]
        self.refresh_uri_qualification(ps);
        #[cfg(feature = "play-uri")]
        let uri_viewer_goal = self.uri_route_attempt.and_then(|route|
            self.backend.viewer_paused(BackendKind::Uri, route.contract_id()));
        let result = self.backend_poll_at(at);
        #[cfg(feature = "play-uri")]
        let mut failed_startup = None;
        #[cfg(feature = "play-uri")]
        for notice in &result.playback {
            if let plx_playback_contract::Fx::AcceptedObservation {
                backend: BackendKind::Uri, attempt, observation,
            } = *notice {
                if matches!(observation, plx_playback_contract::Observation::Playback(observed)
                    if observed.value == Playback::Playing)
                    && self.uri_route_attempt.is_some_and(|route| route.contract_id() == attempt) {
                    self.uri_played_once = true;
                }
                if matches!(observation, plx_playback_contract::Observation::Failed(_)) {
                    self.note_uri_failure(ps, attempt, uri_viewer_goal);
                }
                self.publish_uri_observation(ps, attempt, observation);
            }
            match *notice {
                plx_playback_contract::Fx::AcceptedObservation {
                    backend: BackendKind::Uri, attempt,
                    observation: plx_playback_contract::Observation::Playback(observed),
                } if observed.value == plx_playback_contract::Playback::ReadyAwaitPlay => {
                    if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                        route.contract_id() == attempt && !self.uri_route_settled) {
                        self.uri_route_settled = crate::route::settle_route_start(
                            ps, route_attempt, crate::route::RouteStartResult::Started);
                    }
                }
                plx_playback_contract::Fx::AcceptedObservation {
                    backend: BackendKind::Uri, attempt,
                    observation: plx_playback_contract::Observation::Failed(_),
                } => {
                    if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                        route.contract_id() == attempt && !self.uri_route_settled) {
                        self.uri_route_settled = crate::route::settle_route_start(
                            ps, route_attempt, crate::route::RouteStartResult::StartFailed);
                    }
                }
                plx_playback_contract::Fx::StartupFailed {
                    backend: BackendKind::Uri, attempt, failure, ..
                } => {
                    failed_startup = Some(attempt);
                    self.note_uri_failure(ps, attempt, uri_viewer_goal);
                    self.publish_uri_observation(ps, attempt,
                        plx_playback_contract::Observation::Failed(failure));
                    if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                        route.contract_id() == attempt && !self.uri_route_settled) {
                        self.uri_route_settled = crate::route::settle_route_start(
                            ps, route_attempt, crate::route::RouteStartResult::StartFailed);
                    }
                }
                plx_playback_contract::Fx::AcceptedSeekLanded {
                    backend: BackendKind::Uri, attempt, position, ..
                } => {
                    if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                        route.contract_id() == attempt) {
                        let mut facts = *ps.attempt_view().facts();
                        facts.seek_pending_ns = None;
                        facts.seek_display_ns = None;
                        if let Some(position) = position {
                            facts.position_ns = position.value.as_millis()
                                .checked_mul(1_000_000)
                                .and_then(|ns| i64::try_from(ns).ok());
                        }
                        facts.status = Some(match self.backend.snapshot().playback.map(|p| p.value) {
                            Some(Playback::Playing) => plx_playback_contract::readout::PlaybackState::Playing,
                            Some(Playback::Paused) => plx_playback_contract::readout::PlaybackState::Paused,
                            _ => plx_playback_contract::readout::PlaybackState::Buffering,
                        });
                        let _ = ps.ingest_attempt_facts(route_attempt,
                            super::attempt_view::Backend::Uri, facts);
                    }
                }
                plx_playback_contract::Fx::CommandRefused {
                    backend: BackendKind::Uri, attempt,
                    operation: Operation::Seek(_), ..
                } => {
                    if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                        route.contract_id() == attempt) {
                        let mut facts = *ps.attempt_view().facts();
                        facts.seek_pending_ns = None;
                        facts.seek_display_ns = None;
                        facts.status = Some(match self.backend.snapshot().playback.map(|p| p.value) {
                            Some(Playback::Playing) => plx_playback_contract::readout::PlaybackState::Playing,
                            Some(Playback::Paused) => plx_playback_contract::readout::PlaybackState::Paused,
                            _ => plx_playback_contract::readout::PlaybackState::Buffering,
                        });
                        let _ = ps.ingest_attempt_facts(route_attempt,
                            super::attempt_view::Backend::Uri, facts);
                    }
                }
                plx_playback_contract::Fx::AcceptedTrackSettled {
                    backend: BackendKind::Uri, attempt, command,
                    kind: plx_playback_contract::TrackKind::Audio,
                    choice: plx_playback_contract::TrackChoice::Selected(id),
                } => {
                    if self.uri_route_attempt.is_some_and(|route| route.contract_id() == attempt)
                        && self.pending_uri_audio.as_ref().is_some_and(|pending|
                            pending.attempt == attempt && pending.command == command
                                && u64::try_from(pending.audio.sid).ok() == Some(id)) {
                        let pending = self.pending_uri_audio.take().expect("matched URI audio pick");
                        crate::route::confirm_uri_audio_pick(ps, pending.audio, pending.memo);
                    }
                }
                plx_playback_contract::Fx::AcceptedTrackSettled {
                    backend: BackendKind::Uri, attempt, command,
                    kind: plx_playback_contract::TrackKind::Subtitle,
                    choice: plx_playback_contract::TrackChoice::Selected(id),
                } => {
                    if self.uri_route_attempt.is_some_and(|route| route.contract_id() == attempt)
                        && self.pending_uri_subtitle.as_ref().is_some_and(|pending|
                            pending.attempt == attempt && pending.command == command
                                && u64::try_from(pending.stream_id).ok() == Some(id)) {
                        let pending = self.pending_uri_subtitle.take().expect("matched URI subtitle pick");
                        crate::route::confirm_uri_subtitle_pick(ps, pending.ordinal,
                            pending.stream_id, pending.memo);
                    }
                }
                plx_playback_contract::Fx::CommandRefused {
                    backend: BackendKind::Uri, attempt, command,
                    operation: Operation::SelectTrack { kind, .. }, ..
                } => match kind {
                    plx_playback_contract::TrackKind::Audio => {
                        if self.pending_uri_audio.as_ref().is_some_and(|pending|
                            pending.attempt == attempt && pending.command == command) {
                            self.pending_uri_audio = None;
                        }
                    }
                    plx_playback_contract::TrackKind::Subtitle => {
                        if self.pending_uri_subtitle.as_ref().is_some_and(|pending|
                            pending.attempt == attempt && pending.command == command) {
                            self.pending_uri_subtitle = None;
                        }
                    }
                },
                plx_playback_contract::Fx::ReleaseOutcome {
                    backend: BackendKind::Uri, attempt, status,
                } => {
                    if self.uri_recovery.as_ref().is_some_and(|pending|
                        pending.notice.attempt.contract_id() == attempt) {
                        if status == ReleaseStatus::Released {
                            if let Some(pending) = &mut self.uri_recovery {
                                pending.released = true;
                            }
                        } else if status == ReleaseStatus::Quarantined {
                            self.cancel_uri_recovery(ps);
                        }
                    }
                    if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                        route.contract_id() == attempt && !self.uri_route_settled) {
                        if !self.uri_stop_requested {
                            self.publish_uri_observation(ps, attempt,
                                plx_playback_contract::Observation::Failed(
                                    if status == ReleaseStatus::Quarantined {
                                        Failure::ReleaseUncertain
                                    } else { Failure::Backend }));
                        }
                        self.uri_route_settled = crate::route::settle_route_start(
                            ps, route_attempt, crate::route::RouteStartResult::StartFailed);
                    }
                    if matches!(notice, plx_playback_contract::Fx::ReleaseOutcome {
                        status: ReleaseStatus::Released, .. }) {
                        if let Some(route_attempt) = self.uri_route_attempt.filter(|route|
                            route.contract_id() == attempt) {
                            // Replacement did not run the full stop ritual. Retire this exact
                            // reporter before the release queue starts another backend; its
                            // mailbox stop is sticky and dropping the handle does not join here.
                            let _ = self.stop_timeline(route_attempt);
                        }
                        self.uri_route_attempt = None;
                        self.uri_offer_id = None;
                        self.uri_stop_requested = false;
                        self.pending_uri_audio = None;
                        self.pending_uri_subtitle = None;
                    }
                }
                _ => {}
            }
        }
        #[cfg(feature = "play-uri")]
        if let Some(attempt) = failed_startup.filter(|attempt|
            self.uri_route_attempt.is_some_and(|route| route.contract_id() == *attempt)) {
            // The common startup verdict alone distinguishes mandatory initial selection,
            // seek or Play from a later interactive refusal. Retire its exact native owner.
            let _ = self.backend_send_at(Intent::Stop { backend: BackendKind::Uri, attempt,
                reason: plx_playback_contract::StopReason::Replace },
                super::attempt_metrics::Origin::Fresh, at);
        }
        #[cfg(feature = "play-uri")]
        self.publish_uri_timeline(ps);
        #[cfg(feature = "play-uri")]
        {
            let mut release_fx = std::mem::take(&mut self.release_fx);
            if self.uri_route_attempt.is_some() && self.release.is_busy() {
                self.release_step(&super::release::ReleaseEvent::Tick {
                    now: std::time::Instant::now(),
                }, &mut release_fx);
            }
            super::route_start::run_uri_queued_effects(ps, self, release_fx);
        }
        #[cfg(not(feature = "play-uri"))]
        let _ = ps;
        result
    }

    /// The existing attempt-bound reporter consumes normalized URI clocks too. Missing native
    /// position or a non-playing state clears its mailbox, so it cannot repeat an old playhead.
    #[cfg(feature = "play-uri")]
    fn publish_uri_timeline(&self, ps: &crate::route::PlaybackSession) {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) { return; }
        let Some(attempt) = ps.attempt_view().attempt() else { return; };
        let facts = ps.attempt_view().facts();
        let snapshot = match (facts.position_ns, facts.duration_ns, facts.status) {
            (Some(position), Some(duration), Some(status)) if matches!(status,
                plx_playback_contract::readout::PlaybackState::Playing
                | plx_playback_contract::readout::PlaybackState::Paused) =>
                timeline::TimelineSnapshot::from_content_ns(position, duration,
                    status == plx_playback_contract::readout::PlaybackState::Paused),
            _ => None,
        };
        self.publish_timeline(attempt, snapshot);
    }

    /// Map only addressed URI reports into the session's sparse readout. Playing and clock
    /// remain service reports; neither claims panel presentation or a Feed TX start flag.
    #[cfg(feature = "play-uri")]
    fn publish_uri_observation(&self, ps: &mut crate::route::PlaybackSession,
        attempt: AttemptId, observation: plx_playback_contract::Observation) {
        let Some(route_attempt) = self.uri_route_attempt.filter(|route|
            route.contract_id() == attempt) else { return; };
        let mut facts = *ps.attempt_view().facts();
        match observation {
            plx_playback_contract::Observation::Playback(observed) => {
                facts.status = Some(match observed.value {
                    plx_playback_contract::Playback::ReadyAwaitPlay =>
                        plx_playback_contract::readout::PlaybackState::Buffering,
                    plx_playback_contract::Playback::Playing =>
                        plx_playback_contract::readout::PlaybackState::Playing,
                    plx_playback_contract::Playback::Paused =>
                        plx_playback_contract::readout::PlaybackState::Paused,
                    plx_playback_contract::Playback::Ended => {
                        facts.ended = Some(true);
                        plx_playback_contract::readout::PlaybackState::Idle
                    }
                });
            }
            plx_playback_contract::Observation::Position(observed) => {
                if let Some(ns) = i64::try_from(observed.value.as_millis()).ok()
                    .and_then(|ms| ms.checked_mul(1_000_000)) {
                    facts.position_ns = Some(ns);
                }
            }
            plx_playback_contract::Observation::Failed(failure) => {
                facts.status = Some(plx_playback_contract::readout::PlaybackState::Error);
                facts.runtime_failure = Some(match failure {
                    Failure::TimedOut => plx_playback_contract::readout::RuntimeFailure::LoadTimeout,
                    Failure::ReleaseUncertain => plx_playback_contract::readout::RuntimeFailure::ReleaseTimeout,
                    Failure::Refused | Failure::Unsupported | Failure::Backend =>
                        plx_playback_contract::readout::RuntimeFailure::TvPipeline,
                    Failure::Busy | Failure::Stale | Failure::Unavailable =>
                        plx_playback_contract::readout::RuntimeFailure::Unknown,
                });
            }
            plx_playback_contract::Observation::Presented
            | plx_playback_contract::Observation::TrackSelected(_)
            | plx_playback_contract::Observation::TrackOff(_) => {}
        }
        if facts.seek_pending_ns.is_some()
            && facts.status != Some(plx_playback_contract::readout::PlaybackState::Error)
            && facts.ended != Some(true) {
            facts.status = Some(plx_playback_contract::readout::PlaybackState::Seeking);
        }
        let _ = ps.ingest_attempt_facts(route_attempt,
            super::attempt_view::Backend::Uri, facts);
    }

    /// A locator is only an escrow key, never another playback attempt identity.
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    pub(super) fn next_source_key(&mut self) -> SourceKey {
        self.next_source_key = self.next_source_key.checked_add(1).expect("source keys exhausted");
        SourceKey::new(self.next_source_key)
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn stage_native_feed(&mut self, attempt: AttemptId, key: SourceKey,
        prepared: super::engine::NativePrepared) -> Result<(), Failure> {
        self.feed_driver.stage(attempt, key, prepared)
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn take_backend_feed_stop(&mut self) -> Option<(AttemptId, StopReason)> {
        self.feed_driver.take_stop_request()
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn note_backend_feed_release(&mut self, attempt: AttemptId, status: ReleaseStatus) {
        self.feed_driver.note_release(attempt, status);
    }

    /// The foreground restore's start was just deferred: follow it.
    pub fn watch_queued_start(&mut self) {
        self.queued_watch = QueuedWatch::Watching;
    }

    /// Is the foreground restore following a queued start?
    pub fn watching_queued_start(&self) -> bool {
        self.queued_watch != QueuedWatch::Unwatched
    }

    /// The restore's queued start ran, and how.
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    pub(super) fn queued_start_ran(&mut self, outcome: Option<crate::route::RouteStartAttempt>) {
        if self.queued_watch == QueuedWatch::Watching {
            self.queued_watch = outcome.map_or(QueuedWatch::Failed, QueuedWatch::Launched);
        }
    }

    /// A queued start was cancelled or replaced: whoever watched it has nothing to follow.
    pub(super) fn queued_start_dropped(&mut self) {
        self.queued_watch = QueuedWatch::Unwatched;
    }

    /// Poll the watched start; an answer other than `Waiting` ends the watch.
    pub fn take_queued_landing(&mut self) -> QueuedLanding {
        let landing = match self.queued_watch {
            QueuedWatch::Unwatched => QueuedLanding::Stale,
            QueuedWatch::Watching if self.release.has_queued() => return QueuedLanding::Waiting,
            QueuedWatch::Watching => QueuedLanding::Stale,
            #[cfg(any(feature = "play-feed", feature = "play-uri"))]
            QueuedWatch::Launched(a) => QueuedLanding::Launched(a),
            #[cfg(any(feature = "play-feed", feature = "play-uri"))]
            QueuedWatch::Failed => QueuedLanding::Failed,
        };
        self.queued_watch = QueuedWatch::Unwatched;
        landing
    }

    /// The preview's start was just deferred: it is admitted (`admit`) or refused when it runs.
    #[cfg(feature = "play-feed")]
    pub fn admit_preview_at_launch(&mut self, admit: bool) {
        self.preview_admission = Some(admit);
    }

    /// The queued start ended; settle the preview's admission with how it ended. `launched` false
    /// (refused at the deadline, failed) counts as a refused admission; a cancelled start
    /// (`settle = false`) leaves the preview machine to the stop that cancelled it.
    #[cfg(feature = "play-feed")]
    pub(super) fn settle_preview_admission(&mut self, launched: bool, settle: bool) {
        if let Some(admit) = self.preview_admission.take() {
            if settle {
                if launched && admit {
                    super::preview::note_admitted();
                } else {
                    super::preview::note_admission_refused();
                }
            }
        }
    }

    /// Is a start queued behind Feed `Unload` or a closing URI registration?
    pub fn release_has_queued_start(&self) -> bool {
        self.release.has_queued()
    }

    /// Park a real stop's off-frame release. At most one exists: a start over it is deferred.
    #[cfg(feature = "play-feed")]
    pub fn park_release(&mut self, release: super::engine::PendingRelease) {
        let (epoch, at) = (release.epoch(), release.handed_off_at());
        self.feed.park_release(release);
        self.release_step(&super::release::ReleaseEvent::UnloadHandedOff { epoch, at }, &mut Vec::new());
    }

    /// Is a stopped native object's `Unload` still owed its main-thread tail?
    #[cfg(feature = "play-feed")]
    pub fn has_pending_release(&self) -> bool {
        self.feed.has_pending_release()
    }

    /// The release machine's verdict on one event; the effects are the caller's to run.
    pub(super) fn release_step(&mut self, event: &super::release::ReleaseEvent, fx: &mut Vec<super::release::ReleaseEffect>) {
        self.release.step(event, fx);
        // Publish the one fact `player::state` needs (no session copy of it can drift).
        #[cfg(feature = "play-feed")]
        super::SHARED
            .start_queued
            .store(self.release.has_queued(), std::sync::atomic::Ordering::Release);
    }

    /// An `Unload` is running or overdue: the object slot is not free.
    pub fn release_busy(&self) -> bool {
        self.release.is_busy()
    }

    /// The `Unload` outlived its deadline: it stays parked and every start is refused.
    pub fn release_overdue(&self) -> bool {
        self.release.is_overdue()
    }

    /// The parked release, taken only once its worker has returned (so joining it cannot block).
    #[cfg(feature = "play-feed")]
    pub(super) fn take_finished_release(&self) -> Option<super::engine::PendingRelease> {
        self.feed.take_finished_release()
    }

    /// The parked release whether or not its worker returned: only the exit, which leaks it
    /// (`engine::leak_parked_for_exit`), takes it that way.
    #[cfg(feature = "play-feed")]
    pub(super) fn take_pending_release_any(&self) -> Option<super::engine::PendingRelease> {
        self.feed.take_pending_release_any()
    }

    /// UI-thread resource effect. The owner spends the attempt before the worker can run.
    pub fn repair_sandbox(&mut self, owner: &mut super::machine::RepairAttempt, supported: bool) {
        let Some(token) = owner.begin(supported) else { return; };
        let (tx, rx) = std::sync::mpsc::channel();
        if plx_base::task::spawn_small("jail repair", move || {
            let _ = tx.send(plx_platform::tv::sandbox::repair());
        }) {
            self.repair = Some((token, rx));
        } else {
            owner.complete(token, Err(plx_platform::tv::sandbox::Failure::StartFailed));
        }
    }

    /// Poll on every app frame, including while the player page is absent.
    pub fn poll_repair(&mut self, owner: &mut super::machine::RepairAttempt) -> bool {
        let Some((token, rx)) = self.repair.as_ref() else { return false; };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(plx_platform::tv::sandbox::Failure::StartFailed),
        };
        let changed = owner.complete(*token, result);
        self.repair = None;
        changed
    }

    /// Test-only access to the physical session for the existing hostsim rigs.
    #[cfg(all(test, feature = "play-feed"))]
    #[inline]
    pub fn engine(&mut self) -> Option<&mut Engine> {
        self.feed.engine()
    }

    /// Route identity of the active physical feed, without lending Engine state to admission.
    #[cfg(feature = "play-feed")]
    pub(super) fn active_feed_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        let (attempt, _) = self.feed_attempt?;
        (self.feed.active_attempt() == Some(attempt.contract_id())).then_some(attempt)
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn bind_feed_attempt(&mut self, attempt: crate::route::RouteStartAttempt,
        trace_generation: u32) {
        debug_assert_eq!(self.feed.active_attempt(), Some(attempt.contract_id()));
        self.feed_attempt = Some((attempt, trace_generation));
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn clear_feed_attempt(&mut self) {
        self.feed_attempt = None;
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn take_native_stop(&mut self) -> super::engine::NativeStop {
        let stop = super::engine::NativeStop::take(&mut self.feed);
        self.clear_feed_attempt();
        stop
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn release_native_stop(&mut self, stop: &mut super::engine::NativeStop,
        for_reload: bool) {
        stop.release_and_drain(&self.mt, &mut self.feed, for_reload);
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn finish_native_stop(&mut self, stop: super::engine::NativeStop,
        for_reload: bool) {
        if let Some(release) = stop.finish(for_reload) {
            // This event is the release machine's admission fence for the next start.
            self.park_release(release);
        }
    }

    /// Address the physical callback snapshot to the current route-owned attempt.
    #[cfg(feature = "play-feed")]
    pub(super) fn publish_feed_attempt_view(&self, ps: &mut crate::route::PlaybackSession) {
        let Some(attempt) = self.active_feed_attempt() else { return; };
        let Some(facts) = super::feed_pump::attempt_facts(&self.feed) else { return; };
        let _ = ps.ingest_attempt_facts(attempt, crate::player::attempt_view::Backend::Feed, facts);
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn feed_uses_stream_queues(&self) -> bool { self.feed.uses_stream_queues() }
    #[cfg(feature = "play-feed")]
    pub(super) fn arm_feed_live_clock_prime(&mut self) -> bool {
        let Some(engine) = self.feed.engine() else { return false; };
        super::engine::arm_live_clock_prime(engine);
        true
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn feed_stage(&self) -> Option<super::shared::Stage> { self.feed.stage() }
    #[cfg(feature = "play-feed")]
    pub(super) fn feed_load_thread_finished(&self) -> bool { self.feed.load_thread_finished() }

    /// Whether the current physical feed has yet to leave its Load stage.
    #[cfg(feature = "play-feed")]
    pub(super) fn feed_loading(&self) -> bool { self.feed.loading() }
    #[cfg(feature = "play-feed")]
    pub(super) fn preview_load_join_state(&self) -> Option<(bool, bool)> {
        self.feed.preview_load_join_state()
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn abandon_preview_load(&mut self) { self.feed.abandon_preview(); }
    #[cfg(feature = "play-feed")]
    pub(super) fn foreground_follows_feed_load(&self) -> bool {
        self.feed.foreground_follows_load()
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn mark_foreground_follows_feed_load(&mut self,
        attempt: crate::route::RouteStartAttempt) -> bool {
        if self.active_feed_attempt() != Some(attempt) { return false; }
        self.feed.mark_foreground_follows_load(attempt.contract_id())
    }
    /// Native readiness for a route/action claim; the route policy itself stays in the pump.
    #[cfg(feature = "play-feed")]
    pub(super) fn route_actions_ready(&self) -> bool { self.feed.route_actions_ready() }

    /// One physical seek-preroll tick. The common pump supplies route-flight facts and applies
    /// the returned finish request after this borrow of feed resources ends.
    #[cfg(feature = "play-feed")]
    pub(super) fn paused_seek_step(&mut self, now: u32, route_flight: bool,
        claim_owns_pause: bool) -> super::feed_pump::PausedSeekStep {
        super::feed_pump::paused_seek_step(&mut self.feed, now, route_flight, claim_owns_pause)
    }

    /// Publish native queue and frame diagnostics without exposing Engine to the common prelude.
    #[cfg(feature = "play-feed")]
    pub(super) fn publish_feed_diag(&mut self, now: u32,
        last_frames: &std::sync::atomic::AtomicI32) {
        super::feed_pump::publish_diag(&mut self.feed, now, last_frames);
    }

    /// Feed the dev firmware-refusal probe with route facts, before any route landing can replace
    /// this physical Engine in the same frame.
    #[cfg(feature = "play-feed")]
    pub(super) fn inject_refuse601(&mut self, generation: u32, preview: bool,
        refused_for: &std::sync::atomic::AtomicU32) {
        super::feed_pump::inject_refuse601(&mut self.feed, generation, preview, refused_for);
    }

    /// Probe the native audio feed's own stall evidence after common Load-refusal handling.
    #[cfg(feature = "play-feed")]
    pub(super) fn native_audio_stall_fired(&mut self, now: u32) -> bool {
        super::feed_pump::native_audio_stall_fired(&mut self.feed, now)
    }

    /// Gate the active physical tick on native constructor readiness.
    #[cfg(feature = "play-feed")]
    pub(super) fn feed_ctor_gate(&mut self) -> super::feed_pump::CtorGate {
        super::feed_pump::ctor_gate(&mut self.feed, &self.mt)
    }

    /// Emit the physical callback-cadence diagnostic after first-picture route settlement.
    #[cfg(feature = "play-feed")]
    pub(super) fn note_missing_era_callback(&mut self) {
        super::feed_pump::note_missing_era_callback(&mut self.feed);
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn stuck_seek_due(&mut self, now: u32, feed_allowed: bool,
        route_flight: bool, budget_ms: u32) -> bool {
        super::feed_pump::stuck_seek_due(&mut self.feed, now, feed_allowed, route_flight, budget_ms)
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn retry_stuck_seek(&mut self, now: u32, target_ns: i64,
        can_retry: bool) -> super::feed_pump::StuckSeekAction {
        super::feed_pump::retry_stuck_seek(&mut self.feed, now, target_ns, can_retry)
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn pending_seek_ready(&self, seek_reloads: bool) -> bool {
        super::feed_pump::pending_seek_ready(&self.feed, seek_reloads)
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn prepare_seek_pause(&self) -> super::feed_pump::SeekPause {
        super::feed_pump::prepare_seek_pause(&self.mt)
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn begin_seek_discontinuity(&self) -> bool {
        super::feed_pump::begin_seek_discontinuity(&self.feed)
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn apply_inplace_seek_pre(&mut self, target_ns: i64,
        mode: super::SeekMode, coalesced: u32) {
        super::feed_pump::apply_inplace_seek_pre(&mut self.feed, &self.mt, target_ns, mode, coalesced);
    }
    #[cfg(feature = "play-feed")]
    pub(super) fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32) {
        super::feed_pump::apply_inplace_seek_post(&mut self.feed, target_ns, now);
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn tick_feed_load(&mut self, now: u32, segmented_hls: bool,
        budget: std::time::Duration, speed_deadline_ms: u32) -> super::feed_pump::LoadTick {
        super::feed_pump::tick_load(&mut self.feed, &self.mt, now,
            segmented_hls, budget, speed_deadline_ms)
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn tick_feed_presentation(&mut self, segmented_hls: bool,
        immersive: bool) -> super::feed_pump::PresentationTick {
        super::feed_pump::tick_presentation(&mut self.feed, &self.mt, segmented_hls, immersive)
    }

    /// Is a session live? Distinct from [`engine`](Self::engine) because it answers without taking
    /// the exclusive borrow — `start_bufferfeed`'s double-start guard only needs to ask.
    #[inline]
    #[cfg(feature = "play-feed")]
    pub fn is_live(&self) -> bool {
        self.feed.is_live()
    }

    /// Address one Load verdict without handing route state to the native worker.
    #[cfg(all(test, feature = "play-feed"))]
    pub(super) fn load_receipt(&mut self, attempt: crate::route::RouteStartAttempt, native_epoch: u32,
        _trace_generation: u32) -> threads::LoadReceipt {
        self.feed.load_receipt(attempt.contract_id(), native_epoch)
    }

    /// A refused thread spawn cannot return a receipt; retire its mapping immediately.
    #[cfg(all(test, feature = "play-feed"))]
    pub(super) fn cancel_load_receipt(&mut self, token: u64) { self.feed.cancel_load_receipt(token); }

    /// The main-thread coordinator's only Load-result landing. Late outcomes from an abandoned
    /// or replaced Engine cannot settle a successor's route, even if a worker returns afterward.
    #[cfg(feature = "play-feed")]
    pub(super) fn drain_load_outcomes(&mut self, ps: &mut crate::route::PlaybackSession) {
        // Construction may still be installing the Engine when the synchronous Load returns.
        // Leave the inbox intact until its owner can validate the native epoch and attempt.
        if !self.feed.is_live() { return; }
        for outcome in self.feed.take_load_outcomes() {
            let Some((attempt, trace_generation)) = self.feed_attempt
                .filter(|(attempt, _)| attempt.contract_id() == outcome.attempt) else { continue; };
            if let Some(elapsed_ms) = outcome.elapsed_ms {
                super::report::note_load_gate_for(
                    trace_generation,
                    super::report::LoadElapsedClass::from_ms(elapsed_ms as i64),
                );
            }
            let result = if outcome.ok { crate::route::RouteStartResult::Started }
                else { crate::route::RouteStartResult::StartFailed };
            let _ = crate::route::settle_route_start(ps, attempt, result);
            self.feed_driver.note_load_outcome(outcome.attempt, outcome.ok);
        }
    }

    /// A preview has no route lease, hence no periodic PMS writes. This follows the existing
    /// begin_timeline_reporting gate and keeps the reporter outside the Engine resource set.
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    pub(super) fn start_timeline(&mut self, ps: &crate::route::PlaybackSession,
        attempt: crate::route::RouteStartAttempt) {
        debug_assert!(self.timeline.is_none(), "one reporter per player adapter");
        if self.timeline.is_some() { return; }
        if let Some(lease) = crate::route::begin_timeline_reporting(ps) {
            self.timeline = timeline::TimelineRunner::start(attempt, lease);
        }
    }

    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    pub(super) fn publish_timeline(&self, attempt: crate::route::RouteStartAttempt,
        snapshot: Option<timeline::TimelineSnapshot>) {
        if let Some(runner) = self.timeline.as_ref() { runner.publish(attempt, snapshot); }
    }

    /// Stop and detach without joining on the main thread. Real stop passes this handle to
    /// route::scrobble_stop; reload simply drops it after the stop signal became sticky.
    #[cfg(any(feature = "play-feed", feature = "play-uri"))]
    pub(super) fn stop_timeline(&mut self, attempt: crate::route::RouteStartAttempt)
        -> Option<std::thread::JoinHandle<()>> {
        let runner = self.timeline.take()?;
        debug_assert_eq!(runner.attempt(), attempt, "reporter must belong to the Engine being torn down");
        Some(runner.stop())
    }

    /// Install the freshly-built session. Overwriting a live slot would DROP an Engine whose
    /// workers hold raw pointers into the boxes it owns; `start_bufferfeed` guards on
    /// [`is_live`](Self::is_live) first.
    #[inline]
    #[cfg(all(test, feature = "play-feed"))]
    pub fn install(&mut self, e: Engine) {
        self.feed_attempt = Some((crate::route::RouteStartAttempt::fixture(), 0));
        self.feed.install(e);
    }

    /// Take the session out of the slot; the caller then joins its workers and drops it.
    #[inline]
    #[cfg(all(test, feature = "play-feed"))]
    pub fn take(&mut self) -> Option<Engine> {
        self.feed_attempt = None;
        self.feed.take()
    }

    /// Is a URI-mode probe session running? `start_bufferfeed`'s double-start guard asks this beside
    /// [`is_live`](Self::is_live): a probe holds the route-start attempt and the exported window, so
    /// a second Play cannot begin under it.
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    #[inline]
    pub fn uri_live(&self) -> bool {
        self.uri.is_some()
    }

    /// The service says the probe's video plays (`observe_video_plane`'s term for a session which
    /// has no Engine to read the ACB play state from).
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    #[inline]
    pub fn uri_plane_open(&self) -> bool {
        self.uri.as_ref().is_some_and(|session| session.plane_open())
    }

    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    pub fn uri_closing(&self) -> bool {
        self.uri.as_ref().is_some_and(|session| session.closing())
    }

    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    pub(super) fn uri_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        self.uri.as_ref().map(|session| session.route_start_attempt())
    }

    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    pub(super) fn uri_acb_exit_quarantine(&self) -> bool { self.uri.as_ref().is_some_and(|session| session.acb_exit_quarantine()) }

    /// Install the probe's session. `uriprobe_drive::start` guards on [`uri_live`](Self::uri_live)
    /// first: overwriting a live slot would drop a session without ending it.
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    pub(super) fn install_uri(&mut self, session: super::uriprobe_drive::UriSession) {
        debug_assert!(self.uri.is_none(), "a second URI-mode session cannot start under a live one");
        self.uri = Some(session);
    }

    /// Take a physically released probe out of the slot, or leak unresolved ownership at exit.
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    #[inline]
    pub(super) fn take_uri(&mut self) -> Option<super::uriprobe_drive::UriSession> {
        self.uri.take()
    }

    /// The probe's session AND the token at once, borrowed from disjoint fields (the same split
    /// [`split`](Self::split) makes for the Engine).
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    #[inline]
    pub(super) fn uri_split(&mut self) -> (Option<&mut super::uriprobe_drive::UriSession>, &MainThread) {
        (self.uri.as_mut(), &self.mt)
    }

    /// Park a still-in-flight Load for a later main-thread release. At most one can exist: the
    /// C seam owns one object at a time and every start is refused while this slot is full.
    #[cfg(feature = "play-feed")]
    pub fn park_abandoned_load(&mut self, load: super::engine::AbandonedLoad) {
        self.feed.park_abandoned_load(load);
    }

    /// Is a native object still parked behind a Load that has not been released?
    #[cfg(feature = "play-feed")]
    pub fn has_abandoned_load(&self) -> bool {
        self.feed.has_abandoned_load()
    }

    #[cfg(feature = "play-feed")]
    pub(super) fn begin_native_open(&mut self) -> bool {
        super::engine::begin_native_open(&mut self.feed, &self.mt)
    }

    #[cfg(feature = "play-feed")]
    pub fn reap_abandoned_load(&mut self) -> bool {
        super::engine::reap_abandoned_load_resource(&mut self.feed, &self.mt)
    }

    /// The parked Load, borrowed mutably (its once-only log latch), or taken once it returned.
    #[cfg(feature = "play-feed")]
    pub fn take_abandoned_load(&mut self) -> Option<super::engine::AbandonedLoad> {
        self.feed.take_abandoned_load()
    }

    /// The token, for the ACB/Starfish seam. `tv::sink::VideoSink`'s methods still take one — see the
    /// module doc for why that surface keeps its own argument.
    ///
    /// Does not wait for a pending off-frame release: a caller holding the token may call any verb,
    /// and while the object is `Releasing` `player::sink()` answers a sink on which every verb is
    /// a no-op, so none of them reaches the object the worker is inside.
    #[inline]
    pub fn mt(&self) -> &MainThread {
        &self.mt
    }

    /// Hostsim fixture access to the session and main-thread token together. Production common
    /// code uses the feed's typed actions and does not borrow the Engine.
    #[inline]
    #[cfg(all(test, feature = "play-feed"))]
    pub fn split(&mut self) -> (Option<&mut Engine>, &MainThread) {
        (self.feed.engine(), &self.mt)
    }
}

#[cfg(test)]
mod repair_receipt_tests {
    use super::*;
    use plx_platform::tv::sandbox::{Failure, State};
    #[test]
    fn local_metric_history_evicts_old_effects_after_real_reducer_results() {
        use super::super::attempt_metrics::{Origin, RequestOutcome, Tick, Fx};
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        for revision in 1..=40 {
            let request = backend::LogicalRequestId::new(revision);
            let begun = adapter.backend.begin_request_at(request, Origin::Fresh,
                Tick::from_millis(revision));
            adapter.record_backend_metrics(&begun.metrics);
            let finished = adapter.backend.finish_request_at(request, RequestOutcome::Cancelled,
                Tick::from_millis(revision + 1));
            adapter.record_backend_metrics(&finished.metrics);
        }
        let recent: Vec<_> = adapter.backend_metric_notices().collect();
        assert_eq!(recent.len(), MAX_METRIC_NOTICES);
        assert!(matches!(recent.first(), Some(backend::MetricNotice::Measured(Fx::Finished(s)))
            if s.id.get() == 9));
        assert!(matches!(recent.last(), Some(backend::MetricNotice::Measured(Fx::Finished(s)))
            if s.id.get() == 40));
        assert_eq!(adapter.backend_metrics_snapshot().unwrap().id.get(), 40);
    }

    #[test]
    fn a_receipt_lands_without_a_player_screen_and_cannot_rearm_the_attempt() {
        let mut owner = super::super::machine::RepairAttempt::new();
        let token = owner.begin(true).unwrap();
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        let (tx, rx) = std::sync::mpsc::channel();
        adapter.repair = Some((token, rx));
        assert!(!adapter.poll_repair(&mut owner));
        tx.send(Err(Failure::Timeout)).unwrap();
        assert!(adapter.poll_repair(&mut owner));
        assert_eq!(owner.state(), State::Failed(Failure::Timeout));
        assert!(!adapter.poll_repair(&mut owner));
        assert_eq!(owner.begin(true), None);
    }
}

#[cfg(all(test, feature = "play-uri"))]
mod uri_stop_refusal_tests {
    use super::*;
    use plx_playback_contract::{BackendEvent, DriverEvent, DriverSet, EvidenceSource,
        ExitDisposition, Fidelity, InitialPlayback, Observation, Observed, Playback,
        Requirements, SourceKey, SourceKind, TrackChoice};

    // Only establishes the common active owner. The Stop under test still goes through the
    // adapter's actual PhysicalDrivers set, where the URI port is unavailable.
    struct StartedOnce(Option<BackendEvent>);
    impl DriverSet for StartedOnce {
        type Context = MainThread;
        fn capabilities(&self, backend: BackendKind) -> Option<plx_playback_contract::Capabilities> {
            (backend == BackendKind::Uri).then_some(plx_playback_contract::Capabilities {
                progressive_uri: true, hls_uri: false, packet_feed: false,
                pause: true, seek: false, audio_track: false, subtitle_track: false,
                audio_off: false, subtitle_off: false,
            })
        }
        fn dispatch(&mut self, _: BackendKind, _: &MainThread, _: plx_playback_contract::AttemptId,
            _: plx_playback_contract::CommandId, _: Operation) -> plx_playback_contract::DispatchResult {
            plx_playback_contract::DispatchResult::Queued
        }
        fn poll(&mut self, _: &MainThread) -> Vec<BackendEvent> {
            self.0.take().into_iter().collect()
        }
        fn discard_staged(&mut self, _: BackendKind, _: plx_playback_contract::AttemptId,
            _: SourceKey) {}
        fn exit_disposition(&self) -> ExitDisposition { ExitDisposition::Clean }
    }

    #[test]
    fn unavailable_uri_driver_cannot_begin_release_or_queue_a_successor() {
        let _serial = plx_base::testlock::serial();
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        // The route still names the old physical attempt, but no URI driver can accept Stop.
        // This is the same unavailable-driver result the normal PhysicalDrivers set returns.
        let route = crate::route::RouteStartAttempt::fixture();
        adapter.uri_route_attempt = Some(route);
        let attempt = route.contract_id();
        let mut initial = StartedOnce(Some(BackendEvent { backend: BackendKind::Uri,
            event: DriverEvent::Observation { attempt, observation: Observation::Playback(
                Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) } }));
        let _ = adapter.backend.send_at(&mut initial, &adapter.mt, Intent::Start {
            backend: BackendKind::Uri, attempt, source: SourceKey::new(1),
            requirements: Requirements { source: SourceKind::ProgressiveUri,
                video: Fidelity::Exact, audio: Fidelity::Exact,
                audio_track: TrackChoice::Unspecified,
                subtitle_track: TrackChoice::Unspecified },
            initial: InitialPlayback { position: None, paused: true },
        }, crate::player::attempt_metrics::Origin::Fresh,
            crate::player::attempt_metrics::Tick::Unknown);
        let _ = adapter.backend.poll_at(&mut initial, &adapter.mt,
            crate::player::attempt_metrics::Tick::Unknown);
        assert_eq!(adapter.backend.snapshot().phase, plx_playback_contract::Phase::Active);
        let at = adapter.backend_tick();
        let result = adapter.backend_send_at(Intent::Stop {
            backend: BackendKind::Uri,
            attempt: route.contract_id(),
            reason: plx_playback_contract::StopReason::Replace,
        }, crate::player::attempt_metrics::Origin::Fresh, at);
        assert!(result.playback.iter().any(|notice| matches!(notice,
            plx_playback_contract::Fx::Rejected {
                backend: BackendKind::Uri, attempt, ..
            } if *attempt == route.contract_id())));
        assert!(!adapter.uri_stop_requested,
            "a rejected Stop cannot suppress a later physical failure report");
        assert!(!adapter.release_busy(), "no native release began");
        assert!(!adapter.release_has_queued_start(), "no successor was admitted");
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod load_outcome_tests {
    use super::*;

    #[test]
    fn load_outcome_waits_for_its_engine_and_refuses_late_epoch() {
        let _serial = plx_base::testlock::serial();
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::reset_player_control_for_test(&ps);
        let transaction = crate::route::begin_route_start().expect("start transaction");
        assert!(crate::route::prepare_route_start(transaction));
        let attempt = crate::route::claim_route_start_attempt(transaction).expect("physical attempt");
        let mut pa = PlayerAdapter::new(unsafe { MainThread::assume() });

        // A worker may answer before construction finishes, but its route cannot become Stable
        // without an installed Engine. This was the original Load-return race.
        let early = pa.load_receipt(attempt, 7, 0);
        early.deliver(7, true, Some(31));
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending);

        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.attempt = attempt.contract_id();
        pa.install(eng);
        pa.bind_feed_attempt(attempt, 0);
        let late = pa.load_receipt(attempt, 7, 0);
        late.deliver(7, true, Some(32));
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Pending,
            "an older native epoch cannot settle the same route attempt");

        let current = pa.load_receipt(attempt, 8, 0);
        current.deliver(8, true, Some(33));
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Started);
        pa.drain_load_outcomes(&mut ps);
        assert_eq!(crate::route::route_start_status(attempt), crate::route::RouteStartStatus::Started,
            "one Load result settles at most once");
        drop(pa.take());
        crate::player::SHARED.reset_session();
        crate::player::TX.reset();
        crate::route::reset_player_control_for_test(&ps);
    }

    #[test]
    fn elapsed_fact_keeps_its_captured_trace_after_engine_installation() {
        let _serial = plx_base::testlock::serial();
        let attempt = crate::route::RouteStartAttempt::fixture();
        let mut pa = PlayerAdapter::new(unsafe { MainThread::assume() });
        let mut ps = crate::route::PlaybackSession::IDLE;

        // The worker can return before the Engine is installed. Polling cannot discard that
        // result or report it before the owner can validate the native epoch.
        let early = pa.load_receipt(attempt, 8, 41);
        early.deliver(8, true, Some(123));
        pa.drain_load_outcomes(&mut ps);

        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.attempt = attempt.contract_id();
        pa.install(eng);
        pa.bind_feed_attempt(attempt, 41);
        let old_epoch = pa.load_receipt(attempt, 7, 42);
        old_epoch.deliver(7, false, Some(456));
        let wrong_epoch = pa.load_receipt(attempt, 8, 43);
        wrong_epoch.deliver(9, false, Some(789));
        let wrong_attempt = plx_playback_contract::AttemptId::new(
            attempt.contract_id().owner(), attempt.contract_id().generation() + 1,
        );
        let wrong_owner = pa.feed.load_receipt(wrong_attempt, 8);
        wrong_owner.deliver(8, false, Some(790));
        let cancelled = pa.load_receipt(attempt, 8, 44);
        let cancelled_token = cancelled.token();
        cancelled.deliver(8, false, Some(900));
        pa.cancel_load_receipt(cancelled_token);

        let admitted = pa.feed.take_load_outcomes();
        assert_eq!(admitted.len(), 1, "only the installed Engine's exact attempt and epoch are admitted");
        assert_eq!(admitted[0].attempt, attempt.contract_id());
        assert_eq!(pa.feed_attempt, Some((attempt, 41)), "the common owner retains the captured trace");
        assert_eq!(admitted[0].elapsed_ms, Some(123));
        assert!(admitted[0].ok);
        assert!(pa.feed.take_load_outcomes().is_empty(), "each receipt is consumed once");
        drop(pa.take());
        crate::player::SHARED.reset_session();
        crate::player::TX.reset();
    }
}
