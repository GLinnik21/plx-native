//! HDR10 for a VP9 stream, signalled to the display by rewriting the `sourceInfo` envelope.
//!
//! **The problem, measured on the dev set (webOS 4.5).** The same BT.2020/PQ test pattern, once
//! as HEVC HDR10 and once as VP9 tagged PQ, left the television in HDR for the first and in SDR
//! for the second (the dashboard's `picture.dynamicRange`, and a visibly washed-out picture). The
//! pipeline's own `sourceInfo` envelope shows why. For HEVC the decoder reports the stream's HDR
//! and the envelope carries it:
//!
//! ```text
//! "hdrType":"HDR10", "mediaSei":{displayPrimariesX0..Y2, whitePointX/Y, min/maxDisplayMasteringLuminance,
//! maxContentLightLevel, maxPicAverageLightLevel}, "mediaVui":{transferCharacteristics:16, colorPrimaries:9, matrixCoeffs:9}
//! ```
//!
//! For VP9 it says `"hdrType":"none"` and a `mediaVui` of all 2 ("unspecified"), because a VP9
//! bitstream has nowhere to say "PQ": the transfer function lives in the container. The app
//! forwards that envelope to ACB verbatim (`acb_send_video_data`), and ACB is what puts the panel
//! in HDR, so the fix is app-side: when the demuxer found PQ in the container, say so in the
//! envelope before it is sent. No Load key carries any of this (the only HDR key the firmware reads
//! is Dolby-only, `contents.DolbyHdrInfo`), and there is no `setHdrInfo` on this firmware.
//!
//! **Reach:** the envelope goes out through ACB (`acb_send_video_data`), which exists on the webOS 4
//! family only. On webOS 5+ there is no ACB (`g_acb` is NULL and the send is a no-op), so this does
//! nothing there; how a VP9 PQ stream reaches HDR on those sets is not known. Measured on the dev set
//! (4.x) only.
//!
//! Only HDR10 (PQ, transfer 16) is handled. HLG is not: the envelope's name for it was never
//! observed, and a guessed string is a silent no-op or a rejected envelope.
//!
//! Units are the SEI's own, which is what the television's HEVC envelope shows: chromaticity in
//! 1/50000, luminance in 1/10000 cd/m². The order of the three primaries is **G, B, R** (as in an
//! HEVC mastering-display SEI), not FFmpeg's R, G, B; the HEVC envelope above has G's x in
//! `displayPrimariesX0`.

/// A rational as FFmpeg stores one (`AVRational`).
pub type Rat = (i32, i32);

/// `AVMasteringDisplayMetadata`, read out of the stream's coded side data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mastering {
    /// `display_primaries[3][2]`: R, G, B, each (x, y).
    pub primaries: [[Rat; 2]; 3],
    pub white_point: [Rat; 2],
    pub min_luminance: Rat,
    pub max_luminance: Rat,
    pub has_primaries: bool,
    pub has_luminance: bool,
}

/// What the container said about the picture's colour, as the demuxer read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContainerHdr {
    /// `AVCodecParameters.color_trc`, `color_primaries`, `color_space` (H.273 code points).
    pub trc: i32,
    pub pri: i32,
    pub mc: i32,
    pub mastering: Option<Mastering>,
    /// (MaxCLL, MaxFALL), from `AVContentLightMetadata`.
    pub cll: Option<(u32, u32)>,
}

/// `AVCOL_TRC_SMPTE2084`: the PQ transfer function, i.e. HDR10 when carried with BT.2020.
pub const TRC_PQ: i32 = 16;

fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn rat(b: &[u8], at: usize) -> Option<Rat> {
    Some((le_i32(b, at)?, le_i32(b, at + 4)?))
}

/// An `AVMasteringDisplayMetadata` (88 bytes on 32-bit ARM and on the 64-bit host alike: eight
/// `AVRational` pairs then two ints). `None` when the payload is short.
pub fn parse_mastering(b: &[u8]) -> Option<Mastering> {
    if b.len() < 88 {
        return None;
    }
    let mut primaries = [[(0, 0); 2]; 3];
    for (i, p) in primaries.iter_mut().enumerate() {
        p[0] = rat(b, i * 16)?;
        p[1] = rat(b, i * 16 + 8)?;
    }
    Some(Mastering {
        primaries,
        white_point: [rat(b, 48)?, rat(b, 56)?],
        min_luminance: rat(b, 64)?,
        max_luminance: rat(b, 72)?,
        has_primaries: le_i32(b, 80)? != 0,
        has_luminance: le_i32(b, 84)? != 0,
    })
}

