//! serde response DTOs — only the fields the app consumes. Everything is
//! `#[serde(default)]` to mirror the "all optional" reality of PMS JSON (a trimmed
//! response never fails to deserialize). `kind` renames the JSON `type` field (a Rust
//! keyword). These are read-only: the app's own view structs are populated *from* them.
use serde::Deserialize;

/// `{ "MediaContainer": … }` — every list/detail response.
#[derive(Deserialize, Default)]
pub struct Envelope {
    #[serde(rename = "MediaContainer", default)]
    pub media_container: MediaContainer,
}

/// One flat container; every list field is optional so the same type deserializes a sections
/// list (`Directory`), an items/detail list (`Metadata`), or a hub list (`Hub`).
#[derive(Deserialize, Default)]
pub struct MediaContainer {
    #[serde(rename = "Directory", default)]
    pub directory: Vec<LibrarySection>,
    #[serde(rename = "Metadata", default)]
    pub metadata: Vec<Metadata>,
    #[serde(rename = "Hub", default)]
    pub hub: Vec<Hub>,
    #[serde(default, deserialize_with = "de_i64")]
    pub size: i64,
    #[serde(rename = "totalSize", default, deserialize_with = "de_i64")]
    pub total_size: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub offset: i64,
    /// GET /identity — the server's stable id (the PlayQueue `server://{id}/…` uri needs it).
    #[serde(rename = "machineIdentifier", default)]
    pub machine_identifier: String,
    /// POST /playQueues response ids (0 = absent) — the timeline's playQueueID/playQueueItemID.
    #[serde(rename = "playQueueID", default, deserialize_with = "de_i64")]
    pub play_queue_id: i64,
    #[serde(rename = "playQueueSelectedItemID", default, deserialize_with = "de_i64")]
    pub play_queue_selected_item_id: i64,
    /// How many items the whole queue holds (the returned `Metadata[]` may be a window of it) —
    /// the resolve logs how many remain, derived from this and the offset below.
    #[serde(rename = "playQueueTotalCount", default, deserialize_with = "de_i64")]
    pub play_queue_total_count: i64,
    /// Position of the selected item within the WHOLE queue (not within the returned window).
    #[serde(rename = "playQueueSelectedItemOffset", default, deserialize_with = "de_i64")]
    pub play_queue_selected_item_offset: i64,
    /// /transcode/universal/decision verdict codes — Option (not defaulted 0) so the route
    /// log distinguishes "absent" from a real code, matching the old find_num scan.
    #[serde(rename = "generalDecisionCode", default, deserialize_with = "de_opt_i64")]
    pub general_decision_code: Option<i64>,
    #[serde(rename = "mdeDecisionCode", default, deserialize_with = "de_opt_i64")]
    pub mde_decision_code: Option<i64>,
    /// `?includeMeta=1` on a section listing — the server-driven Sort/Filter menus.
    #[serde(rename = "Meta", default)]
    pub meta: Option<Meta>,
}

/// Doubles as the generic `Directory[]` row: a library section ({key,type,title}), a secondary
/// directory entry, or a filter value ({key = tag id, title}).
#[derive(Deserialize, Default)]
pub struct LibrarySection {
    #[serde(default)]
    pub key: String, // "1" — parse to i64 at the call site (keep raw)
    #[serde(rename = "type", default)]
    pub kind: String, // movie | show | artist
    #[serde(default)]
    pub title: String,
    /// firstCharacter rows: the letter's item count (PMS string-encodes it).
    #[serde(default, deserialize_with = "de_i64")]
    pub size: i64,
}

/// `MediaContainer.Meta` — present with `includeMeta=1` on `/library/sections/{k}/all`.
#[derive(Deserialize, Default)]
pub struct Meta {
    #[serde(rename = "Type", default)]
    pub types: Vec<MetaType>,
}

/// One browsable type of the section (movie / show / season / episode / folder) with its
/// server-defined sort menu. The `active:true` entry matches the current `type=` of the
/// listing. (The wire also carries a `Filter[]` menu — docs/pms-api.md §2b — modelled again
/// when the v1.5 facet menu consumes it; DTOs here keep only consumed fields.)
#[derive(Deserialize, Default)]
pub struct MetaType {
    #[serde(default, deserialize_with = "de_i64")]
    pub active: i64, // bool on the wire; de_i64 folds true/false/"1"
    #[serde(rename = "Sort", default)]
    pub sort: Vec<SortOption>,
}

