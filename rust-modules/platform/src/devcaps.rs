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
/// `plex::is_dp_audio_track_with` for membership, channel bounds and sampling rate
/// ([`audio_track_fits`]), shared with the device profile.
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
/// carries it. FLAC is the case, and ALAC and LPCM ride the same path ([`APP_DECODED`]) — libpf has no FLAC input of its own (its Load vocabulary is
/// AAC/AC3/AC3 PLUS/DTS/MP3/MPEG/OPUS/VORBIS/WMA/PCM), but PCM decoded from it is bit-exact at a rate of [`PCM_SAMPLE_RATES`]
/// (above them the app resamples, [`pcm_load_rate`]). Both
/// the vocabulary and the PCM input were verified on webOS 4.4.3 (libpf-1.0 on an OLED55B8); other
/// webOS versions are unconfirmed. Joins [`Caps::audio`] only on a table that was READ: the assumed
/// fallback stays byte-for-byte what it was.
pub const PCM_AUDIO_CODECS: &str = "flac,alac,pcm";

/// One codec the app decodes itself: its [`audio_codec_id`], the names `avcodec_get_name` gives the
/// streams it covers (what the demuxer sees), and the decoders `ci/build-ffmpeg.sh` must enable to
/// decode them. The two differ for DTS, whose codec is `dts` and whose decoder is `dca`.
pub struct AppDecoded {
    pub id: &'static str,
    pub codecs: &'static [&'static str],
    pub decoders: &'static [&'static str],
    /// Lossy: the decode is NOT bit-exact and is fed as stereo 48 kHz S16 whatever the source
    /// (see [`Caps::app_decodes`]). Only on a value whose [`Caps::app_lossy`] policy admits the row
    /// ([`lossy_row_enabled`]); [`PCM_AUDIO_CODECS`] is the lossless entries.
    pub lossy: bool,
    /// The TV evidence that this row's decode is affordable (the CPU it costs, in the style of
    /// [`Exclusion::evidence`]). A lossy row without it is not enabled on a table that was read
    /// ([`AppLossy::Measured`]); turning a row on is `Some(..)` plus its evidence. Always `None` for a
    /// lossless row, where the field means nothing.
    pub measured: Option<&'static str>,
}

/// The app-decoded set, and the ONLY place a decoder name is tied to a codec: the demuxer asks
/// [`app_decoded_by_ffmpeg_name`], and a test holds every decoder here to `ci/build-ffmpeg.sh`'s
/// `--enable-decoder` list, so a codec cannot be offered that the bundled library cannot decode.
pub const APP_DECODED: &[AppDecoded] = &[
    AppDecoded { id: "flac", codecs: &["flac"], decoders: &["flac"], lossy: false, measured: None },
    AppDecoded { id: "alac", codecs: &["alac"], decoders: &["alac"], lossy: false, measured: None },
    // LPCM in MKV (`A_PCM/INT/LIT`, `/BIG`) and MP4 (`sowt`, `twos`, `in24`, `in32`). The integer
    // widths only: a float or 8-bit track is not claimed ([`audio_codec_id`]), because the
    // path is bit-exact and the Load declares 16 or 32 bits.
    AppDecoded {
        id: "pcm",
        codecs: &["pcm_s16le", "pcm_s16be", "pcm_s24le", "pcm_s24be", "pcm_s32le", "pcm_s32be"],
        decoders: &["pcm_s16le", "pcm_s16be", "pcm_s24le", "pcm_s24be", "pcm_s32le", "pcm_s32be"],
        lossy: false,
        measured: None,
    },
    // DTS core, downmixed to stereo and resampled to 48 kHz: for a set with no DTS decoder (webOS 10+)
    // and for Force Direct Play there. Enabled on a read table: the TV has measured what it costs.
    AppDecoded {
        id: "dts",
        codecs: &["dts"],
        decoders: &["dca"],
        lossy: true,
        measured: Some(
            "U5 2026-10: DTS core decode on the dev set (webOS 4.5), whole app 13.1% of one core of four, \
             system 25% busy, 60 fps held",
        ),
    },
    // MP3 and Vorbis (S4-4): stereo 48 kHz PCM through the same decoder and the same policy as DTS. Unlike
    // a DTS core they can be above 48 kHz, so they stay rate bound ([`audio_rate_bounded`]). Unmeasured:
    // off on a read table until their U5 evidence is recorded here.
    AppDecoded { id: "mp3", codecs: &["mp3"], decoders: &["mp3"], lossy: true, measured: None },
    AppDecoded { id: "vorbis", codecs: &["vorbis"], decoders: &["vorbis"], lossy: true, measured: None },
    // TrueHD (S4-5): up to eight channels (FFmpeg's `truehd` decodes the 7.1 bed; an Atmos object
    // substream is not rendered), mixed by layout to stereo 48 kHz. The only lossy entry without a rate
    // bound beside DTS, because the app's resampler (`speed_audio::Resampler`) low-passes high rates ([`audio_rate_bounded`]).
    // Unmeasured, like MP3 and Vorbis.
    AppDecoded { id: "truehd", codecs: &["truehd"], decoders: &["truehd"], lossy: true, measured: None },
];

/// Which lossy [`APP_DECODED`] rows a [`Caps`] value may decode in the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppLossy {
    /// None: an unread table is not a policy (the PCM precedent, [`PCM_AUDIO_CODECS`]), so the assumed
    /// fallback stays byte-for-byte what shipped.
    #[default]
    Off,
    /// The rows with recorded evidence ([`AppDecoded::measured`]): what a table that was read gets.
    Measured,
    /// Every lossy row: the `appdecodelossy` debug trigger, which is how an unmeasured row is measured.
    All,
}

/// Whether `policy` admits `row`: the ONE answer to "may the app decode this lossy row".
pub(crate) fn lossy_row_enabled(row: &AppDecoded, policy: AppLossy) -> bool {
    row.lossy
        && match policy {
            AppLossy::Off => false,
            AppLossy::Measured => row.measured.is_some(),
            AppLossy::All => true,
        }
}

/// The [`APP_DECODED`] entry covering the stream whose `avcodec_get_name` is `name`.
pub fn app_decoded_by_ffmpeg_name(name: &str) -> Option<&'static AppDecoded> {
    APP_DECODED.iter().find(|c| c.codecs.contains(&name))
}

/// The channel ceiling of the PCM path: `pcmInfo.channelMode` has `mono` and `stereo` (and
/// `dual`), nothing wider, so a multichannel FLAC stays a server conversion.
pub const PCM_MAX_CHANNELS: u32 = 2;

