//! The Watchlist's captions follow the global Under posters setting, the one the Library reads: the
//! Title switch opens the title line, the Details picker the second, and the focused band is 0 / 80 /
//! 112 px by what it will show. (Owner, on a TV: the page showed "Missing Link / 2019" with the Title
//! switch Off.)
use plx_data::watchlist::model::TileState;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_plex::plex::ListingFacts;
use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

use super::caption;
use crate::registry::captions::TEST_UNDER_POSTERS;
use super::cards_harness::{row, tile, Harness};
use super::page::WatchlistScreen;

fn set(title: bool, details: UnderPostersDetails) {
    TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters { title, details, sort_value: false })));
}

fn lines(state: TileState) -> (Option<String>, Option<String>) {
    let label = caption::label(&row(1), state, &caption::policy(), &Default::default());
    let s = |c: Option<std::ffi::CString>| c.map(|c| c.to_string_lossy().into_owned());
    (s(label.title), s(label.caption))
}

#[test]
fn the_title_line_follows_the_title_switch_and_defaults_off() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    set(false, UnderPostersDetails::YearLength);
    assert_eq!(lines(TileState::Available), (None, Some("2001".into())), "Title Off: the year alone");
    set(true, UnderPostersDetails::YearLength);
    assert_eq!(lines(TileState::Available), (Some("1".into()), Some("2001".into())));
    set(true, UnderPostersDetails::None);
    assert_eq!(lines(TileState::Available), (Some("1".into()), None), "Details Nothing: the title only");
}

#[test]
fn a_state_sentence_takes_the_details_line_unless_the_setting_shows_none() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    set(false, UnderPostersDetails::YearLength);
    assert_eq!(lines(TileState::NotOnAnyServer).1.as_deref(), Some("Not on your servers"));
    set(false, UnderPostersDetails::Genre);
    assert_eq!(lines(TileState::ServerOffline).1.as_deref(), Some("Server offline"), "the state sentence replaces the genre");
    set(false, UnderPostersDetails::None);
    assert_eq!(lines(TileState::ServerOffline), (None, None));
}

#[test]
fn the_focused_band_is_the_settings_0_80_or_112() {
    for (title, details, want) in [
        (false, UnderPostersDetails::None, 0.0),
        (false, UnderPostersDetails::YearLength, 80.0),
        (true, UnderPostersDetails::None, 80.0),
        (true, UnderPostersDetails::YearLength, 112.0),
    ] {
        set(title, details);
        let mut h = Harness::new(vec![tile(1), tile(2)]);
        h.run(2);
        assert_eq!(h.screen.band_h(), Some(want), "title={title} details={details:?}");
    }
    let _ = WatchlistScreen::WORD;
}

/// What a server's lookup row says of the title a tile matched.
fn drama() -> ListingFacts {
    ListingFacts {
        genre: Some("Drama".into()),
        video_resolution: Some("4k".into()),
        hdr: true,
        content_rating: Some("PG-13".into()),
        audience_rating: Some(7.8),
        duration_ms: Some(100 * 60_000),
        ..Default::default()
    }
}

fn details_of(row: &plx_data::watchlist::model::WatchRow, state: TileState, copy: &ListingFacts) -> Option<String> {
    caption::label(row, state, &caption::policy(), copy).caption.map(|c| c.to_string_lossy().into_owned())
}

/// **A playable title is worded by its matched copy**, as every poster is: the Genre, Video and Age
/// rating pickers read the copy's genre, resolution and HDR, and age rating and score, and the default
/// reads its length after the year (the row's own year, which the service sent). Before, the details
/// line read the year under every pick, because the lookup's facts never reached the caption.
#[test]
fn an_available_title_reads_its_matched_copys_facts_under_every_details_pick() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    for (details, want) in [
        (UnderPostersDetails::YearLength, "2001 \u{b7} 1h 40m"),
        (UnderPostersDetails::Genre, "Drama"),
        (UnderPostersDetails::Video, "4K \u{b7} HDR"),
        (UnderPostersDetails::AgeRatingScore, "PG-13 \u{b7} 7.8"),
    ] {
        set(false, details);
        assert_eq!(details_of(&row(1), TileState::Available, &drama()).as_deref(), Some(want), "{details:?}");
    }
}

/// A copy the lookup row said nothing of (a server that sent no genre, no media) falls to the same
/// identity line every poster does: the year.
#[test]
fn a_copy_with_no_such_fact_reads_the_year_under_a_pick_it_cannot_fill() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    for details in [UnderPostersDetails::Genre, UnderPostersDetails::Video, UnderPostersDetails::AgeRatingScore] {
        set(false, details);
        assert_eq!(details_of(&row(1), TileState::Available, &ListingFacts::default()).as_deref(), Some("2001"), "{details:?}");
    }
}

/// A show reads its seasons after the year, and its new episodes once it is in progress: the one
/// default every show tile reads.
#[test]
fn a_show_reads_its_seasons_and_its_new_episodes_like_every_show_tile() {
    use plx_data::watchlist::model::Kind;
    let _guard = language_on_this_thread_for_test(Preference::En);
    set(false, UnderPostersDetails::YearLength);
    let mut show = row(1);
    show.kind = Kind::Show;
    let unwatched = ListingFacts { season_count: Some(3), leaf_count: Some(30), viewed_leaf_count: Some(0), ..Default::default() };
    assert_eq!(details_of(&show, TileState::Available, &unwatched).as_deref(), Some("2001 \u{b7} 3 seasons"));
    let started = ListingFacts { viewed_leaf_count: Some(12), ..unwatched };
    assert_eq!(details_of(&show, TileState::Available, &started).as_deref(), Some("18 new episodes"));
}

/// The state sentence still replaces the details line of a title that is not playable, whatever its
/// copy's facts say.
#[test]
fn a_state_sentence_still_replaces_the_copys_facts() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    set(false, UnderPostersDetails::Genre);
    assert_eq!(details_of(&row(1), TileState::ServerOffline, &drama()).as_deref(), Some("Server offline"));
}
