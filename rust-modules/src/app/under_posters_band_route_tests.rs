//! **The room a focused caption opens, graded at the app tier** (`routes.md`, R13): what a poster
//! (or a still) DRAWS under it against the band the Under posters setting reserves for it, on a real
//! `Dispatcher<AppHost>` over a real store. A caption that overruns its band is painted before the
//! next row's tiles (`Grid::draw` paints every caption first), so the row below covers whatever it
//! overruns; these tests read the drawn runs that START in the gap between the focused card and the
//! next row (its caption) against the next row's tile top.

use super::test_support::World;
use super::under_posters_shelf_route_tests::{set, Cell};
use plx_plex::plex::session::UnderPostersDetails as Details;

pub(super) const PICKERS: [Details; 5] =
    [Details::YearLength, Details::Genre, Details::Video, Details::AgeRatingScore, Details::None];

/// **B1.** The Library's Episodes listing (landscape stills) is off the Under posters policy like
/// every other landscape surface: its two-line block (title, release date) is drawn into the legacy
/// band, so the date line never ends under the next row's stills, whatever the setting.
#[test]
fn the_episodes_listings_caption_clears_the_next_row_under_every_setting() {
    let mut w = World::episodes_library("band-episodes");
    let _cell = Cell::default();
    for title in [false, true] {
        for details in PICKERS {
            set(&mut w, title, details);
            let (card, cols) = w.library().probe_cell(0);
            let (below, _) = w.library().probe_cell(cols);
            let setting = format!("title {title}, details {details:?}");
            let drawn = w.drawn();
            let date = plx_ui::fmt::pretty_date("2019-01-03", 2019);
            let under: Vec<_> = drawn.runs.iter()
                .filter(|run| run.at.y >= card.y + card.h && run.at.y < below.y
                    && run.at.x >= card.x - 1.0 && run.at.x < card.x + card.w)
                .collect();
            assert!(under.iter().any(|run| run.text == date),
                "premise: the focused still draws its release date ({setting}): {:?}", drawn.words);
            for run in &under {
                let bottom = run.at.y + run.at.h.max(run.size as f32);
                assert!(bottom <= below.y, "{:?} ends at {bottom} under the next row's tiles at {} ({setting})", run.text, below.y);
            }
        }
    }
}

/// **What a poster grid opens under its focused row, read off the grid's own geometry**: the gap
/// from the focused row's cards to the next row's grows over what a closed band (Title Off +
/// Nothing) leaves by the growth the grid documents for each band (`poster_grid::growth_before_for`:
/// 72 px for a two-line band, 40 for a one-line one, 0 for none), and the caption's lines end inside
/// that gap, so the label drawn and the room reserved for it cannot disagree. The page's first
/// poster holds focus; the page is a Library grid, the Collections page, the Watchlist or the
/// Collection screen's members (`World::poster_cell`).
fn grade_grid_bands(w: &mut World, surface: &str) {
    use plx_ui::caption_policy::{band_h, CaptionPolicy};
    use plx_screens::registry::caption_policy_of;
    let _cell = Cell::default();
    focus_first_poster(w, surface);
    // the first cell of the row after the focused one
    let below = |w: &World| {
        let first = w.poster_cell(0);
        (1..64).map(|i| w.poster_cell(i)).find(|cell| cell.y > first.y + 1.0)
            .unwrap_or_else(|| panic!("{surface}: premise: the grid holds a second row"))
    };
    let gap = |w: &World| below(w).y - (w.poster_cell(0).y + w.poster_cell(0).h);
    set(w, false, Details::None);
    let closed = gap(w);
    for (title, details, band, want) in [(true, Details::YearLength, 112.0, 72.0), (false, Details::YearLength, 80.0, 40.0),
        (true, Details::None, 80.0, 40.0), (false, Details::None, 0.0, 0.0)] {
        set(w, title, details);
        let policy: CaptionPolicy = caption_policy_of(plx_plex::plex::session::UnderPosters { title, details, sort_value: false });
        assert_eq!(band_h(&policy), band, "{surface}: premise: the policy's band");
        let (card, next) = (w.poster_cell(0), below(w));
        let opened = gap(w) - closed;
        assert!((opened - want).abs() < 0.5, "{surface}: the grid opens {opened} px for title {title}, {details:?}; the policy asks {want}");
        let drawn = w.drawn();
        let under: Vec<_> = drawn.runs.iter()
            .filter(|run| run.at.y >= card.y + card.h && run.at.y < next.y && run.at.x < card.x + card.w && run.at.x + run.at.w > card.x)
            .collect();
        assert_eq!(!under.is_empty(), band > 0.0, "{surface}: premise: the focused poster draws a caption exactly when a band is open ({title}, {details:?})");
        for line in under {
            assert!(line.at.y + line.at.h.max(line.size as f32) <= next.y,
                "{surface}: {:?} ends under the next row (title {title}, {details:?})", line.text);
        }
    }
}

