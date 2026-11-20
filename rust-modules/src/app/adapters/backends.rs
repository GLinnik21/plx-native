//! The application's playback-backend composition at boot: the two decisions that belong to the
//! composition root and to no backend. Media names neither backend's presence in policy, and the
//! plane's boot is not a Feed step.
//!
//!   - [`acb_init`] boots the video plane owner (`plx_os::tv::plane`) once per process, for
//!     EVERY backend set. It used to live in the Feed engine, so a URI-only build never created the
//!     ACB handle, and a webOS 4.x television then stalled URI HLS for want of a bound plane. On an
//!     ACB display it creates Feed's handle (player type 10) and no other: the URI engine's handle
//!     (player type 0, which its product binding is gated on) is created when the first source is offered
//!     to it (`PlaneOwner::ensure_handle`, run by `UriDriver::ready_to_start`), so a viewer who
//!     never plays through it never has a second ACB core (#1394), while a normal boot still
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
use plx_os::tv::plane::{self, PlaneFacts, PlaneOwner};
#[cfg(feature = "play-feed")]
use plx_os::tv::plane::DEFAULT_PLAYER_TYPE;
#[cfg(feature = "play-feed")]
use plx_os::tv::plane::PlaneHandle;
#[cfg(feature = "play-uri")]
use plx_os::tv::plane::URI_PLAYER_TYPE;
use plx_os::tv::sink::{self, DisplaySink, VP_EXPORTED, VP_NONE};
#[cfg(feature = "play-feed")]
use std::os::raw::c_int;
#[cfg(feature = "play-feed")]
use plx_play_feed::{control::FeedBackend, feed_resources::{FeedResources, LoadReceipt},
    native_executor::{FeedHooks, LaunchHooks, NativeHooks, PrepareHooks, ReleaseHooks},
    pcm_lead::PcmLead, physical::FeedSet, sf_event::SfEventHooks, shared::{Stage, FEED_SHARED},
    sink_fence::SinkFence, SendPtr};
#[cfg(feature = "play-feed")]
use plx_os::tv::sink::{RefusingSink, UnloadTicket, VideoSink};
#[cfg(feature = "play-uri")]
use plx_media::route::uri_preflight::{KnownSource, OriginalQualifier, OriginalRequest,
    ProductEvidence, ResolvePort, UriDecline, Verdict, SOURCE_VERDICT_TTL};
#[cfg(feature = "play-uri")]
use plx_play_uri::backend::{Binding as UriBinding, Config as UriConfig, MonotonicClock, UriBackend};
#[cfg(feature = "play-uri")]
use plx_playback_contract::uri::{DisplayEvidence, DisplayReadiness, UriDriver};
#[cfg(feature = "play-uri")]
use plx_playback_contract::Capabilities;
#[cfg(feature = "play-uri")]
use std::sync::Arc;

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

/// The webOS 4.x half of [`acb_init_with`]: Feed's handle, created and initialized ONCE and never
/// destroyed (a destroy races `LSClient::stop`), player type 10 (what a normal boot has always
/// initialized), with exactly the calls and log lines it always had. The URI engine's handle (player
/// type 0, the one its product binding is gated on) is NOT a boot's: the offer of its first source
/// creates it, once, on the main thread (`PlaneOwner::ensure_handle`, reached from the URI
/// driver's `ready_to_start`), so a viewer on Standard never has a second core; two cores in one process are
/// independent (`tv::plane`'s module doc). A build with no Feed boots Feed's facts absent.
#[cfg_attr(not(feature = "play-feed"), allow(unused_variables))]
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
    #[cfg(feature = "play-feed")]
    {
        let pt = feed_player_type();
        // The owner creates it through the sink, records the facts, the id the sink knows it by and
        // the library's confirmation of the initialize (read while it is the sink's current handle),
        // and logs `acb create=..` and `acb handle=feed ptype=.. create=.. initialized=..`.
        owner.ensure_handle(display, mt, PlaneHandle::Feed, app_c.as_deref(), pt);
    }
    #[cfg(not(feature = "play-feed"))]
    owner.boot(PlaneFacts::ABSENT);
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
                plane: plx_os::tv::plane::installed(),
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

