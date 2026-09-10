/// **A Detail page's own panel PARKS across a push and comes back the same instance.**
///
/// The property that makes a page-owned surface page-owned (spec §6.2, `Navigation.covered_modals`):
/// present a panel on Detail, push Person over it, pop back, and the panel is still there — same
/// `InstanceId`, same cursor, same rows — rather than a fresh mount or nothing at all.
///
/// **Written against the broken behaviour first, and simulated rather than historical.** The legacy
/// panel was a module-level `static mut POP`, so this cannot be compiled against it at all: `POP`
/// belonged to the process, not to the page, and the question "which page's panel is this" had no
/// answer to assert on. The red was produced by narrowing `Navigation::commit`'s park branch to do
/// nothing (drop the `covered_modals.push`), which is exactly the defect this property guards:
/// with it, the surface is torn down by the orphan sweep on the push and the pop comes back with an
/// empty modal stack — `assert_eq!(d.nav.modals.surfaces.len(), 1)` fails at 0.
#[test]
fn a_detail_panel_parks_across_a_push_and_returns_with_the_same_instance() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    crate::plex::reset_servers_for_test();
    let here = crate::plex::register_for_test("park-here", "127.0.0.1", 1, "t", "c1");
    let other = crate::plex::register_for_test("park-other", "127.0.0.2", 2, "t", "c2");
    crate::metadata::alt_install(
        here,
        "m1",
        vec![
            crate::metadata::AltCopy { sid: here, rk: "m1".into(), library: "Movies".into(), ..Default::default() },
            crate::metadata::AltCopy { sid: other, rk: "copy".into(), library: "Shared".into(), ..Default::default() },
        ],
    );

    // The loop's own trail drives the tree (`bridge::sync_page`), so the push below is a trail push
    // followed by a frame on the new route — the shape the loop really produces.
    let mut trail = super::super::Trail::new();
    trail.push(super::super::Node::Detail {
        sid: here,
        rk: "m1".into(),
        spot: Default::default(),
    });
    let person = super::super::Node::Person {
        sid: here,
        key: "p1".into(),
        guid: "g".into(),
        name: "N".into(),
        thumb: String::new(),
    };

    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    let mut t = 0u32;
    let run = |d: &mut Dispatcher<AppHost>,
               rig: &mut Bridge,
               trail: &super::super::Trail,
               t: &mut u32,
               route: Route,
               n: u32| {
        for _ in 0..n {
            *t += 1;
            super::frame(d, rig, route, trail, tick(*t), vec![]);
        }
    };
    run(&mut d, &mut rig, &trail, &mut t, Route::Detail, 4);
    let host = d.nav.top_page().and_then(|e| e.inst.as_ref()).expect("the page mounted").id;

    // the panel, presented the way the page asks for it
    open_content_panel(
        &mut d,
        host,
        Some((here, "m1")),
        crate::screens::registry::ContentPanel::AltSources { anchor: [300.0f32, 800.0, 300.0, 60.0].map(f32::to_bits) },
    );
    run(&mut d, &mut rig, &trail, &mut t, Route::Detail, 40);
    let panel = d.nav.modals.surfaces.first().expect("the picker is up").entry.id;
    let panel_inst = d.nav.instance_of(panel).expect("…and is mounted");
    // move its cursor, so "the same instance" is a claim about STATE and not only about an id.
    // Through the ordinary input path: the container gives the keys to the topmost open surface,
    // which is what makes this the picker's press and not the page's.
    t += 1;
    super::frame(
        &mut d,
        &mut rig,
        Route::Detail,
        &trail,
        tick(t),
        vec![InputEvent {
            at: tick(t),
            source: Source::Script,
            kind: InputKind::Key {
                key: Key::Down,
                sym: crate::ui::consts::SDLK_DOWN,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        }],
    );
    run(&mut d, &mut rig, &trail, &mut t, Route::Detail, 2);
    let moved = panel_sel(&d, panel);
    assert_eq!(moved, 1, "the cursor is on the second copy");

    // push a page OVER the detail page
    trail.push(person);
    run(&mut d, &mut rig, &trail, &mut t, Route::Person, 8);
    assert!(
        d.nav.modals.surfaces.is_empty(),
        "the ACTIVE stack is the new page's, and it has none"
    );
    assert_eq!(
        d.nav.covered_modals.len(),
        1,
        "the detail page's own stack is PARKED, not torn down"
    );
    assert!(
        d.nav.instance(panel_inst).is_some(),
        "…and its body is still mounted while it waits"
    );

    // …and back
    trail.back();
    run(&mut d, &mut rig, &trail, &mut t, Route::Detail, 40);
    assert!(d.nav.covered_modals.is_empty(), "the park was collected");
    assert_eq!(
        d.nav.modals.surfaces.len(),
        1,
        "the picker came back with the page it belongs to"
    );
    let back = d.nav.modals.surfaces[0].entry.id;
    assert_eq!(
        d.nav.instance_of(back),
        Some(panel_inst),
        "the SAME instance — not a fresh mount"
    );
    assert_eq!(panel_sel(&d, back), moved, "…with the cursor it was left on");
}

