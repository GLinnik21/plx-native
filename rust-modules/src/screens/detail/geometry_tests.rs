//! Geometry-only regressions for Detail's production focus and hit contracts.
//!
//! This file is intentionally separate from the migration census. It exercises
//! DetailScreen::place and HitMap together, so the test does not re-implement
//! any strip geometry or pointer inverse.

use super::*;
use crate::ui::focus::{FocusEngine, Outcome};
use crate::ui::hit::{HitMap, PointerKind};
use crate::ui::machine::{Chrome, Host, InputOwner, PressRead, ScreenId};
use crate::ui::screen::{Activate, At, By, Focusable, Hover, ScreenArg, Stop};

#[derive(Clone, PartialEq, Eq)]
struct TestArg;

impl LogicalState for TestArg {
    fn write(&self, w: &mut Canon) { w.u8(0); }
    fn probe(&self, out: &mut String) { out.push_str("detail-geometry"); }
}

impl ScreenArg for TestArg {
    fn chrome(&self) -> Chrome { Chrome::None }
    fn id(&self) -> ScreenId { ScreenId(701) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, other: &Self) -> bool { self == other }
}

struct TestHost;

impl Host for TestHost {
    type Arg = TestArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    // `super::super::` (detail -> screens -> family) rather than the absolute spelling: `family`
    // is the Settings family's shared vocabulary (the same module `screens::legal` and
    // `screens::settings` already reach with `super::family::`), not a sibling screen — see
    // `screens::family`'s own module doc.
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}

fn cx<'a>(measure: &'a crate::ui::fixture::FixtureMeasure, elem: Option<u32>) -> Cx<'a, TestHost> {
    Cx {
        views: (),
        tick: Default::default(),
        measure,
        press: PressRead::default(),
        focus: crate::ui::machine::FocusRead {
            current: elem.map(|elem| FocusKey { entry: EntryId(8), elem }),
        ..Default::default() },
        owner: crate::ui::machine::InputOwner::Entry(EntryId(8)),
    }
}

fn bare(sid: ServerId, rk: &str) -> DetailScreen {
    let mut screen = DetailScreen {
        entry: EntryId(8),
        sid,
        rk: rk.into(),
        keys: Vec::new(),
        next_elem: FIRST_ITEM_ELEM,
        key_by_local: Default::default(),
        local_by_key: Default::default(),
        return_pending: false,
        pending_season: None,
        season_settle: 0.0,
        restore_intent: None,
        scroll: Spring::at(0.0),
        scroll_target: 0.0,
        episode_scroll: Spring::at(0.0),
        tab_scroll: Spring::at(0.0),
        episode_scale: [Spring::at(1.0); EP_SCALE_MAX],
        related: CardRow::new(),
        cast: CardRow::new(),
        tabs: TabStrip::new(),
        season_pop: CtlPop::new(),
        ctl_pop: CtlPop::new(),
        disc_unfurl: [Spring::at(0.0); 2],
        season_metrics: season::Metrics::new(),
        about_rows: about::Rows::new(),
        ground: AmbientWash::flat(theme::SURFACE_APP),
        spin_ms: 0.0,
        spin_phase: crate::ui::motion::Phase::default(),
    };
    screen.sync_keys();
    screen
}

fn fixture(sid: ServerId) -> Detail {
    let episodes = (0..12).map(|i| crate::metadata::Episode {
        rk: format!("e{i}"),
        index: i,
        season: 1,
        title: format!("Episode {i}"),
        dur_ms: 60_000,
        ..Default::default()
    }).collect();
    let related = (0..12).map(|i| crate::pms::PmsMovie {
        sid,
        rk: format!("r{i}"),
        title: format!("Related {i}"),
        ..Default::default()
    }).collect();
    let cast = (0..12).map(|i| crate::metadata::Cast {
        tag: format!("Person {i}"),
        role: "Role".into(),
        thumb: String::new(),
        id: i,
        tag_key: format!("person-{i}"),
    }).collect();
    Detail {
        sid,
        rk: "show".into(),
        is_show: true,
        kind: "show".into(),
        seasons: vec![crate::metadata::Season {
            rk: "season-1".into(),
            index: 1,
            title: "Season 1".into(),
            leaf_count: 12,
            viewed_leaf_count: 0,
        }],
        episodes,
        related,
        cast,
        ..Default::default()
    }
}

fn install(d: Detail) -> crate::testlock::Serial {
    let guard = crate::testlock::serial();
    crate::metadata::set_current_for_test(Some(d));
    guard
}

fn clear() {
    crate::metadata::set_current_for_test(None);
}

