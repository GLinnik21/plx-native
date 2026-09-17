//! **THE LEFT ROAD, EXECUTED** (§7.3 step 2 + rule 9) — the one road into this surface that
//! no test ran end to end.
//!
//! The BACK road is executed: `app/bridge.rs`'s
//! `the_settings_surface_owns_input_and_walks_its_own_stack` drives a real `Dispatcher`, the
//! real surface and real `Key::Back` presses, root → Legal → back → root → dismissed. LEFT is
//! supposed to mean the same thing — "LEFT returns to the index, and only a second LEFT
//! leaves the family", which `ui/legal.rs`'s
//! `right_enters_a_document_and_left_walks_all_the_way_back_out` pinned for the legacy screens
//! and which went out of the tree WITH that module in phase 5b — and after the port it was
//! true only as the COMPOSITION of three links, each tested somewhere else and never together:
//!
//!  1. the table/document groups declare `EdgeRule::Nav(NavOpKind::Back)` on their LEFT edge
//!     (`ui::table_screen`'s own tests, and `screens::legal`'s, assert the edge array),
//!  2. `dispatch::after_step` turns that outcome into a synthetic `Key::Back` down with
//!     `at_edge: true` delivered to the input OWNER (`dispatch::edge_back_tests`, against a
//!     hand-built screen that counts BACKs and decrements an integer "depth"),
//!  3. `RouteSurface` walks its own `NavStack` on a BACK and declines one at its root (the
//!     tests in `settings_root_navigation_tests.rs`, which hand-feed `Key::Back` with
//!     `at_edge: false` and have no engine at all).
//!
//! Every link can stay green while the chain is broken, because no link's test contains the
//! next one's subject: link 2's owner is not this surface, and link 3's harness cannot
//! produce an edge. That is not hypothetical — link 2 IS a repair, of an engine shortcut that
//! sent the edge straight to `Navigation::back` and so dismissed the whole surface where a
//! BACK press had walked its stack (`after_step`'s own comment). This module runs the chain:
//! a real `Dispatcher`, a real `RouteSurface` presented into it as an `Opaque` modal, a real
//! LEFT through the real engine, and the two ends of the rule asserted — at depth the inner
//! stack pops and the surface stays up, at the root the surface is dismissed.
//!
//! **The link still not executed here is the DOCUMENT's own LEFT.** Reaching a Legal document
//! takes a second press, and at that depth the edge rule is `DocumentFocus`'s rather than the
//! table's — a different link 1, with links 2 and 3 identical (the surface sees a BACK and
//! pops, whatever declared the edge). `screens::legal`'s `a_document_s_left_edge_is_back_to_
//! the_index` grades that declaration on its own. What is proven below is the index level:
//! one LEFT back to the Settings root, a second one out of the family.
//!
//! The bundle is the family's own inner host, which is what makes this possible from this
//! file at all: `InnerHost` is a complete `Host` (`screens::family`) AND satisfies `AppLike`,
//! so `Dispatcher<InnerHost>` mounts the same `RouteSurface` the bridge does. The app's real
//! bundle (`app/bridge.rs`'s `AppHost`) cannot be named here — its `Arg` carries the legacy
//! `Route`, which is `app`-private — so what this cannot see is the app's own mounter and
//! nothing else; the dispatcher, the engine, the containers and the surface are the shipping
//! ones.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::screens::registry::{self, AppFx, AppMsg};
use crate::ui::containers::modal::{Phase, Style};
use crate::ui::dispatch::{CxParts, Dispatcher, NoTap, Rig, Split};
use crate::ui::fixture::{key, tick, FixtureMeasure};
use crate::ui::machine::{InputOwner, TimerId};
use crate::ui::present::Present;

/// The dispatcher's own mounter: the surface for anything but [`SettingsPage::About`],
/// which stands in for whatever page the application has UNDER Settings. The root stack
/// needs a body and this test is not about which one; a document is the family's most
/// inert page (its `prepare` is empty and it draws nothing without a `DrawFrame`).
struct SurfaceMounter;

