//! A signed-in connection to one Jellyfin server, answering in the common item model.
//!
//! [`JfClient`] is to Jellyfin what `plx_plex::plex::Client` is to a Plex Media Server: the one
//! place requests to that server are made. Its read methods return the same `MediaContainer` /
//! `Metadata` the Plex client does, built by [`translate`](super::translate), so the layers above
//! can take either.
//!
//! The wire goes through a [`Transport`], so the whole client runs in host tests against canned
//! answers. [`CurlTransport`] is the real one: libcurl through `plx_net::net`, which handles both
//! `http://` (the usual LAN server) and `https://` (a reverse proxy), with a JSON body when a
//! request has one.

use super::auth::ClientIdentity;
use super::dto::{
    AuthenticationResult, BaseItemDto, MediaSegmentDto, PublicSystemInfo, QueryResult, QuickConnectResult, RemoteSubtitleInfo,
    UserDto,
    UserHomeConfig, UserLanguageConfig, UserLanguageUpdate,
};
use super::ids::{normalize, SectionIds};
use super::image_tags;
use super::languages;
use super::live;
use super::paths::{self, ImageKind, ItemsQuery, SortBy};
use super::time::ms_to_ticks;
use super::translate::{self, Ctx};
use plx_platform::i18n::msg;
use plx_plex::plex::account::PreferenceError;
use plx_plex::plex::{Hub, LibrarySection, MediaContainer, Metadata, OnDeckHub, Trickplay};
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use serde::Deserialize;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The verbs Jellyfin's API uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Get,
    Post,
    Delete,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Get => "GET",
            Verb::Post => "POST",
            Verb::Delete => "DELETE",
        }
    }
}

/// A completed HTTP exchange: whatever status the server sent, and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// One request out. `None` is a transport failure (nothing answered); any HTTP status, a 401
/// included, is `Some`.
pub trait Transport: Send + Sync {
    fn send(&self, url: &str, verb: Verb, headers: &[String], body: Option<&[u8]>) -> Option<Answer>;
}

/// The real transport: libcurl through `plx_net::net`.
pub struct CurlTransport;

impl Transport for CurlTransport {
    fn send(&self, url: &str, verb: Verb, headers: &[String], body: Option<&[u8]>) -> Option<Answer> {
        if carries_credential(url, headers, body.is_some()) && !credential_may_go(url) {
            // Refused like a request nothing answered.
            return None;
        }
        let r = plx_net::net::request(url, headers, verb.as_str(), body, plx_net::net::API, false, None, None)?;
        Some(Answer { status: r.status, body: r.body })
    }
}

/// Does this request carry a credential: the token in the `Authorization` header or the URL, or a
/// body — every request body this client sends is a sign-in (a password, a Quick Connect secret)
/// or rides a signed-in session. Only the tokenless calls (the server's public description, starting
/// Quick Connect, images) go without one.
pub fn carries_credential(url: &str, headers: &[String], has_body: bool) -> bool {
    let url = url.to_ascii_lowercase();
    has_body
        || url.contains("apikey=")
        || url.contains("api_key=")
        || headers.iter().any(|h| {
            h.split_once(':').is_some_and(|(name, value)| {
                name.trim().eq_ignore_ascii_case("authorization") && value.contains(", Token=")
            })
        })
}

/// May a credential go to `url`'s origin: the one authority every Plex request asks too
/// (`plx_plex::plex::grant`) — TLS, a developer build, or a live grant for exactly this plaintext
/// origin. An address that does not parse may not.
fn credential_may_go(url: &str) -> bool {
    plx_plex::plex::Origin::parse(url).is_some_and(|o| plx_plex::plex::grant::credential_allowed(&o))
}

/// Why a sign-in step failed, in the terms the sign-in screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInError {
    /// Nothing answered at that address.
    Unreachable,
    /// Something answered, but not a Jellyfin server.
    NotJellyfin,
    /// Wrong username or password, or a Quick Connect secret the server no longer knows.
    Rejected,
    /// The server has Quick Connect turned off (Dashboard → General → Quick Connect).
    QuickConnectDisabled,
    /// Any other answer, with its status.
    Server(u16),
}

/// Who is signed in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub user_id: String,
    pub user_name: String,
    /// The avatar's image tag, when the user has one.
    pub avatar_tag: Option<String>,
    pub token: String,
    pub server_id: String,
}

impl Session {
    /// [`Self::of`] for a user as the server sent it.
    pub fn of_value(user: &serde_json::Value, token: &str) -> Option<Self> {
        Self::of(&UserDto::deserialize(user).ok()?, token)
    }

