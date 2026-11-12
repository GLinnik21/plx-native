//! Playback-session protocol ops (impl Client): the `/:/timeline` progress report and the
//! two calls that make the session a first-class, remote-controllable player — GET /identity
//! (the server id the PlayQueue uri needs) and POST /playQueues.
//!
//! Rebuilt FROM the live `route.rs`/`threads.rs` (task #26): the timeline carries the
//! per-playback session id, PlayQueue ids, and the SELECTED audio/subtitle stream ids, so
//! /status/sessions shows the right track and the Direct Play vs Transcode badge
//! (correlated by the active encoder's coupled `X-Plex-Session-Identifier == session=`).
//! PMS keeps a dashboard row for every session id that ever reported `playing` until that id goes
//! quiet for minutes, whatever the encoder stop and `/status/sessions/close` did; a report with
//! `state=stopped` and `continuing=1` ([`TimelineReport::continuing`]) retires the row of an id
//! the playback has moved off (issue #1236).
use super::client::{Client, QueryBuilder};
use super::params::{ServerStop, TimelineAnswer, TimelineReport};
use super::servers::ServerId;

impl Client {
    /// POST /:/timeline (the spec verb; params ride the query) — the server updates viewOffset
    /// (the resume point) + watched state. `true` when it took the report.
    ///
    /// The outcome used to stop here (`post_void`), which made this the one write in the playback
    /// protocol that could fail in complete silence: a 401 on a revoked token, a refused connect
    /// and a 500 were indistinguishable from a committed resume point, and the caller logged the
    /// same success line for all four. The reporting is `crate::route::scrobble_stop`'s; this
    /// only has to stop throwing the answer away.
    pub fn timeline(&self, r: &TimelineReport) -> bool {
        self.post_ok(&self.timeline_query(r))
    }

    pub fn timeline_answer(&self, r: &TimelineReport) -> TimelineAnswer {
        match self.post_accepting_json(&self.timeline_query(r)) {
            Some(body) => TimelineAnswer { taken: true, stop: server_stop_of(&body) },
            None => TimelineAnswer::default(),
        }
    }

    /// The request path and query of one report, without the token. Its own function so the wire
    /// shape is graded without a server.
    fn timeline_query(&self, r: &TimelineReport) -> String {
        let q = QueryBuilder::new("/:/timeline")
            .str("ratingKey", r.rating_key)
            .str("key", &format!("/library/metadata/{}", r.rating_key))
            .str("identifier", "com.plexapp.plugins.library")
            .str("state", r.state.as_str())
            .int("time", r.time_ms)
            .int("duration", r.duration_ms)
            .str("X-Plex-Session-Identifier", r.session);
        let q = self
            .playback_identity(q)
            .opt_str("playQueueID", r.play_queue_id)
            .opt_str("playQueueItemID", r.play_queue_item_id)
            .opt_int("audioStreamID", r.audio_stream_id)
            .opt_int("subtitleStreamID", r.subtitle_stream_id)
            .opt_int("continuing", i64::from(r.continuing));
        q.build()
    }

    /// GET /identity → the server's stable machineIdentifier (None on failure/empty).
    pub fn machine_identity(&self) -> Option<String> {
        let mid = self.get_json("/identity")?.machine_identifier;
        if mid.is_empty() {
            None
        } else {
            Some(mid)
        }
    }

