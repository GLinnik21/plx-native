//! The plex.tv Watchlist on `discover.provider.plex.tv` (docs/watchlist-plan.md §1.1, §1.3, §1.4).
//!
//! The types and signatures below are what `data::watchlist` compiles against; a change to a
//! name or a signature here is a change to every dependent, so make it deliberately and say so.
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
use super::paging::{paged_path, PageReq};
use super::account::{endpoint_shape, loopback_http, note_response_contact, AccountClient};
use serde_json::Value;

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
    /// The raw entry count on the wire. Paging advances by this, not by `items.len()`: an entry
    /// with no usable id is dropped from `items` but still occupied a slot.
    pub raw: u32,
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

const DISCOVER: &str = "https://discover.provider.plex.tv";

/// Pages `watchlist_released` will fold before it stops (plan §1.3: cap 20 pages).
const MAX_RELEASED_PAGES: u32 = 20;

/// The discover provider's base URL: `https://discover.provider.plex.tv`, or, in a dev build
/// only, the loopback stand-in named by `/tmp/plxnative-discover` (guarded exactly like
/// `account::plex_tv()`: plain `http://` to `127.0.0.1`/`localhost` or it is refused and logged,
/// so the trigger can never point the account token at another host). Read once.
pub fn discover() -> &'static str {
    static BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BASE.get_or_init(|| match plx_base::devtrig::read("discover") {
        Some(v) if loopback_http(&v) => {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log("watchlist: discover replaced by a loopback stand-in (/tmp/plxnative-discover)");
            v.trim_end_matches('/').to_string()
        }
        Some(_) => {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log("BADTRIGGER discover: only http://127.0.0.1:<port> or http://localhost:<port> is accepted");
            DISCOVER.to_string()
        }
        None => DISCOVER.to_string(),
    })
}

/// Is the discover stand-in armed (the base is not the real service)?
fn stand_in_armed() -> bool {
    discover() != DISCOVER
}

/// The dev-build failsafe: a `devtriggers` build must never write to the real service, so a `PUT`
/// is refused unless the loopback stand-in is armed. Release builds always write.
fn permit_write(method: &str, devtriggers_build: bool, armed: bool) -> Result<(), WatchlistError> {
    if method == "PUT" && devtriggers_build && !armed {
        return Err(WatchlistError::Refused);
    }
    Ok(())
}

/// The log line for a call that did not succeed: method, endpoint SHAPE (ids folded, query
/// dropped) and status. Never the query, the id or any header, so it cannot carry the token.
fn failure_line(method: &str, url: &str, outcome: Result<u16, ()>) -> String {
    let shape = endpoint_shape(url);
    match outcome {
        Ok(status) => format!("watchlist: {method} {shape} -> HTTP {status}"),
        Err(()) => format!("watchlist: {method} {shape} -> no answer"),
    }
}

