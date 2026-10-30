//! **"Convert audio: on the TV"**: audio the television's decoder does not take, decoded here and
//! re-encoded to AC3 so the file still direct-plays.
//!
//! The Starfish pipeline decodes AC3, E-AC3, AAC and DTS (`engine::audio_payload_codec`). Every
//! other codec PMS can carry in a direct-playable container (TrueHD, FLAC, ALAC, Opus, Vorbis,
//! PCM) is a server transcode by default — the whole file re-muxed or re-encoded so its audio
//! arrives as one of those four. With Settings > Playback > Convert audio on the TV the profile
//! offers those codecs for direct play instead (`devcaps::converted_on_tv`), the Load declares AC3,
//! and the demuxer hands each packet of the selected track to an [`AudioConverter`]: the bundled
//! libavcodec decodes it, libswresample converts it to planar float at an AC3 rate and folds it to
//! the AC3 layout, and the bundled `ac3` encoder produces the frames the audio lane carries.
//!
//! **Why AC3 and not PCM.** LG's buffer-feed pipeline accepts PCM only as 16-bit stereo (libpf's
//! `LUT_ChannelModeType`/`LUT_BPSType`, per SchulzCode's decompile below), so PCM would lose every
//! surround channel. AC3 is the format every webOS set and every receiver behind it decodes, and it
//! carries 5.1: 5.1 (and anything from three to six channels) is encoded as 5.1 at 640 kb/s, 7.1 is
//! folded to 5.1 by libswresample's default matrix, and mono or stereo is stereo at 256 kb/s.
//! AC3 has three sample rates (48, 44.1 and 32 kHz): those pass through, a multiple of 44.1 kHz
//! (88.2, 176.4) resamples to 44.1, and anything else to 48.
//!
//! **The clock.** The first decoded frame's timestamp is the stream's anchor, and every AC3 frame
//! after it is placed by the samples the encoder has been fed before it. A seek resets the anchor
//! ([`AudioConverter::reset`]), since the decoder's next frame is from somewhere else in the file.
//!
//! Adapted from SchulzCode/WebOS-Jellyfin-Native (0BSD), `src/player/audio.rs` at 63daba7, which
//! measured the cost at ~11% of one core on webOS 10. Differences: the bundled FFmpeg 9.0's
//! `AVChannelLayout` API rather than the TV's `channel_layout` mask, AVOption names for the encoder
//! where libavcodec has them, and the pure halves (the target, the sample queue, the clock) split
//! out so the host suite can grade them without FFmpeg.
//!
//! **Where it touches `ff.rs`** (kept to code, with the prose here): `ff.rs` also owns the shipping
//! AC3 encode path through this module, besides the dev capture stream's `venc`. It loads FIVE
//! libraries, `libswresample` (this module's `swresample::load`) between `libavutil` and `libavcodec`,
//! because libavformat NEEDs libavcodec NEEDs libswresample and libavutil by SONAME; an
//! all-or-nothing loop over all five would have found four without swscale.
use super::*;

/// Convert the selected audio track to AC3 unless the pipeline takes it as it is. Set by
/// `engine::start_bufferfeed` from the same answer that made its Load declare AC3
/// (`devcaps::converted_on_tv`), so the declaration and the bytes cannot disagree.
static CONVERT_AUDIO: AtomicBool = AtomicBool::new(false);
pub fn set_convert_audio(on: bool) {
    CONVERT_AUDIO.store(on, Ordering::Relaxed);
}

/// Does the app decode this track itself and feed the TV PCM (`devcaps::app_decoded_by_ffmpeg_name`)?
/// Not while the Load declared AC3 for it: a FLAC, ALAC or integer PCM track the PCM path cannot carry
/// (surround, or a rate `pcm_load_rate` has no answer for) is the converter's, and the two must never both claim one
/// stream, or the PCM path refuses a Load that declared no PCM.
pub(super) fn app_decodes(name: &str) -> bool {
    app_decodes_with(CONVERT_AUDIO.load(Ordering::Relaxed), name)
}

