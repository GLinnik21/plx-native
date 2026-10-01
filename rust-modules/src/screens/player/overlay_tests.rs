//! **Issue 28, restated against the surface that now answers it.**
//!
//! Until phase 9 this was `app::playback::overlay_swallows_key` — a pure predicate over
//! `Route::Player { overlay }` that told the loop's key ladder whether to `continue` past its
//! overlay arm or let the press FALL THROUGH to the ordinary transport arms. A surface cannot fall
//! through: the dispatcher hands it the key and the ladder never sees it. So the behaviour the old
//! predicate expressed as "does not swallow" is expressed here as "FORWARDS `PlayerReq::Transport`",
//! and this module grades the same three claims the old one did, one layer lower — over the real
//! `Machine::step`, with the real `consts::classify`, rather than over a hand-written `Key`.
//!
//! **Restructure phase 12 (D2) moved who answers a direction and an OK.** Before this package the
//! `Focusable` impl was a stub — one `Free` region of `len: 1` regardless of the panel, `place`
//! answering `Rect::FULL` for any key — and `step`'s own `key` ladder moved every panel's cursor by
//! hand, swallowing every direction and OK itself (`Handled::Yes` unconditionally). The tests below
//! marked **(D2 repro)** are the ones that were RED against that tree; the rest keep grading exactly
//! what they always did, one behaviour the ENGINE now owns rather than this ladder.
//!
//! What it still cannot say is whether the panel visually stays up; that is a device check.

use super::overlay::{OverlayKind, Panel, PlayerOverlayScreen};
use crate::screens::registry::{AppFx, AppMsg, PageMemory, PlayerReq};
use crate::ui::consts::{SDLK_DOWN, SDLK_RETURN, SDLK_UP, WCODE_BACK, WCODE_PAUSE, WCODE_PLAY,
    WCODE_PLAYPAUSE, WCODE_STOP};
use crate::ui::fixture::FixtureMeasure;
use crate::ui::form::FormId;
use crate::ui::more_menu::{Action as MoreAction, MoreRow, MorePage};
use crate::ui::page_stack::TITLE_KEY;
use crate::ui::track_menu::{StyleField, TrackPage, TrackRow};
use crate::ui::machine::{
    Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Host, InputEvent, InputKind,
    InputOwner, InstanceId, Machine, MachineId, NavOp, PressId, Source, Tick,
};
use crate::ui::screen::{At, By, Focusable, Placed, ScreenEvent};

pub(super) struct TestHost;
impl Host for TestHost {
    type Arg = crate::ui::fixture::FixtureArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    type Init = crate::ui::fixture::FixtureArg;
    type Memory = PageMemory;
}

impl crate::screens::registry::PlayerLike for TestHost {
    fn session<'a>(_cx: &Cx<'a, Self>) -> &'a crate::route::PlaybackSession {
        crate::route::idle_session_for_test()
    }
}

thread_local! {
    // TEST ONLY: see `screens::detail::tests`'s `TEST_METADATA` for why this lives here rather
    // than being threaded as a parameter.
    static TEST_METADATA: std::cell::UnsafeCell<crate::stores::metadata::MetadataStore> =
        std::cell::UnsafeCell::new(crate::stores::metadata::MetadataStore::default());
}

fn test_store() -> &'static mut crate::stores::metadata::MetadataStore {
    TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
}

impl crate::screens::registry::MetadataLike for TestHost {
    fn metadata<'a>(_cx: &Cx<'a, Self>) -> crate::metadata::MetadataView<'a> {
        test_store().view()
    }
}

const ENTRY: EntryId = EntryId(44);
const INST: InstanceId = InstanceId(7);

fn cx() -> Cx<'static, TestHost> {
    Cx {
        views: (),
        tick: Tick::default(),
        measure: &FixtureMeasure,
        focus: Default::default(),
        press: Default::default(),
        owner: InputOwner::Entry(ENTRY),
    }
}

/// Deliver one event through the surface. Returns `(handled, the app requests it raised, whether
/// it asked the container to dismiss it)` — the three things every test below reads back.
fn deliver(page: &mut PlayerOverlayScreen, ev: ScreenEvent<TestHost>) -> (Handled, Vec<PlayerReq>, bool) {
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let handled = page.step(
        &ev,
        &cx(),
        &mut Effects::new(&mut out, MachineId::Instance(INST), &mut present),
    );
    let mut reqs = Vec::new();
    let mut dismissed = false;
    for effect in out {
        match effect.fx {
            Fx::App(AppFx::Player(req)) => reqs.push(req),
            Fx::Nav(NavOp::Dismiss(id)) if id == ENTRY => dismissed = true,
            _ => {}
        }
    }
    (handled, reqs, dismissed)
}

/// One key press through the surface.
fn press(page: &mut PlayerOverlayScreen, sym: u32, wcode: u32, edge: Edge) -> (Handled, Vec<PlayerReq>, bool) {
    deliver(
        page,
        ScreenEvent::Input(InputEvent {
            kind: InputKind::Key {
                key: crate::ui::machine::Key::Other,
                sym,
                wcode,
                edge,
                at_edge: false,
            },
            at: Tick { ms: 1_000, dt_us: 0 },
            source: Source::Sdl,
        }),
    )
}

/// A direction re-delivered at this panel's group EDGE (`EdgeRule::Screen`, §7.3 step 3) — what
/// the engine does after its own `neighbour` answers `Step::Edge` and the group declares `Screen`
/// on that side; `edge_key` is the only place left that a panel decides something outside its own
/// scope, so this is the one path a unit test has to synthesize rather than observe end to end.
fn press_at_edge(page: &mut PlayerOverlayScreen, sym: u32) -> (Handled, Vec<PlayerReq>, bool) {
    deliver(
        page,
        ScreenEvent::Input(InputEvent {
            kind: InputKind::Key {
                key: crate::ui::machine::Key::Other,
                sym,
                wcode: 0,
                edge: Edge::Down,
                at_edge: true,
            },
            at: Tick { ms: 1_000, dt_us: 0 },
            source: Source::Sdl,
        }),
    )
}

fn click(page: &mut PlayerOverlayScreen, x: f32, y: f32) -> (Handled, Vec<PlayerReq>, bool) {
    deliver(page, ScreenEvent::Input(InputEvent {
        kind: InputKind::Click { x, y, hit: None },
        at: Tick { ms: 1_000, dt_us: 0 },
        source: Source::Sdl,
    }))
}

/// A pointer click's `Activate` — delivered directly by the dispatcher's hit map on a hit
/// (§7.5-7.6), never scanned for inside `step` any more.
fn activate(page: &mut PlayerOverlayScreen, elem: u32) -> (Handled, Vec<PlayerReq>, bool) {
    deliver(page, ScreenEvent::Activate(elem))
}

/// A keyboard OK's deferred commit — the engine's own press machinery arms on the down edge and
/// delivers this on release (§7.4).
fn press_commit(page: &mut PlayerOverlayScreen, id: u32) -> (Handled, Vec<PlayerReq>, bool) {
    deliver(page, ScreenEvent::PressCommit(PressId(id)))
}

