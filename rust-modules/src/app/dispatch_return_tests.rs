//! Housekeeping must not copy page memory, or consume a queued navigation bookmark.

//!
//! This was `ui/dispatch_return_tests.rs`, `include!`d into the dispatcher's own test module. It
//! drives `ui::dispatch::Dispatcher` with the application's real effect type (`AppFx`) and the
//! application's own predicate for which of those effects need a bookmark
//! (`AppHost::app_fx_needs_return`), over the store commands the app issues as housekeeping, so
//! it is a whole-app test: it belongs here, to the layer that owns the `AppHost`, `screens` and
//! `stores` it names, and not in the UI library (docs/module-layers.md, step L13).

#[test]
fn filmography_fifo_first_navigation_phase() {
    use plx_screens::registry::{AppArg, ContentArg, ContentReq, PageMemory};
    let _guard = plx_base::testlock::serial();
    let mut d = Dispatcher::<AppHost>::with_transition(Box::new(
        plx_ui::containers::transition::PageDip::new()));
    let mut rig = super::Bridge::for_test(|| 0);
    let detail = AppArg::Content(ContentArg::Detail {
        sid: plx_plex::plex::ServerId::UNSET, rk: "phase-oracle".into(),
    });
    super::nav_root(&mut d, detail.clone());
    for (ms, dt_us) in [(0, 0), (595, 70_000), (615, 20_000),
        (645, 30_000), (659, 14_000), (676, 17_000)] {
        super::frame(&mut d, &mut rig, Tick { ms, dt_us }, vec![]);
    }
    let sender = MachineId::Instance(d.nav.top_page().unwrap().inst.as_ref().unwrap().id);
    let person = ContentArg::Person { sid: plx_plex::plex::ServerId::UNSET,
        key: "19".into(), guid: "phase-person".into(), name: String::new(), thumb: String::new() };
    d.emit(sender, Fx::App(AppFx::Content(ContentReq::Push(person))));
    super::frame(&mut d, &mut rig, Tick { ms: 693, dt_us: 17_000 }, vec![]);
    assert!(d.top_arg() == Some(&detail));
    assert!(matches!(d.nav.top_page().unwrap().ret.memory, PageMemory::Detail(_)));
    assert!(d.nav.top_page().unwrap().ret.focus.is_some());
    assert!(d.nav.tabs.stack.is_pending());
    assert_eq!(d.nav.tabs.stack.transition.phase_word(), "out",
        "FIFO publishes return memory and requests Person before the f36 navigation commit");
    for (ms, dt_us) in [(712, 19_000), (732, 20_000)] {
        super::frame(&mut d, &mut rig, Tick { ms, dt_us }, vec![]);
    }
    assert!(matches!(d.top_arg(), Some(AppArg::Content(ContentArg::Person { .. }))),
        "Person mounts at f38 under FIFO, not the historical f40");
}

#[test]
fn watchlist_commands_use_the_product_fifo_runner() {
    let _guard = plx_base::testlock::serial();
    let rig = super::Bridge::for_test(|| 0);
    assert!(rig.external_app_fx(&AppFx::Watchlist(
        plx_screens::watchlist::page::WatchlistOut::Retry)),
        "Watchlist admission must yield at its FIFO position, not accumulate a family queue");
}

#[test]
fn search_commands_use_the_product_fifo_runner() {
    let _guard = plx_base::testlock::serial();
    let rig = super::Bridge::for_test(|| 0);
    assert!(rig.external_app_fx(&AppFx::Search(plx_screens::registry::SearchReq::Back)),
        "Search commands must yield at their FIFO position");
}

#[test]
fn resource_screen_commands_use_the_product_fifo_runner() {
    let _guard = plx_base::testlock::serial();
    let rig = super::Bridge::for_test(|| 0);
    for fx in [AppFx::Library(LibraryReq::Account),
        AppFx::Home(plx_screens::registry::HomeReq::Account),
        AppFx::Content(plx_screens::registry::ContentReq::Back)] {
        assert!(rig.external_app_fx(&fx), "resource-backed screen commands must yield before execution");
    }
}

