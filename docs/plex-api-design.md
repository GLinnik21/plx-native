# Plex API layer — typed Rust design (`rust-modules/src/plex/`)

Goal: **replace every hand-built Plex path/query string in the app with one typed method per
operation.** After this lands, no module outside `plex/` ever writes `format!("/library/…")`,
`&X-Plex-Token=`, `%2F`, or `http://host:port/…`. The catalog (`docs/plex-api-catalog.md`) is
the operation list this design implements; `docs/pms-api.md` + `docs/plex-openapi.json` are the
field/shape sources.

Hard constraints (unchanged):

- **Transport is the existing raw socket in `stream.rs` only** — `http_get(host,port,path,extra)`,
  `http_put(host,port,path)`, `http_open(hs,ip,port,path,extra,method)`. **No reqwest/hyper/DNS.**
  `host`/`ip` is a **numeric dotted-quad** (the socket does no name resolution); the `plex` layer
  carries that numeric host verbatim and never resolves.
- Chunked decoding, Range headers, and the 64 KB `HttpStream` box stay where they are. The `plex`
  layer produces **paths and stream targets**; the transport owns the socket.

---

## 1. Layout — three alternatives, and the pick

**Alternative A — flat `impl Client`, methods grouped into per-tag files (CHOSEN).**
One `Client` type; each op file (`library.rs`, `hubs.rs`, `transcoder.rs`, `timeline.rs`) adds a
separate `impl Client { … }` block. Call surface is flat: `client.sections()`,
`client.metadata(rk)`, `client.timeline(&r)`. Rust allows multiple `impl` blocks for one type
across files in a crate, so the methods are physically grouped by tag while presenting a single,
discoverable, typed surface. Maps 1:1 to "one typed method per operation" with zero ceremony.

**Alternative B — tag sub-structs / extension traits** (`client.library().sections()`, or
`trait LibraryApi for Client`). Namespaces the surface but adds a borrowed handle type or a trait
per tag, plus imports at every call site. The app has ~15 operations total — the namespacing buys
nothing and costs boilerplate.

**Alternative C — free functions taking `&Client`** (`library::sections(&client)`). Least
idiomatic; every call site must import the op module and thread `&client`. No method discovery.

**Decision: A.** Fewest moving parts, one immutable `Client`, flat typed call surface, files still
split by tag. Percent-encoding and token injection live once inside the `Client`'s private
transport helpers, so no `impl Client` block in any op file can bypass them.

---

## 2. Module tree

```
rust-modules/src/plex/
├── mod.rs          // pub use Client, models::*, StreamUrl, params; declares submodules
├── client.rs       // Client { host, port, token, client_id, product, version, platform }
│                   //   + PRIVATE choke points: enc(), QueryBuilder, get_json/get_bytes/
│                   //     get_void/put/post, with_token(); + StreamUrl; + the shared singleton
├── models.rs       // serde response structs: MediaContainer, Metadata, Hub, Media,
│                   //   MediaPart, Stream, Tag, LibrarySection, UltraBlurColors, Envelope<T>
├── params.rs       // typed request params: TranscodeSpec, TimelineReport, TimelineState,
│                   //   StreamSelection
├── library.rs      // impl Client: sections, section_items(_paged), metadata(_many),
│                   //   children, all_leaves, related, select_streams (PUT parts),
│                   //   direct_play_url
├── hubs.rs         // impl Client: home_hubs, continue_watching, promoted, search
├── transcoder.rs   // impl Client: transcode_decision, transcode_start_url,
│                   //   transcode_subtitles_url, transcode_stop, image_transcode_path
└── timeline.rs     // impl Client: timeline (POST /:/timeline)
```

`params.rs` and the `enc`/`QueryBuilder` staying **inside `client.rs`** are the only additions
beyond the requested `mod/client/models + library/hubs/transcoder/timeline`. Keeping the encoder
and query builder private to `client.rs` is deliberate: they are the centralization guarantee, so
no op file can reach around them.

Wiring in `lib.rs`: add `mod plex;`. Delete `pms::urlenc_str` (moves to `plex::client::enc`),
`route::parse_stream_url`/`engine::parse_stream_url` (moves to `StreamUrl::parse`), and every
inline path `format!` in `pms.rs`, `metadata.rs`, `posters.rs`, `route.rs`, `player/threads.rs`,
`player/engine.rs` (see §8 migration map).

---

## 3. The `Client` (client.rs)