pub(super) fn app_decodes_with(converting: bool, name: &str) -> bool {
    !converting && plx_platform::devcaps::app_decoded_by_ffmpeg_name(name).is_some()
}

plx_base::dynlib! {
    // The audio converter's sample-format, rate and layout conversion (`ff_audio.rs`). REQUIRED,
    // unlike swscale: libavcodec itself names it in DT_NEEDED (its Opus decoder resamples), so a
    // package without it has no libavcodec either.
    swresample: [
        // The ELF the app SHIPS, and the Mach-O the desktop simulator builds beside it.
        "libswresample-plx.so.7",
        "libswresample-plx.7.dylib",
    ] {
    fn swresample_version() -> c_uint;
    fn swr_alloc_set_opts2(
        ps: *mut *mut c_void,
        out_ch_layout: *const AVChannelLayout,
        out_sample_fmt: c_int,
        out_sample_rate: c_int,
        in_ch_layout: *const AVChannelLayout,
        in_sample_fmt: c_int,
        in_sample_rate: c_int,
        log_offset: c_int,
        log_ctx: *mut c_void,
    ) -> c_int;
    fn swr_init(s: *mut c_void) -> c_int;
    fn swr_free(s: *mut *mut c_void);
    fn swr_convert(
        s: *mut c_void,
        out: *mut *mut u8,
        out_count: c_int,
        input: *const *const u8,
        in_count: c_int,
    ) -> c_int;
    fn swr_get_out_samples(s: *mut c_void, in_samples: c_int) -> c_int;
}}

/// The `AVSampleFormat` of the AC3 encoder's input, planar float (asserted in ci/ffabi-assert.c).
const AV_SAMPLE_FMT_FLTP: c_int = 8;
/// `AV_CHANNEL_ORDER_UNSPEC`: a layout that only says how many channels (asserted).
const AV_CHANNEL_ORDER_UNSPEC: c_int = 0;
/// AC3's sample rates, the ones that pass through unchanged.
const AC3_RATES: [c_int; 3] = [48_000, 44_100, 32_000];
/// The AC3 frame, in samples per channel. The encoder states it after `avcodec_open2` and that is
/// what is used; this is only the fallback for an encoder that did not.
const AC3_FRAME_SAMPLES: usize = 1536;

/// `AVFrame` fields the converter reads off a decoded frame and writes into the encoder's input,
/// at the FFmpeg 9.0 offsets `ci/ffabi-assert.c` holds (`tools/ffabi-dump.sh` re-derives them).
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_EXTENDED_DATA: usize = 64;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_NB_SAMPLES: usize = 76;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_SAMPLE_RATE: usize = 132;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_BEST_EFFORT_TS: usize = 208;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_CH_LAYOUT: usize = 256;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_EXTENDED_DATA: usize = 96;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_NB_SAMPLES: usize = 112;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_SAMPLE_RATE: usize = 180;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_BEST_EFFORT_TS: usize = 304;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_CH_LAYOUT: usize = 384;
/// `AVCodecContext.sample_fmt`: the one encoder field libavcodec has no AVOption for (its table has
/// only the decoders' `request_sample_fmt`), so it is poked before `avcodec_open2`.
#[cfg(target_pointer_width = "32")]
const OFF_CTX_SAMPLE_FMT: usize = 304;
#[cfg(target_pointer_width = "64")]
const OFF_CTX_SAMPLE_FMT: usize = 348;

/// What one source becomes: the AC3 the encoder is opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ac3Target {
    pub channels: c_int,
    pub sample_rate: c_int,
    pub bit_rate: i64,
}

