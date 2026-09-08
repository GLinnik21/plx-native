use super::*;
use crate::ui::dispatch::{CxParts, Dispatcher, NoTap, Rig, Split};
use crate::ui::fixture::{tick, FixtureMeasure};
use crate::ui::machine::{Chrome, Host, InputOwner, InstanceId, MachineId, NavOp, ScreenId, TimerId};
use crate::ui::present::Present;
use crate::ui::screen::{Mounter, ReturnState, ScreenArg};

#[derive(Clone, PartialEq, Eq)]
struct Arg(String);
impl LogicalState for Arg {
    fn write(&self, c: &mut Canon) { c.str(&self.0); }
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> Chrome { Chrome::None }
    fn id(&self) -> ScreenId { ScreenId(700) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, other: &Self) -> bool { self == other }
}
struct TestHost;
impl Host for TestHost {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    type Init = crate::screens::family::NoInit;
    type Memory = PageMemory;
}

// No constructor request: this fixture publishes data through the real store-notice and
// container lifecycle seams, without requiring a configured server or a graphics context.
fn body(entry: EntryId, rk: &str) -> DetailScreen {
    DetailScreen {
        entry, sid: ServerId::UNSET, rk: rk.into(), pending_season: None,
        keys: vec![], next_elem: FIRST_ITEM_ELEM, key_by_local: Default::default(),
        local_by_key: Default::default(), return_pending: false,
        season_settle: 0.0, restore_intent: None, scroll: Spring::at(0.0),
        scroll_target: 0.0, episode_scroll: Spring::at(0.0), tab_scroll: Spring::at(0.0),
        episode_scale: [Spring::at(1.0); EP_SCALE_MAX], related: CardRow::new(),
        cast: CardRow::new(), tabs: TabStrip::new(), season_pop: CtlPop::new(),
        ctl_pop: CtlPop::new(), disc_unfurl: [Spring::at(0.0); 2],
        season_metrics: season::Metrics::new(), about_rows: about::Rows::new(),
        ground: AmbientWash::flat(theme::SURFACE_APP), spin_ms: 0.0,
    }
}
struct Mount;
impl Mounter<TestHost> for Mount {
    fn mount(&mut self, _: InstanceId, arg: &Arg, ret: &ReturnState<u32, PageMemory>,
        cx: &Cx<'_, TestHost>, _: &mut Effects<'_, TestHost>) -> Box<dyn Screen<TestHost>> {
        let InputOwner::Entry(entry) = cx.owner else { panic!("page owner") };
        let mut page = body(entry, &arg.0);
        if let PageMemory::Detail(memory) = &ret.memory { page.restore_memory(memory); }
        Box::new(page)
    }
}
struct TestRig { mount: Mount, measure: FixtureMeasure, opened: Vec<ContentArg> }
impl Rig<TestHost> for TestRig {
    fn split(&mut self) -> Split<'_, TestHost> {
        Split { mounter: &mut self.mount, views: (), measure: &self.measure }
    }
    fn deliver(&mut self, _: MachineId, _: &AppMsg, _: &CxParts<u32>, _: &mut Effects<'_, TestHost>) -> Handled { Handled::No }
    fn timer(&mut self, _: MachineId, _: TimerId, _: &CxParts<u32>, _: &mut Effects<'_, TestHost>) {}
    fn app_fx(&mut self, _: MachineId, effect: AppFx, _: &CxParts<u32>, _: &mut Effects<'_, TestHost>) {
        if let AppFx::Content(ContentReq::Push(arg)) = effect { self.opened.push(arg); }
    }
    fn log(&mut self, _: &str) {}
    fn prepare(&mut self, _: &mut Budget, _: &mut Present) {}
    fn ls2_pump(&mut self) {}
    fn opaque_route(&mut self, _: bool) {}
    fn clear_opaque_region(&mut self) {}
    fn now_us(&self) -> u64 { 0 }
}
fn frame(d: &mut Dispatcher<TestHost>, rig: &mut TestRig, ms: u32) {
    let report = d.frame_with(rig, tick(ms), vec![], vec![], &mut NoTap, false);
    d.prune(&report.unmounted);
}
fn item(rk: &str, reverse: bool) -> Detail {
    let mut d = Detail { sid: ServerId::UNSET, rk: rk.into(), is_show: true,
        kind: "show".into(), ..Default::default() };
    for i in 1..=2 {
        d.seasons.push(crate::metadata::Season { rk: format!("s{i}"), index: i,
            title: format!("Season {i}"), leaf_count: 2, viewed_leaf_count: 0 });
        d.episodes.push(crate::metadata::Episode { rk: format!("e{i}"), index: i,
            season: 1, title: format!("Episode {i}"), ..Default::default() });
        d.related.push(crate::pms::PmsMovie { sid: ServerId::UNSET, rk: format!("r{i}"), ..Default::default() });
        d.cast.push(crate::metadata::Cast { id: i, tag: format!("Person {i}"),
            role: "Actor".into(), tag_key: format!("plex://person/{i}"), thumb: String::new() });
    }
    if reverse {
        d.seasons.reverse(); d.cur_season = 1;
        d.episodes.reverse(); d.related.reverse(); d.cast.reverse();
    }
    d
}
fn boot() -> (Dispatcher<TestHost>, TestRig) {
    apply_metadata(MetadataCmd::Clear);
    crate::metadata::set_current_for_test(Some(item("a", false)));
    let mut d = Dispatcher::new();
    d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
    let mut rig = TestRig { mount: Mount, measure: FixtureMeasure, opened: Vec::new() };
    d.request(MachineId::Nav, NavOp::Root(Arg("a".into())));
    frame(&mut d, &mut rig, 0);
    d.store_changed(StoreId::Metadata.ord(), 1);
    frame(&mut d, &mut rig, 16);
    (d, rig)
}
fn screen(d: &Dispatcher<TestHost>) -> &DetailScreen {
    d.nav.top_page().unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<DetailScreen>().unwrap()
}
fn first(d: &Dispatcher<TestHost>, group: GroupId) -> FocusKey<u32> {
    let s = screen(d);
    let measure = FixtureMeasure;
    let cx = Cx::<TestHost> { views: (), tick: tick(16), measure: &measure,
        press: Default::default(), focus: Default::default(), owner: InputOwner::Entry(s.entry) };
    let r = Rect::new(0.0, 0.0, 1.0, 1.0);
    Focusable::<TestHost>::seat(s, group, Placed { rect: r, rest_rect: r, clip: Rect::FULL, index: Some(0) }, &cx)
}
fn land(d: &mut Dispatcher<TestHost>, rig: &mut TestRig, data: Detail, ms: u32) {
    crate::metadata::set_current_for_test(Some(data));
    d.store_changed(StoreId::Metadata.ord(), ms);
    frame(d, rig, ms);
}

#[test]
fn repeated_detail_keys_follow_items_through_all_four_group_reorders() {
    let _guard = crate::testlock::serial();
    for group in [season::SEASON_GROUP, episodes::EPISODES_GROUP, related::RELATED_GROUP, cast::CAST_GROUP] {
        let (mut d, mut rig) = boot();
        let key = first(&d, group);
        assert_eq!(screen(&d).locate(key.elem).unwrap().index(), 0);
        d.set_focus_in(Some(key), Some(group));
        land(&mut d, &mut rig, item("a", true), 32);
        assert_eq!(d.focus(), Some(key), "reorder must preserve identity in {group:?}");
        assert_eq!(screen(&d).locate(key.elem).unwrap().index(), 1,
            "the same key must now project to the item's NEW slot in {group:?}");
    }
    crate::metadata::set_current_for_test(None);
}

#[test]
fn a_removed_detail_item_is_not_reinterpreted_as_its_slot_replacement() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    let mut changed = item("a", false);
    changed.related.remove(0);
    land(&mut d, &mut rig, changed, 32);
    assert_ne!(d.focus(), Some(key), "a removed item and its replacement cannot share a key");
    crate::metadata::set_current_for_test(None);
}