/// The television's (and the simulator's, whose platform table holds the host sink) hooks: the
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
/// and the gate's inputs (so the television shows it without a trigger). On an ACB display the URI
/// engine's own handle (player type 0) does not exist yet: the offer of its first source creates it, so the
/// line says `handle=deferred` and the binding is `Acb`, and each Start's gate decides that play.
/// `firmware_major` (`device::info().major`, read by the caller at the
/// IO side, 0 when unknown) and `strata` (production's `EXPORTED_STRATA`) are parameters, never a
/// global read here, so a test passes its own. The line ends `major=<n> evidence=table|none`:
/// `table` when `strata` admitted that major's exported window, `none` otherwise. `none` says only
/// that the table admitted nothing, not that the play is Standard: an ACB set reads `none` and
/// still plays URI through `Acb` for a viewer who opted in, while a webOS 5+ major with no entry
/// reads `none` and plays Standard. The debug override logs its own `evidence=override` line after
/// this one.
#[cfg(feature = "play-uri")]
fn uri_product_binding(display: &dyn DisplaySink, firmware_major: u32, strata: &[u32]) -> UriBinding {
    let binding = plx_play_uri::backend::product_binding(display.window_mode(), firmware_major, strata);
    let handle = if matches!(binding, UriBinding::Acb) { "deferred" } else { "none" };
    // Only the table composes the exported window here, so an exported binding IS table evidence.
    let evidence = if matches!(binding, UriBinding::Exported { .. }) { "table" } else { "none" };
    log(&format!("uri: product binding={binding:?} mode={} handle={handle} ptype={URI_PLAYER_TYPE} \
        major={firmware_major} evidence={evidence}", display.window_mode()));
    binding
}

/// The product's qualifier of an original file for its URI driver (`plx_play_uri`). Native product
/// source evidence is unqualified for everyone but a viewer who turned on the URI engine in
/// Settings ([`OriginalRequest::uri_opt_in`]): that choice is their own qualification of the URL
/// carrier, so the answer is Unknown without it. Even then it covers only an original Standard
/// would direct-play ([`OriginalRequest::standard`]: the plan's own video, Dolby Vision, audio and
/// container verdicts, which this qualifier reads and never re-derives; not the server's own
/// decision, which the offer is made before): every other file plays exactly as it does in
/// Standard, where the server converts what the set cannot show or decode and remuxes a container
/// Standard does not stream. The same port is run by every real resolve and can consume a future
/// certified provider without changing route policy.
///
/// The one rule of its own is its display's: the exported window's carrier assumes Plex order, so a
/// Part with several audio tracks is left to Standard there. It lives here, beside the display path
/// the composition chose ([`UriBinding`]), and not in the route, which knows a display only as the
/// evidence the driver reports ([`DisplayEvidence`]).
#[cfg(feature = "play-uri")]
pub(crate) struct ProductQualifier { binding: UriBinding }

#[cfg(feature = "play-uri")]
impl ProductQualifier {
    /// The qualifier for a driver composed with `binding`: the binding the driver is built with, so
    /// the rule and the display it guards cannot disagree.
    pub(crate) fn new(binding: UriBinding) -> Self { Self { binding } }
}

#[cfg(feature = "play-uri")]
impl OriginalQualifier for ProductQualifier {
    fn qualify(&self, _off: &plx_base::task::OffFrame, request: OriginalRequest) -> Verdict {
        if !request.uri_opt_in { return Verdict::Unknown(UriDecline::NotOptedIn); }
        if let Some(gap) = request.standard.first_gap() { return Verdict::Unknown(gap); }
        // The product carrier assumes Plex order. Explicitly certified providers may supply
        // their own track map, so this restriction belongs to this qualifier.
        if request.has_multiple_audio_tracks && matches!(self.binding, UriBinding::Exported { .. }) {
            return Verdict::Refused(UriDecline::ManyAudioTracksExported);
        }
        let url = request.source_url.clone();
        Verdict::Known(KnownSource::url_carrier(&request, url, SOURCE_VERDICT_TTL))
    }
}

/// What the composition decides about the URI product before a driver exists: the display path the
/// driver is bound to, what its executor can call and what is certified for everyone, and the
/// qualifier that answers for an original file. The display evidence is not here: it is what the
/// built driver reports ([`UriDriver::display`]), and the route's evidence is these joined to it
/// when the driver is installed ([`install_uri_product_with_ports`]).
#[cfg(feature = "play-uri")]
pub(crate) struct UriProduct {
    pub binding: UriBinding,
    pub callable: Capabilities,
    pub certified: Capabilities,
    pub qualifier: Arc<dyn OriginalQualifier>,
}

