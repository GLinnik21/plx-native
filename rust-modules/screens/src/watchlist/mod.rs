//! **The Watchlist page** (docs/watchlist-plan.md §1.1-§1.11, §2.4 W45).
//!
//! CONTRACT FREEZE: this module currently holds the interfaces the W45 packages build against and
//! stub bodies where a package fills them in. The shapes are what is frozen:
//!
//! * [`page`]: [`page::WatchlistScreen`] (the page's state), its input/output alphabets
//!   [`page::WatchlistIn`] / [`page::WatchlistOut`] and the row snapshot [`page::WatchTile`].
//! * [`options`]: [`options::WatchlistOptionsState`], the Sort & Filter value.
//! * [`header`]: the title + pill header ([`header::draw_header`]).
//! * [`readout`]: the empty / filtered / no-token read-outs anchored at `FULL_ANCHOR_TOP`.
//! * [`hold`]: the grid's hold-menu rows and results.
//!
//! No sibling screen is named here: a Detail or catalog target leaves as an output the app routes.
pub mod header;
pub mod hold;
pub mod options;
pub mod page;
pub mod readout;