impl Mounter<InnerHost> for SurfaceMounter {
    fn mount(
        &mut self,
        id: InstanceId,
        arg: &SettingsPage,
        _ret: &ReturnState<u32>,
        cx: &Cx<'_, InnerHost>,
        fx: &mut Effects<'_, InnerHost>,
    ) -> Box<dyn Screen<InnerHost>> {
        // `mount` is handed the mounting body's OWN entry as `cx.owner`
        // (`Dispatcher::mount`), and the surface must be built with it: every `FocusKey`
        // its pages mint carries that entry, and the engine matches on it.
        let entry = match cx.owner {
            InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        match arg {
            SettingsPage::About => mount_page(entry, SettingsPage::About, cx, fx),
            SettingsPage::ConsentStage(stage) => Box::new(RouteSurface::new(entry, id,
                Family::FirstRunConsent, SettingsPage::ConsentStage(*stage))),
            other => Box::new(RouteSurface::new(entry, id, Family::Settings, *other)),
        }
    }
}

struct SurfaceRig {
    mounter: SurfaceMounter,
    measure: FixtureMeasure,
    stores: crate::stores::Stores,
    directory: crate::stores::browse::DirectorySnapshot,
    /// How many times BACK reached the root of the ROOT stack (the platform's Home).
    roots: u32,
}

impl SurfaceRig {
    fn new() -> Self {
        Self {
            mounter: SurfaceMounter,
            measure: FixtureMeasure,
            stores: crate::stores::Stores::default(),
            directory: Default::default(),
            roots: 0,
        }
    }
}

impl Rig<InnerHost> for SurfaceRig {
    fn split(&mut self) -> Split<'_, InnerHost> {
        self.stores.capture_browse(&mut self.directory);
        Split {
            mounter: &mut self.mounter,
            views: self.directory.view(),
            measure: &self.measure,
        }
    }
    fn deliver(
        &mut self,
        _to: MachineId,
        _msg: &AppMsg,
        _parts: &CxParts<u32>,
        _fx: &mut Effects<'_, InnerHost>,
    ) -> Handled {
        Handled::No
    }
    fn timer(
        &mut self,
        _owner: MachineId,
        _id: TimerId,
        _parts: &CxParts<u32>,
        _fx: &mut Effects<'_, InnerHost>,
    ) {
    }
    fn app_fx(
        &mut self,
        _from: MachineId,
        _fx: AppFx,
        _parts: &CxParts<u32>,
        _out: &mut Effects<'_, InnerHost>,
    ) {
    }
    fn log(&mut self, _line: &str) {}
    fn prepare(&mut self, _b: &mut Budget, _present: &mut Present) {}
    fn ls2_pump(&mut self) {}
    fn opaque_route(&mut self, _bound: bool) {}
    fn clear_opaque_region(&mut self) {}
    fn now_us(&self) -> u64 {
        0
    }
    fn back_at_root(&mut self) {
        self.roots += 1;
    }
}

/// **`draw: false` on every frame, and it is not an optimisation.** `RouteSurface::draw`
/// paints — a scrim, the ground, then each body — and painting measures text through
/// SDL2_ttf and issues GL calls, neither of which a host unit build has. Steps 1–9 are
/// what this module grades (ingest, the engine, the drain, the nav commit), and step 10
/// is the only one it cannot run. The prepare pass still runs, which is safe: every
/// `prepare` in this family is empty.
fn frame(
    d: &mut Dispatcher<InnerHost>,
    rig: &mut SurfaceRig,
    ms: u32,
    inputs: Vec<crate::ui::machine::InputEvent<u32>>,
) {
    d.frame_with(rig, tick(ms), inputs, vec![], &mut NoTap, false);
}

