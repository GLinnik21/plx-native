//! A Seerr server (Overseerr / Jellyseerr / Seerr: one `/api/v1`), the household's own request
//! service (docs/seerr.md). Nothing here is plex.tv or a PMS: the base URL is the one the owner typed
//! in Settings, and the identity is a Seerr SESSION the server hands out for the seated profile's
//! plex.tv token (`POST /api/v1/auth/plex`), sent back as its cookie on every later call.
//!
//! The shape follows the Watchlist client (`watchlist.rs`): every call is blocking (run it on a
//! worker), takes an injectable transport closure (tests pass a canned one, production
//! [`live_transport`]), and logs only the method, the PATH shape and the status — never the host
//! (it is a household address), a query (a search term), a title, the token or the cookie.
//!
//! **The dev-build failsafe.** A `devtriggers` build refuses `POST /api/v1/request` — the one call
//! that starts a download on somebody's server — unless the loopback stand-in is armed
//! (`plxnative-seerr=http://127.0.0.1:<port>`, `tests/mock_pms.py`) or the owner created
//! `plxnative-seerr-live` (`.claude/hooks/watchlist-live-guard.py` refuses a command that would).
//! Release builds always send.
//!
//! Wire facts (Overseerr's `overseerr-api.yml`, which Jellyseerr and Seerr keep): lists answer
//! `{page, totalPages, results}` with `mediaType` `movie`/`tv`/`person` rows (a title is `title` on
//! a movie and `name` on a show); a title's availability is `mediaInfo.status`, 1 unknown, 2
//! pending, 3 processing, 4 partly available, 5 available (6 blocklisted and 7 deleted are the newer
//! servers'); a request is 1 pending approval, 2 approved, 3 declined. A missing `mediaInfo` is a
//! title nobody has asked for.
//!
//! A request can name where it goes (`serverId`, `profileId`, `rootFolder`): the Radarr / Sonarr
//! servers the Seerr server sends to are read with `GET /api/v1/service/{radarr|sonarr}` and one
//! server's quality profiles and root folders with `GET /api/v1/service/{radarr|sonarr}/{id}`
//! (Seerr's `server/routes/service.ts`). Seerr's own request dialog offers that choice only to a user
//! with Manage Requests or Advanced Requests ([`SeerrUser::may_choose_options`]); for anyone else the
//! server applies its override rules over whatever the body names.
use super::account::{endpoint_shape, loopback_http};
use super::client::QueryBuilder;
use serde_json::{json, Value};

/// How many rows one request page asks for (the request list's `take`).
pub const REQUEST_PAGE: u32 = 20;

/// What kind of title a Seerr row is. People rows (`mediaType: person`) are dropped on read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Movie,
    Tv,
}

impl MediaKind {
    fn wire(self) -> &'static str {
        match self {
            MediaKind::Movie => "movie",
            MediaKind::Tv => "tv",
        }
    }
    /// The service a request of this kind goes to: Radarr for a movie, Sonarr for a show.
    fn service(self) -> &'static str {
        match self {
            MediaKind::Movie => "radarr",
            MediaKind::Tv => "sonarr",
        }
    }
    fn of_wire(s: &str) -> Option<Self> {
        match s {
            "movie" => Some(MediaKind::Movie),
            "tv" => Some(MediaKind::Tv),
            _ => None,
        }
    }
}

/// A title's (or one season's) availability, Seerr's `MediaStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Availability {
    /// Nobody has asked for it (no `mediaInfo`, status 1), or it was deleted (7): requestable.
    #[default]
    Unknown,
    /// Asked for, waiting for an approval (2).
    Pending,
    /// Approved and on its way (3).
    Processing,
    /// Some of it is on the server (4): a show with seasons still missing.
    PartlyAvailable,
    /// On the server (5).
    Available,
    /// The server's owner blocked it (6): not requestable.
    Blocked,
}

impl Availability {
    fn of_code(code: i64) -> Self {
        match code {
            2 => Availability::Pending,
            3 => Availability::Processing,
            4 => Availability::PartlyAvailable,
            5 => Availability::Available,
            6 => Availability::Blocked,
            _ => Availability::Unknown,
        }
    }
    /// Can a request for this title (or season) still be made?
    pub fn requestable(self) -> bool {
        matches!(self, Availability::Unknown | Availability::PartlyAvailable)
    }
}

