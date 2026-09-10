//! The §6 container tests (spec §15.1), over the fixture bundle. What these grade is the
//! container's DECISIONS — which entry hears what, in which order, and who owns input — never a
//! pixel: the surfaces' scrims and the dip's colour are device captures.

use super::modal::{HostRender, HostUpdate, Phase, Style};
use super::stack::CAP;

#[test]
fn restored_live_and_evicted_bodies_receive_memory_before_enter() {
    for evict in [false, true] {
        let (mut d, mut rig, _) = booted();
        let root = d.nav.top_page().unwrap().id;
        let count = if evict { CAP + 1 } else { 1 };
        for i in 0..count {
            d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(20 + i as u32)));
            let report = d.frame(&mut rig, tick(16 + i as u32 * 16), vec![], vec![], &mut NoTap);
            d.prune(&report.unmounted);
        }
        assert_eq!(d.nav.entry(root).unwrap().inst.is_none(), evict);
        d.request(MachineId::Nav, NavOp::PopTo(root));
        let report = d.frame(&mut rig, tick(400), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
        let events = events_of(&d, 0);
        let memory = events.rfind("\"restore_memory\"").expect("return hydration is a lifecycle event");
        let enter = events.rfind("\"enter\"").unwrap();
        assert!(memory < enter, "memory precedes restored Enter, evicted={evict}: {events}");
    }
}
use super::transition::PageDip;
use crate::ui::dispatch::{Dispatcher, NoTap};
use crate::ui::fixture::{booted, events_of, key, tick, FixtureArg, FixtureHost, FixtureRig};
use crate::ui::machine::{EntryId, FocusKey, InputOwner, Key, MachineId, NavOp};

fn open_modal(d: &mut Dispatcher<FixtureHost>, rig: &mut FixtureRig, style: Style, ms: u32) -> EntryId {
    d.nav.next_style = style;
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(rig, tick(ms), vec![], vec![], &mut NoTap);
    d.nav.modals.top().expect("presented").entry.id
}

#[test]
fn a_request_freezes_inactive_group_cursors_before_focus_moves_during_the_fade() {
    use crate::ui::machine::GroupId;
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    d.set_focus_in(Some(FocusKey { entry: home, elem: 2 }), Some(GroupId(71)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 1 }), Some(GroupId(1)));
    let saved = d.return_state();
    d.nav.tabs.stack.transition = Box::new(PageDip::new());
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(9)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 4 }), Some(GroupId(71)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    for ms in (32..=192).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.nav.entry(home).unwrap().ret.focus, saved.focus);
    assert_eq!(d.nav.entry(home).unwrap().ret.remembered, saved.remembered);
    d.request(MachineId::Nav, NavOp::Pop);
    for ms in (208..=448).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.nav.top_page().unwrap().id, home);
    assert!(d.return_state().remembered.contains(&(GroupId(71), 2)));
    assert!(!d.return_state().remembered.contains(&(GroupId(71), 4)));
}

#[test]
fn filmography_detail_back_restores_the_same_modal_instance_and_cursor() {
    use crate::ui::machine::GroupId;
    let (mut d, mut rig, _) = booted();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(1)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let person = d.nav.top_page().unwrap().id;
    let person_focus = FocusKey { entry: person, elem: 1 };
    d.set_focus_in(Some(person_focus), Some(GroupId(1)));
    let filmography = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 32);
    let instance = d.nav.instance_of(filmography).unwrap();
    let focus = FocusKey { entry: filmography, elem: 0 };
    d.set_focus_in(Some(focus), Some(GroupId(9)));
    // Page(2) deliberately pushes Page(3) on Enter in FixtureScreen; use a passive page.
    d.request(MachineId::Instance(instance), NavOp::Push(FixtureArg::Page(20)));
    d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert!(d.nav.modals.is_empty(), "Filmography is not drawn over Detail");
    assert_eq!(d.nav.instance_of(filmography), Some(instance), "the covered surface stays mounted");
    let report = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert!(!report.ticked.contains(&instance), "the covered Filmography does not animate");
    d.request(MachineId::Nav, NavOp::Pop);
    d.frame(&mut rig, tick(80), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.top_page().unwrap().id, person);
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(filmography)));
    assert_eq!(d.nav.instance_of(filmography), Some(instance));
    assert_eq!(d.focus(), Some(focus));
    assert_eq!(d.nav.entry(person).unwrap().ret.focus, Some(person_focus), "the modal did not overwrite its host's return state");
    d.request(MachineId::Nav, NavOp::Dismiss(filmography));
    d.frame(&mut rig, tick(96), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(person)));
    assert_eq!(d.focus(), Some(person_focus), "dismiss restores the Person entry control, not Filmography's row key");
}

