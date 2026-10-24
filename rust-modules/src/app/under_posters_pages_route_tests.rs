//! **R13 on EVERY poster tile: Search, the person page's filmography, Detail's Related and
//! collection shelves and the Collection screen's members follow the Under posters setting** the
//! way Home, the Library and Recommended do (`under_posters_shelf_route_tests.rs`), on a real
//! `Dispatcher<AppHost>`.
//!
//! The setting is the stored one (`restore_under_posters`), and the grade is what the product
//! DRAWS for the focused poster (`World::drawn`, text read off the recording painter): the title
//! line follows the Title switch and the second line the Details picker, a line the setting turns on
//! is never drawn empty (a movie the server sent no year or genre for says what it is), and a tile
//! that carries a state of its own (a filmography poster's character) keeps it as its details line.
//! Landscape tiles (Search's episode stills) and headshots are not posters and keep their own block.

use super::*;
use super::test_support::World;
use super::under_posters_shelf_route_tests::{set, Cell};
use plx_data::pms::PmsMovie;
use plx_data::search::{CollectionHit, Item, Kind, Shelf};
use plx_plex::plex::session::UnderPostersDetails as Details;

/// A poster tile as a server describes a film it knows everything about.
fn rich(sid: plx_plex::plex::ServerId, rk: &str, title: &str) -> PmsMovie {
    PmsMovie { sid, rk: rk.into(), title: title.into(), kind: 0, year: 1999, genre: Some("Drama".into()),
        video_resolution: Some("1080".into()), rating: "PG-13".into(), audience_rating: Some(7.8),
        dur_ns: 100 * 60 * 1_000_000_000, ..Default::default() }
}

/// …and as one it knows only the name of: no year, no genre, no length, no stream.
fn bare(sid: plx_plex::plex::ServerId, rk: &str, title: &str) -> PmsMovie {
    PmsMovie { sid, rk: rk.into(), title: title.into(), kind: 0, ..Default::default() }
}

/// One poster of a page and the words the setting owes it: its title line, and its details line
/// under each picker (the tile's identity when it has no such fact).
struct Tile {
    title: &'static str,
    /// The key that brings focus to it from the tile before it (the first is reached with Down).
    key: Key,
    genre: String,
    year_length: String,
    video: String,
    age: String,
}

fn heat(key: Key) -> Tile {
    Tile { title: "Heat", key, genre: "Drama".into(), year_length: "1999 \u{b7} 1h 40m".into(),
        video: "1080p".into(), age: "PG-13 \u{b7} 7.8".into() }
}
/// A film with no year, genre or length: Genre has nothing to say, so the line says what it is.
fn nameless_facts(key: Key) -> Tile {
    Tile { title: "Bare", key, genre: "Movie".into(), year_length: "Movie".into(), video: "Movie".into(), age: "Movie".into() }
}

/// The caption lines the focused poster drew, top to bottom (`Drawn::caption_lines`: the block
/// itself, not a search for words), so a line nothing asked for fails the comparison.
fn caption(w: &mut World) -> Vec<String> { w.drawn().caption_lines() }

/// Bring focus to the poster named `title`: `key` until some poster is focused, then along its row
/// (a row is entered at the tile nearest where focus came from, not at its first), then `key` again
/// for the next row. The title is on while walking, so the focused poster says its name.
fn reach(w: &mut World, title: &str, key: Key) {
    set(w, true, Details::YearLength);
    let on = |w: &mut World| caption(w).iter().any(|line| line == title);
    let mut seen = Vec::new();
    for _ in 0..6 {
        for sweep in [Key::Left, Key::Right] {
            for _ in 0..4 {
                if on(w) { return; }
                seen.push(caption(w));
                w.press(sweep);
            }
        }
        w.press(key);
    }
    panic!("focus never reached {title}: it drew {seen:?}");
}

