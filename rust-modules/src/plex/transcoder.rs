//! Image-transcode operation (impl Client). `image_transcode_path` is the single method that
//! returns a path String (posters uses it as its LRU cache key AND the http_get path).
//!
//! The VIDEO universal-transcoder ops that used to live here (decision/start/subtitles/stop)
//! were deleted: they had zero callers and had drifted stale against the live playback layer
//! in `route.rs` (h264-only profile vs the shipped hevc/direct-play one). When the playback
//! path migrates onto the typed client (task #26), rebuild them FROM route.rs, not git history.
use super::client::{Client, QueryBuilder};

impl Client {
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
