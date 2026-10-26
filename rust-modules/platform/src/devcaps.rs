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
use std::borrow::Cow;
use std::sync::OnceLock;

pub mod dv;

/// The AUDIO codec set the buffer-feed PIPELINE decodes. This is the software half of a
/// two-sided test — what our demuxer/payload path can feed, before asking whether this
/// particular SoC can decode it. The live set is [`Caps::audio`] (this list ∩ the device's own
/// codec table), and the ONE-definition rule lives with it: the Normal routing uses
/// `plex::is_dp_audio_track_with` for membership and channel bounds, shared with the device profile.
/// Forced mode instead uses the implemented software feed formats and its separate profile,
/// without conservative device bounds.
///
/// Defined here, not in `plex`, because this module is the one that intersects it with the
/// device's own table and `plex` sits above the platform layer; `plex` re-exports it
/// (`plex::DP_AUDIO_CODECS`) for the profile string and route's plan.
///
/// `opus` is the one entry the table does not gate ([`PIPELINE_ONLY_AUDIO`]): umediaserver's
/// table describes its own file player, and the dev set's lists no Opus row, while the buffer feed
/// (LG's SMP `OPUS` + `opusInfo`, which moonlight-tv's ss4s feeds on webOS) decodes it.
///
/// What a set announces is this list intersected with ITS table, with the table's channel counts; a
/// combination measured broken on a firmware family is taken off by [`EXCLUSIONS`], not here.
pub const DP_AUDIO_CODECS: &str = "aac,ac3,eac3,dts,opus";

/// The [`DP_AUDIO_CODECS`] entries admitted on a parsed device table even when it has no row for
/// them, because the codec is a property of the buffer-feed pipeline rather than of the table.
const PIPELINE_ONLY_AUDIO: &[&str] = &["opus"];

/// Auto's channel ceiling for Opus, whatever a table row claims: stereo plays cleanly on every set
/// measured, while 5.1 stalls on a C1 (webOS 6) once packets pass ~1500 bytes, with nothing in the
/// log (#917, #918). Wider tracks stay with the server; Force Direct Play applies no ceiling, as for
/// every codec. A 5.1 Opus film did play through on a C5 (webOS 10.3), so this can loosen per model.
const OPUS_AUTO_MAX_CHANNELS: u32 = 2;

/// Audio the app DECODES ITSELF and feeds as PCM, so no row of the device table is needed: the
/// pipeline's PCM input (`libpf`'s `audio/x-raw` caps and the `contents.pcmInfo` node) is what
/// carries it. FLAC is the case — libpf has no FLAC input of its own (its Load vocabulary is
/// AAC/AC3/AC3 PLUS/DTS/MP3/MPEG/OPUS/VORBIS/WMA/PCM), but PCM decoded from it is bit-exact. Both
/// the vocabulary and the PCM input were verified on webOS 4.4.3 (libpf-1.0 on an OLED55B8); other
/// webOS versions are unconfirmed. Joins [`Caps::audio`] only on a table that was READ: the assumed
/// fallback stays byte-for-byte what it was.
pub const PCM_AUDIO_CODECS: &str = "flac";

/// The channel ceiling of the PCM path: `pcmInfo.channelMode` has `mono` and `stereo` (and
/// `dual`), nothing wider, so a multichannel FLAC stays a server conversion.
pub const PCM_MAX_CHANNELS: u32 = 2;

/// The rates `pcmInfo.sampleRate` can name: libpf's `LUT_SampleRateType` (8, 12, 16, 22.05, 24, 32
/// and 48 kHz) plus 44.1 kHz, its fallback. libpf matches the value as a number of KILOHERTZ
/// (`player::engine`); any value that matches nothing (a rate in hertz, 96 kHz, 192 kHz) silently
/// becomes 44.1 kHz and plays at the wrong speed with no error (a 96 kHz FLAC audibly slow on an
/// OLED55B8, webOS 4.4.3; a 48 kHz one 9% slow, a 32 kHz one fast and stuttering on a C1, webOS
/// 6.5.3, when the rate was declared in hertz). The table was read from libpf-1.0 on the B8.
pub const PCM_SAMPLE_RATES: [u32; 8] = [8_000, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// Whether `codec` is one the app decodes itself and feeds as PCM ([`PCM_AUDIO_CODECS`]).
pub fn is_pcm_codec(codec: &str) -> bool {
    let id = audio_codec_id(codec);
    PCM_AUDIO_CODECS.split(',').any(|c| c == id)
}

/// The PCM path's own limits for one track: one or two channels, at a rate `pcmInfo` can name
/// ([`PCM_SAMPLE_RATES`]). A codec that is not app-decoded passes; an unstated count or rate
/// does not.
/// These are limits of the PCM input, not of a hardware decoder, so Force Direct Play applies
/// them as well.
pub fn pcm_track_fits(codec: &str, channels: i64, sample_rate: i64) -> bool {
    !is_pcm_codec(codec)
        || ((1..=i64::from(PCM_MAX_CHANNELS)).contains(&channels)
            && PCM_SAMPLE_RATES.iter().any(|&r| i64::from(r) == sample_rate))
}

const CAPS_TABLE: &str = "/etc/umediaserver/device_codec_capability_config.json";

/// One native-audio combination that is **measured broken** on a firmware family, and so is not
/// announced for it although the family's own codec table lists it. An excluded track is
/// converted exactly as one the set never listed: by the server.
///
/// The key is the firmware codename (`tv::device::Info::codename`, nyx's `webos_release_codename`),
/// the one identity the app reads reliably on every set: it groups the sets that run the same
/// build of the media stack, which is what decides the decoder's behaviour, where the retail model
/// name (`product_id`, absent on some firmware) and the SoC class would split one stack or merge two.
/// A row says `max_channels` native channels of `codec`, `0` meaning the codec is not announced at
/// all. A set no row names announces what its table lists; a row only takes away, so an unmeasured
/// family is never promised less than the firmware says.
pub struct Exclusion {
    pub codename: &'static str,
    pub codec: &'static str,
    pub max_channels: u32,
    pub evidence: &'static str,
}

pub const EXCLUSIONS: &[Exclusion] = &[
    // LG C1 (OLED55C14LB), webOS 6.5.3, measured 2026-10-21 and 2026-10-23.
    Exclusion {
        codename: "kisscurl-koli",
        codec: "opus",
        max_channels: 2,
        evidence: "6-channel Opus stalls the pipeline (Load returns, nothing is accepted, no picture) once \
                   packets pass 1500 bytes: CBR files with 1500 B packets play, 1536 B CBR and VBR noise \
                   (2264-3411 B) stall after ~190 AUs; stereo Opus (<=635 B) is clean. Emby topic 132334 \
                   reports corrupted 5.1 Opus on a 2021 webOS 6 LG (43UP78006LB).",
    },
    Exclusion {
        codename: "kisscurl-koli",
        codec: "vorbis",
        max_channels: 0,
        evidence: "the pipeline refuses the first buffers with type=18 num=200 'Audio Codec Not \
                   Supported' (stereo 44.1/48 kHz and 5.1, with the three header packets fed in-band); \
                   libpf's payload has no key for Vorbis headers.",
    },
];

/// Bound the native channels of `codec` in `caps` to `max` (`0`: the codec is not announced at all),
/// never raising what the table said. Returns what was applied as `codec<=N` / `codec=none`, or
/// nothing when the set does not announce the codec.
fn limit_codec(caps: &mut Caps, codec: &str, max: u32) -> Option<String> {
    if !caps.audio_has(codec) {
        return None;
    }
    if max == 0 {
        caps.audio = caps.audio.split(',').filter(|c| *c != codec).collect::<Vec<_>>().join(",");
        caps.audio_channels.remove(codec);
        return Some(format!("{codec}=none"));
    }
    let limit = caps.audio_channels.get(codec).copied().filter(|l| *l > 0).map_or(max, |l| l.min(max));
    caps.audio_channels.insert(codec.to_string(), limit);
    // Force Direct Play applies no table ceiling, but it does honour a measured one.
    let native = caps.native_ceiling.get(codec).copied().map_or(max, |l| l.min(max));
    caps.native_ceiling.insert(codec.to_string(), native);
    Some(format!("{codec}<={limit}"))
}

/// Take the exclusions for `codename` off `caps`, returning what was applied as `codec<=N` or
/// `codec=none`. `ignore` (the debug flag `/tmp/plxnative-nativeaudiowide`) applies none, so a
/// combination the table keeps off the pipeline can be tried on a set.
pub fn apply_exclusions(caps: &mut Caps, codename: &str, ignore: bool, table: &[Exclusion]) -> Vec<String> {
    if ignore || codename.is_empty() {
        return Vec::new();
    }
    table
        .iter()
        .filter(|e| e.codename.eq_ignore_ascii_case(codename))
        .filter_map(|e| limit_codec(caps, e.codec, e.max_channels))
        .collect()
}

/// A native audio combination this install **watched the pipeline refuse**, remembered so the next
/// play goes straight to conversion. The install preference `native_audio_refused`
/// (`plex::session`) is the one list of them; this is the shape it hands over
/// ([`project_stored_refusals`]) and receives back ([`learn_refusal`]). `channels` is the refused
/// track's channel count.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Refusal {
    pub firmware: String,
    pub codec: String,
    pub channels: u32,
    /// The firmware BUILD the refusal was watched on (`tv::device::Info::build`; empty for an entry
    /// stored before builds were recorded). A different build is a different decoder, so a refusal
    /// is evidence only on the build it was watched on ([`Refusal::watched_on`]).
    pub build: String,
}