    /// POST /playQueues for one item. Best-effort: None on failure — the timeline still works,
    /// just without the queue ids (and the player without an Up Next).
    ///
    /// **An episode's queue is the SHOW's, with the episode selected in it** (`show_rk` non-empty):
    /// the request names the show as the `uri` and the episode as the `key`
    /// ([`show_queue_query`]). The queue then holds the episodes BEFORE the one playing as well as
    /// after it, which is what Previous needs: the episode-only request below answers the playing
    /// episode and its successors and nothing before, so a play started fresh had no predecessor
    /// row and the Previous disc could only ever be a restart of the current episode. This shape
    /// is spec- and client-derived, not observed on a live server (`docs/pms-api.md`, "A show-keyed PlayQueue"),
    /// so the answer is accepted only when its selected row IS the requested episode
    /// ([`PlayQueueResult::selects`]); a server that ignored `key`, or refused, falls back to the
    /// episode request, which is exactly what every play sent before.
    ///
    /// The episode request's `continuous=1` is what makes the response carry the show's remaining
    /// episodes after the one being played, each as a FULL `Metadata` row (thumb, S/E, duration,
    /// viewOffset, and `Media[0].Part[0].key` + codecs — everything `route::request_play` needs).
    /// The Up Next control screen is therefore free: it reads [`PlayQueueResult::next`] from the
    /// queue this playback already had to create, instead of asking the server what plays next.
    ///
    /// Trailer extras omit `continuous` (and pass no `show_rk`): a continuous extras queue can be
    /// sibling clips, and EOS must not Up-Next into a featurette. `opt_int` drops the param when
    /// `continuous` is false.
    ///
    /// The WHOLE window is kept, as [`QueueRow`]s — the queue this round trip already paid for is
    /// the queue a list can draw and jump around in, and throwing it away meant re-asking the
    /// server for something it had already sent.
    pub fn create_play_queue(
        &self,
        machine_id: &str,
        rating_key: &str,
        show_rk: &str,
        session: &str,
        continuous: bool,
    ) -> Option<PlayQueueResult> {
        if continuous && !show_rk.is_empty() {
            let q = self.playback_identity(show_queue_query(machine_id, show_rk, rating_key, session));
            let asked = self
                .post_json(&q.build())
                .map(|mc| PlayQueueResult::of(mc, self.id(), rating_key))
                .filter(|queue| queue.selects(rating_key, self.id()));
            if let Some(queue) = asked {
                return Some(self.complete_before(queue));
            }
        }
        let q = QueryBuilder::new("/playQueues")
            .str("type", "video")
            .str("uri", &item_uri(machine_id, rating_key))
            .opt_int("continuous", i64::from(continuous))
            .int("shuffle", 0)
            .int("repeat", 0)
            .str("X-Plex-Session-Identifier", session);
        let q = self.playback_identity(q);
        Some(PlayQueueResult::of(
            self.post_json(&q.build())?,
            self.id(),
            rating_key,
        ))
    }

    /// A queue the server says has rows before the selected one, but whose answer OPENS on the
    /// selected row (a window that starts where playback does), is read again around its selected
    /// item with the existing window read ([`Client::play_queue_window`]), so Previous is not lost
    /// to the page the create answered with. A read that fails, or that does not hold the selected
    /// row, leaves the answer as it was.
    fn complete_before(&self, queue: PlayQueueResult) -> PlayQueueResult {
        if !queue.opens_on_selected_with_rows_before() {
            return queue;
        }
        match self.play_queue_window(&queue.id.to_string(), queue.selected_item_id) {
            Some(rows) => queue.with_window(rows),
            None => queue,
        }
    }
}

/// The `uri` naming one library item on the server `machine_id`, the form `POST /playQueues` takes
/// (the simple `/library/metadata/{rk}` form answers HTTP 400, `docs/pms-api.md`, "PlayQueue for an
/// extra").
fn item_uri(machine_id: &str, rating_key: &str) -> String {
    format!("server://{machine_id}/com.plexapp.plugins.library/library/metadata/{rating_key}")
}

/// The `POST /playQueues` that builds an episode's queue from its SHOW: the show is the `uri`, the
/// episode the `key` ("the key of the first item to play", `docs/plex-openapi.json`), no
/// `continuous` (the queue already spans every episode, before and after the keyed one). Its own
/// function so the wire shape is graded without a server.
fn show_queue_query(machine_id: &str, show_rk: &str, rating_key: &str, session: &str) -> QueryBuilder {
    QueryBuilder::new("/playQueues")
        .str("type", "video")
        .str("uri", &item_uri(machine_id, show_rk))
        .str("key", &format!("/library/metadata/{rating_key}"))
        .int("shuffle", 0)
        .int("repeat", 0)
        .str("X-Plex-Session-Identifier", session)
}

