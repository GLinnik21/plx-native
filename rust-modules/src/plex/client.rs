//! The `Client` (immutable host/port/token + identity) and the four centralisation
//! choke points every op file routes through:
//!   * `with_token` — the ONLY place `X-Plex-Token` is appended.
//!   * `enc` / `QueryBuilder` — the ONLY place a value is percent-encoded (via
//!     `crate::pms::urlenc_str`) or a query string is assembled.
//!   * `get_json`/`get_bytes`/`get_void`/`put`/`post` — the ONLY code that touches the
//!     raw socket in `crate::stream`.
//! Plus `StreamUrl`, the built-playback-target return type for the demux/cue sockets.
//!
//! Fields + helpers are `pub(super)` (visible inside the `plex` module tree only): the op
//! files live in sibling submodules and add `impl Client` blocks, so they need to read the
//! identity fields and call the helpers — but nothing OUTSIDE `plex` can reach them.
use super::models::{Envelope, MediaContainer};
use std::sync::{OnceLock, RwLock};

const ACCEPT_JSON: &str = "Accept: application/json\r\n";

/// Immutable after construction. Cheap to share by `&ref` across threads (poster workers,
/// the timeline reporter, the detail loader all read it). `host` is a numeric dotted-quad —
/// the raw socket does no DNS.
pub struct Client {
    pub(super) host: String,      // "192.168.0.3" (numeric; passed straight to http_get/http_open)
    pub(super) port: i32,         // 32400
    // X-Plex-Token value. Interior-mutable because the token changes at runtime after boot: it's
    // installed once we've logged in, and swapped when the user switches Plex Home profile (same
    // server, different per-user token). Read in exactly one place (`with_token`).
    pub(super) token: RwLock<String>,
    pub(super) client_id: String, // X-Plex-Client-Identifier — stable device id
    pub(super) product: String,   // "plxnative"
    pub(super) version: String,   // "1"
    pub(super) platform: String,  // "Generic"
}

impl Client {
    pub fn new(host: &str, port: i32, token: &str) -> Client {
        Client {
            host: host.to_owned(),
            port,
            token: RwLock::new(token.to_owned()),
            client_id: "com.beb.plxnative".into(),
            product: "plxnative".into(),
            version: "1".into(),
            platform: "Generic".into(),
        }
    }

    /// Swap the `X-Plex-Token` at runtime (Plex Home profile switch — same server, new per-user
    /// token). Cheap; the next request picks it up via [`Client::with_token`]. Bumps the token
    /// generation so token-baked caches (the poster key memo) invalidate.
    pub fn set_token(&self, token: &str) {
        if let Ok(mut g) = self.token.write() {
            *g = token.to_owned();
        }
        TOKEN_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    /// Monotonic token generation — bumped by [`Client::set_token`]; caches keyed on paths that
    /// embed the token compare this to know when to flush.
    pub fn token_gen(&self) -> u32 {
        TOKEN_GEN.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> i32 {
        self.port
    }

    // ---- transport choke points: the only code that touches crate::stream ----

    /// GET → parse the `{ "MediaContainer": … }` envelope into the flat container.
    pub(super) fn get_json(&self, path_no_token: &str) -> Option<MediaContainer> {
        let path = self.with_token(path_no_token);
        let body = crate::stream::http_get(&self.host, self.port, &path, Some(ACCEPT_JSON))?;
        serde_json::from_slice::<Envelope>(&body).ok().map(|e| e.media_container)
    }

    /// GET raw bytes (image transcode / sidecar sub) — caller decodes.
    pub(super) fn get_bytes(&self, path_no_token: &str) -> Option<Vec<u8>> {
        crate::stream::http_get(&self.host, self.port, &self.with_token(path_no_token), None)
    }

    /// GET whose body is discarded (transcode decision / stop registration side effects).
    pub(super) fn get_void(&self, path_no_token: &str) {
        let _ = crate::stream::http_get(&self.host, self.port, &self.with_token(path_no_token), None);
    }

    /// PUT (no body) — returns the HTTP status (all `select_streams` reads).
    pub(super) fn put(&self, path_no_token: &str) -> i32 {
        crate::stream::http_put(&self.host, self.port, &self.with_token(path_no_token))
    }

    /// POST (no body) — /:/timeline (spec verb). Reuses `crate::stream::http_open` with the
    /// "POST" method over the same raw socket http_put uses; returns the HTTP status.
    pub(super) fn post(&self, path_no_token: &str) -> i32 {
        let path = self.with_token(path_no_token);
        let host_c = match std::ffi::CString::new(self.host.as_str()) {
            Ok(c) => c,
            Err(_) => return -1,
        };
        let path_c = match std::ffi::CString::new(path) {
            Ok(c) => c,
            Err(_) => return -1,
        };
        let mut hs = crate::stream::http_stream_boxed();
        let opened = crate::stream::http_open(
            &mut *hs,
            host_c.as_ptr(),
            self.port,
            path_c.as_ptr(),
            std::ptr::null(),
            "POST",
        );
        let status = crate::stream::hs_status(&*hs);
        if opened == 0 {
            let mut chunk = vec![0u8; 4096];
            while crate::stream::http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as i32) > 0 {}
        }
        crate::stream::http_close(&mut *hs);
        status
    }

    /// THE token choke point. Appends `X-Plex-Token=…` with the right separator.
    pub(super) fn with_token(&self, path: &str) -> String {
        let sep = if path.contains('?') { '&' } else { '?' };
        let tok = self.token.read().map(|g| g.clone()).unwrap_or_default();
        format!("{path}{sep}X-Plex-Token={tok}")
    }
}

// ---- shared singleton (built once in plex_run, read everywhere) ----
static PLEX: OnceLock<Client> = OnceLock::new();
static TOKEN_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Install the process-wide `Client` (call once at boot). No-op if already set.
pub fn init(host: &str, port: i32, token: &str) {
    let _ = PLEX.set(Client::new(host, port, token));
}
/// Install for a (re)login / profile switch. First call sets the singleton; a later call keeps the
/// same server (host/port are fixed once set) and just swaps the token — so the login flow can set
/// the server token, then a Plex Home profile pick swaps in that user's token. Thread-safe.
pub fn install(host: &str, port: i32, token: &str) {
    match PLEX.get() {
        None => {
            let _ = PLEX.set(Client::new(host, port, token));
        }
        Some(c) => c.set_token(token),
    }
}
/// Whether the process-wide client has been installed yet (guards PMS reads before login).
pub fn is_installed() -> bool {
    PLEX.get().is_some()
}
/// The process-wide `Client`. Panics if `init` was never called.
pub fn client() -> &'static Client {
    PLEX.get().expect("plex::init not called")
}

