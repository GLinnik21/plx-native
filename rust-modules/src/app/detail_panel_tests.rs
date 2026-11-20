//! Detail page's own panel parking across a push/pop.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

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
    let _guard = plx_base::testlock::serial();
    // Metadata is owned per-`Bridge` now, so `rig`'s own store drops with it — there is no
    // process-wide metadata state left for a `Cleanup` to clear. Server registration is still
    // process-global, so that reset stays.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_plex::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    plx_plex::plex::reset_servers_for_test();
    let here = plx_plex::plex::register_for_test("park-here", "127.0.0.1", 1, "t", "c1");
    let other = plx_plex::plex::register_for_test("park-other", "127.0.0.2", 2, "t", "c2");

    // Naming the page IS naming the item since the fold: the argument carries the identity, so
    // a frame on `detail_arg("m1")` mounts that detail page and a frame on `person_arg("p1")`
    // stacks the person page over it — which is the shape the loop really produces.
    let mut d = Dispatcher::<AppHost>::new();
    let mut owner = crate::app::runtime_owners::Harness::new(|| 0);
    let mut rig = owner.bridge();
    rig.metadata_run(plx_data::stores::metadata::MetadataCmd::AltInstall {
        sid: here,
        rk: "m1".into(),
        copies: vec![
            plx_data::metadata::AltCopy { sid: here, rk: "m1".into(), library: "Movies".into(), ..Default::default() },
            plx_data::metadata::AltCopy { sid: other, rk: "copy".into(), library: "Shared".into(), ..Default::default() },
        ],
    });
    let mut t = 0u32;
    let run = |d: &mut Dispatcher<AppHost>,
               rig: &mut Bridge,
               t: &mut u32,
               route: AppArg,
               n: u32| {
        for _ in 0..n {
            *t += 1;
            frame(d, rig, route.clone(), tick(*t), vec![]);
        }
    };
    run(&mut d, &mut rig, &mut t, detail_arg("m1"), 4);
    let host = d.nav.top_page().and_then(|e| e.inst.as_ref()).expect("the page mounted").id;

    // the panel, presented the way the page asks for it
    open_content_panel(
        &mut d,
        host,
        Some((here, "m1")),
        plx_screens::registry::ContentPanel::AltSources { anchor: [300.0f32, 800.0, 300.0, 60.0].map(f32::to_bits) },
    );
    run(&mut d, &mut rig, &mut t, detail_arg("m1"), 40);
    let panel = d.nav.modals.surfaces.first().expect("the picker is up").entry.id;
    let panel_inst = d.nav.instance_of(panel).expect("…and is mounted");
    // move its cursor, so "the same instance" is a claim about STATE and not only about an id.
    // Through the ordinary input path: the container gives the keys to the topmost open surface,
    // which is what makes this the picker's press and not the page's.
    t += 1;
    frame(
        &mut d,
        &mut rig,
        detail_arg("m1"),
        tick(t),
        vec![InputEvent {
            at: tick(t),
            source: Source::Script,
            kind: InputKind::Key {
                key: Key::Down,
                sym: plx_ui::consts::SDLK_DOWN,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        }],
    );
    run(&mut d, &mut rig, &mut t, detail_arg("m1"), 2);
    let moved = panel_sel(&d, panel);
    assert_eq!(moved, 1, "the cursor is on the second copy");

    // push a page OVER the detail page
    run(&mut d, &mut rig, &mut t, person_arg("p1"), 8);
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
    run(&mut d, &mut rig, &mut t, detail_arg("m1"), 40);
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
        .and_then(|s| s.downcast_ref::<plx_screens::alt_sources::AltSourcesScreen>())
        .expect("the picker")
        .form
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
    use plx_screens::alt_sources::{AltSourcesArg, AltSourcesScreen};
    let _guard = plx_base::testlock::serial();
    let arg = |x: f32, y: f32| AltSourcesArg {
        host: InstanceId(1),
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "m1".into(),
        anchor: [x, y, 300.0, 60.0].map(f32::to_bits),
    };
    let high = AltSourcesScreen::new(
        EntryId(1),
        arg(320.0, 200.0),
        plx_data::stores::metadata::MetadataStore::default().view(),
    );
    let low = AltSourcesScreen::new(
        EntryId(2),
        arg(700.0, 880.0),
        plx_data::stores::metadata::MetadataStore::default().view(),
    );
    let (a, b) = { let m = plx_ui::fixture::FixtureMeasure; (high.frame(&m), low.frame(&m)) };
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
    let canon = |arg: AltSourcesArg| plx_machine::machine::LogicalState::hash(&arg);
    assert_ne!(canon(arg(320.0, 200.0)), canon(arg(700.0, 880.0)));
    assert_eq!(
        canon(arg(320.0, 200.0)),
        canon(arg(320.0, 200.0)),
        "and the same anchor is the same state"
    );
}