/// A request's own state, Seerr's `MediaRequestStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RequestStatus {
    PendingApproval,
    Approved,
    Declined,
    /// Any later code (Seerr's failed / completed): shown by the title's availability instead.
    Other,
}

impl RequestStatus {
    fn of_code(code: i64) -> Self {
        match code {
            1 => RequestStatus::PendingApproval,
            2 => RequestStatus::Approved,
            3 => RequestStatus::Declined,
            _ => RequestStatus::Other,
        }
    }
}

/// One title as a list row carries it. Every field but `kind` and `tmdb` tolerates absence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Title {
    pub kind: Option<MediaKind>,
    /// The TMDB id: the id `movie`/`tv`/`request` take.
    pub tmdb: i64,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    /// TMDB image PATHS (`/abc.jpg`); [`tmdb_image`] makes a URL of one.
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub status: Availability,
}

/// One season of a show, with what the server holds or was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Season {
    pub number: i32,
    pub episodes: Option<i32>,
    pub status: Availability,
}

/// `GET /api/v1/movie/{id}` or `/tv/{id}`: the title, a show's seasons (specials, season 0, are
/// left out: Seerr does not request them by default) and whether a request of it is still open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TitleDetail {
    pub title: Title,
    pub seasons: Vec<Season>,
    /// A show TMDB files under the "anime" keyword ([`ANIME_KEYWORD`]): Seerr sends it to a Sonarr
    /// server's anime profile and folder unless the request names others ([`ArrServer::defaults`]).
    pub anime: bool,
}

/// TMDB's "anime" keyword, the one Seerr reads a show's anime-ness from (`ANIME_KEYWORD_ID`).
pub const ANIME_KEYWORD: i64 = 210024;

impl TitleDetail {
    /// The season numbers a request could still ask for.
    pub fn requestable_seasons(&self) -> Vec<i32> {
        self.seasons.iter().filter(|s| s.status == Availability::Unknown).map(|s| s.number).collect()
    }
}

/// One page of a list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// 1-based, as the server counts.
    pub page: u32,
    pub pages: u32,
}

/// One of the signed-in user's requests. The list does not carry the title's words: a row is drawn
/// from a [`TitleDetail`] read by `kind` + `tmdb`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MyRequest {
    pub id: i64,
    pub kind: MediaKind,
    pub tmdb: i64,
    pub status: RequestStatus,
    pub availability: Availability,
    pub seasons: Vec<i32>,
}

/// The Seerr user the session speaks for.
#[derive(Clone, PartialEq, Eq)]
pub struct SeerrUser {
    pub id: i64,
    /// `displayName`, else the Plex user name. Shown in Settings only, never logged.
    pub name: String,
    /// Seerr's permission bits (`server/lib/permissions.ts`); 0 when the server sent none.
    pub permissions: u64,
}

/// Seerr's `Permission.ADMIN`: every permission.
pub const PERMISSION_ADMIN: u64 = 2;
/// Seerr's `Permission.MANAGE_REQUESTS`.
pub const PERMISSION_MANAGE_REQUESTS: u64 = 16;
/// Seerr's `Permission.REQUEST_ADVANCED`: may pick a request's server, quality profile and folder.
pub const PERMISSION_REQUEST_ADVANCED: u64 = 8192;

impl SeerrUser {
    /// May this user choose a request's server, quality profile and root folder? Seerr's own
    /// dialog shows the choice to the same users (admin, Manage Requests, Advanced Requests), and its
    /// server keeps their choices over its override rules.
    pub fn may_choose_options(&self) -> bool {
        self.permissions & (PERMISSION_ADMIN | PERMISSION_MANAGE_REQUESTS | PERMISSION_REQUEST_ADVANCED) != 0
    }
}

impl std::fmt::Debug for SeerrUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeerrUser").field("id", &self.id).finish_non_exhaustive()
    }
}

/// A signed-in session: the cookies `auth/plex` set, joined as one `Cookie:` value. A credential:
/// its `Debug` says only whether it is there.
#[derive(Clone, PartialEq, Eq)]
pub struct SeerrSession {
    cookie: String,
}

impl SeerrSession {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(cookie: &str) -> Self {
        Self { cookie: cookie.to_string() }
    }
}

impl std::fmt::Debug for SeerrSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SeerrSession(<cookie>)")
    }
}

