//! The catalog page's data (plan §2.4 W6): the plex.tv discover record of a title no server holds
//! (`GET discover /library/metadata/<hex>`), and the page's membership answer. This module holds
//! the shared shapes, the I/O seam ([`CatalogIo`]: live in production, a script in tests) and the
//! mapping from the service's item to a [`CatalogRecord`]; `stores::catalog::CatalogStore` owns
//! the state, and its fetch is a job (`CatalogReq`, `finish_catalog_job`) that the application's
//! runner starts.
//!
//! No title text appears in a `Debug` of these types' owners; the `Load` itself derives `Debug`
//! because the log scrubber treats `{:?}` of a page record as the screen's concern, so callers
//! must not format it into the event log.

use crate::watchlist::model::Kind;
use crate::watchlist::WatchlistAuth;
use plx_plex::plex::account::AccountClient;
use plx_plex::plex::watchlist::{CatalogItem, WatchKind, WatchlistError};

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

/// What the status line says, and so which icon it wears (clock / circle-slash / server-slash / none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CatalogStatus {
    /// Nothing to say: no icon.
    #[default]
    None,
    /// Not released yet: the clock icon (and the 'Coming <date>' line when `release_date` is set).
    ComingSoon,
    /// Released, and no reachable server holds a copy: the circle-slash icon.
    NotOnAnyServer,
    /// A copy was matched, and every server holding it is unreachable now: the server-slash icon
    /// and "Server offline" (the date says nothing of this; the Watchlist's tile state does).
    ServerOffline,
}

impl CatalogStatus {
    /// The status the title's tile on the Watchlist dictates, if it has one: `None` leaves the
    /// record's own date rule ([`record_of`]) standing (the title is not on the list read), or
    /// says nothing is to be said ([`Some`] of [`CatalogStatus::None`]: an `Unknown` tile, whose
    /// silent server never produces "on no server", and an `Available` one, which has a copy).
    pub fn of_tile(state: crate::watchlist::model::TileState) -> CatalogStatus {
        use crate::watchlist::model::TileState;
        match state {
            TileState::ComingSoon | TileState::ComingSoonMatched => CatalogStatus::ComingSoon,
            TileState::NotOnAnyServer => CatalogStatus::NotOnAnyServer,
            TileState::ServerOffline => CatalogStatus::ServerOffline,
            TileState::Available | TileState::Unknown => CatalogStatus::None,
        }
    }
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
    /// plex.tv could not be reached (or there is no account credential). The page says so; it
    /// reads again on resume or when uncovered (`Bridge::catalog_reread`), there is no control.
    Unreachable,
}

/// Everything the store asks of the outside world. Production is [`LiveCatalogIo`].
pub trait CatalogIo: Send + Sync {
    /// MAIN THREAD: the identity to read as (`None` when the profile has no account credential).
    fn capture_auth(&self) -> Option<WatchlistAuth>;
    /// MAIN THREAD: is `auth` still the seated profile?
    fn is_current(&self, auth: &WatchlistAuth) -> bool;
    /// WORKER THREAD: `GET discover /library/metadata/<hex>`.
    fn fetch(&self, auth: &WatchlistAuth, hex: &str) -> Result<CatalogItem, WatchlistError>;
    /// Today as `YYYY-MM-DD` (UTC): what a release date is compared against.
    fn today(&self) -> Option<String>;
}

/// The production seam: libcurl through `plex::watchlist`, the identity from the session.
pub struct LiveCatalogIo;

impl CatalogIo for LiveCatalogIo {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        WatchlistAuth::capture()
    }
    fn is_current(&self, auth: &WatchlistAuth) -> bool {
        auth.is_current()
    }
    fn fetch(&self, auth: &WatchlistAuth, hex: &str) -> Result<CatalogItem, WatchlistError> {
        let client = AccountClient::new(&auth.client_id, Some(&auth.credential));
        let mut transport = client.watchlist_transport();
        let answer = client.catalog_item(hex, &mut transport);
        answer
    }
    fn today(&self) -> Option<String> {
        <crate::watchlist::LiveIo as crate::watchlist::WatchlistIo>::today(&crate::watchlist::LiveIo)
    }
}

/// The page's record from the service's item. The page opens for a title no reachable server
/// holds, so a released title reads "not on any server" and one whose date is still ahead reads
/// "coming soon"; a title with no date at all has nothing to say about release. `today` is
/// `YYYY-MM-DD`; when it is unknown a dated title is not called coming (the date still shows).
pub fn record_of(item: &CatalogItem, today: Option<&str>) -> CatalogRecord {
    let w = &item.item;
    let release_date = w.originally_available.clone().filter(|d| !d.is_empty());
    let status = match (&release_date, today) {
        // `YYYY-MM-DD` compares as text.
        (Some(d), Some(t)) if d.as_str() > t => CatalogStatus::ComingSoon,
        (Some(_), _) => CatalogStatus::NotOnAnyServer,
        (None, _) => CatalogStatus::None,
    };
    CatalogRecord {
        title: w.title.clone().unwrap_or_default(),
        year: w.year,
        kind: if w.kind == Some(WatchKind::Show) { Kind::Show } else { Kind::Movie },
        summary: w.summary.clone().unwrap_or_default(),
        genres: item.genres.clone(),
        art_url: w.thumb.clone().unwrap_or_default(),
        release_date,
        status,
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
