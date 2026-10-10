//! Hub operations (impl Client): the home shelves, the dedicated Continue Watching /
//! Promoted hubs, and search. All return `.hub[]`.
//!
//! **A hub does NOT always carry its items in `.metadata[]`** — this line said it did, which is
//! the exact misconception that renders two of the search screen's five shelves as nothing at
//! all, silently. The home hubs are `Metadata[]` only because a home shelf holds media;
//! `/hubs/search` also answers with people, and those arrive in `.directory[]` — as do
//! collections, unless the request carries `includeCollections=1` (which [`Client::search`]
//! always sends; they then arrive as full `.metadata[]` rows).
//! See [`super::Hub::directory`] for the per-type split and [`Client::search`] for the endpoint.
use super::client::{Client, QueryBuilder};
use super::models::MediaContainer;
use super::paging::{paged_path, without_window, PageReq};

/// The keys a hub's pages may be fetched from: the only prefixes a server's listing keys use.
pub fn is_pageable_hub_key(key: &str) -> bool {
    key.starts_with("/hubs/") || key.starts_with("/library/sections/") || is_similar_key(key)
}

/// `/library/metadata/{digits}/similar`, with or without a query. Only this one shape under the
/// metadata prefix: its siblings (`children`, `extras`, …) are not listings a hub points at.
fn is_similar_key(key: &str) -> bool {
    let path = key.split_once('?').map_or(key, |(path, _)| path);
    let Some(rating_key) = path.strip_prefix("/library/metadata/").and_then(|rest| rest.strip_suffix("/similar")) else {
        return false;
    };
    !rating_key.is_empty() && rating_key.bytes().all(|b| b.is_ascii_digit())
}

impl Client {
    pub fn hub_items_paged(&self, key: &str, start: i64, size: i64) -> Option<MediaContainer> {
        if !is_pageable_hub_key(key) { return None; }
        self.get_json(&hub_page_path(key, start, size))
    }

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
            &QueryBuilder::new("/hubs")
                .int("count", count)
                .int("excludeContinueWatching", 1)
                .build(),
        )
    }

    /// GET /hubs/sections/{sectionId}?count=… — **one LIBRARY's own shelves**, in the order the
    /// server's owner arranged them in *Plex Web → Manage → Libraries → Library Recommended*.
    ///
    /// The sibling of [`Client::home_hubs`], one level down: that one is whole-SERVER and answers
    /// with rows from every library, this one is scoped to a section and quotes the owner's own
    /// table. Verified live against PMS 1.43.3 on 2026-09-05 — `docs/pms-api.md` §3a is the record,
    /// and five of its findings contradict `docs/plex-openapi.json`. The three that decide code
    /// here:
    ///
    /// * **Items are EMBEDDED** in each hub's `Metadata[]`. The spec's own example shows a nonzero
    ///   `size` with an empty array, which would have meant a second request per shelf; that is an
    ///   artifact of the example.
    /// * **Empty hubs are still returned**, with `size: 0`, and on a sparse library that is most of
    ///   them — one movie section answered with 6 hubs of which 5 were empty. A caller that draws
    ///   what it is given draws five headings over nothing, so dropping them is required.
    /// * **`onlyTransient` is a no-op on this server**, so it is not sent. `count` is
    ///   items-per-hub (default 6) and never changes how many hubs come back.
    ///
    /// **And two hubs change identity between requests** — the genre and actor/director shelves
    /// rotate their subject on every call, sometimes answering empty and vanishing from the drawn
    /// set. A refetch legitimately returns a different shelf count and different ids with nothing
    /// changed on the server, which is a fact about this endpoint rather than about any caller.
    pub fn library_hubs(&self, section_key: i64, count: i64) -> Option<MediaContainer> {
        self.get_json(
            &QueryBuilder::new(&format!("/hubs/sections/{section_key}"))
                .int("count", count)
                .build(),
        )
    }

    /// GET /hubs/continueWatching?count=… — the dedicated Continue Watching hub.
    pub fn continue_watching(&self, count: i64) -> Option<MediaContainer> {
        self.get_json(
            &QueryBuilder::new("/hubs/continueWatching")
                .int("count", count)
                .build(),
        )
    }

    /// GET /hubs/continueWatching/items, one page: the listing behind the dedicated hub, by offset,
    /// last viewed descending. The Continue Watching deck reads each server's listing with this.
    pub fn continue_watching_page(&self, start: i64, size: i64) -> Option<MediaContainer> {
        self.get_json(&continue_watching_page_path(start, size))
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

    /// GET /hubs/search?query=…[&limit=…][&sectionId=…]&includeCollections=1 — the search screen
    /// → `.hub[]`.
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
    ///   `Metadata[]`, `actor`/`director` as `Directory[]`. See [`super::Hub::directory`]; a reader
    ///   that assumes `Metadata` draws the Cast & Crew shelf as nothing, silently.
    /// * **`includeCollections=1` is always sent**, because it moves the `collection` hub from
    ///   `Directory[]` TAG rows (a tag `id`, `count` and `key` — no `ratingKey`, no `thumb`) into
    ///   full collection `Metadata[]` rows (`ratingKey`, `index` == the tag id, `thumb`,
    ///   `childCount`, `UltraBlurColors`), which is what lets a search hit carry artwork and open
    ///   the collection page by ratingKey. Measured live (docs/pms-api.md §2b); every other hub is
    ///   unchanged by it, so the person page's resolve (which reads only `actor`/`director`) is
    ///   indifferent. A server that ignores the flag still answers the old tag rows, which
    ///   `search::project` keeps as the fallback.
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
    ///   RETURNED, already capped — never the total number of matches — and the wire's `more`
    ///   attribute (which [`super::Hub`] does not model) stayed `false` on every truncated hub, so
    ///   it cannot be used to offer a "see all" either.
    /// * **Hub ORDER moves per query** (`sta` ranks `actor` first, `star` ranks `movie` first) and a
    ///   response carries ~17 hubs, most with `size: 0`. `search::KINDS` fixes the shelf order for
    ///   exactly this reason: honouring the server's would move a row under a typing user's focus.
    pub fn search(&self, query: &str, limit: i64, section_id: i64) -> Option<MediaContainer> {
        self.get_json(&search_path(query, limit, section_id))
    }

    /// One window of ONE kind's hits: `/library/search?searchTypes=<kind>`, which pages by offset
    /// where `/hubs/search` cannot (a window there pages the hub list, not a hub's rows;
    /// `docs/pms-api.md`, "Paging, observed"). Rows are in `SearchResult` (`Metadata` for a card, a
    /// `Directory` tag row for a person). Measured: `limit` caps `totalSize`, so `limit` is the
    /// window's end; the end of a listing is a SHORT page, never `totalSize`. An unknown kind
    /// answers empty with no error, which `search::tail` treats as "unsupported", not "end".
    pub fn search_typed(&self, query: &str, kind: SearchKind, req: PageReq) -> Option<MediaContainer> {
        self.get_json(&typed_search_path(query, kind, req))
    }
}

