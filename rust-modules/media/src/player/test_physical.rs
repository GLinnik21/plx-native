//! The tests' composition of the player's physical set: what the application's boot composes
//! (`app::adapters::backends`) from the same hook functions, for the fixtures that drive a Feed,
//! over the host sink.
//!
//! [`PlayerAdapter::new`] starts with the contract's `UriOnlySet` and an inert sink, so a test that
//! starts, feeds or stops a Feed builds its adapter with [`PlayerAdapter::new_fed`] (or
//! `new_probed`, which keeps a handle on the set), and a test that never touches one keeps `new`.

use super::adapter::PlayerAdapter;
use plx_base::task::MainThread;
use plx_play_feed::control::FeedBackend;
use plx_play_feed::feed_resources::{FeedResources, LoadReceipt};
use plx_play_feed::native_executor::{FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks};
use plx_play_feed::pcm_lead::PcmLead;
use plx_play_feed::physical::FeedSet;
use plx_play_feed::sf_event::SfEventHooks;
use plx_play_feed::shared::FEED_SHARED;
use plx_play_feed::sink_fence::SinkFence;
use plx_platform::tv::sink::{RefusingSink, UnloadTicket, VideoSink};
use plx_telemetry::telemetry::classes::DemuxExitClass;
use std::os::raw::c_char;

/// The host sink these fixtures run over: the Feed backend's stand-in with no picture to draw.
/// (The simulator's own, with its decoded picture, is the application's: `app::adapters::hostsim`.)
#[cfg(feature = "hostsim")]
static HOST_SINK: plx_play_feed::ffi_host::HostSink =
    plx_play_feed::ffi_host::HostSink::new(plx_play_feed::ffi_host::Hooks::over(&plx_play_feed::ffi_host::NoFrames));

/// The seam itself, unfenced: the host sink in a hostsim run, the installed port's otherwise.
pub(crate) fn raw_sink() -> &'static dyn VideoSink {
    #[cfg(feature = "hostsim")]
    {
        &HOST_SINK
    }
    #[cfg(not(feature = "hostsim"))]
    {
        plx_platform::tv::sink::installed()
    }
}

const FENCE: SinkFence = SinkFence::new(raw_sink, &RefusingSink(raw_sink));

/// The sink the fixtures reach the seam through, fenced while an off-frame `Unload` owns the
/// native object: what the application's composition hands the adapter beside the set.
pub(crate) fn sink() -> &'static dyn VideoSink { FENCE.current() }

/// [`super::diag`] over the fixtures' sink, for a test that holds no adapter.
#[cfg(feature = "hostsim")]
pub(crate) fn diag(ps: &crate::route::PlaybackSession) -> super::Diag { super::diag_on(ps, sink()) }

thread_local! {
    /// The test feed clock: each read advances it by `FEED_TEST_CLOCK_STEP_US`, which is 0 unless
    /// a test sets it. At 0, the host seam's near-free `sf_feed` never runs out the slice, so a
    /// slow or preempted test machine cannot change what one tick feeds.
    static FEED_TEST_CLOCK_US: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(crate) static FEED_TEST_CLOCK_STEP_US: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Monotonic µs for the executor's slice; only ever differenced.
fn feed_clock_us() -> u64 {
    let step = FEED_TEST_CLOCK_STEP_US.with(|s| s.get());
    FEED_TEST_CLOCK_US.with(|c| {
        c.set(c.get() + step);
        c.get()
    })
}

fn mirror_playstate(mt: &MainThread, stage: plx_playback_contract::feed::Stage, playing: bool) {
    super::acb_mirror_playstate_on(sink(), mt, stage, playing);
}

fn mute_for_preroll(mt: &MainThread) { super::mute_for_preroll_on(sink(), mt); }

/// The Load worker over the fixtures' fenced sink.
pub(crate) fn load_thread(payload: plx_play_feed::SendPtr<c_char>, native_epoch: u32,
    receipt: Option<LoadReceipt>, pcm_lead: PcmLead) {
    plx_play_feed::native_load::load_thread(sink, payload, native_epoch, receipt, pcm_lead);
}

fn spawn_unload(ticket: UnloadTicket) -> Option<std::thread::JoinHandle<()>> {
    #[cfg(feature = "hostsim")]
    let worker = plx_play_feed::ffi_host::UnloadWorker::enter();
    plx_base::task::spawn("sf-unload", move || {
        #[cfg(feature = "hostsim")]
        let _worker = worker;
        unsafe { raw_sink().unload(&ticket) }
    })
}

/// The HTTPS credential facade is media's (`curlio`); the Feed only calls this function. The
/// application's table builds the same value.
fn demux_transport() -> plx_play_feed::DemuxTransport {
    plx_play_feed::DemuxTransport::new(crate::curlio::range_factory(), crate::curlio::CREDENTIAL_GATE)
}

/// The hooks the fixtures' Feed runs over: the application's table (`app::adapters::backends`)
/// with the host sink and the stepped test clock in place of the television's.
pub(super) const TEST_HOOKS: NativeHooks = NativeHooks {
    prepare: PrepareHooks {
        sink,
        on_exported_window_create: super::engine::exported_window_created,
        load_refusal_latch: DemuxExitClass::LoadRefused.latch(),
        side_reader_job: super::subside::JOB,
    },
    launch: LaunchHooks {
        demux_transport,
        load_thread,
        sink,
    },
    feed: FeedHooks { sink, mirror_playstate, mute_for_preroll, feed_clock_us },
    release: ReleaseHooks {
        raw_sink,
        plane: plx_platform::tv::plane::installed(),
        on_release_step: super::engine::release_step,
        on_exported_window_destroy: super::engine::exported_window_destroyed,
        spawn_unload,
    },
};

/// Hand the pipeline callback its hooks, as the application's boot does before the first `Load`.
pub(super) fn install_sf_event_hooks() {
    let _ = plx_play_feed::sf_event::install(
        &SfEventHooks { audio_rebuild_presented: super::audio_rebuild_presented });
}

/// A Feed set over the process's shared feed state and the fixtures' hooks.
pub(super) fn feed_set() -> FeedSet {
    FeedSet::new(FeedBackend::new(FeedResources::new(&FEED_SHARED, &TEST_HOOKS)))
}

/// The start path's hooks over the real Feed, as the application composes them (`app::adapters::backends`).
pub(super) fn start_hooks() -> super::adapter::StartHooks {
    super::adapter::StartHooks {
        demux_session: super::demux_session::capture,
        play_url: plx_play_feed::playurl::playurl,
    }
}

impl PlayerAdapter {
    /// [`Self::new`] with the Feed set installed (a build with no Feed has none to install).
    pub fn new_fed(mt: MainThread) -> Self {
        let mut adapter = Self::new(mt);
        {
            install_sf_event_hooks();
            assert!(adapter.install_physical(Box::new(feed_set()), sink, start_hooks()).is_ok(),
                "a fresh adapter takes its physical set");
        }
        adapter
    }
}