/// One Radarr (movies) or Sonarr (shows) server the Seerr server sends requests to, from
/// `GET /api/v1/service/{radarr|sonarr}`: its name and the profile and folder a request gets when it
/// names none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArrServer {
    pub id: i64,
    /// The name the Seerr owner gave it (server text).
    pub name: String,
    /// A 4K server: the request this client sends is never a 4K one, so these are not offered.
    pub is4k: bool,
    pub is_default: bool,
    /// `activeProfileId` and `activeDirectory`: the server's defaults.
    pub profile: Option<i64>,
    pub folder: Option<String>,
    /// A Sonarr server's `activeAnimeProfileId` and `activeAnimeDirectory`, for an anime show.
    pub anime_profile: Option<i64>,
    pub anime_folder: Option<String>,
}

impl ArrServer {
    /// The profile and folder Seerr gives a request that names neither: a Sonarr server's anime
    /// pair for an anime show when it has one, else its ordinary pair.
    pub fn defaults(&self, anime: bool) -> (Option<i64>, Option<&str>) {
        let profile = if anime { self.anime_profile.or(self.profile) } else { self.profile };
        let folder = if anime { self.anime_folder.as_deref().or(self.folder.as_deref()) } else { self.folder.as_deref() };
        (profile, folder)
    }
}

/// The server a request goes to when it names none: the non-4K server marked default, else the
/// first non-4K one (Seerr's own order).
pub fn default_server(servers: &[ArrServer]) -> Option<&ArrServer> {
    servers.iter().filter(|s| !s.is4k).find(|s| s.is_default).or_else(|| servers.iter().find(|s| !s.is4k))
}

/// One quality profile of a Radarr / Sonarr server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityProfile {
    pub id: i64,
    /// The profile's name (server text).
    pub name: String,
}

/// One root folder of a Radarr / Sonarr server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootFolder {
    /// The folder's path on that server, what a request names (server text).
    pub path: String,
    /// Bytes free there, when the server said.
    pub free: Option<i64>,
}

/// `GET /api/v1/service/{radarr|sonarr}/{id}`: one server's quality profiles and root folders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArrOptions {
    pub profiles: Vec<QualityProfile>,
    pub folders: Vec<RootFolder>,
}

/// What a request names beyond the title: each `None` leaves that choice to the server (its default,
/// and for a user who may not choose, its override rules).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RequestOptions {
    pub server: Option<i64>,
    pub profile: Option<i64>,
    pub folder: Option<String>,
}

/// Which discover row to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiscoverRow {
    Trending,
    PopularMovies,
    PopularTv,
    UpcomingMovies,
    UpcomingTv,
}

impl DiscoverRow {
    fn path(self) -> &'static str {
        match self {
            DiscoverRow::Trending => "/api/v1/discover/trending",
            DiscoverRow::PopularMovies => "/api/v1/discover/movies",
            DiscoverRow::PopularTv => "/api/v1/discover/tv",
            DiscoverRow::UpcomingMovies => "/api/v1/discover/movies/upcoming",
            DiscoverRow::UpcomingTv => "/api/v1/discover/tv/upcoming",
        }
    }
}

/// Why a Seerr call did not produce its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeerrError {
    /// The configured address is not an `http(s)://host[:port][/path]` URL.
    BadAddress,
    /// 401: the session is gone (expired, or the server restarted): sign in again.
    Unauthorized,
    /// 403: this Plex account may not use the server (not imported, Plex sign-in off), or may not
    /// make this request (no permission, over its quota).
    Forbidden,
    /// 404: no such title.
    NotFound,
    /// 409: the title was already requested.
    AlreadyRequested,
    /// No answer: offline, TLS, timeout, a redirect (never followed: the cookie must not travel).
    Transport,
    /// A `devtriggers` build refused a request write (module doc).
    Refused,
    /// A 2xx whose body this module could not read, or a sign-in that set no session cookie.
    Unreadable,
    /// Any other status.
    Other(u16),
}

/// A request as the transport sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeerrRequest {
    /// `"GET"` or `"POST"`.
    pub method: &'static str,
    /// Absolute URL on the resolved base ([`base_for`]).
    pub url: String,
    /// The `Cookie:` value, on every call but the sign-in.
    pub cookie: Option<String>,
    /// A JSON body (`POST` only).
    pub body: Option<Vec<u8>>,
}

/// A transport answer: status, raw body and the cookies it set (`net::Resp::cookies`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeerrResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub cookies: Vec<String>,
}