/// One retained row of the play queue: everything a queue list draws, plus everything
/// `crate::route::request_play` needs to START that row — and nothing else.
///
/// The projection is the whole point. A `Metadata` row carries the entire Media/Part/Stream/Role
/// tree, this runs on the resolve worker of a 32-bit TV, and a `continuous=1` queue can be a whole
/// show — so the rows are retained ONLY in this shape. Field names mirror `route::UpNext` (which
/// is built from one of these) rather than the wire: `dur_ms` is `duration`, `resume_ms` is
/// `viewOffset`, `part`/`vcodec`/`acodec` come from `Media[0]`/`Media[0].Part[0]` exactly as the
/// single-successor projection always did.
///
/// NOT episode-gated: a queue row may be a movie, and the list must be able to show it. The
/// "episodes only" rule belongs to the one-item Up Next control, not to the queue.
#[derive(Clone, Default)]
pub struct QueueRow {
    /// `playQueueItemID` — identity WITHIN the queue. A ratingKey can repeat in a queue (the same
    /// item queued twice), a playQueueItemID cannot, so every lookup keys off this. 0 = a server
    /// that omitted the field.
    pub item_id: i64,
    /// The server that ISSUED this queue, and so the server `rk` is a key on. A PlayQueue is
    /// per-server by construction (it is created with a `server://{machineIdentifier}/…` uri), so
    /// every row of one shares this — but the ratingKey FALLBACK below compares against a key
    /// supplied by the caller, and that one can come from anywhere.
    pub sid: super::ServerId,
    pub rk: String,
    /// `type` — movie | episode | clip …
    pub kind: String,
    /// the item's own title (the episode title for an episode)
    pub title: String,
    /// `grandparentTitle` — the show, empty for a movie
    pub show_title: String,
    /// `grandparentRatingKey` — the show's key on this server, empty for a movie
    pub show_rk: String,
    /// `viewCount > 0` AS THE QUEUE WAS BUILT, i.e. at play start: what the stop record compares
    /// the server's later answer against to tell a first watch (fan out) from a rewatch.
    pub watched: bool,
    /// `parentIndex` — season number (0 for a movie)
    pub season: i64,
    pub index: i64,
    pub thumb: String,
    /// `grandparentThumb` — the SHOW's portrait poster (empty for a movie). Retained beside
    /// `thumb` because the two are different SHAPES: an episode's `thumb` is a landscape still.
    /// Nothing draws it today — the Up Next tile shows the STILL, since the successor is another
    /// episode of the show the viewer is already watching — so this is the queue row's honest
    /// parse of the container and not a promise about a screen.
    pub poster: String,
    pub dur_ms: i64,
    /// `viewOffset` — the resume point, 0 = unwatched/from the start
    pub resume_ms: i64,
    /// `Media[0].Part[0].key` — the direct-play part
    pub part: String,
    pub vcodec: String,
    pub acodec: String,
}

impl QueueRow {
    /// Consume a queue `Metadata` row into its lean projection. BY VALUE: the strings are moved,
    /// not cloned, and the rest of the row's tree is dropped as this returns.
    pub(super) fn of(m: super::models::Metadata, sid: super::ServerId) -> QueueRow {
        // `Media[0]` / `Media[0].Part[0]`, the default version, `Metadata::media_version(None)` — by value,
        // so the other versions and their Stream lists die here.
        let (vcodec, acodec, part) = match m.media.into_iter().next() {
            Some(md) => (
                md.video_codec,
                md.audio_codec,
                md.part
                    .into_iter()
                    .next()
                    .map(|p| p.key)
                    .unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        QueueRow {
            item_id: m.play_queue_item_id,
            sid,
            rk: m.rating_key,
            kind: m.kind,
            title: m.title,
            show_title: m.grandparent_title,
            show_rk: m.grandparent_rating_key,
            watched: m.view_count > 0,
            season: m.parent_index.unwrap_or(0),
            index: m.index,
            thumb: m.thumb,
            poster: m.grandparent_thumb,
            dur_ms: m.duration,
            resume_ms: m.view_offset,
            part,
            vcodec,
            acodec,
        }
    }
}

/// Where a row sits in the queue — THE identity rule, in one place, because everything that
/// points at a queue row needs the same answer (the successor lookup below, and whatever draws
/// "you are here" in a queue list).
///
/// Identity is `playQueueItemID`, not `ratingKey`: a queue may legitimately hold the same item
/// twice, and matching on the rating key would then pick the wrong row. The rating key is only the
/// fallback for a server that omitted the per-row id — for a queue built FROM this item the two
/// agree, and losing the row entirely is the worse failure.
pub fn queue_index_of(
    items: &[QueueRow],
    item_id: i64,
    sid: ServerId,
    rating_key: &str,
) -> Option<usize> {
    let by_id = (item_id != 0)
        .then(|| items.iter().position(|r| r.item_id == item_id))
        .flatten();
    by_id.or_else(|| {
        items
            .iter()
            .position(|r| super::same_item((r.sid, &r.rk), (sid, rating_key)))
    })
}

/// The queue row that follows the selected one — None at the end of the queue, and None when the
/// selection matches no row at all (handing back `items[1]` there would start something the user
/// was never watching).
fn next_after<'a>(
    items: &'a [QueueRow],
    selected_item_id: i64,
    sid: ServerId,
    rating_key: &str,
) -> Option<&'a QueueRow> {
    items.get(queue_index_of(items, selected_item_id, sid, rating_key)? + 1)
}

