//! Library operations (impl Client): sections, section items, metadata, children/leaves,
//! related — plus the two part-level playback ops (stream selection, direct-play target).
use super::client::{Client, QueryBuilder, StreamUrl};
use super::models::{MediaContainer, Metadata};
use super::params::{SectionQuery, StreamSelection};

impl Client {
    /// GET /library/sections (D-3: spec-canonical is /library/sections/all; keep the
    /// known-working bare path). Read `.directory[]` for {kind, key}.
    pub fn sections(&self) -> Option<MediaContainer> {
        self.get_json("/library/sections")
    }

    /// GET /library/sections/{section_key}/all → `.metadata[]`
    pub fn section_items(&self, section_key: i64) -> Option<MediaContainer> {
        self.get_json(&format!("/library/sections/{section_key}/all"))
    }

    /// Paged variant (X-Plex-Container-Start/Size) for large libraries.
    pub fn section_items_paged(&self, section_key: i64, start: i64, size: i64) -> Option<MediaContainer> {
        let path = QueryBuilder::new(format!("/library/sections/{section_key}/all"))
            .int("X-Plex-Container-Start", start)
            .int("X-Plex-Container-Size", size)
            .build();
        self.get_json(&path)
    }

    /// Sorted/filtered/paged section listing — the Library browse grid's one fetch.
    /// `GET /library/sections/{k}/all?includeMeta=1&sort=…&genre=…&X-Plex-Container-Start&Size`
    /// → `.metadata[]` + `total_size` (+ `.meta` when `include_meta`).
    pub fn section_items_query(&self, q: &SectionQuery) -> Option<MediaContainer> {
        let mut b = QueryBuilder::new(format!("/library/sections/{}/all", q.section_key));
        if q.include_meta {
            b = b.int("includeMeta", 1);
        }
        b = b.opt_str("sort", q.sort);
        for (k, v) in q.filters {
            b = b.str(k, v);
        }
        let path = b
            .int("X-Plex-Container-Start", q.start)
            .int("X-Plex-Container-Size", q.size)
            .build();
        self.get_json(&path)
    }

    /// GET /library/sections/{key}/{directory} — a secondary directory: the filter value
    /// lists (`genre`/`year`/`decade`/`collection`/…, rows carry the tag id in `key` + a
    /// ready-made `fastKey` listing URL) and the `firstCharacter` per-letter index.
    /// → `.directory[]`.
    pub fn section_directory(&self, section_key: i64, directory: &str) -> Option<MediaContainer> {
        self.get_json(&format!("/library/sections/{section_key}/{directory}"))
    }

    /// GET /library/metadata/{rating_key} → the single item (`.metadata[0]`), or None.
    /// `includeChapters=1` / `includeMarkers=1` — PMS omits BOTH the `Chapter[]` and `Marker[]`
    /// arrays from the default response. Markers drive the in-player Skip Intro / Skip Credits
    /// prompt, so they ride the detail fetch rather than costing a second round trip.
    pub fn metadata(&self, rating_key: &str) -> Option<Metadata> {
        let path = QueryBuilder::new(format!("/library/metadata/{rating_key}"))
            .int("includeChapters", 1)
            .int("includeMarkers", 1)
            .build();
        self.get_json(&path)?.metadata.into_iter().next()
    }

    /// GET /library/metadata/{csv} — batch (spec `ids` is a CSV array); `.metadata[]`.
    pub fn metadata_many(&self, rating_keys: &[&str]) -> Option<MediaContainer> {
        self.get_json(&format!("/library/metadata/{}", rating_keys.join(",")))
    }

    /// GET /library/metadata/{rating_key}/children (D-5 undocumented but real) → `.metadata[]`.
    pub fn children(&self, rating_key: &str) -> Option<MediaContainer> {
        self.get_json(&format!("/library/metadata/{rating_key}/children"))
    }

    /// GET /library/metadata/{rating_key}/allLeaves — all episodes in one call. Group
    /// client-side by `parent_index`.
    pub fn all_leaves(&self, rating_key: &str) -> Option<MediaContainer> {
        self.get_json(&format!("/library/metadata/{rating_key}/allLeaves"))
    }

    /// GET /library/metadata/{rating_key}/related → `.hub[]`.
    pub fn related(&self, rating_key: &str) -> Option<MediaContainer> {
        self.get_json(&format!("/library/metadata/{rating_key}/related"))
    }

    /// GET /library/people/{person_id}/media → `.metadata[]` — every item this person appears
    /// in, **across every library section in one request** (docs/pms-api.md §2c). `person_id` is
    /// either the numeric [`Tag::id`](super::Tag::id) or the [`Tag::tag_key`](super::Tag::tag_key)
    /// guid; both were verified live against the same record on 2026-07-29.
    ///
    /// **Group the rows by each row's own `type`, never by the container's `viewGroup`** — that
    /// field is unreliable here: it read `"movie"` on a response whose only row was a `show`
    /// (verified on person 6059, 5 movies + 1 show). See `crate::person::split_by_type`.
    pub fn person_media(&self, person_id: &str) -> Option<MediaContainer> {
        self.get_json(&format!("/library/people/{person_id}/media"))
    }

    /// GET /:/scrobble — mark watched without a playback time (docs/pms-api.md §timeline).
    /// On a show/season it marks every leaf watched.
    pub fn scrobble(&self, rating_key: &str) {
        self.get_void(&format!("/:/scrobble?key={rating_key}&identifier=com.plexapp.plugins.library"));
    }

    /// GET /:/unscrobble — mark unwatched (clears viewCount + viewOffset).
    pub fn unscrobble(&self, rating_key: &str) {
        self.get_void(&format!("/:/unscrobble?key={rating_key}&identifier=com.plexapp.plugins.library"));
    }

    /// PUT /library/parts/{id} — select the part's audio/subtitle streams SERVER-side (the
    /// transcoder encodes the SELECTED audio and burns the SELECTED subtitle; a query-param
    /// on the stream URL does NOT change them, only this PUT does). `subtitleStreamID` is
    /// always sent — 0 keeps subs OFF (suppresses a default-selected burn); `audioStreamID`
    /// only when the user switched. Returns the HTTP status (route logs it).
    pub fn select_streams(&self, sel: &StreamSelection) -> i32 {
        let q = QueryBuilder::new(format!("/library/parts/{}", sel.part_id))
            .int("allParts", 1)
            .int("subtitleStreamID", sel.subtitle_stream_id)
            .opt_int("audioStreamID", sel.audio_stream_id);
        self.put(&q.build())
    }

    /// The direct-play stream target: the raw part `key` GET, carrying the per-playback
    /// session id + identity so PMS keys the /status/sessions entry by session (not a
    /// token= fallback), keeping the timeline correlation consistent.
    pub fn direct_play_url(&self, part_key: &str, session: &str) -> StreamUrl {
        let q = QueryBuilder::new(part_key).str("X-Plex-Session-Identifier", session);
        let path = self.playback_identity(q).build();
        StreamUrl { host: self.host.clone(), port: self.port, path: self.with_token(&path) }
    }
}