#[test]
fn scoped_surface_bodies_are_bounded_and_remount_after_their_owner_data_lands() {
    let (mut d, mut rig, _) = booted();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(901)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let owner = d.nav.top_page().unwrap().id;
    let child = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 32);
    let original = d.nav.instance_of(child).unwrap();
    let focus = d.focus();
    let mut ms = 48;
    for n in 0..CAP + 2 {
        d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(100 + n as u32)));
        let report = d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
        ms += 16;
        open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, ms);
        ms += 16;
        assert!(d.nav.bodies().count() <= 2 * CAP, "a child body is bounded by its owner's lifetime");
    }
    assert!(d.nav.entry(owner).unwrap().inst.is_none());
    assert!(d.nav.entry(child).unwrap().inst.is_none());
    assert_eq!(d.nav.entry(child).unwrap().ret.focus, focus);
    d.request(MachineId::Nav, NavOp::PopTo(owner));
    let report = d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    d.prune(&report.unmounted);
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.pending_surface(), Some(child));
    assert!(d.nav.entry(child).unwrap().inst.is_none(), "remount waits for owner data, not just owner body");
    rig.store.view.items.push(7);
    d.store_changed(crate::ui::machine::StoreOrd(0), 1);
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(child)));
    assert_ne!(d.nav.instance_of(child), Some(original), "eviction creates a fresh body");
    assert!(d.nav.instance_of(child).is_some());
    assert_eq!(d.focus(), focus, "the EntryId and its engine focus survive body eviction");
    assert!(d.nav.bodies().count() <= 3);
}

#[test]
fn an_owned_surface_finishes_opening_independently_of_legacy_page_alpha() {
    let (mut d, mut rig, _) = booted();
    rig.page_alpha = 0.52;
    let id = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 16);
    for ms in (32..=9616).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        if d.nav.modals.surface(id).unwrap().phase == Phase::Open { break; }
    }
    let surface = d.nav.modals.surface(id).unwrap();
    assert_eq!(surface.phase, Phase::Open, "the surface must settle within the bounded frame budget");
    assert!(surface.motion.settled());
    let page = d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureModal>().unwrap();
    assert_eq!(page.last_draw_alpha, 1.0, "legacy page fading cannot cap the surface's own slide/opacity");
}

#[test]
fn one_navigation_snapshot_reaches_draw_frames_without_mutating_logical_state() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    rig.page_alpha = 0.21;
    rig.chrome_alpha = 0.37;
    rig.view_tab = Some(2);
    rig.blur_amount = 0.63;
    let before = d.state_hash();
    let reads = rig.navigation_reads.get();
    d.draw(&mut rig, true);
    let modal = d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureModal>().unwrap();
    assert_eq!(modal.last_navigation.chrome_alpha, 0.37);
    assert_eq!(modal.last_navigation.view_tab, Some(2));
    assert_eq!(modal.last_navigation.blur_amount, 0.63);
    assert_eq!(rig.navigation_reads.get(), reads + 1, "one snapshot, not live reads per screen");
    assert_eq!(d.state_hash(), before, "presentation is render state, not a logical mutation");
}

