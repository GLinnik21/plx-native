use super::*;
use plx_ui::dispatch::{CxParts, Dispatcher, NoTap, Rig, Split};
use plx_ui::fixture::{tick, FixtureMeasure};
use plx_machine::machine::{Chrome, Host, InputOwner, InstanceId, MachineId, NavOp, ScreenId, TimerId};
use plx_machine::present::Present;
use plx_ui::screen::{Mounter, ReturnState, ScreenArg};

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
    // `super::super::` (detail -> screens -> family) rather than the absolute spelling: `family`
    // is the Settings family's shared vocabulary, not a sibling screen — see `screens::family`'s
    // own module doc, and `screens::legal`/`screens::settings`'s identical `super::family::` use.
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}

// TEST ONLY: same thread-confined store as `screens::detail::tests`'s `TEST_METADATA` —
// `MetadataStore` gained real owned fields in Stage B, so the old unit-struct `static` no
// longer compiles, and every helper below needs a real, per-owner store rather than a second
// mechanism.
thread_local! {
    static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
        std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
}

fn test_store() -> &'static mut plx_data::stores::metadata::MetadataStore {
    TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
}

impl crate::registry::MetadataLike for TestHost {
    fn metadata<'a>(_cx: &Cx<'a, Self>) -> plx_data::metadata::MetadataView<'a> {
        test_store().view()
    }
}

// No constructor request: this fixture publishes data through the real store-notice and
// container lifecycle seams, without requiring a configured server or a graphics context.
fn body(entry: EntryId, rk: &str) -> DetailScreen {
    DetailScreen {
        entry, sid: ServerId::UNSET, rk: rk.into(), pending_season: None,
        keys: vec![], next_elem: FIRST_ITEM_ELEM, key_by_local: Default::default(),
        local_by_key: Default::default(), prev_by_key: Default::default(), gone: Default::default(), return_pending: false,
        season_settle: 0.0,
        preview_dwell: 0.0,
        preview_promoted: false,
        preview_art: 1.0,
        preview_prose: 1.0,
        preview_synopsis: 1.0,
        preview_chrome: 1.0,
        preview_field: 1.0,
        preview_base_scrim: 1.0,
        preview_logo: Spring::at(0.0),
        preview_played_for: None,
        preview_started_for: None,
        preview_had_picture: false,
        trailer_ctl: super::trailer::Transport::IDLE,
        restore_intent: None, teardown_cleared: false, withdrawn: false, scroll: Spring::at(0.0),
        refresh: DetailRefreshPhase::None,
        refresh_gen: 0,
        scroll_target: 0.0, episode_scroll: Spring::at(0.0), tab_scroll: Spring::at(0.0),
        episode_cells: episodes::Cells::new(),
            ep_want: Default::default(),
        about_card_lift: plx_ui::text_lift::TextLift::new(),
        about_lang_lift: plx_ui::text_lift::TextLift::new(),
        related: plx_ui::cards::Shelf::new(entry, &plx_ui::cards::RowStyle::HOME), collection: plx_ui::cards::Shelf::new(entry, &plx_ui::cards::RowStyle::HOME),
        extras: plx_ui::cards::Shelf::new(entry, &plx_ui::cards::RowStyle::EPISODE),
        cast: plx_ui::cards::Shelf::new(entry, &plx_ui::cards::RowStyle::CAST), tabs: TabStrip::new(), season_pop: CtlPop::new(),
        ctl_pop: CtlPop::new(), disc_unfurl: [Spring::at(0.0); 3],
        season_metrics: season::Metrics::new(), about_rows: about::Rows::new(),
        ground: AmbientWash::flat(theme::SURFACE_APP), selected: None, spin_ms: 0.0,
        spin_phase: plx_machine::motion::Phase::default(),
        layout: std::cell::Cell::new(None),
        layout_pinned: std::cell::Cell::new(false),
        spot_facts: SpotFacts::default(),
        hold_hint: plx_ui::hold_hint::HoldHint::once_per_run(plx_ui::hold_hint::Kind::Detail),
    }
}
struct Mount;
impl Mounter<TestHost> for Mount {
    fn mount(&mut self, _: InstanceId, arg: &Arg, ret: &ReturnState<u32, PageMemory>,
        cx: &Cx<'_, TestHost>, _: &mut Effects<'_, TestHost>) -> Box<dyn Screen<TestHost>> {
        let InputOwner::Entry(entry) = cx.owner else { panic!("page owner") };
        let mut page = body(entry, &arg.0);
        if let PageMemory::Detail(memory) = &ret.memory { page.restore_memory(memory, test_store().view()); }
        Box::new(page)
    }
}
struct TestRig { mount: Mount, measure: FixtureMeasure, opened: Vec<ContentArg>, asked: Vec<MetadataCmd>, backs: u32 }
impl Rig<TestHost> for TestRig {
    fn split(&mut self) -> Split<'_, TestHost> {
        Split { mounter: &mut self.mount, views: (), measure: &self.measure }
    }
    fn deliver(&mut self, _: MachineId, _: &AppMsg, _: &CxParts<u32>, _: &mut Effects<'_, TestHost>) -> Handled { Handled::No }
    fn timer(&mut self, _: MachineId, _: TimerId, _: &CxParts<u32>, _: &mut Effects<'_, TestHost>) {}
    fn app_fx(&mut self, _: MachineId, effect: AppFx, _: &CxParts<u32>, _: &mut Effects<'_, TestHost>) {
        match effect {
            AppFx::Content(ContentReq::Push(arg)) => self.opened.push(arg),
            AppFx::Content(ContentReq::Back) => self.backs += 1,
            AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(cmd)) => self.asked.push(cmd),
            _ => {}
        }
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
    let mut episodes = Vec::new();
    for i in 1..=2 {
        d.seasons.push(plx_data::metadata::Season { rk: format!("s{i}"), index: i,
            title: format!("Season {i}"), leaf_count: 2, viewed_leaf_count: 0 });
        episodes.push(plx_data::metadata::Episode { rk: format!("e{i}"), index: i,
            season: 1, title: format!("Episode {i}"), ..Default::default() });
        d.related.push(plx_data::pms::PmsMovie { sid: ServerId::UNSET, rk: format!("r{i}"), ..Default::default() });
        d.cast.push(plx_data::metadata::Cast { id: i, tag: format!("Person {i}"),
            role: "Actor".into(), tag_key: format!("plex://person/{i}"), thumb: String::new() });
    }
    if reverse {
        d.seasons.reverse(); d.cur_season = 1;
        episodes.reverse(); d.related.reverse(); d.cast.reverse();
    }
    d.episodes = episodes.into();
    d
}
fn boot() -> (Dispatcher<TestHost>, TestRig) {
    boot_with(item("a", false))
}
fn boot_with(detail: Detail) -> (Dispatcher<TestHost>, TestRig) {
    boot_over(detail, false)
}
/// [`boot_with`], the page pushed over a root page `base` when `under` is set (so Back can pop it).
fn boot_over(detail: Detail, under: bool) -> (Dispatcher<TestHost>, TestRig) {
    test_store().run(MetadataCmd::Clear);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail));
    let mut d = Dispatcher::new();
    d.nav.tabs.stack.transition = Box::new(plx_ui::containers::transition::Immediate);
    let mut rig = TestRig { mount: Mount, measure: FixtureMeasure, opened: Vec::new(), asked: Vec::new(), backs: 0 };
    if under {
        d.request(MachineId::Nav, NavOp::Root(Arg("base".into())));
        frame(&mut d, &mut rig, 0);
        d.request(MachineId::Nav, NavOp::Push(Arg("a".into())));
    } else {
        d.request(MachineId::Nav, NavOp::Root(Arg("a".into())));
    }
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
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(data));
    d.store_changed(StoreId::Metadata.ord(), ms);
    frame(d, rig, ms);
}

