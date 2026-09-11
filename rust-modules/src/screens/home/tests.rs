use super::*;

use crate::ui::fixture::FixtureMeasure;
use crate::ui::focus::{FocusEngine, Outcome};
use crate::ui::hit::{HitMap, PointerKind};
use crate::ui::machine::{
    Chrome, FocusRead, Host, InputOwner, PressRead, ScreenId, Source, Stamped, Tick,
};
use crate::ui::screen::{ScreenArg, ScreenEvent};

#[derive(Clone, PartialEq, Eq)]
struct TestArg;

impl LogicalState for TestArg {
    fn write(&self, c: &mut Canon) {
        c.u32(0);
    }
    fn probe(&self, _: &mut String) {}
}

impl ScreenArg for TestArg {
    fn chrome(&self) -> Chrome {
        Chrome::TabBar
    }
    fn id(&self) -> ScreenId {
        ScreenId(800)
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

struct TestHost;

impl Host for TestHost {
    type Arg = TestArg;
    type Fx = AppFx;
    type Msg = super::super::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = HubsView<'a>;
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}

impl HomeLike for TestHost {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> HubsView<'a> {
        cx.views
    }
}

fn cx<'a>(view: HubsView<'a>, focus: Option<FocusKey<u32>>) -> Cx<'a, TestHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx {
        views: view,
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        focus: FocusRead { current: focus , ..Default::default() },
        owner: InputOwner::Entry(focus.map_or(EntryId(7), |k| k.entry)),
    }
}

fn screen(view: HubsView<'_>) -> HomeScreen {
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    s.sync_catalog(&cx(view, None));
    s.layout_grid();
    s
}

fn step(
    s: &mut HomeScreen,
    view: HubsView<'_>,
    focus: Option<FocusKey<u32>>,
    event: &ScreenEvent<TestHost>,
) -> (Handled, Vec<Stamped<TestHost>>, bool) {
    let context = cx(view, focus);
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let handled = {
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(InstanceId(9)),
            &mut present,
        );
        Machine::<TestHost>::step(s, event, &context, &mut fx)
    };
    (handled, out, present.peek(0))
}

fn first_card(s: &HomeScreen) -> FocusKey<u32> {
    FocusKey {
        entry: s.entry,
        elem: s.rows[0].elems[0],
    }
}

fn has_home(out: &[Stamped<TestHost>], pred: impl Fn(&HomeReq) -> bool) -> bool {
    out.iter().any(|stamped| match &stamped.fx {
        Fx::App(AppFx::Home(req)) => pred(req),
        _ => false,
    })
}

#[test]
fn n_hubs_clamps_the_server_count_to_the_shelf_array() {
    assert_eq!(n_hubs_of(0), 0);
    assert_eq!(n_hubs_of(3), 3);
    assert_eq!(n_hubs_of(MAX_HUBS + 1), MAX_HUBS);
    assert_eq!(n_hubs_of(200), MAX_HUBS);
}

#[test]
fn every_tab_pill_round_trips_through_the_focus_packing() {
    let cases = [
        (STRIP_HOME_ELEM, HomeReq::Tab(HomeTab::Home)),
        (STRIP_MOVIES_ELEM, HomeReq::Tab(HomeTab::Movies)),
        (STRIP_SHOWS_ELEM, HomeReq::Tab(HomeTab::Shows)),
        (STRIP_SEARCH_ELEM, HomeReq::Tab(HomeTab::Search)),
        (STRIP_ACCOUNT_ELEM, HomeReq::Account),
    ];
    for (elem, want) in cases {
        assert_eq!(HomeScreen::request_for_strip(elem), Some(want));
    }
    assert_eq!(HomeScreen::request_for_strip(STRIP_ACCOUNT_ELEM + 1), None);
    for page_key in [HERO_PLAY_ELEM, HERO_INFO_ELEM, FIRST_ITEM_ELEM] {
        assert_eq!(HomeScreen::request_for_strip(page_key), None);
    }
}

#[test]
fn top_band_focus_walks_to_the_last_section_whatever_the_count() {
    assert_eq!(STRIP_MOVIES_ELEM - STRIP_HOME_ELEM, 1);
    assert_eq!(STRIP_SHOWS_ELEM - STRIP_HOME_ELEM, 2);
    assert_eq!(STRIP_SEARCH_ELEM - STRIP_HOME_ELEM, 3);
    assert_eq!(STRIP_ACCOUNT_ELEM - STRIP_HOME_ELEM, 4);
}

#[test]
fn set_hero_focus_clamps_onto_the_last_drawable_pill() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let s = screen(snapshot.view());
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&s, &cx(snapshot.view(), None), &mut groups);
    assert_eq!(groups[0].id, HERO_GROUP);
    assert_eq!(groups[0].len, 2);
    assert!(groups
        .iter()
        .all(|g| g.id != crate::ui::containers::tabs::STRIP));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn the_pager_is_not_a_focus_stop_and_the_rows_end_pages_instead() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let context = cx(
        snapshot.view(),
        Some(FocusKey {
            entry: s.entry,
            elem: HERO_INFO_ELEM,
        }),
    );
    let owner = InputOwner::Entry(s.entry);
    let mut engine = FocusEngine::new();
    engine.set(
        owner,
        context.focus.current.unwrap(),
        Some(HERO_GROUP),
        By::Restore,
    );
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    assert_eq!(
        engine.move_dir(owner, &s, &links, Dir::Right, &context),
        Outcome::Edge(EdgeRule::Screen)
    );
    let before = s.carousel.clone();
    assert!(s.flip(snapshot.view(), 1));
    assert_ne!(s.carousel, before);
    assert_eq!(engine.current(owner).unwrap().elem, HERO_INFO_ELEM);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn the_top_band_reports_the_chip_and_the_pills_as_one_answer() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let s = screen(snapshot.view());
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    assert!(links.contains(&Link {
        from: crate::ui::containers::tabs::STRIP,
        dir: Dir::Down,
        to: HERO_GROUP
    }));
    assert!(links.contains(&Link {
        from: HERO_GROUP,
        dir: Dir::Up,
        to: crate::ui::containers::tabs::STRIP
    }));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn the_top_band_walks_permanent_pills_not_the_section_table() {
    assert_ne!(STRIP_HOME_ELEM, STRIP_MOVIES_ELEM);
    assert_ne!(STRIP_MOVIES_ELEM, STRIP_SHOWS_ELEM);
    assert_ne!(STRIP_SHOWS_ELEM, STRIP_SEARCH_ELEM);
    assert!(matches!(
        HomeScreen::request_for_strip(STRIP_SEARCH_ELEM),
        Some(HomeReq::Tab(HomeTab::Search))
    ));
}

#[test]
fn step_row_stays_inside_the_addressable_rows() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let s = screen(snapshot.view());
    let first = first_card(&s);
    assert!(matches!(
        Focusable::<TestHost>::neighbour(&s, first, Dir::Left, &cx(snapshot.view(), Some(first))),
        Step::Edge
    ));
    let last = FocusKey {
        entry: s.entry,
        elem: s.rows[0].elems[2],
    };
    assert!(matches!(
        Focusable::<TestHost>::neighbour(&s, last, Dir::Right, &cx(snapshot.view(), Some(last))),
        Step::Edge
    ));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn vert_cannot_walk_past_the_shelf_array() {
    assert_eq!(n_hubs_of(MAX_HUBS), MAX_HUBS);
    assert_eq!(n_hubs_of(MAX_HUBS + 50), MAX_HUBS);
}