// ---- a pick that closes its panel (#1424) ----------------------------------------------------
//
// *Play Version* and *Also available* are surfaces that close on a pick and REPORT the row to the
// Detail page that opened them (`Fx::Nav(Dismiss)`, then a delivery to the page). The page answers
// with an app command, and the FIFO runner (#1392) gates every page command on "the sender holds
// input". The surface's own dismissal is only parked until NAV COMMIT, so that gate saw the closing
// surface still holding input and refused the page: the panel closed and nothing played, or opened.

/// What each app command asked and how the runner ended it, read off the dispatcher's own tap: the
/// runner's terminal outcome, not a copy of it. A command's `Fx::App` effect is tapped as it leaves
/// the FIFO, and the runner holds exactly one command at a time, so the next `command_done` is that
/// command's.
#[derive(Default)]
struct PickTap {
    last: Option<Asked>,
    /// `(what was asked, who asked, how it ended)`, in the order the runner finished them.
    commands: Vec<(Asked, MachineId, plx_machine::machine::AppCommandOutcome)>,
    /// How many times a panel reported a picked version (`AppMsg::PlayVersion`) to its page.
    play_version_reported: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Play(Option<plx_plex::plex::MediaId>),
    Present(plx_plex::plex::ServerId, String),
    /// Any other app command, named by family: not what these tests grade, but a failure prints it.
    /// (The Play Version page also asks for a hero `PreviewStart` while the panel covers it; that one
    /// IS refused, rightly, before and after the fix: a covered page starts nothing.)
    Other(&'static str),
}

impl plx_ui::dispatch::Tap<AppHost> for PickTap {
    fn effect(&mut self, _: u64, stamped: &plx_machine::machine::Stamped<AppHost>) {
        use plx_screens::registry::{AppFx, AppMsg, ContentArg, ContentReq, PlayIntent};
        if let Fx::Deliver(_, plx_machine::machine::Delivery::Screen(plx_ui::screen::ScreenEvent::App(AppMsg::PlayVersion(_)))) = &stamped.fx {
            self.play_version_reported += 1;
        }
        if let Fx::App(app) = &stamped.fx {
            self.last = Some(match app {
                AppFx::Content(ContentReq::Play { play, .. }) => Asked::Play(match play {
                    PlayIntent::Item { version, .. } | PlayIntent::Movie { version, .. } => *version,
                }),
                AppFx::Content(ContentReq::Present(ContentArg::Detail { sid, rk })) => Asked::Present(*sid, rk.clone()),
                AppFx::Content(ContentReq::PreviewStart { .. }) => Asked::Other("content:PreviewStart"),
                AppFx::Content(_) => Asked::Other("content"),
                AppFx::Store(..) => Asked::Other("store"),
                _ => Asked::Other("app"),
            });
        }
    }
    fn command_done(&mut self, _: u64, _: plx_machine::machine::AppCommandId, from: MachineId,
        outcome: plx_machine::machine::AppCommandOutcome) {
        let asked = self.last.take().unwrap_or(Asked::Other("none"));
        self.commands.push((asked, from, outcome));
    }
}

impl PickTap {
    /// How the commands `host` asked that equal `asked` ended, in order.
    fn ended(&self, host: InstanceId, asked: &Asked) -> Vec<plx_machine::machine::AppCommandOutcome> {
        self.commands.iter()
            .filter(|(what, from, _)| what == asked && *from == MachineId::Instance(host))
            .map(|(_, _, outcome)| *outcome)
            .collect()
    }

