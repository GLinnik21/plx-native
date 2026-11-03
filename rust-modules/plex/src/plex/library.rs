//! Library operations (impl Client): sections, section items (`section_items_query`), metadata,
//! children (`children`, `children_paged`), related (`related`) — plus the two part-level playback ops (stream selection, direct-play target).
use super::client::{urlenc_str, Client, QueryBuilder, StreamUrl};
use super::models::{ListingFacts, MediaContainer, Metadata};
use super::paging::{paged_path, PageReq};
use super::params::{SectionQuery, StreamSelection};

/// Largest number of guids one `find_by_guids` request carries.
pub const GUIDS_PER_REQUEST: usize = 40;

/// The Plex `type` a `find_by_guids` call filters on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuidKind {
    /// `type=1`
    Movie,
    /// `type=2`
    Show,
}

/// One guid this server holds.
#[derive(Clone, Debug, PartialEq)]
pub struct GuidMatch {
    pub guid: String,
    /// This server's own ratingKey for it.
    pub rating_key: String,
    pub library_section_id: Option<i64>,
    /// The row's `UltraBlurColors` corners (`UltraBlurColors::corners`) when the server sent them.
    pub blur: Option<[[f32; 3]; 4]>,
    /// What the row says of the title for a caption (genre, video, ratings, length, counts): the
    /// Watchlist words a matched tile with them, as every poster does.
    pub facts: ListingFacts,
}

/// Why `find_by_guids` could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuidLookupError {
    /// The server did not answer (or answered unreadably) for at least one chunk.
    Unanswered,
}