/// An `AVContentLightMetadata` (two unsigned ints: MaxCLL, MaxFALL).
pub fn parse_cll(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 8 {
        return None;
    }
    Some((
        u32::from_le_bytes(b[0..4].try_into().ok()?),
        u32::from_le_bytes(b[4..8].try_into().ok()?),
    ))
}

/// `num/den * scale`, rounded; `None` for a zero denominator or a value that does not fit.
fn scaled(r: Rat, scale: f64) -> Option<i64> {
    if r.1 == 0 {
        return None;
    }
    let v = (f64::from(r.0) / f64::from(r.1) * scale).round();
    (v.is_finite() && v.abs() < 2e9).then_some(v as i64)
}

/// The envelope's `mediaSei` object, or `None` unless the container stated the whole mastering
/// display (primaries and luminance). A partial block is left out: the television then uses its
/// defaults, which is a known state, instead of a half-filled one that nobody has seen it accept.
fn media_sei(m: &Mastering, cll: Option<(u32, u32)>) -> Option<serde_json::Value> {
    if !m.has_primaries || !m.has_luminance {
        return None;
    }
    // G, B, R: the order of an HEVC mastering-display SEI, which is what the TV's own envelope uses.
    let order = [1usize, 2, 0];
    let mut o = serde_json::Map::new();
    for (i, &p) in order.iter().enumerate() {
        o.insert(format!("displayPrimariesX{i}"), scaled(m.primaries[p][0], 50000.0)?.into());
        o.insert(format!("displayPrimariesY{i}"), scaled(m.primaries[p][1], 50000.0)?.into());
    }
    o.insert("whitePointX".into(), scaled(m.white_point[0], 50000.0)?.into());
    o.insert("whitePointY".into(), scaled(m.white_point[1], 50000.0)?.into());
    o.insert("minDisplayMasteringLuminance".into(), scaled(m.min_luminance, 10000.0)?.into());
    o.insert("maxDisplayMasteringLuminance".into(), scaled(m.max_luminance, 10000.0)?.into());
    let (cll_max, cll_avg) = cll.unwrap_or((0, 0));
    o.insert("maxContentLightLevel".into(), cll_max.into());
    o.insert("maxPicAverageLightLevel".into(), cll_avg.into());
    Some(o.into())
}

/// Rewrite a captured `sourceInfo` envelope (the bytes the pipeline produced, with their trailing
/// NUL) so it declares HDR10, when the container says the picture is PQ and the pipeline said
/// nothing. `None` leaves the envelope as it was: not PQ, already declaring an HDR type (the
/// television's own word wins), or not an envelope of the expected shape.
pub fn rewrite_source_info(envelope: &[u8], h: &ContainerHdr) -> Option<Vec<u8>> {
    if h.trc != TRC_PQ {
        return None;
    }
    let text = std::str::from_utf8(envelope).ok()?.trim_end_matches('\0');
    let mut v: serde_json::Value = serde_json::from_str(text).ok()?;
    let video = v.get_mut("video")?.as_object_mut()?;
    match video.get("hdrType").and_then(|t| t.as_str()) {
        Some("none") | None => {}
        Some(_) => return None,
    }
    video.insert("hdrType".into(), "HDR10".into());
    video.insert(
        "mediaVui".into(),
        serde_json::json!({
            "transferCharacteristics": h.trc,
            "colorPrimaries": h.pri,
            "matrixCoeffs": h.mc,
        }),
    );
    match h.mastering.as_ref().and_then(|m| media_sei(m, h.cll)) {
        Some(sei) => {
            video.insert("mediaSei".into(), sei);
        }
        None => {
            video.remove("mediaSei");
        }
    }
    let mut out = serde_json::to_vec(&v).ok()?;
    out.push(0);
    Some(out)
}

