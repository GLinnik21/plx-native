//! The Watchlist's captions follow the global Under posters setting, the one the Library reads: the
//! Title switch opens the title line, the Details picker the second, and the focused band is 0 / 80 /
//! 112 px by what it will show. (Owner, on a TV: the page showed "Missing Link / 2019" with the Title
//! switch Off.)
use plx_data::watchlist::model::TileState;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

use super::caption::{self, TEST_UNDER_POSTERS};
use super::cards_harness::{row, tile, Harness};
use super::page::WatchlistScreen;

fn set(title: bool, details: UnderPostersDetails) {
    TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters { title, details })));
}

fn lines(state: TileState) -> (Option<String>, Option<String>) {
    let label = caption::label(&row(1), state, &caption::policy());
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
    assert_eq!(lines(TileState::ServerOffline).1.as_deref(), Some("Server offline"), "Genre has nothing to say for a row");
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