/// Press Down until a poster holds focus (the Title switch is on, so a focused poster draws its
/// caption): a page that opens on its header is graded on its first poster.
pub(super) fn focus_first_poster(w: &mut World, surface: &str) {
    set(w, true, Details::Genre);
    for _ in 0..6 {
        if !w.drawn().caption_lines().is_empty() { return; }
        w.press(super::Key::Down);
    }
    panic!("{surface}: premise: Down brought focus onto a poster");
}

/// **The band the Library grid opens under its focused row is the policy's** (112 / 80 / 0 px).
#[test]
fn the_library_grid_opens_the_band_its_setting_asks_for() {
    let mut w = World::movies_library("band-grid", false);
    let _cell = Cell::default();
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.frames(30);
    assert_eq!(w.grid_cell(), Some((0, 0)), "premise: the library opens on its first poster");
    grade_grid_bands(&mut w, "Library");
}

/// The Collections page's tiles are posters in the same grid.
#[test]
fn the_collections_page_opens_the_band_its_setting_asks_for() {
    let mut w = World::collections("band-collections", false);
    grade_grid_bands(&mut w, "Collections");
}

/// The Watchlist's titles are posters in the same grid.
#[test]
fn the_watchlist_opens_the_band_its_setting_asks_for() {
    let mut w = World::watchlist("band-watchlist");
    grade_grid_bands(&mut w, "Watchlist");
}

/// The Collection screen's members are posters in the same grid.
#[test]
fn the_collection_screen_opens_the_band_its_setting_asks_for() {
    let mut w = super::arrival_seat_tests::collection_screen_of("band-collection", |sid| {
        (0..16).map(|i| member(sid, &format!("m{i}"))).collect()
    });
    grade_grid_bands(&mut w, "Collection screen");
}

/// A film a server describes by its name alone.
fn member(sid: plx_plex::plex::ServerId, rk: &str) -> plx_data::pms::PmsMovie {
    plx_data::pms::PmsMovie { sid, sec: 1, rk: rk.into(), title: format!("M{rk}"), kind: 0, year: 1999,
        dur_ns: 100 * 60 * 1_000_000_000, ..Default::default() }
}

// ── The same grade on every shelf surface ───────────────────────────────────────────────────────

/// **What a shelf page opens under its focused poster row, read off the frame it draws**: the pitch
/// between the focused shelf's heading and the next shelf's heading (both text runs of the one frame,
/// so a page scrolling under the focus moves them together and cancels out), and the room the
/// caption's runs leave above that next heading.
///
/// The setting's band is graded by how far the pitch GROWS over the closed band (Title Off +
/// Nothing opens nothing), against the growth the layout documents for each band
/// ([`band_open_for`]: 72 px for a two-line band, 40 for a one-line one, 0 for none): the Library
/// grid's grade, on a shelf. The runs strictly between the two headings are the caption block (a
/// poster draws no text of its own), and none may end under the next heading.
///
/// `deck`: the focused shelf is the Continue Watching deck, whose policy is the setting's
/// `captions::deck_policy` (its progress line is always shown, so Nothing keeps one line): the same
/// grade, with each band and the closed baseline read through that policy.
fn grade_shelf_bands(w: &mut World, surface: &str, focused: &str, next: &str, deck: bool) {
    use plx_screens::registry::caption_policy_of;
    use plx_ui::caption_policy::{band_h, CaptionPolicy};
    use plx_ui::cards::band_open_for;
    let _cell = Cell::default();
    let pitch = |w: &mut World, title: bool, details: Details| {
        set(w, title, details);
        let drawn = w.drawn();
        let top = |text: &str| drawn.words.iter().find(|(words, _)| words == text).unwrap_or_else(|| panic!(
            "{surface}: premise: the heading {text:?} is drawn (title {title}, {details:?}): {:?}", drawn.words)).1.y;
        let (from, to) = (top(focused), top(next));
        for run in drawn.runs.iter().filter(|run| run.at.y > from && run.at.y < to) {
            let bottom = run.at.y + run.at.h.max(run.size as f32);
            assert!(bottom <= to, "{surface}: {:?} ends at {bottom} under the next heading at {to} (title {title}, {details:?})", run.text);
        }
        to - from
    };
    let closed = pitch(w, false, Details::None);
    let effective = |policy: CaptionPolicy| if deck { plx_screens::registry::captions::deck_policy(&policy) } else { policy };
    let baseline = band_open_for(band_h(&effective(caption_policy_of(plx_plex::plex::session::UnderPosters { title: false, details: Details::None, sort_value: false }))));
    for (title, details, band, grows) in [(true, Details::YearLength, 112.0, 72.0), (false, Details::YearLength, 80.0, 40.0),
        (true, Details::None, 80.0, 40.0), (false, Details::None, 0.0, 0.0), (true, Details::Genre, 112.0, 72.0)] {
        let policy: CaptionPolicy = caption_policy_of(plx_plex::plex::session::UnderPosters { title, details, sort_value: false });
        assert_eq!(band_h(&policy), band, "{surface}: premise: the policy's band for title {title}, {details:?}");
        assert!((band_open_for(band) - grows).abs() < 0.5, "{surface}: premise: the layout's growth for a {band} band");
        let (band, grows) = (band_h(&effective(policy)), band_open_for(band_h(&effective(policy))) - baseline);
        let opened = pitch(w, title, details) - closed;
        assert!((opened - grows).abs() < 0.5,
            "{surface}: the page opens {opened} px for title {title}, {details:?}; the {band} px band asks {grows}");
    }
}