/// One sort menu entry: `sort={key}:asc|desc` on the listing.
#[derive(Deserialize, Default)]
pub struct SortOption {
    #[serde(default)]
    pub key: String, // "titleSort"
    #[serde(rename = "defaultDirection", default)]
    pub default_direction: String, // "asc" | "desc"
    #[serde(default)]
    pub title: String, // display: "Title"
}

#[derive(Deserialize, Default)]
pub struct Hub {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(rename = "hubIdentifier", default)]
    pub hub_identifier: String, // e.g. home.continue, home.ondeck — stable, locale-independent
    #[serde(default)]
    pub title: String,
    #[serde(rename = "Metadata", default)]
    pub metadata: Vec<Metadata>,
}

/// The movie/show/season/episode item. Missing fields default (Plex omits optionals).
#[derive(Deserialize, Default)]
pub struct Metadata {
    #[serde(rename = "type", default)]
    pub kind: String, // movie|show|season|episode|clip
    #[serde(rename = "ratingKey", default)]
    pub rating_key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub year: i64,
    #[serde(rename = "contentRating", default)]
    pub content_rating: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub studio: String,
    #[serde(rename = "originallyAvailableAt", default)]
    pub originally_available_at: String,
    #[serde(default, deserialize_with = "de_i64")]
    pub duration: i64, // ms
    #[serde(rename = "viewOffset", default, deserialize_with = "de_i64")]
    pub view_offset: i64, // ms; resume point
    #[serde(rename = "lastViewedAt", default, deserialize_with = "de_i64")]
    pub last_viewed_at: i64, // unix secs; drives Continue Watching recency sort
    #[serde(default, deserialize_with = "de_i64")]
    pub index: i64, // season/episode number
    #[serde(rename = "parentIndex", default, deserialize_with = "de_i64")]
    pub parent_index: i64,
    #[serde(rename = "parentRatingKey", default)]
    pub parent_rating_key: String, // season → its show
    #[serde(rename = "grandparentRatingKey", default)]
    pub grandparent_rating_key: String, // episode → its show
    #[serde(rename = "grandparentTitle", default)]
    pub grandparent_title: String, // episode → show title
    #[serde(rename = "leafCount", default, deserialize_with = "de_i64")]
    pub leaf_count: i64,
    #[serde(rename = "viewedLeafCount", default, deserialize_with = "de_i64")]
    pub viewed_leaf_count: i64, // shows/seasons: episodes watched (watched = viewed >= leaf)
    #[serde(rename = "viewCount", default, deserialize_with = "de_i64")]
    pub view_count: i64, // present only once watched ≥1× (absent = unwatched)
    #[serde(default)]
    pub thumb: String,
    #[serde(default)]
    pub art: String,
    #[serde(rename = "grandparentThumb", default)]
    pub grandparent_thumb: String,
    #[serde(rename = "Media", default)]
    pub media: Vec<Media>,
    #[serde(rename = "Genre", default)]
    pub genre: Vec<Tag>,
    #[serde(rename = "Country", default)]
    pub country: Vec<Tag>,
    #[serde(rename = "Director", default)]
    pub director: Vec<Tag>,
    #[serde(rename = "Writer", default)]
    pub writer: Vec<Tag>,
    #[serde(rename = "Role", default)]
    pub role: Vec<Tag>,
    #[serde(rename = "Chapter", default)]
    pub chapter: Vec<Chapter>,
    #[serde(rename = "Marker", default)]
    pub marker: Vec<Marker>,
    /// POST /playQueues rows only: this row's id within the queue. The up-next lookup walks to
    /// the row AFTER the one `playQueueSelectedItemID` names, so it must be able to identify
    /// rows — a ratingKey can repeat in a queue, a playQueueItemID cannot.
    #[serde(rename = "playQueueItemID", default, deserialize_with = "de_i64")]
    pub play_queue_item_id: i64,
    // D-1: PMS returns UltraBlurColors as an ARRAY `[{…}]` (the old code reads it as an object
    // and misses it). `de_ultrablur` accepts object OR array and yields the first.
    #[serde(rename = "UltraBlurColors", default, deserialize_with = "de_ultrablur")]
    pub ultra_blur_colors: Option<UltraBlurColors>,
    /// Review scores, one row per provider — the SUPERSET, and the only form that carries each
    /// score's provider identity. Present on `/library/metadata/{rk}`; **absent from a section
    /// listing**, which sends only the flat pair below (verified live 2026-07-29), so both forms
    /// are parsed and `metadata::convert_ratings` prefers this one.
    #[serde(rename = "Rating", default)]
    pub ratings: Vec<Rating>,
    /// The flat critic score (0–10) and the provider/state `image` that goes with it — the legacy
    /// pair, kept as the fallback for responses that omit `Rating[]`.
    #[serde(default, deserialize_with = "de_f64")]
    pub rating: f64,
    #[serde(rename = "ratingImage", default)]
    pub rating_image: String,
    /// The flat audience score + its image (`rottentomatoes://image.rating.upright`, and on some
    /// items `imdb://image.rating` / `themoviedb://image.rating`).
    #[serde(rename = "audienceRating", default, deserialize_with = "de_f64")]
    pub audience_rating: f64,
    #[serde(rename = "audienceRatingImage", default)]
    pub audience_rating_image: String,
}