/// The kinds `/library/search` answers for (`music` too, which this app does not draw).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Movies,
    /// Shows first, then episodes, in one list.
    Tv,
    People,
}

impl SearchKind {
    pub fn param(self) -> &'static str {
        match self {
            SearchKind::Movies => "movies",
            SearchKind::Tv => "tv",
            SearchKind::People => "people",
        }
    }
}

fn typed_search_path(query: &str, kind: SearchKind, req: PageReq) -> String {
    let path = QueryBuilder::new("/library/search")
        .str("query", query)
        .str("searchTypes", kind.param())
        .int("limit", (req.start + req.size) as i64)
        .build();
    paged_path(&path, req)
}

fn continue_watching_page_path(start: i64, size: i64) -> String {
    paged_path("/hubs/continueWatching/items", PageReq::window(start, size))
}

fn hub_page_path(key: &str, start: i64, size: i64) -> String {
    paged_path(&without_window(key), PageReq::window(start, size))
}

/// The `/hubs/search` path, split out of [`Client::search`] so the query this app puts on the wire
/// is host-testable without a server — the transport is what makes the method itself untestable,
/// and both numbers here have a value (0) that the server rejects with a body this layer reports
/// as `None`, indistinguishably from a dead socket. `includeCollections=1` rides on every request
/// ([`Client::search`] says why).
fn search_path(query: &str, limit: i64, section_id: i64) -> String {
    QueryBuilder::new("/hubs/search")
        .str("query", query)
        .opt_int("limit", limit)
        .opt_int("sectionId", section_id)
        .int("includeCollections", 1)
        .build()
}

#[cfg(test)]
mod tests {
    use super::{search_path, typed_search_path, SearchKind};
    use super::super::paging::PageReq;

    /// The typed listing is `/library/search` with the kind, the query and a window. `limit` is the
    /// window's end (`start + size`), which is enough for the slice whichever way the server reads
    /// the window parameters; the end of a listing is a short page, never `totalSize`.
    #[test]
    fn a_typed_search_asks_for_its_kind_and_a_limit_that_covers_the_window() {
        assert_eq!(
            typed_search_path("tom & jerry", SearchKind::Movies, PageReq { start: 24, size: 25 }),
            "/library/search?query=tom%20%26%20jerry&searchTypes=movies&limit=49&X-Plex-Container-Start=24&X-Plex-Container-Size=25"
        );
        assert_eq!(
            typed_search_path("ab", SearchKind::Tv, PageReq { start: 12, size: 1 }),
            "/library/search?query=ab&searchTypes=tv&limit=13&X-Plex-Container-Start=12&X-Plex-Container-Size=1"
        );
        assert_eq!(SearchKind::People.param(), "people");
    }

