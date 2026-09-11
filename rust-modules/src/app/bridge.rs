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

#[derive(Clone, Copy)]
pub(crate) struct AppViews<'a> {
    pub(crate) auth: crate::auth::SessionRead<'a>,
    pub(crate) hubs: crate::pms::HubsView<'a>,
    pub(crate) listing: crate::stores::browse::ListingView<'a>,
    pub(crate) directory: crate::stores::browse::DirectoryView<'a>,
    pub(crate) section_hubs: crate::stores::browse::HubsView<'a>,
    pub(crate) search: crate::search::view::SearchView<'a>,
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

// ---------------------------------------------------------------------------------------------
// the rig
// ---------------------------------------------------------------------------------------------

/// The consent MACHINE (§2.2): the one owner of the two decisions. It applies an answer and
/// PUBLISHES it (`telemetry::record` → `consent::install`), which is the snapshot every
/// telemetry thread reads; nothing else writes it.
pub(crate) struct ConsentMachine;

impl ConsentMachine {
    fn record(&mut self, errors: bool, usage: bool) {
        let prev = crate::telemetry::consent::current().unwrap_or_default();
        let next = crate::telemetry::consent::apply(&prev, errors, usage, crate::telemetry::mint_id);
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
        crate::telemetry::record(next);
        crate::telemetry::flush_soon();
    }
}

/// What the bridge lends the dispatcher, and what it collects for the loop.
pub(crate) struct Bridge {
    session: crate::auth::SessionMachine,
    session_adapter: super::adapters::session::SessionAdapter,
    session_ready: Option<(u64, crate::auth::owner::ProfileScope, crate::plex::session::ServerRef, String)>,
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
    measure: &'static dyn Measure,
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
    pub(crate) fn new(now_us: fn() -> u64, init: crate::auth::SessionInit,
        mt: &crate::task::MainThread) -> Self {
        // A `static`, not `&TtfMeasure` inline: a unit-struct literal DOES const-promote to
        // `'static` today, but that is a rule about the expression rather than a promise about
        // this field, and a `static` states the lifetime outright. Same reasoning as the one
        // `screens::settings`'s test module writes out beside its own measure.
        static TTF: crate::text::TtfMeasure = crate::text::TtfMeasure;
        Self::with_measure(&TTF, now_us, init, super::adapters::session::SessionAdapter::live(mt))
    }