fn server_stop_of(body: &[u8]) -> Option<ServerStop> {
    let reply: serde_json::Value = serde_json::from_slice(body).ok()?;
    let container = reply.get("MediaContainer")?;
    let code = container.get("terminationCode")?;
    let code = code.as_i64().or_else(|| code.as_str()?.trim().parse().ok())?;
    let reason = container.get("terminationText").and_then(serde_json::Value::as_str).unwrap_or_default();
    Some(ServerStop { code, reason: reason.to_owned() })
}

#[cfg(test)]
mod server_stop_tests {
    use super::{server_stop_of, ServerStop};

    #[test]
    fn a_termination_on_the_answer_is_read_with_its_reason() {
        let stop = |code, reason: &str| Some(ServerStop { code, reason: reason.to_owned() });
        assert_eq!(
            server_stop_of(br#"{"MediaContainer":{"size":0,"terminationCode":2006,"terminationText":"stopped"}}"#),
            stop(2006, "stopped")
        );
        assert_eq!(
            server_stop_of(br#"{"MediaContainer":{"terminationCode":"2006","terminationText":"stopped"}}"#),
            stop(2006, "stopped")
        );
        assert_eq!(server_stop_of(br#"{"MediaContainer":{"terminationCode":2006}}"#), stop(2006, ""));
    }

    #[test]
    fn an_ordinary_or_unreadable_answer_stops_nothing() {
        let bodies: [&[u8]; 6] = [
            b"",
            b"<MediaContainer size=\"0\"/>",
            br#"{"MediaContainer":{"size":0}}"#,
            br#"{"MediaContainer":{"terminationCode":null}}"#,
            br#"{"MediaContainer":{"terminationCode":"soon"}}"#,
            br#"{}"#,
        ];
        for body in bodies {
            assert_eq!(server_stop_of(body), None, "{}", String::from_utf8_lossy(body));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::models::Envelope;

    /// The server the fixture queues belong to. A PlayQueue is per-server by construction, so one
    /// slot for a whole queue is exactly the real shape.
    const Q: ServerId = ServerId::from_raw(0);

    /// one queue row: (playQueueItemID, ratingKey) on server [`Q`]
    fn row(item_id: i64, rk: &str) -> QueueRow {
        QueueRow {
            item_id,
            sid: Q,
            rk: rk.to_string(),
            ..Default::default()
        }
    }
    fn rks(items: &[QueueRow]) -> Vec<&str> {
        items.iter().map(|r| r.rk.as_str()).collect()
    }

    #[test]
    fn trailer_play_queues_omit_continuous() {
        use super::super::client::QueryBuilder;
        let with = QueryBuilder::new("/playQueues")
            .opt_int("continuous", 1)
            .build();
        let without = QueryBuilder::new("/playQueues")
            .opt_int("continuous", 0)
            .build();
        assert!(with.contains("continuous=1"));
        assert!(
            !without.contains("continuous"),
            "trailer sessions must not send continuous=1"
        );
    }

    /// **The wire shape of an episode's show-based queue**: the SHOW is the `uri`, the episode the
    /// `key` (as a library path), and there is no `continuous` (the queue spans the show before and
    /// after the keyed episode). The episode-only request keeps `continuous=1` and the episode as
    /// its `uri`, so the two cannot be confused.
    #[test]
    fn an_episodes_queue_is_asked_of_its_show_with_the_episode_as_the_key() {
        let show = show_queue_query("machine", "50", "102", "sess").build();
        assert!(show.starts_with("/playQueues?"), "{show}");
        assert!(show.contains("uri=server%3A%2F%2Fmachine%2Fcom.plexapp.plugins.library%2Flibrary%2Fmetadata%2F50"), "{show}");
        assert!(show.contains("key=%2Flibrary%2Fmetadata%2F102"), "{show}");
        assert!(!show.contains("continuous"), "{show}");
        let episode = QueryBuilder::new("/playQueues")
            .str("uri", &item_uri("machine", "102"))
            .opt_int("continuous", 1)
            .build();
        assert!(episode.contains("metadata%2F102") && !episode.contains("key="), "{episode}");
        assert!(episode.contains("continuous=1"), "{episode}");
    }

    /// A queue body whose selected row (`playQueueSelectedItemID`) is `selected` of rows
    /// `101..=104` (item ids 7001..=7004), `offset` rows before it in the whole queue, the window
    /// starting at row `from`.
    fn season(selected: i64, offset: i64, from: usize) -> PlayQueueResult {
        let rows: Vec<String> = (from..4)
            .map(|n| format!(r#"{{"playQueueItemID":{},"ratingKey":"{}","type":"episode","index":{}}}"#, 7001 + n, 101 + n, n + 1))
            .collect();
        result(
            &format!(
                r#"{{"MediaContainer":{{"size":{},"playQueueID":88,"playQueueSelectedItemID":{selected},
                "playQueueSelectedItemOffset":{offset},"playQueueTotalCount":4,"Metadata":[{}]}}}}"#,
                rows.len(),
                rows.join(",")
            ),
            "102",
        )
    }

    /// **A show-based queue is believed only when it selects the episode asked for.** A server that
    /// ignored `key` answers the first row, a stale one some other episode: neither may stand for
    /// the playing episode (its queue item id is what the timeline reports, and its successor is
    /// Up Next).
    #[test]
    fn a_show_queue_is_accepted_only_when_it_selects_the_requested_episode() {
        assert!(season(7002, 1, 0).selects("102", Q), "the keyed episode is the selected row");
        assert!(!season(7001, 0, 0).selects("102", Q), "the key was ignored: the first row is selected");
        assert!(!season(7003, 2, 0).selects("102", Q), "another row is selected");
        assert!(!season(9999, 0, 0).selects("102", Q), "a selected id no row has");
        assert!(!season(7002, 1, 0).selects("102", ServerId::from_raw(3)), "a queue of another server is not this episode's");
    }

    /// The window read is spent only when the create answered with a window that opens ON the
    /// selected row while the server says rows precede it: the predecessors it lost.
    #[test]
    fn a_window_that_opens_on_the_selected_row_with_rows_before_it_is_read_again() {
        assert!(season(7002, 1, 1).opens_on_selected_with_rows_before(), "opens on 102, one row before it");
        assert!(!season(7002, 1, 0).opens_on_selected_with_rows_before(), "the predecessor is already in the window");
        assert!(!season(7001, 0, 0).opens_on_selected_with_rows_before(), "nothing before the first row");
        assert!(!season(7002, 0, 1).opens_on_selected_with_rows_before(), "the server reports no rows before it");
    }

    /// The re-read window replaces the rows (and the successor), keeps the selection and the queue
    /// id, and is refused when it does not hold the selected row.
    #[test]
    fn a_re_read_window_replaces_the_rows_and_never_the_selection() {
        let opened = season(7002, 1, 1);
        assert_eq!(rks(&opened.items), ["102", "103", "104"]);
        let window: Vec<QueueRow> = (0..4)
            .map(|n| QueueRow { item_id: 7001 + n, sid: Q, rk: (101 + n).to_string(), ..Default::default() })
            .collect();
        let full = season(7002, 1, 1).with_window(window.clone());
        assert_eq!(rks(&full.items), ["101", "102", "103", "104"]);
        assert_eq!((full.id, full.selected_item_id), (88, 7002));
        assert_eq!(full.next.as_ref().map(|n| n.rk.as_str()), Some("103"));
        let without = season(7002, 1, 1).with_window(window[..1].to_vec());
        assert_eq!(rks(&without.items), ["102", "103", "104"], "a window that lost the selected row is ignored");
    }

    /// A response body through the SHIPPED mapping — the same call `create_play_queue` makes once
    /// its POST returns, so these tests cannot pass on a projection the app does not use.
    fn result(body: &str, rating_key: &str) -> PlayQueueResult {
        let mc = serde_json::from_str::<Envelope>(body)
            .expect("a PMS body parses")
            .media_container;
        PlayQueueResult::of(mc, Q, rating_key)
    }

    /// The whole up-next extraction. Every case here is one the live server actually produces:
    /// a mid-season episode (a successor exists), a season finale with nothing after it
    /// (`playQueueTotalCount: 1` — verified on a show's last episode), and a movie, whose
    /// `continuous=1` queue is just itself.
    #[test]
    fn the_successor_is_the_row_after_the_selected_one() {
        let q = vec![row(13083, "1804"), row(13084, "1805"), row(13085, "1806")];
        let next = next_after(&q, 13083, Q, "1804").expect("a mid-queue item has a successor");
        assert_eq!(next.rk, "1805");

        // a queue of one — the season finale / movie case
        let solo = vec![row(13092, "774")];
        assert!(next_after(&solo, 13092, Q, "774").is_none());
        // ...and the LAST row of a longer queue is the same thing
        let q = vec![row(1, "a"), row(2, "b")];
        assert!(next_after(&q, 2, Q, "b").is_none());
    }

    #[test]
    fn the_selected_row_is_found_by_queue_item_id_not_rating_key() {
        // A queue holding the same episode twice: matching on the ratingKey would pick the FIRST
        // occurrence and hand back its successor, which is the wrong episode.
        let q = vec![row(1, "dup"), row(2, "mid"), row(3, "dup"), row(4, "after")];
        let next = next_after(&q, 3, Q, "dup").expect("the second occurrence has a successor");
        assert_eq!(next.rk, "after");
    }

    #[test]
    fn a_missing_queue_item_id_falls_back_to_the_rating_key() {
        // Rows with no playQueueItemID (0) — the successor must still be found, or the feature
        // silently disappears on a server that trims the field.
        let q = vec![row(0, "1804"), row(0, "1805")];
        let next = next_after(&q, 0, Q, "1804").expect("the rating key is the fallback identity");
        assert_eq!(next.rk, "1805");
    }

    /// The FALLBACK is the one identity here that a second server can spoof. `playQueueItemID` is
    /// per-queue and safe; the rating key is not, so it is compared as `(server, key)` — otherwise
    /// a queue built on the share and a playing rk read off our own server (or the reverse) would
    /// "find" a row and auto-advance to something the viewer never queued.
    #[test]
    fn the_rating_key_fallback_is_scoped_to_the_queues_own_server() {
        let other = ServerId::from_raw(1);
        let q = vec![row(0, "1804"), row(0, "1805")];
        assert!(
            next_after(&q, 0, other, "1804").is_none(),
            "another server's 1804 is not this row"
        );
        assert_eq!(
            queue_index_of(&q, 0, Q, "1804"),
            Some(0),
            "…and the queue's OWN 1804 still is"
        );

        // the primary key is unaffected: a playQueueItemID is minted by the queue itself, so it
        // needs no scoping and must keep working even when the caller names another server
        let byid = vec![row(13083, "1804"), row(13084, "1805")];
        assert_eq!(queue_index_of(&byid, 13084, other, "zzz"), Some(1));
    }

    #[test]
    fn an_unrecognised_selection_yields_nothing_rather_than_the_first_row() {
        // Neither identity matches: returning `items[0]`'s successor here would start an episode
        // the user was never watching.
        let q = vec![row(1, "a"), row(2, "b")];
        assert!(next_after(&q, 99, Q, "zzz").is_none());
        assert!(
            next_after(&[], 1, Q, "a").is_none(),
            "an empty queue is not a panic"
        );
    }

    /// A real `continuous=1` response — three episodes, the first selected — through the shipped
    /// mapping. Shapes that are here on purpose: PMS string-encodes some numbers (`duration`,
    /// `viewOffset`) and not others, an episode can carry MULTIPLE `Media[]` versions (4K + 1080p
    /// — the projection takes `[0]`, the same pick the single-successor code always made), and the
    /// rows carry the Stream/Role tree that the lean row has nowhere to put.
    #[test]
    fn a_real_playqueue_body_keeps_every_row_as_a_lean_projection() {
        let q = result(
            r#"{"MediaContainer":{"size":3,"playQueueID":40213,"playQueueSelectedItemID":13083,
              "playQueueSelectedItemOffset":0,"playQueueTotalCount":3,"Metadata":[
              {"playQueueItemID":13083,"ratingKey":"1804","type":"episode","title":"Pilot",
               "grandparentTitle":"Example Show","parentIndex":1,"index":1,
               "thumb":"/library/metadata/1804/thumb/1781586780","duration":"3273248",
               "viewOffset":"142000","Role":[{"tag":"Jennifer Aniston"}],
               "Media":[{"videoCodec":"hevc","audioCodec":"eac3",
                 "Part":[{"id":3130,"key":"/library/parts/3130/1781467224/file.mkv",
                   "Stream":[{"id":1,"streamType":1},{"id":2,"streamType":2}]}]},
                {"videoCodec":"h264","audioCodec":"aac",
                 "Part":[{"id":3134,"key":"/library/parts/3134/1781468203/file.mkv"}]}]},
              {"playQueueItemID":13084,"ratingKey":"1805","type":"episode","title":"A Seat at the Table",
               "grandparentTitle":"Example Show","parentIndex":1,"index":2,"duration":3120000,
               "Media":[{"videoCodec":"h264","audioCodec":"ac3",
                 "Part":[{"key":"/library/parts/3140/1781467999/file.mkv"}]}]},
              {"playQueueItemID":13085,"ratingKey":"1806","type":"episode","title":"Chaos Is the New Cocaine",
               "grandparentTitle":"Example Show","parentIndex":1,"index":3,
               "Media":[{"videoCodec":"h264","audioCodec":"ac3","Part":[{"key":"/p/3.mkv"}]}]}]}}"#,
            "1804",
        );
        assert_eq!(
            rks(&q.items),
            ["1804", "1805", "1806"],
            "the WHOLE window is retained, not just the successor"
        );
        assert_eq!(
            (q.id, q.selected_item_id, q.remaining),
            (40213, 13083, 2),
            "the ids and the count still land"
        );

        let first = &q.items[0];
        assert_eq!(first.item_id, 13083);
        assert_eq!(first.kind, "episode");
        assert_eq!(first.title, "Pilot");
        assert_eq!(first.show_title, "Example Show");
        assert_eq!((first.season, first.index), (1, 1));
        assert_eq!(first.thumb, "/library/metadata/1804/thumb/1781586780");
        assert_eq!(
            (first.dur_ms, first.resume_ms),
            (3273248, 142000),
            "string-encoded numbers still land"
        );
        assert_eq!(
            first.part, "/library/parts/3130/1781467224/file.mkv",
            "Media[0].Part[0].key"
        );
        assert_eq!(
            (first.vcodec.as_str(), first.acodec.as_str()),
            ("hevc", "eac3"),
            "Media[0], not the 1080p version"
        );

        // the successor is exactly what it was when it was the only thing kept — and it is the
        // retained row after the selected one, not a separately-derived answer
        let next = q.next.as_ref().expect("the selected row has a successor");
        assert_eq!((next.rk.as_str(), next.index), ("1805", 2));
        assert_eq!(
            next.part, "/library/parts/3140/1781467999/file.mkv",
            "the successor is start-able"
        );
        assert_eq!(next.dur_ms, 3120000, "…and a plain JSON number lands too");
        let at = queue_index_of(&q.items, q.selected_item_id, Q, "1804")
            .expect("the playing row is locatable");
        assert_eq!(
            (at, q.items[at + 1].rk.as_str()),
            (0, next.rk.as_str()),
            "next IS items[selected+1]"
        );
    }

    /// Two rows the LIST must survive that the one-item Up Next control would have refused: a
    /// movie (a `continuous=1` movie queue is just itself — the list still has to be able to draw
    /// it) and a row with no `Media` at all, which must project to empty strings, not a panic.
    #[test]
    fn a_movie_row_and_a_media_less_row_both_project() {
        let q = result(
            r#"{"MediaContainer":{"playQueueSelectedItemID":900,"Metadata":[
              {"playQueueItemID":900,"ratingKey":"774","type":"movie","title":"Sinners",
               "Media":[{"videoCodec":"hevc","audioCodec":"truehd","Part":[{"key":"/p/774.mkv"}]}]},
              {"playQueueItemID":901,"ratingKey":"775","type":"movie","title":"No Media Here"}]}}"#,
            "774",
        );
        let items = &q.items;
        assert_eq!(
            items[0].kind, "movie",
            "the projection is NOT episode-gated"
        );
        assert_eq!(items[0].show_title, "", "a movie has no grandparentTitle");
        assert_eq!(items[0].part, "/p/774.mkv");
        let bare = &items[1];
        assert_eq!(
            (
                bare.part.as_str(),
                bare.vcodec.as_str(),
                bare.acodec.as_str()
            ),
            ("", "", "")
        );
        assert_eq!(bare.rk, "775", "the rest of the row still projects");
    }

    /// Issue #1236: `continuing=1` is what takes a retired session's row off the dashboard, and
    /// it is only true of a report that says so. Every ordinary report (playing, paused, the
    /// final `stopped`) must not carry the key at all, so a server sees the same request it
    /// always did.
    #[test]
    fn continuing_rides_the_timeline_query_only_when_true() {
        use super::super::params::TimelineState;
        let client = Client::new(
            ServerId::from_raw(3),
            "machine",
            crate::plex::Origin::http("10.0.0.1", 32400),
            "tok",
            "cid-1236",
        );
        let query = |state, continuing| {
            client.timeline_query(&TimelineReport {
                rating_key: "42",
                state,
                time_ms: 20_000,
                duration_ms: 60_000,
                session: "logical-abr-1",
                play_queue_id: "pq",
                play_queue_item_id: "pqi",
                audio_stream_id: 0,
                subtitle_stream_id: 0,
                continuing,
            })
        };

        let retired = query(TimelineState::Stopped, true);
        assert!(retired.starts_with("/:/timeline?ratingKey=42&"), "{retired}");
        assert!(retired.contains("&state=stopped&"), "{retired}");
        assert!(retired.contains("&continuing=1"), "{retired}");
        assert!(retired.contains("&X-Plex-Session-Identifier=logical-abr-1&"), "{retired}");

        for state in [TimelineState::Playing, TimelineState::Paused, TimelineState::Stopped] {
            let ordinary = query(state, false);
            assert!(!ordinary.contains("continuing"), "{ordinary}");
        }
    }
}

