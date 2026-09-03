//! What this television can DECODE — the device's own codec table, read once at boot.
//!
//! # Why the app asks the device instead of knowing
//!
//! The capability profile used to be a set of constants describing the author's television: HEVC
//! yes, 4K yes, AAC/AC3/EAC3 yes. Every one of those claims is true on the 49SM9000PLA and was
//! asserted to every server as if it were true of webOS. That is the bug class issue #22 named
//! and `docs/plex-pass-audit.md` enumerates — **a claim true on the development environment,
//! asserted as universal** — and the decode-capability assertion was its last standing member
//! (the audit's closing section): a webOS set whose SoC lacks HEVC would be offered, and would
//! direct-play, streams it cannot decode, and nothing here could catch it because the only
//! device this project owns decodes everything the constants claimed.
//!
//! # Where it comes from
//!
//! `/etc/umediaserver/device_codec_capability_config.json` — published by the platform for its
//! own media stack, verified present on the dev set (webOS 4.5): per-codec rows with max
//! width/height/framerate/bitrate for video, channel counts for audio. Read once at boot, the
//! same shape as `webos::probe` (one file read, can never fail the boot, loud line either way).
//!
//! Two facts about the table shape the parse:
//!
//! * **Duplicate names are one codec.** The dev set lists BOTH `"H.265"` (4096x2304@120) and
//!   `"HEVC"` (4096x2176@60) — same decoder, two rows, different numbers. Rows naming the same
//!   codec are merged by MIN on every axis we consume, because over-claiming is the exact
//!   failure mode this module exists to end.
//! * **It cannot express bit depth.** No row carries one, so the profile's `bitDepth=10` claim
//!   stays the constant it always was (see `plex/transcoder.rs::profile_for`) — the table can
//!   neither confirm nor deny it.
//!
//! Parsed with serde_json (already a dependency of the plex layer) rather than `webos.rs`'s
//! hand scanner: this file is nested arrays of objects, not a flat string map. Any parse
//! failure — or a shape we misread — lands on [`Caps::assumed`], never on a refused boot.
//!
//! # The fallback is the old behavior, verbatim
//!
//! When the file is unreadable or unparseable, [`Caps::assumed`] restores EXACTLY the constants
//! that shipped before this module existed — the 49SM9000PLA values, the only set ever measured
//! before this module. Misreading the table must degrade to yesterday's behavior, not invent a
//! weaker television: the issue-#22 lesson cuts both ways, and a client that suddenly transcodes
//! everything on a healthy 4K panel is as wrong as one that direct-plays HEVC to a SoC without it.
use std::sync::OnceLock;

const CAPS_TABLE: &str = "/etc/umediaserver/device_codec_capability_config.json";

