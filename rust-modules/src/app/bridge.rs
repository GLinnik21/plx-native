//! **The bridge between the legacy loop and the dispatcher** (restructure spec §14, phase 5b —
//! the shadow of phase 3b grown into the seam the migration crosses screen by screen).
//!
//! What lives here: the application's `Host` ([`AppHost`]) — its screen argument [`AppArg`],
//! which since phase 12 (D1) is the WHOLE page alphabet rather than a wrapper around a second one;
//! the mounter's one `match` ([`AppMounter`]); the rig the dispatcher borrows ([`Bridge`]: the real
//! `TtfMeasure`, the store deliveries of phase 4, the consent MACHINE, the loop requests an owned
//! screen makes); [`frame`], which the loop calls once per iteration with the inputs it collected
//! for the dispatcher; and — the part D1 added — **the loop's navigations as container ops**
//! ([`nav_root`], [`nav_push`], [`nav_pop`], [`nav_pop_to`], [`nav_cancel`], [`nav_tab`],
//! [`show_page`], [`open_detail`], [`switch_profile`], [`background`], [`foreground`]). Those are
//! the seam: the `Route` enum, `app/nav.rs` and `ui/trail.rs` were a SECOND navigation system
//! kept in step with this one by a per-frame `sync_page`, and every place the two could disagree
//! was a bug nobody could see. There is one authority now, and it is the container.
//!
//! The coexistence contract (§14), stated once:
//! - **Input.** The loop's ladders ask [`Dispatcher::owns_input`] before their first arm. While a
//!   surface is up, or the top page is an owned one, every key, pointer move, click and wheel is
//!   an `InputEvent` for the dispatcher and the ladders see nothing. Otherwise the ladders answer
//!   as they always did and the dispatcher receives no input.
//! - **Draw.** The loop draws the tree at the positional point its own frame reserves for the
//!   Settings family (after the page and the compact popovers, under the dev glass), on ITS
//!   present gate: [`Dispatcher::draw`]. Owned pages are drawn in the page closure instead.
//!   Navigation presentation is captured into `DrawFrame` once per pass; surfaces keep their own
//!   appear alpha. The dispatcher's own gate wanting a present becomes `idle::invalidate`.
//! - **The host under a surface.** The dispatcher's host fold is the loop's freeze/skip decision
//!   (`host_frozen`/`host_replaced`), and the surface's phases drive `popover`'s host-user
//!   counters ([`sync_host`]) so the FrameCache snapshot and the tab glass behave exactly as they
//!   did under the legacy `Popover`.
//! - **Words.** The heartbeat's `overlay=` reads the topmost surface's `Screen::name`
//!   ([`overlay_word`]), so the fps tier's word table is unchanged (§15.3).
//! - **The three privileged calls.** `ls2_pump`'s rig hook is still a no-op (3b's reason: the
//!   dispatcher does not yet run a phase this early in the frame). `opaque_route` and
//!   `clear_opaque_region` stopped being no-ops in phase 9 — the rig's hooks are real, but they
//!   delegate to `app/run.rs::rig_opaque_route`/`rig_clear_opaque_region` rather than naming
//!   `crate::system::` here, which is what keeps the OS-facing call text in one file (D4;
//!   `ci/check-deps.sh`'s `frame` gate).

use std::ffi::CStr;

use crate::screens::family::SettingsPage;
use crate::screens::registry::{AppArg, AppFx, AppMounter, AppMsg, ConsentCmd, ContentArg, ContentReq, HomeCmd, HomeLike, HomeReq, HomeTab, ItemMenuKind, LibraryReq, LoopReq, PageMemory};
use crate::stores::{StoreCmd, StoreEv, StoreId};
use crate::ui::containers::modal::{HostRender, HostUpdate, Phase, Style};
use crate::ui::dispatch::{CxParts, Dispatcher, FrameReport, Rig, Split};
#[cfg(test)]
use crate::screens::registry::every_surface_arg;
#[cfg(test)]
use crate::ui::dispatch::NoTap;
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, Handled, Host,
    InputEvent, InputKind, InputOwner, InstanceId, Key, LogicalState, Machine, MachineId, Measure, NavOp,
    Source, Tick, TimerId,
};
use crate::ui::present::Present;
use crate::ui::screen::{
    At, DrawFrame, FocusSource, Focusable, ReturnState, Screen, ScreenEvent,
};


// ---------------------------------------------------------------------------------------------
// the bundle
// ---------------------------------------------------------------------------------------------

pub(crate) struct AppHost;

// (`AppArg::from_node` and `AppArg::node` stood here — the two conversions between a screen
// argument and a `ui::trail::Node`, which this module's own doc called "about the LEGACY trail
// rather than about the alphabet". Both go with the trail: an argument IS the identity, and the
// `Spot` a node carried for a detail page is the entry's own `ReturnState::memory`.)

/// Is this argument the Settings family, whatever page it was rooted at?
fn is_settings(a: &AppArg) -> bool {
    matches!(a, AppArg::Settings(_))
}

/// …and the first-run question, whatever stage it was rooted at.
fn is_first_run_consent(a: &AppArg) -> bool {
    matches!(a, AppArg::FirstRunConsent(_))
}

/// Core-side shape of the Chrome refresh boundary. The retained directory is explicit so Chrome
/// always reads this Bridge's captured Browse publication.
fn refresh_chrome(
    chrome: &mut super::chrome::ChromeSnapshot,
    measure: &dyn Measure,
    directory: crate::stores::browse::DirectoryView<'_>,
    captured: Option<(&crate::plex::session::CurrentProfile, &crate::plex::session::Session)>,
) {
    match captured {
        Some(captured) => chrome.refresh_with_profile(measure, directory, Some(captured)),
        None => chrome.refresh(measure, directory),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct AppViews<'a> {
    pub(crate) auth: crate::auth::SessionRead<'a>,
    pub(crate) hubs: crate::pms::HubsView<'a>,
    pub(crate) listing: crate::stores::browse::ListingView<'a>,
    pub(crate) directory: crate::stores::browse::DirectoryView<'a>,
    pub(crate) section_hubs: crate::stores::browse::HubsView<'a>,
    pub(crate) search: crate::search::view::SearchView<'a>,
    pub(crate) person: crate::person::PersonView<'a>,
    /// **The playback session, as this frame's publication** (spec §2.3, phase 9). The Player
    /// machine (`App.player`) owns the value; `Split` can only lend what the RIG owns, so the loop
    /// copies the decisions in once per frame ([`crate::route::PlaybackSession::publication`]) and
    /// a screen reads them here. A screen that wants to CHANGE the playback emits an effect
    /// (`AppFx::Player`, `ContentReq::Play`) — there is no `&mut` on this path by construction.
    pub(crate) session: &'a crate::route::PlaybackSession,
}

#[derive(Default)]
pub(crate) struct BridgeInit;

/// Retained read inputs captured before construction. This is not a store decision owner.
struct StorePublications {
    hubs: crate::pms::HubsSnapshot,
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    section_hubs: crate::stores::browse::HubsSnapshot,
    search: crate::stores::search::SearchSnapshot,
}

impl LogicalState for BridgeInit {
    fn write(&self, _w: &mut Canon) {}
    fn probe(&self, out: &mut String) {
        out.push_str("bridge");
    }
}

impl Host for AppHost {
    type Arg = AppArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = AppViews<'a>;
    type Init = BridgeInit;
    // Entry-owned content memory preserves Detail's Spot and the stable element registries
    // of Detail, Person and Filmography across eviction. The opaque payload belongs to the
    // screen bundle; current focus and group cursors remain the input engine's state.
    type Memory = PageMemory;
}

impl HomeLike for AppHost {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> crate::pms::HubsView<'a> { cx.views.hubs }
}

impl crate::screens::registry::AuthLike for AppHost {
    fn auth<'a>(cx: &Cx<'a, Self>) -> crate::auth::SessionRead<'a> { cx.views.auth }
}

impl crate::auth::owner::SessionHost for AppHost {
    fn session_effect(effect: crate::auth::owner::SessionFx) -> AppFx {
        AppFx::SessionEffect(effect)
    }
}

impl crate::stores::StoreEffectHost for AppHost {
    fn endpoint_refresh(request: crate::stores::EndpointRefresh) -> AppFx {
        AppFx::Session(crate::auth::SessionCmd::RequestEndpoint { sid: request.sid })
    }
}

/// Queue an application command on the same drain as screen effects and worker observations.
pub(crate) fn execute_session_command(d: &mut Dispatcher<AppHost>, command: crate::auth::SessionCmd) {
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Session,
        Delivery::Machine(AppMsg::Session(crate::auth::owner::SessionEvent::Command(command)))));
}

pub(crate) fn execute_endpoint_outcomes(d: &mut Dispatcher<AppHost>, endpoints: crate::stores::EndpointRefreshSet) {
    execute_endpoint_outcomes_with(endpoints, |command| execute_session_command(d, command));
}

fn execute_endpoint_outcomes_with(
    endpoints: crate::stores::EndpointRefreshSet,
    mut execute: impl FnMut(crate::auth::SessionCmd),
) {
    for request in endpoints.iter() {
        execute(crate::auth::SessionCmd::RequestEndpoint { sid: request.sid });
    }
}

impl crate::screens::registry::PlayerLike for AppHost {
    fn session<'a>(cx: &Cx<'a, Self>) -> &'a crate::route::PlaybackSession { cx.views.session }
}

impl crate::screens::registry::SearchLike for AppHost {
    fn search<'a>(cx: &Cx<'a, Self>) -> crate::search::view::SearchView<'a> { cx.views.search }
}

impl crate::screens::registry::LibraryLike for AppHost {
    fn listing<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::HubsView<'a> { cx.views.section_hubs }
}

impl crate::screens::registry::PersonLike for AppHost {
    fn person<'a>(cx: &Cx<'a, Self>) -> crate::person::PersonView<'a> { cx.views.person }
}

// ---------------------------------------------------------------------------------------------
// the rig
// ---------------------------------------------------------------------------------------------

/// The consent MACHINE (§2.2): the physical owner of the two decisions. The adapter persists and
/// publishes immutable snapshots; it never supplies the previous decision back to this owner.
pub(crate) struct ConsentMachine {
    current: crate::telemetry::consent::Consent,
}

impl ConsentMachine {
    pub(crate) const SHAPE: &'static str =
        "Consent{asked_version:u32,errors:bool,usage:bool,install_id:option<string>,errors_id:option<string>}";

    fn from_initial(current: crate::telemetry::consent::Consent) -> Self { Self { current } }

    fn record(&mut self, adapter: &mut super::adapters::consent::ConsentAdapter,
        errors: bool, usage: bool) {
        let next = crate::telemetry::consent::apply(
            &self.current, errors, usage, crate::telemetry::mint_id);
        for (asked, got, channel) in [
            (errors, next.errors, "crash reports"),
            (usage, next.usage, "usage analytics"),
        ] {
            if asked && !got {
                crate::log(&format!(
                    "consent: no /dev/urandom — refusing {channel} rather than inventing an identifier"
                ));
            }
        }
        adapter.commit(&self.current, &next);
        self.current = next;
        crate::telemetry::flush_soon();
    }

    fn forget(&mut self, adapter: &mut super::adapters::consent::ConsentAdapter) {
        adapter.forget(&self.current);
        self.current = Default::default();
    }

    pub(crate) fn subhash(&self) -> u64 {
        let mut c = Canon::new();
        c.u32(self.current.asked_version)
            .bool(self.current.errors)
            .bool(self.current.usage);
        for value in [&self.current.install_id, &self.current.errors_id] {
            c.bool(value.is_some());
            if let Some(value) = value { c.str(value); }
        }
        c.finish()
    }

    #[cfg(test)]
    fn current(&self) -> &crate::telemetry::consent::Consent { &self.current }
}

/// What the bridge lends the dispatcher, and what it collects for the loop.
pub(crate) struct Bridge {
    home_io: Option<super::bootstrap::HomeIo>,
    recorded_clients: std::collections::BTreeMap<u32, &'static crate::plex::Client>,
    initial_subhash: u64,
    session: crate::auth::SessionMachine,
    session_adapter: super::adapters::session::SessionAdapter,
    session_ready: Option<(u64, crate::auth::owner::ProfileScope, crate::plex::session::ServerRef, String, crate::auth::owner::ReadyInstall)>,
    consent_adapter: super::adapters::consent::ConsentAdapter,
    stores: crate::stores::Stores,
    mounter: AppMounter,
    /// This frame's publication of the playback session — see `AppViews::session`. Refreshed by
    /// [`Bridge::publish_playback`] from the loop, once per iteration.
    playback: crate::route::PlaybackSession,
    /// Was the publication above refreshed on the last frame? The one bit that lets the retirement
    /// of a stale publication be a single assignment rather than a per-frame one.
    playback_live: bool,
    /// **Is the hardware video plane bound?** `Player::video_plane_bound`, published here by the
    /// loop on its edges — the rig's own copy of the one bit, for the privileged call at draw
    /// entry (`clear_opaque_region`), which the `Rig` trait gives no argument.
    video_plane: bool,
    /// **Erased, so the host suite can supply one that does not need a font.** The app's own
    /// measure is `text::TtfMeasure`, which reads the fonts `init_text` opened at boot and carries
    /// a deliberate `debug_assert!` when there are none ("no font is loaded") — loud on purpose,
    /// because a UI that silently lays itself out on guessed advances is worse than one that
    /// stops. A host test has no SDL, no `init_text` and no font file, so any test that drives a
    /// real frame through a real screen trips that assertion and dies inside `text.rs` rather than
    /// on its own assertion. `Split::measure` is `&dyn Measure` already, so erasing it here costs
    /// the app nothing and buys [`Bridge::for_test`].
    measure: crate::ui::rec::Measurements,
    hubs: crate::pms::HubsSnapshot,
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    section_hubs: crate::stores::browse::HubsSnapshot,
    search: crate::stores::search::SearchSnapshot,
    chrome: super::chrome::ChromeSnapshot,
    chrome_selection: u32,
    /// The shared top bar's render state — the strip's scroll/capsules/chip unfurl and the tab
    /// track's glass band (restructure phase 12, PX-WIDGETS, review finding 10). `Bridge` is this
    /// bar's one reachable owner: the only `Rig::draw_chrome` implementation, and the only place
    /// `update_home_chrome`/`prepare_home_chrome` are called from.
    strip: crate::ui::widgets::StripRender,
    home_commands: std::collections::VecDeque<HomeCmd>,
    library_commands: std::collections::VecDeque<crate::screens::registry::LibraryCmd>,
    consent: ConsentMachine,
    /// Requests the owned screens made of the loop this frame (§14), drained by [`frame`]'s caller.
    reqs: Vec<LoopReq>,
    content_reqs: Vec<(MachineId, ContentReq, ReturnState<u32, PageMemory>)>,
    home_reqs: Vec<(MachineId, HomeReq, ReturnState<u32, PageMemory>)>,
    library_reqs: Vec<(MachineId, LibraryReq, ReturnState<u32, PageMemory>)>,
    search_reqs: Vec<(MachineId, crate::screens::registry::SearchReq, ReturnState<u32, PageMemory>)>,
    /// What the player's overlay surfaces asked of the loop this frame (§14) — drained by
    /// `playback::player_requests`, which holds the `MainThread` token they cannot.
    player_reqs: Vec<crate::screens::registry::PlayerReq>,
    /// …and what the item context menu asked, drained by `content::content_requests` — which holds
    /// the route, the trail and the playback session's `&mut` that its dispatch needs.
    item_menu_reqs: Vec<crate::screens::registry::ItemMenuReq>,
    #[cfg(test)]
    keyboard_calls: Vec<bool>,
    #[cfg(test)]
    keyboard_adoptions: usize,
    effect_return: ReturnState<u32, PageMemory>,
    /// The surfaces whose host counters this bridge holds: (entry, cached, closing).
    held: Vec<(EntryId, bool, bool)>,
    now_us: fn() -> u64,
}

