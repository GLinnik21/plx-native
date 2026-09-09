//! The dispatcher's INPUT path (spec §15.1, §7.3–§7.6): the owner's first refusal, the engine
//! behind it, the press arm by element kind, the hit map's double buffer and its gates, the
//! television keyboard as an owner, and a legacy page for which all of it is inert.

use super::containers::modal::Style;
use super::containers::tabs::StripMember;
use super::dispatch::{Dispatcher, NoTap, STRIP_BASE};
use super::fixture::{booted, events_of, key, tick, FixtureArg, FixtureHost, FixtureRig};
use super::machine::{Edge, FocusKey, InputEvent, InputKind, Key, MachineId, NavOp, Source, StoreOrd, Tick};
use super::Rect;

fn interactive_event(kind: u8, at: Tick) -> super::screen::ScreenEvent<FixtureHost> {
    use super::machine::PressId;
    use super::screen::ScreenEvent;
    match kind {
        0 => ScreenEvent::Input(key(Key::Ok, at)),
        1 => ScreenEvent::Activate(0),
        2 => ScreenEvent::PressHold(PressId(9)),
        3 => ScreenEvent::PressCommit(PressId(9)),
        _ => unreachable!(),
    }
}

fn rejects_inactive_interactive_delivery(kind: u8) {
    use super::machine::{Delivery, Fx};
    let (mut d, mut rig) = boot(FixtureArg::Page(800));
    d.nav.tabs.stack.transition = Box::new(super::containers::transition::Immediate);
    let old = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    let before = rig.store.view.items.len();
    // Positive control: while the page owns input, its sentinel handler really writes a store.
    d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(old),
        Delivery::Screen(interactive_event(kind, tick(8)))));
    d.frame(&mut rig, tick(8), vec![], vec![], &mut NoTap);
    assert_eq!(rig.store.view.items.len(), before + 1);
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(20)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.tabs.stack.depth(), 2);
    assert_ne!(d.nav.top_page().unwrap().inst.as_ref().unwrap().id, old);
    let events = events_of(&d, 0);
    let items = rig.store.view.items.clone();
    let requests = rig.net_requests.len();
    d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(old),
        Delivery::Screen(interactive_event(kind, tick(32)))));
    let report = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(events_of(&d, 0), events, "a covered live instance must not be stepped");
    assert_eq!(rig.store.view.items, items, "no store write may escape the stale handler");
    assert_eq!(rig.net_requests.len(), requests, "no adapter request may escape the stale handler");
    assert_eq!(report.dropped_deliveries, 1);
}

#[test]
fn inactive_input_delivery_never_steps_the_old_owner() { rejects_inactive_interactive_delivery(0); }
#[test]
fn inactive_activate_delivery_never_steps_the_old_owner() { rejects_inactive_interactive_delivery(1); }
#[test]
fn inactive_press_hold_delivery_never_steps_the_old_owner() { rejects_inactive_interactive_delivery(2); }
#[test]
fn inactive_press_commit_delivery_never_steps_the_old_owner() { rejects_inactive_interactive_delivery(3); }

#[test]
fn covered_entries_still_receive_addressed_commands_memory_and_restore_focus() {
    use super::fixture::FixtureMsg;
    use super::machine::{Delivery, Fx};
    use super::screen::{By, ScreenEvent};
    let (mut d, mut rig) = boot(FixtureArg::Page(800));
    let entry = d.nav.top_page().unwrap().id;
    let old = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    d.nav.next_style = Style::Opaque { snapshot: true };
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    for event in [
        ScreenEvent::StoreChanged(StoreOrd(0), 1),
        ScreenEvent::App(FixtureMsg::Store(StoreOrd(0), 7)),
        ScreenEvent::RestoreMemory(()),
        ScreenEvent::Uncover,
        ScreenEvent::FocusMoved { from: None, to: FocusKey { entry, elem: 0 }, by: By::Restore },
    ] {
        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(old), Delivery::Screen(event)));
    }
    let report = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(report.dropped_deliveries, 0);
    let events = events_of(&d, 0);
    assert!(events.contains("\"store_changed\", \"app\", \"restore_memory\", \"uncover\", \"focus_moved\""), "{events}");
}

fn ev(kind: InputKind<u32>, at: Tick) -> InputEvent<u32> {
    InputEvent {
        at,
        source: Source::Script,
        kind,
    }
}

#[test]
fn remembering_a_projected_item_does_not_move_current_focus() {
    use super::machine::{Fx, GroupId};
    let (mut d, mut rig) = boot(FixtureArg::Page(20));
    let page = d.nav.top_page().unwrap();
    let entry = page.id;
    let source = page.inst.as_ref().unwrap().id;
    let focus = d.focus();
    let focus_hash = |d: &Dispatcher<FixtureHost>| {
        let mut canon = super::machine::Canon::new();
        d.input.engine.write_with(&mut canon, &|elem, canon| { canon.u32(*elem); });
        canon.finish()
    };
    let before = focus_hash(&d);
    d.emit(MachineId::Instance(source), Fx::Remember { group: GroupId(1), elem: 2 });
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    assert_eq!(d.focus(), focus);
    assert!(d.input.engine.remembered_for(entry).contains(&(GroupId(1), 2)));
    assert_ne!(focus_hash(&d), before, "remembered selection participates in logical state");
}

