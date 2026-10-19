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
pub(super) struct Cell;
impl Cell {
    pub(super) fn default() -> Self {
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

pub(super) fn set(w: &mut World, title: bool, details: Details) {
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

// ── One rule for every item kind: a line the setting turns on is never drawn empty ──────────────

/// A Home hub tile of one kind, carrying only the facts a server sends for it.
struct Tile {
    name: &'static str,
    edit: fn(&mut plx_data::pms::PmsMovie),
    /// The title line (the name a reader knows the tile by).
    title: &'static str,
    /// The details line per picker, `[YearLength, Genre, Video, AgeRatingScore]`: the picked fact
    /// when the tile has it, else the tile's identity line.
    details: [&'static str; 4],
}

const PICKERS: [Details; 4] = [Details::YearLength, Details::Genre, Details::Video, Details::AgeRatingScore];

/// Season six of The Office as a Recently Added hub holds it: a season has no length, and this
/// server sent it no year, genre, rating or stream facts.
fn bare_season(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 2; m.title = "Season 6".into(); m.show_title = "The Office".into(); m.season_index = 6;
}
fn rich_season(m: &mut plx_data::pms::PmsMovie) {
    bare_season(m);
    m.year = 2009; m.genre = Some("Comedy".into()); m.rating = "TV-14".into(); m.audience_rating = Some(8.5);
    m.video_resolution = Some("1080".into()); m.hdr = true;
}
/// A season the server titled itself, numbered 0.
fn specials(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 2; m.title = "Specials".into(); m.show_title = "The Office".into(); m.season_index = 0;
}
fn bare_show(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 1; m.title = "Severance".into(); m.year = 2022;
}
fn bare_episode(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 3; m.title = "Pilot".into(); m.show_title = "Severance".into(); m.season_index = 1; m.ep_index = 8;
}
fn bare_movie(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 0; m.title = "Heat".into(); m.year = 1995;
}

const TILES: [Tile; 6] = [
    Tile { name: "a season with no facts", edit: bare_season, title: "The Office", details: ["Season 6"; 4] },
    Tile { name: "a season with every fact but a length", edit: rich_season, title: "The Office",
           details: ["2009", "Comedy", "1080p \u{b7} HDR", "TV-14 \u{b7} 8.5"] },
    Tile { name: "a season the server titled itself", edit: specials, title: "The Office", details: ["Specials"; 4] },
    Tile { name: "a show with a year only", edit: bare_show, title: "Severance", details: ["2022"; 4] },
    Tile { name: "an episode with no facts", edit: bare_episode, title: "Pilot", details: ["S1 \u{2022} E8"; 4] },
    Tile { name: "a movie with a year only", edit: bare_movie, title: "Heat", details: ["1995"; 4] },
];

/// Every string a tile above can put on a line, so a run is read whichever line it is on.
const WORDS: [&str; 17] = [
    "The Office", "Season 6", "Specials", "Severance", "Pilot", "Heat", "2009", "2022", "1995", "Comedy",
    "1080p \u{b7} HDR", "TV-14 \u{b7} 8.5", "S1 \u{2022} E8", "Season 0", "Show", "Drama", "PG-13",
];

/// The caption lines the focused poster drew, top to bottom. Each line is drawn twice, the second
/// run its 1 px shadow (`CAPTION_SHADOW_DY`), which reads as the same line.
fn caption(w: &mut World) -> Vec<String> {
    let mut runs: Vec<_> = w.drawn().words.into_iter().filter(|(words, _)| WORDS.contains(&words.as_str())).collect();
    runs.sort_by(|a, b| a.1.y.total_cmp(&b.1.y));
    runs.dedup_by(|b, a| a.0 == b.0 && (b.1.y - a.1.y).abs() <= plx_ui::caption_policy::CAPTION_SHADOW_DY + 0.5);
    runs.into_iter().map(|(words, _)| words).collect()
}

/// Home with the Continue Watching deck as row 0 and a Recently Added shelf as row 1, focused on the
/// shelf's first poster (`r1`), which `edit` has made the kind of tile the test wants: a poster that
/// carries no state, as the deck's episodes do.
fn home_tile(name: &str, edit: fn(&mut plx_data::pms::PmsMovie)) -> World {
    let mut w = World::home_with_disc(name, [7; 12]);
    w.bridge.stores.hubs.seed_deck_home_for_test(1);
    w.frames(30);
    plx_data::pms::edit_test_item(w.bridge.stores.hubs.state_mut(), "r1", &edit);
    w.frames(30);
    w.press(Key::Down);
    w.press(Key::Down);
    w.frames(60);
    w
}

/// Home's focused tile, whatever its kind, says what the setting turns on: Title names the tile by
/// the name a reader knows it by (a season by its SHOW), and the picked details line is the picked
/// facts, or the tile's identity when it has none of them, never an open band over nothing.
#[test]
fn a_poster_shelf_tile_of_any_kind_never_leaves_a_turned_on_line_empty() {
    for tile in &TILES {
        let mut w = home_tile(&format!("cap-kinds-{}", tile.name.replace(' ', "-")), tile.edit);
        let _cell = Cell::default();
        for (picker, details) in PICKERS.into_iter().zip(tile.details) {
            for title in [true, false] {
                set(&mut w, title, picker);
                let want: Vec<&str> = title.then_some(tile.title).into_iter().chain([details]).collect();
                assert_eq!(caption(&mut w), want, "{}: title {title}, details {picker:?}", tile.name);
            }
        }
        set(&mut w, true, Details::None);
        assert_eq!(caption(&mut w), [tile.title], "{}: Title On with Nothing shows the name alone", tile.name);
        set(&mut w, false, Details::None);
        assert_eq!(caption(&mut w), Vec::<String>::new(), "{}: Title Off with Nothing shows nothing", tile.name);
    }
}
