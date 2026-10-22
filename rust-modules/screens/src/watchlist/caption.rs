//! The focus caption (plan §1.9, boards Watchlist and TileStates): what the focused tile's band shows.
//!
//! **Under posters.** The band is the global Under posters setting's, the one the Library reads
//! ([`policy`]): the Title switch (default Off) opens the title line, the Details picker the second,
//! and the band is 0 / 80 / 112 px tall by what it will show (`Stack::caption_policy`); the lines are
//! built by the one builder the poster screens share ([`crate::registry::captions::label`]). A Watchlist row
//! carries a title, a year and a kind; the rest of what a caption words (genre, video, age rating and
//! score, length, season and episode counts) is the MATCHED COPY's, as the guid lookup's row said it
//! ([`ListingFacts`]), so a playable title reads like any poster and one with no copy falls to its year.
//! An unplayable title's state sentence (below) REPLACES the details line whenever the setting shows
//! one: with Details set to Nothing the sentence is dropped with the details line (and the band is not opened when the Title switch is Off).
//!
//! A playable title reads like any poster under the setting (its copy's year and length, genre, stream or age
//! rating and score), and falls to its year when it has no copy. An unplayable title carries its state as
//! a SENTENCE, because the tile itself says nothing in words (TileStates: "No text on the tile; the
//! focused caption carries the words"): the release date for an upcoming title, "Not on your servers", or
//! "Server offline". The sentences are catalog strings (`watchlist.state.*`) and the date comes from
//! the locale's own month-and-day tables (`LocaleContext::month_day`, W3), medium width, in the one
//! phrase every page uses for it ("Coming 12 Mar", `fmt::coming_on`).
use plx_data::watchlist::model::{Kind, TileState, WatchRow};
use plx_platform::i18n::{msg, MonthWidth};
use plx_plex::plex::ListingFacts;
use plx_ui::caption_policy::{CaptionFacts, CaptionPolicy};
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

/// The Under posters setting as the caption policy this page's band follows: the same value every
/// poster shelf reads ([`crate::registry::captions::policy`]).
pub fn policy() -> CaptionPolicy { crate::registry::captions::policy() }

/// The focused tile's label under `policy`: `[title, details]` as the setting words them, the
/// details line being the state sentence when the title has one. `copy` is what the matched copy's
/// lookup row says of the title (default: no copy, or a row that said nothing).
pub fn label(row: &WatchRow, state: TileState, policy: &CaptionPolicy, copy: &ListingFacts) -> TileLabel {
    let facts = CaptionFacts {
        title: Some(row.title.clone()),
        year: row.year,
        is_show: row.kind == Kind::Show,
        is_movie: row.kind == Kind::Movie,
        // The matched copy's own facts, as the lookup row said them: the row carries none of these.
        duration_min: copy.duration_ms.and_then(|ms| u32::try_from(ms / 60_000).ok()),
        genre: copy.genre.clone(),
        video_resolution: copy.video_resolution.clone(),
        hdr: copy.hdr,
        content_rating: copy.content_rating.clone(),
        audience_rating: copy.audience_rating,
        leaf_count: copy.leaf_count,
        viewed_leaf_count: copy.viewed_leaf_count,
        season_count: copy.season_count,
        ..Default::default()
    };
    crate::registry::captions::label(policy, &facts, state_sentence(state, row.release.as_deref()), false)
}