#[test]
fn the_status_readout_tells_loading_empty_and_failed_apart() {
    let _guard = crate::testlock::serial();
    for (state, kind, action) in [
        (crate::pms::HubState::Loading, StatusKind::Working, false),
        (crate::pms::HubState::Failed, StatusKind::Failed, true),
        (crate::pms::HubState::Ready, StatusKind::Empty, true),
    ] {
        crate::pms::seed_for_test(0, state);
        let snapshot = crate::pms::hubs_snapshot();
        let (_, got, got_action) = status_read(snapshot.view()).unwrap();
        assert_eq!(got, kind);
        assert_eq!(got_action.is_some(), action);
    }
    for state in [crate::pms::HubState::Ready, crate::pms::HubState::Failed] {
        crate::pms::seed_for_test(3, state);
        assert!(status_read(crate::pms::hubs_snapshot().view()).is_none(),
            "a failed refresh retains playable content, not a replacement readout");
    }
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn no_shelves_means_no_grid_snap() {
    assert_eq!(pinned_snap(1.0, 0), 0.0);
    assert_eq!(pinned_snap(0.0, 0), 0.0);
    assert_eq!(pinned_snap(1.0, 1), 1.0);
    assert_eq!(pinned_snap(0.0, 1), 0.0);
    assert_eq!(pinned_snap(1.0, MAX_HUBS), 1.0);
}

#[test]
fn the_status_screen_takes_ok_but_never_the_top_band() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(0, crate::pms::HubState::Failed);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let entry = s.entry;
    let (_, retry, _) = step(
        &mut s,
        snapshot.view(),
        Some(FocusKey {
            entry,
            elem: HERO_PLAY_ELEM,
        }),
        &ScreenEvent::Activate(HERO_PLAY_ELEM),
    );
    assert!(retry.iter().any(|stamped| matches!(
        &stamped.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry)))
    )));
    for elem in [STRIP_HOME_ELEM, STRIP_MOVIES_ELEM, STRIP_SHOWS_ELEM, STRIP_SEARCH_ELEM, STRIP_ACCOUNT_ELEM] {
        let (_, out, _) = step(&mut s, snapshot.view(), None, &ScreenEvent::Activate(elem));
        let expected = HomeScreen::request_for_strip(elem).unwrap();
        assert!(has_home(&out, |r| *r == expected));
        assert!(!out.iter().any(|s| matches!(&s.fx,
            Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry))))));
    }
    crate::pms::seed_for_test(3, crate::pms::HubState::Failed);
    let populated = crate::pms::hubs_snapshot();
    let mut s = screen(populated.view());
    let (_, out, _) = step(&mut s, populated.view(), None, &ScreenEvent::Activate(HERO_PLAY_ELEM));
    assert!(has_home(&out, |r| matches!(r, HomeReq::Play { .. })));
    assert!(!out.iter().any(|s| matches!(&s.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry))))));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn pointer_hit_column_matches_the_drawn_card_at_every_snap_phase() {
    for scroll in [0.0, 415.0, 830.0] {
        for snap in [0.0, 0.37, 1.0] {
            let effective = scroll * snap;
            for col in 0..24 {
                assert_eq!(
                    col_at(card_x(col, effective) + CARD_W * 0.5, effective, 24),
                    Some(col)
                );
            }
        }
    }
}

#[test]
fn drawn_hero_geometry_follows_slide_and_the_captured_press_scale() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.outgoing = s.carousel.clone();
    assert!(s.outgoing.is_some());
    for _ in 0..80 { s.hero_pop.step(Some(0), 0.016); }
    let key = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    for dir in [-1.0, 1.0] {
        s.hero_dir = dir;
        for slide in [0.3, 0.8, 1.0] {
            s.hero_slide.jump(slide);
            for press in [0.93, 1.0, 1.025] {
                let mut context = cx(snapshot.view(), Some(key));
                context.press.scale = press;
                let base = s.hero_button_rect(snapshot.view(), 0, context.measure).unwrap();
                let drawn = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
                let target = Focusable::<TestHost>::place(&s, &key.elem, &context, At::SpringTarget).unwrap();
                assert!((drawn.rect.cx() - base.cx() - dir * (1.0 - slide) * SCR_W).abs() < 0.01);
                assert!((drawn.rect.w - base.w * crate::ui::widgets::CTRL_FOCUS_SCALE * press).abs() < 0.01);
                assert!((target.rect.cx() - base.cx()).abs() < 0.01);
                if dir == 1.0 && slide == 0.8 {
                    let mut frame = DrawFrame::new(&context, Painter::root());
                    s.record_stops(&mut frame, snapshot.view());
                    let mut map = HitMap::new();
                    map.fill(frame.into_stops());
                    map.swap();
                    assert_eq!(map.resolve(Some(s.entry), PointerKind::Click,
                        drawn.rect.cx(), drawn.rect.cy(), Some(key)).hit, Some(key));
                    assert_eq!(map.resolve(Some(s.entry), PointerKind::Click,
                        base.cx(), base.cy(), Some(key)).hit, None);
                }
            }
        }
    }
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn status_action_geometry_does_not_inherit_the_previous_hero_pop_or_slide() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(0, crate::pms::HubState::Failed);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    for _ in 0..80 { s.hero_pop.step(Some(0), 0.016); }
    s.outgoing = Some((crate::plex::ServerId::UNSET, "old".into()));
    s.hero_slide.jump(0.5);
    let key = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    let mut context = cx(snapshot.view(), Some(key));
    context.press.scale = 0.93;
    let base = s.hero_button_rect(snapshot.view(), 0, context.measure).unwrap();
    let drawn = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
    assert_eq!((drawn.rect.x, drawn.rect.y, drawn.rect.w, drawn.rect.h),
        (base.x, base.y, base.w, base.h));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn drawn_card_geometry_includes_press_but_its_rest_anchor_does_not() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    for _ in 0..80 { s.grid.shelves[0].update(3, Some(0), &RowStyle::HOME, 0.016); }
    s.layout_grid();
    let key = first_card(&s);
    for press in [0.93, 1.0, 1.025] {
        let mut context = cx(snapshot.view(), Some(key));
        context.press.scale = press;
        let placed = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
        assert!((placed.rect.w - CARD_W * s.grid.shelves[0].scale(0) * press).abs() < 0.01);
        assert!((placed.rest_rect.w - CARD_W * RowStyle::HOME.focus_scale).abs() < 0.01);
        context.press = PressRead::default();
        let opener = s.focused_rect(Some(key), &context, At::Drawn).unwrap();
        assert!((opener.w - CARD_W * s.grid.shelves[0].scale(0)).abs() < 0.01);
    }
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn the_card_anchor_rect_tracks_the_drawn_card_through_scroll_and_snap() {
    for &scroll in &[0.0f32, 415.0, 830.0] {
        for &sp in &[0.0f32, 0.37, 1.0] {
            let es = scroll * sp;
            for c in 0..8usize {
                for &focus in &[1.0f32, RowStyle::HOME.focus_scale] {
                    let base = Rect::new(card_x(c, es), 176.0 + CARD_DY, CARD_W, CARD_H);
                    let r = base.scaled(focus);
                    // magnification is about the card's CENTRE — the anchor must not slide
                    assert!(
                        (r.cx() - base.cx()).abs() < 0.001
                            && (r.cy() - base.cy()).abs() < 0.001,
                        "card {c} (scroll={scroll}, sp={sp}, focus={focus}) moved its centre"
                    );
                    // …and the rect the panel is placed beside is the one the pointer would hit
                    assert_eq!(
                        col_at(r.cx(), es, 8),
                        Some(c),
                        "anchor centre for card {c} (scroll={scroll}, sp={sp}) is not over that card"
                    );
                    assert!(
                        r.w >= CARD_W && r.h >= CARD_H,
                        "focus must not shrink the card"
                    );
                }
            }
        }
    }
}

fn blurred(blur: [[f32; 3]; 4]) -> PmsMovie {
    PmsMovie {
        has_blur: true,
        blur,
        ..Default::default()
    }
}