#[test]
fn retained_detail_back_keeps_the_engine_key_until_its_own_landing() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    crate::metadata::set_current_for_test(Some(item("b", false)));
    frame(&mut d, &mut rig, 32);
    assert_eq!(d.nav.top_page().unwrap().arg.0, "b");
    crate::metadata::set_current_for_test(None);
    let request = crate::metadata::begin_detail_for_test(ServerId::UNSET, "a");
    d.request(MachineId::Nav, NavOp::Pop);
    frame(&mut d, &mut rig, 48);
    assert_eq!(d.nav.top_page().unwrap().inst.as_ref().unwrap().id, instance);
    assert_eq!(d.focus(), Some(key), "unresolved return must not fall back to Hero");
    assert!(!crate::metadata::land_detail_for_test(ServerId::UNSET, "wrong", request, Some(item("wrong", false))));
    d.store_changed(StoreId::Metadata.ord(), 64);
    frame(&mut d, &mut rig, 64);
    assert_eq!(d.focus(), Some(key), "another item's notice cannot complete restoration");
    assert_eq!(crate::metadata::detail_request_status(ServerId::UNSET, "a"), Some(true));
    assert!(crate::metadata::land_detail_for_test(ServerId::UNSET, "a", request, Some(item("a", true))));
    d.store_changed(StoreId::Metadata.ord(), 80);
    frame(&mut d, &mut rig, 80);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem).unwrap().index(), 1);
    assert!(screen(&d).scroll_target > 0.0, "matching landing must reveal the restored row even when its key never changed");
    crate::metadata::set_current_for_test(None);
}

