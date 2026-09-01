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
use super::params::{Ceiling, TranscodeDelivery, TranscodeSpec};
use super::probe::Location;

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

// ---- the relay policy: what the LINK to a server allows a plan to ask for -------------------

/// What the connection tier a server is reached over lets a playback plan ask for. Both fields
/// are true on every tier but [`Location::Relay`] — see [`link_policy`], which is where the
/// reasoning lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkPolicy {
    /// May the client stream the source file itself? False forces the transcode branch.
    pub direct_play: bool,
    /// May that transcode be a container-only REMUX — codecs copied, no cap? False forces the
    /// RE-ENCODE flavor, the only one whose query carries a bound the server can come in under.
    pub remux: bool,
}

impl LinkPolicy {
    /// Everything allowed: every tier but relay, and every link nothing has told us about.
    ///
    /// **Unknown must be permissive, and that is a decision rather than a default.** Nothing in
    /// this app dials from a [`Location`] yet — `probe::candidates` ranks addresses and the racing
    /// lands with the transport work — so `Client::link()` is `None` on every play today. Reading
    /// `None` as "assume relay" would downgrade every direct play on earth to a server-side
    /// encode, to prevent a stall on a connection this codebase has never once made.
    pub const UNRESTRICTED: LinkPolicy = LinkPolicy {
        direct_play: true,
        remux: true,
    };
}