/// The three panels a viewer reads WHILE the film runs. `More` is deliberately not here.
const MODAL: [(OverlayKind, &str); 3] = [
    (OverlayKind::Tracks { tab: 0 }, "Menu (tracks)"),
    (OverlayKind::Info, "Info"),
    (OverlayKind::Chapters, "Chapters"),
];

/// **The reported bug.** A viewer holding the track menu, the Info card or the Chapters strip open
/// still expects PAUSE/PLAY to work — and the panel to stay up. The old ladder said this by NOT
/// swallowing; the surface says it by forwarding the press to the loop, which spends it on the
/// same toggle. Either way the panel is untouched, which is the half `Fx::Nav(Dismiss)` grades.
#[test]
fn a_transport_key_is_forwarded_by_a_modal_panel_and_leaves_it_up() {
    let ps = crate::route::PlaybackSession::IDLE;
    for (kind, name) in MODAL {
        for (wcode, want) in [
            (WCODE_PAUSE, Some(false)),
            (WCODE_PLAY, Some(true)),
            (WCODE_PLAYPAUSE, None),
        ] {
            let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
            let (handled, reqs, dismissed) = press(&mut page, 0, wcode, Edge::Down);
            assert_eq!(handled, Handled::Yes, "{name}: the surface owns the press");
            assert_eq!(
                reqs,
                vec![PlayerReq::Transport(want)],
                "{name}: wcode {wcode} must reach the toggle",
            );
            assert!(!dismissed, "{name}: the panel stays up under a transport key");
        }
    }
}

/// **(D2 repro)** A fresh DIRECTION is no longer the panel's own at all: it falls through
/// (`Handled::No`) so the ENGINE's `neighbour` can move it (§7.3 step 2). On 2790f47a this
/// returned `Handled::Yes` unconditionally (the ladder moved the cursor itself), which is exactly
/// what made the engine's own geometric stepping dead code. OK is left to the same mechanism
/// (§7.4) — see the `activate`/`press_commit` tests below for what happens once it fires. BACK
/// stays the panel's own, since dismissing a modal is not a focus move.
#[test]
fn a_fresh_direction_and_ok_fall_through_to_the_engine_and_back_is_still_the_panels_own() {
    let ps = crate::route::PlaybackSession::IDLE;
    for (kind, name) in MODAL {
        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
        let (handled, reqs, _) = press(&mut page, SDLK_UP, 0, Edge::Down);
        assert_eq!(handled, Handled::No, "{name}: UP is now the engine's to move");
        assert!(
            !reqs.iter().any(|r| matches!(r, PlayerReq::Transport(_))),
            "{name}: UP is not a transport key",
        );

        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
        let (handled, ..) = press(&mut page, SDLK_RETURN, 0, Edge::Down);
        assert_eq!(handled, Handled::No, "{name}: OK is the engine's Activate/PressArm to answer");

        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
        let (handled, _, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
        assert_eq!(handled, Handled::Yes, "{name}: BACK is still the panel's");
        assert!(dismissed, "{name}: and BACK is what closes it");
    }
}

/// STOP is not one of the transport exceptions. A reading panel owns and swallows it; leaking it
/// would end playback underneath a still-open panel.
#[test]
fn stop_stays_swallowed_by_the_three_reading_panels() {
    let ps = crate::route::PlaybackSession::IDLE;
    for (kind, name) in MODAL {
        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
        let (handled, reqs, dismissed) = press(&mut page, 0, WCODE_STOP, Edge::Down);
        assert_eq!(handled, Handled::Yes, "{name}: STOP is panel-owned");
        assert!(reqs.is_empty(), "{name}: STOP must not reach the player");
        assert!(!dismissed, "{name}: STOP is swallowed, not translated to BACK");
    }
}

/// `More` keeps the old swallow-everything answer, for the reason it always had: the transport
/// exception was reported and reproduced against the other three, and this popover's rows include
/// the failure read-out's own recovery path.
#[test]
fn the_options_popover_keeps_the_old_swallow_everything_behaviour() {
    let ps = crate::route::PlaybackSession::IDLE;
    for wcode in [WCODE_PAUSE, WCODE_PLAY, WCODE_PLAYPAUSE] {
        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::More { quality: false });
        let (handled, reqs, dismissed) = press(&mut page, 0, wcode, Edge::Down);
        assert_eq!(handled, Handled::Yes);
        assert!(
            reqs.is_empty(),
            "More is excluded from the transport exception (wcode {wcode})",
        );
        assert!(!dismissed);
    }
}

#[test]
fn back_dismisses_more_at_the_root_and_pops_the_quality_page_first() {
    let ps = crate::route::PlaybackSession::IDLE;
    // the ordinary entry is the root: BACK dismisses
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::More { quality: false });
    let (handled, reqs, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert_eq!(handled, Handled::Yes, "More owns BACK");
    assert!(reqs.is_empty(), "BACK must not activate a More row");
    assert!(dismissed, "BACK must dismiss the More root");
    // the quality recovery entry opens ON the Quality page: BACK goes up one page, then dismisses
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::More { quality: true });
    assert_eq!(more_page(&page), Some(MorePage::Quality));
    let (handled, reqs, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert_eq!(handled, Handled::Yes);
    assert!(only_hud(&reqs) && !dismissed, "BACK on the Quality page only pops it: {reqs:?}");
    assert_eq!(more_page(&page), None);
    let (_, reqs, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert!(reqs.is_empty() && dismissed, "BACK at the root dismisses");
}

/// Nothing but the read-time `ExtendHud` a menu move raises.
fn only_hud(reqs: &[PlayerReq]) -> bool {
    reqs.iter().all(|r| matches!(r, PlayerReq::ExtendHud(_)))
}

fn more_page(page: &PlayerOverlayScreen) -> Option<MorePage> {
    let Panel::More(menu) = page.panel() else { panic!("More panel") };
    menu.page()
}

/// Focus `row` on the More panel the way the engine would (a `FocusMoved`), then press OK on it the
/// way a click does (`Activate`, the row's key).
fn more_open(page: &mut PlayerOverlayScreen, row: MoreRow) -> (Handled, Vec<PlayerReq>, bool) {
    let key = row.key().0;
    deliver(page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: key }, by: By::Dir });
    activate(page, key)
}

