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
    // D-1: PMS returns UltraBlurColors as an ARRAY `[{…}]` (the old code reads it as an object
    // and misses it). `de_ultrablur` accepts object OR array and yields the first.
    #[serde(rename = "UltraBlurColors", default, deserialize_with = "de_ultrablur")]
    pub ultra_blur_colors: Option<UltraBlurColors>,
}

impl Metadata {
    /// Media[0].Part[0] — the primary playable part (None for a show container, which carries
    /// no Media). The part holds the direct-play `key` and the `Stream[]` list.
    pub fn first_part(&self) -> Option<&MediaPart> {
        self.media.first().and_then(|m| m.part.first())
    }
}

#[derive(Deserialize, Default)]
pub struct Media {
    #[serde(rename = "videoCodec", default)]
    pub video_codec: String,
    #[serde(rename = "audioCodec", default)]
    pub audio_codec: String,
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

#[derive(Deserialize, Default)]
pub struct Tag {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub role: String, // Role[] only (character name)
    #[serde(default)]
    pub thumb: String, // Role[] headshot
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