    /// `user`, signed in with `token`; `None` for a user without an id.
    pub fn of(user: &UserDto, token: &str) -> Option<Self> {
        (!user.id.is_empty()).then(|| Session {
            user_id: normalize(&user.id),
            user_name: user.name.clone(),
            avatar_tag: user.primary_image_tag.clone(),
            token: token.to_string(),
            server_id: user.server_id.as_deref().map(normalize).unwrap_or_default(),
        })
    }
}

/// One Jellyfin server, before or after sign-in.
pub struct JfClient {
    base: String,
    identity: ClientIdentity,
    session: Option<Session>,
    sections: SectionIds,
    /// Play sessions this client has reported as started, so the first report of each is the
    /// `/Sessions/Playing` start and the rest are progress.
    started: Mutex<HashSet<String>>,
    /// Play sessions the server is transcoding, which report as `Transcode` rather than `DirectPlay`.
    transcoding: Mutex<HashSet<String>>,
    /// The item [`Self::metadata`] read last, and when: starting a playback asks for the same item
    /// at each step (the decision, the queue, a transcode's plan), seconds after its page did.
    /// Forgotten by every write this client makes about an item ([`Self::forget_recent`]).
    recent_item: Mutex<Option<(String, Instant, BaseItemDto, Option<Vec<MediaSegmentDto>>)>>,
    /// The signed-in user as the server last described it (`/Users/{id}`: name, avatar,
    /// `Configuration`, `Policy`) ([`Self::user_value`]).
    recent_user: Recent<Arc<serde_json::Value>, PreferenceError>,
    /// The user's libraries as last listed ([`Self::libraries_recent`]).
    recent_libraries: Recent<Arc<QueryResult<BaseItemDto>>, ()>,
    /// This app's server-side display preferences, where the dismissed Next Up episodes are kept
    /// (`display_prefs`): read once per [`RECENT_CONFIG`], and replaced by this client's own write.
    recent_prefs: Recent<Arc<serde_json::Value>, ()>,
    transport: Box<dyn Transport>,
}

/// How long the item [`JfClient::metadata`] read is reused for a playback's next step.
const RECENT_ITEM: Duration = Duration::from_secs(30);
/// How long the user's Home settings are reused by a library page's shelves; Home reads them anew.
const RECENT_CONFIG: Duration = Duration::from_secs(5 * 60);
/// How long a read answers the next call that wants it fresh (Home's settings and libraries, the
/// launch's check of the saved name and avatar): long enough for a launch's calls to share one.
pub(crate) const FRESH: Duration = Duration::from_secs(10);

/// A value read from the server, and when, shared by the calls that want it while it is recent
/// enough for them. Calls that arrive while it is read wait for that read and take its answer,
/// a failure included, rather than send another (a server that does not answer is waited on once).
struct Recent<T, E> {
    last: Mutex<Option<(Instant, Result<T, E>)>>,
    reading: Mutex<()>,
    /// Moves after each read, so a call that waited on [`Self::reading`] knows one just ran.
    reads: AtomicU32,
}

impl<T, E> Default for Recent<T, E> {
    fn default() -> Self {
        Self { last: Mutex::new(None), reading: Mutex::new(()), reads: AtomicU32::new(0) }
    }
}