/// **More's Quality row drills in, and LEFT / BACK / a title click all go back.** OK (or RIGHT) on
/// the row pushes and asks the app for nothing, keeping the panel up; a rung on the page commits
/// the ordinary `More(SetQuality)` request and dismisses exactly as the flat ladder's rung did.
#[test]
fn the_quality_row_pushes_and_left_back_and_the_title_pop() {
    use crate::ui::consts::{SDLK_LEFT, SDLK_RIGHT};
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::More { quality: false });
    assert_eq!(more_page(&page), None);

    // OK on the row
    let (_, reqs, dismissed) = more_open(&mut page, MoreRow::OpenQuality);
    assert!(!dismissed && only_hud(&reqs), "a Nav row keeps the panel open and asks for nothing but read time: {reqs:?}");
    assert_eq!(more_page(&page), Some(MorePage::Quality));
    // LEFT at the edge pops
    press_at_edge(&mut page, SDLK_LEFT);
    assert_eq!(more_page(&page), None);
    // RIGHT at the edge enters
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: MoreRow::OpenQuality.key().0 }, by: By::Dir });
    press_at_edge(&mut page, SDLK_RIGHT);
    assert_eq!(more_page(&page), Some(MorePage::Quality));
    // a click on the title band pops, and acts on no row
    let (_, reqs, dismissed) = activate(&mut page, TITLE_KEY);
    assert!(!dismissed && only_hud(&reqs), "a title click commits nothing: {reqs:?}");
    assert_eq!(more_page(&page), None);
    let Panel::More(menu) = page.panel() else { panic!("More") };
    assert_eq!(menu.sel_id(), Some(MoreRow::OpenQuality), "the pop restores focus on the Quality row");
    // RIGHT / LEFT at the root with an Options row focused: nothing happens
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: MoreRow::Act(MoreAction::ToggleStats).key().0 }, by: By::Dir });
    press_at_edge(&mut page, SDLK_RIGHT);
    press_at_edge(&mut page, SDLK_LEFT);
    assert_eq!(more_page(&page), None);

    // a rung on the page commits `More(SetQuality)` and dismisses
    more_open(&mut page, MoreRow::OpenQuality);
    let rung = MoreRow::Act(MoreAction::SetQuality(crate::route::Quality::P720));
    let (_, reqs, dismissed) = more_open(&mut page, rung);
    assert!(dismissed, "a rung pick closes the menu, as it always did");
    assert!(
        reqs.iter().any(|r| matches!(r, PlayerReq::More(MoreAction::SetQuality(crate::route::Quality::P720)))),
        "the pick reports the ordinary SetQuality request: {reqs:?}"
    );
}

/// The Quality page registers a pointer-only title stop that replay can place, never in the D-pad
/// column; the root registers none. Mid-slide a stop sits where the page is DRAWN.
#[test]
fn the_more_title_band_is_a_pointer_only_stop_and_slides_with_its_page() {
    use crate::ui::screen::{DrawFrame, Screen};
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::More { quality: false });
    let stops_of = |page: &PlayerOverlayScreen| {
        let cx = cx();
        let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
        page.record_stops(&mut f);
        f.into_stops()
    };
    let tick = |page: &mut PlayerOverlayScreen, ms: u32| {
        crate::ui::idle::frame_begin(1.0 / 60.0);
        deliver(page, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    };
    tick(&mut page, 1_000);
    assert!(stops_of(&page).iter().all(|s| s.key.elem != TITLE_KEY), "the root has no title band");
    assert!(!Screen::<TestHost>::pointer_held(&page));

    more_open(&mut page, MoreRow::OpenQuality);
    assert!(Screen::<TestHost>::pointer_held(&page), "a push holds the pointer from the first frame");
    tick(&mut page, 1_016);
    let mid = stops_of(&page);
    let title = mid.iter().find(|s| s.key.elem == TITLE_KEY).expect("a pushed page registers its title stop");
    assert_eq!(title.hover, crate::ui::screen::Hover::Ignore, "pointer-only: hovering it moves no focus");
    assert!(title.rect.x > title.rest_rect.x, "mid-slide the stop is where the page is DRAWN");
    let cx = cx();
    let placed = Focusable::<TestHost>::place(&page, &TITLE_KEY, &cx, At::Drawn).expect("replay can place the title key");
    assert_eq!((placed.rect.x, placed.clip.x), (title.rect.x, title.clip.x), "place and stop agree mid-slide");
    assert_eq!(Focusable::<TestHost>::group_of(&page, &TITLE_KEY, &cx), None, "never in the D-pad column");
    for i in 0..240 {
        tick(&mut page, 1_032 + i * 16);
    }
    assert!(!Screen::<TestHost>::pointer_held(&page), "released once settled");
    let rest = stops_of(&page);
    let title = rest.iter().find(|s| s.key.elem == TITLE_KEY).expect("title stop");
    assert_eq!(title.rect, title.rest_rect, "at rest the stop IS its layout");
}

/// **The held-direction cadence, which moved WITH the input** (unchanged mechanism, graded at the
/// `Handled` level now that a fresh direction no longer moves a cursor `step` itself owns). The
/// loop paced these four lists at 110 ms from its own `HeldKey` timer; the hardware streams
/// `Edge::Repeat` at ~50 ms, so without a gate of its own the surface would ask the engine to walk
/// a menu twice as fast as every other list in the app. A FRESH press is never swallowed by the
/// press before it, which is what `rearm` is for.
#[test]
fn a_held_direction_is_paced_and_a_fresh_press_is_never_swallowed() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::More { quality: false });
    // A fresh press rearms the cadence and is handed to the engine.
    let (handled, ..) = press(&mut page, SDLK_DOWN, 0, Edge::Down);
    assert_eq!(handled, Handled::No, "the fresh press falls through to the engine");
    // Two hardware repeats inside one 110 ms window: the second (here, the very next repeat at
    // the same synthetic instant) is admitted by nothing and stays the panel's own.
    let (handled, ..) = press(&mut page, SDLK_DOWN, 0, Edge::Repeat);
    assert_eq!(
        handled,
        Handled::Yes,
        "a repeat inside the window is swallowed by the panel, not handed to the engine",
    );
    // …and a NEW press at the same instant is not the held key's beat — it always rearms.
    let (handled, ..) = press(&mut page, SDLK_DOWN, 0, Edge::Down);
    assert_eq!(handled, Handled::No, "a fresh press always falls through");
}

/// **(D2 repro)** A click no longer scans pixels — or dismisses — inside `step`. The dispatcher's
/// own hit map resolves it BEFORE the raw event reaches a screen at all (§7.5-7.6: a hit delivers
/// `Activate`, a miss reaches `Style::PlayerPanel`'s own `OnMiss::Dismiss` in
/// `ui/containers/modal.rs`), so every panel's `step` just lets it fall through. On 2790f47a this
/// path read `Panel::More`'s own pixel position by hand and dismissed unconditionally for every
/// other panel — see `activate`/`press_commit` below for what a resolved hit does now.
#[test]
fn a_click_no_longer_scans_pixels_or_dismisses_inside_step() {
    let ps = crate::route::PlaybackSession::IDLE;
    for (kind, name) in MODAL.into_iter().chain([(OverlayKind::More { quality: false }, "More")]) {
        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
        let (handled, reqs, dismissed) = click(&mut page, 10.0, 10.0);
        assert_eq!(handled, Handled::No, "{name}: the engine's hit map resolves a click, not step");
        assert!(reqs.is_empty(), "{name}: step raises nothing from a raw click");
        assert!(!dismissed, "{name}: step no longer dismisses a click itself");
    }
}

/// **(D2 repro)** `groups()` publishes the ACTIVE panel's real row count. On 2790f47a this
/// answered one `Free` region of `len: 1` no matter what — `Info` always has exactly two action
/// buttons regardless of what is playing, so this needs no external data to be a clean assertion
/// either way, and it is the exact case the package brief names.
#[test]
fn groups_publishes_the_active_panels_real_row_count() {
    let ps = crate::route::PlaybackSession::IDLE;
    let page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&page, &cx(), &mut groups);
    assert_eq!(groups.len(), 1, "one focus group for the action column");
    assert_eq!(
        groups[0].len, 2,
        "From Beginning + Go to Show/Movie — not the stub's len 1",
    );
}

