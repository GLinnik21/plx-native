//! A platform that composed no playback driver (W6c): the Detail controls that play are drawn
//! disabled and own no focus stop, so focus travels between the controls that can act, and a press
//! that reaches a disabled one anyway does nothing. The page never OPENS on a control that writes to
//! the account (the bookmark, the watched toggle): its default is the first stop that does not, or,
//! for a row of writes only, the first section below the row, and a restore that names a control
//! the viewer was on keeps it. With a driver composed every one of these is exactly what it was.
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
    // The controls that WRITE to the account: the bookmark and the watched toggle in both faces.
    for ctl in [hero::HeroCtl::Play, hero::HeroCtl::Restart, hero::HeroCtl::Trailer, hero::HeroCtl::Alt,
        hero::HeroCtl::GoToShow, hero::HeroCtl::Version, hero::HeroCtl::More] {
        assert!(!ctl.writes_account(), "{ctl:?} plays, navigates or opens a menu; it writes nothing");
    }
    for ctl in [hero::HeroCtl::Watchlist, hero::HeroCtl::MarkWatched, hero::HeroCtl::MarkUnwatched] {
        assert!(ctl.writes_account(), "{ctl:?} changes the household's record");
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
                // A column the row stops on is that control, any other (a control the row does not
                // stop on, or past the row) is none: a restore then takes the page's default.
                for col in 0..drawn + 2 {
                    let at = hero::stop_at(set, stops, col);
                    assert_eq!(at, (col < drawn && hero::focusable(all[col], stops)).then(|| all[col]),
                        "{set:?} {stops:?}: column {col}");
                }
                // The default stop is the first stop that does not write: Play with a driver, and
                // without one the first of Go to Show, More, Version, Also available the row has,
                // or none at all when every stop the row has writes.
                let default = hero::default_stop(set, stops);
                assert_eq!(default, visible[..n].iter().copied().find(|c| !c.writes_account()), "{set:?} {stops:?}");
                assert!(default.map_or(true, |c| hero::focusable(c, stops) && !c.writes_account()), "{set:?} {stops:?}");
                if !stops.no_playback {
                    assert_eq!(default, Some(hero::HeroCtl::Play), "{set:?} {stops:?}: with a driver Play stays the default");
                }
                if stops.no_playback && !(set.go_show || set.more || set.version || set.alt) {
                    assert_eq!(default, None, "{set:?}: a row of writes only has no default stop of its own");
                }
            }
            // Full-trailer mode is Play alone, whatever the platform composed.
            for no_playback in [false, true] {
                let stops = hero::Stops { full_trailer: true, no_playback };
                assert_eq!(hero::visible_ctls(set, stops).1, 1);
                assert_eq!(hero::first_stop(set, stops), hero::HeroCtl::Play);
                assert_eq!(hero::default_stop(set, stops), Some(hero::HeroCtl::Play));
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

/// The engine key of the first card of the Extras shelf: `resumable`'s item has one (its trailer),
/// so it is the first section below its row.
fn first_extra(screen: &DetailScreen) -> u32 {
    screen.engine_key(extras::elem(0).expect("an extras card")).expect("the card has a key")
}

/// The page opens on Play with a driver. With none its row is the bookmark and the watched toggle,
/// both of which WRITE to the account, so a reflex OK on a page that has just appeared must not land
/// on either: it opens on the first section below the row, seated by that group's own `seat`.
#[test]
fn focus_opens_on_the_first_control_that_can_act() {
    let sid = ServerId::UNSET;
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let screen = bare(&guard, sid, "movie");
        let first = if no_driver { first_extra(&screen) } else { hero::ELEM_PLAY };
        // A page minted fresh seats by the group's own policy...
        assert_eq!(enter(&screen, None), first, "no_driver={no_driver}");
        // ...and one restored onto Play (a remembered cursor, the old default) reconciles to the
        // same place, not to a pill that draws no focus.
        assert_eq!(enter(&screen, Some(key(hero::ELEM_PLAY))), first, "no_driver={no_driver}");
        // Restart is a stop only while a driver can play: without one a focus left on it falls back
        // to the same default.
        assert_eq!(enter(&screen, Some(key(hero::ELEM_RESTART))), if no_driver { first } else { hero::ELEM_RESTART },
            "no_driver={no_driver}");
        // ...but a restore onto the very control the viewer left keeps it, a write control included:
        // that is their own position.
        if no_driver {
            for own in [hero::ELEM_WATCHLIST, hero::ELEM_MARK_WATCHED] {
                assert_eq!(enter(&screen, Some(key(own))), own, "no_driver={no_driver}: the viewer's own focus stays");
            }
        }
        clear();
    }
}