fn stop(key: FocusKey<u32>, placed: Placed) -> Stop<u32> {
    Stop {
        key,
        rect: placed.rect,
        rest_rect: placed.rest_rect,
        clip: placed.clip,
        hover: Hover::Focus,
        activate: Activate::Press,
    }
}

fn move_dir(
    engine: &mut FocusEngine<u32>,
    screen: &DetailScreen,
    owner: InputOwner,
    dir: Dir,
    context: &Cx<'_, TestHost>,
) -> Outcome<u32> {
    engine.move_dir(owner, screen, &[], dir, context)
}

fn expect_move(outcome: Outcome<u32>, expectation: &str) -> FocusKey<u32> {
    match outcome {
        Outcome::Moved { to, .. } => to,
        other => panic!("{expectation}: expected a move, got {other:?}"),
    }
}

fn scroll_to(screen: &mut DetailScreen, section: i32) {
    let top = {
        let detail = screen.detail().expect("fixture detail must be mounted");
        screen.section_top(section, detail)
    };
    screen.scroll.jump(top);
    screen.scroll_target = top;
}

fn assert_strip_hit_geometry(screen: &DetailScreen, elems: &[u32], measure: &crate::ui::fixture::FixtureMeasure) {
    let context = cx(measure, None);
    let placed: Vec<_> = elems.iter().filter_map(|elem| {
        let elem = screen.engine_key(*elem).expect("fixture item has an engine identity");
        Focusable::<TestHost>::place(screen, &elem, &context, At::Drawn).map(|placed| {
            (FocusKey { entry: EntryId(8), elem }, placed)
        })
    }).collect();
    assert_eq!(placed.len(), elems.len(), "every production strip stop must place");

    let mut hit = HitMap::new();
    hit.fill(placed.iter().map(|(key, placed)| stop(*key, *placed)).collect());
    hit.swap();

    let mut visible = 0;
    let mut clipped = 0;
    let mut offscreen = 0;
    for (key, placed) in &placed {
        let shown = placed.rect.intersect(placed.clip).intersect(Rect::FULL);
        if shown.w > 0.0 && shown.h > 0.0 {
            visible += 1;
            if shown.w < placed.rect.w || shown.h < placed.rect.h {
                clipped += 1;
            }
            let resolved = hit.resolve(Some(key.entry), PointerKind::Click, shown.cx(), shown.cy(), None);
            assert_eq!(resolved.hit, Some(*key), "visible intersection must hit its tile");
            assert_eq!(resolved.activate.map(|(hit, _)| hit), Some(*key));
        } else {
            offscreen += 1;
            let resolved = hit.resolve(Some(key.entry), PointerKind::Click, placed.rect.cx(), placed.rect.cy(), None);
            assert_eq!(resolved.hit, None, "a fully offscreen tile must miss");
            assert!(resolved.miss);
        }
    }
    assert!(visible > 0, "fixture must expose visible strip geometry");
    assert!(clipped > 0, "fixture must exercise a clipped strip tile");
    assert!(offscreen > 0, "fixture must exercise a fully offscreen strip tile");

    for pair in placed.windows(2) {
        let left = pair[0].1.rect;
        let right = pair[1].1.rect;
        let gap = right.x - (left.x + left.w);
        if gap > 0.0 {
            let point = (left.x + left.w + gap * 0.5, left.cy());
            if Rect::FULL.contains(point.0, point.1) {
                let resolved = hit.resolve(
                    Some(pair[0].0.entry),
                    PointerKind::Click,
                    point.0,
                    point.1,
                    None,
                );
                assert!(resolved.miss, "the visible gutter between drawn tiles is not a stop");
                assert_eq!(resolved.hit, None);
            }
        }
    }
}

