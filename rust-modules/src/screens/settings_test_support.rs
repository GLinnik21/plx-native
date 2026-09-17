//! Shared fixtures and helpers for the `screens::settings` test modules split out below.
//!
//! **No SDL, no GL, no `Dispatcher`.** `RouteSurface` is generic over any `H: AppLike`
//! (`registry::AppLike`'s blanket impl), and `family::InnerHost` already satisfies that bound
//! — the exact fact that lets the Settings family mount its own pages a second time inside
//! the surface (§6.2). So a test here drives `RouteSurface` as `Machine<InnerHost>` directly,
//! with a hand-built `Cx`/`Effects` standing in for the outer dispatcher, and reads the
//! effects it emits — the same shape `ui/fixture.rs` and `containers/tests.rs` use, minus the
//! `Dispatcher` itself, which only `app/bridge.rs`'s `AppHost` can stand up (its `Arg` carries
//! the legacy `Route`, which this layer may not name). What this style CANNOT see is
//! anything the real focus ENGINE would do — a raw `Key::Down` here moves nothing, because
//! there is no engine in this harness to turn it into a `FocusMoved`; the tests below drive
//! the engine's own primitives (`FocusMoved`, `Activate`) directly instead, which is what
//! `bridge.rs`'s own `the_settings_surface_owns_input_and_walks_its_own_stack` cannot do from
//! outside `app/`, since it drives real keys through the real engine and never inspects a
//! `FocusKey` at all.

use super::*;
use crate::ui::fixture::FixtureMeasure;
// `By` is the odd one out and the split is deliberate rather than untidy: the other seven
// names really are `ui::machine`'s, but `By` — how a focus move was CAUSED (a direction key,
// a pointer, a restore) — belongs to `ui::screen` beside `ScreenEvent::FocusMoved`, the only
// thing that carries one. Writing it as `ui::machine::By` compiles nowhere and is invisible
// to every non-test gate, since this module is `cfg(test)`.
use crate::ui::machine::{Edge, FocusRead, InputEvent, InputKind, InputOwner, PressRead, Source};
use crate::ui::present::Present;

// A `static`, not a `const`: `Cx::measure` needs a genuine `&'static dyn Measure`, and a
// `static` gives one outright rather than leaning on constant-promotion rules at the borrow
// site inside `cx` below.
pub(super) static MEASURE: FixtureMeasure = FixtureMeasure;

pub(super) fn cx_with<'a>(
    focus: Option<FocusKey<u32>>,
    directory: crate::stores::browse::DirectoryView<'a>,
) -> Cx<'a, InnerHost> {
    Cx {
        views: directory,
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        focus: FocusRead { current: focus , ..Default::default() },
        owner: InputOwner::Entry(EntryId(0)),
    }
}

pub(super) fn cx(focus: Option<FocusKey<u32>>) -> Cx<'static, InnerHost> {
    cx_with(focus, crate::stores::browse::DirectoryView::empty_for_test())
}

/// Step the surface once and return what it emitted, the way `RouteSurface::forward` would
/// hand effects up to whatever mounted it.
pub(super) fn step(
    s: &mut RouteSurface,
    ev: ScreenEvent<InnerHost>,
    focus: Option<FocusKey<u32>>,
) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let c = cx(focus);
    let _ = <RouteSurface as Machine<InnerHost>>::step(s, &ev, &c, &mut fx);
    out
}

pub(super) fn name(s: &RouteSurface) -> &'static str {
    <RouteSurface as Screen<InnerHost>>::name(s)
}

pub(super) struct DrawProbe {
    pub(super) id: u32,
    pub(super) seen: std::rc::Rc<std::cell::RefCell<Vec<(u32, crate::ui::screen::NavPresentation)>>>,
}

impl Machine<InnerHost> for DrawProbe {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, _: &Self::Ev, _: &Cx<'_, InnerHost>, _: &mut Effects<'_, InnerHost>) -> Handled {
        Handled::No
    }
}

impl Focusable<InnerHost> for DrawProbe {
    fn groups(&self, _: &Cx<'_, InnerHost>, _: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _: &u32, _: &Cx<'_, InnerHost>) -> Option<GroupId> { None }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, InnerHost>) -> Step<u32> { Step::Edge }
    fn place(&self, _: &u32, _: &Cx<'_, InnerHost>, _: At) -> Option<Placed> { None }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, InnerHost>) -> FocusKey<u32> { want }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        panic!("draw-only probe cannot be seated")
    }
}