#[test]
fn repeated_detail_keys_follow_items_through_all_four_group_reorders() {
    let _guard = plx_base::testlock::serial();
    for group in [season::SEASON_GROUP, episodes::EPISODES_GROUP, related::RELATED_GROUP, cast::CAST_GROUP] {
        let (mut d, mut rig) = boot();
        let key = first(&d, group);
        assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 0);
        d.set_focus_in(Some(key), Some(group));
        land(&mut d, &mut rig, item("a", true), 32);
        assert_eq!(d.focus(), Some(key), "reorder must preserve identity in {group:?}");
        assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 1,
            "the same key must now project to the item's NEW slot in {group:?}");
    }
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

#[test]
fn a_removed_detail_item_is_not_reinterpreted_as_its_slot_replacement() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    let mut changed = item("a", false);
    changed.related.remove(0);
    land(&mut d, &mut rig, changed, 32);
    assert_ne!(d.focus(), Some(key), "a removed item and its replacement cannot share a key");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

#[test]
fn retained_detail_back_keeps_the_engine_key_until_its_own_landing() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    let instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item("b", false)));
    frame(&mut d, &mut rig, 32);
    assert_eq!(d.nav.top_page().unwrap().arg.0, "b");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    let request = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "a");
    d.request(MachineId::Nav, NavOp::Pop);
    frame(&mut d, &mut rig, 48);
    assert_eq!(d.nav.top_page().unwrap().inst.as_ref().unwrap().id, instance);
    assert_eq!(d.focus(), Some(key), "unresolved return must not fall back to Hero");
    assert!(!{ let (__s, __a) = test_store().split_for_test(); plx_data::metadata::land_detail_for_test(__s, __a, ServerId::UNSET, "wrong", request, Some(item("wrong", false))) });
    d.store_changed(StoreId::Metadata.ord(), 64);
    frame(&mut d, &mut rig, 64);
    assert_eq!(d.focus(), Some(key), "another item's notice cannot complete restoration");
    assert_eq!(test_store().view().detail_request_status(ServerId::UNSET, "a"), Some(true));
    // The wrong-key completion was discarded; retry under a fresh admitted address.
    let request = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "a");
    assert!({ let (__s, __a) = test_store().split_for_test(); plx_data::metadata::land_detail_for_test(__s, __a, ServerId::UNSET, "a", request, Some(item("a", true))) });
    d.store_changed(StoreId::Metadata.ord(), 80);
    frame(&mut d, &mut rig, 80);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 1);
    assert!(screen(&d).scroll_target > 0.0, "matching landing must reveal the restored row even when its key never changed");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

#[test]
fn an_evicted_detail_reuses_its_item_registry_after_a_reordered_landing() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    let old_instance = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    for i in 0..=plx_ui::containers::stack::CAP {
        let rk = format!("covered-{i}");
        d.request(MachineId::Nav, NavOp::Push(Arg(rk.clone())));
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item(&rk, false)));
        frame(&mut d, &mut rig, 32 + i as u32 * 16);
        assert_eq!(d.nav.tabs.stack.entries.len(), i + 2, "each push must actually commit");
    }
    assert!(d.nav.entry(key.entry).unwrap().inst.is_none());
    // Remount sees a reordered model before receiving Mount. Its old registry must be seeded
    // first, otherwise the same integer is minted for the replacement at slot zero.
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item("a", true)));
    d.request(MachineId::Nav, NavOp::PopTo(key.entry));
    frame(&mut d, &mut rig, 400);
    assert_ne!(d.nav.top_page().unwrap().inst.as_ref().unwrap().id, old_instance);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 1);
    assert!(screen(&d).scroll_target > 0.0, "a cold body must reveal its restored row on Enter");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

