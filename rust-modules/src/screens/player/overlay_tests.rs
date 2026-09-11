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
use crate::ui::consts::{SDLK_DOWN, SDLK_RETURN, SDLK_UP, WCODE_BACK, WCODE_PAUSE, WCODE_PLAY, WCODE_PLAYPAUSE};
use crate::ui::fixture::FixtureMeasure;
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
            let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
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
        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
        let (handled, reqs, _) = press(&mut page, SDLK_UP, 0, Edge::Down);
        assert_eq!(handled, Handled::No, "{name}: UP is now the engine's to move");
        assert!(
            !reqs.iter().any(|r| matches!(r, PlayerReq::Transport(_))),
            "{name}: UP is not a transport key",
        );

        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
        let (handled, ..) = press(&mut page, SDLK_RETURN, 0, Edge::Down);
        assert_eq!(handled, Handled::No, "{name}: OK is the engine's Activate/PressArm to answer");

        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
        let (handled, _, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
        assert_eq!(handled, Handled::Yes, "{name}: BACK is still the panel's");
        assert!(dismissed, "{name}: and BACK is what closes it");
    }
}

/// `More` keeps the old swallow-everything answer, for the reason it always had: the transport
/// exception was reported and reproduced against the other three, and this popover's rows include
/// the failure read-out's own recovery path.
#[test]
fn the_options_popover_keeps_the_old_swallow_everything_behaviour() {
    let ps = crate::route::PlaybackSession::IDLE;
    for wcode in [WCODE_PAUSE, WCODE_PLAY, WCODE_PLAYPAUSE] {
        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::More { quality: false });
        let (handled, reqs, dismissed) = press(&mut page, 0, wcode, Edge::Down);
        assert_eq!(handled, Handled::Yes);
        assert!(
            reqs.is_empty(),
            "More is excluded from the transport exception (wcode {wcode})",
        );
        assert!(!dismissed);
    }
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
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::More { quality: false });
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
        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
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
    let page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
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
    let page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
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
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
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
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Tracks { tab: 0 });
    let (_, _, dismissed) = activate(&mut page, 0);
    assert!(dismissed, "Tracks: Activate commits and dismisses");

    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::More { quality: false });
    let (_, reqs, dismissed) = activate(&mut page, 0);
    assert!(dismissed, "More: Activate commits and dismisses");
    assert!(
        reqs.iter().any(|r| matches!(r, PlayerReq::More(_))),
        "More: Activate reports its action",
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
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
    let (_, reqs, dismissed) = activate(&mut page, 0);
    assert!(dismissed, "a pointer click applies the card's action at once");
    assert!(reqs.iter().any(|r| matches!(r, PlayerReq::Info(_))));

    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
    let (_, reqs, dismissed) = press_commit(&mut page, 0);
    assert!(!dismissed, "a keyboard OK defers instead of acting now");
    assert!(reqs.iter().any(|r| matches!(r, PlayerReq::ArmInfoPress)));
}

/// `Chapters`' cards answer `ElemKind::Card` (a holdable press for the keyboard): OK's own
/// `PressCommit` on release seeks and dismisses, exactly as the old ladder's `Key::Ok` did.
#[test]
fn chapters_press_commit_seeks_and_dismisses() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Chapters);
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
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Tracks { tab: 0 });
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
    let page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
    assert_eq!(Screen::<TestHost>::focus_source(&page), FocusSource::Engine);
    assert_eq!(Screen::<TestHost>::hit_source(&page), HitSource::Engine);
}

/// One group seats at its real cursor and reports back to itself through `group_of` — the same
/// round-trip property `screens/player/mod.rs`'s own `Focusable` suite pins for its four.
#[test]
fn the_active_groups_seat_round_trips_through_group_of() {
    let ps = crate::route::PlaybackSession::IDLE;
    let page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::Info);
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