```rust
/// Immutable after construction. Cheap to share by &ref across threads (posters
/// workers, the timeline reporter, the detail loader all read it). Host is a numeric
/// dotted-quad — the raw socket does no DNS.
pub struct Client {
    host: String,      // "192.168.0.3"  (numeric; passed straight to http_get/http_open)
    port: i32,         // 32400
    token: String,     // X-Plex-Token value
    client_id: String, // X-Plex-Client-Identifier — stable device id ("com.glin.plexpoc")
    product: String,   // "plexpoc"
    version: String,   // "1"
    platform: String,  // "Generic"
}

impl Client {
    pub fn new(host: &str, port: i32, token: &str) -> Client {
        Client {
            host: host.to_owned(), port, token: token.to_owned(),
            client_id: "com.glin.plexpoc".into(),
            product: "plexpoc".into(), version: "1".into(), platform: "Generic".into(),
        }
    }
    pub fn host(&self) -> &str { &self.host }
    pub fn port(&self) -> i32 { self.port }
}

// ---- shared singleton (built once in plex_run, read everywhere) ----
use std::sync::OnceLock;
static PLEX: OnceLock<Client> = OnceLock::new();
pub fn init(host: &str, port: i32, token: &str) { let _ = PLEX.set(Client::new(host, port, token)); }
pub fn client() -> &'static Client { PLEX.get().expect("plex::init not called") }
```

This retires the three duplicate copies of `(host, port, token)` currently living in
`route::CFG`, `posters::Store`, and the `pms_fetch_*` / `metadata` arguments. `demo_url` stays in
`route` (it is app config, not a Plex operation).

### 3.1 Private transport helpers — the only code that touches `stream::*`

```rust
const ACCEPT_JSON: &str = "Accept: application/json\r\n";

impl Client {
    /// GET → parse the `{ "MediaContainer": … }` envelope into the flat container.
    fn get_json(&self, path_no_token: &str) -> Option<MediaContainer> {
        let path = self.with_token(path_no_token);
        let body = crate::stream::http_get(&self.host, self.port, &path, Some(ACCEPT_JSON))?;
        serde_json::from_slice::<Envelope>(&body).ok().map(|e| e.media_container)
    }
    /// GET raw bytes (image transcode) — caller decodes (img.rs).
    fn get_bytes(&self, path_no_token: &str) -> Option<Vec<u8>> {
        crate::stream::http_get(&self.host, self.port, &self.with_token(path_no_token), None)
    }
    /// GET whose body is discarded (transcode decision / stop registration side effects).
    fn get_void(&self, path_no_token: &str) {
        let _ = crate::stream::http_get(&self.host, self.port, &self.with_token(path_no_token), None);
    }
    /// PUT (no body) — returns the HTTP status (all `select_streams` reads).
    fn put(&self, path_no_token: &str) -> i32 {
        crate::stream::http_put(&self.host, self.port, &self.with_token(path_no_token))
    }
    /// POST (no body) — /:/timeline. `http_post` == `http_open(..,"POST")` mirroring http_put,
    /// a ~10-line addition to stream.rs that reuses the same raw socket (no new deps).
    fn post(&self, path_no_token: &str) -> i32 {
        crate::stream::http_post(&self.host, self.port, &self.with_token(path_no_token))
    }
    /// THE token choke point. Appends `X-Plex-Token=…` with the right separator.
    fn with_token(&self, path: &str) -> String {
        let sep = if path.contains('?') { '&' } else { '?' };
        format!("{path}{sep}X-Plex-Token={}", self.token)
    }
}
```

### 3.2 `enc` + `QueryBuilder` — the percent-encoding choke point (private, client.rs)

```rust
/// RFC3986-unreserved passthrough; everything else → %XX (moved verbatim from pms::urlenc_str).
pub(crate) fn enc(src: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(src.len());
    for &c in src.as_bytes() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else { out.push('%'); out.push(HEX[(c >> 4) as usize] as char); out.push(HEX[(c & 15) as usize] as char); }
    }
    out
}

/// Builds `path?k=enc(v)&k2=42…`. `.str` percent-encodes the value; `.int` does not
/// (digits are unreserved). No op module ever formats a query by hand.
pub(crate) struct QueryBuilder { path: String, parts: Vec<String> }
impl QueryBuilder {
    pub fn new(path: impl Into<String>) -> Self { Self { path: path.into(), parts: Vec::new() } }
    pub fn str(mut self, k: &str, v: &str) -> Self { self.parts.push(format!("{k}={}", enc(v))); self }
    pub fn int(mut self, k: &str, v: i64) -> Self { self.parts.push(format!("{k}={v}")); self }
    pub fn opt_int(self, k: &str, v: i64) -> Self { if v != 0 { self.int(k, v) } else { self } }
    pub fn build(self) -> String {
        if self.parts.is_empty() { self.path } else { format!("{}?{}", self.path, self.parts.join("&")) }
    }
}
```

