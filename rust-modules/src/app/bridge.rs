//! **The bridge between the legacy loop and the dispatcher** (restructure spec §14, phase 5b —
//! the shadow of phase 3b grown into the seam the migration crosses screen by screen).
//!
//! What lives here: the application's `Host` ([`AppHost`]) — its screen argument [`AppArg`],
//! which is a legacy `Route` or one of the OWNED screens; the mounter's one `match`
//! ([`AppMounter`]); the rig the dispatcher borrows ([`Bridge`]: the real `TtfMeasure`, the store
//! deliveries of phase 4, the consent MACHINE, the loop requests an owned screen makes); and
//! [`frame`], which the loop calls once per iteration right after its NAV COMMIT with the inputs
//! it collected for the dispatcher.
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
//! - **The three privileged calls** stay the loop's; the rig's hooks are no-ops (3b's reason).

use std::borrow::Cow;
use std::ffi::CStr;

use crate::screens::family::SettingsPage;
use crate::screens::registry::{AppFx, AppMsg, ConsentCmd, ContentArg, ContentReq, HomeCmd, HomeLike, HomeReq, LoopReq, PageMemory};
use crate::screens::settings::{Family, RouteSurface};
use crate::stores::{StoreCmd, StoreEv, StoreId};
use crate::ui::containers::modal::{HostRender, HostUpdate, Phase, Style};
use crate::ui::dispatch::{CxParts, Dispatcher, FrameReport, Rig, Split};
#[cfg(test)]
use crate::ui::dispatch::NoTap;
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Chrome, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Host,
    InputEvent, InputKind, InputOwner, InstanceId, Key, LogicalState, Machine, MachineId, Measure, NavOp,
    ScreenId, Source, Tick, TimerId,
};
use crate::ui::present::Present;
use crate::ui::screen::{
    At, Dir, DrawFrame, FocusSource, Focusable, GroupSpec, HitSource, Mounter, Placed,
    RenderStrategy, ReturnState, Screen, ScreenEvent, Step,
};

use super::nav::route_wears_tab_bar;
use super::{route_word, Route};

// ---------------------------------------------------------------------------------------------
// the bundle
// ---------------------------------------------------------------------------------------------

pub(super) struct AppHost;

/// A screen argument: the legacy `Route` (§14: "`Route` survives only as its argument"), or an
/// OWNED screen — the Settings surface and the first-run consent surface (both `RouteSurface`).
///
/// **Both owned variants carry the page their inner stack is ROOTED at**, which is there for the
/// dev boot targets and for nothing else. `/tmp/plxnative-settings=privacy` has to put a headless
/// run on a page that is normally two presses inside the surface, and the loop cannot press them:
/// the Settings root's row indices are `RootPage`'s private business (the Favourites row is absent
/// signed out), so a loop that reached the child by delivering `Activate(<row>)` would be encoding
/// a table it does not own and would rot the first time a row is added. Rooting the stack at the
/// target instead needs nothing from the page. The one thing it costs is that BACK at a
/// dev-booted child DISMISSES the surface rather than revealing the root — a difference that
/// exists only under a trigger, and that the fps scenes it serves (`legal-document`,
/// `decision-alert`, `settings-*`) never press.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum AppArg {
    Legacy(Route),
    Content(ContentArg),
    /// The Settings family, rooted at this page (`SettingsPage::Root` for every real opening).
    Settings(SettingsPage),
    /// The first-run consent question, rooted at this stage byte (0 for every real opening;
    /// `screens::consent`'s `STAGE_PRODUCT` for `/tmp/plxnative-consent=product`).
    FirstRunConsent(u8),
}

pub(super) const ARG_SHAPE: &str = "AppArg{Legacy:Route{Login,Profiles,Onboard,Home,Account{over:BarHost{Home,Library,Search}},ItemMenu{over:MenuHost{Home,Detail,Related,Library,Search,Person}},Library,Detail,Person,Search,Player{overlay:Overlay{None,Menu,Info,Chapters,More}}},Content:{Detail{sid:u32,rk:str},Person{sid:u32,key:str,guid:str,name:str,thumb:str},Filmography{sid:u32,key:str}},Settings:SettingsPage{Root,Favourites,Privacy,Legal,About,Document(u8),Preview(u8),ConsentStage(u8)},FirstRunConsent(u8)}";

impl LogicalState for AppArg {
    fn write(&self, c: &mut Canon) {
        match self {
            Self::Legacy(route) => {
                c.u32(0);
                match route {
                    Route::Login => { c.u32(0); }
                    Route::Profiles => { c.u32(1); }
                    Route::Onboard => { c.u32(2); }
                    Route::Home => { c.u32(3); }
                    Route::Account { over } => { c.u32(4).u32(*over as u32); }
                    Route::ItemMenu { over } => { c.u32(5).u32(*over as u32); }
                    Route::Library => { c.u32(6); }
                    Route::Detail => { c.u32(7); }
                    Route::Person => { c.u32(8); }
                    Route::Search => { c.u32(9); }
                    Route::Player { overlay } => { c.u32(10).u32(*overlay as u32); }
                }
            }
            Self::Content(arg) => { c.u32(1); arg.write(c); }
            Self::Settings(page) => { c.u32(2); page.write(c); }
            Self::FirstRunConsent(stage) => { c.u32(3).u8(*stage); }
        }
    }
    fn probe(&self, out: &mut String) { out.push_str("app_arg"); }
}

impl crate::ui::screen::ScreenArg for AppArg {
    fn chrome(&self) -> Chrome {
        match self {
            AppArg::Legacy(r) if route_wears_tab_bar(*r) => Chrome::TabBar,
            _ => Chrome::None,
        }
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            AppArg::Legacy(Route::Login) => 1,
            AppArg::Legacy(Route::Profiles) => 2,
            AppArg::Legacy(Route::Onboard) => 3,
            AppArg::Legacy(Route::Home) => 4,
            AppArg::Legacy(Route::Account { .. }) => 5,
            AppArg::Legacy(Route::ItemMenu { .. }) => 6,
            AppArg::Legacy(Route::Library) => 7,
            AppArg::Legacy(Route::Detail) => 8,
            AppArg::Legacy(Route::Person) => 9,
            AppArg::Legacy(Route::Search) => 10,
            AppArg::Legacy(Route::Player { .. }) => 11,
            // The ROOT payload is a boot address, not an identity: one Settings surface and one
            // consent question, whichever page each happens to have been rooted at.
            AppArg::Settings(_) => 12,
            AppArg::FirstRunConsent(_) => 13,
            AppArg::Content(ContentArg::Detail { .. }) => 8,
            AppArg::Content(ContentArg::Person { .. }) => 9,
            AppArg::Content(ContentArg::Filmography { .. }) => 14,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        if let (Self::Content(a), Self::Content(b)) = (self, other) {
            return a.same_item(b);
        }
        if matches!(self, Self::Content(_)) || matches!(other, Self::Content(_)) {
            return false;
        }
        // …and the same reason `id` collapses the payload: `Settings(Root)` and
        // `Settings(Legal)` are the same SCREEN, so a container must never be able to think it
        // is holding two of them.
        <Self as crate::ui::screen::ScreenArg>::id(self) == <Self as crate::ui::screen::ScreenArg>::id(other)
    }
}

impl AppArg {
    pub(super) fn from_node(node: &super::Node) -> Self {
        match node {
            super::Node::Detail { sid, rk, .. } => Self::Content(ContentArg::Detail { sid: *sid, rk: rk.clone() }),
            super::Node::Person { sid, key, guid, name, thumb } => Self::Content(ContentArg::Person {
                sid: *sid, key: key.clone(), guid: guid.clone(), name: name.clone(), thumb: thumb.clone(),
            }),
            _ => Self::Legacy(super::node_route(node)),
        }
    }

    pub(super) fn route(&self) -> Option<Route> {
        match self {
            Self::Legacy(r) => Some(super::page_of(*r)),
            Self::Content(ContentArg::Detail { .. }) => Some(Route::Detail),
            Self::Content(ContentArg::Person { .. } | ContentArg::Filmography { .. }) => Some(Route::Person),
            _ => None,
        }
    }

