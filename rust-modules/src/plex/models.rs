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
    /// GET / — what the server calls ITSELF ("nas-home"). The MACHINE name, and so the one string
    /// the Sources list heads a server's group with; the app says the owner's HANDLE everywhere
    /// else ("people in content, machines in settings"). Read off the PMS rather than plex.tv on
    /// purpose: a server that answers can always name itself, including on a boot that never
    /// reached plex.tv at all.
    #[serde(rename = "friendlyName", default, deserialize_with = "de_str")]
    pub friendly_name: String,
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
    /// The TRANSCODE lane's own verdict, beside the general one above (`4007` = "cannot convert
    /// this item"). Same `Option` discipline and for the same reason, doubled here: the pre-flight
    /// refusal is graded on a CODE, and a defaulted 0 would be a code this server never sent.
    #[serde(rename = "transcodeDecisionCode", default, deserialize_with = "de_opt_i64")]
    pub transcode_decision_code: Option<i64>,
    /// …and the two HUMAN sentences PMS pairs with those codes — "Cannot convert this item.
    /// Implementation for video encoder 'vp9' not found." / "Neither direct play nor conversion is
    /// available." (both probed live against PMS 1.43.3). They are the server's own wording, quoted
    /// verbatim by the player's failure read-out, so they are read but never parsed: the CODE is
    /// what the app decides on. Absent stays `""` — an empty sentence draws no line at all, which
    /// is the honest reading of "the server named no reason".
    #[serde(rename = "transcodeDecisionText", default, deserialize_with = "de_str")]
    pub transcode_decision_text: String,
    #[serde(rename = "generalDecisionText", default, deserialize_with = "de_str")]
    pub general_decision_text: String,
    /// `?includeMeta=1` on a section listing — the server-driven Sort/Filter menus.
    #[serde(rename = "Meta", default)]
    pub meta: Option<Meta>,
    /// `GET /` (the server root) only: does the server's OWNER hold a Plex Pass? Bool on PMS 1.43
    /// (probed live 2026-08-10), and lenient like every flag here because PMS string-encodes
    /// freely. `Option`, not a defaulted 0: a server old enough not to say must stay UNKNOWN —
    /// defaulting would read as "no Plex Pass", a confident wrong answer on the field issue #22
    /// (docs/plex-pass-audit.md) exists to get right. Consumed by `serverinfo.rs` only.
    #[serde(rename = "myPlexSubscription", default, deserialize_with = "de_opt_i64")]
    pub my_plex_subscription: Option<i64>,
    /// `GET /` only: the PMS build string ("1.43.3.10861-cd85035e7"). Diagnostics
    /// (`serverinfo.rs`); every other endpoint simply omits it.
    #[serde(default)]
    pub version: String,
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
    /// **A hub's items do not all arrive as `Metadata`**, and a search screen that assumes they do
    /// renders two of its five shelves as nothing at all, silently. Measured against this
    /// household's PMS 1.43.3 over six queries on 2026-08-14:
    ///
    /// | payload | hubs |
    /// |---|---|
    /// | `Metadata[]` | `movie`, `show`, `episode`, `album`, `artist`, `track` |
    /// | **`Directory[]`** | **`actor`, `director`, `collection`** |
    ///
    /// So Cast & Crew *and* Collections both come through here. `plex-openapi.json`'s own worked
    /// example for `/hubs/search` disagrees — it puts shows under `Directory` — which is why this
    /// was probed live rather than modelled from the spec, and why the split is written down here
    /// instead of being rediscovered the next time a hub looks empty.
    #[serde(rename = "Directory", default)]
    pub directory: Vec<Tag>,
    /// How many items the hub holds. A search response carries EVERY hub type the server knows
    /// about — 17 of them on this set — most with `size: 0`, so this is the field that says which
    /// ones are worth drawing.
    #[serde(default, deserialize_with = "de_i64")]
    pub size: i64,
}