Path segments that carry an id (`/library/metadata/{rk}`, `/library/parts/{id}`) use `{rk}`
directly — `rating_key`/`part_id` are numeric/`ratingKey` strings from PMS and are never
user-typed, so they need no encoding; only **query values** and the transcode `path=` value are
`enc()`d. (If a future non-numeric key appears, wrap the segment in `enc` too — one call site.)

### 3.3 `StreamUrl` — the streaming return type (client.rs)

```rust
/// A built playback target for the raw demux/cue sockets. NOT a fetched response —
/// the player passes these three fields straight to stream::http_open. Range headers
/// for seeks are added by the player as http_open's `extra`, never by this layer.
pub struct StreamUrl { pub host: String, pub port: i32, pub path: String } // path includes ?query&token

impl StreamUrl {
    /// "http://host:port/path" for route::URL storage, SHARED.next_url, and logs.
    pub fn to_url(&self) -> String { format!("http://{}:{}{}", self.host, self.port, self.path) }
    /// Parse an EXTERNAL full URL (demo_url, /tmp/poc-url override) back into parts —
    /// replaces player::engine::parse_stream_url (same behavior: default port 32400).
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
            StreamUrl { host, port: 32400, path: if rest.is_empty() { "/".into() } else { rest.into() } }
        }
    }
}
```

---

## 4. Transport-integration rules (the contract)

1. **JSON reads** (library/hubs/metadata/related/children) → `Client::get_json` →
   `stream::http_get` + `serde_json::from_slice::<Envelope>` → typed. No op writes the socket.
2. **Token** appears in exactly one function, `with_token`. No op writes `X-Plex-Token`.
3. **Percent-encoding** happens in exactly one function, `enc` (via `QueryBuilder::str` and the
   transcode `path=`). No op writes `%2F` or calls a URL encoder.
4. **Streaming ops do not touch the socket.** `transcode_start_url`, `direct_play_url`,
   `transcode_subtitles_url` return `StreamUrl { host, port, path }`. The player feeds
   `url.host` (numeric) / `url.port` / `url.path` to `stream::http_open`. **Byte-Range headers
   stay in the player** (passed as http_open's `extra`) — the Plex layer never encodes a Range.
5. **One `StreamUrl` per playback serves both the demux and the cue-preflight sockets** — they
   open the *same* part URL (the cue thread just re-opens it header-only, then Range-fetches the
   Cues). The Plex layer is called once; the two sockets are a transport detail.
6. **Side-effecting registration** (`transcode_decision`, `transcode_stop`) uses `get_void` and
   returns `()`/`bool` — never a `StreamUrl`, never leaks a path to the caller.
7. **Stream selection** (`select_streams`) uses `Client::put` and returns the HTTP status `i32`
   (the only thing the app reads).
8. **Timeline** uses `Client::post` (POST, spec-correct — fixes catalog D-8a). `http_post` is a
   thin `http_open(…, "POST")` added to `stream.rs`, reusing the raw socket. (If we choose to keep
   the current known-working GET, swap `post`→`get_void`; the method signature is unchanged.)
9. **Numeric host, no DNS**: `Client::host` and every `StreamUrl::host` are the numeric dotted-quad
   from boot config. `StreamUrl::parse` preserves whatever host the external URL carried (already
   numeric in demo_url / poc-url).
10. **Image transcode is the one method that returns a path `String`** — because `posters.rs` uses
    that exact path as its LRU cache **key** and then http_get's it itself (fetch + decode belong
    to the poster worker, not the Client). Encoding is still centralized in `enc`; the caller still
    writes no path. This is the sole, justified exception to rule 4's "return a target, not a path."

---

## 5. Client API surface (fn signatures)