/// **The relay policy**, and the only place it is spelled out. `None` = nobody has said how this
/// server is reached (see [`LinkPolicy::UNRESTRICTED`]).
///
/// Plex's relay is a tunnel the server holds open to a Plex relay host: a second https connection,
/// conventionally port 8443, **capped at 2 Mbit/s**, with the server transcoding down to fit
/// whatever the client asks for. [`probe`](super::probe) ranks it last, so it only ever wins when
/// there is nothing else — a share behind CGNAT, an owner with no port forward. When it does win,
/// the two fast paths this client is built around become the wrong answer, and both have to go:
///
/// * **No direct play.** Direct play streams the file's own bytes, and a library's own bytes are
///   tens of Mbit/s (the share measured on 2026-08-11: 31 Mbit/s — `docs/shared-servers.md` §2)
///   down a 2 Mbit/s pipe. The server cannot rescue it, because nothing is being transcoded and
///   so there is nothing for it to fit; the cap arrives as a stall two minutes into the film,
///   with the AU queue draining and no error on any surface. Forcing the transcode branch turns
///   that into a `/decision` the server answers with something that fits.
/// * **No container REMUX either** — the half that is easy to miss, because a remux *feels* like
///   a concession already. It is not: a remux copies the codecs and deliberately sends **no
///   cap** (`transcode_query` below omits `videoResolution`/`maxVideoBitrate` on that branch,
///   because a cap is exactly what would force the re-encode it is trying to avoid). So it ships
///   the same bytes at the same rate one layer down, and stalls identically. Denying it leaves
///   the re-encode, which is the only flavor whose whole point is that the server picks the rate.
///
/// **Not a parameter, deliberately — THIS policy sends no number at all.** A cap is meaningless on
/// direct play (no encoder is running to obey it) and on the re-encode branch the server already
/// receives an upper bound it is free to come in far under. The relay's real ceiling is known to
/// the server, which is on the other end of the tunnel and applies it; this policy's whole job is
/// to stop asking for the two flavors that leave it no way to.
///
/// This paragraph used to end "[`TranscodeSpec`] has no bitrate field and this does not add one",
/// and the spec grew [`Ceiling`] on 2026-08-23 — so read it as being about the RELAY, which is
/// what it always was. A **user-chosen** ceiling (`route::Quality`) is a different input through
/// the identical mechanism: `route::quality_policy` returns this same [`LinkPolicy`],
/// `route::flavors_allowed` composes the two so the stricter wins per flavor, and only then does a
/// number reach `transcode_query`. Nothing about the relay tier changed — it still denies both
/// flavors unconditionally and still names no rate.
///
/// **No relay connection has been observed on a device by this codebase.** Before discovery racing,
/// the old chooser discarded every relay. Relay is now a second-phase candidate, but the 2 Mbit/s
/// figure and port-8443 convention remain Plex documentation rather than our measurement, and this
/// policy is **unverified against a real relay** — `docs/shared-servers.md` §7 question 5. What is
/// asserted is the shape, not the number: whatever the ceiling turns out to be, only the server can
/// apply it, and only if an encoder runs.
pub fn link_policy(link: Option<Location>) -> LinkPolicy {
    match link {
        Some(Location::Relay) => LinkPolicy {
            direct_play: false,
            remux: false,
        },
        // Exhaustive on purpose: a new tier must come here and say what it allows, rather than
        // inheriting "unrestricted" from a wildcard because nobody thought about it.
        Some(Location::Local) | Some(Location::Remote) | None => LinkPolicy::UNRESTRICTED,
    }
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
fn profile_for_delivery(caps: &crate::devcaps::Caps, delivery: TranscodeDelivery) -> String {
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
    let target_audio = ["ac3", "eac3", "aac"]
        .into_iter()
        .filter(|c| caps.audio_has(c))
        .collect::<Vec<_>>()
        .join(",");
    let target = match delivery {
        TranscodeDelivery::ProgressiveMkv => format!(
            "add-transcode-target(type=videoProfile&context=streaming&protocol=http\
             &container=matroska&videoCodec={target_video}&audioCodec={target_audio})"
        ),
        // This is the exact narrow target measured against the configured PMS. HLS adaptation is
        // encoder-session replacement, so this target intentionally exposes one H.264/AAC
        // rendition rather than advertising codecs whose in-session switch behaviour is unknown.
        TranscodeDelivery::FixedHls { .. } => {
            "add-transcode-target(type=videoProfile&context=streaming&protocol=hls\
             &container=mpegts&videoCodec=h264&audioCodec=aac)"
                .to_string()
        }
    };
    format!(
        "add-direct-play-profile(type=videoProfile&container=mkv,mp4&videoCodec={dp_video}\
         &audioCodec={dp_audio}&subtitleCodec=srt,subrip,ass,ssa)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value={w}&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value={h}&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.bitDepth&value=10&replace=true)\
         +{target}",
    )
}

fn profile_for(caps: &crate::devcaps::Caps) -> String {
    profile_for_delivery(caps, TranscodeDelivery::ProgressiveMkv)
}

/// The profile for THIS device — [`profile_for`] over the boot-probed caps snapshot.
fn profile_extra(delivery: TranscodeDelivery) -> String {
    profile_for_delivery(crate::devcaps::caps(), delivery)
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
        // `directStream` is the server's permission to COPY a track instead of encoding it, and
        // it is granted on every flavor but one. A cap is not a refusal: with the permission in
        // hand PMS copies whenever the source fits the caps this query carries — resolution,
        // bitrate, and the profile's own limitation axes — and NONE of those axes can express
        // "this file's pixels are wrong for us". So a source refused for what it IS rather than
        // for how big it is (a Dolby Vision base layer that is not self-displayable) came back
        // `Part.decision=transcode` with the video's own decision `copy`: the same bitstream one
        // container down. `no_video_copy` withdraws the permission for exactly that case, and
        // only on the re-encode flavor — a REMUX is a copy by definition, so the two can never
        // both be true (`build_stream` derives `remux` from a gate this flag has already failed).
        // See [`TranscodeSpec::no_video_copy`] for the measurement.
        let hls = matches!(s.delivery, TranscodeDelivery::FixedHls { .. });
        debug_assert!(
            !(hls && s.remux),
            "fixed HLS is an encoded rendition, never a remux"
        );
        let copy_ok = !hls && !(s.no_video_copy && !s.remux);
        let protocol = if hls { "hls" } else { "http" };
        let mut q = QueryBuilder::new("")
            .str("path", &format!("/library/metadata/{}", s.rating_key))
            .int("mediaIndex", 0)
            .int("partIndex", 0)
            .str("protocol", protocol)
            .int("directPlay", 0)
            .int("directStream", copy_ok as i64);
        // the one block the two flavors differ in: container-only REMUX copies the codecs
        // (a resolution/bitrate cap would force a re-encode), RE-ENCODE caps at the ceiling this
        // playback is bound by, so an undecodable source goes to the profile's HEVC target
        // instead of downscaled H264.
        //
        // **The ceiling is the PARAMETER half of a decision already taken.** `None` is Original
        // and resolves to [`Ceiling::NATIVE_4K`] — the literal pair this line has always sent, so
        // that migration-safe path is byte-identical to yesterday's. A `Some` is the user's fixed
        // pick or Auto controller rung, and it can only be spent here because `route::quality_policy`
        // has already denied direct play and the remux for this source: a number is meaningless on
        // a flavor where no encoder runs to read it (`TranscodeSpec`'s own doc, and `link_policy`'s
        // for the relay case that first established the shape).
        //
        // The remux branch still sends no cap, WITH a ceiling set, and that stays correct rather
        // than becoming a hole: `quality_policy` admits a remux only for a source it measured
        // under the ceiling, so those bytes are already inside the bound. A cap here would do the
        // one thing the remux exists to avoid — force the re-encode.
        q = if s.remux && !hls {
            q.int("directStreamAudio", 1)
        } else {
            let c = s.ceiling.unwrap_or(Ceiling::NATIVE_4K);
            let q = q
                .str("videoResolution", &c.resolution())
                .int("maxVideoBitrate", c.max_kbps);
            match s.delivery {
                TranscodeDelivery::ProgressiveMkv => q,
                TranscodeDelivery::FixedHls {
                    seconds_per_segment,
                } => q
                    .int("videoBitrate", c.max_kbps)
                    .int("peakBitrate", c.max_kbps)
                    .int("autoAdjustQuality", 0)
                    .int("secondsPerSegment", seconds_per_segment as i64)
                    .int("videoQuality", 100)
                    .int("mediaBufferSize", 20971)
                    .int("fastSeek", 1),
            }
        };
        if hls {
            q = q.int("directStreamAudio", 0);
        } else if !copy_ok {
            // the audio lane keeps its own permission, so this costs a VIDEO encoder and nothing
            // else — an AC3/E-AC3 track the pipeline already decodes is still copied through
            q = q.int("directStreamAudio", 1);
        }
        q = q.opt_int("audioStreamID", s.audio_stream_id);
        if s.subtitle_stream_id > 0 {
            // burned in (Plex's default decision for our profile — no soft-sub support advertised)
            q = q
                .int("subtitleStreamID", s.subtitle_stream_id)
                .int("subtitleSize", 100)
                .str("subtitles", "burn");
        }
        q = q
            .str("session", s.encoder_session)
            .str("X-Plex-Session-Identifier", s.session);
        q = self
            .playback_identity(q)
            .str("X-Plex-Client-Profile-Name", "Generic")
            .str("X-Plex-Client-Profile-Extra", &profile_extra(s.delivery));
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
            .str(
                "X-Plex-Client-Profile-Extra",
                &profile_extra(TranscodeDelivery::ProgressiveMkv),
            );
        self.get_json(&q.build())
    }

    /// Register `spec` with the universal transcoder (the /decision GET — required before
    /// start.mkv will stream; just a query, it never cuts a live streaming connection) and
    /// return the decision body. Callers building a fresh Load payload MUST feed the body
    /// through route::apply_decision_codecs — the payload has to describe what the server
    /// will actually send.
    pub fn transcode_decision(&self, spec: &TranscodeSpec) -> Option<MediaContainer> {
        let path = format!(
            "/video/:/transcode/universal/decision?{}",
            self.transcode_query(spec)
        );
        let session_header = format!("X-Plex-Session-Identifier: {}", spec.session);
        self.get_json_with_headers(&path, &[&session_header])
    }

    /// The delivery-matched stream target for `spec` — same params as the registering decision.
    pub fn transcode_start_url(&self, spec: &TranscodeSpec) -> StreamUrl {
        let endpoint = match spec.delivery {
            TranscodeDelivery::ProgressiveMkv => "start.mkv",
            TranscodeDelivery::FixedHls { .. } => "start.m3u8",
        };
        let path = format!(
            "/video/:/transcode/universal/{endpoint}?{}",
            self.transcode_query(spec)
        );
        StreamUrl {
            origin: self.origin.clone(),
            path: self.with_token(&path),
        }
    }

    /// GET /video/:/transcode/universal/stop — free the server-side encoder for `session`.
    /// Returns whether the request reached the server and came back accepted.
    ///
    /// Reported rather than discarded, for the reason [`Client::get_ok`] exists: this is a
    /// body-less WRITE whose caller is off the main thread (`route::scrobble_stop`'s worker), and
    /// the bool is the only thing that can say it landed. What a silently lost stop leaves behind is
    /// specific — the server keeps ENCODING into a session nothing will read again, the leaked
    /// server-side encoder `docs/parity-gaps.md` names on the retranscode path. Like
    /// [`super::library::Client::scrobble`], it does not tell a 200 from a 404: `get_ok` is
    /// `http_get`'s own success, which is the honest limit of a GET whose body carries nothing.
    pub fn transcode_stop(&self, session: &str) -> bool {
        let q = QueryBuilder::new("/video/:/transcode/universal/stop")
            .str("session", session)
            .str("X-Plex-Client-Identifier", &self.client_id);
        self.get_ok(&q.build())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        link_policy, Ceiling, Client, LinkPolicy, Location, TranscodeDelivery, TranscodeSpec,
    };
    use crate::devcaps::Caps;
    use crate::plex::{Origin, ServerId};

    // ---- the universal-transcoder query: who is allowed to COPY ---------------------------

    fn a_client() -> Client {
        Client::new(
            ServerId::from_raw(1),
            "mach",
            Origin::http("10.0.0.1", 32400),
            "tok",
            "cid",
        )
    }

    fn spec<'a>(remux: bool, no_video_copy: bool) -> TranscodeSpec<'a> {
        TranscodeSpec {
            rating_key: "5",
            session: "s1",
            encoder_session: "s1",
            delivery: super::TranscodeDelivery::ProgressiveMkv,
            remux,
            no_video_copy,
            audio_stream_id: 0,
            subtitle_stream_id: 0,
            offset_secs: -1,
            ceiling: None,
        }
    }

    /// **`directStream` is the server's permission to copy the video track, and the ordinary
    /// re-encode grants it.** That is right for every refusal the server's own caps can express —
    /// a source over the device's frame-size bound does not fit them, so the permission goes
    /// unused and a copy that DOES fit is a free win. This pins the default, because the bug
    /// below is invisible without it: the flag has to be the exception, not the rule.
    #[test]
    fn the_ordinary_re_encode_still_lets_the_server_copy() {
        let q = a_client().transcode_query(&spec(false, false));
        assert!(q.contains("directStream=1"), "{q}");
        assert!(q.contains("directPlay=0"), "{q}");
        assert!(q.contains("videoResolution=3840x2160"), "{q}");
        assert!(!q.contains("directStream=0"), "{q}");
    }

    /// **The Dolby Vision refusal, and the reason this flag exists.** `route::video_direct_plays`
    /// refuses a Profile 5 file because of what its PIXELS are, and no cap in this query can say
    /// so — resolution, bitrate and the profile's limitation axes are all about size. Measured
    /// against the dev PMS 2026-08-21: with `directStream=1` the server answers
    /// `Part.decision=transcode` while the VIDEO stream's own decision is `copy`, i.e. the same
    /// IPT-PQ bitstream one container down. The refusal changed the container and nothing else.
    /// Withdrawing the permission is the whole fix; `directStreamAudio=1` keeps the audio lane
    /// free, so it costs a video encoder and nothing more.
    #[test]
    fn a_pixel_refusal_withdraws_the_copy_permission_but_not_the_audio_one() {
        let q = a_client().transcode_query(&spec(false, true));
        assert!(
            q.contains("directStream=0"),
            "the server must not copy the video: {q}"
        );
        assert!(
            q.contains("directStreamAudio=1"),
            "audio may still be copied: {q}"
        );
        // the caps still ride along — they bound the encode that now has to run
        assert!(q.contains("videoResolution=3840x2160"), "{q}");
        assert!(q.contains("maxVideoBitrate=60000"), "{q}");
        // exactly ONE `directStream=` in the query. Two would be a contradiction PMS resolves by
        // position, and which position wins is not ours to assume — the first shape of this fix
        // appended `directStream=0` after the `directStream=1` the builder had already written.
        // (`directStreamAudio=` does not match this needle; it is checked above on its own.)
        assert_eq!(
            q.matches("directStream=").count(),
            1,
            "one directStream, not two: {q}"
        );
    }

    /// A REMUX is a copy by definition, so the two can never both be meant — `build_stream`
    /// derives `remux` from the same gate this flag has already failed. If one ever reaches here
    /// anyway, the remux wins and the query stays exactly what it was: the alternative is a
    /// contradiction on the wire (`directStream=0` beside a flavor whose entire content is a
    /// stream copy), which the server would resolve however it liked.
    #[test]
    fn a_remux_is_a_copy_and_the_flag_cannot_turn_it_into_something_else() {
        let plain = a_client().transcode_query(&spec(true, false));
        let flagged = a_client().transcode_query(&spec(true, true));
        assert_eq!(
            plain, flagged,
            "the flag is meaningless on the remux flavor"
        );
        assert!(plain.contains("directStream=1"), "{plain}");
        assert!(
            !plain.contains("videoResolution"),
            "a remux carries no cap, by design: {plain}"
        );
    }

    // ---- the QUALITY ceiling, on the wire -------------------------------------------------

    /// **GATE 5 — the re-encode receives the selected cap, and only the re-encode.**
    ///
    /// This is the PARAMETER half of `route::Quality`, and it is the half that is worthless on its
    /// own: by the time a `Some` gets here, `route::quality_policy` has already refused direct play
    /// and the remux for this source (`route`'s gates 3 and 4). What this pins is that the number
    /// then actually reaches the query, on BOTH axes, replacing the native-4K default rather than
    /// sitting beside it.
    ///
    /// It also pins the other end: **Auto is byte-identical to what this query has always sent**,
    /// which is the parameter half of `route`'s gate 1. A ceiling that leaked a default would be a
    /// change for every transcode in the app.
    #[test]
    fn the_re_encode_query_carries_the_selected_ceiling_and_auto_still_asks_for_native_4k() {
        // Auto — the `None` path. Byte for byte the pre-ceiling literals.
        let auto = a_client().transcode_query(&spec(false, false));
        assert!(auto.contains("videoResolution=3840x2160"), "{auto}");
        assert!(auto.contains("maxVideoBitrate=60000"), "{auto}");

        // A rung: 720p at 4 Mbps. Both axes move, and the old values are GONE — a query carrying
        // both would be a contradiction PMS resolves by position, which is not ours to assume.
        let mut s = spec(false, false);
        s.ceiling = Some(Ceiling {
            max_kbps: 4000,
            max_w: 1280,
            max_h: 720,
        });
        let capped = a_client().transcode_query(&s);
        assert!(capped.contains("videoResolution=1280x720"), "{capped}");
        assert!(capped.contains("maxVideoBitrate=4000"), "{capped}");
        assert!(
            !capped.contains("3840x2160"),
            "the native-4K default must be replaced, not joined: {capped}"
        );
        assert!(!capped.contains("60000"), "{capped}");
        assert_eq!(
            capped.matches("maxVideoBitrate=").count(),
            1,
            "one cap, not two: {capped}"
        );
        assert_eq!(
            capped.matches("videoResolution=").count(),
            1,
            "one resolution, not two: {capped}"
        );

        // …and everything else about the query is untouched by the ceiling: the two differ in
        // exactly the two params above.
        assert_eq!(
            auto.replace("videoResolution=3840x2160", "R")
                .replace("maxVideoBitrate=60000", "B"),
            capped
                .replace("videoResolution=1280x720", "R")
                .replace("maxVideoBitrate=4000", "B"),
            "the ceiling changed something other than the two params it is allowed to"
        );

        // A REMUX still sends no cap WITH a ceiling set, and that is correct rather than a hole:
        // `quality_policy` admits a remux only for a source it measured UNDER the ceiling, so
        // those bytes are already inside the bound — and a cap here would force the very
        // re-encode the remux exists to avoid.
        let mut r = spec(true, false);
        r.ceiling = Some(Ceiling {
            max_kbps: 4000,
            max_w: 1280,
            max_h: 720,
        });
        let remuxed = a_client().transcode_query(&r);
        assert!(
            !remuxed.contains("maxVideoBitrate"),
            "a remux carries no cap, ceiling or not: {remuxed}"
        );
        assert!(!remuxed.contains("videoResolution"), "{remuxed}");
        assert_eq!(
            remuxed,
            a_client().transcode_query(&spec(true, false)),
            "the ceiling is inert on a remux"
        );
    }

    /// The four coupled HLS choices are one typed delivery: protocol, fixed-session controls,
    /// capability target and start endpoint. A half-converted request is worse than a refusal —
    /// PMS can answer it with a valid container the selected demux path cannot consume.
    #[test]
    fn fixed_hls_is_one_coherent_wire_contract() {
        let mut s = spec(false, false);
        s.delivery = TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.ceiling = Some(Ceiling {
            max_kbps: 720,
            max_w: 854,
            max_h: 480,
        });

        let c = a_client();
        let q = c.transcode_query(&s);
        for required in [
            "protocol=hls",
            "directStream=0",
            "directStreamAudio=0",
            "videoResolution=854x480",
            "videoBitrate=720",
            "maxVideoBitrate=720",
            "peakBitrate=720",
            "autoAdjustQuality=0",
            "secondsPerSegment=2",
        ] {
            assert!(q.contains(required), "missing {required}: {q}");
        }
        assert!(!q.contains("protocol=http"), "one protocol only: {q}");
        assert!(c.transcode_start_url(&s).to_url().contains("/start.m3u8?"));

        let profile = super::profile_for_delivery(
            &Caps {
                hevc: true,
                hevc_max: (4096, 2176),
                vp9: true,
                audio: "aac,ac3,eac3".into(),
            },
            s.delivery,
        );
        let target = target_of(&profile);
        assert!(target.contains("protocol=hls"), "{target}");
        assert!(target.contains("container=mpegts"), "{target}");
        assert_eq!(list_of(target, "videoCodec="), ["h264"]);
        assert_eq!(list_of(target, "audioCodec="), ["aac"]);
    }

    #[test]
    fn the_probe_builder_keeps_the_two_session_wires_explicit() {
        let mut s = spec(false, false);
        s.delivery = TranscodeDelivery::FixedHls {
            seconds_per_segment: 2,
        };
        s.session = "playback-stable";
        s.encoder_session = "encoder-next";
        let q = a_client().transcode_query(&s);
        assert!(q.contains("session=encoder-next"), "{q}");
        assert!(
            q.contains("X-Plex-Session-Identifier=playback-stable"),
            "{q}"
        );
        assert!(!q.contains("session=playback-stable"), "{q}");
        // Production deliberately couples these values per encoder: the real simultaneous-
        // session spike proved a shared X-Plex id kills the old encoder before prime completes.
        // This test remains because the redacted protocol probe must still express mismatches.
    }

    // ---- the relay policy ---------------------------------------------------------------

    /// Relay denies BOTH flavors that put the file's own bytes on the wire. Direct play is the
    /// obvious one; the remux is the one a "force a transcode" instinct leaves behind, and it is
    /// no better — it copies the codecs and its query carries no cap by design, so it is the same
    /// 31 Mbit/s down the same 2 Mbit/s tunnel. What survives is the re-encode, where the server
    /// is the one choosing the rate.
    #[test]
    fn a_relay_link_forbids_both_flavors_that_ship_the_file_at_its_own_rate() {
        let p = link_policy(Some(Location::Relay));
        assert!(!p.direct_play, "a relay cannot carry the source file");
        assert!(
            !p.remux,
            "a remux is the same bytes at the same rate, one layer down"
        );
    }

    /// Every other tier — and a link nobody has described — must change nothing at all. This is
    /// the assertion that keeps the policy free for the single-server LAN install: `Client::link`
    /// is `None` on every play today, so a wrong answer here would be a wrong answer for
    /// everybody, on a code path they all take.
    #[test]
    fn every_other_tier_and_an_unknown_link_leave_playback_alone() {
        for l in [Some(Location::Local), Some(Location::Remote), None] {
            assert_eq!(
                link_policy(l),
                LinkPolicy::UNRESTRICTED,
                "link {l:?} must restrict nothing"
            );
        }
    }

    /// The policy's input is the tier of the connection that WON, so grade it from the other end:
    /// a server whose only advertised address is a relay (a share behind CGNAT — the case relay
    /// exists for) can be reached one way, and that way forces the transcode branch. This pins
    /// the join between `probe`'s ranking and this policy: they must speak the same `Location`,
    /// and the last-ranked tier must be the restricted one.
    #[test]
    fn a_server_reachable_only_by_relay_forces_the_transcode_branch() {
        let res: super::super::account::Resource = serde_json::from_str(
            r#"{"name":"cgnat-share","clientIdentifier":"cccc3333","provides":"server","owned":false,
                "connections":[{"protocol":"https","address":"plex-relay.example.net","port":8443,
                 "uri":"https://plex-relay.example.net:8443","local":false,"relay":true}]}"#,
        )
        .expect("fixture parses");

        let cs = super::super::probe::candidates(&res);
        assert_eq!(cs.len(), 1, "a relay gets no plain-http twin: {cs:#?}");
        assert_eq!(cs[0].location, Location::Relay);

        let p = link_policy(Some(cs[0].location));
        assert_eq!(
            p,
            LinkPolicy {
                direct_play: false,
                remux: false
            }
        );
    }

    // ---- the capability profile ----------------------------------------------------------

    /// Split a codec list out of the given profile segment ("add-direct-play-profile…" or
    /// "add-transcode-target…") — shared by every derivation test below.
    fn list_of(segment: &str, key: &str) -> Vec<String> {
        let v = segment
            .split(key)
            .nth(1)
            .expect(key)
            .split('&')
            .next()
            .unwrap_or("");
        v.trim_end_matches(')')
            .split(',')
            .map(str::to_string)
            .collect()
    }
    fn target_of(p: &str) -> &str {
        p.split("add-transcode-target")
            .nth(1)
            .expect("profile declares a transcode target")
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
        let p = super::profile_extra(super::TranscodeDelivery::ProgressiveMkv);
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
        let caps = Caps {
            hevc: false,
            hevc_max: (1920, 1088),
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
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
            Caps {
                hevc: false,
                hevc_max: (1920, 1088),
                vp9: false,
                audio: "aac".into(),
            },
        ] {
            let p = super::profile_for(&caps);
            let video = list_of(target_of(&p), "videoCodec=");
            assert_eq!(
                video.first().map(String::as_str),
                Some(caps.encode_vcodec())
            );
        }
    }

    /// The audio lists follow the caps subset on BOTH sides too: a device table without EAC3/AC3
    /// must not advertise them for copy (direct-stream would hand the panel a stream it cannot
    /// decode), and the transcode chain keeps its ac3-preferred ORDER for whatever remains.
    #[test]
    fn the_audio_lists_are_the_caps_subset_in_both_profiles() {
        let caps = Caps {
            hevc: true,
            hevc_max: (3840, 2176),
            vp9: true,
            audio: "aac".into(),
        };
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