/// A movie with nothing below its row but the About card: the default is that card, not a write
/// control and not nothing.
#[test]
fn a_movie_whose_only_stops_write_opens_on_the_first_section_below_the_row() {
    let sid = ServerId::UNSET;
    let guard = install(Detail {
        sid, rk: "movie".into(), kind: "movie".into(), part: "/library/parts/1/file.mkv".into(),
        guid: "plex://movie/5d776b1d96b655001f2a9c3e".into(), ..Default::default()
    });
    let screen = bare(&guard, sid, "movie");
    let meta = test_store().view();
    {
        let _driver = NoDriver::set(true);
        let set = screen.hero_set(meta);
        let stops = hero::Stops { full_trailer: false, no_playback: true };
        let (row, n) = hero::visible_ctls(set, stops);
        assert_eq!(&row[..n], &[hero::HeroCtl::Watchlist, hero::HeroCtl::MarkWatched], "premise: every stop of the row writes");
        assert_eq!(hero::default_stop(set, stops), None);
        let landed = enter(&screen, None);
        assert!(matches!(screen.locate(landed, meta), Some(Located::About(0))), "the first section below the row");
        assert!(ctl_of(landed).is_none(), "and not a control of the row");
    }
    // With a driver nothing changes: Play.
    let _driver = NoDriver::set(false);
    assert_eq!(enter(&screen, None), hero::ELEM_PLAY);
    clear();
}

/// A row that has a stop which neither plays nor writes opens on it (Go to Show, More, Play Version,
/// Also available), ahead of the writes that sit before it in the row; UP into the row from below
/// seats there as well.
#[test]
fn a_row_with_a_stop_that_does_not_write_opens_on_it() {
    let sid = ServerId::UNSET;
    let episode = Detail {
        sid, rk: "ep".into(), kind: "episode".into(), part: "/library/parts/1/file.mkv".into(),
        show_rk: "show".into(), season: 1, ..Default::default()
    };
    for (what, item, lands) in [
        ("a movie with two versions", versioned("movie", 2), hero::ELEM_VERSION),
        ("an episode", episode, hero::ELEM_GO_TO_SHOW),
    ] {
        let guard = install(item);
        let _driver = NoDriver::set(true);
        let screen = bare(&guard, sid, if what == "an episode" { "ep" } else { "leaf" });
        let set = screen.hero_set(test_store().view());
        let stops = hero::Stops { full_trailer: false, no_playback: true };
        let (row, n) = hero::visible_ctls(set, stops);
        assert!(row[..n].iter().any(|ctl| ctl.writes_account()), "{what}: premise: the row has a write control too");
        assert_eq!(enter(&screen, None), lands, "{what}");
        assert_eq!(enter(&screen, Some(key(hero::ELEM_PLAY))), lands, "{what}: a restore onto Play resolves the same way");
        // UP into the row from the section below seats there as well.
        let about = screen.engine_key(about::CARD_ELEM).expect("the About card has a key");
        let mut engine = FocusEngine::new();
        engine.set(OWNER, key(about), Some(about::ABOUT_GROUP), By::Restore);
        assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Up), "UP into the row"), lands, "{what}");
        clear();
    }
}

