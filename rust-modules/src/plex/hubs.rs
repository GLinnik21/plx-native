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