impl<T: Clone, E: Clone> Recent<T, E> {
    /// The value as read in the last `fresh_for`, else `read` now.
    fn get(&self, fresh_for: Duration, read: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let seen = self.reads.load(Ordering::Acquire);
        if let Some(Ok(v)) = self.within(fresh_for) {
            return Ok(v);
        }
        let _one = self.reading.lock().unwrap_or_else(|p| p.into_inner());
        if self.reads.load(Ordering::Acquire) != seen {
            if let Some(answer) = self.within(Duration::MAX) {
                return answer;
            }
        }
        let answer = read();
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), answer.clone()));
        self.reads.fetch_add(1, Ordering::Release);
        answer
    }

    fn within(&self, fresh_for: Duration) -> Option<Result<T, E>> {
        self.last.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
            .filter(|(at, _)| at.elapsed() < fresh_for)
            .map(|(_, answer)| answer.clone())
    }

    fn put(&self, v: T) {
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), Ok(v)));
    }

    fn forget(&self) {
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// `a` here and `b` on a thread of its own, at once: two requests that need nothing from each
/// other. `b`'s answer is `None` only when its thread panicked.
pub(crate) fn both<A, B: Send>(a: impl FnOnce() -> A, b: impl FnOnce() -> B + Send) -> (A, Option<B>) {
    std::thread::scope(|s| {
        let b = s.spawn(b);
        (a(), b.join().ok())
    })
}

/// How many items each home shelf asks for.
pub const HOME_SHELF_SIZE: i64 = 24;

impl JfClient {
    /// A client for the server at `base` (`http://192.168.1.20:8096`, `https://media.example/jellyfin`).
    /// A trailing slash is dropped; a missing scheme means `http://`, which is what Jellyfin listens on.
    pub fn new(base: &str, identity: ClientIdentity, transport: Box<dyn Transport>) -> Self {
        Self {
            base: normalize_base(base),
            identity,
            session: None,
            sections: SectionIds::new(),
            started: Mutex::new(HashSet::new()),
            transcoding: Mutex::new(HashSet::new()),
            recent_item: Mutex::new(None),
            recent_user: Recent::default(),
            recent_libraries: Recent::default(),
            recent_prefs: Recent::default(),
            transport,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Resume from a saved token alone: asks the server whose it is (`/Users/Me`).
    /// [`SignInError::Unreachable`] when nothing answered, [`SignInError::Rejected`] when the
    /// server no longer accepts the token (401, 403).
    pub fn who_is(&mut self, token: &str) -> Result<Session, SignInError> {
        self.session = Some(Session { token: token.to_string(), ..Default::default() });
        let answer = self.send(Verb::Get, &paths::users_me(), None);
        let me = match answer {
            None => Err(SignInError::Unreachable),
            Some(a) if a.status == 401 || a.status == 403 => Err(SignInError::Rejected),
            Some(a) if !a.ok() => Err(SignInError::Server(a.status)),
            Some(a) => serde_json::from_slice::<serde_json::Value>(&a.body).ok()
                .and_then(|v| Some((Session::of_value(&v, token)?, Arc::new(v))))
                .ok_or(SignInError::Server(a.status)),
        };
        match me {
            Ok((session, user)) => {
                self.session = Some(session.clone());
                self.remember_user(user);
                Ok(session)
            }
            Err(e) => {
                self.session = None;
                Err(e)
            }
        }
    }

    /// Resume a sign-in whose user was saved beside it: no request, so a launch's first fetch is
    /// not held behind a `/Users/Me`. A token the server stopped accepting then fails its first
    /// request instead, which reads the same as a resume that failed.
    pub fn resume_saved(&mut self, session: Session) {
        self.session = Some(session);
    }

    /// The library behind `section_key`. Keys are handed out by the library list
    /// ([`Self::libraries`]); a key this client has not listed yet (a page opened before any list)
    /// lists them once and looks again.
    fn section_guid(&self, section_key: i64) -> Option<String> {
        self.sections.guid(section_key).or_else(|| {
            self.libraries_recent(FRESH)?;
            self.sections.guid(section_key)
        })
    }

    /// A write changed what the server says about an item: the next read asks it again.
    fn forget_recent(&self) {
        *self.recent_item.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn token(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.token.as_str())
    }

    fn user_id(&self) -> &str {
        self.session.as_ref().map_or("", |s| s.user_id.as_str())
    }

    fn send(&self, verb: Verb, path: &str, body: Option<&str>) -> Option<Answer> {
        let mut headers = vec![
            format!("Authorization: {}", self.identity.header(self.token())),
            "Accept: application/json".to_string(),
        ];
        if body.is_some() {
            headers.push("Content-Type: application/json".to_string());
        }
        self.transport.send(&format!("{}{path}", self.base), verb, &headers, body.map(str::as_bytes))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Option<T> {
        let a = self.send(Verb::Get, path, None)?;
        if !a.ok() {
            return None;
        }
        serde_json::from_slice(&a.body).ok()
    }

    /// The signed-in user (`/Users/{id}`), as read in the last `fresh_for`, else read now. Calls
    /// that arrive while a read runs wait for it and take its answer.
    pub fn user_value(&self, fresh_for: Duration) -> Result<Arc<serde_json::Value>, PreferenceError> {
        if self.user_id().is_empty() {
            return Err(PreferenceError::Unavailable);
        }
        self.recent_user.get(fresh_for, || {
            let a = preference_answer(self.send(Verb::Get, &paths::user(self.user_id()), None))?;
            serde_json::from_slice(&a.body).map(Arc::new).map_err(|_| PreferenceError::InvalidResponse)
        })
    }

    /// Keep `user` as the latest read of the signed-in user ([`Self::user_value`]).
    pub fn remember_user(&self, user: Arc<serde_json::Value>) {
        self.recent_user.put(user);
    }

    /// Drop the held read of the user: the server says it changed, or this client changed it.
    pub fn forget_user(&self) {
        self.recent_user.forget();
    }

    fn ctx<'a>(&'a self, section_guid: Option<&'a str>) -> Ctx<'a> {
        Ctx { sections: &self.sections, section_guid }
    }

    /// The items `path` lists, as a container (of the library `section_guid`, when one is known).
    fn listing(&self, path: &str, section_guid: Option<&str>) -> Option<MediaContainer> {
        let page: QueryResult<BaseItemDto> = self.get(path)?;
        Some(translate::container(&page, &self.ctx(section_guid)))
    }

}

/// A preference request's answer, or why there is none in the terms Settings shows.
fn preference_answer(a: Option<Answer>) -> Result<Answer, PreferenceError> {
    match a {
        None => Err(PreferenceError::Unavailable),
        Some(a) if a.ok() => Ok(a),
        Some(Answer { status: 401 | 403, .. }) => Err(PreferenceError::Refused),
        Some(_) => Err(PreferenceError::Unavailable),
    }
}

/// Where playback is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Playing,
    Paused,
    Stopped,
}

/// One playback report.
#[derive(Clone, Debug)]
pub struct PlaybackReport<'a> {
    pub item_id: &'a str,
    pub play_session_id: &'a str,
    pub state: PlayState,
    pub position_ms: i64,
    pub audio_index: i64,
    pub subtitle_index: i64,
}

/// One page of a library grid.
#[derive(Clone, Debug)]
pub struct LibraryPage<'a> {
    /// `Movie` for a movie library, `Series` for a TV library, or the listing the library's TYPE
    /// menu chose: `Season`, `Episode` or `BoxSet`.
    pub item_types: &'a str,
    pub sort_by: SortBy,
    pub descending: bool,
    pub unwatched_only: bool,
    pub starts_with: Option<&'a str>,
    /// The genre the Genre filter chose: a Jellyfin genre id (a `/Genres` row's `Id`).
    pub genre_id: Option<&'a str>,
    pub start: i64,
    pub size: i64,
}

/// An image path the app builds itself in Plex's shape (`/library/metadata/{id}/clearLogo`, `/thumb`,
/// `/art`) as the Jellyfin image it means. `None` for anything else.
pub fn plex_image_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/library/metadata/")?;
    let rest = rest.split('?').next().unwrap_or(rest);
    let mut parts = rest.split('/');
    let id = parts.next().filter(|id| !id.is_empty())?;
    let kind = match parts.next()? {
        "clearLogo" => ImageKind::Logo,
        "thumb" => ImageKind::Primary,
        "art" => ImageKind::Backdrop,
        _ => return None,
    };
    let tag = image_tags::tag(id, kind);
    Some(paths::image(id, kind, tag.as_deref(), 0, 0))
}

