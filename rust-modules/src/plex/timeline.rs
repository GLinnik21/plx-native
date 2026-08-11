//! Playback-session protocol ops (impl Client): the `/:/timeline` progress report and the
//! two calls that make the session a first-class, remote-controllable player — GET /identity
//! (the server id the PlayQueue uri needs) and POST /playQueues.
//!
//! Rebuilt FROM the live `route.rs`/`threads.rs` (task #26): the timeline carries the
//! per-playback session id, PlayQueue ids, and the SELECTED audio/subtitle stream ids, so
//! /status/sessions shows the right track and the Direct Play vs Transcode badge
//! (correlated by `X-Plex-Session-Identifier == transcode session=`).
use super::client::{Client, QueryBuilder};
use super::params::TimelineReport;

impl Client {
    /// POST /:/timeline (the spec verb; params ride the query). Fire-and-forget — the
    /// server updates viewOffset (the resume point) + watched state.
    pub fn timeline(&self, r: &TimelineReport) {
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
            .opt_int("subtitleStreamID", r.subtitle_stream_id);
        self.post_void(&q.build());
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
    /// `continuous=1` is what makes the response carry the show's remaining episodes after the
    /// one being played, each as a FULL `Metadata` row (thumb, S/E, duration, viewOffset, and
    /// `Media[0].Part[0].key` + codecs — everything `route::request_play` needs). The Up Next control
    /// screen is therefore free: it reads [`PlayQueueResult::next`] from the queue this playback
    /// already had to create, instead of asking the server what plays next.
    ///
    /// The WHOLE window is kept, as [`QueueRow`]s — the queue this round trip already paid for is
    /// the queue a list can draw and jump around in, and throwing it away meant re-asking the
    /// server for something it had already sent.
    pub fn create_play_queue(&self, machine_id: &str, rating_key: &str, session: &str) -> Option<PlayQueueResult> {
        let uri = format!("server://{machine_id}/com.plexapp.plugins.library/library/metadata/{rating_key}");
        let q = QueryBuilder::new("/playQueues")
            .str("type", "video")
            .str("uri", &uri)
            .int("continuous", 1)
            .int("shuffle", 0)
            .int("repeat", 0)
            .str("X-Plex-Session-Identifier", session);
        let q = self.playback_identity(q);
        Some(PlayQueueResult::of(self.post_json(&q.build())?, rating_key))
    }
}

/// One retained row of the play queue: everything a queue list draws, plus everything
/// [`crate::route::request_play`] needs to START that row — and nothing else.
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
    pub rk: String,
    /// `type` — movie | episode | clip …
    pub kind: String,
    /// the item's own title (the episode title for an episode)
    pub title: String,
    /// `grandparentTitle` — the show, empty for a movie
    pub show_title: String,
    /// `parentIndex` — season number (0 for a movie)
    pub season: i64,
    pub index: i64,
    pub thumb: String,
    /// `grandparentThumb` — the SHOW's portrait poster (empty for a movie). Retained beside
    /// `thumb` because the two are different SHAPES: an episode's `thumb` is a landscape still,
    /// and the post-play card's 250×375 frame wants the portrait art.
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
    fn of(m: super::models::Metadata) -> QueueRow {
        // `Media[0]` / `Media[0].Part[0]`, the same pick `Metadata::first_part` makes — by value,
        // so the other versions and their Stream lists die here.
        let (vcodec, acodec, part) = match m.media.into_iter().next() {
            Some(md) => (
                md.video_codec,
                md.audio_codec,
                md.part.into_iter().next().map(|p| p.key).unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        QueueRow {
            item_id: m.play_queue_item_id,
            rk: m.rating_key,
            kind: m.kind,
            title: m.title,
            show_title: m.grandparent_title,
            season: m.parent_index,
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
pub fn queue_index_of(items: &[QueueRow], item_id: i64, rating_key: &str) -> Option<usize> {
    let by_id = (item_id != 0).then(|| items.iter().position(|r| r.item_id == item_id)).flatten();
    by_id.or_else(|| items.iter().position(|r| r.rk == rating_key))
}

/// The queue row that follows the selected one — None at the end of the queue, and None when the
/// selection matches no row at all (handing back `items[1]` there would start something the user
/// was never watching).
fn next_after<'a>(items: &'a [QueueRow], selected_item_id: i64, rating_key: &str) -> Option<&'a QueueRow> {
    items.get(queue_index_of(items, selected_item_id, rating_key)? + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::models::Envelope;

    /// one queue row: (playQueueItemID, ratingKey)
    fn row(item_id: i64, rk: &str) -> QueueRow {
        QueueRow { item_id, rk: rk.to_string(), ..Default::default() }
    }
    fn rks(items: &[QueueRow]) -> Vec<&str> {
        items.iter().map(|r| r.rk.as_str()).collect()
    }
    /// A response body through the SHIPPED mapping — the same call `create_play_queue` makes once
    /// its POST returns, so these tests cannot pass on a projection the app does not use.
    fn result(body: &str, rating_key: &str) -> PlayQueueResult {
        let mc = serde_json::from_str::<Envelope>(body).expect("a PMS body parses").media_container;
        PlayQueueResult::of(mc, rating_key)
    }

    /// The whole up-next extraction. Every case here is one the live server actually produces:
    /// a mid-season episode (a successor exists), a season finale with nothing after it
    /// (`playQueueTotalCount: 1` — verified on a show's last episode), and a movie, whose
    /// `continuous=1` queue is just itself.
    #[test]
    fn the_successor_is_the_row_after_the_selected_one() {
        let q = vec![row(13083, "1804"), row(13084, "1805"), row(13085, "1806")];
        let next = next_after(&q, 13083, "1804").expect("a mid-queue item has a successor");
        assert_eq!(next.rk, "1805");

        // a queue of one — the season finale / movie case
        let solo = vec![row(13092, "774")];
        assert!(next_after(&solo, 13092, "774").is_none());
        // ...and the LAST row of a longer queue is the same thing
        let q = vec![row(1, "a"), row(2, "b")];
        assert!(next_after(&q, 2, "b").is_none());
    }

    #[test]
    fn the_selected_row_is_found_by_queue_item_id_not_rating_key() {
        // A queue holding the same episode twice: matching on the ratingKey would pick the FIRST
        // occurrence and hand back its successor, which is the wrong episode.
        let q = vec![row(1, "dup"), row(2, "mid"), row(3, "dup"), row(4, "after")];
        let next = next_after(&q, 3, "dup").expect("the second occurrence has a successor");
        assert_eq!(next.rk, "after");
    }

    #[test]
    fn a_missing_queue_item_id_falls_back_to_the_rating_key() {
        // Rows with no playQueueItemID (0) — the successor must still be found, or the feature
        // silently disappears on a server that trims the field.
        let q = vec![row(0, "1804"), row(0, "1805")];
        let next = next_after(&q, 0, "1804").expect("the rating key is the fallback identity");
        assert_eq!(next.rk, "1805");
    }

    #[test]
    fn an_unrecognised_selection_yields_nothing_rather_than_the_first_row() {
        // Neither identity matches: returning `items[0]`'s successor here would start an episode
        // the user was never watching.
        let q = vec![row(1, "a"), row(2, "b")];
        assert!(next_after(&q, 99, "zzz").is_none());
        assert!(next_after(&[], 1, "a").is_none(), "an empty queue is not a panic");
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
        assert_eq!(rks(&q.items), ["1804", "1805", "1806"], "the WHOLE window is retained, not just the successor");
        assert_eq!((q.id, q.selected_item_id, q.remaining), (40213, 13083, 2), "the ids and the count still land");

        let first = &q.items[0];
        assert_eq!(first.item_id, 13083);
        assert_eq!(first.kind, "episode");
        assert_eq!(first.title, "Pilot");
        assert_eq!(first.show_title, "Example Show");
        assert_eq!((first.season, first.index), (1, 1));
        assert_eq!(first.thumb, "/library/metadata/1804/thumb/1781586780");
        assert_eq!((first.dur_ms, first.resume_ms), (3273248, 142000), "string-encoded numbers still land");
        assert_eq!(first.part, "/library/parts/3130/1781467224/file.mkv", "Media[0].Part[0].key");
        assert_eq!((first.vcodec.as_str(), first.acodec.as_str()), ("hevc", "eac3"), "Media[0], not the 1080p version");

        // the successor is exactly what it was when it was the only thing kept — and it is the
        // retained row after the selected one, not a separately-derived answer
        let next = q.next.as_ref().expect("the selected row has a successor");
        assert_eq!((next.rk.as_str(), next.index), ("1805", 2));
        assert_eq!(next.part, "/library/parts/3140/1781467999/file.mkv", "the successor is start-able");
        assert_eq!(next.dur_ms, 3120000, "…and a plain JSON number lands too");
        let at = queue_index_of(&q.items, q.selected_item_id, "1804").expect("the playing row is locatable");
        assert_eq!((at, q.items[at + 1].rk.as_str()), (0, next.rk.as_str()), "next IS items[selected+1]");
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
        assert_eq!(items[0].kind, "movie", "the projection is NOT episode-gated");
        assert_eq!(items[0].show_title, "", "a movie has no grandparentTitle");
        assert_eq!(items[0].part, "/p/774.mkv");
        let bare = &items[1];
        assert_eq!((bare.part.as_str(), bare.vcodec.as_str(), bare.acodec.as_str()), ("", "", ""));
        assert_eq!(bare.rk, "775", "the rest of the row still projects");
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
}

impl PlayQueueResult {
    /// The WHOLE response→result mapping, kept out of `create_play_queue` so the host tests grade
    /// the code that ships instead of a copy of it (only the request build is left up there).
    fn of(mc: super::models::MediaContainer, rating_key: &str) -> PlayQueueResult {
        // Remaining AFTER the item being played, from the whole-queue counters (the returned
        // `Metadata[]` can be a window). Clamped: a server that omits either counter must read as
        // "nothing after this", not as a negative count the UI would then format.
        let remaining = (mc.play_queue_total_count - mc.play_queue_selected_item_offset - 1).max(0);
        let selected = mc.play_queue_selected_item_id;
        // Project BY VALUE: every `Metadata` row is consumed into a `QueueRow` and its
        // Media/Part/Stream/Role tree dropped right here, on the worker. What the main thread then
        // holds for the length of the playback is a dozen scalars and strings per row.
        let items: Vec<QueueRow> = mc.metadata.into_iter().map(QueueRow::of).collect();
        let next = next_after(&items, selected, rating_key).cloned();
        PlayQueueResult { id: mc.play_queue_id, selected_item_id: selected, remaining, items, next }
    }
}