/// Home: the Continue Watching deck is the focused shelf, Recently Added the one below it.
#[test]
fn home_opens_the_band_its_setting_asks_for() {
    let mut w = World::home_with_disc("band-home", [7; 12]);
    w.bridge.stores.hubs.seed_deck_home_for_test(1);
    w.frames(30);
    w.press(super::Key::Down);
    w.frames(60);
    grade_shelf_bands(&mut w, "Home", "Continue Watching", "Recently Added", true);
}

/// Recommended: its first poster shelf is focused on arrival, a second one sits below it.
#[test]
fn recommended_opens_the_band_its_setting_asks_for() {
    let mut w = World::recommended_pending("band-recommended");
    w.bridge.stores.browse.borrow_mut().seed_named_shelves_for_test(
        0, &[("hub.movie.recent", "", "Recently Added"), ("hub.movie.top", "", "Top Rated")], 3);
    w.frames(60);
    grade_shelf_bands(&mut w, "Recommended", "Recently Added", "Top Rated", false);
}

/// Detail: Related is the focused shelf (three Downs from the top), About the section below it.
#[test]
fn detail_related_opens_the_band_its_setting_asks_for() {
    use plx_data::pms::PmsMovie;
    let sid = plx_plex::plex::ServerId::UNSET;
    let member = |rk: &str| PmsMovie { sid, sec: 1, rk: rk.into(), title: format!("M{rk}"), kind: 0, year: 1999,
        dur_ns: 100 * 60 * 1_000_000_000, ..Default::default() };
    let detail = plx_data::metadata::Detail {
        sid, rk: "movie".into(), kind: "movie".into(),
        collection: Some(plx_data::metadata::CollectionShelf { title: "Trilogy".into(), section: 1, tag: 812,
            members: vec![member("c1"), member("c2")], count: 2 }),
        related: vec![member("r1"), member("r2")],
        ..Default::default()
    };
    let mut w = World::detail_page("band-detail", detail);
    for _ in 0..3 {
        w.press(super::Key::Down);
        w.frames(30);
    }
    grade_shelf_bands(&mut w, "Detail", "Related", "About", false);
}

/// Detail: the collection shelf is the first poster shelf, Related the one below it.
#[test]
fn detail_collection_shelf_opens_the_band_its_setting_asks_for() {
    let sid = plx_plex::plex::ServerId::UNSET;
    let detail = plx_data::metadata::Detail {
        sid, rk: "movie".into(), kind: "movie".into(),
        collection: Some(plx_data::metadata::CollectionShelf { title: "Trilogy".into(), section: 1, tag: 812,
            members: vec![member(sid, "c1"), member(sid, "c2")], count: 2 }),
        related: vec![member(sid, "r1"), member(sid, "r2")],
        ..Default::default()
    };
    let mut w = World::detail_page("band-detail-collection", detail);
    let _cell = Cell::default();
    focus_first_poster(&mut w, "Detail collection");
    grade_shelf_bands(&mut w, "Detail collection", "Trilogy", "Related", false);
}

