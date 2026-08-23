//! The `Client` (immutable origin/token + identity) and the four centralisation
//! choke points every op file routes through:
//!   * `with_token` — the ONLY place `X-Plex-Token` is appended.
//!   * `enc` / `QueryBuilder` — the ONLY place a value is percent-encoded (via
//!     `crate::pms::urlenc_str`) or a query string is assembled.
//!   * `get_json`/`get_bytes`/`get_void`/`put`/`post` — the ONLY code that issues a PMS request.
//!     They went straight at the raw socket in `crate::stream` until this layer learned to speak
//!     to a server that is not on the LAN; they go through [`crate::http`] now, which dispatches
//!     on the origin's SCHEME — `stream.rs` for plaintext, libcurl for TLS. Nothing below this
//!     file knows there is more than one transport.
//! Plus `StreamUrl`, the built-playback-target return type for the demux/cue sockets.
//!
//! Fields + helpers are `pub(super)` (visible inside the `plex` module tree only): the op
//! files live in sibling submodules and add `impl Client` blocks, so they need to read the
//! identity fields and call the helpers — but nothing OUTSIDE `plex` can reach them.
//!
//! A `Client` is ONE server. WHICH servers exist, which one is current, and how a `&'static
//! Client` is handed out live next door in [`super::servers`] — this file is the type, that
//! file is the table.
use super::models::{Envelope, MediaContainer};
use super::origin::Origin;
use super::probe::Location;
use super::servers::ServerId;
use crate::http::{self, Method};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering::Relaxed};
use std::sync::RwLock;

/// `Accept: application/json`, from [`crate::http`] — one spelling for both transports, and
/// without the CRLF the raw socket used to want (each transport frames its own headers now).
use crate::http::ACCEPT_JSON;

/// The headers shared by EVERY PMS operation, over either transport. `X-Plex-Language` belongs
/// here rather than in [`Client::playback_identity`]: it selects server-returned metadata for
/// browse/search reads too, not only playback protocol calls. Owned strings keep the optional
/// locale value alive while `crate::http` borrows the slice.
fn pms_headers(headers: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    if let Some(language) = super::identity::language() {
        out.push(format!("X-Plex-Language: {language}"));
    }
    out
}

