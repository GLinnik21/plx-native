//! The `plxnative-playurl` trigger: a stream to play and the Load payload declaration to play it
//! with, parsed beside the one module that acts on it ([`super::engine`]'s `start_bufferfeed`).
//!
//! It is a TYPED dev trigger. Its value is a JSON object whose decision half is a
//! [`plx_data::metadata::Dovi`], so it can live neither with the trigger primitives (`plx_base::devtrig`
//! is a base-layer module and names no application type) nor in the application's `dev` (the application
//! layer, which the player may not name). It is parsed here from `plx_base::devtrig::read`, and a
//! release build still folds it away at COMPILE time exactly as before: `read` is `None` without
//! the `devtriggers` feature, so [`playurl`] answers `None` and no declaration can be injected.

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
/// as whatever the route happened to hold — in a fresh boot, the empty string, which falls through
/// [`crate::player::engine`]'s `_ =>` arm to `"AC3"` and an H264 payload. The declaration is
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
    /// The Load payload's video codec: `"hevc"` selects the H265 payload, anything else H264.
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
    /// [`HlsActuatorCatalog::limited_to`](crate::abr::HlsActuatorCatalog::limited_to), which makes
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

/// The four DV fields the Load payload actually decides on — [`plx_data::metadata::Dovi`]'s
/// decision half. The three descriptive fields (level, version, bl/rpu present) are read by the
/// tracks panel and by nothing on the playback path, so this trigger does not carry them.
#[derive(serde::Deserialize, Clone, Copy, Default)]
pub struct PlayDovi {
    /// 5 / 7 / 8. **Zero means no Dolby Vision at all** — it is what drives `present` below,
    /// rather than a separate flag that could disagree with it.
    #[serde(default)]
    pub profile: i64,
    /// `DOVIBLCompatID` — 0 none (P5) / 1 HDR10 / 2 SDR / 4 HLG.
    #[serde(default)]
    pub bl_compat: i64,
    /// An enhancement layer is present (P7).
    #[serde(default)]
    pub el_present: bool,
}

impl PlayDovi {
    /// The engine-facing record. `present` is DERIVED from a non-zero profile rather than carried
    /// separately: two fields that can disagree is a way to declare "Dolby Vision, profile 0",
    /// which is not a thing, and the harness would have to keep them in step by hand in every case.
    pub fn to_dovi(self) -> plx_data::metadata::Dovi {
        plx_data::metadata::Dovi {
            present: self.profile > 0,
            profile: self.profile,
            bl_compat: self.bl_compat,
            el_present: self.el_present,
            ..plx_data::metadata::Dovi::NONE
        }
    }
}

/// Parse the `playurl` trigger's content. Pure, so the host suite can pin it.
///
/// An `Err` rather than a defaulted object on malformed input, for the application's `dev`'s `parse_servers`' reason: a
/// run whose declaration was silently dropped grades as "the payload is wrong", when the fault is
/// a typo in the harness. An empty `url` is an `Err` too — an all-defaults object would send the
/// engine looking for `plxnative-url` instead and the case would play something else entirely.
#[cfg(any(feature = "devtriggers", test))]
fn parse_playurl(s: &str) -> Result<PlayUrl, String> {
    let p: PlayUrl = serde_json::from_str(s).map_err(|e| e.to_string())?;
    if p.url.is_empty() {
        return Err("no `url`".to_string());
    }
    Ok(p)
}

/// This boot's URL-and-declaration, if one was armed — `/tmp/plxnative-playurl`.
///
/// `None` = not armed; `Some(Err)` = armed but unreadable, which the caller logs.
///
/// **Not memoized**, unlike the application's `dev::servers`: every `start_bufferfeed` re-reads it, because a seek that
/// escalates to a full reload tears the engine down and builds the payload again, and a
/// declaration that applied only to the first `Load` would make the second one silently wrong.
/// `servers` is memoized for the opposite reason — credentials are a property of the boot.
#[cfg(feature = "devtriggers")]
pub fn playurl() -> Option<Result<PlayUrl, String>> {
    plx_base::devtrig::read("playurl").map(|s| parse_playurl(&s))
}
#[cfg(not(feature = "devtriggers"))]
pub fn playurl() -> Option<Result<PlayUrl, String>> {
    None
}

