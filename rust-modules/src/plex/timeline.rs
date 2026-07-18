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

    /// POST /playQueues for one item → (playQueueID, playQueueSelectedItemID). Best-effort:
    /// None on failure — the timeline still works, just without the queue ids.
    pub fn create_play_queue(&self, machine_id: &str, rating_key: &str, session: &str) -> Option<(i64, i64)> {
        let uri = format!("server://{machine_id}/com.plexapp.plugins.library/library/metadata/{rating_key}");
        let q = QueryBuilder::new("/playQueues")
            .str("type", "video")
            .str("uri", &uri)
            .int("continuous", 1)
            .int("shuffle", 0)
            .int("repeat", 0)
            .str("X-Plex-Session-Identifier", session);
        let q = self.playback_identity(q);
        let mc = self.post_json(&q.build())?;
        Some((mc.play_queue_id, mc.play_queue_selected_item_id))
    }
}