/// The one line the event log gets when the envelope was rewritten.
pub fn log_line(h: &ContainerHdr) -> String {
    let sei = h.mastering.as_ref().is_some_and(|m| m.has_primaries && m.has_luminance);
    let cll = match h.cll {
        Some((a, b)) => format!("{a}/{b}"),
        None => "-".to_string(),
    };
    format!(
        "hdr: vp9 sourceInfo hdrType none -> HDR10 (container trc={} pri={} mc={} sei={sei} cll={cll})",
        h.trc, h.pri, h.mc
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope the television produced for the dev set's VP9 PQ test file (captured on the
    /// device, 2026-10-11), minus the per-run `context`.
    const VP9_SDR: &str = r#"{"context":"_x","content":"movie","video":{"frameRate":0,"scanType":"VIDEO_PROGRESSIVE","width":1920,"height":1080,"bitRate":0,"adaptive":true,"path":"network","pixelAspectRatio":{"width":1,"height":1},"afd":16,"rotation":0,"data3D":{"originalPattern":"2d","currentPattern":"2d","typeLR":"LR"},"hfr":false,"hdrType":"none","mediaVui":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2}}}"#;
    const HEVC_HDR10: &str = r#"{"context":"_x","content":"movie","video":{"hdrType":"HDR10","mediaVui":{"transferCharacteristics":16,"colorPrimaries":9,"matrixCoeffs":9}}}"#;

    fn r(n: i32, d: i32) -> Rat {
        (n, d)
    }

    /// x265's `G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)` as FFmpeg
    /// reads it back (rationals over 50000 / 10000).
    fn mastering() -> Mastering {
        Mastering {
            primaries: [
                [r(34000, 50000), r(16000, 50000)], // R
                [r(13250, 50000), r(34500, 50000)], // G
                [r(7500, 50000), r(3000, 50000)],   // B
            ],
            white_point: [r(15635, 50000), r(16450, 50000)],
            min_luminance: r(1, 10000),
            max_luminance: r(10_000_000, 10000),
            has_primaries: true,
            has_luminance: true,
        }
    }

    fn pq() -> ContainerHdr {
        ContainerHdr { trc: 16, pri: 9, mc: 9, mastering: None, cll: None }
    }

    fn with_nul(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn parsed(b: &[u8]) -> serde_json::Value {
        assert_eq!(*b.last().unwrap(), 0, "the rewritten envelope keeps its NUL");
        serde_json::from_slice(&b[..b.len() - 1]).unwrap()
    }

    #[test]
    fn a_pq_container_turns_a_none_envelope_into_hdr10() {
        let out = rewrite_source_info(&with_nul(VP9_SDR), &pq()).expect("rewritten");
        let v = parsed(&out);
        assert_eq!(v["video"]["hdrType"], "HDR10");
        assert_eq!(v["video"]["mediaVui"]["transferCharacteristics"], 16);
        assert_eq!(v["video"]["mediaVui"]["colorPrimaries"], 9);
        assert_eq!(v["video"]["mediaVui"]["matrixCoeffs"], 9);
        // everything else the pipeline said survives
        assert_eq!(v["video"]["width"], 1920);
        assert_eq!(v["video"]["pixelAspectRatio"]["width"], 1);
        assert_eq!(v["context"], "_x");
        // no mastering metadata in the container: no mediaSei is invented
        assert!(v["video"].get("mediaSei").is_none());
    }

    #[test]
    fn mastering_and_light_level_become_the_sei_block_in_the_tvs_own_order_and_units() {
        let h = ContainerHdr { mastering: Some(mastering()), cll: Some((1000, 400)), ..pq() };
        let v = parsed(&rewrite_source_info(&with_nul(VP9_SDR), &h).unwrap());
        let sei = &v["video"]["mediaSei"];
        // the exact numbers the television reported for the equivalent HEVC file: G, B, R order
        assert_eq!(sei["displayPrimariesX0"], 13250);
        assert_eq!(sei["displayPrimariesY0"], 34500);
        assert_eq!(sei["displayPrimariesX1"], 7500);
        assert_eq!(sei["displayPrimariesY1"], 3000);
        assert_eq!(sei["displayPrimariesX2"], 34000);
        assert_eq!(sei["displayPrimariesY2"], 16000);
        assert_eq!(sei["whitePointX"], 15635);
        assert_eq!(sei["whitePointY"], 16450);
        assert_eq!(sei["minDisplayMasteringLuminance"], 1);
        assert_eq!(sei["maxDisplayMasteringLuminance"], 10_000_000);
        assert_eq!(sei["maxContentLightLevel"], 1000);
        assert_eq!(sei["maxPicAverageLightLevel"], 400);
    }

    #[test]
    fn a_partial_mastering_block_is_left_out_not_half_sent() {
        let mut m = mastering();
        m.has_luminance = false;
        let h = ContainerHdr { mastering: Some(m), cll: Some((1000, 400)), ..pq() };
        let v = parsed(&rewrite_source_info(&with_nul(VP9_SDR), &h).unwrap());
        assert!(v["video"].get("mediaSei").is_none());
        assert_eq!(v["video"]["hdrType"], "HDR10");
    }

    #[test]
    fn only_pq_is_rewritten() {
        for trc in [1, 2, 13, 14, 15, 18] {
            let h = ContainerHdr { trc, ..pq() };
            assert!(rewrite_source_info(&with_nul(VP9_SDR), &h).is_none(), "trc {trc}");
        }
    }

    #[test]
    fn the_televisions_own_hdr_declaration_is_never_overridden() {
        assert!(rewrite_source_info(&with_nul(HEVC_HDR10), &pq()).is_none());
    }

    #[test]
    fn a_malformed_or_foreign_envelope_is_left_alone() {
        assert!(rewrite_source_info(b"not json\0", &pq()).is_none());
        assert!(rewrite_source_info(&with_nul(r#"{"context":"x"}"#), &pq()).is_none());
        assert!(rewrite_source_info(&with_nul(r#"{"video":[]}"#), &pq()).is_none());
        assert!(rewrite_source_info(&[0xff, 0xfe, 0], &pq()).is_none());
    }

    #[test]
    fn a_missing_hdr_type_is_treated_as_none() {
        let env = r#"{"context":"x","video":{"width":1920}}"#;
        let v = parsed(&rewrite_source_info(&with_nul(env), &pq()).unwrap());
        assert_eq!(v["video"]["hdrType"], "HDR10");
    }

    /// An `AVMasteringDisplayMetadata` laid out as FFmpeg's header lays it out (88 bytes).
    #[test]
    fn the_side_data_structs_parse_at_ffmpegs_offsets() {
        let m = mastering();
        let mut b = Vec::new();
        for p in m.primaries {
            for (n, d) in p {
                b.extend_from_slice(&n.to_le_bytes());
                b.extend_from_slice(&d.to_le_bytes());
            }
        }
        for (n, d) in m.white_point.iter().chain([&m.min_luminance, &m.max_luminance]) {
            b.extend_from_slice(&n.to_le_bytes());
            b.extend_from_slice(&d.to_le_bytes());
        }
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        assert_eq!(b.len(), 88);
        assert_eq!(parse_mastering(&b), Some(m));
        assert_eq!(parse_mastering(&b[..87]), None, "a short payload is refused, not read past");
        let mut c = 1000u32.to_le_bytes().to_vec();
        c.extend_from_slice(&400u32.to_le_bytes());
        assert_eq!(parse_cll(&c), Some((1000, 400)));
        assert_eq!(parse_cll(&c[..7]), None);
    }

    #[test]
    fn a_zero_denominator_drops_the_sei_instead_of_dividing() {
        let mut m = mastering();
        m.white_point[0] = (1, 0);
        let h = ContainerHdr { mastering: Some(m), ..pq() };
        let v = parsed(&rewrite_source_info(&with_nul(VP9_SDR), &h).unwrap());
        assert!(v["video"].get("mediaSei").is_none());
    }

    #[test]
    fn the_log_line_names_what_was_sent() {
        let h = ContainerHdr { mastering: Some(mastering()), cll: Some((1000, 400)), ..pq() };
        assert_eq!(
            log_line(&h),
            "hdr: vp9 sourceInfo hdrType none -> HDR10 (container trc=16 pri=9 mc=9 sei=true cll=1000/400)"
        );
        assert!(log_line(&pq()).contains("sei=false cll=-"));
    }
}