/// A booted dispatcher with an `Opaque` Settings surface presented over the root page,
/// and the surface's entry id.
fn opened() -> (Dispatcher<InnerHost>, SurfaceRig, EntryId) {
    let mut d: Dispatcher<InnerHost> = Dispatcher::new();
    let mut rig = SurfaceRig::new();
    d.request(MachineId::Nav, NavOp::Root(SettingsPage::About));
    frame(&mut d, &mut rig, 0, vec![]);
    d.nav.next_style = Style::Opaque { snapshot: true };
    d.request(MachineId::Nav, NavOp::Present(SettingsPage::Root));
    frame(&mut d, &mut rig, 16, vec![]);
    let id = d
        .nav
        .modals
        .top()
        .expect("the surface is presented")
        .entry
        .id;
    (d, rig, id)
}

/// The surface's own `LogicalState::probe` — the path it is standing on, which is the
/// only view of the inner stack a caller outside this file has (the container hands out
/// `&dyn Screen`, so there is no downcast to a `RouteSurface`).
fn path(d: &Dispatcher<InnerHost>, id: EntryId) -> String {
    let mut s = String::new();
    d.nav
        .entry(id)
        .unwrap()
        .inst
        .as_ref()
        .unwrap()
        .screen
        .state()
        .probe(&mut s);
    s
}

/// Seat focus on the surface's Nth element as the test's PREMISE. Without it the first
/// direction key is spent by `move_dir`'s no-focus fallback, which seats and returns
/// `Moved` and never reaches an edge rule — the assertion would then be about seating
/// rather than about the rule under test.
fn seat(d: &mut Dispatcher<InnerHost>, id: EntryId, elem: u32) {
    d.set_focus(Some(FocusKey { entry: id, elem }));
}

fn consent_opened(page: SettingsPage) -> (Dispatcher<InnerHost>, SurfaceRig, EntryId) {
    consent_opened_on(page, SurfaceRig::new())
}

fn consent_opened_on(
    page: SettingsPage,
    mut rig: SurfaceRig,
) -> (Dispatcher<InnerHost>, SurfaceRig, EntryId) {
    let mut d = Dispatcher::new();
    d.request(MachineId::Nav, NavOp::Root(SettingsPage::About));
    frame(&mut d, &mut rig, 0, vec![]);
    d.nav.next_style = Style::Opaque { snapshot: true };
    d.request(MachineId::Nav, NavOp::Present(page));
    frame(&mut d, &mut rig, 16, vec![]);
    let id = d.nav.modals.top().unwrap().entry.id;
    (d, rig, id)
}

#[test]
fn composed_owner_first_run_answers_survive_full_frames() {
    let _g = crate::testlock::serial();
    for stage in [0, 1, 17] {
        let (mut d, mut rig, id) = consent_opened(SettingsPage::ConsentStage(stage));
        assert_eq!(d.focus(), Some(FocusKey { entry: id, elem: registry::BAND }), "stage {stage} mount");
        frame(&mut d, &mut rig, 32, vec![]);
        assert_eq!(d.focus().unwrap().elem, registry::BAND);
        for (round, direction) in [Key::Left, Key::Down].into_iter().enumerate() {
            let ms = 48 + round as u32 * 64;
            seat(&mut d, id, 1); // Privacy policy, not a guessed geometric starting point.
            frame(&mut d, &mut rig, ms, vec![key(direction, tick(ms))]);
            assert!(registry::band_index(d.focus().unwrap().elem).is_some(), "stage {stage}, {direction:?}");
            seat(&mut d, id, registry::BAND);
            frame(&mut d, &mut rig, ms + 16, vec![key(Key::Right, tick(ms + 16))]);
            assert_eq!(d.focus().unwrap().elem, registry::BAND + 1);
            frame(&mut d, &mut rig, ms + 32, vec![]);
            assert_eq!(d.focus().unwrap().elem, registry::BAND + 1);
            frame(&mut d, &mut rig, ms + 48, vec![key(Key::Left, tick(ms + 48))]);
            assert_eq!(d.focus().unwrap().elem, registry::BAND);
        }
        assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
        assert_eq!(rig.roots, 0);
    }
}

