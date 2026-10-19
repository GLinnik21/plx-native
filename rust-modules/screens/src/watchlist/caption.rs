//! The focus caption (plan §1.9, boards Watchlist and TileStates): what the focused tile's band shows.
//!
//! **Under posters.** The band is the global Under posters setting's, the one the Library reads
//! ([`policy`]): the Title switch (default Off) opens the title line, the Details picker the second,
//! and the band is 0 / 80 / 112 px tall by what it will show (`Stack::caption_policy`). A Watchlist row
//! carries a title, a year and a kind, so Genre, Video and Age rating have nothing to say for it.
//! An unplayable title's state sentence (below) REPLACES the details line whenever the setting shows
//! one: with Details set to Nothing the band is not opened, and no sentence is shown.
//!
//! A playable title reads its year under the default Details, as every poster does. An unplayable title carries its state as
//! a SENTENCE, because the tile itself says nothing in words (TileStates: "No text on the tile; the
//! focused caption carries the words"): the release date for an upcoming title, "Not on your servers", or
//! "Server offline". The sentences are catalog strings (`watchlist.state.*`) and the date comes from
//! the locale's own month-and-day tables (`LocaleContext::month_day`, W3), medium width, in the one
//! phrase every page uses for it ("Coming 12 Mar", `fmt::coming_on`).
use plx_data::watchlist::model::{Kind, TileState, WatchRow};
use plx_platform::i18n::{self, msg, MonthWidth};
use plx_ui::caption_policy::{caption_lines, CaptionFacts, CaptionPolicy, DetailsKind};
use plx_ui::cards::TileLabel;
use plx_ui::fmt;

/// The release phrase of an upcoming title ("Coming 12 Mar", the one phrase [`fmt::coming_on`]
/// tells on every page), or "Coming soon" when the service sent no usable date.
fn upcoming(release: Option<&str>) -> String {
    release
        .and_then(|date| fmt::coming_on(date, MonthWidth::Medium))
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

/// The Under posters setting as the caption policy this page's band follows: the same value the
/// Library page reads.
pub fn policy() -> CaptionPolicy {
    #[cfg(test)]
    if let Some(setting) = TEST_UNDER_POSTERS.with(|cell| cell.get()) {
        return crate::registry::caption_policy_of(setting);
    }
    crate::registry::caption_policy_of(plx_media::route::under_posters())
}

#[cfg(test)]
thread_local! {
    /// A per-thread Under posters setting for a test, so it never writes the process-wide cell.
    pub(crate) static TEST_UNDER_POSTERS: std::cell::Cell<Option<plx_plex::plex::session::UnderPosters>> =
        const { std::cell::Cell::new(None) };
}

/// The focused tile's label under `policy`: `[title, details]` as the setting words them, the
/// details line being the state sentence when the title has one.
pub fn label(row: &WatchRow, state: TileState, policy: &CaptionPolicy) -> TileLabel {
    let facts = CaptionFacts {
        title: Some(row.title.clone()),
        year: row.year,
        is_show: row.kind == Kind::Show,
        ..Default::default()
    };
    let [title, details] = caption_lines(policy, &facts, i18n::current());
    let details = if policy.details == DetailsKind::None {
        None
    } else {
        state_sentence(state, row.release.as_deref()).or(details)
    };
    TileLabel::from_lines([title, details])
}