/// A page with no item yet offers Play and the watch disc. With no driver the watch disc is the
/// row's only stop and there is no section below to seat on, so the page opens on it: a control that
/// writes, but one that presses nothing until the item is there (`activate_hero`), and not one focus
/// stays on once it is (`a_page_seated_before_its_item_lands_leaves_the_write_control_when_it_does`).
#[test]
fn a_page_whose_item_has_not_landed_opens_on_the_watch_disc_with_no_driver() {
    let sid = ServerId::UNSET;
    for (no_driver, first) in [(false, hero::ELEM_PLAY), (true, hero::ELEM_MARK_WATCHED)] {
        let guard = install(Detail { sid, rk: "other".into(), ..Default::default() });
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        assert!(screen.detail(test_store().view()).is_none(), "the page's own item is not loaded");
        assert_eq!(enter(&screen, None), first, "no_driver={no_driver}");
        let (_, effects) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_MARK_WATCHED), Some(first));
        assert!(!effects.iter().any(|e| matches!(e.fx, Fx::App(AppFx::Store(StoreId::ViewState, _)))),
            "no_driver={no_driver}: OK on the watch disc before the item is there writes nothing");
        clear();
    }
}

/// **The real order: the page is seated BEFORE its item lands** (the item is requested on every fresh
/// open). With no driver the first seat can only be the watched toggle, the row's one stop until the
/// item is there, and it must not stay there once the toggle is live. Every navigation arrives with a
/// restore armed at the default spot (`DetailSeed`), and a fresh open that came with none arms one
/// itself, so the landing seats the page's default instead of leaving focus on a write control.
#[test]
fn a_page_seated_before_its_item_lands_leaves_the_write_control_when_it_does() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    // with a restore armed by the mount (a navigation's seed), and with none (a fresh open without one)
    for (no_driver, seeded) in [(true, true), (true, false), (false, true), (false, false)] {
        let guard = install(Detail { sid, rk: "other".into(), ..Default::default() });
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        if seeded {
            screen.restore(&Spot::default(), test_store().view());
        }
        step(&mut screen, &ScreenEvent::Enter(plx_ui::screen::Enter::Fresh {
            focus: FocusTarget::FirstInGroup(hero::HERO_GROUP),
        }), None);
        assert_eq!(screen.restore_intent.is_some(), seeded || no_driver,
            "no_driver={no_driver} seeded={seeded}: armed by the seed, or by Enter when no driver can play; with one, Enter arms nothing");
        let mut engine = FocusEngine::new();
        let first = if no_driver { hero::ELEM_MARK_WATCHED } else { hero::ELEM_PLAY };
        match engine.enter::<TestHost>(OWNER, &screen, FocusTarget::FirstInGroup(hero::HERO_GROUP), None, &cx(&measure, None)) {
            Outcome::Moved { to, .. } => assert_eq!(to.elem, first, "no_driver={no_driver}: before the item lands"),
            other => panic!("an entry moves focus onto something: {other:?}"),
        }
        // The item lands, and the engine's reconcile runs after the landing, as `Dispatcher` does.
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(resumable(sid)));
        step(&mut screen, &ScreenEvent::StoreChanged(StoreId::Metadata.ord(), 1), Some(first));
        let landed = Cx { focus: engine.read(OWNER), ..cx(&measure, None) };
        let after = match engine.reconcile::<TestHost>(OWNER, &screen, &landed) {
            Outcome::Moved { to, .. } => to.elem,
            Outcome::Nothing => first,
            other => panic!("{other:?}"),
        };
        let want = if no_driver { first_extra(&screen) } else { hero::ELEM_PLAY };
        assert_eq!(after, want, "no_driver={no_driver} seeded={seeded}: where focus stands once the item is there");
        clear();
    }
}

