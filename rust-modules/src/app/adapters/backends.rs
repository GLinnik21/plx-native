//! The application's playback-backend composition at boot: the two decisions that belong to the
//! composition root and to no backend. Media names neither backend's presence in policy, and the
//! plane's boot is not a Feed step.
//!
//!   - [`acb_init`] boots the video plane owner (`plx_platform::tv::plane`) once per process, for
//!     EVERY backend set: the plane's boot is not a Feed step.
//!   - [`compose_backends`] installs the Feed's physical set and then tells the route session which
//!     backends the installed drivers form, so policy offers what is physically there instead of
//!     what Cargo features happened to be on.
use plx_base::eventlog::log;
use plx_base::task::MainThread;
use plx_media::player::adapter::PlayerAdapter;
#[cfg(feature = "play-feed")]
use plx_media::player::adapter::StartHooks;
use plx_media::route::PlaybackSession;
use plx_platform::tv::plane::{self, PlaneFacts, PlaneOwner, DEFAULT_PLAYER_TYPE};
use plx_platform::tv::sink::{self, DisplaySink, VP_EXPORTED, VP_NONE};
use std::os::raw::c_int;
#[cfg(feature = "play-feed")]
use plx_play_feed::{control::FeedBackend, feed_resources::{FeedResources, LoadReceipt},
    native_executor::{FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks},
    pcm_lead::PcmLead, physical::FeedSet, sf_event::SfEventHooks, shared::{Stage, FEED_SHARED},
    sink_fence::SinkFence, SendPtr};
#[cfg(feature = "play-feed")]
use plx_platform::tv::sink::{RefusingSink, UnloadTicket, VideoSink};

/// Bind the decoded video sink to the display plane, whichever way this television does it.
///
/// **Boot-scoped.** Called once, from the boot, and never again — which is why the webOS 5
/// exported window is NOT created here: that one is created per session in `start_bufferfeed`,
/// beside the payload it has to be spliced into, and destroyed by the matching `teardown`.
///
///   - `VP_ACB` — create + initialize the ACB. We deliberately DON'T register our own
///     com.webos.media client; it collides with the pipeline's uMS connection. The handle lives
///     for the process; what teardown calls is `acb_unload`, a session-scoped state change.
///   - `VP_EXPORTED` — nothing to do at boot. There is no handle, and no state mirroring:
///     webOS 5 deleted that sequence outright rather than replacing it.
///
/// It reads the installed display sink and plane owner, the same objects the Feed engine's old
/// boot resolved through `player::display()` and `player::plane()` (no off-frame `Unload` can own
/// the native object before the first boot, so the fence arm of `display()` never applied here).
pub(crate) fn acb_init(mt: &MainThread) {
    acb_init_with(mt, sink::display_installed(), plane::installed());
}

/// [`acb_init`] over the display sink and plane owner the caller names: the boot passes the
/// installed pair, a test passes a scripted sink and its own owner.
fn acb_init_with(mt: &MainThread, display: &dyn DisplaySink, owner: &PlaneOwner) {
    let facts = match display.window_mode() {
        VP_EXPORTED => {
            log("vplane: exported window (webOS 5+) — created per session");
            // Nothing to bind later; the ACB stages are skipped by the plane having no handle.
            PlaneFacts::ABSENT
        }
        VP_NONE => {
            log(
                "vplane: this device has NEITHER libAcbAPI nor SDL's exported window — audio \
                 will play and the picture will not appear. Please report your webOS version.",
            );
            PlaneFacts::ABSENT
        }
        _ => acb_init_acb(mt, display),
    };
    owner.boot(facts);
}