#[test]
fn product_runner_preserves_mixed_family_order_and_mounts_the_surface_in_the_same_frame() {
    use plx_screens::registry::{AppArg, HomeTab, SearchReq};
    use plx_screens::watchlist::page::WatchlistOut;
    let _guard = plx_base::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = super::Bridge::for_test(|| 0);
    super::nav_root(&mut d, AppArg::Watchlist);
    super::frame(&mut d, &mut rig, Tick::default(), vec![]);
    let watchlist = MachineId::Instance(d.nav.top_page().unwrap().inst.as_ref().unwrap().id);
    super::nav_push(&mut d, AppArg::Search);
    for i in 1..32 { super::frame(&mut d, &mut rig, Tick { ms: i * 16, dt_us: 16_000 }, vec![]); }
    let entry = d.nav.top_page().unwrap().id;
    let search = MachineId::Instance(d.nav.top_page().unwrap().inst.as_ref().unwrap().id);
    rig.completed_screen_commands.clear();
    rig.screen_command_receipts.clear();
    d.emit(search, Fx::App(AppFx::Search(SearchReq::Tab(HomeTab::Search))));
    d.emit(watchlist, Fx::App(AppFx::Watchlist(WatchlistOut::SetOptions(Default::default()))));
    d.emit(search, Fx::App(AppFx::Search(SearchReq::Account)));
    super::frame(&mut d, &mut rig, Tick { ms: 512, dt_us: 16_000 }, vec![]);
    assert_eq!(rig.completed_screen_commands.len(), 3);
    assert_eq!(rig.completed_screen_commands[0], "search:Tab(Search)");
    assert!(rig.completed_screen_commands[1].starts_with("watchlist:SetOptions"));
    assert_eq!(rig.completed_screen_commands[2], "search:Account");
    assert_eq!(rig.screen_command_receipts.iter().map(|(_, from, outcome)| (*from, *outcome)).collect::<Vec<_>>(),
        [(search, AppCommandOutcome::Completed), (watchlist, AppCommandOutcome::Completed),
            (search, AppCommandOutcome::Completed)]);
    assert!(rig.screen_command_receipts.windows(2).all(|pair| pair[0].0.0 < pair[1].0.0));
    assert_eq!(d.nav.top_page().unwrap().id, entry, "the own-tab no-op preserves the page");
    assert!(d.nav.modals.top().is_some_and(|surface| surface.entry.inst.is_some()),
        "the runner finishes ahead of this frame's one navigation commit");
    assert_eq!(d.queued(), 0);
    assert!(d.pending_app_command().is_none());
}

#[test]
fn product_runner_refuses_covered_search_and_retires_an_unmounted_sender() {
    use plx_screens::registry::{AppArg, SearchReq};
    let _guard = plx_base::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = super::Bridge::for_test(|| 0);
    super::nav_root(&mut d, AppArg::Search);
    super::frame(&mut d, &mut rig, Tick::default(), vec![]);
    let search = MachineId::Instance(d.nav.top_page().unwrap().inst.as_ref().unwrap().id);
    super::nav_push(&mut d, AppArg::Watchlist);
    for i in 1..32 { super::frame(&mut d, &mut rig, Tick { ms: i * 16, dt_us: 16_000 }, vec![]); }
    d.emit(search, Fx::App(AppFx::Search(SearchReq::Back)));
    super::frame(&mut d, &mut rig, Tick { ms: 512, dt_us: 16_000 }, vec![]);
    assert_eq!(rig.screen_command_receipts.last().map(|(_, from, outcome)| (*from, *outcome)),
        Some((search, AppCommandOutcome::Refused)));
    assert!(d.top_arg() == Some(&AppArg::Watchlist));
    d.emit(search, Fx::App(AppFx::ItemMenu(plx_screens::registry::ItemMenuReq {
        act: plx_screens::item_menu::Action::GoToCatalog("not-a-catalog-guid".into()),
        sid: plx_plex::plex::ServerId::UNSET, item: None, season: None,
        loaded_episode: false, from_home: false,
    })));
    super::frame(&mut d, &mut rig, Tick { ms: 528, dt_us: 16_000 }, vec![]);
    assert_eq!(rig.screen_command_receipts.last().map(|(_, from, outcome)| (*from, *outcome)),
        Some((search, AppCommandOutcome::Refused)), "a covered page cannot execute an item action");
    super::nav_root(&mut d, AppArg::Home);
    for i in 33..65 { super::frame(&mut d, &mut rig, Tick { ms: i * 16, dt_us: 16_000 }, vec![]); }
    d.emit(search, Fx::App(AppFx::Search(SearchReq::Back)));
    super::frame(&mut d, &mut rig, Tick { ms: 1040, dt_us: 16_000 }, vec![]);
    assert_eq!(rig.screen_command_receipts.last().map(|(_, from, outcome)| (*from, *outcome)),
        Some((search, AppCommandOutcome::RetiredSender)));
    assert!(!rig.completed_screen_commands.iter().any(|label| label == "search:Back"));
    assert!(d.top_arg() == Some(&AppArg::Home));
    assert_eq!(d.queued(), 0);
}

