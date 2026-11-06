//! The Settings push that holds the narrative column, graded through the real `draw_pages` with
//! inert pages: which pairs hold, what each page's frame is told, and that every other pair and
//! the resting surface are drawn exactly as before.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_ui::containers::stack::Entry;
use plx_ui::route_screen::{PushFrame, PushRole};
use std::{cell::RefCell, rc::Rc};

/// What a page saw: its id, the push it was handed and the cascade it was drawn through.
type Seen = Vec<(u32, Option<PushFrame>, f32, f32)>;

struct PushProbe {
    id: u32,
    seen: Rc<RefCell<Seen>>,
}

impl Machine<InnerHost> for PushProbe {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, _: &Self::Ev, _: &Cx<'_, InnerHost>, _: &mut Effects<'_, InnerHost>) -> Handled {
        Handled::No
    }
}

impl Focusable<InnerHost> for PushProbe {
    fn groups(&self, _: &Cx<'_, InnerHost>, _: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _: &u32, _: &Cx<'_, InnerHost>) -> Option<GroupId> { None }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, InnerHost>) -> Step<u32> { Step::Edge }
    fn place(&self, _: &u32, _: &Cx<'_, InnerHost>, _: At) -> Option<Placed> { None }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, InnerHost>) -> FocusKey<u32> { want }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        panic!("draw-only probe cannot be seated")
    }
}

impl Screen<InnerHost> for PushProbe {
    fn name(&self) -> &'static str { "push-probe" }
    fn state(&self) -> &dyn LogicalState { &() }
    fn crumb(&self, _: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        self.seen.borrow_mut().push((self.id, f.push, f.painter.dx(), f.painter.opacity()));
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
}

/// A surface with `stack` (bottom first) installed as inert bodies, the push parked at `t` toward
/// `open`, and `leaving` (a page, drawn in the child role) when the push is a pop.
fn surface(stack: &[SettingsPage], t: f32, open: bool, leaving: Option<SettingsPage>) -> (RouteSurface, Rc<RefCell<Seen>>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let instance = |id: u32| Instance {
        id: InstanceId(id),
        screen: Box::new(PushProbe { id, seen: Rc::clone(&seen) }) as Box<dyn Screen<InnerHost>>,
        inflight: Vec::new(),
        staged: false,
        staged_effects: Vec::new(),
    };
    let mut inner = NavStack::new(Box::new(Immediate));
    for (i, page) in stack.iter().enumerate() {
        let id = i as u32 + 1;
        inner.entries.push(Entry { id: EntryId(id), arg: *page, ret: ReturnState::default(), inst: Some(instance(id)), evicted: false });
    }
    let surface = RouteSurface {
        entry: EntryId(0),
        id: InstanceId(0),
        kind: Family::Settings,
        inner,
        ids: Minter::default(),
        push: Push {
            route: plx_ui::route_screen::RoutePush::at(t),
            open,
            leaving: leaving.map(|_| instance(9)),
            leaving_page: leaving,
        },
        ground: RouteGround::new(),
        ground_ready: false,
        remembered: Vec::new(),
    };
    (surface, seen)
}

fn draw(surface: &mut RouteSurface) {
    let outer = cx(None);
    let c = inner_cx(&outer);
    let mut f = DrawFrame::new(&c, Painter::root());
    surface.draw_pages(&mut f, Painter::root());
}

/// **A push between two pages that hold their column hands each page a `PushFrame` and the plain
/// entrance painter**: nothing translates the page as a whole, so the narrative column cannot move.
#[test]
fn a_push_between_held_pages_hands_each_page_its_frame_and_a_still_painter() {
    let (mut s, seen) = surface(&[SettingsPage::Root, SettingsPage::Playback], 0.5, true, None);
    draw(&mut s);
    let seen = seen.borrow();
    let roles: Vec<_> = seen.iter().map(|(id, push, dx, a)| (*id, push.map(|p| p.role), *dx, *a)).collect();
    assert_eq!(
        roles,
        vec![
            (1, Some(PushRole::Parent), 0.0, 1.0),
            (2, Some(PushRole::Child), 0.0, 1.0),
        ],
        "parent drawn first, then the child, each at the untouched entrance"
    );
    let parent = seen[0].1.unwrap();
    assert_eq!((parent.list, parent.fade), (0.5, 0.5));
}

/// **A pop is the same pair, reversed**: the page returned to is the parent, the retired page the child.
#[test]
fn a_pop_between_held_pages_puts_the_retired_page_in_the_child_role() {
    let (mut s, seen) = surface(&[SettingsPage::Root], 0.5, false, Some(SettingsPage::Playback));
    draw(&mut s);
    let seen = seen.borrow();
    assert_eq!(seen.len(), 2);
    assert_eq!((seen[0].0, seen[0].1.map(|p| p.role)), (1, Some(PushRole::Parent)), "the page returned to");
    assert_eq!((seen[1].0, seen[1].1.map(|p| p.role)), (9, Some(PushRole::Child)), "the retired page");
}

/// **A pair with a page that does not hold its column is drawn whole**, exactly as before: no
/// frame, and the page's painter carries the translate and alpha of the push.
#[test]
fn a_pair_with_a_page_that_does_not_hold_is_drawn_whole_as_before() {
    let w = Rect::FULL.w;
    for (stack, leaving) in [
        (vec![SettingsPage::Root, SettingsPage::Privacy], None),
        (vec![SettingsPage::Favourites, SettingsPage::Playback], None),
        (vec![SettingsPage::Root], Some(SettingsPage::Privacy)),
    ] {
        let popping = leaving.is_some();
        let (mut s, seen) = surface(&stack, 0.5, !popping, leaving);
        draw(&mut s);
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2, "{stack:?} / {leaving:?}");
        for (id, push, _, _) in seen.iter() {
            assert!(push.is_none(), "page {id} of {stack:?} / {leaving:?} was handed a frame");
        }
        let t = s.push.amount();
        assert_eq!((seen[0].2, seen[0].3), (-0.35 * w * t, 1.0 - t), "the parent leaves left, fading");
        assert_eq!((seen[1].2, seen[1].3), (0.22 * w * (1.0 - t), t), "the child leads in, appearing");
    }
}