impl Bridge {
    #[cfg(test)]
    pub(crate) fn measurement_queries(&self)->Vec<crate::ui::rec::MetricKey> { self.measure.queries() }
    pub(crate) fn prepare_measurements(&mut self, replay: Option<&std::collections::HashMap<crate::ui::rec::MetricKey,u32>>) {
        self.measure.prepare(replay);
    }
    pub(crate) fn take_measurements(&self) -> Result<Vec<(crate::ui::rec::MetricKey,u32)>, &'static str> {
        self.measure.drain()
    }
    pub(crate) fn is_controlled_replay(&self) -> bool {
        self.home_io.as_ref().is_some_and(|io| io.replay)
    }
    pub(crate) fn recording_retired_for_erasure(&mut self, failed: bool) {
        debug_assert!(!self.is_controlled_replay());
        self.home_io = None;
        self.measure.retire();
        self.session_adapter.recording_retired_for_erasure(failed);
    }
    pub(crate) fn controlled_failure(&self) -> Option<&'static str> {
        if self.recorded_clients.values().any(|client| client.denied_data_requests() != 0) {
            return Some("unrecorded client IO attempted");
        }
        self.home_io.as_ref().and_then(|io| io.failure)
    }
    pub(crate) fn take_resource_requests(&mut self) -> Vec<serde_json::Value> {
        if let Some(io) = &mut self.home_io {
            if !io.admissions.is_empty() { io.failure = Some("unconsumed synchronous admission"); }
        }
        self.home_io.as_mut().map_or_else(Vec::new, |io| std::mem::take(&mut io.requests))
    }
    pub(crate) fn supply_admissions(&mut self, admissions: std::collections::VecDeque<serde_json::Value>) {
        if let Some(io) = &mut self.home_io {
            if !io.admissions.is_empty() { io.failure = Some("unconsumed synchronous admission"); }
            io.admissions = admissions;
        }
    }
    pub(crate) fn controlled_home(now_us: fn() -> u64, initial: &super::bootstrap::Initial,
        mt: &crate::task::MainThread, replay: bool) -> Self {
        static TTF: crate::text::TtfMeasure = crate::text::TtfMeasure;
        let preferences = initial.session.persisted.clone();
        let mut bridge = Self::with_publications(&TTF, now_us, initial.session.clone(),
            super::adapters::session::SessionAdapter::controlled_home(mt, replay),
            initial.consent.clone(), super::adapters::consent::ConsentAdapter::live(),
            StorePublications {
                hubs:initial.home.snapshot(), listing:crate::stores::browse::ListingSnapshot::empty(),
                directory:Default::default(), section_hubs:crate::stores::browse::HubsSnapshot::empty(),
                search:Default::default(),
            });
        bridge.initial_subhash = initial.hash();
        bridge.measure = if replay {
            crate::ui::rec::Measurements::Pending(Default::default())
        } else { crate::ui::rec::Measurements::record(&TTF) };
        bridge.home_io = Some(super::bootstrap::HomeIo { replay, preferences, requests: Vec::new(), admissions: Default::default(),
            failure: None, profile: bridge.session_adapter.profile_resource_view().expect("controlled publisher") });
        bridge
    }
    pub(crate) fn new(now_us: fn() -> u64, init: crate::auth::SessionInit,
        consent: crate::telemetry::consent::Consent, mt: &crate::task::MainThread) -> Self {
        // A `static`, not `&TtfMeasure` inline: a unit-struct literal DOES const-promote to
        // `'static` today, but that is a rule about the expression rather than a promise about
        // this field, and a `static` states the lifetime outright. Same reasoning as the one
        // `screens::settings`'s test module writes out beside its own measure.
        static TTF: crate::text::TtfMeasure = crate::text::TtfMeasure;
        Self::with_measure(&TTF, now_us, init, super::adapters::session::SessionAdapter::live(mt),
            consent, super::adapters::consent::ConsentAdapter::live())
    }

    /// The same bridge over a measure that needs no fonts — the ONLY constructor a host test may
    /// use. See [`Bridge::measure`] for what happens to a test that reaches for `new` instead: it
    /// dies inside `text.rs` on a `debug_assert!`, several frames away from anything it asserted.
    #[cfg(test)]
    pub(crate) fn for_test(now_us: fn() -> u64) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        Self::with_measure(&FIXTURE, now_us,
            crate::auth::SessionInit::captured(crate::plex::session::Session::default()),
            super::adapters::session::SessionAdapter::fixture(), Default::default(),
            super::adapters::consent::ConsentAdapter::fixture())
    }

    fn with_measure(measure: &'static dyn Measure, now_us: fn() -> u64,
        init: crate::auth::SessionInit, session_adapter: super::adapters::session::SessionAdapter,
        consent: crate::telemetry::consent::Consent,
        consent_adapter: super::adapters::consent::ConsentAdapter) -> Self {
        let stores = crate::stores::Stores::default();
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        let browse = stores.capture_browse(&mut directory);
        let search = crate::stores::search::snapshot_with_directory(browse.directory.view());
        Self::with_publications_and_stores(measure, now_us, init, session_adapter, consent,
            consent_adapter,
            StorePublications {
            hubs: crate::pms::hubs_snapshot(), listing: browse.listing,
            directory: browse.directory, section_hubs: browse.section_hubs,
            search,
        }, stores)
    }

    #[cfg(test)]
    fn for_session_test(init: crate::auth::SessionInit) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        let adapter = super::adapters::session::SessionAdapter::fixture_with(init.persisted.clone());
        Self::with_publications(&FIXTURE, || 0, init, adapter, Default::default(),
            super::adapters::consent::ConsentAdapter::fixture(), StorePublications {
            hubs: crate::pms::HubsSnapshot::empty_for_test(),
            listing: crate::stores::browse::ListingSnapshot::empty_for_test(),
            directory: Default::default(), section_hubs: crate::stores::browse::HubsSnapshot::empty_for_test(),
            search: Default::default(),
        })
    }

    #[cfg(test)]
    fn for_consent_test(consent: crate::telemetry::consent::Consent) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        Self::with_publications(&FIXTURE, || 0,
            crate::auth::SessionInit::captured(crate::plex::session::Session::default()),
            super::adapters::session::SessionAdapter::fixture(), consent,
            super::adapters::consent::ConsentAdapter::fixture(), StorePublications {
                hubs: crate::pms::HubsSnapshot::empty_for_test(),
                listing: crate::stores::browse::ListingSnapshot::empty(),
                directory: Default::default(), section_hubs: crate::stores::browse::HubsSnapshot::empty(),
                search: Default::default(),
            })
    }

    #[cfg(test)]
    pub(crate) fn for_consent_resource_test(consent: crate::telemetry::consent::Consent) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        Self::with_publications(&FIXTURE, || 0,
            crate::auth::SessionInit::captured(crate::plex::session::Session::default()),
            super::adapters::session::SessionAdapter::fixture(), consent,
            super::adapters::consent::ConsentAdapter::live(), StorePublications {
                hubs: crate::pms::HubsSnapshot::empty_for_test(),
                listing: crate::stores::browse::ListingSnapshot::empty(),
                directory: Default::default(), section_hubs: crate::stores::browse::HubsSnapshot::empty(),
                search: Default::default(),
            })
    }

    fn with_publications(measure: &'static dyn Measure, now_us: fn() -> u64,
        init: crate::auth::SessionInit, session_adapter: super::adapters::session::SessionAdapter,
        consent: crate::telemetry::consent::Consent,
        consent_adapter: super::adapters::consent::ConsentAdapter,
        reads: StorePublications) -> Self {
        Self::with_publications_and_stores(measure, now_us, init, session_adapter, consent,
            consent_adapter, reads, Default::default())
    }

    fn with_publications_and_stores(measure: &'static dyn Measure, now_us: fn() -> u64,
        init: crate::auth::SessionInit, session_adapter: super::adapters::session::SessionAdapter,
        consent: crate::telemetry::consent::Consent,
        consent_adapter: super::adapters::consent::ConsentAdapter,
        reads: StorePublications, stores: crate::stores::Stores) -> Self {
        Self {
            home_io: None,
            recorded_clients: std::collections::BTreeMap::new(),
            initial_subhash: 0,
            session: crate::auth::SessionMachine::from_init(init),
            session_adapter,
            session_ready: None,
            consent_adapter,
            stores,
            mounter: AppMounter::default(),
            playback: crate::route::PlaybackSession::IDLE,
            playback_live: false,
            video_plane: false,
            measure: crate::ui::rec::Measurements::Live(measure),
            hubs: reads.hubs,
            listing: reads.listing,
            directory: reads.directory,
            section_hubs: reads.section_hubs,
            search: reads.search,
            chrome: super::chrome::ChromeSnapshot::default(),
            chrome_selection: 0,
            strip: crate::ui::widgets::StripRender::new(),
            home_commands: std::collections::VecDeque::new(),
            library_commands: std::collections::VecDeque::new(),
            consent: ConsentMachine::from_initial(consent),
            reqs: Vec::new(),
            content_reqs: Vec::new(),
            home_reqs: Vec::new(),
            library_reqs: Vec::new(),
            search_reqs: Vec::new(),
            player_reqs: Vec::new(),
            item_menu_reqs: Vec::new(),
            #[cfg(test)]
            keyboard_calls: Vec::new(),
            #[cfg(test)]
            keyboard_adoptions: 0,
            effect_return: ReturnState::default(),
            held: Vec::new(),
            now_us,
        }
    }

    pub(crate) fn take_reqs(&mut self) -> Vec<LoopReq> {
        std::mem::take(&mut self.reqs)
    }

    pub(crate) fn auth_read(&self) -> crate::auth::SessionRead<'_> { self.session.read() }

    /// Cached logical Session hash, including pending work even when its UI read is unchanged.
    pub fn session_subhash(&self) -> u64 { self.session.subhash() }
    /// Physical Consent owner's decision hash; identifiers are folded, never serialized here.
    pub fn consent_subhash(&self) -> u64 { self.consent.subhash() }
    pub(crate) fn initial_subhash(&self) -> u64 { self.initial_subhash }
    #[cfg(test)]
    pub(crate) fn profile_resource_view(&self) -> Option<std::sync::Arc<crate::plex::session::CurrentProfile>> {
        self.session_adapter.profile_resource_view()
    }
    pub(crate) fn snapshot_session_init(&self) -> crate::auth::SessionInit { self.session.snapshot_init() }

    pub(crate) fn take_session_ready(&mut self) -> Option<crate::auth::ReadyCreds> {
        let (epoch, scope, server, token, install) = self.session_ready.take()?;
        if !self.session.ready_is_current(epoch, scope) { return None; }
        Some(crate::auth::ReadyCreds { origin: server.origin(), address: server.address.clone(),
            token, install, tier: server.tier, pin: server.resolve_pin() })
    }

    pub(crate) fn take_content_reqs(&mut self) -> Vec<(MachineId, ContentReq, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.content_reqs)
    }

    pub(crate) fn take_home_reqs(&mut self) -> Vec<(MachineId, HomeReq, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.home_reqs)
    }

    pub(crate) fn take_library_reqs(&mut self) -> Vec<(MachineId, LibraryReq, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.library_reqs)
    }

    pub(crate) fn take_player_reqs(&mut self) -> Vec<crate::screens::registry::PlayerReq> {
        std::mem::take(&mut self.player_reqs)
    }

    pub(crate) fn take_item_menu_reqs(&mut self) -> Vec<crate::screens::registry::ItemMenuReq> {
        std::mem::take(&mut self.item_menu_reqs)
    }

    pub(crate) fn take_search_reqs(&mut self) -> Vec<(MachineId, crate::screens::registry::SearchReq, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.search_reqs)
    }

    pub(crate) fn search_selection(&self, d: &Dispatcher<AppHost>, entry: EntryId, focus: Option<FocusKey<u32>>)
        -> Option<(crate::search::Item, crate::ui::popover::Opener)> {
        let page = d.nav.entry(entry)?.inst.as_ref()?.screen.as_any()?.downcast_ref::<crate::screens::search::SearchScreen>()?;
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: focus, ..Default::default() }, owner: InputOwner::Entry(entry) };
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(), hubs: self.hubs.view(), listing: self.listing.view(),
            directory: self.directory.view(), section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback }, &self.measure);
        let item = page.selected_item(focus, &cx)?.clone();
        let rect = page.place(&focus?.elem, &cx, At::Drawn)?.rest_rect;
        Some((item, crate::ui::popover::Opener { rect: Some(rect), ..crate::ui::popover::Opener::NONE }))
    }

    pub(crate) fn search_tab_available(&self, tab: HomeTab) -> bool {
        match tab {
            HomeTab::Movies => self.directory.view().preferred(crate::browse::SecKind::Movie).is_some(),
            HomeTab::Shows => self.directory.view().preferred(crate::browse::SecKind::Show).is_some(),
            _ => true,
        }
    }

    pub(crate) fn library_selection(&self, d: &Dispatcher<AppHost>, entry: EntryId, focus: Option<FocusKey<u32>>)
        -> Option<(crate::pms::PmsMovie, crate::ui::popover::Opener)> {
        let page = d.nav.entry(entry)?.inst.as_ref()?.screen.as_any()?.downcast_ref::<crate::screens::library::LibraryScreen>()?;
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: focus , ..Default::default() }, owner: InputOwner::Entry(entry) };
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(), hubs: self.hubs.view(), listing: self.listing.view(),
            directory: self.directory.view(), section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback }, &self.measure);
        let item = page.focused_item(focus, &cx)?.clone();
        let rect = page.place(&focus?.elem, &cx, At::Drawn)?.rest_rect;
        Some((item, crate::ui::popover::Opener { rect: Some(rect), ..crate::ui::popover::Opener::NONE }))
    }

    pub(crate) fn library_command(d: &mut Dispatcher<AppHost>, command: crate::screens::registry::LibraryCmd) {
        if let crate::screens::registry::LibraryCmd::SwitchStep(_) = command {
            if let Some(InputOwner::Entry(owner)) = d.nav.input_owner() {
                if let Some(entry) = d.nav.entry(owner).filter(|entry| matches!(entry.arg, AppArg::LibraryMenu(_))) {
                    if let Some(instance) = entry.inst.as_ref().map(|instance| instance.id) {
                        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                            Delivery::Screen(ScreenEvent::App(AppMsg::Library(command)))));
                        return;
                    }
                }
            }
        }
        let Some(entry) = d.nav.top_page() else { return };
        if entry.arg != AppArg::Library { return; }
        let Some(instance) = entry.inst.as_ref().map(|instance| instance.id) else { return };
        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
            Delivery::Screen(ScreenEvent::App(AppMsg::Library(command)))));
    }

    pub(crate) fn library_card_focused(d: &Dispatcher<AppHost>) -> bool {
        let Some(entry) = d.nav.top_page() else { return false };
        let Some(page) = entry.inst.as_ref().and_then(|instance| instance.screen.as_any())
            .and_then(|page| page.downcast_ref::<crate::screens::library::LibraryScreen>()) else { return false };
        matches!(page.probe_viewport(d.input.engine.current(InputOwner::Entry(entry.id))).0, "grid" | "shelf")
    }

    pub(crate) fn enter_library(&mut self, kind: crate::browse::SecKind) {
        self.mounter.library_kind = Some(kind);
        self.library_commands.clear();
        self.library_commands.push_back(crate::screens::registry::LibraryCmd::Enter(kind));
    }

    fn deliver_library_commands(&mut self, d: &mut Dispatcher<AppHost>) {
        if d.nav.top_page().is_none_or(|page| page.arg != AppArg::Library || page.inst.is_none()) { return; }
        while let Some(command) = self.library_commands.pop_front() { Self::library_command(d, command); }
    }

    pub(crate) fn home_opener(&self, d: &Dispatcher<AppHost>, entry: EntryId,
        focus: Option<FocusKey<u32>>) -> crate::ui::popover::Opener {
        let rect = focus.filter(|key| key.entry == entry).and_then(|key| {
            let screen = &d.nav.entry(entry)?.inst.as_ref()?.screen;
            let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
                focus: crate::ui::machine::FocusRead { current: Some(key) , ..Default::default() }, owner: InputOwner::Entry(entry) };
            let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
                hubs: self.hubs.view(),
                listing: self.listing.view(),
                directory: self.directory.view(),
                section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
            }, &self.measure);
            screen.as_any()?.downcast_ref::<crate::screens::home::HomeScreen>()?
                .focused_rect::<AppHost>(Some(key), &cx, At::Drawn)
        });
        crate::ui::popover::Opener { rect, ..crate::ui::popover::Opener::NONE }
    }

    /// Restore a detail page that is about to mount to a `Spot` no `ReturnState` holds — see
    /// [`crate::screens::registry::DetailSeed`].
    pub(crate) fn seed_detail(&mut self, sid: crate::plex::ServerId, rk: &str, spot: crate::metadata::Spot) {
        self.mounter.seed = Some(crate::screens::registry::DetailSeed { sid, rk: rk.to_string(), spot });
    }

    /// Where the next player instance returns to — see `AppMounter::player_origin`.
    pub(crate) fn seed_player_origin(&mut self, origin: crate::screens::player::Origin) {
        self.mounter.player_origin = Some(origin);
    }

    /// How long the next player instance pins its transport for — see
    /// [`AppMounter::player_hud_ms`]. Set by `start_playback` and consumed by the mount.
    pub(crate) fn seed_player_hud(&mut self, ms: u32) {
        self.mounter.player_hud_ms = Some(ms);
    }

    fn home_focus(d: &Dispatcher<AppHost>) -> Option<FocusKey<u32>> {
        let entry = d.nav.top_page()?.id;
        d.input.engine.current(InputOwner::Entry(entry))
    }

    fn with_home<R>(&self, d: &Dispatcher<AppHost>, f: impl for<'a> FnOnce(&crate::screens::home::HomeScreen,
        &'a Cx<'a, AppHost>, Option<FocusKey<u32>>) -> R) -> Option<R> {
        let entry = d.nav.top_page()?;
        let home = entry.inst.as_ref()?.screen.as_any()?.downcast_ref::<crate::screens::home::HomeScreen>()?;
        let focus = Self::home_focus(d);
        let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: focus , ..Default::default() }, owner: InputOwner::Entry(entry.id) };
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
        }, &self.measure);
        Some(f(home, &cx, focus))
    }

    pub(crate) fn home_grid_focused(&self, d: &Dispatcher<AppHost>) -> bool {
        self.with_home(d, |home, cx, focus| home.grid_position::<AppHost>(focus, cx).is_some()).unwrap_or(false)
    }

    pub(crate) fn home_snap_target(&self, d: &Dispatcher<AppHost>) -> f32 {
        self.with_home(d, |home, _, _| home.snap_target()).unwrap_or(0.0)
    }

    fn capture_chrome(&mut self, d: &mut Dispatcher<AppHost>) {
        let route = d.top_arg().cloned().unwrap_or(AppArg::Home);
        let route = &route;
        let search = *route == AppArg::Search;
        if matches!(route, AppArg::Home | AppArg::Library) || search {
            d.nav.tabs.strip_fallback = Some(if search { crate::ui::dispatch::STRIP_BASE + 3 } else { crate::screens::home::STRIP_HOME_ELEM });
            if let Some(io) = &self.home_io {
                refresh_chrome(&mut self.chrome, &self.measure, self.directory.view(),
                    Some((&io.profile, &io.preferences)));
            } else {
                refresh_chrome(&mut self.chrome, &self.measure, self.directory.view(), None);
            }
            self.chrome_selection = if *route == AppArg::Library {
                self.directory.view().current().map(|i| self.chrome.library_selection(self.directory.view().sections()[i].kind)).unwrap_or(0)
            } else if search { self.chrome.search_selection() } else { 0 };
            let selected = self.navigation_presentation().view_tab.unwrap_or(self.chrome_selection) as i32;
            self.chrome.members(selected, Self::home_focus(d), self.strip.scroll_pos(), &mut d.nav.tabs.strip);
        } else {
            d.nav.tabs.strip.clear();
            d.nav.tabs.strip_fallback = None;
        }
    }

    /// Return Browse/Search publication changes so the frame can coalesce them with notices.
    fn capture_views(&mut self, d: &mut Dispatcher<AppHost>) -> (bool, bool) {
        let directory_before = self.directory.clone();
        let hubs_before = (self.section_hubs.view().id(), self.section_hubs.view().revision());
        // One owner borrow, in the load-bearing order: directory capture resolves profile pins
        // and may repoint the current section, so listing and shelves are captured only after it.
        let owned = self.stores.capture_browse(&mut self.directory);
        if self.home_io.is_none() { self.section_hubs = owned.section_hubs; }
        let listing = if self.home_io.is_some() { self.listing.clone() } else { owned.listing };
        let listing_changed = !self.listing.view().same_items(listing.view())
            || self.listing.view().fetch() != listing.view().fetch();
        self.listing = listing;
        let browse_changed = listing_changed || !self.directory.same_publication(&directory_before)
            || hubs_before != (self.section_hubs.view().id(), self.section_hubs.view().revision());
        let search = if self.home_io.is_some() { self.search.clone() } else {
            crate::stores::search::snapshot_with_directory(self.directory.view())
        };
        let search_changed = !self.search.same_publication(&search);
        if search_changed {
            self.search = search;
        }
        let before = (self.hubs.view().generation, self.hubs.view().state);
        self.hubs = crate::pms::hubs_snapshot();
        let after = (self.hubs.view().generation, self.hubs.view().state);
        if before != after {
            // This is queued before input deliveries. Key projections catch up to the newly
            // captured publication before any action or draw can read it, including status-only
            // landings which the legacy pump's catalog-generation notice cannot see.
            d.store_changed(StoreId::Hubs.ord(), after.0);
        }
        (browse_changed, search_changed)
    }

    pub(crate) fn update_home_chrome(&mut self, d: &mut Dispatcher<AppHost>,
        glass: &mut crate::ui::frame::glass::GlassPlan, dt: f32) {
        let selected = self.navigation_presentation().view_tab.unwrap_or(self.chrome_selection) as i32;
        let focus = Self::home_focus(d);
        let labels = self.chrome.labels();
        let chrome_focus = self.chrome.focus(focus);
        self.strip.update(labels, selected, chrome_focus, dt);
        glass.step_tab_band(dt);
        self.chrome.members(selected, focus, self.strip.scroll_pos(), &mut d.nav.tabs.strip);
    }

    pub(crate) fn prepare_home_chrome(&self, glass: &mut crate::ui::frame::glass::GlassPlan) {
        let labels = self.chrome.labels();
        glass.prepare_tab_band(labels);
    }

    #[cfg(test)]
    fn seed_chrome_for_test(&mut self, name: &str, initial: &str, labels: &[&str]) {
        self.chrome.seed_for_test(name, initial, labels, &self.measure);
    }

    pub(crate) fn home_command(&mut self, command: HomeCmd) -> bool {
        if self.home_commands.contains(&command) { return true; }
        if self.home_commands.len() >= 8 { return false; }
        self.home_commands.push_back(command);
        true
    }

    fn deliver_home_commands(&mut self, d: &mut Dispatcher<AppHost>) {
        let Some(entry) = d.nav.top_page() else { return };
        if !matches!(entry.arg, AppArg::Home)
            || d.nav.input_owner() != Some(InputOwner::Entry(entry.id)) { return; }
        let Some(instance) = entry.inst.as_ref().map(|instance| instance.id) else { return };
        let snapshot = crate::pms::hubs_snapshot();
        let ready = snapshot.view().hub_count() > 0;
        // Retain data-dependent boot intentions until the first catalog arrives. A command
        // is addressed only after the Home body exists; no UI state is mutated by this queue.
        while let Some(command) = self.home_commands.front().copied() {
            if !ready && matches!(command, HomeCmd::FocusGrid { .. } | HomeCmd::SelectHero(_) | HomeCmd::Flip(_) | HomeCmd::ItemMenu) {
                break;
            }
            self.home_commands.pop_front();
            d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance), Delivery::Screen(ScreenEvent::App(AppMsg::Home(command)))));
        }
    }

    pub(crate) fn request_home_menu(&mut self, d: &Dispatcher<AppHost>) -> bool {
        let Some(entry) = d.nav.top_page() else { return false };
        let Some(home) = entry.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::home::HomeScreen>()) else { return false };
        if <crate::screens::home::HomeScreen as Screen<AppHost>>::strip_reachable(home) { return false; }
        let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: Self::home_focus(d) , ..Default::default() }, owner: InputOwner::Entry(entry.id) };
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
        }, &self.measure);
        if home.grid_position::<AppHost>(parts.focus.current, &cx).is_none() { return false; }
        self.home_command(HomeCmd::ItemMenu)
    }

    /// **What a CONTENT page's hold is a menu about** — the detail page's season tabs, its episode
    /// filmstrip and its Related shelf, and a person page's filmography — as the argument that
    /// presents it. It used to CALL the three `ui::item_menu::open*` entry points and answer with
    /// a `MenuHost` for the loop to put on `app.route`; the surface's argument carries all of it
    /// now, including the two bits that variant existed to say.
    pub(crate) fn content_menu_arg(&self, d: &Dispatcher<AppHost>, entry: EntryId, ret: &ReturnState<u32, PageMemory>)
        -> Option<crate::screens::registry::ItemMenuArg> {
        let e = d.nav.entry(entry)?;
        let screen = e.inst.as_ref()?.screen.as_any()?;
        let mut parts = CxParts { tick: Tick { ms: 0, dt_us: 0 },
            press: crate::ui::machine::PressRead { scale: 1.0, is_long: false },
            focus: crate::ui::machine::FocusRead { current: None , ..Default::default() },
            owner: crate::ui::machine::InputOwner::Entry(entry) };
        parts.owner = crate::ui::machine::InputOwner::Entry(entry);
        parts.focus.current = ret.focus;
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
        }, &self.measure);
        if let Some(page) = screen.downcast_ref::<crate::screens::detail::DetailScreen>() {
            let sid = match &e.arg { AppArg::Content(ContentArg::Detail { sid, .. }) => *sid, _ => return None };
            let rect = page.focused_rect::<AppHost>(ret.focus, &cx, At::Drawn);
            if let Some((rk, mark)) = page.focused_season(ret.focus) {
                Some(strip_menu_arg(sid, &rk, ItemMenuKind::Season { mark }, entry, ret.focus, rect))
            } else if let Some((rk, mark)) = page.focused_episode(ret.focus) {
                // the ONE entry point whose item is a leaf of the season this page has loaded
                Some(strip_menu_arg(sid, &rk, ItemMenuKind::Episode { mark }, entry, ret.focus, rect))
            } else {
                // …a RELATED tile is a DIFFERENT item standing on the same page: an ordinary card
                // row, which is exactly what `MenuHost::Related` existed to say.
                let item = page.focused_related(ret.focus).filter(|m| crate::screens::item_menu::has_actions(m))?;
                Some(card_menu_arg(item, false, false, entry, ret.focus, rect))
            }
        } else if let Some(page) = screen.downcast_ref::<crate::screens::person::PersonScreen>() {
            let item = page.focused_item(ret.focus, &cx).filter(|m| crate::screens::item_menu::has_actions(m))?;
            let rect = page.focused_rect::<AppHost>(ret.focus, &cx, At::Drawn);
            Some(card_menu_arg(item, false, false, entry, ret.focus, rect))
        } else { None }
    }

    /// **The opener LIFT: the focused tile repainted above the modal dim.** Render-only, and the
    /// pair it works from is the SURFACE's own argument (`ItemMenuScreen::opener`) — it was
    /// `Bridge::menu_opener`, a second copy of the same `(entry, focus)` kept on the side and
    /// written by four separate arms.
    ///
    /// It runs immediately after the container's page pass, which is where `ModalStack::draw_scrims`
    /// laid the dim down: the tile is the panel's whole subject, and the design's stated point is
    /// that the card stays visible behind it. The dim is the CONTAINER's now (`Screen::scrim`), so
    /// what is left here is the half only the page that drew the element can answer — a `fn()` lift
    /// has nothing to borrow a `&Dispatcher` through.
    pub(crate) fn redraw_opener(&self, d: &Dispatcher<AppHost>) {
        let Some((entry, focus)) = item_menu(d).map(|menu| menu.opener()) else { return };
        if d.nav.top_page().map(|e| e.id) != Some(entry) { return; }
        let Some(screen) = d.nav.entry(entry).and_then(|e| e.inst.as_ref()).and_then(|i| i.screen.as_any()) else { return };
        // The page pass may be submitting a cached host quad. Opener lifts are live paint
        // above that quad, like Popover::scrim_lifting's legacy callback scope.
        let _live = crate::ui::popover::host::live();
        let mut parts = CxParts { tick: Tick { ms: 0, dt_us: 0 },
            press: crate::ui::machine::PressRead { scale: 1.0, is_long: false },
            focus: crate::ui::machine::FocusRead { current: None , ..Default::default() },
            owner: crate::ui::machine::InputOwner::Entry(entry) };
        parts.owner = crate::ui::machine::InputOwner::Entry(entry);
        parts.focus.current = focus;
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
        }, &self.measure);
        let mut frame = DrawFrame::with_navigation(&cx, crate::ui::Painter::root(), self.navigation_presentation());
        if let Some(page) = screen.downcast_ref::<crate::screens::detail::DetailScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        } else if let Some(page) = screen.downcast_ref::<crate::screens::person::PersonScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        } else if let Some(page) = screen.downcast_ref::<crate::screens::home::HomeScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        } else if let Some(page) = screen.downcast_ref::<crate::screens::library::LibraryScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        } else if let Some(page) = screen.downcast_ref::<crate::screens::search::SearchScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        }
    }

    /// Drive `popover`'s host-user counters from the surface phases (module doc).
    fn sync_host(&mut self, d: &Dispatcher<AppHost>) {
        let live: Vec<(EntryId, bool, bool)> = d
            .nav
            .modals
            .surfaces
            .iter()
            .filter(|s| s.phase != Phase::Hidden)
            .map(|s| {
                // DERIVED from the container's own policy table, never a second `matches!` over
                // `Style` — see `modal::style_caches_host`, which carries what the hand-written
                // list here cost `fps:library-switch` when `Style::Compact` was left off it.
                let cached = crate::ui::containers::modal::style_caches_host(s.style);
                (s.entry.id, cached, s.phase == Phase::Closing)
            })
            .collect();
        // released: held but no longer live
        let mut keep = Vec::new();
        for (id, cached, closing) in self.held.drain(..) {
            match live.iter().find(|(e, _, _)| *e == id) {
                None => crate::ui::popover::surface_released(cached, closing),
                Some((_, _, now_closing)) => {
                    if *now_closing && !closing {
                        crate::ui::popover::surface_closing(cached);
                    }
                    keep.push((id, cached, *now_closing));
                }
            }
        }
        for (id, cached, closing) in live {
            if !keep.iter().any(|(e, _, _)| *e == id) {
                crate::ui::popover::surface_held(cached);
                if closing {
                    crate::ui::popover::surface_closing(cached);
                }
                keep.push((id, cached, closing));
            }
        }
        self.held = keep;
    }
}