impl Ac3Target {
    /// The AC3 for a source of `channels` at `sample_rate`: stereo at 256 kb/s up to two channels,
    /// 5.1 at 640 kb/s above (7.1 folds to 5.1); the rate kept when AC3 has it, else 44.1 kHz for
    /// a multiple of it and 48 kHz for everything else. An unknown channel count (0) is stereo.
    pub fn for_source(channels: c_int, sample_rate: c_int) -> Ac3Target {
        let surround = channels > 2;
        let rate = if AC3_RATES.contains(&sample_rate) {
            sample_rate
        } else if sample_rate > 0 && sample_rate % 44_100 == 0 {
            44_100
        } else {
            48_000
        };
        Ac3Target {
            channels: if surround { 6 } else { 2 },
            sample_rate: rate,
            bit_rate: if surround { 640_000 } else { 256_000 },
        }
    }

    /// The layout's AVOption spelling, which `av_opt_set` parses into the encoder's `ch_layout`.
    pub fn layout_name(self) -> &'static str {
        if self.channels == 6 { "5.1" } else { "stereo" }
    }
}

/// Planar float samples waiting for a whole AC3 frame, one queue per output channel.
#[derive(Debug, Default)]
pub(crate) struct SampleQueue {
    channels: Vec<Vec<f32>>,
}

impl SampleQueue {
    pub fn new(channels: usize) -> SampleQueue {
        SampleQueue { channels: vec![Vec::new(); channels] }
    }

    /// Append `n` samples of each channel from `planes` (one slice per channel, each at least `n`).
    pub fn push(&mut self, planes: &[&[f32]], n: usize) {
        for (q, p) in self.channels.iter_mut().zip(planes) {
            q.extend_from_slice(&p[..n]);
        }
    }

    /// Samples queued per channel.
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Move the first `n` samples of each channel into `out` (one slice per channel), or do nothing
    /// and return false while fewer than `n` are queued.
    pub fn pop_into(&mut self, n: usize, out: &mut [&mut [f32]]) -> bool {
        if self.len() < n {
            return false;
        }
        for (q, o) in self.channels.iter_mut().zip(out.iter_mut()) {
            o[..n].copy_from_slice(&q[..n]);
            q.drain(..n);
        }
        true
    }

    pub fn clear(&mut self) {
        for q in &mut self.channels {
            q.clear();
        }
    }
}

/// An AC3 frame's position in nanoseconds: the anchor plus the samples (at the output rate) fed
/// to the encoder before it.
pub(crate) fn ac3_pts_ns(anchor_ns: i64, samples_before: i64, sample_rate: c_int) -> i64 {
    if sample_rate <= 0 {
        return anchor_ns;
    }
    anchor_ns + (samples_before as i128 * 1_000_000_000 / sample_rate as i128) as i64
}

/// Whether the pipeline takes this FFmpeg codec name itself, so it is fed as it is and never
/// converted: the four `engine::audio_payload_codec` knows.
pub(crate) fn fed_natively(codec_name: &str) -> bool {
    matches!(codec_name, "aac" | "ac3" | "eac3" | "dts")
}

/// The decoders "Convert audio: on the TV" needs, by FFmpeg name: one per codec it offers
/// (`devcaps::TV_CONVERTED_AUDIO_CODECS`), and the PCM layouts Matroska and MP4 carry most.
const NEEDED_DECODERS: [&std::ffi::CStr; 7] =
    [c"truehd", c"flac", c"alac", c"opus", c"vorbis", c"pcm_s16le", c"pcm_s24le"];

/// Can the FFmpeg this app loaded convert at all: the `ac3` encoder and every needed decoder are in
/// it. `ff::boot` asks once and hands the answer to `devcaps`, so a build whose component list lost
/// one of them offers the server its old profile instead of direct-playing what it cannot decode.
pub(crate) fn converter_available() -> bool {
    unsafe {
        let missing: Vec<&str> = std::iter::once(c"ac3")
            .filter(|n| avcodec_find_encoder_by_name(n.as_ptr()).is_null())
            .chain(NEEDED_DECODERS.into_iter().filter(|n| avcodec_find_decoder_by_name(n.as_ptr()).is_null()))
            .map(|n| n.to_str().unwrap_or("?"))
            .collect();
        if missing.is_empty() {
            crate::player::log("ff: audio converter ready (TrueHD, FLAC, ALAC, Opus, Vorbis, PCM -> AC3)");
            true
        } else {
            crate::player::log(&format!("ff: no audio converter: the bundled FFmpeg lacks {}", missing.join(", ")));
            false
        }
    }
}