/// The webOS 4.x half of [`acb_init_with`].
fn acb_init_acb(mt: &MainThread, display: &dyn DisplaySink) -> PlaneFacts {
    let mut pt = DEFAULT_PLAYER_TYPE;
    if let Some(s) = plx_base::devtrig::read("ptype") {
        if let Ok(p) = s.parse::<c_int>() {
            pt = p;
        }
    }
    log(&format!("ptype={pt}"));
    // Which app ACB is being initialized for. The third argument of `AcbAPI_initialize` is an app
    // id — that much is certain, and both reference implementations pass one (Kodi's webOS port
    // passes `getenv("APPID")`; see docs/distribution.md). What ACB does with it has not been
    // decompiled here, so the rule is simply that it must be the install SAM actually launched —
    // and with a developer build able to sit beside the shipped one, "the id compiled in" and
    // "the id we were launched as" are no longer the same question.
    //
    // It used to be `env::var("APPID")` with a NULL fallback, which `starfish.c` then turned into
    // the shipped app's literal id. That is a double fallback whose failure is invisible: on any
    // launch where SAM did not export APPID, a developer install would announce itself as the
    // shipped app. `paths::app_id` reads the install directory instead, which is the id webOS
    // registered by definition. The environment is still logged, one line at boot, as the
    // independent witness for whether SAM sets it at all on this firmware.
    let app_c = std::ffi::CString::new(plx_base::paths::app_id()).ok();
    let app_ptr = app_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let acb = unsafe { display.plane_create(mt, app_ptr, pt) };
    log(&format!("acb create={acb}"));
    PlaneFacts { handle: acb != 0, player_type: pt }
}

/// Compose the backends this boot supplies: the Feed's physical set, then the session is told the
/// set the installed drivers form.
pub(crate) fn compose_backends(adapter: &mut PlayerAdapter, session: &mut PlaybackSession) {
    #[cfg(feature = "play-feed")]
    if !adapter.composed_backends().feed && !install_feed(adapter, &TV_HOOKS) {
        log("feed: physical set installation refused");
    }
    // A build with no Feed reaches the seam through the platform's own sink, unfenced: there is no
    // off-frame `Unload` to fence it from.
    #[cfg(not(feature = "play-feed"))]
    if !adapter.install_display_sink(sink::installed) {
        log("player: display sink installation refused");
    }
    session.install_backends(adapter.composed_backends());
}

/// The hooks table the Feed's native executor runs over, built over the raw sink, the slice
/// clock and the `sf-unload` worker's guard the composition names, as one value. A macro, not a
/// function, because a hook is a plain `fn` that can capture nothing: the raw sink has to be a path
/// the functions below can name. The television's table ([`TV_HOOKS`]) and the host tests'
/// (`TEST_HOOKS`) are two expansions, so what differs between them is exactly the three paths and
/// no `cfg` stands in the production one. The guard is what the Unload worker holds while it lives:
/// nothing on the television ([`no_unload_worker`]), the host drain's counter in a host test.
///
/// Every hook but the release takes the FENCED sink ([`SinkFence`]), which refuses each verb while
/// an off-frame `Unload` owns the native object; the release takes the raw one. The policy the
/// hooks run (the ACB mirror, the preroll mute, the window breadcrumbs, the HTTPS credential
/// facade) is `plx_media`'s; this table only says which sink it runs over.
#[cfg(feature = "play-feed")]
macro_rules! native_hooks_over {
    ($raw:path, $clock:path, $worker:path) => {{
        const FENCE: SinkFence = SinkFence::new($raw, &RefusingSink($raw));
        fn fenced() -> &'static dyn VideoSink { FENCE.current() }
        fn mirror_playstate(mt: &MainThread, stage: Stage, playing: bool) {
            plx_media::player::acb_mirror_playstate_on(fenced(), mt, stage, playing);
        }
        fn mute_for_preroll(mt: &MainThread) { plx_media::player::mute_for_preroll_on(fenced(), mt); }
        fn load_thread(payload: SendPtr<std::os::raw::c_char>, native_epoch: u32,
            receipt: Option<LoadReceipt>, pcm_lead: PcmLead) {
            plx_play_feed::native_load::load_thread(fenced, payload, native_epoch, receipt, pcm_lead);
        }
        // The HTTPS credential facade is media-owned (`curlio`); the Feed only calls this function.
        fn demux_transport() -> plx_play_feed::DemuxTransport {
            plx_play_feed::DemuxTransport::new(
                plx_media::curlio::range_factory(), plx_media::curlio::CREDENTIAL_GATE)
        }
        fn spawn_unload(ticket: UnloadTicket) -> Option<std::thread::JoinHandle<()>> {
            let worker = $worker();
            plx_base::task::spawn("sf-unload", move || {
                let _worker = worker;
                unsafe { $raw().unload(&ticket) }
            })
        }
        NativeHooks {
            prepare: PrepareHooks {
                sink: fenced,
                on_exported_window_create: plx_media::player::engine::exported_window_created,
                load_refusal_latch: plx_telemetry::telemetry::classes::DemuxExitClass::LoadRefused.latch(),
                side_reader_job: plx_media::player::subside::JOB,
            },
            launch: LaunchHooks {
                demux_transport,
                load_thread,
                sink: fenced,
            },
            feed: FeedHooks { sink: fenced, mirror_playstate, mute_for_preroll, feed_clock_us: $clock },
            release: ReleaseHooks {
                raw_sink: $raw,
                plane: plx_platform::tv::plane::installed(),
                on_release_step: plx_media::player::engine::release_step,
                on_exported_window_destroy: plx_media::player::engine::exported_window_destroyed,
                spawn_unload,
            },
        }
    }};
}

