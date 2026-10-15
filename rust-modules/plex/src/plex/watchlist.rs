//! The plex.tv Watchlist on `discover.provider.plex.tv` (docs/watchlist-plan.md §1.1, §1.3, §1.4).
//!
//! CONTRACT FREEZE: the types and signatures below are what `data::watchlist` (W2) compiles
//! against. The bodies are stubs until W1a fills them; the SHAPE is fixed. A change to a name or
//! a signature here is a change to every dependent, so make it deliberately and say so.
//!
//! Written as an `impl AccountClient` block like `discover.rs`. Every call is blocking; run it on
//! a worker thread. The account token rides in the request the transport builds, never in the
//! [`WatchRequest`] a closure sees, so a test closure cannot leak it and a log line cannot carry
//! it. Logs name the status and the endpoint SHAPE only.
//!
//! Wire facts (plan INPUT 1/2): list `GET /library/sections/watchlist/all` paged by
//! `X-Plex-Container-Start`/`-Size` (Size > 100 answers 400); `released` variant
//! `/library/sections/watchlist/released`; `userState` answers an ARRAY on discover and an OBJECT
//! on metadata.provider; add/remove are `PUT /actions/addToWatchlist?ratingKey=<hex>` /
//! `removeFromWatchlist`. Discover ratingKeys are 24-char hex, NOT PMS ratingKeys.
use super::account::AccountClient;

/// Largest page the service accepts (101 answers 400).
pub const MAX_PAGE: u32 = 100;

/// What kind of title a watchlist row is. Plex also lists collections/other kinds; those decode to
/// [`WatchKind::Other`] and the data layer drops them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WatchKind {
    Movie,
    Show,
    Other,
}

/// One watchlist row. Every field but `hex` and `guid` tolerates absence (shape drift costs the
/// field, never the page).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WatchItem {
    /// The discover catalog id (24-char hex): the id `watchlist_add`/`remove`/`user_state` take.
    pub hex: String,
    /// `plex://movie/<hex>` or `plex://show/<hex>`; the join key to a PMS item.
    pub guid: String,
    pub kind: Option<WatchKind>,
    pub title: Option<String>,
    pub title_sort: Option<String>,
    pub year: Option<i32>,
    /// `watchlistedAt`, unix seconds. Only present while the item is on the list.
    pub added_at: Option<i64>,
    /// `originallyAvailableAt`, `YYYY-MM-DD`.
    pub originally_available: Option<String>,
    pub rating: Option<f32>,
    pub summary: Option<String>,
    pub thumb: Option<String>,
}

/// One page of the list. `total` is the container's `totalSize` when it sent one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WatchPage {
    pub items: Vec<WatchItem>,
    pub total: Option<u32>,
    /// The `start` this page was requested at.
    pub start: u32,
}

/// `GET .../{hex}/userState`: `watchlisted_at` is `Some` only while the item is on the list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserState {
    pub watchlisted_at: Option<i64>,
}

/// `GET /library/metadata/<hex>` on the discover provider: a Discover-only item's page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogItem {
    pub item: WatchItem,
    pub genres: Vec<String>,
    pub content_rating: Option<String>,
    pub duration_ms: Option<i64>,
}

/// Why a watchlist call did not produce its answer. HTTP statuses map to the named variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchlistError {
    /// 304 (only with an If-None-Match, which needs NET1).
    NotModified,
    /// 400: a malformed request (a page Size over 100, a bad id).
    BadRequest,
    /// 401: the token is missing or refused.
    Unauthorized,
    /// 404: no such item or list variant.
    NotFound,
    /// No answer: offline, TLS, timeout.
    Transport,
    /// A `devtriggers` build refused a WRITE because the discover stand-in is not armed.
    Refused,
    /// A 2xx whose body this module could not read.
    Unreadable,
    /// Any other non-2xx status.
    Other(u16),
}

/// A request as the transport sees it. Carries no credential: the live transport adds the
/// identity headers and the token itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchRequest {
    /// `"GET"` or `"PUT"`.
    pub method: &'static str,
    /// Absolute URL, base from [`discover`].
    pub url: String,
    /// `If-None-Match` value (NET1; always `None` until then).
    pub if_none_match: Option<String>,
}

/// A transport answer: status and raw body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The injectable request closure every call takes (tests pass a canned one; production passes
/// [`AccountClient::watchlist_transport`]).
pub type WatchTransport<'a> = &'a mut dyn FnMut(&WatchRequest) -> Result<WatchResponse, WatchlistError>;

/// The discover provider's base URL: `https://discover.provider.plex.tv`, or, in a dev build
/// only, the loopback stand-in named by `/tmp/plxnative-discover` (guarded exactly like
/// `account::plex_tv()`). Read once.
pub fn discover() -> &'static str {
    unimplemented!("W1a")
}

impl AccountClient {
    /// The live transport for this client: libcurl HTTPS with the account headers and token.
    /// W1a: a `devtriggers` build REFUSES a `PUT` with [`WatchlistError::Refused`] unless the
    /// discover stand-in is armed.
    pub fn watchlist_transport(&self) -> impl FnMut(&WatchRequest) -> Result<WatchResponse, WatchlistError> + '_ {
        |_req: &WatchRequest| -> Result<WatchResponse, WatchlistError> { unimplemented!("W1a") }
    }

    /// One page of the default-order list (`start` 0-based, `size` clamped to [`MAX_PAGE`]).
    pub fn watchlist_page(&self, start: u32, size: u32, req: WatchTransport) -> Result<WatchPage, WatchlistError> {
        let _ = (start, size, req);
        unimplemented!("W1a")
    }

    /// The `released` variant of the list (items already released), all pages folded in.
    pub fn watchlist_released(&self, req: WatchTransport) -> Result<Vec<WatchItem>, WatchlistError> {
        let _ = req;
        unimplemented!("W1a")
    }

    /// `userState` for one item; accepts the array form (discover) or the object form.
    pub fn user_state(&self, hex: &str, req: WatchTransport) -> Result<UserState, WatchlistError> {
        let _ = (hex, req);
        unimplemented!("W1a")
    }

    /// A Discover-only item's metadata, for the catalog page.
    pub fn catalog_item(&self, hex: &str, req: WatchTransport) -> Result<CatalogItem, WatchlistError> {
        let _ = (hex, req);
        unimplemented!("W1a")
    }

    /// `PUT /actions/addToWatchlist?ratingKey=<hex>`.
    pub fn watchlist_add(&self, hex: &str, req: WatchTransport) -> Result<(), WatchlistError> {
        let _ = (hex, req);
        unimplemented!("W1a")
    }

    /// `PUT /actions/removeFromWatchlist?ratingKey=<hex>`.
    pub fn watchlist_remove(&self, hex: &str, req: WatchTransport) -> Result<(), WatchlistError> {
        let _ = (hex, req);
        unimplemented!("W1a")
    }
}
