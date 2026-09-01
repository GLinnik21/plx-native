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

    /// GET / — what this server calls itself (`friendlyName`), or `None` when it did not answer
    /// or answered with no name. The Sources list heads each server's group with it, which is the
    /// only place in the app a MACHINE is named.
    ///
    /// Same endpoint `serverinfo`'s version/Plex-Pass probe uses, deliberately not folded into it:
    /// that one is a process-global fact about the CURRENT server refreshed on every session path,
    /// this is a per-server string a roster row needs once. Sharing the state would mean the last
    /// server discovered renamed the one you are signed in to.
    pub fn friendly_name(&self) -> Option<String> {
        self.get_json("/")
            .map(|mc| mc.friendly_name)
            .filter(|s| !s.is_empty())
    }

    /// GET /library/all?guid=… — **does THIS server hold this film, and under which key?**
    ///
    /// The one query in the app that crosses libraries rather than naming one, which is why the
    /// rows it returns carry `librarySectionTitle`: the caller does not know in advance which
    /// library will answer, and "Also available" names the library, not the machine.
    ///
    /// `None` is a transport/parse failure; `Some` with an empty `metadata` is the server
    /// answering *"I do not have it"*, and the two must not be collapsed — a share that is merely
    /// unreachable would otherwise read as one that does not hold the film, which is a row silently
    /// missing from the panel rather than a source visibly not answering.
    ///
    /// Verified live against both of this household's servers, 2026-08-14: `size=0` for a film only
    /// ours holds, `size=1` for one both hold — returning the SHARE's own `ratingKey` and its own
    /// localized title for the same guid.
    /// **`type` is what makes the answer carry `Media[]`**, and without it the row has no quality to
    /// show. Measured against this server 2026-08-14, same guid three ways:
    ///
    /// ```text
    /// ?guid=…               size 1, no Media
    /// ?guid=…&includeMedia=1 size 1, no Media   (not the knob it looks like)
    /// ?guid=…&type=1        size 1, Media[0] = 4k 3840x2160
    /// ```
    ///
    /// The type comes off the guid itself (`plex://movie/…`), which is the only place it is known
    /// without another round trip — and a guid whose kind we do not recognise sends no `type` at
    /// all rather than guessing 1, because a wrong type answers `size 0` and would read as "that
    /// server does not have it".
    pub fn find_by_guid(&self, guid: &str) -> Option<MediaContainer> {
        if guid.is_empty() {
            return None;
        }
        let mut q = QueryBuilder::new("/library/all".to_string()).str("guid", guid);
        if let Some(t) = guid_type(guid) {
            q = q.int("type", t);
        }
        self.get_json(&q.build())
    }

    /// GET /library/sections/{section_key}/all → `.metadata[]`
    pub fn section_items(&self, section_key: i64) -> Option<MediaContainer> {
        self.get_json(&format!("/library/sections/{section_key}/all"))
    }

    /// Paged variant (X-Plex-Container-Start/Size) for large libraries.
    pub fn section_items_paged(
        &self,
        section_key: i64,
        start: i64,
        size: i64,
    ) -> Option<MediaContainer> {
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
    ///
    /// `includeOnDeck=1` adds, for a SHOW, the one episode the server considers next to watch —
    /// `OnDeck.Metadata`, a whole episode record (thumb, summary, `Media`, `viewOffset`). It is the
    /// only show-level answer to "what's next": the client otherwise holds ONE season's episodes at a
    /// time, so anything it computed itself would change with the selected season tab. Verified live
    /// 2026-07-30 on rk 437 → S2E2 at 635510/3130720 while season 1 was the loaded tab.
    pub fn metadata(&self, rating_key: &str) -> Option<Metadata> {
        let path = QueryBuilder::new(format!("/library/metadata/{rating_key}"))
            .int("includeChapters", 1)
            .int("includeMarkers", 1)
            .int("includeOnDeck", 1)
            .build();
        self.get_json(&path)?.metadata.into_iter().next()
    }

    /// GET /library/metadata/{csv} — the FULL records of MANY items in ONE request. The answer
    /// carries one `.metadata[]` row per key, in request order; the CSV is joined HERE, because
    /// assembling path syntax is this layer's job (`plex/CLAUDE.md`: every PMS query is built here).
    ///
    /// **This is the only way to get what a LISTING response strips.** Verified live 2026-07-30
    /// against PMS 1.43.3 while building the person page's role captions:
    /// `/library/people/{id}/media` does return a `Role[]` on every row, but each entry carries
    /// **nothing except `tag`** — no `id`, and no `role` (the character name). `?includeRole=1`
    /// changes nothing. The full item carries both, so one batched read of the shelf's keys is the
    /// cheap way to caption a whole filmography.
    ///
    /// The response is **trimmed to the tag arrays**, which is the whole point of batching it: nobody
    /// wants 48 items' `Media`/`Genre`/`Director`/summary just to read a credit. Measured on four
    /// movies, the untrimmed response is **32.8 KB** against 12.9 KB trimmed. The trim is a property
    /// of this operation, not of the caller, so it lives here rather than as query fragments a store
    /// module passes in. (The server silently KEEPS `Image`, `UltraBlurColors` and `Field` whatever
    /// you ask, so they are not listed — naming them would just read as if they went.)
    ///
    /// Absent from `docs/plex-openapi.json`, which documents only the single-key form.
    pub fn metadata_many(&self, rating_keys: &[&str]) -> Option<MediaContainer> {
        const EXCLUDE_ELEMENTS: &str =
            "Media,Genre,Country,Collection,Director,Writer,Producer,Similar,Chapter,Marker,Guid,Rating,Review,Extras";
        let path = QueryBuilder::new(format!("/library/metadata/{}", rating_keys.join(",")))
            .str("excludeElements", EXCLUDE_ELEMENTS)
            .str("excludeFields", "summary,tagline")
            .build();
        self.get_json(&path)
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
    /// On a show/season it marks every leaf watched. Returns whether the server took it.
    ///
    /// The verdict used to be discarded (`get_void`), which was harmless while the call was inline
    /// on the frame loop and the blocking refetch behind it re-read the truth a moment later. It is
    /// not harmless now: the write runs on a worker (`crate::viewstate`) whose only report to the
    /// main thread is this bool, and "the server never answered" is the case the whole move exists
    /// for. It does NOT distinguish a 200 from a 404 — `get_ok` is `http_get`'s own success — which
    /// is the honest limit of a GET whose body carries nothing.
    pub fn scrobble(&self, rating_key: &str) -> bool {
        self.get_ok(&format!(
            "/:/scrobble?key={rating_key}&identifier=com.plexapp.plugins.library"
        ))
    }

    /// GET /:/unscrobble — mark unwatched (clears viewCount + viewOffset). Reports like
    /// [`Client::scrobble`].
    pub fn unscrobble(&self, rating_key: &str) -> bool {
        self.get_ok(&format!(
            "/:/unscrobble?key={rating_key}&identifier=com.plexapp.plugins.library"
        ))
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
        StreamUrl {
            origin: self.origin.clone(),
            path: self.with_token(&path),
        }
    }
}

/// PMS's numeric `type` for a `plex://<kind>/<id>` guid — the metadata provider's own kind, which is
/// the one thing about an item a guid states outright.
///
/// `None` for a kind this app has no number for, and the caller then omits the parameter: a WRONG
/// type answers `size 0`, which is indistinguishable from "that server does not hold this item" and
/// would quietly drop a real copy out of "Also available".
fn guid_type(guid: &str) -> Option<i64> {
    let kind = guid.strip_prefix("plex://")?.split('/').next()?;
    match kind {
        "movie" => Some(1),
        "show" => Some(2),
        "season" => Some(3),
        "episode" => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::guid_type;

    /// The numbers are PMS's, and the mapping is the only thing standing between "Also available"
    /// showing a quality badge and showing none — `type` is what makes `/library/all?guid=…` return
    /// `Media[]` at all (see `find_by_guid`).
    #[test]
    fn a_guid_states_its_own_kind_and_an_unknown_kind_sends_no_type() {
        assert_eq!(guid_type("plex://movie/6856893830a4aaafd5c4291d"), Some(1));
        assert_eq!(guid_type("plex://show/5d9c081b170e05001f303f9e"), Some(2));
        assert_eq!(guid_type("plex://season/abc"), Some(3));
        assert_eq!(guid_type("plex://episode/abc"), Some(4));
        // an agent guid from before the plex:// scheme, and a kind with no level here: no type
        // rather than a guess
        assert_eq!(
            guid_type("com.plexapp.agents.imdb://tt0083658?lang=en"),
            None
        );
        assert_eq!(guid_type("plex://artist/abc"), None);
        assert_eq!(guid_type(""), None);
    }
}