#[inline]
unsafe fn peek<T: Copy>(base: *const c_void, off: usize) -> T {
    *((base as *const u8).add(off) as *const T)
}

/// The resampler's input as the decoder states it per frame; a change rebuilds the resampler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InputFormat {
    format: c_int,
    sample_rate: c_int,
    channels: c_int,
}

/// What the converter builds from the first decoded frame: the encoder, the resampler into it, and
/// the queue between them. The source's real channel count and rate are known only once a frame
/// has decoded (`avformat_find_stream_info` can leave a TrueHD track at 0 channels), and the Load
/// declares only "AC3", so nothing upstream needs them earlier.
struct Output {
    enc: *mut AVCodecContext,
    target: Ac3Target,
    frame_samples: usize,
    queue: SampleQueue,
}

/// One selected audio track being converted. Every pointer is library-allocated and freed in
/// `Drop`; the demux thread owns it, like the `AVFormatContext` it reads from.
pub(crate) struct AudioConverter {
    dec: *mut AVCodecContext,
    swr: *mut c_void,
    input: Option<InputFormat>,
    out: Option<Output>,
    frame: *mut c_void,
    pkt: *mut AVPacket,
    /// Converted samples, reused per decoded frame (one buffer per output channel).
    scratch: Vec<Vec<f32>>,
    /// The stream's time base, which the decoder's timestamps are in.
    tb: AVRational,
    /// Where the first sample kept since the last anchor sits on the stream clock, in ns.
    anchor_ns: Option<i64>,
    /// Samples handed to the encoder since the anchor.
    fed: i64,
    /// The source codec's FFmpeg name, upper-cased, for the log.
    source: String,
    /// `TRUEHD 8ch 48000 Hz -> AC3 5.1 640 kb/s`, once the first frame has decoded.
    label: String,
    /// Packets the decoder refused, logged at the first and then counted.
    refused: u32,
}

impl AudioConverter {
    /// The converter for the selected audio track `ai` of `streams`, when the Load declared AC3
    /// for it ([`set_convert_audio`]) and the pipeline cannot take it as it is (the viewer may
    /// have picked an AC3 track after the Load). `Err` when it needs one and none opens: an AC3
    /// declaration over nothing is the stall the payload rule exists to prevent.
    pub unsafe fn for_track(streams: *mut *mut AVStream, ai: c_int) -> Result<Option<AudioConverter>, ()> {
        if ai < 0 || !CONVERT_AUDIO.load(Ordering::Relaxed) || !FEED_AUDIO.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let ast = *streams.add(ai as usize);
        let acp = stream_codecpar(ast);
        let name = std::ffi::CStr::from_ptr(avcodec_get_name((*acp).codec_id)).to_string_lossy().into_owned();
        if fed_natively(&name) {
            return Ok(None);
        }
        match AudioConverter::open(acp, stream_time_base(ast)) {
            Some(conv) => Ok(Some(conv)),
            None => {
                crate::player::log(&format!(
                    "ff: the Load declared AC3 for a {name} track this build cannot convert; refusing the stream"
                ));
                Err(())
            }
        }
    }