/// Immutable after construction (apart from the token + its generation, both interior-mutable).
/// Cheap to share by `&ref` across threads (poster workers, the timeline reporter, the detail
/// loader all read it). Its address is an [`Origin`] — see the field, and `origin.rs` for why a
/// `(host, port)` pair could not be the whole of one. The origin's SCHEME picks the transport
/// ([`crate::http`]), so a client whose server is a `plex.direct` name over the public internet is
/// the same type reached a different way — no call site below this layer knows the difference.
pub struct Client {
    /// This client's slot in the [server registry](super::servers) — a stable handle for the
    /// life of the process, which is what lets a caller name a server without holding a
    /// reference to it. `ServerId::UNSET` on a `Client` built outside the registry (tests).
    pub(super) id: ServerId,
    /// The server's `machineIdentifier` — the registry KEY, i.e. the identity that survives the
    /// server changing address. `""` while it is not known yet: the legacy `install(host, port,
    /// token)` has only an address to go on, and a later `register` that learns the id adopts
    /// that slot rather than adding a second one for the same server.
    pub(super) machine_id: String,
    /// **WHERE this server is** — scheme, host and port as one value ([`Origin`]).
    ///
    /// It was a `host: String` + `port: i32` pair, and the pair could not say `https`. That was
    /// not a missing feature so much as a trap: plex.tv advertises a server's TLS origin as the
    /// `plex.direct` HOSTNAME while its `address` stays the dotted quad behind it, so a client
    /// carrying only an address can never validate a certificate however much TLS is added
    /// underneath it. The origin is PARSED from what plex.tv sent — see
    /// [`super::probe::Candidate::origin`].
    ///
    /// It is **the transport selector**: every request this client makes goes through
    /// [`crate::http`], which dispatches on this scheme. A `plex.direct` origin therefore reaches
    /// its server over libcurl with the certificate validated against [`Origin::host`], and a
    /// plaintext one keeps the raw socket it always used. [`Client::host`] and [`Client::port`]
    /// still answer exactly what they always did, which is why the ~30 call sites below this
    /// layer did not move.
    pub(super) origin: Origin,
    // X-Plex-Token value. Interior-mutable because the token changes at runtime after boot: it's
    // installed once we've logged in, and swapped when the user switches Plex Home profile (same
    // server, different per-user token). Read in exactly one place (`with_token`).
    pub(super) token: RwLock<String>,
    // The §3b playback identity (X-Plex-* params on every playback request), so the server names
    // + groups this client as ONE device and shows a proper Player in /status/sessions.
    //
    // This is the SAME id the plex.tv transport sends — `session::Session.client_id`, the v4 UUID
    // minted from /dev/urandom on first boot and persisted. It used to be a hardcoded literal, and
    // the comment here asserted the two identities were deliberately different because "PMS
    // playback keys on this fixed one". That reasoning does not survive being published: PMS keys
    // on whatever this client sends, and a constant compiled into the binary makes EVERY install
    // on earth one device. Two households on one shared server would merge in /status/sessions,
    // their PlayQueues and timelines would collide, and `GET /video/:/transcode/universal/stop`
    // — which is keyed on exactly this value — could stop a stranger's transcode.
    //
    // What "fixed" was actually protecting is stability ACROSS RUNS, and the persisted UUID gives
    // that too, per install rather than per binary.
    pub(super) client_id: String, // X-Plex-Client-Identifier — stable device id (NEVER per-item)
    pub(super) product: String,   // "PlxNative"
    pub(super) version: String,   // "0.1.0"
    pub(super) platform: String,  // "webOS"
    // Token generation, PER SERVER. Bumped by `set_token`; read by caches keyed on a path that
    // bakes the token in (`posters::poster_key`'s memo). This used to be a process-global
    // `static TOKEN_GEN`, which cannot express "server B's token changed" — with a registry that
    // would flush every server's cache on any server's profile switch, and (worse) would say
    // NOTHING changed when the CURRENT server switched from A to B, handing B's requests A's
    // memoised token. Seeded from a global sequence (see `next_gen`) so no two `Client`s ever
    // share a value: a cache that only compares "did this number move" therefore also flushes
    // when `client()` starts answering with a different server.
    token_gen: AtomicU32,
    /// HOW this server is reached — the tier of the connection that won the probe, or "nobody has
    /// said yet" ([`LINK_UNKNOWN`]). A property of the SERVER, not of the request, which is why it
    /// lives beside its address rather than being recomputed at a call site.
    ///
    /// It exists because one tier changes what playback may ask for: Plex's relay is a capped
    /// tunnel, so a plan that streams the file's own bytes over it stalls. The policy that reads
    /// this is [`super::transcoder::link_policy`] — this field is only the fact.
    ///
    /// Interior-mutable and lock-free: written once per activation (a cold path), read once per
    /// playback resolve on a WORKER thread. `Relaxed` is enough because the value stands alone —
    /// nothing else is published with it, and a resolve that raced an activation by a microsecond
    /// would have read the old value a microsecond earlier anyway. A re-point does not carry it
    /// over: a new address is a new tier, and whoever re-points is the one who knows it.
    link: AtomicU8,
}

/// The source of every [`Client::token_gen`] value, process-wide. Only the UNIQUENESS matters
/// (see the field's note); it is never compared for order.
static GEN_SEQ: AtomicU32 = AtomicU32::new(1);
fn next_gen() -> u32 {
    GEN_SEQ.fetch_add(1, Relaxed)
}

/// [`Client::link`] before discovery has activated a candidate. Distinct from every real tier on
/// purpose: "unknown" is not "local", and the policy treats the two differently.
const LINK_UNKNOWN: u8 = 0;

/// `Location` ⇄ `u8`, so the tier fits in an atomic. Written as two total matches rather than a
/// cast: `Location`'s declaration ORDER is its preference order (`probe`'s derived `Ord` is the
/// ranking), so a discriminant cast would silently tie the stored encoding to that ordering and
/// break the moment a tier is inserted in the middle.
fn link_code(l: Location) -> u8 {
    match l {
        Location::Local => 1,
        Location::Remote => 2,
        Location::Relay => 3,
    }
}
fn link_of_code(c: u8) -> Option<Location> {
    match c {
        1 => Some(Location::Local),
        2 => Some(Location::Remote),
        3 => Some(Location::Relay),
        _ => None,
    }
}