#[test]
fn an_evicted_detail_reuses_its_item_registry_after_a_reordered_landing() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    let old_instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    for i in 0..=crate::ui::containers::stack::CAP {
        let rk = format!("covered-{i}");
        d.request(MachineId::Nav, NavOp::Push(Arg(rk.clone())));
        crate::metadata::set_current_for_test(Some(item(&rk, false)));
        frame(&mut d, &mut rig, 32 + i as u32 * 16);
        assert_eq!(d.nav.tabs.stack.entries.len(), i + 2, "each push must actually commit");
    }
    assert!(d.nav.entry(key.entry).unwrap().inst.is_none());
    // Remount sees a reordered model before receiving Mount. Its old registry must be seeded
    // first, otherwise the same integer is minted for the replacement at slot zero.
    crate::metadata::set_current_for_test(Some(item("a", true)));
    d.request(MachineId::Nav, NavOp::PopTo(key.entry));
    frame(&mut d, &mut rig, 400);
    assert_ne!(d.nav.top_page().unwrap().inst.as_ref().unwrap().id, old_instance);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem).unwrap().index(), 1);
    assert!(screen(&d).scroll_target > 0.0, "a cold body must reveal its restored row on Enter");
    crate::metadata::set_current_for_test(None);
}

#[test]
fn retained_detail_back_hydrates_saved_season_before_episode_focus() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let mut second = item("a", false);
    second.cur_season = 1;
    for ep in &mut second.episodes { ep.season = 2; }
    land(&mut d, &mut rig, second, 32);
    let key = first(&d, episodes::EPISODES_GROUP);
    d.set_focus_in(Some(key), Some(episodes::EPISODES_GROUP));
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    crate::metadata::set_current_for_test(Some(item("b", false)));
    frame(&mut d, &mut rig, 48);
    assert_eq!(d.nav.top_page().unwrap().arg.0, "b");
    crate::metadata::set_current_for_test(None);
    let request = crate::metadata::begin_detail_for_test(ServerId::UNSET, "a");
    d.request(MachineId::Nav, NavOp::Pop);
    frame(&mut d, &mut rig, 64);
    assert_eq!(screen(&d).restore_intent.as_ref().map(|intent| intent.spot.season),
        Some(Some(2)), "live return must hydrate its request-time season, not merely its focus");
    assert_eq!(d.focus(), Some(key));
    let mut landed = item("a", true);
    landed.cur_season = 0; // reversed seasons: season 2 is now at index zero
    for ep in &mut landed.episodes { ep.season = 2; }
    assert!(crate::metadata::land_detail_for_test(ServerId::UNSET, "a", request, Some(landed)));
    d.store_changed(StoreId::Metadata.ord(), 80);
    frame(&mut d, &mut rig, 80);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem).unwrap().index(), 1);
    crate::metadata::set_current_for_test(None);
}