#[derive(Deserialize, Default)]
pub struct Metadata {
    #[serde(rename = "type", default)]
    pub kind: String, // movie|show|season|episode|clip
    #[serde(rename = "ratingKey", default)]
    pub rating_key: String,
    /// **The only PORTABLE identity Plex issues** — `plex://movie/6856…291d`, the metadata
    /// provider's id, identical on every server that ever matched this film. Everything else
    /// item-shaped (`ratingKey`, `librarySectionID`, `Part.key`, `Stream.id`) is a server-local
    /// integer dense from 1.
    ///
    /// Measured across this household's two servers 2026-08-14: one film is `ratingKey` **2029**
    /// on ours and **5274** on the share, and their copy is titled in another language entirely —
    /// so matching copies by key offers a different film and matching by title misses this one.
    /// Plex's own client matches these two, which is the behaviour "Also available" reproduces.
    #[serde(default, deserialize_with = "de_str")]
    pub guid: String,
    #[serde(default)]
    pub title: String,
    /// The LIBRARY this row lives in, on the server that answered ("Movies", "Film Club"). Sent on
    /// a cross-library query such as `/library/all?guid=…`, which is the one place the app asks a
    /// server something without already knowing which of its libraries will answer.
    #[serde(rename = "librarySectionTitle", default, deserialize_with = "de_str")]
    pub library_section_title: String,
    /// Which LIBRARY on that server this row belongs to — the pin's grain.
    ///
    /// Present on every `/hubs` and `/hubs/continueWatching` row (verified live), which is what
    /// lets Home honour a per-library pin without a per-library fetch: the whole-server hub request
    /// answers with rows from every library, and this is the only field that says which. 0 = the
    /// server did not send one, and an item with no library cannot be gated out — see
    /// `pms::feeds_home_item`.
    #[serde(rename = "librarySectionID", default, deserialize_with = "de_i64")]
    pub library_section_id: i64,
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
    /// `OnDeck` (SHOWS only, and only with `includeOnDeck=1`) — the one episode the server says is
    /// next to watch, as a whole episode record: thumb, summary, `Media`, `viewOffset`, the lot.
    ///
    /// It is the only SHOW-LEVEL answer to "what's next". The client holds one season's episodes at a
    /// time, so anything it worked out itself would change with the selected season tab — which is
    /// exactly the bug this field exists to fix.
    #[serde(rename = "OnDeck", default)]
    pub on_deck: Option<OnDeckHub>,
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
    /// Video stream only: the transfer characteristic — `smpte2084` (HDR10/PQ) or
    /// `arib-std-b67` (HLG) mark an HDR source (probed live on the dev PMS 2026-08-11:
    /// the HDR10 items send `colorTrc=smpte2084`). Drives the detail facts row's
    /// HDR-without-tone-mapping warning; absent (SDR or old server) is just "".
    #[serde(rename = "colorTrc", default)]
    pub color_trc: String,
    /// Video stream only: Dolby Vision present — HDR whatever `colorTrc` says. Lenient because
    /// PMS sends bools as `true` and as `"1"` depending on the endpoint.
    #[serde(rename = "DOVIPresent", default, deserialize_with = "de_i64")]
    pub dovi_present: i64,
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
    // ---- the SECOND producer of this record: a search hub's `Directory[]` ----
    //
    // `/hubs/search` returns its `actor`, `director` and `collection` hubs as `Directory[]` of
    // exactly this shape (measured live — see `Hub::directory`), so the cast-credit tag and the
    // search hit are one type rather than two that would drift. The three fields below are the
    // ones only the search producer sets; a `Role[]` entry leaves them empty and nothing reads
    // them there.
    /// The listing this tag opens — `/library/sections/1/all?collection=6068`. The only handle a
    /// COLLECTION hit gives you: those carry no `tagKey`, no `thumb` and no `ratingKey`.
    #[serde(default)]
    pub key: String,
    #[serde(rename = "librarySectionID", default, deserialize_with = "de_i64")]
    pub library_section_id: i64,
    /// Why this result, when it is not a direct term match: `section` (the same title in several
    /// sections), `originalTitle`, or another hub's identifier — searching "arnold" returns films
    /// with `reason: actor`. Kept because a shelf that mixes direct and inferred hits without
    /// saying so reads as the server being wrong.
    #[serde(default)]
    pub reason: String,
    #[serde(rename = "reasonTitle", default)]
    pub reason_title: String,
}