impl Metadata {
    /// Media[0].Part[0] — the primary playable part (None for a show container, which carries
    /// no Media). The part holds the direct-play `key` and the `Stream[]` list.
    pub fn first_part(&self) -> Option<&MediaPart> {
        self.media.first().and_then(|m| m.part.first())
    }

    /// `Media[0]` — the FIRST version listed, **not** a chosen-best one, and the honest name for
    /// what every caller here actually reads. An item can carry SEVERAL `Media[]` versions
    /// (docs/pms-api.md §4; the dev library really does — one episode ships a 4k and a 1080
    /// version, another two 4k versions at different bitrates), and picking among them by
    /// codec/resolution needs a version picker this client does not have yet. So anything derived
    /// from this describes **version 0**, and a UI that shows it should be read that way.
    /// [`first_part`](Self::first_part) carries the same caveat.
    pub fn primary_media(&self) -> Option<&Media> {
        self.media.first()
    }
}

#[derive(Deserialize, Default)]
pub struct Media {
    #[serde(rename = "videoCodec", default)]
    pub video_codec: String,
    #[serde(rename = "audioCodec", default)]
    pub audio_codec: String,
    /// Whole-stream bitrate in **kbps** (PMS's unit) — the source rung of the video-quality ladder
    /// ("26.1 Mbps 4K (Original)") and the input to any bandwidth cap.
    #[serde(default, deserialize_with = "de_i64")]
    pub bitrate: i64,
    /// Coded frame size. Note it is the STORED size, not the resolution class: a 2.35:1 1080p
    /// movie is 1918x802 on this server, so `height` alone would label it 720p — prefer
    /// [`video_resolution`](Self::video_resolution) for a badge and keep these for the ladder.
    #[serde(default, deserialize_with = "de_i64")]
    pub width: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub height: i64,
    /// PMS's coarse resolution class, always a STRING even when it looks numeric — verified live:
    /// `"4k"`, `"1080"`, `"720"`, `"576"`, `"sd"`. The version-picker key per docs/pms-api.md §4.
    #[serde(rename = "videoResolution", default, deserialize_with = "de_str")]
    pub video_resolution: String,
    #[serde(rename = "Part", default)]
    pub part: Vec<MediaPart>,
}

#[derive(Deserialize, Default)]
pub struct MediaPart {
    #[serde(default, deserialize_with = "de_i64")]
    pub id: i64,
    #[serde(default)]
    pub key: String, // /library/parts/{id}/{changestamp}/file.mkv
    /// /decision only: the MDE verdict for this part — "directplay" | "transcode" | "copy"
    /// (Media/container carry no decision; the part is the authoritative one).
    #[serde(default)]
    pub decision: String,
    #[serde(rename = "Stream", default)]
    pub stream: Vec<Stream>,
}