use super::AppHost;
use plx_screens::registry::{AppFx, LibraryReq};
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use plx_data::stores::{StoreCmd, StoreId, StoreWork};
use plx_ui::dispatch::{CxParts, Dispatcher, FrameReport, NoTap, Rig, Split};
use plx_ui::fixture::{FixtureArg, FixtureMeasure};
use plx_ui::frame::Budget;
use plx_machine::machine::{
    AppCommandId, AppCommandOutcome, Canon, Cx, Effects, FocusKey, Fx, GroupId, Handled, Host, InstanceId, LogicalState, Machine,
    MachineId, NavOp, Stamped, Tick, TimerId,
};
use plx_machine::present::Present;
use plx_ui::screen::{
    At, Dir, DrawFrame, Focusable, GroupSpec, Mounter, Placed, ReturnState, Screen, ScreenEvent,
    Step,
};
use std::cell::Cell;
use std::rc::Rc;

struct ProbeHost;
impl Host for ProbeHost {
    type Arg = FixtureArg;
    type Fx = AppFx;
    type Msg = ();
    type Elem = u32;
    type Views<'a> = ();
    type Init = ();
    type Memory = Memory;
    fn app_fx_needs_return(fx: &AppFx) -> bool { AppHost::app_fx_needs_return(fx) }
}

#[derive(Clone, Default, Debug)]
struct Memory(u32);
impl LogicalState for Memory {
    fn write(&self, c: &mut Canon) { c.u32(self.0); }
    fn probe(&self, out: &mut String) { out.push_str(&self.0.to_string()); }
}