/// **Hand back every counter this bridge is still holding.**
///
/// `held` is not a cache — it is an OWNERSHIP list. Each entry means this bridge has called
/// `popover::surface_held` and owes the matching `surface_released`, and those counters
/// (`OPEN_COUNT`, `HOST_USERS`) are process-globals that decide whether the tab bar draws its
/// glass and whether the page under a panel stays frozen. Dropping the bridge without paying that
/// debt leaves the app-wide count permanently above zero.
///
/// In the shipped app this never runs: one `Bridge` is built at boot and lives as long as the
/// process. It exists for the HOST SUITE, where every test builds its own bridge, opens a surface
/// and drops both at the end of the function — and it was a real, diagnosed leak rather than a
/// precaution. `app/bridge.rs`'s own tests left `OPEN_COUNT` above zero, and the failure surfaced
/// three modules away as `ui::popover`'s `dismiss_fades_out_over_frames_while_close_hides_at_once`
/// and `the_open_count_survives_re_opens_redundant_closes_and_overlap` asserting `!any_open()` and
/// finding somebody else's surface still counted. Both pass alone and fail in a full run, which is
/// the signature of cross-test global pollution and reads exactly like flakiness.
///
/// `testlock::serial()` cannot fix that and is not the answer here: the lock stops two tests
/// INTERLEAVING, and this is a leak that outlives the guard. The counter has to be given back by
/// whoever took it, which is this type.
impl Drop for Bridge {
    fn drop(&mut self) {
        self.session_adapter.cancel_all();
        for (_, cached, closing) in self.held.drain(..) {
            crate::ui::popover::surface_released(cached, closing);
        }
    }
}

impl Bridge {
    /// Synchronous application boundary for callers whose answer is consumed in the same turn.
    /// This borrows the one store owned by this Bridge.
    pub(crate) fn browse_run(&mut self, cmd: crate::stores::browse::BrowseCmd) -> bool {
        self.stores.browse_run(cmd)
    }

    pub(crate) fn browse_discover_pump(&mut self) -> crate::stores::StoreOutcome {
        self.stores.browse_discover_pump()
    }

    pub(crate) fn person_run(&mut self, cmd: crate::stores::person::PersonCmd) -> bool {
        self.stores.person_run(cmd)
    }

    pub(crate) fn person_pump(&mut self) -> bool {
        self.stores.person_pump()
    }

    pub(crate) fn person_view(&self) -> crate::person::PersonView<'_> {
        self.stores.person_view()
    }

    pub(crate) fn viewstate_run(&mut self, cmd: crate::stores::viewstate::ViewStateCmd) -> bool {
        self.stores.viewstate_run(cmd, self.directory.view())
    }

    pub(crate) fn viewstate_pump(&mut self) -> crate::stores::EndpointRefreshSet {
        self.stores.viewstate_pump(self.directory.view())
    }

    pub(crate) fn take_detail_refresh(&self) -> Option<crate::stores::viewstate::DetailRefresh> {
        self.stores.take_detail_refresh()
    }

    /// Search commands snapshot their initial favourite-library ranking from the same retained
    /// directory the screen read when it emitted the command.
    pub(crate) fn search_run(&self, cmd: crate::stores::search::SearchCmd) -> bool {
        crate::stores::search::run_with_directory(cmd, self.directory.view())
    }

    pub(crate) fn browse_directory(&self) -> crate::stores::browse::DirectoryView<'_> {
        self.directory.view()
    }

    /// Refresh only the retained directory at synchronous application boundaries that must make
    /// a routing decision before the next dispatcher frame.
    pub(crate) fn refresh_browse_directory(&mut self) {
        self.stores.browse.borrow_mut().capture_directory(&mut self.directory);
    }

    #[cfg(test)]
    pub(crate) fn seed_registered_browse_for_test(
        &mut self,
        sids: [crate::plex::ServerId; 2],
    ) {
        self.stores.browse.borrow_mut().seed_registered_table_for_test(sids);
        self.refresh_browse_directory();
    }

    pub(crate) fn bind_primary(&mut self, recorded: u32) -> Result<(), &'static str> {
        let resource = crate::plex::client_opt().ok_or("missing controlled primary")?;
        if resource.instance_gen() != recorded || resource.id().raw() != 0 {
            return Err("initial primary binding mismatch");
        }
        if self.recorded_clients.insert(recorded, resource).is_some() {
            return Err("duplicate initial primary binding");
        }
        Ok(())
    }
    pub(crate) fn recorded_client(&self, id: u32) -> Option<&'static crate::plex::Client> {
        self.recorded_clients.get(&id).copied()
    }
    /// Does [`Rig::draw_chrome`] paint the shared top bar for this argument? Pulled out of that
    /// method as its own named predicate — rather than inlined as a `matches!` — for two reasons:
    /// it is DERIVED from `ScreenArg::chrome()` (`AppArg::chrome` in `screens/registry.rs`, which
    /// is `nav::route_wears_tab_bar`'s body in its new home) instead of listing Home/Library/Search
    /// a second time. That one test is the whole of the continuous-chrome rule, and a literal
    /// `Route::Home | Route::Library` guard
    /// here once drifted from it silently when Search became a third bar-wearing route:
    /// `capture_chrome`/`ChromeSnapshot` kept publishing Search's strip and the dispatcher kept
    /// routing its chrome pass to `draw_chrome`, but the guard returned before ever painting the
    /// pills or the chip it published); and it gives a host test something to call directly —
    /// `draw_chrome`'s own body calls into real text measurement with no font loaded on a host
    /// test, so a test cannot drive it end to end and must instead pin the exact decision it makes.
    /// A surface argument answers `Chrome::None` for itself and never reaches this: the two menus
    /// have been surfaces since phase 10, so the page UNDER one is the top page and answers for
    /// itself. `route_wears_tab_bar`'s old `page_of` resolution — "which page is this popover
    /// route over" — has nothing left to resolve and went with the routes in D1.
    fn draws_chrome_for(arg: &AppArg) -> bool {
        use crate::ui::screen::ScreenArg;
        arg.chrome() == crate::ui::machine::Chrome::TabBar
    }
}