### library.rs — `impl Client`
```rust
/// GET /library/sections  (D-3: spec-canonical is /library/sections/all; keep the
/// known-working bare path). Read `.directory[]` for {type, key}.
pub fn sections(&self) -> Option<MediaContainer>;

/// GET /library/sections/{section_key}/all  → `.metadata[]`
pub fn section_items(&self, section_key: i64) -> Option<MediaContainer>;
/// paged variant (X-Plex-Container-Start/Size) for large libraries
pub fn section_items_paged(&self, section_key: i64, start: i64, size: i64) -> Option<MediaContainer>;

/// GET /library/metadata/{rating_key}  → the single item (`.metadata[0]`), or None
pub fn metadata(&self, rating_key: &str) -> Option<Metadata>;
/// GET /library/metadata/{csv}  — batch (spec `ids` is a CSV array); `.metadata[]`
pub fn metadata_many(&self, rating_keys: &[&str]) -> Option<MediaContainer>;

/// GET /library/metadata/{rating_key}/children  (D-5 undocumented but real) → `.metadata[]`
pub fn children(&self, rating_key: &str) -> Option<MediaContainer>;
/// GET /library/metadata/{rating_key}/allLeaves — all episodes in one call (adopt; replaces
/// the per-season children loop). Group client-side by `parent_index`.
pub fn all_leaves(&self, rating_key: &str) -> Option<MediaContainer>;

/// GET /library/metadata/{rating_key}/related  → `.hub[]`
pub fn related(&self, rating_key: &str) -> Option<MediaContainer>;

/// PUT /library/parts/{part_id}?allParts=1&subtitleStreamID=…[&audioStreamID=…]
/// The correct server-side stream selection (catalog #8). Returns the HTTP status.
pub fn select_streams(&self, sel: &StreamSelection) -> i32;

/// Direct-play target: http://host:port{part_key}?X-Plex-Token — for stream::http_open.
/// `part_key` is the verbatim Media.Part.key from metadata.
pub fn direct_play_url(&self, part_key: &str) -> StreamUrl;
```

### hubs.rs — `impl Client`
```rust
/// GET /hubs?count=…  (D-4: drop the no-op excludeContinueWatching) → `.hub[]`
pub fn home_hubs(&self, count: i64) -> Option<MediaContainer>;
/// GET /hubs/continueWatching?count=…  (adopt; cleaner than the /hubs hack)
pub fn continue_watching(&self, count: i64) -> Option<MediaContainer>;
/// GET /hubs/promoted?count=…  (adopt; the home screen's featured rows)
pub fn promoted(&self, count: i64) -> Option<MediaContainer>;
/// GET /hubs/search?query=…&limit=…  (adopt; a real search screen) → `.hub[]`
pub fn search(&self, query: &str, limit: i64) -> Option<MediaContainer>;
```

### transcoder.rs — `impl Client`
```rust
/// GET /video/:/transcode/universal/decision?… — REGISTERS the session (body discarded).
/// Call before transcode_start_url. Carries the offset for a seek/re-transcode.
pub fn transcode_decision(&self, spec: &TranscodeSpec);

/// GET /video/:/transcode/universal/start.mkv?… — the progressive H264+AC3 Matroska the
/// demuxer eats. Returns the StreamUrl for http_open (NOT fetched here).
pub fn transcode_start_url(&self, spec: &TranscodeSpec) -> StreamUrl;

/// GET /video/:/transcode/universal/subtitles?… — soft (SRT/VTT) subs for client-side
/// rendering instead of burn-in (adopt; avoids a re-transcode per subtitle change).
/// Returns a StreamUrl (open with http_open, or http_get for the small body).
pub fn transcode_subtitles_url(&self, spec: &TranscodeSpec) -> StreamUrl;

/// GET /video/:/transcode/universal/stop?session=… — free the encoder (D-9 undocumented;
/// body discarded).
pub fn transcode_stop(&self, session: &str);

/// GET /photo/:/transcode?width&height&minSize=1&url=…[&format=png]&X-Plex-Token — returns
/// the built PATH (the sole path-returning method): posters uses it as the LRU key AND the
/// http_get path. `src_path` is the raw thumb/art path; encoding is centralized in enc().
pub fn image_transcode_path(&self, src_path: &str, w: i64, h: i64, png: bool) -> String;
```

The transcode `session` token, `X-Plex-Client-Identifier`, `X-Plex-Product/Version/Platform`, and
`X-Plex-Client-Profile-Extra` are assembled **inside** `transcode_decision`/`transcode_start_url`
from `self` + `spec` (currently hand-built in `route::transcode_base`). `session` is derived as
`format!("plexpoc-{}", spec.rating_key)`.