impl Screen<InnerHost> for DrawProbe {
    fn name(&self) -> &'static str { "draw-probe" }
    fn state(&self) -> &dyn LogicalState { &() }
    fn crumb(&self, _: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        self.seen.borrow_mut().push((self.id, crate::ui::screen::NavPresentation {
            page_alpha: f.page_alpha,
            chrome_alpha: f.chrome_alpha,
            view_tab: f.view_tab,
            blur_amount: f.blur_amount,
        }));
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
}

/// **Tests mounting a real root page need a scratch session, because `RootPage::rebuild` asks
/// whether this television is signed in and the row set DEPENDS ON THE ANSWER** — signed in, a `Libraries`
/// section with *Favorite libraries* is prepended, so row 1 stops being *Legal notices* and
/// becomes *Privacy & data*. Without the redirect that question is answered by whatever
/// `auth.json` happens to be on the machine running `make check`, so the two tests below that
/// press row 1 passed on a runner and failed on the maintainer's own Mac — a real signal that
/// reads exactly like flakiness. `session::TempSession` writes a file with no account token
/// and no dialable server, i.e. deterministically SIGNED OUT, which is the state the row
/// comments here already assume. The caller must hold `testlock::serial()` for its whole body
/// (the redirected path is a crate global); every test below takes it first.
pub(super) fn scratch_session(tag: &str) -> crate::plex::session::TempSession {
    crate::plex::session::TempSession::new(tag)
}

pub(super) fn multi_user_session(tag: &str) -> crate::plex::session::TempSession {
    let t = crate::plex::session::TempSession::new(tag);
    crate::plex::session::save(&crate::plex::session::Session {
        client_id: "cid-test".into(),
        account_token: "acct".into(),
        server: crate::plex::session::ServerRef {
            address: "192.168.0.10".into(),
            port: 32400,
            token: "t".into(),
            ..Default::default()
        },
        user: crate::plex::session::UserRef {
            uuid: "u-0".into(),
            token: "ut".into(),
            title: "Admin".into(),
            ..Default::default()
        },
        home_users: vec![
            crate::plex::session::HomeUserRef {
                uuid: "u-0".into(),
                title: "Admin".into(),
                admin: true,
                ..Default::default()
            },
            crate::plex::session::HomeUserRef {
                uuid: "u-1".into(),
                title: "Kid".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    });
    t.watching("u-0");
    t
}

/// Signed-in with a Plex Home roster, row 3 is Automatically Sign In (after Favorite libraries,
/// Privacy & data, Legal notices).
pub(super) const AUTO_SIGN_IN_ROW: u32 = 3;

/// Run the push spring to rest on 16 ms frames — bounded, so a spring that never settles
/// fails the test rather than hanging the suite.
pub(super) fn settle(s: &mut RouteSurface) {
    for i in 1..600u32 {
        step(
            s,
            ScreenEvent::Tick(Tick {
                ms: i * 16,
                dt_us: 16_000,
            }),
            None,
        );
        if s.at_rest() {
            return;
        }
    }
    panic!("the push spring never settled");
}

/// A BACK press as the dispatcher delivers one. `at_edge` is `false` because this harness
/// has no engine to have produced an edge rule; the surface's arm is deliberately blind to
/// the flag (see its comment), so the two roads are one event here.
pub(super) fn back_key() -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    })
}

/// Hand the surface an effect the way an inner page's own `step` does — through `forward`,
/// the one seam every page emission crosses on its way up — and return what the surface
/// emitted outward. Driving `request` directly would test the guard and skip the road, and
/// the road is half the claim: the fix has to hold for a Pop that arrives from a page, not
/// only for one this file spells out.
pub(super) fn forwarded(s: &mut RouteSurface, fx: Fx<InnerHost>) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut sink = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let c = cx(None);
    s.forward(
        vec![Stamped {
            from: MachineId::Instance(InstanceId(1)),
            fx,
        }],
        &c,
        &mut sink,
    );
    out
}