/// At rest one page is on screen, the top, with no frame.
#[test]
fn a_settled_surface_draws_its_top_page_with_no_frame() {
    let (mut s, seen) = surface(&[SettingsPage::Root, SettingsPage::Playback], 1.0, true, None);
    draw(&mut s);
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].0, seen[0].1, seen[0].2, seen[0].3), (2, None, 0.0, 1.0));
}

/// The pages the surface hands a frame to are exactly the ones whose draw is the shared parts.
#[test]
fn only_the_pages_drawn_by_the_shared_table_parts_hold_their_column() {
    use SettingsPage::*;
    for page in [Root, Language, Playback, AudioSubtitles, Legal, Diagnostics] {
        assert!(page.holds_narrative(), "{page:?}");
    }
    for page in [Privacy, Favourites, LibraryFavourites, UnderPosters, UnderPostersDetails, About, Document(0), Preview(0), ConsentStage(0), PlayerHud, HudGallery] {
        assert!(!page.holds_narrative(), "{page:?} draws something the parts do not carry");
    }
}

/// Settings opens on the dip; the first-run question arrives on the spring.
#[test]
fn settings_enters_on_the_dip_and_the_first_run_question_on_the_spring() {
    use plx_ui::containers::modal::EntryMotion;
    let settings = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
    let consent = RouteSurface::new(EntryId(0), InstanceId(0), Family::FirstRunConsent, SettingsPage::ConsentStage(0));
    assert_eq!(<RouteSurface as Screen<InnerHost>>::entry_motion(&settings), EntryMotion::Dip);
    assert_eq!(<RouteSurface as Screen<InnerHost>>::entry_motion(&consent), EntryMotion::Spring);
}