#[test]
fn remembering_rejects_foreign_groups_and_non_instance_sources() {
    use super::machine::{Fx, GroupId};
    let (mut d, mut rig) = boot(FixtureArg::Page(20));
    let page = d.nav.top_page().unwrap();
    let entry = page.id;
    let source = page.inst.as_ref().unwrap().id;
    let before = d.input.engine.remembered_for(entry);
    d.emit(MachineId::Nav, Fx::Remember { group: GroupId(1), elem: 2 });
    d.emit(MachineId::Instance(source), Fx::Remember { group: GroupId(99), elem: 2 });
    d.emit(MachineId::Instance(source), Fx::Remember { group: GroupId(1), elem: 9000 });
    let report = d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    assert_eq!(d.input.engine.remembered_for(entry), before);
    assert_eq!(report.dropped_deliveries, 3);
}

#[test]
fn a_covered_master_cannot_rewrite_either_pages_remembered_cursor() {
    use super::machine::{Fx, GroupId};
    let (mut d, mut rig) = boot(FixtureArg::Page(20));
    d.nav.tabs.stack.transition = Box::new(super::containers::transition::Immediate);
    let old = d.nav.top_page().unwrap();
    let old_entry = old.id;
    let old_source = old.inst.as_ref().unwrap().id;
    let remembered = d.input.engine.remembered_for(old_entry);
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(21)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let new = d.nav.top_page().unwrap();
    let new_entry = new.id;
    let new_source = new.inst.as_ref().unwrap().id;
    let current = d.focus();
    d.emit(MachineId::Instance(old_source), Fx::Remember { group: GroupId(1), elem: 2 });
    d.emit(MachineId::Instance(new_source), Fx::Remember { group: GroupId(1), elem: 1 });
    let report = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(report.dropped_deliveries, 1);
    assert_eq!(d.focus(), current);
    assert_eq!(d.input.engine.remembered_for(old_entry), remembered);
    assert!(d.input.engine.remembered_for(new_entry).contains(&(GroupId(1), 1)));
}

#[test]
fn remembering_a_strip_item_requires_an_actual_member_not_just_the_reserved_range() {
    use super::machine::Fx;
    let (mut d, mut rig) = boot(FixtureArg::Home);
    d.nav.tabs.strip = vec![StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0))];
    let page = d.nav.top_page().unwrap();
    let entry = page.id;
    let source = page.inst.as_ref().unwrap().id;
    let before = d.input.engine.remembered_for(entry);
    d.emit(MachineId::Instance(source), Fx::Remember {
        group: super::containers::tabs::STRIP, elem: STRIP_BASE + 99,
    });
    let report = d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    assert_eq!(d.input.engine.remembered_for(entry), before);
    assert_eq!(report.dropped_deliveries, 1);
}

fn click(x: f32, y: f32, at: Tick) -> InputEvent<u32> {
    ev(InputKind::Click { x, y, hit: None }, at)
}

fn pointer(x: f32, y: f32, at: Tick) -> InputEvent<u32> {
    ev(InputKind::Pointer { x, y, hit: None }, at)
}

fn key_up(k: Key, at: Tick) -> InputEvent<u32> {
    ev(
        InputKind::Key {
            key: k,
            sym: 0,
            wcode: 0,
            edge: Edge::Up,
            at_edge: false,
        },
        at,
    )
}

#[test]
fn fifo_same_frame_card_tap_commits_once_and_never_becomes_a_hold() {
    let (mut d, mut rig) = boot(FixtureArg::Page(700));
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16)), key_up(Key::Ok, tick(16))], vec![], &mut NoTap);
    for ms in (32..=800).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    let events = events_of(&d, 0);
    assert!(!events.contains("\"press_hold\""), "a FIFO tap has a release, so it cannot open the hold menu: {events}");
    assert_eq!(events.matches("\"press_commit\"").count(), 1, "the tap commits exactly once: {events}");
}

#[test]
fn fifo_same_frame_control_tap_does_not_wait_for_the_lost_key_up_cap() {
    let (mut d, mut rig) = boot(FixtureArg::Page(600));
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16)), key_up(Key::Ok, tick(16))], vec![], &mut NoTap);
    for ms in (32..=320).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(events_of(&d, 0).matches("\"press_commit\"").count(), 1);
    // The visual bounce may still be live after commitment; the logical arm must be consumed.
    assert!(d.input.arm.is_none());
}

#[test]
fn a_pointer_hold_on_a_control_commits_on_release_but_a_card_keeps_its_hold_gesture() {
    for (page, holdable) in [(600, false), (700, true)] {
        let (mut d, mut rig) = boot(FixtureArg::Page(page));
        d.draw(&mut rig, true);
        let rect = d.input.hit.front()[0].rect;
        d.frame(&mut rig, tick(16), vec![click(rect.cx(), rect.cy(), tick(16))], vec![], &mut NoTap);
        assert_eq!(d.input.arm.unwrap().holdable, holdable, "pointer and keyboard must use the same element-kind policy");
        for ms in (32..=640).step_by(16) { d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap); }
        d.frame(&mut rig, tick(656), vec![key_up(Key::Ok, tick(656))], vec![], &mut NoTap);
        for ms in (672..=960).step_by(16) { d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap); }
        let events = events_of(&d, 0);
        assert_eq!(events.matches("\"press_hold\"").count(), usize::from(holdable));
        assert_eq!(events.matches("\"press_commit\"").count(), usize::from(!holdable));
    }
}