    /// A converter for the stream `cp` (time base `tb`), or `None` with the reason logged: no
    /// decoder for it in the bundled FFmpeg, no `ac3` encoder, or the decoder refused to open.
    pub unsafe fn open(cp: *const AVCodecParameters, tb: AVRational) -> Option<AudioConverter> {
        let name = std::ffi::CStr::from_ptr(avcodec_get_name((*cp).codec_id)).to_string_lossy().into_owned();
        if avcodec_find_encoder_by_name(c"ac3".as_ptr()).is_null() {
            crate::player::log("ff: audio convert: no ac3 encoder in the bundled FFmpeg");
            return None;
        }
        let codec = avcodec_find_decoder((*cp).codec_id);
        if codec.is_null() {
            crate::player::log(&format!("ff: audio convert: no {name} decoder in the bundled FFmpeg"));
            return None;
        }
        let conv = AudioConverter {
            dec: avcodec_alloc_context3(codec),
            swr: std::ptr::null_mut(),
            input: None,
            out: None,
            frame: av_frame_alloc(),
            pkt: av_packet_alloc(),
            scratch: Vec::new(),
            tb,
            anchor_ns: None,
            fed: 0,
            source: name.to_uppercase(),
            label: String::new(),
            refused: 0,
        };
        if conv.dec.is_null() || conv.frame.is_null() || conv.pkt.is_null() {
            return None;
        }
        // The decoder places its frames by the packets' timestamps, in the stream's time base.
        let tb_str = CString::new(format!("{}/{}", tb.num, tb.den)).unwrap();
        if avcodec_parameters_to_context(conv.dec, cp) < 0
            || av_opt_set(conv.dec as *mut c_void, c"pkt_timebase".as_ptr(), tb_str.as_ptr(), 0) < 0
            || avcodec_open2(conv.dec, codec, std::ptr::null_mut()) < 0
        {
            crate::player::log(&format!("ff: audio convert: the {name} decoder refused to open"));
            return None;
        }
        Some(conv)
    }

    /// What the converter is doing, once the first frame has decoded (empty before).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Forget where the stream was: the next decoded frame is somewhere else (a seek). The decoder
    /// drops what it buffered and the queued samples go; the encoder and the resampler stay.
    pub unsafe fn reset(&mut self) {
        avcodec_flush_buffers(self.dec);
        if let Some(out) = self.out.as_mut() {
            out.queue.clear();
        }
        self.anchor_ns = None;
        self.fed = 0;
    }

    /// Decode one demuxed packet; returns every AC3 frame it completes, as (pts in ns, bytes). A
    /// packet the decoder refuses produces nothing (the next one usually decodes).
    pub unsafe fn decode(&mut self, pkt: *const AVPacket) -> Vec<(i64, Vec<u8>)> {
        let mut out = Vec::new();
        let r = avcodec_send_packet(self.dec, pkt);
        if r < 0 {
            self.refused += 1;
            if self.refused == 1 {
                crate::player::log(&format!("ff: audio convert: the {} decoder refused a packet ({r})", self.source));
            }
            return out;
        }
        while avcodec_receive_frame(self.dec, self.frame) == 0 {
            self.take_frame();
            av_frame_unref(self.frame);
            self.encode_ready(&mut out);
        }
        out
    }

    /// Resample the decoded frame in `self.frame` into the queue.
    unsafe fn take_frame(&mut self) {
        let f = self.frame as *const c_void;
        let n_in: c_int = peek(f, OFF_FRAME_NB_SAMPLES);
        if n_in <= 0 {
            return;
        }
        if self.anchor_ns.is_none() {
            let ts: i64 = peek(f, OFF_FRAME_BEST_EFFORT_TS);
            if ts == AV_NOPTS_VALUE {
                // Nothing to place it by: wait for a frame that says where it is.
                return;
            }
            self.anchor_ns = Some(av_rescale_q(ts, self.tb, NS_TB));
        }
        let layout = (f as *const u8).add(OFF_FRAME_CH_LAYOUT) as *const AVChannelLayout;
        let input = InputFormat {
            format: peek(f, OFF_FRAME_FORMAT),
            sample_rate: peek(f, OFF_FRAME_SAMPLE_RATE),
            channels: (*layout).nb_channels,
        };
        if self.out.is_none() && !self.open_output(input) {
            return;
        }
        if self.input != Some(input) && !self.open_resampler(input, layout) {
            return;
        }
        let cap = swr_get_out_samples(self.swr, n_in).max(0) as usize;
        for b in &mut self.scratch {
            b.resize(cap, 0.0);
        }
        let mut planes: Vec<*mut u8> = self.scratch.iter_mut().map(|b| b.as_mut_ptr() as *mut u8).collect();
        let data: *const *const u8 = peek(f, OFF_FRAME_EXTENDED_DATA);
        let got = swr_convert(self.swr, planes.as_mut_ptr(), cap as c_int, data, n_in);
        if got <= 0 {
            return;
        }
        let slices: Vec<&[f32]> = self.scratch.iter().map(Vec::as_slice).collect();
        if let Some(out) = self.out.as_mut() {
            out.queue.push(&slices, got as usize);
        }
    }