#[cfg(feature = "play-uri")]
impl UriProduct {
    /// Production: the executor's callable set and nothing certified (only a viewer's own opt-in
    /// certifies anything), answered by [`ProductQualifier`] over the same `binding` the driver gets.
    pub(crate) fn production(binding: UriBinding) -> Self {
        Self { binding, callable: ProductEvidence::CALLABLE, certified: ProductEvidence::UNCERTIFIED,
            qualifier: Arc::new(ProductQualifier::new(binding)) }
    }

    /// What the driver is configured with: the capabilities an opted-in viewer certifies. It is
    /// offered a source only by a qualification, which production supplies for that viewer alone.
    fn driver_capabilities(&self) -> Capabilities {
        ProductEvidence::opt_in_certifies(self.callable, self.certified)
    }
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
    let binding = uri_product_binding(display, plx_os::tv::device::info().major,
        plx_play_uri::backend::EXPORTED_STRATA);
    // The exported-window override (`dev::uri_exported`): a debug build armed with its trigger, on a
    // display in `VP_EXPORTED` mode, composes the exported window; otherwise the binding is returned
    // as built. The qualifier is built from the binding that results, so its display rule follows it.
    #[cfg(feature = "devtriggers")]
    let binding = crate::dev::uri_exported::compose(binding, display.window_mode(),
        crate::dev::uri_exported::armed());
    let product = UriProduct::production(binding);
    // The URI dev route (`dev::uri_product`): a debug build armed with its trigger, on an ACB-bound
    // display, swaps in certification and a qualifier for the one Part it names; otherwise the
    // product is returned as built.
    #[cfg(feature = "devtriggers")]
    let product = crate::dev::uri_product::compose(product, crate::dev::uri_product::trigger());
    install_uri_product_with_ports(adapter, session, product, plx_os::tv::uri::installed(), display)
}

/// One boot composition for both real and injected ports. The driver is configured with the binding
/// and the capabilities an opted-in viewer certifies ([`UriProduct::driver_capabilities`]); the
/// resolver receives the product's evidence joined to the display the installed driver reports, only
/// after physical installation succeeds. The boot log names that display, so a television's log
/// shows what the driver reported without a trigger.
#[cfg(feature = "play-uri")]
pub(crate) fn install_uri_product_with_ports(adapter: &mut PlayerAdapter,
    session: &mut PlaybackSession, product: UriProduct,
    transport: &'static dyn plx_os::tv::uri::UriMedia,
    display: &'static dyn DisplaySink) -> bool {
    let settings = UriDriverSettings {
        clock: Box::new(ProductUriClock(plx_media::player::adapter::MonotonicMs::start())),
        app_id: plx_base::paths::app_id().to_owned(),
        binding: product.binding,
        capabilities: product.driver_capabilities(),
        startup_timeout_ms: 15_000,
        release_allowance_ms: 10_000,
        source_ttl_ms: 30_000,
    };
    let Some(path) = install_uri_backend_with_ports(adapter, settings, transport, display) else {
        return false;
    };
    let state = match path.readiness {
        DisplayReadiness::Ready => "ready".to_owned(),
        DisplayReadiness::NotReady(why) => format!("unavailable ({})", why.log_name()),
    };
    log(&format!("uri: display path={} {state}", path.tag));
    let evidence = ProductEvidence { display: path, callable: product.callable,
        certified: product.certified };
    session.install_uri_preflight_port(ResolvePort::new(evidence, product.qualifier));
    true
}