/// The device facts of the playback identity. Shared with the plex.tv transport — see
/// [`identity`](super::identity) for why these stopped being literals in this file.
use super::identity::{device_name, DEVICE, MODEL, PROVIDES};

impl Client {
    /// Build a client for ONE server. `pub(super)`: a `Client` nobody can reach is useless, so
    /// the only construction site is [`super::servers::register`], which leaks it into the slot
    /// named by `id`.
    ///
    /// `client_id` is passed IN. It used to be resolved here as `session::load().client_id` —
    /// but `session::load` reads a file and can WRITE one (it mints + persists the uuid on first
    /// boot), which was tolerable behind a `OnceLock` singleton built exactly once and is not on
    /// a registry that constructs a `Client` per server and re-points slots. The registry does
    /// that read once per registration instead, so this constructor touches no filesystem and no
    /// global but the generation counter.
    pub(super) fn new(id: ServerId, machine_id: &str, origin: Origin, token: &str, client_id: &str) -> Client {
        Client {
            id,
            machine_id: machine_id.to_owned(),
            origin,
            token: RwLock::new(token.to_owned()),
            client_id: client_id.to_owned(),
            product: super::identity::PRODUCT.into(),
            version: super::identity::VERSION.into(),
            platform: super::identity::PLATFORM.into(),
            token_gen: AtomicU32::new(next_gen()),
            link: AtomicU8::new(LINK_UNKNOWN),
        }
    }

    /// Append the full playback identity to a query — every playback-protocol request
    /// (decision/start/stop/playQueues/timeline/direct-play GET) carries it.
    ///
    /// **This surface is what PMS ACTS ON**, which is the rule that decides what belongs here as
    /// against the plex.tv header copy (`account::AccountClient::headers`, whose doc states the
    /// other half). PMS draws these into the Player node of `/status/sessions`, groups a device's
    /// sessions by the identifier, and keys `GET /video/:/transcode/universal/stop` on it. What it
    /// does NOT do is choose a transcode from any of them — that is the capability profile
    /// `transcoder.rs` sends, built from the television's own codec table (`devcaps.rs`).
    ///
    /// **Two headers the official webOS client sends are absent from both surfaces on purpose.**
    /// Each was considered and each is a claim this app cannot honestly make:
    ///
    /// * **`X-Plex-Device-Screen-Resolution`.** The honest value is the PANEL — 3840x2160 on the
    ///   dev set, whose UI surface is 1080p — and the panel is read behind a private accessor in
    ///   `surface.rs`, which this layer does not own. Sending the drawable instead would announce
    ///   `1920x1080` from a 4K television, and it would announce it *alongside* a capability
    ///   profile that already says `videoResolution=3840x2160` with per-codec width and height
    ///   bounds from the device's own table. Two contradictory resolution claims in one request
    ///   can only cost quality, and the one that would lose is the truthful one.
    /// * **`X-Plex-Features: external-media,indirect-media`.** Both are capability CLAIMS, and
    ///   neither is true: nothing in `plex/` or `player/` handles an `indirect="1"` decision
    ///   response (it needs a second fetch this client never makes), and there is no path that
    ///   plays media the server does not host. Claiming them makes a server hand this app a
    ///   payload it cannot play — which is a worse failure than not claiming them, and it is
    ///   issue #22's lesson exactly: a claim true of the development environment asserted as
    ///   universal.
    ///
    /// `X-Plex-Language` is conditional rather than absent, and broader than this PLAYBACK-only
    /// query identity: [`pms_headers`] adds it to every PMS operation from the process locale.
    pub(super) fn playback_identity(&self, q: QueryBuilder) -> QueryBuilder {
        q.str("X-Plex-Client-Identifier", &self.client_id)
            .str("X-Plex-Product", &self.product)
            .str("X-Plex-Version", &self.version)
            .str("X-Plex-Platform", &self.platform)
            .str("X-Plex-Platform-Version", super::identity::platform_version())
            .str("X-Plex-Device", DEVICE)
            .str("X-Plex-Device-Name", device_name())
            .str("X-Plex-Model", MODEL)
            .str("X-Plex-Provides", PROVIDES)
    }

