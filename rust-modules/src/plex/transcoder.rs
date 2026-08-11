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

/// The AUDIO codec set the buffer-feed PIPELINE decodes. This is the software half of a
/// two-sided test — what our demuxer/payload path can feed, before asking whether this
/// particular SoC can decode it. The live set is `devcaps::Caps::audio` (this list ∩ the
/// device's own codec table), and the ONE-definition rule moved there with it: the
/// [`is_dp_audio`] predicate that gates every direct-play decision (route + the track menu's
/// native-switch) and the profile string's audio lists BOTH read the caps snapshot, so the
/// claim sent to PMS and the gate applied locally cannot drift apart.
pub const DP_AUDIO_CODECS: &str = "aac,ac3,eac3";
pub fn is_dp_audio(codec: &str) -> bool {
    crate::devcaps::caps().audio_has(codec)
}

/// Capability profile (X-Plex-Client-Profile-Extra, raw form — the QueryBuilder encodes it),
/// as a PURE function of the device's decode capabilities: direct-play an MKV or MP4 whose
/// video the SoC decodes (H264 everywhere; HEVC when its table says so) and audio is in the
/// caps subset, subs SRT/ASS — plus a transcode target so a source we can't direct-play is
/// re-encoded at the panel's own bound instead of downscaled H264 1080p. The container list
/// must agree with `route.rs::part_is_streamable` (the client-side gate): a container the app
/// streams but the profile omits makes every MDE answer for it a contradiction of what the app
/// then does.
///
/// Pure over `&Caps` — not a reader of `devcaps::caps()` — so every derivation below is
/// host-testable against capability sets no development hardware has (the whole point:
/// issue #22's bug class is dev-TV claims asserted as universal, and the decode-capability
/// claim was its last member — docs/plex-pass-audit.md, closing section).
///
/// Derivations, axis by axis:
///
/// * **Direct-play `videoCodec`** — h264 unconditionally (every webOS SoC decodes it; a table
///   so broken it omits h264 is a misread, and `devcaps::parse` rejects it whole), hevc only
///   when the device's table lists the decoder.
/// * **`video.width`/`video.height` upper bounds** — `caps.hevc_max`, the table's own merged
///   numbers (the dev set: 4096x2176; the pre-devcaps constants 3840x2176 remain the fallback).
/// * **`bitDepth=10`** — a CONSTANT, not a derivation: the device table has no bit-depth axis
///   anywhere, so it can neither confirm nor deny 10-bit. The claim stays because it is what
///   keeps HDR10 through a transcode (HEVC Main10, BT.2020+PQ in-bitstream — the same in-band
///   static HDR10 SEI the direct-play path relies on; ff.rs keeps it).
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
///   Encoding = Always" still picks hevc — verified live; order expresses preference. On a SoC
///   without HEVC the chain is `h264` alone: hevc first would have PMS encode a stream the
///   panel cannot decode — the same wrong-side failure #22 was, pointed the other way.
/// * `audioCodec=ac3,eac3,aac` (the caps subset, ac3-preferred order) — same rule on the audio
///   lane: MDE logged "Cannot direct stream audio stream due to codec aac when profile only
///   allows ac3" and re-encoded audio that the pipeline decodes natively. The list is exactly
///   the caps audio subset, so anything copied is something we both feed and decode; a track
///   that genuinely needs encoding (TrueHD/DTS) goes to the first entry.
///
/// The Load payload cannot drift whatever PMS chooses: `route.rs` reads the OUTPUT codecs off
/// the /decision response (`decision_codecs`) and describes those, not the profile's wish.
fn profile_for(caps: &crate::devcaps::Caps) -> String {
    let dp_video = if caps.hevc { "h264,hevc" } else { "h264" };
    // The chain's head is the ONE encode-target definition, `Caps::encode_vcodec` — the same
    // accessor route.rs's /decision-unreachable Load-payload guess and retranscode read, so the
    // payload can never name a codec this profile did not ask the server to produce — with h264
    // appended as the subscription-free fallback (see below; when the head IS h264 the chain is
    // just h264).
    let target_video = match caps.encode_vcodec() {
        "h264" => "h264".to_string(),
        head => format!("{head},h264"),
    };
    let (w, h) = caps.hevc_max;
    let dp_audio = &caps.audio;
    // ac3 first — the preferred ENCODE target — then the rest of the caps subset as copy lanes.
    let target_audio =
        ["ac3", "eac3", "aac"].into_iter().filter(|c| caps.audio_has(c)).collect::<Vec<_>>().join(",");
    format!(
        "add-direct-play-profile(type=videoProfile&container=mkv,mp4&videoCodec={dp_video}\
         &audioCodec={dp_audio}&subtitleCodec=srt,subrip,ass,ssa)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value={w}&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value={h}&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.bitDepth&value=10&replace=true)\
         +add-transcode-target(type=videoProfile&context=streaming&protocol=http\
         &container=matroska&videoCodec={target_video}&audioCodec={target_audio})",
    )
}