/// The picker's selected row, off the mounted instance.
fn panel_sel(d: &Dispatcher<AppHost>, entry: EntryId) -> i32 {
    let mut probe = String::new();
    d.nav
        .entry(entry)
        .and_then(|e| e.inst.as_ref())
        .expect("mounted")
        .screen
        .state()
        .probe(&mut probe);
    assert_eq!(probe, "alt");
    d.nav
        .entry(entry)
        .and_then(|e| e.inst.as_ref())
        .and_then(|i| i.screen.as_any())
        .and_then(|s| s.downcast_ref::<crate::screens::alt_sources::AltSourcesScreen>())
        .expect("the picker")
        .table
        .sel
}

/// **The ANCHOR travels on the ARGUMENT, not in a module static.**
///
/// `ui/alt_sources.rs` held `static mut ANCHOR: PanelAnchor { rect: Rect, sid, rk }` — a geometry
/// static, which §15.2 says is never an allowlistable render cache and which is why this panel
/// could not be allowlisted through phase 12. The rect it held was written by whichever control
/// opened the panel LAST, process-wide, so the placement was a property of the process rather than
/// of the entry; and being an `f32` rect it could not be canonicalised at all, so a recording could
/// not hash where the panel was.
///
/// This grades the replacement on both counts: two panels for two different pills place
/// independently and simultaneously, and the argument's canonical form distinguishes them
/// bit-for-bit.
///
/// **Red first, simulated rather than historical** (the old spelling is a deleted module): with
/// `AltSourcesScreen::frame` narrowed to ignore its argument and read a fixed rect — the static's
/// behaviour, one answer for every instance — the two frames below are equal and the first
/// assertion fails.
#[test]
fn an_alt_sources_anchor_travels_on_its_arg() {
    use crate::screens::alt_sources::{AltSourcesArg, AltSourcesScreen};
    let _guard = crate::testlock::serial();
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::Clear);
    let arg = |x: f32, y: f32| AltSourcesArg {
        host: InstanceId(1),
        sid: crate::plex::ServerId::UNSET,
        rk: "m1".into(),
        anchor: [x, y, 300.0, 60.0].map(f32::to_bits),
    };
    let high = AltSourcesScreen::new(EntryId(1), arg(320.0, 200.0));
    let low = AltSourcesScreen::new(EntryId(2), arg(700.0, 880.0));
    let (a, b) = (high.frame(), low.frame());
    assert_ne!(
        (a.x, a.y),
        (b.x, b.y),
        "two panels on two pills place off their OWN pill, at the same time"
    );
    assert_eq!(a.x, 320.0, "each hangs off its own button's left edge");
    assert_eq!(b.x, 700.0);
    assert!(a.y > 200.0, "under the high pill…");
    assert!(b.y < 880.0, "…and above the low one, which has no room below");

    // the canonical form carries it: two anchors, two states, no float equality anywhere
    let canon = |arg: AltSourcesArg| crate::ui::machine::LogicalState::hash(&arg);
    assert_ne!(canon(arg(320.0, 200.0)), canon(arg(700.0, 880.0)));
    assert_eq!(
        canon(arg(320.0, 200.0)),
        canon(arg(320.0, 200.0)),
        "and the same anchor is the same state"
    );
}