#[derive(Clone, Default)]
struct Probe {
    captures: Rc<Cell<u32>>,
    value: Rc<Cell<u32>>,
    reply: Rc<Cell<u32>>,
}
impl Machine<ProbeHost> for Probe {
    type Ev = ScreenEvent<ProbeHost>;
    fn step(&mut self, ev: &Self::Ev, _: &Cx<'_, ProbeHost>, _: &mut Effects<'_, ProbeHost>) -> Handled {
        if let ScreenEvent::Async(req, ()) = ev {
            self.reply.set(req.0);
            return Handled::Yes;
        }
        Handled::No
    }
}
impl Focusable<ProbeHost> for Probe {
    fn groups(&self, _: &Cx<'_, ProbeHost>, _: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _: &u32, _: &Cx<'_, ProbeHost>) -> Option<GroupId> { None }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, ProbeHost>) -> Step<u32> { Step::Edge }
    fn place(&self, _: &u32, _: &Cx<'_, ProbeHost>, _: At) -> Option<Placed> { None }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, ProbeHost>) -> FocusKey<u32> { want }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, ProbeHost>) -> FocusKey<u32> { unreachable!() }
}
impl Screen<ProbeHost> for Probe {
    fn name(&self) -> &'static str { "return-probe" }
    fn state(&self) -> &dyn LogicalState { &() }
    fn crumb(&self, _: &Cx<'_, ProbeHost>) -> Option<std::borrow::Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, ProbeHost>) {}
    fn draw(&mut self, _: &mut DrawFrame<'_, '_, ProbeHost>) {}
    fn render(&self) -> plx_ui::screen::RenderStrategy { plx_ui::screen::RenderStrategy::Page }
    fn memory_at(&self, _: Option<FocusKey<u32>>) -> Memory {
        self.captures.set(self.captures.get() + 1);
        Memory(self.value.get())
    }
}
impl Mounter<ProbeHost> for Probe {
    fn mount(&mut self, _: InstanceId, _: &FixtureArg, _: &ReturnState<u32, Memory>,
        _: &Cx<'_, ProbeHost>, _: &mut Effects<'_, ProbeHost>) -> Box<dyn Screen<ProbeHost>> {
        Box::new(self.clone())
    }
}
#[derive(Default)]
struct OwnedTap { calls: Vec<&'static str> }
impl plx_ui::dispatch::Tap<ProbeHost> for OwnedTap {
    fn tick(&mut self, _: u64, _: Tick) { self.calls.push("tick"); }
    fn effect(&mut self, _: u64, _: &plx_machine::machine::Stamped<ProbeHost>) { self.calls.push("effect"); }
    fn frame_done(&mut self, _: u64) { self.calls.push("done"); }
}

#[derive(Default)]
struct ProbeRig {
    probe: Probe,
    current_return: Memory,
    executed: Vec<(bool, u32)>,
    defer_library: bool,
    receipts: Vec<(AppCommandId, MachineId, AppCommandOutcome)>,
        owned_tap: Option<OwnedTap>,
}
impl Rig<ProbeHost> for ProbeRig {
    fn dispatch_tap(&mut self) -> Option<&mut dyn plx_ui::dispatch::Tap<ProbeHost>> {
            self.owned_tap.as_mut().map(|tap| tap as &mut dyn plx_ui::dispatch::Tap<ProbeHost>)
        }
        fn split(&mut self) -> Split<'_, ProbeHost> {
        Split { mounter: &mut self.probe, views: (), measure: &FixtureMeasure }
    }
    fn deliver(&mut self, _: MachineId, _: &(), _: &CxParts<u32>, _: &mut Effects<'_, ProbeHost>) -> Handled { Handled::No }
    fn timer(&mut self, _: MachineId, _: TimerId, _: &CxParts<u32>, _: &mut Effects<'_, ProbeHost>) {}
    fn app_return(&mut self, _: MachineId, ret: ReturnState<u32, Memory>) { self.current_return = ret.memory; }
    fn app_fx(&mut self, _: MachineId, fx: AppFx, _: &CxParts<u32>, _: &mut Effects<'_, ProbeHost>) {

        if let Some(tap) = &mut self.owned_tap { tap.calls.push("execute"); }
                self.executed.push((matches!(fx, AppFx::Library(LibraryReq::Account)), self.current_return.0));
    }
    fn external_app_fx(&self, fx: &AppFx) -> bool {
        self.defer_library && matches!(fx, AppFx::Library(LibraryReq::Account) | AppFx::Content(_) | AppFx::Home(_))
    }
    fn app_command_done(&mut self, id: AppCommandId, from: MachineId, outcome: AppCommandOutcome) {
        self.receipts.push((id, from, outcome));
    }
    fn log(&mut self, _: &str) {}
    fn prepare(&mut self, _: &mut Budget, _: &mut Present) {}
    fn ls2_pump(&mut self) {}
    fn opaque_route(&mut self, _: bool) {}
    fn clear_opaque_region(&mut self) {}
    fn now_us(&self) -> u64 { 0 }
}
fn booted() -> (Dispatcher<ProbeHost>, ProbeRig) {
    let mut d = Dispatcher::new();
    let mut rig = ProbeRig::default();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(1)));
    d.frame(&mut rig, Tick::default(), vec![], vec![], &mut NoTap);
    rig.probe.captures.set(0);
    (d, rig)
}
fn housekeeping() -> Vec<AppFx> {
    let target = SectionAddress { epoch: 0, sid: plx_plex::plex::ServerId::from_raw(0), section: 1 };
    vec![
        AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            target, work: LibraryWork::Want { lo: 0, hi: 24, focus: None },
        })),
        AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed { target, work: LibraryWork::Letters { today: None } })),
        AppFx::Library(LibraryReq::PublishShelves { target, hidden_page: false, at_head: false }),
        AppFx::StoreWork(StoreWork::Browse),
    ]
}
fn enqueue(d: &mut Dispatcher<ProbeHost>, fx: AppFx, absorb: bool) {
    if absorb { d.absorb(vec![Stamped { from: MachineId::Nav, fx: Fx::App(fx) }]); }
    else { d.emit(MachineId::Nav, Fx::App(fx)); }
}
fn drain(d: &mut Dispatcher<ProbeHost>, rig: &mut ProbeRig, max: u32) {
    d.drain(rig, &d.parts(Tick::default()), max, &mut FrameReport::default(), &mut NoTap);
}

