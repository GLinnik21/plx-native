//! A platform that composed no playback driver (W6c): the Detail controls that play are drawn
//! disabled and own no focus stop, so focus opens on, travels between and falls back to the controls
//! that can act, and a press that reaches a disabled one anyway does nothing. With a driver composed
//! every one of these is exactly what it was.
//!
//! The shared rule these pin is `focus.rs`'s "a disabled item owns no focus stop"
//! (`next_enabled_index`; the player HUD's disabled discs): the shared button draws no focused face
//! while disabled, so a stop on one would leave a page that opens with nothing visibly focused.

use super::*;
use plx_machine::machine::{FocusTarget, InputOwner};
use plx_ui::focus::{FocusEngine, Outcome};

/// TEST ONLY: the host publishes a platform with no playback driver while this is alive.
struct NoDriver;
impl NoDriver {
    fn set(on: bool) -> Self {
        NO_DRIVER.with(|flag| flag.set(on));
        NoDriver
    }
}
impl Drop for NoDriver {
    fn drop(&mut self) {
        NO_DRIVER.with(|flag| flag.set(false));
    }
}

const OWNER: InputOwner = InputOwner::Entry(EntryId(7));

fn key(elem: u32) -> FocusKey<u32> {
    FocusKey { entry: EntryId(7), elem }
}

/// A movie with a resume point, a Watchlist guid and a trailer: its row is
/// `[Play, Watchlist, Restart, Mark watched]`, so Restart sits BETWEEN two controls that can act and
/// a skip over it is observable.
fn resumable(sid: ServerId) -> Detail {
    Detail {
        sid, rk: "movie".into(), kind: "movie".into(), part: "/library/parts/1/file.mkv".into(),
        guid: "plex://movie/5d776b1d96b655001f2a9c3e".into(), resume_ms: 30_000, dur_ms: 120_000,
        extras: vec![trailer_extra()], ..Default::default()
    }
}

/// The control the page's focus lands on when it is entered the way the container mints it
/// (`FirstInGroup(hero)`), or, given a `restored` key, reconciled from it.
fn enter(screen: &DetailScreen, restored: Option<FocusKey<u32>>) -> u32 {
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut engine = FocusEngine::new();
    match engine.enter::<TestHost>(OWNER, screen, FocusTarget::FirstInGroup(hero::HERO_GROUP), restored, &cx(&measure, None)) {
        Outcome::Moved { to, .. } => to.elem,
        other => panic!("an entry moves focus onto something: {other:?}"),
    }
}

/// Which hero control `elem` is, if it is one.
fn ctl_of(elem: u32) -> Option<hero::HeroCtl> {
    hero::HeroCtl::of_elem(elem)
}

#[test]
fn only_the_controls_that_play_lose_their_stop_and_the_row_is_never_empty() {
    for ctl in [hero::HeroCtl::Play, hero::HeroCtl::Restart, hero::HeroCtl::Trailer] {
        assert!(ctl.starts_playback(), "{ctl:?}");
    }
    for ctl in [hero::HeroCtl::Alt, hero::HeroCtl::MarkWatched, hero::HeroCtl::MarkUnwatched, hero::HeroCtl::Watchlist,
        hero::HeroCtl::GoToShow, hero::HeroCtl::Version, hero::HeroCtl::More] {
        assert!(!ctl.starts_playback(), "{ctl:?} opens, marks or navigates; it does not play");
    }
    for mark in [PosterMark::None, PosterMark::InProgress, PosterMark::Watched] {
        for bits in 0..64u32 {
            let set = hero::HeroSet {
                watchlist: bits & 1 != 0, go_show: bits & 2 != 0, version: bits & 4 != 0, restart: bits & 8 != 0,
                trailer: bits & 16 != 0, alt: bits & 32 != 0, more: bits & 3 == 0, mark,
            };
            let (all, drawn) = hero::hero_ctls(set);
            for stops in [hero::Stops::default(), hero::Stops { no_playback: true, ..Default::default() }] {
                let (visible, n) = hero::visible_ctls(set, stops);
                assert!(n >= 1, "{set:?} {stops:?}: the watched toggle plays nothing, so the row is never empty");
                assert!(visible[..n].iter().all(|c| hero::focusable(*c, stops)));
                assert_eq!(hero::first_stop(set, stops), visible[0], "{set:?} {stops:?}");
                let want: Vec<_> = all[..drawn].iter().copied().filter(|c| !(stops.no_playback && c.starts_playback())).collect();
                assert_eq!(&visible[..n], &want[..], "{set:?} {stops:?}: a subset of the drawn row, in drawn order");
                let (first, last) = hero::stop_span(set, stops).expect("a stop");
                assert_eq!((all[first], all[last]), (visible[0], visible[n - 1]), "{set:?} {stops:?}");
                // A column the row still stops on resolves to itself, any other to the nearest stop.
                for col in 0..drawn {
                    let near = hero::stop_near(set, stops, col).expect("a stop");
                    assert!(hero::focusable(near, stops));
                    if hero::focusable(all[col], stops) {
                        assert_eq!(near, all[col], "{set:?} {stops:?}: column {col} is itself a stop");
                    }
                }
            }
            // Full-trailer mode is Play alone, whatever the platform composed.
            for no_playback in [false, true] {
                let stops = hero::Stops { full_trailer: true, no_playback };
                assert_eq!(hero::visible_ctls(set, stops).1, 1);
                assert_eq!(hero::first_stop(set, stops), hero::HeroCtl::Play);
            }
        }
    }
}

