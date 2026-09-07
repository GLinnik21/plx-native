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
//!   present gate: [`Dispatcher::draw`]. An owned PAGE (first-run Favourites) is drawn in the
//!   page closure instead. The dispatcher's own gate wanting a present becomes `idle::invalidate`.
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
use crate::screens::registry::{AppFx, AppMsg, ConsentCmd, LoopReq};
use crate::screens::settings::{Family, RouteSurface};
use crate::stores::{StoreCmd, StoreEv, StoreId};
use crate::ui::containers::modal::{HostRender, HostUpdate, Phase, Style};
use crate::ui::dispatch::{CxParts, Dispatcher, FrameReport, NoTap, Rig, Split};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Chrome, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Host,
    InputEvent, InputKind, InstanceId, Key, LogicalState, Machine, MachineId, Measure, NavOp,
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AppArg {
    Legacy(Route),
    /// The Settings family, rooted at this page (`SettingsPage::Root` for every real opening).
    Settings(SettingsPage),
    /// The first-run consent question, rooted at this stage byte (0 for every real opening;
    /// `screens::consent`'s `STAGE_PRODUCT` for `/tmp/plxnative-consent=product`).
    FirstRunConsent(u8),
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
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        // …and the same reason `id` collapses the payload: `Settings(Root)` and
        // `Settings(Legal)` are the same SCREEN, so a container must never be able to think it
        // is holding two of them.
        <Self as crate::ui::screen::ScreenArg>::id(self) == <Self as crate::ui::screen::ScreenArg>::id(other)
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
pub(super) struct AppViews;

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
    type Views<'a> = AppViews;
    type Init = BridgeInit;
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

struct AppMounter;

impl Mounter<AppHost> for AppMounter {
    fn mount(
        &mut self,
        id: InstanceId,
        arg: &AppArg,
        _ret: &ReturnState<u32>,
        cx: &Cx<'_, AppHost>,
        _fx: &mut Effects<'_, AppHost>,
    ) -> Box<dyn Screen<AppHost>> {
        let entry = match cx.owner {
            crate::ui::machine::InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        match arg {
            // the first-run Favourites screen is OWNED (§14: "retirement 5b Onboard"); the route
            // word stays the loop's while the loop still names the page
            AppArg::Legacy(Route::Onboard) => Box::new(crate::screens::onboard::OnboardScreen::first_run(entry)),
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
    consent: ConsentMachine,
    /// Requests the owned screens made of the loop this frame (§14), drained by [`frame`]'s caller.
    reqs: Vec<LoopReq>,
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
        Self {
            mounter: AppMounter,
            measure,
            consent: ConsentMachine,
            reqs: Vec::new(),
            held: Vec::new(),
            now_us,
        }
    }

    pub(super) fn take_reqs(&mut self) -> Vec<LoopReq> {
        std::mem::take(&mut self.reqs)
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
    fn split(&mut self) -> Split<'_, AppHost> {
        Split {
            mounter: &mut self.mounter,
            views: AppViews,
            measure: self.measure,
        }
    }
    fn deliver(&mut self, to: MachineId, msg: &AppMsg, parts: &CxParts<u32>, fx: &mut Effects<'_, AppHost>) -> Handled {
        let AppMsg::Store(cmd) = msg;
        let MachineId::Store(ord) = to else {
            return Handled::No;
        };
        if StoreId::from_ord(ord) != Some(cmd.store()) {
            crate::log(&format!(
                "stores: a {} command was addressed to store ordinal {} — dropped",
                cmd.store().name(),
                ord.0
            ));
            return Handled::No;
        }
        let cx = parts.cx::<AppHost>(AppViews, self.measure);
        step_store(cmd, &cx, fx)
    }
    fn timer(&mut self, _owner: MachineId, _id: TimerId, _parts: &CxParts<u32>, _fx: &mut Effects<'_, AppHost>) {}
    fn app_fx(&mut self, _from: MachineId, fx: AppFx, _parts: &CxParts<u32>, out: &mut Effects<'_, AppHost>) {
        match fx {
            AppFx::Store(id, cmd) => out.push(Fx::Deliver(MachineId::Store(id.ord()), Delivery::Machine(AppMsg::Store(cmd)))),
            AppFx::Consent(ConsentCmd::Record { errors, usage }) => self.consent.record(errors, usage),
            AppFx::Loop(req) => self.reqs.push(req),
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
pub(super) fn frame(
    d: &mut Dispatcher<AppHost>,
    rig: &mut Bridge,
    route: Route,
    tick: Tick,
    inputs: Vec<InputEvent<u32>>,
) -> (&'static str, FrameReport) {
    let top = d.nav.top_page().and_then(|e| d.nav.entry(e.id)).map(|e| e.arg);
    let want = AppArg::Legacy(route);
    match top {
        None => d.request(MachineId::Nav, NavOp::Root(want)),
        Some(r) if r != want => d.request(MachineId::Nav, NavOp::Replace(want)),
        Some(_) => {}
    }
    for (id, gen) in crate::stores::take_notices() {
        d.store_changed(id.ord(), gen);
    }
    let surface = d.surface_up();
    // a surface's springs and inputs are the PANEL's damage, not the page's (`popover::own_motion`)
    let _own = surface.then(crate::ui::popover::own_motion);
    if surface && !inputs.is_empty() {
        crate::ui::popover::note_own_damage();
    }
    let report = d.frame_with(rig, tick, inputs, vec![], &mut NoTap, false);
    d.prune(&report.unmounted);
    rig.sync_host(d);
    if report.presented {
        // the dispatcher's gate wants a frame: the loop's gate presents it
        crate::ui::idle::invalidate();
    }
    let word = d.top_screen().map_or("", |s| s.name());
    debug_assert_eq!(word, route_word(route), "the tree's top page names the committed route");
    (word, report)
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
    d.nav.top_page().map(|e| e.arg) == Some(AppArg::Legacy(route))
        && d.top_screen().map_or(false, |s| s.focus_source() == FocusSource::Engine)
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
    use super::*;
    use crate::ui::screen::ScreenArg;

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
        frame(&mut d, &mut rig, Route::Home, tick(0), vec![]);
        let before = crate::stores::gen(StoreId::Search);
        d.emit(
            MachineId::Nav,
            Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
        );
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
        assert_eq!(crate::stores::gen(StoreId::Search), before + 1, "the store was stepped in the drain");
        frame(&mut d, &mut rig, Route::Home, tick(2), vec![]);
        assert!(notices(&d).ends_with("notices=1"), "{}", notices(&d));
        crate::stores::search::apply(crate::stores::search::SearchCmd::Reset);
        frame(&mut d, &mut rig, Route::Home, tick(3), vec![]);
        assert!(notices(&d).ends_with("notices=2"), "{}", notices(&d));
        let g = crate::stores::gen(StoreId::Search);
        d.emit(
            MachineId::Nav,
            Fx::Deliver(
                MachineId::Store(StoreId::Browse.ord()),
                Delivery::Machine(AppMsg::Store(StoreCmd::Search(crate::stores::search::SearchCmd::Reset))),
            ),
        );
        frame(&mut d, &mut rig, Route::Home, tick(4), vec![]);
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
    fn the_tree_mirrors_a_route_flip_as_a_replace_cut() {
        let _g = crate::testlock::serial();
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        assert_eq!(frame(&mut d, &mut rig, Route::Home, tick(0), vec![]).0, "home");
        assert_eq!(d.nav.tabs.stack.depth(), 1);
        let home = d.nav.top_page().map(|e| e.id);
        assert_eq!(frame(&mut d, &mut rig, Route::Home, tick(1), vec![]).0, "home");
        assert_eq!(d.nav.top_page().map(|e| e.id), home, "a steady route mints nothing");
        assert_eq!(frame(&mut d, &mut rig, Route::Detail, tick(2), vec![]).0, "detail");
        assert_eq!(d.nav.tabs.stack.depth(), 1, "a Replace keeps no page beneath");
        assert_ne!(d.nav.top_page().map(|e| e.id), home);
        assert_eq!(
            frame(&mut d, &mut rig, Route::Player { overlay: super::super::nav::Overlay::None }, tick(3), vec![]).0,
            "player"
        );
        assert_eq!(d.top_screen().map(|s| s.render()), Some(RenderStrategy::VideoPlane));
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
        assert!(!d.owns_input(), "a legacy page keeps the ladders");
        open_settings(&mut d);
        frame(&mut d, &mut rig, Route::Home, tick(1), vec![]);
        assert!(d.owns_input());
        assert_eq!(overlay_word(&d), Some(" overlay=settings"));
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
        assert!(!page_owned(&d, Route::Home), "a legacy page is the loop's to draw");
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
