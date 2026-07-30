//! Hub operations (impl Client): the home shelves, the dedicated Continue Watching /
//! Promoted hubs, and search. All return `.hub[]` (each hub carries `.metadata[]`).
use super::client::{Client, QueryBuilder};
use super::models::MediaContainer;

impl Client {
    /// GET /hubs?count=… (D-4: drop the no-op excludeContinueWatching) → `.hub[]`.
    pub fn home_hubs(&self, count: i64) -> Option<MediaContainer> {
        self.get_json(&QueryBuilder::new("/hubs").int("count", count).build())
    }

    /// GET /hubs/continueWatching?count=… — the dedicated Continue Watching hub.
    pub fn continue_watching(&self, count: i64) -> Option<MediaContainer> {
        self.get_json(&QueryBuilder::new("/hubs/continueWatching").int("count", count).build())
    }

    /// PUT /actions/removeFromContinueWatching?ratingKey=… — **hide** an item from the deck. Returns
    /// whether the server accepted it.
    ///
    /// Absent from `docs/plex-openapi.json` (which knows only `/:/scrobble` and `/:/unscrobble`) —
    /// established live against PMS 1.43.3 on 2026-07-30, along with two things its NAME hides:
    ///
    /// * It is a **hide flag, not a progress reset.** The item's `viewOffset` is untouched, so the
    ///   resume point survives and playing it again picks up where you were. Do NOT reach for
    ///   `unscrobble` to do this — that clears `viewCount` and `viewOffset` and loses the position.
    /// * **It does not affect every "continue watching" hub.** Measured on one episode:
    ///   `/hubs/continueWatching` drops it, `/hubs`'s `home.ondeck` drops it, and `/hubs`'s
    ///   `home.continue` **keeps listing it**. So a Home shelf built from `home.continue` renders a
    ///   card the server has already been told to hide — which is exactly why `pms.rs` sources its
    ///   shelf from [`Client::continue_watching`] instead of merging the `/hubs` pair.
    ///
    /// PUT is the only verb routed here; GET and POST both 404.
    pub fn remove_from_continue_watching(&self, rating_key: &str) -> bool {
        let path = QueryBuilder::new("/actions/removeFromContinueWatching")
            .str("ratingKey", rating_key)
            .build();
        (200..300).contains(&self.put(&path))
    }

    /// GET /hubs/promoted?count=… — the home screen's featured rows.
    pub fn promoted(&self, count: i64) -> Option<MediaContainer> {
        self.get_json(&QueryBuilder::new("/hubs/promoted").int("count", count).build())
    }

    /// GET /hubs/search?query=…&limit=… — a real search screen → `.hub[]`.
    pub fn search(&self, query: &str, limit: i64) -> Option<MediaContainer> {
        let path = QueryBuilder::new("/hubs/search")
            .str("query", query)
            .int("limit", limit)
            .build();
        self.get_json(&path)
    }
}