    pub(super) fn node(&self, memory: &PageMemory) -> Option<super::Node> {
        match self {
            Self::Content(ContentArg::Detail { sid, rk }) => Some(super::Node::Detail {
                sid: *sid, rk: rk.clone(),
                spot: match memory { PageMemory::Detail(s) => s.spot.clone(), _ => Default::default() },
            }),
            Self::Content(ContentArg::Person { sid, key, guid, name, thumb }) =>
                Some(super::Node::Person { sid: *sid, key: key.clone(), guid: guid.clone(), name: name.clone(), thumb: thumb.clone() }),
            Self::Legacy(Route::Home) => Some(super::Node::Home),
            Self::Legacy(Route::Library) => Some(super::Node::Library),
            Self::Legacy(Route::Search) => Some(super::Node::Search),
            _ => None,
        }
    }
}

/// Is this argument the Settings family, whatever page it was rooted at?
fn is_settings(a: &AppArg) -> bool {
    matches!(a, AppArg::Settings(_))
}

/// …and the first-run question, whatever stage it was rooted at.
fn is_first_run_consent(a: &AppArg) -> bool {
    matches!(a, AppArg::FirstRunConsent(_))
}

#[derive(Clone, Copy)]
pub(super) struct AppViews<'a> {
    #[cfg_attr(not(test), allow(dead_code))] // Phase 8: Home mounter integration is the consumer.
    pub(super) hubs: crate::pms::HubsView<'a>,
    #[allow(dead_code)] // Library adoption consumes this retained listing, not browse globals.
    pub(super) listing: crate::stores::browse::ListingView<'a>,
    #[allow(dead_code)] // Phase-8 Library sources/menu consumer.
    pub(super) directory: crate::stores::browse::DirectoryView<'a>,
    #[allow(dead_code)] // Phase-8 Library shelf consumer.
    pub(super) section_hubs: crate::stores::browse::HubsView<'a>,
}

#[derive(Default)]
pub(super) struct BridgeInit;

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

// ---------------------------------------------------------------------------------------------
// the legacy page
// ---------------------------------------------------------------------------------------------

/// A legacy route as a `Screen` (§14): its logical state is the route WORD; the ladders answer
/// everything, the engine and the map are inert for it.
pub(super) struct LegacyPage {
    route: Route,
    state: LegacyState,
}

struct LegacyState {
    word: &'static str,
    notices: u32,
}

impl LogicalState for LegacyState {
    fn write(&self, w: &mut Canon) {
        w.str(self.word);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("{} notices={}", self.word, self.notices));
    }
}

impl LegacyPage {
    fn new(route: Route) -> Self {
        Self {
            route,
            state: LegacyState {
                word: route_word(route),
                notices: 0,
            },
        }
    }
}

impl Machine<AppHost> for LegacyPage {
    type Ev = ScreenEvent<AppHost>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, AppHost>, _fx: &mut Effects<'_, AppHost>) -> Handled {
        if let ScreenEvent::StoreChanged(..) = ev {
            self.state.notices += 1;
        }
        Handled::No
    }
}

impl Focusable<AppHost> for LegacyPage {
    fn groups(&self, _cx: &Cx<'_, AppHost>, _out: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, AppHost>) -> Option<GroupId> {
        None
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, AppHost>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, _key: &u32, _cx: &Cx<'_, AppHost>, _at: At) -> Option<Placed> {
        None
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, AppHost>) -> FocusKey<u32> {
        want
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, AppHost>) -> FocusKey<u32> {
        FocusKey {
            entry: EntryId(0),
            elem: 0,
        }
    }
}

impl Screen<AppHost> for LegacyPage {
    fn name(&self) -> &'static str {
        self.state.word
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, AppHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, AppHost>) {}
    fn draw(&mut self, _f: &mut DrawFrame<'_, AppHost>) {}
    fn render(&self) -> RenderStrategy {
        match self.route {
            Route::Player { .. } => RenderStrategy::VideoPlane,
            _ => RenderStrategy::Page,
        }
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Legacy
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Legacy
    }
}

// ---------------------------------------------------------------------------------------------
// the mounter: the one match
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct AppMounter {
    seed: Option<super::Node>,
}