    /// Swap the `X-Plex-Token` at runtime (Plex Home profile switch — same server, new per-user
    /// token). Cheap; the next request picks it up via [`Client::with_token`]. Bumps the token
    /// generation so token-baked caches (the poster key memo) invalidate.
    pub fn set_token(&self, token: &str) {
        if let Ok(mut g) = self.token.write() {
            *g = token.to_owned();
        }
        self.token_gen.store(next_gen(), Relaxed);
    }
    /// Token generation for THIS server — moved by [`Client::set_token`]; caches keyed on paths
    /// that embed the token compare this to know when to flush. Signature unchanged from the
    /// process-global era on purpose: `posters.rs` reads it through `client()` and must keep
    /// compiling untouched.
    pub fn token_gen(&self) -> u32 {
        self.token_gen.load(Relaxed)
    }
    /// The host to DIAL — never bracketed, even for a v6 literal (see [`Origin::host`]). Unchanged
    /// in meaning and in bytes from when this was a plain field.
    pub fn host(&self) -> &str {
        self.origin.host()
    }
    pub fn port(&self) -> i32 {
        self.origin.port()
    }
    /// **Where this server is, whole** — the value to pass on rather than re-deriving a pair from
    /// [`Client::host`] and [`Client::port`], which cannot carry the scheme.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
    /// This client's registry slot — pass it to [`super::servers::client_for`] to get back a
    /// `&'static Client` from anywhere.
    pub fn id(&self) -> ServerId {
        self.id
    }
    /// The server's `machineIdentifier`, `""` if it has not been learned yet.
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
    /// Record which tier of connection reached this server — for the code that ACTIVATES a
    /// candidate (`probe::candidates` ranks them; racing and dialling them lands with the
    /// transport work). Call it once the probe has answered and the address is the one in use;
    /// until someone does, [`Client::link`] is `None` and playback policy is unrestricted.
    ///
    /// **ORDER MATTERS: register the address FIRST, then set the link on the client you get back**
    /// (`let id = register_origin(mid, &o, tok); client_for(id).unwrap().set_link(l);`). A
    /// `register` whose address differs RE-POINTS the slot — it publishes a fresh `Client`, which
    /// starts at `LINK_UNKNOWN` — so a tier set before that call is simply gone, and the policy
    /// silently reverts to unrestricted. That reset is deliberate rather than a wart: an address
    /// change is exactly the event that can turn a LAN connection into a relay, so the old tier is
    /// not evidence about the new one.
    pub fn set_link(&self, l: Location) {
        self.link.store(link_code(l), Relaxed);
    }
    /// How this server is reached, `None` while nothing has said. Feed it to
    /// [`super::transcoder::link_policy`] rather than matching on it at a call site — a relay is
    /// not the only fact a tier could ever carry, and the policy is the one place that decides.
    pub fn link(&self) -> Option<Location> {
        link_of_code(self.link.load(Relaxed))
    }

    // ---- transport choke points: the only code that touches crate::stream ----

    /// The ONE call every choke point below makes — origin, token, transport, in that order.
    ///
    /// `path_no_token` goes through [`Client::with_token`] (the token choke point) and then
    /// through [`crate::http`], which picks the transport from this client's origin. Splitting it
    /// out is not tidiness: it is what makes "did this request carry the token" and "which
    /// transport did it take" two questions with one answer each, rather than eight copies of the
    /// same three lines that a future scheme has to be threaded through one at a time.
    ///
    /// Returns the whole [`Reply`](crate::http::Reply) — status included — so each caller applies
    /// the fold it actually wants. That is the difference this file cares about: the one-shot
    /// wrapper this used to call folded every non-2xx into `None` for everybody, which is why a
    /// probe could not tell a 401 from a dead router.
    fn send(&self, path_no_token: &str, method: Method, headers: &[&str]) -> Option<http::Reply> {
        let owned = pms_headers(headers);
        let headers: Vec<&str> = owned.iter().map(String::as_str).collect();
        http::request(&self.origin, &self.with_token(path_no_token), method, &headers)
    }