    /// Every command of the run, for a failure message.
    fn account(&self) -> String {
        format!("{:?}", self.commands)
    }
}

/// One key (both edges) and the frames after it, through the product frame with `tap` watching.
/// No `goto`: a pick that enters the player must not be walked back to the page by the driver.
fn pick_key(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, t: &mut u32, key: Key, tap: &mut PickTap) {
    *t += 1;
    let at = tick(*t);
    super::frame_with_tap(d, rig, at, script_key(key, at), tap);
    pick_frames(d, rig, t, 28, tap);
}

fn pick_frames(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, t: &mut u32, n: u32, tap: &mut PickTap) {
    for _ in 0..n {
        *t += 1;
        super::frame_with_tap(d, rig, tick(*t), vec![], tap);
    }
}

fn page_instance(d: &Dispatcher<AppHost>) -> InstanceId {
    d.nav.top_page().and_then(|e| e.inst.as_ref()).expect("the page mounted").id
}

/// **Play Version, the reporter's case (#1424):** a title with two versions, the panel open, the
/// second row picked. The page's play command for THAT version is executed and the Player is
/// entered.
///
/// Red first on `origin/main` (`3878e4f55`): the page asked for the second version and the runner
/// answered `Refused` (`content_command`'s input-owner gate, run before the panel's `Dismiss`
/// committed), so no Player was ever pushed.
#[test]
fn a_picked_version_plays_that_version_though_its_panel_is_still_closing() {
    use plx_machine::machine::AppCommandOutcome;
    let versions = plx_appkit::version_list::sample();
    let pick = versions[1].id;
    let mut w = World::detail_page("play-version-pick", plx_data::metadata::Detail {
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        title: "Movie".into(),
        part: "/library/parts/movie".into(),
        versions,
        ..Default::default()
    });
    let host = page_instance(&w.d);
    open_content_panel(
        &mut w.d,
        host,
        Some((plx_plex::plex::ServerId::UNSET, "movie")),
        plx_screens::registry::ContentPanel::Versions { anchor: [300.0f32, 800.0, 300.0, 60.0].map(f32::to_bits) },
    );
    w.frames(40);
    assert_eq!(w.d.nav.modals.surfaces.len(), 1, "premise: the Play Version panel is up");
    let mut tap = PickTap::default();
    let mut t = w.frame;
    let mut bridge = w.bridge.bridge();
    pick_key(&mut w.d, &mut bridge, &mut t, Key::Down, &mut tap);
    pick_key(&mut w.d, &mut bridge, &mut t, Key::Ok, &mut tap);
    assert_eq!(
        tap.ended(host, &Asked::Play(Some(pick))), [AppCommandOutcome::Completed],
        "the page asked to play the SECOND version, once, and the runner executed it: {}", tap.account());
    assert!(w.d.nav.modals.surfaces.is_empty(), "the panel closed");
    assert!(matches!(w.d.top_arg(), Some(AppArg::Player)), "the pick entered the player");
}

/// **Play Version on a platform that composed no playback driver (W6c):** the panel's rows are the
/// shared disabled rows, so the pick presses nothing: no play is asked, the player is not entered
/// and the panel stays up (OK on a disabled row does nothing; BACK still closes it). The mount
/// reads the platform's fact (`ScreenInputs::can_play`), which the previous test, on the default
/// set, proves is not what stops it there.
#[test]
fn a_version_pick_asks_for_no_play_when_the_platform_composed_no_driver() {
    let versions = plx_appkit::version_list::sample();
    let pick = versions[1].id;
    let mut w = World::detail_page("play-version-no-driver", plx_data::metadata::Detail {
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        title: "Movie".into(),
        part: "/library/parts/movie".into(),
        versions,
        ..Default::default()
    });
    w.bridge.bridge().retained.screen_inputs.backends = plx_media::route::BackendSet::NONE;
    let host = page_instance(&w.d);
    open_content_panel(
        &mut w.d,
        host,
        Some((plx_plex::plex::ServerId::UNSET, "movie")),
        plx_screens::registry::ContentPanel::Versions { anchor: [300.0f32, 800.0, 300.0, 60.0].map(f32::to_bits) },
    );
    w.frames(40);
    assert_eq!(w.d.nav.modals.surfaces.len(), 1, "premise: the Play Version panel is up");
    let mut tap = PickTap::default();
    let mut t = w.frame;
    let mut bridge = w.bridge.bridge();
    pick_key(&mut w.d, &mut bridge, &mut t, Key::Down, &mut tap);
    pick_key(&mut w.d, &mut bridge, &mut t, Key::Ok, &mut tap);
    assert_eq!(tap.play_version_reported, 0, "a disabled row reports nothing to the page: {}", tap.account());
    assert!(tap.ended(host, &Asked::Play(Some(pick))).is_empty(), "no play was asked: {}", tap.account());
    assert!(tap.commands.iter().all(|(asked, ..)| !matches!(asked, Asked::Play(_))), "{}", tap.account());
    assert!(!matches!(w.d.top_arg(), Some(AppArg::Player)), "the player was not entered");
    assert_eq!(w.d.nav.modals.surfaces.len(), 1, "OK on a disabled row left the panel up: {}", tap.account());
    pick_key(&mut w.d, &mut bridge, &mut t, Key::Back, &mut tap);
    assert!(w.d.nav.modals.surfaces.is_empty(), "BACK closes the panel: {}", tap.account());
}

/// The two-copy *Also available* rig of `a_detail_panel_parks_across_a_push_and_returns_with_the_same_instance`:
/// the page on `m1` of server `here`, the picker up over it, its cursor on the first copy.
struct AltRig {
    d: Dispatcher<AppHost>,
    owner: crate::app::runtime_owners::Harness,
    t: u32,
    host: InstanceId,
    other: plx_plex::plex::ServerId,
}

impl AltRig {
    fn open() -> Self {
        let here = plx_plex::plex::register_for_test("alt-pick-here", "127.0.0.1", 1, "t", "c1");
        let other = plx_plex::plex::register_for_test("alt-pick-other", "127.0.0.2", 2, "t", "c2");
        let mut d = Dispatcher::<AppHost>::new();
        let mut owner = crate::app::runtime_owners::Harness::new(|| 0);
        let mut t = 0u32;
        let host = {
            let mut rig = owner.bridge();
            rig.metadata_run(plx_data::stores::metadata::MetadataCmd::AltInstall {
                sid: here,
                rk: "m1".into(),
                copies: vec![
                    plx_data::metadata::AltCopy { sid: here, rk: "m1".into(), library: "Movies".into(), ..Default::default() },
                    plx_data::metadata::AltCopy { sid: other, rk: "copy".into(), library: "Shared".into(), ..Default::default() },
                ],
            });
            for _ in 0..4 {
                t += 1;
                frame(&mut d, &mut rig, detail_arg("m1"), tick(t), vec![]);
            }
            let host = page_instance(&d);
            open_content_panel(
                &mut d,
                host,
                Some((here, "m1")),
                plx_screens::registry::ContentPanel::AltSources { anchor: [300.0f32, 800.0, 300.0, 60.0].map(f32::to_bits) },
            );
            for _ in 0..40 {
                t += 1;
                frame(&mut d, &mut rig, detail_arg("m1"), tick(t), vec![]);
            }
            host
        };
        assert_eq!(d.nav.modals.surfaces.len(), 1, "premise: the picker is up");
        Self { d, owner, t, host, other }
    }
}

struct ResetServers;
impl Drop for ResetServers {
    fn drop(&mut self) {
        plx_plex::plex::reset_servers_for_test();
    }
}

/// **Also available, the same shape (#1424):** picking the other copy closes the picker and the
/// page presents that copy's own page. The page's `ContentReq::Present` met the same gate.
///
/// Red first on `origin/main`: the page asked to present the other copy and the runner answered
/// `Refused`, so the picker closed and the page stayed where it was.
#[test]
fn a_picked_alternative_source_opens_that_copy_though_its_picker_is_still_closing() {
    use plx_machine::machine::AppCommandOutcome;
    let _guard = plx_base::testlock::serial();
    let _reset = ResetServers;
    plx_plex::plex::reset_servers_for_test();
    let mut s = AltRig::open();
    let mut tap = PickTap::default();
    let mut rig = s.owner.bridge();
    pick_key(&mut s.d, &mut rig, &mut s.t, Key::Down, &mut tap);
    pick_key(&mut s.d, &mut rig, &mut s.t, Key::Ok, &mut tap);
    assert_eq!(
        tap.ended(s.host, &Asked::Present(s.other, "copy".into())), [AppCommandOutcome::Completed],
        "the page asked to present the other copy, once, and the runner executed it: {}", tap.account());
    let copy = AppArg::Content(plx_screens::registry::ContentArg::Detail { sid: s.other, rk: "copy".into() });
    assert!(
        s.d.nav.modals.surfaces.iter().any(|surface| surface.entry.arg == copy && surface.phase != Phase::Closing),
        "the other copy's page is up over the first"
    );
}

/// **The gate still refuses a page that a SURFACE COVERS (#1424's negative control).** The same
/// report, delivered to the page while the picker is open and nothing has dismissed it: the page
/// does not hold input, so its command is still refused. Only a surface whose own `Dismiss` is
/// already parked counts as closed.
#[test]
fn a_page_under_a_picker_that_stays_open_is_still_refused() {
    use plx_machine::machine::AppCommandOutcome;
    use plx_screens::registry::AppMsg;
    let _guard = plx_base::testlock::serial();
    let _reset = ResetServers;
    plx_plex::plex::reset_servers_for_test();
    let mut s = AltRig::open();
    let mut tap = PickTap::default();
    let mut rig = s.owner.bridge();
    s.d.emit(MachineId::Nav, Fx::Deliver(
        MachineId::Instance(s.host),
        Delivery::Screen(ScreenEvent::App(AppMsg::AltSourceOpen(
            plx_screens::registry::ContentArg::Detail { sid: s.other, rk: "copy".into() })))));
    pick_frames(&mut s.d, &mut rig, &mut s.t, 4, &mut tap);
    assert_eq!(
        tap.ended(s.host, &Asked::Present(s.other, "copy".into())), [AppCommandOutcome::Refused],
        "the page asked, and a covered page is refused: {}", tap.account());
    assert_eq!(s.d.nav.modals.surfaces.len(), 1, "the picker is still up, and nothing was presented over it");
}
