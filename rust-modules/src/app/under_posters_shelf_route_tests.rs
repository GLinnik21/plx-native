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
        m.video_resolution = Some("1080".into());
        m.hdr = true;
        m.dur_ns = 100 * 60 * 1_000_000_000;
        m.resume_ms = if resume { 20 * 60 * 1000 } else { 0 };
    }
}

const YEAR_LENGTH: &str = "1999 \u{b7} 1h 40m";
const GENRE: &str = "Drama";
const AGE_SCORE: &str = "PG-13 \u{b7} 7.8";
const VIDEO: &str = "1080p \u{b7} HDR";
/// 80 of 100 minutes left, as the deck words it.
fn left() -> String { plx_ui::fmt::time_left(80 * 60 * 1000) }

pub(super) fn set(w: &mut World, title: bool, details: Details) {
    plx_media::route::restore_under_posters(UnderPosters { title, details });
    w.frames(60);
}

/// The caption table both pages must follow, for a tile with no state: (title on, details, the
/// second line it must draw).
const TABLE: [(bool, Details, Option<&str>); 8] = [
    (false, Details::YearLength, Some(YEAR_LENGTH)),
    (false, Details::Genre, Some(GENRE)),
    (true, Details::Genre, Some(GENRE)),
    (true, Details::Video, Some(VIDEO)),
    (false, Details::Video, Some(VIDEO)),
    (true, Details::AgeRatingScore, Some(AGE_SCORE)),
    (true, Details::None, None),
    (false, Details::None, None),
];

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
    for (title, details, line) in TABLE {
        set(&mut w, title, details);
        let want: Vec<&str> = title.then_some("Heat").into_iter().chain(line).collect();
        assert_eq!(w.drawn().caption_lines(), want, "the caption is exactly the setting's lines on Home (title {title}, details {details:?})");
    }
}

#[test]
fn a_continue_watching_tile_on_home_keeps_its_time_left_line() {
    let mut w = home(true, "cap-home-deck");
    let _cell = Cell::default();
    for (title, details, _) in TABLE {
        set(&mut w, title, details);
        // The deck's details line is the time left, not the picked fact, unless the setting shows none.
        let left = left();
        let want: Vec<&str> = title.then_some("Heat").into_iter().chain((details != Details::None).then_some(left.as_str())).collect();
        assert_eq!(w.drawn().caption_lines(), want, "title {title}, details {details:?}");
    }
}

/// **The amber play mark leads the FIRST line drawn whenever OK plays the deck card**, read off the
/// frame the page draws (`Drawn::marks`, since a host paints no icon): the title when the Title switch
/// is on, else the time-left line; Title Off with Details Nothing draws no line and so no mark; and a
/// deck whose OK opens the page (the default) draws none under any setting.
#[test]
fn the_play_mark_leads_the_first_line_drawn_whenever_ok_plays_the_deck_card() {
    use plx_media::route::{deck_press, restore_deck_press, DeckPress};
    struct Back(DeckPress);
    impl Drop for Back {
        fn drop(&mut self) { restore_deck_press(self.0); }
    }
    let mut w = home(true, "cap-home-mark");
    let _cell = Cell::default();
    let _back = Back(deck_press());
    for press in [DeckPress::Play, DeckPress::Details] {
        restore_deck_press(press);
        for (title, details, _) in TABLE {
            set(&mut w, title, details);
            let drawn = w.drawn();
            let lines = drawn.caption_lines();
            let setting = format!("{press:?}, title {title}, details {details:?}");
            let marked = press == DeckPress::Play && !lines.is_empty();
            assert_eq!(drawn.marks.len(), usize::from(marked), "{setting}: the mark is drawn exactly when OK plays and a line is: {lines:?}");
            let Some(mark) = drawn.marks.first() else { continue };
            // the first line drawn is the block's topmost run; the mark hangs left of it, on its line
            let first = drawn.runs.iter().filter(|run| run.label)
                .min_by(|a, b| a.at.y.total_cmp(&b.at.y)).expect("a block with a mark has a line");
            assert_eq!(first.text, lines[0], "{setting}");
            assert!(mark.x + mark.w <= first.at.x + 1.0, "{setting}: the mark {mark:?} hangs left of {:?} {:?}", first.text, first.at);
            let line_mid = first.at.y + first.at.h.max(first.size as f32) * 0.5;
            assert!((mark.cy() - line_mid).abs() < first.size as f32, "{setting}: the mark {mark:?} stands on its line (y {line_mid})");
        }
    }
}