/// Present a Sheet (the account menu's shape): the page beneath is FROZEN and CACHED and
/// receives no Tick; dismiss it: the phase is Closing on the SAME frame, the host is live again,
/// and the surface keeps stepping until `prune` clears it — then, and only then, it unmounts.
#[test]
fn the_closing_phase_is_stepped_even_when_the_host_is_frozen() {
    let (mut d, mut rig, home) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    let surf = d.nav.instance_of(id).unwrap();
    let r = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(r.host_update, Some(HostUpdate::Frozen));
    assert_eq!(r.host_render, Some(HostRender::Cached));
    assert!(!r.ticked.contains(&home), "a frozen host receives no Tick");
    assert!(r.ticked.contains(&surf), "the surface does");
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));

    d.request(MachineId::Nav, NavOp::Dismiss(id));
    d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, Phase::Closing, "Closing from the commit that dismissed");
    assert_ne!(d.nav.input_owner(), Some(InputOwner::Entry(id)), "input returned to the page");
    let r = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert_eq!(r.host_update, Some(HostUpdate::Live), "Closing: the host is live again");
    assert!(r.ticked.contains(&home));
    assert!(r.ticked.contains(&surf), "…and the closing surface still steps");
    // the fade runs down; prune clears it; the body unmounts
    let mut unmounted = false;
    for i in 0..120u32 {
        let r = d.frame(&mut rig, tick(80 + i * 16), vec![], vec![], &mut NoTap);
        if r.unmounted.contains(&surf) {
            unmounted = true;
            d.prune(&r.unmounted);
            break;
        }
        assert!(r.ticked.contains(&surf), "frame {i}: a Closing surface is stepped unconditionally");
    }
    assert!(unmounted, "prune cleared the Closing surface");
    assert!(d.nav.modals.is_empty());
    let home_ev = events_of(&d, 0);
    assert!(home_ev.contains("\"cover\""), "{home_ev}");
    assert!(home_ev.contains("\"uncover\""), "{home_ev}");
}

/// `host_page_lifecycle_tests` ported onto the fold table: a full-screen surface and the
/// profile menu freeze the hidden page; the item menu keeps it live.
#[test]
fn the_fold_freezes_the_page_under_a_sheet_and_keeps_it_live_under_a_compact_popover() {
    use super::modal::surface_policy;
    assert_eq!(surface_policy(Style::Sheet, Phase::Open, false).0, HostUpdate::Frozen);
    assert_eq!(surface_policy(Style::Opaque { snapshot: true }, Phase::Open, true), (HostUpdate::Frozen, HostRender::Replaced));
    assert_eq!(surface_policy(Style::Opaque { snapshot: false }, Phase::Opening, false), (HostUpdate::Frozen, HostRender::Live));
    assert_eq!(surface_policy(Style::Compact, Phase::Open, false), (HostUpdate::Live, HostRender::Cached));
    assert_eq!(surface_policy(Style::PlayerPanel { survives_failure: true }, Phase::Open, false), (HostUpdate::Live, HostRender::Live));
    // the fold: a Compact over a Sheet is still Frozen/Cached; Closing releases the freeze
    let (mut d, mut rig, _) = booted();
    open_modal(&mut d, &mut rig, Style::Sheet, 16);
    open_modal(&mut d, &mut rig, Style::Compact, 32);
    assert_eq!(d.nav.modals.host_policy(), (HostUpdate::Frozen, HostRender::Cached));
}

/// A screen the application never named: mounting it costs the fixture's `Mounter` one arm and
/// the container nothing.
#[test]
fn a_fixture_screen_mounts_with_no_app_change() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Compact, 16);
    let inst = d.nav.instance_of(id).unwrap();
    let s = d.nav.instance_mut(inst).unwrap();
    assert_eq!(s.screen.name(), "settings");
    let mut probe = String::new();
    s.screen.state().probe(&mut probe);
    assert!(probe.contains("\"mount\", \"enter\""), "{probe}");
}

