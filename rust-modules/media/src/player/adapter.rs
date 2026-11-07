//! **The Player ADAPTER** — the main-thread token and the owned backend resources it confines
//! (restructure spec §2.2, phase 9).
//!
//! The adapter owns the one backend attempt runner beside its OS/FFI resources; route and viewer
//! decisions live one field away in [`crate::player::machine::Player`]. Buffer-feed ownership (the [`Engine`] slot, native Load
//! receipts, abandoned Load and parked release) belongs to the physical set the application composes
//! (`plx_play_feed`), which the adapter reaches only through [`PhysicalSet`]. The adapter
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

#[cfg(test)]
use plx_play_feed::feed_state::Engine;
use super::timeline;
use super::backend;
use plx_playback_contract::feed::{FeedTransportAction, OpenRequest};
use plx_playback_contract::feed::{PhysicalSet, UriOnlySet};
use plx_base::task::MainThread;
use plx_platform::tv::sink::{NoSink, VideoSink};
use std::collections::VecDeque;

const MAX_METRIC_NOTICES: usize = 32;
#[cfg(test)]
use plx_playback_contract::Capabilities;
use plx_playback_contract::{AttemptId, BackendKind, CommandId,
    ExitDisposition, Failure, Intent, Operation, SourceKey, UriDriver};
use plx_playback_contract::uri::UriSource;
use plx_playback_contract::ReleaseStatus;
use plx_playback_contract::Playback;

struct PendingUriAudio {
    attempt: AttemptId,
    command: CommandId,
    audio: crate::route::CarriedAudio,
    memo: Option<plx_plex::plex::session::prefs::AudioMemo>,
}

#[cfg(test)]
#[path = "adapter_track_tests.rs"]
mod track_tests;

struct PendingUriSubtitle {
    attempt: AttemptId,
    command: CommandId,
    ordinal: i32,
    stream_id: i64,
    memo: Option<plx_plex::plex::session::prefs::SubtitleMemo>,
}

/// A delivered native track command was refused for this exact URI attempt. The app can ask the
/// existing same-source resolver for a new physical output; this is not a second Start queue.
pub enum UriTrackFallback {
    Audio { audio: crate::route::CarriedAudio,
        memo: Option<plx_plex::plex::session::prefs::AudioMemo> },
    Subtitle { stream_id: i64,
        memo: Option<plx_plex::plex::session::prefs::SubtitleMemo> },
}

pub use crate::route::{QualifiedUriOffer, UriRecoveryNotice, UriSourceRecipe};

/// A monotonic millisecond clock read at the player IO boundary. The composition root hands one to
/// the URI driver as its clock, and the driver only ever receives elapsed values from it.
pub struct MonotonicMs(std::time::Instant);

impl MonotonicMs {
    pub fn start() -> Self { Self(std::time::Instant::now()) }

    pub fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

/// Expiry for an off-frame source qualification, read at the player IO boundary. A qualifier
/// owns the returned deadline with its source; the route only compares the supplied value.
pub fn uri_source_expiry_after(ttl: std::time::Duration) -> std::time::Instant {
    std::time::Instant::now() + ttl
}

#[derive(Clone, Copy)]
struct PendingUriRecovery { notice: UriRecoveryNotice, released: bool }

#[cfg(test)]
mod composed_backends_tests {
    use super::*;
    use plx_playback_contract::BackendSet;
    use plx_playback_contract::test_support::FakeUriDriver;

    /// The Feed driver is part of every adapter a Feed build constructs; nothing else is until a
    /// composition installs it.
    const FEED_ONLY: BackendSet = BackendSet { uri: false, feed: true };

    #[test]
    fn a_fresh_adapter_forms_exactly_the_set_its_constructor_installs() {
        let adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        assert_eq!(adapter.composed_backends(), FEED_ONLY);
    }

    /// The start path's hooks arrive with the physical set: an adapter with none composed captures
    /// no demux session and reads no `playurl` declaration, one with the Feed's captures a session
    /// for the attempt it is asked about.
    #[test]
    fn the_start_hooks_arrive_with_the_physical_set() {
        let bare = PlayerAdapter::new(unsafe { MainThread::assume() });
        assert!(bare.capture_demux_session(AttemptId::new(1, 1), 1).is_none());
        assert!(bare.play_url().is_none());
        let fed = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        let session = fed.capture_demux_session(AttemptId::new(1, 1), 1).expect("the Feed's composition captures one");
        assert_eq!(session.observer().attempt(), AttemptId::new(1, 1));
    }

    /// The sink a composition installs is asked afresh at every use (the Feed's fence changes its
    /// answer while an off-frame `Unload` owns the object), and until one is installed the adapter
    /// reaches the seam through a sink that refuses every verb.
    #[test]
    fn the_adapters_sink_is_inert_until_installed_and_asked_afresh_at_every_use() {
        use plx_platform::tv::sink::{NoSink, VideoSink, VP_NONE};
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn counted() -> &'static dyn VideoSink {
            ASKED.fetch_add(1, SeqCst);
            &NoSink
        }
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        assert_eq!(adapter.display_sink().window_mode(), VP_NONE);
        assert_eq!(ASKED.load(SeqCst), 0, "nothing installed yet: the inert sink answered");
        assert!(adapter.install_display_sink(counted));
        let _ = (adapter.display_sink(), adapter.display_sink());
        assert_eq!(ASKED.load(SeqCst), 2, "the provider is asked at each use, not once at install");
    }

    /// A set the adapter refuses brings its sink with it: the adapter keeps the one it had.
    #[test]
    fn a_refused_physical_set_leaves_the_adapters_sink_alone() {
        use plx_platform::tv::sink::{NoSink, VideoSink};
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn counted() -> &'static dyn VideoSink {
            ASKED.fetch_add(1, SeqCst);
            &NoSink
        }
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        assert!(adapter.install_physical(Box::new(UriOnlySet::new()), counted, crate::player::test_physical::start_hooks()).is_err(),
            "a Feed is already installed");
        let _ = adapter.display_sink();
        assert_eq!(ASKED.load(SeqCst), 0, "the refused set's sink was not installed");
    }

    #[test]
    fn installing_the_uri_driver_adds_uri_and_a_second_driver_is_handed_back() {
        let caps = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        assert_eq!(adapter.composed_backends(), FEED_ONLY);
        let (first, _) = FakeUriDriver::new(caps);
        assert!(adapter.install_uri_driver(Box::new(first)).is_ok());
        assert_eq!(adapter.composed_backends(),
            BackendSet { uri: true, feed: true });
        let (second, handle) = FakeUriDriver::new(caps);
        assert!(adapter.install_uri_driver(Box::new(second)).is_err(),
            "one URI driver per adapter: the second is handed back, not installed");
        assert!(handle.staged().is_empty());
        assert_eq!(adapter.composed_backends(),
            BackendSet { uri: true, feed: true });
    }
}