/// The decode-capability snapshot the playback stack derives from. `OnceLock` for the same
/// reason as `webos::Info`: written exactly once, at boot, then read on every play decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caps {
    /// The SoC decodes HEVC. Gates the `hevc` arm of route.rs's direct-play test and the
    /// hevc entries in BOTH profile codec lists (h264 needs no flag — every webOS SoC has it).
    pub hevc: bool,
    /// The video width/height upper bound — the per-axis MIN across the merged H.264 and HEVC
    /// rows, because both consumers apply one bound to EVERY codec at once: the profile's
    /// `*`-scoped limitation (a 1080p-only SoC must tell PMS 1920x1088 so 4K sources transcode
    /// down) and route.rs's local direct-play resolution gate. Taking the HEVC row alone
    /// whenever one existed — as this first shipped — assumed the dev set's shape ("HEVC is the
    /// tighter of the two decoders") as if it were universal: on a SoC with a 4K HEVC decoder
    /// beside 1080p-limited AVC (a real SoC shape) it advertised a 4K bound for h264, and PMS
    /// would direct-play 4K H.264 onto a decoder that cannot handle it — issue #22's over-claim
    /// class, reintroduced by the module built to end it. The dev set's bound is unchanged:
    /// min((4096,2304), (4096,2176)) = (4096,2176).
    pub hevc_max: (u32, u32),
    /// **Per-codec `(maxWidth, maxHeight, maxFrameRate)` as the table states them**, H.264 and HEVC
    /// respectively, duplicate rows MIN-merged per axis, `0` on any axis the table did not say.
    /// Added 2026-09-03 for ONE consumer — `engine::sink_envelope`, the Starfish Load's
    /// `adaptiveStreaming` ceiling — and clamped there only when [`measured`] is true: the assumed
    /// fallback is `(0, 0, 0)` on purpose, so an unreadable table never clamps anything. The
    /// frame rate is the axis `docs/webos10-resource-allocation.md` names as the likely
    /// discriminator on webOS 10 (the raster was not: that set's own table claims 4096x2176 for
    /// H.264 too); this module used to drop it by design, and `hevc_max` above still folds both
    /// codecs into one raster bound for the PMS profile and the direct-play gate, unchanged.
    pub h264_row: (u32, u32, u32),
    pub hevc_row: (u32, u32, u32),
    /// Diagnostic ONLY — nothing derives from it. The buffer-feed pipeline cannot feed VP9
    /// whatever the panel decodes (route.rs's decode gate explains why), but a support log that
    /// names a codec the panel decodes and the app still transcodes answers its own question.
    pub vp9: bool,
    /// The direct-playable AUDIO subset: `plex::DP_AUDIO_CODECS` (what the pipeline decodes)
    /// intersected with the table's audio rows, in `DP_AUDIO_CODECS`'s own URL form/order.
    /// This field is the one definition both consumers read — `plex::is_dp_audio` (the gate on
    /// every direct-play decision) and the profile string's audio lists — so the two cannot
    /// drift apart (the coupling `DP_AUDIO_CODECS`'s doc has always promised).
    pub audio: String,
}

impl Caps {
    /// The 49SM9000PLA values — the only set ever measured before this module. Byte-for-byte,
    /// these reproduce the profile the app has always sent (the transcoder test pins that), so
    /// an unreadable table changes nothing for any device that was working yesterday.
    pub(crate) fn assumed() -> Caps {
        Caps {
            hevc: true,
            hevc_max: (3840, 2176),
            h264_row: (0, 0, 0),
            hevc_row: (0, 0, 0),
            vp9: true,
            audio: crate::plex::DP_AUDIO_CODECS.to_string(),
        }
    }

    /// Membership test on [`Caps::audio`] — `codec` already lowercase (PMS codec ids are).
    pub(crate) fn audio_has(&self, codec: &str) -> bool {
        self.audio.split(',').any(|c| c == codec)
    }

    /// The transcode-target chain's HEAD — the codec the profile asks PMS to ENCODE when a
    /// source must re-encode: `hevc` where the SoC decodes it (keeps 4K + HDR10 through a
    /// transcode), else `h264`. ONE definition on purpose: route.rs's /decision-unreachable
    /// Load-payload guess and retranscode's codec record must name the codec the profile
    /// actually asked the server to produce, and as three hand-copied `if caps().hevc`
    /// expressions they could drift — a Load payload naming a codec the profile never requested
    /// is exactly the payload/output mismatch of docs/plex-pass-audit.md row 1 (issue #22).
    /// `transcoder.rs::profile_for` builds the target list's head from this, and its test pins
    /// the agreement.
    pub(crate) fn encode_vcodec(&self) -> &'static str {
        if self.hevc {
            "hevc"
        } else {
            "h264"
        }
    }
}

static CAPS: OnceLock<Caps> = OnceLock::new();
/// Did [`probe`] READ the table, or is [`caps`] returning [`Caps::assumed`]?
///
/// The two are indistinguishable from the values alone — on the 49SM9000PLA they are equal by
/// construction, and on every other set the fallback silently claims that set's profile. The event
/// log says which happened, in one line at boot, and that was enough while a log was the only
/// consumer. It stopped being enough when the diagnostics read-out started printing these values:
/// its output format is a PHOTOGRAPH, from a television nobody here owns, and a panel that prints
/// the dev set's decoder profile as if it had measured the reporter's is the [[silent-instrument-
/// trap]] shape exactly — an instrument that cannot say it did not measure.
///
/// **`false` when `probe` has not run**, which covers host tests as well as a boot that never got
/// here: "not measured" is the honest reading of both.
static MEASURED: OnceLock<bool> = OnceLock::new();