/// The Settings family's BACK (§6.2 `SettingsSurface`): the surface walks its OWN stack — two
/// inner pushes, two inner pops — and only at its own depth 0 does BACK reach the container,
/// which dismisses the surface. The app's stack never moves.
#[test]
fn settings_back_walks_its_own_stack_not_the_apps() {
    let (mut d, mut rig, _) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap); // Home → Page(1)
    let depth = d.nav.tabs.stack.depth();
    let id = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 32);
    let inner_depth = |d: &Dispatcher<FixtureHost>| {
        let inst = d.nav.instance_of(id).unwrap();
        let mut s = String::new();
        d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
        let _ = inst;
        s
    };
    d.frame(&mut rig, tick(48), vec![key(Key::Ok, tick(48))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(64), vec![key(Key::Ok, tick(64))], vec![], &mut NoTap);
    assert!(inner_depth(&d).contains("keys=2"), "two inner pushes: {}", inner_depth(&d));
    d.frame(&mut rig, tick(80), vec![key(Key::Back, tick(80))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(96), vec![key(Key::Back, tick(96))], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, Phase::Opening, "two inner pops: the surface is still up");
    assert_eq!(d.nav.tabs.stack.depth(), depth, "the app's stack never moved");
    let r = d.frame(&mut rig, tick(112), vec![key(Key::Back, tick(112))], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, Phase::Closing, "at its own depth 0, BACK dismisses");
    assert!(!r.back_at_root);
    assert_eq!(d.nav.tabs.stack.depth(), depth);
}

/// The spot a Detail was left at is captured at the PRESS (the request), on the entry, and a
/// BACK restores from it: `Uncover` + `Enter(Restored)` on the entry whose `ret` holds the key.
#[test]
fn back_off_a_detail_restores_the_spot_captured_at_the_press() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    let spot = FocusKey { entry: home, elem: 3 };
    d.set_focus(Some(spot));
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert_eq!(d.nav.entry(home).unwrap().ret.focus, Some(spot), "captured at the request");
    // focus moved on the page above; the capture is not overwritten
    d.set_focus(Some(FocusKey { entry: d.nav.top_page().unwrap().id, elem: 0 }));
    d.frame(&mut rig, tick(32), vec![key(Key::Back, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.nav.top_page().unwrap().id, home);
    assert_eq!(d.nav.entry(home).unwrap().ret.focus, Some(spot));
    let ev = events_of(&d, 0);
    assert!(ev.ends_with("\"uncover\", \"restore_memory\", \"enter\"]") || ev.contains("\"uncover\", \"restore_memory\", \"enter\""), "{ev}");
}

/// Detail → Person → Detail is three entries with three ids (§5.1: supersede-by-identity).
#[test]
fn person_detail_person_is_three_entries() {
    let (mut d, mut rig, _) = booted();
    for ms in [16, 32, 48] {
        d.frame(&mut rig, tick(ms), vec![key(Key::Ok, tick(ms))], vec![], &mut NoTap);
    }
    let ids: Vec<EntryId> = d.nav.tabs.stack.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 4, "Home + three pushes");
    let mut sorted = ids.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "every entry has its own id: {ids:?}");
    assert_eq!(d.nav.tabs.stack.entries[1].arg, FixtureArg::Page(1));
    assert_eq!(d.nav.tabs.stack.entries[2].arg, FixtureArg::Page(2));
    assert_eq!(d.nav.tabs.stack.entries[3].arg, FixtureArg::Page(3));
}

/// A page's logical state survives a push over it and a pop back: the Library's section, scroll
/// and cursor are the instance's own while it lives (§6.1 tier 1) — nothing remounts.
#[test]
fn library_remembers_its_section_scroll_and_cursor_across_a_detail_push() {
    let (mut d, mut rig, home) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    let before = d.nav.instance_mut(home).unwrap().screen.state().hash();
    d.frame(&mut rig, tick(32), vec![key(Key::Back, tick(32))], vec![], &mut NoTap);
    let inst = d.nav.instance_of(d.nav.top_page().unwrap().id).unwrap();
    assert_eq!(inst, home, "the same body: no remount");
    let probe = events_of(&d, 0);
    assert!(probe.contains("keys=1"), "the key it handled is still counted: {probe}");
    assert_ne!(before, 0);
}

