//! The product's SHADOW of the container tree (restructure spec §13 phase 3b, §14): a
//! `Dispatcher<AppHost>` whose only screen is [`LegacyPage`] — a `Screen` impl over today's
//! `Route`, with `FocusSource::Legacy`/`HitSource::Legacy`, so the engine and the hit map are
//! INERT for it and the legacy ladders in `app/input.rs` stay the single writer of focus and
//! hits until each screen's own phase.
//!
//! It is a shadow, not the loop: `app/run.rs` still owns the frame, the three privileged calls
//! and the route flip. After every NAV COMMIT the loop calls [`mirror`], which puts the
//! committed route on the shadow's stack as a `Replace` CUT (or a `Root` on the first frame)
//! and steps the dispatcher ONE frame with no input and no result. What that buys, today: the
//! heartbeat word now has a second source (`Screen::name` == `route_word`, asserted in a debug
//! build every frame), the container tree is exercised on every device run rather than only in
//! host tests, and the chrome table (`ScreenArg::chrome` ↔ `route_wears_tab_bar`) is one test
//! rather than two matches that can drift. What it does NOT do: receive an input, mount a real
//! screen, or touch the recorder's state hash (the phase-2 anchor fixture pins that shape; a
//! shadow entry would change it for no behaviour).
//!
//! The rig's privileged hooks are NO-OPS on purpose — `ls2_pump`, `opaque_route` and
//! `clear_opaque_region` are made by `app/run.rs` at their positional frame points and a second
//! caller would double each; `ci/check-deps.sh`'s allowlist names `app/run.rs` for them and this
//! file names none.

use std::borrow::Cow;
use std::ffi::CStr;

use crate::ui::dispatch::{CxParts, Dispatcher, NoTap, Rig, Split};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Chrome, Cx, Effects, EntryId, FocusKey, GroupId, Handled, Host, InstanceId,
    LogicalState, Machine, MachineId, Measure, NavOp, ScreenId, Tick, TimerId,
};
use crate::ui::present::Present;
use crate::ui::screen::{
    At, Dir, FocusSource, Focusable, GroupSpec, HitSource, Mounter, Placed, RenderStrategy,
    ReturnState, Screen, ScreenEvent, Step,
};
use crate::ui::screen::DrawFrame;

use super::nav::route_wears_tab_bar;
use super::{route_word, Route};

// ---------------------------------------------------------------------------------------------
// the bundle
// ---------------------------------------------------------------------------------------------

/// The application's `Host` — phase 3b(c): every screen is a `LegacyPage`, there are no stores
/// behind it yet (phase 4), no effects and no messages.
pub(super) struct AppHost;

/// A screen argument is the legacy `Route` itself (§14: "`Route` survives only as its argument").
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct LegacyArg(pub(super) Route);