/// Build the URI driver from `settings` over the captured platform ports and install it into
/// `adapter`; the display path the installed driver reports ([`UriDriver::display`]), or `None`
/// when the settings are not a URI driver's (it cannot take a packet feed or a track-disable), or
/// the adapter will not take one now (a driver is already installed, or a native attempt still owns
/// the player). Installation supplies no source-specific qualification and therefore does not
/// change the route's default Feed choice. A host rig can pass the same interfaces without a
/// process-wide port.
#[cfg(feature = "play-uri")]
pub(crate) fn install_uri_backend_with_ports(adapter: &mut PlayerAdapter,
    settings: UriDriverSettings, transport: &'static dyn plx_os::tv::uri::UriMedia,
    display: &'static dyn DisplaySink) -> Option<DisplayEvidence> {
    // `UriBackend::new` asserts these; a refusal must come first.
    if settings.capabilities.packet_feed || settings.capabilities.audio_off
        || settings.capabilities.subtitle_off {
        return None;
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
    let path = driver.display();
    adapter.install_uri_driver(Box::new(driver)).ok().map(|()| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_os::tv::plane::PlaneHandle;
    use plx_base::task::MainThread;
    use plx_os::tv::plane::DEFAULT_PLAYER_TYPE;
    use plx_os::tv::sink::{NoSink, VP_ACB};
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

    /// The player types an ACB boot of this build creates: Feed's alone. The URI engine's handle is
    /// created on demand, when its first source is offered (`PlaneOwner::ensure_handle`).
    fn created_player_types() -> Vec<c_int> {
        let mut types = Vec::new();
        #[cfg(feature = "play-feed")]
        types.push(plx_os::tv::plane::DEFAULT_PLAYER_TYPE);
        types
    }

    #[test]
    fn an_acb_boot_creates_only_feeds_handle_and_records_it() {
        let sink = BootSink::answering(VP_ACB, vec![7, 8]);
        let owner = booted(&sink);
        let asked = sink.asked.lock().unwrap();
        let types = created_player_types();
        assert_eq!(*asked, types.iter().map(|t| (plx_base::paths::app_id().to_owned(), *t)).collect::<Vec<_>>(),
            "a boot creates Feed's handle (player type 10) and no player type 0 handle");
        // Feed's handle, its facts and its player type are what a one-handle boot always made.
        #[cfg(feature = "play-feed")]
        {
            assert_eq!(owner.facts(), PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE });
            assert!(owner.initialized(PlaneHandle::Feed));
        }
        #[cfg(not(feature = "play-feed"))]
        assert_eq!(owner.facts(), PlaneFacts::ABSENT, "a build with no Feed has no Feed handle");
        assert_eq!(owner.facts_of(PlaneHandle::Uri), PlaneFacts::ABSENT, "the URI engine's handle is not a boot's");
        assert!(!owner.initialized(PlaneHandle::Uri));
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
    /// opt-in could never play URI. Now the handle is not a boot's, so the binding must not wait for it.
    #[cfg(feature = "play-uri")]
    #[test]
    fn a_normal_boot_composes_the_uri_product_binding_acb_before_the_uri_handle_exists() {
        let sink = BootSink::answering(VP_ACB, vec![7, 8]);
        let owner = booted(&sink);
        assert_eq!(uri_product_binding(&sink, 4, &[]), UriBinding::Acb);
        assert!(!owner.attempted(PlaneHandle::Uri), "composing the binding creates no handle");
        assert_eq!(sink.asked.lock().unwrap().len(), created_player_types().len());
        #[cfg(feature = "play-feed")]
        assert_eq!(owner.facts().player_type, DEFAULT_PLAYER_TYPE, "Feed's handle is still type 10");
    }

    /// A display with no window path binds nothing, not even for a table that lists the major.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_uri_product_binding_is_unbound_without_a_window_path() {
        let sink = BootSink::answering(VP_NONE, vec![7, 8]);
        assert_eq!(uri_product_binding(&sink, 11, &[11]), UriBinding::Unbound);
    }

    /// The exported window follows the firmware major through the composition: production's table
    /// binds majors 5 (webOS 5.6.2) and 10 (the LG C2 on webOS 25), each admitted on a tester's run,
    /// and majors 6, 7 and 9, each bracketed by those two with no tester run of its own, every one
    /// with its `docs/uri-mode.md` Results row, and no other (major 8 has no image read, so it and
    /// every webOS 5+ release without an entry plays Standard), and a table that lists a major binds
    /// it, on a display in the exported-window mode only. The URI engine's own handle is not asked
    /// for: a 5+ set has none.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_exported_window_is_composed_only_for_a_major_the_table_lists() {
        use plx_play_uri::backend::{EXPORTED_CANVAS, EXPORTED_STRATA};
        let exported = UriBinding::Exported { canvas: EXPORTED_CANVAS };
        let sink = BootSink::answering(VP_EXPORTED, vec![7, 8]);
        // Production's table admits majors 5, 6, 7, 9 and 10 and nothing else: every other release,
        // known or not (8, which has no image read, and 11 included), composes Unbound on this
        // display. An entry lands with its Results row and the change to this test.
        for major in 0..=30 {
            let want = if matches!(major, 5 | 6 | 7 | 9 | 10) { exported } else { UriBinding::Unbound };
            assert_eq!(uri_product_binding(&sink, major, EXPORTED_STRATA), want, "major {major}");
        }
        // The same display on a set whose mode is not the exported window: an admitted major binds nothing there.
        for major in [5, 6, 7, 9, 10] {
            for mode in [VP_NONE, plx_os::tv::sink::VP_ACB] {
                let sink = BootSink::answering(mode, vec![7, 8]);
                assert_ne!(uri_product_binding(&sink, major, EXPORTED_STRATA), exported, "major {major} mode {mode}");
            }
        }
        assert_eq!(uri_product_binding(&sink, 11, &[11]), exported);
        assert_eq!(uri_product_binding(&sink, 10, &[11]), UriBinding::Unbound, "an unlisted major");
        assert_eq!(uri_product_binding(&sink, 0, &[11]), UriBinding::Unbound, "unknown firmware");
        // The same table on a display that is not in the exported-window mode.
        for mode in [VP_NONE, plx_os::tv::sink::VP_ACB] {
            let sink = BootSink::answering(mode, vec![7, 8]);
            assert_ne!(uri_product_binding(&sink, 11, &[11]), exported, "mode {mode}");
        }
    }

    /// The boot line names the major and where the binding's evidence came from, so a television's
    /// log shows both without a trigger: `evidence=table` only for a binding the table composed.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_product_binding_boot_line_names_the_major_and_its_evidence() {
        let line = |mode, major, strata: &[u32]| plx_base::eventlog::with_private_log(|| {
            let sink = BootSink::answering(mode, vec![7, 8]);
            uri_product_binding(&sink, major, strata);
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        let listed = line(VP_EXPORTED, 11, &[11]);
        assert!(listed.contains("uri: product binding=Exported { canvas: (1920, 1080) } mode=2 "), "{listed}");
        assert!(listed.contains(" major=11 evidence=table\n"), "{listed}");
        let unlisted = line(VP_EXPORTED, 11, &[]);
        assert!(unlisted.contains("uri: product binding=Unbound mode=2 "), "{unlisted}");
        assert!(unlisted.contains(" major=11 evidence=none\n"), "{unlisted}");
        // Production's own table: a webOS 5.6.2 television's boot line reads `major=5 evidence=table`,
        // the LG C2 on webOS 25's `major=10 evidence=table`, and a bracketed major's (6, 7, 9) the
        // same `evidence=table`: the line does not say whether a tester ran it. Major 8 has no
        // image read and reads `major=8 evidence=none` on the same display.
        for major in [5, 6, 7, 9, 10] {
            let shipped = line(VP_EXPORTED, major, plx_play_uri::backend::EXPORTED_STRATA);
            assert!(shipped.contains("uri: product binding=Exported { canvas: (1920, 1080) } mode=2 "), "{shipped}");
            assert!(shipped.contains(&format!(" major={major} evidence=table\n")), "{shipped}");
        }
        let eight = line(VP_EXPORTED, 8, plx_play_uri::backend::EXPORTED_STRATA);
        assert!(eight.contains("uri: product binding=Unbound mode=2 "), "{eight}");
        assert!(eight.contains(" major=8 evidence=none\n"), "{eight}");
        let acb = line(plx_os::tv::sink::VP_ACB, 4, &[]);
        assert!(acb.contains("uri: product binding=Acb mode=1 handle=deferred ptype=0 "), "{acb}");
        assert!(acb.contains(" major=4 evidence=none\n"), "{acb}");
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
        let ports = (&plx_os::tv::uri::NoUriMedia, &sink::NoSink);
        // A driver that claims packet feed is refused, so the set does not gain Uri.
        assert!(install_uri_backend_with_ports(&mut adapter, settings(true), ports.0, ports.1).is_none());
        assert_eq!(adapter.composed_backends(), feed_only);
        // An installed driver hands back the display it reports: an unbound one has none.
        let installed = install_uri_backend_with_ports(&mut adapter, settings(false), ports.0, ports.1)
            .expect("a URI driver installs");
        assert!(!installed.is_ready());
        assert_eq!(adapter.composed_backends(), BackendSet { uri: true, ..feed_only });
        assert!(install_uri_backend_with_ports(&mut adapter, settings(false), ports.0, ports.1).is_none(),
            "one URI driver per adapter");
    }

    /// A request for an original of the Part the product qualifier is asked about: the viewer's own
    /// opt-in as given, and Standard direct-plays it.
    #[cfg(feature = "play-uri")]
    fn opt_in_request(uri_opt_in: bool) -> OriginalRequest {
        OriginalRequest { selection: plx_media::route::SourceSelection {
                server: plx_plex::plex::ServerId::from_raw(1), part_id: 4, audio_sid: 42,
                subtitle_sid: 0, audio_contract: plx_plex::plex::AudioEnhancements::NONE },
            server_origin: plx_plex::plex::Origin::http("192.0.2.5", 32499),
            source_url: "https://example.invalid/part.mkv".to_owned(), duration_ms: Some(60_000),
            video_codec: "h264".to_owned(), dolby_vision: plx_data::metadata::Dovi::NONE,
            dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode::Auto, uri_opt_in,
            standard: plx_media::route::uri_preflight::StandardDirectPlay { video: true,
                dolby_vision: true, audio: true, container: true },
            audio_codec: "aac".to_owned(), audio_ordinal: Some(1), has_multiple_audio_tracks: false,
            subtitle_source: None, subtitle_choice: plx_playback_contract::TrackChoice::Unspecified,
            source_kbps: Some(4000), raster: Some((1280, 720)),
            requested_rate_permille: plx_playback_contract::RATE_NORMAL, resume_ns: 0,
            quality_allows_original: true, link_allows_original: true, auto_allows_original: true,
            requires_subtitle_burn: false, requires_client_subtitle: false, preview: false }
    }

    /// The kind of an answer and its reason, for a table: a reason must never change a kind.
    #[cfg(feature = "play-uri")]
    fn answer(verdict: &Verdict) -> (&'static str, Option<UriDecline>) {
        match verdict {
            Verdict::Known(_) => ("known", None),
            Verdict::Unknown(why) => ("unknown", Some(*why)),
            Verdict::Refused(why) => ("refused", Some(*why)),
        }
    }

    /// The product's own answers, called directly: the opt-in first, then what Standard would do
    /// (the first verdict that says no), then the exported window's one rule.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_product_qualifier_names_why_it_did_not_certify() {
        let off = plx_base::task::OffFrame::for_test();
        let exported = UriBinding::Exported { canvas: (1920, 1080) };
        let ask = |binding, tweak: &dyn Fn(&mut OriginalRequest)| {
            let mut request = opt_in_request(true);
            tweak(&mut request);
            answer(&ProductQualifier::new(binding).qualify(&off, request))
        };
        assert_eq!(ask(exported, &|_| {}).0, "known");
        assert_eq!(ask(exported, &|r| r.uri_opt_in = false), ("unknown", Some(UriDecline::NotOptedIn)));
        assert_eq!(ask(exported, &|r| r.standard.video = false),
            ("unknown", Some(UriDecline::StandardVideo)));
        assert_eq!(ask(exported, &|r| r.standard.dolby_vision = false),
            ("unknown", Some(UriDecline::StandardDolbyVision)));
        assert_eq!(ask(exported, &|r| r.standard.audio = false),
            ("unknown", Some(UriDecline::StandardAudio)));
        assert_eq!(ask(exported, &|r| r.standard.container = false),
            ("unknown", Some(UriDecline::StandardContainer)));
        assert_eq!(ask(exported, &|r| r.has_multiple_audio_tracks = true),
            ("refused", Some(UriDecline::ManyAudioTracksExported)));
        for binding in [UriBinding::Acb, UriBinding::Unbound] {
            assert_eq!(ask(binding, &|r| r.has_multiple_audio_tracks = true).0, "known",
                "several tracks are only a problem on the exported window");
        }
        // Not opted in is named before what Standard would do, and the first gap before the rest.
        assert_eq!(ask(exported, &|r| { r.uri_opt_in = false; r.standard.video = false }),
            ("unknown", Some(UriDecline::NotOptedIn)));
        assert_eq!(ask(exported, &|r| { r.standard.audio = false; r.standard.container = false }),
            ("unknown", Some(UriDecline::StandardAudio)));
        // A gap in Standard's verdicts is named before the exported window's rule.
        assert_eq!(ask(exported, &|r| { r.has_multiple_audio_tracks = true; r.standard.container = false }),
            ("unknown", Some(UriDecline::StandardContainer)));
    }

    /// The exported window's carrier assumes Plex order, so a Part with several audio tracks stays on
    /// Standard there, through the port a real resolve runs; an ACB display takes it.
    #[cfg(feature = "play-uri")]
    #[test]
    fn exported_uri_leaves_multi_audio_originals_on_standard() {
        let off = plx_base::task::OffFrame::for_test();
        let exported = UriBinding::Exported { canvas: (1920, 1080) };
        let port = |binding, mode| ResolvePort::new(ProductEvidence {
            display: plx_play_uri::backend::display_evidence(binding, mode),
            callable: ProductEvidence::CALLABLE, certified: ProductEvidence::UNCERTIFIED },
            Arc::new(ProductQualifier::new(binding)));
        let exported_port = port(exported, VP_EXPORTED);
        let request = |ordinal, multiple| {
            let mut request = opt_in_request(true);
            request.audio_ordinal = Some(ordinal);
            request.has_multiple_audio_tracks = multiple;
            request
        };
        assert!(matches!(exported_port.qualify(&off, request(0, false)), Verdict::Known(_)));
        for ordinal in [0, 1] {
            assert!(matches!(exported_port.qualify(&off, request(ordinal, true)),
                Verdict::Refused(UriDecline::ManyAudioTracksExported)));
        }
        let acb = port(UriBinding::Acb, VP_ACB);
        assert!(matches!(acb.qualify(&off, request(1, true)), Verdict::Known(_)));
        // The opt-in stays the product's own: without it the port answers nothing, on either display.
        for port in [&exported_port, &acb] {
            assert!(matches!(port.qualify(&off, opt_in_request(false)),
                Verdict::Unknown(UriDecline::NotOptedIn)));
        }
    }

    /// The qualifier is built from the binding the composition ends with, so the exported window's
    /// rule applies to a display a tester's override bound (`dev::uri_exported`) exactly as it does to
    /// one the table bound: composing the override first and the qualifier second is the order.
    #[cfg(all(feature = "play-uri", feature = "devtriggers"))]
    #[test]
    fn the_qualifier_follows_the_binding_the_exported_override_composes() {
        let off = plx_base::task::OffFrame::for_test();
        let several = || { let mut request = opt_in_request(true); request.has_multiple_audio_tracks = true; request };
        for (armed, want) in [(true, "refused"), (false, "known")] {
            let binding = crate::dev::uri_exported::compose(UriBinding::Unbound, VP_EXPORTED, armed);
            let product = UriProduct::production(binding);
            assert_eq!(product.binding, binding);
            assert_eq!(answer(&product.qualifier.qualify(&off, several())).0, want, "armed: {armed}");
        }
    }

    /// Production certifies nothing for everyone and configures the driver with what an opted-in
    /// viewer certifies, which is exactly the evidence's own opt-in, and a driver (`UriBackend::new`
    /// asserts) never claims the packet feed or a track disable.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_production_product_certifies_nothing_and_configures_the_driver_with_the_opt_in_set() {
        let product = UriProduct::production(UriBinding::Acb);
        assert_eq!(product.certified, ProductEvidence::UNCERTIFIED);
        let driver = product.driver_capabilities();
        assert_eq!(driver, ProductEvidence::unqualified_on(DisplayEvidence::ready("any")).opted_in().certified);
        assert!(driver.progressive_uri && driver.pause && driver.seek && driver.audio_track);
        assert!(!driver.packet_feed && !driver.audio_off && !driver.subtitle_off && !driver.hls_uri);
    }

    /// The boot log names the display the installed driver reports, so a television's log shows it
    /// without a trigger: a plane the driver can bind says `ready`, and one it cannot says why not.
    #[cfg(feature = "play-uri")]
    #[test]
    fn the_boot_log_names_the_display_the_installed_driver_reports() {
        let line = |binding| plx_base::eventlog::with_private_log(|| {
            let mut adapter = PlayerAdapter::new(unsafe { MainThread::assume() });
            let mut session = PlaybackSession::IDLE;
            assert!(install_uri_product_with_ports(&mut adapter, &mut session,
                UriProduct::production(binding), &plx_os::tv::uri::NoUriMedia, &sink::NoSink));
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        let acb = line(UriBinding::Acb);
        assert!(acb.contains("uri: display path=acb ready\n"), "{acb}");
        let unbound = line(UriBinding::Unbound);
        assert!(unbound.contains("uri: display path=unbound unavailable (the driver has no display path)\n"),
            "{unbound}");
    }
}