/// **The focused title is drawn in ONE ink on every poster surface** (CONFIRM-4: the Watchlist and
/// Collections page titles read a step dimmer than Home's): the ink of the title line's run, the
/// cascade's alpha folded in, is the same on Home, the Collections page and the Watchlist, so a dimmer page is the ground behind the title and not the title.
#[test]
fn the_focused_title_is_one_ink_on_every_poster_surface() {
    let title_ink = |w: &mut World, surface: &str| {
        super::under_posters_band_route_tests::focus_first_poster(w, surface);
        let drawn = w.drawn();
        let first = drawn.runs.iter().filter(|run| run.label)
            .min_by(|a, b| a.at.y.total_cmp(&b.at.y)).expect("a focused poster draws its title").text.clone();
        // the line's own run, not its 1 px shadow beneath it
        let shadow = plx_ui::caption_policy::CAPTION_SHADOW;
        let ink = drawn.runs.iter().filter(|run| run.label && run.text == first && run.ink[..3] != shadow[..3])
            .map(|run| run.ink).next().expect("the title line draws its ink");
        (first, ink)
    };
    // each world holds the process-global lock the setting needs, so the setting is restored before it drops
    let on_home = { let mut w = home(false, "ink-home"); let _cell = Cell::default(); title_ink(&mut w, "Home").1 };
    let on_collections = { let mut w = World::collections("ink-collections", false); let _cell = Cell::default(); title_ink(&mut w, "Collections").1 };
    let on_watchlist = { let mut w = World::watchlist("ink-watchlist"); let _cell = Cell::default(); title_ink(&mut w, "Watchlist").1 };
    // (the cascade folds alpha in float: one 8-bit step is 0.004)
    let same = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1.0 / 255.0);
    assert!(same(on_collections, on_home), "the Collections page's title ink {on_collections:?} is Home's {on_home:?}");
    assert!(same(on_watchlist, on_home), "the Watchlist's title ink {on_watchlist:?} is Home's {on_home:?}");
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
    for (title, details, line) in TABLE {
        set(&mut w, title, details);
        let want: Vec<&str> = title.then_some("Recently Added 0").into_iter().chain(line).collect();
        assert_eq!(w.drawn().caption_lines(), want, "the caption is exactly the setting's lines on Recommended (title {title}, details {details:?})");
    }
}