fn sidecar_key_allowed(key: &str) -> bool {
    let Some(tail) = key.strip_prefix("/library/streams/") else { return false; };
    let (id, ext) = tail.split_once('.').unwrap_or((tail, ""));
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
        && ext.len() <= 10 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

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

    /// Which of `guids` does THIS server hold? One `GET /library/all?type=T&guid=a,b,c` per chunk
    /// of at most [`GUIDS_PER_REQUEST`], `T` = 1 (movie) or 2 (show) from `kind`. Only `plex://`
    /// guids are sent; anything else is skipped, never matched. A guid the server does not hold is
    /// simply absent from the result; `Err` means a chunk could not be answered at all, which the
    /// caller must NOT read as "absent" (docs/watchlist-plan.md §1.5).
    ///
    /// A guid the server holds in two libraries is reported once (the first row); the caller wants
    /// "this server has it, under this key", not a per-library listing.
    pub fn find_by_guids(&self, kind: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
        let mut found: Vec<GuidMatch> = Vec::new();
        for (paths, chunk) in guid_lookup_paths(kind, guids) {
            let Some(mc) = self.get_json(&paths) else {
                return Err(GuidLookupError::Unanswered);
            };
            for item in mc.metadata {
                if chunk.contains(&item.guid)
                    && !item.rating_key.is_empty()
                    && !found.iter().any(|m| m.guid == item.guid)
                {
                    found.push(GuidMatch {
                        facts: item.listing_facts(),
                        blur: item.ultra_blur_colors.and_then(|c| c.corners()),
                        library_section_id: (item.library_section_id != 0).then_some(item.library_section_id),
                        guid: item.guid,
                        rating_key: item.rating_key,
                    });
                }
            }
        }
        Ok(found)
    }

    /// Sorted/filtered/paged section listing — the Library browse grid's one fetch.
    /// `GET /library/sections/{k}/all?includeMeta=1&sort=…&genre=…` plus the paging window
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
        let path = paged_path(&b.build(), PageReq::window(q.start, q.size));
        self.get_json(&path)
    }

    /// GET /library/sections/{key}/{directory} — a secondary directory: the filter value
    /// lists (`genre`/`year`/`decade`/`collection`/…, rows carry the tag id in `key` + a
    /// ready-made `fastKey` listing URL) and the `firstCharacter` per-letter index.
    /// → `.directory[]`.
    /// `filters` are the wire terms of the flat listing the values describe, in the order its
    /// `/all` request sends them (`type=`, `unwatched`/`unwatchedLeaves`, `genre`, see
    /// [`section_items_query`](Self::section_items_query)): `type=` scopes genre values and letter
    /// counts to the same flat listing as `/all?type=` (without it a TV library's counts describe
    /// shows even in episode view), and the filters scope the counts to what the grid shows, so a
    /// letter's running sum is the offset of that letter's first title in the filtered listing.
    pub fn section_directory(
        &self,
        section_key: i64,
        directory: &str,
        filters: &[(String, String)],
    ) -> Option<MediaContainer> {
        self.get_json(&section_directory_path(section_key, directory, filters))
    }

    /// How many items of a section satisfy `terms`: one count-only read, `GET
    /// /library/sections/{key}/all?<terms>` with `X-Plex-Container-Start: 0` and `X-Plex-Container-Size:
    /// 0`, which answers no rows and the number as `totalSize`. `terms` are the wire terms in the
    /// order `/all` sends them: the listing's own filters ([`section_items_query`](Self::section_items_query),
    /// `type=`, `unwatched`/`unwatchedLeaves`, `genre`) and, for a band edge, the range term after them
    /// (`rating>>` and `9.49`). Every key is percent-encoded (`rating%3E%3E=9.49`; a filter's key comes
    /// out unchanged), so the grid's terms are byte for byte the page request's. `None` when the read
    /// did not answer; [`SectionTotal::Ignored`] when it answered ROWS (the window was ignored, and
    /// will be again). Docs: `docs/pms-api.md`, "Count probes at band edges".
    pub fn section_total(&self, section_key: i64, terms: &[(String, String)]) -> Option<SectionTotal> {
        total_of_probe(&self.get_json(&section_total_path(section_key, terms))?)
    }

    /// The OLDEST value a date field holds among the items of a listing: one read of `GET
    /// /library/sections/{key}/all?<terms>&sort=<field>:asc:nullsLast` with `Start=0, Size=1`, the
    /// first row's `field` as a number ([`Oldest`]). `nullsLast` is what makes the first row the oldest
    /// VALUE: a plain `:asc` sorts the items with no value first, so the first row would say nothing.
    /// `terms` are the wire terms of the listing, as for [`section_total`](Self::section_total), and
    /// `field` is one of `addedAt`, `lastViewedAt` and `originallyAvailableAt`: the dates a row of the
    /// listing carries itself. `None` when the read did not answer; [`Oldest::Nothing`] when it did and
    /// no item of the listing has a value, the answer to a listing of nothing included. The A-Z rail
    /// reads it to know how far back its year bands go. Docs: `docs/pms-api.md`, "Count probes at band
    /// edges".
    pub fn section_oldest(&self, section_key: i64, terms: &[(String, String)], field: &str) -> Option<Oldest> {
        Some(oldest_of_page(&self.get_json(&section_oldest_path(section_key, terms, field))?, field))
    }

    /// A SHOW's language settings (its Advanced dialog in Plex Web: `audioLanguage`,
    /// `subtitleLanguage`, `subtitleMode`) — see [`crate::plex::ShowLangPrefs`]. None when they
    /// cannot be read.
    ///
    /// Try `includePreferences=1` first. This compatibility parameter is NOT in the vendored
    /// OpenAPI spec, so it is backed by the one read
    /// the spec DOES document carrying these settings, `/library/metadata/{id}/tree`, whose
    /// container holds a `Setting[]` — asked only after a successful response without preferences.
    /// Both requests share a 1500 ms budget: optional settings must not consume the ordinary
    /// bulk-read timeout on the play path. HTTP, transport and parse errors fall back immediately.
    pub fn show_language_prefs(&self, show_rk: &str) -> Option<crate::plex::ShowLangPrefs> {
        if show_rk.is_empty() || !show_rk.bytes().all(|b| b.is_ascii_digit()) {
            return None; // a key is server data: only ever a plain ratingKey
        }
        let path = QueryBuilder::new(format!("/library/metadata/{show_rk}"))
            .int("includePreferences", 1)
            .build();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        let read = |path: &str| match self.get_json_with_headers_until(path, &[], deadline) {
            super::client::JsonDeadlineOutcome::Response { parsed, .. } => parsed,
            _ => None,
        };
        let metadata = read(&path)?;
        if let Some(found) = metadata.metadata.into_iter().next()
            .and_then(|m| crate::plex::ShowLangPrefs::from_settings(&m.preferences.setting))
        {
            return Some(found);
        }
        let tree = read(&format!("/library/metadata/{show_rk}/tree"))?;
        crate::plex::ShowLangPrefs::from_settings(&tree.setting)
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

    /// GET /library/metadata/{rating_key}/extras → clip rows (`subtype` trailer / behindTheScenes / …).
    /// Same playable `Media`/`Part` as `?includeExtras=1` nested under the parent (docs/pms-api.md §4).
    /// A refused GET is `None`; an empty list is `Some` with `metadata` empty.
    pub fn extras(&self, rating_key: &str) -> Option<MediaContainer> {
        self.get_json(&format!("/library/metadata/{rating_key}/extras"))
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

    /// GET /library/metadata/{rating_key}/children as one window: `size` rows from `start`, with
    /// `totalSize` the whole listing. A season's episodes page through this; `children` stays for
    /// the season tabs, which need every row.
    pub fn children_paged(&self, rating_key: &str, start: i64, size: i64) -> Option<MediaContainer> {
        let path = paged_path(
            &format!("/library/metadata/{rating_key}/children"),
            PageReq::window(start, size),
        );
        self.get_json(&path)
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

    /// Fetch a SIDECAR subtitle (`Stream.key`, i.e. `/library/streams/{id}`) for the client
    /// renderer. The endpoint takes `encoding` and `format` (docs/plex-openapi.json). ASS/SSA
    /// requests only UTF-8 re-encoding, preserving styles, drawings and overlapping events.
    /// Other text formats request UTF-8 SubRip so formats such as SAMI keep their conversion
    /// path. Conversion is the server's and has been seen refusing a FORMAT before (`.vtt` → 501):
    /// without `format` PMS re-encodes only, and the bare key is the file as it lies on disk.
    /// `player::sidecar` retains styled scripts and parses plain captions from the response.
    pub fn sidecar_subtitle(&self, key: &str, codec: &str) -> Option<Vec<u8>> {
        self.sidecar_subtitle_detailed(key, codec).ok()
    }

    /// [`Self::sidecar_subtitle`] keeping WHY there is no file: one `label: reason` per attempt
    /// (`HTTP 404 (0 bytes)`, `curl rc=28`, `empty body`, ...), joined with `; `. No URL, host or
    /// token appears in it, so a caller may log it.
    pub fn sidecar_subtitle_detailed(&self, key: &str, codec: &str) -> Result<Vec<u8>, String> {
        if !sidecar_key_allowed(key) {
            // a key is server data: only ever the path this method is for
            return Err("key refused (not /library/streams/<id>[.ext])".into());
        }
        let sep = if key.contains('?') { '&' } else { '?' };
        let mut paths = Vec::with_capacity(3);
        if !codec.eq_ignore_ascii_case("ass") && !codec.eq_ignore_ascii_case("ssa") {
            paths.push(("srt", format!("{key}{sep}encoding=utf-8&format=srt")));
        }
        paths.push(("utf8", format!("{key}{sep}encoding=utf-8")));
        paths.push(("bare", key.to_string()));
        let mut why = Vec::with_capacity(paths.len());
        for (label, path) in &paths {
            match self.get_sidecar_bytes(path) {
                Ok(body) if !body.is_empty() => return Ok(body),
                Ok(_) => why.push(format!("{label}: empty body")),
                Err(e) => why.push(format!("{label}: {e}")),
            }
        }
        Err(why.join("; "))
    }

    /// PUT /library/parts/{id} — select the part's audio/subtitle streams SERVER-side (the
    /// transcoder encodes the SELECTED audio and burns the SELECTED subtitle; a query-param
    /// on the stream URL does NOT change them, only this PUT does). `subtitleStreamID` is sent
    /// whenever the selection names one — `Some(0)` keeps subs OFF (suppresses a default-selected
    /// burn) — and omitted for `None` (an audio-only write that must not move the subtitle);
    /// `audioStreamID` only when the user switched, or when an applied remembered audio is written
    /// back. Returns the HTTP status (route logs it).
    #[track_caller]
    pub fn select_streams(&self, sel: &StreamSelection) -> i32 {
        let mut q = QueryBuilder::new(format!("/library/parts/{}", sel.part_id)).int("allParts", 1);
        if let Some(subtitle) = sel.subtitle_stream_id {
            q = q.int("subtitleStreamID", subtitle);
        }
        let q = q.opt_int("audioStreamID", sel.audio_stream_id);
        self.put(&q.build())
    }

    /// The direct-play stream target: the raw part `key` GET, carrying the per-playback
    /// session id + identity so PMS keys the /status/sessions entry by session (not a
    /// token= fallback), keeping the timeline correlation consistent.
    ///
    /// `part_key` may already contain a query. Library parts do not; IVA extras do
    /// (`/services/iva/assets?…`). [`QueryBuilder`] joins onto that query instead of
    /// writing a second `?`.
    pub fn direct_play_url(&self, part_key: &str, session: &str) -> StreamUrl {
        self.part_url(part_key, Some(session))
    }

    /// [`Self::direct_play_url`] with the session id optional: `None` builds the same URL without
    /// the `X-Plex-Session-Identifier` parameter at all (a part GET that names no session). `Some` is byte-identical to the old body.
    pub(super) fn part_url(&self, part_key: &str, session: Option<&str>) -> StreamUrl {
        let q = QueryBuilder::new(part_key);
        let q = match session {
            Some(session) => q.str("X-Plex-Session-Identifier", session),
            None => q,
        };
        let path = self.playback_identity(q).build();
        StreamUrl {
            origin: self.origin.clone(),
            path: self.with_token(&path),
        }
    }
}

/// The request paths `find_by_guids` issues: only `plex://` guids, de-duplicated in order, in
/// chunks of at most [`GUIDS_PER_REQUEST`]. Each guid is percent-encoded on its own and the chunk is
/// joined with a literal `,` — the one separator PMS splits the `guid` param on (measured,
/// docs/watchlist-plan.md); an encoded comma inside a guid cannot occur, since ids are hex. Each
/// entry pairs the path with the guids that chunk asked for, so a reply row is matched to a guid
/// that was actually requested. Empty input yields no request at all.
fn guid_lookup_paths(kind: GuidKind, guids: &[String]) -> Vec<(String, Vec<String>)> {
    let mut wanted: Vec<&String> = Vec::new();
    for g in guids {
        if g.starts_with("plex://") && g.len() > "plex://".len() && !g.contains(',') && !wanted.contains(&g) {
            wanted.push(g);
        }
    }
    let t = match kind {
        GuidKind::Movie => 1,
        GuidKind::Show => 2,
    };
    wanted
        .chunks(GUIDS_PER_REQUEST)
        .map(|chunk| {
            let joined = chunk.iter().map(|g| urlenc_str(g)).collect::<Vec<_>>().join(",");
            (format!("/library/all?type={t}&guid={joined}"), chunk.iter().map(|g| (*g).clone()).collect())
        })
        .collect()
}

/// The path of a section's secondary directory, `filters` appended the way
/// [`Client::section_items_query`] appends its own (one `QueryBuilder::str` per pair, in order), so
/// the grid page and the directory it is read against are built by the same call shape.
fn section_directory_path(section_key: i64, directory: &str, filters: &[(String, String)]) -> String {
    let mut query = QueryBuilder::new(format!("/library/sections/{section_key}/{directory}"));
    for (k, v) in filters {
        query = query.str(k, v);
    }
    query.build()
}

/// The path of [`Client::section_total`]: `/all` with `terms` (every key and value encoded) and the
/// count-only window, which `paged_path` spells (`Start=0`, `Size=0`: `Size` alone is ignored).
fn section_total_path(section_key: i64, terms: &[(String, String)]) -> String {
    let mut query = QueryBuilder::new(format!("/library/sections/{section_key}/all"));
    for (k, v) in terms {
        query = query.range(k, v);
    }
    paged_path(&query.build(), PageReq::window(0, 0))
}

/// The path of [`Client::section_oldest`]: the listing's `terms` as [`section_total_path`] writes them,
/// the sort that puts the oldest value first and the items with none last, and a window of one row.
fn section_oldest_path(section_key: i64, terms: &[(String, String)], field: &str) -> String {
    let mut query = QueryBuilder::new(format!("/library/sections/{section_key}/all"));
    for (k, v) in terms {
        query = query.range(k, v);
    }
    paged_path(&query.str("sort", &format!("{field}:asc:nullsLast")).build(), PageReq::window(0, 1))
}

/// The oldest value a date field holds, as [`Client::section_oldest`] found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Oldest {
    /// No item of the listing has a value in the field (or the listing is empty).
    Nothing,
    /// A moment, in Unix seconds: `addedAt` and `lastViewedAt` are instants.
    Instant(i64),
    /// The calendar year of a date: `originallyAvailableAt` is a date (`YYYY-MM-DD`), with no zone.
    Year(i32),
}

/// The oldest value in the first row of a `:asc:nullsLast` page: `Nothing` for a page with no row, or a
/// first row that has no value (`nullsLast` puts those last, so every row has none). A zero stamp is the
/// lenient parse's "not sent" ([`Metadata::added_at`]), and a text that is no date is no value.
fn oldest_of_page(container: &MediaContainer, field: &str) -> Oldest {
    let Some(row) = container.metadata.first() else { return Oldest::Nothing };
    let instant = |stamp: i64| if stamp > 0 { Oldest::Instant(stamp) } else { Oldest::Nothing };
    match field {
        "addedAt" => instant(row.added_at),
        "lastViewedAt" => instant(row.last_viewed_at),
        "originallyAvailableAt" => {
            let digits = row.originally_available_at.get(..4).filter(|year| year.bytes().all(|b| b.is_ascii_digit()));
            digits.and_then(|year| year.parse().ok()).map_or(Oldest::Nothing, Oldest::Year)
        }
        other => {
            debug_assert!(false, "no oldest value is read from {other}");
            Oldest::Nothing
        }
    }
}

/// What a count-only read ([`Client::section_total`]) found, when the server answered at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionTotal {
    /// The number of items that match: `totalSize`.
    Count(i64),
    /// The reply carried rows and no count: the server ignored the `Size=0` window. It answers the
    /// same to every such read, so a caller does not ask again for another answer.
    Ignored,
}

