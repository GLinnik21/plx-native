//! The application's playback-backend composition at boot: the two decisions that belong to the
//! composition root and to no backend. Media names neither backend's presence in policy, and the
//! plane's boot is not a Feed step.
//!
//!   - [`acb_init`] boots the video plane owner (`plx_platform::tv::plane`) once per process, for
//!     EVERY backend set. It used to live in the Feed engine, so a URI-only build never created the
//!     ACB handle, and a webOS 4.x television then stalled URI HLS for want of a bound plane. On an
//!     ACB display it creates one handle per backend the build composes: Feed's (player type 10) and
//!     the URI engine's (player type 0, which its product binding is gated on), so a normal boot
//!     composes the URI driver `Acb` with no `ptype` trigger.
//!   - [`compose_backends`] installs the product URI driver and then tells the route session which
//!     backends the installed drivers form, so policy offers what is physically there instead of
//!     what Cargo features happened to be on.
//!   - [`install_uri_product`] builds the product URI driver (`plx_play_uri::backend::UriBackend`)
//!     from the installed display and URI bus and hands it to the player adapter, which holds it
//!     only as a `UriDriver`. The player crate names neither the driver nor its settings.
use plx_base::eventlog::log;
use plx_base::task::MainThread;
use plx_media::player::adapter::PlayerAdapter;
#[cfg(feature = "play-feed")]
use plx_media::player::adapter::StartHooks;
use plx_media::route::PlaybackSession;
use plx_platform::tv::plane::{self, PlaneFacts, PlaneOwner};
#[cfg(feature = "play-feed")]
use plx_platform::tv::plane::DEFAULT_PLAYER_TYPE;
#[cfg(any(feature = "play-feed", feature = "play-uri"))]
use plx_platform::tv::plane::{HandleSeat, PlaneHandle};
#[cfg(feature = "play-uri")]
use plx_platform::tv::plane::URI_PLAYER_TYPE;
use plx_platform::tv::sink::{self, DisplaySink, VP_EXPORTED, VP_NONE};
#[cfg(any(feature = "play-feed", feature = "play-uri"))]
use std::os::raw::c_int;
#[cfg(feature = "play-feed")]
use plx_play_feed::{control::FeedBackend, feed_resources::{FeedResources, LoadReceipt},
    native_executor::{FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks},
    pcm_lead::PcmLead, physical::FeedSet, sf_event::SfEventHooks, shared::{Stage, FEED_SHARED},
    sink_fence::SinkFence, SendPtr};
#[cfg(feature = "play-feed")]
use plx_platform::tv::sink::{RefusingSink, UnloadTicket, VideoSink};
#[cfg(feature = "play-uri")]
use plx_media::route::uri_preflight::{OriginalQualifier, ProductEvidence, ProductQualifier, ResolvePort};
#[cfg(feature = "play-uri")]
use plx_play_uri::backend::{Binding as UriBinding, Config as UriConfig, MonotonicClock, UriBackend};
#[cfg(feature = "play-uri")]
use plx_playback_contract::Capabilities;

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
    match display.window_mode() {
        VP_EXPORTED => {
            log("vplane: exported window (webOS 5+) — created per session");
            // Nothing to bind later; the ACB stages are skipped by the plane having no handle.
            owner.boot(PlaneFacts::ABSENT);
        }
        VP_NONE => {
            log(
                "vplane: this device has NEITHER libAcbAPI nor SDL's exported window — audio \
                 will play and the picture will not appear. Please report your webOS version.",
            );
            owner.boot(PlaneFacts::ABSENT);
        }
        _ => acb_init_acb(mt, display, owner),
    }
}

/// The player type Feed's handle is initialized with: [`DEFAULT_PLAYER_TYPE`], or what the
/// `/tmp/plxnative-ptype` developer trigger says (the URI probe's controlled arms set it).
#[cfg(feature = "play-feed")]
fn feed_player_type() -> c_int {
    let mut pt = DEFAULT_PLAYER_TYPE;
    if let Some(s) = plx_base::devtrig::read("ptype") {
        if let Ok(p) = s.parse::<c_int>() {
            pt = p;
        }
    }
    log(&format!("ptype={pt}"));
    pt
}