impl Bridge {
    /// **Publish this frame's playback session** (spec §2.3) — see `AppViews::session`.
    ///
    /// Called once per iteration by the loop, BEFORE the dispatcher frame and the tree draw, so
    /// every screen in one frame reads one consistent picture of the playback. The copy is skipped
    /// entirely when nothing that reads it is mounted: off the player route the publication is
    /// `PlaybackSession::IDLE` and stays there, which is what keeps a still Home grid free of the
    /// dozen small allocations this otherwise costs at the loop rate.
    /// The video plane's EDGE, from `Player::set_video_plane_bound` and from nowhere else
    /// (spec §16 risk 10). Reaches both the dispatcher's present gate and this rig's own copy.
    pub(crate) fn publish_video_plane(&mut self, bound: bool) {
        self.video_plane = bound;
    }

    pub(crate) fn publish_playback(&mut self, session: &crate::route::PlaybackSession, live: bool) {
        if live {
            self.playback = session.publication();
            self.playback_live = true;
        } else if self.playback_live {
            self.playback = crate::route::PlaybackSession::IDLE;
            self.playback_live = false;
        }
    }
}

impl Rig<AppHost> for Bridge {
    fn draw_chrome(&mut self, arg: &AppArg, _parts: &CxParts<u32>,
        nav: crate::ui::screen::NavPresentation,
        glass: Option<&mut crate::ui::frame::glass::GlassPlan>) {
        if !Self::draws_chrome_for(arg) { return; }
        let Some(glass) = glass else { return };
        let p = crate::ui::Painter::root().alpha(nav.chrome_alpha);
        let chrome = self.chrome.read(self.strip.chip_expand_pos());
        self.strip.draw(chrome.labels, p, glass.tab_band_mut());
        let glass_wanted = crate::ui::widgets::bar_glass_wanted_with(chrome.labels);
        crate::ui::widgets::profile_chip_with(
            p,
            chrome.profile,
            glass_wanted,
            chrome.chip_expand,
            glass.tab_face(),
        );
    }
    /// See [`crate::ui::dispatch::Rig::scrim_chrome_read`] — the account menu's chip lift borrows
    /// the SAME captured profile, labels and unfurl [`Bridge::draw_chrome`] used. The dispatcher
    /// adds this frame's material from `GlassPlan`; neither value crosses through a static.
    fn scrim_chrome_read(&self) -> Option<crate::ui::widgets::ChromeRead<'_>> {
        Some(self.chrome.read(self.strip.chip_expand_pos()))
    }
    fn page_alpha(&self) -> f32 { crate::ui::nav::page_alpha() }
    fn navigation_presentation(&self) -> crate::ui::screen::NavPresentation {
        crate::ui::screen::NavPresentation {
            page_alpha: crate::ui::nav::page_alpha(),
            chrome_alpha: crate::ui::nav::chrome_alpha(),
            view_tab: u32::try_from(crate::ui::nav::view_tab(-1)).ok(),
            blur_amount: crate::ui::nav::blur_amount(),
        }
    }
    fn surface_scope(&mut self) -> Option<crate::ui::popover::host::Live> {
        Some(crate::ui::popover::host::live())
    }
    fn split(&mut self) -> Split<'_, AppHost> {
        Split {
            mounter: &mut self.mounter,
            views: AppViews { auth: self.session.read(),
                hubs: self.hubs.view(),
                listing: self.listing.view(),
                directory: self.directory.view(),
                section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
            },
            measure: &self.measure,
        }
    }
    fn deliver(&mut self, to: MachineId, msg: &AppMsg, parts: &CxParts<u32>, fx: &mut Effects<'_, AppHost>) -> Handled {
        if let AppMsg::Consent(command) = msg {
            if to != MachineId::Consent { return Handled::No; }
            match command {
                ConsentCmd::Record { errors, usage } => {
                    self.consent.record(&mut self.consent_adapter, *errors, *usage);
                }
            }
            return Handled::Yes;
        }
        if let AppMsg::Session(event) = msg {
            use crate::auth::owner::SessionEvent;
            if to != MachineId::Session { return Handled::No; }
            match event {
                SessionEvent::Result(envelope) if !self.session_adapter.admitted(envelope) => return Handled::No,
                SessionEvent::Admission(reply) if !reply.accepted
                    && self.session_adapter.resource_admitted(reply) => return Handled::No,
                SessionEvent::Command(crate::auth::owner::Command::BackAtRoot { .. })
                    if !self.session_adapter.claim_root_press() => return Handled::No,
                _ => {}
            }
            // The owner reads its retained publication, not a borrow through its own mutable
            // field. Every other view still comes from this same Bridge frame boundary.
            let publication = self.session.publication();
            let cx = parts.cx::<AppHost>(AppViews { auth: publication.read(),
                hubs: self.hubs.view(), listing: self.listing.view(), directory: self.directory.view(),
                section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
            }, &self.measure);
            return self.session.step(event, &cx, fx);
        }
        let store = match msg {
            AppMsg::Store(cmd) => cmd.store(),
            AppMsg::StoreWork(work) => work.store(),
            AppMsg::HubsResult(_) => StoreId::Hubs,
            _ => return Handled::No,
        };
        let MachineId::Store(ord) = to else {
            return Handled::No;
        };
        if StoreId::from_ord(ord) != Some(store) {
            crate::log(&format!(
                "stores: a {} event was addressed to store ordinal {} — dropped",
                store.name(),
                ord.0
            ));
            return Handled::No;
        }
        if let Some(io) = &mut self.home_io {
            match msg {
                AppMsg::Store(StoreCmd::Hubs(cmd)) => {
                    io.hubs_with_directory(Some(cmd.clone()), 0.0, self.directory.view()).endpoints.emit(fx);
                    return Handled::Yes;
                }
                AppMsg::Store(StoreCmd::Browse(crate::stores::browse::BrowseCmd::Discovery(result))) => {
                    let mut endpoints = crate::stores::EndpointRefreshSet::default();
                    let outcome = self.stores.browse.borrow_mut()
                        .apply_discovery(result, &io.preferences);
                    endpoints.merge(outcome.endpoints);
                    // Results are ingested after the frame's ordinary publication capture. Make
                    // the retained directory visible to Onboard's Tick in this same dispatcher
                    // turn instead of leaving the newly discovered rows one frame behind.
                    if outcome.changed {
                        self.stores.browse.borrow_mut().capture_directory(&mut self.directory);
                    }
                    endpoints.emit(fx);
                    return Handled::Yes;
                }
                AppMsg::StoreWork(crate::stores::StoreWork::Hubs) => {
                    io.hubs_with_directory(None, parts.tick.dt(), self.directory.view()).endpoints.emit(fx);
                    return Handled::Yes;
                }
                AppMsg::StoreWork(crate::stores::StoreWork::BrowseDiscovery) => {
                    io.discovery_owned(&self.stores);
                    return Handled::Yes;
                }
                _ => {}
            }
        }
        match msg {
            AppMsg::Store(StoreCmd::Person(command)) => {
                self.person_run(command.clone());
                return Handled::Yes;
            }
            AppMsg::Store(StoreCmd::ViewState(command)) => {
                self.viewstate_run(command.clone());
                return Handled::Yes;
            }
            _ => {}
        }
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(), search: self.search.view(), person: self.stores.person_view(), session: &self.playback,
        }, &self.measure);
        match msg {
            AppMsg::Store(StoreCmd::Browse(c)) => {
                self.stores.browse.borrow_mut().step(&StoreEv::Cmd(c.clone()), &cx, fx)
            }
            AppMsg::Store(StoreCmd::Search(c)) => {
                self.search_run(c.clone());
                Handled::Yes
            }
            AppMsg::Store(StoreCmd::Hubs(c)) => {
                crate::stores::hubs::apply_with_directory(c.clone(), self.directory.view())
                    .endpoints.emit(fx);
                Handled::Yes
            }
            AppMsg::Store(cmd) => step_store(cmd, &cx, fx),
            AppMsg::HubsResult(result) => {
                crate::stores::hubs::land_with_directory(result, self.directory.view())
                    .endpoints.emit(fx);
                Handled::Yes
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Hubs) => {
                crate::stores::hubs::tick_with_directory(parts.tick.dt(), self.directory.view())
                    .endpoints.emit(fx);
                Handled::Yes
            }
            AppMsg::StoreWork(crate::stores::StoreWork::BrowseDiscovery) => {
                self.stores.browse.borrow_mut().discover_pump().endpoints.emit(fx);
                Handled::Yes
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Browse) => {
                self.stores.browse.borrow_mut()
                    .step(&crate::stores::StoreEv::Pump { dt: parts.tick.dt() }, &cx, fx)
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Search { dt_us }) => {
                crate::stores::search::pump_with_directory(
                    *dt_us as f32 / 1_000_000.0, self.directory.view());
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn timer(&mut self, _owner: MachineId, _id: TimerId, _parts: &CxParts<u32>, _fx: &mut Effects<'_, AppHost>) {}
    fn app_return(&mut self, _from: MachineId, ret: ReturnState<u32, PageMemory>) {
        self.effect_return = ret;
    }
    fn app_fx(&mut self, from: MachineId, fx: AppFx, _parts: &CxParts<u32>, out: &mut Effects<'_, AppHost>) {
        self.app_effect(from, fx, out);
    }
    fn log(&mut self, line: &str) {
        crate::log(line);
    }
    fn system_keyboard(&mut self, up: bool) {
        #[cfg(test)]
        self.keyboard_calls.push(up);
        #[cfg(not(test))]
        if up { crate::textinput::start(); } else { crate::textinput::stop(); }
    }
    fn adopt_system_keyboard(&mut self) {
        #[cfg(test)]
        { self.keyboard_adoptions += 1; }
        #[cfg(not(test))]
        crate::textinput::adopt();
    }
    /// §3.3 step 9's application half — the render cache's upload step — and it is EMPTY on
    /// purpose while the legacy loop owns the frame. The dispatcher's prepare pass runs from
    /// inside `frame_with_tap`, which the loop calls before its own present decision; the upload
    /// has to happen after that decision and before the draw (it draws, so it needs a presenting
    /// frame's GL scope and the page's `frame_clear` behind it). So the loop calls
    /// `adapters::poster::prepare` itself, in the right window, spending the same one `Budget`
    /// this hook would have been handed. It becomes real when the dispatcher owns the frame.
    fn prepare(&mut self, _b: &mut Budget, _present: &mut Present) {}
    fn ls2_pump(&mut self) {}
    /// §3.3 step 9, every frame, presented or not. Real since phase 9: the argument is the
    /// dispatcher's `Present::video_plane()`, i.e. the Player machine's own bit arriving as
    /// `PresentEvent::VideoPlane`, and `system::opaque_route` only sends a wayland request when
    /// the answer CHANGES — so the loop's own call beside it (`app/run.rs`, from the same bit) is
    /// a `static` read and a return, not a second claim. Delegates to `run::rig_opaque_route`
    /// (D4) rather than naming `crate::system::opaque_route` here directly — see that function's
    /// doc for why.
    fn opaque_route(&mut self, video_plane_bound: bool) {
        super::run::rig_opaque_route(video_plane_bound);
    }
    /// §3.3 step 10, at draw entry. Real since phase 9, and conditional on the same bit: NULLing
    /// the opaque region on a frame with no plane under it would silently retract the claim
    /// `opaque_route` had just made, on every route, for the rest of the process —
    /// `G_OPAQUE_SENT` would still read `full` and never re-send. Delegates to
    /// `run::rig_clear_opaque_region` (D4); the guard stays here.
    fn clear_opaque_region(&mut self) {
        if self.video_plane {
            super::run::rig_clear_opaque_region();
        }
    }
    fn now_us(&self) -> u64 {
        (self.now_us)()
    }
    fn back_at_root(&mut self) {
        self.reqs.push(LoopReq::BackAtRoot);
    }
}

impl Bridge {
    fn app_effect(&mut self, from: MachineId, fx: AppFx, out: &mut Effects<'_, AppHost>) {
        if let Some(io) = &mut self.home_io {
            if let Err(reason) = super::bootstrap::effects::app(&fx) {
                io.failure = Some(reason);
                return;
            }
        }
        match fx {
            AppFx::Session(command) => out.push(Fx::Deliver(MachineId::Session,
                Delivery::Machine(AppMsg::Session(crate::auth::owner::SessionEvent::Command(command))))),
            AppFx::SessionEffect(effect) => self.session_effect(effect, out),
            AppFx::Store(id, cmd) => out.push(Fx::Deliver(MachineId::Store(id.ord()), Delivery::Machine(AppMsg::Store(cmd)))),
            AppFx::StoreWork(work) => out.push(Fx::Deliver(
                MachineId::Store(work.store().ord()), Delivery::Machine(AppMsg::StoreWork(work)))),
            AppFx::Consent(command) => out.push(Fx::Deliver(
                MachineId::Consent, Delivery::Machine(AppMsg::Consent(command)))),
            AppFx::Loop(req) => self.reqs.push(req),
            AppFx::Content(req) => self.content_reqs.push((from, req, self.effect_return.clone())),
            AppFx::Home(req) => self.home_reqs.push((from, req, self.effect_return.clone())),
            AppFx::Library(req) => self.library_reqs.push((from, req, self.effect_return.clone())),
            AppFx::Search(req) => self.search_reqs.push((from, req, self.effect_return.clone())),
            AppFx::Player(req) => self.player_reqs.push(req),
            AppFx::ItemMenu(req) => self.item_menu_reqs.push(req),
        }
    }

    /// Resource execution returns typed deliveries to the existing FIFO. In particular a
    /// commit acknowledgement never recursively steps the owner outside drain/carry budgets.
    fn session_effect(&mut self, effect: crate::auth::owner::SessionFx, out: &mut Effects<'_, AppHost>) {
        use crate::auth::owner::{SessionEvent, SessionFx, AdmissionReply};
        use crate::ui::machine::{Addr, RequestId};
        if let Some(io) = &mut self.home_io {
            if matches!(effect, SessionFx::Capture { .. } | SessionFx::Work { .. }
                | SessionFx::Coordinator(_) | SessionFx::Erase { .. }) {
                io.failure = Some("unsupported controlled Home Session operation");
                return;
            }
        }
        let mut deliver = |event| out.push(Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(event))));
        match effect {
            SessionFx::Commit { req, epoch, arrival, plan } => {
                if let Some(permit) = self.session.commit_permit(req, epoch, arrival) {
                    let reply = self.session_adapter.commit(permit, &plan);
                    deliver(SessionEvent::Commit(reply));
                    // The live writer is synchronous, so its real durability verdict is already
                    // known. Deliver it as a typed completion on the ordinary effect path; the
                    // owner fences it before it may stand as saved-login evidence. A verdict that
                    // the owner has already superseded is dropped by that fence, not here.
                    if let Some(completion) = self.session_adapter.take_live_completion() {
                        deliver(SessionEvent::Persistence(completion));
                    }
                }
            }
            SessionFx::Pump => deliver(SessionEvent::Pump),
            SessionFx::Acknowledge(receipts) => {
                self.session_adapter.acknowledge(&receipts);
            }
            SessionFx::Retire { req } => self.session_adapter.cancel(RequestId(req)),
            SessionFx::Cancel { requests, .. } => {
                for req in requests { self.session_adapter.cancel(RequestId(req)); }
            }
            SessionFx::Capture { req, epoch, request } => {
                if self.session.read_is_current(req, epoch, request) {
                    deliver(SessionEvent::Read(self.session_adapter.capture(req, epoch, request)));
                }
            }
            SessionFx::Work { req, key, admission, input } => {
                if self.session.work_is_current(req, key, admission) {
                    let reply = self.session_adapter.start_work(RequestId(req), key, admission, input)
                        .err().unwrap_or(AdmissionReply { addr: Addr { to: MachineId::Session, req: RequestId(req) },
                            key, correlation: admission, accepted: true });
                    deliver(SessionEvent::Admission(reply));
                }
            }
            SessionFx::PublishProfile(publication) => {
                if self.session.publication_is_current(&publication) {
                    self.session_adapter.publish_profile(publication);
                    if let Some(io) = &mut self.home_io {
                        io.profile = self.session_adapter.profile_resource_view().expect("controlled publisher");
                    }
                }
            }
            SessionFx::Ready { epoch, scope, server, token, install } => {
                if self.session.ready_is_current(epoch, scope) {
                    self.session_ready = Some((epoch, scope, server, token, install));
                }
            }
            // Erase is ordered work, not a stale asynchronous completion: a subsequent login
            // may already have advanced the epoch, but cannot skip deleting old credentials.
            SessionFx::Erase { epoch, all_local, .. } => {
                self.session_ready = None;
                let leftovers = self.session_adapter.erase(all_local);
                deliver(SessionEvent::Erased { epoch, leftovers });
            }
            SessionFx::Coordinator(action) => {
                if matches!(action, crate::auth::owner::CoordinatorAction::LocalDataErased) {
                    self.reqs.push(LoopReq::LocalDataErased);
                }
                if matches!(action, crate::auth::owner::CoordinatorAction::CloseTelemetry) {
                    self.consent.forget(&mut self.consent_adapter);
                }
                self.session_adapter.coordinator(action);
            }
            SessionFx::RestartReply { to, accepted } => out.push(Fx::Deliver(
                MachineId::Instance(InstanceId(to.instance)), Delivery::Screen(ScreenEvent::Async(
                    RequestId(to.correlation), AppMsg::RestartReply { correlation: to.correlation, accepted })))),
            SessionFx::SelectionReply { to, accepted, flow_epoch } => out.push(Fx::Deliver(
                MachineId::Instance(InstanceId(to.instance)), Delivery::Screen(ScreenEvent::Async(
                    RequestId(to.correlation), AppMsg::SelectionReply { correlation: to.correlation, accepted, flow_epoch })))),
            SessionFx::BackReply { to, resumed } => {
                self.session_adapter.finish_back(resumed);
                out.push(Fx::Deliver(MachineId::Instance(InstanceId(to.instance)),
                    Delivery::Screen(ScreenEvent::Async(RequestId(to.correlation),
                        AppMsg::BackReply { correlation: to.correlation, resumed }))));
            }
        }
    }
}