#[test]
fn the_wash_floors_on_the_app_surface_and_never_dims_toward_black() {
    let flat = [theme::SURFACE_APP; 4];
    // "no artwork" IS the app's own ground, with no special case
    assert_eq!(
        wash_corners(None, flat, 0.0),
        flat,
        "an empty pool is the app's ground"
    );
    assert_eq!(
        wash_corners(Some(&PmsMovie::default()), flat, 0.0),
        flat,
        "…and so is an item with no envelope"
    );
    // the grid end of the snap lands on exactly the colour `frame_clear` already laid down —
    // where the old `dim` ramp landed on black
    let bright = blurred([
        [0.95, 0.93, 0.90],
        [1.0, 1.0, 1.0],
        [0.8, 0.75, 0.2],
        [0.1, 0.1, 0.12],
    ]);
    assert_eq!(
        wash_corners(Some(&bright), flat, 1.0),
        flat,
        "the dive resolves to the app ground, not to black"
    );
    assert_eq!(
        wash_corners(Some(&bright), flat, 1.5),
        flat,
        "…and an overshooting snap cannot push past it"
    );

    // **The grid end held at the app's own surface, which is what makes every assertion below
    // hold VERBATIM through the change that gave this function a second subject.** With
    // `grid = SURFACE_APP` the lerp `mix(hero_t, SURFACE, sp)` is algebraically the old
    // `keyed(blur, W * (1 - sp))` weight ramp, corner for corner — so these are still the same
    // measurements of the same behaviour, not loosened ones. The grid end's own bound is
    // `the_grid_end_of_the_dive_is_the_focused_tiles_ground` below.
    const FLAT_GRID: [[f32; 4]; 4] = [theme::SURFACE_APP; 4];

    // The tail of the dive — the exact frames that used to be near-black — for a DARK envelope
    // as much as a bright one. The bound is the one invariant that separates a WEIGHT scale
    // from a brightness ramp: a corner can only travel as far from the app surface as the lean
    // weight STILL IN PLAY allows it to, so the deviation is capped by that corner's own
    // remaining weight times the furthest a mix could take it (toward white above the surface,
    // toward black below it). Do not loosen this to a flat percentage — the whole point is that
    // it tightens to nothing as `sp → 1`, which is what pins the grid end of the snap.
    //
    // For scale: at sp=0.9 this allows ±0.046 on the two top corners, and the old `dim` ramp
    // put them at 0.010 — a deviation of 0.163, off by three and a half times the budget.
    for m in [&bright, &blurred([[0.0; 3]; 4])] {
        for &sp in &[0.9f32, 0.99, 0.996] {
            for (c, w) in wash_corners(Some(m), FLAT_GRID, sp).iter().zip(HERO_WASH_W) {
                let lean = w * (1.0 - sp);
                for (ch, s) in c.iter().zip(theme::SURFACE_APP) {
                    let budget = lean * s.max(1.0 - s);
                    assert!(
                        (ch - s).abs() <= budget + 1e-6,
                        "at sp={sp} the ground is {ch} against a {s} surface — that is {} off a \
                         {budget} budget, so the old dim ramp is back",
                        (ch - s).abs()
                    );
                }
                assert_eq!(c[3], 1.0, "a wash corner is opaque");
            }
        }
    }

    // the lean is a WEIGHT SCALE on the shared mix, not a brightness scale over it. Re-deriving
    // it as `dim(keyed(blur, W), lean)` would darken the surface itself and fails here.
    //
    // Compared to a float epsilon rather than exactly, because the two spellings of the same
    // algebra — the weight ramp on the right, the lerp between two finished targets on the left
    // — associate their multiplies differently and land up to one ULP apart. The failure this
    // guards against is a brightness scale, which is wrong by three and a half TIMES the budget
    // asserted above, so an ULP of slack costs it nothing.
    for (c, w) in wash_corners(Some(&bright), FLAT_GRID, 0.5)
        .iter()
        .zip(AmbientWash::keyed(bright.blur, HERO_WASH_W.map(|w| w * 0.5)))
    {
        for (a, b) in c.iter().zip(w) {
            assert!(
                (a - b).abs() <= 1e-6,
                "half-dived is half the lean toward the artwork ({b}), not half the \
                 brightness ({a})"
            );
        }
    }
    // …and the lean does go TOWARD the artwork: a bright envelope lifts the panel off the
    // surface rather than sinking it.
    let lit = wash_corners(Some(&bright), FLAT_GRID, 0.0);
    assert!(
        lit[1][0] > theme::SURFACE_APP[0],
        "a white corner must brighten the ground it keys"
    );
    assert!(
        lit[1][0] < 1.0,
        "…but never all the way: it is a wash, not the photograph"
    );
}

#[test]
fn the_grid_end_of_the_dive_is_the_focused_tiles_ground() {
    let hero = blurred([[0.95, 0.1, 0.1]; 4]); // red billboard
    let tile = AmbientWash::keyed([[0.1, 0.1, 0.95]; 4], PageGround::CARD_W); // blue shelf tile
    // A lerp evaluated at its own endpoints lands one ULP off them, so these are graded to a
    // float epsilon. The failure they guard against is a wash carrying the WRONG SUBJECT'S
    // colours, which is a whole ground apart.
    let same = |got: [[f32; 4]; 4], want: [[f32; 4]; 4], what: &str| {
        for (c, w) in got.iter().zip(want) {
            for (a, b) in c.iter().zip(w) {
                assert!((a - b).abs() <= 1e-6, "{what}: got {a}, wanted {b}");
            }
        }
    };

    // at the top the grid end contributes NOTHING…
    same(
        wash_corners(Some(&hero), tile, 0.0),
        AmbientWash::keyed(hero.blur, HERO_WASH_W),
        "the billboard is the hero's alone",
    );
    // …and in the grid the hero contributes nothing.
    same(
        wash_corners(Some(&hero), tile, 1.0),
        tile,
        "the shelves are the focused tile's alone",
    );
    // An overshooting snap cannot push past the tile's own ground either — the clamp that used
    // to stop the lean going negative now stops the lerp extrapolating past the far end.
    same(
        wash_corners(Some(&hero), tile, 1.5),
        tile,
        "an overshooting snap",
    );

    // The grid end is still floored on the app surface, which is what keeps "no artwork" the
    // flat clear down here exactly as it is up top — and keeps `is_flat` able to skip the pass
    // on a library with no envelopes at all.
    same(
        wash_corners(Some(&hero), [theme::SURFACE_APP; 4], 1.0),
        [theme::SURFACE_APP; 4],
        "an artless shelf is the app's own ground, not a leftover hero tint",
    );

    // Halfway is halfway between the two grounds — a lerp, not a sum. Summing the two leans
    // would make the middle of the dive the BRIGHTEST part of the animation, which is the one
    // place neither subject is the page's.
    for (c, (h, g)) in wash_corners(Some(&hero), tile, 0.5)
        .iter()
        .zip(AmbientWash::keyed(hero.blur, HERO_WASH_W).iter().zip(tile))
    {
        for (i, v) in c.iter().enumerate() {
            let want = h[i] + (g[i] - h[i]) * 0.5;
            assert!(
                (v - want).abs() <= 1e-6,
                "mid-dive corner channel is {v}, halfway between the two grounds is {want}"
            );
        }
    }
}

#[test]
fn prefetch_order_wraps_and_never_warms_the_page_on_screen() {
    let mut out = [0 as i32; 2 * HERO_PREFETCH];
    assert_eq!(
        prefetch_order(0, 0, &mut out),
        0,
        "an empty pool has no neighbours"
    );
    assert_eq!(
        prefetch_order(0, 1, &mut out),
        0,
        "…and neither does a one-page pool"
    );
    assert_eq!(
        prefetch_order(0, 2, &mut out),
        1,
        "in a two-page pool +1 and -1 are the same page"
    );
    assert_eq!(out[0], 1);

    for n in 2..=8 as i32 {
        for cur in 0..n {
            let k = prefetch_order(cur, n, &mut out);
            assert!(
                k <= 2 * HERO_PREFETCH,
                "wrote {k} entries into a {}-slot buffer",
                2 * HERO_PREFETCH
            );
            assert_eq!(
                out[0],
                (cur + 1).rem_euclid(n),
                "forward first (n={n}, cur={cur})"
            );
            for i in 0..k {
                assert!(
                    (0..n).contains(&out[i]),
                    "page {} is outside a {n}-page pool",
                    out[i]
                );
                assert_ne!(
                    out[i], cur,
                    "warmed the page already on screen (n={n}, cur={cur})"
                );
                assert!(!out[..i].contains(&out[i]), "warmed page {} twice", out[i]);
            }
        }
    }
}

