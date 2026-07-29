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
        let mut mc = self.post_json(&q.build())?;
        // Remaining AFTER the item being played, from the whole-queue counters (the returned
        // `Metadata[]` can be a window). Clamped: a server that omits either counter must read as
        // "nothing after this", not as a negative count the UI would then format.
        let remaining = (mc.play_queue_total_count - mc.play_queue_selected_item_offset - 1).max(0);
        let selected = mc.play_queue_selected_item_id;
        let next = next_after(&mut mc.metadata, selected, rating_key);
        Some(PlayQueueResult { id: mc.play_queue_id, selected_item_id: selected, remaining, next })
    }
}

/// The queue row that follows the selected one — pulled OUT of the item list rather than cloned,
/// because a `Metadata` row carries the whole Media/Part/Stream/Role tree and this runs on the
/// resolve worker on a 32-bit TV.
///
/// Identity is `playQueueItemID`, not `ratingKey`: a queue may legitimately hold the same item
/// twice, and matching on the rating key would then pick the wrong successor. The rating key is
/// only the fallback for a server that omitted the per-row id — for a queue built FROM this item
/// the two agree, and losing Up Next entirely is the worse failure.
fn next_after(
    items: &mut Vec<super::models::Metadata>,
    selected_item_id: i64,
    rating_key: &str,
) -> Option<super::models::Metadata> {
    let by_id = (selected_item_id != 0)
        .then(|| items.iter().position(|m| m.play_queue_item_id == selected_item_id))
        .flatten();
    let at = by_id.or_else(|| items.iter().position(|m| m.rating_key == rating_key))?;
    // `drain` on an empty trailing range is legal and yields None, which is the same answer a
    // bounds check would produce — so there is no bounds check.
    items.drain(at + 1..).next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::models::Metadata;

    /// one queue row: (playQueueItemID, ratingKey)
    fn row(item_id: i64, rk: &str) -> Metadata {
        Metadata { play_queue_item_id: item_id, rating_key: rk.to_string(), ..Default::default() }
    }
    fn rks(items: &[Metadata]) -> Vec<&str> {
        items.iter().map(|m| m.rating_key.as_str()).collect()
    }

    /// The whole up-next extraction. Every case here is one the live server actually produces:
    /// a mid-season episode (a successor exists), a season finale with nothing after it
    /// (`playQueueTotalCount: 1` — verified on The Office S5E26), and a movie, whose
    /// `continuous=1` queue is just itself.
    #[test]
    fn the_successor_is_the_row_after_the_selected_one() {
        let mut q = vec![row(13083, "1804"), row(13084, "1805"), row(13085, "1806")];
        let next = next_after(&mut q, 13083, "1804").expect("a mid-queue item has a successor");
        assert_eq!(next.rating_key, "1805");
        assert_eq!(rks(&q), ["1804"], "the rows after the successor are dropped with it");

        // a queue of one — the season finale / movie case
        let mut solo = vec![row(13092, "774")];
        assert!(next_after(&mut solo, 13092, "774").is_none());
        // ...and the LAST row of a longer queue is the same thing
        let mut q = vec![row(1, "a"), row(2, "b")];
        assert!(next_after(&mut q, 2, "b").is_none());
    }

    #[test]
    fn the_selected_row_is_found_by_queue_item_id_not_rating_key() {
        // A queue holding the same episode twice: matching on the ratingKey would pick the FIRST
        // occurrence and hand back its successor, which is the wrong episode.
        let mut q = vec![row(1, "dup"), row(2, "mid"), row(3, "dup"), row(4, "after")];
        let next = next_after(&mut q, 3, "dup").expect("the second occurrence has a successor");
        assert_eq!(next.rating_key, "after");
    }

    #[test]
    fn a_missing_queue_item_id_falls_back_to_the_rating_key() {
        // Rows with no playQueueItemID (0) — the successor must still be found, or the feature
        // silently disappears on a server that trims the field.
        let mut q = vec![row(0, "1804"), row(0, "1805")];
        let next = next_after(&mut q, 0, "1804").expect("the rating key is the fallback identity");
        assert_eq!(next.rating_key, "1805");
    }

    #[test]
    fn an_unrecognised_selection_yields_nothing_rather_than_the_first_row() {
        // Neither identity matches: returning `items[0]`'s successor here would start an episode
        // the user was never watching.
        let mut q = vec![row(1, "a"), row(2, "b")];
        assert!(next_after(&mut q, 99, "zzz").is_none());
        assert!(next_after(&mut Vec::new(), 1, "a").is_none(), "an empty queue is not a panic");
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
    /// the item that plays next, or None at the end of the queue (and for a movie, whose
    /// continuous queue is just itself — verified live: total count 1)
    pub next: Option<super::models::Metadata>,
}