#[test]
fn retained_detail_back_hydrates_saved_season_before_episode_focus() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let mut second = item("a", false);
    second.cur_season = 1;
    second.episodes = second.episodes.iter_loaded().map(|(_, ep)| plx_data::metadata::Episode { season: 2, ..ep.clone() }).collect();
    land(&mut d, &mut rig, second, 32);
    let key = first(&d, episodes::EPISODES_GROUP);
    d.set_focus_in(Some(key), Some(episodes::EPISODES_GROUP));
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item("b", false)));
    frame(&mut d, &mut rig, 48);
    assert_eq!(d.nav.top_page().unwrap().arg.0, "b");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    let request = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "a");
    d.request(MachineId::Nav, NavOp::Pop);
    frame(&mut d, &mut rig, 64);
    assert_eq!(screen(&d).restore_intent.as_ref().map(|intent| intent.spot.season),
        Some(Some(2)), "live return must hydrate its request-time season, not merely its focus");
    assert_eq!(d.focus(), Some(key));
    let mut landed = item("a", true);
    landed.cur_season = 0; // reversed seasons: season 2 is now at index zero
    landed.episodes = landed.episodes.iter_loaded().map(|(_, ep)| plx_data::metadata::Episode { season: 2, ..ep.clone() }).collect();
    assert!({ let (__s, __a) = test_store().split_for_test(); plx_data::metadata::land_detail_for_test(__s, __a, ServerId::UNSET, "a", request, Some(landed)) });
    d.store_changed(StoreId::Metadata.ord(), 80);
    frame(&mut d, &mut rig, 80);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 1);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

#[test]
fn reordered_detail_keys_activate_the_same_related_cast_and_episode_text_targets() {
    let _guard = plx_base::testlock::serial();
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
            plx_machine::machine::Delivery::Screen(ScreenEvent::Activate(key))));
        frame(&mut d, &mut rig, 48);
        assert_eq!(rig.opened.len(), 1);
        assert!(rig.opened[0] == expected, "activation follows identity, never the stale local slot");
    }
    test_store().run(MetadataCmd::Clear);
}

#[test]
fn a_failed_addressed_return_retires_the_intent_and_falls_back() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item("b", false)));
    frame(&mut d, &mut rig, 32);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    let request = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "a");
    d.request(MachineId::Nav, NavOp::Pop);
    frame(&mut d, &mut rig, 48);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(test_store().view().detail_request_status(ServerId::UNSET, "a"), Some(true));
    assert_eq!(test_store().view().detail_request_status(ServerId::UNSET, "b"), None);
    assert!(!{ let (__s, __a) = test_store().split_for_test(); plx_data::metadata::land_detail_for_test(__s, __a, ServerId::UNSET, "a", request, None) });
    assert_eq!(test_store().view().detail_request_status(ServerId::UNSET, "a"), Some(false));
    d.store_changed(StoreId::Metadata.ord(), 64);
    frame(&mut d, &mut rig, 64);
    assert_eq!(d.focus().unwrap().elem, hero::ELEM_PLAY);
    assert!(!screen(&d).return_pending && screen(&d).restore_intent.is_none());
    test_store().run(MetadataCmd::Clear);
}

#[test]
fn a_live_return_does_not_rewind_ids_minted_after_its_request_snapshot() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let saved = d.return_state().memory;
    let mut newer = item("a", false);
    newer.related.push(plx_data::pms::PmsMovie { sid: ServerId::UNSET, rk: "r3".into(), ..Default::default() });
    land(&mut d, &mut rig, newer, 32);
    let third_key = screen(&d).key_of(Located::Related(2)).unwrap();
    let counter = screen(&d).next_elem;
    let id = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(id),
        plx_machine::machine::Delivery::Screen(ScreenEvent::RestoreMemory(saved))));
    frame(&mut d, &mut rig, 48);
    assert_eq!(screen(&d).next_elem, counter);
    assert_eq!(screen(&d).key_of(Located::Related(2)), Some(third_key));
    test_store().run(MetadataCmd::Clear);
}

#[test]
fn cold_entry_argument_and_return_memory_both_change_the_tree_hash() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot();
    let key = first(&d, related::RELATED_GROUP);
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    for i in 0..=plx_ui::containers::stack::CAP {
        let rk = format!("covered-{i}");
        d.request(MachineId::Nav, NavOp::Push(Arg(rk.clone())));
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item(&rk, false)));
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
    test_store().run(MetadataCmd::Clear);
}

/// **The pseudo-locale sweep.** Draw the whole detail page with every catalog accessor on this
/// thread answering in the expanded pseudo-locale, through the text-recording painter that sees
/// every run the page hands to the text renderer. A run is accounted for when it came through the
/// catalog (it carries the `[!! … !!]` marker, or on a wrapped line the pseudo-locale's accented
/// vowels), is made of the fixture's own server values, or has
/// no letters at all (numbers, separators, glyph marks). Anything else is English the app drew
/// without the catalog — what `ci/check-localization.py` hunts for in source, caught here on the
/// drawn page itself.
fn stray_runs(detail: Detail, server_values: &[&str]) -> Vec<String> {
    use plx_ui::screen::DrawFrame;
    let _pseudo = plx_platform::i18n::pseudo_on_this_thread_for_test();
    let (mut d, _rig) = boot_with(detail);
    let runs = plx_gfx::text::capture_text_runs_for_test(|| {
        let entry = d.nav.tabs.stack.top_mut().expect("detail page");
        let owner = InputOwner::Entry(entry.id);
        let inst = entry.inst.as_mut().expect("mounted detail");
        let measure = FixtureMeasure;
        let cx = Cx::<TestHost> { views: (), tick: tick(32), measure: &measure,
            press: Default::default(), focus: Default::default(), owner };
        let mut f = DrawFrame::new(&cx, plx_ui::Painter::recording());
        plx_gfx::gfx::without_frame_clear(|| inst.screen.draw(&mut f));
    });
    assert!(runs.iter().any(|run| run.contains("[!!")), "the page drew catalog text: {runs:?}");
    // A wrapped catalog paragraph draws its later lines without the brackets, but still in the
    // pseudo-locale's accented vowels, which no English run and no fixture value here contains.
    let pseudo = |run: &str| run.contains("[!!") || run.contains(['á', 'ë', 'ï', 'ö', 'ü']);
    runs.into_iter()
        .filter(|run| !pseudo(run))
        .filter(|run| {
            // Strip every server value, then anything left that is a word is the app's own.
            let mut rest = run.replace('\u{a0}', " ");
            for value in server_values {
                rest = rest.replace(value, "");
            }
            rest.chars().any(char::is_alphabetic)
        })
        .collect()
}