    /// A 2xx response body, or `None` — the fold the raw socket used to apply for every caller,
    /// written once here now that the transport no longer does it. Every read below wants exactly
    /// this: a non-2xx PMS answer is not a container, and `http_open` has already logged the
    /// status line (`stream: GET /path status=…`) on the plaintext arm.
    fn body_2xx(&self, path_no_token: &str, method: Method, headers: &[&str]) -> Option<Vec<u8>> {
        let r = self.send(path_no_token, method, headers)?;
        r.ok().then_some(r.body)
    }

    /// The read twin of [`Client::body_2xx`] for a content-dependent PMS body. On HTTPS it keeps
    /// the connect deadline but has no 25 s whole-transfer cutoff; on plaintext it is the same
    /// socket policy `stream.rs` has always used.
    fn body_2xx_bulk(&self, path_no_token: &str, headers: &[&str]) -> Option<Vec<u8>> {
        let owned = pms_headers(headers);
        let headers: Vec<&str> = owned.iter().map(String::as_str).collect();
        let r =
            http::request_bulk(&self.origin, &self.with_token(path_no_token), Method::Get, &headers)?;
        r.ok().then_some(r.body)
    }

    /// GET → parse the `{ "MediaContainer": … }` envelope into the flat container.
    pub(super) fn get_json(&self, path_no_token: &str) -> Option<MediaContainer> {
        let body = self.body_2xx_bulk(path_no_token, &[ACCEPT_JSON])?;
        match serde_json::from_slice::<Envelope>(&body) {
            Ok(e) => Some(e.media_container),
            Err(e) => {
                // A 2xx whose body will not parse is the failure that used to be indistinguishable
                // from "the server never answered": both left by the same `None`. `stream.rs` had
                // a short-body notice for the truncation case, which reads two of its private
                // fields and so does not survive the move behind `crate::http`; this line answers
                // the same question from the other side, and covers the cases that one could not
                // (XML because an Accept header was rewritten in flight, an error page from a
                // proxy, a body that is simply a different shape).
                //
                // The ENDPOINT only — never the built path, which carries `X-Plex-Token`. Serde's
                // own message names a type and an offset and quotes no content.
                crate::log(&format!(
                    "pms: GET {} answered {} bytes that will not parse — {e}",
                    path_no_token.split('?').next().unwrap_or(path_no_token),
                    body.len()
                ));
                None
            }
        }
    }

    /// GET raw bytes (image transcode / sidecar sub) — caller decodes.
    pub(super) fn get_bytes(&self, path_no_token: &str) -> Option<Vec<u8>> {
        self.body_2xx_bulk(path_no_token, &[])
    }

    /// GET raw bytes for a path this server ALREADY BUILT — the one entry point that does **not**
    /// append `X-Plex-Token`, because the path handed in already ends in one.
    ///
    /// Its only caller is the poster store, and the reason is structural rather than stylistic:
    /// there, the built `/photo/:/transcode?…&X-Plex-Token=…` path *is* the LRU key, so the key
    /// and the request must be the same bytes. Routing it through [`Client::get_bytes`] would
    /// append a second token — a URL with two `X-Plex-Token` params, whose meaning is the
    /// server's business and not ours. `pub(crate)` because `posters.rs` lives outside this
    /// module tree; it exists so that file stops calling `crate::stream` behind this layer's
    /// back, which is what the module doc above has always claimed nothing does.
    ///
    /// The token is therefore in the CALLER's string. It must not be logged — the poster store
    /// logs no keys, and neither may anything else that holds one.
    pub(crate) fn fetch_built(&self, path_with_token: &str) -> Option<Vec<u8>> {
        // NOT `body_2xx`, for the same reason this method exists at all: that helper appends the
        // token, and this path already ends in one.
        let owned = pms_headers(&[]);
        let headers: Vec<&str> = owned.iter().map(String::as_str).collect();
        let r = http::request_bulk(&self.origin, path_with_token, Method::Get, &headers)?;
        r.ok().then_some(r.body)
    }

    /// GET whose body is discarded (transcode decision / stop registration side effects).
    pub(super) fn get_void(&self, path_no_token: &str) {
        let _ = self.send(path_no_token, Method::Get, &[]);
    }

