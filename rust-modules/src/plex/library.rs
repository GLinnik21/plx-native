//! Library operations (impl Client): sections, section items, metadata, children/leaves,
//! related — plus the two part-level playback ops (stream selection, direct-play target).
use super::client::{Client, QueryBuilder, StreamUrl};
use super::models::{MediaContainer, Metadata};
use super::params::{SectionQuery, StreamSelection};

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

    /// Fetch a SIDECAR subtitle (`Stream.key`, i.e. `/library/streams/{id}`) for the client
    /// renderer. The endpoint takes `encoding` and `format` (docs/plex-openapi.json). ASS/SSA
    /// requests only UTF-8 re-encoding, preserving styles, drawings and overlapping events.
    /// Other text formats request UTF-8 SubRip so formats such as SAMI keep their conversion
    /// path. Conversion is the server's and has been seen refusing a FORMAT before (`.vtt` → 501):
    /// without `format` PMS re-encodes only, and the bare key is the file as it lies on disk.
    /// `player::sidecar` retains styled scripts and parses plain captions from the response.
    pub fn sidecar_subtitle(&self, key: &str, codec: &str) -> Option<Vec<u8>> {
        if !sidecar_key_allowed(key) {
            return None; // a key is server data: only ever the path this method is for
        }
        let sep = if key.contains('?') { '&' } else { '?' };
        let mut paths = Vec::with_capacity(3);
        if !codec.eq_ignore_ascii_case("ass") && !codec.eq_ignore_ascii_case("ssa") {
            paths.push(format!("{key}{sep}encoding=utf-8&format=srt"));
        }
        paths.push(format!("{key}{sep}encoding=utf-8"));
        paths.push(key.to_string());
        paths.iter().find_map(|path| self.get_sidecar_bytes(path).filter(|b| !b.is_empty()))
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
    ///
    /// `part_key` may already contain a query. Library parts do not; IVA extras do
    /// (`/services/iva/assets?…`). [`QueryBuilder`] joins onto that query instead of
    /// writing a second `?`.
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
    use super::*;

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
                    let Ok((mut socket, _)) = crate::testnet::accept(&listener) else {
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
                    let Ok((mut socket, _)) = crate::testnet::accept(&listener) else {
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