/// What a POST /playQueues gives the app: the two ids the `/:/timeline` report carries, plus the
/// queue's own view of what comes next. Kept as a struct rather than a tuple because `next` made
/// the third and fourth members meaningless positionally.
pub struct PlayQueueResult {
    pub id: i64,
    pub selected_item_id: i64,
    /// items queued AFTER the one now playing (0 = this is the last)
    pub remaining: i64,
    /// The returned window of the queue, projected — the row now playing INCLUDED, in queue order,
    /// so a list can show where playback sits. Its length may DIFFER from `remaining + 1` in both
    /// directions: the window can be a slice of a long queue, and it also carries the rows BEFORE
    /// the selected one when `playQueueSelectedItemOffset > 0`. `playQueueTotalCount` is the whole
    /// queue; this is what the server actually sent.
    pub items: Vec<QueueRow>,
    /// the item that plays next, or None at the end of the queue (and for a movie, whose
    /// continuous queue is just itself — verified live: total count 1). A copy of the `items` row
    /// after the selected one; one lean row is worth not making every caller redo the lookup.
    pub next: Option<QueueRow>,
    /// `playQueueSelectedItemOffset`: how many rows the WHOLE queue holds before the selected one
    /// (0 when the server omitted it). A window that opens on the selected row while this is above
    /// zero has lost the predecessors the server says exist ([`Self::opens_on_selected_with_rows_before`]).
    before: i64,
}

