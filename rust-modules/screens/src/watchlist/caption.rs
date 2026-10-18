//! The focus caption (plan §1.9, boards Watchlist and TileStates): the line under a focused tile.
//!
//! A playable title reads its year, as every poster does. An unplayable title carries its state as
//! a SENTENCE, because the tile itself says nothing in words (TileStates: "No text on the tile; the
//! focused caption carries the words"): the release date for an upcoming title, "Not on your servers", or
//! "Server offline". The sentences are catalog strings (`watchlist.state.*`) and the date comes from
//! the locale's own month-and-day tables (`LocaleContext::month_day`, W3), medium width as the
//! board's "Releasing 12 Mar".
use plx_data::watchlist::model::{TileState, WatchRow};
use plx_platform::i18n::{self, msg, MonthWidth};

/// `YYYY-MM-DD` (what `originallyAvailableAt` carries) as `(month, day)`; `None` for anything else,
/// including a month or day the calendar does not have.
pub fn month_day(release: &str) -> Option<(u8, u8)> {
    let mut parts = release.split('-');
    let _year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((month, day))
}

/// The release phrase of an upcoming title ("Releasing 12 Mar"), or "Coming soon" when the service
/// sent no usable date.
fn upcoming(release: Option<&str>) -> String {
    release
        .and_then(month_day)
        .and_then(|(month, day)| i18n::current().month_day(month, day, MonthWidth::Medium))
        .map(|date| msg::watchlist_state_releasing_on(&date))
        .unwrap_or_else(|| msg::watchlist_state_coming_soon().to_string())
}

/// The state sentence for an unplayable tile, `None` for a state with no sentence of its own
/// (playable, or unknown: no match yet, so nothing true to say).
pub fn state_sentence(state: TileState, release: Option<&str>) -> Option<String> {
    match state {
        // Q6: coming soon AND matched keeps the clock, so it keeps the date.
        TileState::ComingSoon | TileState::ComingSoonMatched => Some(upcoming(release)),
        TileState::NotOnAnyServer => Some(msg::watchlist_state_on_no_server().to_string()),
        TileState::ServerOffline => Some(msg::watchlist_state_server_offline().to_string()),
        TileState::Available | TileState::Unknown => None,
    }
}

/// The caption line under the focused tile: the state sentence when there is one, else the year
/// (empty when the row has none, so the label draws its title alone).
pub fn caption(row: &WatchRow, state: TileState) -> String {
    state_sentence(state, row.release.as_deref())
        .unwrap_or_else(|| row.year.filter(|&y| y > 0).map(|y| y.to_string()).unwrap_or_default())
}
