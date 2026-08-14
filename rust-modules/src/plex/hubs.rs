//! Hub operations (impl Client): the home shelves, the dedicated Continue Watching /
//! Promoted hubs, and search. All return `.hub[]`.
//!
//! **A hub does NOT always carry its items in `.metadata[]`** — this line said it did, which is
//! the exact misconception that renders three of the search screen's five shelves as nothing at
//! all, silently. The home hubs are `Metadata[]` only because a home shelf holds media;
//! `/hubs/search` also answers with people and collections, and those arrive in `.directory[]`.
//! See [`super::Hub::directory`] for the per-type split and [`Client::search`] for the endpoint.
use super::client::{Client, QueryBuilder};
use super::models::MediaContainer;

impl Client {
    /// GET /hubs?count=…&excludeContinueWatching=1 → `.hub[]`.
    ///
    /// **`excludeContinueWatching` is NOT a no-op**, which this call and its comment asserted for
    /// as long as they existed ("D-4: drop the no-op excludeContinueWatching"). Measured against
    /// this server 2026-08-14: `/hubs?count=12` returns 10 hubs, `&excludeContinueWatching=1`
    /// returns 8 — it drops `home.continue` and `home.ondeck`.
    ///
    /// Sending it is right for us on both counts. Those two hubs are exactly what
    /// [`continue_watching`] fetches properly (see its doc for why the dedicated endpoint is the
    /// only honest one), so we parse and discard them today; and with several sources on Home the
    /// waste is per source per fetch, not once.
    pub fn home_hubs(&self, count: i64) -> Option<MediaContainer> {
        self.get_json(
            &QueryBuilder::new("/hubs").int("count", count).int("excludeContinueWatching", 1).build(),
        )
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

    /// GET /hubs/search?query=…[&limit=…][&sectionId=…] — the search screen → `.hub[]`.
    ///
    /// **Both numbers are optional and 0 means "don't send it"** — `limit` 0 = the server's own
    /// default (3 rows per hub), `section_id` 0 = the whole server. Neither may go on the wire as
    /// a literal 0; see the trap below.
    ///
    /// Everything here was measured against this household's PMS 1.43.3 on 2026-08-14 (six
    /// queries, three `sectionId` variants, four `limit` variants), because the endpoint's
    /// behaviour is not what its parameter names suggest and `docs/plex-openapi.json` is wrong
    /// about the response shape. The written form is `docs/pms-api.md` §3b.
    ///
    /// * **Items arrive in TWO containers** — `movie`/`show`/`episode`/`album`/`artist`/`track` as
    ///   `Metadata[]`, `actor`/`director`/`collection` as `Directory[]`. See [`super::Hub::directory`];
    ///   a reader that assumes `Metadata` draws three of the five design shelves as nothing, silently.
    /// * **`sectionId` RANKS, it does not filter.** With `sectionId=1` (Movies) the `movie` hub moves
    ///   ahead of `show`, but every row from every *other* section still comes back — the section 2
    ///   episodes, shows and actors are all present and unchanged. So it cannot be used to scope a
    ///   search to one library; the client must filter if it wants that.
    /// * **A ZERO on either number is an ERROR the app cannot see.** `sectionId=0` is a section no
    ///   server has (**400**) and `limit=0` is a **500** — while OMITTING either is a healthy 200.
    ///   Every PMS error body here is `text/html`, so `get_json` returns `None`, which is the same
    ///   `None` a dead socket gives: a search that never works and looks like the network. That is
    ///   what both `opt_int`s are for, and why neither may go back to `int`.
    /// * **A blank query is a 400 too**, and a ONE-character query is a 200 with every hub empty
    ///   (`a` → nothing, `to` → seven populated hubs) — which is what `search::MIN_QUERY` is.
    /// * **`limit` caps each hub separately**, not the response: `limit=2` turned a 6-row `movie`
    ///   hub into 2 and left the 1-row `show` hub alone. `Hub.size` is therefore the count
    ///   RETURNED, already capped — never the total number of matches — and `Hub.more` stayed
    ///   `false` on every truncated hub, so it cannot be used to offer a "see all" either.
    /// * **Hub ORDER moves per query** (`sta` ranks `actor` first, `star` ranks `movie` first) and a
    ///   response carries ~17 hubs, most with `size: 0`. `search::KINDS` fixes the shelf order for
    ///   exactly this reason: honouring the server's would move a row under a typing user's focus.
    pub fn search(&self, query: &str, limit: i64, section_id: i64) -> Option<MediaContainer> {
        self.get_json(&search_path(query, limit, section_id))
    }
}

/// The `/hubs/search` path, split out of [`Client::search`] so the query this app puts on the wire
/// is host-testable without a server — the transport is what makes the method itself untestable,
/// and both numbers here have a value (0) that the server rejects with a body this layer reports
/// as `None`, indistinguishably from a dead socket.
fn search_path(query: &str, limit: i64, section_id: i64) -> String {
    QueryBuilder::new("/hubs/search")
        .str("query", query)
        .opt_int("limit", limit)
        .opt_int("sectionId", section_id)
        .build()
}

#[cfg(test)]
mod tests {
    use super::search_path;

    /// The query is typed by a user, so it is arbitrary text — and it is the one parameter here
    /// that can carry a `&`, a `?` or a `=`. Un-encoded, "tom & jerry" would split into a bogus
    /// second parameter and the server would answer for "tom"; a `#` would truncate the path
    /// outright. `QueryBuilder::str` is the choke point that prevents it, and this is the test
    /// that keeps this call site using it.
    #[test]
    fn the_typed_query_is_percent_encoded_because_a_user_can_type_anything() {
        assert_eq!(search_path("tom & jerry", 8, 0), "/hubs/search?query=tom%20%26%20jerry&limit=8");
        // an apostrophe, an accent and a '+' are all reserved or non-ASCII here
        assert_eq!(search_path("l'été+", 5, 0), "/hubs/search?query=l%27%C3%A9t%C3%A9%2B&limit=5");
    }

    /// **Neither number may reach the server as a literal 0**, and the reason is not tidiness —
    /// both zeros are errors, and both errors are invisible to this layer.
    ///
    /// Probed live 2026-08-14: `sectionId=0` is a section no server has (**400**) and `limit=0` is
    /// a **500**, while omitting either is a healthy 200 — `limit` defaults to 3 rows per hub.
    /// Every PMS error body on this endpoint is `text/html`, so `get_json` parses nothing and
    /// returns `None`, exactly the `None` a dead socket gives. A search built the obvious way with
    /// `.int` would therefore never work AND would report itself as a network fault.
    #[test]
    fn a_zero_sends_no_parameter_because_the_server_rejects_both_zeros() {
        assert_eq!(search_path("wallace", 0, 0), "/hubs/search?query=wallace", "neither number");
        assert_eq!(search_path("wallace", 8, 0), "/hubs/search?query=wallace&limit=8");
        assert_eq!(search_path("wallace", 0, 1), "/hubs/search?query=wallace&sectionId=1");
        assert_eq!(search_path("wallace", 8, 1), "/hubs/search?query=wallace&limit=8&sectionId=1");
        // …and the order is the server's: query, then limit, then the scope hint
        assert!(search_path("x", 3, 2).ends_with("?query=x&limit=3&sectionId=2"));
    }
}