impl Tag {
    /// Is this tag the person addressed by `id` (a local `personId`) or `guid` (a `tagKey`)?
    ///
    /// **Both, because the two id spaces are not interchangeable and a caller rarely has only one.**
    /// A credit row may carry no numeric [`Tag::id`], in which case whatever addressed the person is
    /// their `tagKey` — so an id-only comparison silently never matches (a 24-char hex guid never
    /// equals a decimal id string). `id == 0` means "the server sent none" and must never match a
    /// caller's literal `"0"`; an empty `guid` must never match a tag whose `tagKey` is also empty.
    pub fn is_person(&self, id: &str, guid: &str) -> bool {
        (self.id != 0 && self.id.to_string() == id) || (!guid.is_empty() && self.tag_key == guid)
    }
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
/// Verified against the live server 2026-07-29: one episode carries both an intro
/// (0.99s–99.6s) and a `final` credits marker (3065.6s–3130.7s); two other episodes on the same
/// server carry credits only. The `id` field repeats across items (every marker came back as
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
/// `…rating.certified`, `…rating.spilled`, `imdb://image.rating`, `themoviedb://image.rating`),
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

impl UltraBlurColors {
    /// The four corners in `Painter::ambient`'s order — top-left, top-**right**, bottom-**right**,
    /// bottom-left (a RING, not the reading order the JSON field names suggest) — or `None` when the
    /// envelope is empty. PMS sends the key on some items with every corner defaulted to black, and
    /// a pure-black gradient is not a colour scheme: keyed as one it paints a page darker than the
    /// app's own ground for no reason. TWO stores parse this field (the hub catalog in `pms.rs` and
    /// the detail store in `metadata.rs`), so the shape and the guard live here rather than being
    /// written twice.
    pub fn corners(self) -> Option<[[f32; 3]; 4]> {
        let c = [self.top_left.0, self.top_right.0, self.bottom_right.0, self.bottom_left.0];
        c.iter().any(|x| *x != [0.0, 0.0, 0.0]).then_some(c)
    }
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
/// `OnDeck`'s envelope. Its `Metadata` is a single **object**, not the array every other nested hub
/// in this file uses — precisely the shape inconsistency `plex/CLAUDE.md` warns about, and a strict
/// field here would fail the WHOLE `MediaContainer` parse (an empty detail page), not just drop the
/// value. So [`de_on_deck`] accepts either form.
#[derive(Deserialize, Default)]
pub struct OnDeckHub {
    #[serde(rename = "Metadata", default, deserialize_with = "de_on_deck")]
    pub metadata: Option<Box<Metadata>>,
}

/// Accept `OnDeck.Metadata` as an object OR a one-element array, taking the first either way.
fn de_on_deck<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Box<Metadata>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(Box<Metadata>),
        Many(Vec<Metadata>),
    }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        Some(OneOrMany::One(m)) => Some(m),
        Some(OneOrMany::Many(v)) => v.into_iter().next().map(Box::new),
        None => None,
    })
}

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
pub(super) fn de_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
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

/// Lenient `Vec<T>`: a JSON array, or null → empty. Same trap as [`de_str`], one container up:
/// `#[serde(default)]` fills an ABSENT field and does nothing for one that is present and `null`,
/// and a strict `Vec` meeting a `null` fails the whole array it sits in. `connections` is the field
/// that matters — a server with a null connection list must cost that server, never the roster.
pub(super) fn de_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

/// Lenient bool: a JSON bool, the `1`/`0` (or `"1"`/`"0"`) forms Plex also uses, or null → false.
///
/// The PMS DTOs above keep their 0/1 flags as `i64` and test `!= 0`, because those fields are read
/// once, near the wire. This adapter exists for the plex.tv **connection-policy** flags
/// (`account.rs`'s `httpsRequired`/`publicAddressMatches`/`local`/`relay`/`IPv6`), which are read in
/// boolean position by `probe.rs` on every candidate: folding the encodings here once beats
/// spelling `!= 0` at every branch of a policy that has to stay readable to be trusted. Leniency
/// itself is not optional — plex.tv sends these as real JSON bools today, and a strict `bool` that
/// meets a `"1"` fails the WHOLE resources parse, which is a silent sign-in failure.
pub(super) fn de_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolIntStr {
        B(bool),
        I(i64),
        S(String),
    }
    Ok(match Option::<BoolIntStr>::deserialize(d)? {
        Some(BoolIntStr::B(b)) => b,
        Some(BoolIntStr::I(n)) => n != 0,
        Some(BoolIntStr::S(s)) => matches!(s.trim(), "1" | "true" | "True"),
        None => false,
    })
}