/// With a driver the focus group is what it always was: every drawn control is a stop and the extent
/// runs from the margin to the end of the last one. With none it spans the stops only.
#[test]
fn the_hero_group_spans_the_controls_that_own_a_stop() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let screen = bare(&guard, sid, "movie");
        let meta = test_store().view();
        let set = screen.hero_set(meta);
        assert_eq!(hero::hero_ctls(set).1, 4, "Play, Watchlist, Restart, Mark watched");
        let mut groups = Vec::new();
        Focusable::<TestHost>::groups(&screen, &cx(&measure, None), &mut groups);
        let spec = groups.iter().find(|g| g.id == hero::HERO_GROUP).expect("the hero group");
        let widths = hero::hero_widths(&measure, set, set.restart, screen.disc_unfurl.map(|s| s.pos), screen.named_episode(meta));
        let y = screen.hero_chain(&measure, meta).btn_y;
        let rect = |i| hero::hero_btn_rect_at(set, i, y, widths);
        let (len, first) = if no_driver { (2, 1) } else { (4, 0) };
        let (head, tail) = (rect(first), rect(3));
        assert_eq!(spec.len, len, "no_driver={no_driver}");
        assert_eq!(spec.extent, Rect::new(head.x, y - screen.scroll_target, tail.x + tail.w - head.x, hero::CD), "no_driver={no_driver}");
        if !no_driver {
            // The row as it stood before a platform could compose nothing: from the margin.
            assert_eq!(spec.extent.x, plx_ui::consts::MARGIN_X);
        }
        let mut listed = Vec::new();
        Focusable::<TestHost>::a11y_elems(&screen, hero::HERO_GROUP, &cx(&measure, None), &mut listed);
        assert_eq!(listed.len(), len, "the ids the harness can name are the stops the group counts");
        clear();
    }
}

#[test]
fn focus_opens_on_the_first_control_that_can_act() {
    let sid = ServerId::UNSET;
    for (no_driver, first) in [(false, hero::ELEM_PLAY), (true, hero::ELEM_WATCHLIST)] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let screen = bare(&guard, sid, "movie");
        // A page minted fresh seats by the group's own policy...
        assert_eq!(enter(&screen, None), first, "no_driver={no_driver}");
        // ...and one restored onto Play (a remembered cursor, the old default) reconciles to the
        // same control, not to a pill that draws no focus.
        assert_eq!(enter(&screen, Some(key(hero::ELEM_PLAY))), first, "no_driver={no_driver}");
        clear();
    }
}

/// A page with no item yet offers Play and the watch disc: with no driver the watch disc is the
/// first stop there.
#[test]
fn a_page_whose_item_has_not_landed_opens_on_the_watch_disc_with_no_driver() {
    let sid = ServerId::UNSET;
    for (no_driver, first) in [(false, hero::ELEM_PLAY), (true, hero::ELEM_MARK_WATCHED)] {
        let guard = install(Detail { sid, rk: "other".into(), ..Default::default() });
        let _driver = NoDriver::set(no_driver);
        let screen = bare(&guard, sid, "movie");
        assert!(screen.detail(test_store().view()).is_none(), "the page's own item is not loaded");
        assert_eq!(enter(&screen, None), first, "no_driver={no_driver}");
        clear();
    }
}