#[test]
fn the_prefetch_is_armed_only_from_a_settled_hero() {
    assert!(prefetch_armed(0.0, false), "billboard up, nothing moving");
    assert!(
        !prefetch_armed(0.0, true),
        "mid-flip, the incoming layer IS the thing being waited on"
    );
    for &sp in &[0.05f32, 0.2, 1.0] {
        assert!(
            !prefetch_armed(sp, false),
            "at sp={sp} the billboard is on its way out"
        );
    }
}

#[test]
fn the_ground_is_skipped_only_when_opaque_art_covers_it() {
    assert!(
        wash_hidden(0.0, 1.0, None),
        "one fully revealed layer at rest covers the panel"
    );
    assert!(wash_hidden(0.0, 1.0, Some(1.0)), "…and so do two, mid-flip");
    assert!(
        !wash_hidden(0.0, 0.7, None),
        "a layer still dissolving in shows the ground through"
    );
    assert!(
        !wash_hidden(0.0, 1.0, Some(0.7)),
        "…and so does the OTHER layer of a flip"
    );
    assert!(
        !wash_hidden(0.0, 0.0, None),
        "no art at all is exactly what the ground is for"
    );
    // the snap both fades the art by (1 - sp) and slides it up off the bottom of the panel, so
    // full reveals do not mean coverage once the dive has begun
    assert!(
        !wash_hidden(0.2, 1.0, Some(1.0)),
        "a diving hero uncovers the panel however revealed its art is"
    );
    assert!(
        !wash_hidden(1.0, 1.0, None),
        "the grid shows the ground (which by then is the flat surface)"
    );
}

#[test]
fn the_home_hero_logo_never_reaches_the_top_bar() {
    const META_H: f32 = 28.0 * 1.32;
    let synopsis = crate::ui::hero_syn_h(crate::ui::HERO_SYN_MAXLINES);
    let band = hero_logo::band_h(LogoRung::Hero);
    let top = hero_stack_top(band, META_H, theme::space::SM + synopsis);
    assert!(top - (theme::logo::HERO_H_MAX - band) > crate::ui::widgets::TOP_BAR_BOTTOM);
}

#[test]
fn the_hero_logo_key_is_the_shows_for_an_episode() {
    let ep = PmsMovie {
        kind: 3,
        rk: "42".into(),
        show_rk: "7".into(),
        ..Default::default()
    };
    assert_eq!(
        hero_logo_rk(&ep),
        "7",
        "an episode's hero wears the show's logotype"
    );
    let orphan = PmsMovie {
        kind: 3,
        rk: "42".into(),
        ..Default::default()
    };
    assert_eq!(
        hero_logo_rk(&orphan),
        "42",
        "…falling back to its own when the server sent no parent"
    );
    let movie = PmsMovie {
        kind: 0,
        rk: "42".into(),
        show_rk: "7".into(),
        ..Default::default()
    };
    assert_eq!(
        hero_logo_rk(&movie),
        "42",
        "a movie is never keyed to a stray parent"
    );
}

#[test]
fn the_continue_watching_caption_promises_time_left_only_when_the_bar_is_drawn() {
    let ep = |resume_ms: i64| PmsMovie {
        kind: 3,
        dur_ns: 45 * 60 * 1_000_000_000,
        resume_ms,
        show_title: "Laura".into(),
        ..Default::default()
    };
    // never started, stopped exactly at the end, and a stale offset PAST it
    for m in [ep(0), ep(45 * 60_000), ep(60 * 60_000)] {
        assert!(
            m.resume_frac().is_none(),
            "offset {} is not in progress",
            m.resume_ms
        );
        let cap = card_row::focused_caption(&m, true).expect("a Continue Watching episode always captions");
        assert!(
            !cap.to_str().unwrap().contains("left"),
            "offset {}: no bar, so the caption must not promise time remaining ({cap:?})",
            m.resume_ms
        );
    }
    let mid = ep(20 * 60_000);
    assert!(
        mid.resume_frac().is_some(),
        "20 minutes into 45 IS in progress"
    );
    assert_eq!(
        card_row::focused_caption(&mid, true).unwrap().to_str().unwrap(),
        "Laura \u{00b7} 25 min left"
    );
}

fn top_band_bottom() -> f32 {
    let chip = crate::ui::widgets::CHIP_CAP_MAX;
    assert_eq!(chip.y + chip.h, crate::ui::widgets::TOP_BAR_BOTTOM,
        "the chip capsule and the tab track are one band");
    chip.y + chip.h
}
fn heading_top(row: usize, focus_row: usize, scroll: f32) -> f32 {
    let lift = if row == focus_row {
        card_row::heading_lift_max(&RowStyle::HOME)
    } else {
        0.0
    };
    heading_y(
        GRID_TOP_Y + shelf_top_settled(row, focus_row) - scroll,
        lift,
    )
}
fn settled_scroll(rows: usize, focus_row: usize, current: f32) -> f32 {
    let (lo, hi) = row_reveal_band(shelf_top_settled(focus_row, focus_row));
    card_row::reveal(current, lo, hi, grid_max_scroll(rows))
}
fn from_below(rows: usize) -> f32 {
    grid_max_scroll(rows) + ROW_PITCH
}

#[test]
fn the_first_shelfs_raised_heading_settles_clear_of_the_profile_chip() {
    assert!(heading_top(0, 0, settled_scroll(5, 0, from_below(5))) >= top_band_bottom());
}

#[test]
fn no_shelf_heading_settles_inside_the_shared_top_band() {
    const HEADING_MAX_H: f32 = 2.0 * theme::size::HEADLINE as f32;
    for rows in 1..=MAX_HUBS {
        for focus_row in 0..rows {
            let scrolls = [
                settled_scroll(rows, focus_row, 0.0),
                settled_scroll(rows, focus_row, from_below(rows)),
            ];
            for row in 0..rows {
                let ys = [
                    heading_top(row, focus_row, scrolls[0]),
                    heading_top(row, focus_row, scrolls[1]),
                ];
                let (lo, hi) = (ys[0].min(ys[1]), ys[0].max(ys[1]));
                assert!(lo >= top_band_bottom() || hi + HEADING_MAX_H <= top_band_bottom());
            }
        }
    }
}

#[test]
fn every_settled_row_keeps_its_focused_label_block_above_the_overscan_bottom() {
    for rows in 1..=MAX_HUBS {
        for focus_row in 0..rows {
            for scroll in [
                settled_scroll(rows, focus_row, 0.0),
                settled_scroll(rows, focus_row, from_below(rows)),
            ] {
                let row_y = GRID_TOP_Y + shelf_top_settled(focus_row, focus_row) - scroll;
                assert!(row_y + CARD_DY + CARD_H + card_row::UNDER_LABEL_H <= SCR_H - MARGIN_Y);
            }
        }
    }
}

#[test]
fn the_grids_resting_top_is_the_highest_a_shelf_may_settle() {
    assert_eq!(row_reveal_band(0.0).1, 0.0);
    assert_eq!(
        row_reveal_band(shelf_top_settled(3, 3)).1,
        shelf_top_settled(3, 3)
    );
    assert!(heading_top(0, 0, 0.0) - top_band_bottom() >= theme::space::XS);
}