/// **(D2 repro)** `place(k, At::Drawn)` answers each row's OWN drawn rect, not one `Rect::FULL`
/// for every key regardless of which — the hit map needs the real rect to resolve a click at all.
#[test]
fn place_answers_each_rows_own_rect_not_one_full_screen_stop() {
    let ps = crate::route::PlaybackSession::IDLE;
    let page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    let first: Placed = Focusable::<TestHost>::place(&page, &0u32, &cx(), At::Drawn).expect("row 0 places");
    let second: Placed = Focusable::<TestHost>::place(&page, &1u32, &cx(), At::Drawn).expect("row 1 places");
    assert_ne!(
        (first.rect.y, first.rect.h),
        (second.rect.y, second.rect.h),
        "each row is its own rect, not the whole screen twice",
    );
    let full = crate::ui::Rect::FULL;
    assert_ne!(
        (first.rect.x, first.rect.y, first.rect.w, first.rect.h),
        (full.x, full.y, full.w, full.h),
        "not the stub's whole-screen placement",
    );
    assert!(
        Focusable::<TestHost>::place(&page, &99u32, &cx(), At::Drawn).is_none(),
        "an out-of-range row does not place at all",
    );
}

/// **(D2 repro)** §7.3 step 5: the ENGINE owns the current element; the owner's `step` only reacts
/// to a `FocusMoved` it is told about. On 2790f47a nothing in `step` handled this event, so a
/// panel's own cursor could never follow an engine-driven move at all.
#[test]
fn focus_moved_writes_the_new_cursor_into_the_open_panel() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    assert_eq!(page.sel(), 0);
    let (_, reqs, _) = deliver(
        &mut page,
        ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: 1 }, by: By::Dir },
    );
    assert_eq!(page.sel(), 1, "the panel's cursor follows the engine's FocusMoved");
    assert!(
        reqs.iter().any(|r| matches!(r, PlayerReq::ExtendHud(_))),
        "a moved cursor keeps the transport up for a menu's read time",
    );
}

/// **(D2 repro)** `Tracks`' and `More`'s rows answer `ElemKind::Bare`: a key-down or pointer-click
/// `Activate` commits immediately — the engine's own mechanism replacing the old ladder's direct
/// `Key::Ok` arms.
#[test]
fn activate_commits_the_bare_rows_directly() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Tracks { tab: 0 });
    let (_, _, dismissed) = activate(&mut page, 0);
    assert!(dismissed, "Tracks: Activate commits and dismisses");

    // an Options row: the root's first row is the Quality drill-in, which opens a page instead
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::More { quality: false });
    let (_, reqs, dismissed) = more_open(&mut page, MoreRow::Act(MoreAction::ToggleStats));
    assert!(dismissed, "More: Activate commits and dismisses");
    assert!(
        reqs.iter().any(|r| matches!(r, PlayerReq::More(_))),
        "More: Activate reports its action",
    );
}

/// `resolve_menupick_row` is the `/tmp/plxnative-menupick` trigger's own parser: a plain number
/// always wins (the original "row N" contract, on either tab); a name is only ever tried on the
/// Audio tab, and an unrecognized one — or a name asked of the Subtitles tab, which has no such
/// map — resolves to nothing, which `menupick_arm` turns into its "unknown target" log rather than
/// a commit.
#[test]
fn resolve_menupick_row_parses_a_number_or_an_audio_tab_name() {
    let ps = crate::route::PlaybackSession::IDLE;
    let audio_page =
        PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Tracks { tab: 0 });
    assert_eq!(audio_page.resolve_menupick_row("3"), Some(3), "a plain row number always resolves");
    // no enhancement offered on an idle session with no playing item: the names resolve to nothing
    assert_eq!(audio_page.resolve_menupick_row("boost"), None);
    assert_eq!(audio_page.resolve_menupick_row("loudness"), None);
    assert_eq!(audio_page.resolve_menupick_row("not-a-thing"), None);

    let sub_page =
        PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    assert_eq!(sub_page.resolve_menupick_row("2"), Some(2), "a plain row number still resolves on Subtitles");
    assert_eq!(sub_page.resolve_menupick_row("boost"), None, "names are an Audio-tab-only contract");
}

/// Focus `id` on the Subtitles panel the way the engine would (a `FocusMoved`), then press OK on it
/// the way a click does (`Activate`, the row's key).
fn open_row(page: &mut PlayerOverlayScreen, id: TrackRow) -> (Handled, Vec<PlayerReq>, bool) {
    let key = id.key().0;
    deliver(page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: key }, by: By::Dir });
    activate(page, key)
}

fn tracks_path(page: &PlayerOverlayScreen) -> Vec<TrackPage> {
    let Panel::Tracks(menu) = page.panel() else { panic!("Tracks panel") };
    menu.page_path()
}

fn tracks_selected(page: &PlayerOverlayScreen) -> Option<TrackRow> {
    let Panel::Tracks(menu) = page.panel() else { panic!("Tracks panel") };
    menu.selected_id()
}

/// **A Style pick commits and leaves the Subtitles panel UP; Style and its fields are drill-ins.**
/// The tone is found by picking and watching the caption change; every track row (including Off)
/// still commits and closes. OK on a Nav row pushes a page and asks for nothing.
#[test]
fn a_style_pick_commits_without_dismissing_the_tracks_panel() {
    let _g = crate::testlock::serial(); // the panel seeds its tone from the player's global
    crate::player::restore_subtitle_tone(crate::plex::session::SubtitleTone::White);
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    // no playing item: Off, then the headerless Timing + Style section; the element is the row's KEY
    let (_, reqs, dismissed) = open_row(&mut page, TrackRow::Style);
    assert!(!dismissed, "a Nav row keeps the panel open");
    assert!(!reqs.iter().any(|r| matches!(r, PlayerReq::CommitTrack(_))), "and commits nothing: {reqs:?}");
    assert_eq!(tracks_path(&page), [TrackPage::Style]);
    let (_, _, dismissed) = open_row(&mut page, TrackRow::OpenField(StyleField::Color));
    assert!(!dismissed);
    assert_eq!(tracks_path(&page), [TrackPage::Style, TrackPage::Picker(StyleField::Color)]);
    assert_eq!(tracks_selected(&page), Some(TrackRow::Choice(StyleField::Color, 0)), "opens on the checked tone");

    let (_, reqs, dismissed) = open_row(&mut page, TrackRow::Choice(StyleField::Color, 1));
    assert!(!dismissed, "a Style pick keeps the panel open");
    assert!(reqs.iter().any(|r| matches!(
        r,
        PlayerReq::CommitTrack(crate::ui::track_menu::TrackCommit::SubtitleTone(
            crate::plex::session::SubtitleTone::Silver
        ))
    )));
    assert!(reqs.iter().any(|r| matches!(r, PlayerReq::ExtendHud(_))));
    assert_eq!(tracks_path(&page).len(), 2, "and stays on the picker page");

    // BACK twice returns to the root, where Off still commits and closes
    press(&mut page, 0, WCODE_BACK, Edge::Down);
    press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert!(tracks_path(&page).is_empty());
    let (_, _, dismissed) = open_row(&mut page, TrackRow::SubOff);
    assert!(dismissed, "Off still commits and closes");
}