/// The rates `pcmInfo.sampleRate` can name: libpf's `LUT_SampleRateType` (8, 12, 16, 22.05, 24, 32
/// and 48 kHz) plus 44.1 kHz, its fallback. libpf matches the value as a number of KILOHERTZ
/// (`player::engine`); any value that matches nothing (a rate in hertz, 96 kHz, 192 kHz) silently
/// becomes 44.1 kHz and plays at the wrong speed with no error (a 96 kHz FLAC audibly slow on an
/// OLED55B8, webOS 4.4.3; a 48 kHz one 9% slow, a 32 kHz one fast and stuttering on a C1, webOS
/// 6.5.3, when the rate was declared in hertz). The table was read from libpf-1.0 on the B8. It
/// bounds what the PCM input is DECLARED at, not what a source may be: a faster FLAC, ALAC or LPCM is
/// resampled to its top ([`pcm_load_rate`]). Every other codec's Load carries no rate
/// ([`AUDIO_RATE_CEILING`]).
pub const PCM_SAMPLE_RATES: [u32; 8] = [8_000, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// **The rate the PCM Load declares for a source sampled at `source` Hz**, or `None` when the app cannot
/// make the source into one: the single rule for the PCM path's rate.
///
/// * a rate in [`PCM_SAMPLE_RATES`] is itself: the bit-exact path, no resampler built;
/// * every rate above 48 kHz is resampled by the app (`speed_audio::Resampler`, a linear-phase low-pass
///   decimator) to the top of the table that keeps its family: 44.1 kHz for a multiple of 44.1 kHz
///   (88.2, 176.4, 352.8, 705.6 kHz: an integer ratio), 48 kHz for any other (96, 192, 384, 64, 50 kHz...);
/// * `<= 0` (unstated) and a rate below 48 kHz outside the table (11.025, 37.8 kHz) are `None`: upsampling
///   was not asked for, so those tracks stay with the server.
///
/// **There is no ceiling on the rate.** What is bounded is the converter, not the content that reaches it
/// (`speed_audio::Resampler`: the memory of its filter tables is a constant of the filter, whatever
/// the rate, and its per-input-frame work is constant). The one limit is `i32::MAX`, which is not a cap
/// on a file: FFmpeg states a stream's rate in a C int (`AVCodecParameters::sample_rate`), so a rate past
/// it cannot be one.
///
/// Every `Some(r)` is in [`PCM_SAMPLE_RATES`], so the Load's kilohertz declaration (`player::engine`)
/// needs no new rate. The declared Load is then the shape a 24-bit 48 or 44.1 kHz FLAC already has.
pub fn pcm_load_rate(source: i64) -> Option<u32> {
    let top = PCM_SAMPLE_RATES[PCM_SAMPLE_RATES.len() - 1];
    if !(1..=i64::from(i32::MAX)).contains(&source) {
        return None;
    }
    if let Some(r) = PCM_SAMPLE_RATES.iter().copied().find(|&r| i64::from(r) == source) {
        return Some(r);
    }
    if source < i64::from(top) {
        return None;
    }
    let family = PCM_SAMPLE_RATES[PCM_SAMPLE_RATES.len() - 2];
    Some(if source % i64::from(family) == 0 { family } else { top })
}

/// The highest sample rate, in hertz, any audio Load path here is known to accept: the
/// `audio.samplingRate` upper bound the transcode profile asks PMS for and the one
/// [`audio_track_fits`] holds a codec that [`audio_rate_bounded`] names to. An LG B8 (webOS 4.4.3)
/// refused the Load of a 96 kHz AAC that PMS had made from a 96 kHz FLAC (#951, `smp_cb type=18
/// num=100`). WHERE that limit lives (the Load payload, libpf, the decoder) is not determined; 48 kHz
/// is used because nothing above it is known to load (it is the top of [`PCM_SAMPLE_RATES`] too).
pub const AUDIO_RATE_CEILING: u32 = 48_000;

/// Whether `codec` is held to [`AUDIO_RATE_CEILING`], by [`audio_track_fits`] and by the profile's
/// `audio.samplingRate` limitation alike: every codec but DTS, TrueHD and the lossless PCM codecs
/// ([`PCM_AUDIO_CODECS`]: FLAC, ALAC, LPCM). The app feeds only the DTS core
/// (`ff.rs::dts_core_packet`), 48 kHz at most by format, so a DTS-HD 96/24 source plays today and
/// bounding it would push that source into a lossy AC-3 conversion. TrueHD and the lossless PCM codecs,
/// when the app decodes them, are resampled to 48 or 44.1 kHz by the app itself (`speed_audio::Resampler`
/// low-passes before it decimates from 88.2 kHz and up), so their Load never carries the source rate;
/// bounding them would send every high-resolution track to a lossy server conversion for nothing
/// ([`pcm_load_rate`] is the rule for the PCM codecs). MP3 and Vorbis stay bound: the resampler exists,
/// but a 96 kHz Vorbis has not been through the TV. Opus, AC-3 and E-AC-3 are
/// 48 kHz at most by format, so their bound never fires; they stay in the set so that "every other
/// codec" is the one rule, with no per-codec list to keep in step.
pub fn audio_rate_bounded(codec: &str) -> bool {
    let id = audio_codec_id(codec);
    !matches!(id.as_ref(), "dts" | "truehd") && !PCM_AUDIO_CODECS.split(',').any(|c| c == id)
}

/// Whether `codec` is one the app decodes itself and feeds as PCM ([`PCM_AUDIO_CODECS`]).
pub fn is_pcm_codec(codec: &str) -> bool {
    let id = audio_codec_id(codec);
    PCM_AUDIO_CODECS.split(',').any(|c| c == id)
}

/// The sample-rate (and, for PCM, channel) limits of one audio track, on top of the codec rules in
/// [`Caps::audio_supports`] / [`Caps::forced_feeds_audio`]: the ONE rate-fit predicate for every
/// route that decides without PMS's `/decision`, mirrored by the transcode profile's
/// `audio.samplingRate` limitation. A PCM codec ([`PCM_AUDIO_CODECS`]) needs one or two channels at
/// a rate the app can make a `pcmInfo` rate of ([`pcm_load_rate`]: a table rate, or any faster one,
/// resampled); an unstated count or rate does not pass. Any
/// other codec passes at an unstated rate (`<= 0`, the planner's `0`) or at most
/// [`AUDIO_RATE_CEILING`], DTS and TrueHD excepted ([`audio_rate_bounded`]). These are limits of the Load,
/// not of a hardware decoder, so Force Direct Play applies them as well.
pub fn audio_track_fits(codec: &str, channels: i64, sample_rate: i64) -> bool {
    if is_pcm_codec(codec) {
        (1..=i64::from(PCM_MAX_CHANNELS)).contains(&channels) && pcm_load_rate(sample_rate).is_some()
    } else {
        sample_rate <= i64::from(AUDIO_RATE_CEILING) || !audio_rate_bounded(codec)
    }
}

/// Whether the app's lossy decode of a source with `channels` channels MIXES IT DOWN: the decode's one
/// output is stereo ([`PCM_MAX_CHANNELS`] channels), so a source with more than two channels loses its
/// layout, and one with an unstated count (`<= 0`) is not known to keep it. **The ONE predicate of the
/// owner's rule (2026-10-28, #1082): the app never mixes a surround source down by default.** The
/// planner ([`Caps::app_decodes_lossy_track`]), the failed-open ladder (`player::recovery`'s order of
/// the app decode and the manual encode, and Force Direct Play's rung) and the track menu's "played in
/// stereo" mark all ask this, so none of them can disagree about which tracks are mixed.
pub fn app_decode_mixes_down(channels: i64) -> bool {
    !(1..=i64::from(PCM_MAX_CHANNELS)).contains(&channels)
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
/// (`Session::applied_refusals`, which the play start passes to [`start_caps`]); the row is stamped
/// with [`learning_firmware`]. `channels` is the refused track's channel count. The row's firmware
/// codename stays in the stored record; a BUILD implies its codename, so the build is the only
/// identity applying a row asks ([`Refusal::watched_on`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Refusal {
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

/// What a refusal of a `channels`-channel track leaves of its codec ([`limit_codec`]'s `max`): a
/// stereo (or mono) refusal takes the codec away (`0`), a surround one keeps it to stereo, which is
/// what the static rows do for the same measurements. The one rule for a refusal the install learned
/// ([`Caps::minus`]) and one an attempt only supposes ([`Caps::without`]).
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


/// One stream class an attempt can be told to do without ([`Caps::without`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Facet {
    /// `channels` channels of `codec`, in any spelling [`audio_codec_id`] knows.
    Audio { codec: String, channels: u32 },
    /// A `width`x`height` video of `codec` (PMS's lower-case spelling: `h264`, `hevc`, `vp9`,
    /// `av1`, the names the profile's `videoCodec` lists use). Taking it out lowers that codec's
    /// raster bound to [`VIDEO_REFUSAL_FLOOR`] ([`Caps::video_ceiling`]) when the stream is above
    /// it, so the attempt neither direct plays nor copies a raster the pipeline refused.
    Video { codec: String, width: u32, height: u32 },
}

/// The raster an attempt narrowed by a refused video [`Facet::Video`] is held to: the repo's 1080p
/// bound (the profile's `1920x1088`, the macroblock-aligned height a 1080p-only SoC states; the
/// Load maps 1088 back to 1080).
pub const VIDEO_REFUSAL_FLOOR: (u32, u32) = (1920, 1088);

/// The decode-capability snapshot the playback stack derives from. Probed once, at boot
/// ([`probe`]), and never written again: [`caps`] answers the same value for the life of the process.
/// A play's start value is that snapshot less the install's stored refusals ([`start_caps`]); the
/// attempt carries it (`route::PlaybackSession::attempt_caps`, which the request's `OpenRecovery`
/// owns) and narrows its own copy ([`Caps::without`]) instead of re-reading anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// The SoC decodes HEVC. Gates the `hevc` arm of route.rs's direct-play test and the
    /// hevc entries in BOTH profile codec lists (h264 needs no flag — every webOS SoC has it).
    pub hevc: bool,
    /// The video width/height upper bound — the per-axis MIN across the merged H.264 and HEVC
    /// rows, because the shared consumers apply one bound to EVERY codec at once (each codec's own
    /// bound is [`Caps::raster_bound`], which only ever tightens this one): the profile's
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
    /// Normal routing reads this, the channel ceilings and the rate ceiling ([`audio_track_fits`])
    /// through `plex::is_dp_audio_track_with`, as does its PMS profile. Forced mode starts from the
    /// pipeline's feed set with no channel ceiling and admits DTS only with a DTS row in
    /// `audio_channels` (`Caps::forced_feeds_audio`).
    pub audio: String,
    /// Nonzero, per-codec channel ceilings from the device table, duplicate rows MIN-merged.
    /// Missing legacy limits remain unknown; DTS is admitted only with a measured limit.
    pub audio_channels: std::collections::BTreeMap<String, u32>,
    /// Per-codec channel ceilings that were **measured** on this firmware ([`EXCLUSIONS`]) or
    /// learned from a refusal, as opposed to the table's. Force Direct Play applies no table
    /// ceiling but does honour these ([`Caps::forced_feeds_audio_track`]).
    pub native_ceiling: std::collections::BTreeMap<String, u32>,
    /// Per-codec raster ceilings an attempt was narrowed to by a refused video stream
    /// ([`Caps::without`], [`Facet::Video`]); empty in [`Caps::assumed`] and in a table read, and
    /// never stored on the install (a video facet never proves). Read through
    /// [`Caps::raster_bound`], never directly. A `BTreeMap` keeps `Eq` and deterministic profile bytes.
    pub video_ceiling: std::collections::BTreeMap<String, (u32, u32)>,
    /// Which LOSSY entries of [`APP_DECODED`] this value may decode itself and feed as stereo 48 kHz
    /// PCM, instead of the set's own decoder or the server. [`AppLossy::Off`] in [`Caps::assumed`],
    /// [`AppLossy::Measured`] in a table read, [`AppLossy::All`] under the `appdecodelossy` dev
    /// trigger ([`Caps::with_app_lossy`]).
    pub app_lossy: AppLossy,
    /// The lossy codecs ([`audio_codec_id`] spelling) this value decodes in the app WHATEVER its own
    /// decoder says: set by the recovery rung for the one codec a Load refused
    /// ([`Caps::with_app_lossy_for`]), or for every lossy row by the debug trigger. Empty at boot.
    pub app_lossy_in_force: std::collections::BTreeSet<String>,
}