/// D-2: channels/title/hearingImpaired/audioDescription/forced are real PMS fields the spec
/// omits — kept here. Plex 0/1 booleans stay i64; the app tests `!= 0`.
#[derive(Deserialize, Default)]
pub struct Stream {
    #[serde(default, deserialize_with = "de_i64")]
    pub id: i64,
    #[serde(rename = "streamType", default, deserialize_with = "de_i64")]
    pub stream_type: i64, // 1 video, 2 audio, 3 subtitle
    /// PMS's stream index within the part — container (ffmpeg) order. The track mapping sorts
    /// by this instead of trusting Stream[] document order (PMS may reorder), so the demuxer's
    /// nth-of-type selection can't drift from the row the user picked.
    #[serde(default, deserialize_with = "de_i64")]
    pub index: i64,
    /// Delivery path — present ONLY on external/sidecar streams (a downloaded .srt has
    /// key=/library/streams/{id}; embedded container streams carry no key). The client
    /// renderer uses this to exclude sidecars from the embedded-subtitle mapping.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub language: String,
    #[serde(rename = "languageCode", default)]
    pub language_code: String, // ISO-639 code, e.g. "eng" (route audio-track pick)
    // Video stream only: source fps for the Load esInfo. Lenient (number OR numeric string)
    // so a non-numeric frameRate never fails the whole detail parse — matches the old jfloat.
    #[serde(rename = "frameRate", default, deserialize_with = "de_f64")]
    pub frame_rate: f64,
    #[serde(default, deserialize_with = "de_i64")]
    pub channels: i64,
    #[serde(rename = "audioChannelLayout", default)]
    pub audio_channel_layout: String,
    #[serde(rename = "displayTitle", default)]
    pub display_title: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "hearingImpaired", default, deserialize_with = "de_i64")]
    pub hearing_impaired: i64,
    #[serde(rename = "audioDescription", default, deserialize_with = "de_i64")]
    pub audio_description: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub forced: i64,
    // "default"/"selected" mark the file's default track and the server's current pick. Lenient
    // (number OR numeric string) like the other flags. `default` drives the "Original:" audio label.
    #[serde(rename = "default", default, deserialize_with = "de_i64")]
    pub is_default: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub selected: i64,
}

/// A tag row — `Genre[]`, `Country[]`, `Role[]`, `Director[]`, `Writer[]`. The three PEOPLE
/// arrays carry six attributes, all of them modelled here; verified live 2026-07-29:
/// `{"id":161,"filter":"actor=161","tag":"Idina Menzel","tagKey":"5d77682a…","count":3,
///   "role":"Elsa (voice)","thumb":"https://metadata-static.plex.tv/…jpg"}`.
/// [`id`](Tag::id)/[`tag_key`](Tag::tag_key) are what make the person page reachable — either is
/// the `personId` of `/library/people/{personId}[/media]` (docs/pms-api.md §2c).
#[derive(Deserialize, Default)]
pub struct Tag {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub role: String, // Role[] only (character name)
    #[serde(default)]
    pub thumb: String, // headshot — on the crew arrays (Director[]/Writer[]) as well as Role[]
    /// The tag's numeric library id — the `personId` of `/library/people/{id}[/media]`, and the
    /// value behind `?actor=<id>` on a section listing. 0 = absent.
    #[serde(default, deserialize_with = "de_i64")]
    pub id: i64,
    /// Plex's global person guid (`"5d77682aeb5d26001f1de4b0"`) — stable across servers, and the
    /// alternate `personId` (both forms verified live against the same record).
    #[serde(rename = "tagKey", default)]
    pub tag_key: String,
    /// The server's own ready-made listing filter for this tag, e.g. `"actor=161"` /
    /// `"director=459"` — append it to `/library/sections/{k}/all?` to list ONE section's items
    /// for this person. Carries the tag's ROLE in the library (actor vs director vs writer),
    /// which `id` alone does not.
    #[serde(default)]
    pub filter: String,
    /// How many items in this library carry the tag — Plex's count badge. NOT emitted by every
    /// server (this one omits it on `Role[]`), so 0 means "unknown", never "none".
    #[serde(default, deserialize_with = "de_i64")]
    pub count: i64,
}

/// Plex `Chapter[]` on a leaf item (movies/episodes with chapter data). Sibling of `Media[]`,
/// present only with `?includeChapters=1`. `tag` is the title (often empty → synthesize
/// "Chapter N"); `thumb` is a server image path for the poster pipeline (empty if the server
/// never generated chapter thumbs). Offsets are ms; PMS string-encodes them → de_i64.
#[derive(Deserialize, Default)]
pub struct Chapter {
    #[serde(default, deserialize_with = "de_i64")]
    pub index: i64,
    #[serde(rename = "startTimeOffset", default, deserialize_with = "de_i64")]
    pub start_time_offset: i64,
    #[serde(rename = "endTimeOffset", default, deserialize_with = "de_i64")]
    pub end_time_offset: i64,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub thumb: String,
}