#[test]
fn every_app_owned_run_on_a_show_page_comes_from_the_catalog() {
    let _guard = plx_base::testlock::serial();
    let stray = stray_runs(item("a", false),
        &["Season", "Episode", "Person", "Actor"]);
    assert!(stray.is_empty(), "text drawn without the catalog: {stray:?}");
}

#[test]
fn every_app_owned_run_on_a_film_page_comes_from_the_catalog() {
    let _guard = plx_base::testlock::serial();
    let stream = |codec: &str| plx_data::metadata::Stream {
        lang: "Deutsch".into(), lang_code: "deu".into(), codec: codec.into(), channels: 6,
        ..Default::default()
    };
    let film = Detail {
        sid: ServerId::UNSET, rk: "a".into(), kind: "movie".into(), title: "Zzyzx".into(),
        year: 1999, summary: "Qwerty".into(), rating: "R".into(),
        genres: vec!["Drama".into()], directors: vec!["Person 9".into()],
        audio: vec![plx_data::metadata::Stream { ad: true, ..stream("eac3") }],
        subs: vec![plx_data::metadata::Stream { sdh: true, ..stream("srt") }],
        ..Default::default()
    };
    let stray = stray_runs(film, &["Zzyzx", "Qwerty", "Vlox", "Drama", "Person", "Deutsch",
        "EAC3", "SRT", "R", "Dolby Digital Plus", "Dolby"]);
    assert!(stray.is_empty(), "text drawn without the catalog: {stray:?}");
}

/// A show with `n` seasons and otherwise the fixture page.
fn show(n: i64) -> Detail {
    let mut d = item("a", false);
    d.seasons = (1..=n).map(|i| plx_data::metadata::Season { rk: format!("s{i}"), index: i,
        title: format!("Season {i}"), leaf_count: 2, viewed_leaf_count: 0 }).collect();
    d
}

/// The whole page's per-frame draw census — every primitive the page hands the painter in one
/// frame, by `(command tag, whether it lands off-screen)` — through the recording painter, which
/// walks exactly the tree a real frame walks with no GL behind it.
fn census(detail: Detail) -> std::collections::BTreeMap<(u64, bool), usize> {
    use plx_ui::screen::DrawFrame;
    let (mut d, _rig) = boot_with(detail);
    let log = plx_ui::draw_census::capture(|| {
        let entry = d.nav.tabs.stack.top_mut().expect("detail page");
        let owner = InputOwner::Entry(entry.id);
        let inst = entry.inst.as_mut().expect("mounted detail");
        let measure = FixtureMeasure;
        let cx = Cx::<TestHost> { views: (), tick: tick(32), measure: &measure,
            press: Default::default(), focus: Default::default(), owner };
        let mut f = DrawFrame::new(&cx, plx_ui::Painter::recording());
        plx_gfx::gfx::without_frame_clear(|| inst.screen.draw(&mut f));
    });
    let mut out = std::collections::BTreeMap::new();
    for (tag, r) in log {
        let off = r.x >= plx_ui::consts::SCR_W || r.x + r.w.max(1.0) <= 0.0
            || r.y >= plx_ui::consts::SCR_H || r.y + r.h.max(1.0) <= 0.0;
        *out.entry((tag, off)).or_insert(0) += 1;
    }
    out
}

/// **Issue 18: a show page's frame costs its VISIBLE season pills, never its season count.** Once
/// the season row overflows the screen every further season is off the row, and the page must
/// draw exactly the same primitives — text runs, plates, capsules — whether the show has 16
/// seasons or the 64 the page addresses. Measured 2026-09-28 on this fixture page: 5 seasons = 32
/// primitives per frame, 40 = 44 (the row filling up), the same page as a film = 16.
#[test]
fn a_show_pages_draw_does_not_grow_with_seasons_off_the_row() {
    let _guard = plx_base::testlock::serial();
    let full = census(show(16));
    assert_eq!(census(show(64)), full, "draw census grew with off-screen seasons");
    assert!(full.get(&(100, false)).copied().unwrap_or(0) > 0, "the page drew text: {full:?}");
}

/// A page whose Related row is a 3-card head and a 24-card window of its tail starting at tail
/// position `from`; card `t{n}` is tail position `n`.
fn with_tail(from: usize) -> Detail {
    let mut d = item("a", false);
    d.related = (0..3).map(|i| plx_data::pms::PmsMovie { sid: ServerId::UNSET, rk: format!("h{i}"), ..Default::default() }).collect();
    d.related.extend((from..from + 24).map(|n| plx_data::pms::PmsMovie { sid: ServerId::UNSET, rk: format!("t{n}"), ..Default::default() }));
    d.related_tail = plx_data::metadata::RelatedTail {
        hubs: vec![plx_data::metadata::TailHub { key: "/library/metadata/1/similar".into(), preview: 3, len: Some(100) }],
        head: 3,
        positions: (from..from + 24).collect(),
        offset: from,
        end: from + 24,
        total: 100,
        more: true,
        ..Default::default()
    };
    d
}