/// Search's query and shelves are the instance's; a result pushed over it and popped leaves them.
#[test]
fn search_keeps_its_query_and_shelves_across_a_result_push() {
    let (mut d, mut rig, home) = booted();
    // a result opened from Home (Page(1)), a key handled on Home first so its state is non-trivial
    d.frame(&mut rig, tick(16), vec![key(Key::Down, tick(16))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(32), vec![key(Key::Ok, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.nav.tabs.stack.depth(), 2);
    d.frame(&mut rig, tick(48), vec![key(Key::Back, tick(48))], vec![], &mut NoTap);
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    assert_eq!(d.nav.instance_of(d.nav.top_page().unwrap().id), Some(home));
    let probe = events_of(&d, 0);
    assert!(probe.contains("keys=1"), "{probe}");
    assert_eq!(probe.matches("\"cover\"").count(), 1);
    assert_eq!(probe.matches("\"uncover\"").count(), 1);
}

/// A profile switch drops every entry — surfaces first, then the pages top-down.
#[test]
fn switching_profile_drops_every_tab_instance() {
    let (mut d, mut rig, _) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    open_modal(&mut d, &mut rig, Style::Compact, 32);
    assert_eq!(d.nav.bodies().count(), 3);
    d.reset_for_profile();
    let r = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert_eq!(r.unmounted.len(), 3);
    d.prune(&r.unmounted);
    assert_eq!(d.nav.bodies().count(), 0);
    assert_eq!(d.nav.tabs.stack.depth(), 0);
    assert!(d.nav.modals.is_empty());
    assert_eq!(d.nav.input_owner(), None);
    // the next Root rebuilds the tree
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert_eq!(d.top_screen().unwrap().name(), "home");
}

/// Push past `CAP`: the oldest body below the top is evicted (its `Unmount` delivered, its
/// inflight retired), the ENTRY and its `ReturnState` stay, and popping back to it remounts it
/// under the same `EntryId` with the focus it was left at.
#[test]
fn an_evicted_entry_keeps_its_focus_identity_on_remount() {
    let (mut d, mut rig, _) = booted();
    // The original test only inspected the saved key. The stronger engine assertion below
    // needs slot 5 to exist, so reconciliation cannot legitimately clamp it on remount.
    rig.store.view.items.resize(6, 0);
    d.store_changed(crate::ui::machine::StoreOrd(0), 1);
    d.frame(&mut rig, tick(1), vec![], vec![], &mut NoTap);
    let home = d.nav.top_page().unwrap().id;
    d.set_focus_in(Some(FocusKey { entry: home, elem: 2 }), Some(crate::ui::machine::GroupId(71)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 4 }), Some(crate::ui::machine::GroupId(72)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 5 }), Some(crate::ui::machine::GroupId(1)));
    let remembered = d.return_state().remembered;
    let mut evicted_at = None;
    for i in 0..(CAP as u32 + 2) {
        let ms = 16 * (i + 1);
        let r = d.frame(&mut rig, tick(ms), vec![key(Key::Ok, tick(ms))], vec![], &mut NoTap);
        if !r.unmounted.is_empty() && evicted_at.is_none() {
            evicted_at = Some(i);
        }
        d.prune(&r.unmounted);
    }
    assert!(evicted_at.is_some(), "an eviction happened past CAP");
    let home_entry = d.nav.entry(home).unwrap();
    assert!(home_entry.inst.is_none(), "Home's body was evicted");
    assert!(home_entry.evicted);
    assert_eq!(home_entry.ret.focus, Some(FocusKey { entry: home, elem: 5 }), "its return state stayed");
    let bodies = d.nav.bodies().count();
    assert!(bodies <= CAP, "bodies={bodies}");
    // pop all the way back down: Home remounts under the SAME id with its focus
    let mut ms = 1000;
    let mut guard = 0;
    while d.nav.tabs.stack.depth() > 1 {
        let r = d.frame(&mut rig, tick(ms), vec![key(Key::Back, tick(ms))], vec![], &mut NoTap);
        d.prune(&r.unmounted);
        ms += 16;
        guard += 1;
        assert!(guard < 64, "BACK stopped popping at depth {} (owner {:?}, top body {:?})", d.nav.tabs.stack.depth(), d.nav.input_owner(), d.nav.top_page().map(|e| (e.id, e.inst.is_some(), e.evicted)));
    }
    let e = d.nav.top_page().unwrap();
    assert_eq!(e.id, home, "the same EntryId");
    assert!(e.inst.is_some(), "remounted");
    assert!(!e.evicted);
    assert_eq!(e.ret.focus, Some(FocusKey { entry: home, elem: 5 }));
    assert_eq!(e.ret.remembered, remembered, "inactive groups survive body eviction");
    for cursor in remembered {
        assert!(d.return_state().remembered.contains(&cursor));
    }
    let ev = events_of(&d, 0);
    assert!(ev.contains("\"mount\", \"uncover\", \"restore_memory\", \"enter\""), "remount then restore: {ev}");
}

/// While a surface is up it owns input: a key goes to it and never to the page beneath.
#[test]
fn a_modal_scopes_focus_to_its_own_groups() {
    let (mut d, mut rig, home) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Compact, 16);
    d.frame(&mut rig, tick(32), vec![key(Key::Ok, tick(32))], vec![], &mut NoTap);
    let mut s = String::new();
    d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
    assert!(s.contains("keys=1"), "the surface got the key: {s}");
    let mut h = String::new();
    d.nav.instance_mut(home).unwrap().screen.state().probe(&mut h);
    assert!(h.contains("keys=0"), "the page did not: {h}");
    assert_eq!(d.nav.tabs.stack.depth(), 1, "and no page opened");
}