/// Plex `Marker[]` on a leaf item — the server-detected intro / credits segments, present only
/// with `?includeMarkers=1`. `kind` is `"intro"` | `"credits"` (PMS also emits `"commercial"` on
/// recorded content); offsets are ms into the item. `is_final` marks a credits segment that runs
/// to the end of the file — the usual case, and what makes "skip credits" equivalent to "finish".
///
/// Verified against the live server 2026-07-29: The Morning Show S2E2 carries both an intro
/// (0.99s–99.6s) and a `final` credits marker (3065.6s–3130.7s); The Office S5E26 and Top Gear
/// S14E7 carry credits only. The `id` field repeats across items (every marker came back as
/// `id: 3096`), so it is NOT an identity — the app keys markers by kind + offsets, never by id.
#[derive(Deserialize, Default)]
pub struct Marker {
    #[serde(rename = "type", default)]
    pub kind: String, // intro | credits | commercial
    #[serde(rename = "startTimeOffset", default, deserialize_with = "de_i64")]
    pub start_time_offset: i64,
    #[serde(rename = "endTimeOffset", default, deserialize_with = "de_i64")]
    pub end_time_offset: i64,
    /// `final: true` — this credits marker runs to the end of the item. (`final` is a reserved
    /// Rust keyword, hence the rename.) Arrives as a JSON bool; `de_i64` folds it to 1/0.
    #[serde(rename = "final", default, deserialize_with = "de_i64")]
    pub is_final: i64,
}

/// One row of a leaf's `Rating[]` — a single provider's review score.
///
/// `image` names BOTH the provider and the icon state (`rottentomatoes://image.rating.ripe`,
/// `rottentomatoes://image.rating.spilled`, `imdb://image.rating`, `themoviedb://image.rating`),
/// which is why the badge art is chosen by parsing this string rather than by thresholding
/// `value` — see `metadata::RatingArt`. `value` is normalised 0–10 by PMS for every provider
/// (a 91% tomato arrives as 9.1). `kind` is `"critic"` | `"audience"`; note it does NOT identify
/// the provider — IMDb and TMDB both arrive as `audience` (verified live 2026-07-29).
#[derive(Deserialize, Default)]
pub struct Rating {
    #[serde(default)]
    pub image: String,
    #[serde(default, deserialize_with = "de_f64")]
    pub value: f64,
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Deserialize, Default, Clone, Copy)]
pub struct UltraBlurColors {
    #[serde(rename = "topLeft", default)]
    pub top_left: HexColor,
    #[serde(rename = "topRight", default)]
    pub top_right: HexColor,
    #[serde(rename = "bottomRight", default)]
    pub bottom_right: HexColor,
    #[serde(rename = "bottomLeft", default)]
    pub bottom_left: HexColor,
}

/// "1a2b3c" (with/without '#') → linear [r,g,b] 0..1. Replaces pms::hex3.
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(from = "String")]
pub struct HexColor(pub [f32; 3]);
impl From<String> for HexColor {
    fn from(s: String) -> Self {
        let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0);
        HexColor([
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
        ])
    }
}

/// Lenient f64: a JSON number, a numeric string, or null → 0.0 (matches the old `jfloat`
/// scrape). Called only when the field is present; a missing field uses `default` (0.0).
fn de_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        N(f64),
        S(String),
    }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::N(n)) => n,
        Some(NumOrStr::S(s)) => s.parse().unwrap_or(0.0),
        None => 0.0,
    })
}

/// Lenient i64: a JSON integer, a float (truncated), a numeric string, or null → 0. The old
/// code read every number with `.as_i64().unwrap_or(0)`, degrading a bad value to 0 for THAT
/// field only. serde's strict i64 would instead fail the WHOLE container parse on a
/// string-encoded int (PMS does this: e.g. `size:"40"`, `streamType:"2"`), dropping every
/// item in the response. This keeps one field's-worth of degradation (and actually recovers a
/// stringy int rather than zeroing it). Applied to every i64 in a parsed DTO.
fn de_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntFloatStrBool {
        I(i64),
        F(f64),
        S(String),
        // PMS sends the flag fields (default/selected/forced/…) as JSON booleans on some
        // endpoints (e.g. Stream inside /hubs) and as "1"/"0" strings on others. A missing bool
        // arm here fails the untagged enum → the WHOLE MediaContainer parse fails (blast radius),
        // so accept it: true→1, false→0.
        B(bool),
    }
    Ok(match Option::<IntFloatStrBool>::deserialize(d)? {
        Some(IntFloatStrBool::I(n)) => n,
        Some(IntFloatStrBool::F(f)) => f as i64,
        Some(IntFloatStrBool::S(s)) => s.trim().parse().unwrap_or(0),
        Some(IntFloatStrBool::B(b)) => b as i64,
        None => 0,
    })
}