#[test]
fn housekeeping_never_captures_page_memory_at_emission_or_execution() {
    let _guard = plx_base::testlock::serial();
    for absorb in [false, true] {
        let (mut d, mut rig) = booted();
        rig.probe.value.set(525);
        for fx in housekeeping() { enqueue(&mut d, fx, absorb); }
        drain(&mut d, &mut rig, 100);
        assert_eq!(rig.probe.captures.get(), 0, "absorb={absorb}: housekeeping copied the page bookmark");
        assert_eq!(rig.executed, vec![(false, 0); 4]);
    }
}

#[test]
fn interleaved_housekeeping_keeps_navigation_bookmarks_at_emission_time_across_carry() {
    let _guard = plx_base::testlock::serial();
    for absorb in [false, true] {
        let (mut d, mut rig) = booted();
        rig.probe.value.set(11);
        enqueue(&mut d, housekeeping().remove(0), absorb);
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), absorb);
        rig.probe.value.set(22);
        enqueue(&mut d, housekeeping().remove(2), absorb);
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), absorb);
        enqueue(&mut d, housekeeping().remove(3), absorb);
        rig.probe.value.set(99);
        for _ in 0..5 { drain(&mut d, &mut rig, 1); }
        assert_eq!(rig.executed, [(false, 0), (true, 11), (false, 0), (true, 22), (false, 0)],
            "absorb={absorb}: housekeeping stole a later bookmark or reused a prior one");
        assert_eq!(rig.probe.captures.get(), 2, "only the two navigation requests need bookmarks");
        assert!(d.app_returns.is_empty());
    }
}

#[test]
fn owned_observer_and_command_resources_are_borrowed_sequentially_once() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = booted();
    rig.defer_library = true;
    rig.owned_tap = Some(OwnedTap::default());
    enqueue(&mut d, AppFx::Library(LibraryReq::Account), false);
    enqueue(&mut d, housekeeping().remove(0), false);
    let mut report = FrameReport::default();
    let mut fallback = CountEffects(0);
    let parts = d.parts(Tick::default());
    assert_eq!(d.drain(&mut rig, &parts, 2, &mut report, &mut fallback), 1);
    let command = d.take_app_command().unwrap();
    let mut out = vec![];
    rig.app_return(command.from, command.return_state);
    rig.app_fx(command.from, command.effect.unwrap_or_else(|_| panic!("live command")), &parts,
        &mut Effects::new(&mut out, command.from, &mut d.present));
    assert_eq!(d.resume_app_command(&mut rig, command.id, AppCommandOutcome::Completed,
        out, &mut report, &mut fallback), Ok(1));
    assert_eq!(rig.owned_tap.as_ref().unwrap().calls, ["effect", "execute", "effect", "execute"]);
    assert_eq!(fallback.0, 0, "a single owner must receive every callback, never two observers");
    assert_eq!(rig.receipts.len(), 1);
}

#[test]
fn an_external_family_command_blocks_the_fifo_tail() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = booted();
    rig.defer_library = true;
    enqueue(&mut d, AppFx::Library(LibraryReq::Account), false);
    enqueue(&mut d, housekeeping().remove(0), false);
    drain(&mut d, &mut rig, 8);
    assert!(rig.executed.is_empty(), "a deferred family command was overtaken by its FIFO tail");
    assert_eq!(d.queued(), 2, "the head remains an obligation");
    assert!(d.pending_app_command().is_some());
}