impl Mounter<AppHost> for AppMounter {
    fn mount(
        &mut self,
        id: InstanceId,
        arg: &AppArg,
        ret: &ReturnState<u32, PageMemory>,
        cx: &Cx<'_, AppHost>,
        _fx: &mut Effects<'_, AppHost>,
    ) -> Box<dyn Screen<AppHost>> {
        let entry = match cx.owner {
            crate::ui::machine::InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        match arg {
            AppArg::Content(ContentArg::Detail { sid, rk }) => {
                let mut page = crate::screens::detail::DetailScreen::new(entry, *sid, rk.clone());
                if let PageMemory::Detail(spot) = &ret.memory {
                    page.restore_memory(spot);
                } else if let Some(super::Node::Detail { sid: seed_sid, rk: seed_rk, spot }) = self.seed.take() {
                    if seed_sid == *sid && seed_rk == *rk { page.restore(&spot); }
                }
                Box::new(page)
            }
            AppArg::Content(ContentArg::Person { sid, key, guid, name, thumb }) => {
                let mut page = crate::screens::person::PersonScreen::new(entry, *sid, key.clone(), guid.clone(), name.clone(), thumb.clone());
                if let PageMemory::Person(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            AppArg::Content(ContentArg::Filmography { sid, key }) => {
                let mut page = crate::screens::filmography::FilmographyScreen::new(entry, *sid, key.clone());
                if let PageMemory::Filmography(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            // the first-run Favourites screen is OWNED (§14: "retirement 5b Onboard"); the route
            // word stays the loop's while the loop still names the page
            AppArg::Legacy(Route::Onboard) => Box::new(crate::screens::onboard::OnboardScreen::first_run(entry)),
            // Phase 6: the QR sign-in and the who's-watching picker are OWNED screens too, mounted
            // exactly the same way — the route word is still the loop's (`route_word`), and
            // naming the route is the whole of (re)mounting either: a fresh instance is built
            // every time `bridge::frame` follows a `Replace` onto one of them, which is what lets
            // every remaining `app::input`/`app::run` call site drop its own `enter()`-equivalent
            // reset (see `input::enter_profiles_from_onboard`'s doc for the same argument made
            // about `screens::onboard` in 5b).
            AppArg::Legacy(Route::Login) => Box::new(crate::screens::login::LoginScreen::new(entry)),
            AppArg::Legacy(Route::Profiles) => Box::new(crate::screens::profiles::ProfilesScreen::new(entry)),
            AppArg::Legacy(Route::Home) => {
                let mut page = crate::screens::home::HomeScreen::new(entry, id);
                if let PageMemory::Home(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            AppArg::Legacy(r) => Box::new(LegacyPage::new(*r)),
            AppArg::Settings(root) => Box::new(RouteSurface::new(entry, id, Family::Settings, *root)),
            AppArg::FirstRunConsent(stage) => Box::new(RouteSurface::new(
                entry,
                id,
                Family::FirstRunConsent,
                SettingsPage::ConsentStage(*stage),
            )),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the rig
// ---------------------------------------------------------------------------------------------

/// The consent MACHINE (§2.2): the one owner of the two decisions. It applies an answer and
/// PUBLISHES it (`telemetry::record` → `consent::install`), which is the snapshot every
/// telemetry thread reads; nothing else writes it.
pub(super) struct ConsentMachine;

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
pub(super) struct Bridge {
    mounter: AppMounter,
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
    chrome: super::chrome::ChromeSnapshot,
    legacy_host_live: bool,
    home_commands: std::collections::VecDeque<HomeCmd>,
    consent: ConsentMachine,
    /// Requests the owned screens made of the loop this frame (§14), drained by [`frame`]'s caller.
    reqs: Vec<LoopReq>,
    content_reqs: Vec<(MachineId, ContentReq, ReturnState<u32, PageMemory>)>,
    home_reqs: Vec<(MachineId, HomeReq, ReturnState<u32, PageMemory>)>,
    effect_return: ReturnState<u32, PageMemory>,
    pub(super) menu_opener: Option<(EntryId, Option<FocusKey<u32>>)>,
    /// The surfaces whose host counters this bridge holds: (entry, cached, closing).
    held: Vec<(EntryId, bool, bool)>,
    now_us: fn() -> u64,
}

impl Bridge {
    pub(super) fn new(now_us: fn() -> u64) -> Self {
        // A `static`, not `&TtfMeasure` inline: a unit-struct literal DOES const-promote to
        // `'static` today, but that is a rule about the expression rather than a promise about
        // this field, and a `static` states the lifetime outright. Same reasoning as the one
        // `screens::settings`'s test module writes out beside its own measure.
        static TTF: crate::text::TtfMeasure = crate::text::TtfMeasure;
        Self::with_measure(&TTF, now_us)
    }

    /// The same bridge over a measure that needs no fonts — the ONLY constructor a host test may
    /// use. See [`Bridge::measure`] for what happens to a test that reaches for `new` instead: it
    /// dies inside `text.rs` on a `debug_assert!`, several frames away from anything it asserted.
    #[cfg(test)]
    pub(super) fn for_test(now_us: fn() -> u64) -> Self {
        static FIXTURE: crate::ui::fixture::FixtureMeasure = crate::ui::fixture::FixtureMeasure;
        Self::with_measure(&FIXTURE, now_us)
    }

    fn with_measure(measure: &'static dyn Measure, now_us: fn() -> u64) -> Self {
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        directory.capture();
        Self {
            mounter: AppMounter::default(),
            measure,
            hubs: crate::pms::hubs_snapshot(),
            listing: crate::stores::browse::listing_snapshot(),
            directory,
            section_hubs: crate::stores::browse::hubs_snapshot(),
            chrome: super::chrome::ChromeSnapshot::default(),
            legacy_host_live: true,
            home_commands: std::collections::VecDeque::new(),
            consent: ConsentMachine,
            reqs: Vec::new(),
            content_reqs: Vec::new(),
            home_reqs: Vec::new(),
            effect_return: ReturnState::default(),
            menu_opener: None,
            held: Vec::new(),
            now_us,
        }
    }

    pub(super) fn take_reqs(&mut self) -> Vec<LoopReq> {
        std::mem::take(&mut self.reqs)
    }

    pub(super) fn take_content_reqs(&mut self) -> Vec<(MachineId, ContentReq, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.content_reqs)
    }

    pub(super) fn take_home_reqs(&mut self) -> Vec<(MachineId, HomeReq, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.home_reqs)
    }

    pub(super) fn home_opener(&self, d: &Dispatcher<AppHost>, entry: EntryId,
        focus: Option<FocusKey<u32>>) -> crate::ui::popover::Opener {
        let rect = focus.filter(|key| key.entry == entry).and_then(|key| {
            let screen = &d.nav.entry(entry)?.inst.as_ref()?.screen;
            let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
                focus: crate::ui::machine::FocusRead { current: Some(key) }, owner: InputOwner::Entry(entry) };
            let cx = parts.cx::<AppHost>(AppViews {
                hubs: self.hubs.view(),
                listing: self.listing.view(),
                directory: self.directory.view(),
                section_hubs: self.section_hubs.view(),
            }, self.measure);
            screen.as_any()?.downcast_ref::<crate::screens::home::HomeScreen>()?
                .focused_rect::<AppHost>(Some(key), &cx, At::Drawn)
        });
        crate::ui::popover::Opener { rect, ..crate::ui::popover::Opener::NONE }
    }

    pub(super) fn seed_node(&mut self, node: &super::Node) {
        self.mounter.seed = Some(node.clone());
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
            focus: crate::ui::machine::FocusRead { current: focus }, owner: InputOwner::Entry(entry.id) };
        let cx = parts.cx::<AppHost>(AppViews {
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(),
        }, self.measure);
        Some(f(home, &cx, focus))
    }

    pub(super) fn home_grid_focused(&self, d: &Dispatcher<AppHost>) -> bool {
        self.with_home(d, |home, cx, focus| home.grid_position::<AppHost>(focus, cx).is_some()).unwrap_or(false)
    }

    pub(super) fn home_snap_target(&self, d: &Dispatcher<AppHost>) -> f32 {
        self.with_home(d, |home, _, _| home.snap_target()).unwrap_or(0.0)
    }

    fn capture_chrome(&mut self, d: &mut Dispatcher<AppHost>, route: Route) {
        self.legacy_host_live = super::host_page_updates(route, false);
        if matches!(super::page_of(route), Route::Home) {
            d.nav.tabs.strip_fallback = Some(crate::screens::home::STRIP_HOME_ELEM);
            self.chrome.refresh(self.measure);
            let selected = self.navigation_presentation().view_tab.unwrap_or(0) as i32;
            self.chrome.members(selected, Self::home_focus(d), &mut d.nav.tabs.strip);
        } else {
            d.nav.tabs.strip.clear();
            d.nav.tabs.strip_fallback = None;
        }
    }

    fn capture_views(&mut self, d: &mut Dispatcher<AppHost>) {
        self.section_hubs = crate::stores::browse::hubs_snapshot();
        self.directory.capture();
        self.listing = crate::stores::browse::listing_snapshot();
        let before = (self.hubs.view().generation, self.hubs.view().state);
        self.hubs = crate::pms::hubs_snapshot();
        let after = (self.hubs.view().generation, self.hubs.view().state);
        if before != after {
            // This is queued before input deliveries. Key projections catch up to the newly
            // captured publication before any action or draw can read it, including status-only
            // landings which the legacy pump's catalog-generation notice cannot see.
            d.store_changed(StoreId::Hubs.ord(), after.0);
        }
    }

    pub(super) fn update_home_chrome(&mut self, d: &mut Dispatcher<AppHost>, dt: f32) {
        let selected = self.navigation_presentation().view_tab.unwrap_or(0) as i32;
        let focus = Self::home_focus(d);
        crate::ui::widgets::tab_row_update_with(self.chrome.labels(), selected, self.chrome.focus(focus), dt);
        self.chrome.members(selected, focus, &mut d.nav.tabs.strip);
    }

    pub(super) fn prepare_home_chrome(&self) {
        crate::ui::widgets::tab_glass_prepare_with(self.chrome.labels());
    }

    pub(super) fn home_command(&mut self, command: HomeCmd) -> bool {
        if self.home_commands.contains(&command) { return true; }
        if self.home_commands.len() >= 8 { return false; }
        self.home_commands.push_back(command);
        true
    }

    fn deliver_home_commands(&mut self, d: &mut Dispatcher<AppHost>, route: Route) {
        if !matches!(route, Route::Home) { return; }
        let Some(entry) = d.nav.top_page() else { return };
        if !matches!(entry.arg.route(), Some(Route::Home))
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

    pub(super) fn request_home_menu(&mut self, d: &Dispatcher<AppHost>) -> bool {
        let Some(entry) = d.nav.top_page() else { return false };
        let Some(home) = entry.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::home::HomeScreen>()) else { return false };
        if <crate::screens::home::HomeScreen as Screen<AppHost>>::strip_reachable(home) { return false; }
        let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: Self::home_focus(d) }, owner: InputOwner::Entry(entry.id) };
        let cx = parts.cx::<AppHost>(AppViews {
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(),
        }, self.measure);
        if home.grid_position::<AppHost>(parts.focus.current, &cx).is_none() { return false; }
        self.home_command(HomeCmd::ItemMenu)
    }

    pub(super) fn open_content_menu(&mut self, d: &Dispatcher<AppHost>, entry: EntryId, ret: &ReturnState<u32, PageMemory>) -> Option<super::MenuHost> {
        let e = d.nav.entry(entry)?;
        let screen = e.inst.as_ref()?.screen.as_any()?;
        let mut parts = CxParts { tick: Tick { ms: 0, dt_us: 0 },
            press: crate::ui::machine::PressRead { scale: 1.0, is_long: false },
            focus: crate::ui::machine::FocusRead { current: None },
            owner: crate::ui::machine::InputOwner::Entry(entry) };
        parts.owner = crate::ui::machine::InputOwner::Entry(entry);
        parts.focus.current = ret.focus;
        let cx = parts.cx::<AppHost>(AppViews {
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(),
        }, self.measure);
        let host = if let Some(page) = screen.downcast_ref::<crate::screens::detail::DetailScreen>() {
            let sid = match &e.arg { AppArg::Content(ContentArg::Detail { sid, .. }) => *sid, _ => return None };
            let opener = crate::ui::popover::Opener {
                rect: page.focused_rect::<AppHost>(ret.focus, &cx, At::Drawn),
                ..crate::ui::popover::Opener::NONE
            };
            if let Some((rk, mark)) = page.focused_season(ret.focus) {
                crate::ui::item_menu::open_season(sid, &rk, mark, opener);
                super::MenuHost::Detail
            } else if let Some((rk, mark)) = page.focused_episode(ret.focus) {
                crate::ui::item_menu::open_episode(sid, &rk, mark, opener);
                super::MenuHost::Detail
            } else if let Some(item) = page.focused_related(ret.focus).filter(|m| crate::ui::item_menu::has_actions(m)) {
                crate::ui::item_menu::open(item, false, opener);
                super::MenuHost::Related
            } else { return None; }
        } else if let Some(page) = screen.downcast_ref::<crate::screens::person::PersonScreen>() {
            let item = page.focused_item(ret.focus).filter(|m| crate::ui::item_menu::has_actions(m))?;
            let opener = crate::ui::popover::Opener {
                rect: page.focused_rect::<AppHost>(ret.focus, &cx, At::Drawn),
                ..crate::ui::popover::Opener::NONE
            };
            crate::ui::item_menu::open(item, false, opener);
            super::MenuHost::Person
        } else { return None; };
        self.menu_opener = Some((entry, ret.focus));
        Some(host)
    }

    /// Render-only inspection of the captured opener. The entry, not a global screen, owns it.
    pub(super) fn redraw_opener(&self, d: &Dispatcher<AppHost>) {
        if !crate::ui::item_menu::visible() { return; }
        let Some((entry, focus)) = self.menu_opener else { return };
        if d.nav.top_page().map(|e| e.id) != Some(entry) { return; }
        let Some(screen) = d.nav.entry(entry).and_then(|e| e.inst.as_ref()).and_then(|i| i.screen.as_any()) else { return };
        // The page pass may be submitting a cached host quad. Opener lifts are live paint
        // above that quad, like Popover::scrim_lifting's legacy callback scope.
        let _live = crate::ui::popover::host::live();
        let mut parts = CxParts { tick: Tick { ms: 0, dt_us: 0 },
            press: crate::ui::machine::PressRead { scale: 1.0, is_long: false },
            focus: crate::ui::machine::FocusRead { current: None },
            owner: crate::ui::machine::InputOwner::Entry(entry) };
        parts.owner = crate::ui::machine::InputOwner::Entry(entry);
        parts.focus.current = focus;
        let cx = parts.cx::<AppHost>(AppViews {
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(),
        }, self.measure);
        let mut frame = DrawFrame::with_navigation(&cx, crate::ui::Painter::root(), self.navigation_presentation());
        if let Some(page) = screen.downcast_ref::<crate::screens::detail::DetailScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        } else if let Some(page) = screen.downcast_ref::<crate::screens::person::PersonScreen>() {
            page.redraw_focused::<AppHost>(&mut frame, focus);
        } else if let Some(page) = screen.downcast_ref::<crate::screens::home::HomeScreen>() {
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
                let cached = matches!(s.style, Style::Sheet | Style::Opaque { snapshot: true } | Style::Alert);
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
        for (_, cached, closing) in self.held.drain(..) {
            crate::ui::popover::surface_released(cached, closing);
        }
    }
}

impl Rig<AppHost> for Bridge {
    fn page_updates(&self) -> bool { self.legacy_host_live }
    fn draw_chrome(&mut self, arg: &AppArg, _parts: &CxParts<u32>, nav: crate::ui::screen::NavPresentation) {
        if !matches!(arg.route(), Some(Route::Home)) { return; }
        let p = crate::ui::Painter::root().alpha(nav.chrome_alpha);
        let labels = self.chrome.labels();
        crate::ui::widgets::draw_tab_row_with(labels, p);
        crate::ui::widgets::profile_chip_with(p, self.chrome.profile(), crate::ui::widgets::bar_glass_wanted_with(labels));
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
            views: AppViews {
                hubs: self.hubs.view(),
                listing: self.listing.view(),
                directory: self.directory.view(),
                section_hubs: self.section_hubs.view(),
            },
            measure: self.measure,
        }
    }
    fn deliver(&mut self, to: MachineId, msg: &AppMsg, parts: &CxParts<u32>, fx: &mut Effects<'_, AppHost>) -> Handled {
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
        let cx = parts.cx::<AppHost>(AppViews {
            hubs: self.hubs.view(),
            listing: self.listing.view(),
            directory: self.directory.view(),
            section_hubs: self.section_hubs.view(),
        }, self.measure);
        match msg {
            AppMsg::Store(cmd) => step_store(cmd, &cx, fx),
            AppMsg::HubsResult(result) => {
                crate::stores::hubs::land(result);
                Handled::Yes
            }
            AppMsg::StoreWork(crate::stores::StoreWork::Hubs) => {
                crate::stores::hubs::HubsStore.step(&crate::stores::StoreEv::Pump { dt: parts.tick.dt() }, &cx, fx)
            }
            AppMsg::StoreWork(crate::stores::StoreWork::BrowseDiscovery) => {
                crate::stores::browse::discover_pump();
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
        match fx {
            AppFx::Store(id, cmd) => out.push(Fx::Deliver(MachineId::Store(id.ord()), Delivery::Machine(AppMsg::Store(cmd)))),
            AppFx::StoreWork(work) => out.push(Fx::Deliver(
                MachineId::Store(work.store().ord()), Delivery::Machine(AppMsg::StoreWork(work)))),
            AppFx::Consent(ConsentCmd::Record { errors, usage }) => self.consent.record(errors, usage),
            AppFx::Loop(req) => self.reqs.push(req),
            AppFx::Content(req) => self.content_reqs.push((from, req, self.effect_return.clone())),
            AppFx::Home(req) => self.home_reqs.push((from, req, self.effect_return.clone())),
        }
    }
    fn log(&mut self, line: &str) {
        crate::log(line);
    }
    fn prepare(&mut self, _b: &mut Budget, _present: &mut Present) {}
    fn ls2_pump(&mut self) {}
    fn opaque_route(&mut self, _video_plane_bound: bool) {}
    fn clear_opaque_region(&mut self) {}
    fn now_us(&self) -> u64 {
        (self.now_us)()
    }
    fn back_at_root(&mut self) {
        self.reqs.push(LoopReq::BackAtRoot);
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

/// The dispatcher's frame, once per loop iteration right after NAV COMMIT: the committed route
/// as a `Root` on the first frame or a `Replace` CUT when it moved, the stores' notices, then
/// the ten steps WITHOUT the draw (the loop draws at its own slot, on its own gate). Returns the
/// top page's heartbeat word.
#[cfg(test)]
pub(super) fn frame(
    d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: Route, trail: &super::Trail,
    tick: Tick, inputs: Vec<InputEvent<u32>>,
) -> (&'static str, FrameReport) {
    frame_with_tap(d, rig, route, trail, tick, inputs, &mut NoTap)
}

pub(super) fn frame_with_tap(
    d: &mut Dispatcher<AppHost>,
    rig: &mut Bridge,
    route: Route,
    trail: &super::Trail,
    tick: Tick,
    inputs: Vec<InputEvent<u32>>,
    tap: &mut dyn crate::ui::dispatch::Tap<AppHost>,
) -> (&'static str, FrameReport) {
    frame_with_results(d, rig, route, trail, tick, inputs, take_live_results, tap)
}

pub(super) type AppResults = Vec<(crate::ui::machine::Addr, AppMsg)>;

fn take_live_results() -> AppResults {
    let mut results = crate::stores::hubs::take_results();
    results.sort_by_key(|result| result.request_id());
    results.into_iter().map(|result| (
        crate::ui::machine::Addr {
            to: MachineId::Store(StoreId::Hubs.ord()),
            req: crate::ui::machine::RequestId(result.request_id()),
        },
        AppMsg::HubsResult(result),
    )).collect()
}

/// One dispatcher path for live or supplied adapter results. The supplier runs at ingest, after
/// frame-view capture, and this function never additionally polls a live mailbox. Supplying
/// results alone is not offline replay: boot restoration and request suppression are separate.
#[allow(clippy::too_many_arguments)]
pub(super) fn frame_with_results(
    d: &mut Dispatcher<AppHost>,
    rig: &mut Bridge,
    route: Route,
    trail: &super::Trail,
    tick: Tick,
    inputs: Vec<InputEvent<u32>>,
    take: impl FnOnce() -> AppResults,
    tap: &mut dyn crate::ui::dispatch::Tap<AppHost>,
) -> (&'static str, FrameReport) {
    rig.capture_views(d);
    sync_page(d, route, trail);
    rig.capture_chrome(d, route);
    rig.deliver_home_commands(d, route);
    for (id, gen) in crate::stores::take_notices() {
        d.store_changed(id.ord(), gen);
    }
    let surface = d.surface_up();
    // a surface's springs and inputs are the PANEL's damage, not the page's (`popover::own_motion`)
    let _own = surface.then(crate::ui::popover::own_motion);
    if surface && !inputs.is_empty() {
        crate::ui::popover::note_own_damage();
    }
    let results = take();
    let report = d.frame_with(rig, tick, inputs, results, tap, false);
    d.prune(&report.unmounted);
    rig.sync_host(d);
    if report.presented {
        // the dispatcher's gate wants a frame: the loop's gate presents it
        crate::ui::idle::invalidate();
    }
    let word = d.top_screen().map_or("", |s| s.name());
    debug_assert_eq!(word, route_word(super::page_of(route)), "the tree's top page names the committed route");
    (word, report)
}

/// Follow external legacy route changes without replacing a covered content instance.
fn sync_page(d: &mut Dispatcher<AppHost>, route: Route, trail: &super::Trail) {
    use crate::ui::screen::ScreenArg;
    if d.has_pending_navigation() { return; }
    let route = super::page_of(route);
    let want = match route {
        Route::Detail | Route::Person => AppArg::from_node(trail.top()),
        Route::Player { .. } => AppArg::Legacy(Route::Player { overlay: super::Overlay::None }),
        _ => AppArg::Legacy(route),
    };
    if d.nav.top_page().map(|e| e.arg.same_instance(&want)).unwrap_or(false) {
        return;
    }
    let existing = d.nav.tabs.stack.entries.iter().rev()
        .find(|e| e.arg.same_instance(&want)).map(|e| e.id);
    let op = if let Some(id) = existing {
        NavOp::PopTo(id)
    } else if d.nav.top_page().is_none() || matches!(route, Route::Login | Route::Profiles | Route::Onboard | Route::Home) {
        NavOp::Root(want)
    } else {
        NavOp::Push(want)
    };
    d.request(MachineId::Nav, op);
}

pub(super) fn page_node(d: &Dispatcher<AppHost>) -> Option<super::Node> {
    let entry = d.nav.top_page()?;
    match &entry.arg {
        AppArg::Content(ContentArg::Detail { sid, rk }) => Some(super::Node::Detail {
            sid: *sid, rk: rk.clone(),
            spot: match d.return_state().memory { PageMemory::Detail(s) => s.spot, _ => Default::default() },
        }),
        AppArg::Content(ContentArg::Person { sid, key, guid, name, thumb }) =>
            Some(super::Node::Person { sid: *sid, key: key.clone(), guid: guid.clone(), name: name.clone(), thumb: thumb.clone() }),
        AppArg::Legacy(Route::Library) => Some(super::Node::Library),
        AppArg::Legacy(Route::Search) => Some(super::Node::Search),
        AppArg::Legacy(Route::Home) => Some(super::Node::Home),
        _ => None,
    }
}

pub(super) fn content_probe(d: &Dispatcher<AppHost>, rig: &Bridge) -> String {
    use std::fmt::Write;
    let Some(page) = d.nav.top_page() else { return String::new() };
    if matches!(page.arg, AppArg::Legacy(Route::Home)) {
        let Some(home) = page.inst.as_ref().and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<crate::screens::home::HomeScreen>()) else { return String::new() };
        let focus = Bridge::home_focus(d);
        let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
            focus: crate::ui::machine::FocusRead { current: focus }, owner: InputOwner::Entry(page.id) };
        let cx = parts.cx::<AppHost>(AppViews {
            hubs: rig.hubs.view(),
            listing: rig.listing.view(),
            directory: rig.directory.view(),
            section_hubs: rig.section_hubs.view(),
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
    if !matches!(page.arg, AppArg::Content(_)) { return String::new(); }
    let Some(InputOwner::Entry(owner)) = d.nav.input_owner() else { return String::new() };
    let Some(instance) = d.nav.entry(owner).and_then(|e| e.inst.as_ref()) else { return String::new() };
    let focus = d.focus();
    let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 },
        press: crate::ui::machine::PressRead { scale: 1.0, is_long: false },
        focus: crate::ui::machine::FocusRead { current: focus }, owner: InputOwner::Entry(owner) };
    let cx = parts.cx::<AppHost>(AppViews {
        hubs: rig.hubs.view(),
        listing: rig.listing.view(),
        directory: rig.directory.view(),
        section_hubs: rig.section_hubs.view(),
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
            let _ = write!(out, " card={} alt={} show={} sid={} rk=", card as u8, crate::ui::alt_sources::is_open() as u8, show as u8, sid.raw());
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

/// Present the Settings surface at its root (idempotent while it is up) — the account menu's
/// `Settings` row, by key and by click.
pub(super) fn open_settings(d: &mut Dispatcher<AppHost>) {
    open_settings_at(d, SettingsPage::Root);
}

/// …and the DEV boot target's door: the same surface, rooted at `page` (see [`AppArg`] for why
/// the target is a root rather than a push).
pub(super) fn open_settings_at(d: &mut Dispatcher<AppHost>, page: SettingsPage) {
    if surface_up(d, is_settings) {
        return;
    }
    d.nav.next_style = Style::Opaque { snapshot: true };
    d.request(MachineId::Nav, NavOp::Present(AppArg::Settings(page)));
}

/// Present the first-run consent question at its first stage (idempotent while it is up).
pub(super) fn open_first_run_consent(d: &mut Dispatcher<AppHost>) {
    open_first_run_consent_at(d, 0);
}

/// …at `stage`, which only `/tmp/plxnative-consent=product` ever names.
pub(super) fn open_first_run_consent_at(d: &mut Dispatcher<AppHost>, stage: u8) {
    if surface_up(d, is_first_run_consent) {
        return;
    }
    d.nav.next_style = Style::Opaque { snapshot: false };
    d.request(MachineId::Nav, NavOp::Present(AppArg::FirstRunConsent(stage)));
}

/// Dismiss whichever surface is up (the loop's teardown paths: sign-out, a profile reset).
pub(super) fn dismiss_surfaces(d: &mut Dispatcher<AppHost>) {
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
pub(super) fn dismiss_surfaces_now(d: &mut Dispatcher<AppHost>) {
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
fn surface_up(d: &Dispatcher<AppHost>, which: fn(&AppArg) -> bool) -> bool {
    d.nav
        .modals
        .surfaces
        .iter()
        .any(|s| which(&s.entry.arg) && s.phase != Phase::Hidden)
}

/// Is the Settings family up (any phase)? The loop's `settings::is_open()` twin.
pub(super) fn settings_up(d: &Dispatcher<AppHost>) -> bool {
    surface_up(d, is_settings)
}

/// Is the first-run question up (any phase)?
pub(super) fn consent_up(d: &Dispatcher<AppHost>) -> bool {
    surface_up(d, is_first_run_consent)
}

/// Is the tree's top PAGE an owned screen rather than a `LegacyPage` — i.e. does the DISPATCHER
/// draw it? The predicate is the screen's own `FocusSource`, not a list of routes, so a page
/// migrated in a later phase joins this answer by being written, not by being enumerated here.
/// Today the one page it is true for is first-run Favourites (`Route::Onboard`).
///
/// **`route` is not redundant, and leaving it out drew a stale page for a frame.** A `LoopReq`
/// that flips the route (`OnboardDone` → Home) is drained AFTER the dispatcher's frame, so
/// between that drain and the next NAV COMMIT the tree's top page is still the one the loop has
/// just navigated away from. Answering `true` there would skip the loop's own page pass and draw
/// the retired screen for one more frame — the first-run editor flashing over the Home the user
/// has just asked for. Requiring the tree to AGREE with the committed route makes the disagreement
/// resolve the safe way round: the loop draws its legacy page and the tree draws only surfaces,
/// which is what it did on every frame before the migration.
pub(super) fn page_owned(d: &Dispatcher<AppHost>, route: Route) -> bool {
    d.nav.top_page().and_then(|e| e.arg.route()) == Some(super::page_of(route))
        && d.top_screen().map_or(false, |s| s.focus_source() == FocusSource::Engine)
}

pub(super) fn owns_input(d: &Dispatcher<AppHost>, route: Route) -> bool {
    d.surface_up() || (matches!(super::modal_of(route), super::Modal::None) && d.owns_input())
}

/// The topmost surface's heartbeat word, for the dev triggers that have to wait for a particular
/// page of the family to be up (`plxnative-legaldoc`, `plxnative-alert`).
pub(super) fn surface_word(d: &Dispatcher<AppHost>) -> Option<&'static str> {
    d.top_surface_name()
}

/// The host page under the surfaces is FROZEN (§8.3): the loop skips its update.
pub(super) fn host_frozen(d: &Dispatcher<AppHost>) -> bool {
    d.host_policy().0 == HostUpdate::Frozen
}

/// The host page is REPLACED: the loop skips drawing it.
pub(super) fn host_replaced(d: &Dispatcher<AppHost>) -> bool {
    d.host_policy().1 == HostRender::Replaced
}

/// The heartbeat's ` overlay=<word>` from the topmost surface, if one is up.
///
/// **`"onboard"` is the Home-sources editor mounted a second time, inside the family** (§6.2
/// "Onboard ×2"): `SettingsPage::Favourites` mounts the very same `OnboardScreen` the first-run
/// route does, and `RouteSurface::top_word` (`screens/settings.rs`) answers whatever the top
/// INNER page's own `Screen::name` says without caring which stack put it there — so this arm is
/// what turns that page's name into the family's `overlay=` word once `OnboardScreen::name`
/// itself answers `word::ONBOARD` for the settings-mounted instance rather than `word::SETTINGS`
/// (a `screens/onboard.rs` change; `app/mod.rs`'s `heartbeat_word_tests` has the full account of
/// why the shared `settings` word was silently wrong). Until that companion change lands this arm
/// is reachable from no live path — `top_surface_name()` never yields `"onboard"` today — but it
/// costs nothing to carry ahead of it, and leaving it out is exactly the kind of gap that turns
/// "the screen was renamed" into "the fps scene silently measures the wrong page" the day someone
/// does make that name change without checking here.
pub(super) fn overlay_word(d: &Dispatcher<AppHost>) -> Option<&'static str> {
    Some(match d.top_surface_name()? {
        "settings" => " overlay=settings",
        "privacy" => " overlay=privacy",
        "legal" => " overlay=legal",
        "consent" => " overlay=consent",
        "onboard" => " overlay=onboard",
        _ => return None,
    })
}

/// A key for the dispatcher: the raw SDL fields classified once (`ui::consts::classify`), the
/// fork's packed state read as an edge.
pub(super) fn key_input(sym: u32, wcode: u32, state: u32, now: Tick, source: Source) -> InputEvent<u32> {
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

pub(super) fn pointer_input(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    InputEvent {
        at: now,
        source: Source::Sdl,
        kind: InputKind::Pointer { x, y, hit: None },
    }
}

pub(super) fn click_input(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    InputEvent {
        at: now,
        source: Source::Sdl,
        kind: InputKind::Click { x, y, hit: None },
    }
}

/// A wheel tick as the key it stands for on a vertical flow (the family's `on_updown`).
pub(super) fn wheel_input(dy: i32, now: Tick) -> Vec<InputEvent<u32>> {
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
pub(super) fn release_input(now: Tick) -> InputEvent<u32> {
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
pub(super) fn script_key(key: Key, now: Tick) -> Vec<InputEvent<u32>> {
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
mod tests {
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
                crate::browse::reset();
                crate::pms::seed_grid_for_test(3, 4);
                let mut d = Dispatcher::<AppHost>::new();
                let mut rig = Bridge::for_test(|| 0);
                frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
                if reordered { crate::pms::reverse_test_shelves(); }
                frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
                let entry = d.nav.top_page().unwrap().id;
                let instance = d.nav.instance_of(entry).unwrap();
                d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                    Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row, col })))));
                for i in 2..40 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
                let removed_rk = rig.with_home(&d, |home, cx, focus|
                    home.focused_item::<AppHost>(focus, cx).unwrap().rk.clone()).unwrap();
                d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
                let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
                for i in 0..count {
                    d.request(MachineId::Nav, NavOp::Push(AppArg::Legacy(Route::Library)));
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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn mounted_home_navigation_stays_inside_a_full_or_oversized_catalog() {
        let _guard = crate::testlock::serial();
        for offered in [crate::pms::MAX_SHELVES, crate::pms::MAX_SHELVES + 5] {
            crate::browse::reset();
            crate::pms::seed_grid_for_test(offered, 3);
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
            frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
            let mut i = 2;
            for row in 0..crate::pms::MAX_SHELVES {
                frame(&mut d, &mut rig, Route::Home, tick(i), script_key(Key::Down, tick(i)));
                i += 1;
                assert_eq!(rig.with_home(&d, |home, cx, focus| home.grid_position::<AppHost>(focus, cx)),
                    Some(Some((row, 0))), "offered={offered}, row={row}");
            }
            let last = d.focus();
            for _ in 0..3 {
                frame(&mut d, &mut rig, Route::Home, tick(i), script_key(Key::Down, tick(i)));
                i += 1;
                assert_eq!(d.focus(), last, "DOWN cannot escape the capped final shelf");
            }
            let instance = d.nav.instance_of(last.unwrap().entry).unwrap();
            for (row, col) in [(usize::MAX, 0), (0, usize::MAX)] {
                d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                    Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row, col })))));
                frame(&mut d, &mut rig, Route::Home, tick(i), vec![]);
                i += 1;
                assert_eq!(d.focus(), last, "an invalid addressed request must not displace focus");
            }
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn hero_edge_keys_page_without_seating_a_pager_or_leaving_the_control() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
        let selected = |rig: &Bridge, d: &Dispatcher<AppHost>| rig.with_home(d,
            |home, cx, _| home.hero_item::<AppHost>(cx).unwrap().rk.clone()).unwrap();
        let mut i = 2;
        for (direction, control) in [(Key::Left, 0), (Key::Right, 1), (Key::Right, 1)] {
            if control == 1 && d.focus().unwrap().elem == 0 {
                frame(&mut d, &mut rig, Route::Home, tick(i), script_key(Key::Right, tick(i)));
                i += 1;
            }
            let before = selected(&rig, &d);
            frame(&mut d, &mut rig, Route::Home, tick(i), script_key(direction, tick(i)));
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
                frame(&mut d, &mut rig, Route::Home, tick(i), vec![]);
                i += 1;
            }
        }
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn a_removed_home_type_tab_recovers_to_home_not_the_profile_chip() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
        let entry = d.nav.top_page().unwrap().id;
        let movies = crate::screens::home::STRIP_MOVIES_ELEM;
        d.nav.tabs.strip.push(crate::ui::containers::tabs::StripMember::new(movies,
            crate::ui::Rect::new(800.0, 50.0, 160.0, 60.0)));
        d.set_focus_in(Some(FocusKey { entry, elem: movies }), Some(crate::ui::containers::tabs::STRIP));
        // Republish the current empty-library strip: the previous Movies destination is gone.
        frame(&mut d, &mut rig, Route::Home, tick(2), vec![]);
        assert!(!d.nav.tabs.strip.iter().any(|member| member.elem == movies));
        assert_eq!(d.focus(), Some(FocusKey { entry, elem: crate::screens::home::STRIP_HOME_ELEM }));
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn home_return_restores_the_offscreen_item_and_viewport_after_retention_or_eviction() {
        let _guard = crate::testlock::serial();
        for (evict, reorder) in [(true, false), (false, false), (true, true), (false, true)] {
            crate::browse::reset();
            crate::pms::seed_grid_for_test(6, 24);
            let mut d = Dispatcher::<AppHost>::new();
            let mut rig = Bridge::for_test(|| 0);
            frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
            d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
            let home = d.nav.top_page().unwrap().id;
            let instance = d.nav.instance_of(home).unwrap();
            d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 4, col: 18 })))));
            for i in 1..80 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
            for i in 80..84 { frame(&mut d, &mut rig, Route::Home, tick(i), script_key(Key::Left, tick(i))); }
            for i in 84..160 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
            let focus = d.focus().unwrap();
            let before = rig.with_home(&d, |s, cx, f| {
                assert_eq!(s.grid_position::<AppHost>(f, cx), Some((4, 14)));
                s.focused_rect::<AppHost>(f, cx, At::Drawn).unwrap()
            }).unwrap();
            let count = if evict { crate::ui::containers::stack::CAP + 1 } else { 1 };
            for i in 0..count {
                d.request(MachineId::Nav, NavOp::Push(AppArg::Legacy(Route::Library)));
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
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
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
            crate::browse::reset();
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
            crate::pms::reset();
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
        crate::browse::reset();
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, Route::Home, tick(1), script_key(Key::Down, tick(1)));
        for i in 2..40 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
        let pressed = d.focus().unwrap();
        let mut down = script_key(Key::Ok, tick(40));
        down.truncate(1);
        frame(&mut d, &mut rig, Route::Home, tick(40), down);
        assert_eq!(d.input.arm.unwrap().key, pressed);
        crate::pms::remove_test_item("1");
        frame(&mut d, &mut rig, Route::Home, tick(41), vec![]);
        assert_ne!(d.focus(), Some(pressed));
        frame(&mut d, &mut rig, Route::Home, tick(42), vec![release_input(tick(42))]);
        for i in 43..80 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
        assert!(rig.take_home_reqs().is_empty(), "a removed arm must not become a press on the replacement cursor");
        crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
    }

    #[test]
    fn a_midframe_reorder_keeps_painted_keys_matched_and_a_click_activates_the_seen_item() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        frame(&mut d, &mut rig, Route::Home, tick(1), script_key(Key::Down, tick(1)));
        for i in 2..40 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
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

        frame(&mut d, &mut rig, Route::Home, tick(40), vec![click_input(rect.cx(), rect.cy(), tick(40))]);
        frame(&mut d, &mut rig, Route::Home, tick(41), vec![release_input(tick(41))]);
        for i in 42..60 { frame(&mut d, &mut rig, Route::Home, tick(i), vec![]); }
        let requests = rig.take_home_reqs();
        assert!(requests.iter().any(|(_, request, _)| matches!(request, HomeReq::Play { rk, .. } if rk == "1")),
            "the old presented map named item 1, not the replacement now at its old position");
        assert!(!requests.iter().any(|(_, request, _)| matches!(request, HomeReq::Play { rk, .. } if rk == "3")));
        crate::pms::reset();
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
        crate::browse::reset();
        crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        let trail = super::super::Trail::new();
        let mut tap = Results::default();
        let req = crate::pms::queue_test_landing(Some(5));
        super::frame_with_tap(&mut d, &mut rig, Route::Home, &trail, tick(0), vec![], &mut tap);
        assert_eq!(crate::pms::hub_len(0), 5);
        assert_eq!(tap.0, vec![Addr {
            to: MachineId::Store(StoreId::Hubs.ord()), req: crate::ui::machine::RequestId(req),
        }]);
        super::frame_with_tap(&mut d, &mut rig, Route::Home, &trail, tick(1), vec![], &mut tap);
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
        crate::pms::reset();
    }

    #[test]
    fn supplied_home_results_use_the_dispatcher_without_consuming_live_arrivals() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
        crate::pms::queue_test_landing(Some(5));
        let captured = take_live_results().pop().unwrap();
        let AppMsg::HubsResult(result) = captured.1 else { unreachable!() };
        let payload = crate::pms::record::encode(&result);
        let decoded = crate::pms::record::decode(payload, |_| None).unwrap();
        crate::pms::queue_test_landing(Some(9));

        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        let trail = super::super::Trail::new();
        super::frame_with_results(&mut d, &mut rig, Route::Home, &trail, tick(0), vec![],
            || vec![(captured.0, AppMsg::HubsResult(decoded))], &mut NoTap);
        assert_eq!(crate::pms::hub_len(0), 5, "the decoded result reaches the actual store");
        super::frame_with_results(&mut d, &mut rig, Route::Home, &trail, tick(1), vec![],
            Vec::new, &mut NoTap);
        assert_eq!(crate::pms::hub_len(0), 5, "an empty supplied frame cannot fall back to live data");
        super::frame_with_tap(&mut d, &mut rig, Route::Home, &trail, tick(2), vec![], &mut NoTap);
        assert_eq!(crate::pms::hub_len(0), 9, "the live arrival was preserved for a live ingest");
        crate::pms::reset();
    }

    #[test]
    fn store_work_is_addressed_and_idle_polling_does_not_invent_a_change() {
        use crate::stores::StoreWork;
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::pms::seed_for_test(0, crate::pms::HubState::Ready);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        let before = crate::stores::gen(StoreId::Hubs);
        d.emit(MachineId::Nav, Fx::App(AppFx::StoreWork(StoreWork::Hubs)));
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
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
        crate::pms::reset();
    }

    use super::*;
    use crate::ui::screen::ScreenArg;

    fn frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: Route, tick: Tick, inputs: Vec<InputEvent<u32>>) -> (&'static str, FrameReport) {
        let mut trail = super::super::Trail::new();
        if matches!(route, Route::Detail) {
            trail.push(super::super::to_detail(crate::plex::ServerId::UNSET, "1001"));
        }
        super::frame(d, rig, route, &trail, tick, inputs)
    }

    #[test]
    fn content_instances_compare_item_identity_and_keep_distinct_entries() {
        let a = AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: "1001".into() });
        let b = AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: "1002".into() });
        assert_eq!(a.id(), b.id());
        assert!(!a.same_instance(&b));
        assert!(a.same_instance(&a.clone()));
        let legacy = AppArg::Legacy(Route::Detail);
        assert!(!a.same_instance(&legacy));
    }

    fn tick(i: u32) -> Tick {
        Tick {
            ms: i * 16,
            dt_us: 16_000,
        }
    }

    const EVERY_ROUTE: [Route; 11] = [
        Route::Login,
        Route::Profiles,
        Route::Onboard,
        Route::Home,
        Route::Account {
            over: super::super::nav::BarHost::Library,
        },
        Route::ItemMenu {
            over: super::super::nav::MenuHost::Home,
        },
        Route::Library,
        Route::Detail,
        Route::Person,
        Route::Search,
        Route::Player {
            overlay: super::super::nav::Overlay::None,
        },
    ];

    #[test]
    fn a_legacy_arg_wears_the_chrome_the_route_table_says() {
        for r in EVERY_ROUTE {
            let want = if route_wears_tab_bar(r) { Chrome::TabBar } else { Chrome::None };
            assert_eq!(AppArg::Legacy(r).chrome(), want, "{}", route_word(r));
        }
        assert_eq!(AppArg::Settings(SettingsPage::Root).chrome(), Chrome::None);
        // …and the root payload is a boot address, never an identity: the two Settings arguments
        // below are ONE screen, which is what stops a dev boot target minting a second surface.
        assert!(AppArg::Settings(SettingsPage::Root).same_instance(&AppArg::Settings(SettingsPage::Legal)));
        assert!(!AppArg::Settings(SettingsPage::Root).same_instance(&AppArg::FirstRunConsent(0)));
    }

    #[test]
    fn every_legacy_page_names_the_heartbeat_word() {
        for r in EVERY_ROUTE {
            assert_eq!(LegacyPage::new(r).name(), route_word(r));
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
        frame(&mut d, &mut rig, Route::Library, tick(0), vec![]);
        let before = crate::stores::gen(StoreId::Search);
        d.emit(
            MachineId::Nav,
            Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
        );
        frame(&mut d, &mut rig, Route::Library, tick(1), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Search), before + 1, "the store was stepped in the drain");
        frame(&mut d, &mut rig, Route::Library, tick(2), vec![]);
        assert!(notices(&d).ends_with("notices=1"), "{}", notices(&d));
        crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
        frame(&mut d, &mut rig, Route::Library, tick(3), vec![]);
        assert!(notices(&d).ends_with("notices=2"), "{}", notices(&d));
        let g = crate::stores::gen(StoreId::Search);
        d.emit(
            MachineId::Nav,
            Fx::Deliver(
                MachineId::Store(StoreId::Browse.ord()),
                Delivery::Machine(AppMsg::Store(StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
            ),
        );
        frame(&mut d, &mut rig, Route::Library, tick(4), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Search), g);
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
    /// precisely what fixes it.
    #[test]
    fn route_flips_preserve_content_and_player_origin_entries() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        assert_eq!(frame(&mut d, &mut rig, Route::Home, tick(0), vec![]).0, "home");
        assert_eq!(d.nav.tabs.stack.depth(), 1);
        let home = d.nav.top_page().map(|e| e.id);
        assert_eq!(frame(&mut d, &mut rig, Route::Home, tick(1), vec![]).0, "home");
        assert_eq!(d.nav.top_page().map(|e| e.id), home, "a steady route mints nothing");
        assert_eq!(frame(&mut d, &mut rig, Route::Detail, tick(2), vec![]).0, "detail");
        assert_eq!(d.nav.tabs.stack.depth(), 2, "Detail preserves its legacy Home origin");
        assert_ne!(d.nav.top_page().map(|e| e.id), home);
        let detail = d.nav.top_page().map(|e| e.id);
        let body = d.top_page();
        assert_eq!(
            frame(&mut d, &mut rig, Route::Player { overlay: super::super::nav::Overlay::None }, tick(3), vec![]).0,
            "player"
        );
        assert_eq!(d.top_screen().map(|s| s.render()), Some(RenderStrategy::VideoPlane));
        assert_eq!(d.nav.tabs.stack.depth(), 3);
        frame(&mut d, &mut rig, Route::Detail, tick(4), vec![]);
        assert_eq!(d.nav.top_page().map(|e| e.id), detail);
        assert_eq!(d.top_page(), body, "player return uncovers the same Detail instance");
    }

    /// Phase 5b: the Settings surface is PRESENTED on the tree, owns input from its first frame,
    /// names the heartbeat word of its top page, walks its own stack on BACK (root → Legal →
    /// back → root) and only then lets the container dismiss it — with the app's page untouched.
    #[test]
    fn the_settings_surface_owns_input_and_walks_its_own_stack() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        assert!(d.owns_input(), "Home is an owned page too");
        let home_owner = d.nav.input_owner();
        open_settings(&mut d);
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
        assert!(d.owns_input());
        assert_eq!(overlay_word(&d), Some(" overlay=settings"));
        assert_ne!(d.nav.input_owner(), home_owner, "Settings takes input from its Home host");
        assert!(host_frozen(&d));
        // DOWN, DOWN to Legal notices (Favourites is absent signed out: Privacy, Legal, About)
        let mut t = 2;
        let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
            let ev = script_key(key, tick(t));
            frame(d, rig, Route::Home, tick(t), ev);
            t += 1;
            frame(d, rig, Route::Home, tick(t), vec![]);
            t += 1;
        };
        press(&mut d, &mut rig, Key::Down);
        press(&mut d, &mut rig, Key::Ok);
        assert_eq!(overlay_word(&d), Some(" overlay=legal"), "OK on Legal notices pushed the index");
        press(&mut d, &mut rig, Key::Back);
        assert_eq!(overlay_word(&d), Some(" overlay=settings"), "BACK popped the inner stack");
        assert!(settings_up(&d));
        press(&mut d, &mut rig, Key::Back);
        assert_eq!(
            d.nav.modals.top().map(|s| s.phase),
            Some(Phase::Closing),
            "BACK at the surface's own root dismisses it"
        );
        assert_eq!(d.nav.tabs.stack.depth(), 1, "the app's page never moved");
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
        frame(&mut d, &mut rig, Route::Profiles, tick(0), vec![]);
        open_first_run_consent(&mut d);
        frame(&mut d, &mut rig, Route::Profiles, tick(1), vec![]);
        assert!(consent_up(&d));
        assert_eq!(overlay_word(&d), Some(" overlay=consent"));
        let _ = rig.take_reqs(); // the mount's own effects are not what this grades
        frame(&mut d, &mut rig, Route::Profiles, tick(2), script_key(Key::Back, tick(2)));
        assert_eq!(
            rig.take_reqs(),
            vec![LoopReq::BackAtRoot],
            "the screen asks the loop for the root press rather than swallowing the key"
        );
        frame(&mut d, &mut rig, Route::Profiles, tick(3), vec![]);
        assert!(
            consent_up(&d),
            "…and the question is still up: the platform took the screen, nothing was answered"
        );
    }

    /// The other half of the coexistence contract: an OWNED PAGE (first-run Favourites) is the
    /// dispatcher's to draw, and [`page_owned`] says so only while the tree AGREES with the
    /// committed route. The second assertion is the stale-frame guard its doc describes — a
    /// `LoopReq` that flips the route is drained after the dispatcher's frame, and answering
    /// `true` in that window would draw the screen the loop has just left over the one it went to.
    #[test]
    fn the_first_run_favourites_page_is_owned_only_while_the_tree_agrees_with_the_route() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        assert!(page_owned(&d, Route::Home), "Home now draws through its owned instance");
        assert_eq!(frame(&mut d, &mut rig, Route::Onboard, tick(1), vec![]).0, "onboard");
        assert!(d.owns_input(), "an owned page takes the ladders' input too, not only a surface");
        assert!(page_owned(&d, Route::Onboard));
        assert!(
            !page_owned(&d, Route::Home),
            "the route moved and the tree has not followed yet — the loop draws its own page"
        );
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
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        open_settings(&mut d);
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
        assert!(settings_up(&d));
        // **A second frame, and it is not padding.** The dispatcher's NAV COMMIT is step 7 of its
        // frame and the container Tick is step 4, so the surface `open_settings` parked is mounted
        // AFTER this frame's motion step has already run: at the end of the `tick(1)` frame it
        // exists with its spring untouched at exactly 0.0. That is correct — nothing can integrate
        // motion for a surface that did not exist when the tick ran — but it means one frame is
        // not yet "mid-open", and asserting `appear > 0.0` there fails on the frame ORDER rather
        // than on anything this test is about.
        frame(&mut d, &mut rig, Route::Home, tick(2), vec![]);
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
}