struct Run {
    text: String,
    dx: f32,
    sz: i32,
    bold: i32,
    ink: [f32; 4],
}
fn width_of(text: &str, size: i32, bold: i32) -> f32 {
    text.chars().count() as f32 * (size as f32 + 6.0 * bold as f32)
}
fn heading_flow(title: &str, source: &str) -> (f32, Vec<Run>) {
    let mut runs = Vec::new();
    let width = card_row::heading_flow(title, source, |text, dx, size, bold, ink| {
        runs.push(Run {
            text: text.into(),
            dx,
            sz: size,
            bold,
            ink,
        });
        width_of(text, size, bold)
    });
    (width, runs)
}

#[test]
fn a_shelf_with_no_source_draws_exactly_the_title_and_nothing_else() {
    let (w, runs) = heading_flow("Recently Added", "");
    assert_eq!(
        runs.len(),
        1,
        "an empty source must produce no further runs at all"
    );
    assert_eq!(runs[0].text, "Recently Added");
    assert_eq!(runs[0].dx, 0.0, "the title starts at the heading origin");
    assert_eq!(
        w,
        width_of("Recently Added", theme::size::HEADLINE, 1),
        "the title's own advance, exactly"
    );
}

#[test]
fn a_shared_source_extends_the_heading_past_the_title() {
    let (bare, _) = heading_flow("Recently Added in Film Club", "");
    let (annotated, runs) = heading_flow("Recently Added in Film Club", "friend");
    assert!(
        annotated > bare,
        "the annotation must extend the heading ({annotated} vs {bare})"
    );
    assert_eq!(runs.len(), 3, "title, separator, handle");
    assert_eq!(runs[0].dx, 0.0, "the title still starts at the origin");
    assert_eq!(
        annotated,
        bare + 2.0 * SOURCE_PAD
            + width_of("\u{b7}", theme::size::BODY, 0)
            + width_of("friend", theme::size::BODY, 0),
        "the growth is exactly the dot, the handle and one pad either side of the dot"
    );
    assert_eq!(
        runs[1].dx,
        bare + SOURCE_PAD,
        "the dot is one pad past the title"
    );
    assert_eq!(
        runs[2].dx,
        runs[1].dx + width_of("\u{b7}", theme::size::BODY, 0) + SOURCE_PAD,
        "the handle is one pad past the dot"
    );
}

#[test]
fn each_heading_run_carries_its_own_size_weight_and_ink() {
    let (_, runs) = heading_flow("Recently Added in Film Club", "friend");
    assert_eq!(
        (runs[0].sz, runs[0].bold),
        (theme::size::HEADLINE, 1),
        "the title is HEADLINE bold"
    );
    assert_eq!(
        runs[0].ink,
        theme::TEXT_HEADING,
        "…in the shared section-heading ink"
    );
    assert_eq!(runs[1].text, "\u{b7}");
    assert_eq!(
        (runs[1].sz, runs[1].bold),
        (theme::size::BODY, 0),
        "the separator is measured at BODY regular"
    );
    assert_eq!(
        runs[1].ink,
        theme::TEXT_SEPARATOR,
        "…at the separator token's own .45"
    );
    assert_eq!(runs[2].text, "friend");
    assert_eq!(
        (runs[2].sz, runs[2].bold),
        (theme::size::BODY, 0),
        "the handle is BODY regular, not the title's"
    );
    assert_eq!(runs[2].ink, theme::TEXT_TERTIARY);
    assert!(
        (runs[1].sz, runs[1].bold) == (runs[2].sz, runs[2].bold),
        "the dot and the handle are one annotation: same rung, same weight, so they drop onto the title's baseline together"
    );
    assert!(runs[1].sz < runs[0].sz, "the annotation is a rung DOWN from the title — the drop is why it must be baseline-aligned");
}

struct MetaRun {
    text: String,
    dx: f32,
    budget: f32,
    sz: i32,
    bold: i32,
    ink: [f32; 4],
}
fn meta_flow(base: f32, source: &str) -> (f32, Vec<MetaRun>) {
    let mut runs = Vec::new();
    let width = meta_source_flow(base, source, |text, dx, budget, size, bold, ink| {
        runs.push(MetaRun {
            text: text.into(),
            dx,
            budget,
            sz: size,
            bold,
            ink,
        });
        width_of(text, size, bold).min(budget.max(0.0))
    });
    (width, runs)
}

#[test]
fn a_hero_from_our_own_server_draws_no_source_run_at_all() {
    let (w, runs) = meta_flow(420.0, "");
    assert!(
        runs.is_empty(),
        "an empty source must produce no runs, no pad and no dot"
    );
    assert_eq!(w, 420.0, "…and must not move the line's own end by a pixel");
}

#[test]
fn a_borrowed_hero_states_its_owner_as_the_last_run_on_the_line() {
    let base = 420.0;
    let (w, runs) = meta_flow(base, "friend");
    assert_eq!(runs.len(), 2, "the separator and the run, and nothing else");
    assert_eq!(runs[0].text, "\u{b7}");
    assert_eq!(
        runs[0].dx,
        base + SOURCE_PAD,
        "the dot is one pad past the facts"
    );
    assert_eq!(
        runs[1].text, "Shared by friend",
        "the person, not the machine"
    );
    assert_eq!(
        runs[1].dx,
        runs[0].dx + width_of("\u{b7}", theme::size::BODY, 0) + SOURCE_PAD,
        "the run is one pad past the dot"
    );
    assert_eq!(
        w,
        base + 2.0 * SOURCE_PAD
            + width_of("\u{b7}", theme::size::BODY, 0)
            + width_of("Shared by friend", theme::size::BODY, 0),
        "the line grows by exactly the dot, the run and one pad either side of the dot"
    );
}

#[test]
fn the_hero_source_run_keeps_the_lines_rung_and_takes_one_step_of_ink() {
    let (_, runs) = meta_flow(420.0, "friend");
    for r in &runs {
        assert_eq!(
            (r.sz, r.bold),
            (theme::size::BODY, 0),
            "'{}' left the meta line's own rung",
            r.text
        );
        assert_ne!(
            r.sz,
            theme::size::CAPTION,
            "the hero line is BODY — it is not E's line and must not shrink to it"
        );
    }
    assert_eq!(
        runs[0].ink,
        theme::TEXT_SEPARATOR,
        "the middot carries the separator token's own .45"
    );
    assert_eq!(
        runs[1].ink,
        theme::TEXT_TERTIARY,
        "one step under the line's TEXT_SECONDARY, never level with it"
    );
    assert_ne!(runs[1].ink, theme::TEXT_SECONDARY);
}

#[test]
fn an_over_long_handle_truncates_rather_than_wrapping() {
    let ridiculous = "a-very-long-plex-account-handle-that-nobody-would-ever-choose";
    for base in [0.0f32, 420.0, HERO_COL_W] {
        let (w, runs) = meta_flow(base, ridiculous);
        assert_eq!(
            runs.len(),
            2,
            "base {base}: a long handle is still ONE run — there is no second line to go to"
        );
        for r in &runs {
            assert!(
                r.budget > 0.0,
                "base {base}: '{}' was given no room at all ({})",
                r.text,
                r.budget
            );
            assert!(
                r.dx + r.budget <= META_FLOW_W + 1e-3,
                "base {base}: '{}' may elide past the bound",
                r.text
            );
        }
        assert!(
            w <= META_FLOW_W + 1e-3,
            "base {base}: the line ran to {w}, past its {META_FLOW_W} bound"
        );
    }
    // …and the room left at that worst case is a real annotation's worth rather than a stub —
    // a QUARTER of the whole line, which is the guard that a future widening of the hero column
    // (or a tightening of the bound) cannot quietly starve the run into a bare ellipsis. It is
    // stated as a share of the line and not in pixels of text because the synthetic metric here
    // is roughly twice the shipped font's advance; the share is a claim about the geometry,
    // which is the part the host can actually speak for.
    let (_, worst) = meta_flow(HERO_COL_W, "friend");
    assert!(
        worst[1].budget >= 0.25 * META_FLOW_W,
        "a meta line whose facts fill the column leaves the run only {} of {META_FLOW_W}",
        worst[1].budget
    );
}