/// The grade, on every tile of a page: under Title On + Genre the name over the genre, under Title
/// Off + Year · Length the year and length alone, under Nothing no line (or the name alone with the
/// Title switch on). `state` is a tile's own state line, which stands in for the details line under
/// every setting but Nothing.
fn grade(w: &mut World, tiles: &[Tile], state: &dyn Fn(&Tile) -> Option<&'static str>) {
    for tile in tiles {
        reach(w, tile.title, tile.key);
        let own = state(tile);
        let details = |picked: &str| own.map_or_else(|| picked.to_owned(), str::to_owned);
        let table: [(bool, Details, Vec<String>); 6] = [
            (true, Details::Genre, vec![tile.title.into(), details(&tile.genre)]),
            (false, Details::YearLength, vec![details(&tile.year_length)]),
            (true, Details::Video, vec![tile.title.into(), details(&tile.video)]),
            (false, Details::AgeRatingScore, vec![details(&tile.age)]),
            (true, Details::None, vec![tile.title.into()]),
            (false, Details::None, vec![]),
        ];
        for (title, picker, want) in table {
            set(w, title, picker);
            assert_eq!(caption(w), want, "{}: title {title}, details {picker:?}", tile.title);
        }
    }
}

fn no_state(_: &Tile) -> Option<&'static str> { None }

/// Search's poster rows: a movie, a film with nothing to say, a show and a collection. The Episode
/// row is a landscape still and keeps its own block.
#[test]
fn search_results_caption_the_focused_poster_per_the_stored_setting() {
    let mut w = World::search_results("pages-search", |sid| {
        let shelf = |kind, items| Shelf { epoch: 0, window: Default::default(), kind, items };
        let show = PmsMovie { kind: 1, year: 2022, ..bare(sid, "s1", "Severance") };
        let collection = Item::Collection(CollectionHit {
            // with a thumb, so its face is a poster still loading and not the neutral tile that
            // prints the collection's name on itself
            item: PmsMovie { kind: plx_data::pms::KIND_COLLECTION, child_count: 7, subtype: Some("movie".into()),
                thumb: "/library/collections/c1/thumb/1".into(), ..bare(sid, "c1", "Noir") },
            tag: 1,
        });
        vec![
            shelf(Kind::Movie, vec![Item::Media(rich(sid, "m1", "Heat")), Item::Media(bare(sid, "m2", "Bare"))]),
            shelf(Kind::Show, vec![Item::Media(show)]),
            shelf(Kind::Collection, vec![collection]),
        ]
    });
    let tiles = [
        heat(Key::Down),
        nameless_facts(Key::Right),
        Tile { title: "Severance", key: Key::Down, genre: "2022".into(), year_length: "2022".into(),
            video: "2022".into(), age: "2022".into() },
        Tile { title: "Noir", key: Key::Down, genre: "7 films".into(), year_length: "7 films".into(),
            video: "7 films".into(), age: "7 films".into() },
    ];
    let _cell = Cell::default();
    grade(&mut w, &tiles, &no_state);
}

/// The person page's filmography: the character a credit plays is the tile's own state, so it is the
/// details line under every setting but Nothing, as a Continue Watching tile's time left is; a
/// credit with no character (every crew credit) reads the picked fact.
#[test]
fn a_filmography_poster_keeps_its_role_as_its_details_line() {
    let sid = plx_plex::plex::ServerId::UNSET;
    let mut w = World::person_page("pages-person", vec![rich(sid, "m1", "Heat"), bare(sid, "m2", "Bare")], Vec::new(), &["Cop", ""]);
    let tiles = [heat(Key::Down), nameless_facts(Key::Right)];
    let _cell = Cell::default();
    grade(&mut w, &tiles, &|tile| (tile.title == "Heat").then_some("Cop"));
}

/// Detail's Related shelf is a poster shelf.
#[test]
fn detail_related_posters_caption_the_focused_poster_per_the_stored_setting() {
    let sid = plx_plex::plex::ServerId::UNSET;
    let detail = plx_data::metadata::Detail {
        sid, rk: "movie".into(), kind: "movie".into(),
        related: vec![PmsMovie { sec: 1, ..rich(sid, "r1", "Heat") }, PmsMovie { sec: 1, ..bare(sid, "r2", "Bare") }],
        ..Default::default()
    };
    let mut w = World::detail_page("pages-detail", detail);
    let tiles = [heat(Key::Down), nameless_facts(Key::Right)];
    let _cell = Cell::default();
    grade(&mut w, &tiles, &no_state);
}

/// The Collection screen's members are posters.
#[test]
fn collection_members_caption_the_focused_poster_per_the_stored_setting() {
    let mut w = super::arrival_seat_tests::collection_screen_of("pages-collection", |sid| {
        vec![rich(sid, "m1", "Heat"), bare(sid, "m2", "Bare")]
    });
    let tiles = [heat(Key::Down), nameless_facts(Key::Right)];
    let _cell = Cell::default();
    grade(&mut w, &tiles, &no_state);
}