#[test]
fn detail_focus_navigation_walks_the_filmstrip_through_tabs_and_both_rows() {
    let sid = ServerId::UNSET;
    let _guard = install(fixture(sid));
    let mut screen = bare(sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    let detail = crate::metadata::current().expect("fixture detail must be mounted");
    screen.season_metrics.update(detail, &measure);
    let context = cx(&measure, None);
    let owner = InputOwner::Entry(EntryId(8));
    let mut engine = FocusEngine::new();
    let tab = FocusKey { entry: EntryId(8), elem: screen.engine_key(season::elem(0).unwrap()).unwrap() };
    let still = FocusKey { entry: EntryId(8), elem: screen.engine_key(episodes::elem(0, episodes::Row::Still).unwrap()).unwrap() };
    let text = FocusKey { entry: EntryId(8), elem: screen.engine_key(episodes::elem(0, episodes::Row::Text).unwrap()).unwrap() };

    assert!(matches!(engine.set(owner, tab, Some(season::SEASON_GROUP), By::Restore), Outcome::Moved { .. }));
    let got_still = expect_move(
        move_dir(&mut engine, &screen, owner, Dir::Down, &context),
        "season tabs must enter the filmstrip",
    );
    assert_eq!(got_still, still);
    let got_text = expect_move(
        move_dir(&mut engine, &screen, owner, Dir::Down, &context),
        "still must descend to its metadata row",
    );
    assert_eq!(got_text, text);
    let out = expect_move(
        move_dir(&mut engine, &screen, owner, Dir::Down, &context),
        "metadata row must enter the next section",
    );
    assert_eq!(out.elem, screen.engine_key(cast::elem(0).unwrap()).unwrap());

    let back_text = expect_move(
        move_dir(&mut engine, &screen, owner, Dir::Up, &context),
        "cast must return to the metadata row",
    );
    assert_eq!(back_text, text);
    let back_still = expect_move(
        move_dir(&mut engine, &screen, owner, Dir::Up, &context),
        "metadata row must return to still",
    );
    assert_eq!(back_still, still);
    let back_tab = expect_move(
        move_dir(&mut engine, &screen, owner, Dir::Up, &context),
        "still must return to season tabs",
    );
    assert_eq!(back_tab, tab);

    for row in [episodes::Row::Still, episodes::Row::Text] {
        let first = FocusKey { entry: EntryId(8), elem: screen.engine_key(episodes::elem(0, row).unwrap()).unwrap() };
        let last = FocusKey { entry: EntryId(8), elem: screen.engine_key(episodes::elem(11, row).unwrap()).unwrap() };
        let mut at = first;
        assert!(matches!(engine.set(owner, first, Some(episodes::EPISODES_GROUP), By::Restore), Outcome::Moved { .. }));
        let mut right_stops = 0;
        for _ in 0..20 {
            match move_dir(&mut engine, &screen, owner, Dir::Right, &context) {
                Outcome::Moved { to, .. } => at = to,
                Outcome::Edge(_) | Outcome::Nothing => {
                    right_stops += 1;
                    assert_eq!(engine.current(owner), Some(at), "RIGHT edge must preserve focus");
                }
            }
        }
        assert_eq!(at, last, "RIGHT must clamp at the last episode for {row:?}");
        assert!(right_stops > 0, "RIGHT must exercise the last-episode edge for {row:?}");
        let mut left_stops = 0;
        for _ in 0..20 {
            match move_dir(&mut engine, &screen, owner, Dir::Left, &context) {
                Outcome::Moved { to, .. } => at = to,
                Outcome::Edge(_) | Outcome::Nothing => {
                    left_stops += 1;
                    assert_eq!(engine.current(owner), Some(at), "LEFT edge must preserve focus");
                }
            }
        }
        assert_eq!(at, first, "LEFT must clamp at the first episode for {row:?}");
        assert!(left_stops > 0, "LEFT must exercise the first-episode edge for {row:?}");
    }
    clear();
}

#[test]
fn detail_focus_places_and_hit_map_agree_for_all_three_scrolled_strips() {
    let sid = ServerId::UNSET;
    let _guard = install(fixture(sid));
    let mut screen = bare(sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    let episode_elems: Vec<_> = (0..12).map(|i| episodes::elem(i, episodes::Row::Still).unwrap()).collect();
    let related_elems: Vec<_> = (0..12).map(|i| related::elem(i).unwrap()).collect();
    let cast_elems: Vec<_> = (0..12).map(|i| cast::elem(i).unwrap()).collect();

    for scroll in [0.0, 137.0, 1024.0] {
        screen.episode_scroll.jump(scroll);
        scroll_to(&mut screen, 2);
        assert_strip_hit_geometry(&screen, &episode_elems, &measure);
    }

    for focus in [0, 6, 11] {
        for _ in 0..180 {
            screen.related.update(12, Some(focus), &crate::ui::card_row::RowStyle::HOME, 1.0 / 60.0);
            screen.cast.update(12, Some(focus), &crate::ui::card_row::RowStyle::CAST, 1.0 / 60.0);
        }
        for _ in 0..180 {
            screen.related.update(12, None, &crate::ui::card_row::RowStyle::HOME, 1.0 / 60.0);
            screen.cast.update(12, None, &crate::ui::card_row::RowStyle::CAST, 1.0 / 60.0);
        }
        scroll_to(&mut screen, 3);
        assert_strip_hit_geometry(&screen, &related_elems, &measure);
        scroll_to(&mut screen, 4);
        assert_strip_hit_geometry(&screen, &cast_elems, &measure);
    }
    clear();
}