    /// [`Client::get_void`], but reporting whether the request actually reached the server and came
    /// back accepted. For the body-less **writes** whose caller is off the main thread and has no
    /// other way to know — see [`super::library::Client::scrobble`]. `false` covers a refused or
    /// timed-out connect as much as a rejected status, which is the distinction that matters here:
    /// a share that is asleep answers nothing at all.
    pub(super) fn get_ok(&self, path_no_token: &str) -> bool {
        self.body_2xx(path_no_token, Method::Get, &[]).is_some()
    }

    /// PUT (no body) — returns the HTTP status (all `select_streams` reads), or `-1` when the
    /// request never completed. `-1` is the value the `stream.rs` wrapper this replaced always
    /// returned for a transport failure, and it is kept because a caller reading a status must not
    /// mistake "the server refused" for "nothing was sent".
    pub(super) fn put(&self, path_no_token: &str) -> i32 {
        self.send(path_no_token, Method::Put, &[]).map_or(-1, |r| r.status)
    }

    /// POST whose body carries nothing to read — /:/timeline (spec verb; params ride the query
    /// string) — reporting whether it reached the server and came back accepted.
    ///
    /// The POST twin of [`Client::get_ok`], and it exists for the same reason: this is a body-less
    /// **write**, its caller is off the main thread, and the return value is the only thing that
    /// can say the write landed. `false` covers a refused or timed-out connect as much as a
    /// rejected status — a revoked token 401s and a sleeping server answers nothing at all, and
    /// neither of those may read as a committed resume point.
    ///
    /// There is no discarding `post_void` beside it (as `get_void` sits beside `get_ok`): the
    /// timeline is the only body-less POST in the layer, so a twin that dropped the outcome would
    /// be a method with no callers.
    pub(super) fn post_ok(&self, path_no_token: &str) -> bool {
        self.body_2xx(path_no_token, Method::Post, &[]).is_some()
    }

    /// POST → parse the `{ "MediaContainer": … }` envelope — /playQueues (the returned ids).
    pub(super) fn post_json(&self, path_no_token: &str) -> Option<MediaContainer> {
        let body = self.body_2xx(path_no_token, Method::Post, &[ACCEPT_JSON])?;
        serde_json::from_slice::<Envelope>(&body).ok().map(|e| e.media_container)
    }

    /// THE token choke point. Appends `X-Plex-Token=…` with the right separator.
    pub(super) fn with_token(&self, path: &str) -> String {
        let sep = if path.contains('?') { '&' } else { '?' };
        let tok = self.token.read().map(|g| g.clone()).unwrap_or_default();
        format!("{path}{sep}X-Plex-Token={tok}")
    }
}

