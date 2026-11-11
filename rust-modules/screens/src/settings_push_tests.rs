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
        (vec![SettingsPage::Root, SettingsPage::ConsentStage(0)], None),
        (vec![SettingsPage::Playback, SettingsPage::HudPreview(plx_data::hudpacks::Sha::of(b"push-test"))], None),
        (vec![SettingsPage::Root], Some(SettingsPage::ConsentStage(0))),
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

/// Every Settings page the surface hands a `PushFrame` to: its draw is the narrative parts and the
/// list part of the shared screens (`Header`/`BandPart` and `TablePart`/`DocumentPart`), or goes
/// through `DrawFrame::narrative_painter` / `list_painter` itself.
fn held_pages() -> Vec<SettingsPage> {
    use SettingsPage::*;
    let sha = plx_data::hudpacks::Sha::of(b"push-test");
    let mut pages = vec![
        Root, Language, UnderPosters, UnderPostersDetails(PosterKind::Movie), UnderPostersDetails(PosterKind::Show), Contribute,
        Playback, AudioSubtitles, Favourites, LibraryFavourites, Privacy, Legal, About, Document(0), Preview(0), Diagnostics,
        PlayerHud, HudGallery, HudDetail(sha), HudAbout(sha),
    ];
    pages.extend(PICKERS.iter().map(|k| Picker(*k)));
    pages
}

/// The pages drawn WHOLE through the push's translate, and the precise reason each one is.
fn not_held() -> [(SettingsPage, &'static str); 2] {
    [
        (SettingsPage::HudPreview(plx_data::hudpacks::Sha::of(b"push-test")), "a full-screen scene with its own controls drawn over it: there is no narrative column to hold"),
        (SettingsPage::ConsentStage(0), "the first-run question: its own family never pushes a PushFrame, and its card is one piece"),
    ]
}

const PICKERS: [PickerKind; 19] = [
    PickerKind::Quality, PickerKind::DirectPlay, PickerKind::SubtitleSize, PickerKind::SubtitlePosition, PickerKind::SubtitleColor, PickerKind::HdrOverlay,
    PickerKind::NextEpisode, PickerKind::SkipInterval, PickerKind::DeckPress, PickerKind::AudioLanguage, PickerKind::SubtitleMode,
    PickerKind::SubtitleLanguage, PickerKind::ForcedSubtitles, PickerKind::UiResolution, PickerKind::InstantDpadSeeking, PickerKind::UriEngine,
    PickerKind::DolbyVision, PickerKind::IntroSkip, PickerKind::CreditsSkip,
];

/// The pages the surface hands a frame to are exactly the ones whose draw is the shared parts, and
/// the ones it does not are listed here with the reason.
#[test]
fn only_the_pages_drawn_by_the_shared_parts_hold_their_column() {
    for page in held_pages() {
        assert!(page.holds_narrative(), "{page:?}");
    }
    for (page, why) in not_held() {
        assert!(!page.holds_narrative(), "{page:?}: {why}");
    }
}

/// What a recording draw of `page` put on screen as text runs, at rest (`push: None`) or under `push`.
fn page_runs(page: SettingsPage, push: Option<PushFrame>) -> Vec<plx_ui::draw_census::Run> {
    let outer = cx(None);
    let c = inner_cx(&outer);
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let mut screen = mount_page(EntryId(1), page, &c, &mut fx);
    let mut f = DrawFrame::new(&c, Painter::recording());
    f.push = push;
    plx_ui::draw_census::capture_runs(|| screen.draw(&mut f))
}

/// **Under a push the narrative column's frame is constant and only the list travels**, for every
/// page that holds. A page is drawn at rest and under each role of a push (`list` and `fade` set
/// apart so the two columns cannot be mistaken for each other): every text run is either in the
/// narrative column - the SAME position, its ink scaled by the cross-fade - or in the list, moved
/// by the push spring's offset and scaled by its alpha; and a page's title is always the former.
#[test]
fn the_narrative_frame_is_constant_and_the_list_travels_on_every_held_page() {
    let _serial = plx_base::testlock::serial();
    let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let w = Rect::FULL.w;
    let (list, fade) = (0.25_f32, 0.75_f32);
    for page in held_pages() {
        let rest = page_runs(page, None);
        assert!(!rest.is_empty(), "{page:?} drew no text at rest");
        for role in [PushRole::Parent, PushRole::Child] {
            let frame = PushFrame { role, list, fade };
            let under = page_runs(page, Some(frame));
            assert_eq!(under.len(), rest.len(), "{page:?} {role:?}: a push changes no run, only where and how strongly it draws");
            let (narrative_alpha, list_alpha, list_dx) = match role {
                PushRole::Parent => (1.0 - fade, 1.0 - list, -0.35 * w * list),
                PushRole::Child => (fade, list, 0.22 * w * (1.0 - list)),
            };
            // the page's title is the largest run standing in the narrative column at rest (the header's heading,
            // over its crumb and copy): that column holds it still under either role, at the narrative's alpha
            let column = plx_ui::route_screen::RouteLayout::settings_page().narrative;
            let in_column = |r: &plx_ui::draw_census::Run| r.at.x >= column.x - 0.5 && r.at.x < column.x + column.w;
            let (title_at, title) = rest.iter().enumerate().filter(|(_, r)| in_column(r))
                .fold(None, |best: Option<(usize, &plx_ui::draw_census::Run)>, (i, r)| match best { Some(b) if b.1.size >= r.size => Some(b), _ => Some((i, r)) })
                .unwrap_or_else(|| panic!("{page:?}: no run in the narrative column at rest"));
            let held_title = &under[title_at];
            assert!(
                (held_title.at.x - title.at.x).abs() < 0.01 && (held_title.at.y - title.at.y).abs() < 0.01
                    && (held_title.ink[3] - title.ink[3] * narrative_alpha).abs() < 0.01,
                "{page:?} {role:?}: the title {:?} is not held with the narrative column ({:?} -> {:?})", title.text, title.at, held_title.at
            );
            let (mut held, mut travelled) = (0, 0);
            for (at_rest, now) in rest.iter().zip(&under) {
                assert_eq!(at_rest.text, now.text, "{page:?} {role:?}");
                let near = |a: f32, b: f32| (a - b).abs() < 0.01;
                let alpha_of = |r: &plx_ui::draw_census::Run| r.ink[3];
                let still = near(now.at.x, at_rest.at.x) && near(now.at.y, at_rest.at.y);
                let moved = near(now.at.x, at_rest.at.x + list_dx) && near(now.at.y, at_rest.at.y);
                if still {
                    held += 1;
                    assert!(near(alpha_of(now), alpha_of(at_rest) * narrative_alpha), "{page:?} {role:?} {:?}: narrative alpha", now.text);
                } else {
                    travelled += 1;
                    assert!(moved, "{page:?} {role:?} {:?} moved from {:?} to {:?}, not by the list's {list_dx}", now.text, at_rest.at, now.at);
                    assert!(near(alpha_of(now), alpha_of(at_rest) * list_alpha), "{page:?} {role:?} {:?}: list alpha", now.text);
                }
            }
            assert!(held > 0, "{page:?} {role:?}: no text held still, so the page has no narrative column");
            // the sources editor has no rows until a directory lands, so its list is the spinner, not text
            let listless = matches!(page, SettingsPage::Favourites | SettingsPage::LibraryFavourites);
            assert!(travelled > 0 || listless, "{page:?} {role:?}: nothing travelled, so the list was drawn with the narrative painter");
        }
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