/// **Nav keys through the surface**: BACK and LEFT pop a sub-page (focus returns to the opener by
/// id), BACK on the root dismisses, LEFT on the root is still the tab switch, and RIGHT on a Nav row
/// enters it (and on anything else is the tab switch, as before).
#[test]
fn back_and_left_pop_a_sub_page_and_right_enters_a_nav_row() {
    use crate::ui::consts::{SDLK_LEFT, SDLK_RIGHT};
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });

    // RIGHT on the Style row enters the page (an edge re-delivery, as the engine does)
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: TrackRow::Style.key().0 }, by: By::Dir });
    let (handled, _, dismissed) = press_at_edge(&mut page, SDLK_RIGHT);
    assert_eq!(handled, Handled::Yes);
    assert!(!dismissed);
    assert_eq!(tracks_path(&page), [TrackPage::Style]);

    // LEFT pops and lands back on the opener
    let (_, _, dismissed) = press_at_edge(&mut page, SDLK_LEFT);
    assert!(!dismissed, "LEFT on a sub-page is back, not a tab switch or a dismissal");
    assert!(tracks_path(&page).is_empty());
    assert_eq!(tracks_selected(&page), Some(TrackRow::Style), "focus returned to the opener by id");

    // BACK pops a sub-page but dismisses the root
    open_row(&mut page, TrackRow::Style);
    let (_, _, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert!(!dismissed, "BACK on a sub-page pops");
    assert!(tracks_path(&page).is_empty());
    let (_, _, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert!(dismissed, "BACK on the root dismisses, as before");

    // root LEFT is still the tab switch: Subtitles -> Audio
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    press_at_edge(&mut page, SDLK_LEFT);
    let Panel::Tracks(menu) = page.panel() else { panic!("Tracks panel") };
    assert_eq!(menu.ids(), vec![], "the Audio tab of an empty item has no rows: LEFT switched tabs");
}

/// **Clicking the "< TITLE" band pops**, acts on no row, and the band is a pointer-only stop: it is
/// registered with the hit map and placeable for replay, but it is not in the D-pad column.
#[test]
fn clicking_the_title_band_pops_one_page() {
    use crate::ui::screen::DrawFrame;
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });

    let stops_of = |page: &PlayerOverlayScreen| {
        let cx = cx();
        let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
        page.record_stops(&mut f);
        f.into_stops()
    };
    assert!(stops_of(&page).iter().all(|s| s.key.elem != TITLE_KEY), "the root has no title band");

    open_row(&mut page, TrackRow::Style);
    let stops = stops_of(&page);
    let title = stops.iter().find(|s| s.key.elem == TITLE_KEY).expect("a pushed page registers its title stop");
    assert_eq!(title.hover, crate::ui::screen::Hover::Ignore, "pointer-only: hovering it moves no focus");
    let cx = cx();
    let placed = Focusable::<TestHost>::place(&page, &TITLE_KEY, &cx, At::Drawn).expect("replay can place the title key");
    assert_eq!((placed.rect.x, placed.rect.y), (title.rect.x, title.rect.y), "place and stop agree");
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&page, &cx, &mut groups);
    assert_eq!(Focusable::<TestHost>::group_of(&page, &TITLE_KEY, &cx), None, "never in the D-pad column");

    let (_, reqs, dismissed) = activate(&mut page, TITLE_KEY);
    assert!(!dismissed);
    assert!(!reqs.iter().any(|r| matches!(r, PlayerReq::CommitTrack(_))), "a title click commits nothing");
    assert!(tracks_path(&page).is_empty(), "the click went back one page");
    assert_eq!(tracks_selected(&page), Some(TrackRow::Style));
}

/// **The panel holds the pointer while it moves and releases it at rest**, and a stop registered
/// during the slide sits where the page is drawn that frame (the slide's offset), not at its
/// layout.
#[test]
fn the_pointer_is_held_while_a_page_slides_and_released_at_rest() {
    use crate::ui::screen::{DrawFrame, Screen};
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    let tick = |page: &mut PlayerOverlayScreen, ms: u32| {
        crate::ui::idle::frame_begin(1.0 / 60.0);
        deliver(page, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    };
    tick(&mut page, 1_000);
    assert!(!Screen::<TestHost>::pointer_held(&page), "a panel at rest takes the pointer");

    open_row(&mut page, TrackRow::Style);
    assert!(Screen::<TestHost>::pointer_held(&page), "a push holds the pointer from the first frame");
    let stops_of = |page: &PlayerOverlayScreen| {
        let cx = cx();
        let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
        page.record_stops(&mut f);
        f.into_stops()
    };
    tick(&mut page, 1_016);
    let mid = stops_of(&page);
    let title = mid.iter().find(|s| s.key.elem == TITLE_KEY).expect("title stop");
    assert!(title.rect.x > title.rest_rect.x, "mid-slide the stop is where the page is DRAWN (arriving from the right)");
    let cx = cx();
    let placed = Focusable::<TestHost>::place(&page, &TITLE_KEY, &cx, At::Drawn).expect("place");
    assert_eq!((placed.rect.x, placed.clip.x), (title.rect.x, title.clip.x), "place and stop agree mid-slide");

    for i in 0..240 {
        tick(&mut page, 1_032 + i * 16);
    }
    assert!(!Screen::<TestHost>::pointer_held(&page), "released once settled");
    let rest = stops_of(&page);
    let title = rest.iter().find(|s| s.key.elem == TITLE_KEY).expect("title stop");
    assert_eq!(title.rect, title.rest_rect, "at rest the stop IS its layout");
}

/// **The replay canon tells pages and return stacks apart**: the tab, the page path, each opener
/// and the selected KEY all move the hash; the same state hashes the same.
#[test]
fn the_replay_canon_includes_the_page_path_and_return_ids() {
    use crate::ui::machine::{Canon, LogicalState};
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let fp = |page: &PlayerOverlayScreen| {
        let mut c = Canon::new();
        page.write(&mut c);
        c.finish()
    };
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    let root = fp(&page);
    assert_eq!(root, fp(&PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 })));
    open_row(&mut page, TrackRow::Style);
    let style = fp(&page);
    assert_ne!(style, root, "a pushed page is not the root");
    open_row(&mut page, TrackRow::OpenField(StyleField::Size));
    let size = fp(&page);
    open_row(&mut page, TrackRow::Choice(StyleField::Size, 0)); // inert re-pick keeps the page
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: TrackRow::Choice(StyleField::Size, 2).key().0 }, by: By::Dir });
    let moved = fp(&page);
    assert_ne!(moved, size, "the selected key is in the canon");
    press(&mut page, 0, WCODE_BACK, Edge::Down);
    assert_eq!(fp(&page), style, "popping returns to the Style page's own state");

    // the same page reached from a different opener is a different state
    let mut other = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    open_row(&mut other, TrackRow::Style);
    open_row(&mut other, TrackRow::OpenField(StyleField::Color));
    open_row(&mut page, TrackRow::OpenField(StyleField::Size));
    assert_ne!(fp(&other), fp(&page));
}