#[test]
fn focus_on_a_tail_card_survives_the_window_sliding() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot_with(with_tail(0));
    let at = |d: &Dispatcher<TestHost>, n: usize| screen(d).engine_key(related::elem(3 + n).unwrap()).unwrap();
    let key = FocusKey { entry: screen(&d).entry, elem: at(&d, 20) };
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 23);
    // The window moves twelve forward: `t20` is now the ninth tail card, and the cards it left are gone.
    land(&mut d, &mut rig, with_tail(12), 32);
    assert_eq!(d.focus(), Some(key), "the focused card keeps its key through the slide");
    assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 3 + 8);
    // And back.
    land(&mut d, &mut rig, with_tail(0), 48);
    assert_eq!(d.focus(), Some(key));
    assert_eq!(screen(&d).locate(key.elem, test_store().view()).unwrap().index(), 23);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The page is read again for the same item (a return from the player, a watched toggle) while
/// focus stands on a card deep in the Related row, away from the head. The fresh read holds only
/// the head page; it lands through the store's own install, and the card under the focus is the
/// same card (by ratingKey) at the same place, not whatever the head's column index now names.
#[test]
fn a_reread_of_the_page_leaves_focus_on_the_same_related_card() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot_with(with_tail(72));
    let key = FocusKey { entry: screen(&d).entry, elem: screen(&d).engine_key(related::elem(3 + 8).unwrap()).unwrap() };
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    let card = |d: &Dispatcher<TestHost>| {
        let index = screen(d).locate(d.focus().unwrap().elem, test_store().view())?.index();
        Some((index, test_store().view().current()?.related.get(index)?.rk.clone()))
    };
    assert_eq!(card(&d), Some((11, "t80".to_string())));
    // the fresh page: the head and no window
    let mut fresh = with_tail(0);
    fresh.related.truncate(3);
    fresh.related_tail.positions.clear();
    fresh.related_tail.end = 24;
    let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "a");
    let (state, adapter) = test_store().split_for_test();
    assert!(plx_data::metadata::land_detail_for_test(state, adapter, ServerId::UNSET, "a", generation, Some(fresh)));
    d.store_changed(StoreId::Metadata.ord(), 32);
    frame(&mut d, &mut rig, 32);
    assert_eq!(d.focus(), Some(key), "the focused card keeps its key through the re-read");
    assert_eq!(card(&d), Some((11, "t80".to_string())), "and it is still the same card, in the same place");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The common path: the user walks the Related row well past the head, opens a card (item B's page
/// replaces the store's item), and presses Back. The store holds B, so A is read again and lands
/// with only the Related head; the window the user stood in is gone. Focus must still come back to
/// the card the user left, among its true neighbours, and never rest on another card on the way.
/// `answer` is the stand-in store's reply to a window ask (the window it reads, or `None` for a
/// failed read), after the store's own refusal of an ask that names a window it has replaced.
/// Returns the dispatcher, the cards focus rested on in order, and the window asks made.
fn back_from_deep_in_the_related_row(answer: impl FnMut(usize) -> Option<Detail>) -> (Dispatcher<TestHost>, Vec<String>, usize) {
    back_from_deep_pressing(600, None, answer)
}

/// [`back_from_deep_in_the_related_row`] run for `frames` frames, with `press` (frame, key) pressed
/// once on the way.
fn back_from_deep_pressing(
    frames: u32,
    press: Option<(u32, plx_machine::machine::Key)>,
    answer: impl FnMut(usize) -> Option<Detail>,
) -> (Dispatcher<TestHost>, Vec<String>, usize) {
    let (d, _, seated, asks) = back_from_deep_over(false, frames, press, answer);
    (d, seated, asks)
}

/// [`back_from_deep_pressing`] with the page over a root page when `under` is set, handing back the
/// rig as well so the run can go on.
fn back_from_deep_over(
    under: bool,
    frames: u32,
    press: Option<(u32, plx_machine::machine::Key)>,
    mut answer: impl FnMut(usize) -> Option<Detail>,
) -> (Dispatcher<TestHost>, TestRig, Vec<String>, usize) {
    use plx_machine::machine::{Edge, InputEvent, InputKind, Source};
    let (mut d, mut rig) = boot_over(with_tail(72), under);
    let key = FocusKey { entry: screen(&d).entry, elem: screen(&d).engine_key(related::elem(3 + 8).unwrap()).unwrap() };
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    assert_eq!(related_card(&d, 0).as_deref(), Some("t80"));
    // OK on it: B's page covers A, which the store now replaces with B.
    d.request(MachineId::Nav, NavOp::Push(Arg("b".into())));
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(item("b", false)));
    frame(&mut d, &mut rig, 32);
    assert_eq!(d.nav.top_page().unwrap().arg.0, "b");
    // Back: A is requested afresh and lands as the server would send it, the head and no window.
    d.request(MachineId::Nav, NavOp::Pop);
    let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "a");
    frame(&mut d, &mut rig, 48);
    let mut fresh = with_tail(0);
    fresh.related.truncate(3);
    (fresh.related_tail.positions, fresh.related_tail.offset, fresh.related_tail.end) = (Vec::new(), 0, 0);
    {
        let (state, adapter) = test_store().split_for_test();
        assert!(plx_data::metadata::land_detail_for_test(state, adapter, ServerId::UNSET, "a", generation, Some(fresh)));
    }
    d.store_changed(StoreId::Metadata.ord(), 64);
    // Pump until settled. Focus is on a card the user left, or on no card, on every frame: it is
    // never seated on a card that then changes under it.
    let (mut seated, mut asks) = (Vec::new(), 0);
    for i in 0..frames {
        let ms = 64 + i * 16;
        match press.filter(|(at, _)| *at == i) {
            Some((_, key)) => {
                let down = InputEvent { at: tick(ms), source: Source::Script, kind: InputKind::Key {
                    key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } };
                d.frame_with(&mut rig, tick(ms), vec![down], vec![], &mut NoTap, false);
            }
            None => frame(&mut d, &mut rig, ms),
        }
        // the application answers `ContentReq::Back` by popping the page
        while std::mem::take(&mut rig.backs) > 0 { d.request(MachineId::Nav, NavOp::Pop); }
        for cmd in rig.asked.drain(..) {
            let MetadataCmd::SeekRelated { at, seen } = cmd else { continue };
            asks += 1;
            let now = test_store().view().current().map(|c| (c.related_tail.offset, c.related_tail.end));
            if Some(seen) != now { continue }
            if let Some(window) = answer(at) {
                let (state, adapter) = test_store().split_for_test();
                plx_data::metadata::land_related_for_test(adapter, seen, &window);
                plx_data::metadata::pump_related_pages(state, adapter);
                d.store_changed(StoreId::Metadata.ord(), ms);
            }
        }
        if let Some(rk) = related_card(&d, 0) {
            if seated.last() != Some(&rk) { seated.push(rk); }
        }
    }
    (d, rig, seated, asks)
}