#[test]
fn the_meta_lines_bound_keeps_the_run_inside_the_hero_wedge() {
    use crate::ui::widgets::hero_scrim_a;
    assert!(
        hero_scrim_a(HERO_META_R, 1.0) > 0.0,
        "the line ends where the wedge has already given up"
    );
    let wedge_end = (0..=SCR_W as i32)
        .map(|x| x as f32)
        .find(|&x| hero_scrim_a(x, 1.0) <= 0.0)
        .expect("the wedge must end inside the frame");
    assert!(
        HERO_META_R <= wedge_end - 0.2 * SCR_W,
        "the bound ({HERO_META_R}) must stay a fifth of the panel short of the wedge's end ({wedge_end})"
    );
    assert!(
        HERO_META_R > MARGIN_X + HERO_COL_W,
        "…and past the text column, or the run has nowhere to go"
    );
}

// Additional phase-8 ownership proofs: real engine movement, stable identity, effects and map.

#[test]
fn engine_links_hero_to_the_first_shelf_and_back() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let s = screen(snapshot.view());
    let owner = InputOwner::Entry(s.entry);
    let hero = FocusKey {
        entry: s.entry,
        elem: HERO_PLAY_ELEM,
    };
    let context = cx(snapshot.view(), Some(hero));
    let mut engine = FocusEngine::new();
    engine.set(owner, hero, Some(HERO_GROUP), By::Restore);
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    let Outcome::Moved { to, .. } = engine.move_dir(owner, &s, &links, Dir::Down, &context) else {
        panic!("hero DOWN must move")
    };
    assert_eq!(to.elem, s.rows[0].elems[0]);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn down_from_the_first_shelf_chooses_the_next_shelf_not_the_folded_hero() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let second_elem = s.elem_for(HomeItemIdentity::Item {
        hub: HomeHubIdentity::Key {
            sid: crate::plex::ServerId::UNSET,
            key: "/hubs/second".into(),
        },
        sid: crate::plex::ServerId::UNSET,
        rk: "second".into(),
    });
    s.rows.push(HubProjection {
        identity: HomeHubIdentity::Key {
            sid: crate::plex::ServerId::UNSET,
            key: "/hubs/second".into(),
        },
        group: GroupId(FIRST_HUB_GROUP + 1),
        elems: vec![second_elem],
    });
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();

    let first = first_card(&s);
    let owner = InputOwner::Entry(s.entry);
    let context = cx(snapshot.view(), Some(first));
    let mut engine = FocusEngine::new();
    engine.set(owner, first, Some(s.rows[0].group), By::Restore);
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    let Outcome::Moved { to, .. } = engine.move_dir(owner, &s, &links, Dir::Down, &context) else {
        panic!("first-shelf DOWN must reach the second shelf");
    };
    assert_eq!(to.elem, second_elem);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn down_from_the_last_shelf_never_reenters_the_offscreen_hero() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    let last = FocusKey {
        entry: s.entry,
        elem: *s.rows[0].elems.last().unwrap(),
    };
    let owner = InputOwner::Entry(s.entry);
    let context = cx(snapshot.view(), Some(last));
    let mut engine = FocusEngine::new();
    engine.set(owner, last, Some(s.rows[0].group), By::Restore);
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    assert_eq!(
        engine.move_dir(owner, &s, &links, Dir::Down, &context),
        Outcome::Nothing
    );
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn repeated_item_keys_are_scoped_by_hub_identity() {
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    let a = HomeItemIdentity::Item {
        hub: HomeHubIdentity::ContinueWatching,
        sid: crate::plex::ServerId::UNSET,
        rk: "7".into(),
    };
    let b = HomeItemIdentity::Item {
        hub: HomeHubIdentity::Key {
            sid: crate::plex::ServerId::UNSET,
            key: "/hubs/new".into(),
        },
        sid: crate::plex::ServerId::UNSET,
        rk: "7".into(),
    };
    let ka = s.elem_for(a.clone());
    assert_eq!(s.elem_for(a), ka);
    assert_ne!(s.elem_for(b), ka);
}

#[test]
fn unknown_provider_identity_is_explicitly_generation_scoped() {
    assert_ne!(
        HomeHubIdentity::Ephemeral {
            generation: 4,
            ordinal: 2
        },
        HomeHubIdentity::Ephemeral {
            generation: 5,
            ordinal: 2
        },
    );
}

#[test]
fn memory_round_trip_preserves_registries_and_carousel_identity() {
    let mut a = HomeScreen::new(EntryId(7), InstanceId(9));
    let hub = HomeHubIdentity::ContinueWatching;
    a.group_for(&hub);
    a.elem_for(HomeItemIdentity::Item {
        hub,
        sid: crate::plex::ServerId::UNSET,
        rk: "42".into(),
    });
    a.carousel = Some((crate::plex::ServerId::UNSET, "42".into()));
    a.strip_chosen = true;
    let memory = match <HomeScreen as Screen<TestHost>>::memory(&a) {
        PageMemory::Home(m) => m,
        _ => unreachable!(),
    };
    let mut b = HomeScreen::new(EntryId(7), InstanceId(10));
    b.restore(&memory);
    assert_eq!(b.groups, a.groups);
    assert_eq!(b.items, a.items);
    assert_eq!(b.carousel, a.carousel);
    assert_eq!(b.strip_chosen, a.strip_chosen);
}

#[test]
fn activation_across_the_snap_midpoint_is_not_a_canonical_collision() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut hero_picture = screen(snapshot.view());
    let mut grid_picture = screen(snapshot.view());
    for s in [&mut hero_picture, &mut grid_picture] {
        s.carousel = Some((crate::plex::ServerId::UNSET, "2".into()));
        s.snap_target = 1.0;
        s.visible_activation = Some(HERO_PLAY_ELEM);
    }
    hero_picture.snap.jump(0.49);
    grid_picture.snap.jump(0.51);
    let card = first_card(&hero_picture);
    let event = ScreenEvent::PressCommit(crate::ui::machine::PressId(1));
    let (_, a, _) = step(&mut hero_picture, snapshot.view(), Some(card), &event);
    let (_, b, _) = step(&mut grid_picture, snapshot.view(), Some(card), &event);
    assert!(has_home(&a, |r| matches!(r, HomeReq::Play { rk, .. } if rk == "2")));
    assert!(has_home(&b, |r| matches!(r, HomeReq::Play { rk, .. } if rk == "1")));
    assert_ne!(hero_picture.hash(), grid_picture.hash(),
        "the same input activates different items, so these cannot be the same logical state");
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn the_home_census_covers_input_motion_and_current_projection() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let baseline = screen(snapshot.view()).hash();
    let changes: &[fn(&mut HomeScreen)] = &[
        |s| s.snap.vel = 1.0,
        |s| s.hero_slide.pos = 0.4,
        |s| s.hero_slide.vel = 1.0,
        |s| s.hero_dir = -1.0,
        |s| s.outgoing = Some((crate::plex::ServerId::UNSET, "old".into())),
        |s| s.grid.scroll_y.vel = 1.0,
        |s| s.grid.scroll_target = 100.0,
        |s| s.rows[0].elems.swap(0, 1),
        |s| s.items[0].last_row += 1,
        |s| s.items[0].last_col += 1,
        |s| s.projected_generation = None,
        |s| s.hero_pop.step(Some(0), 0.016),
        |s| s.grid.shelves[0].update(3, Some(1), &RowStyle::HOME, 0.016),
    ];
    for (i, change) in changes.iter().enumerate() {
        let mut s = screen(snapshot.view());
        change(&mut s);
        assert_ne!(s.hash(), baseline, "census omitted input-state variation {i}");
    }
    // These extents are part of SHAPE, not merely runtime sequence lengths.
    assert_eq!(HERO_NBTN, 2);
    assert_eq!(crate::ui::card_row::MAX_ROW_ITEMS, 24);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn paint_only_backdrop_and_spinner_state_do_not_change_the_canonical_hash() {
    let mut a = HomeScreen::new(EntryId(7), InstanceId(9));
    let mut b = HomeScreen::new(EntryId(7), InstanceId(9));
    b.status_ms = 900.0;
    b.backdrop.art.pos = 0.5;
    assert_eq!(a.hash(), b.hash());
    a.carousel = Some((crate::plex::ServerId::UNSET, "a".into()));
    assert_ne!(a.hash(), b.hash());
}

