//! **R13 at the app tier, past the Library: the Under posters setting reaches Home's shelves and
//! the Recommended page's poster shelves** (`routes.md`), on a real `Dispatcher<AppHost>`.
//!
//! The setting is published the way the write door ends (`restore_under_posters`, the cell's own
//! door: the storage worker's publish is refused a process global by the test lock), and the grade
//! is what the product DRAWS for the focused poster (`World::drawn`, text read off the recording
//! painter): the title line follows the Title switch, the second line the Details picker, and a
//! Continue Watching tile keeps its time-left line under every details kind but Nothing.

use super::*;
use super::test_support::World;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails as Details};

/// Holds the live setting on its default for the test and puts it back after it.
struct Cell;
impl Cell {
    fn default() -> Self {
        plx_media::route::restore_under_posters(UnderPosters::DEFAULT);
        Cell
    }
}
impl Drop for Cell {
    fn drop(&mut self) { plx_media::route::restore_under_posters(UnderPosters::DEFAULT); }
}

/// The facts a focused poster is worded from; `resume` puts it 20 of its 100 minutes in.
fn facts(resume: bool) -> impl Fn(&mut plx_data::pms::PmsMovie) {
    move |m| {
        m.year = 1999;
        m.genre = Some("Drama".into());
        m.rating = "PG-13".into();
        m.audience_rating = Some(7.8);
        m.dur_ns = 100 * 60 * 1_000_000_000;
        m.resume_ms = if resume { 20 * 60 * 1000 } else { 0 };
    }
}

const YEAR_LENGTH: &str = "1999 \u{b7} 1h 40m";
const GENRE: &str = "Drama";
const AGE_SCORE: &str = "PG-13 \u{b7} 7.8";
/// 80 of 100 minutes left, as the deck words it.
fn left() -> String { plx_ui::fmt::time_left(80 * 60 * 1000) }

fn set(w: &mut World, title: bool, details: Details) {
    plx_media::route::restore_under_posters(UnderPosters { title, details });
    w.frames(60);
}

/// The caption table both pages must follow, for a tile with no state: (title on, details, the
/// second line it must draw).
const TABLE: [(bool, Details, Option<&str>); 6] = [
    (false, Details::YearLength, Some(YEAR_LENGTH)),
    (false, Details::Genre, Some(GENRE)),
    (true, Details::Genre, Some(GENRE)),
    (true, Details::AgeRatingScore, Some(AGE_SCORE)),
    (true, Details::None, None),
    (false, Details::None, None),
];

/// What the focused poster drew, as `(title line, details line)` words out of `candidates`.
fn drew(w: &mut World, title: &str, details: &[&str]) -> (bool, Vec<String>) {
    let drawn = w.drawn();
    (drawn.says(title), details.iter().filter(|d| drawn.says(d)).map(|d| (*d).to_owned()).collect())
}

fn home(resume: bool, name: &str) -> World {
    let mut w = World::home_with_disc(name, [7; 12]);
    plx_data::pms::edit_test_item(w.bridge.stores.hubs.state_mut(), "1", &|m| { facts(resume)(m); m.title = "Heat".into(); });
    w.frames(30);
    w.press(Key::Down);
    w.frames(60);
    w
}

#[test]
fn home_shelves_caption_the_focused_poster_per_the_stored_setting() {
    let mut w = home(false, "cap-home");
    let _cell = Cell::default();
    let every = [YEAR_LENGTH, GENRE, AGE_SCORE];
    for (title, details, line) in TABLE {
        set(&mut w, title, details);
        let (title_drawn, lines) = drew(&mut w, "Heat", &every);
        let setting = format!("title {title}, details {details:?}");
        assert_eq!(title_drawn, title, "the title line follows the Title switch on Home ({setting})");
        assert_eq!(lines, line.map(str::to_owned).into_iter().collect::<Vec<_>>(), "the details line is the picked one ({setting})");
    }
}

#[test]
fn a_continue_watching_tile_on_home_keeps_its_time_left_line() {
    let mut w = home(true, "cap-home-deck");
    let _cell = Cell::default();
    let every = [YEAR_LENGTH, GENRE, AGE_SCORE];
    for (title, details, _) in TABLE {
        set(&mut w, title, details);
        let (title_drawn, lines) = drew(&mut w, "Heat", &every);
        let drawn_left = w.drawn().says(&left());
        let setting = format!("title {title}, details {details:?}");
        assert_eq!(title_drawn, title, "the Title switch still decides the title line ({setting})");
        assert!(lines.is_empty(), "the deck's details line is not the picked fact ({setting}): {lines:?}");
        assert_eq!(drawn_left, details != Details::None, "time left is its details line unless the setting shows none ({setting})");
    }
}

/// A Recommended page whose shelves are posters, the page arriving on the first poster of the first
/// shelf: the Continue Watching deck first or second; every card carries the facts of [`facts`].
fn recommended(resume_on_deck: bool, name: &str, deck_first: bool) -> World {
    let mut w = World::recommended_pending(name);
    {
        let mut browse = w.bridge.stores.browse.borrow_mut();
        let deck = ("hub.movie.inprogress.1", "", "Continue Watching");
        let recent = ("hub.movie.recent", "", "Recently Added");
        browse.seed_named_shelves_for_test(0, &if deck_first { [deck, recent] } else { [recent, deck] }, 3);
        browse.edit_shelf_items_for_test(0, &|m| facts(resume_on_deck && m.rk.contains("inprogress"))(m));
    }
    w.frames(60);
    w
}

#[test]
fn recommended_shelves_caption_the_focused_poster_per_the_stored_setting() {
    let mut w = recommended(false, "cap-rec", false);
    let _cell = Cell::default();
    let every = [YEAR_LENGTH, GENRE, AGE_SCORE];
    for (title, details, line) in TABLE {
        set(&mut w, title, details);
        let (title_drawn, lines) = drew(&mut w, "Recently Added 0", &every);
        let setting = format!("title {title}, details {details:?}");
        assert_eq!(title_drawn, title, "the title line follows the Title switch on Recommended ({setting})");
        assert_eq!(lines, line.map(str::to_owned).into_iter().collect::<Vec<_>>(), "the details line is the picked one ({setting})");
    }
}

#[test]
fn a_continue_watching_tile_on_recommended_keeps_its_time_left_line() {
    let mut w = recommended(true, "cap-rec-deck", true);
    let _cell = Cell::default();
    let every = [YEAR_LENGTH, GENRE, AGE_SCORE];
    for (title, details, _) in TABLE {
        set(&mut w, title, details);
        let (title_drawn, lines) = drew(&mut w, "Continue Watching 0", &every);
        let drawn_left = w.drawn().says(&left());
        let setting = format!("title {title}, details {details:?}");
        assert_eq!(title_drawn, title, "the Title switch still decides the title line ({setting})");
        assert!(lines.is_empty(), "the deck's details line is not the picked fact ({setting}): {lines:?}");
        assert_eq!(drawn_left, details != Details::None, "time left is its details line unless the setting shows none ({setting})");
    }
}
