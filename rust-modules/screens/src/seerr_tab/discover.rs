//! Discover's shelves (docs/seerr.md): the server's suggestion rows, each a shelf of the shared
//! Request card, with the titles already wholly on the server left out unless the header's
//! "Available" control shows them. A partly available show stays: it still has seasons to ask for.
//!
//! **Every page stays reachable.** Pages are kept per slot (`Slot::Discover(row, page)`), and a
//! shelf whose server has more asks for the next page as the reader walks near its end
//! (`CardSource::more`, `CardEvent::Want`), as a library shelf does. Leaving titles out can thin a
//! row the server filled, so a row whose shown cards fall short of [`ROW_FILL`] reads on by itself,
//! [`MAX_FILL_PAGES`] pages at most: past that the reader's walk asks. A title the server repeats on
//! a later page is shown once.
use std::collections::HashSet;
use std::ffi::CStr;

use plx_data::seerr::{Availability, DiscoverRow, SeerrData};
use plx_data::stores::seerr::{Answer, SeerrStore, Slot};
use plx_platform::i18n::msg;

use crate::registry::seerr_cards::RequestCard;

/// The rows, top to bottom.
pub const ROWS: [DiscoverRow; 5] = [
    DiscoverRow::Trending,
    DiscoverRow::PopularMovies,
    DiscoverRow::PopularTv,
    DiscoverRow::UpcomingMovies,
    DiscoverRow::UpcomingTv,
];

/// A shelf with fewer cards than this reads the server's next page by itself: about a screenful
/// and a half.
pub const ROW_FILL: usize = 10;
/// The pages a row reads by itself to fill up to [`ROW_FILL`]; further pages are read as the reader
/// walks the row.
pub const MAX_FILL_PAGES: u32 = 3;

/// `row`'s place in [`ROWS`].
pub fn ordinal(row: DiscoverRow) -> usize {
    ROWS.iter().position(|r| *r == row).unwrap_or(0)
}

pub fn heading(row: DiscoverRow) -> &'static CStr {
    match row {
        DiscoverRow::Trending => msg::requests_row_trending_c(),
        DiscoverRow::PopularMovies => msg::requests_row_popular_movies_c(),
        DiscoverRow::PopularTv => msg::requests_row_popular_tv_c(),
        DiscoverRow::UpcomingMovies => msg::requests_row_upcoming_movies_c(),
        DiscoverRow::UpcomingTv => msg::requests_row_upcoming_tv_c(),
    }
}

/// One row as the pages that have landed make it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    /// The cards it shows, in the server's order.
    pub cards: Vec<RequestCard>,
    /// The pages read so far (the leading run of landed pages).
    pub read: u32,
    /// The server has pages past them.
    pub more: bool,
}

impl Row {
    /// The page the row reads by itself now: the next one while it is short of [`ROW_FILL`], within
    /// [`MAX_FILL_PAGES`].
    pub fn fill_page(&self) -> Option<u32> {
        (self.read > 0 && self.more && self.cards.len() < ROW_FILL && self.read < MAX_FILL_PAGES).then_some(self.read + 1)
    }

    /// The page a walk near the row's end asks for.
    pub fn next_page(&self) -> Option<u32> {
        (self.read > 0 && self.more).then_some(self.read + 1)
    }
}

/// `row` as its landed pages make it, the titles on the server left out unless `show_available`.
pub fn row(seerr: &SeerrStore, row: DiscoverRow, show_available: bool) -> Row {
    let mut seen = HashSet::new();
    let mut out = Row::default();
    while let Some(Answer::Ready(SeerrData::Titles(answer))) = seerr.answer(&Slot::Discover(row, out.read + 1)) {
        out.read += 1;
        out.more = out.read < answer.pages;
        for title in &answer.items {
            if !show_available && title.status == Availability::Available {
                continue;
            }
            if let Some(card) = RequestCard::of(title).filter(|c| seen.insert((c.kind, c.tmdb))) {
                out.cards.push(card);
            }
        }
    }
    out
}