#[test]
fn a_card_held_with_repeat_beats_still_delivers_one_hold_and_no_tap() {
    let (mut d, mut rig) = boot(FixtureArg::Page(700));
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    for ms in (32..=640).step_by(16) {
        let inputs = if ms % 64 == 0 { vec![ev(InputKind::Key {
            key: Key::Ok, sym: 0, wcode: 0, edge: Edge::Repeat, at_edge: false,
        }, tick(ms))] } else { vec![] };
        d.frame(&mut rig, tick(ms), inputs, vec![], &mut NoTap);
    }
    d.frame(&mut rig, tick(656), vec![key_up(Key::Ok, tick(656))], vec![], &mut NoTap);
    for ms in (672..=960).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    let events = events_of(&d, 0);
    assert_eq!(events.matches("\"press_hold\"").count(), 1, "{events}");
    assert!(!events.contains("\"press_commit\""), "a real hold does not also activate the card");
}

/// Boot straight into a page of the given argument (a `Root` at frame 1).
fn boot(arg: FixtureArg) -> (Dispatcher<FixtureHost>, FixtureRig) {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(arg));
    d.frame(&mut rig, tick(0), vec![], vec![], &mut NoTap);
    (d, rig)
}

fn elem(d: &Dispatcher<FixtureHost>) -> Option<u32> {
    d.focus().map(|k| k.elem)
}

/// §7.3 step 1: a control that handles a direction keeps it from the engine — the modal's
/// slider-shaped arm answers `Handled::Yes` and focus does not move.
#[test]
fn a_control_that_handles_a_direction_keeps_it_from_the_engine() {
    let (mut d, mut rig, _) = booted();
    d.nav.next_style = Style::Compact;
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let before = d.focus();
    assert!(before.is_some(), "Enter seated the surface's one control");
    d.frame(&mut rig, tick(32), vec![key(Key::Down, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.focus(), before, "the engine was never asked");
    let id = d.nav.modals.top().unwrap().entry.id;
    let mut s = String::new();
    d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
    assert!(s.contains("keys=10"), "the control took it: {s}");
}

/// §6.2: the strip is the CONTAINER's group, contributed above the page's — UP from the row
/// lands on the nearest pill — and the page gates it: a page snapped to its grid has no strip
/// to reach.
#[test]
fn the_strip_is_the_containers_group_and_the_page_gates_it() {
    let (mut d, mut rig, _) = booted();
    d.nav.tabs.strip = vec![
        StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0)),
        StripMember::new(STRIP_BASE + 1, Rect::new(280.0, 10.0, 160.0, 48.0)),
        StripMember::new(STRIP_BASE + 2, Rect::new(460.0, 10.0, 160.0, 48.0)),
    ];
    let home = d.nav.top_page().unwrap().id;
    d.set_focus(Some(FocusKey { entry: home, elem: 2 })); // the tile at x 400..580
    d.frame(&mut rig, tick(16), vec![key(Key::Up, tick(16))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(STRIP_BASE + 2), "the pill over the tile");
    d.frame(&mut rig, tick(32), vec![key(Key::Left, tick(32))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(STRIP_BASE + 1), "LEFT walks the strip");
    d.frame(&mut rig, tick(48), vec![key(Key::Down, tick(48))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(1), "DOWN returns to the row under the pill");
    let ev = events_of(&d, 0);
    assert!(ev.matches("\"focus_moved\"").count() >= 3, "the owner heard every move: {ev}");

    // the gate: a snapped page contributes no strip
    let (mut d, mut rig) = boot(FixtureArg::Snapped);
    d.nav.tabs.strip = vec![StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0))];
    let e = d.nav.top_page().unwrap().id;
    d.set_focus(Some(FocusKey { entry: e, elem: 0 }));
    d.frame(&mut rig, tick(16), vec![key(Key::Up, tick(16))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(0), "nothing above a snapped grid");
}

#[test]
fn the_home_strip_walks_both_endpoints_for_every_type_composition() {
    for types in [vec![], vec![1], vec![2], vec![1, 2]] {
        let (mut d, mut rig) = boot(FixtureArg::Home);
        let entry = d.nav.top_page().unwrap().id;
        let mut keys = vec![STRIP_BASE + 4, STRIP_BASE]; // chip, Home
        keys.extend(types.into_iter().map(|id| STRIP_BASE + id));
        keys.push(STRIP_BASE + 3); // Search is always the last destination
        d.nav.tabs.strip = keys.iter().enumerate().map(|(i, &key)|
            StripMember::new(key, Rect::new(100.0 + i as f32 * 180.0, 10.0, 160.0, 48.0))).collect();
        d.set_focus(Some(FocusKey { entry, elem: STRIP_BASE }));
        let mut ms = 0;
        let mut walk = |dir, expected| {
            ms += 16;
            d.frame(&mut rig, tick(ms), vec![key(dir, tick(ms))], vec![], &mut NoTap);
            assert_eq!(d.focus(), Some(FocusKey { entry, elem: expected }));
        };
        walk(Key::Left, keys[0]);
        walk(Key::Left, keys[0]);
        for &expected in &keys[1..] { walk(Key::Right, expected); }
        walk(Key::Right, *keys.last().unwrap());
        for &expected in keys[..keys.len() - 1].iter().rev() { walk(Key::Left, expected); }
        walk(Key::Left, keys[0]);
    }
}

#[test]
fn removing_a_preceding_tab_keeps_the_same_destination_focused() {
    let (mut d, mut rig) = boot(FixtureArg::Home);
    d.nav.tabs.strip = vec![
        StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0)),
        StripMember::new(STRIP_BASE + 1, Rect::new(280.0, 10.0, 160.0, 48.0)),
        StripMember::new(STRIP_BASE + 2, Rect::new(460.0, 10.0, 160.0, 48.0)),
    ];
    let home = d.nav.top_page().unwrap().id;
    let search = FocusKey { entry: home, elem: STRIP_BASE + 2 };
    d.set_focus(Some(search));
    // The middle destination disappears (its last favourite library was removed).
    d.nav.tabs.strip.remove(1);
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    assert_eq!(d.focus(), Some(search), "position changed, destination identity did not");
    d.frame(&mut rig, tick(32), vec![key(Key::Left, tick(32))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(STRIP_BASE), "LEFT walks surviving visual order, not key arithmetic");
    d.frame(&mut rig, tick(48), vec![key(Key::Right, tick(48))], vec![], &mut NoTap);
    assert_eq!(d.focus(), Some(search));
}

#[test]
fn strip_identity_is_logical_but_its_animation_geometry_is_not() {
    let (mut d, _) = boot(FixtureArg::Home);
    d.nav.tabs.strip = vec![StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0))];
    let before = d.state_hash();
    d.nav.tabs.strip[0].drawn.x += 20.0;
    d.nav.tabs.strip[0].target.x += 40.0;
    assert_eq!(d.state_hash(), before);
    d.nav.tabs.strip[0].elem = STRIP_BASE + 3;
    assert_ne!(d.state_hash(), before, "different destinations must not produce an identical recording state");
}