    /// Open the AC3 encoder this source calls for, from its first decoded frame.
    unsafe fn open_output(&mut self, input: InputFormat) -> bool {
        let target = Ac3Target::for_source(input.channels, input.sample_rate);
        let Some((enc, frame_samples)) = open_ac3_encoder(target) else { return false };
        self.label = format!(
            "{} {}ch {} Hz -> AC3 {} {} kb/s",
            self.source,
            input.channels,
            input.sample_rate,
            target.layout_name(),
            target.bit_rate / 1000
        );
        crate::player::log(&format!("ff: audio convert on the TV: {}", self.label));
        self.scratch = vec![Vec::new(); target.channels as usize];
        self.out = Some(Output { enc, target, frame_samples, queue: SampleQueue::new(target.channels as usize) });
        true
    }

    /// (Re)build the resampler for this frame's format, rate and layout. A layout that names only
    /// a channel count (PCM in Matroska) gets the default layout for that count.
    unsafe fn open_resampler(&mut self, input: InputFormat, layout: *const AVChannelLayout) -> bool {
        if !self.swr.is_null() {
            swr_free(&mut self.swr);
        }
        self.input = None;
        let Some(target) = self.out.as_ref().map(|o| o.target) else { return false };
        let mut default_in: AVChannelLayout = std::mem::zeroed();
        let in_layout = if (*layout).order == AV_CHANNEL_ORDER_UNSPEC {
            av_channel_layout_default(&mut default_in, input.channels.max(1));
            &default_in as *const AVChannelLayout
        } else {
            layout
        };
        let mut out_layout: AVChannelLayout = std::mem::zeroed();
        av_channel_layout_default(&mut out_layout, target.channels);
        let r = swr_alloc_set_opts2(
            &mut self.swr,
            &out_layout,
            AV_SAMPLE_FMT_FLTP,
            target.sample_rate,
            in_layout,
            input.format,
            input.sample_rate,
            0,
            std::ptr::null_mut(),
        );
        av_channel_layout_uninit(&mut out_layout);
        av_channel_layout_uninit(&mut default_in);
        if r < 0 || self.swr.is_null() || swr_init(self.swr) < 0 {
            crate::player::log(&format!("ff: audio convert: no resampler for {input:?}"));
            if !self.swr.is_null() {
                swr_free(&mut self.swr);
            }
            return false;
        }
        self.input = Some(input);
        true
    }