#[test]
fn a_continue_watching_tile_on_recommended_keeps_its_time_left_line() {
    let mut w = recommended(true, "cap-rec-deck", true);
    let _cell = Cell::default();
    for (title, details, _) in TABLE {
        set(&mut w, title, details);
        let left = left();
        let want: Vec<&str> = title.then_some("Continue Watching 0").into_iter().chain((details != Details::None).then_some(left.as_str())).collect();
        assert_eq!(w.drawn().caption_lines(), want, "title {title}, details {details:?}");
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
/// An episode the server numbered neither by season nor by episode (a date-numbered series, an
/// item mid-scan): its address is empty, so the line says what it is.
fn unnumbered_episode(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 3; m.title = "Pilot".into(); m.show_title = "Severance".into(); m.season_index = 0; m.ep_index = 0;
}
/// A season the server gave neither a title nor a number: the specials folder.
fn untitled_specials(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 2; m.title = String::new(); m.show_title = "The Office".into(); m.season_index = 0;
}
fn bare_movie(m: &mut plx_data::pms::PmsMovie) {
    m.kind = 0; m.title = "Heat".into(); m.year = 1995;
}

const TILES: [Tile; 8] = [
    Tile { name: "a season with no facts", edit: bare_season, title: "The Office", details: ["Season 6"; 4] },
    Tile { name: "a season with every fact but a length", edit: rich_season, title: "The Office",
           details: ["Season 6", "Comedy", "1080p \u{b7} HDR", "TV-14 \u{b7} 8.5"] },
    Tile { name: "a season the server titled itself", edit: specials, title: "The Office", details: ["Specials"; 4] },
    Tile { name: "a show with a year only", edit: bare_show, title: "Severance", details: ["2022"; 4] },
    Tile { name: "an episode with no facts", edit: bare_episode, title: "Pilot", details: ["S1 \u{b7} E8"; 4] },
    Tile { name: "an unnumbered episode", edit: unnumbered_episode, title: "Pilot", details: ["Episode"; 4] },
    Tile { name: "a season with no title or number", edit: untitled_specials, title: "The Office", details: ["Specials"; 4] },
    Tile { name: "a movie with a year only", edit: bare_movie, title: "Heat", details: ["1995"; 4] },
];

/// The caption lines the focused poster drew, top to bottom (`Drawn::caption_lines`: the block
/// itself, not a search for words), so a line nothing asked for fails the comparison.
fn caption(w: &mut World) -> Vec<String> { w.drawn().caption_lines() }

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

// ── An item the server left untitled is still a card ────────────────────────────────────────────

/// Home as one server's own `/hubs` answer builds it: the hubs go through the refresh's projection
/// (`seed_hubs_response_for_test`), so a row the projection drops is not on the page, which the
/// hand-built `edit_test_item` tiles above can never show.
fn home_from_hubs(name: &str, hubs: &str) -> World {
    let mut w = World::home_with_disc(name, [7; 12]);
    let body = format!(r#"{{"MediaContainer":{{"Hub":{hubs}}}}}"#);
    let mc = serde_json::from_str::<plx_plex::plex::Envelope>(&body).expect("a /hubs answer").media_container;
    w.bridge.stores.hubs.seed_hubs_response_for_test(&mc);
    w.frames(60);
    w
}

/// **"Recently Added Seasons" draws a season the server gave no title** (the mock's
/// `--caption-edge-cases` season 0). The projection used to drop any row with an empty `title`, so
/// the season was not on Home at all and came back the moment it was titled; now it is the shelf's
/// second poster and its caption says "Specials" itself, and the Title switch names its show.
#[test]
fn an_untitled_season_is_drawn_in_homes_recently_added_seasons() {
    let hubs = r#"[
      {"type":"movie","hubIdentifier":"home.movies.recent","title":"Recently Added","key":"/hubs/home/recentlyAdded",
       "Metadata":[{"ratingKey":"1","type":"movie","title":"Heat","year":1995,"thumb":"/t/1","art":"/a/1"}]},
      {"type":"season","hubIdentifier":"mock.seasons","title":"Recently Added Seasons","key":"/hubs/mock/seasons",
       "Metadata":[{"ratingKey":"20","type":"season","title":"Season 1","index":1,"parentTitle":"The Office","thumb":"/t/20"},
                   {"ratingKey":"21","type":"season","title":"","index":0,"parentTitle":"The Office","thumb":"/t/21"}]}]"#;
    let mut w = home_from_hubs("untitled-season-home", hubs);
    let _cell = Cell::default();
    w.press(Key::Down);
    w.press(Key::Down);
    w.frames(60);
    assert_eq!(caption(&mut w), ["Season 1"], "premise: the focus is on the Seasons shelf's first poster");
    w.press(Key::Right);
    w.frames(60);
    assert_eq!(caption(&mut w), ["Specials"], "the untitled season is the shelf's second poster, worded by its identity");
    set(&mut w, true, Details::Genre);
    assert_eq!(caption(&mut w), ["The Office", "Specials"], "Title On names its show over its identity line");
}