#[test]
fn strip_pointer_stops_use_drawn_geometry_and_stable_keys() {
    let (mut d, mut rig) = boot(FixtureArg::Home);
    let entry = d.nav.top_page().unwrap().id;
    let drawn = Rect::new(100.0, 10.0, 160.0, 48.0);
    let target = Rect::new(140.0, 10.0, 160.0, 48.0);
    let clip = Rect::new(110.0, 0.0, 150.0, 100.0);
    d.nav.tabs.strip = vec![StripMember { elem: STRIP_BASE + 3, drawn, target, clip }];
    d.draw(&mut rig, true);
    let stop = d.input.hit.front().iter().find(|s| s.key.elem == STRIP_BASE + 3)
        .expect("the container must register its own drawn strip in the hit map");
    assert_eq!(stop.key, FocusKey { entry, elem: STRIP_BASE + 3 });
    let bounds = |r: Rect| (r.x, r.y, r.w, r.h);
    assert_eq!(bounds(stop.rect), bounds(drawn));
    assert_eq!(bounds(stop.rest_rect), bounds(target));
    assert_eq!(bounds(stop.clip), bounds(clip));
}

#[test]
fn a_folded_page_keeps_visible_strip_clicks_without_hover_reseating() {
    let (mut d, mut rig) = boot(FixtureArg::Snapped);
    d.nav.tabs.strip = vec![StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0))];
    let before = d.focus();
    d.draw(&mut rig, true);
    assert!(d.input.hit.front().iter().any(|s| s.key.elem == STRIP_BASE),
        "a visible strip is still clickable when directional entry is disabled");
    d.frame(&mut rig, tick(16), vec![pointer(150.0, 30.0, tick(16))], vec![], &mut NoTap);
    assert_eq!(d.focus(), before, "hovering the strip cannot fold the page");
    d.frame(&mut rig, tick(32), vec![click(150.0, 30.0, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.focus(), before);
    assert!(events_of(&d, 0).contains("\"activate\""));
}

#[test]
fn a_covered_strip_cannot_focus_or_activate_the_modal_and_counts_as_an_outside_click() {
    let (mut d, mut rig) = boot(FixtureArg::Home);
    let page = d.nav.top_page().unwrap().id;
    d.nav.tabs.strip = vec![StripMember::new(STRIP_BASE, Rect::new(100.0, 10.0, 160.0, 48.0))];
    d.nav.next_style = Style::Compact;
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    for ms in (32..=1600).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.nav.modals.top().unwrap().phase, super::containers::modal::Phase::Open);
    d.draw(&mut rig, true);
    let modal = d.nav.modals.top().unwrap().entry.id;
    let focus = d.focus();
    assert_eq!(focus.unwrap().entry, modal);
    d.frame(&mut rig, tick(1616), vec![click(150.0, 30.0, tick(1616))], vec![], &mut NoTap);
    let modal_focus = d.input.engine.current(super::machine::InputOwner::Entry(modal));
    assert_ne!(modal_focus.map(|k| k.entry), Some(page), "the modal must not adopt a covered page's key");
    assert_eq!(d.nav.modals.top().unwrap().phase, super::containers::modal::Phase::Closing,
        "a click outside the compact panel dismisses it even over a covered tab");
}

