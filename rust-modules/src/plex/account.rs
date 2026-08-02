//! The **plex.tv account** API — the login (PIN/QR), server discovery, and Plex Home managed-user
//! surface. This is a *different service* from the Plex Media Server: the repo's OpenAPI spec
//! (`docs/plex-openapi.json`) is PMS-only and every PMS op assumes you already hold a token, so
//! these endpoints have no entry there. They're modelled here in the same typed style as the rest
//! of the `plex` layer (`hubs.rs`/`library.rs`): typed methods returning serde DTOs, with all the
//! transport, identity headers, and token injection centralised on [`AccountClient`].
//!
//! Transport is [`crate::net`] (libcurl HTTPS) — the TLS analog of `stream::http_get`, since plex.tv
//! needs DNS + TLS that the plain-HTTP PMS socket can't do. Every call is **blocking**, so callers
//! run it on a background thread (the login-poll / discovery / switch threads), never the SDL loop.
//!
//! Tokens (`Pin.auth_token`, `Resource.access_token`, `SwitchedUser.auth_token`) are secrets: they
//! are never logged here and never printed by callers.
//!
//! `discover.rs` is this client's op-file sibling: the plex.tv **metadata provider** speaks the
//! same transport and the same identity headers, so it adds an `impl AccountClient` block rather
//! than a second client — which is why [`AccountClient::get`] is `pub(super)`.
use serde::de::DeserializeOwned;
use serde::Deserialize;

const PLEX_TV: &str = "https://plex.tv";

/// Identity + optional account token for plex.tv calls. `client_id` is the stable per-device
/// `X-Plex-Client-Identifier` (persisted across launches — plex.tv keys the authorized-device list
/// and the pin↔device binding on it). `token` is the account token, absent during the login handshake
/// and set once a pin resolves.
pub struct AccountClient {
    client_id: String,
    token: Option<String>,
}

impl AccountClient {
    pub fn new(client_id: &str, token: Option<&str>) -> AccountClient {
        AccountClient { client_id: client_id.to_owned(), token: token.map(|t| t.to_owned()) }
    }

    /// The `X-Plex-*` identity headers every plex.tv request carries (+ the token when present).
    /// These are also what plex.tv shows in the account's "authorized devices" list — which is
    /// why every value comes from [`identity`](super::identity) rather than a literal here: this
    /// list and the PMS's query-parameter copy had drifted on five of seven fields, and one of
    /// them ("Plex for webOS") read as an official Plex client in a stranger's account.
    fn headers(&self) -> Vec<String> {
        use super::identity as id;
        let mut h = vec![
            "Accept: application/json".to_string(),
            format!("X-Plex-Product: {}", id::PRODUCT),
            format!("X-Plex-Version: {}", id::VERSION),
            format!("X-Plex-Platform: {}", id::PLATFORM),
            format!("X-Plex-Device: {}", id::DEVICE),
            format!("X-Plex-Device-Name: {}", id::DEVICE_NAME),
            format!("X-Plex-Model: {}", id::MODEL),
            format!("X-Plex-Client-Identifier: {}", self.client_id),
        ];
        if let Some(t) = &self.token {
            h.push(format!("X-Plex-Token: {t}"));
        }
        h
    }

    /// `pub(super)` so the sibling op file `discover.rs` can add its `impl AccountClient` block on
    /// top of this ONE transport + identity choke point instead of hand-rolling a second one.
    pub(super) fn get<T: DeserializeOwned>(&self, url: &str) -> Option<T> {
        let resp = crate::net::https_get(url, &self.headers())?;
        if !resp.ok() {
            return None;
        }
        serde_json::from_slice::<T>(&resp.body).ok()
    }

    fn post<T: DeserializeOwned>(&self, url: &str) -> Option<T> {
        let resp = crate::net::https_post(url, &self.headers(), b"")?;
        if !resp.ok() {
            return None;
        }
        serde_json::from_slice::<T>(&resp.body).ok()
    }

    // ---- login (PIN/QR) ----

    /// POST /api/v2/pins — create a link PIN. `strong=false` yields a short human-typeable `code`
    /// (for the `plex.tv/link` fallback) alongside the QR; the returned `auth_token` is null until
    /// the user authorizes it on another device.
    pub fn create_pin(&self) -> Option<Pin> {
        self.post(&format!("{PLEX_TV}/api/v2/pins?strong=false"))
    }

    /// GET /api/v2/pins/{id} — poll a pending PIN. `Pin.auth_token` becomes `Some` once the user
    /// approves it (scans the QR / enters the code + signs in); poll until then or `expires_in`.
    pub fn poll_pin(&self, id: i64) -> Option<Pin> {
        self.get(&format!("{PLEX_TV}/api/v2/pins/{id}"))
    }