/// The session one demux attempt is handed: the Feed's clock, adaptive actuator, observer and effect
/// sink over media's caption half and its two common-store hooks. The player asks for it at a start
/// ([`plx_media::player::adapter::StartHooks`]); it names the Feed, which media may not.
#[cfg(feature = "play-feed")]
fn demux_session(attempt: plx_playback_contract::AttemptId, native_epoch: u32)
    -> Box<dyn plx_playback_contract::demux::DemuxSession> {
    use plx_media::player::demux_session::{demux_exit_latch, replace_track_names, PlayerDemuxSubtitle};
    plx_play_feed::demux_clock::FeedDemuxSession::capture(
        attempt, native_epoch,
        plx_play_feed::demux_clock::DemuxMediaHooks { exit_latch: demux_exit_latch, replace_track_names },
        PlayerDemuxSubtitle::new)
}

/// What the player's start path calls of the Feed: the demux session of an attempt and the
/// `plxnative-playurl` dev trigger's reader (`None` in a build without `devtriggers`).
#[cfg(feature = "play-feed")]
const START_HOOKS: StartHooks = StartHooks {
    demux_session,
    play_url: plx_play_feed::playurl::playurl,
};

/// The television's (and the simulator's, whose port table holds the host sink) hooks: the
/// installed port's sink and the Feed's own monotonic clock.
#[cfg(feature = "play-feed")]
const TV_HOOKS: NativeHooks =
    native_hooks_over!(sink::installed, plx_play_feed::shared::vclock_us, no_unload_worker);

/// What the television's `sf-unload` worker holds while it lives: nothing.
#[cfg(feature = "play-feed")]
struct NoWorker;

#[cfg(feature = "play-feed")]
fn no_unload_worker() -> NoWorker { NoWorker }

/// The host tests' hooks: the host sink (a hostsim test binary installs no port) and a clock that
/// never moves, so the host seam's near-free `sf_feed` cannot run out a slice on a slow machine.
#[cfg(all(test, feature = "play-feed"))]
const TEST_HOOKS: NativeHooks = native_hooks_over!(test_raw_sink, test_clock_us, test_unload_worker);

/// The host drain counts each `sf-unload` worker from before it is spawned until it ends, so a
/// test's reset can wait it out (`ffi_host::UnloadWorker`); a build with no host sink has no drain.
#[cfg(all(test, feature = "play-feed"))]
fn test_unload_worker() -> impl Send + 'static {
    #[cfg(feature = "hostsim")]
    {
        plx_play_feed::ffi_host::UnloadWorker::enter()
    }
    #[cfg(not(feature = "hostsim"))]
    {
        NoWorker
    }
}

#[cfg(all(test, feature = "play-feed"))]
fn test_raw_sink() -> &'static dyn VideoSink {
    #[cfg(feature = "hostsim")]
    {
        &super::hostsim::HOST_SINK
    }
    #[cfg(not(feature = "hostsim"))]
    {
        sink::installed()
    }
}