/// **OK on the dim Timing row while subtitles are Off is inert — it neither opens the capsule
/// nor closes the panel.** The row reads "nothing to time"; an OK that silently dismissed the
/// panel would throw away the viewer's place for no effect at all.
#[test]
fn ok_on_the_dim_timing_row_while_off_keeps_the_panel_open() {
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    crate::player::set_subtitle_offset(0);
    let ps = crate::route::PlaybackSession::IDLE;
    let meta = crate::stores::metadata::MetadataStore::default();
    let mut page = PlayerOverlayScreen::new(&ps, meta.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    // no playing item: Off, then Timing, then Color — and Off is the checked row
    let timing = TrackRow::Timing.key().0;
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: timing }, by: By::Dir });
    {
        let Panel::Tracks(menu) = page.panel() else { panic!("Tracks panel") };
        assert_eq!(menu.selected_id(), Some(TrackRow::Timing), "the key names the Timing row");
    }
    let (_, reqs, dismissed) = activate(&mut page, timing);
    assert!(!dismissed, "an inert row keeps the panel up");
    assert!(
        !reqs.iter().any(|r| matches!(r, PlayerReq::OpenOverlay(_) | PlayerReq::CommitTrack(_))),
        "and asks for nothing: {reqs:?}",
    );
}

/// A playing item whose audio is English and whose subtitles are `subs`: the audio's language is
/// "yours" (`subtitle_yours_langs`), so English subtitles are rows on the Subtitles root, not behind
/// "Other languages".
fn english_audio_with_subs(ps: &crate::route::PlaybackSession, subs: Vec<crate::metadata::Stream>) -> crate::metadata::PlayingItem {
    let mut item = crate::metadata::PlayingItem::with_subs(subs);
    item.audio = vec![crate::metadata::Stream {
        id: crate::route::cur_audio_sid(ps),
        lang: "English".into(),
        lang_code: "eng".into(),
        codec: "ac3".into(),
        ..Default::default()
    }];
    item
}

/// **The Timing hand-off.** Selecting the Subtitles menu's own Timing row while a subtitle is
/// active returns `TrackOk::OpenTiming` (`ui::track_menu`'s own
/// `timing_returns_open_timing_once_a_subtitle_is_active_and_is_inert_while_off`); `activate`'s
/// Tracks arm spends that by dismissing the Tracks panel and asking for the capsule to open in its
/// place, WITHOUT the read-time `ExtendHud` every ordinary commit raises — the capsule owns its own
/// visible time (it hides the HUD outright, `OverlayKind::hud_policy`), so extending a HUD it is
/// about to hide would be dead motion.
#[test]
fn open_timing_dismisses_tracks_and_opens_the_capsule_with_no_extend_hud() {
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    crate::player::set_subtitle_offset(0);
    let ps = crate::route::PlaybackSession::IDLE;
    let mut store = crate::stores::metadata::MetadataStore::default();
    assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
        english_audio_with_subs(&ps, vec![crate::metadata::Stream {
            id: 1,
            index: 0,
            lang: "English".into(),
            lang_code: "eng".into(),
            codec: "srt".into(),
            ..Default::default()
        }]),
    ))));
    let mut page = PlayerOverlayScreen::new(&ps, store.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    let (sub_row, timing_row) = (TrackRow::Sub(0).key().0, TrackRow::Timing.key().0);
    // Select the subtitle track first — Timing is inert while subtitles are Off.
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: sub_row }, by: By::Dir });
    activate(&mut page, sub_row);
    deliver(&mut page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: ENTRY, elem: timing_row }, by: By::Dir });
    let (_, reqs, dismissed) = activate(&mut page, timing_row);
    assert!(dismissed, "OpenTiming dismisses the Tracks panel");
    assert_eq!(
        reqs,
        vec![PlayerReq::OpenOverlay(OverlayKind::Timing)],
        "and asks for exactly the capsule to open — no ExtendHud alongside it",
    );
}

/// The Timing overlay's own word and slot, read by the same heartbeat/replay machinery every
/// other `OverlayKind` answers through (`app::words`, `PlayerOverlayArg::write`).
#[test]
fn timings_word_and_slot() {
    assert_eq!(OverlayKind::Timing.word(), "timing");
    assert_eq!(OverlayKind::Timing.slot(), 4);
}

/// LEFT/RIGHT on the capsule commit a new `SubtitleOffset` through the ordinary `CommitTrack`
/// path — the same request every other Tracks row's step uses, so the loop's one handler serves
/// both.
#[test]
fn timing_left_and_right_commit_subtitle_offset() {
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    crate::player::set_subtitle_offset(0);
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Timing);
    let (handled, reqs, dismissed) = press(&mut page, crate::ui::consts::SDLK_RIGHT, 0, Edge::Down);
    assert_eq!(handled, Handled::Yes, "the capsule owns every key, never falls through");
    assert!(!dismissed);
    assert_eq!(
        reqs,
        vec![PlayerReq::CommitTrack(crate::ui::track_menu::TrackCommit::SubtitleOffset(100))],
        "RIGHT steps +100ms",
    );

    let (_, reqs, _) = press(&mut page, crate::ui::consts::SDLK_LEFT, 0, Edge::Down);
    assert_eq!(
        reqs,
        vec![PlayerReq::CommitTrack(crate::ui::track_menu::TrackCommit::SubtitleOffset(0))],
        "LEFT steps back down",
    );
}

/// OK and BACK both close the capsule (`TimingCapsule::key`'s `Key::Ok | Key::Back` arm) —
/// dismissing the surface with the offset kept: neither key raises a reset commit, and neither
/// asks for anything else (the transport staying down is `PlayerScreen::set_hud_policy`'s, for
/// every way the surface can close).
#[test]
fn timing_ok_and_back_close_and_keep_the_offset() {
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    crate::player::set_subtitle_offset(300);
    let ps = crate::route::PlaybackSession::IDLE;
    for (sym, wcode) in [(SDLK_RETURN, 0), (0, WCODE_BACK)] {
        let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Timing);
        let (handled, reqs, dismissed) = press(&mut page, sym, wcode, Edge::Down);
        assert_eq!(handled, Handled::Yes);
        assert!(dismissed, "sym={sym} wcode={wcode}: closes the capsule");
        assert_eq!(reqs, vec![], "sym={sym} wcode={wcode}: no offset reset alongside it");
    }
}

/// PLAY/PAUSE still reach the player while the capsule is up — the same transport exception the
/// three reading panels get (`a_transport_key_is_forwarded_by_a_modal_panel_and_leaves_it_up`),
/// checked before the Timing branch in `key()` so the capsule never swallows it.
#[test]
fn timing_forwards_transport_and_leaves_the_capsule_up() {
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    crate::player::set_subtitle_offset(0);
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Timing);
    let (handled, reqs, dismissed) = press(&mut page, 0, WCODE_PAUSE, Edge::Down);
    assert_eq!(handled, Handled::Yes);
    assert_eq!(reqs, vec![PlayerReq::Transport(Some(false))]);
    assert!(!dismissed, "the capsule stays up under a transport key");
}