    // ---- server discovery ----

    /// GET /api/v2/resources — the account's servers (+ shared), each with its connection list and a
    /// per-server `access_token`. Requires the account token. `includeHttps`/`includeRelay` surface
    /// the LAN + relay connections so we can pick a local one for offline play.
    pub fn resources(&self) -> Option<Vec<Resource>> {
        self.get(&format!("{PLEX_TV}/api/v2/resources?includeHttps=1&includeRelay=1"))
    }

    // ---- Plex Home managed users ----

    /// GET /api/v2/home/users — the Home (managed) users for the account: the "who's watching"
    /// roster. Requires the (admin) account token.
    pub fn home_users(&self) -> Option<Vec<HomeUser>> {
        let hu: HomeUsers = self.get(&format!("{PLEX_TV}/api/v2/home/users"))?;
        Some(hu.users)
    }

    /// POST /api/v2/home/users/{uuid}/switch[?pin=NNNN] — exchange the admin token for the chosen
    /// user's own token (the thing PMS scopes watch state by). `pin` is required for a `protected`
    /// user, ignored otherwise.
    pub fn switch_user(&self, uuid: &str, pin: Option<&str>) -> Option<SwitchedUser> {
        let q = match pin {
            Some(p) if !p.is_empty() => format!("?pin={p}"),
            _ => String::new(),
        };
        self.post(&format!("{PLEX_TV}/api/v2/home/users/{uuid}/switch{q}"))
    }
}

// ---- serde DTOs (only the fields the app consumes; all optional to tolerate shape drift) ----

/// A link PIN (`/api/v2/pins`). `code` feeds both the QR (`app.plex.tv/auth`) and the typed
/// `plex.tv/link` fallback; `auth_token` is the account token once authorized.
#[derive(Deserialize, Default)]
pub struct Pin {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub code: String,
    #[serde(rename = "authToken", default)]
    pub auth_token: Option<String>,
    #[serde(rename = "expiresIn", default)]
    pub expires_in: i64,
    /// URL of a server-rendered QR PNG for this pin — the exact QR the official apps show, so we
    /// display it directly instead of hand-building (and mis-encoding) a deep link.
    #[serde(default)]
    pub qr: String,
}

/// One account server/resource (`/api/v2/resources`). We only act on `owned` servers that
/// `provides` "server", using `access_token` + a `local` connection.
#[derive(Deserialize, Default)]
pub struct Resource {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "clientIdentifier", default)]
    pub client_identifier: String,
    #[serde(default)]
    pub provides: String, // may be a comma list ("server,player")
    #[serde(default)]
    pub owned: bool,
    #[serde(rename = "accessToken", default)]
    pub access_token: String,
    #[serde(default)]
    pub connections: Vec<Connection>,
}
impl Resource {
    pub fn is_server(&self) -> bool {
        self.provides.split(',').any(|p| p.trim() == "server")
    }
    /// The best connection for offline LAN play: a `local`, non-`relay` one — its `address`:`port`
    /// is reached over plain HTTP by the existing PMS socket, so it keeps working with no internet.
    pub fn local_connection(&self) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|c| c.local && !c.relay)
            .or_else(|| self.connections.iter().find(|c| !c.relay))
    }
}

/// One reachable address for a [`Resource`]. `address`:`port` is the raw host:port (we use plain
/// HTTP to it locally); `uri` is the full scheme URL (https `*.plex.direct` for remote).
#[derive(Deserialize, Default)]
pub struct Connection {
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub port: i64,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub relay: bool,
}

/// `/api/v2/home/users` envelope.
#[derive(Deserialize, Default)]
struct HomeUsers {
    #[serde(default)]
    users: Vec<HomeUser>,
}

/// A Plex Home managed user — one tile on the "who's watching" screen. `protected` = has a PIN;
/// `thumb` is the avatar URL (plex.tv HTTPS — shown via the PMS image proxy).
#[derive(Deserialize, Default)]
pub struct HomeUser {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub title: String,
    // NOTE: only fields that are always present AND non-null in the response — plex.tv sends
    // `null` for absent strings (e.g. `username`/`email` on managed users), and serde's `default`
    // does NOT cover an explicit `null`, so a nullable String field fails the whole parse. `title`
    // and `thumb` are always concrete strings; anything nullable is deliberately omitted.
    #[serde(default)]
    pub thumb: String,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub restricted: bool,
    #[serde(default)]
    pub protected: bool,
}

/// Result of a user switch — carries `auth_token`, the per-user token PMS scopes watch state by.
#[derive(Deserialize, Default)]
pub struct SwitchedUser {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "authToken", alias = "authenticationToken", default)]
    pub auth_token: String,
}