/// Search: its Movies row is the focused shelf, the Shows row the one below it (the Stack hands each
/// shelf its policy, `cards/stack.rs`'s `Body::Shelf` arm).
#[test]
fn search_opens_the_band_its_setting_asks_for() {
    use plx_data::search::{Item, Kind, Shelf};
    let mut w = World::search_results("band-search", |sid| {
        let shelf = |kind, items| Shelf { epoch: 0, window: Default::default(), kind, items };
        let show = plx_data::pms::PmsMovie { kind: 1, ..member(sid, "s1") };
        vec![
            shelf(Kind::Movie, vec![Item::Media(member(sid, "m1")), Item::Media(member(sid, "m2"))]),
            shelf(Kind::Show, vec![Item::Media(show)]),
        ]
    });
    let _cell = Cell::default();
    focus_first_poster(&mut w, "Search");
    grade_shelf_bands(&mut w, "Search", Kind::Movie.title(), Kind::Show.title(), false);
}

/// The person page's filmography: the Movies shelf is the focused one, the Shows shelf below it.
#[test]
fn the_filmography_opens_the_band_its_setting_asks_for() {
    use plx_platform::i18n::msg;
    let sid = plx_plex::plex::ServerId::UNSET;
    let show = plx_data::pms::PmsMovie { kind: 1, ..member(sid, "s1") };
    let mut w = World::person_page("band-person", vec![member(sid, "m1"), member(sid, "m2")], vec![show], &["", ""]);
    let _cell = Cell::default();
    focus_first_poster(&mut w, "Filmography");
    grade_shelf_bands(&mut w, "Filmography", msg::browse_kind_movies(), msg::browse_kind_shows(), false);
}

// ── Detail names what the server did not: a season 0 and an episode with no number ───────────────

/// **Detail's season tab and episode card use the same identity rule the captions do.** A show whose
/// season 0 the server gave no title reads "Specials" on its tab (the caption under the same
/// season's poster says it), never "Season 0"; and the card of an episode it did not number reads
/// the kind word in place of "EPISODE 0", beside the cards that keep "EPISODE N".
#[test]
fn detail_names_an_untitled_season_zero_and_an_unnumbered_episode_by_their_identity() {
    use plx_data::metadata::{Detail, Episode, EpisodeList, Season};
    let sid = plx_plex::plex::ServerId::UNSET;
    let mut episodes = EpisodeList::with_total(2);
    episodes.set_page(0, vec![
        Episode { rk: "e1".into(), index: 1, season: 0, title: "Numbered".into(), dur_ms: 60_000, ..Default::default() },
        Episode { rk: "e2".into(), index: 0, season: 0, title: "Unnumbered".into(), aired: "2016-04-21".into(), dur_ms: 60_000, ..Default::default() },
    ]);
    let detail = Detail {
        sid, rk: "movie".into(), kind: "show".into(), is_show: true, title: "The Office".into(),
        seasons: vec![
            Season { rk: "s0".into(), index: 0, title: String::new(), leaf_count: 2, viewed_leaf_count: 0 },
            Season { rk: "s1".into(), index: 1, title: "Season 1".into(), leaf_count: 1, viewed_leaf_count: 0 },
        ],
        cur_season: 0,
        episodes,
        ..Default::default()
    };
    let mut w = World::detail_page("detail-identity", detail);
    let mut seen = Vec::new();
    for _ in 0..8 {
        w.frames(30);
        seen.extend(w.drawn().words.into_iter().map(|(text, _)| text));
        w.press(super::Key::Down);
    }
    let kind = plx_platform::i18n::msg::browse_kind_episode().to_uppercase();
    let has = |text: &str| seen.iter().any(|seen| seen == text);
    assert!(has("Specials"), "the untitled season 0's tab reads its identity: {seen:?}");
    assert!(!has("Season 0"), "never Season 0: {seen:?}");
    assert!(has("Season 1"), "a titled season keeps its title: {seen:?}");
    assert!(has("EPISODE 1"), "a numbered episode keeps EPISODE N: {seen:?}");
    assert!(has(&kind), "the unnumbered episode's card says {kind:?}: {seen:?}");
    assert!(!has("EPISODE 0"), "never EPISODE 0: {seen:?}");
}