/// No `ExtendHud` is ever raised while the capsule is the active panel's `Tick` — the capsule
/// itself hides the HUD (`OverlayKind::extends_hud` is false), so keeping it "alive" for the transport's own
/// read time would fight that.
#[test]
fn timing_ticks_do_not_extend_the_hud() {
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    crate::player::set_subtitle_offset(0);
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Timing);
    let (_, reqs, _) = deliver(&mut page, ScreenEvent::Tick(Tick { ms: 1_016, dt_us: 16_000 }));
    assert!(
        !reqs.iter().any(|r| matches!(r, PlayerReq::ExtendHud(_))),
        "a Timing tick must not extend the HUD it is itself hiding",
    );
}

/// **Info's split, kept from the old ladder's `Key::Ok if p.focus_is_ctl()` arm** (restructure
/// phase 12): a POINTER click's `Activate` is already a precise, instantaneous gesture, so it
/// applies the card's action at once; a keyboard OK arms the engine's own (non-holdable) press and
/// only reaches here as `PressCommit`, on release — which defers instead to the loop's tvOS dip
/// (`PlayerReq::ArmInfoPress`, read back on the spring-back by `commit_info_press`).
#[test]
fn infos_activate_and_press_commit_take_different_roads() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    let (_, reqs, dismissed) = activate(&mut page, 0);
    assert!(dismissed, "a pointer click applies the card's action at once");
    assert!(reqs.iter().any(|r| matches!(r, PlayerReq::Info(_))));

    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    let (_, reqs, dismissed) = press_commit(&mut page, 0);
    assert!(!dismissed, "a keyboard OK defers instead of acting now");
    assert!(reqs.iter().any(|r| matches!(r, PlayerReq::ArmInfoPress)));
}

/// `Chapters`' cards answer `ElemKind::Card` (a holdable press for the keyboard): OK's own
/// `PressCommit` on release seeks and dismisses, exactly as the old ladder's `Key::Ok` did.
#[test]
fn chapters_press_commit_seeks_and_dismisses() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Chapters);
    let (_, _, dismissed) = press_commit(&mut page, 0);
    assert!(
        dismissed,
        "Chapters: PressCommit (the Card's release) dismisses regardless of the seek target",
    );
}

/// **The one thing a panel still decides outside its own scope**: the engine re-delivers a
/// direction at this panel's group EDGE (`EdgeRule::Screen`), and Tracks answers by switching its
/// tab rather than moving within the group — `TrackMenuState::focus_tab`, unchanged from the old
/// ladder's LEFT/RIGHT arm.
#[test]
fn tracks_edge_key_switches_tab_instead_of_moving_within_the_group() {
    use crate::ui::consts::SDLK_RIGHT;
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Tracks { tab: 0 });
    assert!(matches!(page.kind(), OverlayKind::Tracks { .. }));
    let (handled, ..) = press_at_edge(&mut page, SDLK_RIGHT);
    assert_eq!(handled, Handled::Yes, "the panel answers its own edge crossing");
    assert!(matches!(page.panel(), Panel::Tracks(_)), "still the same panel, just retabbed");
}

/// **`FocusSource::Engine`/`HitSource::Engine` (restructure phase 12, D2 Part A)** — this surface
/// registers through the engine's own hit map and `Focusable` bookkeeping, rather than being
/// invisible to both the way a `Legacy` screen is.
#[test]
fn the_overlay_answers_engine_for_both_focus_and_hits() {
    use crate::ui::screen::{FocusSource, HitSource, Screen};
    let ps = crate::route::PlaybackSession::IDLE;
    let page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    assert_eq!(Screen::<TestHost>::focus_source(&page), FocusSource::Engine);
    assert_eq!(Screen::<TestHost>::hit_source(&page), HitSource::Engine);
}

/// One group seats at its real cursor and reports back to itself through `group_of` — the same
/// round-trip property `screens/player/mod.rs`'s own `Focusable` suite pins for its four.
#[test]
fn the_active_groups_seat_round_trips_through_group_of() {
    let ps = crate::route::PlaybackSession::IDLE;
    let page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Info);
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&page, &cx(), &mut groups);
    assert_eq!(groups.len(), 1, "one focus group for the action column");
    let seated = Focusable::<TestHost>::seat(
        &page,
        groups[0].id,
        Placed {
            rect: crate::ui::Rect::FULL,
            rest_rect: crate::ui::Rect::FULL,
            clip: crate::ui::Rect::FULL,
            index: None,
        },
        &cx(),
    );
    assert_eq!(
        Focusable::<TestHost>::group_of(&page, &seated.elem, &cx()),
        Some(GroupId(0)),
    );
}

/// **The player's panels dim through the container, inheriting the PLAYING item's own light.** GL
/// cannot read the video plane, so the track menu and the `…` menu ask for a dim over
/// [`UnderlaySource::Corners`](crate::ui::screen::UnderlaySource::Corners) — the leaf's UltraBlur
/// envelope — at their `theme::underlay` roles; the Info card and Chapters strip ask for none, as
/// they drew none; and an item with no envelope falls back to the flat ink.
///
/// Observed RED before this package: every kind answered `Scrim::NONE` (the dims were hand-drawn
/// inside `TrackMenuState::draw`/`MoreMenuState::draw`), so the first assertion failed at 0.0.
#[test]
fn the_player_panels_dim_through_the_container_from_the_playing_items_corners() {
    use crate::ui::screen::{Screen, UnderlaySource};
    use crate::ui::theme::underlay::{DIM_PLAYER, DIM_SHEET};
    let ps = crate::route::PlaybackSession::IDLE;
    let corners = [[0.1, 0.5, 0.2], [0.2, 0.4, 0.1], [0.6, 0.2, 0.1], [0.1, 0.1, 0.4]];
    let mut store = crate::stores::metadata::MetadataStore::default();
    assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(crate::metadata::PlayingItem {
        sid: crate::plex::ServerId::from_raw(0), rk: "rk".into(), show_rk: String::new(), audio: Vec::new(), subs: Vec::new(),
        video_fps: 0.0, width: 0, height: 0, bitrate: 0, dovi: Default::default(),
        markers: Vec::new(), chapters: Vec::new(), blur: Some(corners),
    }))));
    for (kind, alpha) in [
        (OverlayKind::Tracks { tab: 0 }, DIM_PLAYER),
        (OverlayKind::More { quality: false }, DIM_SHEET),
        (OverlayKind::Info, 0.0),
        (OverlayKind::Chapters, 0.0),
    ] {
        let page = PlayerOverlayScreen::new(&ps, store.view(), ENTRY, kind);
        let scrim = Screen::<TestHost>::scrim(&page);
        assert_eq!(scrim.alpha, alpha, "{}: its role's weight", kind.word());
        if alpha > 0.0 {
            assert_eq!(scrim.source, UnderlaySource::Corners(corners), "{}: the item's own light", kind.word());
        }
    }
    let bare = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, OverlayKind::Tracks { tab: 0 });
    assert_eq!(Screen::<TestHost>::scrim(&bare).source, UnderlaySource::Flat, "no envelope: the flat ink");
}