/// What this device decodes. Falls back to [`Caps::assumed`] if [`probe`] has not run — the
/// boot calls it first, so that path exists for host tests, not the TV.
pub(crate) fn caps() -> &'static Caps {
    CAPS.get_or_init(Caps::assumed)
}

/// One codec name, canonicalized: the table writes `"H.264"`, `"H.265"`, `"HEVC"`, `"EAC3"`;
/// dropping dots/dashes and case folds the spelling variants ("H265", "E-AC3") onto one key
/// so a firmware's formatting choice cannot un-recognize a codec.
fn canon(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '.' | '-'))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Per-axis conservative merge: min of the nonzero values, 0 (= the row didn't say) only when
/// neither row said. A row missing its dimensions must not zero out one that has them.
fn min_nz(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (a, b) => a.min(b),
    }
}

/// The table's shape, structurally: unknown fields (maxBitRate, channels, the license blurb) are
/// ignored by serde, and every field is defaulted so one malformed row degrades to "row said
/// nothing" instead of failing the whole parse. `maxFrameRate` was in that ignored list until
/// 2026-09-03; it is read now for the per-codec rows and nothing else.
#[derive(serde::Deserialize)]
struct Table {
    #[serde(default, rename = "videoCodecs")]
    video_codecs: Vec<VideoRow>,
    #[serde(default, rename = "audioCodecs")]
    audio_codecs: Vec<AudioRow>,
}
#[derive(serde::Deserialize)]
struct VideoRow {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "maxWidth")]
    max_width: u32,
    #[serde(default, rename = "maxHeight")]
    max_height: u32,
    /// A float on the wire is tolerated and rounded UP (59.94 → 60): this is a ceiling the
    /// stream has to fit under, and flooring it would clamp a 59.94 stream to 59. The table on
    /// the dev set writes integers.
    #[serde(default, rename = "maxFrameRate")]
    max_frame_rate: f64,
}
#[derive(serde::Deserialize)]
struct AudioRow {
    #[serde(default)]
    name: String,
}