impl PlayQueueResult {
    /// Whether the selected row of this answer IS `rating_key` on `sid`: the check a queue built
    /// from the show with the episode as its `key` must pass before the app believes it. A server
    /// that ignored the `key` answers a queue selected on some other row, whose item ids the
    /// timeline would then report for the wrong episode and whose Up Next would be wrong.
    pub(super) fn selects(&self, rating_key: &str, sid: ServerId) -> bool {
        // By the queue item id alone: the rating-key fallback of `queue_index_of` would believe a
        // `playQueueSelectedItemID` that names no row, and the timeline reports that id.
        self.selected_item_id != 0
            && self.items.iter().any(|row| {
                row.item_id == self.selected_item_id
                    && super::same_item((row.sid, &row.rk), (sid, rating_key))
            })
    }

    /// The server says rows precede the selected one, and the returned window starts ON it.
    fn opens_on_selected_with_rows_before(&self) -> bool {
        self.before > 0
            && self.selected_item_id != 0
            && self.items.first().is_some_and(|row| row.item_id == self.selected_item_id)
    }

    /// This result over `rows`, a window read of the same queue around the selected row. Only a
    /// window that holds the selected row replaces the answer: an expired or mismatched one cannot
    /// discard the selection.
    fn with_window(self, rows: Vec<QueueRow>) -> PlayQueueResult {
        let Some(selected) = self.items.iter().find(|row| row.item_id == self.selected_item_id) else {
            return self;
        };
        let (sid, rk) = (selected.sid, selected.rk.clone());
        if !rows.iter().any(|row| row.item_id == self.selected_item_id && row.rk == rk) {
            return self;
        }
        let next = next_after(&rows, self.selected_item_id, sid, &rk).cloned();
        PlayQueueResult { items: rows, next, ..self }
    }