/// The profile for THIS device — [`profile_for`] over the boot-probed caps snapshot.
fn profile_extra() -> String {
    profile_for(crate::devcaps::caps())
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
    use crate::devcaps::Caps;

    /// Split a codec list out of the given profile segment ("add-direct-play-profile…" or
    /// "add-transcode-target…") — shared by every derivation test below.
    fn list_of(segment: &str, key: &str) -> Vec<String> {
        let v = segment.split(key).nth(1).expect(key).split('&').next().unwrap_or("");
        v.trim_end_matches(')').split(',').map(str::to_string).collect()
    }
    fn target_of(p: &str) -> &str {
        p.split("add-transcode-target").nth(1).expect("profile declares a transcode target")
    }

    /// Issue #22: with `videoCodec=hevc` alone, a server without Plex Pass has no legal video
    /// target — it drops the video track and sends audio only, so every non-MKV item "fails to
    /// play" on every firmware. The target lists are fallback CHAINS and each must end in
    /// something a free server can produce: h264 (encode) for video, and for audio every codec
    /// we direct-play, so a source track we could decode is copied rather than re-encoded.
    /// Grades `profile_extra` (the composed path), which on the host resolves to the assumed
    /// caps — the same hevc-capable panel this rule was written on.
    #[test]
    fn the_transcode_target_never_strands_a_server_without_plex_pass() {
        let p = super::profile_extra();
        let target = target_of(&p);
        let video = list_of(target, "videoCodec=");
        assert!(video.contains(&"h264".to_string()),
            "no subscription-free video fallback in {video:?} — a free server drops the video track");
        assert_eq!(video.first().map(String::as_str), Some("hevc"),
            "hevc must stay FIRST: order is preference, and hevc is what keeps 4K+HDR10 through a re-encode");
        for c in super::DP_AUDIO_CODECS.split(',') {
            assert!(list_of(target, "audioCodec=").contains(&c.to_string()),
                "{c} is direct-playable but absent from the target — the server would re-encode a track we decode natively");
        }
    }

    /// The other side of issue #22's fallback-chain rule: on a SoC whose table has no HEVC row,
    /// hevc must vanish from BOTH lists — from direct-play (we would feed a stream the panel
    /// cannot decode) and from the transcode target (hevc-first would have PMS encode one for
    /// it) — while h264 stays, and the resolution bound is the device's own, not the dev TV's.
    #[test]
    fn a_soc_without_hevc_gets_an_h264_only_profile_at_its_own_bound() {
        let caps =
            Caps { hevc: false, hevc_max: (1920, 1088), vp9: false, audio: "aac,ac3,eac3".into() };
        let p = super::profile_for(&caps);
        assert!(!p.contains("hevc"), "hevc must not appear anywhere in {p}");
        let dp = p.split("add-transcode-target").next().unwrap();
        assert_eq!(list_of(dp, "videoCodec="), ["h264"]);
        assert_eq!(list_of(target_of(&p), "videoCodec="), ["h264"]);
        assert!(p.contains("name=video.width&value=1920&") && p.contains("name=video.height&value=1088&"),
            "the upper bounds must be the device table's, or PMS direct-plays 4K onto a 1080p decoder: {p}");
    }

    /// PIN: the transcode target's FIRST entry is `Caps::encode_vcodec` — the one definition
    /// route.rs's Load-payload guess and retranscode's `set_stream_codecs` also read. Before the
    /// accessor existed the head was spelled three times by hand (`if caps().hevc { "hevc" } else
    /// { "h264" }`), and drift meant a Load payload naming a codec the profile never asked the
    /// server to produce — the payload/output mismatch of docs/plex-pass-audit.md row 1.
    #[test]
    fn the_target_head_is_the_shared_encode_vcodec_definition() {
        for caps in [
            Caps::assumed(),
            Caps { hevc: false, hevc_max: (1920, 1088), vp9: false, audio: "aac".into() },
        ] {
            let p = super::profile_for(&caps);
            let video = list_of(target_of(&p), "videoCodec=");
            assert_eq!(video.first().map(String::as_str), Some(caps.encode_vcodec()));
        }
    }

    /// The audio lists follow the caps subset on BOTH sides too: a device table without EAC3/AC3
    /// must not advertise them for copy (direct-stream would hand the panel a stream it cannot
    /// decode), and the transcode chain keeps its ac3-preferred ORDER for whatever remains.
    #[test]
    fn the_audio_lists_are_the_caps_subset_in_both_profiles() {
        let caps = Caps { hevc: true, hevc_max: (3840, 2176), vp9: true, audio: "aac".into() };
        let p = super::profile_for(&caps);
        let dp = p.split("add-transcode-target").next().unwrap();
        assert_eq!(list_of(dp, "audioCodec="), ["aac"]);
        assert_eq!(list_of(target_of(&p), "audioCodec="), ["aac"]);
    }

    /// PIN: the assumed (table-unreadable) profile is byte-identical to the constant string the
    /// app sent before devcaps existed. This is the fallback half of devcaps' contract — the
    /// derivation may never drift for a device that was working yesterday, and any deliberate
    /// profile change must update this literal to say so.
    #[test]
    fn the_assumed_profile_is_byte_identical_to_the_shipped_one() {
        assert_eq!(
            super::profile_for(&Caps::assumed()),
            "add-direct-play-profile(type=videoProfile&container=mkv,mp4&videoCodec=h264,hevc\
             &audioCodec=aac,ac3,eac3&subtitleCodec=srt,subrip,ass,ssa)\
             +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value=3840&replace=true)\
             +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value=2176&replace=true)\
             +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.bitDepth&value=10&replace=true)\
             +add-transcode-target(type=videoProfile&context=streaming&protocol=http\
             &container=matroska&videoCodec=hevc,h264&audioCodec=ac3,eac3,aac)"
        );
    }
}
