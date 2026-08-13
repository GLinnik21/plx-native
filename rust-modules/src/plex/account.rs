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

// The lenient wire adapters live once, in `models.rs`, next to the note that explains why every
// number and flag needs one — see `de_bool` there for why the plex.tv policy flags fold to `bool`
// while the PMS DTOs keep theirs as `i64`.
use super::models::{de_bool, de_i64, de_str, de_vec};

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
    /// the LAN + relay connections so we can pick a local one for offline play; `includeIPv6` adds
    /// the v6 connections, which are *ranked last* rather than used first (`probe.rs`) — we ask for
    /// them so the ranking is choosing between a known set instead of a set plex.tv edited for us.
    pub fn resources(&self) -> Option<Vec<Resource>> {
        self.get(&format!("{PLEX_TV}/api/v2/resources?includeHttps=1&includeRelay=1&includeIPv6=1"))
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

/// One account server/resource (`/api/v2/resources`) — owned **and** shared alike; `owned` is the
/// only thing that tells them apart, and it is a preference here, never a wall.
///
/// Everything past `connections` is a **connection-policy input** rather than something drawn:
/// `https_required` and `public_address_matches` decide which candidate URLs may exist at all
/// (`probe.rs`), and `source_title` is the owner's plex.tv handle — the one string the UI ever says
/// about a shared server ("Shared by bamx23"), the machine name staying in the sources list.
///
/// **No field here is strict, and that is the whole point.** plex.tv sends an explicit `null` for an
/// absent value (`sourceTitle` is null on every owned server), and serde's `default` covers a field
/// that is ABSENT, not one that is present and `null` — a distinction that costs nothing until it
/// costs everything: one strict `String` meeting one `null` fails the WHOLE resources array, so a
/// single odd row takes every other server with it and sign-in ends at "no server found".
///
/// So `sourceTitle` is a real [`Option`] (absent and empty mean different things — it is the handle
/// or there is no handle), every other string goes through `de_str`, `connections` through `de_vec`,
/// the flags through `de_bool` and the ids through `de_i64`. Adding a field means picking one of
/// those, never a bare `String`. Same trap as [`HomeUser`] below, which documents the day it bit.
#[derive(Deserialize, Default)]
pub struct Resource {
    #[serde(default, deserialize_with = "de_str")]
    pub name: String,
    #[serde(rename = "clientIdentifier", default, deserialize_with = "de_str")]
    pub client_identifier: String,
    #[serde(default, deserialize_with = "de_str")]
    pub provides: String, // may be a comma list ("server,player")
    #[serde(default, deserialize_with = "de_bool")]
    pub owned: bool,
    #[serde(rename = "accessToken", default, deserialize_with = "de_str")]
    pub access_token: String,
    /// The owner's plex.tv username, present ONLY on a shared resource (null when `owned`). This is
    /// what "shared" looks like on the wire, and the label Plex's own TV client shows.
    #[serde(rename = "sourceTitle", default)]
    pub source_title: Option<String>,
    /// plex.tv id of the account that owns the server; 0 on our own. Identity, not a label — the
    /// handle to show a user is `source_title`.
    #[serde(rename = "ownerId", default, deserialize_with = "de_i64")]
    pub owner_id: i64,
    /// The server is in the account's Plex Home (a shared *household*, not a friend share).
    #[serde(default, deserialize_with = "de_bool")]
    pub home: bool,
    /// plex.tv's own liveness hint from its last check-in. A hint, never a verdict: only a probe
    /// that answered with the right `machineIdentifier` proves a server reachable from this TV.
    #[serde(default, deserialize_with = "de_bool")]
    pub presence: bool,
    /// **Our** public IP equals the one this server checked in from, i.e. we really are behind the
    /// same NAT — the only field that means "you are on that LAN". `Connection.local` does NOT
    /// (see [`Connection::local`]); this is the field that makes a non-owned `local` usable.
    #[serde(rename = "publicAddressMatches", default, deserialize_with = "de_bool")]
    pub public_address_matches: bool,
    /// The owner set *Require secure connections*, so plain HTTP is refused. It suppresses every
    /// synthesized `http://` candidate — see `probe::candidates`.
    #[serde(rename = "httpsRequired", default, deserialize_with = "de_bool")]
    pub https_required: bool,
    #[serde(default, deserialize_with = "de_vec")]
    pub connections: Vec<Connection>,
}
impl Resource {
    pub fn is_server(&self) -> bool {
        self.provides.split(',').any(|p| p.trim() == "server")
    }
}

/// One reachable address for a [`Resource`]. `address`:`port` is the raw host:port (plain HTTP is
/// all the PMS socket can speak); `uri` is the full scheme URL — an https `*.plex.direct` name whose
/// dashed-IP label resolves to `address`, held verbatim because the hash label is the certificate's,
/// not the machine id, and https to the bare IP fails validation by design.
#[derive(Deserialize, Default)]
pub struct Connection {
    #[serde(default, deserialize_with = "de_str")]
    pub protocol: String,
    #[serde(default, deserialize_with = "de_str")]
    pub address: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub port: i64,
    #[serde(default, deserialize_with = "de_str")]
    pub uri: String,
    /// **"This address is RFC1918", not "you are on that LAN."** Measured 2026-08-11: a shared
    /// server advertises `local:true` on the OWNER's `172.20.x.x`, which from here times out after
    /// 8 s — or, worse, reaches a different machine of ours at that address.
    /// `Resource::public_address_matches` is the field that means what this one looks like it means.
    #[serde(default, deserialize_with = "de_bool")]
    pub local: bool,
    #[serde(default, deserialize_with = "de_bool")]
    pub relay: bool,
    /// Capital on the wire. Ranked last rather than dropped (`probe.rs`).
    #[serde(rename = "IPv6", default, deserialize_with = "de_bool")]
    pub ipv6: bool,
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

#[cfg(test)]
mod tests {
    use super::Resource;

    /// The real `/api/v2/resources` shape, both kinds of server in one array: ours (`owned`, with
    /// `sourceTitle`/`ownerId` sent as explicit **nulls**) and a share (`owned:false`, a handle in
    /// `sourceTitle`, `httpsRequired:false`). Shaped on the live response measured 2026-08-11
    /// (docs/shared-servers.md §2); addresses and identifiers are stand-ins, the SHAPE is not.
    ///
    /// The null is the whole point. serde's `default` does not cover an explicit null, so a strict
    /// `String` on `sourceTitle` fails the entire array — not one label, but every server, i.e.
    /// sign-in reporting "no server found" on an account that has two.
    #[test]
    fn owned_and_shared_resources_round_trip_with_explicit_nulls() {
        let json = br#"[
          {"name":"Gleb's Mac mini","clientIdentifier":"aaaa1111","provides":"server",
           "owned":true,"home":true,"presence":true,"publicAddressMatches":true,
           "httpsRequired":false,"sourceTitle":null,"ownerId":null,"accessToken":"tok-own",
           "connections":[
             {"protocol":"https","address":"192.168.0.10","port":32400,
              "uri":"https://192-168-0-10.hash1.plex.direct:32400","local":true,"relay":false,"IPv6":false},
             {"protocol":"https","address":"2001:db8::1","port":32400,
              "uri":"https://2001-db8--1.hash1.plex.direct:32400","local":true,"relay":false,"IPv6":true}]},
          {"name":"bx23-ldn","clientIdentifier":"bbbb2222","provides":"server",
           "owned":false,"home":false,"presence":true,"publicAddressMatches":false,
           "httpsRequired":false,"sourceTitle":"bamx23","ownerId":987654,"accessToken":"tok-share",
           "connections":[
             {"protocol":"https","address":"172.20.4.7","port":32400,
              "uri":"https://172-20-4-7.hash2.plex.direct:32400","local":true,"relay":false,"IPv6":false},
             {"protocol":"https","address":"203.0.113.9","port":26937,
              "uri":"https://203-0-113-9.hash2.plex.direct:26937","local":false,"relay":false,"IPv6":false}]}
        ]"#;
        let rs: Vec<Resource> = serde_json::from_slice(json).expect("explicit nulls must not fail the array");
        assert_eq!(rs.len(), 2, "both servers survive — the null did not take the container with it");

        let own = &rs[0];
        assert!(own.owned && own.is_server());
        assert_eq!(own.source_title, None, "an owned server has no owner to name");
        assert_eq!(own.owner_id, 0);
        assert!(own.home && own.presence && own.public_address_matches && !own.https_required);
        assert!(own.connections[1].ipv6, "IPv6 is capital on the wire");

        let share = &rs[1];
        assert!(!share.owned && share.is_server());
        // the handle is the ONE string the browsing UI says about a share; the machine name
        // (`name`) stays in the sources list.
        assert_eq!(share.source_title.as_deref(), Some("bamx23"));
        assert_eq!(share.owner_id, 987_654);
        assert!(!share.public_address_matches, "their 172.20 LAN is not ours — the load-bearing flag");
        assert_eq!(share.access_token, "tok-share", "per-(user,server) grant, never the account token");
        assert_eq!(share.connections.len(), 2);
    }

    /// Shape drift must cost one field, never the container: the 0/1 encodings Plex also uses for
    /// its flags, a string-encoded port, an absent flag, and an unknown field all land. A resource
    /// that fails to parse here would take every OTHER server in the array down with it.
    #[test]
    fn a_malformed_or_oddly_encoded_field_does_not_fail_the_container() {
        let json = br#"[
          {"name":"quirky","clientIdentifier":"cccc3333","provides":"server,player",
           "owned":1,"httpsRequired":"1","publicAddressMatches":0,"sourceTitle":null,
           "unknownFutureField":{"nested":true},
           "connections":[{"address":"10.0.0.4","port":"32400","uri":"","local":"1","IPv6":null}]},
          {"name":"sparse","clientIdentifier":"dddd4444","provides":"server"}
        ]"#;
        let rs: Vec<Resource> = serde_json::from_slice(json).expect("lenient parse");
        assert_eq!(rs.len(), 2);
        assert!(rs[0].owned, "1 is true");
        assert!(rs[0].https_required, "\"1\" is true");
        assert!(!rs[0].public_address_matches, "0 is false");
        assert_eq!(rs[0].connections[0].port, 32400, "a string-encoded port is still a port");
        assert!(rs[0].connections[0].local);
        assert!(!rs[0].connections[0].ipv6, "an explicit null flag is false, not a parse failure");
        // everything absent on the second resource degrades to the zero value, and it still counts
        // as a server — which is what keeps ONE odd row from emptying the roster.
        assert!(rs[1].is_server() && !rs[1].owned && rs[1].connections.is_empty());
        assert_eq!(rs[1].source_title, None);
    }

    /// An explicit `null` on EVERY string and on the connection list itself. This is the shape the
    /// struct's doc promises to survive, and until `de_str`/`de_vec` were applied to the non-`Option`
    /// fields it did not: `name`, `provides`, `clientIdentifier`, `accessToken`, `connections` and a
    /// connection's own `address`/`uri`/`protocol` were bare, and any one of them meeting a null
    /// failed the whole array. The blast radius is the point — the SECOND resource here is a
    /// perfectly good server, and the test is really asserting that it still arrives.
    #[test]
    fn an_explicit_null_on_any_string_costs_that_field_and_never_the_roster() {
        let json = br#"[
          {"name":null,"clientIdentifier":null,"provides":null,"accessToken":null,
           "sourceTitle":null,"ownerId":null,"connections":null},
          {"name":"survivor","clientIdentifier":"eeee5555","provides":"server","owned":true,
           "connections":[{"protocol":null,"address":null,"uri":null,"port":32400}]}
        ]"#;
        let rs: Vec<Resource> = serde_json::from_slice(json).expect("a null must not fail the array");
        assert_eq!(rs.len(), 2, "the good server must survive the bad row");

        // the all-null row degrades field by field, and stops being a server rather than exploding
        assert!(rs[0].name.is_empty() && rs[0].access_token.is_empty());
        assert!(!rs[0].is_server(), "a null `provides` names no capability");
        assert!(rs[0].connections.is_empty(), "a null connection list is an empty one");

        // and the row that matters is untouched
        assert!(rs[1].is_server() && rs[1].owned);
        assert_eq!(rs[1].name, "survivor");
        assert_eq!(rs[1].connections.len(), 1);
        assert!(rs[1].connections[0].address.is_empty(), "null address degrades to empty");
        assert_eq!(rs[1].connections[0].port, 32400);
    }
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
