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
//! What it still cannot say is whether the panel visually stays up; that is a device check.

use super::overlay::{OverlayKind, PlayerOverlayScreen};
use crate::screens::registry::{AppFx, AppMsg, PageMemory, PlayerReq};
use crate::ui::consts::{
    SDLK_DOWN, SDLK_UP, WCODE_BACK, WCODE_PAUSE, WCODE_PLAY, WCODE_PLAYPAUSE,
};
use crate::ui::fixture::FixtureMeasure;
use crate::ui::machine::{
    Cx, Edge, Effects, EntryId, Fx, Handled, Host, InputEvent, InputKind, InputOwner, InstanceId,
    Machine, MachineId, NavOp, Source, Tick,
};
use crate::ui::screen::ScreenEvent;

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

/// One key press through the surface. Returns `(handled, the app requests it raised, whether it
/// asked the container to dismiss it)` — the three things a caller of the old predicate had to
/// derive from a bool plus the arm it fell into.
fn press(
    page: &mut PlayerOverlayScreen,
    sym: u32,
    wcode: u32,
    edge: Edge,
) -> (Handled, Vec<PlayerReq>, bool) {
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let ev = ScreenEvent::<TestHost>::Input(InputEvent {
        kind: InputKind::Key {
            key: crate::ui::machine::Key::Other,
            sym,
            wcode,
            edge,
            at_edge: false,
        },
        at: Tick {
            ms: 1_000,
            dt_us: 0,
        },
        source: Source::Sdl,

    });
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

/// The other half of the old predicate: everything that is not a transport key is still the
/// panel's own, and OK/BACK are the two that end it.
#[test]
fn every_other_key_is_still_the_panels_own() {
    let ps = crate::route::PlaybackSession::IDLE;
    for (kind, name) in MODAL {
        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
        let (handled, reqs, _) = press(&mut page, SDLK_UP, 0, Edge::Down);
        assert_eq!(handled, Handled::Yes, "{name}: UP is the panel's");
        assert!(
            !reqs.iter().any(|r| matches!(r, PlayerReq::Transport(_))),
            "{name}: UP is not a transport key",
        );
        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
        let (handled, _, dismissed) = press(&mut page, 0, WCODE_BACK, Edge::Down);
        assert_eq!(handled, Handled::Yes, "{name}: BACK is the panel's");
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

/// **The held-direction cadence, which moved WITH the input.** The loop paced these four lists at
/// 110 ms from its own `HeldKey` timer; the hardware streams `Edge::Repeat` at ~50 ms, so without
/// a gate of its own the surface would walk a menu twice as fast as every other list in the app.
/// A FRESH press is never swallowed by the press before it, which is what `rearm` is for.
#[test]
fn a_held_direction_is_paced_and_a_fresh_press_is_never_swallowed() {
    let ps = crate::route::PlaybackSession::IDLE;
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::More { quality: false });
    let before = page.sel();
    // Two hardware repeats inside one 110 ms window: the second must move nothing.
    press(&mut page, SDLK_DOWN, 0, Edge::Down);
    let after_first = page.sel();
    assert_ne!(after_first, before, "the fresh press moves");
    press(&mut page, SDLK_DOWN, 0, Edge::Repeat);
    assert_eq!(
        page.sel(),
        after_first,
        "a repeat inside the window is admitted by nothing",
    );
    // …and a NEW press at the same instant is not the held key's beat.
    press(&mut page, SDLK_DOWN, 0, Edge::Down);
    assert_ne!(page.sel(), after_first, "a fresh press always moves");
}

/// A click on the `…` popover commits the row under the cursor and closes; a click on any of the
/// other three just closes. Four `modal_of` arms of the loop's pointer path, as one answer.
#[test]
fn a_click_closes_the_panel_and_commits_only_the_popovers_row() {
    let ps = crate::route::PlaybackSession::IDLE;
    for (kind, name) in MODAL {
        let mut page = PlayerOverlayScreen::new(&ps, ENTRY, kind);
        let (handled, reqs, dismissed) = click(&mut page, 10.0, 10.0);
        assert_eq!(handled, Handled::Yes, "{name}");
        assert!(dismissed, "{name}: a click closes it");
        assert!(
            !reqs.iter().any(|r| matches!(r, PlayerReq::More(_))),
            "{name}: only the popover's rows are actions",
        );
    }
    let mut page = PlayerOverlayScreen::new(&ps, ENTRY, OverlayKind::More { quality: false });
    let (_, reqs, dismissed) = click(&mut page, 10.0, 10.0);
    assert!(dismissed);
    assert!(
        reqs.iter().any(|r| matches!(r, PlayerReq::More(_))),
        "a click outside the rows still reports an action (None) and dismisses",
    );
}

fn click(page: &mut PlayerOverlayScreen, x: f32, y: f32) -> (Handled, Vec<PlayerReq>, bool) {
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let ev = ScreenEvent::<TestHost>::Input(InputEvent {
        kind: InputKind::Click { x, y, hit: None },
        at: Tick {
            ms: 1_000,
            dt_us: 0,
        },
        source: Source::Sdl,

    });
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