/// §8.3: the render set is checked over the WHOLE composition — a Cached host counts the one
/// shared FrameCache, never a render of its own; a synthetic set over the ceiling fails.
#[test]
fn the_render_set_is_checked_over_the_whole_frame() {
    use crate::ui::frame::{RenderBreach, RenderSet, FRAME_CACHE_BYTES, RENDER_BYTES_MAX};
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    d.present.note(crate::ui::present::PresentEvent::Damage(crate::ui::present::Provenance::Input));
    let r = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert!(r.presented);
    assert_eq!(r.render_set.pages, 1);
    assert_eq!(r.render_set.surfaces, vec![(id, 1)]);
    assert_eq!(r.render_set.frame_cache_bytes, FRAME_CACHE_BYTES, "the Cached host is one FrameCache");
    assert!(r.render_set.check().is_ok());
    let over = RenderSet {
        pages: 1,
        surfaces: vec![(id, 1)],
        bytes: RENDER_BYTES_MAX,
        frame_cache_bytes: FRAME_CACHE_BYTES,
    };
    assert!(matches!(over.check(), Err(RenderBreach::Bytes(_))));
    let three = RenderSet {
        pages: 3,
        ..Default::default()
    };
    assert_eq!(three.check(), Err(RenderBreach::Pages(3)));
}

/// §4.4 `MotionScope`: a surface's foreground spring reports `Motion` (so the frame presents)
/// without the PAGE reading as moving — the host snapshot is not re-taken for it.
#[test]
fn a_modal_foreground_spring_does_not_invalidate_the_host_snapshot() {
    let (mut d, mut rig, _) = booted();
    open_modal(&mut d, &mut rig, Style::Sheet, 16);
    let r = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert!(r.presented, "the appear spring and the surface's own pop report motion");
    assert!(!r.underlay_moving, "…none of it attributed to the page");
}

/// §5.4: a presenting frame's prepare pass leaves the logical-state hash where it was. The
/// dispatcher asserts it on every presenting frame of a debug build; this is the named test.
#[test]
fn prepare_does_not_change_the_logical_state_hash() {
    let (mut d, mut rig, _) = booted();
    // an event frame that presents: `r.state_hash` is taken after the drains and BEFORE prepare
    // and draw; the hash after the frame must be the same number
    let r = d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert!(r.presented, "the key invalidated");
    let pre_prepare = r.state_hash.expect("an event frame hashes");
    assert_eq!(d.state_hash(), pre_prepare);
}