fn step_store(cmd: &StoreCmd, cx: &Cx<'_, AppHost>, fx: &mut Effects<'_, AppHost>) -> Handled {
    use crate::stores::metadata;
    match cmd {
        StoreCmd::Browse(_) => unreachable!("Browse is stepped by Bridge's owned store"),
        StoreCmd::Hubs(_) => unreachable!("Hubs is stepped by Bridge with its Browse directory"),
        StoreCmd::Metadata(c) => metadata::MetadataStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
        StoreCmd::Search(_) => unreachable!("Search is stepped by Bridge with its Browse directory"),
        StoreCmd::Person(_) => unreachable!("Person is stepped by Bridge's owned store"),
        StoreCmd::ViewState(_) => unreachable!("ViewState is stepped by Bridge with its Browse owner"),
    }
}

// ---------------------------------------------------------------------------------------------
// the frame, and the loop's questions
// ---------------------------------------------------------------------------------------------

/// The dispatcher's frame, once per loop iteration: the pending navigation's own commit (at
/// [`PageDip`](crate::ui::containers::transition::PageDip)'s floor), the stores' notices, then
/// the ten steps WITHOUT the draw (the loop draws at its own slot, on its own gate). Returns the
/// top page's heartbeat word.
///
/// **It takes no route.** It used to take the loop's `Route` and turn it into a `Root` on the first
/// frame or a `Replace` CUT when it moved — `sync_page`, the mirror D1 deleted. A navigation is
/// asked for at the press that wants it now, so there is nothing per-frame left to reconcile.
#[cfg(test)]
pub(crate) fn frame(
    d: &mut Dispatcher<AppHost>, rig: &mut Bridge,
    tick: Tick, inputs: Vec<InputEvent<u32>>,
) -> (&'static str, FrameReport) {
    frame_with_tap(d, rig, tick, inputs, &mut NoTap)
}

pub(crate) fn frame_with_tap(
    d: &mut Dispatcher<AppHost>,
    rig: &mut Bridge,
    tick: Tick,
    inputs: Vec<InputEvent<u32>>,
    tap: &mut dyn crate::ui::dispatch::Tap<AppHost>,
) -> (&'static str, FrameReport) {
    if matches!(d.top_arg(), Some(AppArg::Login | AppArg::Profiles)) && rig.session.needs_ready_commit() {
        execute_session_command(d, crate::auth::SessionCmd::TakeReady);
    }
    frame_ingest(d, rig, tick, inputs, Bridge::take_live_results, tap)
}

pub(crate) type AppResults = Vec<(crate::ui::machine::Addr, AppMsg)>;

/// Home's hubs — the one adapter result the dispatcher delivers, and a LANDING SITE exactly like
/// the legacy pumps' mailboxes, so it goes through `ui::landgate`: under a replay the arrival
/// waits for the frame the recording delivered it on (§3.3 step 3). Off a replay, one relaxed
/// atomic load and the same call.
pub(crate) fn take_hubs_results() -> AppResults {
    let mut results =
        crate::ui::landgate::take_all(StoreId::Hubs.ord(), crate::stores::hubs::take_results);
    results.sort_by_key(|result| result.request_id());
    results.into_iter().map(|result| (
        crate::ui::machine::Addr {
            to: MachineId::Store(StoreId::Hubs.ord()),
            req: crate::ui::machine::RequestId(result.request_id()),
        },
        AppMsg::HubsResult(result),
    )).collect()
}

impl Bridge {
    fn take_live_results(&mut self) -> AppResults {
        // Landing sequence within auth is authoritative. Do not sort it by request ID:
        // profile Ready and its late roster can be separated by other requests' progress.
        let mut results: AppResults = self.session_adapter.take_results().into_iter()
            .map(|envelope| (envelope.addr, AppMsg::Session(crate::auth::owner::SessionEvent::Result(envelope))))
            .collect();
        results.extend(take_hubs_results());
        if self.home_io.is_some() {
            if let Some(result) = crate::ui::landgate::take(StoreId::Browse.ord(),
                || self.stores.browse.borrow_mut().take_discovery()) {
                results.push((crate::ui::machine::Addr {
                    to: MachineId::Store(StoreId::Browse.ord()),
                    req: crate::ui::machine::RequestId(result.request_id()),
                }, AppMsg::Store(StoreCmd::Browse(crate::stores::browse::BrowseCmd::Discovery(result)))));
            }
        }
        results
    }
}

/// One dispatcher path for live or supplied adapter results. The supplier runs at ingest, after
/// frame-view capture, and this function never additionally polls a live mailbox. Supplying
/// results alone is not offline replay: boot restoration and request suppression are separate.
///
/// **The trunk every app frame passes through, so it is where the test-only lock rule is
/// ENFORCED** — the rule stated in prose below
/// `route_flips_preserve_content_and_player_origin_entries` and broken from another module
/// anyway. A frame is not a walk of a nav tree: it drains the aggregate's Browse notice plus the
/// remaining stores' compatibility notices, and it pumps every store — and `browse`'s pump ends in
/// `sync_roster`, which calls `browse::reset()` the moment the section table holds a source the
/// live registry does not. Every `browse` fixture in the suite leaves it holding exactly that
/// (`ServerId::UNSET` sources), so an unguarded frame ANYWHERE empties another module's seeded
/// table, on another thread, and fails that module's test instead of this one. `app::mod`'s three
/// heartbeat-word tests did it through `every_surface_word`, wanting nothing but a list of words.
#[allow(clippy::too_many_arguments)]
pub(crate) fn frame_with_results(
    d: &mut Dispatcher<AppHost>,
    rig: &mut Bridge,
    tick: Tick,
    inputs: Vec<InputEvent<u32>>,
    take: impl FnOnce() -> AppResults,
    tap: &mut dyn crate::ui::dispatch::Tap<AppHost>,
) -> (&'static str, FrameReport) {
    frame_ingest(d, rig, tick, inputs, |_| take(), tap)
}

fn frame_ingest(
    d: &mut Dispatcher<AppHost>,
    rig: &mut Bridge,
    tick: Tick,
    inputs: Vec<InputEvent<u32>>,
    take: impl FnOnce(&mut Bridge) -> AppResults,
    tap: &mut dyn crate::ui::dispatch::Tap<AppHost>,
) -> (&'static str, FrameReport) {
    #[cfg(test)]
    crate::testlock::assert_held("the store pump behind an app::bridge frame");
    // Library used to be the only route that pumped Browse. Onboard schedules the roster-only
    // owner work from its Tick, but Tick runs after views are captured; land the roster first so
    // a discovery result and the directory publication are observed in one frame. Controlled
    // execution instead recaptures at its addressed result delivery above, preserving recording.
    if rig.home_io.is_none() {
        let outcome = rig.stores.browse_discover_pump();
        execute_endpoint_outcomes(d, outcome.endpoints);
    }
    let (browse_changed, search_changed) = rig.capture_views(d);
    rig.capture_chrome(d);
    rig.deliver_home_commands(d);
    rig.deliver_library_commands(d);
    let mut search_notified = false;
    let mut browse_notified = false;
    for (id, gen) in rig.stores.take_notices() {
        search_notified |= id == StoreId::Search;
        browse_notified |= id == StoreId::Browse;
        d.store_changed(id.ord(), gen);
    }
    if browse_changed && !browse_notified {
        d.store_changed(StoreId::Browse.ord(), rig.stores.gen(StoreId::Browse));
    }
    // Search can still change through a producer outside the dispatcher queue. Announce that
    // captured change, but do not double-deliver an ordinary queued Search notice.
    if search_changed && !search_notified {
        d.store_changed(StoreId::Search.ord(), crate::stores::gen(StoreId::Search));
    }
    let surface = d.surface_up();
    // A surface's INPUTS are the panel's own damage — the glass cadence's ledger, which asks
    // "did the panel change" rather than "did the page". The MOTION half is no longer stated
    // here: this used to wrap the WHOLE dispatcher frame in `popover::own_motion`, so a PAGE's
    // springs stepped inside it were attributed to the panel and `idle::page_moving` read false
    // for as long as any surface was up — including a DISMISSED one, where `host_refresh`'s
    // `fading_only` term is the only thing that re-takes the snapshot for a page the user is
    // driving again. `ModalStack::tick` and `Dispatcher`'s per-surface step and draw open one
    // scope each (§4.4) now; the page's tick runs in none.
    if surface && !inputs.is_empty() {
        crate::ui::popover::note_own_damage();
    }
    let results = take(rig);
    let session_records: Vec<_> = results.iter().filter_map(|(addr, message)| match message {
        AppMsg::Session(crate::auth::owner::SessionEvent::Result(envelope)) => Some((*addr, envelope.clone())),
        _ => None,
    }).collect();
    rig.session_adapter.validate_supplied(&session_records)
        .expect("Session ingest requires an exactly addressed, admitted transfer batch");
    let report = d.frame_with(rig, tick, inputs, results, tap, false);
    d.prune(&report.unmounted);
    rig.sync_host(d);
    if report.presented {
        // The dispatcher's gate wants a frame: the loop's gate presents it. While a surface is up
        // this bump is the PANEL's — `take_page_damage` subtracts the panel's claims BY COUNT, and
        // a page's own landings reach `ui::idle` from the pumps outside this frame (the poster
        // adapter, `pms::commit`), where nothing claims them.
        let _own = surface.then(crate::ui::idle::OwnScope::open);
        crate::ui::idle::invalidate();
    }
    // **Publish the transition's presentation** for the readers that draw outside a page's own
    // `DrawFrame` (`ui::popover`'s panel and scrim, the profile chip's redraw, the glass track's
    // settled test, the navblur prototype). One writer, immediately after the container's own
    // frame, from the container's own transition — see `ui::nav`'s module doc.
    let tab = d.nav.tabs.stack.pending_dest()
        .and_then(|arg| pill_of_arg(arg, rig.directory.view()));
    crate::ui::nav::publish(d.nav.tabs.stack.page_alpha(), d.nav.tabs.stack.chrome_alpha(), tab);
    // **The heartbeat's `route=` word IS the top page's own name** (§15.2). It used to be
    // `route_word(app.route)` with this line asserting the two agreed every frame; there is one
    // source now, and the `debug_assert_eq!` that guarded the pair is gone with the pair.
    let word = d.top_screen().map_or("", |s| s.name());
    (word, report)
}

// (`sync_page` stood here — the whole coexistence mechanism, and the reason `Route::` deletion was
// a migration of AUTHORITY rather than a rename. Every frame it read the loop's committed route,
// derived the argument the tree ought to be showing and emitted the `Root`/`Push`/`PopTo` that
// made it so, which is how ~25 bare `app.route = …` writes scattered through the lifecycle, the
// auth landing, `exit_player`, `start_playback`, boot and `LoopReq` each became a container op
// without any of them naming one. The loop asks for the op it wants now — `nav_root`, `nav_push`,
// `nav_pop`, `nav_pop_to` above — and there is no second copy of "which page is on top" for a
// mirror to follow.)

// (`page_node` stood here — "the top page, as a trail node", the read that kept `App.trail` in
// step with the container. Its last caller was the dev content boot, which now compares the top
// ARGUMENT with the identity it is waiting for.)

pub(crate) fn content_probe(d: &Dispatcher<AppHost>, rig: &Bridge) -> String {
    let mut out = page_probe(d, rig);
    // **The profile menu's own fields, from the SURFACE** (phase 10). They used to hang off
    // `focusprobe::Screen::Account`, which named the host page and then recursed into its fields;
    // the host is the top PAGE now and the line already names it as `route=`, so the panel's two
    // fields simply ride on `content` — exactly as the player's four panels and the Library menu
    // do. The spellings are unchanged (`acct=`/`asel=`) so a reader's grammar is, but their
    // POSITION on the line moved (they follow the page's fields rather than being nested under an
    // `over=`), which is one of the reasons the committed replay fixtures are re-recorded.
    if let Some(menu) = account_menu(d) {
        use std::fmt::Write;
        let _ = write!(out, " acct=1 asel={}", menu.sel());
    }
    // …and the item context menu's, for the same reason and by the same route (phase 10). They
    // used to hang off `focusprobe::Screen::ItemMenu`, which named the host with ` over=<word>`
    // and then recursed into that screen's own fields — five hosts, five different `route=itemmenu`
    // field sets. The host is the top PAGE now and the line names it as `route=`, so ` over=` is
    // gone and the panel's three fields simply follow the page's, exactly as the player's four
    // panels and the Library menu already do. The SPELLINGS are unchanged (`imenu=`/`isel=`/
    // `imsid=`) so a reader's grammar is; their POSITION on the line moved, which is one of the
    // reasons the committed replay fixtures are re-recorded.
    if let Some(menu) = item_menu(d) {
        use std::fmt::Write;
        let sid = menu.sid();
        let _ = write!(out, " imenu=1 isel={} imsid=", menu.sel());
        // The same `-` for an unset server the probe's own `push_sid` writes, so a line taken
        // before this phase and one taken after are comparable field for field.
        if sid.is_set() {
            let _ = write!(out, "{}", sid.raw());
        } else {
            out.push('-');
        }
    }
    out
}