#[test]
fn an_explicit_hero_reseat_keeps_the_fold_animation_unlike_page_restoration() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let from = first_card(&s);
    let to = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    step(&mut s, snapshot.view(), Some(to), &ScreenEvent::FocusMoved {
        from: Some(from), to, by: By::Restore,
    });
    assert_eq!(s.snap_target, 0.0);
    assert_eq!(s.snap.pos, 1.0, "fresh reseating still animates the door");
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn shelf_viewports_follow_identity_across_a_catalog_reorder() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_grid_for_test(2, 24);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let identity = s.rows[0].identity.clone();
    s.grid.shelves[0].restore_scroll(900.0, 24, &RowStyle::HOME);
    crate::pms::reverse_test_hubs();
    let changed = crate::pms::hubs_snapshot();
    s.sync_catalog(&cx(changed.view(), None));
    assert_eq!(s.rows[1].identity, identity);
    assert_eq!(s.grid.shelves[1].scroll_x(), 900.0);
    assert_eq!(s.grid.shelves[0].scroll_x(), 0.0);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn an_empty_loading_publication_does_not_consume_restored_viewports() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_grid_for_test(2, 24);
    let snapshot = crate::pms::hubs_snapshot();
    let mut original = screen(snapshot.view());
    original.grid.shelves[1].restore_scroll(900.0, 24, &RowStyle::HOME);
    let PageMemory::Home(memory) = <HomeScreen as Screen<TestHost>>::memory(&original) else { unreachable!() };
    crate::pms::seed_for_test(0, crate::pms::HubState::Loading);
    let loading = crate::pms::hubs_snapshot();
    let mut restored = screen(loading.view());
    restored.restore(&memory);
    restored.sync_catalog(&cx(loading.view(), None));
    // Another eviction while loading must not discard the still-unmatched saved rows.
    let PageMemory::Home(pending) = <HomeScreen as Screen<TestHost>>::memory(&restored) else { unreachable!() };
    let mut restored = screen(loading.view());
    restored.restore(&pending);
    crate::pms::seed_grid_for_test(2, 24);
    let ready = crate::pms::hubs_snapshot();
    restored.sync_catalog(&cx(ready.view(), None));
    assert_eq!(restored.grid.shelves[1].scroll_x(), 900.0);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn viewport_memory_is_canonical_because_return_reuses_it() {
    let a = HomeScreen::new(EntryId(7), InstanceId(9));
    let mut b = HomeScreen::new(EntryId(7), InstanceId(9));
    b.grid.scroll_y.pos = 320.0;
    assert_ne!(a.hash(), b.hash());
    assert_ne!(<HomeScreen as Screen<TestHost>>::memory(&a).hash(),
        <HomeScreen as Screen<TestHost>>::memory(&b).hash());
}

#[test]
fn visible_tick_emits_both_store_work_requests_after_the_step() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(0, crate::pms::HubState::Loading);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    let work: Vec<_> = out
        .iter()
        .filter_map(|stamped| match stamped.fx {
            Fx::App(AppFx::StoreWork(work)) => Some(work),
            _ => None,
        })
        .collect();
    assert_eq!(work, vec![StoreWork::Hubs, StoreWork::BrowseDiscovery]);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn continue_watching_commit_plays_while_an_ordinary_shelf_opens_detail() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let card = first_card(&s);
    let (_, play, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
    );
    assert!(has_home(
        &play,
        |r| matches!(r, HomeReq::Play { rk, .. } if rk == "1")
    ));
    s.rows[0].identity = HomeHubIdentity::Key {
        sid: crate::plex::ServerId::UNSET,
        key: "/hubs/recent".into(),
    };
    let (_, detail, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(2)),
    );
    assert!(has_home(
        &detail,
        |r| matches!(r, HomeReq::Detail { rk, .. } if rk == "1")
    ));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn holding_a_shelf_card_opens_the_item_menu_without_activation() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let card = first_card(&s);
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::PressHold(crate::ui::machine::PressId(1)),
    );
    assert!(has_home(
        &out,
        |r| matches!(r, HomeReq::ItemMenu { rk, .. } if rk == "1")
    ));
    assert!(!has_home(&out, |r| matches!(
        r,
        HomeReq::Play { .. } | HomeReq::Detail { .. }
    )));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn a_partially_visible_focused_row_allows_hover_to_its_neighbors_only() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.grid.shelves[0].update(2, Some(0), &RowStyle::HOME, 1.0);
    s.layout_grid();
    s.grid.shelves[0].base_y = -20.0;
    let first = first_card(&s);
    let neighbor = s.rows[0].elems[1];
    for (focus, expected) in [(Some(first), Hover::Focus), (None, Hover::OnlyIfFocused)] {
        let context = cx(snapshot.view(), focus);
        let mut frame = DrawFrame::new(&context, Painter::root());
        s.record_stops(&mut frame, snapshot.view());
        let stops = frame.into_stops();
        let stop = stops.iter().find(|stop| stop.key.elem == neighbor).unwrap();
        assert!(stop.rest_rect.y < 40.0);
        assert_eq!(stop.hover, expected);
    }
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn drawn_stops_feed_the_real_hit_map_with_scoped_card_keys() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let card = first_card(&s);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.grid.shelves[0].update(2, Some(0), &RowStyle::HOME, 1.0);
    s.layout_grid();
    let context = cx(snapshot.view(), Some(card));
    let mut frame = DrawFrame::new(&context, Painter::root());
    s.record_stops(&mut frame, snapshot.view());
    let mut map = HitMap::new();
    map.fill(frame.into_stops());
    map.swap();
    let placed = Focusable::<TestHost>::place(&s, &card.elem, &context, At::Drawn).unwrap();
    let resolution = map.resolve(Some(card.entry), PointerKind::Click,
        placed.rect.cx(),
        placed.rect.cy(),
        Some(card),
    );
    assert_eq!(resolution.hit, Some(card));
    assert_eq!(resolution.activate, Some((card, Activate::Press)));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn quick_down_then_ok_activates_the_hero_still_visible_before_the_snap_midpoint() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.carousel = Some((crate::plex::ServerId::UNSET, "2".into()));
    let hero = FocusKey {
        entry: s.entry,
        elem: HERO_PLAY_ELEM,
    };
    let card = first_card(&s);
    let move_event = ScreenEvent::FocusMoved {
        from: Some(hero),
        to: card,
        by: By::Dir,
    };
    let _ = step(&mut s, snapshot.view(), Some(card), &move_event);
    assert_eq!(s.snap_target, 1.0);
    assert!(s.snap.pos < 0.5);
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(7)),
    );
    assert!(has_home(
        &out,
        |req| matches!(req, HomeReq::Play { rk, .. } if rk == "2")
    ));
    assert!(!has_home(
        &out,
        |req| matches!(req, HomeReq::Play { rk, .. } if rk == "1")
    ));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn back_from_grid_folds_to_hero_before_root_back() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let card = first_card(&s);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let back = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    });
    let (_, fold, _) = step(&mut s, snapshot.view(), Some(card), &back);
    assert_eq!(s.snap_target, 0.0);
    assert!(has_home(&fold, |req| matches!(req, HomeReq::FoldToHero)));
    assert!(!fold
        .iter()
        .any(|stamped| matches!(&stamped.fx, Fx::App(AppFx::Loop(LoopReq::BackAtRoot)))));

    s.snap.jump(0.0);
    let hero = FocusKey {
        entry: s.entry,
        elem: HERO_PLAY_ELEM,
    };
    let (_, root, _) = step(&mut s, snapshot.view(), Some(hero), &back);
    assert!(root
        .iter()
        .any(|stamped| matches!(&stamped.fx, Fx::App(AppFx::Loop(LoopReq::BackAtRoot)))));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