    /// The WHOLE response→result mapping, kept out of `create_play_queue` so the host tests grade
    /// the code that ships instead of a copy of it (only the request build is left up there).
    fn of(
        mc: super::models::MediaContainer,
        sid: super::ServerId,
        rating_key: &str,
    ) -> PlayQueueResult {
        // Remaining AFTER the item being played, from the whole-queue counters (the returned
        // `Metadata[]` can be a window). Clamped: a server that omits either counter must read as
        // "nothing after this", not as a negative count the UI would then format.
        let remaining = (mc.play_queue_total_count - mc.play_queue_selected_item_offset - 1).max(0);
        let selected = mc.play_queue_selected_item_id;
        // Project BY VALUE: every `Metadata` row is consumed into a `QueueRow` and its
        // Media/Part/Stream/Role tree dropped right here, on the worker. What the main thread then
        // holds for the length of the playback is a dozen scalars and strings per row.
        let items: Vec<QueueRow> = mc
            .metadata
            .into_iter()
            .map(|m| QueueRow::of(m, sid))
            .collect();
        let next = next_after(&items, selected, sid, rating_key).cloned();
        PlayQueueResult {
            id: mc.play_queue_id,
            selected_item_id: selected,
            remaining,
            items,
            next,
            before: mc.play_queue_selected_item_offset.max(0),
        }
    }
}
