//! The dispatcher's INPUT path (spec §15.1, §7.3–§7.6): the owner's first refusal, the engine
//! behind it, the press arm by element kind, the hit map's double buffer and its gates, the
//! television keyboard as an owner, and a legacy page for which all of it is inert.

use super::containers::modal::Style;
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
    d.nav.tabs.pill_rects = vec![
        Rect::new(100.0, 10.0, 160.0, 48.0),
        Rect::new(280.0, 10.0, 160.0, 48.0),
        Rect::new(460.0, 10.0, 160.0, 48.0),
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
    d.nav.tabs.pill_rects = vec![Rect::new(100.0, 10.0, 160.0, 48.0)];
    let e = d.nav.top_page().unwrap().id;
    d.set_focus(Some(FocusKey { entry: e, elem: 0 }));
    d.frame(&mut rig, tick(16), vec![key(Key::Up, tick(16))], vec![], &mut NoTap);
    assert_eq!(elem(&d), Some(0), "nothing above a snapped grid");
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
