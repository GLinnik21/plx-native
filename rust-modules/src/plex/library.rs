//! Library operations (impl Client): sections, section items, metadata, children/leaves,
//! and related.
use super::client::{Client, QueryBuilder};
use super::models::{MediaContainer, Metadata};

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
    /// `includeChapters=1` — PMS omits the `Chapter[]` array from the default response.
    pub fn metadata(&self, rating_key: &str) -> Option<Metadata> {
        let path = QueryBuilder::new(format!("/library/metadata/{rating_key}"))
            .int("includeChapters", 1)
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

}
