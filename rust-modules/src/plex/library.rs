//! Library operations (impl Client): sections, section items, metadata, children/leaves,
//! related, server-side stream selection, and the direct-play target.
use super::client::{Client, QueryBuilder, StreamUrl};
use super::models::{MediaContainer, Metadata};
use super::params::StreamSelection;

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

    /// GET /library/metadata/{rating_key} → the single item (`.metadata[0]`), or None.
    pub fn metadata(&self, rating_key: &str) -> Option<Metadata> {
        self.get_json(&format!("/library/metadata/{rating_key}"))?
            .metadata
            .into_iter()
            .next()
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

    /// PUT /library/parts/{part_id}?allParts=1&subtitleStreamID=…[&audioStreamID=…] — the
    /// correct server-side stream selection. Returns the HTTP status.
    pub fn select_streams(&self, sel: &StreamSelection) -> i32 {
        let mut q = QueryBuilder::new(format!("/library/parts/{}", sel.part_id));
        if sel.all_parts {
            q = q.int("allParts", 1);
        }
        // subtitleStreamID is ALWAYS sent (0 = subs off / no burn); audioStreamID only when
        // the user switched (else keep the server default).
        q = q.int("subtitleStreamID", sel.subtitle_stream_id);
        if sel.audio_stream_id > 0 {
            q = q.int("audioStreamID", sel.audio_stream_id);
        }
        self.put(&q.build())
    }

    /// Direct-play target: http://host:port{part_key}?X-Plex-Token — for stream::http_open.
    /// `part_key` is the verbatim Media.Part.key from metadata.
    pub fn direct_play_url(&self, part_key: &str) -> StreamUrl {
        StreamUrl {
            host: self.host.clone(),
            port: self.port,
            path: self.with_token(part_key),
        }
    }
}