/// §7.3 step 6: a store landing that shrinks the row makes the owner's `reconcile` answer a
/// different key — delivered as `FocusMoved{Reconcile}` after the notice and before the draw.
#[test]
fn reconcile_runs_after_a_landing_and_before_draw() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    d.set_focus(Some(FocusKey { entry: home, elem: 2 }));
    rig.store.add(1);
    d.store_changed(StoreOrd(0), 1);
    let r = d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    assert!(r.presented, "the landing invalidated");
    assert_eq!(elem(&d), Some(0), "clamped to the one item that landed");
    let ev = events_of(&d, 0);
    let sc = ev.rfind("\"store_changed\"").unwrap();
    let fm = ev.rfind("\"focus_moved\"").unwrap();
    assert!(sc < fm, "reconcile after the landing: {ev}");
    assert_eq!(d.last_stops().len(), 1, "…and before the draw, which drew the one tile");
}

/// §7.4: a pointer press is cancelled when the pointer's hit leaves the stop that armed it.
#[test]
fn a_pointer_press_is_cancelled_when_the_hit_leaves_its_arm() {
    let (mut d, mut rig, _) = booted();
    assert_eq!(d.last_stops().len(), 3, "the map has the row");
    d.frame(&mut rig, tick(16), vec![click(90.0, 190.0, tick(16))], vec![], &mut NoTap);
    let arm = d.input.arm.expect("a click on a card arms a press");
    assert_eq!(arm.key.elem, 0);
    assert!(d.input.press.is_live());
    d.frame(&mut rig, tick(32), vec![pointer(290.0, 190.0, tick(32))], vec![], &mut NoTap);
    assert!(d.input.arm.is_none(), "the hit left the arm: cancelled");
    assert!(!d.input.press.is_live());
    assert_eq!(elem(&d), Some(1), "…while hover parked focus on the new tile");
}

/// §7.4: with no key-up ever arriving, the press machine's cap resolves the hold from the Tick
/// and the commit is delivered to the arming owner.
#[test]
fn press_identity_and_queued_press_identity_are_canonical_state() {
    use super::machine::{Canon, Delivery, Fx, PressArm, PressFrom, PressId};
    let (mut d, _) = boot(FixtureArg::Page(600));
    let key = d.focus().unwrap();
    let owner = MachineId::Instance(d.nav.instance_of(key.entry).unwrap());
    d.input.arm(PressArm { key, from: PressFrom::Key, holdable: true }, owner, 16);
    let hash = |input: &super::input::InputMachine<u32>| {
        let mut c = Canon::new();
        input.write_with(&mut c, &|key, c| { c.u32(*key); });
        c.finish()
    };
    let baseline = hash(&d.input);
    let original = d.input.arm.unwrap();
    for variant in 0..5 {
        let mut arm = original;
        match variant {
            0 => arm.key.elem += 1,
            1 => arm.key.entry.0 += 1,
            2 => arm.owner = MachineId::Nav,
            3 => arm.from = PressFrom::Pointer,
            4 => arm.held_delivered = true,
            _ => unreachable!(),
        }
        d.input.arm = Some(arm);
        assert_ne!(hash(&d.input), baseline, "arm field {variant} is omitted");
    }

    let mut hashes = Vec::new();
    for elem in [0, 1] {
        let (mut d, _) = boot(FixtureArg::Page(600));
        d.emit(MachineId::Input, Fx::Deliver(owner, Delivery::Press {
            id: PressId(1), key: FocusKey { entry: key.entry, elem }, held: false,
        }));
        hashes.push(d.state_hash());
    }
    assert_ne!(hashes[0], hashes[1], "same queue depth does not mean same pending activation");
}

#[test]
fn a_budget_carried_press_keeps_its_original_item_identity() {
    use super::machine::{Delivery, Fx};
    use super::screen::ScreenEvent;
    for (held, move_focus) in [(false, false), (false, true), (true, false), (true, true)] {
        let (mut d, mut rig) = boot(FixtureArg::Page(if held { 700 } else { 600 }));
        let original = d.focus().unwrap();
        let instance = d.nav.instance_of(original.entry).unwrap();
        d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
        if !held {
            d.frame(&mut rig, tick(32), vec![key_up(Key::Ok, tick(32))], vec![], &mut NoTap);
            d.frame(&mut rig, tick(112), vec![], vec![], &mut NoTap);
        }
        for _ in 0..super::dispatch::MAX_STEPS_PRE + super::dispatch::MAX_STEPS_POST {
            d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                Delivery::Screen(ScreenEvent::Uncover)));
        }
        let now = if held { 600 } else { 240 };
        let event = if held { "\"press_hold\"" } else { "\"press_commit\"" };
        let report = d.frame(&mut rig, tick(now), vec![], vec![], &mut NoTap);
        assert!(report.carried > 0, "the test must exercise the real carry budget");
        assert_eq!(d.input.arm.is_some(), held, "commit retires its arm; a held press stays armed");
        assert!(!events_of(&d, 0).contains(event));
        if move_focus {
            d.set_focus_in(Some(FocusKey { entry: original.entry, elem: 1 }), Some(super::machine::GroupId(1)));
        }
        d.frame(&mut rig, tick(now + 16), vec![], vec![], &mut NoTap);
        assert_eq!(events_of(&d, 0).matches(event).count(), usize::from(!move_focus),
            "a queued commit may not be retargeted to the new cursor");
    }
}