impl Caps {
    /// The 49SM9000PLA values — the only set ever measured before this module. These reproduce the
    /// profile the app has always sent (the transcoder test pins that), plus the
    /// `audio.samplingRate` limits of #951, so an unreadable table changes nothing else for any
    /// device that was working yesterday.
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
            video_ceiling: Default::default(),
            app_lossy: AppLossy::Off,
            app_lossy_in_force: Default::default(),
        }
    }

    /// Whether `codec` is a lossy [`APP_DECODED`] row this value's policy admits ([`lossy_row_enabled`]).
    /// Says nothing of whether it is in force.
    pub fn app_may_decode_lossy(&self, codec: &str) -> bool {
        let id = audio_codec_id(codec);
        APP_DECODED.iter().any(|r| r.id == id && lossy_row_enabled(r, self.app_lossy))
    }

    /// This value with `codec` decoded in the app whatever the set's decoder says: put in
    /// [`Caps::app_lossy_in_force`], announced whatever the table says (the app decodes it, so no
    /// decoder row is needed), up to eight channels (the output is stereo; the extensions of DTS-HD are
    /// not read). A codec the policy does not admit leaves the value unchanged.
    pub fn with_app_lossy_for(mut self, codec: &str) -> Caps {
        if !self.app_may_decode_lossy(codec) {
            return self;
        }
        let id = audio_codec_id(codec).into_owned();
        if !self.audio_has(&id) {
            self.audio.push(',');
            self.audio.push_str(&id);
        }
        self.audio_channels.insert(id.clone(), 8);
        self.app_lossy_in_force.insert(id);
        self
    }

    /// Whether this value lets the app decode a source with `channels` channels, in the mode `forced`
    /// says (Force Direct Play), WITHOUT mixing it down against the viewer's choice: false only for a
    /// Force Direct Play of a source [`app_decode_mixes_down`] says is mixed down. Force Direct Play
    /// never mixes a surround source down (owner, 2026-10-28, #1082): it plays what the set decodes or
    /// shows the refusal. Auto's last rung may, and so may the explicit `appdecodelossy` debug trigger
    /// ([`AppLossy::All`]), which exists to measure a decode on any file.
    pub fn app_may_mix_down(&self, channels: i64, forced: bool) -> bool {
        !(forced && app_decode_mixes_down(channels)) || self.app_lossy == AppLossy::All
    }

    /// The debug form ([`AppLossy::All`], every lossy row in force): even a codec the set decodes
    /// natively goes through the app, so each row's cost can be measured on a set that has its decoder.
    pub fn with_app_lossy(mut self) -> Caps {
        self.app_lossy = AppLossy::All;
        for r in APP_DECODED.iter().filter(|r| r.lossy) {
            self = self.with_app_lossy_for(r.id);
        }
        self
    }

    /// Whether the app decodes `codec` itself on this value: the lossless set always
    /// ([`PCM_AUDIO_CODECS`]), a lossy codec only once it is in force ([`Caps::app_lossy_in_force`]).
    #[cfg(test)]
    pub fn app_decodes(&self, codec: &str) -> bool {
        is_pcm_codec(codec) || self.app_lossy_in_force.contains(audio_codec_id(codec).as_ref())
    }

    /// The SET'S OWN decoder plays this track from the file (rate and channel limits included): false
    /// for a codec in force, which the app decodes instead. `channels` and `sample_rate` are 0 when not
    /// known. `forced` is Force Direct Play, which applies no table ceiling
    /// ([`Caps::forced_feeds_audio_track`]).
    pub fn native_audio_track(&self, codec: &str, channels: i64, sample_rate: i64, forced: bool) -> bool {
        if self.app_lossy_in_force.contains(audio_codec_id(codec).as_ref()) {
            return false;
        }
        let feeds = if forced {
            self.forced_feeds_audio_track(codec, channels)
        } else {
            self.audio_supports(codec, channels)
        };
        feeds && audio_track_fits(codec, channels, sample_rate)
    }

    /// The app decodes this lossy track to stereo 48 kHz PCM on this value: the policy admits the
    /// row, and the codec is either in force or Force Direct Play has no native path for the track (so
    /// the app replaces what would be a refusal; a set that plays it natively keeps doing so). The
    /// track's own limit is the rate bound of a rate-bound row ([`audio_track_fits`]); the channel
    /// count is no limit of the DECODER (it mixes by layout down to two), but **Force Direct Play
    /// never mixes a surround source down** ([`Caps::app_may_mix_down`], owner 2026-10-28, #1082): there
    /// the app decodes a stereo or mono track only, and a surround one keeps its refusal. Auto's value
    /// with the codec in force (the recovery ladder's last rung) decodes any width.
    pub fn app_decodes_lossy_track(&self, codec: &str, channels: i64, sample_rate: i64, forced: bool) -> bool {
        let id = audio_codec_id(codec);
        self.app_may_decode_lossy(&id)
            && audio_track_fits(&id, channels, sample_rate)
            && self.app_may_mix_down(channels, forced)
            && (self.app_lossy_in_force.contains(id.as_ref())
                || (forced && !self.native_audio_track(&id, channels, sample_rate, true)))
    }

    /// The width/height upper bound for `vcodec` (PMS's lower-case spelling), per axis the lowest
    /// of: the shared [`Caps::hevc_max`], the codec's own table row on each axis it states
    /// (`av1_row`, `vp9_row`; H.264 and HEVC are already folded into `hevc_max`), and the attempt's
    /// [`Caps::video_ceiling`] entry for the codec. The ONE bound the direct-play gate, the
    /// profile's codec limitations, the Auto HLS catalog and the Load envelope take.
    pub fn raster_bound(&self, vcodec: &str) -> (u32, u32) {
        let (mut w, mut h) = self.hevc_max;
        let (rw, rh, _) = match vcodec {
            "av1" => self.av1_row,
            "vp9" => self.vp9_row,
            _ => (0, 0, 0),
        };
        if rw > 0 {
            w = w.min(rw);
        }
        if rh > 0 {
            h = h.min(rh);
        }
        if let Some(&(cw, ch)) = self.video_ceiling.get(vcodec) {
            w = w.min(cw);
            h = h.min(ch);
        }
        (w, h)
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
        // Pipeline-only Opus and the app-decoded lossless set need only a table that was read. Such a
        // track's own limits are [`audio_track_fits`], which the planner applies in both modes.
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
    /// ([`Caps::minus`]): a stereo or mono one takes the codec off the announced set, a surround
    /// one keeps it to stereo. A codec this value does not announce is already without, so the copy
    /// is equal to `self`.
    ///
    /// A video facet above [`VIDEO_REFUSAL_FLOOR`] lowers that codec's [`Caps::raster_bound`] to the
    /// floor ([`Caps::video_ceiling`]) and nothing else: the other codecs, the audio and the shared
    /// bound stay. One at or under the floor, or a bound the floor does not tighten, leaves the copy
    /// equal to `self` (nothing to lower is how the recovery ladder skips a rung that would
    /// change nothing).
    pub fn without(&self, facet: &Facet) -> Caps {
        let mut next = self.clone();
        match facet {
            Facet::Audio { codec, channels } => {
                limit_codec(&mut next, &audio_codec_id(codec), refusal_ceiling(*channels));
            }
            Facet::Video { codec, width, height } => {
                let (floor_w, floor_h) = VIDEO_REFUSAL_FLOOR;
                if *width > floor_w || *height > floor_h {
                    let codec = codec.to_ascii_lowercase();
                    let (bw, bh) = self.raster_bound(&codec);
                    let tighter = (bw.min(floor_w), bh.min(floor_h));
                    if tighter != (bw, bh) {
                        next.video_ceiling.insert(codec, tighter);
                    }
                }
            }
        }
        next
    }

    /// This value less the install's `rows` watched on `build` ([`Refusal::watched_on`]): each row by
    /// [`refusal_ceiling`], the one rule [`Caps::without`] uses. PURE: no registry, no flag, no log.
    /// A build-blind caller (`build` empty) applies every row, as `watched_on` says. Monotone and
    /// idempotent: it only ever removes.
    pub fn minus(&self, rows: &[Refusal], build: &str) -> Caps {
        let mut next = self.clone();
        for r in rows.iter().filter(|r| r.watched_on(build)) {
            limit_codec(&mut next, &audio_codec_id(&r.codec), refusal_ceiling(r.channels));
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
/// minus the codecs gstcool.conf shows nothing can decode, minus [`EXCLUSIONS`]. **Write-once**: no
/// lock, no later writer. What the install later learns is NOT folded into it. The install preference
/// `native_audio_refused` (`plex::session`) owns the list of refusals, and a play start derives its
/// own value from this snapshot and that list ([`start_caps`]).
struct Boot {
    /// What [`caps`] answers, the same `Arc` for the life of the process.
    caps: std::sync::Arc<Caps>,
    codename: String,
    /// The running firmware build (`tv::device::Info::build`; empty when the set did not say).
    build: String,
    /// Whether this set learns refusals and applies the ones it learned: it names its firmware, and
    /// `plxnative-nativeaudiowide` has not switched the learned layer off (`plxnative-nativeaudiolearn`
    /// beside it keeps the layer, so the learn-and-convert path can be shown on a set the exclusions
    /// would keep off it). Read once in [`probe`] (the flags are a `stat` per read) and never changes.
    learns: bool,
    /// `plxnative-nativeaudioforget`: the stored refusals are not applied, so a set can be shown
    /// learning from nothing. Recording is unchanged, and so is the record: a row recorded during a
    /// forgetful run is stored and applies from the next launch without the flag. The request in
    /// flight still converts, because its machine narrowed its own attempt.
    forget: bool,
}

impl Boot {
    fn new(caps: Caps, codename: String, build: String, (wide, learn, forget): (bool, bool, bool)) -> Boot {
        let learns = !ignores_learned(wide, learn) && !codename.is_empty();
        Boot { caps: std::sync::Arc::new(caps), codename, build, learns, forget }
    }
}

/// The boot answer, set once by [`probe`].
static BOOT: OnceLock<Boot> = OnceLock::new();
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

/// Run `f` on this set's boot value: the test thread's own when a test installed one, else the
/// boot's. `None` before [`probe`] has run (every host test that installed nothing).
fn with_boot<R>(f: impl FnOnce(&Boot) -> R) -> Option<R> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(boot) = THREAD_BOOT.with(|slot| slot.borrow().clone()) {
        return Some(f(&boot));
    }
    BOOT.get().map(f)
}

/// What this device decodes: the boot snapshot, the SAME `Arc` on every call. Falls back to
/// [`Caps::assumed`] if [`probe`] has not run (the boot calls it first, so that path exists for host
/// tests, not the TV). It is write-once: a refusal the install learns never moves it, so a reader
/// that wants "what would a play start offer" reads [`start_caps`].
pub fn caps() -> std::sync::Arc<Caps> {
    with_boot(|b| b.caps.clone()).unwrap_or_else(assumed_shared)
}

/// The one shared [`Caps::assumed`] value: [`caps`]' answer before [`probe`] has run, and the
/// stand-in a session that has not begun an attempt plans from (`route::PlaybackSession::attempt_caps`).
pub fn assumed_shared() -> std::sync::Arc<Caps> {
    ASSUMED.get_or_init(|| std::sync::Arc::new(Caps::assumed())).clone()
}

/// The play start's capability value: the boot snapshot less the install's `stored` refusals in force
/// (`Session::applied_refusals`, read by the caller at the play-start edge). The one reader that turns
/// the write-once boot and the install's record into a start value; PURE, so the same inputs answer
/// the same value. A set that does not learn applies no row (no firmware name, or
/// `plxnative-nativeaudiowide` without `nativeaudiolearn`), and `plxnative-nativeaudioforget` applies
/// none either. Rows watched on another build say nothing ([`Refusal::watched_on`]). Returns the boot
/// `Arc` itself when nothing applies (the common path, with no clone), and a new value only when a
/// row took something away.
pub fn start_caps(stored: &[Refusal]) -> std::sync::Arc<Caps> {
    let boot = caps();
    let Some((build, applies)) = with_boot(|b| (b.build.clone(), b.learns && !b.forget)) else {
        return boot;
    };
    if stored.is_empty() || !applies {
        return boot;
    }
    let next = boot.minus(stored, &build);
    if next == *boot { boot } else { std::sync::Arc::new(next) }
}

/// The firmware codename and build a refusal is recorded under, or `None` when this set cannot
/// learn: no firmware name, the debug flag (`nativeaudiowide` without `nativeaudiolearn`), or no
/// [`probe`]. A read, never a write: the install's learner (`plex::session::record_named_refusal`,
/// `record_refusal_proof`) calls it to stamp the row it queues, and the row reaches a play only
/// through the start edge ([`start_caps`]).
pub fn learning_firmware() -> Option<(String, String)> {
    with_boot(|b| b.learns.then(|| (b.codename.clone(), b.build.clone()))).flatten()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static THREAD_BOOT: std::cell::RefCell<Option<std::sync::Arc<Boot>>> = const { std::cell::RefCell::new(None) };
}

/// Make [`caps`] answer `caps` on THIS test thread until the guard drops (the
/// `i18n::language_on_this_thread_for_test` precedent), for a set that names its firmware and
/// build, so the thread can name its learning firmware ([`learning_firmware`]) and derive a start
/// value from a stored record ([`start_caps`]) as a television would. Tests run in parallel and the
/// process-wide answer is write-once, so a thread-local door is how one test plays a set whose table
/// lists a DTS decoder.
#[cfg(any(test, feature = "test-support"))]
pub fn firmware_on_this_thread_for_test(codename: &str, build: &str, caps: Caps) -> CapsGuard {
    firmware_with_flags_on_this_thread_for_test(codename, build, caps, (false, false, false))
}

/// [`firmware_on_this_thread_for_test`] with the debug flags a launch read, `(nativeaudiowide,
/// nativeaudiolearn, nativeaudioforget)`, so a test reaches them the way the product does: through
/// [`start_caps`] and [`learning_firmware`].
#[cfg(any(test, feature = "test-support"))]
pub fn firmware_with_flags_on_this_thread_for_test(
    codename: &str,
    build: &str,
    caps: Caps,
    flags: (bool, bool, bool),
) -> CapsGuard {
    let boot = Boot::new(caps, codename.to_owned(), build.to_owned(), flags);
    CapsGuard(THREAD_BOOT.with(|slot| slot.replace(Some(std::sync::Arc::new(boot)))))
}

#[cfg(any(test, feature = "test-support"))]
pub struct CapsGuard(Option<std::sync::Arc<Boot>>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for CapsGuard {
    fn drop(&mut self) {
        THREAD_BOOT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// The one canonical spelling of an AUDIO codec name, for every comparison between vocabularies:
/// lowercase, PMS's `dca` is the pipeline's `dts`, `ec-3` is `eac3`, and every decodable LPCM flavour is `pcm`. Applied at membership
/// ([`Caps::audio_has`]) and at the Load payload; inbound data stays raw (track memory compares
/// the raw string).
pub fn audio_codec_id(codec: &str) -> Cow<'_, str> {
    if codec.bytes().any(|b| b.is_ascii_uppercase()) {
        return Cow::Owned(audio_codec_id(&codec.to_ascii_lowercase()).into_owned());
    }
    match codec {
        "dca" => Cow::Borrowed("dts"),
        "ec-3" => Cow::Borrowed("eac3"),
        // Every LPCM flavour the bundled FFmpeg decodes is one codec to the planner: PMS names the
        // media's `pcm` and the stream's `pcm_s16le`, `pcm_s24le`, … The flavour is the demuxer's
        // business (`ff_pcm`); a flavour not listed here stays itself and is not app-decoded.
        "lpcm" => Cow::Borrowed("pcm"),
        c if APP_DECODED.iter().any(|d| d.id == "pcm" && d.codecs.contains(&c)) => Cow::Borrowed("pcm"),
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
        video_ceiling: Default::default(),
        app_lossy: AppLossy::Measured,
        app_lossy_in_force: Default::default(),
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

/// Which audio stack the set's sound output runs on, the fact the app-decoded audio's lead hangs on.
///
/// LG ships a different GStreamer sink plugin family per generation (a 2018 image also ships the
/// 2016/2017 one, so the probe order decides; Realtek and MStar images have none of the four; read
/// from the 19 unpacked firmware images, 2016 to 2025). The plugin is what [`probe`] fingerprints,
/// because in the firmware images read directly `liblxa.so.2` is not under the image's `/usr/lib` on
/// any (it is in the BSP partition, `/mnt/bsppart/bsp/usr/lib`, found through `ld.so.conf`), the dev
/// set's own firmware included, although a runtime stat of
/// `/usr/lib/liblxa.so.2` did answer yes there: the runtime layout is not the image's. Checked in
/// this order, the first present file decides:
///
/// | Stack | Present | Sets |
/// |---|---|---|
/// | 1 [`Liblxa`](AudioStack::Liblxa) | `libgstlxaudiosink.so` | 2019-2021 LG chips |
/// | 2 [`LgAudioSink`](AudioStack::LgAudioSink) | `libgstlgaudiosink.so` | 2022/23 OLEDs on webOS 11, 2024, 2025 |
/// | 3 [`Liblxa`](AudioStack::Liblxa) | `/usr/lib/liblxa.so.2` (the fallback) | the dev set answers it at runtime; 2022/23 OLEDs before webOS 11 (inferred from the `tools/fwcompat.py` inventories, not from an image read) |
/// | 4 [`AudioSinkNew`](AudioStack::AudioSinkNew) | `libgstaudiosink_new-1.0.so` | 2018 LG chips (webOS 4.4.3) |
/// | 5 [`AdecSink`](AudioStack::AdecSink) | `libgstaudiosink-1.0.so` (after `_new`: 2018 ships both) | 2016, 2017 LG chips |
/// | 6 [`Other`](AudioStack::Other) | none | Realtek, MStar |
///
/// The `liblxa.so.2` stat comes after the webOS 11 plugin because its runtime meaning is not understood
/// (no image has it under `/usr/lib`, yet the dev set answers yes), so a 2022/23 OLED updated to webOS 11
/// that kept a `liblxa.so.2` must still be read as the `lgaudiosink` it plays through.
///
/// (The plugins live in `/usr/lib/gstreamer-1.0/`.) One webOS version therefore spans several stacks,
/// and the firmware version cannot stand in for it. The `tools/fwcompat.py` inventories are
/// per-library symbol tables: they carry `liblxa.so.2.0.0` on 7.4, 8.3 and 9.2 and hold no GStreamer
/// plugin (`/usr/lib/gstreamer-1.0/*.so`), which is why the stack is fingerprinted from the plugin files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioStack {
    /// LG's `liblxa` renderer behind the `lxaudiosink` plugin: the stack the lead was filmed on (the
    /// dev set, webOS 4.10.2).
    Liblxa,
    /// LG's renderer built into the sink `libgstlgaudiosink.so` (element `lgaudiosink`).
    LgAudioSink,
    /// 2018's `audiosink` element (`libgstaudiosink_new-1.0.so`), which renders through `libkadaptor`.
    AudioSinkNew,
    /// The two `libgstaudiosink-1.0.so` stacks of 2016 (element `audio_sink`) and 2017 (element
    /// `adecsink`); the label is `adecsink` for both.
    AdecSink,
    /// [`probe`] ran on a device that has none of those plugins (Realtek, MStar).
    Other,
    /// [`probe`] has not run: a host, the simulator, a test, or a boot that never got here.
    Unknown,
}

impl AudioStack {
    /// The word the event log uses for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Liblxa => "liblxa",
            Self::LgAudioSink => "lgaudiosink",
            Self::AudioSinkNew => "audiosink_new",
            Self::AdecSink => "adecsink",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }
}

/// The GStreamer plugin of [`AudioStack::Liblxa`]. Every probe path is a stat, deliberately, and not a
/// symbol lookup: the build links LG's libraries through stub `.so` files that define every symbol the
/// app names, so a lookup in our own process proves nothing about the set, and `dlopen` would run LG's
/// library initialisation inside the app to learn a fact a directory entry already states.
pub const LXAUDIOSINK_PLUGIN: &str = "/usr/lib/gstreamer-1.0/libgstlxaudiosink.so";

/// The soname link of `liblxa`, the fallback way to say [`AudioStack::Liblxa`], stat after the webOS 11
/// plugin: the dev set answers yes to it although no firmware image has it under `/usr/lib`, so its
/// runtime meaning is not understood.
pub const LIBLXA_SONAME_LINK: &str = "/usr/lib/liblxa.so.2";

/// The plugin of [`AudioStack::LgAudioSink`].
pub const LGAUDIOSINK_PLUGIN: &str = "/usr/lib/gstreamer-1.0/libgstlgaudiosink.so";

/// The plugin of [`AudioStack::AudioSinkNew`].
pub const AUDIOSINK_NEW_PLUGIN: &str = "/usr/lib/gstreamer-1.0/libgstaudiosink_new-1.0.so";

/// The plugin of [`AudioStack::AdecSink`]; checked after [`AUDIOSINK_NEW_PLUGIN`], because 2018
/// firmware ships both.
pub const ADECSINK_PLUGIN: &str = "/usr/lib/gstreamer-1.0/libgstaudiosink-1.0.so";

/// The stack for a set on which `present(path)` says whether a file exists, in the order of the
/// [`AudioStack`] table; the first present file decides. The seam [`probe`] uses with the real
/// filesystem (so `present` runs up to five times); PURE apart from `present`.
pub fn audio_stack_from(mut present: impl FnMut(&str) -> bool) -> AudioStack {
    if present(LXAUDIOSINK_PLUGIN) {
        AudioStack::Liblxa
    } else if present(LGAUDIOSINK_PLUGIN) {
        AudioStack::LgAudioSink
    } else if present(LIBLXA_SONAME_LINK) {
        AudioStack::Liblxa
    } else if present(AUDIOSINK_NEW_PLUGIN) {
        AudioStack::AudioSinkNew
    } else if present(ADECSINK_PLUGIN) {
        AudioStack::AdecSink
    } else {
        AudioStack::Other
    }
}

/// The boot answer for [`audio_stack`], set once by [`probe`].
static AUDIO_STACK: OnceLock<AudioStack> = OnceLock::new();

/// This set's audio stack: what [`probe`] found, or [`AudioStack::Unknown`] before it ran (hosts,
/// hostsim and tests, unless a test installed one with [`audio_stack_on_this_thread_for_test`]).
pub fn audio_stack() -> AudioStack {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(stack) = THREAD_AUDIO_STACK.with(|slot| slot.get()) {
        return stack;
    }
    AUDIO_STACK.get().copied().unwrap_or(AudioStack::Unknown)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static THREAD_AUDIO_STACK: std::cell::Cell<Option<AudioStack>> = const { std::cell::Cell::new(None) };
}

/// Make [`audio_stack`] answer `stack` on THIS test thread until the guard drops (the
/// [`firmware_on_this_thread_for_test`] precedent: the process-wide answer is write-once and tests
/// run in parallel).
#[cfg(any(test, feature = "test-support"))]
pub fn audio_stack_on_this_thread_for_test(stack: AudioStack) -> AudioStackGuard {
    AudioStackGuard(THREAD_AUDIO_STACK.with(|slot| slot.replace(Some(stack))))
}

#[cfg(any(test, feature = "test-support"))]
pub struct AudioStackGuard(Option<AudioStack>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for AudioStackGuard {
    fn drop(&mut self) {
        THREAD_AUDIO_STACK.with(|slot| slot.set(self.0.take()));
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
    // [`EXCLUSIONS`]. That is the boot snapshot, and it is final. The refusals this install learned
    // come after, from the stored record alone, at each play's start ([`start_caps`]): named or
    // proved, a refusal reaches a play only once its queued write has landed. `nativeaudiowide` skips
    // the exclusions and the learned layer (`nativeaudiolearn` beside it keeps the layer),
    // `nativeaudioforget` the record.
    let stack = audio_stack_from(|path| std::path::Path::new(path).exists());
    let _ = AUDIO_STACK.set(stack);
    let mut boot = caps.clone();
    let excluded = apply_exclusions(&mut boot, &info.codename, wide, EXCLUSIONS);
    if plx_base::devtrig::flag("appdecodelossy") {
        boot = boot.with_app_lossy();
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log(&format!(
            "devcaps: appdecodelossy trigger: every lossy APP_DECODED row decoded in the app (All, in force: {:?})",
            boot.app_lossy_in_force
        ));
    }
    let lossy_rows = || APP_DECODED.iter().filter(|r| r.lossy);
    plx_base::eventlog::log(&format!(
        "devcaps: app lossy decode {:?}: enabled [{}] unmeasured [{}]",
        boot.app_lossy,
        lossy_rows().filter(|r| lossy_row_enabled(r, boot.app_lossy)).map(|r| r.id).collect::<Vec<_>>().join(","),
        lossy_rows().filter(|r| r.measured.is_none()).map(|r| r.id).collect::<Vec<_>>().join(","),
    ));
    plx_base::eventlog::log(&format!(
        "devcaps: firmware={:?} build={:?} audio stack={} audio layers: table+gst={} | exclusions: {} | -> audio={}{}{}",
        info.codename,
        info.build,
        stack.label(),
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
    let _ = BOOT.set(Boot::new(boot, info.codename.clone(), info.build.clone(), (wide, learn, forget)));
}

/// Whether [`caps`] reflects THIS set's table or the assumed profile — see [`MEASURED`].
pub fn measured() -> bool {
    *MEASURED.get().unwrap_or(&false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack_on(files: &[&str]) -> AudioStack {
        audio_stack_from(|path| files.contains(&path))
    }

    fn stats_made(answer: bool) -> Vec<String> {
        let asked = std::cell::RefCell::new(Vec::new());
        audio_stack_from(|path| {
            asked.borrow_mut().push(path.to_string());
            answer
        });
        asked.into_inner()
    }

    /// One row per stack of the probe table: the plugin (or library) that says it, alone.
    #[test]
    fn each_lg_sink_plugin_alone_names_its_stack() {
        assert_eq!(stack_on(&[LXAUDIOSINK_PLUGIN]), AudioStack::Liblxa, "2019-2021 LG chips (and, inferred from the fwcompat inventories, not from an image read, the 2022/23 OLEDs before webOS 11): the lxaudiosink plugin");
        assert_eq!(stack_on(&[LIBLXA_SONAME_LINK]), AudioStack::Liblxa, "the fallback: the dev set answered the soname link at runtime");
        assert_eq!(stack_on(&[LGAUDIOSINK_PLUGIN]), AudioStack::LgAudioSink, "webOS 11 on the 2022/23 OLEDs, 2024 and 2025");
        assert_eq!(stack_on(&[AUDIOSINK_NEW_PLUGIN]), AudioStack::AudioSinkNew, "2018 LG chips");
        assert_eq!(stack_on(&[ADECSINK_PLUGIN]), AudioStack::AdecSink, "2016 and 2017 LG chips");
        assert_eq!(stack_on(&[]), AudioStack::Other);
    }

    #[test]
    fn the_plugin_paths_are_the_firmware_file_names() {
        assert_eq!(LXAUDIOSINK_PLUGIN, "/usr/lib/gstreamer-1.0/libgstlxaudiosink.so");
        assert_eq!(LGAUDIOSINK_PLUGIN, "/usr/lib/gstreamer-1.0/libgstlgaudiosink.so");
        assert_eq!(AUDIOSINK_NEW_PLUGIN, "/usr/lib/gstreamer-1.0/libgstaudiosink_new-1.0.so");
        assert_eq!(ADECSINK_PLUGIN, "/usr/lib/gstreamer-1.0/libgstaudiosink-1.0.so");
        assert_eq!(LIBLXA_SONAME_LINK, "/usr/lib/liblxa.so.2");
    }

    /// 2018 ships both its own plugin and the older one: `_new` has to answer first.
    #[test]
    fn the_2018_plugin_wins_over_the_older_one_it_ships_beside() {
        assert_eq!(stack_on(&[ADECSINK_PLUGIN, AUDIOSINK_NEW_PLUGIN]), AudioStack::AudioSinkNew);
    }

    /// The lxaudiosink plugin outranks every other file; the bare `liblxa.so.2` stat outranks only the 2018 and
    /// 2016/2017 plugins, never the webOS 11 one.
    #[test]
    fn the_webos11_plugin_outranks_the_bare_liblxa_soname_stat() {
        assert_eq!(
            stack_on(&[LGAUDIOSINK_PLUGIN, LIBLXA_SONAME_LINK]),
            AudioStack::LgAudioSink,
            "a set updated to webOS 11 that kept a liblxa.so.2 plays through lgaudiosink"
        );
        assert_eq!(
            stack_on(&[LXAUDIOSINK_PLUGIN, LGAUDIOSINK_PLUGIN]),
            AudioStack::Liblxa,
            "cannot happen per the images; the order is pinned anyway"
        );
        assert_eq!(stack_on(&[LXAUDIOSINK_PLUGIN, LIBLXA_SONAME_LINK]), AudioStack::Liblxa);
        for other in [LGAUDIOSINK_PLUGIN, AUDIOSINK_NEW_PLUGIN, ADECSINK_PLUGIN] {
            assert_eq!(stack_on(&[LXAUDIOSINK_PLUGIN, other]), AudioStack::Liblxa, "lxaudiosink + {other}");
        }
        for other in [AUDIOSINK_NEW_PLUGIN, ADECSINK_PLUGIN] {
            assert_eq!(stack_on(&[LIBLXA_SONAME_LINK, other]), AudioStack::Liblxa, "liblxa.so.2 + {other}");
        }
        assert_eq!(stack_on(&[LGAUDIOSINK_PLUGIN, AUDIOSINK_NEW_PLUGIN, ADECSINK_PLUGIN]), AudioStack::LgAudioSink);
    }

    /// The probe is stats, in a fixed order, and stops at the first that answers.
    #[test]
    fn the_probe_stats_in_the_table_order_and_stops_at_the_first_yes() {
        assert_eq!(
            stats_made(false),
            [LXAUDIOSINK_PLUGIN, LGAUDIOSINK_PLUGIN, LIBLXA_SONAME_LINK, AUDIOSINK_NEW_PLUGIN, ADECSINK_PLUGIN],
            "five stats, nothing else"
        );
        assert_eq!(stats_made(true), [LXAUDIOSINK_PLUGIN], "the first yes ends it");
        let until = |last: &'static str| {
            let order = stats_made(false);
            let cut = order.iter().position(|p| p == last).unwrap();
            let asked = std::cell::RefCell::new(Vec::new());
            audio_stack_from(|path| {
                asked.borrow_mut().push(path.to_string());
                path == last
            });
            assert_eq!(*asked.borrow(), order[..=cut]);
        };
        for last in [LGAUDIOSINK_PLUGIN, LIBLXA_SONAME_LINK, AUDIOSINK_NEW_PLUGIN, ADECSINK_PLUGIN] {
            until(last);
        }
    }

    /// Realtek and MStar images ship none of LG's plugins: their own sink names are not LG's.
    #[test]
    fn realtek_and_mstar_plugins_are_not_an_lg_stack() {
        for other in [
            "/usr/lib/gstreamer-1.0/libgstrtkalsa.so",
            "/usr/lib/gstreamer-1.0/libgstrtkaudio-1.0.so",
            "/usr/lib/gstreamer-1.0/libgstomx.so",
            "/usr/lib/gstreamer-1.0/libgstmasink.so",
            "/usr/lib/openmax/libOMX.MStar.so.1.0.0",
        ] {
            assert_eq!(audio_stack_from(|path| path == other), AudioStack::Other, "{other}");
        }
        assert_eq!(audio_stack_from(|path| path.contains("rtkaudio")), AudioStack::Other);
    }

    #[test]
    fn a_host_answers_unknown_and_a_test_can_install_either_answer() {
        assert_eq!(audio_stack(), AudioStack::Unknown, "nothing in the host suite calls probe");
        {
            let _liblxa = audio_stack_on_this_thread_for_test(AudioStack::Liblxa);
            assert_eq!(audio_stack(), AudioStack::Liblxa);
            {
                let _other = audio_stack_on_this_thread_for_test(AudioStack::Other);
                assert_eq!(audio_stack(), AudioStack::Other);
            }
            assert_eq!(audio_stack(), AudioStack::Liblxa, "a guard restores what it replaced");
        }
        assert_eq!(audio_stack(), AudioStack::Unknown);
        assert_eq!(
            [
                AudioStack::Liblxa,
                AudioStack::LgAudioSink,
                AudioStack::AudioSinkNew,
                AudioStack::AdecSink,
                AudioStack::Other,
                AudioStack::Unknown
            ]
            .map(AudioStack::label),
            ["liblxa", "lgaudiosink", "audiosink_new", "adecsink", "other", "unknown"]
        );
    }

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
        assert_eq!(c.audio, "aac,ac3,eac3,dts,opus,flac,alac,pcm");
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
        assert_eq!(c.audio, "aac,ac3,eac3,dts,opus,flac,alac,pcm");
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

        // A surround facet keeps the codec to stereo (what `minus` does for a 6-channel refusal).
        let surround = base.without(&facet("dts", 6));
        assert!(!surround.audio_supports("dts", 6) && surround.audio_has("dts"));
        assert_eq!(surround.audio_channels.get("dts"), Some(&2));
        // A stereo facet takes the codec away, whatever spelling named it.
        let stereo = base.without(&facet("DCA", 2));
        assert!(!stereo.audio_has("dts") && !stereo.audio_supports("dts", 2));
        assert!(!stereo.forced_feeds_audio("dts"), "Force Direct Play offers nothing it cannot take either");
        // The same rule as a refusal the install learned.
        assert_eq!(stereo, native_set().minus(&[refusal("dts", 2)], ""));

        // Only the facet moved: the source value is a different, untouched object, and the rest of it holds.
        assert_eq!(base, native_set());
        assert!(stereo.audio_supports("eac3", 8) && stereo.audio_supports("ac3", 6));
        // A codec this value never announced is already without: the copy equals the source.
        assert_eq!(base.without(&facet("truehd", 8)), base);
        // Monotone and idempotent, like `minus`.
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
        Refusal { codec: codec.into(), channels, build: String::new() }
    }

    fn refusal_on(build: &str, codec: &str, channels: u32) -> Refusal {
        Refusal { build: build.into(), ..refusal(codec, channels) }
    }

    #[test]
    fn a_learned_refusal_of_a_stereo_track_takes_the_codec_away_and_a_surround_one_keeps_stereo() {
        let c = native_set().minus(&[refusal("opus", 2), refusal("eac3", 8)], "");
        assert!(!c.audio_has("opus") && !c.forced_feeds_audio("opus"));
        assert!(c.audio_supports("eac3", 2) && !c.audio_supports("eac3", 6));
        assert!(!c.forced_feeds_audio_track("eac3", 6), "Force holds a learned ceiling too");
        // The rest of the table is untouched.
        assert!(c.audio_supports("dts", 6) && c.audio_supports("aac", 6) && c.audio_supports("ac3", 6));
        // No rows is the same value, and the source is a different, untouched object.
        assert_eq!(native_set().minus(&[], ""), native_set());
        // A codec the set never announced is already without.
        let e = Caps::assumed();
        assert_eq!(e.minus(&[refusal("opus", 6)], ""), e, "the fallback never announced Opus");
        // Any spelling of the codec names the same row.
        assert!(!native_set().minus(&[refusal("DCA", 2)], "").audio_has("dts"));
    }

    #[test]
    fn the_layers_stack_in_order_table_gst_exclusions_learned() {
        // gstcool.conf drops DTS; the C1 row bounds Opus to stereo; a learned refusal then removes
        // the codec the row left (stereo Opus).
        let mut c = native_set();
        assert_eq!(apply_gst_decoders(&mut c, &GST_C1.replace("dts_audiodec=290\n", "")).0, ["dts"]);
        assert_eq!(apply_exclusions(&mut c, "kisscurl-koli", false, EXCLUSIONS), ["opus<=2"]);
        let c = c.minus(&[refusal("opus", 2)], "");
        assert_eq!(c.audio, "aac,ac3,eac3,flac,alac,pcm");
    }

    /// A start value for a set on `build`, with `rows` stored in its record.
    fn start_on(build: &str, rows: &[Refusal]) -> std::sync::Arc<Caps> {
        let _set = firmware_on_this_thread_for_test("kisscurl-koli", build, native_set());
        start_caps(rows)
    }

    /// The start lacks what an applied row says the pipeline refuses, and is the boot `Arc` itself
    /// when nothing applies. A stereo row takes the codec away, a surround one keeps it to stereo.
    #[test]
    fn the_start_lacks_an_applied_row_and_keeps_the_boot() {
        let _set = firmware_on_this_thread_for_test("kisscurl-koli", "B", native_set());
        let boot = caps();
        assert_eq!(*boot, native_set());
        let surround = start_caps(&[refusal_on("B", "opus", 6)]);
        assert!(!surround.audio_supports("opus", 6) && surround.audio_supports("opus", 2));
        let stereo = start_caps(&[refusal_on("B", "dts", 2)]);
        assert!(!stereo.audio_has("dts"));
        assert!(std::sync::Arc::ptr_eq(&start_caps(&[]), &boot), "no rows: the boot value itself, not a copy");
        assert!(
            std::sync::Arc::ptr_eq(&start_caps(&[refusal_on("B", "truehd", 8)]), &boot),
            "a row for a codec the set never announced takes nothing: the boot value itself"
        );
        assert_eq!(*caps(), native_set(), "deriving a start value wrote nothing");
    }

    /// An LG update replaces the decoder a refusal was watched on: another build's entry, and one an
    /// older record never tagged, is no evidence on this build, while a set that did not name its
    /// build judges nothing and takes every entry.
    #[test]
    fn a_build_change_clears_the_rows_from_the_start() {
        for stale in ["C", ""] {
            let _set = firmware_on_this_thread_for_test("kisscurl-koli", "B", native_set());
            assert!(
                std::sync::Arc::ptr_eq(&start_caps(&[refusal_on(stale, "opus", 2)]), &caps()),
                "a row watched on build {stale:?} says nothing on build B"
            );
        }
        assert_eq!(native_set().minus(&[refusal_on("B", "opus", 2)], "C"), native_set());
        assert!(!native_set().minus(&[refusal_on("B", "opus", 2)], "").audio_has("opus"), "a blind build takes every row");
        assert!(refusal("opus", 2).watched_on("") && !refusal("opus", 2).watched_on("6.5.3-47"));
        // A set that did not name its build judges nothing: every row counts.
        let blind = start_on("", &[refusal_on("B", "opus", 2)]);
        assert!(!blind.audio_has("opus"));
        // The matching build applies.
        assert!(!start_on("B", &[refusal_on("B", "opus", 2)]).audio_has("opus"));
    }

    /// The boot readers (the engine's sink envelope, the diagnostics, the lab snapshot) see ONE
    /// value: deriving a start value from the record is a read, and `caps()` is the same `Arc`
    /// before and after.
    #[test]
    fn the_boot_readers_see_one_unchanged_value() {
        let _set = firmware_on_this_thread_for_test("kisscurl-koli", "B", native_set());
        let boot = caps();
        let narrower = start_caps(&[refusal_on("B", "opus", 2)]);
        assert!(!narrower.audio_has("opus"), "sanity: the start value is narrower");
        assert!(std::sync::Arc::ptr_eq(&boot, &caps()), "the boot value is the same Arc");
        assert_eq!(*boot, native_set());
        assert!(boot.audio_has("opus"));
    }

    /// The debug flags decide which stored rows the start applies: `nativeaudiowide` ignores every
    /// row, `nativeaudiolearn` beside it keeps them, `nativeaudioforget` ignores every stored row
    /// (a row recorded during a forgetful run is stored and applies from the next launch without
    /// the flag), and a set with no firmware name learns, and so applies, nothing.
    #[test]
    fn the_debug_flags_decide_which_stored_rows_the_start_applies() {
        let applied = |codename: &str, flags| {
            let _set = firmware_with_flags_on_this_thread_for_test(codename, "B", native_set(), flags);
            let start = start_caps(&[refusal_on("B", "opus", 2)]);
            assert_eq!(std::sync::Arc::ptr_eq(&start, &caps()), start.audio_has("opus"), "ptr_eq iff nothing was taken");
            !start.audio_has("opus")
        };
        let fw = "kisscurl-koli";
        assert!(applied(fw, (false, false, false)), "the baseline: the row takes Opus away");
        assert!(!applied(fw, (false, false, true)), "forget ignores the stored row");
        assert!(!applied(fw, (true, false, false)), "wide ignores the row");
        assert!(applied(fw, (true, true, false)), "learn beside wide keeps it");
        assert!(!applied("", (false, false, false)), "a set with no firmware name applies nothing");
    }

    /// Learning is a READ of the firmware the row is stamped with: the thread's set names it, the
    /// read takes nothing from the answer, and `caps()` never moves. A set with no firmware name
    /// learns nothing.
    #[test]
    fn the_learning_firmware_is_a_read() {
        {
            let _set = firmware_on_this_thread_for_test("kisscurl-koli", "6.5.3-47", native_set());
            let before = caps();
            assert!(before.audio_supports("opus", 2), "sanity: announced");
            assert_eq!(learning_firmware(), Some(("kisscurl-koli".to_owned(), "6.5.3-47".to_owned())));
            assert!(std::sync::Arc::ptr_eq(&before, &caps()), "reading the firmware takes nothing away");
        }
        let _nameless = firmware_on_this_thread_for_test("", "", native_set());
        assert_eq!(learning_firmware(), None, "no firmware name: nothing can be learned about this set");
        assert!(caps().audio_supports("opus", 2));
        let _wide = firmware_with_flags_on_this_thread_for_test("kisscurl-koli", "B", native_set(), (true, false, false));
        assert_eq!(learning_firmware(), None, "nativeaudiowide without nativeaudiolearn learns nothing");
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
        assert_eq!(c.audio, "aac,ac3,eac3,opus,flac,alac,pcm");
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

    /// `pcmInfo` can name only the rates in libpf's table (plus 44.1 kHz), so the app resamples a faster
    /// FLAC to the top of it ([`pcm_load_rate`]) and the Load never carries the source rate. A rate the
    /// resampler cannot make a table rate of (below 48 kHz and outside the table, unstated) and a
    /// channel count past two still go to the server.
    #[test]
    fn the_pcm_path_admits_the_table_rates_and_resamples_the_faster_ones() {
        for rate in [8_000, 22_050, 44_100, 48_000] {
            assert!(audio_track_fits("flac", 2, rate), "{rate}");
        }
        assert!(audio_track_fits("FLAC", 1, 48_000));
        for rate in [64_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000] {
            assert!(audio_track_fits("flac", 2, rate), "{rate}: resampled to the table's top");
            assert!(audio_track_fits("flac", 1, rate), "{rate}: mono");
        }
        for rate in [0, -1, 11_025, 37_800] {
            assert!(!audio_track_fits("flac", 2, rate), "{rate} is unstated or not a table rate");
        }
        assert!(!audio_track_fits("flac", 6, 48_000));
        assert!(!audio_track_fits("flac", 6, 96_000), "a 5.1 hi-res FLAC still converts on the server");
        assert!(!audio_track_fits("flac", 0, 48_000));
        // not app-decoded: the hardware decoder's own channel rules apply, not these
        assert!(audio_track_fits("ac3", 6, 0));
    }

    /// **The rate the Load declares for a source rate** ([`pcm_load_rate`]): a table rate is itself (the
    /// bit-exact path), every rate above 48 kHz goes to the top of the table that keeps its family
    /// (a multiple of 44.1 kHz to 44.1 kHz, any other to 48 kHz), and a rate below 48 kHz that is not in
    /// the table, or none, is `None`. There is no ceiling: the only limit is the type FFmpeg states a
    /// rate in (a C int). Whatever it returns is in [`PCM_SAMPLE_RATES`], which is why the Load's
    /// kilohertz declaration needs no change.
    #[test]
    fn pcm_load_rate_resamples_every_rate_above_the_table_to_its_top() {
        for r in PCM_SAMPLE_RATES {
            assert_eq!(pcm_load_rate(i64::from(r)), Some(r), "{r}: a table rate is itself");
        }
        for (src, want) in [
            (88_200, 44_100), (176_400, 44_100), (352_800, 44_100), (705_600, 44_100), (2_822_400, 44_100),
            (96_000, 48_000), (192_000, 48_000), (384_000, 48_000), (768_000, 48_000),
            (64_000, 48_000), (128_000, 48_000), (50_000, 48_000), (48_001, 48_000), (1_000_003, 48_000),
            (5_644_800, 44_100), (i64::from(i32::MAX), 48_000),
        ] {
            assert_eq!(pcm_load_rate(src), Some(want), "{src}");
        }
        for src in [i64::MIN, -48_000, -1, 0, 1, 7_999, 11_025, 37_800, 47_999] {
            assert_eq!(pcm_load_rate(src), None, "{src}: unstated, or below the table's top and not in it");
        }
        assert_eq!(pcm_load_rate(i64::from(i32::MAX) + 1), None, "FFmpeg states a rate in a C int: no file has this");
        for src in (0..400).map(|k| 1_000 * k + 7).chain([48_000 * 3, 44_100 * 3]) {
            assert!(pcm_load_rate(src).is_none_or(|r| PCM_SAMPLE_RATES.contains(&r)), "{src}: the Load can name it");
        }
    }

    /// Issue #951: an LG B8 refused a 96 kHz AAC Load, so a codec the hardware decodes is admitted
    /// only up to the ceiling the profile asks PMS for, by the same predicate. An unstated rate
    /// passes (the planner asks with `0` when it has no track list), and DTS is exempt: the app
    /// feeds only its core, which is 48 kHz at most by format, so a 96/24 DTS-HD source plays today.
    #[test]
    fn a_hardware_codec_above_the_load_ceiling_is_not_admitted_but_dts_is_exempt() {
        for rate in [0, 8_000, 11_025, 44_100, 48_000] {
            assert!(audio_track_fits("aac", 2, rate), "{rate}");
        }
        for rate in [48_001, 88_200, 96_000, 192_000] {
            assert!(!audio_track_fits("aac", 2, rate), "{rate} is above the ceiling");
            assert!(!audio_track_fits("eac3", 6, rate), "{rate}");
        }
        assert!(PCM_SAMPLE_RATES.iter().all(|&r| r <= AUDIO_RATE_CEILING), "the PCM table names nothing above the ceiling");
        for dts in ["dca", "dts", "DTS"] {
            assert!(audio_track_fits(dts, 8, 96_000), "{dts}: only the core is fed");
        }
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

    /// The set of codecs the app decodes is one table, and the list the rest of the code splits is
    /// that table's ids, in order.
    #[test]
    fn the_pcm_codec_list_is_the_app_decoded_table() {
        let ids: Vec<&str> = APP_DECODED.iter().filter(|c| !c.lossy).map(|c| c.id).collect();
        assert_eq!(PCM_AUDIO_CODECS.split(',').collect::<Vec<_>>(), ids);
        let all: Vec<&str> = APP_DECODED.iter().map(|c| c.id).collect();
        assert!(all.iter().all(|id| audio_codec_id(id) == *id), "an id is its own canonical spelling");
    }

    /// "Derived from the bundled decoder list": every decoder the table names is enabled in
    /// `ci/build-ffmpeg.sh`, the one list the shipped FFmpeg is built from.
    #[test]
    fn every_app_decoded_codec_has_its_decoders_in_the_bundled_ffmpeg_build() {
        let script = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ci/build-ffmpeg.sh"))
            .expect("ci/build-ffmpeg.sh");
        let enabled: Vec<&str> = script
            .split_whitespace()
            .filter_map(|w| w.strip_prefix("--enable-decoder="))
            .flat_map(|l| l.trim_end_matches('\\').split(','))
            .collect();
        assert!(enabled.contains(&"flac"), "the parse found the decoder list: {enabled:?}");
        for c in APP_DECODED {
            assert!(!c.decoders.is_empty() && !c.codecs.is_empty(), "{}", c.id);
            for d in c.decoders {
                assert!(enabled.contains(d), "{}: decoder {d} is not in ci/build-ffmpeg.sh's --enable-decoder list", c.id);
            }
            for n in c.codecs {
                assert_eq!(app_decoded_by_ffmpeg_name(n).map(|c| c.id), Some(c.id));
            }
        }
        assert!(app_decoded_by_ffmpeg_name("ac3").is_none(), "a hardware codec is not app-decoded");
    }

    /// **The lossy policy.** DTS is app-decoded (in force) only on a value that put it there: the
    /// assumed fallback and a table read have it ADMITTED at most ([`AppLossy`]), never in force, so the
    /// set's own decoder keeps playing what it can. In force, DTS is announced without a table row, with
    /// no rate bound, in both modes, and a native ceiling measured on the set's own decoder does not
    /// apply to it (the app decodes it, and `native_audio_track` is false for it).
    #[test]
    fn dts_is_app_decoded_only_on_a_value_that_turned_the_gate_on() {
        for off in [Caps::assumed(), parse(REAL).unwrap()] {
            assert!(off.app_lossy_in_force.is_empty() && !off.app_decodes("dts") && !off.app_decodes("dca"));
            assert!(off.app_decodes("flac"), "the lossless set is never lossy-policy bound");
        }
        assert_eq!(Caps::assumed().app_lossy, AppLossy::Off);
        assert_eq!(parse(REAL).unwrap().app_lossy, AppLossy::Measured);
        assert!(!Caps::assumed().audio_has("dts"), "off: the assumed fallback is what it was");
        let on = Caps::assumed().with_app_lossy();
        for spelling in ["dts", "dca", "DTS"] {
            assert!(on.app_decodes(spelling), "{spelling}");
            assert!(on.audio_has(spelling) && on.audio_supports(spelling, 8), "{spelling}");
            assert!(on.forced_feeds_audio(spelling), "{spelling}");
        }
        assert!(!on.app_decodes("ac3") && !on.app_decodes("eac3") && !on.app_decodes("mlp"));
        assert!(audio_track_fits("dts", 6, 96_000), "no rate bound: the core is at most 48 kHz");
        let measured = Caps { native_ceiling: [("dts".into(), 2)].into(), ..Caps::assumed() }.with_app_lossy();
        assert!(measured.app_decodes_lossy_track("dts", 6, 0, true), "a ceiling of the native decoder is not the app's");
        assert!(!measured.native_audio_track("dts", 6, 0, true), "and the set's decoder is not asked");
        assert!(!Caps { native_ceiling: [("dts".into(), 2)].into(), ..parse(REAL).unwrap() }.forced_feeds_audio_track("dts", 6));
        assert_eq!(on.clone().with_app_lossy(), on, "idempotent");
        let dts_in_list = on.audio.split(',').filter(|c| *c == "dts").count();
        assert_eq!(dts_in_list, 1);
    }

    /// **S4-4: MP3 and Vorbis ride the same policy as DTS**: in force only when the debug trigger or the
    /// rescue rung puts them there, and, being unmeasured, never enabled on a read table.
    /// On, each is announced without a table row and fed in both modes; unlike a DTS core, either can
    /// be above 48 kHz (a 96 kHz Vorbis), which has not been through the TV, so the rate bound stays
    /// and such a track is converted by the server.
    #[test]
    fn mp3_and_vorbis_are_app_decoded_only_where_in_force() {
        for off in [Caps::assumed(), parse(REAL).unwrap()] {
            for codec in ["mp3", "vorbis", "MP3"] {
                assert!(!off.app_decodes(codec) && !off.audio_has(codec) && !off.forced_feeds_audio(codec), "{codec}");
                assert!(!off.app_decodes_lossy_track(codec, 2, 44_100, true), "{codec}: unmeasured, so never enabled on a read table");
            }
        }
        let on = Caps::assumed().with_app_lossy();
        for codec in ["mp3", "vorbis", "MP3", "Vorbis"] {
            assert!(on.app_decodes(codec) && on.app_may_decode_lossy(codec), "{codec}");
            assert!(on.audio_has(codec) && on.audio_supports(codec, 2), "{codec}");
            assert!(on.app_decodes_lossy_track(codec, 2, 44_100, true) && !on.native_audio_track(codec, 2, 44_100, true), "{codec}");
        }
        assert!(audio_track_fits("mp3", 2, 44_100) && audio_track_fits("vorbis", 6, 48_000));
        assert!(audio_track_fits("vorbis", 2, 0), "an unstated rate passes, as for every codec");
        assert!(!audio_track_fits("vorbis", 2, 96_000), "above 48 kHz the server converts it");
        assert!(!on.app_decodes("mp2") && !on.app_decodes("wma") && !on.app_decodes("mlp"));
        for codec in ["mp3", "vorbis"] {
            assert_eq!(on.audio.split(',').filter(|c| *c == codec).count(), 1, "{codec} once");
        }
        assert_eq!(on.clone().with_app_lossy(), on, "idempotent");
        assert_eq!(audio_codec_id("MP3"), "mp3");
        assert_eq!(app_decoded_by_ffmpeg_name("mp3").map(|c| (c.id, c.lossy)), Some(("mp3", true)));
        assert_eq!(app_decoded_by_ffmpeg_name("vorbis").map(|c| (c.id, c.lossy)), Some(("vorbis", true)));
    }

    /// **S4-5: TrueHD rides the same gate**, up to eight channels in and any rate the codec has (192 kHz
    /// at most), mixed to the one stereo 48 kHz shape. Unlike MP3 and Vorbis it is NOT rate bound: the
    /// app's resampler low-passes before it decimates, so a 96 or 192 kHz track is decoded here instead
    /// of being converted by the server. Gate off, nothing changes; the planner and the profile see no
    /// TrueHD.
    #[test]
    fn truehd_is_app_decoded_under_the_gate_with_no_rate_bound() {
        for off in [Caps::assumed(), parse(REAL).unwrap()] {
            assert!(!off.app_decodes("truehd") && !off.audio_has("truehd") && !off.forced_feeds_audio("truehd"));
        }
        let on = Caps::assumed().with_app_lossy();
        for codec in ["truehd", "TrueHD"] {
            assert!(on.app_decodes(codec) && on.app_may_decode_lossy(codec), "{codec}");
            assert!(on.audio_has(codec) && on.audio_supports(codec, 8), "{codec}");
            assert!(on.app_decodes_lossy_track(codec, 8, 96_000, true), "{codec}");
        }
        assert!(!audio_rate_bounded("truehd") && audio_rate_bounded("vorbis") && audio_rate_bounded("mp3"));
        // the lossless app decode resamples too, so the profile asks PMS for no rate bound on it either
        for codec in ["flac", "FLAC", "alac", "pcm", "pcm_s24le", "dts", "dca"] {
            assert!(!audio_rate_bounded(codec), "{codec}");
        }
        for codec in ["aac", "ac3", "eac3", "opus", "mp3", "vorbis"] {
            assert!(audio_rate_bounded(codec), "{codec}");
        }
        for rate in [0, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
            assert!(audio_track_fits("truehd", 8, rate), "{rate}");
        }
        assert!(!on.app_decodes("mlp"), "only the codec PMS spells truehd");
        assert_eq!(on.audio.split(',').filter(|c| *c == "truehd").count(), 1);
        assert_eq!(on.clone().with_app_lossy(), on, "idempotent");
        assert_eq!(app_decoded_by_ffmpeg_name("truehd").map(|c| (c.id, c.lossy)), Some(("truehd", true)));
    }

    /// The policy is one predicate over the table: a read table admits exactly the rows with recorded
    /// evidence (DTS today), the assumed fallback none, the debug trigger every lossy row, and a lossless
    /// row is never a lossy one whatever the policy.
    #[test]
    fn a_read_table_enables_only_measured_lossy_rows() {
        for row in APP_DECODED {
            for policy in [AppLossy::Off, AppLossy::Measured, AppLossy::All] {
                let want = row.lossy
                    && match policy {
                        AppLossy::Off => false,
                        AppLossy::Measured => row.measured.is_some(),
                        AppLossy::All => true,
                    };
                assert_eq!(lossy_row_enabled(row, policy), want, "{} under {policy:?}", row.id);
            }
        }
        let read = parse(REAL).unwrap();
        for spelling in ["dts", "dca", "DTS"] {
            assert!(read.app_may_decode_lossy(spelling), "{spelling}: measured");
        }
        for codec in ["mp3", "vorbis", "truehd", "flac", "ac3", "mlp"] {
            assert!(!read.app_may_decode_lossy(codec), "{codec}: unmeasured, lossless or not ours");
            assert_eq!(read.clone().with_app_lossy_for(codec), read, "{codec}: nothing to put in force");
        }
        assert!(!Caps::assumed().app_may_decode_lossy("dts"), "an unread table is not a policy");
        assert_eq!(Caps::assumed().with_app_lossy_for("dts"), Caps::assumed());
        let all = Caps::assumed().with_app_lossy();
        assert_eq!(all.app_lossy, AppLossy::All);
        let lossy: Vec<&str> = APP_DECODED.iter().filter(|r| r.lossy).map(|r| r.id).collect();
        assert_eq!(all.app_lossy_in_force.iter().map(String::as_str).collect::<Vec<_>>().len(), lossy.len());
        assert!(lossy.iter().all(|id| all.app_lossy_in_force.contains(*id)), "every lossy row in force");
    }

    /// A row is turned on by its evidence, so the evidence has to say something, and a lossless row
    /// (where the field means nothing) carries none.
    #[test]
    fn every_enabled_lossy_row_carries_its_measurement() {
        for row in APP_DECODED {
            match (row.lossy, row.measured) {
                (true, Some(evidence)) => assert!(evidence.len() > 20 && evidence.contains("U5"), "{}: {evidence}", row.id),
                (false, measured) => assert!(measured.is_none(), "{}: lossless rows carry no measurement", row.id),
                (true, None) => {}
            }
        }
        assert!(APP_DECODED.iter().any(|r| r.id == "dts" && r.measured.is_some()), "DTS is the measured row");
    }

    /// Force Direct Play decodes in the app only where the set has no path of its own for the track: a
    /// set whose table lists DTS keeps playing it natively, and one without a DTS row (webOS 10+) does
    /// not. Auto never takes this path on its own, and a refusal the install learned
    /// ([`Caps::minus`]) is "no native path" for that width.
    #[test]
    fn forced_app_decode_is_only_where_the_set_has_no_native_path() {
        let native = parse(REAL).unwrap();
        assert!(native.audio_has("dts"));
        assert!(!native.app_decodes_lossy_track("dts", 6, 0, true), "the set decodes DTS itself");
        assert!(native.native_audio_track("dts", 6, 0, true));
        let rowless = parse(&REAL.replace("DTS", "WMA")).unwrap();
        assert!(!rowless.audio_has("dts"));
        for spelling in ["dts", "dca", "DTS"] {
            // a stereo core at any rate: nothing to mix down (a surround one is `forced_app_decode_never_mixes_a_surround_source_down`)
            assert!(rowless.app_decodes_lossy_track(spelling, 2, 96_000, true), "{spelling}");
        }
        assert!(!rowless.native_audio_track("dts", 8, 0, true));
        assert!(!rowless.app_decodes_lossy_track("dts", 6, 0, false), "Auto converts on the server instead");
        assert!(!Caps::assumed().app_decodes_lossy_track("dts", 2, 0, true), "off is off");
        let learned = native.minus(&[refusal("dts", 6)], "");
        assert!(
            !learned.app_decodes_lossy_track("dts", 6, 0, true),
            "a learned surround refusal is still a surround track: Force Direct Play shows the refusal, it does not mix down"
        );
        assert!(!learned.app_decodes_lossy_track("dts", 2, 0, true));
        let unmeasured = rowless.clone();
        for codec in ["truehd", "mp3", "vorbis"] {
            assert!(!unmeasured.app_decodes_lossy_track(codec, 2, 44_100, true), "{codec}: no evidence, no decode");
        }
        assert!(!rowless.app_decodes_lossy_track("ac3", 6, 0, true), "only the lossy rows");
    }

    /// **The app never mixes a surround source down by default** (owner, 2026-10-28, #1082). Force Direct
    /// Play, on a set with no path of its own for the track, decodes in the app only a source that is
    /// already stereo or mono; a surround one (or one whose count is not stated) is refused, whatever
    /// codec and rate. The rule holds for a codec the recovery rung put in force too (the rung is
    /// reached by a stereo track only under Forced), but not for Auto's last rung, which is exactly
    /// where the downmix is allowed, nor under the `appdecodelossy` trigger, an explicit override.
    #[test]
    fn forced_app_decode_never_mixes_a_surround_source_down() {
        for (channels, mixes) in [(-1, true), (0, true), (1, false), (2, false), (3, true), (6, true), (8, true)] {
            assert_eq!(app_decode_mixes_down(channels), mixes, "{channels} channels");
        }
        let rowless = parse(&REAL.replace("DTS", "WMA")).unwrap();
        assert!(rowless.app_lossy == AppLossy::Measured && !rowless.audio_has("dts"));
        for spelling in ["dts", "dca", "DTS"] {
            for channels in [0, 3, 6, 8] {
                assert!(!rowless.app_decodes_lossy_track(spelling, channels, 48_000, true), "{spelling} {channels}ch");
            }
            for channels in [1, 2] {
                assert!(rowless.app_decodes_lossy_track(spelling, channels, 48_000, true), "{spelling} {channels}ch");
            }
        }
        assert!(!rowless.app_may_mix_down(6, true) && rowless.app_may_mix_down(2, true));
        assert!(rowless.app_may_mix_down(6, false), "Auto asks the ladder's order, not this gate");
        // A set that has the decoder and a refusal the install learned: a learned surround refusal leaves
        // stereo to the set's own decoder, and the surround track stays refused under Force.
        let learned = parse(REAL).unwrap().minus(&[refusal("dts", 6)], "");
        assert!(!learned.app_decodes_lossy_track("dts", 6, 0, true));
        // The recovery rung's value: Auto may use it for any width (the last rung), Force only for stereo.
        let rescue = rowless.clone().with_app_lossy_for("dts");
        assert!(rescue.app_decodes_lossy_track("dts", 6, 0, false), "Auto's last rung decodes a surround track");
        assert!(rescue.app_decodes_lossy_track("dts", 2, 0, true));
        assert!(
            !rescue.app_decodes_lossy_track("dts", 6, 0, true),
            "a codec in force by the rung still does not mix a surround pick down under Force"
        );
        // The debug trigger is the explicit override (it measures a decode on any file), in both modes.
        let all = Caps::assumed().with_app_lossy();
        for forced in [false, true] {
            assert!(all.app_decodes_lossy_track("dts", 8, 96_000, forced) && all.app_may_mix_down(8, forced));
        }
    }

    /// The recovery rung's value puts ONE codec in force: the others, and the policy, stay as they were.
    #[test]
    fn the_rescue_value_decodes_one_codec_in_the_app_and_no_other() {
        let base = parse(REAL).unwrap();
        let rescue = base.clone().with_app_lossy_for("DCA");
        assert_eq!(rescue.app_lossy_in_force.iter().map(String::as_str).collect::<Vec<_>>(), ["dts"]);
        assert_eq!(rescue.app_lossy, AppLossy::Measured);
        assert_eq!(rescue.audio_channels.get("dts"), Some(&8));
        assert!(rescue.app_decodes("dts") && !rescue.app_decodes("truehd") && !rescue.app_decodes("mp3"));
        assert!(!rescue.native_audio_track("dts", 6, 0, false), "the set's own decoder was refused");
        assert!(rescue.app_decodes_lossy_track("dts", 6, 0, false), "so Auto plans the app decode too");
        assert!(rescue.native_audio_track("ac3", 6, 0, false) && rescue.native_audio_track("eac3", 8, 0, false));
        let rowless = parse(&REAL.replace("DTS", "WMA")).unwrap().with_app_lossy_for("dts");
        assert!(rowless.audio_has("dts") && rowless.audio_supports("dts", 8), "announced without a table row");
        assert_eq!(rowless.audio.split(',').filter(|c| *c == "dts").count(), 1);
        assert_eq!(rescue.clone().with_app_lossy_for("dts"), rescue, "idempotent");
        assert_ne!(rescue, base);
    }

    /// Lossless two-channel codecs share FLAC's path (S4, PR 1): ALAC, and LPCM under every spelling
    /// PMS or the container uses for the byte orders and widths the bundled FFmpeg decodes. A PCM
    /// flavour it cannot decode (float, 8-bit, an exotic layout) is NOT claimed: the server converts it.
    #[test]
    fn alac_and_lpcm_are_pcm_decoded_like_flac() {
        for spelling in ["pcm", "PCM", "lpcm", "pcm_s16le", "pcm_s16be", "PCM_S24LE", "pcm_s24be", "pcm_s32le", "pcm_s32be"] {
            assert_eq!(audio_codec_id(spelling), "pcm", "{spelling}");
            assert!(is_pcm_codec(spelling), "{spelling}");
        }
        for not_ours in ["pcm_f32le", "pcm_u8", "pcm_s8", "pcm_bluray", "pcm_dvd", "pcm_mulaw", "adpcm_ima_wav"] {
            assert_eq!(audio_codec_id(not_ours), not_ours, "{not_ours} stays unclaimed");
            assert!(!is_pcm_codec(not_ours), "{not_ours}");
        }
        assert!(is_pcm_codec("alac") && is_pcm_codec("ALAC"));
        let caps = parse(REAL).unwrap();
        for codec in ["alac", "pcm", "pcm_s24le", "flac"] {
            assert!(caps.audio_has(codec), "{codec}");
            assert!(caps.audio_supports(codec, 2), "{codec}");
            assert!(!caps.audio_supports(codec, 6), "{codec}: wider than stereo is the server's");
            assert!(!caps.audio_supports(codec, 0), "{codec}: an unknown count is not admitted");
            assert!(caps.forced_feeds_audio(codec), "{codec}");
            assert!(!Caps::assumed().forced_feeds_audio(codec), "{codec}: the assumed fallback stays what shipped");
            assert!(audio_track_fits(codec, 2, 44_100), "{codec}");
            assert!(audio_track_fits(codec, 2, 96_000), "{codec}: 96 kHz is resampled to 48 kHz");
            assert!(!audio_track_fits(codec, 6, 96_000), "{codec}: wider than stereo is the server's at any rate");
            assert!(!audio_track_fits(codec, 6, 48_000), "{codec}");
        }
        assert_eq!(Caps::assumed().audio, "aac,ac3,eac3");
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
        assert_eq!(c.audio, "aac,opus,flac,alac,pcm");
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
        assert_eq!(c.audio, "aac,ac3,eac3,opus,flac,alac,pcm");
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

    /// A video facet above the 1080p floor lowers THAT codec's raster bound and nothing else; one at
    /// or under the floor, or a bound already at it, changes nothing, so the recovery ladder's loop
    /// guard (`start.without(f) != start`) skips a rung that would not change the request.
    #[test]
    fn without_a_video_facet_lowers_that_codecs_raster_to_the_1080p_floor() {
        let assumed = Caps::assumed();
        let video = |codec: &str, width, height| Facet::Video { codec: codec.into(), width, height };
        let narrowed = assumed.without(&video("h264", 3840, 2160));
        assert_eq!(narrowed.raster_bound("h264"), VIDEO_REFUSAL_FLOOR);
        assert_eq!(narrowed.raster_bound("hevc"), (3840, 2176));
        assert_eq!((&narrowed.audio, &narrowed.audio_channels), (&assumed.audio, &assumed.audio_channels));
        assert_eq!(assumed.without(&video("h264", 1920, 1080)), assumed, "nothing to lower");
        assert_eq!(narrowed.without(&video("h264", 3840, 2160)), narrowed, "idempotent");
        let mut one_080p = parse(REAL).unwrap();
        one_080p.hevc_max = (1920, 1088);
        assert_eq!(one_080p.without(&video("h264", 3840, 2160)), one_080p, "a bound already at the floor");
    }

    /// `raster_bound` is the per-axis lowest of the shared bound, the codec's own row and the
    /// attempt's ceiling; a row that states nothing bounds nothing.
    #[test]
    fn raster_bound_is_the_min_of_the_shared_bound_the_row_and_the_ceiling() {
        let caps = Caps { av1: true, av1_row: (1920, 1080, 60), ..Caps::assumed() };
        assert_eq!(caps.raster_bound("av1"), (1920, 1080));
        assert_eq!(caps.raster_bound("vp9"), caps.hevc_max, "no VP9 row states nothing");
        assert_eq!(caps.raster_bound("h264"), caps.hevc_max);
        let capped = Caps { video_ceiling: [("av1".to_owned(), (1280, 720))].into(), ..caps };
        assert_eq!(capped.raster_bound("av1"), (1280, 720));
    }
}