    /// The same bridge over a measure that needs no fonts — the ONLY constructor a host test may
    /// use. See [`Bridge::measure`] for what happens to a test that reaches for `new` instead: it
    /// dies inside `text.rs` on a `debug_assert!`, several frames away from anything it asserted.
    #[cfg(test)]
    pub(crate) fn for_test(now_us: fn() -> u64) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        Self::with_measure(&FIXTURE, now_us,
            crate::auth::SessionInit::captured(crate::plex::session::Session::default()),
            super::adapters::session::SessionAdapter::fixture())
    }

    fn with_measure(measure: &'static dyn Measure, now_us: fn() -> u64,
        init: crate::auth::SessionInit, session_adapter: super::adapters::session::SessionAdapter) -> Self {
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        directory.capture();
        Self::with_publications(measure, now_us, init, session_adapter, StorePublications {
            hubs: crate::pms::hubs_snapshot(), listing: crate::stores::browse::listing_snapshot(),
            directory, section_hubs: crate::stores::browse::hubs_snapshot(),
            search: crate::stores::search::snapshot(),
        })
    }

    #[cfg(test)]
    fn for_session_test(init: crate::auth::SessionInit) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        let adapter = super::adapters::session::SessionAdapter::fixture_with(init.persisted.clone());
        Self::with_publications(&FIXTURE, || 0, init, adapter, StorePublications {
            hubs: crate::pms::HubsSnapshot::empty_for_test(),
            listing: crate::stores::browse::ListingSnapshot::empty_for_test(),
            directory: Default::default(), section_hubs: crate::stores::browse::HubsSnapshot::empty_for_test(),
            search: Default::default(),
        })
    }

    fn with_publications(measure: &'static dyn Measure, now_us: fn() -> u64,
        init: crate::auth::SessionInit, session_adapter: super::adapters::session::SessionAdapter,
        reads: StorePublications) -> Self {
        Self {
            session: crate::auth::SessionMachine::from_init(init),
            session_adapter,
            session_ready: None,
            mounter: AppMounter::default(),
            playback: crate::route::PlaybackSession::IDLE,
            playback_live: false,
            video_plane: false,
            measure,
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
            consent: ConsentMachine,
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

    pub(crate) fn take_session_ready(&mut self) -> Option<crate::auth::ReadyCreds> {
        let (epoch, scope, server, token) = self.session_ready.take()?;
        if !self.session.ready_is_current(epoch, scope) { return None; }
        Some(crate::auth::ReadyCreds { origin: server.origin(), token,
            tier: server.tier, pin: server.resolve_pin() })
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
            directory: self.directory.view(), section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback }, self.measure);
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
            directory: self.directory.view(), section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback }, self.measure);
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
                section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
            }, self.measure);
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
            section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
        }, self.measure);
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
            self.chrome.refresh(self.measure);
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

    /// Return Search's publication change so the frame can coalesce it with queued notices.
    fn capture_views(&mut self, d: &mut Dispatcher<AppHost>) -> bool {
        let search = crate::stores::search::snapshot();
        let search_changed = !self.search.same_publication(&search);
        if search_changed {
            self.search = search;
        }
        let directory_before = self.directory.clone();
        let hubs_before = (self.section_hubs.view().id(), self.section_hubs.view().revision());
        self.directory.capture();
        // Directory capture resolves profile pins and may repoint the active section. Both
        // content snapshots must name the resulting section, not opposite sides of that repoint.
        self.section_hubs = crate::stores::browse::hubs_snapshot();
        let listing = crate::stores::browse::listing_snapshot();
        let listing_changed = !self.listing.view().same_items(listing.view())
            || self.listing.view().fetch() != listing.view().fetch();
        self.listing = listing;
        if listing_changed || !self.directory.same_publication(&directory_before)
            || hubs_before != (self.section_hubs.view().id(), self.section_hubs.view().revision()) {
            // Bring Library's key projection up to the captured frame before any input uses it.
            d.store_changed(StoreId::Browse.ord(), crate::stores::gen(StoreId::Browse));
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
        search_changed
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
        self.chrome.seed_for_test(name, initial, labels, self.measure);
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
            section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
        }, self.measure);
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
            section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
        }, self.measure);
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
            let item = page.focused_item(ret.focus).filter(|m| crate::screens::item_menu::has_actions(m))?;
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
            section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
        }, self.measure);
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
                section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
            },
            measure: self.measure,
        }
    }
    fn deliver(&mut self, to: MachineId, msg: &AppMsg, parts: &CxParts<u32>, fx: &mut Effects<'_, AppHost>) -> Handled {
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
                section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
            }, self.measure);
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
        let cx = parts.cx::<AppHost>(AppViews { auth: self.session.read(),
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(), search: self.search.view(), session: &self.playback,
        }, self.measure);
        match msg {
            AppMsg::Store(cmd) => step_store(cmd, &cx, fx),
            AppMsg::HubsResult(result) => {
                crate::stores::hubs::land(result).endpoints.emit(fx);
                Handled::Yes
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Hubs) => {
                crate::stores::hubs::HubsStore.step(&crate::stores::StoreEv::Pump { dt: parts.tick.dt() }, &cx, fx)
            }
            AppMsg::StoreWork(crate::stores::StoreWork::BrowseDiscovery) => {
                crate::stores::browse::discover_pump().emit(fx);
                Handled::Yes
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Browse) => {
                crate::stores::browse::BrowseStore.step(&crate::stores::StoreEv::Pump { dt: parts.tick.dt() }, &cx, fx)
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Search { dt_us }) => {
                crate::stores::search::SearchStore.step(&crate::stores::StoreEv::Pump { dt: *dt_us as f32 / 1_000_000.0 }, &cx, fx)
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
        match fx {
            AppFx::Session(command) => out.push(Fx::Deliver(MachineId::Session,
                Delivery::Machine(AppMsg::Session(crate::auth::owner::SessionEvent::Command(command))))),
            AppFx::SessionEffect(effect) => self.session_effect(effect, out),
            AppFx::Store(id, cmd) => out.push(Fx::Deliver(MachineId::Store(id.ord()), Delivery::Machine(AppMsg::Store(cmd)))),
            AppFx::StoreWork(work) => out.push(Fx::Deliver(
                MachineId::Store(work.store().ord()), Delivery::Machine(AppMsg::StoreWork(work)))),
            AppFx::Consent(ConsentCmd::Record { errors, usage }) => self.consent.record(errors, usage),
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
        let mut deliver = |event| out.push(Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(event))));
        match effect {
            SessionFx::Commit { req, epoch, arrival, plan } => {
                if let Some(permit) = self.session.commit_permit(req, epoch, arrival) {
                    let reply = self.session_adapter.commit(permit, &plan);
                    deliver(SessionEvent::Commit(reply));
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
                }
            }
            SessionFx::Ready { epoch, scope, server, token } => {
                if self.session.ready_is_current(epoch, scope) {
                    self.session_ready = Some((epoch, scope, server, token));
                }
            }
            // Erase is ordered work, not a stale asynchronous completion: a subsequent login
            // may already have advanced the epoch, but cannot skip deleting old credentials.
            SessionFx::Erase { .. } => {
                self.session_ready = None;
                self.session_adapter.erase();
            }
            SessionFx::Coordinator(action) => self.session_adapter.coordinator(action),
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
    use crate::stores::{browse, hubs, metadata, person, search, viewstate};
    match cmd {
        StoreCmd::Browse(c) => browse::BrowseStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
        StoreCmd::Hubs(c) => hubs::HubsStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
        StoreCmd::Metadata(c) => metadata::MetadataStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
        StoreCmd::Search(c) => search::SearchStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
        StoreCmd::Person(c) => person::PersonStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
        StoreCmd::ViewState(c) => viewstate::ViewStateStore.step(&StoreEv::Cmd(c.clone()), cx, fx),
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
/// anyway. A frame is not a walk of a nav tree: it drains `crate::stores::take_notices()`, a
/// process-global dirty queue, and it pumps every store — and `browse`'s pump ends in
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
    let search_changed = rig.capture_views(d);
    rig.capture_chrome(d);
    rig.deliver_home_commands(d);
    rig.deliver_library_commands(d);
    let mut search_notified = false;
    for (id, gen) in crate::stores::take_notices() {
        search_notified |= id == StoreId::Search;
        d.store_changed(id.ord(), gen);
    }
    // A publication can change through a legacy producer without a store notice. Announce
    // that captured change, but do not double-deliver an ordinary queued Search notice.
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
    let tab = d.nav.tabs.stack.pending_dest().and_then(pill_of_arg);
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
            section_hubs: rig.section_hubs.view(), search: rig.search.view(), session: &rig.playback,
        }, rig.measure);
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
            directory: rig.directory.view(), section_hubs: rig.section_hubs.view(), search: rig.search.view(), session: &rig.playback }, rig.measure);
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
        section_hubs: rig.section_hubs.view(), search: rig.search.view(), session: &rig.playback,
    }, rig.measure);
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
                .and_then(|s| s.focused_item(focus));
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
fn pill_of_arg(arg: &AppArg) -> Option<usize> {
    use crate::app::chrome::{pill_of, Pill};
    match arg {
        AppArg::Home => pill_of(Pill::Home),
        AppArg::Search => pill_of(Pill::Search),
        AppArg::Library => {
            let mut d = crate::stores::browse::DirectorySnapshot::default();
            d.capture();
            let view = d.view();
            let kind = view.current().map(|i| view.sections()[i].kind)?;
            pill_of(Pill::Section(kind))
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
            if let Some(pill) = focus_pill.filter(|pill| crate::app::chrome::pill_of(*pill).is_some()) {
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
mod tests {
    #[test]
    fn session_boot_picker_publishes_captured_profile_without_ready_handoff() {
        let stored = crate::plex::session::Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            server: crate::plex::session::ServerRef { machine_id: "synthetic-server".into(),
                address: "192.0.2.1".into(), port: 32400, token: "synthetic-server-token".into(),
                ..Default::default() },
            user: crate::plex::session::UserRef { uuid: "synthetic-user".into(), title: "A".into(),
                ..Default::default() },
            home_users: vec![crate::plex::session::HomeUserRef { uuid: "synthetic-user".into(),
                title: "A".into(), protected: true, ..Default::default() }], ..Default::default()
        };
        let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(stored));
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, crate::auth::SessionCmd::StartSwitch(crate::auth::Picker::Boot));
        d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(rig.auth_read().0.phase, crate::auth::Phase::Profiles);
        let profile = rig.session_adapter.fixture_resources().profile.as_ref()
            .expect("boot profile publication must come from the concrete owner, not prior global seeding");
        assert_eq!(profile.profile.as_ref().unwrap().uuid, "synthetic-user");
        assert_eq!(profile.scope.0, 1);
        assert!(rig.take_session_ready().is_none(), "a protected boot picker has not seated a viewer");
        assert!(rig.session_adapter.fixture_resources().disk.home_users[0].protected);
    }

    #[test]
    fn session_stored_boot_publishes_before_handoff_without_saving_credentials() {
        let stored = crate::plex::session::Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            server: crate::plex::session::ServerRef { machine_id: "synthetic-server".into(),
                address: "192.0.2.1".into(), port: 32400, token: "synthetic-server-token".into(),
                ..Default::default() },
            user: crate::plex::session::UserRef { uuid: "synthetic-user".into(), title: "A".into(),
                ..Default::default() }, ..Default::default()
        };
        assert!(stored.can_go_local());
        let mut rig = Bridge::for_session_test(crate::auth::SessionInit::captured(stored));
        // Stored boot installs the captured authority; it must not rewrite a more recent
        // file merely to publish the initial profile and restore its granted registry.
        rig.session_adapter.fixture_resources().disk.account_token = "synthetic-new-disk-token".into();
        let mut d = Dispatcher::<AppHost>::new();
        assert!(rig.take_session_ready().is_none());
        execute_session_command(&mut d, crate::auth::SessionCmd::ResumeStored);
        assert!(rig.session_adapter.fixture_resources().profile.is_none());
        d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        let publication = rig.session_adapter.fixture_resources().profile.as_ref().unwrap();
        assert_eq!(publication.profile.as_ref().unwrap().uuid, "synthetic-user");
        assert_eq!(publication.scope.0, 1);
        let ready = rig.take_session_ready().unwrap();
        assert_eq!(ready.token, "synthetic-server-token");
        assert!(rig.take_session_ready().is_none(), "the handoff is consumed exactly once");
        assert_eq!(rig.session_adapter.fixture_resources().disk.account_token, "synthetic-new-disk-token");
        execute_session_command(&mut d, crate::auth::SessionCmd::ResumeStored);
        d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(rig.session.read().0.scope.0, 1, "repeated bootstrap cannot allocate a second profile scope");
        assert!(rig.take_session_ready().is_none());
    }

    #[test]
    fn session_current_negative_commit_reply_unblocks_independent_carried_work() {
        use crate::auth::owner::{AdmissionId, AdmissionState, Command, Identity, Pending,
            SessionEvent, SessionOp, SessionWorkKey, StreamPhase};
        use crate::auth::{AuthProgress, LoginProgress, RegistryProgress};
        use crate::ui::machine::RequestId;
        let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            ..Default::default()
        });
        init.phase = crate::auth::Phase::Discovering;
        init.next_req = 2;
        let a_key = SessionWorkKey { epoch: 1, op: SessionOp::Login };
        let b_key = SessionWorkKey { epoch: 1, op: SessionOp::ServerRoster };
        for (req, key) in [(1, a_key), (2, b_key)] {
            init.pending.insert(req, Pending { key, expected: Identity::of(&init.persisted),
                lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
                admission: AdmissionState::Awaiting(AdmissionId(req)) });
        }
        let expected = crate::auth::SessionIdentity::of(&init.persisted);
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter.launch(RequestId(1), a_key, true, |job| { job(); true }, |output| {
            assert!(output.complete(LoginProgress::SignedIn { epoch: 1,
                server: crate::plex::session::ServerRef { machine_id: "synthetic-server".into(),
                    address: "192.0.2.1".into(), port: 32400, token: "synthetic-token".into(),
                    ..Default::default() }, sources: Vec::new(), users: Vec::new() }.into()).is_ok());
        }).unwrap();
        rig.session_adapter.launch(RequestId(2), b_key, true, |job| { job(); true }, move |output| {
            assert!(output.progress(AuthProgress::Registry(RegistryProgress::Install {
                epoch: 1, expected: Some(expected), sources: Vec::new(), primary: None,
            })).is_ok());
            // Actual completion guard supplies the terminal; no test-only retirement path.
        }).unwrap();
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 3);
        let results = records.iter().cloned().map(|envelope| (envelope.addr,
            AppMsg::Session(SessionEvent::Result(envelope)))).collect();
        // The owner permit stays current, but latest disk identity refuses A's credential
        // patch. B has a separate accepted request and must not be discarded with A.
        rig.session_adapter.fixture_resources().disk.client_id = "synthetic-new-disk-identity".into();
        let mut d = Dispatcher::<AppHost>::new();
        for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 4 {
            execute_session_command(&mut d, Command::DismissPinError);
        }
        let first = d.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
        assert!(first.carried > 0, "A's negative reply must actually cross the frame boundary");
        assert!(rig.session.commit_is_current(1, 1, records[0].arrival));
        assert_eq!(rig.session.snapshot_init().inbox.len(), 2);
        assert_eq!(rig.session.read().0.phase, crate::auth::Phase::Discovering);
        assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
        assert!(records.iter().all(|record| rig.session_adapter.admitted(record)));
        d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(rig.session_adapter.fixture_resources().registry_writes.len(), 1,
            "B's independent commit must execute after A's queued negative reply");
        assert_eq!(rig.session_adapter.fixture_resources().disk.client_id, "synthetic-new-disk-identity");
        let state = rig.session.snapshot_init();
        assert!(state.pending.is_empty());
        assert!(state.pending_commit.is_none());
        assert!(state.inbox.is_empty());
        assert!(!state.pump_pending);
        assert!(records.iter().all(|record| !rig.session_adapter.admitted(record)));
    }

    #[test]
    fn session_cancel_preserves_carried_receipts_until_unique_discard() {
        use crate::auth::owner::{AdmissionId, AdmissionState, Command, Identity, Pending, Receipt,
            SessionEvent, SessionFx, SessionOp, SessionWorkKey, StreamPhase};
        use crate::auth::{AuthProgress, LoginProgress, RegistryProgress};
        use crate::ui::machine::RequestId;
        let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
            client_id: "synthetic-client".into(), ..Default::default()
        });
        let key = SessionWorkKey { epoch: 1, op: SessionOp::Login };
        init.next_req = 1;
        init.pending.insert(1, Pending { key, expected: Identity::of(&init.persisted),
            lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(1)) });
        let mut rig = Bridge::for_session_test(init);
        rig.session_adapter.launch(RequestId(1), key, true, |job| { job(); true }, |output| {
            assert!(output.progress(AuthProgress::Registry(RegistryProgress::Install {
                epoch: 1, expected: None, sources: Vec::new(), primary: None,
            })).is_ok());
            assert!(output.complete(LoginProgress::Failed { epoch: 1, message: "synthetic".into() }.into()).is_ok());
        }).unwrap();
        let records = rig.session_adapter.take_results();
        assert_eq!(records.len(), 2);
        let a = records[0].clone();
        let b = records[1].clone();
        let old_ack = Receipt::of(&a);
        let mut d = Dispatcher::<AppHost>::new();
        for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 2 {
            execute_session_command(&mut d, Command::DismissPinError);
        }
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(SessionEvent::Result(a.clone())))));
        execute_session_command(&mut d, Command::EraseLocal);
        d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
            Delivery::Machine(AppMsg::Session(SessionEvent::Result(b.clone())))));
        let first = d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert!(first.carried > 0);
        assert_eq!(rig.session.read().0.phase, crate::auth::Phase::Deleted);
        assert!(rig.session.snapshot_init().pending_commit.is_none());
        assert!(rig.session_adapter.admitted(&b), "cancel cannot return a carried record's credit");

        let next_key = SessionWorkKey { epoch: 2, op: SessionOp::Login };
        rig.session_adapter.launch(RequestId(2), next_key, true, |job| { job(); true }, |output| {
            assert!(output.complete(LoginProgress::Failed { epoch: 2, message: "synthetic-new".into() }.into()).is_ok());
        }).unwrap();
        // Return A's unique credit twice while B is still carried. Neither can release B.
        rig.session_adapter.acknowledge(&[old_ack, old_ack]);
        assert!(rig.session_adapter.take_results().is_empty());
        assert!(rig.session_adapter.admitted(&b));
        for record in [a, b.clone()] {
            d.emit(MachineId::Session, Fx::Deliver(MachineId::Session,
                Delivery::Machine(AppMsg::Session(SessionEvent::Result(record)))));
        }
        d.emit(MachineId::Session, Fx::App(AppFx::SessionEffect(SessionFx::Acknowledge(vec![old_ack]))));
        d.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
        assert!(!rig.session_adapter.admitted(&b));
        let next = rig.session_adapter.take_results();
        assert_eq!(next.len(), 1, "the next batch opens only after B's unique discard ACK");
        rig.session_adapter.acknowledge(&[old_ack]);
        assert!(rig.session_adapter.admitted(&next[0]), "late ACK cannot free a newer batch");
        assert!(rig.session_adapter.fixture_resources().disk.account_token.is_empty());
        assert!(rig.session_adapter.fixture_resources().registry_writes.iter()
            .all(|write| matches!(write, crate::auth::owner::RegistryPlan::Revoke)));
        assert!(rig.session.snapshot_init().pending_commit.is_none());
    }

    #[test]
    fn two_session_bridges_dispatch_without_global_capture_or_a_serial_lock() {
        use crate::auth::{Phase, SessionCmd, SessionInit};
        let init = || SessionInit::captured(crate::plex::session::Session {
            client_id: "synthetic-client".into(), ..Default::default()
        });
        let mut a = Bridge::for_session_test(init());
        let mut b_init = init();
        b_init.phase = Phase::Waiting;
        b_init.pin_code = "BBBB".into();
        b_init.qr_png = vec![2, 3, 4];
        b_init.qr_gen = 7;
        b_init.next_qr = 7;
        let mut b = Bridge::for_session_test(b_init);
        let retained_b = b.session.publication();
        let before_b = b.session.subhash();
        let mut da = Dispatcher::<AppHost>::new();
        let mut db = Dispatcher::<AppHost>::new();
        execute_session_command(&mut da, SessionCmd::StartLogin);
        da.frame_with(&mut a, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(a.session.read().0.phase, Phase::Creating);
        assert_eq!(b.session.subhash(), before_b);
        assert!(b.session_adapter.take_results().is_empty());
        let results: AppResults = a.session_adapter.take_results().into_iter().map(|envelope|
            (envelope.addr, AppMsg::Session(crate::auth::owner::SessionEvent::Result(envelope)))).collect();
        assert_eq!(results.len(), 1, "fixture spawn refusal must use the production Landing terminal");
        da.frame_with(&mut a, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), results, &mut NoTap, false);
        assert_eq!(a.session.read().0.phase, Phase::Error);
        assert_eq!(b.session.subhash(), before_b);
        assert!(std::sync::Arc::ptr_eq(&retained_b, &b.session.publication()));
        execute_session_command(&mut db, SessionCmd::NoteDeleteLeftovers(3));
        db.frame_with(&mut b, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(a.session.read().0.delete_leftovers, 0);
        assert_eq!(b.session.read().0.delete_leftovers, 3);
        assert_eq!(retained_b.read().0.phase, Phase::Waiting);
        assert_eq!(&*retained_b.read().0.code, "BBBB");
        assert_eq!(&*retained_b.read().0.png, &[2, 3, 4]);
        assert_eq!(retained_b.read().0.qr_generation, 7);
        assert!(b.session_adapter.fixture_resources().registry_writes.is_empty());
        assert!(a.session_adapter.fixture_resources().registry_writes.is_empty());
        execute_session_command(&mut da, SessionCmd::EraseLocal);
        da.frame_with(&mut a, Tick { ms: 32, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
        assert_eq!(a.session.read().0.phase, Phase::Deleted);
        assert_eq!(a.session.read().0.scope.0, 1);
        let published = a.session_adapter.fixture_resources().profile.as_ref().unwrap();
        assert_eq!(published.scope.0, 1, "the adapter publishes the owner's explicit generation");
        assert!(published.profile.is_none());
        assert!(a.session_adapter.fixture_resources().disk.account_token.is_empty());
        assert_eq!(b.session.read().0.scope.0, 0);
        assert_eq!(b.session.read().0.phase, Phase::Waiting);
        assert!(b.session_adapter.fixture_resources().profile.is_none());
    }

    #[test]
    fn session_registry_then_terminal_waits_for_queued_commit_replies() {
        use crate::auth::owner::{AdmissionId, AdmissionState, Identity, Pending, SessionOp,
            SessionWorkKey, StreamPhase};
        use crate::auth::{AuthProgress, LoginProgress, RegistryProgress};
        use crate::ui::machine::RequestId;
        for (success, carry) in [(true, false), (false, false), (true, true), (false, true)] {
            let mut init = crate::auth::SessionInit::captured(crate::plex::session::Session {
                client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
                ..Default::default()
            });
            init.phase = crate::auth::Phase::Discovering;
            init.next_req = 1;
            let key = SessionWorkKey { epoch: init.epoch, op: SessionOp::Login };
            init.pending.insert(1, Pending { key, expected: Identity::of(&init.persisted),
                lifecycle: None, last_arrival: None, phase: StreamPhase::Running, capture: None,
                admission: AdmissionState::Awaiting(AdmissionId(1)) });
            let mut rig = Bridge::for_session_test(init);
            rig.session_adapter.launch(RequestId(1), key, true, |job| { job(); true }, move |output| {
                for _ in 0..2 {
                    assert!(output.progress(AuthProgress::Registry(RegistryProgress::Install {
                        epoch: key.epoch, expected: None, sources: Vec::new(), primary: None,
                    })).is_ok());
                }
                let terminal = if success {
                    LoginProgress::SignedIn { epoch: key.epoch, server: crate::plex::session::ServerRef {
                        machine_id: "synthetic-server".into(), address: "192.0.2.1".into(),
                        port: 32400, token: "synthetic-server-token".into(), ..Default::default()
                    }, sources: Vec::new(), users: Vec::new() }
                } else {
                    LoginProgress::Failed { epoch: key.epoch, message: "synthetic failure".into() }
                };
                assert!(output.complete(AuthProgress::Login(terminal)).is_ok());
            }).unwrap();
            let records = rig.session_adapter.take_results();
            assert_eq!(records.len(), 3);
            assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
            let results = records.into_iter().map(|envelope| (envelope.addr,
                AppMsg::Session(crate::auth::owner::SessionEvent::Result(envelope)))).collect();
            let mut dispatcher = Dispatcher::<AppHost>::new();
            if carry {
                // Consume the real pre+post budgets so A's commit effect, the terminal, and
                // B retained behind A must survive into the next frame. No test-only drain.
                for _ in 0..crate::ui::dispatch::MAX_STEPS_PRE + crate::ui::dispatch::MAX_STEPS_POST - 2 {
                    dispatcher.emit(MachineId::Nav, Fx::Deliver(MachineId::Session,
                        Delivery::Machine(AppMsg::Session(crate::auth::owner::SessionEvent::Command(
                            crate::auth::owner::Command::DismissPinError)))));
                }
            }
            let report = dispatcher.frame_with(&mut rig, Tick::default(), Vec::new(), results, &mut NoTap, false);
            if carry {
                assert!(report.carried > 0, "the regression must actually exercise cross-frame carry");
                let retained = rig.session.snapshot_init();
                assert!(retained.pending_commit.is_some());
                assert_eq!(retained.inbox.len(), 1);
                assert!(rig.session_adapter.fixture_resources().registry_writes.is_empty());
                dispatcher.frame_with(&mut rig, Tick { ms: 16, dt_us: 16_000 }, Vec::new(), Vec::new(), &mut NoTap, false);
            }
            assert_eq!(rig.session_adapter.fixture_resources().registry_writes.len(), 2,
                "both commit-bearing progress observations must precede the terminal");
            assert_eq!(rig.session.read().0.phase,
                if success { crate::auth::Phase::Ready } else { crate::auth::Phase::Error });
            let state = rig.session.snapshot_init();
            assert!(state.pending_commit.is_none());
            assert!(state.pending.is_empty());
            assert!(state.inbox.is_empty());
            assert!(!state.pump_pending);
        }
    }

    #[test]
    fn session_replies_cross_the_production_queued_drain_with_exact_correlation() {
        use crate::auth::owner::{Command, ReplyTo, SessionEvent};
        use crate::ui::dispatch::Tap;
        struct Replies(Vec<(u32, u32, bool)>);
        impl Tap<AppHost> for Replies {
            fn effect(&mut self, _: u64, stamped: &crate::ui::machine::Stamped<AppHost>) {
                if let Fx::Deliver(MachineId::Instance(instance),
                    Delivery::Screen(ScreenEvent::Async(req, message))) = &stamped.fx {
                    match message {
                        AppMsg::RestartReply { correlation, accepted } => {
                            assert_eq!(req.0, *correlation);
                            self.0.push((instance.0, *correlation, *accepted));
                        }
                        AppMsg::BackReply { correlation, resumed } => {
                            assert_eq!(req.0, *correlation);
                            self.0.push((instance.0, *correlation, *resumed));
                        }
                        _ => {}
                    }
                }
            }
        }
        // This existing constructor captures other global store publications. This test is
        // dispatch evidence, not the still-owed lock-free two-Bridge fixture proof.
        let _guard = crate::testlock::serial();
        let mut rig = Bridge::for_test(|| 0);
        let mut dispatcher = Dispatcher::<AppHost>::new();
        let mut replies = Replies(Vec::new());
        for command in [
            Command::RestartWait { phase: crate::auth::Phase::Waiting, qr_generation: 9,
                reply: ReplyTo { instance: 41, correlation: 7 } },
            Command::BackAtRoot { reply: ReplyTo { instance: 42, correlation: 8 } },
        ] {
            dispatcher.emit(MachineId::Nav, Fx::Deliver(MachineId::Session,
                Delivery::Machine(AppMsg::Session(SessionEvent::Command(command)))));
        }
        assert!(replies.0.is_empty());
        dispatcher.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut replies, false);
        assert_eq!(replies.0, [(41, 7, false), (42, 8, false)]);
        assert_eq!(rig.session_adapter.fixture_resources().back_results, [false]);
        assert_eq!(rig.session.read().0.phase, crate::auth::Phase::Idle);
    }

    #[test]
    fn session_frame_read_borrows_the_bridge_publication() {
        use crate::screens::registry::AuthLike;
        let _guard = crate::testlock::serial();
        let mut rig = Bridge::for_test(|| 0);
        let retained = rig.session.publication();
        let parts = CxParts { tick: Tick::default(), press: Default::default(),
            focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
        let split = rig.split();
        let cx = parts.cx::<AppHost>(split.views, split.measure);
        assert!(std::ptr::eq(AppHost::auth(&cx).0, &*retained));
    }
    #[test]
    fn endpoint_outcomes_cross_central_dispatch_machine_bridge_and_boot() {
        let _g = crate::testlock::serial();
        let _session = crate::plex::session::TempSession::new("endpoint-edges");
        crate::plex::reset_servers_for_test();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
        let a = crate::plex::register_for_test("endpoint-a", "127.0.0.1", 9, "synthetic", "cid");
        let b = crate::plex::register_for_test("endpoint-b", "127.0.0.1", 10, "synthetic", "cid");
        crate::plex::describe_server(a, "Synthetic", "Synthetic share", false);
        let expected = [b, a]; // Home's own-first observation order, deliberately not slot order.
        crate::pms::with_refused_fetches_for_test(|| {
            for cmd in [crate::stores::hubs::HubsCmd::RefetchHubs, crate::stores::hubs::HubsCmd::Retry] {
                let _ = crate::stores::take_notices();
                let generation = crate::stores::gen(StoreId::Hubs);
                let outcome = crate::stores::apply(StoreCmd::Hubs(cmd));
                assert!(outcome.changed);
                assert_eq!(outcome.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), expected);
                assert_eq!(crate::stores::take_notices(), [(StoreId::Hubs, generation + 1)]);
            }
            let mut rig = Bridge::for_test(|| 0);
            let parts = CxParts { tick: Tick::default(), press: Default::default(),
                focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
            let mut present = crate::ui::present::Present::default();
            let mut out = Vec::new();
            let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Hubs.ord()), &mut present);
            rig.deliver(MachineId::Store(StoreId::Hubs.ord()),
                &AppMsg::Store(StoreCmd::Hubs(crate::stores::hubs::HubsCmd::Retry)), &parts, &mut fx);
            drop(fx);
            assert_eq!(out.len(), 2, "one command per observed source");
            let mut executed = Vec::new();
            let mut next = Vec::new();
            let mut fx = Effects::new(&mut next, MachineId::Session, &mut present);
            for stamped in out {
                let Fx::App(command @ AppFx::Session(_)) = stamped.fx
                    else { panic!("recovery was not translated into a Session command") };
                rig.app_fx(stamped.from, command, &parts, &mut fx);
            }
            drop(fx);
            for stamped in next {
                let Fx::Deliver(MachineId::Session, Delivery::Machine(AppMsg::Session(
                    crate::auth::owner::SessionEvent::Command(crate::auth::SessionCmd::RequestEndpoint { sid })))) = stamped.fx
                    else { panic!("endpoint effect did not enter the owner's queued delivery path") };
                executed.push(sid);
            }
            assert_eq!(executed, expected);
            crate::stores::viewstate::apply(crate::stores::viewstate::ViewStateCmd::Reset);
            crate::viewstate::owe_hubs_refresh_for_test();
            let split = rig.split();
            let cx = parts.cx::<AppHost>(split.views, split.measure);
            let mut out = Vec::new();
            let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::ViewState.ord()), &mut present);
            crate::stores::viewstate::ViewStateStore.step(
                &crate::stores::StoreEv::Pump { dt: 0.0 }, &cx, &mut fx);
            drop(fx);
            drop(cx);
            assert_eq!(out.len(), 2);
            let actual: Vec<_> = out.into_iter().map(|e| match e.fx {
                Fx::App(AppFx::Session(crate::auth::SessionCmd::RequestEndpoint { sid })) => sid,
                _ => panic!("ViewState pump discarded its recovery outcome"),
            }).collect();
            assert_eq!(actual, expected);
            crate::pms::queue_test_landing(None);
            let result = crate::stores::hubs::take_results().pop().unwrap();
            let mut out = Vec::new();
            let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Hubs.ord()), &mut present);
            rig.deliver(MachineId::Store(StoreId::Hubs.ord()), &AppMsg::HubsResult(result), &parts, &mut fx);
            drop(fx);
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0].fx, Fx::App(AppFx::Session(
                crate::auth::SessionCmd::RequestEndpoint { sid })) if sid == b));
            crate::browse::with_refused_discovery_for_test(|| {
                let client = crate::plex::client_for(a).unwrap();
                crate::browse::queue_discovery_for_test(client, client.token_gen(), false);
                let mut out = Vec::new();
                let mut fx = Effects::new(&mut out, MachineId::Store(StoreId::Browse.ord()), &mut present);
                rig.deliver(MachineId::Store(StoreId::Browse.ord()),
                    &AppMsg::StoreWork(crate::stores::StoreWork::BrowseDiscovery), &parts, &mut fx);
                drop(fx);
                assert_eq!(out.len(), 1);
                assert!(matches!(out[0].fx, Fx::App(AppFx::Session(
                    crate::auth::SessionCmd::RequestEndpoint { sid })) if sid == a));
                let endpoints = crate::app::boot::activate_server();
                let mut boot_executed = Vec::new();
                execute_endpoint_outcomes_with(endpoints, |command| {
                    let crate::auth::SessionCmd::RequestEndpoint { sid } = command
                        else { panic!("boot emitted a non-endpoint command") };
                    boot_executed.push(sid);
                });
                assert_eq!(boot_executed, expected);
            });
        });
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
        crate::plex::reset_servers_for_test();
    }
    include!("library_bookmark_tests.rs");
    include!("library_diagnostic_tests.rs");
    include!("library_query_tests.rs");
    include!("library_deferred_tests.rs");
    include!("library_navigation_tests.rs");
    include!("library_host_freeze_tests.rs");
    include!("library_shelf_action_tests.rs");
    include!("search_publication_tests.rs");
    include!("search_owned_tests.rs");
    include!("detail_panel_tests.rs");
    #[test]
    fn home_requests_keep_the_emitting_instance_and_captured_return_memory() {
        use crate::screens::registry::{HomeGroupKey, HomeHubIdentity, HomeItemIdentity, HomeItemKey, HomeMemory, HomeTab};
        let _guard = crate::testlock::serial();
        let mut rig = Bridge::for_test(|| 0);
        let sid = crate::plex::ServerId::UNSET;
        let groups = vec![HomeHubIdentity::ContinueWatching,
            HomeHubIdentity::Identifier { sid, id: "recent".into() },
            HomeHubIdentity::Key { sid, key: "/library/collections/7/children".into() },
            HomeHubIdentity::Ephemeral { generation: 9, ordinal: 0 }];
        let memory = HomeMemory {
            groups: groups.iter().enumerate().map(|(i, identity)| HomeGroupKey { identity: identity.clone(), group: i as u32 }).collect(),
            items: vec![
                HomeItemKey { elem: 10, identity: HomeItemIdentity::Item { hub: groups[0].clone(), sid, rk: "1".into() }, last_row: 0, last_col: 0 },
                HomeItemKey { elem: 11, identity: HomeItemIdentity::Slot { hub: groups[3].clone(), generation: 9, ordinal: 0 }, last_row: 0, last_col: 0 },
            ], next_elem: 12, next_group: 4, carousel: Some((sid, "1".into())), strip_chosen: false,
            ..Default::default()
        };
        let ret = ReturnState { memory: PageMemory::Home(memory), ..Default::default() };
        let source = MachineId::Instance(InstanceId(20));
        rig.app_return(source, ret.clone());
        let requests = vec![HomeReq::Play { sid, rk: "1".into(), resume_ns: 1_000_000 },
            HomeReq::Detail { sid, rk: "1".into() }, HomeReq::ItemMenu { sid, rk: "1".into() }, HomeReq::Account,
            HomeReq::Tab(HomeTab::Home), HomeReq::Tab(HomeTab::Movies), HomeReq::Tab(HomeTab::Shows), HomeReq::Tab(HomeTab::Search)];
        let parts = CxParts { tick: tick(0), press: Default::default(), focus: Default::default(), owner: InputOwner::Entry(EntryId(1)) };
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, source, &mut present);
        for request in &requests { rig.app_fx(source, AppFx::Home(request.clone()), &parts, &mut fx); }
        let queued = rig.take_home_reqs();
        assert_eq!(queued.len(), requests.len());
        for ((from, request, saved), expected) in queued.iter().zip(requests) {
            assert_eq!(*from, source);
            assert_eq!(*request, expected);
            assert_eq!(saved.memory.hash(), ret.memory.hash());
        }
        assert!(rig.take_home_reqs().is_empty());
        let split = rig.split();
        let cx = parts.cx::<AppHost>(split.views, split.measure);
        assert_eq!(<AppHost as HomeLike>::hubs(&cx).generation, split.views.hubs.generation);
        assert_eq!(crate::screens::registry::word::HOME, "home");
    }

    #[test]
    fn removed_home_items_recover_near_their_old_slot_after_live_or_evicted_return() {
        let _guard = crate::testlock::serial();
        for evict in [false, true] {
            for (row, col, removed_hub, reordered, expected_row, expected_col, expected_rk) in [
                (1, 2, false, false, 1, 2, "4"),
                (1, 3, false, false, 1, 2, "3"),
                (2, 2, true, false, 1, 2, "3"),
                (1, 2, false, true, 1, 2, "1"),
            ] {
                crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
                crate::pms::seed_grid_for_test(3, 4);
                let mut d = Dispatcher::<AppHost>::new();
                let mut rig = Bridge::for_test(|| 0);
                frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
                if reordered { crate::pms::reverse_test_shelves(); }
                frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
                let entry = d.nav.top_page().unwrap().id;
                let instance = d.nav.instance_of(entry).unwrap();
                d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                    Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row, col })))));
                for i in 2..40 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
                let removed_rk = rig.with_home(&d, |home, cx, focus|
                    home.focused_item::<AppHost>(focus, cx).unwrap().rk.clone()).unwrap();
                d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
                let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
                for i in 0..count {
                    d.request(MachineId::Nav, NavOp::Push(AppArg::Library));
                    let report = d.frame_with(&mut rig, tick(40 + i as u32), vec![], vec![], &mut NoTap, false);
                    d.prune(&report.unmounted);
                }
                assert_eq!(d.nav.entry(entry).unwrap().inst.is_none(), evict);
                if removed_hub { crate::pms::seed_grid_for_test(2, 4); }
                else { crate::pms::remove_test_item(&removed_rk); }
                rig.capture_views(&mut d);
                d.request(MachineId::Nav, NavOp::PopTo(entry));
                let report = d.frame_with(&mut rig, tick(80), vec![], vec![], &mut NoTap, false);
                d.prune(&report.unmounted);
                rig.with_home(&d, |home, cx, focus| {
                    assert_eq!(home.grid_position::<AppHost>(focus, cx), Some((expected_row, expected_col)),
                        "evict={evict}, old=({row},{col}), removed_hub={removed_hub}");
                    assert_eq!(home.focused_item::<AppHost>(focus, cx).unwrap().rk, expected_rk);
                }).unwrap();
            }
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn mounted_home_navigation_stays_inside_a_full_or_oversized_catalog() {
        let _guard = crate::testlock::serial();
        for offered in [crate::pms::MAX_SHELVES, crate::pms::MAX_SHELVES + 5] {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::pms::seed_grid_for_test(offered, 3);
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
            frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
            let mut i = 2;
            for row in 0..crate::pms::MAX_SHELVES {
                frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Down, tick(i)));
                i += 1;
                assert_eq!(rig.with_home(&d, |home, cx, focus| home.grid_position::<AppHost>(focus, cx)),
                    Some(Some((row, 0))), "offered={offered}, row={row}");
            }
            let last = d.focus();
            for _ in 0..3 {
                frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Down, tick(i)));
                i += 1;
                assert_eq!(d.focus(), last, "DOWN cannot escape the capped final shelf");
            }
            let instance = d.nav.instance_of(last.unwrap().entry).unwrap();
            for (row, col) in [(usize::MAX, 0), (0, usize::MAX)] {
                d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                    Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row, col })))));
                frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]);
                i += 1;
                assert_eq!(d.focus(), last, "an invalid addressed request must not displace focus");
            }
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn hero_edge_keys_page_without_seating_a_pager_or_leaving_the_control() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        let selected = |rig: &Bridge, d: &Dispatcher<AppHost>| rig.with_home(d,
            |home, cx, _| home.hero_item::<AppHost>(cx).unwrap().rk.clone()).unwrap();
        let mut i = 2;
        for (direction, control) in [(Key::Left, 0), (Key::Right, 1), (Key::Right, 1)] {
            if control == 1 && d.focus().unwrap().elem == 0 {
                frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Right, tick(i)));
                i += 1;
            }
            let before = selected(&rig, &d);
            frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(direction, tick(i)));
            i += 1;
            assert_eq!(d.focus().unwrap().elem, control);
            assert_ne!(selected(&rig, &d), before, "the delivered edge must page, not just report an edge");
            rig.with_home(&d, |home, cx, _| {
                let mut draw = DrawFrame::new(cx, crate::ui::Painter::root());
                home.record_stops(&mut draw, cx.views.hubs);
                let mut keys: Vec<_> = draw.into_stops().into_iter().map(|s| s.key.elem).collect();
                keys.sort();
                assert_eq!(keys, vec![0, 1], "pager chevron/dots are not hit stops");
            }).unwrap();
            for _ in 0..30 {
                frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]);
                i += 1;
            }
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn a_removed_home_type_tab_recovers_to_home_not_the_profile_chip() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        let entry = d.nav.top_page().unwrap().id;
        let movies = crate::screens::home::STRIP_MOVIES_ELEM;
        d.nav.tabs.strip.push(crate::ui::containers::tabs::StripMember::new(movies,
            crate::ui::Rect::new(800.0, 50.0, 160.0, 60.0)));
        d.set_focus_in(Some(FocusKey { entry, elem: movies }), Some(crate::ui::containers::tabs::STRIP));
        // Republish the current empty-library strip: the previous Movies destination is gone.
        frame(&mut d, &mut rig, AppArg::Home, tick(2), vec![]);
        assert!(!d.nav.tabs.strip.iter().any(|member| member.elem == movies));
        assert_eq!(d.focus(), Some(FocusKey { entry, elem: crate::screens::home::STRIP_HOME_ELEM }));
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn home_return_restores_the_offscreen_item_and_viewport_after_retention_or_eviction() {
        let _guard = crate::testlock::serial();
        for (evict, reorder) in [(true, false), (false, false), (true, true), (false, true)] {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::pms::seed_grid_for_test(6, 24);
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
            d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
            let home = d.nav.top_page().unwrap().id;
            let instance = d.nav.instance_of(home).unwrap();
            d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 4, col: 18 })))));
            for i in 1..80 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
            for i in 80..84 { frame(&mut d, &mut rig, AppArg::Home, tick(i), script_key(Key::Left, tick(i))); }
            for i in 84..160 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
            let focus = d.focus().unwrap();
            let before = rig.with_home(&d, |s, cx, f| {
                assert_eq!(s.grid_position::<AppHost>(f, cx), Some((4, 14)));
                s.focused_rect::<AppHost>(f, cx, At::Drawn).unwrap()
            }).unwrap();
            let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
            for i in 0..count {
                d.request(MachineId::Nav, NavOp::Push(AppArg::Library));
                let report = d.frame_with(&mut rig, tick(160 + i as u32), vec![], vec![], &mut NoTap, false);
                d.prune(&report.unmounted);
            }
            assert_eq!(d.nav.entry(home).unwrap().inst.is_none(), evict);
            if reorder {
                crate::pms::reverse_test_hubs();
                rig.capture_views(&mut d);
            }
            d.request(MachineId::Nav, NavOp::PopTo(home));
            for i in 200..280 {
                let report = d.frame_with(&mut rig, tick(i), vec![], vec![], &mut NoTap, false);
                d.prune(&report.unmounted);
                if i == 200 {
                    rig.with_home(&d, |s, cx, _| {
                        let mut draw = DrawFrame::new(cx, crate::ui::Painter::root());
                        s.record_stops(&mut draw, cx.views.hubs);
                        let stops = draw.into_stops();
                        let stop = stops.iter().find(|stop| stop.key == focus)
                            .expect("the restored card must be interactive on the first returned frame");
                        assert!(stop.rect.x < stop.clip.x + stop.clip.w && stop.rect.x + stop.rect.w > stop.clip.x
                            && stop.rect.y < stop.clip.y + stop.clip.h && stop.rect.y + stop.rect.h > stop.clip.y,
                            "the restored stop must actually be onscreen: x={} y={}", stop.rect.x, stop.rect.y);
                    }).unwrap();
                }
            }
            assert_eq!(d.focus(), Some(focus));
            let after = rig.with_home(&d, |s, cx, f| {
                assert_eq!(s.snap_target(), 1.0, "restored grid must not remain behind the hero");
                s.focused_rect::<AppHost>(f, cx, At::Drawn).unwrap()
            }).unwrap();
            assert!((before.x - after.x).abs() < 1.0, "horizontal viewport changed: {} -> {}", before.x, after.x);
            if !reorder {
                assert!((before.y - after.y).abs() < 1.0, "vertical viewport changed: {} -> {}", before.y, after.y);
            }
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn library_detail_return_restores_engine_card_and_viewport_after_stack_eviction() {
        let _guard = crate::testlock::serial();
        struct RegistryCleanup;
        impl Drop for RegistryCleanup {
            fn drop(&mut self) { crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset); crate::plex::reset_servers_for_test(); }
        }
        let _cleanup = RegistryCleanup;
        let session = crate::plex::session::TempSession::new("owned-library-return");
        session.watching("u-owned-library-return");
        for evict in [false, true] {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
            let sid = crate::plex::register_for_test("library-return-own", "127.0.0.1", 9, "synthetic", "fixture");
            let shared = crate::plex::register_for_test("library-return-shared", "127.0.0.1", 10, "synthetic", "fixture");
            crate::plex::set_current(sid);
            crate::browse::seed_registered_table_for_test([sid, shared]);
            let mut directory = crate::stores::browse::DirectorySnapshot::default();
            directory.capture();
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
            crate::browse::seed_items_for_test(120);
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
            d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
            Bridge::library_command(&mut d, crate::screens::registry::LibraryCmd::FocusGrid { row: 8, col: 4 });
            for i in 1..80 { frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]); }
            let entry = d.nav.top_page().unwrap().id;
            let focus = d.focus().unwrap();
            let (item, opener) = rig.library_selection(&d, entry, Some(focus)).unwrap_or_else(|| panic!(
                "fixture must reach grid: focus={focus:?}, listing={:?}, total={}, current={:?}",
                rig.listing.view().id(), rig.listing.view().total(), rig.directory.view().current()));
            let before = opener.rect.unwrap();
            let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
            for i in 0..count {
                d.request(MachineId::Nav, NavOp::Push(AppArg::Content(ContentArg::Detail {
                    sid: item.sid, rk: if i == 0 { item.rk.clone() } else { format!("return-{i}") },
                })));
                let report = d.frame_with(&mut rig, tick(80 + i as u32), vec![], vec![], &mut NoTap, false);
                d.prune(&report.unmounted);
            }
            assert_eq!(d.nav.entry(entry).unwrap().inst.is_none(), evict);
            d.request(MachineId::Nav, NavOp::PopTo(entry));
            for i in 100..180 {
                let report = d.frame_with(&mut rig, tick(i), vec![], vec![], &mut NoTap, false);
                d.prune(&report.unmounted);
                assert_eq!(d.focus(), Some(focus), "every post-return frame keeps the engine card; evicted={evict}");
                let (returned, opener) = rig.library_selection(&d, entry, d.focus()).unwrap();
                assert_eq!((returned.sid, returned.rk.as_str()), (item.sid, item.rk.as_str()));
                let after = opener.rect.unwrap();
                assert!((before.x - after.x).abs() < 0.01 && (before.y - after.y).abs() < 0.01,
                    "return geometry changed: {before:?} -> {after:?}, evicted={evict}, frame={i}");
            }
        }
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    }

    #[test]
    fn library_publishes_the_actual_container_strip() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();
        let mut dispatcher = Dispatcher::<AppHost>::new();
        let mut bridge = Bridge::for_test(|| 0);
        super::show_page(&mut dispatcher, AppArg::Library);
        bridge.capture_chrome(&mut dispatcher);
        let base = crate::ui::dispatch::STRIP_BASE;
        assert_eq!(dispatcher.nav.tabs.strip.iter().map(|member| member.elem).collect::<Vec<_>>(),
            vec![base + 4, base, base + 1, base + 2, base + 3]);
        assert_eq!(dispatcher.nav.tabs.strip_fallback, Some(base));
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    }

    #[test]
    fn all_splits_in_one_frame_keep_the_same_library_listing() {
        use crate::ui::dispatch::Rig;
        let _guard = crate::testlock::serial();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(3);
        crate::browse::section_hubs::seed_shelves_for_test(crate::browse::cur(), &["shelves"], 3);
        let mut rig = super::Bridge::for_test(|| 0);
        let id = rig.split().views.listing.id();
        {
            let split = rig.split();
            assert_eq!(split.views.listing.total(), 3);
            assert_eq!(split.views.directory.sections().len(), 4);
            assert_eq!(split.views.section_hubs.shelves()[0].items.len(), 3);
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            assert!(split.views.listing.item(2).is_some());
            assert_eq!(split.views.section_hubs.shelves()[0].items.len(), 3);
            assert_eq!(split.views.directory.sections().len(), 4);
        }
        assert_eq!(rig.split().views.listing.id(), id);
        assert_eq!(rig.split().views.listing.total(), 3);
        assert_eq!(rig.split().views.section_hubs.shelves()[0].items.len(), 3);
        assert_eq!(rig.split().views.directory.sections().len(), 4);
        let mut dispatcher = Dispatcher::<AppHost>::new();
        rig.capture_views(&mut dispatcher);
        assert!(rig.split().views.listing.id().is_none());
        assert_eq!(rig.split().views.listing.total(), -1);
        assert!(rig.split().views.section_hubs.id().is_none());
        assert!(rig.split().views.directory.sections().is_empty());
    }

    #[test]
    fn all_splits_in_one_frame_keep_the_same_home_publication() {
        use crate::ui::dispatch::Rig;
        let _guard = crate::testlock::serial();
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut rig = super::Bridge::for_test(|| 0);
        {
            let split = rig.split();
            assert_eq!(split.views.hubs.hub(0).unwrap().items.len(), 3);
            crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
            assert_eq!(split.views.hubs.hub(0).unwrap().items.len(), 3);
        }
        assert_eq!(rig.split().views.hubs.hub_count(), 1,
            "a post-step draw must not pair new data with the old element projection");
        let mut dispatcher = Dispatcher::<AppHost>::new();
        rig.capture_views(&mut dispatcher);
        assert_eq!(rig.split().views.hubs.hub_count(), 0, "the next frame adopts the publication");
    }

    #[test]
    fn removing_the_pressed_home_item_cancels_instead_of_activating_its_replacement() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), script_key(Key::Down, tick(1)));
        for i in 2..40 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
        let pressed = d.focus().unwrap();
        let mut down = script_key(Key::Ok, tick(40));
        down.truncate(1);
        frame(&mut d, &mut rig, AppArg::Home, tick(40), down);
        assert_eq!(d.input.arm.unwrap().key, pressed);
        crate::pms::remove_test_item("1");
        frame(&mut d, &mut rig, AppArg::Home, tick(41), vec![]);
        assert_ne!(d.focus(), Some(pressed));
        frame(&mut d, &mut rig, AppArg::Home, tick(42), vec![release_input(tick(42))]);
        for i in 43..80 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
        assert!(rig.take_home_reqs().is_empty(), "a removed arm must not become a press on the replacement cursor");
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn a_midframe_reorder_keeps_painted_keys_matched_and_a_click_activates_the_seen_item() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), script_key(Key::Down, tick(1)));
        for i in 2..40 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
        let focus = d.focus().unwrap();
        let (stops, rect) = rig.with_home(&d, |home, cx, _| {
            assert_eq!(home.focused_item::<AppHost>(Some(focus), cx).unwrap().rk, "1");
            let mut draw = DrawFrame::new(cx, crate::ui::Painter::root());
            home.record_stops::<AppHost>(&mut draw, cx.views.hubs);
            (draw.into_stops(), home.focused_rect::<AppHost>(Some(focus), cx, At::Drawn).unwrap())
        }).unwrap();
        d.input.hit.fill(stops);
        d.input.hit.swap();

        crate::pms::reverse_test_shelves();
        assert_eq!(crate::pms::hub_item(0, 0).unwrap().rk, "3");
        let _ = rig.split(); // the subsequent draw still belongs to this frame's publication
        assert_eq!(rig.with_home(&d, |home, cx, _| home.focused_item::<AppHost>(Some(focus), cx).unwrap().rk.clone()), Some("1".into()));

        frame(&mut d, &mut rig, AppArg::Home, tick(40), vec![click_input(rect.cx(), rect.cy(), tick(40))]);
        frame(&mut d, &mut rig, AppArg::Home, tick(41), vec![release_input(tick(41))]);
        for i in 42..60 { frame(&mut d, &mut rig, AppArg::Home, tick(i), vec![]); }
        let requests = rig.take_home_reqs();
        assert!(requests.iter().any(|(_, request, _)| matches!(request, HomeReq::Play { rk, .. } if rk == "1")),
            "the old presented map named item 1, not the replacement now at its old position");
        assert!(!requests.iter().any(|(_, request, _)| matches!(request, HomeReq::Play { rk, .. } if rk == "3")));
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn home_worker_results_cross_the_addressed_dispatcher_ingest_once() {
        use crate::ui::dispatch::Tap;
        use crate::ui::machine::Addr;
        #[derive(Default)]
        struct Results(Vec<Addr>);
        impl Tap<AppHost> for Results {
            fn result(&mut self, _f: u64, addr: &Addr, msg: &AppMsg) {
                let AppMsg::HubsResult(result) = msg else { panic!("wrong result type") };
                assert_eq!(addr.req.0, result.request_id());
                self.0.push(*addr);
            }
        }
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        let mut tap = Results::default();
        let req = crate::pms::queue_test_landing(Some(5));
        frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(0), vec![], &mut tap);
        assert_eq!(crate::pms::hub_len(0), 5);
        assert_eq!(tap.0, vec![Addr {
            to: MachineId::Store(StoreId::Hubs.ord()), req: crate::ui::machine::RequestId(req),
        }]);
        frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(1), vec![], &mut tap);
        assert_eq!(tap.0.len(), 1, "the store tick must not re-deliver the result");

        crate::pms::queue_test_landing(Some(9));
        let result = crate::stores::hubs::take_results().pop().unwrap();
        let parts = CxParts { tick: tick(2), press: Default::default(), focus: Default::default(),
            owner: crate::ui::machine::InputOwner::Entry(EntryId(0)) };
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Nav, &mut present);
        assert_eq!(rig.deliver(MachineId::Store(StoreId::Search.ord()),
            &AppMsg::HubsResult(result), &parts, &mut fx), Handled::No);
        assert_eq!(crate::pms::hub_len(0), 5, "a misaddressed result must not apply");
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn supplied_home_results_use_the_dispatcher_without_consuming_live_arrivals() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
        crate::pms::queue_test_landing(Some(5));
        let captured = take_hubs_results().pop().unwrap();
        let AppMsg::HubsResult(result) = captured.1 else { unreachable!() };
        let payload = crate::pms::record::encode(&result);
        let decoded = crate::pms::record::decode(payload, |_| None).unwrap();
        crate::pms::queue_test_landing(Some(9));

        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame_with_results(&mut d, &mut rig, AppArg::Home, tick(0), vec![],
            || vec![(captured.0, AppMsg::HubsResult(decoded))], &mut NoTap);
        assert_eq!(crate::pms::hub_len(0), 5, "the decoded result reaches the actual store");
        frame_with_results(&mut d, &mut rig, AppArg::Home, tick(1), vec![],
            Vec::new, &mut NoTap);
        assert_eq!(crate::pms::hub_len(0), 5, "an empty supplied frame cannot fall back to live data");
        frame_with_tap(&mut d, &mut rig, AppArg::Home, tick(2), vec![], &mut NoTap);
        assert_eq!(crate::pms::hub_len(0), 9, "the live arrival was preserved for a live ingest");
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    #[test]
    fn store_work_is_addressed_and_idle_polling_does_not_invent_a_change() {
        use crate::stores::StoreWork;
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::pms::seed_for_test(0, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        let before = crate::stores::gen(StoreId::Hubs);
        d.emit(MachineId::Nav, Fx::App(AppFx::StoreWork(StoreWork::Hubs)));
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Hubs), before);

        let parts = CxParts { tick: tick(2), press: Default::default(), focus: Default::default(),
            owner: crate::ui::machine::InputOwner::Entry(EntryId(0)) };
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Nav, &mut present);
        for work in [StoreWork::Hubs, StoreWork::BrowseDiscovery] {
            assert_eq!(rig.deliver(MachineId::Store(StoreId::Search.ord()),
                &AppMsg::StoreWork(work), &parts, &mut fx), Handled::No);
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed;
    }

    use super::*;
    use crate::ui::machine::Chrome;
    use crate::ui::screen::ScreenArg;

    /// **Put `route` on top, then run one frame** — the TEST driver that replaced `sync_page`.
    ///
    /// It is deliberately the same three-line decision `sync_page` made every frame in production
    /// (reuse an entry that is already on the stack, otherwise root a peer or stack a page), kept
    /// here so that ~240 existing assertions still read as "drive a frame on this screen". What
    /// changed is WHO says it: production asks for the op it wants at the press, and no frame
    /// derives a navigation from a second copy of the route.
    fn goto(d: &mut Dispatcher<AppHost>, want: AppArg) {
        if d.has_pending_navigation() { return; }
        super::show_page(d, want);
    }

    fn frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick, inputs: Vec<InputEvent<u32>>) -> (&'static str, FrameReport) {
        goto(d, route);
        super::frame(d, rig, tick, inputs)
    }

    /// …and the same driver for the two frames that also want the effect tap or a supplied result
    /// set. Only the `goto` is the test's: everything after it is production's own frame.
    fn frame_with_tap(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
        inputs: Vec<InputEvent<u32>>, tap: &mut dyn crate::ui::dispatch::Tap<AppHost>)
        -> (&'static str, FrameReport) {
        goto(d, route);
        super::frame_with_tap(d, rig, tick, inputs, tap)
    }

    fn frame_with_results(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
        inputs: Vec<InputEvent<u32>>, take: impl FnOnce() -> AppResults,
        tap: &mut dyn crate::ui::dispatch::Tap<AppHost>) -> (&'static str, FrameReport) {
        goto(d, route);
        super::frame_with_results(d, rig, tick, inputs, take, tap)
    }

    /// A detail page's argument, for the tests that used to name `Route::Detail` and let a trail
    /// supply the identity. The fold puts the identity ON the argument, which is what makes the
    /// trail seeding this helper used to do unnecessary.
    fn detail_arg(rk: &str) -> AppArg {
        AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: rk.into() })
    }

    /// …and a person page's.
    fn person_arg(key: &str) -> AppArg {
        AppArg::Content(ContentArg::Person {
            sid: crate::plex::ServerId::UNSET,
            key: key.into(),
            guid: format!("tag://{key}"),
            name: String::new(),
            thumb: String::new(),
        })
    }

    #[test]
    fn content_instances_compare_item_identity_and_keep_distinct_entries() {
        let a = AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: "1001".into() });
        let b = AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: "1002".into() });
        assert_eq!(a.id(), b.id());
        assert!(!a.same_instance(&b));
        assert!(a.same_instance(&a.clone()));
        assert!(!a.same_instance(&AppArg::Home), "a page with an identity is never a bare page");
    }

    fn tick(i: u32) -> Tick {
        Tick {
            ms: i * 16,
            dt_us: 16_000,
        }
    }

    // One list, in `app/mod.rs` beside `route_word` — it was a second copy of the same nine
    // variants here, and phase 10 deleted a route from both.
    use super::super::words::every_route;

    #[test]
    fn a_page_arg_wears_the_chrome_its_own_table_says() {
        for r in every_route() {
            let want = if matches!(r, AppArg::Home | AppArg::Library | AppArg::Search) {
                Chrome::TabBar
            } else {
                Chrome::None
            };
            assert_eq!(r.chrome(), want, "{}", super::super::words::route_word(&r));
        }
        assert_eq!(AppArg::Settings(SettingsPage::Root).chrome(), Chrome::None);
        // …and the root payload is a boot address, never an identity: the two Settings arguments
        // below are ONE screen, which is what stops a dev boot target minting a second surface.
        assert!(AppArg::Settings(SettingsPage::Root).same_instance(&AppArg::Settings(SettingsPage::Legal)));
        assert!(!AppArg::Settings(SettingsPage::Root).same_instance(&AppArg::FirstRunConsent(0)));
    }

    /// **Every route mounts a screen that NAMES the heartbeat word** (§15.2).
    ///
    /// This was an assertion over the retired route-word page's `name()` — a claim about a type
    /// nothing mounted, which by phase 10 graded the one screen impl that was never on screen.
    /// The intent moves onto the screens that ARE mounted: the mounter is asked for each route's
    /// argument and the instance it builds must answer with the word `route_word` gives that
    /// route. It is graded against `route_word(r)` and not against `route_word(page_of(r))`
    /// because `page_of` is gone with the two routes that made it more than the identity: since
    /// phase 10 the profile and card menus are SURFACES, their words are the overlay alphabet's
    /// (`overlay_word`), and `every_route()` is nine pages with nothing to fold.
    #[test]
    fn every_route_mounts_a_screen_that_names_the_heartbeat_word() {
        let _g = crate::testlock::serial();
        for r in every_route() {
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            let (word, _) = frame(&mut d, &mut rig, r.clone(), tick(0), vec![]);
            assert_eq!(
                word,
                super::super::words::route_word(&r),
                "{} mounted {:?}",
                super::super::words::route_word(&r),
                d.top_screen().map(|s| s.name())
            );
        }
    }

    fn notices(d: &Dispatcher<AppHost>) -> String {
        let mut s = String::new();
        if let Some(sc) = d.top_screen() {
            sc.state().probe(&mut s);
        }
        s
    }

    #[test]
    fn a_store_command_through_the_dispatcher_steps_the_store_and_notifies_the_page() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        let _ = crate::stores::take_notices();
        // **This used to also assert the retired route-word page's generic notice COUNTER, and
        // cannot any more.** Search left that population with the phase 7 cutover
        // and `Player` with phase 9, and `Account`/`ItemMenu` never had a page of their own —
        // `page_of` maps each to its host, which is an owned screen. So what this grades is the
        // half that is still observable here: a store command emitted as an `Fx::App` is applied
        // in the DRAIN, exactly once, and a direct `apply` is not applied a second time by it.
        // The delivery of the resulting `StoreChanged` to a page is graded where the page can be
        // seen — `ui::dispatch`'s own fixture pages, and each owned screen's store arm.
        let route = AppArg::Player;
        frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
        assert_eq!(d.top_screen().map(|s| s.name()), Some("player"));
        let before = crate::stores::gen(StoreId::Search);
        d.emit(
            MachineId::Nav,
            Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
        );
        frame(&mut d, &mut rig, route.clone(), tick(1), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Search), before + 1, "the store was stepped in the drain");
        frame(&mut d, &mut rig, route.clone(), tick(2), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Search), before + 1, "and exactly once");
        let g = crate::stores::gen(StoreId::Search);
        d.emit(
            MachineId::Nav,
            Fx::Deliver(
                MachineId::Store(StoreId::Browse.ord()),
                Delivery::Machine(AppMsg::Store(StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
            ),
        );
        frame(&mut d, &mut rig, route.clone(), tick(4), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Search), g);
    }

    /// **A profile switch must leave the container holding nothing of the profile before it.**
    ///
    /// RED FIRST (D1). Until this landed, `Dispatcher::reset_for_profile` had NO production caller
    /// anywhere in the tree: the switch worked only because the loop wrote `route = Profiles` and
    /// `sync_page` turned that into a `NavOp::Root(Profiles)` — and `NavStack::apply`'s `Root` arm
    /// unwinds everything ABOVE the root while leaving the previous root COVERED and alive, memory
    /// and all. So the outgoing profile's Home entry survived the switch, still holding its
    /// `ReturnState` and (until eviction) its body. That is a privacy-shaped defect rather than a
    /// cosmetic one, and deleting `sync_page` without wiring the reset explicitly would have
    /// preserved it silently.
    ///
    /// Observed RED against `switch_profile` without its `reset_for_profile` call: depth 2 with
    /// `home` still at the bottom of the stack.
    #[test]
    fn switching_profile_leaves_the_container_holding_nothing_of_the_previous_profile() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, detail_arg("1001"), tick(1), vec![]);
        assert_eq!(d.nav.tabs.stack.depth(), 2, "the outgoing profile browsed two pages deep");
        let before: Vec<_> = d.nav.tabs.stack.entries.iter().map(|e| e.id).collect();

        switch_profile(&mut d);
        frame(&mut d, &mut rig, AppArg::Profiles, tick(2), vec![]);

        assert_eq!(
            d.nav.tabs.stack.depth(), 1,
            "the picker is the only entry: {:?}",
            d.nav.tabs.stack.entries.iter().map(|e| e.arg.id()).collect::<Vec<_>>(),
        );
        assert!(d.nav.top_page().is_some_and(|e| e.arg == AppArg::Profiles));
        for id in before {
            assert!(
                !d.nav.tabs.stack.entries.iter().any(|e| e.id == id),
                "an entry of the previous profile is still on the stack",
            );
        }
        assert!(d.nav.modals.surfaces.is_empty(), "…and no surface of it either");
    }

    /// **An app switch parks the tree; it does not tear the session's page history down.**
    ///
    /// RED FIRST (D1). The 0x103/0x106 lifecycle used to `suspend()` the tree and then write
    /// `route = Home`, which `sync_page` turned into a `NavOp::Root(Home)` — unwinding the player
    /// entry AND the detail page it was launched from — and the foreground arm wrote
    /// `route = Player`, minting a FRESH entry over a stack that was now just Home. The playback
    /// SESSION survived that (it is `player::machine`'s, not the container's) and `App.play_from`
    /// survived it (a separate field), which is exactly why nothing caught it: the two pieces of
    /// state that would have noticed were both mirrors kept outside the tree.
    ///
    /// With one authority they are not, so the park has to be real. `PlayerScreen.origin` is the
    /// entry beneath the player, and an exit is a `PopTo` of it — both of which are meaningless if
    /// a background destroys the entry.
    ///
    /// Observed RED before the lifecycle arms stopped writing a route: after foreground the stack
    /// was `[home, player]` with two fresh ids, so the origin an exit would pop to was Home
    /// rather than the detail page the session was launched from.
    #[test]
    fn an_app_switch_parks_the_page_stack_and_gives_the_same_entries_back() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, detail_arg("1001"), tick(1), vec![]);
        let origin = d.nav.top_page().map(|e| e.id).expect("the page the session is launched from");
        frame(&mut d, &mut rig, AppArg::Player, tick(2), vec![]);
        let player = d.nav.top_page().map(|e| e.id).expect("the player page");
        assert_eq!(d.nav.tabs.stack.depth(), 3);

        // BACKGROUND (0x103/0x104): the tree is parked. Nothing about the page stack moves.
        background(&mut d);
        frame(&mut d, &mut rig, AppArg::Player, tick(3), vec![]);
        assert!(d.nav.suspended, "the tree knows it is parked");
        assert_eq!(d.nav.tabs.stack.depth(), 3, "a park is not a teardown");

        // FOREGROUND (0x105/0x106).
        foreground(&mut d);
        frame(&mut d, &mut rig, AppArg::Player, tick(4), vec![]);
        assert!(!d.nav.suspended);
        assert_eq!(
            d.nav.top_page().map(|e| e.id), Some(player),
            "the SAME player entry came back, not a fresh one",
        );
        assert_eq!(
            d.nav.tabs.stack.under_top().map(|e| e.id), Some(origin),
            "…standing on the page it was launched from, so an exit still lands there",
        );
    }

    /// **Every test in this module that calls [`frame`] must hold `testlock::serial()`, even when
    /// it asserts nothing about the stores.** `frame`'s second act is
    /// `crate::stores::take_notices()`, which DRAINS a process-global dirty flag — so a test that
    /// merely walks the nav tree still consumes whatever notice another test was about to observe.
    /// This one had no guard until 2026-09-07 and stole
    /// `a_store_command_through_the_dispatcher_steps_the_store_and_notifies_the_page`'s notice at
    /// roughly one run in five, which surfaced there as `home notices=0` — a failure in the other
    /// test, in the other direction, that reads exactly like an ordering bug in the bridge and is
    /// not one. Note that `Bridge`'s `Drop` is NOT the answer to this class (see its doc): a leaked
    /// counter outlives a guard, whereas a drained notice is a pure interleaving and the lock is
    /// precisely what fixes it. **The rule is asserted now, not merely written here** — see
    /// [`frame_with_results`], which every frame passes through; it was stated in this comment for
    /// a month and broken from `app::mod` anyway, through `every_surface_word`.
    #[test]
    fn route_flips_preserve_content_and_player_origin_entries() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        assert_eq!(frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]).0, "home");
        assert_eq!(d.nav.tabs.stack.depth(), 1);
        let home = d.nav.top_page().map(|e| e.id);
        assert_eq!(frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]).0, "home");
        assert_eq!(d.nav.top_page().map(|e| e.id), home, "a steady route mints nothing");
        assert_eq!(frame(&mut d, &mut rig, detail_arg("1001"), tick(2), vec![]).0, "detail");
        assert_eq!(d.nav.tabs.stack.depth(), 2, "Detail preserves its legacy Home origin");
        assert_ne!(d.nav.top_page().map(|e| e.id), home);
        let detail = d.nav.top_page().map(|e| e.id);
        let body = d.top_page();
        assert_eq!(
            frame(&mut d, &mut rig, AppArg::Player, tick(3), vec![]).0,
            "player"
        );
        assert_eq!(d.top_screen().map(|s| s.render()), Some(crate::ui::screen::RenderStrategy::VideoPlane));
        assert_eq!(d.nav.tabs.stack.depth(), 3);
        frame(&mut d, &mut rig, detail_arg("1001"), tick(4), vec![]);
        assert_eq!(d.nav.top_page().map(|e| e.id), detail);
        assert_eq!(d.top_page(), body, "player return uncovers the same Detail instance");
    }

    /// **Leaving the player while one of its panels is still up** — EOS, the Stop key, or an Info
    /// press that navigates — and the page has to follow the route in the SAME frame.
    ///
    /// `playback::exit_player` performs its two acts before the loop's `bridge::frame`: it parks a
    /// `NavOp::Dismiss` for every open panel (`dismiss_player_overlays`) and then flips the route
    /// to the origin page. Both reach the dispatcher through the one queue, so the frame that
    /// carries the new route also carries a parked SURFACE op — which `sync_page`'s guard read as
    /// "a page navigation is already in flight" and skipped the page sync for, leaving the tree's
    /// top page on `player` under a route that already said `home`.
    ///
    /// Reproduced on the simulator by `tests/player_shots.sh`, whose clip reaches EOS with the Info
    /// card open: `assertion failed: the tree's top page names the committed route, left: "player",
    /// right: "home"` at `frame_with_results`'s `debug_assert_eq!`.
    #[test]
    fn leaving_the_player_with_a_panel_up_returns_the_page_in_the_route_s_own_frame() {
        use crate::screens::player::overlay::OverlayKind;
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        let home = d.nav.top_page().map(|e| e.id);
        assert_eq!(frame(&mut d, &mut rig, AppArg::Player, tick(1), vec![]).0, "player");
        open_player_overlay(crate::route::idle_session_for_test(), &mut d, OverlayKind::Info);
        frame(&mut d, &mut rig, AppArg::Player, tick(2), vec![]);
        assert!(player_overlay_up(&d), "the panel is on the player page's own ModalStack");
        // …`exit_player`'s own order: the panels are dismissed, then the route is the origin's.
        dismiss_player_overlays(&mut d);
        assert_eq!(frame(&mut d, &mut rig, AppArg::Home, tick(3), vec![]).0, "home");
        assert_eq!(d.nav.top_page().map(|e| e.id), home, "…and it is the origin page, not a new one");
        assert!(!player_overlay_up(&d), "the panel left with the page that hosted it");
    }

    /// Phase 5b: the Settings surface is PRESENTED on the tree, owns input from its first frame,
    /// names the heartbeat word of its top page, walks its own stack on BACK (root → Legal →
    /// back → root) and only then lets the container dismiss it — with the app's page untouched.
    #[test]
    fn the_settings_surface_owns_input_and_walks_its_own_stack() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        assert!(d.owns_input(), "Home is an owned page too");
        let home_owner = d.nav.input_owner();
        open_settings(&mut d);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        assert!(d.owns_input());
        assert_eq!(overlay_word(&d), Some("settings"));
        assert_ne!(d.nav.input_owner(), home_owner, "Settings takes input from its Home host");
        assert!(host_frozen(&d));
        // DOWN, DOWN to Legal notices (Favourites is absent signed out: Privacy, Legal, About)
        let mut t = 2;
        let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
            let ev = script_key(key, tick(t));
            frame(d, rig, AppArg::Home, tick(t), ev);
            t += 1;
            frame(d, rig, AppArg::Home, tick(t), vec![]);
            t += 1;
        };
        press(&mut d, &mut rig, Key::Down);
        press(&mut d, &mut rig, Key::Ok);
        assert_eq!(overlay_word(&d), Some("legal"), "OK on Legal notices pushed the index");
        press(&mut d, &mut rig, Key::Back);
        assert_eq!(overlay_word(&d), Some("settings"), "BACK popped the inner stack");
        assert!(settings_up(&d));
        press(&mut d, &mut rig, Key::Back);
        assert_eq!(
            d.nav.modals.top().map(|s| s.phase),
            Some(Phase::Closing),
            "BACK at the surface's own root dismisses it"
        );
        assert_eq!(d.nav.tabs.stack.depth(), 1, "the app's page never moved");
    }

    /// **Phase 10: the profile menu is a SURFACE over the page whose chip was pressed.**
    ///
    /// `Route::Account { over: BarHost }` existed to answer three questions the container answers
    /// for free, and this test is those three: the page under the panel does not change, the panel
    /// owns input, and the heartbeat names the pair as `route=<host> overlay=account` rather than
    /// as one route word for three different screens. Driven over all three bar-wearing hosts,
    /// because the whole reason the route grew an `over` field was that a press on the Library's
    /// chip used to cut the page underneath to Home.
    #[test]
    fn the_profile_menu_is_a_surface_over_the_page_whose_chip_was_pressed() {
        let _g = crate::testlock::serial();
        for (route, word) in [
            (AppArg::Home, "home"),
            (AppArg::Library, "library"),
            (AppArg::Search, "search"),
        ] {
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
            let host = d.nav.top_page().expect("a host page").id;
            let host_owner = d.nav.input_owner();
            open_account_menu(&mut d);
            let (top, _) = frame(&mut d, &mut rig, route.clone(), tick(1), vec![]);

            assert!(account_menu_up(&d), "{word}: the chip's press presented the menu");
            assert_eq!(
                d.nav.top_page().map(|e| e.id),
                Some(host),
                "{word}: a surface is presented OVER the top page and never replaces it"
            );
            assert_eq!(top, word, "{word}: the heartbeat's route= is still the HOST's word");
            assert_eq!(overlay_word(&d), Some("account"));
            assert_ne!(
                d.nav.input_owner(),
                host_owner,
                "{word}: the menu takes input from its host"
            );
            // `Style::Sheet` — the page beneath is frozen AND cached, which is what
            // `host_page_updates`'s deleted `Route::Account` arm and `Popover::caching_host()`
            // used to say in two places.
            assert_eq!(d.host_policy(), (HostUpdate::Frozen, HostRender::Cached), "{word}");

            // …and BACK dismisses the surface without moving the page.
            let ev = script_key(Key::Back, tick(2));
            frame(&mut d, &mut rig, route.clone(), tick(2), ev);
            assert_eq!(
                d.nav.modals.top().map(|s| s.phase),
                Some(Phase::Closing),
                "{word}: BACK dismisses the menu"
            );
            assert_eq!(d.nav.top_page().map(|e| e.id), Some(host), "{word}: …and nothing else");
        }
    }

    /// **The card menu is a surface over the page the hold happened on, and the page stays put.**
    ///
    /// `Route::ItemMenu { over: MenuHost }` is what this replaces, and every claim below is a bug
    /// that shape could produce. The route was a UNIT variant meaning "Home, plus the panel" until
    /// a second card surface existed; naming the host fixed the cut-to-Home but left the page's
    /// identity, its chrome, its focus and its trail answers all derived through `page_of` from a
    /// route that was not the page. A surface is presented OVER the top page and never replaces
    /// it, so there is nothing to name, nothing to close back to, and no second answer to keep in
    /// step.
    ///
    /// Driven on all five card surfaces' routes at once, because the six-variant enum's whole
    /// failure mode was per-host and silent — a page falling through to Home's draw, a tab bar
    /// disappearing mid-hold — and the assertion that catches it is the same one each time.
    #[test]
    fn the_card_menu_is_a_surface_over_the_page_the_hold_happened_on() {
        let _g = crate::testlock::serial();
        for (route, word) in [
            (AppArg::Home, "home"),
            (AppArg::Library, "library"),
            (AppArg::Search, "search"),
            (detail_arg("1001"), "detail"),
            (person_arg("9"), "person"),
        ] {
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, route.clone(), tick(0), vec![]);
            let host = d.nav.top_page().expect("a host page").id;
            let host_owner = d.nav.input_owner();
            let mut row = crate::pms::PmsMovie::default();
            row.rk = "42".into();
            row.kind = 3;
            row.show_rk = "7".into();
            open_item_menu(&mut d, card_menu_arg(&row, false, matches!(route, AppArg::Home), host, None, None));
            let (top, _) = frame(&mut d, &mut rig, route.clone(), tick(1), vec![]);

            assert!(item_menu_up(&d), "{word}: the hold presented the menu");
            assert_eq!(
                d.nav.top_page().map(|e| e.id),
                Some(host),
                "{word}: a surface is presented OVER the top page and never replaces it"
            );
            assert_eq!(top, word, "{word}: the heartbeat's route= is still the HOST's word");
            assert_eq!(overlay_word(&d), Some("itemmenu"));
            assert_ne!(d.nav.input_owner(), host_owner, "{word}: the menu takes input from its host");
            // `Style::Compact` — the page beneath is served from the shared snapshot while its
            // own motion keeps running, which is exactly the pair the legacy code stated in two
            // places: `Popover::caching_host()` for the render half, and `host_page_updates`
            // answering TRUE for `Route::ItemMenu` for the update half ("an item menu keeps its
            // anchored page live"). Deliberately not the profile menu's `(Frozen, Cached)`: this
            // panel hangs BESIDE the card it is about and the shelf stays legible behind it.
            assert_eq!(d.host_policy(), (HostUpdate::Live, HostRender::Cached), "{word}");
            // …and the chrome question the `MenuHost` arm of `route_wears_tab_bar` answered by
            // hand: the bar is drawn (or not) because the PAGE wears it, with nothing to derive.
            assert_eq!(
                d.nav.top_page().is_some_and(|e| e.arg.chrome() == Chrome::TabBar),
                route.chrome() == Chrome::TabBar,
                "{word}: the host page answers for its own chrome"
            );

            // BACK dismisses the surface without moving the page…
            let ev = script_key(Key::Back, tick(2));
            frame(&mut d, &mut rig, route.clone(), tick(2), ev);
            assert_eq!(
                d.nav.modals.top().map(|s| s.phase),
                Some(Phase::Closing),
                "{word}: BACK dismisses the menu"
            );
            assert_eq!(d.nav.top_page().map(|e| e.id), Some(host), "{word}: …and nothing else");
            // …and it reported nothing: a dismissal is not a commit.
            assert!(rig.take_item_menu_reqs().is_empty(), "{word}");
        }
    }

    /// **OK on a row reports ONE request and dismisses in the same drain**, carrying the row and
    /// the server the panel captured.
    ///
    /// It was two producers and a static: `key_item_menu` (and a second, drifting copy on the
    /// pointer path) called `item_menu::on_ok`, which CLOSED the popover and returned the action,
    /// and the dispatch then read `item_menu::ITEM`/`SID` — statics deliberately not cleared by
    /// the close, because the drain read them a frame later. The request carries all three, so
    /// nothing is read after the dismissal at all.
    #[test]
    fn a_card_menus_commit_reports_one_request_carrying_the_row_it_captured() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        let host = d.nav.top_page().unwrap().id;
        let mut row = crate::pms::PmsMovie::default();
        row.rk = "42".into();
        row.kind = 0; // a movie: [Go to Movie, —, Mark as Watched, Play from Start]
        row.unwatched = true;
        row.part = "/library/parts/42/file.mkv".into();
        open_item_menu(&mut d, card_menu_arg(&row, false, true, host, None, None));
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        let menu = d.nav.modals.top().unwrap().entry.id;

        // A movie's rows are [Go to Movie, —, Mark as Watched, Play from Start], so the SEPARATOR
        // is index 1 and the row this test commits is index 3. Two DOWNs reach it, and the first
        // of them is what proves the separator is not a stop: it lands on 2, not on 1.
        let mut t = 2;
        let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
            let ev = script_key(key, tick(t));
            frame(d, rig, AppArg::Home, tick(t), ev);
            t += 1;
        };
        press(&mut d, &mut rig, Key::Down);
        assert_eq!(
            d.focus().map(|k| (k.entry, k.elem)),
            Some((menu, 2)),
            "the engine steps OVER the separator at index 1, which carries no action"
        );
        press(&mut d, &mut rig, Key::Down);
        assert_eq!(d.focus().map(|k| (k.entry, k.elem)), Some((menu, 3)));
        press(&mut d, &mut rig, Key::Ok);

        let reqs = rig.take_item_menu_reqs();
        assert_eq!(reqs.len(), 1, "one commit, one request");
        assert!(matches!(&reqs[0].act, crate::screens::item_menu::Action::PlayFromStart(rk) if rk == "42"));
        assert!(reqs[0].from_home, "…and the trail reset the HOME root earns (`menu_leave`)");
        assert!(!reqs[0].loaded_episode);
        assert_eq!(
            reqs[0].item.as_ref().map(|m| m.part.as_str()),
            Some("/library/parts/42/file.mkv"),
            "the WHOLE row rides on the request — a key alone cannot start playback"
        );
        assert_eq!(
            d.nav.modals.surfaces.iter().find(|s| s.entry.id == menu).map(|s| s.phase),
            Some(Phase::Closing),
            "every commit dismisses, exactly as the legacy `on_ok` closed first"
        );
        assert_eq!(d.nav.top_page().map(|e| e.id), Some(host), "the page never moved");
    }

    /// **The Settings row hands one surface to another without ever un-freezing the host** (§16.5).
    ///
    /// The two ops are parked one frame apart — the screen's own `NavOp::Dismiss` commits on the
    /// press frame, and `LoopReq::AccountSettings` is drained after `bridge::frame` returns, so
    /// `open_settings`' `Present` commits on the next one. The window between them is the risk: if
    /// the account sheet let go of the host on its press frame, the page would be re-rendered in
    /// full for one frame and re-snapshotted for the next, under a panel nobody can see, on the
    /// exact frame the Settings ground is being composed over it.
    ///
    /// It does not, and the reason is structural rather than lucky: a dismissed `Style::Sheet` is
    /// `Phase::Closing`, whose policy is `(Live, Cached)` — the update half goes live, the RENDER
    /// half keeps the snapshot for the length of the fade — and the incoming `Opaque { snapshot:
    /// true }` is `(Frozen, Cached)` from its first frame. `HostRender` therefore never returns to
    /// `Live` at all, so the fold never passes through `(Live, Live)`.
    ///
    /// **Pinned here rather than on the television** (§16.5): `fps:modal-ramp` would pass either
    /// way — the measured difference between a cached host and a live one on that scene is 2.3 ms
    /// against 75 — so a device gate could not see the regression this test exists for.
    ///
    /// Red first, simulated: with the screen's `Dismiss` removed from `activate`, the account
    /// sheet stays `Open` `(Frozen, Cached)` under the Settings surface and this passes — so the
    /// discriminating half is the `Closing` assertion below, which fails without it. With
    /// `LoopReq::AccountSettings` mapped to `dismiss_surfaces` + `open_settings` in the WRONG
    /// order (present first, then dismiss all) the fold reads `(Frozen, Cached)` throughout and
    /// the Settings surface is dismissed with the sheet — caught by the `settings_up` assertion.
    #[test]
    fn account_to_settings_never_unfreezes_the_host() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        let host = d.nav.top_page().unwrap().id;
        open_account_menu(&mut d);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        assert_eq!(d.host_policy(), (HostUpdate::Frozen, HostRender::Cached));

        // Focus the Settings row and commit it. Signed out (a host test has no session file the
        // fixture wrote), the rows are [Sign in, Settings], so one DOWN then OK.
        let menu_entry = d.nav.modals.top().unwrap().entry.id;
        let mut t = 2;
        let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
            let ev = script_key(key, tick(t));
            frame(d, rig, AppArg::Home, tick(t), ev);
            t += 1;
        };
        press(&mut d, &mut rig, Key::Down);
        assert_eq!(
            d.focus().map(|k| (k.entry, k.elem)),
            Some((menu_entry, 1)),
            "the engine walked the menu's own rows"
        );
        press(&mut d, &mut rig, Key::Ok);

        // The commit frame: the sheet is dismissed and the host's RENDER half is still Cached.
        assert_eq!(
            d.nav.modals.surfaces.iter().find(|s| s.entry.id == menu_entry).map(|s| s.phase),
            Some(Phase::Closing),
            "the row's commit dismisses the sheet"
        );
        let mut renders = vec![d.host_policy().1];

        // …and the loop's drain performs the request on the next frame.
        let reqs = rig.take_reqs();
        assert_eq!(reqs, vec![LoopReq::AccountSettings], "one request, and it is the Settings row's");
        open_settings(&mut d);
        for i in 0..12u32 {
            frame(&mut d, &mut rig, AppArg::Home, tick(20 + i), vec![]);
            renders.push(d.host_policy().1);
        }
        assert!(settings_up(&d), "the Settings surface is up");
        assert_eq!(
            d.nav.top_page().map(|e| e.id),
            Some(host),
            "the host page never moved under either surface"
        );
        assert!(
            !renders.contains(&HostRender::Live),
            "the host was re-rendered in full during the handover: {renders:?}"
        );
    }

    /// **Phase 9: the player's four panels are entries on ITS page's own stack, not on the route.**
    ///
    /// Three claims, and each one is a bug the `Route::Player { overlay }` shape could produce.
    /// (1) Presenting a panel makes it the INPUT OWNER, which is what replaces the ladder's four
    /// `if …overlay… { continue }` arms. (2) It does NOT move the app's page stack — the player
    /// stays the top page, so the video plane, the subtitle bitmaps and the transport's springs
    /// are not torn down to open a menu over them. (3) Dismissing it gives input back to the
    /// player, with the SAME instance underneath: a remount here would reset the HUD's timer and
    /// the control row's springs, which is precisely what `same_instance` ignoring the overlay is
    /// for (§16.9).
    #[test]
    fn a_player_panel_is_a_surface_on_the_players_own_page_and_leaves_the_instance_alone() {
        let ps = crate::route::PlaybackSession::IDLE;
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Player, tick(0), vec![]);
        let page = d.nav.top_page().expect("the player is a page").id;
        let instance = d.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id);
        assert!(instance.is_some(), "…with a live instance");
        let depth = d.nav.tabs.stack.depth();
        for kind in [
            crate::screens::player::overlay::OverlayKind::Tracks { tab: 1 },
            crate::screens::player::overlay::OverlayKind::Info,
            crate::screens::player::overlay::OverlayKind::Chapters,
            crate::screens::player::overlay::OverlayKind::More { quality: false },
        ] {
            open_player_overlay(&ps, &mut d, kind);
            frame(&mut d, &mut rig, AppArg::Player, tick(1), vec![]);
            assert_eq!(player_overlay_kind(&d), Some(kind), "{kind:?} is up");
            assert_eq!(
                overlay_word(&d),
                Some(kind.word()),
                "and the heartbeat says so from the surface, not from a second table",
            );
            assert_ne!(
                d.nav.input_owner(),
                Some(InputOwner::Entry(page)),
                "{kind:?} owns input while it is up",
            );
            assert_eq!(d.nav.tabs.stack.depth(), depth, "the app's page stack never moved");
            assert_eq!(d.nav.top_page().map(|e| e.id), Some(page), "…and the player is still it");

            dismiss_player_overlays(&mut d);
            frame(&mut d, &mut rig, AppArg::Player, tick(2), vec![]);
            frame(&mut d, &mut rig, AppArg::Player, tick(3), vec![]);
            assert_eq!(player_overlay_kind(&d), None, "{kind:?} dismissed");
            assert_eq!(
                d.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id),
                instance,
                "{kind:?} closed onto the SAME player instance — no remount",
            );
        }
    }

    /// **`ScreenArg::same_instance` compares the PLAYBACK, never the panel** (§16.9), and the two
    /// arguments are different KINDS: a panel is `AppArg::PlayerOverlay`, so it can never be
    /// mistaken for the page it stands on. Two panels of different kinds are two instances —
    /// which is what makes `sync_page`'s "is the top page already what I want" answer stable while
    /// a menu is open, and what stops the track menu being reused as the Info card.
    #[test]
    fn same_instance_reads_the_playback_and_not_the_overlay() {
        use crate::screens::player::overlay::{OverlayKind, PlayerOverlayArg};
        use crate::ui::screen::ScreenArg;
        let player = AppArg::Player;
        assert!(player.same_instance(&AppArg::Player));
        for kind in [
            OverlayKind::Tracks { tab: 0 },
            OverlayKind::Info,
            OverlayKind::Chapters,
            OverlayKind::More { quality: false },
        ] {
            let panel = AppArg::PlayerOverlay(PlayerOverlayArg { kind });
            assert!(
                !player.same_instance(&panel) && !panel.same_instance(&player),
                "{kind:?} is not the playback",
            );
            assert!(panel.same_instance(&AppArg::PlayerOverlay(PlayerOverlayArg { kind })));
        }
        assert!(!AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Info })
            .same_instance(&AppArg::PlayerOverlay(PlayerOverlayArg {
                kind: OverlayKind::Chapters
            })));
        // The panel that carries a parameter is still ONE panel: reopening the track menu on the
        // other tab reuses the entry rather than stacking a second one, exactly as
        // `open_player_overlay`'s early return says.
        assert!(AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Tracks { tab: 0 } })
            .same_instance(&AppArg::PlayerOverlay(PlayerOverlayArg {
                kind: OverlayKind::Tracks { tab: 1 }
            })));
    }

    /// **THE FOURTH ROOT** (`app/input.rs`'s `back_at_root`, and the key ladder's dispatcher arm).
    ///
    /// BACK at the FIRST consent stage was SWALLOWED for as long as that screen was a `Popover`:
    /// the step behind it is sign-in, which cannot be undone, and `ui::consent::on_back` reported
    /// `true` for both the stepped-back and the swallowed case — so the loop's BACK arm could not
    /// tell them apart and the 2026-09-03 root rule could not reach this screen. What is graded
    /// here is that the owned screen SAYS which it is, as a request the loop performs, and that it
    /// does so WITHOUT dismissing itself: going to the television's Home neither answers nor
    /// dismisses the question, so selecting the tile again must come straight back to it.
    #[test]
    fn back_at_the_first_consent_stage_is_the_root_press_and_leaves_the_question_up() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Profiles, tick(0), vec![]);
        open_first_run_consent(&mut d);
        frame(&mut d, &mut rig, AppArg::Profiles, tick(1), vec![]);
        assert!(consent_up(&d));
        assert_eq!(overlay_word(&d), Some("consent"));
        let _ = rig.take_reqs(); // the mount's own effects are not what this grades
        frame(&mut d, &mut rig, AppArg::Profiles, tick(2), script_key(Key::Back, tick(2)));
        assert_eq!(
            rig.take_reqs(),
            vec![LoopReq::BackAtRoot],
            "the screen asks the loop for the root press rather than swallowing the key"
        );
        frame(&mut d, &mut rig, AppArg::Profiles, tick(3), vec![]);
        assert!(
            consent_up(&d),
            "…and the question is still up: the platform took the screen, nothing was answered"
        );
    }

    /// TV session 4 (2026-09-09): `settings-root` fell from 60 to 40 fps the day Home became an
    /// owned page. The fold said Replaced, but the loop's guard read `!host_replaced ||
    /// page_owned`, so the page closure still ran under the opaque ground and served the frozen
    /// full-screen snapshot quad every frame on top of the ground's own wash. A Replaced host
    /// receives nothing (§8.3), whoever owns the page — this pins the plan the loop draws by.
    #[test]
    fn a_replaced_host_draws_surfaces_only_whoever_owns_the_page() {
        assert_eq!(
            page_plan(true, true),
            PagePlan::SurfacesOnly,
            "the owned Home under an opaque Settings ground: no page pass, no cached quad"
        );
        assert_eq!(
            page_plan(true, false),
            PagePlan::SurfacesOnly,
            "a legacy page under one, exactly as before phase 8"
        );
        assert_eq!(page_plan(false, true), PagePlan::Owned, "the closure draws an owned page and its surfaces");
        assert_eq!(page_plan(false, false), PagePlan::LegacyThenSurfaces);
    }

    /// The other half of the coexistence contract: an OWNED PAGE (first-run Favourites) is the
    /// dispatcher's to draw, and [`page_owned`] is what says so.
    ///
    /// **The second half of this test was a stale-frame guard and is retired with D1.** It asserted
    /// that `page_owned` answers `false` while the committed route and the tree DISAGREE — a
    /// window that existed because the loop held a second copy of "which page is on top" and a
    /// `LoopReq` could flip it after the dispatcher's frame. There is one authority now, so the
    /// two cannot disagree and the predicate has no route argument left to compare against.
    #[test]
    fn the_first_run_favourites_page_is_owned_because_the_container_holds_it() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        assert!(page_owned(&d), "Home now draws through its owned instance");
        assert_eq!(frame(&mut d, &mut rig, AppArg::Onboard, tick(1), vec![]).0, "onboard");
        assert!(d.owns_input(), "an owned page takes the ladders' input too, not only a surface");
        assert!(page_owned(&d), "…and the page the container holds is the one that is owned");
    }

    /// **The regression this pass exists to close**: the confirmed "Delete all local data" sweep
    /// used `dismiss_surfaces` (the ordinary, spring-driven path) to tear down the Settings
    /// surface it had just been answered inside of, which left a Cached snapshot of the erased
    /// Settings/Home page compositing over the freshly-mounted sign-in screen for as long as the
    /// appear spring took to settle. What must be true instead: `dismiss_surfaces_now` puts the
    /// surface at ITS OWN `Phase::Closing` target — `motion.appear == 0.0`, already `settled()` —
    /// in the SAME call, with no frame boundary in between. Contrast with `dismiss_surfaces`,
    /// which only PARKS a `NavOp::Dismiss` that does not even start applying until the next
    /// `frame()`'s NAV COMMIT — that one extra frame, running over a route that has already
    /// flipped, was the ghost.
    #[test]
    fn dismiss_surfaces_now_settles_the_surface_in_the_same_call_unlike_the_ordinary_path() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, AppArg::Home, tick(0), vec![]);
        open_settings(&mut d);
        frame(&mut d, &mut rig, AppArg::Home, tick(1), vec![]);
        assert!(settings_up(&d));
        // **A second frame, and it is not padding.** The dispatcher's NAV COMMIT is step 7 of its
        // frame and the container Tick is step 4, so the surface `open_settings` parked is mounted
        // AFTER this frame's motion step has already run: at the end of the `tick(1)` frame it
        // exists with its spring untouched at exactly 0.0. That is correct — nothing can integrate
        // motion for a surface that did not exist when the tick ran — but it means one frame is
        // not yet "mid-open", and asserting `appear > 0.0` there fails on the frame ORDER rather
        // than on anything this test is about.
        frame(&mut d, &mut rig, AppArg::Home, tick(2), vec![]);
        // Now one tick HAS run over it: the appear spring is climbing toward its open target and
        // the phase has not yet been promoted past `Opening` (`ModalStack::tick` only promotes it
        // to `Open` once `motion.settled()`) — i.e. deliberately mid-open, nowhere near the
        // "already at the closed end" state `dismiss_surfaces_now` must reach in the SAME call.
        let before = d.nav.modals.top().expect("just presented");
        assert_eq!(before.phase, Phase::Opening);
        assert!(before.motion.appear > 0.0, "the spring has moved off its starting rest position");
        assert!(!before.motion.settled(), "one tick cannot have already reached the open target");
        dismiss_surfaces_now(&mut d);
        let top = d
            .nav
            .modals
            .top()
            .expect("hide() only changes phase/motion; the surface is still in the tree until prune() next runs");
        assert_eq!(top.phase, Phase::Closing, "hide() moves the phase immediately, exactly like dismiss()");
        assert!(
            top.motion.settled(),
            "…but hide() also SNAPS the motion to its target, so it is settled with no frame having run"
        );
        assert_eq!(top.motion.appear, 0.0, "settled at the closed end, not merely heading there");
    }

    /// **Regression pin for `run::update`'s chrome chain's third (Search) arm** — the one that
    /// steps `update_home_chrome` while standing on Search because `SearchScreen::tick` only
    /// steps its OWN rows and never touches the shared strip's capsule/scroll springs that live in
    /// `Bridge`'s own `strip: StripRender` field. Without that arm the strip's published rects
    /// (`d.nav.tabs.strip`, read by pointer hit-testing and focus) freeze at whatever the
    /// previously-drawn page left them at.
    ///
    /// Reproduced here without a font, `App`, SDL or GL: `Bridge::for_test` supplies a
    /// `Measure`-only chrome, and the shared strip's scroll spring is `rig.strip`'s own field —
    /// this test seeds it directly (a private field, reachable because this module is `Bridge`'s
    /// own) rather than through a process-wide static, driving it away from rest with a synthetic,
    /// wide vocabulary (independent of Search's own short "Home"/"Movies"/"TV Shows"/search-icon
    /// labels, which never grow wide enough on their own to need scrolling) to stand in for
    /// "wherever the previous page left it", then `capture_chrome(Route::Search)` publishes the
    /// strip once against that stale scroll — exactly the one-shot capture `run::update`'s NAV
    /// COMMIT already does every frame regardless of this arm. What only the missing arm can
    /// fix is calling `update_home_chrome` afterwards: this test asserts the published rect
    /// travels back to its (correct, un-stale) target once that call is made every frame, as the
    /// Search arm does.
    #[test]
    fn search_route_steps_the_shared_strip_so_its_published_rects_do_not_go_stale() {
        let _guard = crate::testlock::serial();
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();

        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);

        // A synthetic, artificially wide vocabulary — nothing to do with Search's real labels —
        // whose reveal target for its last entry sits well past the real strip's viewport, so
        // jumping straight to it (`StripRender::reveal`, a JUMP, not a step) leaves `rig.strip`'s
        // scroll spring far from where Search's own (short) labels want it.
        let wide: Vec<String> = (0..8).map(|i| format!("Wide Destination Label Number {i}")).collect();
        rig.strip.reveal(
            crate::ui::widgets::TabLabels { generation: 1, labels: &wide },
            wide.len() - 1,
        );

        super::show_page(&mut d, AppArg::Search);
        rig.capture_chrome(&mut d);

        let base = crate::ui::dispatch::STRIP_BASE;
        let search_member = |d: &Dispatcher<AppHost>| {
            d.nav.tabs.strip.iter().find(|m| m.elem == base + 3).copied().expect("Search pill published")
        };
        let before = search_member(&d);
        let stale_gap = (before.drawn.x - before.target.x).abs();
        assert!(stale_gap > 10.0,
            "the synthetic wide vocabulary must actually leave the scroll away from Search's own target; gap={stale_gap}");

        // The fix under test: the Search arm of `run::update`'s chrome chain calls exactly this,
        // every frame, while standing on Search.
        let mut glass = crate::ui::frame::glass::GlassPlan::new();
        for _ in 0..60 {
            rig.update_home_chrome(&mut d, &mut glass, 1.0 / 60.0);
        }

        let after = search_member(&d);
        let settled_gap = (after.drawn.x - after.target.x).abs();
        assert!(settled_gap < 1.0,
            "the strip's published rect must travel to its target once Search steps it each frame \
             (stale={stale_gap}, settled after 1s={settled_gap}) — losing the Search chrome arm \
             again silently reproduces the frozen capsule / stale pointer targets this pins");
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    }

    /// The account-menu lift is a second PAINT of this bridge's captured chrome, not a second
    /// publication. Changing the process globals after A captured must not make A draw B's chip.
    #[test]
    fn two_bridges_keep_their_own_captured_profile_and_labels_for_a_scrim_lift() {
        let _guard = crate::testlock::serial();
        struct Restore(Option<crate::plex::session::UserRef>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::plex::session::set_current(self.0.take());
            }
        }
        let _restore = Restore(crate::plex::session::current());
        let mut a = Bridge::for_test(|| 0);
        let mut b = Bridge::for_test(|| 0);
        a.seed_chrome_for_test("Owner A", "A", &["Home", "Movies", ""]);
        b.seed_chrome_for_test("Owner B", "B", &["Home", "TV Shows", ""]);
        crate::plex::session::set_current(Some(crate::plex::session::UserRef {
            title: "Global B".into(),
            ..Default::default()
        }));

        let ar = <Bridge as crate::ui::dispatch::Rig<AppHost>>::scrim_chrome_read(&a)
            .expect("a bar-wearing bridge publishes lift chrome");
        let br = <Bridge as crate::ui::dispatch::Rig<AppHost>>::scrim_chrome_read(&b)
            .expect("the second bridge publishes its own lift chrome");
        assert_eq!(ar.profile.name.to_bytes(), b"Owner A");
        assert_eq!(ar.profile.initial.to_bytes(), b"A");
        assert_eq!(ar.labels.labels, ["Home", "Movies", ""]);
        assert_eq!(br.profile.name.to_bytes(), b"Owner B");
        assert_eq!(br.labels.labels, ["Home", "TV Shows", ""]);
    }
}