/// A server address as typed into a sign-in screen, as a base URL with an explicit port:
///
/// - `192.168.1.20` → `http://192.168.1.20:8096` (no scheme: Jellyfin's own default port)
/// - `192.168.1.20:8096/` → `http://192.168.1.20:8096`
/// - `https://media.example/jellyfin/` → `https://media.example:443/jellyfin` (a scheme written
///   without a port means that scheme's standard port, as a browser reads it)
///
/// The port is always spelled because the app's `Origin` reads a URL without one as Plex's 32400.
pub fn normalize_base(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    let (scheme, rest, default_port) = match b.split_once("://") {
        Some((s, r)) => {
            let s = s.to_ascii_lowercase();
            let port = if s == "https" { 443 } else { 80 };
            (s, r, port)
        }
        None => ("http".to_string(), b, 8096),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // A port is written when the authority ends in `:digits` outside any `[v6]` brackets.
    let has_port = match authority.rfind(':') {
        Some(i) => !authority[i..].contains(']') && authority[i + 1..].bytes().all(|c| c.is_ascii_digit()) && i + 1 < authority.len(),
        None => false,
    };
    if has_port {
        format!("{scheme}://{authority}{path}")
    } else {
        format!("{scheme}://{authority}:{default_port}{path}")
    }
}

/// The path part of a base URL (`/jellyfin` for a server behind a reverse-proxy path, `""` at the
/// root). Requests and stream URLs built against the server's `Origin` are prefixed with it.
pub fn base_path(base: &str) -> &str {
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    rest.find('/').map_or("", |i| &rest[i..])
}

mod browse;
mod items;
mod playback;
mod signin;

#[cfg(test)]
mod tests;