/// The injectable request closure every call takes.
pub type SeerrTransport<'a> = &'a mut dyn FnMut(&SeerrRequest) -> Result<SeerrResponse, SeerrError>;

/// A TMDB image URL for a path a row carries, at a width TMDB serves (`w342` for a poster card,
/// `w1280` for a backdrop). The app draws it through a PMS photo transcoder, as it draws a Discover
/// headshot (`discover.rs`), so this process never dials the image host itself.
pub fn tmdb_image(path: &str, width: &str) -> Option<String> {
    let ok = path.starts_with('/') && path.len() > 1
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'));
    ok.then(|| format!("https://image.tmdb.org/t/p/{width}{path}"))
}

/// The address the owner typed, made a base URL: `http://` when no scheme was typed, `http` or
/// `https` only, a host, an optional port and path prefix (a reverse proxy's `/seerr`), no user
/// info, query or fragment, no trailing `/` and no trailing `/api/v1` (pasted from the API docs).
/// `None` for anything else, so a bad address never reaches a request.
pub fn normalize_address(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() || typed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = typed.to_ascii_lowercase();
    let (scheme, rest) = if lower.starts_with("https://") {
        ("https", &typed[8..])
    } else if lower.starts_with("http://") {
        ("http", &typed[7..])
    } else if typed.contains("://") {
        return None;
    } else {
        ("http", typed)
    };
    if rest.contains(['?', '#', '@', '\\']) {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // `[v6]` or `[v6]:port`, else `host` or `host:port`.
    let (host_ok, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (addr, after) = v6.split_once(']')?;
            let port = if after.is_empty() { None } else { Some(after.strip_prefix(':')?) };
            (!addr.is_empty() && addr.bytes().all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.')), port)
        }
        None => {
            let (host, port) = match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            (!host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')), port)
        }
    };
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
        && p.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p)));
    if !host_ok || !port_ok {
        return None;
    }
    let mut path = path.trim_end_matches('/');
    if path.to_ascii_lowercase().ends_with("/api/v1") {
        path = path[..path.len() - "/api/v1".len()].trim_end_matches('/');
    }
    Some(format!("{scheme}://{authority}{path}"))
}

/// The base every call is built on: the dev-build loopback stand-in when one is armed
/// (`plxnative-seerr`, never anything but `http://127.0.0.1|localhost:<port>`: the trigger must not
/// be able to send the plex.tv token anywhere else), else the owner's address, normalized.
pub fn base_for(configured: &str) -> Option<String> {
    resolve_base(plx_base::devtrig::read("seerr").as_deref(), configured)
}

fn resolve_base(trigger: Option<&str>, configured: &str) -> Option<String> {
    match trigger {
        Some(v) if loopback_http(v) => Some(v.trim_end_matches('/').to_string()),
        _ => normalize_address(configured),
    }
}

/// Is the loopback stand-in armed? Read where it is asked — on a worker, once per call, the same
/// read [`base_for`] makes — so the answer and the base a call is built on cannot disagree. A
/// release build has no triggers (`devtrig::read` is `None` there) and reads nothing.
pub fn stand_in_armed() -> bool {
    plx_base::devtrig::read("seerr").is_some_and(|v| loopback_http(&v))
}

#[cfg(feature = "devtriggers")]
plx_base::devtrig::latched_flag!(
    /// **The owner's live switch** (`plxnative-seerr-live` in the install's runtime root): a
    /// `devtriggers` build sends request writes to the owner's real Seerr while it exists. Read once
    /// at boot ([`announce`]). **No agent creates this file unless the owner asks for it in that
    /// session** (AGENTS.md; `.claude/hooks/watchlist-live-guard.py` refuses a command that would).
    fn live_armed = "seerr-live";
);
#[cfg(not(feature = "devtriggers"))]
fn live_armed() -> bool {
    false
}

/// BOOT: log once which Seerr this build may write to, so an event log says it before any request.
/// Silent in a release build; the trigger names stay out of a release binary (`ci/check-package.py`).
pub fn announce() {
    #[cfg(feature = "devtriggers")]
    {
        if plx_base::devtrig::read("seerr").is_some_and(|v| !loopback_http(&v)) {
            plx_base::eventlog::log("BADTRIGGER seerr: only http://127.0.0.1:<port> or http://localhost:<port> is accepted");
        }
        let base = if stand_in_armed() { "stand-in" } else { "configured" };
        let writes = if stand_in_armed() || live_armed() { "open" } else { "refused" };
        plx_base::eventlog::log(&format!("seerr: base={base} writes={writes}"));
    }
}

