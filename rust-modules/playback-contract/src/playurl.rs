//! The `plxnative-playurl` trigger's value: a stream to play and the Load payload declaration to
//! play it with. Plain data, so the player (which reads it at a start) and the Feed's parser of the
//! trigger file (`plx_play_feed::playurl`) share one type without either naming the other; the
//! application composes the reader ([`PlayUrlSource`]) into the adapter.
//!
//! The decision half of the DV object is a plain record here ([`PlayDovi`]); the conversion to the
//! data layer's `Dovi` stays with media's `feed_open`, the one caller, because this layer may not
//! name `plx_data`.

/// A stream to play and **the Load payload declaration to play it with**, with no library item
/// behind it — `/tmp/plxnative-playurl`, the player-PIPELINE test tier's one entry point.
///
/// This is the trigger that makes the pipeline testable without Plex. `plxnative-url` already
/// hands the engine a URL, and everything downstream of it — `stream.rs`, `ff.rs`, `aq.rs`, the
/// pump's `Feed()`, the ACB bind — is byte-identical to a real playback. What it CANNOT do is say
/// what the stream *is*: the Starfish `Load` payload takes its codecs from `route::stream_vcodec`
/// / `stream_acodec`, and its Dolby nodes from `stream_dovi` / `stream_immersive`, all five of
/// which are normally installed together by `route::apply_plan` from a PMS decision and replaced
/// together by later route transitions. So a URL-fed 4K HEVC file was declared to the television
/// as whatever the route happened to hold — in a fresh boot, the empty string, which
/// `plx_play_feed::audio_payload_codec`'s `_ => None` arm refuses as an unsupported audio codec (it
/// once fell through to `"AC3"`; `LoadVideo::of_stream` deliberately maps it to H264). The declaration is
/// precisely what governs HEVC-vs-H264 payload selection, the `"AC3 PLUS"` naming trap, and both
/// Dolby nodes, so a tier that cannot set it cannot test them.
///
/// JSON, whole-file, one object. Chosen over a `key=value` line for three reasons: the DV node is
/// nested, `serde_json` is already a dependency and the application's `dev::DevServer` is the established precedent for
/// exactly this shape, and JSON contains no apostrophes — which matters because `tests/run.py`
/// writes triggers through a single-quoted `printf` with no escaping.
///
/// ```jsonc
/// {"url":"http://192.0.2.10:8020/pipe_hevc_eac3_4k_dovi_p8.mkv",
///  "vcodec":"hevc", "acodec":"eac3", "fps":23.976,
///  "dovi":{"profile":8,"bl_compat":1,"el_present":false},
///  "atmos":false}
/// ```
///
/// Deliberately **not** `Debug`, for the application's `dev::DevServer`'s reason one step removed: `url` is a free
/// string, and while the pipeline tier's own URLs carry no credentials, the field is the same
/// shape as `route::url()` — which for a real playback carries `X-Plex-Token` in its query. A
/// derived `Debug` is how that reaches a log the day someone points this trigger at a PMS part.
#[derive(serde::Deserialize, Clone)]
pub struct PlayUrl {
    /// `http://<dotted-quad>:<port>/<path>`. **A dotted quad, not a hostname** — `stream.rs` does
    /// no DNS on this path, so a name is a flat failure to open with nothing to read it by.
    #[serde(default)]
    pub url: String,
    /// The Load payload's video codec, matched case-insensitively: `"h264"` or empty selects H264,
    /// `"hevc"` / `"h265"` H265, `"av1"` / `"vp9"` the AV1 / VP9 ones (only on a set whose table lists
    /// that decoder; elsewhere the Load is refused). Any other value refuses the Load with a log line
    /// naming the codec; it is never declared as H264.
    #[serde(default)]
    pub vcodec: String,
    /// The Load payload's audio codec, in FFmpeg's spelling (`"eac3"`, not `"AC3 PLUS"`) — the
    /// engine does the LG-side renaming, which is the trap this tier exists to keep testing.
    #[serde(default)]
    pub acodec: String,
    /// Source frame rate for the Load `esInfo`; 0 omits it, exactly as a transcode does.
    #[serde(default)]
    pub fps: f64,
    /// Dolby Vision layering, for the payload's `contents.DolbyHdrInfo` node. Absent = none.
    #[serde(default)]
    pub dovi: PlayDovi,
    /// Dolby Atmos, for the payload's `contents.immersive` node.
    #[serde(default)]
    pub atmos: bool,
    /// Pipeline-tier Auto watchdog seam: whole Original wire bitrate. Zero leaves the ordinary
    /// Plex-free one-shot playback unchanged.
    #[serde(default)]
    pub auto_source_kbps: u32,
    /// Same-origin fixture HLS root used after the synthetic Original becomes unsustainable.
    /// Present only in debug/test artifacts; production playback obtains replacement URLs from
    /// PMS through `HlsAbrControl`.
    #[serde(default)]
    pub auto_hls_base: String,
    /// **Start in HLS instead of arriving there through a starvation.** The pipeline tier's ABR
    /// cases exist to exercise the HLS controller, and until 2026-08-27 their only way in was to
    /// declare an Original source rate no link could carry (900 000 kbps) and let the starvation
    /// horizon fire. That worked only because the horizon fired without checking whether the
    /// reserve was actually draining — on an unshaped link it was FILLING — so the entry depended
    /// on a defect, and it stopped working the moment the defect was fixed
    /// (`docs/measurements/local-original-blind.md`, `docs/measurements/orig-first-window-fallback.md`).
    /// With this set, `route::arm_auto_fixture` installs the post-fallback state directly and the
    /// controller runs from the first segment. `pipe_auto_original_slow_recover` deliberately does
    /// NOT set it: the transition is what that case grades.
    #[serde(default)]
    pub auto_start_hls: bool,
    /// **The SOURCE raster, which decides whether the 4K actuator is feasible at all.**
    ///
    /// `route::arm_auto_fixture` hardcoded `1920x1080`, and its comment gave the honest reason:
    /// an unknown source raster is treated as UNBOUNDED by
    /// `abr::HlsActuatorCatalog::limited_to`, which makes
    /// the Uhd rung feasible — and `tests/serve_fixtures.py` served no 22000 rung, so a candidate
    /// there would 404 and read on the television as a rejected encoder. That is a fixture gap
    /// standing in for a policy, and it is what kept the plan's I9 blocked: with every
    /// `auto_network` case pinned to a 1080p source, `admits` deletes Uhd and the two entries the
    /// production table calls empirical are the two no case can reach.
    ///
    /// `[w, h]`. Absent or malformed keeps the 1080p default, so every existing case is unchanged
    /// by construction. The server now answers 22000 with a real 4K clip, so declaring 4K here
    /// selects a rung that exists rather than one that 404s.
    #[serde(default)]
    pub source_raster: Option<[u16; 2]>,
}

/// The four DV fields the Load payload actually decides on — the data layer's `Dovi` decision half.
/// The three descriptive fields (level, version, bl/rpu present) are read by the
/// tracks panel and by nothing on the playback path, so this trigger does not carry them.
#[derive(serde::Deserialize, Clone, Copy, Default)]
pub struct PlayDovi {
    /// 5 / 7 / 8. **Zero means no Dolby Vision at all** — it is what drives the data
    /// layer's `present` (media's `feed_open`), rather than a separate flag that could disagree with it.
    #[serde(default)]
    pub profile: i64,
    /// `DOVIBLCompatID` — 0 none (P5) / 1 HDR10 / 2 SDR / 4 HLG.
    #[serde(default)]
    pub bl_compat: i64,
    /// An enhancement layer is present (P7).
    #[serde(default)]
    pub el_present: bool,
}

/// What the application hands the player to read the trigger with: this boot's URL-and-declaration,
/// if one was armed. `None` = not armed; `Some(Err)` = armed but unreadable, which the caller logs.
/// Not memoized by the reader: a seek that escalates to a full reload builds the payload again and
/// must re-apply the declaration.
pub type PlayUrlSource = fn() -> Option<Result<PlayUrl, String>>;
