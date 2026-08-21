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
//!
//! A `Client` is ONE server. WHICH servers exist, which one is current, and how a `&'static
//! Client` is handed out live next door in [`super::servers`] — this file is the type, that
//! file is the table.
use super::models::{Envelope, MediaContainer};
use super::probe::Location;
use super::servers::ServerId;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering::Relaxed};
use std::sync::RwLock;

const ACCEPT_JSON: &str = "Accept: application/json\r\n";

/// Immutable after construction (apart from the token + its generation, both interior-mutable).
/// Cheap to share by `&ref` across threads (poster workers, the timeline reporter, the detail
/// loader all read it). `host` is a numeric dotted-quad — the raw socket does no DNS.
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
    pub(super) host: String,      // e.g. "192.0.2.10" (numeric; passed straight to http_get/http_open)
    pub(super) port: i32,         // 32400
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

/// [`Client::link`] before anything has told us how this server is reached — the state every
/// client is in today, since nothing dials from a [`Location`] yet. Distinct from every real tier
/// on purpose: "unknown" is not "local", and the policy treats the two differently.
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
    pub(super) fn new(id: ServerId, machine_id: &str, host: &str, port: i32, token: &str, client_id: &str) -> Client {
        Client {
            id,
            machine_id: machine_id.to_owned(),
            host: host.to_owned(),
            port,
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
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> i32 {
        self.port
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
    /// (`let id = register(mid, host, port, tok); client_for(id).unwrap().set_link(l);`). A
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
        crate::stream::http_get(&self.host, self.port, path_with_token, None)
    }

    /// GET whose body is discarded (transcode decision / stop registration side effects).
    pub(super) fn get_void(&self, path_no_token: &str) {
        let _ = crate::stream::http_get(&self.host, self.port, &self.with_token(path_no_token), None);
    }

    /// [`Client::get_void`], but reporting whether the request actually reached the server and came
    /// back accepted. For the body-less **writes** whose caller is off the main thread and has no
    /// other way to know — see [`super::library::Client::scrobble`]. `false` covers a refused or
    /// timed-out connect as much as a rejected status, which is the distinction that matters here:
    /// a share that is asleep answers nothing at all.
    pub(super) fn get_ok(&self, path_no_token: &str) -> bool {
        crate::stream::http_get(&self.host, self.port, &self.with_token(path_no_token), None).is_some()
    }

    /// PUT (no body) — returns the HTTP status (all `select_streams` reads).
    pub(super) fn put(&self, path_no_token: &str) -> i32 {
        crate::stream::http_put(&self.host, self.port, &self.with_token(path_no_token))
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
        crate::stream::http_post(&self.host, self.port, &self.with_token(path_no_token), None).is_some()
    }

    /// POST → parse the `{ "MediaContainer": … }` envelope — /playQueues (the returned ids).
    pub(super) fn post_json(&self, path_no_token: &str) -> Option<MediaContainer> {
        let path = self.with_token(path_no_token);
        let body = crate::stream::http_post(&self.host, self.port, &path, Some(ACCEPT_JSON))?;
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
/// passes these three fields straight to `crate::stream::http_open`. Range headers for seeks
/// are added by the player as `http_open`'s `extra`, never by this layer. `path` includes the
/// `?query&X-Plex-Token`.
pub struct StreamUrl {
    pub host: String,
    pub port: i32,
    pub path: String,
}

impl StreamUrl {
    /// The full `http://host:port/path?…` form — what `route` stores as the playback URL
    /// (the engine later splits it back with [`StreamUrl::parse`]).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn a_client(machine: &str, token: &str) -> Client {
        Client::new(ServerId::from_raw(3), machine, "10.0.0.1", 32400, token, "cid-42")
    }

    /// A `Client` is one server's identity plus its token, and every piece of it now arrives
    /// through the constructor — including the device id, which used to be read from the session
    /// FILE in here (a read that can also write). Nothing is resolved behind the caller's back,
    /// which is what makes the registry able to build one per server on a cold path.
    #[test]
    fn a_client_carries_the_identity_it_was_built_with() {
        let c = a_client("mach-A", "tok-a");
        assert_eq!((c.host(), c.port()), ("10.0.0.1", 32400));
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
        // a client built outside the registry says so rather than claiming slot 0
        assert!(!Client::new(ServerId::UNSET, "", "1.2.3.4", 32400, "t", "cid").id().is_set());
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
}