/// Lenient Option<i64>: like `de_i64` but preserves "field absent/null" as None (the decision
/// verdict codes are logged as `Some(code)`/`None`, mirroring the old find_num scan).
fn de_opt_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IntStrBool {
        I(i64),
        S(String),
        // `myPlexSubscription` on the root envelope is a JSON BOOL on PMS 1.43. A missing arm
        // here fails the untagged enum → the WHOLE MediaContainer parse (`de_i64` learned the
        // same lesson) — which on `GET /` reads as "server info unavailable" forever.
        B(bool),
    }
    Ok(match Option::<IntStrBool>::deserialize(d)? {
        Some(IntStrBool::I(n)) => Some(n),
        Some(IntStrBool::S(s)) => s.trim().parse().ok(),
        Some(IntStrBool::B(b)) => Some(b as i64),
        None => None,
    })
}

/// Lenient String: a JSON string, a number stringified, or null → "". `videoResolution` is a
/// string on this server ("1080"/"4k", verified live) but reads like a number, and a strict
/// `String` that meets one is a WHOLE-`MediaContainer` parse failure — the same blast radius
/// `de_i64` exists to avoid, just pointing the other way. Use it for any stringly-typed number.
///
/// `pub(super)` because `account.rs` needs the null half of it: plex.tv sends an explicit `null` for
/// an absent string, and there a whole-container failure is the account's whole server list.
pub(super) fn de_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
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
        let json = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"1859","title":"Example Movie",
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

    /// The `/decision` verdict block, in BOTH encodings PMS uses for it. This body is the one the
    /// player quotes to the user when the server refuses an item before playback, so a strict field
    /// here would not lose the sentence — it would fail the whole `MediaContainer` and turn a
    /// server that told us exactly what was wrong into the generic failure this block exists to
    /// end. The absent case is the third assertion: no code and no sentence, never a confident 0.
    #[test]
    fn the_decision_verdict_survives_both_encodings_and_stays_absent_when_unsent() {
        // shaped on the live PMS 1.43.3 refusal (a VP9 source, no encoder for it)
        let json = br#"{"MediaContainer":{"size":1,"generalDecisionCode":2000,
            "generalDecisionText":"Neither direct play nor conversion is available.",
            "transcodeDecisionCode":4007,
            "transcodeDecisionText":"Cannot convert this item. Implementation for video encoder 'vp9' not found."}}"#;
        let mc = serde_json::from_slice::<Envelope>(json).expect("lenient parse").media_container;
        assert_eq!(mc.general_decision_code, Some(2000));
        assert_eq!(mc.transcode_decision_code, Some(4007));
        assert_eq!(mc.general_decision_text, "Neither direct play nor conversion is available.");
        assert!(mc.transcode_decision_text.ends_with("'vp9' not found."));

        // the same body with every number string-encoded (PMS does this per endpoint, not per field)
        let json = br#"{"MediaContainer":{"generalDecisionCode":"2000","transcodeDecisionCode":"4007",
            "transcodeDecisionText":"Cannot convert this item."}}"#;
        let mc = serde_json::from_slice::<Envelope>(json).expect("lenient parse").media_container;
        assert_eq!((mc.general_decision_code, mc.transcode_decision_code), (Some(2000), Some(4007)));
        assert_eq!(mc.transcode_decision_text, "Cannot convert this item.");
        assert_eq!(mc.general_decision_text, "", "a sentence the server never sent is empty, not invented");

        // a healthy decision names no verdict at all — and absent must stay absent
        let json = br#"{"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"4"}]}}"#;
        let mc = serde_json::from_slice::<Envelope>(json).expect("parse").media_container;
        assert_eq!(mc.general_decision_code, None);
        assert_eq!(mc.transcode_decision_code, None);
        assert_eq!((mc.general_decision_text.as_str(), mc.transcode_decision_text.as_str()), ("", ""));
    }

    /// A show container carries no `Media` at all — the accessor must say so rather than panic,
    /// because the detail page asks every item for its primary version.
    #[test]
    fn a_show_container_has_no_primary_media() {
        let json = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"9","type":"show","title":"A Show"}]}}"#;
        let env: Envelope = serde_json::from_slice(json).expect("parse");
        assert!(env.media_container.metadata[0].primary_media().is_none());
    }

    // ---- /hubs/search ----
    //
    // A captured live response, trimmed to the fields under test and otherwise VERBATIM: field
    // names, nesting, ordering and every value's JSON encoding are the server's, taken from
    // `GET /hubs/search?query=wallace&limit=8` against this household's PMS 1.43.3 on 2026-08-14.
    // The only edits are dropping unread fields and cutting the two longest hubs to two rows each
    // (their `size` was corrected to match) — the whole 40 KB body is the same shape repeated.
    //
    // It is INLINE rather than an `include_str!`'d `.json` because there is no fixture-file
    // precedent under `rust-modules/` (every DTO test in this module inlines its body), and
    // because a whole capture would put one household's library — titles, people, section names —
    // into a PUBLIC repo for no test value. Trimming is what makes it publishable, and once it is
    // trimmed it is small enough to read in place, where the assertions can point at it.
    const SEARCH_WALLACE: &[u8] = br#"{"MediaContainer":{"size":17,"Hub":[
  {"type":"show","hubIdentifier":"show","title":"Shows","size":1,"more":false,"style":"shelf","Metadata":[
    {"ratingKey":"1975","type":"show","title":"Wallace & Gromit's Cracking Contraptions","year":2002,"librarySectionID":2,"score":"0.73084","guid":"plex://show/5d9c08804eefaa001f5df5fc"}
  ]},
  {"type":"movie","hubIdentifier":"movie","title":"Movies","size":2,"more":false,"style":"shelf","Metadata":[
    {"ratingKey":"1973","type":"movie","title":"Wallace & Gromit: The Wrong Trousers","year":1993,"librarySectionID":1,"score":"0.73092","guid":"plex://movie/5d7768264de0ee001fcc87e4"},
    {"ratingKey":"1971","type":"movie","title":"Wallace & Gromit: A Close Shave","year":1995,"librarySectionID":1,"score":"0.73092","guid":"plex://movie/5d776827a091de001f2e62cb"}
  ]},
  {"type":"collection","hubIdentifier":"collection","title":"Collections","size":1,"more":false,"style":"shelf","Directory":[
    {"key":"/library/sections/1/all?collection=6068","librarySectionID":1,"librarySectionTitle":"Movies","reason":"section","reasonTitle":"Movies","score":"0.52000","type":"tag","id":6068,"filter":"collection=6068","tag":"Wallace & Gromit Collection","tagType":2,"count":6,"guid":"collection://10c9bd0a-40ce-400c-bf57-dfd4009bb216"}
  ]},
  {"type":"actor","hubIdentifier":"actor","title":"Actors","size":3,"more":false,"style":"shelf","Directory":[
    {"key":"/library/sections/1/all?actor=921","librarySectionID":1,"librarySectionTitle":"Movies","reason":"section","reasonTitle":"Movies","score":"0.52000","type":"tag","id":921,"filter":"actor=921","tag":"Wallace Shawn","tagType":6,"tagKey":"5d776827151a60001f24ab18","thumb":"https://metadata-static.plex.tv/a/people/a8285fd57cda1effc4119eb9d63aec8f.jpg","count":5},
    {"key":"/library/sections/2/all?actor=921","librarySectionID":2,"librarySectionTitle":"TV Shows","reason":"section","reasonTitle":"TV Shows","score":"0.52000","type":"tag","id":921,"filter":"actor=921","tag":"Wallace Shawn","tagType":6,"tagKey":"5d776827151a60001f24ab18","thumb":"https://metadata-static.plex.tv/a/people/a8285fd57cda1effc4119eb9d63aec8f.jpg","count":3},
    {"key":"/library/sections/2/all?actor=1378","librarySectionID":2,"librarySectionTitle":"TV Shows","reason":"section","reasonTitle":"TV Shows","score":"0.32000","type":"tag","id":1378,"filter":"actor=1378","tag":"Dee Wallace","tagType":6,"tagKey":"5d776827eb5d26001f1dd893","thumb":"https://metadata-static.plex.tv/f/people/fb7113ed77c8a6ed4a992547c9faf12b.jpg","count":1}
  ]},
  {"type":"artist","hubIdentifier":"artist","title":"Artists","size":0,"more":false,"style":"shelf"},
  {"type":"album","hubIdentifier":"album","title":"Albums","size":0,"more":false,"style":"shelf"},
  {"type":"photoalbum","hubIdentifier":"photoalbum","title":"Photo Albums","size":0,"more":false,"style":"shelf"},
  {"type":"autotag","hubIdentifier":"autotag","title":"Automatic Tags","size":0,"more":false,"style":"shelf"},
  {"type":"photo","hubIdentifier":"photo","title":"Photos","size":0,"more":false,"style":"shelf"},
  {"type":"tag","hubIdentifier":"tag","title":"Tags","size":0,"more":false,"style":"shelf"},
  {"type":"track","hubIdentifier":"track","title":"Tracks","size":0,"more":false,"style":"shelf"},
  {"type":"director","hubIdentifier":"director","title":"Directors","size":0,"more":false,"style":"shelf"},
  {"type":"genre","hubIdentifier":"genre","title":"Genres","size":0,"more":false,"style":"shelf"},
  {"type":"episode","hubIdentifier":"episode","title":"Episodes","size":2,"more":false,"style":"shelf","Metadata":[
    {"ratingKey":"1990","type":"episode","title":"Shopper 13","year":2002,"librarySectionID":2,"score":"0.31085","guid":"plex://episode/5fbd929d55d986002d63c021"},
    {"ratingKey":"1988","type":"episode","title":"The Autochef","year":2002,"librarySectionID":2,"score":"0.31075","guid":"plex://episode/5fbd929a55d986002d63bffb"}
  ]},
  {"type":"playlist","hubIdentifier":"playlist","title":"Playlists","size":0,"more":false,"style":"shelf"},
  {"type":"shared","hubIdentifier":"shared","title":"Shared","size":0,"more":false,"style":"shelf"},
  {"type":"place","hubIdentifier":"place","title":"Places","size":0,"more":false,"style":"shelf"}
]}}"#;

    fn search_hub<'a>(mc: &'a super::MediaContainer, kind: &str) -> &'a super::Hub {
        mc.hub.iter().find(|h| h.kind == kind).unwrap_or_else(|| panic!("no {kind} hub"))
    }

    /// **The one fact the whole search screen rests on: a hub's items arrive in TWO different
    /// containers, and which one depends on the hub's type.**
    ///
    /// `movie`/`show`/`episode` come as `Metadata[]`; `actor`/`director`/`collection` come as
    /// `Directory[]`. `docs/plex-openapi.json`'s own worked example for this endpoint disagrees —
    /// it files shows under `Directory` — so the split was probed live rather than modelled from
    /// the spec, and this is the test that keeps the live answer.
    ///
    /// The failure it guards is silent and total, not partial: nothing errors, no field is
    /// missing, `Hub.size` still says 3 — the Cast & Crew and Collections shelves simply draw
    /// zero cards, because the reader looked in `.metadata` and the rows were in `.directory`.
    /// **Two** of the five design shelves, gone, with a green parse (three hub types, but
    /// `search::Kind::hubs` folds `actor` + `director` into one Cast & Crew shelf).
    #[test]
    fn a_search_response_delivers_its_items_in_two_different_containers() {
        let mc = serde_json::from_slice::<Envelope>(SEARCH_WALLACE).expect("lenient parse").media_container;
        assert_eq!(mc.hub.len(), 17, "every hub type the server knows, populated or not");

        for kind in ["movie", "show", "episode"] {
            let h = search_hub(&mc, kind);
            assert!(!h.metadata.is_empty(), "{kind} rows are Metadata[]");
            assert!(h.directory.is_empty(), "{kind} sends no Directory[]");
            // `size` round-trips the wire number. It is NOT evidence about what the server counts:
            // two of these hubs were trimmed here and their `size` edited to match (see above), so
            // the wire fact — `size` is the count RETURNED, capped by `limit` — is `docs/pms-api.md`
            // §3b's, measured by varying `limit`, not something this body could show.
            assert_eq!(h.size, h.metadata.len() as i64);
        }
        // `director` belongs in this list and cannot be tested from THIS capture: "wallace" matched
        // no directors, so its hub is one of the twelve that came back `size: 0`. The hub type is
        // in the split table on the strength of the `sta` query, which does populate it.
        for kind in ["actor", "collection"] {
            let h = search_hub(&mc, kind);
            assert!(!h.directory.is_empty(), "{kind} rows are Directory[] — the whole point");
            assert!(h.metadata.is_empty(), "…and a reader that only looks HERE draws nothing");
            assert_eq!(h.size, h.directory.len() as i64);
        }

        // the payload is keyed off the hub, and `hubIdentifier` is the stable, locale-independent
        // name to key off (`title` is "Actors" today and localised tomorrow) — search::Kind::hubs
        assert_eq!(search_hub(&mc, "actor").hub_identifier, "actor");
        assert_eq!(search_hub(&mc, "actor").title, "Actors");
    }

    /// The `Directory[]` row is a [`super::Tag`] — the SAME record the detail page's cast row is
    /// built from — and every field the search screen reads off one must survive the round trip.
    ///
    /// Two of them are the reason a person hit and a collection hit cannot share a code path:
    /// a person carries `tagKey` (the portable guid, and the only id `discover.provider.plex.tv`
    /// answers to) and an ABSOLUTE `metadata-static.plex.tv` `thumb`; a collection carries
    /// **neither**, nor a `ratingKey`. It is not identity-less — it has the server-local `id`,
    /// `filter` and `key` — but it has nothing that means anything OFF this server, so a screen
    /// that keys tags by `tagKey` silently loses every collection.
    #[test]
    fn a_person_hit_carries_a_portable_guid_and_a_collection_hit_carries_no_portable_identity() {
        let mc = serde_json::from_slice::<Envelope>(SEARCH_WALLACE).expect("lenient parse").media_container;

        let p = &search_hub(&mc, "actor").directory[0];
        assert_eq!(p.tag, "Wallace Shawn");
        assert_eq!(p.tag_key, "5d776827151a60001f24ab18");
        assert_eq!((p.id, p.count), (921, 5));
        assert_eq!(p.key, "/library/sections/1/all?actor=921");
        assert_eq!(p.filter, "actor=921");
        assert_eq!(p.library_section_id, 1);
        assert_eq!((p.reason.as_str(), p.reason_title.as_str()), ("section", "Movies"));
        assert!(p.thumb.starts_with("https://"), "a person thumb is ABSOLUTE, not a PMS path");
        // …and it is the same record `is_person` matches, by either id space
        assert!(p.is_person("921", ""));
        assert!(p.is_person("", "5d776827151a60001f24ab18"));

        let c = &search_hub(&mc, "collection").directory[0];
        assert_eq!(c.tag, "Wallace & Gromit Collection");
        assert_eq!((c.id, c.count), (6068, 6));
        assert_eq!(c.key, "/library/sections/1/all?collection=6068");
        assert_eq!(c.filter, "collection=6068");
        assert_eq!(c.tag_key, "", "a collection has no portable guid");
        assert_eq!(c.thumb, "", "…and no artwork of its own");
        // so it is addressable on THIS server and nowhere else
        assert!(c.is_person("6068", ""), "the server-local id still matches");
        assert!(!c.is_person("", "5d776827151a60001f24ab18"), "but no guid ever will");

        // …and a row the server sent no id for must not match a caller's literal "0" — the guard
        // in `is_person` that the collection above cannot exercise, because it HAS an id
        assert!(!super::Tag::default().is_person("0", ""), "id 0 means absent, not id zero");
        assert!(!super::Tag::default().is_person("", ""), "nor does an empty guid match an empty one");
    }

    /// **The same person arrives ONCE PER LIBRARY SECTION**, as separate rows with the same `id`
    /// and the same `tagKey`. Wallace Shawn is here twice — section 1 with `count: 5`, section 2
    /// with `count: 3` — because the hub is a union of per-section tag listings, which is also
    /// what `reason: "section"` is saying.
    ///
    /// Left alone that draws him twice in Cast & Crew, and a merge that keeps the first row
    /// reports 5 credits for a person who has 8. Neither is visible from a single row, which is
    /// why the shape is pinned here rather than left for the shelf to discover: dedupe on
    /// `tagKey`/`id`, and SUM the counts.
    #[test]
    fn one_person_arrives_once_per_library_section_with_a_per_section_count() {
        let mc = serde_json::from_slice::<Envelope>(SEARCH_WALLACE).expect("lenient parse").media_container;
        let rows = &search_hub(&mc, "actor").directory;

        let shawn: Vec<_> = rows.iter().filter(|t| t.tag == "Wallace Shawn").collect();
        assert_eq!(shawn.len(), 2, "one row per section, not one row per person");
        assert_eq!(shawn[0].id, shawn[1].id, "the same person: same server-local id");
        assert_eq!(shawn[0].tag_key, shawn[1].tag_key, "…and the same portable guid");
        assert_ne!(shawn[0].library_section_id, shawn[1].library_section_id);
        assert_eq!(shawn[0].count + shawn[1].count, 8, "the whole truth is the SUM, not the first row");
        // `key` is per-section too, so it is not an identity either — it is where that row leads
        assert_ne!(shawn[0].key, shawn[1].key);
    }

    /// Every number in a search hub, string-encoded — the encoding PMS is free to switch to per
    /// endpoint and has already switched on `size`, `streamType` and the decision codes. A strict
    /// field meeting one does not lose a count: it fails the WHOLE `MediaContainer`, so the search
    /// screen goes blank and stays blank while the server keeps answering correctly.
    ///
    /// `score` is the live proof that this endpoint really does it — it arrives as `"0.52000"`,
    /// a float in a string, on every row of every hub. It is deliberately not modelled (shelf
    /// order is fixed, so nothing ranks on it), and the body below keeps it to pin the other half:
    /// an unmodelled field must be IGNORED, never a parse failure.
    #[test]
    fn every_number_in_a_search_hub_survives_string_encoding() {
        let json = br#"{"MediaContainer":{"size":"2","Hub":[
          {"type":"actor","hubIdentifier":"actor","title":"Actors","size":"1","more":"1","Directory":[
            {"tag":"Wallace Shawn","id":"921","count":"5","librarySectionID":"1","score":"0.52000",
             "tagKey":"5d776827151a60001f24ab18","key":"/library/sections/1/all?actor=921"}]},
          {"type":"movie","hubIdentifier":"movie","title":"Movies","size":"1","Metadata":[
            {"ratingKey":"1973","type":"movie","title":"The Wrong Trousers","year":"1993",
             "librarySectionID":"1","score":"0.73092"}]}
        ]}}"#;
        let mc = serde_json::from_slice::<Envelope>(json).expect("lenient parse").media_container;
        assert_eq!(mc.size, 2);

        let p = &search_hub(&mc, "actor").directory[0];
        assert_eq!((p.id, p.count, p.library_section_id), (921, 5, 1));
        assert_eq!(search_hub(&mc, "actor").size, 1);

        let m = &search_hub(&mc, "movie").metadata[0];
        assert_eq!((m.year, m.library_section_id), (1993, 1));
        assert_eq!(m.rating_key, "1973");
    }

    /// A one-character query is a **200 with every hub empty** — not an error, and not an absent
    /// `Hub[]`: all seventeen hubs arrive carrying `size: 0` and no items array at all. (Measured:
    /// `a` returns this; `to` returns seven populated hubs. It is why `search::MIN_QUERY` is 2.)
    /// Three hubs stand in for the seventeen below; the shape is per-hub, so the count adds nothing.
    ///
    /// So "the server answered with nothing" and "the request failed" are different states that
    /// the store must not conflate, and the DTO's job is to make the first one arrive intact:
    /// both item vectors empty, no panic, and the hubs still countable.
    #[test]
    fn a_query_the_server_matched_nothing_for_is_an_answer_not_a_failure() {
        let json = br#"{"MediaContainer":{"size":3,"Hub":[
          {"type":"movie","hubIdentifier":"movie","title":"Movies","size":0,"more":false,"style":"shelf"},
          {"type":"actor","hubIdentifier":"actor","title":"Actors","size":0,"more":false,"style":"shelf"},
          {"type":"collection","hubIdentifier":"collection","title":"Collections","size":0,"more":false,"style":"shelf"}
        ]}}"#;
        let mc = serde_json::from_slice::<Envelope>(json).expect("parse");
        let mc = mc.media_container;
        assert_eq!(mc.hub.len(), 3, "the hubs are there — it is the ITEMS that are absent");
        for h in &mc.hub {
            assert_eq!(h.size, 0);
            assert!(h.metadata.is_empty() && h.directory.is_empty());
        }

        // and a container with no `Hub` key at all is still a container, not a parse failure
        let mc = serde_json::from_slice::<Envelope>(br#"{"MediaContainer":{"size":0}}"#)
            .expect("parse")
            .media_container;
        assert!(mc.hub.is_empty());
    }
}