impl Refusal {
    /// Whether this refusal says anything about a set running `build`. An LG update replaces the
    /// decoder a refusal was watched on, so another build's entry (and an older record's, which
    /// never named one) is not evidence and the new firmware gets a fresh native try. An unknown
    /// running build (no nyx file) judges nothing, so everything counts.
    pub fn watched_on(&self, build: &str) -> bool {
        build.is_empty() || self.build == build
    }
}

/// Take the learned refusals for `codename` off `caps`, as [`apply_exclusions`] does. A refusal of a
/// stereo (or mono) track takes the codec away altogether; a refusal of a surround track keeps the
/// codec to stereo, which is what the static rows do for the same measurements. `ignore` is the debug
/// flag again.
pub fn apply_learned(caps: &mut Caps, codename: &str, ignore: bool, learned: &[Refusal]) -> Vec<String> {
    if ignore || codename.is_empty() {
        return Vec::new();
    }
    learned
        .iter()
        .filter(|r| r.firmware.eq_ignore_ascii_case(codename))
        .filter_map(|r| limit_codec(caps, &r.codec, refusal_ceiling(r.channels)))
        .collect()
}

/// What a refusal of a `channels`-channel track leaves of its codec ([`limit_codec`]'s `max`): a
/// stereo (or mono) refusal takes the codec away (`0`), a surround one keeps it to stereo. The one
/// rule for a refusal the install learned ([`apply_learned`]) and one an attempt only supposes
/// ([`Caps::without`]).
fn refusal_ceiling(channels: u32) -> u32 {
    if channels <= 2 { 0 } else { 2 }
}

/// gst's rank file: which decoder elements the firmware leaves switched on.
const GST_CONF: &str = "/etc/gst/gstcool.conf";

/// For each audio codec the pipeline can be fed, the gst decoder elements that can decode it: LG's
/// hardware `<codec>_audiodec` first, then the software decoders (`avdec_dca` is DTS's, which
/// webosbrew's `dts_restore` re-ranks). Codec ids are [`audio_codec_id`]'s.
const DECODER_ELEMENTS: &[(&str, &[&str])] = &[
    ("aac", &["aac_audiodec", "avdec_aac", "faad"]),
    ("ac3", &["ac3_audiodec", "avdec_ac3"]),
    ("eac3", &["ac3_audiodec", "avdec_eac3"]),
    ("dts", &["dts_audiodec", "avdec_dca"]),
    ("opus", &["opus_audiodec", "opusdec", "avdec_opus"]),
    ("vorbis", &["vorbis_audiodec", "vorbisdec", "ivorbisdec"]),
];

/// `[rank]` and `[sw_decoder]` of `gstcool.conf` as `element -> rank` (the later line wins). Other
/// sections, comments and lines that are not `name=integer` are ignored.
pub fn parse_gst_ranks(text: &str) -> std::collections::BTreeMap<String, i64> {
    let mut ranks = std::collections::BTreeMap::new();
    let mut wanted = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            wanted = matches!(section.trim(), "rank" | "sw_decoder");
        } else if wanted {
            if let Some((name, value)) = line.split_once('=') {
                if let Ok(rank) = value.trim().parse::<i64>() {
                    ranks.insert(name.trim().to_string(), rank);
                }
            }
        }
    }
    ranks
}

/// Drop from `caps` the audio codecs that `gstcool.conf` shows **nothing can decode**, and say what
/// happened as `(dropped, kept because the file never names a decoder for them)`.
///
/// The device table says what the hardware block can do; this file says which gst decoder elements
/// the firmware left switched on. A codec is dropped only on positive evidence: some decoder element
/// of it is listed and every listed one is rank 0 (the C5 whose table lists DTS has `avdec_dca=0` and
/// no hardware `dts_audiodec`; `dts_restore` gives `avdec_dca` a rank again). A codec the file does
/// not mention at all (Opus on a C1: `opus_audiodec` keeps its default rank) stays: no line means the
/// default rank, not unsupported. An empty or unreadable file therefore changes nothing. The channel
/// ceiling goes with the codec, so Force Direct Play's "DTS needs a surviving table row" rule sees
/// the same answer.
pub fn apply_gst_decoders(caps: &mut Caps, conf: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let ranks = parse_gst_ranks(conf);
    let (mut dropped, mut unlisted) = (Vec::new(), Vec::new());
    for (codec, elements) in DECODER_ELEMENTS {
        if !caps.audio_has(codec) {
            continue;
        }
        let listed: Vec<i64> = elements.iter().filter_map(|e| ranks.get(*e).copied()).collect();
        if listed.is_empty() {
            unlisted.push(*codec);
        } else if listed.iter().all(|r| *r <= 0) {
            caps.audio = caps.audio.split(',').filter(|c| *c != *codec).collect::<Vec<_>>().join(",");
            caps.audio_channels.remove(*codec);
            dropped.push(*codec);
        }
    }
    (dropped, unlisted)
}


/// One stream class an attempt can be told to do without ([`Caps::without`]). Audio only for now:
/// a refused video decode comes with the video flight that needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Facet {
    /// `channels` channels of `codec`, in any spelling [`audio_codec_id`] knows.
    Audio { codec: String, channels: u32 },
}

/// The decode-capability snapshot the playback stack derives from. Probed once, at boot
/// ([`probe`]); the app reads [`caps`] ONCE per attempt, at the play's start, and the attempt carries
/// that value (`route::PlaybackSession::attempt_caps`, which the request's `OpenRecovery` owns). The process-wide answer is not frozen after
/// boot: [`learn_refusal`] can still move it, and an attempt already in flight keeps (and narrows,
/// [`Caps::without`]) its own value instead of re-reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
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
    /// The device table lists a VP9 decoder row. False in `assumed()` and on every table without
    /// the row, which keeps VP9 a server job there. Until VP9 direct play this was a diagnostic
    /// only and `assumed()` claimed it; now it gates the feed, so an unreadable table must not.
    pub vp9: bool,
    /// The VP9 row's (max width, max height, ceil max frame rate), MIN-merged across duplicate
    /// rows; 0 = not stated. Read only when `vp9`.
    pub vp9_row: (u32, u32, u32),
    /// The deepest VP9 bit depth the profile claims: 10 where a 10-bit VP9 stream has been measured
    /// to decode with the right levels AND to reach HDR when its container says PQ, 8 everywhere
    /// else (the table has no bit-depth axis, so this is device evidence, not table data). Only the
    /// webOS 4 family (ACB video path) is measured, so [`vp9_bit_depth_for`] is the one place the
    /// 10 comes from; the assumed profile and a table read on any other set say 8.
    pub vp9_bit_depth: u32,
    /// The device table lists an AV1 decoder row. False in `assumed()` and on every table without
    /// the row, which keeps AV1 a server transcode there. Issue #593.
    pub av1: bool,
    /// The AV1 row's (max width, max height, ceil max frame rate), MIN-merged across duplicate
    /// rows; 0 = not stated. Read only when `av1`.
    pub av1_row: (u32, u32, u32),
    /// The direct-playable AUDIO subset: [`DP_AUDIO_CODECS`] (what the pipeline decodes)
    /// intersected with the table's audio rows, in `DP_AUDIO_CODECS`'s own URL form/order.
    /// Normal routing reads this and the channel ceilings through `plex::is_dp_audio_track_with`,
    /// as does its PMS profile. Forced mode starts from the pipeline's feed set with no channel
    /// ceiling and admits DTS only with a DTS row in `audio_channels` (`Caps::forced_feeds_audio`).
    pub audio: String,
    /// Nonzero, per-codec channel ceilings from the device table, duplicate rows MIN-merged.
    /// Missing legacy limits remain unknown; DTS is admitted only with a measured limit.
    pub audio_channels: std::collections::BTreeMap<String, u32>,
    /// Per-codec channel ceilings that were **measured** on this firmware ([`EXCLUSIONS`]) or
    /// learned from a refusal, as opposed to the table's. Force Direct Play applies no table
    /// ceiling but does honour these ([`Caps::forced_feeds_audio_track`]).
    pub native_ceiling: std::collections::BTreeMap<String, u32>,
}

