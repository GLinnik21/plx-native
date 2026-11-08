//! The `plxnative-playurl` trigger: a stream to play and the Load payload declaration to play it
//! with. The value ([`PlayUrl`], [`PlayDovi`]) is the contract's plain data
//! (`plx_playback_contract::playurl`); the parse is here, and the application hands [`playurl`] to
//! the player adapter as its `PlayUrlSource`.
//!
//! It is a TYPED dev trigger. Its value is a JSON object, so it can live neither with the trigger
//! primitives (`plx_base::devtrig` is a base-layer module and names no application type) nor in the
//! application's `dev` (the application layer, which the player may not name). It is parsed here
//! from `plx_base::devtrig::read`, and a release build still folds it away at COMPILE time exactly
//! as before: `read` is `None` without the `devtriggers` feature, so [`playurl`] answers `None` and
//! no declaration can be injected. The conversion of the DV record to the data layer's `Dovi` stays
//! with media's `feed_open`, the one caller, because this layer may not name `plx_data`.

pub use plx_playback_contract::playurl::{PlayDovi, PlayUrl};

/// Parse the `playurl` trigger's content. Pure, so the host suite can pin it.
///
/// An `Err` rather than a defaulted object on malformed input, for the application's `dev`'s `parse_servers`' reason: a
/// run whose declaration was silently dropped grades as "the payload is wrong", when the fault is
/// a typo in the harness. An empty `url` is an `Err` too — an all-defaults object would send the
/// engine looking for `plxnative-url` instead and the case would play something else entirely.
#[cfg(any(feature = "devtriggers", test))]
pub fn parse_playurl(s: &str) -> Result<PlayUrl, String> {
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
        let dv = p.dovi;
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
        let dv = p.dovi;
        assert_eq!(
            (dv.profile, dv.bl_compat, dv.el_present),
            (0, 0, false),
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