/// The count a `Size=0` probe answered: `totalSize`, or [`SectionTotal::Ignored`] for a reply with
/// rows (read with no `totalSize` as 0 by the lenient parse, which is no count). A negative
/// `totalSize` is no answer.
fn total_of_probe(container: &MediaContainer) -> Option<SectionTotal> {
    if container.total_size < 0 {
        return None;
    }
    Some(if container.metadata.is_empty() { SectionTotal::Count(container.total_size) } else { SectionTotal::Ignored })
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
#[path = "library_guid_tests.rs"]
mod library_guid_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    /// A directory read without terms is byte-identical to what it always was, and the terms
    /// follow in the order given, as `/all` sends them.
    #[test]
    fn a_directory_path_carries_the_listings_terms_in_order() {
        assert_eq!(section_directory_path(1, "firstCharacter", &[]), "/library/sections/1/firstCharacter");
        assert_eq!(section_directory_path(2, "genre", &terms(&[("type", "2")])), "/library/sections/2/genre?type=2");
        assert_eq!(section_directory_path(1, "firstCharacter", &terms(&[("unwatched", "1"), ("genre", "7")])),
            "/library/sections/1/firstCharacter?unwatched=1&genre=7");
        assert_eq!(
            section_directory_path(2, "firstCharacter", &terms(&[("type", "3"), ("unwatchedLeaves", "1")])),
            "/library/sections/2/firstCharacter?type=3&unwatchedLeaves=1");
    }

    /// A count probe is `/all` with the listing's terms, then the range term, then the count-only
    /// window; the operator in the key is encoded and the listing's own terms are not changed.
    #[test]
    fn a_total_probe_is_the_listings_terms_then_the_encoded_range_then_the_count_window() {
        let window = "X-Plex-Container-Start=0&X-Plex-Container-Size=0";
        assert_eq!(section_total_path(1, &[]), format!("/library/sections/1/all?{window}"));
        let grid = terms(&[("type", "2"), ("unwatchedLeaves", "1"), ("genre", "7")]);
        let mut probe = grid.clone();
        probe.push(("rating>>".into(), "9.49".into()));
        assert_eq!(section_total_path(4, &probe),
            format!("/library/sections/4/all?type=2&unwatchedLeaves=1&genre=7&rating%3E%3E=9.49&{window}"));
        // the filters come out as the page request sends them, byte for byte
        let page = section_directory_path(4, "all", &grid);
        assert!(section_total_path(4, &grid).starts_with(&page), "{page}");
        // a date edge's inclusive operator, a scoped field and a negative value
        assert_eq!(section_total_path(2, &terms(&[("addedAt>", "1793422800")])),
            format!("/library/sections/2/all?addedAt%3E=1793422800&{window}"));
        assert_eq!(section_total_path(2, &terms(&[("episode.addedAt>", "-315619200")])),
            format!("/library/sections/2/all?episode.addedAt%3E=-315619200&{window}"));
    }

    fn container(json: &str) -> MediaContainer {
        serde_json::from_str(json).expect("a container")
    }

    /// The extent read is the listing's own terms, then the sort that puts the oldest VALUE first
    /// (`nullsLast`: a plain ascending sort leads with the items that have none), then a window of ONE.
    #[test]
    fn an_oldest_read_sorts_ascending_nulls_last_and_asks_for_one_row() {
        let grid = terms(&[("type", "2"), ("unwatchedLeaves", "1"), ("genre", "7")]);
        assert_eq!(section_oldest_path(4, &grid, "addedAt"),
            "/library/sections/4/all?type=2&unwatchedLeaves=1&genre=7&sort=addedAt%3Aasc%3AnullsLast\
             &X-Plex-Container-Start=0&X-Plex-Container-Size=1");
        let counted = section_total_path(4, &grid);
        let page = counted.split("&X-Plex").next().unwrap();
        assert!(section_oldest_path(4, &grid, "lastViewedAt").starts_with(page), "the listing's terms, byte for byte");
        assert!(section_oldest_path(1, &[], "originallyAvailableAt").contains("sort=originallyAvailableAt%3Aasc%3AnullsLast"));
    }

    /// An instant stays one, a date is read as its year, and a first row with no value, no row or no
    /// date is "nothing": never a year 0 or an instant at the epoch.
    #[test]
    fn the_oldest_value_is_the_first_rows_field_or_nothing() {
        let page = |row: &str| container(&format!(r#"{{"size":1,"totalSize":9,"Metadata":[{row}]}}"#));
        assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1","addedAt":1700000000}"#), "addedAt"), Oldest::Instant(1_700_000_000));
        assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1","lastViewedAt":"1650000000"}"#), "lastViewedAt"), Oldest::Instant(1_650_000_000));
        assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1","originallyAvailableAt":"1931-04-02"}"#), "originallyAvailableAt"), Oldest::Year(1931));
        assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1","originallyAvailableAt":"0999-12-31"}"#), "originallyAvailableAt"), Oldest::Year(999),
            "the year is read as it stands: the caller judges it");
        // the field asked for is the one read: a row's other dates are not it
        assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1","addedAt":1700000000}"#), "lastViewedAt"), Oldest::Nothing);
        for field in ["addedAt", "lastViewedAt", "originallyAvailableAt"] {
            assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1"}"#), field), Oldest::Nothing, "{field}: no value");
            assert_eq!(oldest_of_page(&container(r#"{"size":0,"totalSize":0}"#), field), Oldest::Nothing, "{field}: no row");
        }
        for text in ["", "n/a", "19", "-001-01-01", "20x6-01-01"] {
            let row = format!(r#"{{"ratingKey":"1","originallyAvailableAt":"{text}"}}"#);
            assert_eq!(oldest_of_page(&page(&row), "originallyAvailableAt"), Oldest::Nothing, "{text:?}");
        }
        assert_eq!(oldest_of_page(&page(r#"{"ratingKey":"1","addedAt":0}"#), "addedAt"), Oldest::Nothing, "a zero stamp");
    }

    /// The count is `totalSize`, however the server spells the number; an answer that carries rows
    /// is a read whose window was ignored, and is no count.
    #[test]
    fn a_total_probe_reads_total_size_and_refuses_an_answer_with_rows() {
        let count = |n| Some(SectionTotal::Count(n));
        assert_eq!(total_of_probe(&container(r#"{"size":0,"totalSize":38,"offset":0}"#)), count(38));
        assert_eq!(total_of_probe(&container(r#"{"size":0,"totalSize":"38"}"#)), count(38));
        assert_eq!(total_of_probe(&container(r#"{"size":0,"totalSize":0}"#)), count(0), "a real zero");
        assert_eq!(total_of_probe(&container(r#"{"size":2,"Metadata":[{"ratingKey":"1"},{"ratingKey":"2"}]}"#)),
            Some(SectionTotal::Ignored), "rows and no totalSize: the window was ignored");
        assert_eq!(total_of_probe(&container(r#"{"size":1,"totalSize":9,"Metadata":[{"ratingKey":"1"}]}"#)),
            Some(SectionTotal::Ignored), "rows are never a count-only answer");
        assert_eq!(total_of_probe(&container(r#"{"totalSize":-1}"#)), None);
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn sidecar_download_preserves_ass_and_keeps_other_format_conversion() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        let script = b"[Script Info]\nScriptType: v4.00+\n[Events]\n\
            Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\pos(300,100)}SIGN\n";
        let converted = b"1\n00:00:01,000 --> 00:00:03,000\nSIGN\n";
        // SSA also exercises a PMS that refuses re-encoding: the original file is the fallback,
        // never a style-destroying SubRip request. SAMI still needs its existing conversion.
        for (codec, failed_requests, styled) in [("ass", 0, true), ("SSA", 1, true), ("sami", 0, false)] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let mut requests = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(3);
                while requests.len() <= failed_requests && Instant::now() < deadline {
                    let Ok((mut socket, _)) = plx_base::testnet::accept(&listener) else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut request = [0; 8192];
                    let n = socket.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..n]).into_owned();
                    let body: &[u8] = if request.contains("format=srt") { converted } else { script };
                    let status = if requests.len() < failed_requests { "501 Not Implemented" } else { "200 OK" };
                    write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    socket.write_all(body).unwrap();
                    requests.push(request);
                }
                requests
            });
            let client = Client::new(
                crate::plex::ServerId::UNSET, "fixture",
                crate::plex::Origin::http("127.0.0.1", port as i32), "", "cid",
            );
            // The source has no extension: metadata's codec must determine download policy.
            let body = client.sidecar_subtitle("/library/streams/42", codec);
            let requests = server.join().unwrap();
            let expected: &[u8] = if styled { script } else { converted };
            assert_eq!(body.as_deref(), Some(expected), "{codec}: {requests:?}");
            assert_eq!(requests.len(), failed_requests + 1);
            assert!(requests[0].contains("encoding=utf-8"));
            if styled {
                assert!(requests.iter().all(|request| !request.contains("format=srt")));
            }
            if failed_requests > 0 {
                assert!(!requests.last().unwrap().contains("encoding="), "retry the original file");
            }
        }
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn sidecar_failure_names_status_per_attempt_and_no_url() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let (mut served, deadline) = (0, Instant::now() + Duration::from_secs(3));
            while served < 3 && Instant::now() < deadline {
                let Ok((mut socket, _)) = plx_base::testnet::accept(&listener) else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = [0; 8192];
                let _ = socket.read(&mut request).unwrap();
                let (status, body) = match served {
                    0 => ("404 Not Found", &b"gone"[..]),
                    1 => ("200 OK", &b""[..]),
                    _ => ("400 Bad Request", &b""[..]),
                };
                write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(body).unwrap();
                served += 1;
            }
        });
        let client = Client::new(
            crate::plex::ServerId::UNSET, "fixture",
            crate::plex::Origin::http("127.0.0.1", port as i32), "tok3n", "cid",
        );
        let why = client.sidecar_subtitle_detailed("/library/streams/16368", "srt").unwrap_err();
        server.join().unwrap();
        assert_eq!(why, "srt: HTTP 404 (0 bytes); utf8: empty body; bare: HTTP 400 (0 bytes)");
        assert!(!why.contains("127.0.0.1") && !why.contains("tok3n"));
        assert_eq!(
            client.sidecar_subtitle_detailed("/library/streams/1?x=1", "srt").unwrap_err(),
            "key refused (not /library/streams/<id>[.ext])"
        );
    }

    /// **The selection PUT's wire shape** (write-back, W5): `subtitleStreamID` rides the PUT only
    /// when the selection names one — `0` is the viewer's Off and is still sent — and an
    /// audio-only write that has no subtitle to keep omits it, so it cannot create an explicit Off.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn select_streams_sends_the_subtitle_only_when_the_selection_names_one() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        let cases: [(i64, Option<i64>, &[(&str, &str)], &[&str]); 4] = [
            (36015, Some(55001), &[("audioStreamID", "36015"), ("subtitleStreamID", "55001")], &[]),
            (36015, Some(0), &[("audioStreamID", "36015"), ("subtitleStreamID", "0")], &[]),
            (36015, None, &[("audioStreamID", "36015")], &["subtitleStreamID"]),
            (0, Some(55001), &[("subtitleStreamID", "55001")], &["audioStreamID"]),
        ];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let expected = cases.len();
        let server = std::thread::spawn(move || {
            let (mut requests, deadline) = (Vec::new(), Instant::now() + Duration::from_secs(5));
            while requests.len() < expected && Instant::now() < deadline {
                let Ok((mut socket, _)) = plx_base::testnet::accept(&listener) else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = [0; 8192];
                let n = socket.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..n]).into_owned();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                requests.push(request.lines().next().unwrap_or("").to_owned());
            }
            requests
        });
        let client = Client::new(
            crate::plex::ServerId::UNSET, "fixture",
            crate::plex::Origin::http("127.0.0.1", port as i32), "", "cid",
        );
        for (audio, subtitle, _, _) in cases {
            let status = client.select_streams(&StreamSelection { part_id: 77, audio_stream_id: audio, subtitle_stream_id: subtitle });
            assert_eq!(status, 200);
        }
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), cases.len(), "{requests:?}");
        for (line, (_, _, present, absent)) in requests.iter().zip(cases) {
            assert!(line.starts_with("PUT /library/parts/77?"), "{line}");
            assert!(line.contains("allParts=1"), "{line}");
            let query = line.split_once('?').unwrap().1.split_whitespace().next().unwrap();
            for (key, value) in present {
                assert!(query.split('&').any(|pair| pair == format!("{key}={value}")), "{key}={value} missing: {line}");
            }
            for key in absent {
                assert!(!query.split('&').any(|pair| pair.starts_with(&format!("{key}="))), "{key} must be omitted: {line}");
            }
        }
    }

    #[test]
    fn show_preferences_do_not_retry_a_transport_failure() {
        let client = Client::new(
            crate::plex::ServerId::UNSET, "fixture",
            crate::plex::Origin::http("127.0.0.1", 9), "", "cid",
        );
        client.disable_data_io();
        assert_eq!(client.show_language_prefs("42"), None);
        assert_eq!(client.denied_data_requests(), 1, "a failed optional read must not retry");
    }

    // A failed optional preference read must not spend another ordinary PMS timeout.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn show_preferences_fail_fast_without_retrying_errors() {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        for (status, body, delay) in [
            ("404 Not Found", "{}", 0),
            ("200 OK", "not json", 0),
            ("200 OK", r#"{"MediaContainer":{}}"#, 2200),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_millis(2500);
                let mut requests = 0;
                while Instant::now() < end {
                    let Ok((mut socket, _)) = plx_base::testnet::accept(&listener) else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    let mut request = [0; 4096];
                    let n = socket.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..n]);
                    assert!(request.contains("Accept: application/json"));
                    requests += 1;
                    if requests == 1 { std::thread::sleep(Duration::from_millis(delay)); }
                    let _ = write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
                requests
            });
            let client = Client::new(
                crate::plex::ServerId::UNSET, "fixture",
                crate::plex::Origin::http("127.0.0.1", port as i32), "", "cid",
            );
            let start = Instant::now();
            assert_eq!(client.show_language_prefs("42"), None);
            let elapsed = start.elapsed();
            let requests = server.join().unwrap();
            assert!(elapsed < Duration::from_millis(1500 + 300), "optional GET delayed play: {elapsed:?}");
            assert_eq!(requests, 1, "failed preference GET must not fetch the show tree");
        }
    }

    #[test]
    fn sidecar_download_refuses_path_traversal() {
        for key in ["/library/streams/../../identity", "/library/streams/%2e%2e/identity",
                    "/library/streams/1?path=/identity", "/library/streams/1#fragment"] {
            assert!(!sidecar_key_allowed(key), "{key}");
        }
        assert!(sidecar_key_allowed("/library/streams/123"));
        assert!(sidecar_key_allowed("/library/streams/123.srt"));
    }

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