/// The dip lifted off `ui::nav`: over a `PageDip` the op applies at the FLOOR, several frames
/// after the press, while a cut applies at the same commit — and BACK inside the window
/// withdraws the transition with nothing mounted.
#[test]
fn a_page_dip_commits_at_its_floor_and_a_back_inside_the_window_withdraws_it() {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    // Root with nothing mounted yet: the dip runs out from alpha 1 and the floor mounts Home
    let mut mounted_at = None;
    for i in 0..20u32 {
        let r = d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
        if !r.mounted.is_empty() {
            mounted_at = Some(i);
            break;
        }
    }
    let at = mounted_at.expect("Home mounted at the floor");
    assert!(at >= 4, "70 ms out at 16 ms frames: the floor is ~5 frames in, not the press frame (got {at})");
    assert_eq!(d.top_screen().unwrap().name(), "home");
    // let the IN ramp settle (a request mid-ramp fades out from wherever the alpha is — a
    // retarget, whose floor comes sooner)
    for i in 0..20u32 {
        d.frame(&mut rig, tick(400 + i * 16), vec![], vec![], &mut NoTap);
    }
    assert!(!d.nav.tabs.stack.transition.in_flight());
    // a press opens a page: nothing mounts on the press frame
    let r = d.frame(&mut rig, tick(1000), vec![key(Key::Ok, tick(1000))], vec![], &mut NoTap);
    assert!(r.mounted.is_empty());
    assert!(d.nav.tabs.stack.is_pending());
    // a Cancel two frames in withdraws it: the pending transition reverses rather than committing
    let r = d.frame(&mut rig, tick(1016), vec![], vec![], &mut NoTap);
    assert!(r.mounted.is_empty(), "16 ms into a 70 ms fade: not the floor");
    d.request(MachineId::Nav, NavOp::Cancel);
    let r = d.frame(&mut rig, tick(1032), vec![], vec![], &mut NoTap);
    assert!(r.mounted.is_empty());
    assert!(!d.nav.tabs.stack.is_pending(), "withdrawn");
    for i in 0..30u32 {
        let r = d.frame(&mut rig, tick(1048 + i * 16), vec![], vec![], &mut NoTap);
        assert!(r.mounted.is_empty(), "a withdrawn transition never mounts");
    }
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    assert_eq!(d.nav.tabs.stack.page_alpha(), 1.0);
}

/// **`has_pending_navigation` is a question about the PAGE stack**, and [`Navigation::moves_page`]
/// is the one classifier that answers it — the same one [`Navigation::request`] routes by, so the
/// guard and the commit cannot disagree about what a parked op is.
///
/// Its caller is `app::bridge::sync_page`, which mirrors the committed route onto the tree and
/// stands down for a frame whose page op the loop already parked. Reading a parked SURFACE op as
/// one is what stranded the player page under a route that had already left it: `exit_player`
/// parks a `Dismiss` for the panel that was up and flips the route in the same breath.
///
/// [`Navigation::moves_page`]: super::Navigation::moves_page
/// [`Navigation::request`]: super::Navigation::request
#[test]
fn only_a_parked_page_op_counts_as_a_pending_navigation() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    assert!(!d.has_pending_navigation(), "a settled tree has nothing parked");
    let surface = open_modal(&mut d, &mut rig, Style::Compact, 16);
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    assert!(!d.has_pending_navigation(), "presenting a surface does not move the page");
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    d.request(MachineId::Nav, NavOp::Dismiss(surface));
    assert!(!d.has_pending_navigation(), "…and neither does dismissing one");
    d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    let mut ms = 64;
    for op in [
        NavOp::Push(FixtureArg::Page(9)),
        NavOp::Pop,
        NavOp::Root(FixtureArg::Home),
        // a `Dismiss` naming a PAGE entry reaches `NavStack::apply`'s `PopTo` arm, so it is one
        NavOp::Dismiss(home),
        NavOp::PopTo(home),
    ] {
        d.request(MachineId::Nav, op);
        assert!(d.has_pending_navigation(), "a page op is a pending navigation");
        let report = d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
        ms += 16;
        assert!(!d.has_pending_navigation(), "…consumed at the commit");
    }
}
