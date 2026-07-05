//! Playback-progress report (impl Client): POST /:/timeline (spec verb — fixes D-8a).
use super::client::{Client, QueryBuilder};
use super::params::TimelineReport;

impl Client {
    /// POST /:/timeline?ratingKey&key&state&time&duration&X-Plex-Client-Identifier. The `key=`
    /// value (/library/metadata/{rk}) is enc()'d internally. Fire-and-forget.
    pub fn timeline(&self, report: &TimelineReport) {
        let key = format!("/library/metadata/{}", report.rating_key);
        let path = QueryBuilder::new("/:/timeline")
            .str("ratingKey", report.rating_key)
            .str("key", &key)
            .str("state", report.state.as_str())
            .int("time", report.time_ms)
            .int("duration", report.duration_ms)
            .str("X-Plex-Client-Identifier", &self.client_id)
            .build();
        let _ = self.post(&path);
    }
}