    /// Encode every whole AC3 frame the queue holds.
    unsafe fn encode_ready(&mut self, sink: &mut Vec<(i64, Vec<u8>)>) {
        let (Some(anchor), Some(out)) = (self.anchor_ns, self.out.as_mut()) else { return };
        let n = out.frame_samples;
        while out.queue.len() >= n {
            let mut ef = av_frame_alloc();
            if ef.is_null() {
                return;
            }
            poke_i32(ef, OFF_FRAME_NB_SAMPLES, n as i32);
            poke_i32(ef, OFF_FRAME_FORMAT, AV_SAMPLE_FMT_FLTP);
            poke_i32(ef, OFF_FRAME_SAMPLE_RATE, out.target.sample_rate);
            av_channel_layout_default((ef as *mut u8).add(OFF_FRAME_CH_LAYOUT) as *mut AVChannelLayout, out.target.channels);
            if av_frame_get_buffer(ef, 0) < 0 {
                av_frame_free(&mut ef);
                return;
            }
            let data = (ef as *mut u8).add(OFF_FRAME_DATA) as *const *mut f32;
            let mut planes: Vec<&mut [f32]> = (0..out.target.channels as usize)
                .map(|c| std::slice::from_raw_parts_mut(*data.add(c), n))
                .collect();
            out.queue.pop_into(n, &mut planes);
            drop(planes);
            let frame_pts = self.fed;
            poke_i64(ef, OFF_FRAME_PTS, frame_pts);
            self.fed += n as i64;
            let r = avcodec_send_frame(out.enc, ef);
            av_frame_free(&mut ef);
            if r < 0 {
                return;
            }
            while avcodec_receive_packet(out.enc, self.pkt) == 0 {
                let p = self.pkt;
                if !(*p).data.is_null() && (*p).size > 0 {
                    let bytes = std::slice::from_raw_parts((*p).data, (*p).size as usize).to_vec();
                    // Placed at the samples it was fed from, not at the packet's own `pts`, which
                    // is 256 samples earlier for the encoder's priming: the first AU would land
                    // before the anchor (before 0 at the start of a file), and leaving the 5 ms
                    // in costs nothing anyone can hear.
                    sink.push((ac3_pts_ns(anchor, frame_pts, out.target.sample_rate), bytes));
                }
                av_packet_unref(p);
            }
        }
    }
}

/// The `ac3` encoder opened for `target`, with its frame size in samples.
unsafe fn open_ac3_encoder(target: Ac3Target) -> Option<(*mut AVCodecContext, usize)> {
    let codec = avcodec_find_encoder_by_name(c"ac3".as_ptr());
    if codec.is_null() {
        return None;
    }
    let mut enc = avcodec_alloc_context3(codec);
    if enc.is_null() {
        return None;
    }
    let obj = enc as *mut c_void;
    let layout = CString::new(target.layout_name()).unwrap();
    let tb = CString::new(format!("1/{}", target.sample_rate)).unwrap();
    poke_i32(obj, OFF_CTX_SAMPLE_FMT, AV_SAMPLE_FMT_FLTP);
    let ok = av_opt_set_int(obj, c"ar".as_ptr(), i64::from(target.sample_rate), 0) >= 0
        && av_opt_set_int(obj, c"b".as_ptr(), target.bit_rate, 0) >= 0
        && av_opt_set(obj, c"ch_layout".as_ptr(), layout.as_ptr(), 0) >= 0
        && av_opt_set(obj, c"time_base".as_ptr(), tb.as_ptr(), 0) >= 0
        && avcodec_open2(enc, codec, std::ptr::null_mut()) >= 0;
    if !ok {
        crate::player::log(&format!("ff: audio convert: the ac3 encoder refused {target:?}"));
        avcodec_free_context(&mut enc);
        return None;
    }
    // The frame size, read back through the library rather than at an AVCodecContext offset.
    let par = avcodec_parameters_alloc();
    let mut frame_samples = AC3_FRAME_SAMPLES;
    if !par.is_null() {
        if avcodec_parameters_from_context(par, enc) >= 0 && (*par).frame_size > 0 {
            frame_samples = (*par).frame_size as usize;
        }
        avcodec_parameters_free(&mut { par });
    }
    Some((enc, frame_samples))
}

impl Drop for AudioConverter {
    fn drop(&mut self) {
        unsafe {
            if !self.swr.is_null() {
                swr_free(&mut self.swr);
            }
            if !self.frame.is_null() {
                av_frame_free(&mut self.frame);
            }
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if let Some(mut out) = self.out.take() {
                avcodec_free_context(&mut out.enc);
            }
            if !self.dec.is_null() {
                avcodec_free_context(&mut self.dec);
            }
        }
    }
}