/// Lenient Option<i64>: like `de_i64` but preserves "field absent/null" as None (the decision
/// verdict codes are logged as `Some(code)`/`None`, mirroring the old find_num scan).
fn de_opt_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntStr {
        I(i64),
        S(String),
    }
    Ok(match Option::<IntStr>::deserialize(d)? {
        Some(IntStr::I(n)) => Some(n),
        Some(IntStr::S(s)) => s.trim().parse().ok(),
        None => None,
    })
}

/// Lenient String: a JSON string, a number stringified, or null → "". `videoResolution` is a
/// string on this server ("1080"/"4k", verified live) but reads like a number, and a strict
/// `String` that meets one is a WHOLE-`MediaContainer` parse failure — the same blast radius
/// `de_i64` exists to avoid, just pointing the other way. Use it for any stringly-typed number.
fn de_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrIntFloatBool {
        S(String),
        I(i64),
        F(f64),
        // no arm for a shape = the whole container fails on it, which is the one outcome this
        // adapter exists to prevent — so carry the bool too (`de_i64` learned that the hard way)
        B(bool),
    }
    Ok(match Option::<StrIntFloatBool>::deserialize(d)? {
        Some(StrIntFloatBool::S(s)) => s,
        Some(StrIntFloatBool::I(n)) => n.to_string(),
        Some(StrIntFloatBool::F(f)) => f.to_string(),
        Some(StrIntFloatBool::B(b)) => b.to_string(),
        None => String::new(),
    })
}

/// Accept `{…}` OR `[{…}]` (D-1) and return the first, or None.
fn de_ultrablur<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<UltraBlurColors>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(UltraBlurColors),
        Many(Vec<UltraBlurColors>),
    }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        Some(OneOrMany::One(u)) => Some(u),
        Some(OneOrMany::Many(v)) => v.into_iter().next(),
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::Envelope;

    /// The `Media[]` technical fields feed the resolution badge and (later) the quality ladder, and
    /// PMS is free to string-encode any of them. A strict field there does not lose one number —
    /// it fails the WHOLE `MediaContainer`, i.e. the entire detail page. Both encodings must land,
    /// and the several-versions case must stay several (`primary_media` is version 0, not the only
    /// one) — see docs/pms-api.md §4.
    #[test]
    fn media_technical_fields_survive_both_encodings_and_keep_every_version() {
        let json = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"1859","title":"Every Summer After",
            "Media":[
              {"bitrate":14663,"width":3840,"height":2160,"videoResolution":"4k","videoCodec":"hevc",
               "Part":[{"id":1,"key":"/library/parts/1/1/file.mkv"}]},
              {"bitrate":"2372","width":"1920","height":"1080","videoResolution":1080,
               "videoCodec":"h264","Part":[{"id":2,"key":"/library/parts/2/1/file.mkv"}]}
            ]}]}}"#;
        let env: Envelope = serde_json::from_slice(json).expect("lenient parse");
        let it = &env.media_container.metadata[0];
        assert_eq!(it.media.len(), 2, "both versions are kept for a future picker");

        let v0 = it.primary_media().expect("Media[0]");
        assert_eq!((v0.bitrate, v0.width, v0.height), (14663, 3840, 2160));
        assert_eq!(v0.video_resolution, "4k");
        // version 1 sends the same fields string-encoded, and videoResolution as a bare NUMBER
        let v1 = &it.media[1];
        assert_eq!((v1.bitrate, v1.width, v1.height), (2372, 1920, 1080));
        assert_eq!(v1.video_resolution, "1080");
        // the two versions therefore badge differently ("4K" vs "1080p" — see ui::fmt::resolution's
        // own tests), which is the whole reason the accessor has to name WHICH one it returned.
    }

    /// A show container carries no `Media` at all — the accessor must say so rather than panic,
    /// because the detail page asks every item for its primary version.
    #[test]
    fn a_show_container_has_no_primary_media() {
        let json = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"9","type":"show","title":"A Show"}]}}"#;
        let env: Envelope = serde_json::from_slice(json).expect("parse");
        assert!(env.media_container.metadata[0].primary_media().is_none());
    }
}