fn page_probe(d: &Dispatcher<AppHost>, rig: &Bridge) -> String {
    use std::fmt::Write;
    let Some(page) = d.nav.top_page() else { return String::new() };
    // **The player's panels and its countdown** — `focusprobe::push_player`'s other half. Every
    // one of these was a module global until restructure phase 9 and is a mounted instance's own
    // state now, so it is read here, where the container is in scope, exactly as Home's, the
    // Library's, Search's and the content pages' fields are.
    //
    // The panel fields keep the spellings the characterization line has always used
    // (`menu=`/`msel=`/`taudio=`/`tsub=`/`info=`/`isel=`/`infolast=`/`chap=`/`csel=`/`more=`/
    // `osel=`), because a recording taken before this phase and one taken after must be
    // comparable. `haschap=` stays in `push_player`: it is a fact about the ITEM, not the panel.
    if matches!(page.arg, AppArg::Player) {
        use crate::screens::player::overlay::{OverlayKind, Panel};
        let player = page.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::player::PlayerScreen>());
        let surface = d.nav.modals.surfaces.iter().rev()
            .filter(|s| matches!(s.entry.arg, AppArg::PlayerOverlay(_)))
            .find_map(|s| s.entry.inst.as_ref())
            .and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::player::overlay::PlayerOverlayScreen>());
        let kind = surface.map(|s| s.kind());
        let is = |want: fn(&OverlayKind) -> bool| kind.as_ref().is_some_and(want);
        let sel = surface.map_or(0, |s| s.sel());
        let mut out = format!(" upnext={}", u8::from(player.is_some_and(|p| p.up_next.armed())));
        let (taudio, tsub) = match surface.map(|s| s.panel()) {
            Some(Panel::Tracks(t)) => (t.active_audio(), t.active_sub()),
            _ => (0, -1),
        };
        let infolast = matches!(surface.map(|s| s.panel()), Some(Panel::Info(p)) if p.at_last());
        let menu = is(|k| matches!(k, OverlayKind::Tracks { .. }));
        let info = is(|k| matches!(k, OverlayKind::Info));
        let chap = is(|k| matches!(k, OverlayKind::Chapters));
        let more = is(|k| matches!(k, OverlayKind::More { .. }));
        let _ = write!(
            out,
            " menu={} msel={} taudio={taudio} tsub={tsub} info={} isel={} infolast={} chap={} csel={} more={} osel={}",
            u8::from(menu), if menu { sel } else { 0 },
            u8::from(info), if info { sel } else { 0 }, u8::from(infolast),
            u8::from(chap), if chap { sel } else { 0 },
            u8::from(more), if more { sel } else { 0 },
        );
        return out;
    }
    if matches!(page.arg, AppArg::Home) {
        let Some(home) = page.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::home::HomeScreen>()) else { return String::new() };
        let focus = Bridge::home_focus(d);
        let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: focus , ..Default::default() }, owner: InputOwner::Entry(page.id) };
        let cx = parts.cx::<AppHost>(AppViews { auth: rig.session.read(),
            hubs: rig.hubs.view(),
            listing: rig.listing.view(),
            directory: rig.directory.view(),
            section_hubs: rig.section_hubs.view(), search: rig.search.view(), person: rig.stores.person_view(), session: &rig.playback,
        }, &rig.measure);
        let position = home.grid_position::<AppHost>(focus, &cx);
        let (row, col) = position.map(|(r, c)| (r as i64, c as i64)).unwrap_or((-1, -1));
        let hf = match rig.chrome.focus(focus) {
            crate::ui::widgets::TopFocus::Chip => -1,
            crate::ui::widgets::TopFocus::Pill(i) => -(i as i64 + 2),
            crate::ui::widgets::TopFocus::Away => focus.filter(|key| key.elem < 2).map_or(-1, |key| key.elem as i64),
        };
        let grid = home.snap_target() >= 0.5;
        let mut out = format!(" snapt={} snapp={} hf={hf} row={row} col={col}", grid as u8,
            (!<crate::screens::home::HomeScreen as Screen<AppHost>>::strip_reachable(home)) as u8);
        crate::focusprobe::push_item(&mut out, if grid { home.focused_item::<AppHost>(focus, &cx) } else { home.hero_item::<AppHost>(&cx) });
        return out;
    }
    if matches!(page.arg, AppArg::Library) {
        let Some(library) = page.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::library::LibraryScreen>()) else { return String::new() };
        let focus = d.input.engine.current(InputOwner::Entry(page.id));
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: focus , ..Default::default() }, owner: InputOwner::Entry(page.id) };
        let cx = parts.cx::<AppHost>(AppViews { auth: rig.session.read(), hubs: rig.hubs.view(), listing: rig.listing.view(),
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view(), person: rig.stores.person_view(), session: &rig.playback }, &rig.measure);
        let menu = d.top_surface_name() == Some("library_menu");
        let pill = if menu { -1 } else { match rig.chrome.focus(focus) {
            crate::ui::widgets::TopFocus::Pill(index) => index as i32, _ => -1,
        }};
        let item = library.focused_item(focus, &cx);
        let (region, row, col, x, y) = library.probe_viewport(focus);
        let mut out = format!(" pill={pill} card={} menu={} region={region} row={row} col={col} viewport_x={x:.3} viewport_y={y:.3} sid=", u8::from(item.is_some() && !menu), u8::from(menu));
        if let Some(item) = item {
            let _ = write!(out, "{} rk=", item.sid.raw());
            crate::focusprobe::push_rk(&mut out, &item.rk);
        } else { out.push_str("- rk=-"); }
        return out;
    }
    if matches!(page.arg, AppArg::Search) {
        let Some(search) = page.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::search::SearchScreen>()) else { return String::new() };
        let focus = d.input.engine.current(InputOwner::Entry(page.id));
        // The shared bar decides Chip/Strip first — those elems live outside this screen's own
        // group space (`SearchScreen::probe`'s doc), so asking the screen about them would just
        // be asking it about a `FocusKey` it has no group for and getting the same default back.
        let top = rig.chrome.focus(focus);
        let (zone, row, col, recent, card) = match top {
            crate::ui::widgets::TopFocus::Chip => ("Chip", -1i64, -1i64, -1i64, false),
            crate::ui::widgets::TopFocus::Pill(_) => ("Strip", -1i64, -1i64, -1i64, false),
            crate::ui::widgets::TopFocus::Away => search.probe(focus),
        };
        let pill = match top { crate::ui::widgets::TopFocus::Pill(i) => i as i64, _ => -1 };
        return format!(" zone={zone} editing={} row={row} col={col} recent={recent} pill={pill} card={} below={} clear={}",
            u8::from(search.is_editing()), u8::from(card), search.probe_below(), search.recents_shown());
    }
    if !matches!(page.arg, AppArg::Content(_)) { return String::new(); }
    let Some(InputOwner::Entry(owner)) = d.nav.input_owner() else { return String::new() };
    let Some(instance) = d.nav.entry(owner).and_then(|e| e.inst.as_ref()) else { return String::new() };
    let focus = d.focus();
    let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 },
        press: crate::ui::machine::PressRead { scale: 1.0, is_long: false },
        focus: crate::ui::machine::FocusRead { current: focus , ..Default::default() }, owner: InputOwner::Entry(owner) };
    let cx = parts.cx::<AppHost>(AppViews { auth: rig.session.read(),
        hubs: rig.hubs.view(),
        listing: rig.listing.view(),
        directory: rig.directory.view(),
        section_hubs: rig.section_hubs.view(), search: rig.search.view(), person: rig.stores.person_view(), session: &rig.playback,
    }, &rig.measure);
    let mut groups = Vec::new();
    instance.screen.groups(&cx, &mut groups);
    let group = focus.and_then(|f| instance.screen.group_of(&f.elem, &cx));
    let card = group.and_then(|g| groups.iter().find(|x| x.id == g))
        .is_some_and(|g| g.elem == crate::ui::screen::ElemKind::Card);
    let mut out = String::new();
    match &page.arg {
        AppArg::Content(ContentArg::Detail { sid, rk }) => {
            let mut sp = match instance.screen.memory_at(focus) {
                PageMemory::Detail(s) => s.spot, _ => Default::default(),
            };
            for (_, elem) in d.return_state().remembered {
                if let PageMemory::Detail(saved) = instance.screen.memory_at(Some(FocusKey { entry: owner, elem })) {
                    let saved = saved.spot;
                    if let Some(col) = sp.saved_col.get_mut(saved.section.max(0) as usize) { *col = saved.col; }
                }
            }
            let _ = write!(out, " sec={} col={} eptext={}", sp.section, sp.col, sp.ep_text as u8);
            match sp.season { Some(n) => { let _ = write!(out, " season={n}"); }, None => out.push_str(" season=-") }
            out.push_str(" saved=");
            for (i, col) in sp.saved_col.iter().enumerate() {
                if i > 0 { out.push(','); }
                let _ = write!(out, "{col}");
            }
            let show = crate::metadata::current().is_some_and(|m| m.sid == *sid && m.rk == *rk && m.kind == "show");
            // `alt=` is now a question about the TREE — the *Also available* picker is a surface,
            // so "is it up" is the container's answer and not a module flag's.
            let alt = surface_up(d, |arg| matches!(arg, AppArg::AltSources(_)));
            // **`tracks=`/`tpage=` are the DETAIL page's fields**, and they used to be written on
            // the PLAYER line (`focusprobe::push_player`) beside the four playback overlays. No
            // Detail panel can be up on the player route, so every player recording carried a
            // constant `tracks=0 tpage=1` and the page that actually opens the sheet recorded
            // nothing — a paging press, which moves that number and nothing else in the app, was
            // invisible to the recorder (§5.3). `tpage` is the surface's own cursor, read off the
            // instance the way `alt=` reads its phase: one producer, the container.
            let (tracks, tpage) = tracks_probe(d);
            let _ = write!(out, " tracks={} tpage={}", tracks as u8, tpage);
            let _ = write!(out, " card={} alt={} show={} sid={} rk=", card as u8, alt as u8, show as u8, sid.raw());
            crate::focusprobe::push_rk(&mut out, rk);
            out.push_str(" ep=");
            let episode = instance.screen.as_any()
                .and_then(|s| s.downcast_ref::<crate::screens::detail::DetailScreen>())
                .and_then(|s| s.focused_episode(focus));
            if let Some((rk, mark)) = episode {
                crate::focusprobe::push_rk(&mut out, &rk);
                out.push_str(match mark {
                    crate::ui::widgets::PosterMark::None => " epwatched=no",
                    crate::ui::widgets::PosterMark::InProgress => " epwatched=part",
                    crate::ui::widgets::PosterMark::Watched => " epwatched=yes",
                });
            } else { out.push_str("- epwatched=-"); }
        }
        AppArg::Content(ContentArg::Person { .. }) => {
            let filmography = d.nav.is_surface(owner)
                && d.nav.entry(owner).is_some_and(|e| matches!(e.arg, AppArg::Content(ContentArg::Filmography { .. })));
            let _ = write!(out, " card={} filmography={}", card as u8, filmography as u8);
            let item = instance.screen.as_any()
                .and_then(|s| s.downcast_ref::<crate::screens::person::PersonScreen>())
                .and_then(|s| s.focused_item(focus, &cx));
            if let Some(item) = item {
                let _ = write!(out, " sid={} rk=", item.sid.raw());
                crate::focusprobe::push_rk(&mut out, &item.rk);
            } else { out.push_str(" sid=- rk=-"); }
        }
        _ => {}
    }
    let _ = write!(out, " group={} elem={}", group.map(|g| g.0 as i64).unwrap_or(-1),
        focus.map(|f| f.elem as i64).unwrap_or(-1));
    out
}

/// **Open one of the player's four panels on the PLAYER PAGE's own `ModalStack`** (§6.2).
///
/// Idempotent per KIND while that kind is up, for `open_settings`'s reason: a second press on the
/// disc that opened it must not stack a second copy. Opening a DIFFERENT kind over an open one is
/// allowed and is what the transport's own tab row does — the container's `input_owner()` gives
/// the topmost surface the keys, so the stack is the answer to "which panel owns the frame".
///
/// The style is `PlayerPanel { survives_failure }`, whose host policy is `(Live, Live)`: what is
/// behind these panels is a hardware video plane GL cannot read back, so there is no host snapshot
/// to take and nothing to freeze (`ui/popover.rs`'s `HostPolicy::Live` says exactly this about the
/// player route). `survives_failure` is the `…` popover's alone — see `OverlayKind`.
pub(crate) fn open_player_overlay(
    ps: &crate::route::PlaybackSession,
    d: &mut Dispatcher<AppHost>,
    kind: crate::screens::player::overlay::OverlayKind,
) {
    // Already up? Then this press is a re-ADDRESS of the entry that exists — the Audio disc
    // pressed while the Subtitles tab is showing — and never a second surface of the same kind.
    if player_overlay_kind(d).is_some_and(|up| up.slot() == kind.slot()) {
        if let Some(surface) = player_overlay_mut(d) {
            surface.retarget(ps, kind);
        }
        return;
    }
    d.nav.next_style = Style::PlayerPanel { survives_failure: kind.survives_failure() };
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::PlayerOverlay(
            crate::screens::player::overlay::PlayerOverlayArg { kind },
        )),
    );
}

/// Which player panel owns input right now, if one does.
pub(crate) fn player_overlay_kind(
    d: &Dispatcher<AppHost>,
) -> Option<crate::screens::player::overlay::OverlayKind> {
    let InputOwner::Entry(id) = d.nav.input_owner()? else { return None };
    match &d.nav.entry(id)?.arg {
        AppArg::PlayerOverlay(arg) => Some(arg.kind),
        _ => None,
    }
}

/// Is ANY player panel up (including one still fading out)? The successor of
/// `matches!(route, Route::Player { overlay }) if overlay != Overlay::None`.
///
/// **Test-only since restructure phase 12** (PX-PLAYER), and the reason is the point rather than
/// a tidy-up: its last production caller was `key_player_failed`'s BACK arm, which asked this in
/// order to close a panel before leaving a failed playback. A panel is a SURFACE and answers its
/// own BACK before the page under it is offered the key at all, so the question no longer has a
/// caller that can act on the answer — but it is still exactly what a test asserting the
/// container's own bookkeeping wants to ask. `dismiss_player_overlays` below is the ritual half
/// and is very much alive.
#[cfg(test)]
pub(crate) fn player_overlay_up(d: &Dispatcher<AppHost>) -> bool {
    d.nav
        .modals
        .surfaces
        .iter()
        .any(|s| matches!(s.entry.arg, AppArg::PlayerOverlay(_)))
}

/// Dismiss every player panel — the exit ritual's half of `close_player_overlays`.
pub(crate) fn dismiss_player_overlays(d: &mut Dispatcher<AppHost>) {
    let ids: Vec<EntryId> = d
        .nav
        .modals
        .surfaces
        .iter()
        .filter(|s| matches!(s.entry.arg, AppArg::PlayerOverlay(_)))
        .map(|s| s.entry.id)
        .collect();
    for id in ids {
        d.request(MachineId::Nav, NavOp::Dismiss(id));
    }
}

/// A live panel's own state, for the dev triggers that drive one by hand
/// (`plxnative-menupick`) and for the focus probe's `sel=`.
pub(crate) fn player_overlay_mut(
    d: &mut Dispatcher<AppHost>,
) -> Option<&mut crate::screens::player::overlay::PlayerOverlayScreen> {
    let InputOwner::Entry(id) = d.nav.input_owner()? else { return None };
    d.nav
        .entry_mut(id)?
        .inst
        .as_mut()?
        .screen
        .as_any_mut()?
        .downcast_mut::<crate::screens::player::overlay::PlayerOverlayScreen>()
}

/// **Open one of the Detail page's own panels on the container tree** (§6.2's page-owned panels).
///
/// Idempotent per PANEL while that panel is up, for `open_settings`'s reason: a second press on
/// the control that opened it must not stack a second copy. `host` is the Detail instance the
/// panel reports back to, and `sid`/`rk` the copy the page is standing on — the picker's tick, and
/// the pair its addressed store is read with.
///
/// Each panel's STYLE is its own shape and not a preference, and it is declared beside the panel
/// (`ContentPanel::surface`) rather than here: a read-only sheet with no control to miss answers a
/// click beside it with nothing (`Style::Alert`), an anchored menu whose OK navigates is the
/// Library's Sort/Filter chip (`Style::Compact`). All of them are `HostRender::Cached`, which is
/// what the detail page under one has needed since the host snapshot landed (`fps:page-panel`).
pub(crate) fn open_content_panel(
    d: &mut Dispatcher<AppHost>,
    host: InstanceId,
    subject: Option<(crate::plex::ServerId, &str)>,
    panel: crate::screens::registry::ContentPanel,
) {
    // WHICH surface a panel is — its style and its argument — is the registry's
    // (`ContentPanel::surface`), so a new page-owned panel is declared where the screen is. What is
    // this function's is the PRESENTING: refuse a second copy of one already up, hand the
    // container the style through its one-shot handshake, and request the op.
    let Some((style, arg)) = panel.surface(host, subject) else { return };
    let id = crate::ui::screen::ScreenArg::id(&arg);
    if surface_up(d, move |up| crate::ui::screen::ScreenArg::id(up) == id) {
        return;
    }
    d.nav.next_style = style;
    d.request(MachineId::Nav, NavOp::Present(arg));
}

/// A rect as the bit-preserving anchor an argument carries — `LibraryMenuArg::anchor`'s rule, so
/// a canonical argument needs no float equality. `None` (a host with nothing focused, or the
/// headless trigger) resolves to the panel's own centred fallback HERE, once, rather than every
/// frame inside the screen.
fn anchor_bits(rect: Option<crate::ui::Rect>) -> [u32; 4] {
    let r = rect.unwrap_or_else(crate::screens::item_menu::fallback_anchor);
    [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()]
}

/// The argument for a hold on a CARD — a home shelf, the Library grid, a Search result shelf, a
/// person's filmography, the detail page's Related shelf. All five were `MenuHost` variants and
/// all five are the same arm: the row rides in the argument instead of being looked up in the hub
/// catalog, which only Home's cards are ever in.
pub(crate) fn card_menu_arg(
    item: &crate::pms::PmsMovie,
    from_deck: bool,
    from_home: bool,
    host: EntryId,
    focus: Option<FocusKey<u32>>,
    rect: Option<crate::ui::Rect>,
) -> crate::screens::registry::ItemMenuArg {
    crate::screens::registry::ItemMenuArg {
        sid: item.sid, // the ROW's server, not the current one
        rk: item.rk.clone(),
        kind: ItemMenuKind::Card { row: Box::new(item.clone()), from_deck },
        host,
        focus,
        anchor: anchor_bits(rect),
        loaded_episode: false,
        from_home,
    }
}

/// …and for the detail page's two strips, whose item is a child of the show the page has loaded
/// rather than a catalog row: no row to carry, and `loaded_episode` set for the filmstrip so the
/// dispatch routes Play from Start and the scrobble through that page's own episode path.
fn strip_menu_arg(
    sid: crate::plex::ServerId,
    rk: &str,
    kind: ItemMenuKind,
    host: EntryId,
    focus: Option<FocusKey<u32>>,
    rect: Option<crate::ui::Rect>,
) -> crate::screens::registry::ItemMenuArg {
    crate::screens::registry::ItemMenuArg {
        sid,
        rk: rk.to_string(),
        loaded_episode: matches!(kind, ItemMenuKind::Episode { .. }),
        kind,
        host,
        focus,
        anchor: anchor_bits(rect),
        from_home: false,
    }
}

/// **Present the item context menu** over the page the hold happened on (idempotent while one is
/// up, for `open_settings`'s reason: a second hold must not stack a second panel).
///
/// The style is `Compact`, whose host policy is `(Frozen, Cached)`: the page under the panel is
/// drawn once into the shared snapshot and served from it, and its focus springs do not advance
/// while the menu owns input. That was `Popover::caching_host()` plus the loop's own
/// `Route::ItemMenu` draw/update arms — one policy stated in three places — and it is the
/// container's single answer now.
pub(crate) fn open_item_menu(d: &mut Dispatcher<AppHost>, arg: crate::screens::registry::ItemMenuArg) {
    if surface_up(d, |a| matches!(a, AppArg::ItemMenu(_))) {
        return;
    }
    d.nav.next_style = Style::Compact;
    d.request(MachineId::Nav, NavOp::Present(AppArg::ItemMenu(arg)));
}

/// Is the item menu up (any phase)?
pub(crate) fn item_menu_up(d: &Dispatcher<AppHost>) -> bool {
    surface_up(d, |a| matches!(a, AppArg::ItemMenu(_)))
}

/// The item menu's own instance — its cursor and its captured opener, read where the container is
/// in scope, exactly as the player's panels are.
pub(crate) fn item_menu(d: &Dispatcher<AppHost>) -> Option<&crate::screens::item_menu::ItemMenuScreen> {
    d.nav
        .modals
        .surfaces
        .iter()
        .find(|s| matches!(s.entry.arg, AppArg::ItemMenu(_)))?
        .entry
        .inst
        .as_ref()?
        .screen
        .as_any()?
        .downcast_ref::<crate::screens::item_menu::ItemMenuScreen>()
}