### timeline.rs — `impl Client`
```rust
/// POST /:/timeline?ratingKey&key&state&time&duration (spec verb POST — fixes D-8a). The
/// key= value (/library/metadata/{rk}) is enc()'d internally. Fire-and-forget.
pub fn timeline(&self, report: &TimelineReport);
```

---

## 6. Request params (params.rs)

```rust
pub struct StreamSelection {
    pub part_id: i64,
    pub audio_stream_id: i64,    // 0 = keep server default (omit from query)
    pub subtitle_stream_id: i64, // 0 = subs OFF (always sent; 0 disables burn)
    pub all_parts: bool,         // true → allParts=1
}

/// Everything transcode_decision + transcode_start_url need. The Client fills in token,
/// client_id, product, version, platform, session, and the fixed profile string.
pub struct TranscodeSpec<'a> {
    pub rating_key: &'a str,
    pub audio_stream_id: i64,     // 0 = server default
    pub subtitle_stream_id: i64,  // 0 = none (burn when >0)
    pub offset_secs: i64,         // 0 = from start; >0 = seek/re-transcode point
    pub max_video_bitrate: i64,   // 20000 (D-6a: spec name is videoBitrate; keep legacy honored)
    pub video_resolution: &'a str,// "1920x1080"
}

pub enum TimelineState { Playing, Paused, Stopped, Buffering }
impl TimelineState { fn as_str(&self) -> &'static str { /* "playing" | "paused" | "stopped" | "buffering" */ } }

pub struct TimelineReport<'a> {
    pub rating_key: &'a str,
    pub state: TimelineState,
    pub time_ms: i64,
    pub duration_ms: i64,
}
```

---

## 7. Response models (models.rs) — only the fields the app consumes

