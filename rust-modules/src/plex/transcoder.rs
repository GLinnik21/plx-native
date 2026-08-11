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
/// direct-play an MKV or MP4 whose video is H264 or HEVC and audio AAC/AC3/EAC3, subs SRT/ASS,
/// up to 4K — plus a transcode target so a source we can't direct-play is re-encoded, at native
/// resolution (the panel decodes HEVC 4K natively) instead of downscaled H264 1080p. The
/// container list must agree with `route.rs::part_is_streamable` (the client-side gate): a
/// container the app streams but the profile omits makes every MDE answer for it a
/// contradiction of what the app then does.
///
/// The target's codec lists are FALLBACK CHAINS, not single choices, and the order encodes the
/// whole free-vs-Plex-Pass story (issue #22, found on a reviewer's server):
///
/// * `videoCodec=hevc,h264` — HEVC encoding sits behind Plex Pass. When this list held only
///   `hevc`, a free server found no usable video target and **dropped the video track**: the
///   transcoder job carried a single audio `-map`, the demuxer correctly said
///   `ff: no video stream`, and every MP4 in the library "failed to play" on every firmware.
///   (`TranscoderHEVCEncoding=1` does not help — the subscription gate sits behind the
///   preference.) With h264 in the list PMS always has a working encode, and — just as
///   important — an H.264 source reaching this path for its *container* alone (mp4 → mkv remux)
///   can now be **direct-streamed** (copied) instead of failing: direct-stream requires the
///   source codec to appear in the target list. A Plex Pass server with "Enable HEVC video
///   Encoding = Always" still picks hevc — verified live; order expresses preference.
/// * `audioCodec=ac3,eac3,aac` — same rule on the audio lane: MDE logged "Cannot direct stream
///   audio stream due to codec aac when profile only allows ac3" and re-encoded audio that the
///   pipeline decodes natively. All three are in `DP_AUDIO_CODECS`, so anything copied is
///   something we play; a track that genuinely needs encoding (TrueHD/DTS) goes to ac3, the
///   first entry.
///
/// The Load payload cannot drift whatever PMS chooses: `route.rs` reads the OUTPUT codecs off
/// the /decision response (`decision_codecs`) and describes those, not the profile's wish.
///
/// The bitDepth=10 upper bound declares 10-bit support so PMS keeps HDR10 through the transcode
/// (HEVC Main10, BT.2020+PQ in-bitstream) — the same in-band static HDR10 SEI the direct-play
/// path relies on (ff.rs keeps it).
fn profile_extra() -> String {
    format!(
        "add-direct-play-profile(type=videoProfile&container=mkv,mp4&videoCodec=h264,hevc\
         &audioCodec={DP_AUDIO_CODECS}&subtitleCodec=srt,subrip,ass,ssa)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value=3840&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value=2176&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.bitDepth&value=10&replace=true)\
         +add-transcode-target(type=videoProfile&context=streaming&protocol=http\
         &container=matroska&videoCodec=hevc,h264&audioCodec=ac3,eac3,aac)",
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

#[cfg(test)]
mod tests {
    /// Issue #22: with `videoCodec=hevc` alone, a server without Plex Pass has no legal video
    /// target — it drops the video track and sends audio only, so every non-MKV item "fails to
    /// play" on every firmware. The target lists are fallback CHAINS and each must end in
    /// something a free server can produce: h264 (encode) for video, and for audio every codec
    /// we direct-play, so a source track we could decode is copied rather than re-encoded.
    #[test]
    fn the_transcode_target_never_strands_a_server_without_plex_pass() {
        let p = super::profile_extra();
        let target = p.split("add-transcode-target").nth(1).expect("profile declares a transcode target");
        let list_of = |key: &str| -> Vec<String> {
            let v = target.split(key).nth(1).expect(key).split('&').next().unwrap_or("");
            v.trim_end_matches(')').split(',').map(str::to_string).collect()
        };
        let video = list_of("videoCodec=");
        assert!(video.contains(&"h264".to_string()),
            "no subscription-free video fallback in {video:?} — a free server drops the video track");
        assert_eq!(video.first().map(String::as_str), Some("hevc"),
            "hevc must stay FIRST: order is preference, and hevc is what keeps 4K+HDR10 through a re-encode");
        for c in super::DP_AUDIO_CODECS.split(',') {
            assert!(list_of("audioCodec=").contains(&c.to_string()),
                "{c} is direct-playable but absent from the target — the server would re-encode a track we decode natively");
        }
    }
}