/// **Present the profile menu** over whichever bar-wearing page is on top (idempotent while it is
/// up, for `open_settings`'s reason: a second press on the chip must not stack a second copy).
///
/// The style is `Sheet`, whose host policy is `(Frozen, Cached)`: the page under this panel is
/// drawn once into the shared snapshot and served from it, and neither its focus springs nor its
/// hero drift advance while the menu owns input. That was `host_page_updates`'s `Route::Account`
/// arm and `Popover::caching_host()` — one policy stated in two places — and it is the container's
/// single answer now.
pub(crate) fn open_account_menu(d: &mut Dispatcher<AppHost>) {
    if surface_up(d, |a| matches!(a, AppArg::AccountMenu)) {
        return;
    }
    d.nav.next_style = Style::Sheet;
    d.request(MachineId::Nav, NavOp::Present(AppArg::AccountMenu));
}

/// Is the profile menu up (any phase)?
pub(crate) fn account_menu_up(d: &Dispatcher<AppHost>) -> bool {
    surface_up(d, |a| matches!(a, AppArg::AccountMenu))
}

/// The profile menu's own cursor, for the focus probe — the surface's state, read where the
/// container is in scope, exactly as the player's panels are.
pub(crate) fn account_menu(
    d: &Dispatcher<AppHost>,
) -> Option<&crate::screens::account_menu::AccountMenuScreen> {
    d.nav
        .modals
        .surfaces
        .iter()
        .find(|s| matches!(s.entry.arg, AppArg::AccountMenu))?
        .entry
        .inst
        .as_ref()?
        .screen
        .as_any()?
        .downcast_ref::<crate::screens::account_menu::AccountMenuScreen>()
}

/// Present the Settings surface at its root (idempotent while it is up) — the account menu's
/// `Settings` row, by key and by click.
// ---------------------------------------------------------------------------------------------
// the loop's navigations, as container ops
// ---------------------------------------------------------------------------------------------

/// **Go to a PEER — Home, the Library or Search** (spec §3.4 `NavOp::Root`).
///
/// The three strip pills are peers of one another and all stand on Home, so arriving at one
/// unwinds whatever was above the root: `Root` is a `PopTo(root)` when the root is already this
/// page and a cover-and-mint otherwise. That is exactly what `Trail::reset()` + `Trail::push()`
/// spelled by hand, and what the container did NOT do before D1 — `sync_page` pushed for anything
/// that was not a boot gate, so `Library → Search` left the container three deep while the trail
/// said two, and BACK's destination came off the container. The trail's own doc named that
/// divergence as the bug ("BACK off a result eventually lands on the browse grid for one user and
/// Home for another"); one authority is what settles it.
pub(crate) fn nav_root(d: &mut Dispatcher<AppHost>, arg: AppArg) {
    d.request(MachineId::Nav, NavOp::Root(arg));
}

/// **Put `want` on top, reusing an entry that already holds it.**
///
/// The three-line decision `sync_page` made EVERY FRAME off the loop's route mirror, kept as an
/// explicit operation called at the handful of moments that really mean "land on this page,
/// whatever the history is": the Info card's *Go to Show*, the dev boot targets, and the auth
/// landings. It is a LANDING rather than a navigation, which is why it reuses: pushing blindly
/// would put a second copy of a page the user is already standing on over the first.
pub(crate) fn show_page(d: &mut Dispatcher<AppHost>, want: AppArg) {
    use crate::ui::screen::ScreenArg;
    if d.nav.top_page().map(|e| e.arg.same_instance(&want)).unwrap_or(false) { return; }
    let existing = d.nav.tabs.stack.entries.iter().rev()
        .find(|e| e.arg.same_instance(&want)).map(|e| e.id);
    if let Some(id) = existing {
        nav_pop_to(d, id);
    } else if d.nav.top_page().is_none()
        || matches!(want, AppArg::Login | AppArg::Profiles | AppArg::Onboard | AppArg::Home) {
        nav_root(d, want);
    } else {
        nav_push(d, want);
    }
}

/// **Stack a page** — a detail page, a person page, the player.
pub(crate) fn nav_push(d: &mut Dispatcher<AppHost>, arg: AppArg) {
    d.request(MachineId::Nav, NavOp::Push(arg));
}

/// …with the outgoing page's `ReturnState` supplied rather than read off the engine, for a request
/// an owned screen froze on ITS press frame (`content::freeze_request`'s successor).
pub(crate) fn nav_push_with_return(
    d: &mut Dispatcher<AppHost>, arg: AppArg, ret: ReturnState<u32, PageMemory>,
) {
    d.request_with_return(MachineId::Nav, NavOp::Push(arg), ret);
}

/// **Which PILL of the shared strip a page argument is**, for the pending selection the capsule
/// travels to. `None` for everything that is not a strip destination, which is most of the
/// alphabet — the row shows no pending selection while a detail page is coming up.
///
/// The Library's pill is its TYPE, and the argument carries none (which library the grid shows is
/// the `browse` store's business): the answer is the section the store is pointing at, which
/// `nav_tab`'s `LibraryCmd::Enter` has already aimed.
fn pill_of_arg(arg: &AppArg, directory: crate::stores::browse::DirectoryView<'_>) -> Option<usize> {
    use crate::app::chrome::{pill_of, Pill};
    match arg {
        AppArg::Home => pill_of(directory, Pill::Home),
        AppArg::Search => pill_of(directory, Pill::Search),
        AppArg::Library => {
            let kind = directory.current().map(|i| directory.sections()[i].kind)?;
            pill_of(directory, Pill::Section(kind))
        }
        _ => None,
    }
}

/// **A press on the shared top strip** — the ONE door for its four destinations, so the seed or
/// command each one carries cannot be forgotten at one of the three screens that wear the bar.
///
/// It is `app::nav`'s four `Nav` variants, minus the enum: what each arm did BESIDE the flip is a
/// seed or a queued command that the destination's mount consumes, so it is set at the PRESS and
/// spent when the page comes up — which is where the fade floor used to run it, at alpha 0.
///
/// **`nav_peer`, not `nav_root`**, and the difference is the Library: the three pills are peers
/// that stand on Home, but a Movies→Shows press is not a navigation at all, it is a TELEPORT
/// inside one page (`LibraryCmd::Enter`, the store swap and `restore_view`'s scroll jump). Rooting
/// there would retire the entry and remount the screen, throwing away the viewport memory the
/// teleport exists to restore.
pub(crate) fn nav_tab(
    d: &mut Dispatcher<AppHost>, rig: &mut Bridge, tab: HomeTab,
    focus_pill: Option<crate::app::chrome::Pill>,
    ret: Option<ReturnState<u32, PageMemory>>,
) {
    use crate::app::chrome::Pill;
    let arg = match tab {
        HomeTab::Home => {
            // Keep the pill the user was standing on under focus. An IDENTITY, not an index: a
            // pill can appear or disappear while the dip runs, and `HomeCmd::FocusStrip` is
            // delivered when Home MOUNTS, not now.
            if let Some(pill) = focus_pill.filter(|pill| crate::app::chrome::pill_of(rig.directory.view(), *pill).is_some()) {
                let want = match pill {
                    Pill::Home => HomeTab::Home,
                    Pill::Search => HomeTab::Search,
                    Pill::Section(crate::browse::SecKind::Movie) => HomeTab::Movies,
                    Pill::Section(crate::browse::SecKind::Show) => HomeTab::Shows,
                };
                rig.home_command(HomeCmd::FocusStrip(want));
            }
            AppArg::Home
        }
        HomeTab::Movies => { rig.enter_library(crate::browse::SecKind::Movie); AppArg::Library }
        HomeTab::Shows => { rig.enter_library(crate::browse::SecKind::Show); AppArg::Library }
        HomeTab::Search => AppArg::Search,
    };
    nav_peer(d, arg, ret);
}

/// A peer of Home: root there unless it is already the page on top.
fn nav_peer(d: &mut Dispatcher<AppHost>, arg: AppArg, ret: Option<ReturnState<u32, PageMemory>>) {
    use crate::ui::screen::ScreenArg;
    if d.nav.top_page().map(|e| e.arg.same_instance(&arg)).unwrap_or(false) { return; }
    match ret {
        Some(ret) => d.request_with_return(MachineId::Nav, NavOp::Root(arg), ret),
        None => nav_root(d, arg),
    }
}

/// **Open a DETAIL page** — the one forward entry, so a new way in cannot push without seeding or
/// seed without pushing (`app::nav::nav_open` + `to_detail` + `seed_node`, in one call).
///
/// `season` is the one mount an argument cannot express: a SHOW opened with one season already
/// selected. An argument names a PAGE and a season is a tab inside one, so it rides on the mount
/// SEED (`DetailSeed`) exactly as it rode on the trail node's `Spot` before.
pub(crate) fn open_detail(
    d: &mut Dispatcher<AppHost>, rig: &mut Bridge,
    sid: crate::plex::ServerId, rk: &str, season: Option<std::os::raw::c_int>,
    ret: Option<ReturnState<u32, PageMemory>>,
) {
    let spot = crate::metadata::Spot {
        season: season.map(|s| s as i64),
        ..Default::default()
    };
    rig.seed_detail(sid, rk, spot);
    let arg = AppArg::Content(ContentArg::Detail { sid, rk: rk.to_string() });
    match ret {
        Some(ret) => nav_push_with_return(d, arg, ret),
        None => nav_push(d, arg),
    }
}

/// **BACK off a stacking page.**
pub(crate) fn nav_pop(d: &mut Dispatcher<AppHost>) {
    d.request(MachineId::Nav, NavOp::Pop);
}

pub(crate) fn nav_pop_with_return(d: &mut Dispatcher<AppHost>, ret: ReturnState<u32, PageMemory>) {
    d.request_with_return(MachineId::Nav, NavOp::Pop, ret);
}

/// **Back to a NAMED entry** — the player's exit (§5.1: the origin is an `EntryId`).
///
/// `PopTo` is a no-op for an entry that is no longer on the stack, and a player whose origin has
/// gone would then have no way off the screen at all — so an absent origin falls back to Home,
/// the one page that is always there. That fallback is `return_page`'s `unwrap_or(Node::Home)`
/// in its new home.
pub(crate) fn nav_pop_to(d: &mut Dispatcher<AppHost>, entry: EntryId) {
    if d.nav.tabs.stack.entries.iter().any(|e| e.id == entry) {
        d.request(MachineId::Nav, NavOp::PopTo(entry));
    } else {
        nav_root(d, AppArg::Home);
    }
}

/// **Withdraw a transition that has not committed**, iff the page that asked for it is still the
/// top (`NavStack::cancel`'s own rule, §6.2). Returns whether there was one, so an input that
/// cancelled NOTHING falls through to its normal handling instead of being swallowed.
///
/// This is `app::nav::nav_cancel`'s successor and the supersede test came with it: the `from ==
/// cur` route compare is `pending.from == top().id`, an ENTRY compare, which strictly dominates it
/// — two detail pages are one route and two entries.
pub(crate) fn nav_cancel(d: &mut Dispatcher<AppHost>) -> bool {
    let Some(top) = d.nav.top_page().map(|e| e.id) else { return false };
    d.nav.tabs.stack.cancel(top)
}

/// **Seat the who's-watching picker for a PROFILE SWITCH** — the one door, so that dropping the
/// outgoing profile's pages cannot be forgotten at one of the sites that switch.
///
/// `reset_for_profile` had no production caller at all before D1; see
/// `switching_profile_leaves_the_container_holding_nothing_of_the_previous_profile`.
pub(crate) fn switch_profile(d: &mut Dispatcher<AppHost>) {
    d.reset_for_profile();
    d.request(MachineId::Nav, NavOp::Root(AppArg::Profiles));
}

/// **The OS took the screen** (SDL `0x103`/`0x104`): the tree is PARKED (§9, §12.1).
///
/// The page stack does not move. It used to — the loop wrote `route = Home`, which `sync_page`
/// turned into a `Root(Home)` that retired the player entry and the page it was launched from, and
/// the foreground arm minted a fresh player over a stack that was now just Home. Nothing noticed,
/// because the two things that would have (the playback session and `App.play_from`) were both
/// held OUTSIDE the tree. They are not any more: the player's origin is the entry beneath it, so a
/// background that destroys entries destroys the way back. See
/// `an_app_switch_parks_the_page_stack_and_gives_the_same_entries_back`.
pub(crate) fn background(d: &mut Dispatcher<AppHost>) {
    d.suspend();
}

/// **…and gave it back** (SDL `0x105`/`0x106`). Idempotent, which is why the loop calls it on both
/// edges: webOS sends `will` and `did` and tells the app nothing about which arrives first.
pub(crate) fn foreground(d: &mut Dispatcher<AppHost>) {
    d.resume();
}

pub(crate) fn open_settings(d: &mut Dispatcher<AppHost>) {
    open_settings_at(d, SettingsPage::Root);
}

/// …and the DEV boot target's door: the same surface, rooted at `page` (see [`AppArg`] for why
/// the target is a root rather than a push).
pub(crate) fn open_settings_at(d: &mut Dispatcher<AppHost>, page: SettingsPage) {
    if surface_up(d, is_settings) {
        return;
    }
    d.nav.next_style = Style::Opaque { snapshot: true };
    d.request(MachineId::Nav, NavOp::Present(AppArg::Settings(page)));
}

/// Present the first-run consent question at its first stage (idempotent while it is up).
pub(crate) fn open_first_run_consent(d: &mut Dispatcher<AppHost>) {
    open_first_run_consent_at(d, 0);
}

/// …at `stage`, which only `/tmp/plxnative-consent=product` ever names.
pub(crate) fn open_first_run_consent_at(d: &mut Dispatcher<AppHost>, stage: u8) {
    if surface_up(d, is_first_run_consent) {
        return;
    }
    d.nav.next_style = Style::Opaque { snapshot: false };
    d.request(MachineId::Nav, NavOp::Present(AppArg::FirstRunConsent(stage)));
}

/// Dismiss whichever surface is up (the loop's teardown paths: sign-out, a profile reset).
pub(crate) fn dismiss_surfaces(d: &mut Dispatcher<AppHost>) {
    let ids: Vec<EntryId> = d
        .nav
        .modals
        .surfaces
        .iter()
        .filter(|s| matches!(s.phase, Phase::Opening | Phase::Open))
        .map(|s| s.entry.id)
        .collect();
    for id in ids {
        d.request(MachineId::Nav, NavOp::Dismiss(id));
    }
}

/// The INSTANT twin of [`dismiss_surfaces`], for a teardown where the page under the surface is
/// ALSO leaving this same frame — Privacy & data → **Delete all local data**, confirmed, whose
/// sweep signs the account out and hands the loop a fresh `Route::Login` before this function is
/// even reached (`loop_requests`'s `DeleteAllLocalData` arm runs
/// `delete_all_local_data_and_sign_out` first).
///
/// `dismiss_surfaces` goes through `d.request(NavOp::Dismiss)`, which is a SPRING back to 0 that
/// only starts applying at the NEXT frame's NAV COMMIT (`Dispatcher::request` merely parks it) and
/// then runs for however long the appear spring takes to settle — during which
/// `modal::surface_policy`'s `Phase::Closing` render policy is `HostRender::Cached`, i.e. the
/// surface keeps compositing the ONE glass snapshot it took of the page it was opened over. That
/// page is Home; the page underneath it a frame later is the freshly-mounted sign-in screen; so
/// for every frame the spring is still running, a cached picture of a page that no longer exists
/// draws on top of the one that replaced it. Legacy hit this exact seam and answered it with
/// `crate::ui::settings::hide()` — direct, synchronous field mutation, not a queued request — and
/// `ModalStack::hide` is that same shape ported to the container tree: it snaps `Phase::Closing`
/// AND the motion to 0 (already settled) on THIS surface, in THIS call, rather than parking a
/// request for later. Bypassing `d.request` here is deliberate for the same reason `hide`'s own
/// doc gives — there is no host left to hand an `Uncover`, so the `Navigation`-mediated path
/// buys nothing and only adds the one frame of queueing lag that produced the ghost in the first
/// place. The surface's actual retirement (`WillLeave`/`Unmount`) still runs one frame later,
/// through the ordinary `prune()` pass inside `bridge::frame` — harmless, since a `hide`d surface
/// draws nothing between now and then.
pub(crate) fn dismiss_surfaces_now(d: &mut Dispatcher<AppHost>) {
    let ids: Vec<EntryId> = d
        .nav
        .modals
        .surfaces
        .iter()
        .filter(|s| s.phase != Phase::Hidden)
        .map(|s| s.entry.id)
        .collect();
    for id in ids {
        d.nav.modals.hide(id);
    }
}