/// Everything [`probe`] extracts, as a pure function of the file's text — testable without a
/// filesystem. `None` means "this is not a codec table we understand" — bad JSON, no video rows,
/// no H.264 row (every webOS SoC decodes it), or no stated width/height bound — and the caller
/// falls back to [`Caps::assumed`] WHOLE: a misread must never splice assumed numbers into
/// table-derived ones (that is how "(device table)" once logged invented values).
fn parse(s: &str) -> Option<Caps> {
    let t: Table = serde_json::from_str(s).ok()?;
    if t.video_codecs.is_empty() {
        // A table that names no video codec is a shape we misread, not a TV that decodes
        // nothing — fall back whole rather than deriving a client that transcodes everything.
        return None;
    }
    let (mut h264, mut hevc, mut vp9) = (false, false, false);
    let (mut hevc_wh, mut h264_wh) = ((0u32, 0u32), (0u32, 0u32));
    let (mut hevc_fps, mut h264_fps) = (0u32, 0u32);
    for row in &t.video_codecs {
        let wh = (row.max_width, row.max_height);
        let fps = if row.max_frame_rate.is_finite() && row.max_frame_rate > 0.0 {
            row.max_frame_rate.ceil() as u32
        } else {
            0
        };
        match canon(&row.name).as_str() {
            // "H.265" and "HEVC" are duplicate rows for one decoder — MIN on every axis.
            "h265" | "hevc" => {
                hevc = true;
                hevc_wh = (min_nz(hevc_wh.0, wh.0), min_nz(hevc_wh.1, wh.1));
                hevc_fps = min_nz(hevc_fps, fps);
            }
            "h264" => {
                h264 = true;
                h264_wh = (min_nz(h264_wh.0, wh.0), min_nz(h264_wh.1, wh.1));
                h264_fps = min_nz(h264_fps, fps);
            }
            "vp9" => vp9 = true,
            _ => {}
        }
    }
    // Every webOS SoC decodes H.264, so a table without the row (VP9-only, say) is a misread of
    // the file, not a TV without the codec — and the reject must be WHOLE, like the empty-list
    // guard above: `transcoder.rs`'s profile doc promises exactly that, yet this used to splice
    // `assumed` axes into table-derived ones instead, so probe() logged invented numbers under
    // the "(device table)" provenance tag.
    if !h264 {
        return None;
    }
    // The bound BOTH consumers apply to every codec at once — the per-axis MIN across the two
    // decoders' merged rows, so neither decoder is over-claimed (see the field doc).
    let hevc_max = (min_nz(h264_wh.0, hevc_wh.0), min_nz(h264_wh.1, hevc_wh.1));
    if hevc_max.0 == 0 || hevc_max.1 == 0 {
        // Rows present but no stated bound on some axis: the same misread rule — fall back
        // whole rather than publish half a table's numbers as the device's.
        return None;
    }
    // DP_AUDIO_CODECS ∩ the table, keeping DP_AUDIO_CODECS's order so the profile string stays
    // stable across firmwares that merely reorder their rows.
    let table_audio: Vec<String> = t.audio_codecs.iter().map(|r| canon(&r.name)).collect();
    let audio: Vec<&str> = crate::plex::DP_AUDIO_CODECS
        .split(',')
        .filter(|c| table_audio.iter().any(|t| t == c))
        .collect();
    let audio = if audio.is_empty() {
        // Every webOS SoC decodes AAC; a table naming none of the three is evidence about our
        // reading of it, not about the television. Same reasoning as the empty-video guard.
        Caps::assumed().audio
    } else {
        audio.join(",")
    };
    Some(Caps {
        hevc,
        hevc_max,
        h264_row: (h264_wh.0, h264_wh.1, h264_fps),
        hevc_row: if hevc {
            (hevc_wh.0, hevc_wh.1, hevc_fps)
        } else {
            (0, 0, 0)
        },
        vp9,
        audio,
    })
}

/// Read the table once and log what was derived. Called at boot right after `webos::probe`,
/// before anything can fail; safe when the file does not exist (older/odd firmware → assumed).
pub(crate) fn probe() {
    let mut measured = false;
    let caps = match std::fs::read_to_string(CAPS_TABLE) {
        Ok(s) => match parse(&s) {
            Some(c) => {
                measured = true;
                crate::log(&format!(
                    "devcaps: hevc={} {}x{} vp9={} audio={} rows: h264={}x{}@{} hevc={}x{}@{} (device table)",
                    c.hevc,
                    c.hevc_max.0,
                    c.hevc_max.1,
                    c.vp9,
                    c.audio,
                    c.h264_row.0,
                    c.h264_row.1,
                    c.h264_row.2,
                    c.hevc_row.0,
                    c.hevc_row.1,
                    c.hevc_row.2
                ));
                c
            }
            None => {
                crate::log(&format!(
                    "devcaps: {CAPS_TABLE} unparseable — assuming the 49SM9000PLA profile"
                ));
                Caps::assumed()
            }
        },
        Err(e) => {
            crate::log(&format!(
                "devcaps: {CAPS_TABLE} unreadable ({e}) — assuming the 49SM9000PLA profile"
            ));
            Caps::assumed()
        }
    };
    let _ = MEASURED.set(measured);
    let _ = CAPS.set(caps);
}