    /// A row is `{Metadata}` or `{Directory}` (a person), each beside a score; both land in the
    /// container's `search_result` rows.
    #[test]
    fn a_search_result_row_wraps_a_card_or_a_person() {
        let mc: super::MediaContainer = serde_json::from_str(r#"{"size":2,"totalSize":9,"offset":4,
            "SearchResult":[{"Metadata":{"ratingKey":"7","type":"movie","title":"A"},"score":0.9},
                            {"Directory":{"tag":"P","tagKey":"k1","id":3,"count":2},"score":0.8}]}"#).unwrap();
        assert_eq!(mc.offset, 4);
        assert_eq!(mc.search_result.len(), 2);
        assert_eq!(mc.search_result[0].metadata.as_ref().map(|m| m.rating_key.as_str()), Some("7"));
        assert_eq!(mc.search_result[1].directory.as_ref().map(|t| t.tag_key.as_str()), Some("k1"));
    }

    /// The query is typed by a user, so it is arbitrary text — and it is the one parameter here
    /// that can carry a `&`, a `?` or a `=`. Un-encoded, "tom & jerry" would split into a bogus
    /// second parameter and the server would answer for "tom"; a `#` would truncate the path
    /// outright. `QueryBuilder::str` is the choke point that prevents it, and this is the test
    /// that keeps this call site using it.
    #[test]
    fn the_typed_query_is_percent_encoded_because_a_user_can_type_anything() {
        assert_eq!(
            search_path("tom & jerry", 8, 0),
            "/hubs/search?query=tom%20%26%20jerry&limit=8&includeCollections=1"
        );
        // an apostrophe, an accent and a '+' are all reserved or non-ASCII here
        assert_eq!(
            search_path("l'été+", 5, 0),
            "/hubs/search?query=l%27%C3%A9t%C3%A9%2B&limit=5&includeCollections=1"
        );
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
        assert_eq!(
            search_path("wallace", 0, 0),
            "/hubs/search?query=wallace&includeCollections=1",
            "neither number"
        );
        assert_eq!(
            search_path("wallace", 8, 0),
            "/hubs/search?query=wallace&limit=8&includeCollections=1"
        );
        assert_eq!(
            search_path("wallace", 0, 1),
            "/hubs/search?query=wallace&sectionId=1&includeCollections=1"
        );
        assert_eq!(
            search_path("wallace", 8, 1),
            "/hubs/search?query=wallace&limit=8&sectionId=1&includeCollections=1"
        );
    }

    /// **Collections are asked for as full rows.** Without `includeCollections=1` the server
    /// files its `collection` hub as tag-shaped `Directory[]` rows with no `ratingKey` and no
    /// `thumb` (measured, docs/pms-api.md §2b), and the search shelf can neither draw the
    /// collection's poster nor open its page by ratingKey.
    #[test]
    fn every_search_asks_for_collections_as_full_metadata_rows() {
        for (limit, section) in [(0, 0), (8, 0), (0, 1), (8, 1)] {
            let path = search_path("wallace", limit, section);
            assert!(path.ends_with("&includeCollections=1"), "{path}");
        }
    }
}

#[cfg(test)]
mod paging_tests {
    use super::{continue_watching_page_path, hub_page_path, is_pageable_hub_key};

    #[test]
    fn only_a_similar_list_under_a_metadata_item_is_pageable_from_that_prefix() {
        assert!(is_pageable_hub_key("/library/metadata/5/similar"));
        assert!(is_pageable_hub_key("/library/metadata/5/similar?sort=titleSort"));
        // The `/library/metadata/` prefix is NOT admitted: these are the shapes it would let in.
        for key in [
            "/library/metadata/5/children",
            "/library/metadata/5/extras",
            "/library/metadata/5",
            "/library/metadata/x/similar",
            "/library/metadata//similar",
            "/library/metadata/5/similar/extra",
        ] {
            assert!(!is_pageable_hub_key(key), "{key}");
        }
    }

    #[test]
    fn paging_preserves_the_provider_filter_and_replaces_its_page_parameters() {
        assert_eq!(hub_page_path("/hubs/home/recentlyAdded?type=2&sectionID=7", 12, 24),
            "/hubs/home/recentlyAdded?type=2&sectionID=7&X-Plex-Container-Start=12&X-Plex-Container-Size=24");
        assert_eq!(hub_page_path("/library/sections/7/all?sort=addedAt%3Adesc&X-Plex-Container-Size=12&X-Plex-Container-Start=0", 36, 24),
            "/library/sections/7/all?sort=addedAt%3Adesc&X-Plex-Container-Start=36&X-Plex-Container-Size=24");
    }

    #[test]
    fn the_continue_watching_listing_is_read_by_offset() {
        assert_eq!(continue_watching_page_path(11, 13),
            "/hubs/continueWatching/items?X-Plex-Container-Start=11&X-Plex-Container-Size=13");
    }
}