#[test]
fn reordered_detail_keys_activate_the_same_related_cast_and_episode_text_targets() {
    let _guard = crate::testlock::serial();
    for (located, expected) in [
        (Located::Related(0), ContentArg::Detail { sid: ServerId::UNSET, rk: "r1".into() }),
        (Located::Cast(0), ContentArg::Person { sid: ServerId::UNSET, key: "1".into(),
            guid: "plex://person/1".into(), name: "Person 1".into(), thumb: String::new() }),
        (Located::Episode(0, episodes::Row::Text), ContentArg::Detail { sid: ServerId::UNSET, rk: "e1".into() }),
    ] {
        let (mut d, mut rig) = boot();
        let key = screen(&d).key_of(located).unwrap();
        land(&mut d, &mut rig, item("a", true), 32);
        let id = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(id),
            crate::ui::machine::Delivery::Screen(ScreenEvent::Activate(key))));
        frame(&mut d, &mut rig, 48);
        assert_eq!(rig.opened.len(), 1);
        assert!(rig.opened[0] == expected, "activation follows identity, never the stale local slot");
    }
    apply_metadata(MetadataCmd::Clear);
}

#[test]
fn a_failed_addressed_return_retires_the_intent_and_falls_back() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    crate::metadata::set_current_for_test(Some(item("b", false)));
    frame(&mut d, &mut rig, 32);
    crate::metadata::set_current_for_test(None);
    let request = crate::metadata::begin_detail_for_test(ServerId::UNSET, "a");
    d.request(MachineId::Nav, NavOp::Pop);
    frame(&mut d, &mut rig, 48);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(crate::metadata::detail_request_status(ServerId::UNSET, "a"), Some(true));
    assert_eq!(crate::metadata::detail_request_status(ServerId::UNSET, "b"), None);
    assert!(!crate::metadata::land_detail_for_test(ServerId::UNSET, "a", request, None));
    assert_eq!(crate::metadata::detail_request_status(ServerId::UNSET, "a"), Some(false));
    d.store_changed(StoreId::Metadata.ord(), 64);
    frame(&mut d, &mut rig, 64);
    assert_eq!(d.focus().unwrap().elem, hero::ELEM_PLAY);
    assert!(!screen(&d).return_pending && screen(&d).restore_intent.is_none());
    apply_metadata(MetadataCmd::Clear);
}

#[test]
fn a_live_return_does_not_rewind_ids_minted_after_its_request_snapshot() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let saved = d.return_state().memory;
    let mut newer = item("a", false);
    newer.related.push(crate::pms::PmsMovie { sid: ServerId::UNSET, rk: "r3".into(), ..Default::default() });
    land(&mut d, &mut rig, newer, 32);
    let third_key = screen(&d).key_of(Located::Related(2)).unwrap();
    let counter = screen(&d).next_elem;
    let id = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(id),
        crate::ui::machine::Delivery::Screen(ScreenEvent::RestoreMemory(saved))));
    frame(&mut d, &mut rig, 48);
    assert_eq!(screen(&d).next_elem, counter);
    assert_eq!(screen(&d).key_of(Located::Related(2)), Some(third_key));
    apply_metadata(MetadataCmd::Clear);
}

#[test]
fn cold_entry_argument_and_return_memory_both_change_the_tree_hash() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    for i in 0..=crate::ui::containers::stack::CAP {
        let rk = format!("covered-{i}");
        d.request(MachineId::Nav, NavOp::Push(Arg(rk.clone())));
        crate::metadata::set_current_for_test(Some(item(&rk, false)));
        frame(&mut d, &mut rig, 32 + i as u32 * 16);
    }
    assert!(d.nav.entry(key.entry).unwrap().inst.is_none());
    let before = d.state_hash();
    d.nav.entry_mut(key.entry).unwrap().arg.0 = "different-cold-item".into();
    assert_ne!(d.state_hash(), before, "cold constructor arguments remain logical state");
    d.nav.entry_mut(key.entry).unwrap().arg.0 = "a".into();
    assert_eq!(d.state_hash(), before);
    let PageMemory::Detail(memory) = &mut d.nav.entry_mut(key.entry).unwrap().ret.memory else { panic!("detail memory") };
    let Some(DetailKey { identity: DetailIdentity::Related { rk, .. }, .. }) = memory.keys.iter_mut()
        .find(|key| matches!(key.identity, DetailIdentity::Related { .. })) else { panic!("related identity") };
    *rk = "changed-retained-key".into();
    assert_ne!(d.state_hash(), before, "cold registry CONTENTS are hashed, not only their count");
    apply_metadata(MetadataCmd::Clear);
}