fn enter_target(out: &[Stamped<TestHost>]) -> Option<FocusTarget<u32>> {
    out.iter().find_map(|stamped| match &stamped.fx {
        Fx::Deliver(
            MachineId::Instance(InstanceId(9)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus })),
        ) => Some(*focus),
        _ => None,
    })
}

#[test]
fn addressed_focus_commands_reseat_only_through_enter_fresh() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    let card = first_card(&s);

    let (_, grid, _) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 0, col: 0 })),
    );
    assert!(matches!(enter_target(&grid), Some(FocusTarget::Elem(key)) if key == card));

    let (_, hero, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::App(AppMsg::Home(HomeCmd::Hero)),
    );
    assert!(matches!(
        enter_target(&hero),
        Some(FocusTarget::ContainerGroup(HERO_GROUP))
    ));

    for (tab, elem) in [
        (HomeTab::Home, STRIP_HOME_ELEM),
        (HomeTab::Movies, STRIP_MOVIES_ELEM),
        (HomeTab::Shows, STRIP_SHOWS_ELEM),
        (HomeTab::Search, STRIP_SEARCH_ELEM),
    ] {
        let (_, strip, _) = step(
            &mut s,
            snapshot.view(),
            None,
            &ScreenEvent::App(AppMsg::Home(HomeCmd::FocusStrip(tab))),
        );
        assert!(matches!(enter_target(&strip), Some(FocusTarget::Elem(key)) if key.elem == elem));
    }
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn addressed_carousel_commands_mutate_the_owned_identity_not_focus() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());

    let (handled, _, _) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::App(AppMsg::Home(HomeCmd::SelectHero(2))),
    );
    assert_eq!(handled, Handled::Yes);
    assert_eq!(s.carousel.as_ref().map(|(_, rk)| rk.as_str()), Some("3"));

    s.hero_flip_cd = 0.0;
    let before = s.carousel.clone();
    let _ = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::App(AppMsg::Home(HomeCmd::Flip(1))),
    );
    assert_ne!(s.carousel, before);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn loading_has_no_phantom_hero_and_terminal_status_has_one_action() {
    let _guard = crate::testlock::serial();
    for (state, expected) in [
        (crate::pms::HubState::Loading, 0),
        (crate::pms::HubState::Failed, 1),
        (crate::pms::HubState::Ready, 1),
    ] {
        crate::pms::seed_for_test(0, state);
        let snapshot = crate::pms::hubs_snapshot();
        let s = screen(snapshot.view());
        let context = cx(snapshot.view(), None);
        let mut groups = Vec::new();
        Focusable::<TestHost>::groups(&s, &context, &mut groups);
        assert_eq!(groups[0].len, expected);
        let retry = FocusKey {
            entry: s.entry,
            elem: HERO_PLAY_ELEM,
        };
        assert!(matches!(
            Focusable::<TestHost>::neighbour(&s, retry, Dir::Right, &context),
            Step::Edge
        ));
        assert_eq!(
            Focusable::<TestHost>::group_of(&s, &HERO_INFO_ELEM, &context),
            None
        );
    }
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn first_catalog_landing_reseats_the_default_cta_unless_the_strip_was_chosen() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(0, crate::pms::HubState::Loading);
    let empty = crate::pms::hubs_snapshot();
    let mut automatic = screen(empty.view());
    let fallback = FocusKey {
        entry: automatic.entry,
        elem: STRIP_ACCOUNT_ELEM,
    };

    crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
    let landed = crate::pms::hubs_snapshot();
    let (_, out, _) = step(
        &mut automatic,
        landed.view(),
        Some(fallback),
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    assert!(matches!(
        enter_target(&out),
        Some(FocusTarget::ContainerGroup(HERO_GROUP))
    ));

    crate::pms::seed_for_test(0, crate::pms::HubState::Loading);
    let empty = crate::pms::hubs_snapshot();
    let mut chosen = screen(empty.view());
    let moved = ScreenEvent::FocusMoved {
        from: None,
        to: fallback,
        by: By::Pointer,
    };
    let _ = step(&mut chosen, empty.view(), Some(fallback), &moved);
    crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
    let landed = crate::pms::hubs_snapshot();
    let (_, out, _) = step(
        &mut chosen,
        landed.view(),
        Some(fallback),
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    assert!(enter_target(&out).is_none());
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn addressed_item_menu_uses_the_current_owned_grid_item() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(1, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let card = first_card(&s);
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::App(AppMsg::Home(HomeCmd::ItemMenu)),
    );
    assert!(has_home(
        &out,
        |req| matches!(req, HomeReq::ItemMenu { rk, .. } if rk == "1")
    ));
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn parent_read_only_api_projects_engine_focus_without_setters() {
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    let s = screen(snapshot.view());
    let card = first_card(&s);
    let context = cx(snapshot.view(), Some(card));

    assert!(SHAPE.contains("visible_activation:Option<u32>"));
    assert_eq!(
        s.hero_item(&context).map(|item| item.rk.as_str()),
        Some("1")
    );
    assert_eq!(
        s.focused_item(Some(card), &context)
            .map(|item| item.rk.as_str()),
        Some("1")
    );
    assert_eq!(s.grid_position(Some(card), &context), Some((0, 0)));
    assert_eq!(s.snap_target(), 0.0);
    assert!(s.focused_rect(Some(card), &context, At::Drawn).is_some());

    // The lifted redraw query is safe with no focused key and never reaches a global cursor.
    let empty_context = cx(snapshot.view(), None);
    let mut frame = DrawFrame::new(&empty_context, Painter::root());
    s.redraw_focused(&mut frame, None);
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}

#[test]
fn the_hero_carousel_auto_advance_reports_motion_on_every_tick_while_armed() {
    // D4 (phase 12): `hero_auto`'s countdown itself is deliberately UNCHANGED arithmetic — it is
    // hashed `LogicalState` across three committed replay fixtures, and moving it onto
    // `motion::Ramp` measurably diverged the hash (`tests/focusfp.sh --replay`, flow 1). What DID
    // change is the actual bug this conversion exists to catch: the countdown never reported
    // `Motion`, so a hero left mid-count-down on an otherwise-settled screen could silently freeze
    // under `ui::idle`'s present gate, exactly like the `Xfade`/`Spinner` cases the module doc
    // already names. `tick` now notes `Motion` explicitly every frame the countdown runs.
    let _guard = crate::testlock::serial();
    crate::pms::seed_for_test(2, crate::pms::HubState::Ready);
    let snapshot = crate::pms::hubs_snapshot();
    assert!(
        snapshot.view().hero_count() > 1,
        "the auto-advance branch only arms with more than one hero slot"
    );
    let mut s = screen(snapshot.view());
    let (_, _, motion) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    assert!(
        motion,
        "the hero countdown must report Motion on every tick while the carousel auto-advance is armed"
    );
    // A second tick, still short of HERO_AUTO_S, keeps reporting motion rather than going quiet
    // once started.
    let (_, _, motion_again) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::Tick(Tick {
            ms: 32,
            dt_us: 16_000,
        }),
    );
    assert!(motion_again, "the ramp must keep reporting motion on the next tick too");
    crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset);
}