#[cfg(test)]
mod qualified_uri_offer_tests {
    use super::*;
    use plx_playback_contract::{Fidelity, InitialPlayback, Requirements, SourceKind, TrackChoice};
    use plx_playback_contract::test_support::FakeUriDriver;

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
        assert!(session.qualify_resolved_feed_offer(
            crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        assert!(matches!(session.backend_offer(),
            crate::route::backend_policy::Selection::Open(offer)
                if offer.backend == BackendKind::Feed));
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        // The driver queues a Start, and its open is refused afterwards, as a transport that
        // cannot open does.
        let (driver, handle) = FakeUriDriver::new(Capabilities { progressive_uri: true,
            hls_uri: false, packet_feed: false, pause: true, seek: true, audio_track: true,
            subtitle_track: true, audio_off: false, subtitle_off: false });
        handle.fail_open_after_start(Failure::Backend);
        assert!(adapter.install_uri_driver(Box::new(driver)).is_ok());
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
            "queued URI Open is distinct from the driver's later refusal");
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
        assert!(matches!(session.backend_offer(),
            crate::route::backend_policy::Selection::Open(offer)
                if offer.backend == BackendKind::Feed),
            "URI refusal retains the qualified Feed choice");
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

fn uri_offer_executable(ps: &crate::route::PlaybackSession, offer: &QualifiedUriOffer) -> bool {
    // The URI driver has no speed transform. The server output, when it claims an enhancement,
    // must already have it applied; an app-only/pending ask cannot be silently dropped.
    ps.requested_rate_permille() == plx_playback_contract::RATE_NORMAL
        && crate::route::displayed_audio_enhancements(ps) == offer.recipe.audio()
        && (offer.recipe.audio().is_none()
            || crate::route::applied_audio_enhancements(ps) == offer.recipe.audio())
}

/// The record of the physical attempt the adapter has bound ([`PlayerAdapter::bind_feed_attempt`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BoundFeed {
    attempt: crate::route::RouteStartAttempt,
    /// The product trace captured at the start, for the reports of this attempt.
    trace_generation: u32,
    /// The foreground lifecycle follows this attempt's Load: it then owns the Load call's own
    /// synchronous result (`route_start_status` `Failed`, the refusal #1018 retries), so the
    /// pump's failed-open ladder leaves that one alone (`settled_by_foreground`). False at every
    /// bind; the bound record is dropped with the attempt, so it cannot outlive it.
    foreground_follows_load: bool,
}

/// The seam's sink, asked afresh at every use. A provider and not a stored reference because the
/// Feed's composition hands a fence in front of the real sink, and which of the two answers
/// changes while an off-frame `Unload` owns the native object.
pub type SinkProvider = fn() -> &'static dyn VideoSink;

/// Captures the session one demux attempt is handed: the Feed's clock, adaptive actuator, callback
/// observer and effect sink over the composition's caption half and media hooks
/// ([`super::demux_session`]). Built by the application, which names the Feed; media asks for it
/// at a start.
pub type DemuxSessionProvider = fn(AttemptId, u32) -> Box<dyn plx_playback_contract::demux::DemuxSession>;

/// What the application composes beside the physical set for the start path to call: the demux
/// session of an attempt and the dev `playurl` trigger's reader. Media cannot name the Feed that
/// supplies them, so they arrive as plain function pointers.
#[derive(Clone, Copy)]
pub struct StartHooks {
    pub demux_session: DemuxSessionProvider,
    /// The reader of the `plxnative-playurl` dev trigger (`None` in a build without `devtriggers`).
    pub play_url: plx_playback_contract::playurl::PlayUrlSource,
}

/// What an adapter reaches the seam through until the composition installs a sink: every verb
/// refused ([`NoSink`]).
fn inert_sink() -> &'static dyn VideoSink { &NoSink }

/// Common coordinator and main-thread proof, as one owned value (`App.adapters.player`).
pub struct PlayerAdapter {
    /// Proof, held rather than passed: see the module doc. It is a ZST, so this costs nothing and
    /// is what makes `&mut PlayerAdapter` mean "the main thread, exclusively".
    mt: MainThread,
    /// The persistent physical executors: the native feed (its slot, Load and release workers
    /// and driver) and the optional URI driver, as the one set the runner and the pump drive.
    /// The application composes it ([`Self::install_physical`]); until it does, and in a build with
    /// no Feed, the set is the contract's [`UriOnlySet`]. The adapter reaches it only through
    /// [`PhysicalSet`].
    pub(super) physical: Box<dyn PhysicalSet>,
    /// The sink the seam is reached through, composed beside the set ([`Self::install_physical`],
    /// [`Self::install_display_sink`]) and read through [`Self::display_sink`]. Until the
    /// composition installs one it refuses every verb ([`inert_sink`]).
    sink: SinkProvider,
    /// The start path's hooks, composed beside the set ([`Self::install_physical`]); `None` until
    /// then, so a start over the URI-only set that reaches for one is refused.
    start_hooks: Option<StartHooks>,
    /// The test fixtures' handle on the Feed set boxed behind `physical`
    /// ([`Self::new_probed`]); `None` on an adapter built by [`Self::new`].
    #[cfg(test)]
    probe: Option<plx_play_feed::physical::probe::Probe>,
    /// The one neutral attempt owner, over the physical executors above.
    backend: backend::BackendRunner,
    /// Recent typed measurements for local diagnostics. Older notices are evicted; the
    /// reducer's aggregate summary keeps the logical request's counts and durations.
    metric_notices: VecDeque<backend::MetricNotice>,
    next_source_key: u64,
    backend_epoch: std::time::Instant,
    qualified_uri: Option<QualifiedUriOffer>,
    uri_route_attempt: Option<crate::route::RouteStartAttempt>,
    uri_offer_id: Option<crate::route::backend_policy::OfferId>,
    uri_recovery: Option<PendingUriRecovery>,
    uri_route_settled: bool,
    uri_stop_requested: bool,
    uri_finalized: bool,
    pending_uri_audio: Option<PendingUriAudio>,
    pending_uri_subtitle: Option<PendingUriSubtitle>,
    refused_uri_audio: Option<PendingUriAudio>,
    refused_uri_subtitle: Option<PendingUriSubtitle>,
    /// The bound physical attempt: its route identity, product trace and foreground follow mark.
    /// The feed itself receives only the canonical contract ID and native epoch. Bound right after
    /// the Engine is installed, dropped by [`Self::stop_begin`]: an Engine is installed
    /// exactly while this is `Some`, which is what [`Self::is_live`] answers.
    feed_attempt: Option<BoundFeed>,
    /// The adaptive worker's channel of the open claimed under that attempt: the worker is a thread
    /// of the Engine, so the receiver stays on the main thread beside the attempt and the pump
    /// drains it into the session's readout ([`Self::drain_abr`]). Closed once the workers are joined.
    abr_inbox: Option<(crate::route::RouteStartAttempt, crate::adaptive::AbrInbox)>,
    /// Backend-independent periodic reporting resource for the current physical attempt.
    timeline: Option<timeline::TimelineRunner>,
    repair: Option<(u64, std::sync::mpsc::Receiver<Result<(), plx_platform::tv::sandbox::Failure>>)>,
    release: super::release::NativeRelease,
    /// Effects produced by an addressed URI release while the backend poll has no session
    /// borrow. The release machine owns the sole queued transaction; these are only its IO work.
    release_fx: Vec<super::release::ReleaseEffect>,
    /// What became of the start the FOREGROUND restore queued behind native release
    /// ([`watch_queued_start`](Self::watch_queued_start)); the lifecycle machine polls it.
    queued_watch: QueuedWatch,
    /// A hero PREVIEW's start was queued behind Feed release: whether its slot counts as admitted
    /// once the Load actually launches (`None`: no preview start is queued). Admission is taken
    /// when the queued start runs, never when it is merely queued.
    preview_admission: Option<bool>,
    /// The URI-mode probe's session (issue #1064, dev trigger `plxnative-uriprobe`): the probe plays
    /// through `com.webos.media` instead of an [`Engine`], so it lives BESIDE the engine slot and
    /// never in it. [`is_live`](Self::is_live) stays Engine-only: the Engine paths (`pump`, the
    /// HUD, the stop ritual's `take`) must never see a probe. Owned here, not by a static, for the
    /// same reason the Engine is: ending it calls the window verbs of the sink, which only the
    /// main thread may. Absent from a release build.
    #[cfg(feature = "devtriggers")]
    uri: Option<Box<dyn super::uriprobe_drive::UriProbeSession>>,
    /// The application's probe (`dev::uriprobe`), installed at boot: it reads the trigger text and
    /// opens the [`uri`](Self::uri) session. Absent when nothing composed one, and then the trigger
    /// file is not read.
    #[cfg(feature = "devtriggers")]
    uri_probe: Option<Box<dyn super::uriprobe_drive::UriProbe>>,
}

/// The foreground restore's view of its own queued start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuedWatch {
    /// Nobody follows the queued start (an ordinary play lands through `finish_landed_play`).
    Unwatched,
    /// The restore's start is queued; it has not run.
    Watching,
    /// The queued start ran: the Load was launched ...
    Launched(crate::route::RouteStartAttempt),
    /// ... or ended without one (refused at the deadline, or the start itself failed).
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
            physical: Box::new(UriOnlySet::new()),
            sink: inert_sink,
            start_hooks: None,
            #[cfg(test)]
            probe: None,
            backend: backend::BackendRunner::new(),
            metric_notices: VecDeque::new(),
            next_source_key: 0,
            backend_epoch: std::time::Instant::now(),
            qualified_uri: None,
            uri_route_attempt: None,
            uri_offer_id: None,
            uri_recovery: None,
            uri_route_settled: false,
            uri_stop_requested: false,
            uri_finalized: false,
            pending_uri_audio: None,
            pending_uri_subtitle: None,
            refused_uri_audio: None,
            refused_uri_subtitle: None,
            feed_attempt: None,
            abr_inbox: None,
            timeline: None,
            repair: None,
            release: super::release::NativeRelease::new(),
            release_fx: Vec::new(),
            queued_watch: QueuedWatch::Unwatched,
            preview_admission: None,
            #[cfg(feature = "devtriggers")]
            uri: None,
            #[cfg(feature = "devtriggers")]
            uri_probe: None,
        }
    }

    /// [`Self::new`] over a set that forwards to the Feed set the Engine fixtures keep a
    /// handle on ([`Self::engine`], [`Self::install`], [`Self::take`], [`Self::split`],
    /// [`Self::has_pending_release`]): the adapter drives the same set through the same
    /// `dyn PhysicalSet`, and only the fixtures reach it through the probe. A test that calls one
    /// of those fixtures builds its adapter with this; one that does not uses [`Self::new_fed`].
    #[cfg(test)]
    pub fn new_probed(mt: MainThread) -> Self {
        let mut adapter = Self::new(mt);
        let (probe, physical) = plx_play_feed::physical::probe::Probe::new(
            super::test_physical::feed_set());
        super::test_physical::install_sf_event_hooks();
        adapter.physical = physical;
        adapter.sink = super::test_physical::sink;
        adapter.start_hooks = Some(super::test_physical::start_hooks());
        adapter.probe = Some(probe);
        adapter
    }

    /// The fixtures' handle on the boxed Feed set.
    #[cfg(test)]
    pub(super) fn probe(&self) -> &plx_play_feed::physical::probe::Probe {
        self.probe.as_ref().expect("this fixture needs an adapter built by PlayerAdapter::new_probed")
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

    /// A handled error may classify only the physical failure of this session's current
    /// logical request and exact attempt. A predecessor's URI failure is never borrowed by a
    /// later Feed plan that failed before claiming its own attempt.
    pub fn terminal_failure_source(&self, ps: &crate::route::PlaybackSession)
        -> Option<backend::TerminalFailureSource> {
        let view = ps.attempt_view();
        let backend = view.backend().map(|backend| match backend {
            super::attempt_view::Backend::Feed => super::attempt_metrics::Backend::Feed,
            super::attempt_view::Backend::Uri => super::attempt_metrics::Backend::Uri,
        });
        backend::terminal_failure_source(ps.metric_request(),
            view.attempt().map(|attempt| attempt.contract_id()), backend,
            self.backend.metrics_snapshot())
    }

    /// The same fresh offer decision the start door will use. IO retires an expired URI
    /// authorization before the app arms any backend-specific foreground transport hold.
    pub fn planned_start_backend(&mut self, ps: &mut crate::route::PlaybackSession)
        -> Option<BackendKind> {
        self.refresh_uri_qualification(ps);
        ps.planned_start_backend()
    }

    /// Product URI plane ownership is separate from the diagnostic probe and panel evidence.
    pub fn uri_product_plane_owned(&self) -> bool {
        self.physical.uri().is_some_and(|driver| driver.plane_owned())
    }

    /// A mounted page is not the owner of a physical attempt. A URI EOS may have already
    /// released its native session while its final Plex stop still belongs to the route.
    pub fn playback_needs_teardown(&self, ps: &crate::route::PlaybackSession) -> bool {
        if self.is_live() { return true; }
        if self.uri_route_attempt.is_some()
            || (!self.uri_finalized
                && ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri)
                && ps.attempt_view().facts().ended == Some(true)) {
            return true;
        }
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
    pub fn backend_transport_control(&mut self, ps: &mut crate::route::PlaybackSession,
        paused: bool) -> Option<bool> {
        let snapshot = self.backend.snapshot();
        let (Some(backend), Some(attempt)) = (snapshot.backend, snapshot.attempt) else {
            return None;
        };
        let operation = if paused { Operation::Pause } else { Operation::Play };
        let at = self.backend_tick();
        let result = self.backend_send_at(ps, Intent::Control { backend, attempt, operation },
            super::attempt_metrics::Origin::Fresh, at);
        Some(!result.playback.iter().any(|fx| matches!(fx,
            plx_playback_contract::Fx::Rejected { .. }
                | plx_playback_contract::Fx::CommandRefused { .. }
        )))
    }

    /// The app's one seek door. URI uses the addressed runner; Feed retains its existing
    /// coalescing/reload planner. An unavailable physical backend refuses without global writes.
    pub fn request_seek(&mut self, ps: &mut crate::route::PlaybackSession, target_ns: i64) -> bool {
        if let Some(accepted) = self.seek_uri(ps, target_ns) { return accepted; }
        if !ps.backends().feed { return false; }
        super::request_seek(ps, self, target_ns);
        true
    }

    /// Playback-rate conversion is a Feed capability. A URI attempt never mutates the Feed
    /// rate or requests its reload as a substitute for a missing native URI operation.
    pub fn set_play_rate(&mut self, ps: &mut crate::route::PlaybackSession, permille: i32) -> bool {
        if ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri) {
            return false;
        }
        if !ps.backends().feed { return false; }
        super::set_play_rate(ps, self, permille)
    }

    /// `None` is a Feed attempt. A URI answer is derived from the active source's private
    /// native map, so a PMS output with one selected track never advertises source siblings.
    pub fn uri_maps_track(&self, ps: &crate::route::PlaybackSession,
        kind: plx_playback_contract::TrackKind, source_id: i64) -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) {
            return None;
        }
        let Some(id) = u64::try_from(source_id).ok().filter(|id| *id > 0) else {
            return Some(false);
        };
        Some(self.physical.uri().is_some_and(|driver| driver.maps_track(
            plx_playback_contract::TrackSelection { kind, id })))
    }

    /// Drain one exact refused interactive choice after polling the native driver. The two
    /// per-kind slots are bounded and a later choice supersedes an earlier one of that kind.
    pub fn take_uri_track_fallback(&mut self) -> Option<UriTrackFallback> {
        let active = self.uri_route_attempt.map(|route| route.contract_id());
        if let Some(pending) = self.refused_uri_audio.take() {
            if Some(pending.attempt) == active {
                return Some(UriTrackFallback::Audio { audio: pending.audio, memo: pending.memo });
            }
        }
        if let Some(pending) = self.refused_uri_subtitle.take() {
            if Some(pending.attempt) == active {
                return Some(UriTrackFallback::Subtitle { stream_id: pending.stream_id,
                    memo: pending.memo });
            }
        }
        None
    }

    /// An accepted explicit source reconfiguration supersedes interactive native selections
    /// still awaiting proof on the old URI attempt. Late replies may reach the runner, but they
    /// cannot commit old route or track-memory state into the replacement request.
    pub fn cancel_uri_interactive_picks(&mut self) {
        self.pending_uri_audio = None;
        self.pending_uri_subtitle = None;
        self.refused_uri_audio = None;
        self.refused_uri_subtitle = None;
    }

    /// `None` means the active route is Feed. `Some(false)` is an explicit URI refusal, so a
    /// caller must never fall through to Feed track or sidecar controls for this route.
    pub fn select_uri_audio(&mut self, ps: &mut crate::route::PlaybackSession,
        audio: crate::route::CarriedAudio,
        memo: Option<plx_plex::plex::session::prefs::AudioMemo>) -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) { return None; }
        if crate::route::cur_sub_mode(ps) == crate::route::SubMode::ForcedOnly {
            return Some(false);
        }
        let Some(route_attempt) = self.uri_route_attempt else { return Some(false); };
        self.refused_uri_audio = None;
        let Ok(id) = u64::try_from(audio.sid) else { return Some(false); };
        let attempt = route_attempt.contract_id();
        let operation = Operation::SelectTrack { kind: plx_playback_contract::TrackKind::Audio,
            choice: plx_playback_contract::TrackChoice::Selected(id) };
        let result = self.backend_send_at(ps, Intent::Control { backend: BackendKind::Uri, attempt,
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

    pub fn select_uri_subtitle(&mut self, ps: &mut crate::route::PlaybackSession,
        ordinal: i32, stream_id: i64, has_sidecar: bool,
        memo: Option<plx_plex::plex::session::prefs::SubtitleMemo>) -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) { return None; }
        // Off and external sidecars have no confirmed native operation in this transport.
        let Ok(id) = u64::try_from(stream_id) else { return Some(false); };
        if id == 0 || ordinal < 0 || has_sidecar { return Some(false); }
        let Some(route_attempt) = self.uri_route_attempt else { return Some(false); };
        self.refused_uri_subtitle = None;
        let attempt = route_attempt.contract_id();
        let operation = Operation::SelectTrack { kind: plx_playback_contract::TrackKind::Subtitle,
            choice: plx_playback_contract::TrackChoice::Selected(id) };
        let result = self.backend_send_at(ps, Intent::Control { backend: BackendKind::Uri, attempt,
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
        if let Some(accepted) = self.select_uri_audio(ps, audio.clone(), memo.clone()) {
            return accepted;
        }
        if !ps.backends().feed { return false; }
        super::pump::deliver_container_names(ps);
        crate::route::commit_audio_pick(ps, audio, memo)
    }

    pub fn commit_subtitle_track(&mut self, ps: &mut crate::route::PlaybackSession,
        ordinal: i32, stream_id: i64, sidecar_key: Option<String>, sidecar_codec: String,
        memo: Option<plx_plex::plex::session::prefs::SubtitleMemo>) -> bool {
        if let Some(accepted) = self.select_uri_subtitle(ps, ordinal, stream_id,
            sidecar_key.is_some(), memo.clone()) {
            return accepted;
        }
        if !ps.backends().feed { return false; }
        crate::route::commit_subtitle_and_sidecar(ps, ordinal, stream_id,
            sidecar_key, sidecar_codec, memo);
        true
    }

    pub fn commit_forced_only_track(&mut self, ps: &mut crate::route::PlaybackSession,
        ordinal: i32, stream_id: i64, lang: &str, changed: bool) -> bool {
        if ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri) {
            return false;
        }
        if !ps.backends().feed { return false; }
        crate::route::commit_forced_only_and_sidecar(ps, ordinal, stream_id, lang, changed);
        true
    }

    pub fn commit_audio_enhancement(&mut self, ps: &mut crate::route::PlaybackSession,
        enhancement: plx_plex::plex::AudioEnhancements) -> bool {
        if ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri) {
            return false;
        }
        if !ps.backends().feed { return false; }
        super::request_audio_enhancement(ps, enhancement);
        true
    }

    /// Address a seek to the active URI attempt. A refused command changes no Feed mailbox or
    /// route selection; the visible target is held until that command's native landing report.
    pub fn seek_uri(&mut self, ps: &mut crate::route::PlaybackSession, target_ns: i64)
        -> Option<bool> {
        if ps.attempt_view().backend() != Some(super::attempt_view::Backend::Uri) {
            return None;
        }
        let Some(route_attempt) = self.uri_route_attempt else { return Some(false); };
        let attempt = route_attempt.contract_id();
        let target_ms = u64::try_from(target_ns.max(0) / 1_000_000).ok()?;
        let result = self.backend_send_at(ps, Intent::Control { backend: BackendKind::Uri,
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
    /// supplies the physical attempt's origin and a monotonic tick for local measurements, and
    /// the session the intent's results step (the release queue today).
    pub fn backend_send_at(&mut self, ps: &mut crate::route::PlaybackSession, intent: Intent,
        origin: super::attempt_metrics::Origin, at: super::attempt_metrics::Tick)
        -> backend::RunResult {
        let result = self.backend.send_at(&mut *self.physical, &self.mt, intent, origin, at);
        let result = self.finish_backend_actions(ps, result, at);
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
                    self.release_step(ps, &super::release::ReleaseEvent::UriReleaseStarted {
                        attempt, at: std::time::Instant::now(),
                    }, &mut Vec::new());
                }
            }
            self.capture_uri_release(ps, &result.playback);
        }
        self.record_backend_metrics(&result.metrics);
        result
    }

    /// Poll both physical executors even when no player page is mounted. A terminal observation
    /// and a same-poll release retain their order in the returned addressed results.
    pub fn backend_poll_at(&mut self, ps: &mut crate::route::PlaybackSession,
        at: super::attempt_metrics::Tick) -> backend::RunResult {
        let result = self.backend.poll_at(&mut *self.physical, &self.mt, at);
        let result = self.finish_backend_actions(ps, result, at);
        self.capture_uri_release(ps, &result.playback);
        self.record_backend_metrics(&result.metrics);
        result
    }

    fn capture_uri_release(&mut self, ps: &mut crate::route::PlaybackSession,
        notices: &[plx_playback_contract::Fx]) {
        for notice in notices {
            if let plx_playback_contract::Fx::ReleaseOutcome {
                backend: BackendKind::Uri, attempt, status,
            } = *notice {
                let mut fx = Vec::new();
                self.release_step(ps, &match status {
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

    pub fn backend_set_foreground_at(&mut self, ps: &mut crate::route::PlaybackSession,
        foreground: bool, at: super::attempt_metrics::Tick) -> backend::RunResult {
        let result = self.backend.set_foreground_at(&mut *self.physical, &self.mt, foreground, at);
        let result = self.finish_backend_actions(ps, result, at);
        self.record_backend_metrics(&result.metrics);
        result
    }

    /// The native user transport door owns TX, Starfish and ACB together. A queued Feed control
    /// is executed only after the runner's temporary driver borrow ends, then its addressed
    /// acceptance is returned through the same runner before another command can advance. `ps` is
    /// the session the door carries to those answers' readout events: the Feed's Pause and Resume
    /// commit the transport, and `pause_result` / `resume_result` step the readout's copy from the
    /// answer.
    fn finish_backend_actions(&mut self, ps: &mut crate::route::PlaybackSession,
        result: backend::RunResult, at: super::attempt_metrics::Tick) -> backend::RunResult {
        let mut result = result;
        loop {
            let next = self.physical.feed().and_then(|feed| feed.take_transport_action());
            let Some(action) = next else { break };
            let transport = match action {
                FeedTransportAction::Play { .. } | FeedTransportAction::ResumeInitial { .. } =>
                    super::resume_result(ps, self),
                FeedTransportAction::Pause { .. } => super::pause_result(ps, self),
            };
            if let Some(feed) = self.physical.feed() {
                feed.note_transport_result(&self.mt, action, transport.accepted,
                    transport.native_acknowledged);
            }
            let next = self.backend.poll_at(&mut *self.physical, &self.mt, at);
            result.playback.extend(next.playback);
            result.metrics.extend(next.metrics);
            result.controls.extend(next.controls);
        }
        result
    }

    /// The playback backends this adapter's installed drivers form: a backend is in the set when
    /// its driver answers [`plx_playback_contract::DriverSet::capabilities`]. Boot hands it to
    /// `PlaybackSession::install_backends`, so route policy offers what is physically installed
    /// (a URI driver the product install refused is not offered) and never reads a Cargo feature.
    pub fn composed_backends(&self) -> plx_playback_contract::BackendSet {
        plx_playback_contract::BackendSet::from_capabilities(&*self.physical)
    }

    pub fn backend_exit_disposition(&self) -> ExitDisposition {
        if self.backend.snapshot().phase != plx_playback_contract::Phase::Vacant {
            return ExitDisposition::PreserveNativeOwnership;
        }
        self.physical.exit_disposition()
    }

    /// Install the URI driver the application's composition built, while no physical attempt
    /// owns the player. Installation supplies no source-specific qualification and therefore
    /// does not change the route's default Feed choice. A second driver, or one offered while
    /// a native attempt is still being released, is handed back untouched.
    pub fn install_uri_driver(&mut self, driver: Box<dyn UriDriver<Context = MainThread>>)
        -> Result<(), Box<dyn UriDriver<Context = MainThread>>> {
        if self.physical.uri().is_some()
            || self.backend_exit_disposition() != ExitDisposition::Clean {
            return Err(driver);
        }
        self.physical.install_uri(driver)
    }

    /// Install the physical set the application composed (the Feed backend and the URI driver's
    /// seat) with the sink its verbs go through and the hooks its start path calls, replacing the
    /// [`UriOnlySet`] and the inert sink the adapter starts with. It is the composition's one call,
    /// made before the first Start; a set that would lose something is handed back untouched: the
    /// current set seats a URI driver, a Feed is already installed, or a native attempt is still
    /// being released.
    pub fn install_physical(&mut self, set: Box<dyn PhysicalSet>, display_sink: SinkProvider,
        start_hooks: StartHooks) -> Result<(), Box<dyn PhysicalSet>> {
        if self.physical.uri().is_some() || self.physical.feed().is_some()
            || self.backend_exit_disposition() != ExitDisposition::Clean {
            return Err(set);
        }
        self.physical = set;
        self.sink = display_sink;
        self.start_hooks = Some(start_hooks);
        Ok(())
    }

    /// The session one demux attempt is handed, captured by the composition's provider; `None`
    /// when no Feed composition is installed.
    pub(super) fn capture_demux_session(&self, attempt: AttemptId, native_epoch: u32)
        -> Option<Box<dyn plx_playback_contract::demux::DemuxSession>> {
        self.start_hooks.map(|hooks| (hooks.demux_session)(attempt, native_epoch))
    }

    /// This boot's `plxnative-playurl` declaration, if one was armed (see
    /// [`plx_playback_contract::playurl`]); `None` with no Feed composition installed.
    pub(super) fn play_url(&self) -> Option<Result<plx_playback_contract::playurl::PlayUrl, String>> {
        self.start_hooks.and_then(|hooks| (hooks.play_url)())
    }

    /// Install the sink of a composition with no Feed set (the URI-only build hands the platform
    /// sink). False, and nothing changes, while a native attempt is still being released.
    pub fn install_display_sink(&mut self, display_sink: SinkProvider) -> bool {
        if self.backend_exit_disposition() != ExitDisposition::Clean { return false; }
        self.sink = display_sink;
        true
    }

    /// The composed sink, right now: the Feed's fence answers in its place while an off-frame
    /// `Unload` owns the native object, so a main-thread verb cannot reach what the worker is
    /// inside (PLX-NATIVE-1Q). Every media-side verb that is not a hook goes through here.
    pub fn display_sink(&self) -> &'static dyn VideoSink { (self.sink)() }

    /// Escrow an exact resolved URL and its source-specific native track map. The route owner
    /// still has to supply a qualified offer and send an addressed Start through the runner.
    pub fn stage_uri_source(&mut self, attempt: AttemptId, source: UriSource)
        -> Result<SourceKey, Failure> {
        let key = self.next_source_key();
        self.physical.uri_mut().ok_or(Failure::Unavailable)?
            .stage_source(attempt, key, source)?;
        Ok(key)
    }

    /// Accept source-specific qualification only for this session's current Part and selected
    /// streams. The external qualifier supplies an expiry; a deferred native release cannot
    /// retain an authorization past that deadline.
    pub fn offer_qualified_uri(&mut self, ps: &mut crate::route::PlaybackSession,
        offer: QualifiedUriOffer) -> Result<(), Failure> {
        if self.physical.uri().is_none() { return Err(Failure::Unavailable); }
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
    pub(super) fn discard_qualified_uri_for(&mut self, ps: &mut crate::route::PlaybackSession,
        route_start: crate::route::RouteStartTransaction) {
        if self.qualified_uri.as_ref().is_some_and(|offer| offer.route_start == route_start) {
            if let Some(offer) = self.qualified_uri.take() {
                ps.qualify_backend_offer(BackendKind::Uri, offer.offer_id,
                    crate::route::backend_policy::Qualification::Unknown);
            }
        }
    }

    pub(super) fn drop_qualified_uri_for_exit(&mut self,
        route_start: crate::route::RouteStartTransaction) {
        if self.qualified_uri.as_ref().is_some_and(|offer| offer.route_start == route_start) {
            self.qualified_uri = None;
        }
    }

    pub(super) fn take_release_effects(&mut self) -> Vec<super::release::ReleaseEffect> {
        std::mem::take(&mut self.release_fx)
    }

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

    pub(super) fn bind_uri_attempt(&mut self, attempt: crate::route::RouteStartAttempt,
        offer_id: crate::route::backend_policy::OfferId) {
        self.uri_route_attempt = Some(attempt);
        self.uri_offer_id = Some(offer_id);
        self.uri_recovery = None;
        self.uri_route_settled = false;
        self.uri_stop_requested = false;
        self.uri_finalized = false;
        self.pending_uri_audio = None;
        self.pending_uri_subtitle = None;
        self.refused_uri_audio = None;
        self.refused_uri_subtitle = None;
    }

    pub fn take_released_uri_recovery(&mut self) -> Option<UriRecoveryNotice> {
        if self.uri_recovery.is_some_and(|pending| pending.released) {
            self.uri_recovery.take().map(|pending| pending.notice)
        } else { None }
    }

    pub(super) fn cancel_uri_recovery(&mut self, ps: &mut crate::route::PlaybackSession) {
        if let Some(pending) = self.uri_recovery.take() {
            ps.clear_uri_recovery_pending(pending.notice.attempt);
        }
    }

    fn note_uri_failure(&mut self, ps: &mut crate::route::PlaybackSession,
        attempt: AttemptId, viewer_paused: Option<bool>) {
        let Some(route_attempt) = self.uri_route_attempt.filter(|route|
            route.contract_id() == attempt) else { return; };
        if self.uri_recovery.as_ref().is_some_and(|pending|
            pending.notice.attempt == route_attempt) { return; }
        let Some(offer_id) = self.uri_offer_id else { return; };
        let played_once = ps.attempt_view().facts().playing_observed == Some(true);
        let phase = if played_once { crate::route::UriFailurePhase::Runtime }
            else { crate::route::UriFailurePhase::Startup };
        // A URI service clock is content-time evidence even though it cannot prove panel
        // presentation. Once Playing was observed, the unpresented startup resume slot may be
        // older than this clock; an absent runtime clock must stay unknown, not become that old
        // resume point or an invented zero. A pending viewer seek is the intended target.
        let known_position_ns = if played_once {
            let facts = ps.attempt_view().facts();
            facts.seek_display_ns.or(facts.position_ns)
        } else {
            super::intended_pos_known_ns(ps)
        };
        if !ps.can_retry_uri_after_release(route_attempt, offer_id, phase, known_position_ns) { return; }
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

    pub(super) fn finalize_uri_once(&mut self) -> bool {
        if self.uri_finalized { false } else {
            self.uri_finalized = true;
            true
        }
    }

    pub(super) fn active_uri_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        self.uri_route_attempt
    }

    pub(super) fn uri_backend_exit_pending(&self) -> bool {
        self.physical.uri().is_some_and(|driver|
            driver.exit_disposition() == ExitDisposition::PreserveNativeOwnership)
    }

    /// A quarantined native registration must survive the stack unwind before `_exit`.
    pub(super) fn park_uri_backend_for_exit(&mut self) {
        if let Some(driver) = self.physical.take_uri() { std::mem::forget(driver); }
    }

    /// Land URI readiness/failure against the same route transaction as a Feed Load receipt.
    /// A generic queue answer or clock observation cannot settle the start.
    pub fn backend_poll_session_at(&mut self, ps: &mut crate::route::PlaybackSession,
        at: super::attempt_metrics::Tick) -> backend::RunResult {
        self.refresh_uri_qualification(ps);
        let uri_viewer_goal = self.uri_route_attempt.and_then(|route|
            self.backend.viewer_paused(BackendKind::Uri, route.contract_id()));
        let result = self.backend_poll_at(ps, at);
        let mut failed_startup = None;
        for notice in &result.playback {
            if let plx_playback_contract::Fx::AcceptedObservation {
                backend: BackendKind::Uri, attempt, observation,
            } = *notice {
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
                        self.refused_uri_audio = None;
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
                        self.refused_uri_subtitle = None;
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
                            self.refused_uri_audio = self.pending_uri_audio.take();
                        }
                    }
                    plx_playback_contract::TrackKind::Subtitle => {
                        if self.pending_uri_subtitle.as_ref().is_some_and(|pending|
                            pending.attempt == attempt && pending.command == command) {
                            self.refused_uri_subtitle = self.pending_uri_subtitle.take();
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
                        self.refused_uri_audio = None;
                        self.refused_uri_subtitle = None;
                    }
                }
                _ => {}
            }
        }
        if let Some(attempt) = failed_startup.filter(|attempt|
            self.uri_route_attempt.is_some_and(|route| route.contract_id() == *attempt)) {
            // The common startup verdict alone distinguishes mandatory initial selection,
            // seek or Play from a later interactive refusal. Retire its exact native owner.
            let _ = self.backend_send_at(ps, Intent::Stop { backend: BackendKind::Uri, attempt,
                reason: plx_playback_contract::StopReason::Replace },
                super::attempt_metrics::Origin::Fresh, at);
        }
        self.publish_uri_timeline(ps);
        {
            let mut release_fx = std::mem::take(&mut self.release_fx);
            if self.uri_route_attempt.is_some() && self.release.is_busy() {
                self.release_step(ps, &super::release::ReleaseEvent::Tick {
                    now: std::time::Instant::now(),
                }, &mut release_fx);
            }
            super::route_start::run_uri_queued_effects(ps, self, release_fx);
        }
        result
    }

    /// The existing attempt-bound reporter consumes normalized URI clocks too. Missing native
    /// position or a non-playing state clears its mailbox, so it cannot repeat an old playhead.
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
                    plx_playback_contract::Playback::Playing => {
                        facts.playing_observed = Some(true);
                        // Playing again ends a rebuffer whose `bufferingEnd` never arrived;
                        // a rebuffer never reported stays unknown.
                        if facts.rebuffering == Some(true) { facts.rebuffering = Some(false); }
                        plx_playback_contract::readout::PlaybackState::Playing
                    },
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
            plx_playback_contract::Observation::Rebuffering(observed) => {
                // The same readout the Feed publishes: `rebuffering` plus the Buffering state.
                // The service need not re-send Playing after `bufferingEnd`, so the state is
                // restored here, and only from the Buffering this event itself entered.
                let state = plx_playback_contract::readout::PlaybackState::Buffering;
                if observed.value {
                    facts.rebuffering = Some(true);
                    if facts.status == Some(plx_playback_contract::readout::PlaybackState::Playing) {
                        facts.status = Some(state);
                    }
                } else {
                    if facts.rebuffering == Some(true) && facts.status == Some(state)
                        && facts.playing_observed == Some(true) {
                        facts.status = Some(plx_playback_contract::readout::PlaybackState::Playing);
                    }
                    facts.rebuffering = Some(false);
                }
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
    pub(super) fn next_source_key(&mut self) -> SourceKey {
        self.next_source_key = self.next_source_key.checked_add(1).expect("source keys exhausted");
        SourceKey::new(self.next_source_key)
    }

    /// Begin a native open: `false` while an abandoned Load still owns the native object.
    pub(super) fn open_begin(&mut self) -> bool {
        self.physical.feed().is_some_and(|feed| feed.open_begin(&self.mt))
    }

    /// Prepare the native open and answer its native epoch; the feed parks the prepared open
    /// until [`Self::open_stage`] or [`Self::open_abort`].
    pub(super) fn open_prepare(&mut self, request: OpenRequest) -> Result<u32, Failure> {
        let feed = self.physical.feed().ok_or(Failure::Unavailable)?;
        feed.open_prepare(&self.mt, request)
    }

    pub(super) fn open_stage(&mut self, attempt: AttemptId, key: SourceKey,
        session: Box<dyn plx_playback_contract::demux::DemuxSession>) -> Result<(), Failure> {
        let feed = self.physical.feed().ok_or(Failure::Unavailable)?;
        feed.open_stage(attempt, key, session)
    }

    /// Drop the parked open of a start that was lost before it was staged.
    pub(super) fn open_abort(&mut self) {
        if let Some(feed) = self.physical.feed() { feed.open_abort(); }
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
            QueuedWatch::Launched(a) => QueuedLanding::Launched(a),
            QueuedWatch::Failed => QueuedLanding::Failed,
        };
        self.queued_watch = QueuedWatch::Unwatched;
        landing
    }

    /// The preview's start was just deferred: it is admitted (`admit`) or refused when it runs.
    pub fn admit_preview_at_launch(&mut self, admit: bool) {
        self.preview_admission = Some(admit);
    }

    /// The queued start ended; settle the preview's admission with how it ended. `launched` false
    /// (refused at the deadline, failed) counts as a refused admission; a cancelled start
    /// (`settle = false`) leaves the preview machine to the stop that cancelled it.
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

    /// Is a stopped native object's `Unload` still owed its main-thread tail? Tests only: the
    /// production readers are the release machine and the Feed's own exit disposition.
    #[cfg(test)]
    pub fn has_pending_release(&self) -> bool {
        self.probe().resources(|resources| resources.has_pending_release())
    }

    /// The release machine's verdict on one event; the effects are the caller's to run. The
    /// machine stays the owner of the queue; the session's readout copy of the one fact
    /// `player::state` needs (`start_queued`) is stepped here, at the step that changed it, so
    /// every path that steps the machine (a URI `Released` verdict taken in `backend_poll_at`
    /// included) reaches the readout without waiting for its queued effect to run.
    pub(super) fn release_step(&mut self, ps: &mut crate::route::PlaybackSession,
        event: &super::release::ReleaseEvent, fx: &mut Vec<super::release::ReleaseEffect>) {
        self.release.step(event, fx);
        ps.step_readout(super::readout::ReadoutEvent::ReleaseQueue {
            queued: self.release.has_queued() });
    }

    /// An `Unload` is running or overdue: the object slot is not free.
    pub fn release_busy(&self) -> bool {
        self.release.is_busy()
    }

    /// The `Unload` outlived its deadline: it stays parked and every start is refused.
    pub fn release_overdue(&self) -> bool {
        self.release.is_overdue()
    }

    /// The epoch of the parked release whose `Unload` worker has returned and been joined (so
    /// joining it cannot block the frame): the release machine's `UnloadReturned`.
    pub(super) fn take_returned_release(&mut self) -> Option<u32> {
        self.physical.feed().and_then(|feed| feed.take_returned_release())
    }

    /// The main-thread tail of the release [`Self::take_returned_release`] reported for `epoch`.
    pub(super) fn run_release_tail(&mut self, epoch: u32) -> bool {
        let mt = &self.mt;
        self.physical.feed().is_some_and(|feed| feed.run_release_tail(mt, epoch))
    }

    /// The exit leaks what a worker still in firmware reads (`route_start::exit_native_with`).
    pub(super) fn leak_for_exit(&mut self) {
        if let Some(feed) = self.physical.feed() { feed.leak_for_exit(); }
        self.clear_feed_attempt();
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
    #[cfg(test)]
    #[inline]
    pub fn engine(&mut self) -> Option<std::cell::RefMut<'_, Engine>> {
        self.probe().engine()
    }

    /// Route identity of the active physical feed, without lending Engine state to admission.
    pub(super) fn active_feed_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        self.feed_attempt.map(|bound| bound.attempt)
    }

    /// Record the attempt the Engine just installed belongs to. Called by the one start that
    /// installs, right after the install.
    pub(super) fn bind_feed_attempt(&mut self, attempt: crate::route::RouteStartAttempt,
        trace_generation: u32) {
        debug_assert!(self.physical.feed().is_some_and(|feed| feed.sample().is_some()),
            "a bound attempt has an installed Engine");
        self.feed_attempt = Some(BoundFeed { attempt, trace_generation,
            foreground_follows_load: false });
    }

    pub(super) fn clear_feed_attempt(&mut self) {
        self.feed_attempt = None;
    }

    /// Begin the native stop: the Feed stops its HLS clock, samples the outgoing attempt's last
    /// facts while it is still the addressed one (published into `ps` here) and takes its Engine
    /// out; the bound attempt is dropped with it.
    pub(super) fn stop_begin(&mut self, ps: &mut crate::route::PlaybackSession, terminal: bool) {
        let attempt = self.active_feed_attempt();
        let last = self.physical.feed().and_then(|feed| feed.stop_begin(terminal));
        if let (Some(attempt), Some(facts)) = (attempt, last) {
            let _ = ps.ingest_feed_facts(attempt, facts);
        }
        self.clear_feed_attempt();
    }

    pub(super) fn stop_side_reader(&mut self, for_reload: bool) {
        if let Some(feed) = self.physical.feed() { feed.stop_side_reader(for_reload); }
    }

    pub(super) fn stop_unblock(&mut self) -> plx_playback_contract::feed::UnblockFacts {
        self.physical.feed().map(|feed| feed.stop_unblock())
            .expect("a stop in progress has a feed")
    }

    pub(super) fn stop_report_dispatched(&mut self) {
        if let Some(feed) = self.physical.feed() { feed.stop_report_dispatched(); }
    }

    pub(super) fn stop_release(&mut self, for_reload: bool) {
        let mt = &self.mt;
        if let Some(feed) = self.physical.feed() { feed.stop_release(mt, for_reload); }
    }

    /// End the native stop. A release the Feed handed off to its off-frame worker is the release
    /// machine's admission fence for the next start, so the machine hears of it here.
    pub(super) fn stop_finish(&mut self, ps: &mut crate::route::PlaybackSession, for_reload: bool) {
        let handoff = self.physical.feed().and_then(|feed| feed.stop_finish(for_reload));
        if let Some(plx_playback_contract::feed::ReleaseHandoff { epoch, at }) = handoff {
            self.release_step(ps, &super::release::ReleaseEvent::UnloadHandedOff { epoch, at },
                &mut Vec::new());
        }
    }

    /// Reset the Feed's session mirrors, and `common` (the player's own session stores) with
    /// them inside the Feed's callback/reset barrier. A composition with no feed has only
    /// `common`.
    pub(super) fn reset_session(&mut self, scope: plx_playback_contract::feed::ResetScope,
        common: &mut dyn FnMut()) {
        match self.physical.feed() {
            Some(feed) => feed.reset(scope, common),
            None => common(),
        }
    }

    /// One sample of the live feed, `None` with no session: an idle feed's mailboxes are not an
    /// observation of any attempt.
    pub(super) fn sample_feed(&mut self) -> Option<plx_playback_contract::feed::FeedFacts> {
        self.physical.feed().and_then(|feed| feed.sample())
    }

    /// Address the physical callback snapshot to the current route-owned attempt.
    pub(super) fn publish_feed_attempt_view(&mut self, ps: &mut crate::route::PlaybackSession) {
        let Some(attempt) = self.active_feed_attempt() else { return; };
        let Some(facts) = self.sample_feed() else { return; };
        let _ = ps.ingest_feed_facts(attempt, facts);
    }

    /// Hold the inbox of the adaptive worker of the open just claimed under `attempt`.
    pub(super) fn bind_abr_inbox(
        &mut self, attempt: crate::route::RouteStartAttempt, inbox: crate::adaptive::AbrInbox,
    ) {
        self.abr_inbox = Some((attempt, inbox));
    }

    /// Take what the adaptive worker has reported since the last call into the session's readout
    /// (its photograph, the carried link estimate and the Original failure edit it has not handed
    /// out yet), addressed to the attempt its open was claimed under (a late report of a retired open
    /// changes nothing). Never blocks. The age of the carried estimate is read here, where the clock is.
    pub(super) fn drain_abr(&mut self, ps: &mut crate::route::PlaybackSession) {
        let Some((attempt, inbox)) = self.abr_inbox.as_ref() else { return; };
        let Some(shipment) = inbox.newest() else { return; };
        let seed = shipment.seed.map(|(seed, observed_at)| {
            (seed, u64::try_from(observed_at.elapsed().as_millis()).unwrap_or(u64::MAX))
        });
        let _ = ps.ingest_abr(*attempt, shipment.readout, seed);
        if let Some((_, edit)) = shipment.failure {
            let _ = ps.ingest_original_failure(*attempt, edit);
        }
    }

    /// The worker is joined: nothing more can be reported, and the receiver goes.
    pub(super) fn close_abr_inbox(&mut self) { self.abr_inbox = None; }

    /// Hand the feed one player intent. A composition with no feed drops it, as it does every
    /// other feed verb.
    pub(super) fn feed_command(&mut self, command: plx_playback_contract::feed::FeedCommand) {
        if let Some(feed) = self.physical.feed() { feed.command(command); }
    }
    /// The viewer's Pause, carried out by the feed (clock transition, native Pause, transport
    /// commit). A composition with no feed has nothing to pause: the Pause is deferred.
    pub(super) fn feed_user_pause(&mut self) -> plx_playback_contract::feed::UserPauseOutcome {
        self.physical.feed().map_or(plx_playback_contract::feed::UserPauseOutcome::Deferred,
            |feed| feed.user_pause(&self.mt))
    }
    /// The viewer's Resume, carried out by the feed. A composition with no feed holds nothing.
    pub(super) fn feed_user_resume(&mut self) -> plx_playback_contract::feed::UserResumeOutcome {
        self.physical.feed().map_or(plx_playback_contract::feed::UserResumeOutcome::NoHold,
            |feed| feed.user_resume(&self.mt))
    }
    /// The volume a paused preroll's mute saved, taken from the feed; `None` when nothing is muted.
    pub(super) fn feed_take_preroll_volume(&mut self) -> Option<i32> {
        self.physical.feed().and_then(|feed| feed.take_preroll_volume())
    }
    /// Retiring a preview: the feed defers the join of a Load that has not returned and marks the
    /// preview abandoned (see [`FeedLoad::defer_preview_join`]); `true` when it deferred.
    pub(super) fn defer_preview_join(&mut self) -> bool {
        self.physical.feed().is_some_and(|feed| feed.defer_preview_join())
    }
    /// Whether the foreground lifecycle follows the bound attempt's Load.
    pub(super) fn foreground_follows_feed_load(&self) -> bool {
        self.feed_attempt.is_some_and(|bound| bound.foreground_follows_load)
    }
    /// Mark the foreground as following `attempt`'s Load: true only for the bound attempt.
    pub(super) fn mark_foreground_follows_feed_load(&mut self,
        attempt: crate::route::RouteStartAttempt) -> bool {
        let Some(bound) = self.feed_attempt.as_mut().filter(|bound| bound.attempt == attempt)
            else { return false; };
        bound.foreground_follows_load = true;
        true
    }

    /// One physical seek-preroll tick. The common pump supplies route-flight facts and applies
    /// the returned finish request after this borrow of feed resources ends.
    pub(super) fn paused_seek_step(&mut self, now: u32, route_flight: bool,
        claim_owns_pause: bool) -> plx_playback_contract::feed::PausedSeekStep {
        self.physical.feed().map_or_else(plx_playback_contract::feed::PausedSeekStep::default,
            |feed| feed.paused_seek_step(now, route_flight, claim_owns_pause))
    }

    /// Publish native queue and frame diagnostics without exposing Engine to the common prelude.
    pub(super) fn publish_feed_diag(&mut self, now: u32,
        last_frames: &std::sync::atomic::AtomicI32) {
        let diag_readout_on = super::DIAG_READOUT_ON.load(std::sync::atomic::Ordering::Relaxed);
        if let Some(feed) = self.physical.feed() {
            feed.publish_diag(now, last_frames, diag_readout_on);
        }
    }

    /// Feed the dev firmware-refusal probe with route facts, before any route landing can replace
    /// this physical Engine in the same frame.
    pub(super) fn inject_refuse601(&mut self, generation: u32, preview: bool,
        refused_for: &std::sync::atomic::AtomicU32) {
        if let Some(feed) = self.physical.feed() {
            feed.inject_refuse601(generation, preview, refused_for);
        }
    }

    /// Probe the native audio feed's own stall evidence after common Load-refusal handling.
    pub(super) fn native_audio_stall_fired(&mut self, now: u32) -> bool {
        self.physical.feed().is_some_and(|feed| feed.native_audio_stall_fired(now))
    }

    /// The Dolby Vision landing guard's verdict for this frame (#1166): the withdrawal the player must
    /// answer with one reload, if any.
    pub(super) fn dv_landing_due(&mut self, now: u32, route_flight: bool)
        -> Option<plx_playback_contract::feed::DvWithdrawal> {
        self.physical.feed().and_then(|feed| feed.dv_landing_due(now, route_flight))
    }

    /// Gate the active physical tick on native constructor readiness.
    pub(super) fn feed_ctor_gate(&mut self) -> plx_playback_contract::feed::CtorGate {
        let mt = &self.mt;
        self.physical.feed().map_or(
            plx_playback_contract::feed::CtorGate::Awaiting { timeout: None },
            |feed| feed.ctor_gate(mt))
    }

    /// Emit the physical callback-cadence diagnostic after first-picture route settlement.
    pub(super) fn note_missing_era_callback(&mut self) {
        if let Some(feed) = self.physical.feed() { feed.note_missing_era_callback(); }
    }

    pub(super) fn stuck_seek_due(&mut self, now: u32, feed_allowed: bool,
        route_flight: bool, budget_ms: u32) -> bool {
        self.physical.feed().is_some_and(|feed|
            feed.stuck_seek_due(now, feed_allowed, route_flight, budget_ms))
    }

    pub(super) fn retry_stuck_seek(&mut self, now: u32, target_ns: i64,
        can_retry: bool) -> plx_playback_contract::feed::StuckSeekAction {
        self.physical.feed().map_or(plx_playback_contract::feed::StuckSeekAction::Reload,
            |feed| feed.retry_stuck_seek(now, target_ns, can_retry))
    }

    pub(super) fn pending_seek_ready(&mut self, seek_reloads: bool) -> bool {
        self.physical.feed().is_some_and(|feed| feed.pending_seek_ready(seek_reloads))
    }
    /// One native Pause transition before an in-place seek. A composition with no feed has no
    /// native session to pause: `Busy`, so the seek waits and applies nothing.
    pub(super) fn prepare_seek_pause(&mut self) -> plx_playback_contract::feed::SeekPause {
        let mt = &self.mt;
        self.physical.feed().map_or(plx_playback_contract::feed::SeekPause::Busy,
            |feed| feed.prepare_seek_pause(mt))
    }
    pub(super) fn begin_seek_discontinuity(&mut self) -> bool {
        self.physical.feed().is_some_and(|feed| feed.begin_seek_discontinuity())
    }
    pub(super) fn apply_inplace_seek_pre(&mut self, target_ns: i64,
        mode: plx_playback_contract::feed::SeekMode, coalesced: u32) {
        let mt = &self.mt;
        if let Some(feed) = self.physical.feed() {
            feed.apply_inplace_seek_pre(mt, target_ns, mode, coalesced);
        }
    }
    pub(super) fn apply_inplace_seek_post(&mut self, target_ns: i64, now: u32) {
        if let Some(feed) = self.physical.feed() { feed.apply_inplace_seek_post(target_ns, now); }
    }

    pub(super) fn tick_feed_load(&mut self, now: u32, segmented_hls: bool,
        speed_deadline_ms: u32) -> plx_playback_contract::feed::LoadTick {
        let mt = &self.mt;
        self.physical.feed().map_or_else(plx_playback_contract::feed::LoadTick::default, |feed|
            feed.tick_load(mt, now, segmented_hls, speed_deadline_ms,
                super::no_audio_armed()))
    }

    pub(super) fn tick_feed_presentation(&mut self, segmented_hls: bool,
        immersive: bool) -> plx_playback_contract::feed::PresentationTick {
        let mt = &self.mt;
        self.physical.feed().map_or(
            plx_playback_contract::feed::PresentationTick {
                stage: plx_playback_contract::feed::Stage::Loading, eos_pushed: false,
            },
            |feed| feed.tick_presentation(mt, segmented_hls, immersive))
    }

    /// Is a session live? Answered from the bound attempt, which the one start that installs the
    /// Engine binds right after the install and [`stop_begin`](Self::stop_begin)
    /// drops in the call that takes it out; `start_bufferfeed`'s double-start guard only needs to
    /// ask.
    #[inline]
    pub fn is_live(&self) -> bool {
        self.feed_attempt.is_some()
    }

    /// Address one Load verdict without handing route state to the native worker.
    #[cfg(all(test, feature = "hostsim"))]
    pub(super) fn load_receipt(&mut self, attempt: crate::route::RouteStartAttempt, native_epoch: u32,
        _trace_generation: u32) -> plx_play_feed::feed_resources::LoadReceipt {
        self.probe().resources(|resources|
            resources.load_receipt(attempt.contract_id(), native_epoch))
    }

    /// A refused thread spawn cannot return a receipt; retire its mapping immediately.
    #[cfg(all(test, feature = "hostsim"))]
    pub(super) fn cancel_load_receipt(&mut self, token: u64) {
        self.probe().resources(|resources| resources.cancel_load_receipt(token));
    }

    /// The main-thread coordinator's only Load-result landing. Late outcomes from an abandoned
    /// or replaced Engine cannot settle a successor's route, even if a worker returns afterward.
    pub(super) fn drain_load_outcomes(&mut self, ps: &mut crate::route::PlaybackSession) {
        // Construction may still be installing the Engine when the synchronous Load returns.
        // Leave the inbox intact until its owner can validate the native epoch and attempt.
        if self.feed_attempt.is_none() { return; }
        let Some(feed) = self.physical.feed() else { return; };
        let outcomes = feed.take_load_outcomes();
        for outcome in outcomes {
            let Some(BoundFeed { attempt, trace_generation, .. }) = self.feed_attempt
                .filter(|bound| bound.attempt.contract_id() == outcome.attempt) else { continue; };
            if let Some(elapsed_ms) = outcome.elapsed_ms {
                super::report::note_load_gate_for(
                    trace_generation,
                    super::report::LoadElapsedClass::from_ms(elapsed_ms as i64),
                );
            }
            let result = if outcome.ok { crate::route::RouteStartResult::Started }
                else { crate::route::RouteStartResult::StartFailed };
            let _ = crate::route::settle_route_start(ps, attempt, result);
        }
    }

    /// A preview has no route lease, hence no periodic PMS writes. This follows the existing
    /// begin_timeline_reporting gate and keeps the reporter outside the Engine resource set.
    pub(super) fn start_timeline(&mut self, ps: &crate::route::PlaybackSession,
        attempt: crate::route::RouteStartAttempt) {
        debug_assert!(self.timeline.is_none(), "one reporter per player adapter");
        if self.timeline.is_some() { return; }
        if let Some(lease) = crate::route::begin_timeline_reporting(ps) {
            self.timeline = timeline::TimelineRunner::start(attempt, lease);
        }
    }

    pub(super) fn publish_timeline(&self, attempt: crate::route::RouteStartAttempt,
        snapshot: Option<timeline::TimelineSnapshot>) {
        if let Some(runner) = self.timeline.as_ref() { runner.publish(attempt, snapshot); }
    }

    /// Stop and detach without joining on the main thread. Real stop passes this handle to
    /// route::scrobble_stop; reload simply drops it after the stop signal became sticky.
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
    #[cfg(test)]
    pub fn install(&mut self, e: Engine) {
        self.feed_attempt = Some(BoundFeed { attempt: crate::route::RouteStartAttempt::fixture(),
            trace_generation: 0, foreground_follows_load: false });
        self.probe().resources(|resources| resources.install(e));
    }

    /// Take the session out of the slot; the caller then joins its workers and drops it.
    #[inline]
    #[cfg(test)]
    pub fn take(&mut self) -> Option<Engine> {
        self.feed_attempt = None;
        self.probe().resources(|resources| resources.take())
    }

    /// Is a URI-mode probe session running? `start_bufferfeed`'s double-start guard asks this beside
    /// [`is_live`](Self::is_live): a probe holds the route-start attempt and the exported window, so
    /// a second Play cannot begin under it.
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub fn uri_live(&self) -> bool {
        self.uri.is_some()
    }

    /// The service says the probe's video plays (`observe_video_plane`'s term for a session which
    /// has no Engine to read the ACB play state from).
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub fn uri_plane_open(&self) -> bool {
        self.uri.as_ref().is_some_and(|session| session.plane_open())
    }

    #[cfg(feature = "devtriggers")]
    pub fn uri_closing(&self) -> bool {
        self.uri.as_ref().is_some_and(|session| session.closing())
    }

    #[cfg(feature = "devtriggers")]
    pub(super) fn uri_attempt(&self) -> Option<crate::route::RouteStartAttempt> {
        self.uri.as_ref().map(|session| session.route_start_attempt())
    }

    #[cfg(feature = "devtriggers")]
    pub(super) fn uri_acb_exit_quarantine(&self) -> bool { self.uri.as_ref().is_some_and(|session| session.acb_exit_quarantine()) }

    /// [`uri_acb_exit_quarantine`](Self::uri_acb_exit_quarantine) for a suite outside this crate
    /// (`dev::uriprobe`'s tests); the product reads it only inside `exit_native`.
    #[cfg(all(feature = "devtriggers", any(test, feature = "test-support")))]
    pub fn uri_acb_exit_quarantine_for_test(&self) -> bool { self.uri_acb_exit_quarantine() }

    /// Install the probe's session. `uriprobe_drive::start` guards on [`uri_live`](Self::uri_live)
    /// first: overwriting a live slot would drop a session without ending it.
    #[cfg(feature = "devtriggers")]
    pub(super) fn install_uri(&mut self, session: Box<dyn super::uriprobe_drive::UriProbeSession>) {
        debug_assert!(self.uri.is_none(), "a second URI-mode session cannot start under a live one");
        self.uri = Some(session);
    }

    /// Install the application's probe at boot (`dev::uriprobe`). A later install replaces it; a
    /// session already open keeps running under the probe which opened it.
    #[cfg(feature = "devtriggers")]
    pub fn install_uri_probe(&mut self, probe: Box<dyn super::uriprobe_drive::UriProbe>) {
        self.uri_probe = Some(probe);
    }

    /// Seat a probe session the way `uriprobe_drive::start` does, for a suite outside this crate that
    /// drives a session it built over a scripted transport (`dev::uriprobe`'s tests).
    #[cfg(all(feature = "devtriggers", any(test, feature = "test-support")))]
    pub fn install_uri_for_test(&mut self, session: Box<dyn super::uriprobe_drive::UriProbeSession>) {
        self.install_uri(session);
    }

    /// Did the composition install a probe? Without one the trigger file is not read at all.
    #[cfg(feature = "devtriggers")]
    pub fn uri_probe_installed(&self) -> bool {
        self.uri_probe.is_some()
    }

    /// Whether the installed probe finds the native object free of an earlier session (see
    /// [`UriProbe::native_slot_available`](super::uriprobe_drive::UriProbe::native_slot_available)).
    #[cfg(feature = "devtriggers")]
    pub(super) fn uri_probe_native_slot_available(&self) -> bool {
        self.uri_probe.as_ref().is_some_and(|probe| probe.native_slot_available())
    }

    /// The launch the installed probe arms for a `plxnative-uriprobe` trigger text; `None` when it
    /// arms none (or nothing is installed).
    #[cfg(feature = "devtriggers")]
    pub fn uri_probe_armed(&self, text: &str) -> Option<Box<dyn super::uriprobe_drive::ProbeLaunch>> {
        self.uri_probe.as_ref().and_then(|probe| probe.armed(text))
    }

    /// Take a physically released probe out of the slot, or leak unresolved ownership at exit.
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub(super) fn take_uri(&mut self) -> Option<Box<dyn super::uriprobe_drive::UriProbeSession>> {
        self.uri.take()
    }

    /// The probe's session AND the token at once, borrowed from disjoint fields (the same split
    /// [`split`](Self::split) makes for the Engine).
    #[cfg(feature = "devtriggers")]
    #[inline]
    pub(super) fn uri_split(&mut self) -> (Option<&mut Box<dyn super::uriprobe_drive::UriProbeSession>>, &MainThread) {
        (self.uri.as_mut(), &self.mt)
    }

    /// Is a native object still parked behind a Load that has not been released? (At most one can
    /// exist: the C seam owns one object at a time and every start is refused while it does.)
    pub fn has_abandoned_load(&mut self) -> bool {
        self.physical.feed().is_some_and(|feed| feed.has_abandoned_load())
    }

    pub fn reap_abandoned_load(&mut self) -> bool {
        self.physical.feed().is_some_and(|feed| feed.reap_abandoned_load(&self.mt))
    }

    /// The token, for the ACB/Starfish seam. `tv::sink::VideoSink`'s methods still take one — see the
    /// module doc for why that surface keeps its own argument.
    ///
    /// Does not wait for a pending off-frame release: a caller holding the token may call any verb,
    /// and while the object is `Releasing` [`Self::display_sink`] answers a sink on which every verb
    /// is a no-op, so none of them reaches the object the worker is inside.
    #[inline]
    pub fn mt(&self) -> &MainThread {
        &self.mt
    }

    /// Hostsim fixture access to the session and main-thread token together. Production common
    /// code uses the feed's typed actions and does not borrow the Engine.
    #[inline]
    #[cfg(test)]
    pub fn split(&mut self) -> (Option<std::cell::RefMut<'_, Engine>>, &MainThread) {
        (self.probe().engine(), &self.mt)
    }
}

#[cfg(test)]
mod repair_receipt_tests {
    use super::*;
    use plx_platform::tv::sandbox::{Failure, State};
    #[test]
    fn local_metric_history_evicts_old_effects_after_real_reducer_results() {
        use super::super::attempt_metrics::{Origin, RequestOutcome, Tick, Fx};
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
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
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
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

#[cfg(test)]
mod uri_stop_refusal_tests {
    use super::*;
    use plx_playback_contract::{BackendEvent, DriverEvent, DriverSet, EvidenceSource,
        ExitDisposition, Fidelity, InitialPlayback, Observation, Observed, Playback,
        Requirements, SourceKey, SourceKind, TrackChoice};

    // Only establishes the common active owner. The Stop under test still goes through the
    // adapter's actual physical set, where the URI port is unavailable.
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
    fn uri_rebuffering_publishes_the_shared_readout_and_unreported_stays_unknown() {
        use plx_playback_contract::readout::PlaybackState as S;
        let _serial = plx_base::testlock::serial();
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        let route = crate::route::RouteStartAttempt::fixture();
        adapter.uri_route_attempt = Some(route);
        let mut ps = crate::route::PlaybackSession::default();
        ps.begin_attempt_view(route, super::super::attempt_view::Backend::Uri);
        let attempt = route.contract_id();
        let say = |value| Observation::Playback(Observed { value,
            from: EvidenceSource::BackendReport });
        let rebuffer = |value| Observation::Rebuffering(Observed { value,
            from: EvidenceSource::BackendReport });
        let facts = |ps: &crate::route::PlaybackSession| {
            let f = ps.attempt_view().facts(); (f.status, f.rebuffering)
        };
        adapter.publish_uri_observation(&mut ps, attempt, say(Playback::ReadyAwaitPlay));
        assert_eq!(facts(&ps), (Some(S::Buffering), None), "startup buffering is not a rebuffer");
        adapter.publish_uri_observation(&mut ps, attempt, say(Playback::Playing));
        assert_eq!(facts(&ps), (Some(S::Playing), None), "unreported stays unknown, not false");
        adapter.publish_uri_observation(&mut ps, attempt, rebuffer(true));
        assert_eq!(facts(&ps), (Some(S::Buffering), Some(true)));
        adapter.publish_uri_observation(&mut ps, attempt, rebuffer(false));
        assert_eq!(facts(&ps), (Some(S::Playing), Some(false)),
            "bufferingEnd restores Playing; the service need not re-send it");
        adapter.publish_uri_observation(&mut ps, attempt, rebuffer(true));
        adapter.publish_uri_observation(&mut ps, attempt, say(Playback::Playing));
        assert_eq!(facts(&ps), (Some(S::Playing), Some(false)),
            "a Playing report after a lost bufferingEnd ends the rebuffer");
    }

    #[test]
    fn unavailable_uri_driver_cannot_begin_release_or_queue_a_successor() {
        let _serial = plx_base::testlock::serial();
        let mut adapter = PlayerAdapter::new_fed(unsafe { MainThread::assume() });
        // The route still names the old physical attempt, but no URI driver can accept Stop.
        // This is the same unavailable-driver result the adapter's physical set returns.
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
        let mut ps = crate::route::PlaybackSession::IDLE;
        let at = adapter.backend_tick();
        let result = adapter.backend_send_at(&mut ps, Intent::Stop {
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
    use plx_play_feed::shared::FEED_TX;
    use super::*;

    #[test]
    fn load_outcome_waits_for_its_engine_and_refuses_late_epoch() {
        let _serial = plx_base::testlock::serial();
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::reset_player_control_for_test(&ps);
        let transaction = crate::route::begin_route_start().expect("start transaction");
        assert!(crate::route::prepare_route_start(transaction));
        let attempt = crate::route::claim_route_start_attempt(transaction).expect("physical attempt");
        let mut pa = PlayerAdapter::new_probed(unsafe { MainThread::assume() });

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
        crate::player::reset_session_for_test();
        FEED_TX.reset();
        crate::route::reset_player_control_for_test(&ps);
    }

    /// `is_live` is the bound attempt: the Engine the start installs is bound right after, and the
    /// call that takes it out drops the binding.
    #[test]
    fn a_session_is_live_exactly_while_an_attempt_is_bound() {
        let _serial = plx_base::testlock::serial();
        let attempt = crate::route::RouteStartAttempt::fixture();
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut pa = PlayerAdapter::new_probed(unsafe { MainThread::assume() });
        assert!(!pa.is_live() && pa.active_feed_attempt().is_none());
        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.attempt = attempt.contract_id();
        pa.install(eng);
        pa.bind_feed_attempt(attempt, 7);
        assert!(pa.is_live());
        assert_eq!(pa.active_feed_attempt(), Some(attempt));
        assert!(pa.sample_feed().is_some(), "the Feed has the Engine the binding names");
        pa.stop_begin(&mut ps, false);
        pa.stop_finish(&mut ps, false);
        assert!(!pa.is_live() && pa.active_feed_attempt().is_none(), "the stop drops the binding");
        assert!(pa.sample_feed().is_none(), "and the Feed has no session to sample");
        crate::player::reset_session_for_test();
        FEED_TX.reset();
    }

    /// The foreground's "I follow this Load" mark belongs to the bound attempt: a mark for another
    /// attempt, or with nothing bound, is refused; every bind starts unmarked; the stop drops it.
    #[test]
    fn the_foreground_follows_mark_belongs_to_the_bound_attempt() {
        let _serial = plx_base::testlock::serial();
        let bound = crate::route::RouteStartAttempt::fixture();
        let other = crate::route::RouteStartAttempt::fixture_n(2);
        let mut pa = PlayerAdapter::new_probed(unsafe { MainThread::assume() });
        assert!(!pa.mark_foreground_follows_feed_load(bound), "nothing is bound");
        assert!(!pa.foreground_follows_feed_load());

        let mut eng = crate::player::engine::streaming_engine_for_test(8);
        eng.attempt = bound.contract_id();
        pa.install(eng);
        pa.bind_feed_attempt(bound, 0);
        assert!(!pa.foreground_follows_feed_load(), "a bind starts unmarked");
        assert!(!pa.mark_foreground_follows_feed_load(other), "another attempt's mark is refused");
        assert!(!pa.foreground_follows_feed_load());
        assert!(pa.mark_foreground_follows_feed_load(bound));
        assert!(pa.foreground_follows_feed_load());

        pa.bind_feed_attempt(bound, 0);
        assert!(!pa.foreground_follows_feed_load(), "a rebind resets the mark");
        assert!(pa.mark_foreground_follows_feed_load(bound));
        let mut ps = crate::route::PlaybackSession::IDLE;
        pa.stop_begin(&mut ps, false);
        pa.stop_finish(&mut ps, false);
        assert!(!pa.foreground_follows_feed_load(), "the stop drops the mark with the binding");
        assert!(!pa.mark_foreground_follows_feed_load(bound), "and nothing is bound to mark");
        crate::player::reset_session_for_test();
        FEED_TX.reset();
    }

    #[test]
    fn elapsed_fact_keeps_its_captured_trace_after_engine_installation() {
        let _serial = plx_base::testlock::serial();
        let attempt = crate::route::RouteStartAttempt::fixture();
        let mut pa = PlayerAdapter::new_probed(unsafe { MainThread::assume() });
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
        let wrong_owner = pa.probe().resources(|resources|
            resources.load_receipt(wrong_attempt, 8));
        wrong_owner.deliver(8, false, Some(790));
        let cancelled = pa.load_receipt(attempt, 8, 44);
        let cancelled_token = cancelled.token();
        cancelled.deliver(8, false, Some(900));
        pa.cancel_load_receipt(cancelled_token);

        let admitted = pa.physical.feed().expect("the feed is installed").take_load_outcomes();
        assert_eq!(admitted.len(), 1, "only the installed Engine's exact attempt and epoch are admitted");
        assert_eq!(admitted[0].attempt, attempt.contract_id());
        assert_eq!(pa.feed_attempt.map(|bound| (bound.attempt, bound.trace_generation)),
            Some((attempt, 41)), "the common owner retains the captured trace");
        assert_eq!(admitted[0].elapsed_ms, Some(123));
        assert!(admitted[0].ok);
        assert!(pa.physical.feed().expect("the feed is installed").take_load_outcomes().is_empty(),
            "each receipt is consumed once");
        drop(pa.take());
        crate::player::reset_session_for_test();
        FEED_TX.reset();
    }
}