/// The page opens on a control that draws a focused face. The shared button draws none while
/// disabled (`widgets::Button::face_now`), which is why a disabled control owns no stop.
#[test]
fn the_control_focus_lands_on_draws_a_focused_face_and_a_disabled_one_never_would() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    // What the row draws once control `focus`'s lift spring has settled (`ctl_pop` is what `Tick`
    // steps); `None` is the row at rest.
    let drawn = |screen: &mut DetailScreen, focus: Option<usize>| {
        for _ in 0..120 {
            screen.ctl_pop.step(focus, 0.05);
        }
        plx_ui::draw_census::capture_inks(|| screen.draw_buttons(plx_ui::Painter::recording(), &cx(&measure, None), 600.0, 1.0))
    };
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        let set = screen.hero_set(test_store().view());
        let at_rest = drawn(&mut screen, None);
        let landed = ctl_of(enter(&screen, None)).expect("a hero control");
        let index = hero::index_of(set, landed).expect("in the row");
        assert_ne!(drawn(&mut screen, Some(index)), at_rest,
            "no_driver={no_driver}: the control the page opens on ({landed:?}) draws a focused face");
        if no_driver {
            for ctl in [hero::HeroCtl::Play, hero::HeroCtl::Restart] {
                let index = hero::index_of(set, ctl).expect("in the row");
                assert_eq!(drawn(&mut screen, Some(index)), at_rest,
                    "{ctl:?} is drawn disabled: focused or not it draws the same, which is why it owns no stop");
            }
        }
        clear();
    }
}

#[test]
fn focus_travels_between_the_controls_that_can_act_and_never_onto_one_that_cannot() {
    let sid = ServerId::UNSET;
    // (driver composed, the row's stops in order)
    for (no_driver, stops) in [
        (false, vec![hero::ELEM_PLAY, hero::ELEM_WATCHLIST, hero::ELEM_RESTART, hero::ELEM_MARK_WATCHED]),
        (true, vec![hero::ELEM_WATCHLIST, hero::ELEM_MARK_WATCHED]),
    ] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let screen = bare(&guard, sid, "movie");
        let mut engine = FocusEngine::new();
        engine.set(OWNER, key(stops[0]), Some(hero::HERO_GROUP), By::Restore);
        for pair in stops.windows(2) {
            assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Right), "RIGHT along the row"), pair[1],
                "no_driver={no_driver}: RIGHT from {:?}", pair[0]);
        }
        assert!(matches!(collection_move(&screen, &mut engine, Dir::Right), Outcome::Nothing), "the row ends");
        for pair in stops.windows(2).rev() {
            assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Left), "LEFT along the row"), pair[0],
                "no_driver={no_driver}: LEFT from {:?}", pair[1]);
        }
        assert!(matches!(collection_move(&screen, &mut engine, Dir::Left), Outcome::Nothing),
            "no_driver={no_driver}: LEFT of the first stop is the row's edge, not a disabled pill");
        // Whatever key is pressed from whatever stop, focus never lands on a control that plays.
        for from in &stops {
            for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
                engine.set(OWNER, key(*from), Some(hero::HERO_GROUP), By::Restore);
                if let Outcome::Moved { to, .. } = collection_move(&screen, &mut engine, dir) {
                    assert!(!no_driver || !ctl_of(to.elem).is_some_and(hero::HeroCtl::starts_playback),
                        "no_driver={no_driver}: {dir:?} from {from} reached a control that plays");
                }
            }
        }
        clear();
    }
}

/// Coming back UP into the row from the section below seats on the row's first stop, never on a
/// disabled Play: the engine's `Seat::First` is `DetailScreen::seat`.
#[test]
fn up_into_the_row_seats_on_its_first_stop() {
    let sid = ServerId::UNSET;
    for (no_driver, first) in [(false, hero::ELEM_PLAY), (true, hero::ELEM_WATCHLIST)] {
        let mut d = resumable(sid);
        // No Extras shelf (the trailer) between the row and Related: UP from Related is the row.
        d.extras.clear();
        d.related = vec![Default::default(), Default::default()];
        let guard = install(d);
        let _driver = NoDriver::set(no_driver);
        let screen = bare(&guard, sid, "movie");
        let related = screen.engine_key(related::elem(0).unwrap()).unwrap();
        let mut engine = FocusEngine::new();
        engine.set(OWNER, key(related), Some(related::RELATED_GROUP), By::Restore);
        let up = moved_to(collection_move(&screen, &mut engine, Dir::Up), "UP out of the first section");
        assert_eq!(up, first, "no_driver={no_driver}");
        clear();
    }
}