/// Whether [`caps`] reflects THIS set's table or the assumed profile — see [`MEASURED`].
pub(crate) fn measured() -> bool {
    *MEASURED.get().unwrap_or(&false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev set's real file, trimmed but formatting-verbatim (the ` : ` spacing, the license
    /// blurb, the fields nothing consumes): the parser has to survive the platform's own
    /// formatting, not a tidied version of it. Full copy captured off the TV 2026-08-10.
    const REAL: &str = r#"{
  "license" : "@@@LICENSE  Copyright (c) 2018 LG Electronics, Inc.  LICENSE@@@",
  "version" : "1.0",
  "videoCodecs" : [
    {
      "name" : "H.264",
      "maxWidth" : 4096,
      "maxHeight" : 2304,
      "maxFrameRate" : 60,
      "maxBitRate" : 50
    },

    {
      "name" : "H.265",
      "maxWidth" : 4096,
      "maxHeight" : 2304,
      "maxFrameRate" : 120,
      "maxBitRate" : 50
    },

    {
      "name" : "HEVC",
      "maxWidth" : 4096,
      "maxHeight" : 2176,
      "maxFrameRate" : 60,
      "maxBitRate" : 50
    },

    {
      "name" : "MPEG2",
      "maxWidth" : 1920,
      "maxHeight" : 1088,
      "maxFrameRate" : 30,
      "maxBitRate" : 40
    },

    {
      "name" : "VP8",
      "maxWidth" : 1920,
      "maxHeight" : 1088,
      "maxFrameRate" : 30,
      "maxBitRate" : 40
    },

    {
      "name" : "VP9",
      "maxWidth" : 4096,
      "maxHeight" : 2304,
      "maxFrameRate" : 60,
      "maxBitRate" : 50
    }
  ],
  "audioCodecs" : [
    {
      "name" : "MPEG",
      "channels" : 2
    },

    {
      "name" : "AAC",
      "channels" : 6
    },

    {
      "name" : "AC3",
      "channels" : 6
    },

    {
      "name" : "EAC3",
      "channels" : 8
    },

    {
      "name" : "DTS",
      "channels" : 6
    },

    {
      "name" : "FLAC",
      "channels" : 6
    }
  ]
}"#;

    #[test]
    fn reads_the_dev_sets_real_table() {
        let c = parse(REAL).expect("the real table parses");
        assert!(c.hevc && c.vp9);
        // H.265 (4096x2304) merged with HEVC (4096x2176) by min — NOT either row verbatim.
        assert_eq!(c.hevc_max, (4096, 2176));
        // the per-codec rows, frame rate included: H.264 is one row; "H.265"+"HEVC" MIN-merge
        // to 4096x2176@60 (the 120 on the H.265 row loses to the HEVC row's 60)
        assert_eq!(c.h264_row, (4096, 2304, 60));
        assert_eq!(c.hevc_row, (4096, 2176, 60));
        // a fractional cap is a CEILING the stream must fit under: 59.94 reads as 60, never 59
        let frac = parse(r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088,"maxFrameRate":59.94}]}"#).unwrap();
        assert_eq!(frac.h264_row, (1920, 1088, 60));
        // DTS/FLAC/MPEG are in the table but not in the pipeline's decode set; the subset keeps
        // DP_AUDIO_CODECS's own order, not the table's.
        assert_eq!(c.audio, "aac,ac3,eac3");
    }

    /// The merge is per-AXIS min, not a pick of the smaller row: a table whose duplicate rows
    /// each win one axis must combine into the bound neither row states alone. (The roomier
    /// H.264 row is there because an h264-less table is rejected whole — and it must not win
    /// an axis back from the hevc merge.)
    #[test]
    fn duplicate_rows_merge_by_min_on_every_axis() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":4096,"maxHeight":2304},
                {"name":"H.265","maxWidth":3000,"maxHeight":2304},
                {"name":"HEVC","maxWidth":4096,"maxHeight":2176}
            ]}"#,
        )
        .unwrap();
        assert_eq!(c.hevc_max, (3000, 2176));
    }

    /// The issue-#22 shape the first cut of this module REINTRODUCED: a 4K HEVC decoder beside
    /// 1080p-limited AVC. Bounding at the HEVC rows alone ("HEVC is the tighter decoder" — a
    /// dev-set fact asserted as universal) advertised a `*`-scoped 3840x2160, and PMS would
    /// direct-play 4K H.264 onto the 1080p decoder. The bound is the min across BOTH.
    #[test]
    fn the_bound_is_the_min_across_both_decoders_not_the_hevc_rows_alone() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1088},
                {"name":"HEVC","maxWidth":3840,"maxHeight":2160}
            ]}"#,
        )
        .unwrap();
        assert!(c.hevc);
        assert_eq!(c.hevc_max, (1920, 1088));
    }

    /// A row that omits its dimensions says nothing — it must not zero out the rows that spoke.
    #[test]
    fn a_dimensionless_duplicate_does_not_erase_the_stated_bound() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":4096,"maxHeight":2304},
                {"name":"HEVC","maxWidth":4096,"maxHeight":2176},
                {"name":"H.265"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(c.hevc_max, (4096, 2176));
    }

    /// The issue-#22 case this module exists for: a SoC whose table has no HEVC row. The width/
    /// height bound must then come from the H.264 row — telling PMS 3840 wide on a 1080p-only
    /// decoder would direct-play 4K H.264 onto hardware that cannot play it.
    #[test]
    fn a_soc_without_hevc_bounds_at_its_h264_row() {
        let c = parse(
            r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088}],
                "audioCodecs":[{"name":"AAC","channels":2}]}"#,
        )
        .unwrap();
        assert!(!c.hevc);
        assert_eq!(c.hevc_max, (1920, 1088));
        assert_eq!(c.audio, "aac");
    }

    /// Codec-name matching survives the spelling variants firmwares use: case, dots, dashes
    /// ("EAC3" here, "E-AC3" elsewhere in LG's own configs).
    #[test]
    fn codec_names_match_case_and_punctuation_insensitively() {
        let c = parse(
            r#"{"videoCodecs":[{"name":"H264","maxWidth":3840,"maxHeight":2160},
                              {"name":"h265","maxWidth":3840,"maxHeight":2160}],
                "audioCodecs":[{"name":"aAc"},{"name":"Ac-3"},{"name":"E-AC3"}]}"#,
        )
        .unwrap();
        assert!(c.hevc);
        assert_eq!(c.audio, "aac,ac3,eac3");
    }

    /// Garbage in must not panic AND must not half-parse: every unusable input lands on None so
    /// probe() falls back to the assumed profile whole — never spliced with it, which is how a
    /// misread once logged assumed numbers under the "(device table)" provenance tag. This runs
    /// during boot.
    #[test]
    fn garbage_and_vacuous_tables_are_none_not_a_panic() {
        for bad in [
            "",
            "not json at all",
            "{",
            "{}",
            r#"{"videoCodecs":[]}"#,
            r#"{"videoCodecs":"nope"}"#,
            // no H.264 row (every webOS SoC decodes it — a misread, not a VP9-only television)
            r#"{"videoCodecs":[{"name":"VP9","maxWidth":3840,"maxHeight":2160}]}"#,
            // rows, but no stated bound on an axis — half a table is not a device profile
            r#"{"videoCodecs":[{"name":"H.264"}]}"#,
            r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920}]}"#,
        ] {
            assert!(parse(bad).is_none(), "input {bad:?}");
        }
    }

    /// A parsed table whose audio rows miss every direct-playable codec falls back to the full
    /// set: that is a misread (every webOS SoC decodes AAC), and misreads must degrade to
    /// yesterday's behavior, not to a client that transcodes all audio.
    #[test]
    fn an_empty_audio_intersection_falls_back_to_the_full_dp_set() {
        let c = parse(
            r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088}],
                "audioCodecs":[{"name":"DTS"},{"name":"WMA"}]}"#,
        )
        .unwrap();
        assert_eq!(c.audio, crate::plex::DP_AUDIO_CODECS);
    }

    /// The fallback IS yesterday's constants — the values the app asserted for every device
    /// before this module existed (the profile-string pin in plex/transcoder.rs is the other
    /// half of this contract).
    #[test]
    fn the_assumed_profile_is_todays_constants() {
        let c = Caps::assumed();
        assert!(c.hevc);
        assert_eq!(c.hevc_max, (3840, 2176));
        assert_eq!(c.audio, "aac,ac3,eac3");
    }
}
