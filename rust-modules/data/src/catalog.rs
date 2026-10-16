//! The catalog page's data (plan §2.4 W6): the plex.tv discover record of a title no server holds
//! (`GET discover /library/metadata/<hex>`), and the page's membership answer. CONTRACT-FREEZE
//! STAGE: the types below are the shared shape; `stores::catalog::CatalogStore` owns the fetch.
//!
//! No title text appears in a `Debug` of these types' owners; the `Load` itself derives `Debug`
//! because the log scrubber treats `{:?}` of a page record as the screen's concern, so callers
//! must not format it into the event log.

use crate::watchlist::model::Kind;

/// Is the title on the signed-in profile's Watchlist? Read from `WatchlistStore` (never fetched by
/// the catalog store itself). `Unknown` while the list has not landed or plex.tv is unreachable:
/// the page then wears the Add face (the PUT is idempotent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Membership {
    On,
    Off,
    #[default]
    Unknown,
}

/// What the status line says, and so which icon it wears (clock / server-slash / none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CatalogStatus {
    /// Nothing to say: no icon.
    #[default]
    None,
    /// Not released yet: the clock icon (and the 'Coming <date>' line when `release_date` is set).
    ComingSoon,
    /// Released, and no reachable server holds a copy: the server-slash icon.
    NotOnAnyServer,
}

/// The loaded record. Strings are empty, not optional, when the service sent none.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRecord {
    pub title: String,
    pub year: Option<i32>,
    pub kind: Kind,
    pub summary: String,
    pub genres: Vec<String>,
    /// The discover item's art URL (absolute); empty when none. Proxied through a reachable PMS
    /// by the screen's image path, else the page draws a neutral ground.
    pub art_url: String,
    /// `originallyAvailableAt` as `YYYY-MM-DD`.
    pub release_date: Option<String>,
    pub status: CatalogStatus,
}

/// Where the page's fetch stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    Loading,
    Loaded(Box<CatalogRecord>),
    /// The service answered and has no such title.
    NotFound,
    /// plex.tv could not be reached (or there is no account credential): Try again, no list needed.
    Unreachable,
}