/// Does this request start something on the server? Only `POST .../api/v1/request`; the sign-in
/// is a POST too, but it changes nothing anybody can see.
fn is_write(method: &str, url: &str) -> bool {
    method == "POST" && url.split('?').next().is_some_and(|p| p.ends_with("/api/v1/request"))
}

/// The dev-build failsafe (module doc).
fn permit_write(method: &str, url: &str, devtriggers_build: bool, armed: bool, live: bool) -> Result<(), SeerrError> {
    if is_write(method, url) && devtriggers_build && !armed && !live {
        return Err(SeerrError::Refused);
    }
    Ok(())
}

/// The PATH shape of a URL for a log line: ids folded, query dropped, and — unlike plex.tv's — the
/// host dropped too, because a Seerr host is the household's own address.
fn path_shape(url: &str) -> String {
    let shape = endpoint_shape(url);
    match shape.find('/') {
        Some(i) => shape[i..].to_string(),
        None => "/".to_string(),
    }
}

fn line(method: &str, url: &str, outcome: Result<u16, ()>, live: bool) -> String {
    let tag = if live { "LIVE " } else { "" };
    let shape = path_shape(url);
    match outcome {
        Ok(status) => format!("seerr: {tag}{method} {shape} -> HTTP {status}"),
        Err(()) => format!("seerr: {tag}{method} {shape} -> no answer"),
    }
}

/// The live transport: libcurl, JSON in and out, the session cookie when there is one, redirects
/// never followed (a caller header must not ride a redirect, and a cookie must not reach another
/// host). A failure logs its shape; a write logs its shape whatever the answer.
pub fn live_transport() -> impl FnMut(&SeerrRequest) -> Result<SeerrResponse, SeerrError> {
    move |req: &SeerrRequest| -> Result<SeerrResponse, SeerrError> {
        let dev = cfg!(feature = "devtriggers");
        let (armed, live) = (stand_in_armed(), live_armed());
        if let Err(e) = permit_write(req.method, &req.url, dev, armed, live) {
            plx_base::eventlog::log(&format!(
                "seerr: {} {} refused (dev build: neither the Seerr stand-in nor the live switch is armed)",
                req.method, path_shape(&req.url)));
            return Err(e);
        }
        let mut headers = vec!["Accept: application/json".to_string()];
        if req.body.is_some() {
            headers.push("Content-Type: application/json".to_string());
        }
        if let Some(cookie) = &req.cookie {
            headers.push(format!("Cookie: {cookie}"));
        }
        let response = plx_net::net::request_evidence(&req.url, &headers, req.method, req.body.as_deref(),
            plx_net::net::API, false, None, None);
        let status = match &response {
            Ok(r) => Ok(r.status),
            Err(f) => f.status.ok_or(()),
        };
        let write = is_write(req.method, &req.url);
        if write || !status.is_ok_and(|s| (200..300).contains(&s)) {
            plx_base::eventlog::log(&line(req.method, &req.url, status, write && dev && live && !armed));
        }
        match response {
            Ok(r) => Ok(SeerrResponse { status: r.status, body: r.body, cookies: r.cookies }),
            Err(f) => match f.status {
                Some(status) if !(200..300).contains(&status) => Ok(SeerrResponse { status, ..Default::default() }),
                _ => Err(SeerrError::Transport),
            },
        }
    }
}

