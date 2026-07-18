//! Universal-transcoder + image-transcode operations (impl Client).
//!
//! The VIDEO ops were rebuilt FROM the live `route.rs` (task #26) — NOT from the deleted
//! originals, which had drifted stale (h264-only profile, no MDE handshake, no session
//! correlation). Two flavors hit `/video/:/transcode/universal/decision`:
//!   * [`Client::mde_decision`] — the hasMDE=1 "should this item direct-play?" ask
//!     (`directPlay=1`); the caller reads `Part.decision` off the returned body.
//!   * [`Client::transcode_decision`] — registers a transcode/remux session
//!     (`directPlay=0` + the caps of the chosen flavor) before `start.mkv` will stream.
//!     The caller MUST read the OUTPUT codecs off the returned body (Part.Stream[].codec):
//!     the Load payload has to describe what the server will actually send, not the source
//!     (see route::apply_decision_codecs and [[audio-payload-codecs]]).
//! Both return the parsed MediaContainer; a `?`/None degrades exactly like the old raw-body
//! scan (caller falls back to the local codec heuristic / skips the codec override).
use super::client::{Client, QueryBuilder, StreamUrl};
use super::models::MediaContainer;
use super::params::TranscodeSpec;

/// The direct-playable AUDIO codec set — what the buffer-feed pipeline decodes natively. ONE
/// definition: the [`is_dp_audio`] predicate gates every direct-play decision (route + the track
/// menu's native-switch), and `DP_AUDIO_CODECS` is the same set in the profile string's URL form.
pub const DP_AUDIO_CODECS: &str = "aac,ac3,eac3";
pub fn is_dp_audio(codec: &str) -> bool {
    matches!(codec, "aac" | "ac3" | "eac3")
}

/// Capability profile (X-Plex-Client-Profile-Extra, raw form — the QueryBuilder encodes it):
/// direct-play an MKV whose video is H264 or HEVC and audio AAC/AC3/EAC3, subs SRT/ASS, up to
/// 4K — plus an HEVC/AC3 transcode target so a source we can't direct-play (AV1/VP9/…) is
/// re-encoded to HEVC at native resolution (the panel decodes HEVC 4K natively) instead of
/// downscaled H264 1080p. NB: the SERVER must have HEVC encoding enabled for non-HEVC sources
/// (Settings → Transcoder → "Enable HEVC video Encoding = Always"); otherwise PMS drops the
/// video (audio-only) for an HEVC-only target. The bitDepth=10 upper bound declares 10-bit
/// support so PMS keeps HDR10 through the transcode (HEVC Main10, BT.2020+PQ in-bitstream) —
/// the same in-band static HDR10 SEI the direct-play path relies on (ff.rs keeps it).
fn profile_extra() -> String {
    format!(
        "add-direct-play-profile(type=videoProfile&container=mkv&videoCodec=h264,hevc\
         &audioCodec={DP_AUDIO_CODECS}&subtitleCodec=srt,subrip,ass,ssa)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value=3840&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value=2176&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.bitDepth&value=10&replace=true)\
         +add-transcode-target(type=videoProfile&context=streaming&protocol=http\
         &container=matroska&videoCodec=hevc&audioCodec=ac3)",
    )
}

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

    /// The ONE universal-transcoder param builder (was `route::universal_base` + flavor bases):
    /// the offset-free core is identical for [`Client::transcode_decision`] and
    /// [`Client::transcode_start_url`] — the decision that registers a session and the start.mkv
    /// it streams from MUST carry the same params.
    fn transcode_query(&self, s: &TranscodeSpec) -> String {
        let mut q = QueryBuilder::new("")
            .str("path", &format!("/library/metadata/{}", s.rating_key))
            .int("mediaIndex", 0)
            .int("partIndex", 0)
            .str("protocol", "http")
            .int("directPlay", 0)
            .int("directStream", 1);
        // the one block the two flavors differ in: container-only REMUX copies the codecs
        // (a resolution/bitrate cap would force a re-encode), RE-ENCODE caps at native 4K so
        // an undecodable source goes to the profile's HEVC target instead of downscaled H264.
        q = if s.remux {
            q.int("directStreamAudio", 1)
        } else {
            q.str("videoResolution", "3840x2160").int("maxVideoBitrate", 60000)
        };
        q = q.opt_int("audioStreamID", s.audio_stream_id);
        if s.subtitle_stream_id > 0 {
            // burned in (Plex's default decision for our profile — no soft-sub support advertised)
            q = q.int("subtitleStreamID", s.subtitle_stream_id).int("subtitleSize", 100).str("subtitles", "burn");
        }
        q = q.str("session", s.session).str("X-Plex-Session-Identifier", s.session);
        q = self
            .playback_identity(q)
            .str("X-Plex-Client-Profile-Name", "Generic")
            .str("X-Plex-Client-Profile-Extra", &profile_extra());
        if s.offset_secs >= 0 {
            q = q.int("offset", s.offset_secs);
        }
        q.query()
    }

    /// GET /video/:/transcode/universal/decision with hasMDE=1 + directPlay=1: ask the Media
    /// Decision Engine whether the item direct-plays given our capability profile. Registers
    /// the session as a side effect. The caller reads `Part.decision` ("directplay" vs
    /// "transcode") and the verdict codes off the returned container.
    pub fn mde_decision(&self, rating_key: &str, session: &str) -> Option<MediaContainer> {
        let q = QueryBuilder::new("/video/:/transcode/universal/decision")
            .str("path", &format!("/library/metadata/{rating_key}"))
            .int("mediaIndex", 0)
            .int("partIndex", 0)
            .str("protocol", "http")
            .int("hasMDE", 1)
            .int("directPlay", 1)
            .int("directStream", 1)
            .int("directStreamAudio", 1)
            .int("mediaBufferSize", 20971)
            .str("session", session)
            .str("X-Plex-Session-Identifier", session);
        let q = self
            .playback_identity(q)
            .str("X-Plex-Client-Profile-Name", "Generic")
            .str("X-Plex-Client-Profile-Extra", &profile_extra());
        self.get_json(&q.build())
    }

    /// Register `spec` with the universal transcoder (the /decision GET — required before
    /// start.mkv will stream; just a query, it never cuts a live streaming connection) and
    /// return the decision body. Callers building a fresh Load payload MUST feed the body
    /// through route::apply_decision_codecs — the payload has to describe what the server
    /// will actually send.
    pub fn transcode_decision(&self, spec: &TranscodeSpec) -> Option<MediaContainer> {
        self.get_json(&format!("/video/:/transcode/universal/decision?{}", self.transcode_query(spec)))
    }

    /// The start.mkv stream target for `spec` — same params as the registering decision.
    pub fn transcode_start_url(&self, spec: &TranscodeSpec) -> StreamUrl {
        let path = format!("/video/:/transcode/universal/start.mkv?{}", self.transcode_query(spec));
        StreamUrl { host: self.host.clone(), port: self.port, path: self.with_token(&path) }
    }

    /// GET /video/:/transcode/universal/stop — free the server-side encoder for `session`.
    pub fn transcode_stop(&self, session: &str) {
        let q = QueryBuilder::new("/video/:/transcode/universal/stop")
            .str("session", session)
            .str("X-Plex-Client-Identifier", &self.client_id);
        self.get_void(&q.build());
    }
}