#[test]
fn external_commands_keep_frozen_sender_bookmarks_budget_and_once_only_trace() {
    let _guard = plx_base::testlock::serial();
    for absorb in [false, true] {
        let (mut d, mut rig) = booted();
        rig.defer_library = true;
        rig.probe.value.set(11);
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), absorb);
        enqueue(&mut d, housekeeping().remove(0), absorb);
        rig.probe.value.set(22);
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), absorb);
        rig.probe.value.set(99);
        let mut report = FrameReport::default();
        let mut tap = CountEffects(0);
        let parts = d.parts(Tick::default());
        assert_eq!(d.drain(&mut rig, &parts, 2, &mut report, &mut tap), 1);
        let command = d.take_app_command().expect("yielded head");
        let first = command.id;
        assert_eq!(command.from, MachineId::Nav);
        assert_eq!(command.return_state.memory.0, 11);
        assert!(matches!(command.effect, Ok(AppFx::Library(LibraryReq::Account))));
        assert!(d.take_app_command().is_none());
        assert_eq!(d.drain(&mut rig, &parts, 100, &mut report, &mut tap), 0);
        assert_eq!(tap.0, 1, "a held head must not be traced again");
        assert!(d.resume_app_command(&mut rig, AppCommandId(first.0 + 1), AppCommandOutcome::Completed,
            vec![], &mut report, &mut tap).is_err());
        // The outside runner executes the owned command once; no borrowed page crosses this call.
        rig.app_return(command.from, command.return_state);
        let mut out = vec![];
        rig.app_fx(command.from, command.effect.ok().unwrap(), &parts,
            &mut Effects::new(&mut out, command.from, &mut d.present));
        assert_eq!(d.resume_app_command(&mut rig, first, AppCommandOutcome::Completed,
            out, &mut report, &mut tap), Ok(1));
        assert_eq!(rig.executed, [(true, 11), (false, 0)]);
        assert_eq!(d.queued(), 1, "resume must not grant another two steps");
        assert!(d.resume_app_command(&mut rig, first, AppCommandOutcome::Completed,
            vec![], &mut report, &mut tap).is_err());
        assert_eq!(d.drain(&mut rig, &parts, 1, &mut report, &mut tap), 1);
        let second = d.take_app_command().unwrap();
        assert!(second.id.0 > first.0);
        assert_eq!(second.return_state.memory.0, 22);
        assert_eq!(d.resume_app_command(&mut rig, second.id, AppCommandOutcome::Refused,
            vec![], &mut report, &mut tap), Ok(0));
        assert_eq!(tap.0, 3);
        assert_eq!(rig.receipts, [(first, MachineId::Nav, AppCommandOutcome::Completed),
            (second.id, MachineId::Nav, AppCommandOutcome::Refused)]);
        assert!(d.app_returns.is_empty());
        assert_eq!(rig.probe.captures.get(), 2);
    }
}

#[test]
fn mixed_app_families_are_yielded_in_emission_order_not_family_order() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = booted();
    rig.defer_library = true;
    enqueue(&mut d, AppFx::Content(plx_screens::registry::ContentReq::Back), false);
    enqueue(&mut d, AppFx::Home(plx_screens::registry::HomeReq::FoldToHero), false);
    enqueue(&mut d, AppFx::Library(LibraryReq::Account), false);
    drain(&mut d, &mut rig, 3);
    let mut seen = vec![];
    while let Some(command) = d.take_app_command() {
        seen.push(match command.effect {
            Ok(AppFx::Content(_)) => "content",
            Ok(AppFx::Home(_)) => "home",
            Ok(AppFx::Library(_)) => "library",
            _ => panic!("unexpected command"),
        });
        d.resume_app_command(&mut rig, command.id, AppCommandOutcome::Completed, vec![],
            &mut FrameReport::default(), &mut NoTap).unwrap();
    }
    assert_eq!(seen, ["content", "home", "library"]);
    assert_eq!(rig.receipts.len(), 3);
    assert_eq!(d.queued(), 0);
}

#[test]
fn an_external_failure_returns_an_addressed_event_through_the_same_fifo() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = booted();
    rig.defer_library = true;
    let source = MachineId::Instance(d.nav.top_page().unwrap().inst.as_ref().unwrap().id);
    d.emit(source, Fx::App(AppFx::Library(LibraryReq::Account)));
    enqueue(&mut d, housekeeping().remove(0), false);
    drain(&mut d, &mut rig, 3);
    let command = d.take_app_command().unwrap();
    assert_eq!(command.from, source);
    let result = Stamped { from: MachineId::Nav,
        fx: Fx::Deliver(source, plx_machine::machine::Delivery::Screen(
            ScreenEvent::Async(plx_machine::machine::RequestId(42), ()))) };
    assert_eq!(d.resume_app_command(&mut rig, command.id, AppCommandOutcome::Failed, vec![result],
        &mut FrameReport::default(), &mut NoTap), Ok(2));
    assert_eq!(rig.probe.reply.get(), 42);
    assert_eq!(rig.executed, [(false, 0)]);
    assert_eq!(rig.receipts, [(command.id, source, AppCommandOutcome::Failed)]);
    assert_eq!(d.queued(), 0);
}