/// A restored spot names a column of the drawn row; a column that is no longer a stop resolves onto
/// the nearest stop, the later of two equally near.
#[test]
fn a_restored_hero_spot_lands_on_a_control_the_row_still_stops_on() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    // columns: 0 Play, 1 Watchlist, 2 Restart, 3 Mark watched
    for (no_driver, col, lands) in [
        (false, 0, hero::ELEM_PLAY), (false, 2, hero::ELEM_RESTART),
        (true, 0, hero::ELEM_WATCHLIST), (true, 1, hero::ELEM_WATCHLIST), (true, 2, hero::ELEM_MARK_WATCHED),
        (true, 3, hero::ELEM_MARK_WATCHED),
    ] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        screen.restore(&Spot { section: 0, col, ..Default::default() }, test_store().view());
        let got = Focusable::<TestHost>::reconcile(&screen, key(hero::ELEM_PLAY), &cx(&measure, None));
        assert_eq!(got.elem, lands, "no_driver={no_driver}: column {col}");
        clear();
    }
}

/// The pointer reaches only what owns a stop: hovering a disabled pill must not park focus on it.
#[test]
fn the_pointer_registers_no_stop_on_a_control_that_cannot_act() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        let stops = drawn_stops(&mut screen, &cx(&measure, None));
        let has = |elem: u32| stops.iter().any(|stop| stop.key.elem == elem);
        assert!(has(hero::ELEM_WATCHLIST) && has(hero::ELEM_MARK_WATCHED), "no_driver={no_driver}");
        assert_eq!((has(hero::ELEM_PLAY), has(hero::ELEM_RESTART)), (!no_driver, !no_driver), "no_driver={no_driver}");
        clear();
    }
}

/// Play, Restart, Trailer and a Play Version pick start a playback with a driver and do nothing
/// without one; a control that does not play still acts.
#[test]
fn every_control_that_plays_is_inert_with_no_driver_and_unchanged_with_one() {
    let sid = ServerId::UNSET;
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        for (name, event, focus) in [
            ("Play", ScreenEvent::Activate(hero::ELEM_PLAY), hero::ELEM_PLAY),
            ("Restart", ScreenEvent::Activate(hero::ELEM_RESTART), hero::ELEM_RESTART),
            ("Trailer", ScreenEvent::Activate(hero::ELEM_TRAILER), hero::ELEM_TRAILER),
            ("Play Version", ScreenEvent::App(AppMsg::PlayVersion(plx_plex::plex::MediaId(7))), hero::ELEM_VERSION),
        ] {
            let (_, effects) = step(&mut screen, &event, Some(focus));
            assert_eq!(play_item(&effects).is_some(), !no_driver, "{name}, no_driver={no_driver}");
        }
        // The watched toggle plays nothing and still writes.
        let (_, effects) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_MARK_WATCHED), Some(hero::ELEM_MARK_WATCHED));
        assert!(effects.iter().any(|e| matches!(e.fx, Fx::App(AppFx::Store(StoreId::ViewState, _)))),
            "Mark watched still acts, no_driver={no_driver}");
        clear();
    }
}

/// A version pick plays THAT version with a driver (it is `Some(media)` on the intent, not the
/// default one), so the inertness above is the guard and not a pick that never played.
#[test]
fn a_version_pick_plays_that_version_when_a_driver_is_composed() {
    let sid = ServerId::UNSET;
    let guard = install(resumable(sid));
    let mut screen = bare(&guard, sid, "movie");
    let (_, effects) = step(&mut screen, &ScreenEvent::App(AppMsg::PlayVersion(plx_plex::plex::MediaId(7))), None);
    let (play, _) = play_item(&effects).expect("a version pick plays");
    assert!(matches!(play, PlayIntent::Item { version: Some(plx_plex::plex::MediaId(7)), .. }), "the pick names its version");
    clear();
}