// The singleton that used to live here — `static PLEX: OnceLock<Client>`, `TOKEN_GEN`, `install`,
// `client`, `client_opt` — is now the registry in `servers.rs`. `client()`/`client_opt()` keep
// their exact signatures there and mean "the CURRENT server", so no call site outside `plex`
// changed.

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
    pub(super) fn opt_str(self, k: &str, v: &str) -> Self {
        if !v.is_empty() {
            self.str(k, v)
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
/// passes its origin and path straight to `crate::stream::http_open`. Range headers for seeks
/// are added by the player as `http_open`'s `extra`, never by this layer. `path` includes the
/// `?query&X-Plex-Token`.
pub struct StreamUrl {
    /// Where the bytes come from ([`Origin`]) — copied from the [`Client`] that built this, so a
    /// stream target can never disagree with the control plane about which scheme, host and port
    /// it means.
    pub origin: Origin,
    pub path: String,
}

impl StreamUrl {
    /// The full `{scheme}://{authority}{path}` form — what `route` stores as the playback URL
    /// (the engine later splits it back with [`StreamUrl::parse`]).
    ///
    /// Byte-identical to the `format!("http://{host}:{port}{path}")` it replaced for a legacy
    /// plaintext origin, and correct for the two shapes that spelling could not express: an https
    /// origin, and a v6 literal (which must be BRACKETED in a URL and BARE at the resolver — see
    /// [`Origin`]).
    pub fn to_url(&self) -> String {
        format!("{}{}", self.origin.base(), self.path)
    }

    /// Parse an EXTERNAL full URL (the `/tmp/plxnative-url` override) back into parts —
    /// replaces `player::engine::parse_stream_url`.
    ///
    /// **Total**, because its caller has no failure path: a garbage override has to come back as
    /// *something* to fail on at `http_open`. That is why it goes through [`super::origin::split`]
    /// rather than [`Origin::parse`] — the defaults are the ones this function has always had (no
    /// scheme means `http`, no port means 32400), and an undialable port becomes the default
    /// instead of wrapping into one nobody wrote down.
    pub fn parse(url: &str) -> StreamUrl {
        let (origin, path) = super::origin::split(url);
        StreamUrl { origin, path: if path.is_empty() { "/".into() } else { path.into() } }
    }

    /// The host to DIAL — bare, never bracketed. `crate::stream` takes this; a URL takes
    /// [`StreamUrl::to_url`].
    pub fn host(&self) -> &str {
        self.origin.host()
    }
    pub fn port(&self) -> i32 {
        self.origin.port()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_client(machine: &str, token: &str) -> Client {
        Client::new(ServerId::from_raw(3), machine, Origin::http("10.0.0.1", 32400), token, "cid-42")
    }

    /// A `Client` is one server's identity plus its token, and every piece of it now arrives
    /// through the constructor — including the device id, which used to be read from the session
    /// FILE in here (a read that can also write). Nothing is resolved behind the caller's back,
    /// which is what makes the registry able to build one per server on a cold path.
    #[test]
    fn a_client_carries_the_identity_it_was_built_with() {
        let c = a_client("mach-A", "tok-a");
        assert_eq!((c.host(), c.port()), ("10.0.0.1", 32400));
        assert_eq!(c.origin().base(), "http://10.0.0.1:32400", "…and those two are one value now");
        assert_eq!(c.machine_id(), "mach-A");
        assert_eq!(c.id(), ServerId::from_raw(3));
        // the token choke point picks the right separator either way
        assert_eq!(c.with_token("/library/sections"), "/library/sections?X-Plex-Token=tok-a");
        assert_eq!(c.with_token("/x?a=1"), "/x?a=1&X-Plex-Token=tok-a");
        // the device id that was passed in is the one that goes on the wire
        assert!(c
            .playback_identity(QueryBuilder::new("/p"))
            .build()
            .contains("X-Plex-Client-Identifier=cid-42"));
        let headers = pms_headers(&[ACCEPT_JSON]);
        let sent = headers.iter().find_map(|h| h.strip_prefix("X-Plex-Language: "));
        match super::super::identity::language() {
            Some(language) => assert_eq!(sent, Some(language)),
            None => assert_eq!(sent, None),
        }
        // a client built outside the registry says so rather than claiming slot 0
        assert!(!Client::new(ServerId::UNSET, "", Origin::http("1.2.3.4", 32400), "t", "cid").id().is_set());
    }

    /// A fresh client knows nothing about how it is reached, and says so rather than guessing a
    /// tier. That default is load-bearing: `transcoder::link_policy` reads `None` as "no
    /// restriction", so every client built before an activation path exists plays exactly as it
    /// did — and a client that IS told it is on a relay carries that fact to the resolve worker
    /// without the worker having to ask a global which server is current.
    #[test]
    fn a_client_starts_with_no_known_link_and_remembers_the_one_it_is_told() {
        let c = a_client("mach-A", "tok-a");
        assert_eq!(c.link(), None, "nothing has probed anything yet");

        for l in [Location::Local, Location::Remote, Location::Relay] {
            c.set_link(l);
            assert_eq!(c.link(), Some(l), "the tier must round trip through the atomic");
        }
    }

    /// The token generation is per-CLIENT (it was a process-global `TOKEN_GEN`). Two properties
    /// matter to `posters::poster_key`'s token-baked memo, which is the only reader: a swap must
    /// MOVE this server's number and no other's, and two servers must never share a value — the
    /// memo compares one number, so identical generations across servers would let server B be
    /// served server A's memoised, token-bearing paths.
    #[test]
    fn a_token_swap_moves_this_clients_generation_and_no_other() {
        let (a, b) = (a_client("mach-A", "tok-a"), a_client("mach-B", "tok-b"));
        let (ga, gb) = (a.token_gen(), b.token_gen());
        assert_ne!(ga, gb, "distinct clients, distinct generations");

        a.set_token("tok-a2");
        assert_eq!(a.with_token("/x"), "/x?X-Plex-Token=tok-a2");
        assert_ne!(a.token_gen(), ga, "the swapped client's generation moved");
        assert_eq!(b.token_gen(), gb, "the other client's did not");
    }

    /// **A client's ORIGIN is the whole address; `host()`/`port()` are views on it.** Those two
    /// accessors are what ~30 call sites below this layer read, so they must keep answering
    /// exactly what they did as plain fields — including for the shapes the pair could not spell.
    #[test]
    fn a_clients_host_and_port_are_views_on_its_origin() {
        let tls = Origin::parse("https://nas.hash.plex.direct:32400").expect("parses");
        let c = Client::new(ServerId::UNSET, "m", tls, "t", "cid");
        assert_eq!(c.host(), "nas.hash.plex.direct", "the NAME a certificate is validated against");
        assert_eq!(c.port(), 32400);
        assert!(c.origin().is_tls());

        // a v6 origin: bare at the resolver, bracketed in a URL. `host()` is the resolver's half.
        let v6 = Client::new(ServerId::UNSET, "m", Origin::http("2001:db8::1", 32400), "t", "cid");
        assert_eq!(v6.host(), "2001:db8::1");
        assert_eq!(v6.origin().authority(), "[2001:db8::1]:32400");
    }

    /// `StreamUrl`'s two halves answer two different questions: [`StreamUrl::to_url`] is what
    /// `route` STORES (a URL string) and [`StreamUrl::host`] is what `crate::stream` DIALS. The
    /// engine really does round-trip the stored string through [`StreamUrl::parse`], so that trip
    /// is graded rather than assumed — and the `to_url` bytes are pinned, because a change there
    /// is a change to every playback URL in the app.
    #[test]
    fn a_stream_url_round_trips_through_the_string_route_stores() {
        let su = StreamUrl { origin: Origin::http("10.0.0.1", 32400), path: "/library/parts/9?X-Plex-Token=t".into() };
        assert_eq!(su.to_url(), "http://10.0.0.1:32400/library/parts/9?X-Plex-Token=t");

        let back = StreamUrl::parse(&su.to_url());
        assert_eq!((back.host(), back.port()), ("10.0.0.1", 32400));
        assert_eq!(back.path, su.path);
        assert_eq!(back.to_url(), su.to_url());
    }

    /// The two shapes `StreamUrl` could not previously express. Nothing calls it with either yet —
    /// they are here so the transport lane inherits a splitter that is already right, and so the
    /// v6 case cannot regress to the old reading, which took `[` as the entire host.
    #[test]
    fn a_stream_url_understands_https_and_a_bracketed_v6_literal() {
        let s = StreamUrl::parse("https://nas.hash.plex.direct:32400/video/x?a=1");
        assert!(s.origin.is_tls());
        assert_eq!((s.host(), s.port(), s.path.as_str()), ("nas.hash.plex.direct", 32400, "/video/x?a=1"));
        assert_eq!(s.to_url(), "https://nas.hash.plex.direct:32400/video/x?a=1");

        let v6 = StreamUrl::parse("http://[2001:db8::1]:32400/p");
        assert_eq!(v6.host(), "2001:db8::1", "the resolver never sees the brackets");
        assert_eq!(v6.to_url(), "http://[2001:db8::1]:32400/p", "…and the URL always carries them");
    }

    /// The defaults `player::engine::parse_stream_url` had and this inherited: no scheme means
    /// http, no port means 32400, and no path at all means `/` — a bare host is a request for the
    /// server's root, not for the empty string. The `/tmp/plxnative-url` override is written by
    /// hand, so all three are reachable.
    #[test]
    fn a_stream_url_keeps_the_defaults_the_override_trigger_relies_on() {
        let bare = StreamUrl::parse("192.0.2.10");
        assert_eq!((bare.host(), bare.port(), bare.path.as_str()), ("192.0.2.10", 32400, "/"));
        assert_eq!(StreamUrl::parse("192.0.2.10/x").port(), 32400);
        assert_eq!(StreamUrl::parse("http://192.0.2.10:8020/f.mkv").port(), 8020);
    }
}