```rust
use serde::Deserialize;

/// { "MediaContainer": { … } } — every list/detail response.
#[derive(Deserialize)]
pub struct Envelope {
    #[serde(rename = "MediaContainer", default)]
    pub media_container: MediaContainer,
}

/// One flat container; every list field is optional so the same type deserializes a
/// sections list (`Directory`), an items/detail list (`Metadata`), or a hub list (`Hub`).
#[derive(Deserialize, Default)]
pub struct MediaContainer {
    #[serde(rename = "Directory", default)] pub directory: Vec<LibrarySection>,
    #[serde(rename = "Metadata",  default)] pub metadata:  Vec<Metadata>,
    #[serde(rename = "Hub",       default)] pub hub:       Vec<Hub>,
    #[serde(default)] pub size: i64,
    #[serde(rename = "totalSize", default)] pub total_size: i64,
    #[serde(default)] pub offset: i64,
}

#[derive(Deserialize, Default)]
pub struct LibrarySection {
    #[serde(default)] pub key: String,          // "1" — parse to i64 at the call site (keep raw)
    #[serde(rename = "type", default)] pub kind: String, // movie | show | artist
    #[serde(default)] pub title: String,
}

#[derive(Deserialize, Default)]
pub struct Hub {
    #[serde(rename = "type", default)] pub kind: String,
    #[serde(default)] pub title: String,
    #[serde(rename = "Metadata", default)] pub metadata: Vec<Metadata>,
}

/// The movie/show/season/episode item. Missing fields default (Plex omits optionals).
#[derive(Deserialize, Default)]
pub struct Metadata {
    #[serde(rename = "type", default)] pub kind: String,       // movie|show|season|episode|clip
    #[serde(rename = "ratingKey", default)] pub rating_key: String,
    #[serde(default)] pub title: String,
    #[serde(default)] pub year: i64,
    #[serde(rename = "contentRating", default)] pub content_rating: String,
    #[serde(default)] pub summary: String,
    #[serde(default)] pub tagline: String,
    #[serde(default)] pub studio: String,
    #[serde(rename = "originallyAvailableAt", default)] pub originally_available_at: String,
    #[serde(default)] pub duration: i64,   // ms
    #[serde(rename = "viewOffset", default)] pub view_offset: i64, // ms; resume point
    #[serde(default)] pub index: i64,       // season/episode number
    #[serde(rename = "parentIndex", default)] pub parent_index: i64,
    #[serde(rename = "leafCount", default)] pub leaf_count: i64,
    #[serde(default)] pub thumb: String,
    #[serde(default)] pub art: String,
    #[serde(rename = "grandparentThumb", default)] pub grandparent_thumb: String,
    #[serde(rename = "Media",    default)] pub media:   Vec<Media>,
    #[serde(rename = "Genre",    default)] pub genre:   Vec<Tag>,
    #[serde(rename = "Country",  default)] pub country: Vec<Tag>,
    #[serde(rename = "Director", default)] pub director: Vec<Tag>,
    #[serde(rename = "Writer",   default)] pub writer:  Vec<Tag>,
    #[serde(rename = "Role",     default)] pub role:    Vec<Tag>,
    // D-1: PMS returns UltraBlurColors as an ARRAY `[{…}]` (the current code reads it as an
    // object and misses it). `de_ultrablur` accepts object OR array and yields the first.
    #[serde(rename = "UltraBlurColors", default, deserialize_with = "de_ultrablur")]
    pub ultra_blur_colors: Option<UltraBlurColors>,
}

#[derive(Deserialize, Default)]
pub struct Media {
    #[serde(rename = "videoCodec", default)] pub video_codec: String,
    #[serde(rename = "audioCodec", default)] pub audio_codec: String,
    #[serde(rename = "Part", default)] pub part: Vec<MediaPart>,
}

#[derive(Deserialize, Default)]
pub struct MediaPart {
    #[serde(default)] pub id: i64,
    #[serde(default)] pub key: String, // /library/parts/{id}/{changestamp}/file.mkv
    #[serde(rename = "Stream", default)] pub stream: Vec<Stream>,
}

/// D-2: channels/title/hearingImpaired/audioDescription/forced are real PMS fields the
/// spec omits — kept here. Plex 0/1 booleans stay i64; the app tests `!= 0`.
#[derive(Deserialize, Default)]
pub struct Stream {
    #[serde(default)] pub id: i64,
    #[serde(rename = "streamType", default)] pub stream_type: i64, // 1 video, 2 audio, 3 subtitle
    #[serde(default)] pub codec: String,
    #[serde(default)] pub language: String,
    #[serde(default)] pub channels: i64,
    #[serde(rename = "audioChannelLayout", default)] pub audio_channel_layout: String,
    #[serde(rename = "displayTitle", default)] pub display_title: String,
    #[serde(default)] pub title: String,
    #[serde(rename = "hearingImpaired", default)] pub hearing_impaired: i64,
    #[serde(rename = "audioDescription", default)] pub audio_description: i64,
    #[serde(default)] pub forced: i64,
}

#[derive(Deserialize, Default)]
pub struct Tag {
    #[serde(default)] pub tag: String,
    #[serde(default)] pub role: String,   // Role[] only (character name)
    #[serde(default)] pub thumb: String,  // Role[] headshot
}

#[derive(Deserialize, Default, Clone, Copy)]
pub struct UltraBlurColors {
    #[serde(rename = "topLeft",     default)] pub top_left: HexColor,
    #[serde(rename = "topRight",    default)] pub top_right: HexColor,
    #[serde(rename = "bottomRight", default)] pub bottom_right: HexColor,
    #[serde(rename = "bottomLeft",  default)] pub bottom_left: HexColor,
}

/// "1a2b3c" (with/without '#') → linear [r,g,b] 0..1. Replaces pms::hex3.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(from = "String")]
pub struct HexColor(pub [f32; 3]);
impl From<String> for HexColor {
    fn from(s: String) -> Self {
        let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0);
        HexColor([((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0])
    }
}

/// Accept `{…}` OR `[{…}]` (D-1) and return the first, or None.
fn de_ultrablur<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<UltraBlurColors>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany { One(UltraBlurColors), Many(Vec<UltraBlurColors>) }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        Some(OneOrMany::One(u)) => Some(u),
        Some(OneOrMany::Many(v)) => v.into_iter().next(),
        None => None,
    })
}
```

Notes:
- **`#[serde(default)]` everywhere** matches the "all optional" reality of PMS JSON — no field is
  required, so a trimmed response never fails to deserialize (the current string-scrape tolerates
  missing keys; this keeps that tolerance).
- **`kind`** is used for the `type` field throughout (`type` is a Rust keyword). One rename convention.
- Models are **read-only DTOs**: the app's own view structs (`pms::PmsMovie` fixed C buffers,
  `metadata::Detail`) are populated *from* these — the `plex` layer does not own UI state.

---

## 8. Migration map — current raw sites → typed calls