/// Is a surface matching `which` on screen in ANY phase — a dismissal that is still fading
/// included? That is the conservative reading and the one every caller wants: `open_*` must not
/// present a second surface over one that is on its way out.
///
/// **This is a DELIBERATE change from legacy, not a match for it.** The comment here used to
/// claim the opposite — that "the loop's `settings::is_open()` answered the same while the legacy
/// `Popover` was `closing`" — which is false: `ui/settings.rs`'s `is_open()` forwards to
/// `Popover::is_open()`, i.e. the bare `open` flag, and `Popover::dismiss()` clears `open` at the
/// same moment it sets `closing = true`. So legacy's `is_open()` was FALSE for the whole fade,
/// which meant `open()` could re-present a fresh popover over one still visibly closing — the
/// closing panel and the new one both drawing at once was a real legacy possibility, not
/// something this function is preserving. `surface_up`'s `phase != Hidden` is deliberately
/// STRICTER: it refuses a second `open_*` for the entire fade, closing included, so two Settings
/// surfaces can never be on screen together. The predicate that DOES line up with legacy's
/// behaviour is `Dispatcher::owns_input` (via `ModalStack::input_owner`), which excludes
/// `Phase::Closing` exactly as legacy's `visible() = open || closing` handed input back to the
/// page while the panel was still fading — that one genuinely is unchanged.
/// Is the *Track information* sheet up, and at which 1-based page? — `content_probe`'s two Detail
/// fields, taken from the surface itself rather than from a module flag.
fn tracks_probe(d: &Dispatcher<AppHost>) -> (bool, i32) {
    let Some(s) = d
        .nav
        .modals
        .surfaces
        .iter()
        .find(|s| matches!(s.entry.arg, AppArg::TracksPanel(_)) && s.phase != Phase::Hidden)
    else {
        return (false, 0);
    };
    let page = s
        .entry
        .inst
        .as_ref()
        .and_then(|i| i.screen.as_any())
        .and_then(|a| a.downcast_ref::<crate::screens::tracks_panel::TracksPanelScreen>())
        .map(|p| p.page())
        .unwrap_or(0);
    (true, page)
}

fn surface_up(d: &Dispatcher<AppHost>, which: impl Fn(&AppArg) -> bool) -> bool {
    d.nav
        .modals
        .surfaces
        .iter()
        .any(|s| which(&s.entry.arg) && s.phase != Phase::Hidden)
}

/// Is the Settings family up (any phase)? The loop's `settings::is_open()` twin.
pub(crate) fn settings_up(d: &Dispatcher<AppHost>) -> bool {
    surface_up(d, is_settings)
}

/// Is the first-run question up (any phase)?
pub(crate) fn consent_up(d: &Dispatcher<AppHost>) -> bool {
    surface_up(d, is_first_run_consent)
}

/// Is the tree's top PAGE engine-focused rather than ladder-focused — i.e. does the DISPATCHER
/// draw it? The predicate is the screen's own `FocusSource`, not a list of routes, so a page
/// migrated in a later phase joins this answer by being written, not by being enumerated here.
/// Since phase 8 it is true for every content page (Home, Library, Search) as well as first-run
/// Favourites, Login and Profiles; the legacy closure paints only Detail, Person and the player.
///
/// **`route` is not redundant, and leaving it out drew a stale page for a frame.** A `LoopReq`
/// that flips the route (`OnboardDone` → Home) is drained AFTER the dispatcher's frame, so
/// between that drain and the next NAV COMMIT the tree's top page is still the one the loop has
/// just navigated away from. Answering `true` there would skip the loop's own page pass and draw
/// the retired screen for one more frame — the first-run editor flashing over the Home the user
/// has just asked for. Requiring the tree to AGREE with the committed route makes the disagreement
/// resolve the safe way round: the loop draws its legacy page and the tree draws only surfaces,
/// which is what it did on every frame before the migration.
/// **The player instance, if one is mounted** — the one door to the state phase 9 moved off `App`.
///
/// It is an `Option` and every caller must treat `None` as "there is no playback on screen", which
/// is exactly what it means: the page is mounted when the `NavOp::Push(AppArg::Player)` that
/// `app::playback::enter_player` asks for commits, and unmounted when the entry is popped or
/// retired. The window between the request and the mount is ONE frame, and nothing in the ladders
/// acts on the transport inside it — `enter_player` seeds the mount rather than writing through
/// this. (It used to be `sync_page` following the loop's route mirror onto `Route::Player`; the
/// mirror is deleted, so the request and the mount are the only two moments there are.)
///
/// The pair mirrors `Dispatcher::top_page`'s own shape rather than searching the whole tree: an
/// overlay presented on the player's page-owned `ModalStack` is a SURFACE, so the player stays the
/// top PAGE and this keeps answering with it while a panel is up.
pub(crate) fn player(d: &Dispatcher<AppHost>) -> Option<&crate::screens::player::PlayerScreen> {
    d.nav
        .top_page()?
        .inst
        .as_ref()?
        .screen
        .as_any()?
        .downcast_ref::<crate::screens::player::PlayerScreen>()
}

pub(crate) fn player_mut(
    d: &mut Dispatcher<AppHost>,
) -> Option<&mut crate::screens::player::PlayerScreen> {
    let entry = d.nav.top_page()?.id;
    d.nav
        .entry_mut(entry)?
        .inst
        .as_mut()?
        .screen
        .as_any_mut()?
        .downcast_mut::<crate::screens::player::PlayerScreen>()
}

/// **The top page answers its own keys** — the engine owns the input for it.
///
/// The `route: Route` parameter these three took is gone with the fold (D1): `owns_input` already
/// ignored its argument outright, and the other two used theirs only to re-assert what the
/// container is the authority for — that the top page is the committed route, which
/// `frame_with_results` asserts every frame anyway. Every one of them was already reading the top
/// entry.
pub(crate) fn page_owned(d: &Dispatcher<AppHost>) -> bool {
    d.top_screen().map_or(false, |s| s.focus_source() == FocusSource::Engine)
}

pub(crate) fn search_owns_input(d: &Dispatcher<AppHost>) -> bool {
    !d.surface_up()
        && d.top_screen().and_then(|screen| screen.as_any())
            .is_some_and(|screen| screen.is::<crate::screens::search::SearchScreen>())
}

pub(crate) fn owns_input(d: &Dispatcher<AppHost>) -> bool {
    d.surface_up() || d.owns_input()
}

/// The topmost surface's heartbeat word, for the dev triggers that have to wait for a particular
/// page of the family to be up (`plxnative-legaldoc`, `plxnative-alert`).
pub(crate) fn surface_word(d: &Dispatcher<AppHost>) -> Option<&'static str> {
    d.top_surface_name()
}

/// The host page under the surfaces is FROZEN (§8.3): the loop skips its update.
pub(crate) fn host_frozen(d: &Dispatcher<AppHost>) -> bool {
    d.host_policy().0 == HostUpdate::Frozen
}

/// The host page is REPLACED: the loop skips drawing it.
pub(crate) fn host_replaced(d: &Dispatcher<AppHost>) -> bool {
    d.host_policy().1 == HostRender::Replaced
}

/// What the loop's PAGE half draws this frame, from the host fold and who owns the top page.
///
/// §8.3: a Replaced host "receives nothing" — and that includes its cached quad. Through phase 7
/// the loop's guard was `!host_replaced || page_owned`, which was right while the only owned
/// pages (first-run Favourites, Login, Profiles) could not host an opaque surface: `page_owned`
/// there meant "the dispatcher draws the page, not the closure", and `host_replaced` never held
/// on those routes. Phase 8 made Home an owned page AND the host of Settings, so both were true
/// at once and the page closure ran under the opaque ground on every frame — where
/// `popover::host::page_pass` served the frozen page snapshot as one full-screen `Class::Image`
/// quad, a second full-screen pass beneath the `RouteGround`'s own ambient wash. The draw-mask
/// census priced that quad at the whole regression: `settings-root` 40 fps unmasked, 60 with
/// either class masked (TV session 4, 2026-09-09). A Replaced host is drawn by nobody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PagePlan {
    /// The closure draws the page and, through `Dispatcher::draw(.., true)`, the surfaces.
    Owned,
    /// The closure draws the legacy fallback; the loop draws the surfaces after it.
    LegacyThenSurfaces,
    /// The host is replaced: no page pass, no cached quad — the surfaces alone.
    SurfacesOnly,
}

pub(crate) fn page_plan(host_replaced: bool, page_owned: bool) -> PagePlan {
    match (host_replaced, page_owned) {
        (true, _) => PagePlan::SurfacesOnly,
        (false, true) => PagePlan::Owned,
        (false, false) => PagePlan::LegacyThenSurfaces,
    }
}

/// **The heartbeat's ` overlay=` word IS the topmost surface's own `Screen::name`** — this
/// function is the read, and there is nothing else to it.
///
/// It was an eleven-arm `match` mapping each name to `" overlay=<the same name>"`, i.e. a second
/// transcription of an alphabet the screens already own, with the prefix baked into every literal
/// so it could stay `&'static str`. Two things were wrong with that and both had bitten: a screen
/// renamed without its arm following silently stopped printing an overlay at all (the fps scene
/// keyed on it then measures whatever else the route matched, which reads as a pass), and a NEW
/// surface printed nothing until somebody remembered — which is why `library_menu`, a surface
/// since phase 8, was invisible in the heartbeat for two phases. The prefix belongs to the
/// heartbeat's own format string (`run`'s `route={rn}{ov}`), not to the word.
///
/// **`"onboard"` is worth knowing about**, because it is one screen wearing two hats (§6.2
/// "Onboard ×2"): `SettingsPage::Favourites` mounts the very same `OnboardScreen` the first-run
/// ROUTE does, and `RouteSurface::top_word` (`screens/settings.rs`) answers whatever the top INNER
/// page's own `Screen::name` says without caring which stack put it there. So the same word is an
/// `overlay=` here and a `route=` there, which `tests/run.py` reads without ambiguity because a
/// scene declares only the field it needs.
pub(crate) fn overlay_word(d: &Dispatcher<AppHost>) -> Option<&'static str> {
    d.top_surface_name()
}

/// …and the ` overlay=` word each one PRESENTS as, read back through [`overlay_word`].
///
/// Presented on a real tree rather than handed to `Mounter::mount` directly, and the Settings
/// family is why: `RouteSurface::top_word` answers for whichever page of its own INNER stack is on
/// top, and that stack has nothing on it until the surface has been mounted AND stepped — a bare
/// `mount` call answers `settings` for the first-run consent question, which is precisely the
/// "two screens print one word" failure the family's own word split exists to prevent. Going
/// through `frame` also means this reads the same function the heartbeat does, on the same
/// container, rather than a second path that could agree with nothing.
///
/// The player's four panels are presented over the Player page, the rest over Home — a surface is
/// presented over the TOP PAGE, and a panel needs its player there.
#[cfg(test)]
pub(crate) fn every_surface_word() -> Vec<&'static str> {
    let mut out = Vec::new();
    for arg in every_surface_arg() {
        let route = if matches!(arg, AppArg::PlayerOverlay(_)) { AppArg::Player } else { AppArg::Home };
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        show_page(&mut d, route.clone());
        frame(&mut d, &mut rig, Tick { ms: 0, dt_us: 16_000 }, vec![]);
        // The style is the application's to choose per surface (`Navigation::next_style`), and it
        // does not change the word — `Compact` is enough for every one of them here.
        d.nav.next_style = Style::Compact;
        d.request(MachineId::Nav, NavOp::Present(arg));
        frame(&mut d, &mut rig, Tick { ms: 16, dt_us: 16_000 }, vec![]);
        out.push(overlay_word(&d).expect("a presented surface names a word"));
    }
    out
}

/// A key for the dispatcher: the raw SDL fields classified once (`ui::consts::classify`), the
/// fork's packed state read as an edge.
pub(crate) fn key_input(sym: u32, wcode: u32, state: u32, now: Tick, source: Source) -> InputEvent<u32> {
    use crate::ui::consts::{classify, Key as K};
    let key = match classify(sym, wcode) {
        K::Up => Key::Up,
        K::Down => Key::Down,
        K::Left { .. } => Key::Left,
        K::Right { .. } => Key::Right,
        K::Ok => Key::Ok,
        K::Back => Key::Back,
        _ => Key::Other,
    };
    let edge = if (state & 0xff) != 1 {
        Edge::Up
    } else if state & 0x100 != 0 {
        Edge::Repeat
    } else {
        Edge::Down
    };
    InputEvent {
        at: now,
        source,
        kind: InputKind::Key {
            key,
            sym,
            wcode,
            edge,
            at_edge: false,
        },
    }
}

pub(crate) fn pointer_input(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    InputEvent {
        at: now,
        source: Source::Sdl,
        kind: InputKind::Pointer { x, y, hit: None },
    }
}

pub(crate) fn click_input(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    InputEvent {
        at: now,
        source: Source::Sdl,
        kind: InputKind::Click { x, y, hit: None },
    }
}

/// **Pointer motion with a button held down** (§7.5, restructure phase 12) — the third pointer
/// kind, and the one that drives a CONTROL rather than focus: `ui::hit` resolves the hit and then
/// deliberately raises neither hover nor activation for it, so the only thing a drag can do is
/// what the receiving screen makes of it. Its one consumer today is the player's scrub bar, whose
/// preview follows the pointer between the click that seated the gesture and the button-up that
/// commits it.
pub(crate) fn drag_input(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    InputEvent {
        at: now,
        source: Source::Sdl,
        kind: InputKind::Drag { x, y, hit: None },
    }
}

/// A wheel tick as the key it stands for on a vertical flow (the family's `on_updown`).
pub(crate) fn wheel_input(dy: i32, now: Tick) -> Vec<InputEvent<u32>> {
    let key = if dy < 0 { Key::Down } else { Key::Up };
    let (sym, wcode) = (0, 0);
    vec![
        InputEvent {
            at: now,
            source: Source::Sdl,
            kind: InputKind::Key { key, sym, wcode, edge: Edge::Down, at_edge: false },
        },
        InputEvent {
            at: now,
            source: Source::Sdl,
            kind: InputKind::Key { key, sym, wcode, edge: Edge::Up, at_edge: false },
        },
    ]
}

/// **A pointer button-UP as the press machine's release.** The dispatcher's `InputKind` has no
/// release of its own: `InputMachine::release` is reached from ONE place, an `Ok` key on the `Up`
/// edge (`dispatch`'s ingest), whatever armed the press. So a click that armed a control face is
/// released by handing the tree that edge — and it is inert everywhere else, because every screen
/// in the family matches `Edge::Down` (or `!= Edge::Up`) and the engine's own half skips `Up`
/// outright. Without it a mouse press would sit dipped until `press::MAX_HOLD_MS` (1 s) committed
/// it, which on the simulator reads as a UI that answers a click a second late.
pub(crate) fn release_input(now: Tick) -> InputEvent<u32> {
    InputEvent {
        at: now,
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Ok,
            sym: 0,
            wcode: 0,
            edge: Edge::Up,
            at_edge: false,
        },
    }
}

/// A scripted direction (the dev oscillators), both edges.
pub(crate) fn script_key(key: Key, now: Tick) -> Vec<InputEvent<u32>> {
    [Edge::Down, Edge::Up]
        .into_iter()
        .map(|edge| InputEvent {
            at: now,
            source: Source::Script,
            kind: InputKind::Key { key, sym: 0, wcode: 0, edge, at_edge: false },
        })
        .collect()
}

#[allow(dead_code)]
fn _measure_is_object_safe(m: &dyn Measure, s: &CStr) -> f32 {
    m.width(s, 1, false)
}

#[cfg(test)]
#[path = "session_protocol_tests.rs"]
mod session_protocol_tests;

#[cfg(test)]
#[path = "session_profile_regression_tests.rs"]
mod session_profile_regression_tests;

#[cfg(test)]
#[path = "session_resource_tests.rs"]
mod session_resource_tests;
#[cfg(test)]
#[path = "recording_erasure_tests.rs"]
mod recording_erasure_tests;
#[cfg(test)]
#[path = "consent_owner_tests.rs"]
mod consent_owner_tests;

#[cfg(test)]
#[path = "session_controller_regression_tests.rs"]
mod session_controller_regression_tests;

#[cfg(test)]
#[path = "session_picker_regression_tests.rs"]
mod session_picker_regression_tests;

#[cfg(test)]
#[path = "session_dev_bootstrap_tests.rs"]
mod session_dev_bootstrap_tests;

#[cfg(test)]
#[path = "session_endpoint_policy_tests.rs"]
mod session_endpoint_policy_tests;

#[cfg(test)]
#[path = "session_stored_home_tests.rs"]
mod session_stored_home_tests;

#[cfg(test)]
#[path = "bridge_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "session_plaintext_repair_tests.rs"]
mod session_plaintext_repair_tests;

#[cfg(test)]
#[path = "session_dispatch_tests.rs"]
mod session_dispatch_tests;

#[cfg(test)]
#[path = "home_retention_tests.rs"]
mod home_retention_tests;

#[cfg(test)]
#[path = "library_bookmark_tests.rs"]
mod library_bookmark_tests;

#[cfg(test)]
#[path = "library_diagnostic_tests.rs"]
mod library_diagnostic_tests;

#[cfg(test)]
#[path = "library_query_tests.rs"]
mod library_query_tests;

#[cfg(test)]
#[path = "library_deferred_tests.rs"]
mod library_deferred_tests;

#[cfg(test)]
#[path = "library_navigation_tests.rs"]
mod library_navigation_tests;

#[cfg(test)]
#[path = "library_host_freeze_tests.rs"]
mod library_host_freeze_tests;

#[cfg(test)]
#[path = "library_shelf_action_tests.rs"]
mod library_shelf_action_tests;

#[cfg(test)]
#[path = "search_publication_tests.rs"]
mod search_publication_tests;

#[cfg(test)]
#[path = "search_owned_tests.rs"]
mod search_owned_tests;

#[cfg(test)]
#[path = "detail_panel_tests.rs"]
mod detail_panel_tests;

#[cfg(test)]
#[path = "surface_navigation_tests.rs"]
mod surface_navigation_tests;

#[cfg(test)]
#[path = "viewstate_directory_policy_tests.rs"]
mod viewstate_directory_policy_tests;

#[cfg(test)]
#[path = "person_lifecycle_tests.rs"]
mod person_lifecycle_tests;