/// The Related card `by` places from the one focus rests on, by ratingKey.
fn related_card(d: &Dispatcher<TestHost>, by: isize) -> Option<String> {
    let index = screen(d).locate(d.focus()?.elem, test_store().view())?.index();
    Some(test_store().view().current()?.related.get(index.checked_add_signed(by)?)?.rk.clone())
}

/// The window the listing gives when opened for tail position `at`: 24 cards, `at` the thirteenth.
fn a_window_read_at(at: usize) -> Option<Detail> {
    Some(with_tail(at.saturating_sub(12).min(76)))
}

#[test]
fn back_from_a_card_opened_deep_in_the_related_row_returns_to_that_card() {
    let _guard = plx_base::testlock::serial();
    let (d, seated, asks) = back_from_deep_in_the_related_row(a_window_read_at);
    assert_eq!(related_card(&d, 0).as_deref(), Some("t80"), "the card left is the card returned to (seated on {seated:?})");
    assert_eq!((related_card(&d, -1).as_deref(), related_card(&d, 1).as_deref()), (Some("t79"), Some("t81")),
        "and its neighbours are its true neighbours");
    assert_eq!(seated, vec!["t80".to_string()], "no other card held focus on the way");
    assert_eq!(asks, 1, "one ask opens the window");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The listing changed while the user was away and the card they left is not in the window that
/// opens at its place: focus goes to the nearest surviving place, and to no other card before.
#[test]
fn back_to_a_row_whose_card_left_the_listing_seats_the_nearest_place() {
    let _guard = plx_base::testlock::serial();
    let (d, seated, _) = back_from_deep_in_the_related_row(|at| {
        let mut window = a_window_read_at(at)?;
        let t = &mut window.related_tail;
        let k = t.positions.iter().position(|&p| p == 80).unwrap();
        t.positions.remove(k);
        let head = t.head;
        window.related.remove(head + k);
        Some(window)
    });
    assert_eq!(related_card(&d, 0).as_deref(), Some("t79"), "seated on {seated:?}");
    assert_eq!(seated, vec!["t79".to_string()]);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The read never answers: the ask is repeated on a capped ladder for as long as the page is on
/// screen, and focus is never moved to a card the user did not leave (not the row's first).
#[test]
fn back_to_a_row_whose_window_cannot_be_read_keeps_asking_and_keeps_the_place() {
    let _guard = plx_base::testlock::serial();
    let (d, seated, asks) = back_from_deep_pressing(60 * 120, None, |_| None);
    assert!(seated.is_empty(), "no card took the focus: {seated:?}");
    assert!(related_card(&d, 0).is_none(), "focus is still held, not seated on the head");
    assert!(asks >= 10, "the ask keeps being repeated, {asks} asks in two minutes");
    assert!(asks <= 14, "on a ladder, not every frame: {asks}");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The read fails for a while and then answers: the card the user left is seated, however late.
#[test]
fn back_to_a_row_whose_window_lands_after_many_failures_seats_the_card_left() {
    let _guard = plx_base::testlock::serial();
    let mut failures = 0;
    let (d, seated, asks) = back_from_deep_pressing(60 * 60, None, |at| {
        failures += 1;
        if failures <= 7 { None } else { a_window_read_at(at) }
    });
    assert_eq!(related_card(&d, 0).as_deref(), Some("t80"), "seated on {seated:?}");
    assert_eq!(seated, vec!["t80".to_string()], "no other card held focus on the way");
    assert_eq!(asks, 8);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// During the hold the page is not stuck: a direction key leaves the held place for a card and ends
/// the restore (no further asks).
#[test]
fn a_key_during_the_hold_for_an_unreadable_window_ends_the_restore() {
    use plx_machine::machine::Key;
    let _guard = plx_base::testlock::serial();
    for key in [Key::Up, Key::Down, Key::Left, Key::Right] {
        let (_, seated, asks) = back_from_deep_pressing(600, Some((100, key)), |_| None);
        let before_press = back_from_deep_pressing(100, None, |_| None).2;
        assert!(!seated.is_empty(), "{key:?}: the press seated focus somewhere: {seated:?}");
        assert!(asks <= before_press + 1, "{key:?}: the restore ended with the press, {asks} asks vs {before_press}");
        plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    }
}

/// Back during the hold for a window that never lands leaves the page like Back anywhere: it pops,
/// nothing is left pending, and the page entered again later opens at its head instead of resuming
/// a seek for the place the abandoned page was holding.
#[test]
fn back_during_the_hold_for_an_unreadable_window_leaves_the_page_and_leaves_no_seek_behind() {
    use plx_machine::machine::Key;
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig, _, asks) = back_from_deep_over(true, 300, Some((100, Key::Back)), |_| None);
    let before_press = back_from_deep_over(true, 100, None, |_| None).3;
    assert_eq!(d.nav.top_page().unwrap().arg.0, "base", "Back asked to leave and the page went");
    assert!(asks <= before_press + 1, "and the restore went with it: {asks} asks vs {before_press} by the press");
    rig.asked.clear();
    // the same item entered again: a fresh read holds only the head and focus has never been deep
    let mut fresh = with_tail(0);
    fresh.related.truncate(3);
    (fresh.related_tail.positions, fresh.related_tail.offset, fresh.related_tail.end) = (Vec::new(), 0, 0);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(fresh));
    d.request(MachineId::Nav, NavOp::Push(Arg("a".into())));
    for i in 0..600 {
        d.store_changed(StoreId::Metadata.ord(), 5000 + i * 16);
        frame(&mut d, &mut rig, 5000 + i * 16);
    }
    assert_eq!(d.nav.top_page().unwrap().arg.0, "a");
    let seeks = rig.asked.iter().filter(|cmd| matches!(cmd, MetadataCmd::SeekRelated { .. })).count();
    assert_eq!(seeks, 0, "no seek for a place the page never held");
    assert!(related_card(&d, 0).is_none_or(|rk| rk.starts_with('h')), "focus is not held for a tail place");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The invariant behind "focus never moves under the user" on the Related row, driven through the
/// real dispatcher (keys, the focus engine, the page) and a stand-in store that applies the
/// commands it is sent the way the metadata store does: a `WantRelated` is refused when it names a
/// window the store has since replaced (`seen`), when one is already out, or when there is nothing
/// that way; it lands after a random delay by sliding the 24-card tail window twelve places; a
/// `CancelRelated` drops the one out. A landing is installed and announced at the start of a
/// frame, ahead of that frame's key, as the application does it. Random walks of presses (runs,
/// bursts, reversals, 100 ms repeats) meet random landing delays (one frame to five seconds); on
/// EVERY frame the focused card changes only by a press and by exactly one place in the row, and
/// is a card the store holds. A held key then reaches the last card and walks back to the first.
/// Landings slower than the walk are the case that matters on the way back: the hold arrives at
/// the window's first card before the window before it has landed.
#[test]
fn no_landing_on_the_related_row_ever_moves_the_card_under_the_focus() {
    use plx_machine::machine::{Edge, InputEvent, InputKind, Key, Source};
    let _guard = plx_base::testlock::serial();
    const TAIL: usize = 100;
    let window = |from: usize| {
        let mut d = with_tail(from);
        d.related_tail.total = TAIL;
        d.related_tail.more = from + 24 < TAIL;
        d
    };
    // a card's place in the whole row: the head's three, then the tail
    let place = |rk: &str| -> usize {
        let n: usize = rk[1..].parse().unwrap();
        if rk.starts_with('h') { n } else { 3 + n }
    };
    for seed in 1..=8u64 {
        let mut rng = plx_ui::fixture::Walk(seed);
        let mut offset = 0usize;
        let (mut d, mut rig) = boot_with(window(offset));
        let key = FocusKey { entry: screen(&d).entry, elem: screen(&d).engine_key(related::elem(3).unwrap()).unwrap() };
        d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
        let card_now = |d: &Dispatcher<TestHost>| -> Option<String> {
            let key = d.focus()?;
            let index = screen(d).locate(key.elem, test_store().view())?.index();
            Some(test_store().view().current()?.related.get(index)?.rk.clone())
        };
        let mut pending: Option<(u32, bool)> = None;
        let (mut dir_right, mut run_left, mut cadence) = (true, 40u64, 6u32);
        let mut held = card_now(&d).expect("focus starts on the first tail card");
        assert_eq!(held, "t0");
        let mut reached = (false, false);
        let mut landings = 0u32;
        let frames = 24_000u32;
        for frame in 1..frames + 6000 {
            let at = format!("seed {seed}, frame {frame}, offset {offset}");
            let ms = 100 + frame * 1000 / 60;
            let tail = frame >= frames;
            if !tail && run_left == 0 {
                dir_right = rng.below(3) != 0 && place(&held) + 4 < 3 + TAIL;
                let longest = if rng.below(4) == 0 { 40 } else { 14 };
                run_left = 1 + rng.below(longest);
                cadence = [1, 2, 6, 6, 6, 12, 30][rng.below(7) as usize];
            }
            let (right, due) = if tail { (!reached.0, frame % 6 == 0) } else { (dir_right, frame % cadence == 0) };
            if pending.is_some_and(|(due, _)| frame >= due) {
                let (_, before) = pending.take().unwrap();
                let next = if before { offset.saturating_sub(12) } else { (offset + 12).min(TAIL - 24) };
                if next != offset {
                    offset = next;
                    landings += 1;
                    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(window(offset)));
                    d.store_changed(StoreId::Metadata.ord(), frame);
                }
            }
            let inputs = if due {
                run_left = run_left.saturating_sub(1);
                vec![InputEvent { at: tick(ms), source: Source::Script, kind: InputKind::Key {
                    key: if right { Key::Right } else { Key::Left }, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } }]
            } else { Vec::new() };
            let focus_before = d.focus();
            let report = d.frame_with(&mut rig, tick(ms), inputs, vec![], &mut NoTap, false);
            d.prune(&report.unmounted);
            for cmd in rig.asked.drain(..) {
                match cmd {
                    MetadataCmd::WantRelated { before, seen } => {
                        let room = if before { offset > 0 } else { offset + 24 < TAIL };
                        if seen == (offset, offset + 24) && pending.is_none() && room {
                            pending = Some((frame + 1 + rng.below(300) as u32, before));
                        }
                    }
                    MetadataCmd::CancelRelated => pending = None,
                    _ => {}
                }
            }
            let now = card_now(&d).unwrap_or_else(|| panic!("{at}: the focused card is not one the store holds"));
            if d.focus() == focus_before {
                assert_eq!(now, held, "{at}: the card under the focus changed without a press");
            } else {
                assert!(due, "{at}: focus moved without a press");
                // one place along the whole row. The head's last card and the window's first sit
                // side by side while the window is away from the tail's start; a press there
                // waits for the way back rather than crossing every card between them.
                let (a, b) = (place(&held), place(&now));
                assert!(if right { b == a + 1 } else { a == b + 1 }, "{at}: one press took focus from {held} to {now}");
                held = now;
            }
            if held == format!("t{}", TAIL - 1) { reached.0 = true; }
            if reached.0 && held == "h0" { reached.1 = true; }
        }
        assert!(reached.0, "seed {seed}: the last card is reachable (stopped on {held})");
        assert!(reached.1, "seed {seed}: and so is the first (stopped on {held})");
        assert!(landings >= 14, "seed {seed}: the window slid there and back under the walk: {landings}");
    }
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The row is the head and a window of the tail, drawn side by side. While the window is away
/// from the tail's start, the cards between the head and the window are not in the row: a press
/// at that seam does not cross them (seen on the simulator on a slow link: a held Left outran the
/// way-back read and went from the 286th card to the 9th), and focus resting on the head asks
/// for the way back, so the seam closes instead of staying shut.
#[test]
fn a_press_never_crosses_the_cards_between_the_head_and_a_window_that_slid_away() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot_with(with_tail(12));
    let key_at = |d: &Dispatcher<TestHost>, index: usize| FocusKey { entry: screen(d).entry,
        elem: screen(d).engine_key(related::elem(index).unwrap()).unwrap() };
    let step = |d: &Dispatcher<TestHost>, from: FocusKey<u32>, dir: Dir| {
        let measure = FixtureMeasure;
        let cx = Cx::<TestHost> { views: (), tick: tick(16), measure: &measure,
            press: Default::default(), focus: Default::default(), owner: InputOwner::Entry(from.entry) };
        Focusable::<TestHost>::neighbour(screen(d), from, dir, &cx)
    };
    let (head_last, window_first) = (key_at(&d, 2), key_at(&d, 3));
    assert!(matches!(step(&d, window_first, Dir::Left), Step::Edge), "t12 is not next to h2");
    assert!(matches!(step(&d, head_last, Dir::Right), Step::Edge), "h2 is not next to t12");
    assert!(matches!(step(&d, window_first, Dir::Right), Step::Move(to) if to == key_at(&d, 4)), "inside the window a press steps");
    assert!(matches!(step(&d, head_last, Dir::Left), Step::Move(to) if to == key_at(&d, 1)), "and inside the head");
    // resting on the head with the window away: the way back is asked for
    d.set_focus_in(Some(head_last), Some(related::RELATED_GROUP));
    rig.asked.clear();
    for i in 0..4 { frame(&mut d, &mut rig, 100 + i * 16); }
    assert!(rig.asked.iter().any(|cmd| matches!(cmd, MetadataCmd::WantRelated { before: true, seen: (12, 36) })),
        "the head asks for the window before this one: {} asked", rig.asked.len());
    // with the window at the tail's start the row is whole: the same press steps
    land(&mut d, &mut rig, with_tail(0), 400);
    assert!(matches!(step(&d, key_at(&d, 2), Dir::Right), Step::Move(to) if to == key_at(&d, 3)), "h2 is next to t0");
    assert!(matches!(step(&d, key_at(&d, 3), Dir::Left), Step::Move(to) if to == key_at(&d, 2)));
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// Walking back toward the head of a slid window after the way-back read failed or was refused is
/// not a dead end: with no key pressed the ask is repeated on the shelf's ladder while focus stands
/// at the leading edge, and stops once the window lands (or focus has left).
#[test]
fn a_refused_way_back_read_is_repeated_while_focus_stands_at_the_leading_edge() {
    let _guard = plx_base::testlock::serial();
    let (mut d, mut rig) = boot_with(with_tail(12));
    let key = FocusKey { entry: screen(&d).entry, elem: screen(&d).engine_key(related::elem(3 + 1).unwrap()).unwrap() };
    d.set_focus_in(Some(key), Some(related::RELATED_GROUP));
    rig.asked.clear();
    let mut asks = 0;
    for i in 0..60 * 60u32 {
        frame(&mut d, &mut rig, 100 + i * 16);
        asks += rig.asked.drain(..).filter(|cmd| matches!(cmd, MetadataCmd::WantRelated { before: true, .. })).count();
    }
    assert!(asks >= 8, "the unanswered way-back ask is repeated, {asks} asks in a minute");
    assert!(asks <= 12, "on a ladder, not every frame: {asks}");
    assert_eq!(d.focus(), Some(key), "and focus never moved");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
}

/// The focus read-out (`cdg`) names a Related card in head-then-listing order: the head keeps its
/// index; a tail card sits at the head's length plus the tail position it was read at.
#[test]
fn related_cards_are_named_in_head_then_listing_order() {
    let global = |d: &Detail, which, i| {
        let keys = std::collections::HashMap::new();
        plx_ui::cards::CardSource::<TestHost>::global(&cards::Cards::new(which, d, &keys, &keys), i)
    };
    let (opening, slid) = (with_tail(0), with_tail(12));
    assert_eq!(global(&opening, cards::Which::Related, 2), 2, "the last head card");
    assert_eq!(global(&opening, cards::Which::Related, 3), 3, "the first tail card of the opening window");
    assert_eq!(global(&slid, cards::Which::Related, 2), 2, "the head does not move with the window");
    assert_eq!(global(&slid, cards::Which::Related, 3), 15, "tail position 12 follows the 3-card head");
    assert_eq!(global(&slid, cards::Which::Related, 26), 38);
    assert_eq!(global(&slid, cards::Which::Cast, 26), 26, "other shelves hold their listing whole");
}