| current site | raw string today | replacement |
|---|---|---|
| `pms::pms_fetch_movies` | `/library/sections?X-Plex-Token=` | `client().sections()` → iterate `.directory` |
| `pms::fetch_section` | `/library/sections/{sec}/all?…` | `client().section_items(sec)` → `.metadata` |
| `pms::pms_fetch_hubs` | `/hubs?count=12&excludeContinueWatching=0&…` | `client().home_hubs(12)` → `.hub` |
| `pms::urlenc_str` | (encoder) | **deleted** → `plex::client::enc` |
| `pms::hex3`, `parse_item` blur | inline hex + object read | `Metadata.ultra_blur_colors` (`HexColor`) |
| `metadata::fetch_detail` / `fetch_item_streams` | `/library/metadata/{rk}?…` | `client().metadata(rk)` |
| `metadata::fetch_seasons` / `fetch_episodes` | `/library/metadata/{rk}/children?…` | `client().children(rk)` (or `all_leaves`) |
| `metadata::fetch_related` | `/library/metadata/{rk}/related?…` | `client().related(rk)` → `.hub` |
| `posters::poster_key` | `/photo/:/transcode?…&X-Plex-Token=` | `client().image_transcode_path(src,w,h,png)` |
| `route::transcode_base` + `build_stream` decision | `/video/:/transcode/universal/decision?…` | `client().transcode_decision(&spec)` |
| `route::build_stream` / `transcode_seek` / `retranscode` | `…/start.mkv?…` (full URL) | `client().transcode_start_url(&spec)` → `.to_url()` into `route::URL` |
| `route::build_stream` direct-play | `http://host:port{part}?X-Plex-Token=` | `client().direct_play_url(part_key)` |
| `route::stop_transcode` | `…/universal/stop?session=…` | `client().transcode_stop(&session)` |
| `route::put_selection` | `PUT /library/parts/{id}?…` | `client().select_streams(&sel)` |
| `player::engine::parse_stream_url` / `route` re-parse | manual URL split | `StreamUrl::parse` (for demo_url/poc-url); typed paths return `StreamUrl` directly |
| `player::threads::timeline_thread` + `engine` final report | `GET /:/timeline?…` | `client().timeline(&report)` (POST) |
| `app::plex_run` config | `route::set_config` + `posters_init` + `pms_fetch(host,port,token)` | `plex::init(host,port,token)` once; others read `plex::client()` |

The demux/cue threads (`player::threads`) keep their `http_open` + Range logic unchanged; they now
receive a `StreamUrl` (host/port/path) instead of a `String` URL they must parse. `SHARED.next_url`
can stay `Option<String>` (store `stream_url.to_url()`), or tighten to `Option<StreamUrl>` — either
works; string keeps the diff smallest.

---

## 9. Divergences resolved by the design

- **D-1 (UltraBlurColors)** — fixed: `de_ultrablur` accepts the array shape PMS actually returns
  (the current object read misses the colors on a real server).
- **D-3 (`/library/sections`)** — kept as-is inside `sections()` (known-working); switching to
  `/library/sections/all` is a one-line change in one method.
- **D-4 (`excludeContinueWatching`)** — dropped from `home_hubs`; `continue_watching()` added.
- **D-6a/b/c, D-7** — the transcode query is assembled once in `transcoder.rs` from `TranscodeSpec`
  + `Client` identity fields, so the legacy-vs-spec param choices (`maxVideoBitrate` vs
  `videoBitrate`, `session` vs `transcodeSessionId`, query-vs-header X-Plex-\*, dropping the
  no-op `audioStreamID` on the GET) are all edited in **one place** instead of three `format!`s.
- **D-8a (timeline GET→POST)** — `timeline()` uses `Client::post`.

---

## 10. Verification (on-device, no host runtime)

No host test harness exists (the app only runs on the TV). Validate as usual:

- Deserialization is checked against `docs/plex-openapi.json` (field names/types) and the live
  JSON captured in `docs/pms-api.md`. A quick host-side `cargo test` with the sample JSON blobs
  from `pms-api.md` (§2/§3/§4) asserts each model parses and the fields the app reads are
  populated — pure `serde_json` unit tests, no socket, so they run on the build host.
- End-to-end: `make test` and read `/tmp/poc-events.log` — the home shelves, detail page,
  direct-play, transcode, seek, audio/subtitle switch, and the `/:/timeline` reports must behave
  exactly as before (the design is a refactor of path-building, not of playback semantics).