impl crate::ui::screen::ScreenArg for LegacyArg {
    /// The ONE chrome answer — `route_wears_tab_bar`'s, so the table and the container agree.
    fn chrome(&self) -> Chrome {
        if route_wears_tab_bar(self.0) {
            Chrome::TabBar
        } else {
            Chrome::None
        }
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self.0 {
            Route::Login => 1,
            Route::Profiles => 2,
            Route::Onboard => 3,
            Route::Home => 4,
            Route::Account { .. } => 5,
            Route::ItemMenu { .. } => 6,
            Route::Library => 7,
            Route::Detail => 8,
            Route::Person => 9,
            Route::Search => 10,
            Route::Player { .. } => 11,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    /// Reuse-vs-remount (§5.1): the same route is the same instance. A payload-carrying route
    /// (`Account { over }`, `Player { overlay }`) compares its payload too, which is right for
    /// the shadow — the legacy loop's own cut is what changed the overlay word.
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

/// No application effect exists in the shadow: nothing it does reaches an adapter.
pub(super) enum ShadowFx {}

/// No message either: nothing is delivered to it.
pub(super) enum ShadowMsg {}

/// The read side is empty until phase 4 puts a store behind `StoreOrd`.
#[derive(Clone, Copy)]
pub(super) struct ShadowViews;

/// The recording header's initial conditions are `recorder::AppInit`'s; the shadow's are none.
#[derive(Default)]
pub(super) struct ShadowInit;

impl LogicalState for ShadowInit {
    fn write(&self, _w: &mut Canon) {}
    fn probe(&self, out: &mut String) {
        out.push_str("shadow");
    }
}

impl Host for AppHost {
    type Arg = LegacyArg;
    type Fx = ShadowFx;
    type Msg = ShadowMsg;
    type Elem = u32;
    type Views<'a> = ShadowViews;
    type Init = ShadowInit;
}

// ---------------------------------------------------------------------------------------------
// the page
// ---------------------------------------------------------------------------------------------

/// A legacy route as a `Screen` (§14). Its logical state is the route WORD, which is what the
/// legacy loop's own state is from the shadow's point of view; everything else lives in the
/// screen modules' statics until their phase.
pub(super) struct LegacyPage {
    route: Route,
    state: LegacyState,
}

struct LegacyState {
    word: &'static str,
}

impl LogicalState for LegacyState {
    fn write(&self, w: &mut Canon) {
        w.str(self.word);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(self.word);
    }
}

impl LegacyPage {
    fn new(route: Route) -> Self {
        Self {
            route,
            state: LegacyState {
                word: route_word(route),
            },
        }
    }
}

impl Machine<AppHost> for LegacyPage {
    type Ev = ScreenEvent<AppHost>;
    /// The ladders answer everything; the shadow receives no input, so `No` is a statement of
    /// ownership rather than a fallback.
    fn step(&mut self, _ev: &Self::Ev, _cx: &Cx<'_, AppHost>, _fx: &mut Effects<'_, AppHost>) -> Handled {
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
    /// The heartbeat word, byte-identical to `route_word` (§15.3) — it IS `route_word`.
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
// the rig
// ---------------------------------------------------------------------------------------------

struct ShadowMounter;

impl Mounter<AppHost> for ShadowMounter {
    fn mount(
        &mut self,
        _id: InstanceId,
        arg: &LegacyArg,
        _ret: &ReturnState<u32>,
        _cx: &Cx<'_, AppHost>,
        _fx: &mut Effects<'_, AppHost>,
    ) -> Box<dyn Screen<AppHost>> {
        Box::new(LegacyPage::new(arg.0))
    }
}

/// A `Measure` nothing reads (a `LegacyPage` measures no text) — the shape `TtfMeasure` takes
/// when the first real screen mounts here.
struct ShadowMeasure;

impl Measure for ShadowMeasure {
    fn width(&self, s: &CStr, sz: i32, _bold: bool) -> f32 {
        s.to_bytes().len() as f32 * sz as f32 * 0.5
    }
    fn cap_h(&self, sz: i32) -> f32 {
        sz as f32 * 0.7
    }
    fn line_h(&self, sz: i32) -> f32 {
        sz as f32 * 1.2
    }
}

/// What the shadow dispatcher borrows from the application: a mounter that only knows
/// `LegacyPage`, no views, a measure nothing reads, and privileged hooks that do NOTHING (the
/// module doc says why).
pub(super) struct ShadowRig {
    mounter: ShadowMounter,
    measure: ShadowMeasure,
    now_us: fn() -> u64,
}

impl ShadowRig {
    pub(super) fn new(now_us: fn() -> u64) -> Self {
        Self {
            mounter: ShadowMounter,
            measure: ShadowMeasure,
            now_us,
        }
    }
}

impl Rig<AppHost> for ShadowRig {
    fn split(&mut self) -> Split<'_, AppHost> {
        Split {
            mounter: &mut self.mounter,
            views: ShadowViews,
            measure: &self.measure,
        }
    }
    fn deliver(
        &mut self,
        _to: MachineId,
        msg: &ShadowMsg,
        _parts: &CxParts<u32>,
        _fx: &mut Effects<'_, AppHost>,
    ) -> Handled {
        match *msg {}
    }
    fn timer(&mut self, _owner: MachineId, _id: TimerId, _parts: &CxParts<u32>, _fx: &mut Effects<'_, AppHost>) {}
    fn app_fx(&mut self, _from: MachineId, fx: ShadowFx, _parts: &CxParts<u32>, _out: &mut Effects<'_, AppHost>) {
        match fx {}
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
}

// ---------------------------------------------------------------------------------------------
// the mirror
// ---------------------------------------------------------------------------------------------

/// The shadow's frame, called ONCE per loop iteration right after NAV COMMIT with the route the
/// loop committed: a `Root` when the stack is empty (the first frame), a `Replace` CUT when the
/// route moved, nothing otherwise; then one dispatcher frame with no input and no result.
/// Returns the top page's heartbeat word, which a debug build asserts against `route_word`.
pub(super) fn mirror(d: &mut Dispatcher<AppHost>, rig: &mut ShadowRig, route: Route, tick: Tick) -> &'static str {
    let top = d.nav.top_page().and_then(|e| d.nav.entry(e.id)).map(|e| e.arg.0);
    match top {
        None => d.request(MachineId::Nav, NavOp::Root(LegacyArg(route))),
        Some(r) if r != route => d.request(MachineId::Nav, NavOp::Replace(LegacyArg(route))),
        Some(_) => {}
    }
    let report = d.frame(rig, tick, vec![], vec![], &mut NoTap);
    d.prune(&report.unmounted);
    let word = d.top_screen().map_or("", |s| s.name());
    debug_assert_eq!(word, route_word(route), "the shadow's top page names the committed route");
    word
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

    /// `route_tests`' chrome half, ported (spec §11): the container's `Chrome` answer IS
    /// `route_wears_tab_bar`'s, for every route, so the two cannot drift.
    #[test]
    fn a_legacy_arg_wears_the_chrome_the_route_table_says() {
        for r in EVERY_ROUTE {
            let want = if route_wears_tab_bar(r) { Chrome::TabBar } else { Chrome::None };
            assert_eq!(LegacyArg(r).chrome(), want, "{}", route_word(r));
        }
        assert_eq!(LegacyArg(Route::Library).chrome(), Chrome::TabBar);
        assert_eq!(LegacyArg(Route::Detail).chrome(), Chrome::None);
    }

    /// Every route's page names itself with the heartbeat word — the second source of the word
    /// table (`heartbeat_word_tests` grades the first).
    #[test]
    fn every_legacy_page_names_the_heartbeat_word() {
        for r in EVERY_ROUTE {
            assert_eq!(LegacyPage::new(r).name(), route_word(r));
        }
    }

    /// The mirror: a `Root` on the first frame, a `Replace` CUT on a route flip, nothing on a
    /// steady route — and the page beneath is never kept (the legacy loop owns history).
    #[test]
    fn the_shadow_mirrors_a_route_flip_as_a_replace_cut() {
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = ShadowRig::new(|| 0);
        assert_eq!(mirror(&mut d, &mut rig, Route::Home, tick(0)), "home");
        assert_eq!(d.nav.tabs.stack.depth(), 1);
        let home = d.nav.top_page().map(|e| e.id);
        assert_eq!(mirror(&mut d, &mut rig, Route::Home, tick(1)), "home");
        assert_eq!(d.nav.top_page().map(|e| e.id), home, "a steady route mints nothing");
        assert_eq!(mirror(&mut d, &mut rig, Route::Detail, tick(2)), "detail");
        assert_eq!(d.nav.tabs.stack.depth(), 1, "a Replace keeps no page beneath");
        assert_ne!(d.nav.top_page().map(|e| e.id), home);
        assert_eq!(
            mirror(&mut d, &mut rig, Route::Player { overlay: super::super::nav::Overlay::Info }, tick(3)),
            "player"
        );
        assert_eq!(d.top_screen().map(|s| s.render()), Some(RenderStrategy::VideoPlane));
    }
}