/// **A pointer click on a row activates THAT row** — the keys the panel registers as hit stops, the
/// keys its `Focusable` places, and what `Activate(key)` spends all agree. The cursor is left on
/// Off (never moved by a `FocusMoved`), so a click that still read the cursor would commit the
/// wrong row; each stop is resolved at its own centre through the hit map the way the dispatcher
/// does, then activated by the key the map returned.
#[test]
fn a_pointer_click_activates_the_row_it_hit_by_key() {
    use crate::ui::hit::HitMap;
    use crate::ui::screen::DrawFrame;
    let _g = crate::testlock::serial();
    crate::player::sidecar::reset();
    let ps = crate::route::PlaybackSession::IDLE;
    let mut store = crate::stores::metadata::MetadataStore::default();
    let sub = |id: i64, index: i64, lang: &str, code: &str| crate::metadata::Stream {
        id,
        index,
        lang: lang.into(),
        lang_code: code.into(),
        codec: "srt".into(),
        ..Default::default()
    };
    assert!(store.run(crate::stores::metadata::MetadataCmd::InstallPlaying(Some(
        english_audio_with_subs(&ps, vec![sub(11, 0, "English", "eng"), sub(22, 1, "English", "eng")]),
    ))));
    let mut page = PlayerOverlayScreen::new(&ps, store.view(), ENTRY, OverlayKind::Tracks { tab: 1 });
    let cx = cx();
    let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
    page.record_stops(&mut f);
    let stops = f.into_stops();
    let mut map = HitMap::new();
    map.fill(stops.clone());
    map.swap();

    let Panel::Tracks(menu) = page.panel() else { panic!("Tracks panel") };
    let mut keys = menu.keys();
    assert!(keys.contains(&TrackRow::Sub(1).key().0), "premise: both tracks are rows");
    let mut stop_keys: Vec<u32> = stops.iter().map(|s| s.key.elem).collect();
    keys.sort_unstable();
    stop_keys.sort_unstable();
    assert_eq!(stop_keys, keys, "every focusable row registers exactly one stop, under its key");
    for stop in &stops {
        let (x, y) = (stop.rect.x + stop.rect.w / 2.0, stop.rect.y + stop.rect.h / 2.0);
        assert_eq!(map.top_at(x, y).map(|s| s.key.elem), Some(stop.key.elem), "the hit map resolves the row's centre");
        let placed = Focusable::<TestHost>::place(&page, &stop.key.elem, &cx, At::Drawn).expect("placed");
        assert_eq!((placed.rect.x, placed.rect.y), (stop.rect.x, stop.rect.y), "place and stop agree");
    }

    // click the second English track (subs index 1) while the cursor still sits on Off
    let second = TrackRow::Sub(1).key().0;
    let (_, reqs, dismissed) = activate(&mut page, second);
    assert!(dismissed, "a track pick closes the panel");
    assert!(reqs.iter().any(|r| matches!(r, PlayerReq::CommitTrack(crate::ui::track_menu::TrackCommit::Subtitle { .. }))));
    // (the harness host's metadata view is empty, so the commit carries no stream id; the id the
    // panel resolved the key to is what it records as the checked track)
    let Panel::Tracks(menu) = page.panel() else { panic!("Tracks panel") };
    assert_eq!(menu.active_sub(), 1, "the click picked the second English track (subs index 1), not the row under the cursor");
    assert_eq!(menu.selected_id(), Some(TrackRow::Sub(1)));
}

/// **Issue #162's census, for the four panels over the player**: every row a panel's `Focusable`
/// declares — what the D-pad walks — is clickable with the pointer over its whole visible rect,
/// against the map the panel's own paint-free `record_stops` fills. It grades the HIT MAP only —
/// what a click there spends is `activate_commits_the_bare_rows_directly`'s — and with the empty
/// test metadata the Tracks and Chapters panels declare no rows, so for those two it is vacuous.
#[test]
fn every_panel_row_the_dpad_reaches_is_clickable_with_the_pointer() {
    use crate::ui::hit::{pointer_gaps, HitMap};
    use crate::ui::screen::DrawFrame;
    let _g = crate::testlock::serial();
    let ps = crate::route::PlaybackSession::IDLE;
    for kind in [
        OverlayKind::Tracks { tab: 0 },
        OverlayKind::Tracks { tab: 1 },
        OverlayKind::Info,
        OverlayKind::Chapters,
        OverlayKind::More { quality: false },
        OverlayKind::More { quality: true },
    ] {
        let page = PlayerOverlayScreen::new(&ps, crate::stores::metadata::MetadataStore::default().view(), ENTRY, kind);
        let cx = cx();
        let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
        page.record_stops(&mut f);
        let mut map = HitMap::new();
        map.fill(f.into_stops());
        map.swap();
        let mut groups = Vec::new();
        Focusable::<TestHost>::groups(&page, &cx, &mut groups);
        let mut rows = Vec::new();
        for g in &groups {
            // the More and Tracks menus' elements are row identities; every other panel's are positions
            let elems: Vec<u32> = match page.panel() {
                Panel::More(p) => p.keys(),
                Panel::Tracks(p) => p.keys(),
                _ => (0..g.len as u32).collect(),
            };
            for elem in elems {
                let p = Focusable::<TestHost>::place(&page, &elem, &cx, At::Drawn)
                    .unwrap_or_else(|| panic!("{kind:?}: row {elem} is declared but does not place"));
                let visible = p.rect.intersect(p.clip);
                if visible.w > 0.0 && visible.h > 0.0 {
                    rows.push((FocusKey { entry: ENTRY, elem }, visible));
                }
            }
        }
        if matches!(kind, OverlayKind::Info | OverlayKind::More { .. }) {
            assert!(!rows.is_empty(), "{kind:?} always has rows to press");
        }
        let gaps = pointer_gaps(&mut map, ENTRY, &rows);
        assert!(gaps.is_empty(), "{kind:?}: rows the pointer cannot click:\n{}", gaps.join("\n"));
    }
}

/// **`OverlayKind::ALL` lists every kind exactly once**, and the census derived from it cannot
/// silently miss a new one: the `match` below names every variant with no wildcard, so adding a
/// variant fails to compile HERE — the reminder to add it to `ALL` as well — and the slots `ALL`
/// covers must be exactly `0..ALL.len()`, which a kind left out of `ALL` would break.
#[test]
fn every_kind_is_listed_once() {
    let listed = |k: OverlayKind| match k {
        OverlayKind::Tracks { .. }
        | OverlayKind::Info
        | OverlayKind::Chapters
        | OverlayKind::More { .. }
        | OverlayKind::Timing => OverlayKind::ALL.iter().any(|a| a.slot() == k.slot()),
    };
    let mut slots: Vec<u8> = OverlayKind::ALL.iter().map(|k| k.slot()).collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..OverlayKind::ALL.len() as u8).collect::<Vec<_>>());
    assert!(OverlayKind::ALL.into_iter().all(listed));
}