/// `Enter` arms the default restore only for the page it is about: a FRESH open, with no driver, whose
/// item is not there yet and that is not already returning. A page whose item is there is seated by
/// the default at once, and a Back return brings its own spot.
#[test]
fn enter_arms_the_default_restore_only_for_a_fresh_open_whose_item_is_not_there() {
    use plx_ui::screen::Enter;
    let sid = ServerId::UNSET;
    let fresh = Enter::Fresh { focus: FocusTarget::FirstInGroup(hero::HERO_GROUP) };
    let _driver = NoDriver::set(true);
    // (what, item landed, return pending, the enter, armed)
    for (what, landed, returning, enter, armed) in [
        ("a fresh open before its item lands", false, false, fresh, true),
        ("a fresh open whose item is already there", true, false, fresh, false),
        ("a page that is returning", false, true, fresh, false),
        ("a restored enter", false, false, Enter::Restored, false),
    ] {
        let guard = install(if landed { resumable(sid) } else { Detail { sid, rk: "other".into(), ..Default::default() } });
        let mut screen = bare(&guard, sid, "movie");
        screen.return_pending = returning;
        step(&mut screen, &ScreenEvent::Enter(enter), None);
        assert_eq!(screen.restore_intent.is_some(), armed, "{what}");
        clear();
    }
}

/// A restore the page already carries is never swapped for the default one: a spot a navigation
/// seeded (`DetailSeed`) is the viewer's own place, and `Enter` arms the default only where none
/// came.
#[test]
fn enter_keeps_the_restore_the_page_already_carries() {
    use plx_ui::screen::Enter;
    let sid = ServerId::UNSET;
    let _driver = NoDriver::set(true);
    let guard = install(Detail { sid, rk: "other".into(), ..Default::default() });
    let mut screen = bare(&guard, sid, "movie");
    screen.restore(&Spot { section: 3, col: 2, ..Default::default() }, test_store().view());
    let fresh = Enter::Fresh { focus: FocusTarget::FirstInGroup(hero::HERO_GROUP) };
    step(&mut screen, &ScreenEvent::Enter(fresh), None);
    let spot = &screen.restore_intent.as_ref().expect("the restore is still armed").spot;
    assert_eq!((spot.section, spot.col), (3, 2), "the spot the page came with is the one it keeps");
    clear();
}

/// Focus only ever lands on a control of the row that draws a focused face. The shared button draws
/// none while disabled (`widgets::Button::face_now`), which is why a disabled control owns no stop.
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
        // Every control the row owns a stop on draws a focused face: a focus that travels onto one
        // is never invisible. (With a driver the page opens on Play; without one it opens below the
        // row, and the row's own stops are reached by travel.)
        let (row, n) = hero::visible_ctls(set, hero::Stops { full_trailer: false, no_playback: no_driver });
        for &stop in &row[..n] {
            let index = hero::index_of(set, stop).expect("in the row");
            assert_ne!(drawn(&mut screen, Some(index)), at_rest, "no_driver={no_driver}: {stop:?} draws a focused face");
        }
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

/// Coming back UP into the row from the section below seats on a stop of the row, never on a
/// disabled Play, and on the first one even when it writes (here every stop does): the viewer chose
/// the row, and the write controls stay reachable by travel. The engine's `Seat::First` is
/// `DetailScreen::seat`.
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