/// A catalog id is exactly 24 hex digits. Anything else never reaches a URL.
fn is_hex_id(s: &str) -> bool {
    s.len() == 24 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Run one request through the transport and map the status to a typed answer: 2xx yields the
/// response, everything else its [`WatchlistError`]. Failures are logged by shape.
fn call(req: WatchTransport, method: &'static str, url: String) -> Result<WatchResponse, WatchlistError> {
    let request = WatchRequest { method, url, if_none_match: None };
    match req(&request) {
        Ok(response) if (200..300).contains(&response.status) => Ok(response),
        Ok(response) => {
            plx_base::eventlog::log(&failure_line(method, &request.url, Ok(response.status)));
            Err(match response.status {
                304 => WatchlistError::NotModified,
                400 => WatchlistError::BadRequest,
                401 => WatchlistError::Unauthorized,
                404 => WatchlistError::NotFound,
                other => WatchlistError::Other(other),
            })
        }
        Err(e) => {
            if e != WatchlistError::Refused {
                plx_base::eventlog::log(&failure_line(method, &request.url, Err(())));
            }
            Err(e)
        }
    }
}

fn parse(response: &WatchResponse) -> Result<Value, WatchlistError> {
    serde_json::from_slice(&response.body).map_err(|_| WatchlistError::Unreadable)
}

/// `MediaContainer` of a decoded body; its absence is a drifted body, not an empty list.
fn container(body: &Value) -> Result<&Value, WatchlistError> {
    body.get("MediaContainer").filter(|c| c.is_object()).ok_or(WatchlistError::Unreadable)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn int(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn float(v: &Value, key: &str) -> Option<f32> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// One `Metadata` entry as a [`WatchItem`]. Shape drift costs the field; an entry whose id cannot
/// be recovered (no usable `ratingKey`, no `plex://` guid to take it from) is skipped because
/// nothing could act on it. `fallback_hex` is the id the caller asked for.
fn item_from(v: &Value, fallback_hex: Option<&str>) -> Option<WatchItem> {
    let kind = text(v, "type").map(|t| match t.as_str() {
        "movie" => WatchKind::Movie,
        "show" => WatchKind::Show,
        _ => WatchKind::Other,
    });
    let guid = text(v, "guid");
    let from_guid = guid.as_deref().and_then(|g| g.strip_prefix("plex://")).and_then(|r| r.rsplit('/').next());
    let hex = text(v, "ratingKey").filter(|h| is_hex_id(h))
        .or_else(|| from_guid.filter(|h| is_hex_id(h)).map(str::to_string))
        .or_else(|| fallback_hex.filter(|h| is_hex_id(h)).map(str::to_string))?;
    let guid = guid.filter(|g| g.starts_with("plex://")).or_else(|| match kind {
        Some(WatchKind::Movie) => Some(format!("plex://movie/{hex}")),
        Some(WatchKind::Show) => Some(format!("plex://show/{hex}")),
        _ => None,
    })?;
    Some(WatchItem {
        hex,
        guid,
        kind,
        title: text(v, "title"),
        title_sort: text(v, "titleSort"),
        year: int(v, "year").and_then(|y| i32::try_from(y).ok()),
        added_at: int(v, "watchlistedAt"),
        originally_available: text(v, "originallyAvailableAt"),
        rating: float(v, "rating"),
        summary: text(v, "summary"),
        thumb: text(v, "thumb"),
    })
}

/// A list body: the usable items, the raw entry count (paging follows the wire, not the filter)
/// and the container's `totalSize`.
fn list_from(response: &WatchResponse, start: u32) -> Result<(WatchPage, usize), WatchlistError> {
    let body = parse(response)?;
    let c = container(&body)?;
    let entries: &[Value] = c.get("Metadata").and_then(Value::as_array).map_or(&[], Vec::as_slice);
    let items = entries.iter().filter_map(|e| item_from(e, None)).collect();
    let total = int(c, "totalSize").and_then(|t| u32::try_from(t).ok());
    Ok((WatchPage { items, total, start, raw: u32::try_from(entries.len()).unwrap_or(u32::MAX) }, entries.len()))
}

impl AccountClient {
    /// The live transport for this client: libcurl HTTPS with the account headers and token.
    /// A `devtriggers` build REFUSES a `PUT` with [`WatchlistError::Refused`] unless the
    /// discover stand-in is armed.
    pub fn watchlist_transport(&self) -> impl FnMut(&WatchRequest) -> Result<WatchResponse, WatchlistError> + '_ {
        move |req: &WatchRequest| -> Result<WatchResponse, WatchlistError> {
            if let Err(e) = permit_write(req.method, cfg!(feature = "devtriggers"), stand_in_armed()) {
                plx_base::eventlog::log(&format!(
                    "watchlist: {} {} refused (dev build, discover stand-in not armed)",
                    req.method, endpoint_shape(&req.url)));
                return Err(e);
            }
            let mut headers = self.headers();
            if let Some(tag) = &req.if_none_match {
                headers.push(format!("If-None-Match: {tag}"));
            }
            let body = (req.method == "PUT").then_some(b"".as_slice());
            let response = plx_net::net::request_evidence(&req.url, &headers, req.method, body,
                plx_net::net::API, false, None, None);
            note_response_contact(&req.url, &response);
            match response {
                Ok(r) => Ok(WatchResponse { status: r.status, body: r.body }),
                // A refusal whose body did not finish is still an authoritative status; a 2xx
                // that did not finish is not an answer.
                Err(f) => match f.status {
                    Some(status) if !(200..300).contains(&status) => Ok(WatchResponse { status, body: Vec::new() }),
                    _ => Err(WatchlistError::Transport),
                },
            }
        }
    }

    /// One page of the default-order list (`start` 0-based, `size` clamped to [`MAX_PAGE`]).
    pub fn watchlist_page(&self, start: u32, size: u32, req: WatchTransport) -> Result<WatchPage, WatchlistError> {
        self.list_page("all", start, size, req).map(|(page, _)| page)
    }

    /// The `released` variant of the list (items already released), all pages folded in.
    pub fn watchlist_released(&self, req: WatchTransport) -> Result<Vec<WatchItem>, WatchlistError> {
        let mut out = Vec::new();
        let mut start = 0;
        for _ in 0..MAX_RELEASED_PAGES {
            let (page, raw) = self.list_page("released", start, MAX_PAGE, &mut *req)?;
            let total = page.total;
            out.extend(page.items);
            start += raw as u32;
            if raw == 0 || raw < MAX_PAGE as usize || total.is_some_and(|t| start >= t) {
                break;
            }
        }
        Ok(out)
    }

    /// `userState` for one item; accepts the array form (discover) or the object form.
    pub fn user_state(&self, hex: &str, req: WatchTransport) -> Result<UserState, WatchlistError> {
        if !is_hex_id(hex) {
            return Err(WatchlistError::BadRequest);
        }
        let response = call(req, "GET", format!("{}/library/metadata/{hex}/userState", discover()))?;
        let body = parse(&response)?;
        let state = container(&body)?.get("UserState").ok_or(WatchlistError::Unreadable)?;
        let state = match state {
            Value::Array(a) => a.first(),
            Value::Object(_) => Some(state),
            _ => return Err(WatchlistError::Unreadable),
        };
        Ok(UserState { watchlisted_at: state.and_then(|s| int(s, "watchlistedAt")) })
    }

    /// A Discover-only item's metadata, for the catalog page.
    pub fn catalog_item(&self, hex: &str, req: WatchTransport) -> Result<CatalogItem, WatchlistError> {
        if !is_hex_id(hex) {
            return Err(WatchlistError::BadRequest);
        }
        let response = call(req, "GET", format!("{}/library/metadata/{hex}", discover()))?;
        let body = parse(&response)?;
        let entry = container(&body)?.get("Metadata").and_then(Value::as_array).and_then(|a| a.first())
            .ok_or(WatchlistError::Unreadable)?;
        let item = item_from(entry, Some(hex)).ok_or(WatchlistError::Unreadable)?;
        let genres = entry.get("Genre").and_then(Value::as_array)
            .map(|g| g.iter().filter_map(|t| text(t, "tag")).collect()).unwrap_or_default();
        Ok(CatalogItem { item, genres, content_rating: text(entry, "contentRating"), duration_ms: int(entry, "duration") })
    }

    /// `PUT /actions/addToWatchlist?ratingKey=<hex>`.
    pub fn watchlist_add(&self, hex: &str, req: WatchTransport) -> Result<(), WatchlistError> {
        self.write_action("addToWatchlist", hex, req)
    }

    /// `PUT /actions/removeFromWatchlist?ratingKey=<hex>`.
    pub fn watchlist_remove(&self, hex: &str, req: WatchTransport) -> Result<(), WatchlistError> {
        self.write_action("removeFromWatchlist", hex, req)
    }

    fn write_action(&self, action: &str, hex: &str, req: WatchTransport) -> Result<(), WatchlistError> {
        if !is_hex_id(hex) {
            return Err(WatchlistError::BadRequest);
        }
        call(req, "PUT", format!("{}/actions/{action}?ratingKey={hex}", discover())).map(|_| ())
    }

    fn list_page(&self, variant: &str, start: u32, size: u32, req: WatchTransport)
        -> Result<(WatchPage, usize), WatchlistError>
    {
        let size = size.clamp(1, MAX_PAGE);
        let order = if variant == "all" { "&sort=watchlistedAt:desc" } else { "" };
        let path = format!(
            "/library/sections/watchlist/{variant}?includeCollections=1&includeExternalMedia=1{order}");
        let url = format!("{}{}", discover(),
            paged_path(&path, PageReq { start: start as usize, size: size as usize }));
        list_from(&call(req, "GET", url)?, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "5d776824999c4a001f9a1b2c";
    const HEX2: &str = "5d776824999c4a001f9a1b2d";

    fn client() -> AccountClient {
        AccountClient::new("test-client", Some("TOKEN-SECRET-123"))
    }

    fn ok(body: &str) -> Result<WatchResponse, WatchlistError> {
        Ok(WatchResponse { status: 200, body: body.as_bytes().to_vec() })
    }

    fn status(code: u16) -> Result<WatchResponse, WatchlistError> {
        Ok(WatchResponse { status: code, body: b"{}".to_vec() })
    }

    /// Run `f` against a closure that answers `replies` in order and records every request.
    fn run<T>(replies: Vec<Result<WatchResponse, WatchlistError>>,
        f: impl FnOnce(&AccountClient, WatchTransport) -> T) -> (T, Vec<WatchRequest>)
    {
        let mut seen = Vec::new();
        let mut queue = replies.into_iter();
        let out = {
            let mut transport = |r: &WatchRequest| {
                seen.push(r.clone());
                queue.next().expect("more requests than canned replies")
            };
            f(&client(), &mut transport)
        };
        (out, seen)
    }

    fn list_body(hexes: &[&str], total: u32) -> String {
        let items: Vec<String> = hexes.iter().map(|h| format!(
            r#"{{"ratingKey":"{h}","guid":"plex://movie/{h}","type":"movie","title":"T {h}","year":1999,"watchlistedAt":1700000000}}"#)).collect();
        format!(r#"{{"MediaContainer":{{"size":{},"totalSize":{total},"Metadata":[{}]}}}}"#, hexes.len(), items.join(","))
    }

    #[test]
    fn the_page_url_carries_the_paging_and_sort_params() {
        let (page, seen) = run(vec![ok(&list_body(&[HEX], 1))], |c, t| c.watchlist_page(200, 100, t));
        let page = page.unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].url, format!(
            "{}/library/sections/watchlist/all?includeCollections=1&includeExternalMedia=1&sort=watchlistedAt:desc&X-Plex-Container-Start=200&X-Plex-Container-Size=100",
            discover()));
        assert_eq!(seen[0].if_none_match, None);
        assert_eq!(page.start, 200);
        assert_eq!(page.total, Some(1));
        assert_eq!(page.items[0].hex, HEX);
        assert_eq!(page.items[0].added_at, Some(1_700_000_000));
        assert_eq!(page.items[0].year, Some(1999));
        assert_eq!(page.items[0].kind, Some(WatchKind::Movie));
    }

    #[test]
    fn the_raw_count_counts_entries_the_decoder_dropped() {
        // 100 entries, one without any usable id, no totalSize: paging must still see 100.
        let mut entries: Vec<String> = (1..100).map(|i| format!(
            r#"{{"ratingKey":"{i:024x}","guid":"plex://movie/{i:024x}","type":"movie","title":"T"}}"#)).collect();
        entries.push(r#"{"type":"movie","title":"No id"}"#.to_owned());
        let body = format!(r#"{{"MediaContainer":{{"size":100,"Metadata":[{}]}}}}"#, entries.join(","));
        let (page, _) = run(vec![ok(&body)], |c, t| c.watchlist_page(0, 100, t));
        let page = page.unwrap();
        assert_eq!(page.total, None);
        assert_eq!(page.items.len(), 99);
        assert_eq!(page.raw, 100);
    }

    #[test]
    fn a_page_size_over_the_limit_is_clamped_never_sent() {
        let (_, seen) = run(vec![ok(&list_body(&[], 0))], |c, t| c.watchlist_page(0, 500, t));
        assert!(seen[0].url.ends_with("X-Plex-Container-Size=100"), "{}", seen[0].url);
        let (_, seen) = run(vec![ok(&list_body(&[], 0))], |c, t| c.watchlist_page(0, 0, t));
        assert!(seen[0].url.ends_with("X-Plex-Container-Size=1"), "{}", seen[0].url);
    }

    #[test]
    fn the_released_list_folds_pages_and_stops_on_a_short_one() {
        let full: Vec<String> = (0..100).map(|i| format!("{i:024x}")).collect();
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        let (out, seen) = run(vec![ok(&list_body(&refs, 101)), ok(&list_body(&[HEX], 101))],
            |c, t| c.watchlist_released(t));
        assert_eq!(out.unwrap().len(), 101);
        assert_eq!(seen.len(), 2);
        assert!(seen[0].url.contains("/library/sections/watchlist/released?"), "{}", seen[0].url);
        assert!(!seen[0].url.contains("sort="), "released is read unsorted: {}", seen[0].url);
        assert!(seen[0].url.contains("X-Plex-Container-Start=0&X-Plex-Container-Size=100"));
        assert!(seen[1].url.contains("X-Plex-Container-Start=100&X-Plex-Container-Size=100"));
    }

    #[test]
    fn the_released_list_stops_at_the_page_cap() {
        let full: Vec<String> = (0..100).map(|i| format!("{i:024x}")).collect();
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        let body = list_body(&refs, 100_000);
        let replies = (0..30).map(|_| ok(&body)).collect();
        let (out, seen) = run(replies, |c, t| c.watchlist_released(t));
        assert!(out.is_ok());
        assert_eq!(seen.len(), MAX_RELEASED_PAGES as usize);
    }

    #[test]
    fn a_released_failure_on_a_later_page_fails_the_whole_read() {
        let full: Vec<String> = (0..100).map(|i| format!("{i:024x}")).collect();
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        let (out, _) = run(vec![ok(&list_body(&refs, 150)), status(401)], |c, t| c.watchlist_released(t));
        assert_eq!(out, Err(WatchlistError::Unauthorized));
    }

    #[test]
    fn each_status_maps_to_its_typed_error() {
        for (code, want) in [(304, WatchlistError::NotModified), (400, WatchlistError::BadRequest),
            (401, WatchlistError::Unauthorized), (404, WatchlistError::NotFound),
            (503, WatchlistError::Other(503)), (403, WatchlistError::Other(403))]
        {
            let (out, _) = run(vec![status(code)], |c, t| c.watchlist_page(0, 100, t));
            assert_eq!(out, Err(want), "status {code}");
        }
    }

    #[test]
    fn a_200_is_read_and_a_transport_error_passes_through() {
        let (out, _) = run(vec![ok(&list_body(&[HEX, HEX2], 2))], |c, t| c.watchlist_page(0, 100, t));
        assert_eq!(out.unwrap().items.len(), 2);
        let (out, _) = run(vec![Err(WatchlistError::Transport)], |c, t| c.watchlist_page(0, 100, t));
        assert_eq!(out, Err(WatchlistError::Transport));
    }

    #[test]
    fn a_body_that_is_not_the_container_is_unreadable_not_empty() {
        for body in ["not json", "{}", r#"{"MediaContainer":[]}"#] {
            let (out, _) = run(vec![ok(body)], |c, t| c.watchlist_page(0, 100, t));
            assert_eq!(out, Err(WatchlistError::Unreadable), "{body}");
        }
        let (out, _) = run(vec![ok(r#"{"MediaContainer":{"size":0}}"#)], |c, t| c.watchlist_page(0, 100, t));
        assert_eq!(out.unwrap().items, Vec::new());
    }

    #[test]
    fn drifted_fields_cost_the_field_not_the_page() {
        let body = format!(r#"{{"MediaContainer":{{"totalSize":"3","Metadata":[
            {{"ratingKey":"{HEX}","guid":"plex://show/{HEX}","type":"show","title":7,"year":"2001","rating":"7.5","extra":{{"x":1}}}},
            {{"ratingKey":"12","title":"numeric key, no guid"}},
            {{"guid":"plex://movie/{HEX2}","type":"movie","year":null}},
            {{"ratingKey":"{HEX2}","type":"collection"}}
        ]}}}}"#);
        let (out, _) = run(vec![ok(&body)], |c, t| c.watchlist_page(0, 100, t));
        let page = out.unwrap();
        assert_eq!(page.total, Some(3));
        assert_eq!(page.items.len(), 2, "{:?}", page.items);
        assert_eq!(page.items[0].kind, Some(WatchKind::Show));
        assert_eq!(page.items[0].title, None);
        assert_eq!(page.items[0].year, Some(2001));
        assert_eq!(page.items[0].rating, Some(7.5));
        assert_eq!(page.items[1].hex, HEX2);
        assert_eq!(page.items[1].guid, format!("plex://movie/{HEX2}"));
        assert_eq!(page.items[1].year, None);
    }

    #[test]
    fn user_state_reads_the_array_form_and_the_object_form() {
        let array = format!(r#"{{"MediaContainer":{{"UserState":[{{"ratingKey":"{HEX}","type":"movie","viewCount":0,"watchlistedAt":1700000123}}]}}}}"#);
        let object = r#"{"MediaContainer":{"UserState":{"viewCount":1,"watchlistedAt":1700000456}}}"#;
        let bare = r#"{"MediaContainer":{"UserState":[{"viewCount":0}]}}"#;
        let empty = r#"{"MediaContainer":{"UserState":[]}}"#;
        for (body, want) in [(array.as_str(), Some(1_700_000_123)), (object, Some(1_700_000_456)), (bare, None), (empty, None)] {
            let (out, seen) = run(vec![ok(body)], |c, t| c.user_state(HEX, t));
            assert_eq!(out.unwrap().watchlisted_at, want, "{body}");
            assert_eq!(seen[0].method, "GET");
            assert_eq!(seen[0].url, format!("{}/library/metadata/{HEX}/userState", discover()));
        }
        let (out, _) = run(vec![ok(r#"{"MediaContainer":{}}"#)], |c, t| c.user_state(HEX, t));
        assert_eq!(out, Err(WatchlistError::Unreadable));
        let (out, _) = run(vec![status(404)], |c, t| c.user_state(HEX, t));
        assert_eq!(out, Err(WatchlistError::NotFound));
    }

    #[test]
    fn catalog_item_reads_the_item_and_its_extras() {
        let body = format!(r#"{{"MediaContainer":{{"Metadata":[{{"ratingKey":"{HEX}","guid":"plex://movie/{HEX}","type":"movie",
            "title":"A","contentRating":"PG","duration":5400000,"Genre":[{{"tag":"Drama"}},{{"tag":"Comedy"}},{{"id":3}}]}}]}}}}"#);
        let (out, seen) = run(vec![ok(&body)], |c, t| c.catalog_item(HEX, t));
        let item = out.unwrap();
        assert_eq!(seen[0].url, format!("{}/library/metadata/{HEX}", discover()));
        assert_eq!(item.item.hex, HEX);
        assert_eq!(item.genres, vec!["Drama", "Comedy"]);
        assert_eq!(item.content_rating.as_deref(), Some("PG"));
        assert_eq!(item.duration_ms, Some(5_400_000));
        let (out, _) = run(vec![ok(r#"{"MediaContainer":{"size":0}}"#)], |c, t| c.catalog_item(HEX, t));
        assert_eq!(out, Err(WatchlistError::Unreadable));
    }

    #[test]
    fn add_and_remove_are_puts_to_their_action_urls() {
        let (out, seen) = run(vec![ok(r#"{"MediaContainer":{"size":0}}"#)], |c, t| c.watchlist_add(HEX, t));
        assert_eq!(out, Ok(()));
        assert_eq!((seen[0].method, seen[0].url.as_str()), ("PUT",
            format!("{}/actions/addToWatchlist?ratingKey={HEX}", discover()).as_str()));
        let (out, seen) = run(vec![ok(r#"{"MediaContainer":{"size":0}}"#)], |c, t| c.watchlist_remove(HEX, t));
        assert_eq!(out, Ok(()));
        assert_eq!((seen[0].method, seen[0].url.as_str()), ("PUT",
            format!("{}/actions/removeFromWatchlist?ratingKey={HEX}", discover()).as_str()));
        let (out, _) = run(vec![status(401)], |c, t| c.watchlist_add(HEX, t));
        assert_eq!(out, Err(WatchlistError::Unauthorized));
        let (out, _) = run(vec![status(404)], |c, t| c.watchlist_remove(HEX, t));
        assert_eq!(out, Err(WatchlistError::NotFound));
    }

    #[test]
    fn an_id_that_is_not_a_catalog_hex_never_reaches_the_transport() {
        for bad in ["", "12", "5d776824999c4a001f9a1b2", "5d776824999c4a001f9a1b2cc", "zd776824999c4a001f9a1b2c",
            "5d776824999c4a001f9a1b2c&x=1"]
        {
            let (a, seen) = run(vec![], |c, t| c.watchlist_add(bad, t));
            let (b, _) = run(vec![], |c, t| c.watchlist_remove(bad, t));
            let (c_, _) = run(vec![], |c, t| c.user_state(bad, t));
            let (d, _) = run(vec![], |c, t| c.catalog_item(bad, t));
            assert_eq!([a.err(), b.err(), c_.err(), d.err()], [const { Some(WatchlistError::BadRequest) }; 4], "{bad}");
            assert!(seen.is_empty());
        }
    }

    #[test]
    fn a_dev_build_refuses_a_write_unless_the_stand_in_is_armed() {
        assert_eq!(permit_write("PUT", true, false), Err(WatchlistError::Refused));
        assert_eq!(permit_write("PUT", true, true), Ok(()));
        assert_eq!(permit_write("PUT", false, false), Ok(()), "release builds write");
        assert_eq!(permit_write("GET", true, false), Ok(()), "reads are never refused");
        // The refusal travels up as the write's own error.
        let (out, _) = run(vec![Err(WatchlistError::Refused)], |c, t| c.watchlist_add(HEX, t));
        assert_eq!(out, Err(WatchlistError::Refused));
    }

    #[test]
    fn the_base_is_the_real_service_unless_a_dev_build_armed_a_loopback() {
        // No trigger exists in the test root, so the real base is in force and no write is armed.
        assert_eq!(discover(), DISCOVER);
        assert!(!stand_in_armed());
    }

    #[test]
    fn a_request_and_a_failure_line_carry_no_token_id_or_query() {
        // The request type has no credential field, so a closure cannot see (or log) the token.
        let client = client();
        let req = WatchRequest { method: "PUT", url: format!("{}/actions/addToWatchlist?ratingKey={HEX}", discover()),
            if_none_match: None };
        assert!(!format!("{req:?}").contains("TOKEN-SECRET-123"));
        // The token lives in the client's headers, which the log lines never format.
        assert!(client.headers().iter().any(|h| h.contains("TOKEN-SECRET-123")));
        for line in [failure_line(req.method, &req.url, Ok(401)), failure_line(req.method, &req.url, Err(()))] {
            assert!(!line.contains("TOKEN-SECRET-123"), "{line}");
            assert!(!line.contains(HEX), "{line}");
            assert!(!line.contains("ratingKey"), "{line}");
            assert!(line.contains("addToWatchlist"), "{line}");
        }
        assert_eq!(failure_line("PUT", &req.url, Ok(401)),
            "watchlist: PUT discover.provider.plex.tv/actions/addToWatchlist -> HTTP 401");
        let page = failure_line("GET", &format!("{}/library/metadata/{HEX}/userState?X-Plex-Token=abc", discover()), Ok(404));
        assert_eq!(page, "watchlist: GET discover.provider.plex.tv/library/metadata/{id}/userState -> HTTP 404");
    }
}
