//! **The Watchlist page** (docs/watchlist-plan.md §1.1-§1.11, §2.4 W45).
//!
//! The modules:
//!
//! * [`page`]: [`page::WatchlistScreen`] (the page's state), its input/output alphabets
//!   [`page::WatchlistIn`] / [`page::WatchlistOut`] and the row snapshot [`page::WatchTile`].
//! * [`options`]: [`options::WatchlistOptionsState`], the Sort & Filter value.
//! * [`header`]: the title + pill header ([`header::draw_header`]).
//! * [`readout`]: the empty / filtered / no-token read-outs anchored at `FULL_ANCHOR_TOP`.
//!
//! No sibling screen is named here: a Detail or catalog target leaves as an output the app routes.
pub mod caption;
pub mod header;
pub mod menu;
pub mod options;
pub mod page;
pub mod readout;
pub mod tiles;

#[cfg(test)]
pub(crate) mod cards_harness;
#[cfg(test)]
mod page_tests;
#[cfg(test)]
mod reflow_tests;