/// A page that opened below its row (a movie with no driver) still reaches the row's write controls
/// by travel: UP seats on the first, RIGHT reaches the next, and DOWN leaves the row again. The page's
/// FIRST seat is the default below the row even when focus is still on another page's key, while an
/// UP from a focus already on this page is the viewer's own choice of the row.
#[test]
fn the_write_controls_stay_reachable_by_travel_from_a_page_that_opened_below_them() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    let guard = install(resumable(sid));
    let _driver = NoDriver::set(true);
    let screen = bare(&guard, sid, "movie");
    // The container's fresh enter, while the scope still reads the page that was left (another entry).
    let mut elsewhere = cx(&measure, None);
    elsewhere.focus.current = Some(FocusKey { entry: EntryId(9), elem: hero::ELEM_PLAY });
    let mut engine = FocusEngine::new();
    let landed = match engine.enter::<TestHost>(OWNER, &screen, FocusTarget::FirstInGroup(hero::HERO_GROUP), None, &elsewhere) {
        Outcome::Moved { to, .. } => to.elem,
        other => panic!("an entry moves focus onto something: {other:?}"),
    };
    assert_eq!(landed, first_extra(&screen), "another entry's focus is not this page's: it opens by default");
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Up), "UP into the row"), hero::ELEM_WATCHLIST);
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Right), "RIGHT along the row"), hero::ELEM_MARK_WATCHED);
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Down), "DOWN out of the row"), first_extra(&screen));
    clear();
}

/// A restored spot names a column of the drawn row. A column the row still stops on keeps that
/// control (the viewer's own position, a write control included); one that is no longer a stop
/// (Play and Restart with no driver) resolves by the page's default rule, not onto the nearest stop,
/// which would be a write: here, below the row.
#[test]
fn a_restored_hero_spot_keeps_the_control_it_names_and_otherwise_takes_the_default() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    // columns: 0 Play, 1 Watchlist, 2 Restart, 3 Mark watched; `None` is the default below the row
    for (no_driver, col, lands) in [
        (false, 0, Some(hero::ELEM_PLAY)), (false, 2, Some(hero::ELEM_RESTART)),
        (true, 0, None), (true, 1, Some(hero::ELEM_WATCHLIST)), (true, 2, None),
        (true, 3, Some(hero::ELEM_MARK_WATCHED)),
    ] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        screen.restore(&Spot { section: 0, col, ..Default::default() }, test_store().view());
        let got = Focusable::<TestHost>::reconcile(&screen, key(hero::ELEM_PLAY), &cx(&measure, None));
        assert_eq!(got.elem, lands.unwrap_or_else(|| first_extra(&screen)), "no_driver={no_driver}: column {col}");
        clear();
    }
}

/// A spot saved on a section the item no longer has (no related shelf here) falls back to the page's
/// default: Play when a driver can play, else below the row.
#[test]
fn a_restore_whose_section_is_gone_falls_back_to_the_default() {
    let sid = ServerId::UNSET;
    let measure = plx_ui::fixture::FixtureMeasure;
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        screen.restore(&Spot { section: 3, col: 2, ..Default::default() }, test_store().view());
        let got = Focusable::<TestHost>::reconcile(&screen, key(hero::ELEM_MARK_WATCHED), &cx(&measure, None));
        let first = if no_driver { first_extra(&screen) } else { hero::ELEM_PLAY };
        assert_eq!(got.elem, first, "no_driver={no_driver}");
        clear();
    }
}

/// A restore retires once focus sits on the place it resolves to: with no driver that is the default
/// below the row, not the Play the spot named, so the page does not keep re-seating a focus that
/// already landed.
#[test]
fn a_restore_retires_once_focus_is_on_the_place_the_page_resolves_it_to() {
    let sid = ServerId::UNSET;
    for no_driver in [false, true] {
        let guard = install(resumable(sid));
        let _driver = NoDriver::set(no_driver);
        let mut screen = bare(&guard, sid, "movie");
        // `step` takes the page's own (local) elem and keys it itself.
        let lands = if no_driver { extras::elem(0).expect("an extras card") } else { hero::ELEM_PLAY };
        screen.restore(&Spot { section: 0, col: 0, ..Default::default() }, test_store().view());
        assert!(screen.restore_intent.is_some(), "premise: the restore is armed");
        step(&mut screen, &ScreenEvent::Tick(plx_machine::machine::Tick { ms: 16, dt_us: 16_667 }), Some(lands));
        assert!(screen.restore_intent.is_none(), "no_driver={no_driver}: focus is on {lands}, the restore's target");
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