#[cfg(test)]
mod tests {
    /// The payload `tests/run.py` writes, verbatim. Pinned as a literal for `dev::parse_servers`'
    /// reason: nothing links the two languages at build time, so if this has to change, the
    /// harness's writer changes with it.
    #[test]
    fn the_harness_payload_parses_to_the_declaration_it_names() {
        let p = super::parse_playurl(
            r#"{"url":"http://192.0.2.10:8020/pipe_hevc_eac3_4k_dovi_p8.mkv","vcodec":"hevc",
                "acodec":"eac3","fps":23.976,
                "dovi":{"profile":8,"bl_compat":1,"el_present":false},"atmos":false}"#,
        )
        .unwrap();
        assert_eq!(
            p.url,
            "http://192.0.2.10:8020/pipe_hevc_eac3_4k_dovi_p8.mkv"
        );
        assert_eq!((p.vcodec.as_str(), p.acodec.as_str()), ("hevc", "eac3"));
        assert!((p.fps - 23.976).abs() < 1e-9);
        assert!(!p.atmos);
        let dv = p.dovi.to_dovi();
        assert!(dv.present, "profile 8 must derive present=true");
        assert_eq!((dv.profile, dv.bl_compat, dv.el_present), (8, 1, false));
    }

    /// The baseline case declares two fields and omits the rest, which must mean "no Dolby
    /// anything" — not a parse failure and not an inherited value.
    #[test]
    fn omitted_fields_default_to_a_silent_declaration() {
        let p = super::parse_playurl(
            r#"{"url":"http://10.0.0.2:8020/a.mkv","vcodec":"h264","acodec":"ac3"}"#,
        )
        .unwrap();
        assert_eq!(p.fps, 0.0);
        assert!(!p.atmos);
        let dv = p.dovi.to_dovi();
        assert!(!dv.present);
        assert_eq!(
            dv,
            plx_data::metadata::Dovi::NONE,
            "an absent dovi node must be silence itself"
        );
    }

    /// The synthetic Auto case carries only public fixture coordinates and a declared source
    /// rate. Keep that cross-language seam pinned separately from the ordinary declaration: a
    /// parser that silently defaults either field would play Original forever and make the TV
    /// network-profile case grade the wrong path.
    #[test]
    fn the_auto_fixture_fields_reach_the_runtime_verbatim() {
        let p = super::parse_playurl(
            r#"{"url":"http://192.0.2.10:8020/original.mp4","vcodec":"h264","acodec":"aac",
                "auto_source_kbps":8000,"auto_hls_base":"http://192.0.2.10:8020/__abr"}"#,
        )
        .unwrap();
        assert_eq!(p.auto_source_kbps, 8_000);
        assert_eq!(p.auto_hls_base, "http://192.0.2.10:8020/__abr");
    }

    /// `present` is DERIVED, so it cannot disagree with the profile in either direction.
    #[test]
    fn dovi_presence_follows_the_profile() {
        let none = super::PlayDovi {
            profile: 0,
            bl_compat: 1,
            el_present: true,
        }
        .to_dovi();
        assert!(
            !none.present,
            "profile 0 is not Dolby Vision whatever else is set"
        );
        let p7 = super::PlayDovi {
            profile: 7,
            bl_compat: 6,
            el_present: true,
        }
        .to_dovi();
        assert!(p7.present);
        assert_eq!((p7.profile, p7.bl_compat, p7.el_present), (7, 6, true));
    }

    /// An empty `url` must be an Err, not an all-defaults object: on `Ok` the engine would take
    /// the empty URL, fall through to `plxnative-url` or a local sample, and PLAY SOMETHING ELSE —
    /// a case grading a stream it was never pointed at. Same class as `dev::parse_servers`' untagged
    /// ordering trap, in different clothes.
    #[test]
    fn an_empty_url_is_refused_rather_than_defaulted() {
        assert!(super::parse_playurl(r#"{"vcodec":"hevc"}"#).is_err());
        assert!(super::parse_playurl(r#"{"url":""}"#).is_err());
        assert!(super::parse_playurl("").is_err());
        assert!(super::parse_playurl("not json at all").is_err());
    }

    /// The harness writes triggers through a single-quoted `printf` with NO escaping
    /// (`tests/run.py::apply_triggers`), so an apostrophe anywhere in the payload would end the
    /// quoting and hand the rest to the TV's shell. JSON has no apostrophe in its syntax; this
    /// pins that the fields we generate carry none either.
    #[test]
    fn the_harness_payload_carries_no_apostrophe() {
        let payload = r#"{"url":"http://192.0.2.10:8020/pipe_h264_ac3_1080p.mkv","vcodec":"h264","acodec":"ac3","fps":24.0}"#;
        assert!(
            !payload.contains('\''),
            "would break apply_triggers' single-quoted printf"
        );
        assert!(super::parse_playurl(payload).is_ok());
    }
}