/// The webOS 4.x half of [`acb_init_with`]: one handle per backend this build composes, each
/// created and initialized ONCE and never destroyed (a destroy races `LSClient::stop`). Feed's is
/// player type 10 (what a normal boot has always initialized) and the URI engine's is player type 0,
/// the one its product binding is gated on; two cores in one process are independent
/// (`tv::plane`'s module doc). Feed's is created first and with exactly the calls it always made, so
/// its handle, its facts and its log lines are what they were. A build with no Feed boots Feed's
/// facts absent; a build with no URI engine creates no second handle.
#[cfg_attr(not(any(feature = "play-feed", feature = "play-uri")), allow(unused_variables))]
fn acb_init_acb(mt: &MainThread, display: &dyn DisplaySink, owner: &PlaneOwner) {
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
    #[cfg(feature = "play-feed")]
    {
        let pt = feed_player_type();
        boot_handle(mt, display, owner, app_ptr, PlaneHandle::Feed, pt);
    }
    #[cfg(not(feature = "play-feed"))]
    owner.boot(PlaneFacts::ABSENT);
    #[cfg(feature = "play-uri")]
    boot_handle(mt, display, owner, app_ptr, PlaneHandle::Uri, URI_PLAYER_TYPE);
}

/// Create and initialize one boot handle of player type `pt` and tell the owner what came of it:
/// the facts, the id the sink knows it by and the library's confirmation of the initialize (read
/// now, while this is the sink's current handle). One log line says all of it.
#[cfg(any(feature = "play-feed", feature = "play-uri"))]
fn boot_handle(mt: &MainThread, display: &dyn DisplaySink, owner: &PlaneOwner, app_ptr: *const std::os::raw::c_char,
    handle: PlaneHandle, pt: c_int) {
    let acb = unsafe { display.plane_create(mt, app_ptr, pt) };
    let initialized = acb != 0 && unsafe { display.plane_initialized(mt) };
    if handle == PlaneHandle::Feed {
        log(&format!("acb create={acb}"));
    }
    log(&format!("acb handle={handle} ptype={pt} create={acb} initialized={initialized}"));
    owner.boot_handle(handle, PlaneFacts { handle: acb != 0, player_type: pt }, HandleSeat { id: acb, initialized });
}