#[test]
fn a_covered_live_sender_is_not_retired_and_lifecycle_after_take_is_revalidated() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = booted();
    rig.defer_library = true;
    let covered = d.top_page().unwrap();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(9)));
    d.frame(&mut rig, Tick::default(), vec![], vec![], &mut NoTap);
    assert_ne!(d.top_page(), Some(covered));
    d.emit(MachineId::Instance(covered), Fx::App(AppFx::Library(LibraryReq::Account)));
    enqueue(&mut d, housekeeping().remove(0), false);
    drain(&mut d, &mut rig, 2);
    let command = d.take_app_command().unwrap();
    assert!(command.effect.is_ok(), "covered live pages retain their lifetime");
    assert!(d.app_command_sender_is_live(command.id));
    // A lifecycle operation explicitly invoked by the outside runner may retire even a taken
    // sender. Revalidate before starting IO, and answer retirement rather than executing it.
    d.reset_for_profile();
    assert!(!d.app_command_sender_is_live(command.id));
    assert_eq!(d.resume_app_command(&mut rig, command.id, AppCommandOutcome::RetiredSender, vec![],
        &mut FrameReport::default(), &mut NoTap), Ok(1));
    assert_eq!(rig.executed, [(false, 0)]);
    assert_eq!(rig.receipts, [(command.id, MachineId::Instance(covered), AppCommandOutcome::RetiredSender)]);
}

struct CountEffects(u32);
impl plx_ui::dispatch::Tap<ProbeHost> for CountEffects {
    fn effect(&mut self, _: u64, _: &Stamped<ProbeHost>) { self.0 += 1; }
}

#[test]
fn every_external_terminal_outcome_unblocks_without_reexecuting_the_head() {
    let _guard = plx_base::testlock::serial();
    for outcome in [AppCommandOutcome::Completed, AppCommandOutcome::Failed,
        AppCommandOutcome::Refused, AppCommandOutcome::Cancelled, AppCommandOutcome::RetiredSender] {
        let (mut d, mut rig) = booted();
        rig.defer_library = true;
        let source = if outcome == AppCommandOutcome::RetiredSender {
            let retired = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
            d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(9)));
            let report = d.frame(&mut rig, Tick::default(), vec![], vec![], &mut NoTap);
            assert!(report.unmounted.contains(&retired), "the original owner received Unmount");
            assert!(!d.nav.bodies().any(|entry| entry.inst.as_ref().is_some_and(|inst| inst.id == retired)),
                "the original owner is retired, not merely covered");
            // Before frame-tail prune, lifecycle reachability must not authorize new work.
            assert!(d.nav.entry_of_instance(retired).is_some());
            MachineId::Instance(retired)
        } else { MachineId::Nav };
        d.emit(source, Fx::App(AppFx::Library(LibraryReq::Account)));
        enqueue(&mut d, housekeeping().remove(0), false);
        drain(&mut d, &mut rig, 2);
        let id = d.pending_app_command().unwrap();
        let mut report = FrameReport::default();
        assert!(d.resume_app_command(&mut rig, id, AppCommandOutcome::Completed,
            vec![], &mut report, &mut NoTap).is_err(), "untaken success is invalid");
        let command = d.take_app_command().unwrap();
        if outcome == AppCommandOutcome::RetiredSender {
            assert!(matches!(command.effect, Err(AppCommandOutcome::RetiredSender)));
            assert!(d.resume_app_command(&mut rig, id, AppCommandOutcome::Completed,
                vec![], &mut report, &mut NoTap).is_err());
        } else { assert!(command.effect.is_ok()); }
        assert_eq!(d.resume_app_command(&mut rig, id, outcome, vec![], &mut report, &mut NoTap), Ok(1));
        assert_eq!(rig.executed, [(false, 0)]);
        assert_eq!(rig.receipts, [(id, source, outcome)]);
        assert_eq!(d.queued(), 0);
    }
}

#[test]
fn untaken_external_commands_can_be_cancelled_or_refused_and_remain_charged() {
    let _guard = plx_base::testlock::serial();
    for outcome in [AppCommandOutcome::Cancelled, AppCommandOutcome::Refused] {
        let (mut d, mut rig) = booted();
        rig.defer_library = true;
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), false);
        enqueue(&mut d, housekeeping().remove(0), false);
        drain(&mut d, &mut rig, 1);
        let id = d.pending_app_command().unwrap();
        assert_eq!(d.resume_app_command(&mut rig, id, outcome, vec![],
            &mut FrameReport::default(), &mut NoTap), Ok(0));
        assert!(rig.executed.is_empty());
        assert!(d.take_app_command().is_none());
        assert_eq!(d.queued(), 1);
        drain(&mut d, &mut rig, 1);
        assert_eq!(rig.executed, [(false, 0)]);
    }
}