impl Caps {
    /// The 49SM9000PLA values — the only set ever measured before this module. Byte-for-byte,
    /// these reproduce the profile the app has always sent (the transcoder test pins that), so
    /// an unreadable table changes nothing for any device that was working yesterday.
    pub fn assumed() -> Caps {
        Caps {
            hevc: true,
            hevc_max: (3840, 2176),
            h264_row: (0, 0, 0),
            hevc_row: (0, 0, 0),
            vp9: false,
            vp9_row: (0, 0, 0),
            vp9_bit_depth: 8,
            av1: false,
            av1_row: (0, 0, 0),
            audio: "aac,ac3,eac3".to_string(),
            audio_channels: Default::default(),
            native_ceiling: Default::default(),
        }
    }

    /// Membership test on [`Caps::audio`]. `codec` is any spelling of the name
    /// ([`audio_codec_id`]): PMS says `dca` where the table and the pipeline say `dts`.
    pub fn audio_has(&self, codec: &str) -> bool {
        let id = audio_codec_id(codec);
        self.audio.split(',').any(|c| c == id)
    }

    /// Local counterpart of the profile's codec-scoped audio.channels limitations.
    pub fn audio_supports(&self, codec: &str, channels: i64) -> bool {
        let id = audio_codec_id(codec);
        if !self.audio_has(&id) {
            return false;
        }
        // DTS and the PCM path both need a stated channel count: an unknown one is not admitted.
        let needs_count = id == "dts" || is_pcm_codec(&id);
        match self.audio_channels.get(id.as_ref()) {
            Some(&limit) => (channels > 0 || !needs_count) && channels <= i64::from(limit),
            None => !needs_count,
        }
    }

    /// Force Direct Play's audio gate: the pipeline's feed formats ([`DP_AUDIO_CODECS`]), with
    /// no channel ceiling. DTS additionally needs the evidence Auto asks for
    /// ([`Caps::forced_admits_dts`]), and a pipeline-only codec (Opus) the table read Auto needs
    /// ([`Caps::audio_has`]). The planner and the forced profile both read this.
    pub fn forced_feeds_audio(&self, codec: &str) -> bool {
        let id = audio_codec_id(codec);
        if id == "dts" {
            return self.forced_admits_dts();
        }
        // Pipeline-only Opus and app-decoded FLAC need only a table that was read. A FLAC
        // track's own limits are [`pcm_track_fits`], which the planner applies in both modes.
        if PIPELINE_ONLY_AUDIO.contains(&id.as_ref()) || is_pcm_codec(&id) {
            return self.audio_has(&id);
        }
        DP_AUDIO_CODECS.split(',').any(|c| c == id)
    }

    /// [`Caps::forced_feeds_audio`] for one TRACK: Force Direct Play applies no table ceiling, but a
    /// ceiling measured on this firmware ([`EXCLUSIONS`], or a refusal learned from it) holds, so a
    /// 5.1 Opus track that stalls a C1's pipeline is not forced into it. `channels` 0 = not known.
    pub fn forced_feeds_audio_track(&self, codec: &str, channels: i64) -> bool {
        if !self.forced_feeds_audio(codec) {
            return false;
        }
        match self.native_ceiling.get(audio_codec_id(codec).as_ref()) {
            Some(&max) => channels <= i64::from(max),
            None => true,
        }
    }

    /// The Forced-mode DTS rule, one predicate so the owner can flip it: the table has a DTS
    /// row with a stated channel count (an absent decoder would hang the Load like #584 instead
    /// of refusing). The channel ceiling itself is not applied.
    pub fn forced_admits_dts(&self) -> bool {
        self.audio_channels.contains_key("dts")
    }

    /// A narrower copy of this value, with `facet` taken out of it: the capability an attempt plans
    /// and profiles from when it must not offer that stream class again. PURE: it reads and writes
    /// no registry, so the value can be derived for a recovery flight without recording anything
    /// before the flight has proved anything.
    ///
    /// An audio facet takes native audio away the way a refusal the install learned does
    /// ([`apply_learned`]): a stereo or mono one takes the codec off the announced set, a surround
    /// one keeps it to stereo. A codec this value does not announce is already without, so the copy
    /// is equal to `self`.
    pub fn without(&self, facet: &Facet) -> Caps {
        let mut next = self.clone();
        match facet {
            Facet::Audio { codec, channels } => {
                limit_codec(&mut next, &audio_codec_id(codec), refusal_ceiling(*channels));
            }
        }
        next
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
    pub fn encode_vcodec(&self) -> &'static str {
        if self.hevc {
            "hevc"
        } else {
            "h264"
        }
    }
}

/// `nativeaudiowide` ignores the learned refusals as it ignores the static rows; `nativeaudiolearn`
/// beside it keeps the learned layer (and the learning) on.
fn ignores_learned(wide: bool, learn: bool) -> bool {
    wide && !learn
}

/// This set's audio capability answer and what derives it. [`probe`] builds it once: the table,
/// minus the codecs gstcool.conf shows nothing can decode, minus [`EXCLUSIONS`]. The one thing that
/// moves afterwards is [`Layers::caps`], and only one way: it LOSES native audio as refusals arrive
/// ([`Layers::fold`]). The list of refusals is not kept here. The install preference
/// `native_audio_refused` (`plex::session`) owns it, and this holds the answer derived from it.
struct Layers {
    /// What [`caps`] answers. Replaced, never edited, so a reader holding the `Arc` keeps one
    /// consistent snapshot and nothing is leaked for it.
    caps: std::sync::RwLock<std::sync::Arc<Caps>>,
    codename: String,
    /// The running firmware build (`tv::device::Info::build`; empty when the set did not say).
    build: String,
    wide: bool,
    /// `plxnative-nativeaudiolearn` beside `wide`: the learned refusals still apply and are still
    /// learned, so the learn-and-convert path can be shown on a set the exclusions would keep off it.
    learn: bool,
    /// `plxnative-nativeaudioforget`: the stored refusals are not applied this run, so a set can be
    /// shown learning from nothing. A refusal learned during the run still applies at once.
    forget: bool,
}

impl Layers {
    fn new(caps: Caps, codename: String, build: String, wide: bool, learn: bool, forget: bool) -> Layers {
        Layers { caps: std::sync::RwLock::new(std::sync::Arc::new(caps)), codename, build, wide, learn, forget }
    }

    /// Take away from the answer whatever `refusals` (this firmware's, this build's) say the pipeline
    /// cannot take, and report whether that took anything. Monotone and idempotent: it only ever
    /// removes, so a list that is behind (a record read before a refusal's write landed) cannot give
    /// back what an earlier call took, and applying the same list twice is a no-op.
    fn fold(&self, refusals: &[Refusal]) -> bool {
        let current: Vec<Refusal> = refusals.iter().filter(|r| r.watched_on(&self.build)).cloned().collect();
        // The write lock spans the read-modify-write so two folds cannot lose each other's removal.
        let mut answer = self.caps.write().unwrap_or_else(|e| e.into_inner());
        let mut next = Caps::clone(&answer);
        let applied = apply_learned(&mut next, &self.codename, ignores_learned(self.wide, self.learn), &current);
        if next == **answer {
            return false;
        }
        plx_base::eventlog::log(&format!(
            "devcaps: firmware={:?} learned refusals: {} -> audio={}",
            self.codename,
            applied.join(" "),
            next.audio
        ));
        *answer = std::sync::Arc::new(next);
        true
    }
}

/// The boot answer, set once by [`probe`].
static BOOT: OnceLock<Layers> = OnceLock::new();
static ASSUMED: OnceLock<std::sync::Arc<Caps>> = OnceLock::new();
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

/// Run `f` on this set's layers: the test thread's own when a test installed one, else the boot's.
/// `None` before [`probe`] has run (every host test that installed nothing).
fn with_layers<R>(f: impl FnOnce(&Layers) -> R) -> Option<R> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(layers) = THREAD_LAYERS.with(|slot| slot.borrow().clone()) {
        return Some(f(&layers));
    }
    BOOT.get().map(f)
}