#[test]
fn a_press_commit_fires_from_tick_with_no_key_up() {
    let (mut d, mut rig) = boot(FixtureArg::Page(600)); // a Control row: not holdable
    assert_eq!(elem(&d), Some(0), "Enter seated the first control");
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert!(d.input.arm.is_some(), "OK on a control armed a non-holdable press");
    assert!(!d.input.arm.unwrap().holdable);
    let mut committed_at = None;
    for i in 2..120u32 {
        d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
        if events_of(&d, 0).contains("\"press_commit\"") {
            committed_at = Some(i * 16);
            break;
        }
    }
    let at = committed_at.expect("the commit fired from a Tick");
    assert!(at >= 1000, "after the 1000 ms hold cap, not before ({at} ms)");
    assert!(d.input.arm.is_none());
}

/// §7.4: a `Bare` element activates on the DOWN edge and never arms a press.
#[test]
fn a_bare_element_activates_on_the_down_edge() {
    let (mut d, mut rig) = boot(FixtureArg::Page(500)); // a Bare row
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    let ev = events_of(&d, 0);
    assert!(ev.contains("\"activate\""), "{ev}");
    assert!(d.input.arm.is_none(), "nothing armed");
    assert!(!d.input.press.is_active());
    // and a Card row arms instead of activating
    let (mut d, mut rig, _) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Right, tick(16))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(1), "Enter seated tile 0; RIGHT walked to 1");
    let before = events_of(&d, 0);
    // Home handles OK itself (it opens a page); Page(1) does too — use a card page that does not:
    // the fixture's Home consumes OK, so grade the arm on the pointer path's twin instead
    d.frame(&mut rig, tick(32), vec![click(90.0, 190.0, tick(32))], vec![], &mut NoTap);
    assert!(d.input.arm.map_or(false, |a| a.holdable), "a card arms a HOLDABLE press");
    assert!(!events_of(&d, 0).replace(&before, "").contains("\"activate\""));
    d.frame(&mut rig, tick(48), vec![key_up(Key::Ok, tick(48))], vec![], &mut NoTap);
}