// ---- enc + QueryBuilder — the percent-encoding choke point ----

/// RFC3986-unreserved passthrough; everything else → %XX. Delegates to the shared
/// `crate::pms::urlenc_str` so the encoder lives in exactly one place.
pub(super) fn enc(src: &str) -> String {
    crate::pms::urlenc_str(src)
}

/// Builds `path?k=enc(v)&k2=42…`. `.str` percent-encodes the value; `.int` does not
/// (digits are unreserved). No op file ever formats a query by hand. `.query()` returns just
/// the joined params (no path/`?`) for the transcode endpoints that embed them after a fixed
/// `start.mkv?`/`decision?` prefix.
pub(super) struct QueryBuilder {
    path: String,
    parts: Vec<String>,
}
impl QueryBuilder {
    pub(super) fn new(path: impl Into<String>) -> Self {
        Self { path: path.into(), parts: Vec::new() }
    }
    pub(super) fn str(mut self, k: &str, v: &str) -> Self {
        self.parts.push(format!("{k}={}", enc(v)));
        self
    }
    pub(super) fn int(mut self, k: &str, v: i64) -> Self {
        self.parts.push(format!("{k}={v}"));
        self
    }
    pub(super) fn opt_int(self, k: &str, v: i64) -> Self {
        if v != 0 {
            self.int(k, v)
        } else {
            self
        }
    }
    pub(super) fn build(self) -> String {
        if self.parts.is_empty() {
            self.path
        } else {
            format!("{}?{}", self.path, self.parts.join("&"))
        }
    }
    pub(super) fn query(self) -> String {
        self.parts.join("&")
    }
}

// ---- StreamUrl — the streaming return type ----

/// A built playback target for the raw demux/cue sockets. NOT a fetched response — the player
/// passes these three fields straight to `crate::stream::http_open`. Range headers for seeks
/// are added by the player as `http_open`'s `extra`, never by this layer. `path` includes the
/// `?query&X-Plex-Token`.
pub struct StreamUrl {
    pub host: String,
    pub port: i32,
    pub path: String,
}

impl StreamUrl {
    /// "http://host:port/path" for URL storage + logs.
    pub fn to_url(&self) -> String {
        format!("http://{}:{}{}", self.host, self.port, self.path)
    }
    /// Parse an EXTERNAL full URL (the /tmp/plxnative-url override) back into parts —
    /// replaces `player::engine::parse_stream_url` (same behavior: default port 32400).
    pub fn parse(url: &str) -> StreamUrl {
        let s = url.strip_prefix("http://").unwrap_or(url);
        let he = s.find(|c| c == ':' || c == '/').unwrap_or(s.len());
        let (host, rest) = (s[..he].to_string(), &s[he..]);
        if let Some(r) = rest.strip_prefix(':') {
            let pe = r.find('/').unwrap_or(r.len());
            let port = r[..pe].parse().unwrap_or(32400);
            let path = if pe < r.len() { r[pe..].into() } else { "/".into() };
            StreamUrl { host, port, path }
        } else {
            StreamUrl {
                host,
                port: 32400,
                path: if rest.is_empty() { "/".into() } else { rest.into() },
            }
        }
    }
}