/// Compose the backends this boot supplies. A normal boot installs the product URI driver first
/// (a refused install leaves URI out of the set); a controlled run (replay, scripted Home) touches
/// no native port and so installs none. Then the session is told the set the installed drivers
/// form.
pub(crate) fn compose_backends(adapter: &mut PlayerAdapter, session: &mut PlaybackSession,
    controlled: bool) {
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
    #[cfg(feature = "play-uri")]
    if !controlled && !install_uri_product(adapter, session) {
        log("uri: product driver installation refused");
    }
    #[cfg(not(feature = "play-uri"))]
    let _ = controlled;
    // The URI-mode probe (issue #1064) rides every boot, controlled or not: it reads its trigger
    // file only when a play starts.
    #[cfg(all(feature = "devtriggers", feature = "play-uri"))]
    crate::dev::uriprobe::install(adapter);
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
/// process's shared feed state and `hooks`, and the sink the set's hooks go through handed to the
/// adapter beside it. The pipeline callback has no user-data pointer, so its hooks are installed
/// here too, once per process, before the first `Load`. This runs before the session is told the
/// set. False when the adapter refused the set.
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

/// Explicit product URI settings captured with the resolve port's certification evidence.
/// Platform transport and display are supplied at the IO composition boundary.
#[cfg(feature = "play-uri")]
pub(crate) struct UriDriverSettings {
    pub clock: Box<dyn MonotonicClock>,
    pub app_id: String,
    pub binding: UriBinding,
    pub capabilities: Capabilities,
    pub startup_timeout_ms: u64,
    pub release_allowance_ms: u64,
    pub source_ttl_ms: u64,
}

/// The URI driver's clock: the player's IO-boundary monotonic clock behind the driver's trait. The
/// wall clock itself is read in `plx_media` (the `wall` gate keeps raw clock reads out of `app/`).
#[cfg(feature = "play-uri")]
struct ProductUriClock(plx_media::player::adapter::MonotonicMs);

#[cfg(feature = "play-uri")]
impl MonotonicClock for ProductUriClock {
    fn now_ms(&self) -> u64 { self.0.now_ms() }
}

/// The display path the product URI driver is composed with, and the boot log line that names it
/// and the gate's inputs (so the television shows it without a trigger). It reads the URI engine's
/// own handle (player type 0), booted beside Feed's by `acb_init`, and the library's confirmation of
/// its initialize, read there. `firmware_major` (`device::info().major`, read by the caller at the
/// IO side, 0 when unknown) and `strata` (production's `EXPORTED_STRATA`) are parameters, never a
/// global read here, so a test passes its own. The line ends `major=<n> evidence=table|none`:
/// `table` when `strata` admitted that major's exported window, `none` otherwise. `none` says only
/// that the table admitted nothing, not that the play is Standard: an ACB set reads `none` and
/// still plays URI through `Acb` for a viewer who opted in, while a webOS 5+ major with no entry
/// reads `none` and plays Standard. The debug override logs its own `evidence=override` line after
/// this one.
#[cfg(feature = "play-uri")]
fn uri_product_binding(display: &dyn DisplaySink, owner: &PlaneOwner, firmware_major: u32,
    strata: &[u32]) -> UriBinding {
    let facts = owner.facts_of(PlaneHandle::Uri);
    let initialized = owner.initialized(PlaneHandle::Uri);
    let binding = plx_play_uri::backend::product_binding(display.window_mode(), facts, initialized,
        firmware_major, strata);
    // Only the table composes the exported window here, so an exported binding IS table evidence.
    let evidence = if matches!(binding, UriBinding::Exported { .. }) { "table" } else { "none" };
    log(&format!("uri: product binding={binding:?} mode={} handle={} ptype={} initialized={initialized} \
        major={firmware_major} evidence={evidence}",
        display.window_mode(), facts.handle, facts.player_type));
    binding
}

/// Capture the installed display and URI bus at the playback IO boundary. Current product
/// evidence remains unqualified for everyone but a viewer who opted in to the URI engine in
/// Settings; returning a port never certifies a source by itself. (A debug
/// build's URI dev route, armed by a trigger read once here, is the other thing that changes that:
/// `dev::uri_product`. A debug build's exported-window override, `dev::uri_exported`, is read here
/// too but certifies nothing: it only composes the display path.)
#[cfg(feature = "play-uri")]
pub(crate) fn install_uri_product(adapter: &mut PlayerAdapter, session: &mut PlaybackSession) -> bool {
    let display = sink::display_installed();
    let binding = uri_product_binding(display, plane::installed(), plx_platform::tv::device::info().major,
        plx_play_uri::backend::EXPORTED_STRATA);
    // The exported-window override (`dev::uri_exported`): a debug build armed with its trigger, on a
    // display in `VP_EXPORTED` mode, composes the exported window; otherwise the binding is returned
    // as built.
    #[cfg(feature = "devtriggers")]
    let binding = crate::dev::uri_exported::compose(binding, display.window_mode(),
        crate::dev::uri_exported::armed());
    let evidence = ProductEvidence::unqualified_on(display.window_mode(), binding);
    let qualifier: std::sync::Arc<dyn OriginalQualifier> = std::sync::Arc::new(ProductQualifier);
    // The URI dev route (`dev::uri_product`): a debug build armed with its trigger, on an ACB-bound
    // display, swaps in evidence and a qualifier for the one Part it names; otherwise this pair is
    // returned as built.
    #[cfg(feature = "devtriggers")]
    let (evidence, qualifier) = crate::dev::uri_product::compose(evidence, qualifier,
        crate::dev::uri_product::trigger());
    let port = ResolvePort::new(evidence, qualifier);
    install_uri_product_with_ports(adapter, session, port, plx_platform::tv::uri::installed(), display)
}

/// One boot composition for both real and injected ports. The driver is configured with the binding
/// and the capabilities an opted-in viewer certifies (`ProductEvidence::opted_in`), and the resolver
/// receives the exact evidence, only after physical installation succeeds.
#[cfg(feature = "play-uri")]
pub(crate) fn install_uri_product_with_ports(adapter: &mut PlayerAdapter,
    session: &mut PlaybackSession, port: ResolvePort,
    transport: &'static dyn plx_platform::tv::uri::UriMedia,
    display: &'static dyn DisplaySink) -> bool {
    let evidence = port.evidence();
    let settings = UriDriverSettings {
        clock: Box::new(ProductUriClock(plx_media::player::adapter::MonotonicMs::start())),
        app_id: plx_base::paths::app_id().to_owned(),
        binding: evidence.binding,
        // What the driver can carry is what an opted-in viewer certifies; it is offered a source
        // only by a qualification, which production supplies for that viewer alone.
        capabilities: evidence.opted_in().certified,
        startup_timeout_ms: 15_000,
        release_allowance_ms: 10_000,
        source_ttl_ms: 30_000,
    };
    if !install_uri_backend_with_ports(adapter, settings, transport, display) {
        return false;
    }
    session.install_uri_preflight_port(port);
    true
}

/// Build the URI driver from `settings` over the captured platform ports and install it into
/// `adapter`; `false` when the settings are not a URI driver's (it cannot take a packet feed or a
/// track-disable), or the adapter will not take one now (a driver is already installed, or a
/// native attempt still owns the player). Installation supplies no source-specific
/// qualification and therefore does not change the route's default Feed choice. A host rig
/// can pass the same interfaces without a process-wide port.
#[cfg(feature = "play-uri")]
pub(crate) fn install_uri_backend_with_ports(adapter: &mut PlayerAdapter,
    settings: UriDriverSettings, transport: &'static dyn plx_platform::tv::uri::UriMedia,
    display: &'static dyn DisplaySink) -> bool {
    // `UriBackend::new` asserts these; a refusal must come first.
    if settings.capabilities.packet_feed || settings.capabilities.audio_off
        || settings.capabilities.subtitle_off {
        return false;
    }
    let driver = UriBackend::new(UriConfig {
        transport,
        display,
        plane: plane::installed(),
        clock: settings.clock,
        app_id: settings.app_id,
        binding: settings.binding,
        capabilities: settings.capabilities,
        startup_timeout_ms: settings.startup_timeout_ms,
        release_allowance_ms: settings.release_allowance_ms,
        source_ttl_ms: settings.source_ttl_ms,
    });
    adapter.install_uri_driver(Box::new(driver)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::task::MainThread;
    use plx_platform::tv::plane::DEFAULT_PLAYER_TYPE;
    use plx_platform::tv::sink::{NoSink, VP_ACB};
    use std::os::raw::{c_char, c_int, c_long};
    use std::sync::Mutex;

    /// A display sink whose window mode and ACB create answers are scripted (one per create, the
    /// last repeating), which confirms the initialize of what it created, and which records what
    /// `plane_create` was asked for. Every other verb is `NoSink`'s refusal.
    struct BootSink { mode: c_int, creates: Vec<c_long>, asked: Mutex<Vec<(String, c_int)>>,
        select: Mutex<Vec<c_long>> }
    impl BootSink {
        fn new(mode: c_int, create: c_long) -> Self { Self::answering(mode, vec![create]) }
        fn answering(mode: c_int, creates: Vec<c_long>) -> Self {
            Self { mode, creates, asked: Mutex::new(Vec::new()), select: Mutex::new(Vec::new()) }
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
            let mut asked = self.asked.lock().unwrap();
            asked.push((id, player_type));
            self.creates[(asked.len() - 1).min(self.creates.len() - 1)]
        }
        unsafe fn plane_select(&self, _: &MainThread, handle: c_long) -> bool {
            self.select.lock().unwrap().push(handle);
            true
        }
        unsafe fn plane_initialized(&self, _: &MainThread) -> bool {
            let asked = self.asked.lock().unwrap().len();
            asked != 0 && self.creates[(asked - 1).min(self.creates.len() - 1)] != 0
        }
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

    /// The player types an ACB boot of this build creates, in order: Feed's, then the URI engine's.
    fn created_player_types() -> Vec<c_int> {
        let mut types = Vec::new();
        #[cfg(feature = "play-feed")]
        types.push(plx_platform::tv::plane::DEFAULT_PLAYER_TYPE);
        #[cfg(feature = "play-uri")]
        types.push(plx_platform::tv::plane::URI_PLAYER_TYPE);
        types
    }

    #[test]
    fn an_acb_device_creates_one_handle_per_backend_for_this_install_and_records_them() {
        let sink = BootSink::answering(VP_ACB, vec![7, 8]);
        let owner = booted(&sink);
        let asked = sink.asked.lock().unwrap();
        let types = created_player_types();
        assert_eq!(*asked, types.iter().map(|t| (plx_base::paths::app_id().to_owned(), *t)).collect::<Vec<_>>());
        // Feed's handle, its facts and its player type are what a one-handle boot always made.
        #[cfg(feature = "play-feed")]
        {
            assert_eq!(owner.facts(), PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE });
            assert!(owner.initialized(PlaneHandle::Feed));
        }
        #[cfg(not(feature = "play-feed"))]
        assert_eq!(owner.facts(), PlaneFacts::ABSENT, "a build with no Feed has no Feed handle");
        #[cfg(feature = "play-uri")]
        {
            assert_eq!(owner.facts_of(PlaneHandle::Uri), PlaneFacts { handle: true, player_type: 0 });
            assert!(owner.initialized(PlaneHandle::Uri));
        }
        #[cfg(not(feature = "play-uri"))]
        assert_eq!(owner.facts_of(PlaneHandle::Uri), PlaneFacts::ABSENT, "a build with no URI engine creates no second handle");
    }

    #[test]
    fn an_acb_device_whose_create_fails_boots_without_a_handle() {
        let sink = BootSink::new(VP_ACB, 0);
        let owner = booted(&sink);
        assert_eq!(owner.facts(), PlaneFacts { handle: false, player_type: DEFAULT_PLAYER_TYPE });
        assert_eq!(sink.asked.lock().unwrap().len(), created_player_types().len());
        assert!(!owner.initialized(PlaneHandle::Feed) && !owner.initialized(PlaneHandle::Uri));
        assert_eq!(owner.phase(), plane::Phase::Unavailable);
    }

    /// The bug: a normal boot (no `ptype` trigger) initialized ONE handle as player type 10, so the
    /// URI product binding (which needs player type 0) was composed `Unbound` and the Settings
    /// opt-in could never play URI.
    #[cfg(feature = "play-uri")]
    #[test]
    fn a_normal_boot_composes_the_uri_product_binding_acb_while_feed_keeps_player_type_10() {
        let sink = BootSink::answering(VP_ACB, vec![7, 8]);
        let owner = booted(&sink);
        assert_eq!(uri_product_binding(&sink, &owner, 4, &[]), UriBinding::Acb);
        #[cfg(feature = "play-feed")]
        assert_eq!(owner.facts().player_type, DEFAULT_PLAYER_TYPE, "Feed's handle is still type 10");
    }

    #[cfg(feature = "play-uri")]
    #[test]
    fn the_uri_product_binding_is_unbound_unless_the_uri_handle_was_created_and_confirmed() {
        // A second create that fails leaves Feed's handle alone and the URI engine unbound.
        let sink = BootSink::answering(VP_ACB, vec![7, 0]);
        let owner = booted(&sink);
        #[cfg(feature = "play-feed")]
        assert_eq!(owner.facts(), PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE });
        #[cfg(feature = "play-feed")]
        assert_eq!(uri_product_binding(&sink, &owner, 4, &[]), UriBinding::Unbound);
        // No window path: not even a table that lists the major binds it.
        let sink = BootSink::answering(VP_NONE, vec![7, 8]);
        assert_eq!(uri_product_binding(&sink, &booted(&sink), 11, &[11]), UriBinding::Unbound);
    }

    /// The exported window follows the firmware major through the composition: production's table
    /// binds major 5 (webOS 5.6.2, `docs/uri-mode.md`) and no other (a webOS 5+ release without an
    /// entry plays Standard), and a table that lists a major binds it, on a display in the
    /// exported-window mode only. The URI engine's own handle is not asked for: a 5+ set has none.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_exported_window_is_composed_only_for_a_major_the_table_lists() {
        use plx_play_uri::backend::{EXPORTED_CANVAS, EXPORTED_STRATA};
        let exported = UriBinding::Exported { canvas: EXPORTED_CANVAS };
        let sink = BootSink::answering(VP_EXPORTED, vec![7, 8]);
        let owner = booted(&sink);
        // Production's table admits major 5 and nothing else: every other release, known or not,
        // composes Unbound on this display. An entry lands with its Results row and the change to
        // this test.
        for major in 0..=30 {
            let want = if major == 5 { exported } else { UriBinding::Unbound };
            assert_eq!(uri_product_binding(&sink, &owner, major, EXPORTED_STRATA), want, "major {major}");
        }
        // The same display on a set whose mode is not the exported window: major 5 binds nothing there.
        for mode in [VP_NONE, plx_platform::tv::sink::VP_ACB] {
            let sink = BootSink::answering(mode, vec![7, 8]);
            assert_ne!(uri_product_binding(&sink, &booted(&sink), 5, EXPORTED_STRATA), exported, "mode {mode}");
        }
        assert_eq!(uri_product_binding(&sink, &owner, 11, &[11]), exported);
        assert_eq!(uri_product_binding(&sink, &owner, 10, &[11]), UriBinding::Unbound, "an unlisted major");
        assert_eq!(uri_product_binding(&sink, &owner, 0, &[11]), UriBinding::Unbound, "unknown firmware");
        // The same table on a display that is not in the exported-window mode.
        for mode in [VP_NONE, plx_platform::tv::sink::VP_ACB] {
            let sink = BootSink::answering(mode, vec![7, 8]);
            assert_ne!(uri_product_binding(&sink, &booted(&sink), 11, &[11]), exported, "mode {mode}");
        }
    }

    /// The boot line names the major and where the binding's evidence came from, so a television's
    /// log shows both without a trigger: `evidence=table` only for a binding the table composed.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_product_binding_boot_line_names_the_major_and_its_evidence() {
        let line = |mode, major, strata: &[u32]| plx_base::eventlog::with_private_log(|| {
            let sink = BootSink::answering(mode, vec![7, 8]);
            uri_product_binding(&sink, &booted(&sink), major, strata);
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        let listed = line(VP_EXPORTED, 11, &[11]);
        assert!(listed.contains("uri: product binding=Exported { canvas: (1920, 1080) } mode=2 "), "{listed}");
        assert!(listed.contains(" major=11 evidence=table\n"), "{listed}");
        let unlisted = line(VP_EXPORTED, 11, &[]);
        assert!(unlisted.contains("uri: product binding=Unbound mode=2 "), "{unlisted}");
        assert!(unlisted.contains(" major=11 evidence=none\n"), "{unlisted}");
        // Production's own table: the webOS 5.6.2 television's boot line reads `major=5 evidence=table`.
        let shipped = line(VP_EXPORTED, 5, plx_play_uri::backend::EXPORTED_STRATA);
        assert!(shipped.contains("uri: product binding=Exported { canvas: (1920, 1080) } mode=2 "), "{shipped}");
        assert!(shipped.contains(" major=5 evidence=table\n"), "{shipped}");
        let acb = line(plx_platform::tv::sink::VP_ACB, 4, &[]);
        assert!(acb.contains("uri: product binding=Acb ") && acb.contains(" major=4 evidence=none\n"), "{acb}");
    }

    #[test]
    #[cfg(feature = "play-uri")]
    fn the_session_set_gains_uri_only_when_the_product_driver_is_installed() {
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        let mut session = PlaybackSession::IDLE;
        // A controlled run installs no driver: the set is what the drivers form, not what
        // Cargo features were on.
        compose_backends(&mut adapter, &mut session, true);
        assert!(!session.backends().contains(plx_playback_contract::BackendKind::Uri));
        compose_backends(&mut adapter, &mut session, false);
        assert!(session.backends().contains(plx_playback_contract::BackendKind::Uri),
            "an installed product driver is offered");
    }

    #[cfg(feature = "play-uri")]
    fn settings(packet_feed: bool) -> UriDriverSettings {
        struct Clock;
        impl MonotonicClock for Clock { fn now_ms(&self) -> u64 { 0 } }
        UriDriverSettings {
            clock: Box::new(Clock), app_id: "fixture".to_owned(), binding: UriBinding::Unbound,
            capabilities: Capabilities { progressive_uri: true, hls_uri: false, packet_feed,
                pause: true, seek: true, audio_track: true, subtitle_track: true,
                audio_off: false, subtitle_off: false },
            startup_timeout_ms: 100, release_allowance_ms: 100, source_ttl_ms: 100,
        }
    }

    #[test]
    #[cfg(feature = "play-uri")]
    fn a_driver_that_claims_packet_feed_is_refused_and_a_second_install_is_too() {
        use plx_playback_contract::BackendSet;
        let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
        let feed_only = BackendSet { uri: false, feed: adapter.composed_backends().feed };
        let ports = (&plx_platform::tv::uri::NoUriMedia, &sink::NoSink);
        // A driver that claims packet feed is refused, so the set does not gain Uri.
        assert!(!install_uri_backend_with_ports(&mut adapter, settings(true), ports.0, ports.1));
        assert_eq!(adapter.composed_backends(), feed_only);
        assert!(install_uri_backend_with_ports(&mut adapter, settings(false), ports.0, ports.1));
        assert_eq!(adapter.composed_backends(), BackendSet { uri: true, ..feed_only });
        assert!(!install_uri_backend_with_ports(&mut adapter, settings(false), ports.0, ports.1),
            "one URI driver per adapter");
    }
}