/// §7.3 step 7: while the television's keyboard is up it is the input owner — keys still reach
/// the page (the field consumes them) but the engine is never consulted.
#[test]
fn the_system_keyboard_is_an_input_owner() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    d.set_focus(Some(FocusKey { entry: home, elem: 1 }));
    d.frame(&mut rig, tick(16), vec![ev(InputKind::SystemKeyboard(true), tick(16))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(32), vec![key(Key::Right, tick(32))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(1), "the keyboard owns the direction");
    let ev1 = events_of(&d, 0);
    assert!(ev1.contains("\"input\""), "…the page still heard it: {ev1}");
    d.frame(&mut rig, tick(48), vec![ev(InputKind::SystemKeyboard(false), tick(48))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(64), vec![key(Key::Right, tick(64))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(2), "keyboard down: the engine answers again");
}

#[test]
fn keyboard_ownership_changes_take_effect_in_input_delivery_order() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    d.set_focus(Some(FocusKey { entry: home, elem: 1 }));
    d.frame(&mut rig, tick(16), vec![
        ev(InputKind::SystemKeyboard(true), tick(16)),
        key(Key::Right, tick(16)),
        ev(InputKind::SystemKeyboard(false), tick(16)),
    ], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(1), "the later dismissal cannot give the earlier direction to the page engine");
    d.frame(&mut rig, tick(32), vec![key(Key::Right, tick(32))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(2));
    d.frame(&mut rig, tick(48), vec![
        key(Key::Left, tick(48)),
        ev(InputKind::SystemKeyboard(true), tick(48)),
        key(Key::Left, tick(48)),
    ], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(1), "opening the keyboard cannot swallow an earlier page direction");
}

#[test]
fn keyboard_context_keeps_entry_focus_but_does_not_fall_through_back() {
    let (mut d, mut rig) = boot(FixtureArg::Page(801));
    let report = d.frame(&mut rig, tick(16), vec![
        ev(InputKind::SystemKeyboard(true), tick(16)),
        key(Key::Back, tick(16)),
        ev(InputKind::SystemKeyboard(false), tick(16)),
    ], vec![], &mut NoTap);
    assert!(!report.back_at_root, "a system-owned BACK must not escape through the page container");
    let events = events_of(&d, 0);
    assert!(events.contains(concat!(
        "\"input\", \"system_owner\", \"has_focus\", ",
        "\"input\", \"system_owner\", \"has_focus\", ",
        "\"input\", \"entry_owner\", \"has_focus\"")), "{events}");
    let report = d.frame(&mut rig, tick(32), vec![key(Key::Back, tick(32))], vec![], &mut NoTap);
    assert!(report.back_at_root, "after dismissal BACK belongs to the page again");
}

#[test]
fn keyboard_edges_cancel_page_gestures_and_never_arm_system_owned_ok() {
    for already_armed in [false, true] {
        let (mut d, mut rig) = boot(FixtureArg::Page(700));
        if already_armed {
            d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
            assert!(d.input.arm.is_some());
        }
        d.frame(&mut rig, tick(32), vec![
            ev(InputKind::SystemKeyboard(true), tick(32)),
            key(Key::Ok, tick(32)),
            ev(InputKind::SystemKeyboard(false), tick(32)),
        ], vec![], &mut NoTap);
        assert!(d.input.arm.is_none());
        d.frame(&mut rig, tick(48), vec![key_up(Key::Ok, tick(48))], vec![], &mut NoTap);
        d.frame(&mut rig, tick(1200), vec![], vec![], &mut NoTap);
        let events = events_of(&d, 0);
        assert!(!events.contains("\"press_hold\"") && !events.contains("\"press_commit\""), "{events}");
    }
}

#[test]
fn a_keyboard_round_trip_revokes_press_results_already_queued_for_this_frame() {
    for handoff in [false, true] {
        let (mut d, mut rig) = boot(FixtureArg::Page(700));
        d.draw(&mut rig, true);
        let rect = d.input.hit.front()[0].rect;
        d.frame(&mut rig, tick(16), vec![click(rect.cx(), rect.cy(), tick(16))], vec![], &mut NoTap);
        assert!(d.input.arm.is_some());
        // The hold is evaluated before the drain; its delivery waits behind these edges.
        let inputs = if handoff { vec![
            ev(InputKind::SystemKeyboard(true), tick(528)),
            ev(InputKind::SystemKeyboard(false), tick(528)),
        ] } else { vec![] };
        d.frame(&mut rig, tick(528), inputs, vec![], &mut NoTap);
        let events = events_of(&d, 0);
        assert_eq!(events.matches("\"press_hold\"").count(), usize::from(!handoff),
            "the same page is back, but the gesture was revoked: {events}");
    }
}

#[test]
fn whole_text_commits_and_keyboard_edges_round_trip_in_order() {
    use super::fixture::{fixture_state_fp, FixtureCodec, FixtureInit, RecTap};
    use super::machine::{LogicalState, TextEdit};
    use super::rec::{Header, MemSink, Recording, Writer};
    use super::replay::{Codec, run_resolve, run_targets};
    use serde_json::json;
    let long = "synthetic whole commit longer than an SDL text array 🙂";
    let edits = [
        TextEdit::Commit("с".into()), TextEdit::Commit("у".into()),
        TextEdit::Commit("б".into()), TextEdit::Commit("суббота ".into()),
        TextEdit::Left, TextEdit::Right, TextEdit::Backspace, TextEdit::Clear,
        TextEdit::Commit(long.into()),
    ];
    let mut inputs = vec![ev(InputKind::SystemKeyboard(true), tick(16))];
    inputs.extend(edits.iter().cloned().map(|edit| ev(InputKind::Text(edit), tick(16))));
    inputs.push(ev(InputKind::SystemKeyboard(false), tick(16)));
    let sink = MemSink::default();
    let segments = sink.segments.clone();
    let init = FixtureInit { seed: 1 };
    let header = Header::new(fixture_state_fp(), &init);
    let mut tap = RecTap { w: Writer::open(Box::new(sink), &header, 0).unwrap() };
    let mut d = Dispatcher::<FixtureHost>::new();
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(801)));
    d.frame(&mut rig, tick(0), vec![], vec![], &mut tap);
    d.frame(&mut rig, tick(16), inputs, vec![], &mut tap);
    assert_eq!(events_of(&d, 0).matches("\"system_owner\"").count(), edits.len() + 1);
    tap.w.finish();
    let manifest = json!({"schema": super::rec::SCHEMA, "state_fp": fixture_state_fp(),
        "init": {"probe": "seed=1", "hash": init.hash()}}).to_string();
    let segments = segments.borrow();
    let slices: Vec<&[u8]> = segments.iter().map(Vec::as_slice).collect();
    let rec = Recording::parse(&manifest, &slices, fixture_state_fp()).unwrap();
    let decoded: Vec<_> = rec.frames[1].inputs.iter()
        .map(|v| FixtureCodec.decode_input(v).unwrap()).collect();
    assert_eq!(decoded.len(), edits.len() + 2, "one record per whole input, never per character");
    assert!(matches!(decoded.first().unwrap().kind, InputKind::SystemKeyboard(true)));
    assert!(matches!(decoded.last().unwrap().kind, InputKind::SystemKeyboard(false)));
    let mut buffer = super::text_buffer::TextBuffer::new(String::new(), 0);
    for (input, expected) in decoded[1..decoded.len()-1].iter().zip(&edits) {
        let InputKind::Text(edit) = &input.kind else { panic!("text event changed kind") };
        assert_eq!(edit, expected, "commit bytes and edit boundaries survive recording");
        buffer.edit(edit);
    }
    assert_eq!(buffer.text(), long);
    assert_eq!(buffer.caret(), long.len());
    for resolve in [false, true] {
        let mut d = Dispatcher::<FixtureHost>::new();
        let mut rig = FixtureRig::new();
        d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(801)));
        let report = if resolve { run_resolve(&rec, &FixtureCodec, &mut d, &mut rig, &|| None) }
            else { run_targets(&rec, &FixtureCodec, &mut d, &mut rig, &|| None) };
        assert!(report.is_clean(), "{:?}", report.safe_lines());
    }
}