#[cfg(all(test, feature = "play-feed"))]
fn test_clock_us() -> u64 { 0 }

/// Compose the buffer-feed backend into the player's physical set: the Feed backend over the
/// process's shared feed state and `hooks`, with the URI driver's seat beside it, and the sink the
/// set's hooks go through handed to the adapter beside it. The pipeline callback has no user-data
/// pointer, so its hooks are installed here too, once per process, before the first `Load`. The
/// adapter starts with the contract's URI-only set, so this runs before the URI driver is
/// installed and before the session is told the set. False when the adapter refused the set.
#[cfg(feature = "play-feed")]
pub(crate) fn install_feed(adapter: &mut PlayerAdapter, hooks: &'static NativeHooks) -> bool {
    let set = FeedSet::new(FeedBackend::new(FeedResources::new(&FEED_SHARED, hooks)));
    let display_sink = set.display_sink();
    if adapter.install_physical(Box::new(set), display_sink, START_HOOKS).is_err() {
        return false;
    }
    let _ = plx_play_feed::sf_event::install(
        &SfEventHooks { audio_rebuild_presented: plx_media::player::audio_rebuild_presented });
    true
}

/// An adapter with the Feed composed in, as the boot composes it: what an app test builds when
/// the player's own physical set is part of what it drives.
#[cfg(test)]
pub(crate) fn test_adapter() -> PlayerAdapter {
    #[cfg_attr(not(feature = "play-feed"), allow(unused_mut))]
    let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
    #[cfg(feature = "play-feed")]
    assert!(install_feed(&mut adapter, &TEST_HOOKS), "a fresh adapter takes the composed Feed set");
    adapter
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::task::MainThread;
    use plx_platform::tv::sink::{NoSink, VP_ACB};
    use std::os::raw::{c_char, c_long};
    use std::sync::Mutex;

    /// A display sink whose window mode and ACB create answer are scripted, and which records
    /// what `plane_create` was asked for. Every other verb is `NoSink`'s refusal.
    struct BootSink { mode: c_int, create: c_long, asked: Mutex<Vec<(String, c_int)>> }
    impl BootSink {
        fn new(mode: c_int, create: c_long) -> Self {
            Self { mode, create, asked: Mutex::new(Vec::new()) }
        }
    }
    impl DisplaySink for BootSink {
        fn window_mode(&self) -> c_int { self.mode }
        fn window_id(&self) -> &'static std::ffi::CStr { c"" }
        unsafe fn create_window(&self, mt: &MainThread) -> *const c_char { NoSink.create_window(mt) }
        unsafe fn place_window(&self, mt: &MainThread, a: c_int, b: c_int, c: c_int, d: c_int,
            e: c_int, f: c_int) -> c_int { NoSink.place_window(mt, a, b, c, d, e, f) }
        unsafe fn destroy_window(&self, mt: &MainThread) { NoSink.destroy_window(mt) }
        unsafe fn plane_create(&self, _: &MainThread, app_id: *const c_char, player_type: c_int)
            -> c_long {
            let id = std::ffi::CStr::from_ptr(app_id).to_string_lossy().into_owned();
            self.asked.lock().unwrap().push((id, player_type));
            self.create
        }
        unsafe fn plane_initialized(&self, mt: &MainThread) -> bool { NoSink.plane_initialized(mt) }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_dass_available(&self, mt: &MainThread) -> bool {
            NoSink.plane_probe_dass_available(mt)
        }
        unsafe fn plane_media_id(&self, mt: &MainThread, id: *const c_char) -> c_int {
            NoSink.plane_media_id(mt, id)
        }
        unsafe fn plane_loaded(&self, mt: &MainThread) -> c_int { NoSink.plane_loaded(mt) }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_connect_dass(&self, mt: &MainThread) -> c_int {
            NoSink.plane_probe_connect_dass(mt)
        }
        unsafe fn plane_finish(&self, mt: &MainThread) -> sink::PlaneFinish { NoSink.plane_finish(mt) }
        unsafe fn plane_bind(&self, mt: &MainThread, id: *const c_char) { NoSink.plane_bind(mt, id) }
        unsafe fn plane_send_video_data(&self, mt: &MainThread, info: *const c_char) -> c_int {
            NoSink.plane_send_video_data(mt, info)
        }
        unsafe fn plane_send_atmos(&self, mt: &MainThread, id: *const c_char) -> c_int {
            NoSink.plane_send_atmos(mt, id)
        }
        unsafe fn plane_start(&self, mt: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long) {
            NoSink.plane_start(mt, x, y, w, h)
        }
        unsafe fn plane_unload(&self, mt: &MainThread) -> sink::PlaneUnload { NoSink.plane_unload(mt) }
        fn plane_completions(&self, mt: &MainThread) -> Vec<sink::PlaneCompletion> {
            NoSink.plane_completions(mt)
        }
        unsafe fn plane_pause(&self, mt: &MainThread) { NoSink.plane_pause(mt) }
        unsafe fn plane_resume(&self, mt: &MainThread) { NoSink.plane_resume(mt) }
    }

    /// The host drain (`ffi_host::reset_native_lifecycle_for_test`) waits out every `sf-unload` worker
    /// so none delivers UNLOADCOMPLETED into the next test's session; it can only wait for the
    /// workers it counts, and a worker is counted by the hooks' `spawn_unload` entering
    /// `UnloadWorker` before it spawns. The application's test hooks are the table the app's player
    /// tests run their Unload over, so their worker has to be one of those.
    #[cfg(all(feature = "hostsim", feature = "play-feed"))]
    #[test]
    fn the_test_hooks_unload_worker_is_counted_by_the_drain_wait() {
        use plx_play_feed::ffi_host;
        let _serial = plx_base::testlock::serial();
        let mt = unsafe { MainThread::assume() };
        ffi_host::reset_native_lifecycle_for_test();
        ffi_host::hold_unload_for_test();
        let worker = (TEST_HOOKS.release.spawn_unload)(sink::UnloadTicket::issue(&mt));
        let counted = ffi_host::unload_workers_for_test();
        ffi_host::release_unload_for_test();
        if let Some(worker) = worker {
            worker.join().unwrap();
        }
        ffi_host::reset_native_lifecycle_for_test();
        assert_eq!(counted, 1, "the held Unload's worker was not counted: the drain wait cannot wait it out");
        assert_eq!(ffi_host::unload_workers_for_test(), 0, "the worker ended with the release");
    }

    /// Run the boot over a scripted sink on a private owner, then offer the owner a DIFFERENT boot:
    /// the owner takes its boot once, so a facts value that survives the offer was the first.
    fn booted(sink: &BootSink) -> PlaneOwner {
        let mt = unsafe { MainThread::assume() };
        let owner = PlaneOwner::new();
        acb_init_with(&mt, sink, &owner);
        owner.boot(PlaneFacts { handle: true, player_type: 99 });
        owner
    }

    #[test]
    fn an_exported_window_device_boots_absent_facts_and_never_creates_a_handle() {
        let sink = BootSink::new(VP_EXPORTED, 1);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts::ABSENT, "the boot ran, so the later offer was ignored");
        assert!(sink.asked.lock().unwrap().is_empty(), "webOS 5 creates its window per session");
    }

    #[test]
    fn a_device_with_no_plane_at_all_boots_absent_facts() {
        let sink = BootSink::new(VP_NONE, 1);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts::ABSENT);
        assert!(sink.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn an_acb_device_creates_the_handle_for_this_install_and_records_it() {
        let sink = BootSink::new(VP_ACB, 7);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE });
        let asked = sink.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0], (plx_base::paths::app_id().to_owned(), DEFAULT_PLAYER_TYPE));
    }

    #[test]
    fn an_acb_device_whose_create_fails_boots_without_a_handle() {
        let sink = BootSink::new(VP_ACB, 0);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts { handle: false, player_type: DEFAULT_PLAYER_TYPE });
        assert_eq!(sink.asked.lock().unwrap().len(), 1);
    }
}