#[test]
fn composed_owner_settings_done_survives_then_disappears_with_reverted_draft() {
    let _g = crate::testlock::serial();
    let (mut d, mut rig, id) = consent_opened(SettingsPage::Privacy);
    seat(&mut d, id, 0);
    frame(&mut d, &mut rig, 32, vec![key(Key::Ok, tick(32))]); // draft only
    frame(&mut d, &mut rig, 48, vec![key(Key::Left, tick(48))]);
    assert_eq!(d.focus().unwrap().elem, registry::BAND);
    frame(&mut d, &mut rig, 64, vec![]);
    assert_eq!(d.focus().unwrap().elem, registry::BAND);
    seat(&mut d, id, 0);
    frame(&mut d, &mut rig, 80, vec![key(Key::Ok, tick(80))]); // reverse draft
    seat(&mut d, id, registry::BAND); // a retained key for a removed control
    frame(&mut d, &mut rig, 96, vec![]);
    assert!(registry::band_index(d.focus().unwrap().elem).is_none());
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
}

#[test]
fn composed_owner_pointer_seats_and_validates_answer_press_identity_without_committing() {
    use crate::ui::machine::{InputEvent, InputKind, PressId, Source};
    use crate::ui::screen::{Activate, Hover, Stop};
    let _g = crate::testlock::serial();
    for stage in [0, 1] {
        let (mut d, mut rig, id) = consent_opened(SettingsPage::ConsentStage(stage));
        for (round, elem) in [registry::BAND, registry::BAND + 1].into_iter().enumerate() {
            let ms = 32 + round as u32 * 32;
            let unchanged_path = path(&d, id);
            let focused = FocusKey { entry: id, elem };
            let c = cx(None);
            let screen = &d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen;
            let placed = screen.place(&elem, &c, At::Drawn).expect("real Consent button geometry");
            // No GL draw on the host: publish the real queried geometry to the real hit map.
            d.input.hit.fill(vec![Stop { key: focused, rect: placed.rect, rest_rect: placed.rest_rect,
                clip: placed.clip, hover: Hover::Focus, activate: Activate::Press }]);
            d.input.hit.swap();
            d.input.hit.dpad_mode = false;
            frame(&mut d, &mut rig, ms, vec![InputEvent { at: tick(ms), source: Source::Script,
                kind: InputKind::Pointer { x: placed.rect.cx(), y: placed.rect.cy(), hit: None } }]);
            assert_eq!(d.focus(), Some(focused));
            let instance = d.nav.instance_of(id).unwrap();
            // The production typed press validator, but Hold rather than Commit: this
            // page does not answer on hold, so neither consent choice is made or saved.
            d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(instance),
                Delivery::Press { id: PressId(1), key: focused, held: true }));
            let report = d.frame_with(&mut rig, tick(ms + 16), vec![], vec![], &mut NoTap, false);
            assert_eq!(report.dropped_deliveries, 0, "a valid button was refused by press identity validation");
            assert_eq!(d.focus(), Some(focused));
            assert_eq!(path(&d, id), unchanged_path, "holding must not answer or advance the stage");
            assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
        }
    }
}

#[test]
fn composed_owner_favourites_footer_survives_left_down_and_idle_frames() {
    let _g = crate::testlock::serial();
    let _session = scratch_session("composed-owner-favourites");
    struct ResetSources;
    impl Drop for ResetSources {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
        }
    }
    let _reset = ResetSources;
    crate::plex::reset_servers_for_test();
    let a = crate::plex::register_for_test("focus-a", "127.0.0.1", 9, "synthetic", "focus-test");
    let b = crate::plex::register_for_test("focus-b", "127.0.0.1", 9, "synthetic", "focus-test");
    // The existing fixture pins client identities and marks sections/counts complete:
    // the real Onboard Tick can poll discovery without spawning network work.
    let mut rig = SurfaceRig::new();
    rig.stores.browse.borrow_mut().seed_registered_table_for_test([a, b]);
    rig.stores.capture_browse(&mut rig.directory);
    let pins = rig.directory.view().favorite_sections().to_vec();
    let (mut d, mut rig, id) = consent_opened_on(SettingsPage::Favourites, rig);
    seat(&mut d, id, 0);
    frame(&mut d, &mut rig, 32, vec![key(Key::Ok, tick(32))]); // local draft only
    for (round, direction) in [Key::Left, Key::Down].into_iter().enumerate() {
        let ms = 48 + round as u32 * 32;
        let screen = &d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen;
        let mut groups = Vec::new();
        screen.groups(&cx_with(None, rig.directory.view()), &mut groups);
        let table = groups.iter().find(|g| g.id == GroupId(0)).unwrap();
        assert!(table.len > 0, "actual Favourites rows must exist");
        assert!(groups.iter().any(|g| g.id == GroupId(1) && g.len == 1), "draft offers Done");
        seat(&mut d, id, table.len as u32 - 1);
        frame(&mut d, &mut rig, ms, vec![key(direction, tick(ms))]);
        assert_eq!(d.focus().unwrap().elem, registry::BAND, "{direction:?} reaches Done after reconciliation");
        frame(&mut d, &mut rig, ms + 16, vec![]);
        assert_eq!(d.focus().unwrap().elem, registry::BAND);
    }
    rig.stores.capture_browse(&mut rig.directory);
    assert_eq!(rig.directory.view().favorite_sections(), pins,
        "draft navigation cannot persist pins");
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
}