#[test]
fn pending_input_hash_distinguishes_text_and_ownership_edges_not_arc_addresses() {
    use super::machine::{Delivery, Fx, TextEdit};
    let hash = |kind| {
        let (mut d, _) = boot(FixtureArg::Page(801));
        let target = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
        d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(target),
            Delivery::Screen(super::screen::ScreenEvent::Input(ev(kind, tick(16))))));
        d.state_hash()
    };
    let text = |s: &str| InputKind::Text(TextEdit::Commit(s.into()));
    assert_eq!(hash(text("alpha")), hash(text("alpha")));
    assert_ne!(hash(text("alpha")), hash(text("beta")));
    assert_ne!(hash(text("")), hash(InputKind::Text(TextEdit::Clear)));
    assert_ne!(hash(InputKind::Text(TextEdit::Left)), hash(InputKind::Text(TextEdit::Right)));
    assert_ne!(hash(InputKind::SystemKeyboard(true)), hash(InputKind::SystemKeyboard(false)));
}

/// §7.6: a `LegacyPage` declares `FocusSource::Legacy`/`HitSource::Legacy` — the engine, the
/// map and `on_miss` are INERT for it; its own ladders stay the single writer.
#[test]
fn a_legacy_page_never_consults_the_map_or_the_engine() {
    let (mut d, mut rig) = boot(FixtureArg::Legacy);
    assert!(d.focus().is_none(), "no Enter seating");
    assert!(d.last_stops().is_empty(), "the draw registered nothing in the map");
    d.frame(&mut rig, tick(16), vec![key(Key::Right, tick(16))], vec![], &mut NoTap);
    assert!(d.focus().is_none(), "a direction moved nothing");
    d.nav.next_style = Style::Compact;
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    // a click beside everything on a legacy host: the map is inert, so it is no MISS either
    d.frame(&mut rig, tick(48), vec![click(1800.0, 1000.0, tick(48))], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, super::containers::modal::Phase::Opening, "not dismissed by a miss the map never saw");
    assert!(d.input.arm.is_none());
}

/// §5.5 `--resolve`: a recording whose focus record was tampered at one frame produces exactly
/// one focus divergence, and replay continues FROM THE RECORDING (the tampered key is what the
/// next frame starts from); `--targets` takes the same recording silently.
#[test]
fn resolve_mode_reports_every_mismatch_and_continues_from_the_recording() {
    use super::fixture::{fixture_state_fp, FixtureCodec, FixtureInit, RecTap};
    use super::machine::LogicalState;
    use super::rec::{Header, MemSink, Recording, Writer};
    use super::replay::{run_resolve, run_targets};
    use serde_json::json;

    // record: boot, RIGHT, RIGHT on Home (the engine walks the row 0 → 1 → 2)
    let sink = MemSink::default();
    let segs = sink.segments.clone();
    let header = Header::new(fixture_state_fp(), &FixtureInit { seed: 1 });
    let w = Writer::open(Box::new(sink), &header, 0).unwrap();
    let mut tap = RecTap { w };
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    d.frame(&mut rig, tick(0), vec![], vec![], &mut tap);
    d.frame(&mut rig, tick(16), vec![key(Key::Right, tick(16))], vec![], &mut tap);
    d.frame(&mut rig, tick(32), vec![key(Key::Right, tick(32))], vec![], &mut tap);
    d.frame(&mut rig, tick(48), vec![], vec![], &mut tap);
    assert_eq!(elem(&d), Some(2));
    tap.w.finish();
    let manifest = serde_json::to_string(&json!({
        "schema": super::rec::SCHEMA, "state_fp": fixture_state_fp(),
        "init": {"probe": "seed=1", "hash": FixtureInit { seed: 1 }.hash()}
    }))
    .unwrap();
    let s = segs.borrow();
    let refs: Vec<&[u8]> = s.iter().map(|v| v.as_slice()).collect();
    let mut rec = Recording::parse(&manifest, &refs, fixture_state_fp()).unwrap();
    assert_eq!(rec.frames[2].focus, Some(Some((1, 2, Some(1)))), "the recording carries the engine's answer");

    // a clean replay in both modes
    let replay = |rec: &Recording, resolve: bool| {
        let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
        let mut rig = FixtureRig::new();
        d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
        if resolve {
            run_resolve(rec, &FixtureCodec, &mut d, &mut rig, &|| None)
        } else {
            run_targets(rec, &FixtureCodec, &mut d, &mut rig, &|| None)
        }
    };
    assert!(replay(&rec, true).is_clean());
    assert!(replay(&rec, false).is_clean());

    // tamper frame 2's resolution: the engine says 1, the recording now says 0
    rec.frames[1].focus = Some(Some((1, 0, Some(1))));
    let report = replay(&rec, true);
    // frame 2: the engine's 1 against the recorded 0 — one line, and replay CONTINUES FROM THE
    // RECORDING; frame 3: from the recorded 0 the engine's RIGHT lands on 1, the recording says
    // 2 — a second pointwise line; frame 4 (no input) agrees again. Never an avalanche.
    let frames: Vec<u64> = report.focus_diffs.iter().map(|d| d.0).collect();
    assert_eq!(frames, vec![2, 3], "{:?}", report.safe_lines());
    assert_eq!(report.frames, rec.frames.len() as u64, "replay continued");
    assert!(report.safe_lines().iter().any(|l| l.starts_with("focus f=2 recorded=Some((1, 0")), "{:?}", report.safe_lines());
    let silent = replay(&rec, false);
    assert!(silent.focus_diffs.is_empty(), "targets mode does not grade the engine");
}