/// One call: 2xx yields the response, everything else its [`SeerrError`].
fn call(req: SeerrTransport, method: &'static str, url: String, session: Option<&SeerrSession>, body: Option<Value>)
    -> Result<SeerrResponse, SeerrError>
{
    let request = SeerrRequest {
        method,
        url,
        cookie: session.map(|s| s.cookie.clone()),
        body: body.map(|b| b.to_string().into_bytes()),
    };
    let response = req(&request)?;
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    Err(match response.status {
        401 => SeerrError::Unauthorized,
        403 => SeerrError::Forbidden,
        404 => SeerrError::NotFound,
        409 => SeerrError::AlreadyRequested,
        other => SeerrError::Other(other),
    })
}

fn parse(response: &SeerrResponse) -> Result<Value, SeerrError> {
    serde_json::from_slice(&response.body).map_err(|_| SeerrError::Unreadable)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn int(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn year_of(date: Option<String>) -> Option<i32> {
    date.and_then(|d| d.get(..4).and_then(|y| y.parse().ok()))
}

/// A list row or a detail record's title. `None` for a person, an unknown kind or a row with no id.
fn title_from(v: &Value, kind_hint: Option<MediaKind>) -> Option<Title> {
    let kind = text(v, "mediaType").and_then(|k| MediaKind::of_wire(&k)).or(kind_hint)?;
    if text(v, "mediaType").is_some_and(|k| MediaKind::of_wire(&k).is_none()) {
        return None;
    }
    let tmdb = int(v, "id").filter(|id| *id > 0)?;
    let (name, date) = match kind {
        MediaKind::Movie => (text(v, "title"), text(v, "releaseDate")),
        MediaKind::Tv => (text(v, "name"), text(v, "firstAirDate")),
    };
    Some(Title {
        kind: Some(kind),
        tmdb,
        title: name.or_else(|| text(v, "originalTitle")).or_else(|| text(v, "originalName")).unwrap_or_default(),
        year: year_of(date),
        overview: text(v, "overview"),
        poster: text(v, "posterPath"),
        backdrop: text(v, "backdropPath"),
        status: v.get("mediaInfo").and_then(|m| int(m, "status")).map_or(Availability::Unknown, Availability::of_code),
    })
}

fn page_from(v: &Value, kind_hint: Option<MediaKind>) -> Page<Title> {
    let items = v.get("results").and_then(Value::as_array).map_or(Vec::new(), |rows|
        rows.iter().filter_map(|r| title_from(r, kind_hint)).collect());
    let page = int(v, "page").and_then(|p| u32::try_from(p).ok()).unwrap_or(1);
    let pages = int(v, "totalPages").and_then(|p| u32::try_from(p).ok()).unwrap_or(page);
    Page { items, page, pages }
}

/// A show's seasons: what the server holds (`mediaInfo.seasons`) folded with what an open request
/// asked for (a pending request's seasons are Pending, an approved one's Processing); the stronger
/// state wins. Specials (season 0) are dropped.
fn seasons_from(v: &Value) -> Vec<Season> {
    let mut seasons: Vec<Season> = v.get("seasons").and_then(Value::as_array).map_or(Vec::new(), |rows|
        rows.iter().filter_map(|s| {
            let number = i32::try_from(int(s, "seasonNumber")?).ok().filter(|n| *n > 0)?;
            Some(Season { number, episodes: int(s, "episodeCount").and_then(|n| i32::try_from(n).ok()),
                status: Availability::Unknown })
        }).collect());
    let mut raise = |number: i64, status: Availability| {
        if let Some(season) = seasons.iter_mut().find(|s| i64::from(s.number) == number) {
            season.status = season.status.max(status);
        }
    };
    let info = v.get("mediaInfo");
    for s in info.and_then(|m| m.get("seasons")).and_then(Value::as_array).into_iter().flatten() {
        if let (Some(n), Some(code)) = (int(s, "seasonNumber"), int(s, "status")) {
            raise(n, Availability::of_code(code));
        }
    }
    for r in info.and_then(|m| m.get("requests")).and_then(Value::as_array).into_iter().flatten() {
        let status = match int(r, "status").map(RequestStatus::of_code) {
            Some(RequestStatus::PendingApproval) => Availability::Pending,
            Some(RequestStatus::Approved) => Availability::Processing,
            _ => continue,
        };
        for s in r.get("seasons").and_then(Value::as_array).into_iter().flatten() {
            if let Some(n) = int(s, "seasonNumber") {
                raise(n, status);
            }
        }
    }
    seasons.sort_by_key(|s| s.number);
    seasons
}

fn request_from(v: &Value) -> Option<MyRequest> {
    let media = v.get("media")?;
    let kind = text(media, "mediaType").and_then(|k| MediaKind::of_wire(&k))
        .or_else(|| text(v, "type").and_then(|k| MediaKind::of_wire(&k)))?;
    Some(MyRequest {
        id: int(v, "id")?,
        kind,
        tmdb: int(media, "tmdbId").filter(|id| *id > 0)?,
        status: int(v, "status").map_or(RequestStatus::Other, RequestStatus::of_code),
        availability: int(media, "status").map_or(Availability::Unknown, Availability::of_code),
        seasons: v.get("seasons").and_then(Value::as_array).map_or(Vec::new(), |rows|
            rows.iter().filter_map(|s| int(s, "seasonNumber").and_then(|n| i32::try_from(n).ok())).collect()),
    })
}

fn user_from(v: &Value) -> Option<SeerrUser> {
    let id = int(v, "id")?;
    let name = text(v, "displayName").or_else(|| text(v, "plexUsername")).or_else(|| text(v, "username"))
        .unwrap_or_default();
    let permissions = int(v, "permissions").and_then(|p| u64::try_from(p).ok()).unwrap_or(0);
    Some(SeerrUser { id, name, permissions })
}

/// Does a show's detail carry TMDB's anime keyword?
fn anime_from(v: &Value) -> bool {
    v.get("keywords").and_then(Value::as_array).is_some_and(|k| k.iter().any(|k| int(k, "id") == Some(ANIME_KEYWORD)))
}

fn server_from(v: &Value) -> Option<ArrServer> {
    Some(ArrServer {
        id: int(v, "id").filter(|id| *id >= 0)?,
        name: text(v, "name").unwrap_or_default(),
        is4k: v.get("is4k").and_then(Value::as_bool).unwrap_or(false),
        is_default: v.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
        profile: int(v, "activeProfileId"),
        folder: text(v, "activeDirectory"),
        anime_profile: int(v, "activeAnimeProfileId"),
        anime_folder: text(v, "activeAnimeDirectory"),
    })
}

fn options_from(v: &Value) -> ArrOptions {
    let rows = |key: &str| v.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    ArrOptions {
        profiles: rows("profiles").iter()
            .filter_map(|p| Some(QualityProfile { id: int(p, "id")?, name: text(p, "name")? })).collect(),
        folders: rows("rootFolders").iter()
            .filter_map(|f| Some(RootFolder { path: text(f, "path")?, free: int(f, "freeSpace") })).collect(),
    }
}

/// One Seerr server, by its base URL ([`base_for`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeerrClient {
    base: String,
}

impl SeerrClient {
    /// A client for the owner's configured address, or the stand-in. `None` for a bad address.
    pub fn for_address(configured: &str) -> Option<Self> {
        base_for(configured).map(|base| Self { base })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(base: &str) -> Self {
        Self { base: base.to_string() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// `POST /api/v1/auth/plex {"authToken"}`: sign in as the Plex account the token speaks for.
    /// A 403 is a Plex user this server does not let in; a 200 that set no cookie is unreadable.
    pub fn sign_in(&self, plex_token: &str, req: SeerrTransport) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        let response = call(req, "POST", self.url("/api/v1/auth/plex"), None, Some(json!({ "authToken": plex_token })))?;
        if response.cookies.is_empty() {
            return Err(SeerrError::Unreadable);
        }
        let user = user_from(&parse(&response)?).ok_or(SeerrError::Unreadable)?;
        Ok((SeerrSession { cookie: response.cookies.join("; ") }, user))
    }

    /// `GET /api/v1/auth/me`: is the session still good, and who is it.
    pub fn me(&self, session: &SeerrSession, req: SeerrTransport) -> Result<SeerrUser, SeerrError> {
        let response = call(req, "GET", self.url("/api/v1/auth/me"), Some(session), None)?;
        user_from(&parse(&response)?).ok_or(SeerrError::Unreadable)
    }

    /// `GET /api/v1/search?query=…&page=n`: movies and shows only (people are dropped).
    pub fn search(&self, session: &SeerrSession, query: &str, page: u32, req: SeerrTransport)
        -> Result<Page<Title>, SeerrError>
    {
        let path = QueryBuilder::new("/api/v1/search").str("query", query).int("page", i64::from(page.max(1))).build();
        let response = call(req, "GET", self.url(&path), Some(session), None)?;
        Ok(page_from(&parse(&response)?, None))
    }

    /// One discover row, one page. A movie-only or show-only row's rows may omit `mediaType`.
    pub fn discover(&self, session: &SeerrSession, row: DiscoverRow, page: u32, req: SeerrTransport)
        -> Result<Page<Title>, SeerrError>
    {
        let hint = match row {
            DiscoverRow::PopularMovies | DiscoverRow::UpcomingMovies => Some(MediaKind::Movie),
            DiscoverRow::PopularTv | DiscoverRow::UpcomingTv => Some(MediaKind::Tv),
            DiscoverRow::Trending => None,
        };
        let path = QueryBuilder::new(row.path()).int("page", i64::from(page.max(1))).build();
        let response = call(req, "GET", self.url(&path), Some(session), None)?;
        Ok(page_from(&parse(&response)?, hint))
    }

    /// `GET /api/v1/movie/{id}` or `/api/v1/tv/{id}`.
    pub fn detail(&self, session: &SeerrSession, kind: MediaKind, tmdb: i64, req: SeerrTransport)
        -> Result<TitleDetail, SeerrError>
    {
        if tmdb <= 0 {
            return Err(SeerrError::NotFound);
        }
        let response = call(req, "GET", self.url(&format!("/api/v1/{}/{tmdb}", kind.wire())), Some(session), None)?;
        let body = parse(&response)?;
        let title = title_from(&body, Some(kind)).ok_or(SeerrError::Unreadable)?;
        let seasons = if kind == MediaKind::Tv { seasons_from(&body) } else { Vec::new() };
        Ok(TitleDetail { title, seasons, anime: kind == MediaKind::Tv && anime_from(&body) })
    }

    /// `GET /api/v1/service/radarr` (a movie) or `/sonarr` (a show): the servers a request of that
    /// kind can go to.
    pub fn servers(&self, session: &SeerrSession, kind: MediaKind, req: SeerrTransport)
        -> Result<Vec<ArrServer>, SeerrError>
    {
        let response = call(req, "GET", self.url(&format!("/api/v1/service/{}", kind.service())), Some(session), None)?;
        let body = parse(&response)?;
        let rows = body.as_array().ok_or(SeerrError::Unreadable)?;
        Ok(rows.iter().filter_map(server_from).collect())
    }

    /// `GET /api/v1/service/{radarr|sonarr}/{id}`: one server's quality profiles and root folders.
    pub fn server_options(&self, session: &SeerrSession, kind: MediaKind, server: i64, req: SeerrTransport)
        -> Result<ArrOptions, SeerrError>
    {
        if server < 0 {
            return Err(SeerrError::NotFound);
        }
        let path = format!("/api/v1/service/{}/{server}", kind.service());
        let response = call(req, "GET", self.url(&path), Some(session), None)?;
        Ok(options_from(&parse(&response)?))
    }

    /// One page of the signed-in user's own requests, newest first.
    pub fn my_requests(&self, session: &SeerrSession, user: i64, page: u32, req: SeerrTransport)
        -> Result<Page<MyRequest>, SeerrError>
    {
        let skip = i64::from(page.max(1) - 1) * i64::from(REQUEST_PAGE);
        let path = QueryBuilder::new("/api/v1/request").int("take", i64::from(REQUEST_PAGE)).int("skip", skip)
            .str("filter", "all").str("sort", "added").int("requestedBy", user).build();
        let response = call(req, "GET", self.url(&path), Some(session), None)?;
        let body = parse(&response)?;
        let items = body.get("results").and_then(Value::as_array).map_or(Vec::new(), |rows|
            rows.iter().filter_map(request_from).collect());
        let info = body.get("pageInfo");
        let pages = info.and_then(|i| int(i, "pages")).and_then(|p| u32::try_from(p).ok()).unwrap_or(1);
        Ok(Page { items, page: page.max(1), pages })
    }

    /// `POST /api/v1/request`: a movie, or a show's `seasons` (the server rejects an empty list),
    /// with the server, quality profile and root folder `options` names (each one left out when
    /// `None`, so the server decides it).
    pub fn request(&self, session: &SeerrSession, kind: MediaKind, tmdb: i64, seasons: &[i32], options: &RequestOptions,
        req: SeerrTransport) -> Result<(), SeerrError>
    {
        if tmdb <= 0 || (kind == MediaKind::Tv && seasons.is_empty()) {
            return Err(SeerrError::NotFound);
        }
        let mut body = match kind {
            MediaKind::Movie => json!({ "mediaType": "movie", "mediaId": tmdb }),
            MediaKind::Tv => json!({ "mediaType": "tv", "mediaId": tmdb, "seasons": seasons }),
        };
        if let Some(server) = options.server {
            body["serverId"] = json!(server);
        }
        if let Some(profile) = options.profile {
            body["profileId"] = json!(profile);
        }
        if let Some(folder) = &options.folder {
            body["rootFolder"] = json!(folder);
        }
        call(req, "POST", self.url("/api/v1/request"), Some(session), Some(body)).map(|_| ())
    }
}

#[cfg(test)]
#[path = "seerr_tests.rs"]
mod tests;