/// What this device decodes. Falls back to [`Caps::assumed`] if [`probe`] has not run — the
/// boot calls it first, so that path exists for host tests, not the TV. The answer can change
/// afterwards, only by losing native audio the pipeline was watched refusing
/// ([`learn_refusal`], [`project_stored_refusals`]), so a decision that must hold across a whole
/// attempt reads it once.
pub fn caps() -> std::sync::Arc<Caps> {
    with_layers(|l| l.caps.read().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or_else(assumed_shared)
}

/// The one shared [`Caps::assumed`] value: [`caps`]' answer before [`probe`] has run, and the
/// stand-in a session that has not begun an attempt plans from (`route::PlaybackSession::attempt_caps`).
pub fn assumed_shared() -> std::sync::Arc<Caps> {
    ASSUMED.get_or_init(|| std::sync::Arc::new(Caps::assumed())).clone()
}

/// Apply the refusals the install's stored record holds to the answer ([`Layers::fold`]). The plex
/// session calls it with every record it installs, under its cache lock, so it does no IO and takes
/// no lock the session could hold. `plxnative-nativeaudioforget` applies none. True when it took
/// something away.
pub fn project_stored_refusals(stored: &[Refusal]) -> bool {
    with_layers(|l| !l.forget && l.fold(stored)).unwrap_or(false)
}

/// The pipeline refused `codec` with `channels` channels on this firmware, fed natively: take it
/// off the process-wide answer at once (the NEXT play's start reads it without the audio; the
/// recovery in flight does not read this answer, it narrows its own attempt's value by the same
/// rule, [`Caps::without`], which shares `refusal_ceiling` with [`apply_learned`]) and return the
/// entry for the caller to persist. `None` when this set cannot learn (no firmware name, the debug
/// flag, no [`probe`]), or the combination changes nothing: the codec is not announced to that
/// many channels any more, so the same play converting it would not be this refusal.
pub fn learn_refusal(codec: &str, channels: u32) -> Option<Refusal> {
    with_layers(|l| {
        if ignores_learned(l.wide, l.learn) || l.codename.is_empty() {
            return None;
        }
        // The pipeline's spelling (PMS says `dca` where the table and the pipeline say `dts`).
        let refusal = Refusal {
            firmware: l.codename.clone(),
            codec: audio_codec_id(codec).into_owned(),
            channels,
            build: l.build.clone(),
        };
        l.fold(std::slice::from_ref(&refusal)).then_some(refusal)
    })
    .flatten()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static THREAD_LAYERS: std::cell::RefCell<Option<std::sync::Arc<Layers>>> = const { std::cell::RefCell::new(None) };
}

/// Make [`caps`] answer `caps` on THIS test thread until the guard drops (the
/// `i18n::language_on_this_thread_for_test` precedent). Tests run in parallel and the
/// process-wide answer is write-once, so a thread-local door is how one test plays a set whose
/// table lists a DTS decoder. The set has no firmware name, so it learns nothing
/// ([`firmware_on_this_thread_for_test`] is the one that does).
#[cfg(any(test, feature = "test-support"))]
pub fn caps_on_this_thread_for_test(caps: Caps) -> CapsGuard {
    firmware_on_this_thread_for_test("", "", caps)
}

/// [`caps_on_this_thread_for_test`] for a set that names its firmware and build, so the thread
/// can learn a refusal ([`learn_refusal`]) and be handed a stored record
/// ([`project_stored_refusals`]) as a television would.
#[cfg(any(test, feature = "test-support"))]
pub fn firmware_on_this_thread_for_test(codename: &str, build: &str, caps: Caps) -> CapsGuard {
    let layers = Layers::new(caps, codename.to_owned(), build.to_owned(), false, false, false);
    CapsGuard(THREAD_LAYERS.with(|slot| slot.replace(Some(std::sync::Arc::new(layers)))))
}

#[cfg(any(test, feature = "test-support"))]
pub struct CapsGuard(Option<std::sync::Arc<Layers>>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for CapsGuard {
    fn drop(&mut self) {
        THREAD_LAYERS.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// The one canonical spelling of an AUDIO codec name, for every comparison between vocabularies:
/// lowercase, PMS's `dca` is the pipeline's `dts`, and `ec-3` is `eac3`. Applied at membership
/// ([`Caps::audio_has`]) and at the Load payload; inbound data stays raw (track memory compares
/// the raw string).
pub fn audio_codec_id(codec: &str) -> Cow<'_, str> {
    if codec.bytes().any(|b| b.is_ascii_uppercase()) {
        return Cow::Owned(audio_codec_id(&codec.to_ascii_lowercase()).into_owned());
    }
    match codec {
        "dca" => Cow::Borrowed("dts"),
        "ec-3" => Cow::Borrowed("eac3"),
        _ => Cow::Borrowed(codec),
    }
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

/// The table's audio rows verbatim (`NAME/channels`, file order), for the boot log: [`Caps::audio`]
/// only keeps the [`DP_AUDIO_CODECS`] subset, so without this a row the pipeline does not feed yet
/// (FLAC, Opus) never shows up anywhere.
fn table_audio_rows(s: &str) -> String {
    serde_json::from_str::<Table>(s)
        .map(|t| {
            t.audio_codecs
                .iter()
                .map(|r| format!("{}/{}", r.name, r.channels))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// Per-axis conservative merge: min of the nonzero values, 0 (= the row didn't say) only when
/// neither row said. A row missing its dimensions must not zero out one that has them.
fn min_nz(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (a, b) => a.min(b),
    }
}

/// The table's shape, structurally: unknown fields (maxBitRate, the license blurb) are
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
    #[serde(default)]
    channels: u32,
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
    let (mut h264, mut hevc, mut vp9, mut av1) = (false, false, false, false);
    let (mut hevc_wh, mut h264_wh, mut vp9_wh, mut av1_wh) =
        ((0u32, 0u32), (0u32, 0u32), (0u32, 0u32), (0u32, 0u32));
    let (mut hevc_fps, mut h264_fps, mut vp9_fps, mut av1_fps) = (0u32, 0u32, 0u32, 0u32);
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
            // VP9 and AV1 are capability bits with their own bounds. Neither feeds `hevc_max`, the
            // H.264 guard or the audio logic: those stay exactly as the table without them
            // computes them.
            "vp9" => {
                vp9 = true;
                vp9_wh = (min_nz(vp9_wh.0, wh.0), min_nz(vp9_wh.1, wh.1));
                vp9_fps = min_nz(vp9_fps, fps);
            }
            "av1" => {
                av1 = true;
                av1_wh = (min_nz(av1_wh.0, wh.0), min_nz(av1_wh.1, wh.1));
                av1_fps = min_nz(av1_fps, fps);
            }
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
    let mut audio_channels = std::collections::BTreeMap::new();
    for row in &t.audio_codecs {
        if row.channels > 0 {
            let codec = canon(&row.name);
            let old = audio_channels.get(&codec).copied().unwrap_or(0);
            audio_channels.insert(codec, min_nz(old, row.channels));
        }
    }
    let table_audio: Vec<String> = t.audio_codecs.iter().map(|r| canon(&r.name)).collect();
    let audio: Vec<&str> = DP_AUDIO_CODECS
        .split(',')
        .filter(|c| !PIPELINE_ONLY_AUDIO.contains(c))
        .filter(|c| table_audio.iter().any(|t| t == c))
        .filter(|c| *c != "dts" || audio_channels.contains_key("dts"))
        .collect();
    let audio = if audio.is_empty() {
        // Every webOS SoC decodes AAC; a table naming none of the three is evidence about our
        // reading of it, not about the television. Same reasoning as the empty-video guard.
        Caps::assumed().audio
    } else {
        // The pipeline-only codecs ride on a table we did read, never on the assumed fallback.
        let pipeline = DP_AUDIO_CODECS.split(',').filter(|c| PIPELINE_ONLY_AUDIO.contains(c));
        // The app-decoded PCM formats follow the hardware ones, with the PCM path's own ceiling
        // replacing whatever the table says about a decoder we never feed.
        for c in PCM_AUDIO_CODECS.split(',') {
            audio_channels.insert(c.to_string(), PCM_MAX_CHANNELS);
        }
        audio.into_iter().chain(pipeline).chain(PCM_AUDIO_CODECS.split(',')).collect::<Vec<_>>().join(",")
    };
    if audio.split(',').any(|c| c == "opus") {
        let limit = audio_channels.entry("opus".to_owned()).or_default();
        *limit = min_nz(*limit, OPUS_AUTO_MAX_CHANNELS);
    }
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
        vp9_row: (vp9_wh.0, vp9_wh.1, vp9_fps),
        vp9_bit_depth: 8,
        av1,
        av1_row: (av1_wh.0, av1_wh.1, av1_fps),
        audio,
        audio_channels,
        native_ceiling: Default::default(),
    })
}

/// The VP9 bit depth to claim on a set running webOS `major` (0 = unknown). 10 on webOS 1 to 4, the
/// ACB video path, where the dev set (4.x) decoded a 10-bit Profile 2 stream with correct levels and a
/// PQ-tagged one switched the panel to HDR through `player::vp9_hdr`. 8 on every other set: on webOS 5+
/// there is no ACB, so `vp9_hdr` sends nothing, and whether a PQ VP9 stream reaches HDR there is
/// unknown, which means a 10-bit file might direct-play with washed-out colour where the server
/// would have converted it correctly.
pub fn vp9_bit_depth_for(major: u32) -> u32 {
    if (1..=4).contains(&major) {
        10
    } else {
        8
    }
}

/// Read the table once and log what was derived. Called at boot right after `webos::probe`,
/// before anything can fail; safe when the file does not exist (older/odd firmware → assumed).
pub fn probe() {
    let mut measured = false;
    let caps = match std::fs::read_to_string(CAPS_TABLE) {
        Ok(s) => match parse(&s) {
            Some(c) => {
                measured = true;
                let av1_row = if c.av1 {
                    format!(" av1={}x{}@{}", c.av1_row.0, c.av1_row.1, c.av1_row.2)
                } else {
                    String::new()
                };
                let vp9_row = if c.vp9 {
                    format!(" vp9={}x{}@{}", c.vp9_row.0, c.vp9_row.1, c.vp9_row.2)
                } else {
                    String::new()
                };
                let mut c = c;
                plx_base::eventlog::log(&format!("devcaps: table audio rows: {}", table_audio_rows(&s)));
                c.vp9_bit_depth = vp9_bit_depth_for(crate::tv::device::info().major);
                // Before the exclusions: a codec nothing can decode is not a candidate to limit.
                let (gst_dropped, gst_unlisted) = match std::fs::read_to_string(GST_CONF) {
                    Ok(conf) => apply_gst_decoders(&mut c, &conf),
                    Err(_) => (Vec::new(), Vec::new()),
                };
                plx_base::eventlog::log(&format!(
                    "devcaps: gstcool dropped={gst_dropped:?} kept-unlisted={gst_unlisted:?}"
                ));
                plx_base::eventlog::log(&format!(
                    "devcaps: hevc={} {}x{} vp9={} av1={} audio={} rows: h264={}x{}@{} hevc={}x{}@{}{vp9_row}{av1_row} vp9bits={} (device table)",
                    c.hevc,
                    c.hevc_max.0,
                    c.hevc_max.1,
                    c.vp9,
                    c.av1,
                    c.audio,
                    c.h264_row.0,
                    c.h264_row.1,
                    c.h264_row.2,
                    c.hevc_row.0,
                    c.hevc_row.1,
                    c.hevc_row.2,
                    c.vp9_bit_depth
                ));
                c
            }
            None => {
                plx_base::eventlog::log(&format!(
                    "devcaps: {CAPS_TABLE} unparseable — assuming the 49SM9000PLA profile"
                ));
                Caps::assumed()
            }
        },
        Err(e) => {
            plx_base::eventlog::log(&format!(
                "devcaps: {CAPS_TABLE} unreadable ({e}) — assuming the 49SM9000PLA profile"
            ));
            Caps::assumed()
        }
    };
    let _ = MEASURED.set(measured);
    let info = crate::tv::device::info();
    let (wide, learn, forget) = (
        plx_base::devtrig::flag("nativeaudiowide"),
        plx_base::devtrig::flag("nativeaudiolearn"),
        plx_base::devtrig::flag("nativeaudioforget"),
    );
    // The layers in order: the table, minus gstcool.conf's no-decoder codecs (above), minus
    // [`EXCLUSIONS`]. The refusals this install learned come after, from the stored record and from
    // the pipeline itself ([`Layers::fold`]). The debug flags skip the last two.
    let mut boot = caps.clone();
    let excluded = apply_exclusions(&mut boot, &info.codename, wide, EXCLUSIONS);
    plx_base::eventlog::log(&format!(
        "devcaps: firmware={:?} build={:?} audio layers: table+gst={} | exclusions: {} | -> audio={}{}{}",
        info.codename,
        info.build,
        caps.audio,
        if excluded.is_empty() { "none".to_string() } else { excluded.join(" ") },
        boot.audio,
        match (wide, learn) {
            (true, true) => " (nativeaudiowide: exclusions ignored; nativeaudiolearn: refusals still learned)",
            (true, false) => " (nativeaudiowide: exclusions and learned entries ignored)",
            _ => "",
        },
        if forget { " (nativeaudioforget: stored refusals ignored this run)" } else { "" },
    ));
    let _ = BOOT.set(Layers::new(boot, info.codename.clone(), info.build.clone(), wide, learn, forget));
}

/// Whether [`caps`] reflects THIS set's table or the assumed profile — see [`MEASURED`].
pub fn measured() -> bool {
    *MEASURED.get().unwrap_or(&false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_learn_switch_keeps_the_learned_layer_under_the_wide_flag() {
        assert!(!ignores_learned(false, false));
        assert!(!ignores_learned(false, true));
        assert!(ignores_learned(true, false));
        assert!(!ignores_learned(true, true));
    }

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
        // the dev set's VP9 row is its own bound, not folded into the shared one
        assert_eq!(c.vp9_row, (4096, 2304, 60));
        // H.265 (4096x2304) merged with HEVC (4096x2176) by min — NOT either row verbatim.
        assert_eq!(c.hevc_max, (4096, 2176));
        // the per-codec rows, frame rate included: H.264 is one row; "H.265"+"HEVC" MIN-merge
        // to 4096x2176@60 (the 120 on the H.265 row loses to the HEVC row's 60)
        assert_eq!(c.h264_row, (4096, 2304, 60));
        assert_eq!(c.hevc_row, (4096, 2176, 60));
        // a table with no AV1 row leaves the AV1 capability off, with no bound stated
        assert!(!c.av1 && c.av1_row == (0, 0, 0));
        // a fractional cap is a CEILING the stream must fit under: 59.94 reads as 60, never 59
        let frac = parse(r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088,"maxFrameRate":59.94}]}"#).unwrap();
        assert_eq!(frac.h264_row, (1920, 1088, 60));
        // MPEG is in the table but not in the pipeline's decode set; the subset keeps
        // DP_AUDIO_CODECS's own order, not the table's. Opus has no row and is admitted anyway,
        // and the app-decoded PCM formats follow.
        assert_eq!(c.audio, "aac,ac3,eac3,dts,opus,flac");
    }

    /// Opus is a pipeline capability: admitted on a table without its row, but never on the assumed
    /// fallback, and a table whose rows name no table-gated codec still falls back whole.
    #[test]
    fn opus_rides_on_a_read_table_only() {
        assert!(parse(REAL).unwrap().audio_has("opus"));
        assert!(!Caps::assumed().audio_has("opus"));
        assert!(parse(REAL).unwrap().forced_feeds_audio("opus"));
        assert!(!Caps::assumed().forced_feeds_audio("opus"), "Forced gates Opus on the table too");
        // Auto takes stereo Opus only; a wider track goes to the server, even when a table row
        // claims more (#918). Forced keeps no ceiling.
        let real = parse(REAL).unwrap();
        assert!(real.audio_supports("opus", 2) && real.audio_supports("opus", 1));
        assert!(!real.audio_supports("opus", 6), "surround Opus is the server's in Auto");
        assert_eq!(real.audio_channels.get("opus"), Some(&2));
        let claims_six = r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1080}],
            "audioCodecs":[{"name":"AAC","channels":6},{"name":"OPUS","channels":6}]}"#;
        assert_eq!(parse(claims_six).unwrap().audio_channels.get("opus"), Some(&2));
        assert!(!Caps::assumed().audio_channels.contains_key("opus"));
        let no_audio = r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1080}],
            "audioCodecs":[{"name":"WMA","channels":2}]}"#;
        assert_eq!(parse(no_audio).unwrap().audio, Caps::assumed().audio);
        assert_eq!(table_audio_rows(REAL), "MPEG/2,AAC/6,AC3/6,EAC3/8,DTS/6,FLAC/6");
    }

    /// The dev table: FLAC listed at six channels, Opus admitted by the pipeline (Auto: stereo).
    fn native_set() -> Caps {
        parse(REAL).unwrap()
    }

    #[test]
    fn an_unknown_firmware_family_announces_what_its_table_lists() {
        for codename in ["", "goldilocks2-grampians", "some-future-build"] {
            let mut c = native_set();
            assert!(apply_exclusions(&mut c, codename, false, EXCLUSIONS).is_empty(), "{codename:?}");
            assert_eq!(c, native_set(), "{codename:?}");
            assert!(c.audio_supports("opus", 2));
            assert!(c.forced_feeds_audio_track("opus", 6), "no row, no ceiling");
        }
    }

    #[test]
    fn the_c1s_firmware_family_loses_the_combinations_measured_broken() {
        let mut c = native_set();
        let applied = apply_exclusions(&mut c, "kisscurl-koli", false, EXCLUSIONS);
        // The Vorbis row has nothing to take away: the pipeline's list does not feed Vorbis at all.
        assert_eq!(applied, ["opus<=2"]);
        assert_eq!(c.audio, "aac,ac3,eac3,dts,opus,flac");
        assert!(c.audio_supports("opus", 2) && !c.audio_supports("opus", 6));
        // Force Direct Play applies no table ceiling, but it holds a measured one.
        assert!(c.forced_feeds_audio_track("opus", 2) && !c.forced_feeds_audio_track("opus", 6));
        assert!(c.forced_feeds_audio_track("eac3", 8), "a codec without a row keeps its freedom");
        // The rest of the table is untouched.
        assert!(c.audio_supports("eac3", 8) && c.audio_supports("dts", 6) && c.audio_supports("aac", 6));
        // The codename compares without regard to case.
        let mut d = native_set();
        assert_eq!(apply_exclusions(&mut d, "Kisscurl-Koli", false, EXCLUSIONS).len(), 1);
    }

    /// `without` is the learned-refusal rule, applied to a copy and nothing else: no global is read
    /// or written, so the value a recovery flight plans from can be derived before anything is proved.
    #[test]
    fn without_an_audio_facet_narrows_a_copy_by_the_learned_refusal_rule() {
        let base = native_set();
        let facet = |codec: &str, channels: u32| Facet::Audio { codec: codec.into(), channels };

        // A surround facet keeps the codec to stereo (what `apply_learned` does for a 6-channel refusal).
        let surround = base.without(&facet("dts", 6));
        assert!(!surround.audio_supports("dts", 6) && surround.audio_has("dts"));
        assert_eq!(surround.audio_channels.get("dts"), Some(&2));
        // A stereo facet takes the codec away, whatever spelling named it.
        let stereo = base.without(&facet("DCA", 2));
        assert!(!stereo.audio_has("dts") && !stereo.audio_supports("dts", 2));
        assert!(!stereo.forced_feeds_audio("dts"), "Force Direct Play offers nothing it cannot take either");
        // The same rule as a refusal the install learned.
        let mut learned = native_set();
        apply_learned(&mut learned, "kisscurl-koli", false, &[refusal("dts", 2)]);
        assert_eq!(stereo, learned);

        // Only the facet moved: the source value is a different, untouched object, and the rest of it holds.
        assert_eq!(base, native_set());
        assert!(stereo.audio_supports("eac3", 8) && stereo.audio_supports("ac3", 6));
        // A codec this value never announced is already without: the copy equals the source.
        assert_eq!(base.without(&facet("truehd", 8)), base);
        // Monotone and idempotent, like the fold.
        assert_eq!(stereo.without(&facet("dts", 2)), stereo);
    }

    #[test]
    fn the_debug_flag_ignores_the_exclusions() {
        let mut c = native_set();
        assert!(apply_exclusions(&mut c, "kisscurl-koli", true, EXCLUSIONS).is_empty());
        assert_eq!(c, native_set());
    }

    #[test]
    fn an_exclusion_only_takes_away() {
        // A table row of two channels is not raised to the exclusion's four; a codec the table
        // never listed is not added.
        let table = [
            Exclusion { codename: "x", codec: "opus", max_channels: 4, evidence: "" },
            Exclusion { codename: "x", codec: "truehd", max_channels: 2, evidence: "" },
        ];
        let mut c = parse(&REAL.replace(
            "\"name\" : \"FLAC\"",
            "\"name\" : \"OPUS\", \"channels\" : 2 }, { \"name\" : \"FLAC\"",
        ))
        .unwrap();
        assert_eq!(apply_exclusions(&mut c, "x", false, &table), ["opus<=2"]);
        assert_eq!(c.audio_channels.get("opus"), Some(&2));
        assert!(!c.audio_has("truehd"));
    }

    fn refusal(codec: &str, channels: u32) -> Refusal {
        Refusal { firmware: "kisscurl-koli".into(), codec: codec.into(), channels, build: String::new() }
    }

    #[test]
    fn a_learned_refusal_of_a_stereo_track_takes_the_codec_away_and_a_surround_one_keeps_stereo() {
        let mut c = native_set();
        let applied = apply_learned(&mut c, "kisscurl-koli", false, &[refusal("opus", 2), refusal("eac3", 8)]);
        assert_eq!(applied, ["opus=none", "eac3<=2"]);
        assert!(!c.audio_has("opus") && !c.forced_feeds_audio("opus"));
        assert!(c.audio_supports("eac3", 2) && !c.audio_supports("eac3", 6));
        assert!(!c.forced_feeds_audio_track("eac3", 6), "Force holds a learned ceiling too");
        // Another firmware's lessons, an unannounced codec and the debug flag do nothing.
        for (name, ignore) in [("goldilocks2-grampians", false), ("", false), ("kisscurl-koli", true)] {
            let mut d = native_set();
            assert!(apply_learned(&mut d, name, ignore, &[refusal("opus", 2)]).is_empty(), "{name:?} {ignore}");
            assert_eq!(d, native_set());
        }
        let mut e = Caps::assumed();
        assert!(apply_learned(&mut e, "kisscurl-koli", false, &[refusal("opus", 6)]).is_empty(), "the fallback never announced Opus");
    }

    #[test]
    fn the_layers_stack_in_order_table_gst_exclusions_learned() {
        // gstcool.conf drops DTS; the C1 row bounds Opus to stereo; a learned refusal then removes
        // the codec the row left (stereo Opus).
        let mut c = native_set();
        assert_eq!(apply_gst_decoders(&mut c, &GST_C1.replace("dts_audiodec=290\n", "")).0, ["dts"]);
        assert_eq!(apply_exclusions(&mut c, "kisscurl-koli", false, EXCLUSIONS), ["opus<=2"]);
        assert_eq!(apply_learned(&mut c, "kisscurl-koli", false, &[refusal("opus", 2)]), ["opus=none"]);
        assert_eq!(c.audio, "aac,ac3,eac3,flac");
    }

    fn layers_on(build: &str) -> Layers {
        Layers::new(native_set(), "kisscurl-koli".into(), build.into(), false, false, false)
    }

    fn announced(layers: &Layers, codec: &str, channels: i64) -> bool {
        layers.caps.read().unwrap().audio_supports(codec, channels)
    }

    /// The answer only ever loses native audio: folding is idempotent, a list that is behind (an
    /// older record, an empty one) gives nothing back, and a snapshot a reader already holds is not
    /// touched by the next fold.
    #[test]
    fn folding_refusals_only_ever_takes_native_audio_away() {
        let layers = layers_on("6.5.3-47");
        let before = layers.caps.read().unwrap().clone();
        let stereo_opus = Refusal { build: "6.5.3-47".into(), ..refusal("opus", 2) };
        assert!(layers.fold(&[stereo_opus.clone()]));
        assert!(!announced(&layers, "opus", 2));
        assert!(!layers.fold(&[stereo_opus.clone()]), "the same lesson is not news");
        assert!(!layers.fold(&[]), "an empty record removes nothing");
        assert!(!announced(&layers, "opus", 2), "and gives nothing back");
        assert!(before.audio_has("opus"), "the snapshot a reader held is unchanged");
        // A surround refusal keeps the codec to stereo.
        assert!(layers.fold(&[Refusal { build: "6.5.3-47".into(), ..refusal("eac3", 6) }]));
        assert!(announced(&layers, "eac3", 2) && !announced(&layers, "eac3", 6));
    }

    /// An LG update replaces the decoder a refusal was watched on: another build's entry, and one an
    /// older record never tagged, is no evidence on this build, while a set that did not name its
    /// build judges nothing and takes every entry.
    #[test]
    fn a_refusal_is_evidence_only_on_the_build_it_was_watched_on() {
        let layers = layers_on("6.5.3-47");
        for stale in ["6.5.3-48", ""] {
            assert!(!layers.fold(&[Refusal { build: stale.into(), ..refusal("opus", 2) }]), "{stale:?}");
            assert!(announced(&layers, "opus", 2));
        }
        let blind = layers_on("");
        assert!(blind.fold(&[Refusal { build: "6.5.3-48".into(), ..refusal("opus", 2) }]));
        assert!(!announced(&blind, "opus", 2));
        assert!(refusal("opus", 2).watched_on("") && !refusal("opus", 2).watched_on("6.5.3-47"));
    }

    /// The debug flags: `nativeaudiowide` ignores every refusal, `nativeaudiolearn` beside it keeps
    /// them, and `nativeaudioforget` ignores the stored record but not a refusal learned this run.
    #[test]
    fn the_debug_flags_decide_which_refusals_apply() {
        let fresh = |wide, learn, forget| Layers::new(native_set(), "kisscurl-koli".into(), String::new(), wide, learn, forget);
        assert!(!fresh(true, false, false).fold(&[refusal("opus", 2)]));
        assert!(fresh(true, true, false).fold(&[refusal("opus", 2)]));
        assert!(fresh(false, false, true).fold(&[refusal("opus", 2)]), "learned this run, it applies");
    }

    /// Learning: the thread's set names its firmware, so a refusal changes its answer at once, is
    /// handed back once for the caller to persist, and is not news the second time (a conversion
    /// that still feeds it natively ends in the ordinary failure instead of looping). A set with no
    /// firmware name, or no table read, learns nothing.
    #[test]
    fn a_learned_refusal_changes_the_answer_once() {
        {
            let _set = firmware_on_this_thread_for_test("kisscurl-koli", "6.5.3-47", native_set());
            assert!(caps().audio_supports("opus", 2), "sanity: announced");
            let learned = learn_refusal("Opus", 2).expect("news");
            assert_eq!(learned, Refusal { firmware: "kisscurl-koli".into(), codec: "opus".into(), channels: 2, build: "6.5.3-47".into() });
            assert!(!caps().audio_has("opus"));
            assert_eq!(learn_refusal("opus", 2), None, "known");
            assert_eq!(learn_refusal("wmapro", 6), None, "never announced, nothing to take away");
            assert!(!project_stored_refusals(&[learned]), "the record catching up changes nothing");
        }
        let _nameless = caps_on_this_thread_for_test(native_set());
        assert_eq!(learn_refusal("opus", 2), None, "no firmware name: nothing can be learned about this set");
        assert!(caps().audio_supports("opus", 2));
    }

    /// The C1's real `/etc/gst/gstcool.conf` (webOS 6.5.3, `kisscurl-koli`), read over ssh 2026-10-21;
    /// the license, debug and buffering sections are trimmed.
    const GST_C1: &str = "[buffering]\nmax-size-buffers=0\n\n[rank]\nmpegpsdemux=310\naudio_sink=0\ndecproxy=300\naudiodec=0\naudio_dec=0\nac4_dec=0\nomx_lxvideosink=256\nomx_lxdumpsink=0\nomx_lxvideodec=290\nalsasink=0\npulsesink=0\nmediainfo=64\nosssink=0\noss4sink=0\naac_audiodec=290\nac3_audiodec=290\nac4_audiodec=290\nadpcm_audiodec=290\namr_audiodec=290\ndts_audiodec=290\nflac_audiodec=290\nmpegh_audiodec=290\npcm_audiodec=290\nvorbis_audiodec=290\nwma_audiodec=290\navenc_ac3=0\navenc_eac3=0\n\n[sw_decoder]\navdec_mp3=0\navdec_aac=0\navdec_ac3=0\navdec_dca=0\navdec_alac=0\navdec_amrnb=0\navdec_amrwb=0\navdec_eac3=0\nwavpackdec=0\navdec_wmapro=0\navdec_wmav1=0\navdec_wmav2=0\navdec_wmavoice=0\nalawdec=0\nmulawdec=0\navdec_flac=0\nadpcmdec=0\nfaad=0\ndvdlpcmdec=0\nvorbisdec=0\n\n[sw_video_decoder]\navdec_h264=0\n\n[default_sink]\nvideo=omx_lxvideosink\naudio=audiosink\n";

    #[test]
    fn the_c1s_gstcool_conf_drops_nothing_and_keeps_opus_unlisted() {
        let mut c = native_set();
        let before = c.clone();
        let (dropped, unlisted) = apply_gst_decoders(&mut c, GST_C1);
        assert!(dropped.is_empty(), "{dropped:?}");
        // The file names no Opus decoder at all: no line means the default rank, not unsupported.
        assert_eq!(unlisted, ["opus"]);
        assert_eq!(c, before);
        assert!(c.audio_has("opus") && c.audio_supports("dts", 6));
    }

    #[test]
    fn a_set_with_no_dts_decoder_loses_dts_and_its_forced_evidence() {
        // The C5 shape (#899): the table lists DTS, gstcool.conf has no hardware dts_audiodec and
        // switches avdec_dca off.
        let conf = GST_C1.replace("dts_audiodec=290\n", "");
        let mut c = native_set();
        let (dropped, _) = apply_gst_decoders(&mut c, &conf);
        assert_eq!(dropped, ["dts"]);
        assert!(!c.audio_has("dts") && !c.audio_supports("dts", 6));
        // The table row goes with it, so a rule that asks for a DTS row (Force Direct Play) agrees.
        assert!(!c.audio_channels.contains_key("dts"));
        assert_eq!(c.audio, "aac,ac3,eac3,opus,flac");
        // A hardware element listed at rank 0 is the same evidence.
        let mut d = native_set();
        let off = GST_C1.replace("dts_audiodec=290", "dts_audiodec=0");
        assert_eq!(apply_gst_decoders(&mut d, &off).0, ["dts"]);
    }

    #[test]
    fn a_dts_restore_patched_set_keeps_dts() {
        // webosbrew's dts_restore gives the software decoder a rank again.
        let conf = GST_C1.replace("dts_audiodec=290\n", "").replace("avdec_dca=0", "avdec_dca=290");
        let mut c = native_set();
        let before = c.clone();
        let (dropped, _) = apply_gst_decoders(&mut c, &conf);
        assert!(dropped.is_empty());
        assert_eq!(c, before);
    }

    #[test]
    fn a_missing_empty_or_unreadable_gstcool_conf_changes_nothing() {
        for conf in ["", "\n\n", "garbage", "[rank]\nnot a line\n=\n=5\nx=notanumber\n", "[sw_decoder]\navdec_dca\n"] {
            let mut c = native_set();
            let before = c.clone();
            let (dropped, _) = apply_gst_decoders(&mut c, conf);
            assert!(dropped.is_empty(), "{conf:?}");
            assert_eq!(c, before, "{conf:?}");
        }
    }

    #[test]
    fn only_rank_and_sw_decoder_sections_count_and_the_last_line_wins() {
        let ranks = parse_gst_ranks("[other]\navdec_dca=0\n[rank]\n avdec_dca = 5 \n; comment\n[sw_decoder]\navdec_dca=0\nvorbisdec=-1\n");
        assert_eq!(ranks.get("avdec_dca"), Some(&0));
        assert_eq!(ranks.get("vorbisdec"), Some(&-1));
        assert_eq!(ranks.len(), 2);
    }

    #[test]
    fn a_codec_with_a_listed_working_decoder_is_kept_whatever_the_others_say() {
        // Only the software element has rank, the hardware one is off: still decodable.
        let conf = "[rank]\naac_audiodec=0\n[sw_decoder]\navdec_aac=250\nopusdec=0\n";
        let mut c = native_set();
        let (dropped, unlisted) = apply_gst_decoders(&mut c, conf);
        assert_eq!(dropped, ["opus"]);
        assert!(c.audio_has("aac") && !c.audio_has("opus"));
        assert!(unlisted.contains(&"dts"));
    }

    /// PMS names DTS `dca` (every DTS entry in the profiles shipped with PMS spells it so); the
    /// pipeline and the device table say `dts`. Both spellings must reach the same answer.
    #[test]
    fn pms_spelling_dca_is_dts_for_membership_and_channel_bounds() {
        let caps = parse(REAL).unwrap();
        assert!(caps.audio_supports("dca", 6));
        assert!(!caps.audio_supports("dca", 8));
        assert!(!caps.audio_supports("dca", 0));
        assert!(caps.audio_supports("DCA", 6), "case folds too");
        assert!(caps.audio_supports("ec-3", 8), "ec-3 is eac3");
        assert!(!Caps::assumed().audio_has("dca"));
        assert_eq!(audio_codec_id("DCA"), "dts");
        assert_eq!(audio_codec_id("AC3"), "ac3");
    }

    /// Forced mode admits DTS only on table evidence (a DTS row with channels), but skips the
    /// channel ceiling: 7.1 DTS-HD MA is refused in Auto and admitted in Forced.
    #[test]
    fn forced_dts_needs_a_table_row_but_no_channel_ceiling() {
        let caps = parse(REAL).unwrap();
        assert!(caps.forced_feeds_audio("dca"));
        assert!(caps.forced_feeds_audio("dts"));
        assert!(!caps.audio_supports("dca", 8));
        assert!(!Caps::assumed().forced_feeds_audio("dca"));
        assert!(!Caps::assumed().forced_feeds_audio("dts"));
        let no_row = parse(&REAL.replace("DTS", "WMA")).unwrap();
        assert!(!no_row.forced_feeds_audio("dca"));
        for c in ["aac", "ac3", "eac3", "EAC3"] {
            assert!(Caps::assumed().forced_feeds_audio(c), "{c}");
        }
        assert!(!Caps::assumed().forced_feeds_audio("truehd"));
    }

    /// `pcmInfo` can name only the rates in libpf's table (plus 44.1 kHz): a 96 kHz or 192 kHz FLAC
    /// played audibly slow on the B8, with no error anywhere. The track gate refuses those, and an
    /// unstated rate or channel count, for app-decoded codecs only.
    #[test]
    fn the_pcm_path_admits_only_the_rates_and_channels_it_plays_correctly() {
        for rate in [8_000, 22_050, 44_100, 48_000] {
            assert!(pcm_track_fits("flac", 2, rate), "{rate}");
        }
        assert!(pcm_track_fits("FLAC", 1, 48_000));
        for rate in [0, 11_025, 88_200, 96_000, 176_400, 192_000] {
            assert!(!pcm_track_fits("flac", 2, rate), "{rate} plays at the wrong speed or is unknown");
        }
        assert!(!pcm_track_fits("flac", 6, 48_000));
        assert!(!pcm_track_fits("flac", 0, 48_000));
        // not app-decoded: the hardware decoder's own rules apply, not these
        assert!(pcm_track_fits("ac3", 6, 0) && pcm_track_fits("dca", 8, 96_000));
    }

    /// FLAC is decoded in the app and fed as PCM: it needs no table row, but it does need a
    /// table that was READ (the assumed fallback must stay exactly what shipped), a stated channel
    /// count, and at most the PCM path's two channels — a 5.1 FLAC stays a server conversion. The
    /// table's own FLAC row (6 channels on the dev set) says nothing about our decoder.
    #[test]
    fn flac_is_pcm_decoded_stereo_only_and_only_on_a_read_table() {
        let caps = parse(REAL).unwrap();
        assert!(caps.audio_has("flac") && caps.audio_has("FLAC"));
        assert!(caps.audio_supports("flac", 1));
        assert!(caps.audio_supports("flac", 2));
        assert!(!caps.audio_supports("flac", 6));
        assert!(!caps.audio_supports("flac", 0), "an unknown channel count is not admitted");
        assert_eq!(caps.audio_channels.get("flac"), Some(&PCM_MAX_CHANNELS));
        let no_row = parse(&REAL.replace("\"name\" : \"FLAC\"", "\"name\" : \"WMA\"")).unwrap();
        assert!(no_row.audio_supports("flac", 2), "the PCM path does not need a FLAC decoder row");
        assert!(!Caps::assumed().audio_has("flac"));
        assert_eq!(Caps::assumed().audio, "aac,ac3,eac3");
        assert!(caps.forced_feeds_audio("flac"));
        assert!(!Caps::assumed().forced_feeds_audio("flac"));
    }

    #[test]
    fn dts_requires_an_explicit_device_row() {
        assert!(parse(REAL).unwrap().audio_has("dts"));
        assert!(!Caps::assumed().audio_has("dts"));
        assert!(!parse(&REAL.replace("DTS", "WMA")).unwrap().audio_has("dts"));
    }

    #[test]
    fn audio_limits_retain_and_merge_the_devices_channel_bounds() {
        let caps = parse(REAL).unwrap();
        assert!(caps.audio_supports("dts", 6));
        assert!(!caps.audio_supports("dts", 8));
        assert!(!caps.audio_supports("dts", 0));
        assert!(caps.audio_supports("aac", 2));
        assert!(caps.audio_supports("aac", 6));
        assert!(!caps.audio_supports("aac", 8));
        assert!(caps.audio_supports("eac3", 8));
        let merged = parse(&REAL.replace("\"name\" : \"FLAC\"", "\"name\" : \"EAC3\"" )).unwrap();
        assert!(!merged.audio_supports("eac3", 8));
        let unbounded = parse(&REAL.replace("\"channels\" : 6", "\"channels\" : 0")).unwrap();
        assert!(!unbounded.audio_has("dts"));
        assert!(!Caps::assumed().audio_supports("truehd", 6));
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
        assert_eq!(c.audio, "aac,opus,flac");
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
        assert_eq!(c.audio, "aac,ac3,eac3,opus,flac");
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
        assert_eq!(c.audio, "aac,ac3,eac3");
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

    /// An AV1 row is a capability bit with its own bound, and it does not move the shared
    /// bound: `hevc_max` is what the table says without the AV1 row. The row's nested
    /// `restrictions` array and `maxBitRate` are unknown fields to this parser and must parse
    /// through without touching the row's own dimensions. Synthetic table, not a device file.
    #[test]
    fn an_av1_row_sets_the_av1_capability_and_its_bound() {
        let base = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60},
                {"name":"HEVC","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60}
            ]}"#;
        let with_av1 = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60},
                {"name":"HEVC","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60},
                {"name":"AV1","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60,
                 "maxBitRate":50,
                 "restrictions":[{"mode":"x","maxWidth":1920,"maxHeight":1088,"maxFrameRate":60}]}
            ]}"#;
        let c = parse(with_av1).expect("a table with an AV1 row parses");
        assert!(c.av1);
        assert_eq!(c.av1_row, (3840, 2160, 60));
        assert_eq!(c.hevc_max, parse(base).unwrap().hevc_max);
    }

    /// Duplicate AV1 rows MIN-merge per axis like the other decoders' duplicates do: the
    /// roomier 8K row cannot win an axis back from the 4K one.
    #[test]
    fn duplicate_av1_rows_merge_to_the_minimum() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080},
                {"name":"AV1","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60},
                {"name":"AV1","maxWidth":7680,"maxHeight":4320,"maxFrameRate":30}
            ]}"#,
        )
        .unwrap();
        assert!(c.av1);
        assert_eq!(c.av1_row, (3840, 2160, 30));
    }

    /// The AV1 row is not an H.264 row: a table with AV1 and no H.264 is rejected whole, the
    /// same rule every table without H.264 follows, so AV1 can never be the only thing a
    /// misread table claims.
    #[test]
    fn an_av1_only_table_is_still_rejected() {
        assert!(parse(r#"{"videoCodecs":[{"name":"AV1","maxWidth":3840,"maxHeight":2160}]}"#).is_none());
    }

    /// The fallback never claims AV1: an unreadable table, or a table that lacks the row, keeps
    /// AV1 a server transcode, exactly as before this capability existed.
    #[test]
    fn assumed_never_claims_av1() {
        let c = Caps::assumed();
        assert!(!c.av1);
        assert_eq!(c.av1_row, (0, 0, 0));
    }

    /// A VP9 row is a capability bit with its own bound, like AV1's: duplicate rows MIN-merge per
    /// axis, and it never moves the shared bound (`hevc_max`) the other codecs derive from.
    #[test]
    fn a_vp9_row_sets_the_vp9_capability_and_its_bound() {
        let base = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60}
            ]}"#;
        let with_vp9 = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60},
                {"name":"VP9","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60},
                {"name":"VP9","maxWidth":4096,"maxHeight":2304,"maxFrameRate":30}
            ]}"#;
        let c = parse(with_vp9).expect("a table with a VP9 row parses");
        assert!(c.vp9);
        assert_eq!(c.vp9_row, (3840, 2160, 30));
        assert_eq!(c.hevc_max, parse(base).unwrap().hevc_max);
        let without = parse(base).unwrap();
        assert!(!without.vp9 && without.vp9_row == (0, 0, 0));
    }

    /// The fallback never claims VP9 either: VP9 direct play is gated on the table's own row, so
    /// an unreadable table keeps VP9 a server job, as it was before the gate existed.
    #[test]
    fn assumed_never_claims_vp9() {
        let c = Caps::assumed();
        assert!(!c.vp9);
        assert_eq!(c.vp9_row, (0, 0, 0));
    }

    /// 10-bit VP9 is claimed only on the webOS 4 family (the ACB path it was measured on); an unknown
    /// release, webOS 5+ and the assumed profile all say 8.
    #[test]
    fn vp9_bit_depth_is_ten_only_where_it_was_measured() {
        assert_eq!(vp9_bit_depth_for(0), 8, "unknown release");
        for major in 1..=4 {
            assert_eq!(vp9_bit_depth_for(major), 10, "webOS {major}");
        }
        for major in [5, 6, 7, 8, 9, 10, 11, 25] {
            assert_eq!(vp9_bit_depth_for(major), 8, "webOS {major}");
        }
        assert_eq!(Caps::assumed().vp9_bit_depth, 8);
        assert_eq!(parse(REAL).unwrap().vp9_bit_depth, 8, "parse() alone never claims it; probe() sets it");
    }
}
