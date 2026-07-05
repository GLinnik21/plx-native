//! Transcode + image operations (impl Client). The universal-transcoder query (the shared
//! param set for decision / start.mkv / subtitles) is assembled ONCE here from
//! `TranscodeSpec` + the `Client` identity fields, so the legacy-vs-spec param choices
//! (maxVideoBitrate, session=, query-form X-Plex-*, the burn subtitle block) all live in one
//! place. `image_transcode_path` is the single method that returns a path String (posters
//! uses it as its LRU cache key AND the http_get path).
use super::client::{Client, QueryBuilder, StreamUrl};
use super::params::TranscodeSpec;

/// Fixed client profile advertised to the universal transcoder: progressive Matroska,
/// H264 + AC3 (what the in-app MKV demuxer + pipeline decode natively).
const PROFILE_EXTRA: &str = "add-transcode-target(type=videoProfile&context=streaming&protocol=http&container=matroska&videoCodec=h264&audioCodec=ac3)";

impl Client {
    /// The shared universal-transcode param string (no leading `?`, no token — the transport
    /// helper appends X-Plex-Token). Carries the current audio/subtitle selection and, when
    /// >0, the seek `offset`.
    fn transcode_query(&self, spec: &TranscodeSpec) -> String {
        let session = format!("plexpoc-{}", spec.rating_key);
        let meta_path = format!("/library/metadata/{}", spec.rating_key);
        let mut q = QueryBuilder::new("")
            .str("path", &meta_path)
            .int("mediaIndex", 0)
            .int("partIndex", 0)
            .str("protocol", "http")
            .int("directPlay", 0)
            .int("directStream", 1)
            .str("videoResolution", spec.video_resolution)
            .int("maxVideoBitrate", spec.max_video_bitrate);
        if spec.audio_stream_id > 0 {
            q = q.int("audioStreamID", spec.audio_stream_id);
        }
        if spec.subtitle_stream_id > 0 {
            q = q
                .int("subtitleStreamID", spec.subtitle_stream_id)
                .int("subtitleSize", 100)
                .str("subtitles", "burn");
        }
        if spec.offset_secs > 0 {
            q = q.int("offset", spec.offset_secs);
        }
        q.str("session", &session)
            .str("X-Plex-Session-Identifier", &session)
            .str("X-Plex-Client-Identifier", &self.client_id)
            .str("X-Plex-Product", &self.product)
            .str("X-Plex-Version", &self.version)
            .str("X-Plex-Platform", &self.platform)
            .str("X-Plex-Client-Profile-Extra", PROFILE_EXTRA)
            .query()
    }

    /// GET /video/:/transcode/universal/decision?… — REGISTERS the session (body discarded).
    /// Call before `transcode_start_url`. Carries the offset for a seek/re-transcode.
    pub fn transcode_decision(&self, spec: &TranscodeSpec) {
        self.get_void(&format!("/video/:/transcode/universal/decision?{}", self.transcode_query(spec)));
    }

    /// GET /video/:/transcode/universal/start.mkv?… — the progressive H264+AC3 Matroska the
    /// demuxer eats. Returns the StreamUrl for http_open (NOT fetched here).
    pub fn transcode_start_url(&self, spec: &TranscodeSpec) -> StreamUrl {
        let path = self.with_token(&format!(
            "/video/:/transcode/universal/start.mkv?{}",
            self.transcode_query(spec)
        ));
        StreamUrl { host: self.host.clone(), port: self.port, path }
    }

    /// GET /video/:/transcode/universal/subtitles?… — soft (SRT/VTT) subs for client-side
    /// rendering instead of burn-in. Returns a StreamUrl (open with http_open, or http_get the
    /// small body).
    pub fn transcode_subtitles_url(&self, spec: &TranscodeSpec) -> StreamUrl {
        let path = self.with_token(&format!(
            "/video/:/transcode/universal/subtitles?{}",
            self.transcode_query(spec)
        ));
        StreamUrl { host: self.host.clone(), port: self.port, path }
    }

    /// GET /video/:/transcode/universal/stop?session=… — free the encoder (D-9 undocumented;
    /// body discarded).
    pub fn transcode_stop(&self, session: &str) {
        let path = QueryBuilder::new("/video/:/transcode/universal/stop")
            .str("session", session)
            .str("X-Plex-Client-Identifier", session)
            .build();
        self.get_void(&path);
    }

    /// GET /photo/:/transcode?width&height&minSize=1&url=…[&format=png]&X-Plex-Token — returns
    /// the built PATH (the sole path-returning method): posters uses it as the LRU key AND the
    /// http_get path. `src_path` is the raw thumb/art path; encoding is centralized in enc().
    pub fn image_transcode_path(&self, src_path: &str, w: i64, h: i64, png: bool) -> String {
        let mut q = QueryBuilder::new("/photo/:/transcode")
            .int("width", w)
            .int("height", h)
            .int("minSize", 1)
            .str("url", src_path);
        if png {
            q = q.str("format", "png");
        }
        self.with_token(&q.build())
    }
}