/// **The composition, at depth.** Signed out, the Settings root's rows are Privacy & data
/// / Legal notices / About, so OK on row 1 pushes the Legal index; LEFT off that index's
/// column then runs the whole chain — edge rule, synthetic BACK, the surface's own pop —
/// and lands back on the Settings root with the surface still up and still owning input.
#[test]
fn left_inside_the_family_pops_the_inner_stack_and_never_dismisses_the_surface() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("composed-left-inner");
    let (mut d, mut rig, id) = opened();
    assert!(
        path(&d, id).starts_with("settings/root:"),
        "{}",
        path(&d, id)
    );

    seat(&mut d, id, 1);
    frame(&mut d, &mut rig, 32, vec![key(Key::Ok, tick(32))]);
    assert!(
        path(&d, id).contains("/legal:"),
        "OK on Legal notices pushed the index: {}",
        path(&d, id)
    );

    seat(&mut d, id, 0);
    frame(&mut d, &mut rig, 48, vec![key(Key::Left, tick(48))]);
    let p = path(&d, id);
    assert!(
        !p.contains("/legal:"),
        "LEFT popped the index off the surface's stack: {p}"
    );
    assert!(
        p.starts_with("settings/root:"),
        "…and landed on the Settings root: {p}"
    );
    assert_ne!(
        d.nav.modals.top().unwrap().phase,
        Phase::Closing,
        "a LEFT the surface answered is not the container's BACK"
    );
    assert_eq!(
        d.nav.input_owner(),
        Some(InputOwner::Entry(id)),
        "…and the surface still owns input"
    );
    assert_eq!(rig.roots, 0, "nothing reached the platform");
}

/// **The composition, at the surface's own root — the second LEFT.** The same press, one
/// level shallower, is declined by the surface (`Handled::No`), becomes the dispatcher's
/// `pending_back` and is resolved at the same frame's nav commit by dismissing the whole
/// surface. This is the half that must NOT change: `request`'s new "a Pop that would
/// empty the stack dismisses" guard deliberately does not cover BACK, because the
/// container's answer to a root BACK can be more than a dismissal.
#[test]
fn left_at_the_surfaces_own_root_dismisses_it() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("composed-left-root");
    let (mut d, mut rig, id) = opened();
    seat(&mut d, id, 0);
    frame(&mut d, &mut rig, 32, vec![key(Key::Left, tick(32))]);
    assert!(
        path(&d, id).starts_with("settings/root:"),
        "the stack never moved: {}",
        path(&d, id)
    );
    assert_eq!(
        d.nav.modals.top().unwrap().phase,
        Phase::Closing,
        "the refusal became the container's BACK, in the same frame"
    );
    assert_ne!(
        d.nav.input_owner(),
        Some(InputOwner::Entry(id)),
        "input left with it"
    );
    assert_eq!(
        rig.roots, 0,
        "a surface dismissal is not the platform's BACK"
    );
}
