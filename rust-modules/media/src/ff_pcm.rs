//! Audio decoded in the app and fed as PCM: the lossless FLAC, ALAC and integer LPCM, and DTS, MP3, Vorbis
//! and TrueHD, the lossy entries, converted to stereo 48 kHz S16 and only where the attempt decodes the track in
//! the app (`Caps::app_decodes_lossy_track`; `devcaps::APP_DECODED` is the set). One converter, [`PcmConverter`],
//! serves both: lossy it downmixes, resamples to 48 kHz and writes S16; lossless above the PCM input's table it
//! resamples to the table's top and writes S32.
//!
//! libpf has no FLAC or ALAC input — its Load vocabulary is AAC / AC3 / AC3 PLUS / DTS / MP3 / MPEG
//! / OPUS / VORBIS / WMA / PCM (read from `libpf-1.0` on a webOS 4.4.3 OLED55B8) — but it does take
//! raw PCM through `contents.pcmInfo` (`player::engine::with_pcm_info`). At a rate of the input's table
//! (8 to 48 kHz) the PCM decoded from these is bit-exact: at 16 bits as S16, and at 24 bits too, carried
//! left-justified in S32 exactly as FFmpeg's decoders produce it; a decode that would need narrowing is
//! refused, never truncated. A track above the table (88.2, 96, 176.4, 192 kHz and up) is resampled by the
//! app, by `speed_audio::Resampler`, to the table's top rate (44.1 kHz for a multiple of it, else 48 kHz) and
//! carried as S32 whatever its own depth, at unity gain, without dither or limiter; the clipped samples and
//! the peak are counted and logged once per stream.
//!
//! The Load is built before the demuxer opens the file, so it declares the shape from the PMS
//! track facts (`route::stream_pcm`), and [`PcmDecoder::open`] refuses a stream whose own rate or
//! channel count contradicts that declaration — a wrong rate is wrong pitch, not an error anyone
//! would see.

use std::os::raw::c_int;

use crate::ff::{
    AVCodecContext, AVCodecParameters, AVPacket, OFF_AFRAME_NB_CHANNELS, OFF_AFRAME_NB_SAMPLES,
    OFF_AFRAME_SAMPLE_RATE, OFF_FRAME_DATA, OFF_FRAME_FORMAT,
};
use crate::speed_audio::{self, Resampler, SampleFormat};

/// `av_opt_set`'s `search_flags`: look in the object's children (the decoder's private options,
/// where `core_only` lives) too. A value FFmpeg has not changed since the flag was added.
const AV_OPT_SEARCH_CHILDREN: c_int = 1;


/// FFmpeg `AVSampleFormat` values the lossless decoders produce (FLAC, ALAC, `pcm_s*`). The enum has been stable since
/// FFmpeg 1.x (`S16 = 1`, `S32 = 2`, `S16P = 6`, `S32P = 7`).
const AV_SAMPLE_FMT_S16: c_int = 1;
const AV_SAMPLE_FMT_S32: c_int = 2;
const AV_SAMPLE_FMT_S16P: c_int = 6;
const AV_SAMPLE_FMT_S32P: c_int = 7;

/// The PCM the Load declared: the rate the decoder emits, the sample width (16 or 32), the channels, and the
/// rate the track itself is encoded at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PcmOut {
    /// What the Load declared (a rate of `devcaps::PCM_SAMPLE_RATES`), and what the decoder emits.
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
    /// The rate of the file; [`PcmDecoder::open`] refuses a lossless one that is not this. Equal to `rate` at a table
    /// rate (the bit-exact path); otherwise the app resamples from it to `rate` ([`PcmOut::resamples`]).
    pub source_rate: u32,
}

impl PcmOut {
    /// A shape the decoder passes through bit-exact: the file's rate is the declared one.
    pub const fn exact(rate: u32, bits: u32, channels: u32) -> PcmOut {
        PcmOut { rate, bits, channels, source_rate: rate }
    }

    /// Whether a lossless decode of this shape is resampled (the file's rate is not the declared one). When it
    /// is not, no converter is built and the bytes are the decoder's own.
    pub const fn resamples(self) -> bool {
        self.source_rate != self.rate
    }
}

/// The shape the lossy decode outputs, whatever the source: stereo, 48 kHz, 16 bits
/// (`route::PcmFormat::STEREO_48K`). Its `source_rate` is nominal: the lossy decode accepts any source.
pub const STEREO_48K: PcmOut = PcmOut::exact(speed_audio::OUT_RATE, 16, 2);

pub struct PcmDecoder {
    ctx: *mut AVCodecContext,
    frame: *mut std::ffi::c_void,
    out: PcmOut,
    buf: Vec<u8>,
    /// Some when the bytes are not the decoder's own: a lossy codec (DTS, MP3, Vorbis, TrueHD), converted to
    /// [`STEREO_48K`], or a lossless one above the table, resampled to `out.rate` as S32. None is the bit-exact path.
    conv: Option<PcmConverter>,
    /// The codec is a lossy one (`conv` is then the lossy shape): its bad packets are skipped, a lossless one ends the stream.
    lossy: bool,
    /// `core_only` took on a lossy decoder (FFmpeg accepted the option). Reported in the log line
    /// and held by a host test; the decode works without it, only costlier.
    core_only: bool,
    errors: ErrorBudget,
    /// The pts of the packet being decoded until its first frame is converted (a later frame of the same
    /// packet follows the converter's own clock), and the stamp of the first block of it the converter produced.
    packet_pts: Option<i64>,
    stamp: Option<i64>,
}

/// Which conversion a [`PcmConverter`] does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// Any layout and rate: downmixed to stereo ([`speed_audio::downmix_by_mask`]), resampled to 48 kHz, written as S16.
    Lossy,
    /// The decoder's integer frame (S16 or S32, `channels` of 1 or 2, no downmix) read exactly, resampled to the
    /// declared rate, written as S32.
    Lossless { channels: usize },
}

/// What a converter has done to a stream so far.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ConvertReport {
    /// Frames written.
    pub frames_out: u64,
    /// Samples whose rounded value lay outside the output width and were saturated.
    pub clipped: u64,
    /// The largest absolute sample value before rounding, as a fraction of full scale (above 1.0 when something clipped).
    pub peak: f64,
}

/// Where the converter's output sits in content time: the time of the first input frame it was fed since the
/// anchor, and the frames fed and written since. The stamp of a block is `anchor + frames written` at the output
/// rate, so it never drifts, whatever the packets' own (millisecond-rounded) stamps do.
#[derive(Clone, Copy)]
struct Clock {
    anchor: i64,
    frames_in: u64,
    frames_out: u64,
}

impl Clock {
    /// Content time of `frames` frames at `rate` after the anchor (exact: i128).
    fn after(&self, frames: u64, rate: u32) -> i64 {
        self.anchor + (i128::from(frames) * 1_000_000_000 / i128::from(rate.max(1))) as i64
    }
}

/// PURE (no FFmpeg): the app's one audio conversion, [`Shape::Lossy`] (any layout and rate to interleaved stereo S16 LE
/// at 48 kHz: the downmix is [`speed_audio::downmix_by_mask`]) or [`Shape::Lossless`] (a 1 or 2 channel integer
/// stream above the PCM input's table to the table's top rate as S32 LE: the frame is read to f64 exactly,
/// `i16 / 2^15`, `i32 / 2^31`, mono through the stereo kernel with one channel kept, and written as
/// `round(v * 2^31)` saturated to `i32`, at unity gain, no dither, no limiter). The resample is the
/// [`Resampler`] (linear up to the output rate, a Kaiser windowed-sinc low-pass decimator from above it; the bundled
/// FFmpeg is built without swresample), kept across frames so their edges are seamless.
///
/// The converter owns the output's content time ([`Clock`]): it is anchored at the first block's pts and stamps each
/// block it returns with `anchor + frames written / rate`. A block whose pts is more than [`AU_GAP_TOLERANCE_NS`] from where
/// the frames fed so far put it (a skipped packet, a discontinuity) re-anchors the clock and starts the filter
/// again, and so does a change of the frame's own rate. A seek ([`PcmConverter::reset`]) does both.
pub struct PcmConverter {
    shape: Shape,
    out_rate: u32,
    /// The rate of the frames fed so far.
    in_rate: u32,
    resampler: Resampler,
    clock: Option<Clock>,
    /// Scratch, kept to spare an allocation a block.
    input: Vec<[f64; 2]>,
    output: Vec<[f64; 2]>,
    report: ConvertReport,
}

impl PcmConverter {
    fn with(shape: Shape, in_rate: u32, out_rate: u32) -> Self {
        PcmConverter {
            shape,
            out_rate,
            in_rate,
            resampler: Resampler::new(in_rate, out_rate),
            clock: None,
            input: Vec::new(),
            output: Vec::new(),
            report: ConvertReport::default(),
        }
    }

    /// The lossy shape: any source to stereo 48 kHz S16.
    pub fn lossy() -> Self {
        Self::with(Shape::Lossy, speed_audio::OUT_RATE, speed_audio::OUT_RATE)
    }

    /// The lossless shape for a declared `out` that [`resamples`](PcmOut::resamples): `out.source_rate` to `out.rate`,
    /// `out.channels` (1 or 2) kept, S32. (`bits` is 32 by `route::pcm_format_of`'s rule; [`PcmDecoder::open`] refuses
    /// any other.)
    pub fn resampled(out: PcmOut) -> Self {
        Self::with(Shape::Lossless { channels: out.channels.clamp(1, 2) as usize }, out.source_rate, out.rate)
    }

    /// The rate of the frames fed last, and the rate written.
    pub fn rates(&self) -> (u32, u32) {
        (self.in_rate, self.out_rate)
    }

    /// What has been done to the stream so far (not cleared by a seek: it is the stream's).
    pub fn report(&self) -> ConvertReport {
        self.report
    }

    /// A seek: a fresh filter, no carried tail, and no clock; the next block anchors it at its own pts.
    pub fn reset(&mut self) {
        self.resampler = Resampler::new(self.in_rate, self.out_rate);
        self.clock = None;
    }

    /// Settle the clock for a block of rate `rate` stamped `pts` (None: it follows another block of the same packet, so it
    /// is where the clock says): a new rate or a gap starts the filter and the clock again, and an unanchored clock anchors.
    fn sync(&mut self, pts: Option<i64>, rate: u32) {
        let gap = match (self.clock, pts) {
            (Some(c), Some(p)) => (p - c.after(c.frames_in, self.in_rate)).abs() > AU_GAP_TOLERANCE_NS,
            _ => false,
        };
        if rate != self.in_rate || gap {
            self.in_rate = rate;
            self.resampler = Resampler::new(rate, self.out_rate);
            self.clock = None;
        }
        if self.clock.is_none() {
            self.clock = Some(Clock { anchor: pts.unwrap_or(0), frames_in: 0, frames_out: 0 });
        }
    }

    /// Convert `frames` frames (`planes`: one slice per channel when `planar`, else one) and append the result to `out`.
    /// `pts` is the content time of the first of them (None when the packet's time was spent on an earlier frame of it);
    /// `mask` the layout's native channel mask (0 = not stated, mixed by count; the lossless shape does not mix).
    /// The lossless shape takes `fmt` of S16 or S32 only, and `channels` frames of its own (what it was built for).
    /// Returns the content time of the block appended, None when nothing was (the filter holds back its lookahead).
    #[allow(clippy::too_many_arguments)]
    pub fn convert(
        &mut self, pts: Option<i64>, rate: u32, fmt: SampleFormat, planar: bool, channels: usize, mask: u64,
        frames: usize, planes: &[&[u8]], out: &mut Vec<u8>,
    ) -> Option<i64> {
        if rate == 0 || frames == 0 {
            return None;
        }
        self.input.clear();
        match self.shape {
            Shape::Lossy => {
                let stereo = speed_audio::downmix_by_mask(fmt, planar, mask, channels, frames, planes);
                self.input.extend(stereo.iter().map(|f| [f64::from(f[0]), f64::from(f[1])]));
            }
            Shape::Lossless { channels: kept } => {
                let (width, scale) = match fmt {
                    SampleFormat::S16 => (2, 1.0 / 32_768.0),
                    SampleFormat::S32 => (4, 1.0 / 2_147_483_648.0),
                    _ => return None,
                };
                let sample = |c: usize, i: usize| -> f64 {
                    let (plane, idx) = if planar { (planes[c], i) } else { (planes[0], i * kept + c) };
                    let b = &plane[idx * width..(idx + 1) * width];
                    let v = if width == 2 { i32::from(i16::from_ne_bytes([b[0], b[1]])) } else { i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) };
                    f64::from(v) * scale
                };
                self.input.extend((0..frames).map(|i| {
                    let l = sample(0, i);
                    [l, if kept == 2 { sample(1, i) } else { l }]
                }));
            }
        }
        self.sync(pts, rate);
        self.output.clear();
        self.resampler.run(&self.input, &mut self.output);
        let clock = self.clock.as_mut()?;
        clock.frames_in += frames as u64;
        if self.output.is_empty() {
            return None;
        }
        let stamp = clock.after(clock.frames_out, self.out_rate);
        clock.frames_out += self.output.len() as u64;
        self.report.frames_out += self.output.len() as u64;
        self.write(out);
        Some(stamp)
    }

    /// Append `self.output` to `out` in the shape's width, counting what saturates.
    fn write(&mut self, out: &mut Vec<u8>) {
        let (wide, kept) = match self.shape {
            Shape::Lossy => (false, 2),
            Shape::Lossless { channels } => (true, channels),
        };
        out.reserve(self.output.len() * kept * if wide { 4 } else { 2 });
        for frame in &self.output {
            for &v in &frame[..kept] {
                self.report.peak = self.report.peak.max(v.abs());
                if wide {
                    let s = (v * 2_147_483_648.0).round();
                    self.report.clipped += u64::from(s > 2_147_483_647.0 || s < -2_147_483_648.0);
                    out.extend_from_slice(&(s.clamp(-2_147_483_648.0, 2_147_483_647.0) as i32).to_le_bytes());
                } else {
                    let s = (v * 32_768.0).round();
                    self.report.clipped += u64::from(s > 32_767.0 || s < -32_768.0);
                    out.extend_from_slice(&(s.clamp(-32_768.0, 32_767.0) as i16).to_le_bytes());
                }
            }
        }
    }
}

// Owned by the one demux thread that opened it.
unsafe impl Send for PcmDecoder {}

impl PcmDecoder {
    /// Open the FFmpeg decoder for `par`'s codec, refusing a stream that is not the declared shape
    /// or whose codec is not one of the app-decoded set.
    pub unsafe fn open(par: *const AVCodecParameters, out: PcmOut) -> Result<PcmDecoder, String> {
        let name = std::ffi::CStr::from_ptr(crate::ff::avcodec_get_name((*par).codec_id)).to_string_lossy();
        let Some(entry) = plx_platform::devcaps::app_decoded_by_ffmpeg_name(&name) else {
            return Err(format!("{name} is not an app-decoded codec"));
        };
        // A lossless stream must be the declared shape (a wrong rate is wrong pitch); a lossy one
        // is converted to its one output shape whatever the source is.
        if entry.lossy {
            if out != STEREO_48K {
                return Err(format!("{name} is converted to stereo 48 kHz but the Load declared {out:?}"));
            }
        } else {
            let rate = (*par).sample_rate;
            let channels = (*par).ch_layout.nb_channels;
            if rate as u32 != out.source_rate || channels as u32 != out.channels {
                return Err(format!(
                    "the file is {rate}Hz {channels}ch but the Load declared {}Hz {}ch",
                    out.source_rate, out.channels
                ));
            }
            if out.resamples() && (out.bits != 32 || !(1..=2).contains(&out.channels)) {
                return Err(format!(
                    "the file is {rate}Hz but the Load declared {}Hz {}-bit {}ch: only mono or stereo S32 is resampled",
                    out.rate, out.bits, out.channels
                ));
            }
        }
        let codec = crate::ff::avcodec_find_decoder((*par).codec_id);
        if codec.is_null() {
            return Err(format!("this FFmpeg has no {name} decoder (ci/build-ffmpeg.sh)"));
        }
        let mut ctx = crate::ff::avcodec_alloc_context3(codec);
        if ctx.is_null() {
            return Err("avcodec_alloc_context3 failed".into());
        }
        let r = crate::ff::avcodec_parameters_to_context(ctx, par);
        let mut core_only = false;
        if r >= 0 && entry.lossy {
            // DTS: the 5.1 core only. The extensions (XLL, XBR, X96) are costly and the output is
            // stereo anyway; best-effort, so an FFmpeg without the option still decodes the core.
            // MP3, Vorbis and TrueHD have no such option: `av_opt_set` fails, `core_only` stays false.
            core_only = crate::ff::av_opt_set(
                ctx as *mut std::ffi::c_void, c"core_only".as_ptr(), c"1".as_ptr(), AV_OPT_SEARCH_CHILDREN,
            ) >= 0;
        }
        let r = if r < 0 { r } else { crate::ff::avcodec_open2(ctx, codec, std::ptr::null_mut()) };
        if r < 0 {
            crate::ff::avcodec_free_context(&mut ctx);
            return Err(format!("could not open the {name} decoder ({r})"));
        }
        let frame = crate::ff::av_frame_alloc();
        if frame.is_null() {
            crate::ff::avcodec_free_context(&mut ctx);
            return Err("av_frame_alloc failed".into());
        }
        let conv = if entry.lossy {
            Some(PcmConverter::lossy())
        } else {
            out.resamples().then(|| PcmConverter::resampled(out))
        };
        Ok(PcmDecoder {
            ctx, frame, out, buf: Vec::new(), conv, lossy: entry.lossy, core_only, errors: ErrorBudget::default(),
            packet_pts: None, stamp: None,
        })
    }

    /// The batcher for this decoder's output: blocks shorter than [`MIN_AU_MS`] are gathered into one AU.
    pub fn au_batcher(&self) -> AuBatcher {
        AuBatcher::new(self.out)
    }

    /// Whether the decoder is a lossy one running its core only (see the field).
    pub fn core_only(&self) -> bool {
        self.core_only
    }

    /// Decode one packet (stamped `pts`, content time in ns) into interleaved little-endian PCM of the declared
    /// width, and return the content time of the first of the bytes with them. Bit-exact (the rate is a table
    /// rate), the lossless codecs have no decoder delay: one packet is one frame, so the packet's pts is the PCM's
    /// and is what comes back. Through a converter ([`PcmConverter`]: a lossy codec, or a lossless one resampled)
    /// the stamp is the converter's own, `anchor + frames written / rate`, so it counts the frames the filter
    /// held back: the decimator's lookahead lateness (about a millisecond) is corrected by that stamp. A DTS, MP3
    /// or TrueHD frame is the same. Vorbis' decoder delay is NOT: the first packet yields nothing and the rest
    /// come out about half a block, tens of milliseconds, after their packet's pts, until a 20 ms drift re-anchors
    /// the clock; whether that matters for sync is not determined. The pts is content time and stays so: the set renders a PCM Load
    /// through a mixer channel that lacks the lip sync its own decoder gets (`player::engine::pcm_output_lead` is the
    /// table: on LG's `liblxa` stack 400 ms, less the output's own delay on a wireless one, the one
    /// stack that was filmed; 2018's `audiosink_new` the same figures and `lgaudiosink` 400 ms on every
    /// output, both read from the firmware; none on 2016/2017's `adecsink`, Realtek and MStar), and
    /// the correction is NOT made here but at the audio lane's one Feed (`player::engine`,
    /// `PCM_SINK_LEAD_NS`), so every producer of PCM gets it once and none stamps its own. The
    /// app-decoded codecs here are not filmed.
    ///
    /// A LOSSY stream (DTS, MP3, Vorbis, TrueHD) skips a packet or frame that fails, as the speed decoder does, and
    /// ends only after [`MAX_CONSECUTIVE_DECODE_ERRORS`] in a row; a lossless one, bit-exact or resampled, is whole
    /// or nothing, so its first failure ends it.
    pub unsafe fn decode(&mut self, pkt: *const AVPacket, pts: i64) -> Result<(i64, &[u8]), String> {
        self.buf.clear();
        self.packet_pts = Some(pts);
        self.stamp = None;
        match self.decode_packet(pkt) {
            Ok(()) => self.errors.ok(),
            Err(why) if self.lossy && self.errors.skip() => {
                crate::player::log(&format!("ff: a packet was skipped — {why}"));
            }
            Err(why) => return Err(why),
        }
        Ok((self.stamp.unwrap_or(pts), &self.buf))
    }

    unsafe fn decode_packet(&mut self, pkt: *const AVPacket) -> Result<(), String> {
        let r = crate::ff::avcodec_send_packet(self.ctx, pkt);
        if r < 0 {
            return Err(format!("avcodec_send_packet failed ({r})"));
        }
        loop {
            let r = crate::ff::avcodec_receive_frame(self.ctx, self.frame);
            if r == crate::ff::AVERROR_EAGAIN || r == crate::ff::AVERROR_EOF {
                return Ok(());
            }
            if r < 0 {
                return Err(format!("avcodec_receive_frame failed ({r})"));
            }
            self.append_frame()?;
        }
    }

    /// The demuxer sought: what the decode carried from the old position is dropped, so the first
    /// sound at the new one is not blended with the last of the old. The decoder is flushed
    /// (`avcodec_flush_buffers`: a DTS synthesis filter bank or an MP3 bit reservoir carries state from frame
    /// to frame, which is a click at every seek), the frame buffer emptied, the error count started again and a converter's
    /// filter tail and clock forgotten ([`PcmConverter::reset`]).
    pub fn reset_stream(&mut self) {
        unsafe { crate::ff::avcodec_flush_buffers(self.ctx) };
        self.buf.clear();
        self.errors = ErrorBudget::default();
        if let Some(conv) = self.conv.as_mut() {
            conv.reset();
        }
    }
}

/// How many bad packets in a row a lossy stream is allowed before it is declared dead.
const MAX_CONSECUTIVE_DECODE_ERRORS: u32 = 50;

/// PURE: the count of consecutive failures a lossy stream has had.
#[derive(Default)]
struct ErrorBudget(u32);

impl ErrorBudget {
    /// A failure: whether it may be skipped (the budget is not spent).
    fn skip(&mut self) -> bool {
        self.0 += 1;
        self.0 <= MAX_CONSECUTIVE_DECODE_ERRORS
    }

    fn ok(&mut self) {
        self.0 = 0;
    }
}

impl PcmDecoder {
    /// The ONE place a decoded frame becomes the declared PCM, appended to `buf`. (Anything that
    /// ever converts the decoder's output — downmix, resample — belongs here and nowhere else.)
    unsafe fn append_frame(&mut self) -> Result<(), String> {
        let base = self.frame as *const u8;
        let nb = *(base.add(OFF_AFRAME_NB_SAMPLES) as *const c_int) as usize;
        let format = *(base.add(OFF_FRAME_FORMAT) as *const c_int);
        let data = base.add(OFF_FRAME_DATA) as *const *const u8;
        if let Some(conv) = self.conv.as_mut() {
            let rate = *(base.add(OFF_AFRAME_SAMPLE_RATE) as *const c_int);
            if rate <= 0 {
                return Err(format!("decoder reported sample rate {rate}"));
            }
            let Some((fmt, planar)) = SampleFormat::from_av(format) else {
                return Err(format!("unexpected decoder sample format {format}"));
            };
            let (channels, mask) = if self.lossy {
                let channels = (*(base.add(OFF_AFRAME_NB_CHANNELS) as *const c_int)).max(0) as usize;
                // `data[]` holds eight planes: a DTS core has at most six channels, TrueHD's 7.1 bed eight.
                if channels == 0 || channels > 8 {
                    return Err(format!("unexpected decoder channel count {channels}"));
                }
                (channels, crate::ff::frame_channel_mask(base))
            } else {
                // resampled lossless: S16 or S32 only, the channels the Load declared (`open` checked the file's)
                if sample_width(format).is_none() {
                    return Err(format!("unexpected decoder sample format {format}"));
                }
                (self.out.channels as usize, 0)
            };
            let width = fmt.bytes();
            let planes: Vec<&[u8]> = if planar {
                (0..channels).map(|c| std::slice::from_raw_parts(*data.add(c), nb * width)).collect()
            } else {
                vec![std::slice::from_raw_parts(*data, nb * width * channels)]
            };
            let stamp = conv.convert(self.packet_pts.take(), rate as u32, fmt, planar, channels, mask, nb, &planes, &mut self.buf);
            self.stamp = self.stamp.or(stamp);
            return Ok(());
        }
        let ch = self.out.channels as usize;
        let Some(width) = sample_width(format) else {
            return Err(format!("unexpected decoder sample format {format}"));
        };
        let out_width = self.out.bits as usize / 8;
        if !width_is_exact(width, out_width) {
            return Err(format!(
                "the file decodes to {}-bit samples but the Load declared {} bits",
                width * 8, self.out.bits
            ));
        }
        let planar = matches!(format, AV_SAMPLE_FMT_S16P | AV_SAMPLE_FMT_S32P);
        let planes: Vec<&[u8]> = if planar {
            (0..ch).map(|c| std::slice::from_raw_parts(*data.add(c), nb * width)).collect()
        } else {
            vec![std::slice::from_raw_parts(*data, nb * width * ch)]
        };
        interleave_into(&mut self.buf, &planes, planar, width, nb, ch, out_width);
        Ok(())
    }
}

impl Drop for PcmDecoder {
    fn drop(&mut self) {
        // one line a stream with a converter: what it did, and what it clipped
        if let Some(conv) = &self.conv {
            let (report, (from, to)) = (conv.report(), conv.rates());
            if report.frames_out > 0 {
                crate::player::log(&format!(
                    "ff: PCM resampler {from}->{to}Hz: {} frames, {} clipped, peak {:.4}",
                    report.frames_out, report.clipped, report.peak
                ));
            }
        }
        unsafe {
            crate::ff::av_frame_free(&mut self.frame);
            crate::ff::avcodec_free_context(&mut self.ctx);
        }
    }
}

/// PURE: feeding `in_width`-byte samples as `out_width`-byte ones loses nothing (equal, or widening).
fn width_is_exact(in_width: usize, out_width: usize) -> bool {
    out_width >= in_width
}

fn sample_width(format: c_int) -> Option<usize> {
    match format {
        AV_SAMPLE_FMT_S16 | AV_SAMPLE_FMT_S16P => Some(2),
        AV_SAMPLE_FMT_S32 | AV_SAMPLE_FMT_S32P => Some(4),
        _ => None,
    }
}

/// PURE: append `nb` frames of `ch` channels to `out` as interleaved little-endian samples of
/// `out_width` bytes. The decoder's samples are native-endian (little on ARM and on every host
/// this builds on) `in_width`-byte integers, packed in one plane or one plane per channel.
/// Widening 16 → 32 shifts left (the value S32 means); narrowing never happens on a declared
/// path — `route::pcm_format_of` declares 32 for anything that is not known to be ≤ 16 bits.
pub fn interleave_into(
    out: &mut Vec<u8>, planes: &[&[u8]], planar: bool, in_width: usize, nb: usize, ch: usize,
    out_width: usize,
) {
    out.reserve(nb * ch * out_width);
    for i in 0..nb {
        for c in 0..ch {
            let (plane, idx) = if planar { (planes[c], i) } else { (planes[0], i * ch + c) };
            let s = &plane[idx * in_width..(idx + 1) * in_width];
            let v: i32 = if in_width == 2 {
                i32::from(i16::from_ne_bytes([s[0], s[1]])) << 16
            } else {
                i32::from_ne_bytes([s[0], s[1], s[2], s[3]])
            };
            if out_width == 2 {
                out.extend_from_slice(&((v >> 16) as i16).to_le_bytes());
            } else {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// The shortest AU the audio lane is fed, in milliseconds: blocks decoded shorter than this are gathered
/// into one AU ([`AuBatcher`]). TrueHD's access units are 1/1200 s (40 frames at 48 kHz), 1200 queue
/// entries a second where the lane is used to tens (the speed path's AUs are `speed_audio::CHUNK` frames,
/// 21 ms). A DTS (10.7 ms), MP3 (26 ms) or lossless block is already longer, and while the stream is
/// contiguous passes through whole, unchanged; a short block (a Vorbis one of a few milliseconds, a
/// small LPCM packet) is gathered like TrueHD's, and so is the block after a gap.
const MIN_AU_MS: u32 = 10;

/// How far a block's stamp may sit from where the length of what is gathered puts it before the content
/// time counts as having a gap. Matroska stamps in milliseconds, so the blocks of a stream shorter than
/// a millisecond jitter by about one around their true time.
const AU_GAP_TOLERANCE_NS: i64 = 20_000_000;

/// PURE: gathers decoded PCM blocks into AUs of at least [`MIN_AU_MS`], stamped with the content time
/// of the first block in them. A block that is long enough is its own AU, and a gap in the content time
/// (a skipped packet) ends the AU being gathered, so one stamp never spans it.
pub struct AuBatcher {
    rate: u32,
    frame_bytes: usize,
    min_bytes: usize,
    /// What is being gathered and the content time of its first block.
    buf: Vec<u8>,
    pts: i64,
    /// The AU last handed out (the borrow `push` and `take` return).
    ready: Vec<u8>,
    ready_pts: i64,
}

impl AuBatcher {
    pub fn new(out: PcmOut) -> AuBatcher {
        let frame_bytes = (out.bits / 8) as usize * out.channels as usize;
        AuBatcher {
            rate: out.rate,
            frame_bytes,
            min_bytes: (out.rate * MIN_AU_MS / 1000) as usize * frame_bytes,
            buf: Vec::new(),
            pts: 0,
            ready: Vec::new(),
            ready_pts: 0,
        }
    }

    /// Hand `buf` out as the AU.
    fn release(&mut self) -> Option<(i64, &[u8])> {
        std::mem::swap(&mut self.buf, &mut self.ready);
        self.ready_pts = self.pts;
        self.buf.clear();
        Some((self.ready_pts, &self.ready))
    }

    /// Add one decoded block (content time `pts`); `Some` when an AU is complete. At a gap the part
    /// gathered so far is the AU (short) and the new block starts the next one.
    pub fn push(&mut self, pts: i64, pcm: &[u8]) -> Option<(i64, &[u8])> {
        if pcm.is_empty() {
            return None;
        }
        let gathered_ns = (self.buf.len() / self.frame_bytes.max(1)) as i64 * 1_000_000_000 / self.rate.max(1) as i64;
        let gap = !self.buf.is_empty() && (pts - (self.pts + gathered_ns)).abs() > AU_GAP_TOLERANCE_NS;
        if gap {
            std::mem::swap(&mut self.buf, &mut self.ready);
            self.ready_pts = self.pts;
            self.buf.clear();
            self.pts = pts;
            self.buf.extend_from_slice(pcm);
            return Some((self.ready_pts, &self.ready));
        }
        if self.buf.is_empty() {
            self.pts = pts;
        }
        self.buf.extend_from_slice(pcm);
        if self.buf.len() < self.min_bytes {
            return None;
        }
        self.release()
    }

    /// What is gathered but not yet an AU: the end of the stream.
    pub fn take(&mut self) -> Option<(i64, &[u8])> {
        if self.buf.is_empty() {
            return None;
        }
        self.release()
    }

    /// Drop what is gathered: the demuxer sought.
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s16(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_ne_bytes()).collect()
    }
    fn s32(v: &[i32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_ne_bytes()).collect()
    }

    #[test]
    fn packed_s16_passes_through_bit_exact() {
        let src = s16(&[1, -2, 32767, -32768]);
        let mut out = Vec::new();
        interleave_into(&mut out, &[&src], false, 2, 2, 2, 2);
        let le: Vec<u8> = [1i16, -2, 32767, -32768].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(out, le);
    }

    #[test]
    fn planar_is_interleaved_frame_by_frame() {
        let (l, r) = (s16(&[10, 11, 12]), s16(&[-10, -11, -12]));
        let mut out = Vec::new();
        interleave_into(&mut out, &[&l, &r], true, 2, 3, 2, 2);
        let want: Vec<u8> = [10i16, -10, 11, -11, 12, -12].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(out, want);
    }

    #[test]
    fn a_24_bit_source_in_s32_keeps_every_bit() {
        // FFmpeg left-justifies 24-bit FLAC in S32: the low byte is zero, the value is exact.
        let samples = [0x7fff_ff00_u32 as i32, 0x8000_0100_u32 as i32, 0x0012_3400];
        let src = s32(&samples);
        let mut out = Vec::new();
        interleave_into(&mut out, &[&src], false, 4, 3, 1, 4);
        let want: Vec<u8> = samples.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(out, want);
    }

    #[test]
    fn widening_16_to_32_is_the_same_value() {
        let src = s16(&[-1, 1]);
        let mut out = Vec::new();
        interleave_into(&mut out, &[&src], false, 2, 1, 2, 4);
        let want: Vec<u8> = [-1i32 << 16, 1 << 16].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(out, want);
    }

    /// The Load declared the width from the PMS facts; a file that decodes wider than that would be
    /// truncated by [`interleave_into`], which is not bit-exact. It is refused instead.
    #[test]
    fn a_decode_wider_than_the_declared_width_is_refused_not_truncated() {
        assert!(width_is_exact(2, 2) && width_is_exact(2, 4) && width_is_exact(4, 4));
        assert!(!width_is_exact(4, 2), "24-bit audio declared as 16");
    }

    /// **Tiny blocks become AUs of about 10 ms**: TrueHD's 40-frame access units are gathered, each AU
    /// carries the pts of its first block, a block already long enough passes through whole under its
    /// own pts, the tail comes out at the end of the stream and a seek drops it.
    #[test]
    fn small_blocks_are_gathered_into_ten_millisecond_aus_and_big_ones_pass_whole() {
        let mut b = AuBatcher::new(STEREO_48K);
        let block = vec![7u8; 40 * 4]; // 40 frames: 0.83 ms
        let mut aus: Vec<(i64, usize)> = Vec::new();
        for i in 0..30 {
            if let Some((pts, au)) = b.push(i * 833_333, &block) {
                assert!(au.iter().all(|&v| v == 7));
                aus.push((pts, au.len()));
            }
        }
        assert_eq!(aus, [(0, 480 * 4), (12 * 833_333, 480 * 4)], "480 frames each, stamped by their first block");
        assert_eq!(b.take().map(|(p, a)| (p, a.len())), Some((24 * 833_333, 6 * 40 * 4)), "the tail at the end of the stream");
        assert!(b.take().is_none(), "and only once");
        let big = vec![1u8; 512 * 4]; // a DTS frame: 10.7 ms
        assert_eq!(b.push(5_000, &big).map(|(p, a)| (p, a.len())), Some((5_000, 512 * 4)), "passes whole");
        assert!(b.push(9_000, &[]).is_none(), "an empty block is nothing");
        b.push(10_000, &block);
        b.clear();
        assert!(b.take().is_none(), "a seek drops what was gathered");
        assert_eq!(b.push(20_000, &block).map(|a| a.0), None);
        let mut lossless = AuBatcher::new(PcmOut::exact(96_000, 32, 2));
        assert!(lossless.push(0, &vec![0u8; 960 * 8]).is_some(), "10 ms of 96 kHz 32-bit stereo is its own AU");
        assert!(lossless.push(1, &vec![0u8; 100 * 8]).is_none());
    }

    /// **A gap in the content time splits the AU.** Matroska stamps in milliseconds, so the 0.83 ms
    /// blocks of a TrueHD stream jitter by up to a millisecond around where their length puts them and
    /// are still one run; a skipped packet or a jump of tens of milliseconds is not, and one stamp must
    /// not span it (the audio after it would play early). The gathered part goes out first, short, and
    /// the block that revealed the gap starts the next AU.
    #[test]
    fn a_gap_in_the_content_time_ends_the_au_but_millisecond_jitter_does_not() {
        let block = vec![3u8; 40 * 4]; // 40 frames = 833_333 ns
        let mut b = AuBatcher::new(STEREO_48K);
        let jitter = [0i64, 1_000_000, -900_000, 700_000, -600_000, 900_000];
        for (i, j) in jitter.iter().enumerate() {
            assert!(b.push(i as i64 * 833_333 + j, &block).is_none(), "block {i}: jitter is not a gap");
        }
        // a 30 ms hole after six blocks
        let after = 6 * 833_333 + 30_000_000;
        let (pts, len) = b.push(after, &block).map(|(p, a)| (p, a.len())).expect("the gap ends the AU");
        assert_eq!((pts, len), (jitter[0], 6 * 40 * 4), "the six blocks before it, stamped by the first");
        let (pts, len) = b.take().map(|(p, a)| (p, a.len())).expect("the block after the gap is pending");
        assert_eq!((pts, len), (after, 40 * 4), "and stamped by its own time");
        // running time moves backwards (a duplicate packet): also a gap
        b.push(1_000_000_000, &block);
        assert!(b.push(500_000_000, &block).is_some());
    }

    fn flt(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_ne_bytes()).collect()
    }
    fn s16_le(out: &[u8]) -> Vec<i16> {
        out.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
    }

    /// **The lossy shape's conversion**: any layout in, interleaved stereo S16 LE at 48 kHz out.
    /// A planar 5.1 of silence plus a full-scale front-left becomes a stereo frame whose left is
    /// the scaled front-left and whose right is silent, so the downmix really ran.
    #[test]
    fn a_planar_51_is_downmixed_to_interleaved_stereo_s16() {
        let mut conv = PcmConverter::lossy();
        let one = flt(&[1.0, 1.0]);
        let zero = flt(&[0.0, 0.0]);
        let planes: Vec<&[u8]> = vec![&one, &zero, &zero, &zero, &zero, &zero];
        let mut out = Vec::new();
        conv.convert(None, 48_000, crate::speed_audio::SampleFormat::Flt, true, 6, 0, 2, &planes, &mut out);
        let v = s16_le(&out);
        assert_eq!(v.len(), 4, "two stereo frames");
        assert!(v[0] > 8_000 && v[0] < 32_767, "front-left scaled so a full-scale 5.1 cannot clip: {}", v[0]);
        assert_eq!((v[1], v[3]), (0, 0), "nothing on the right");
        assert_eq!(v[0], v[2]);
    }

    /// The layout reaches the mix: a quad (FL FR SL SR) with sound only on its left surround comes
    /// out on the left, where mixing by count (a 4-channel layout has no surrounds) would drop it.
    #[test]
    fn the_frames_own_channel_layout_decides_where_a_surround_goes() {
        let mut conv = PcmConverter::lossy();
        let sl_only = [flt(&[0.0]), flt(&[0.0]), flt(&[0.0]), flt(&[0.0])];
        let (fl, fr, sl, sr) = (sl_only[0].clone(), sl_only[1].clone(), flt(&[0.5]), sl_only[3].clone());
        let planes: Vec<&[u8]> = vec![&fl, &fr, &sl, &sr];
        let quad = (1u64 << 0) | (1 << 1) | (1 << 9) | (1 << 10);
        let mut out = Vec::new();
        conv.convert(None, 48_000, crate::speed_audio::SampleFormat::Flt, true, 4, quad, 1, &planes, &mut out);
        let v = s16_le(&out);
        assert!(v[0] > 1_000 && v[1] == 0, "the left surround is on the left only: {v:?}");
    }

    #[test]
    fn a_44100_source_comes_out_at_48000_in_the_same_duration() {
        let mut conv = PcmConverter::lossy();
        let block = flt(&vec![0.25; 4410]);
        let mut out = Vec::new();
        for _ in 0..10 {
            conv.convert(None, 44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 4410, &[&block], &mut out);
        }
        let frames = out.len() / 4;
        assert!((47_990..=48_010).contains(&frames), "1 s of 44.1 kHz is ~48000 frames, got {frames}");
    }

    #[test]
    fn a_rate_change_mid_stream_is_followed_not_played_at_the_old_pitch() {
        let mut conv = PcmConverter::lossy();
        let mut out = Vec::new();
        let a = flt(&vec![0.1; 480]);
        conv.convert(None, 48_000, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 480, &[&a], &mut out);
        assert_eq!(out.len() / 4, 480);
        let b = flt(&vec![0.1; 441]);
        let before = out.len();
        conv.convert(None, 44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 441, &[&b], &mut out);
        let got = (out.len() - before) / 4;
        assert!((470..=482).contains(&got), "10 ms of 44.1 kHz is ~480 frames, got {got}");
    }

    #[test]
    fn samples_past_full_scale_clamp_instead_of_wrapping() {
        let mut conv = PcmConverter::lossy();
        let src = flt(&[2.0, -2.0, 1.0, -1.0]);
        let mut out = Vec::new();
        conv.convert(None, 48_000, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 4, &[&src], &mut out);
        assert_eq!(s16_le(&out), vec![32_767, 32_767, -32_768, -32_768, 32_767, 32_767, -32_768, -32_768]);
    }

    /// A lossy stream survives the odd bad packet (the speed decoder skips them too) but not a
    /// dead one: a run of consecutive failures ends it, and any success starts the count again.
    #[test]
    fn a_lossy_stream_skips_isolated_bad_packets_and_gives_up_on_a_run() {
        let mut b = ErrorBudget::default();
        for _ in 0..MAX_CONSECUTIVE_DECODE_ERRORS {
            assert!(b.skip(), "within the budget");
        }
        assert!(!b.skip(), "one past it: the stream is dead");
        let mut b = ErrorBudget::default();
        for _ in 0..10 * MAX_CONSECUTIVE_DECODE_ERRORS {
            assert!(b.skip());
            b.ok();
        }
    }

    #[test]
    fn a_reset_forgets_the_resampler_tail_so_a_seek_does_not_blend_positions() {
        let mut conv = PcmConverter::lossy();
        let mut out = Vec::new();
        let a = flt(&vec![1.0; 441]);
        conv.convert(None, 44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 441, &[&a], &mut out);
        conv.reset();
        out.clear();
        let b = flt(&vec![-1.0; 441]);
        conv.convert(None, 44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 441, &[&b], &mut out);
        let first = i16::from_le_bytes([out[0], out[1]]);
        assert_eq!(first, -32_768, "the first frame after a seek is the new position's, not a blend with the old tail");
    }

    // ---- the converter: stamps, seeks, saturation, the lossless-resampled shape ----

    fn s32_le(out: &[u8]) -> Vec<i32> {
        out.chunks(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
    }

    /// `frames` interleaved stereo S32 frames of repeatable noise, up to half scale.
    fn noise(frames: usize, seed: u32) -> Vec<i32> {
        let mut x = seed;
        (0..frames * 2).map(|_| { x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (x as i32) / 2 }).collect()
    }

    /// Content time of `frames` frames at `rate` Hz after `base`: the exact (i128) value the clock must give.
    fn after(base: i64, frames: u64, rate: u32) -> i64 {
        base + (i128::from(frames) * 1_000_000_000 / i128::from(rate)) as i64
    }

    /// Feed interleaved stereo S32 `input` to `conv` in blocks of `block` frames, each stamped where its position in
    /// the stream puts it (from `base`, at `rate`); the (stamp, bytes) of every block that wrote.
    fn run_blocks(conv: &mut PcmConverter, input: &[i32], rate: u32, block: usize, base: i64) -> Vec<(i64, Vec<u8>)> {
        let mut got = Vec::new();
        for (i, b) in input.chunks(block * 2).enumerate() {
            let bytes = s32(b);
            let mut out = Vec::new();
            let pts = after(base, (i * block) as u64, rate);
            if let Some(stamp) = conv.convert(Some(pts), rate, SampleFormat::S32, false, 2, 0, b.len() / 2, &[&bytes], &mut out) {
                got.push((stamp, out));
            }
        }
        got
    }

    /// The stamps of `blocks` are `base` plus the frames written before each, at `rate`, to the nanosecond.
    fn assert_stamps_step_by_frames_written(blocks: &[(i64, Vec<u8>)], base: i64, rate: u32, frame_bytes: usize, what: &str) {
        let mut written = 0u64;
        for (n, (stamp, bytes)) in blocks.iter().enumerate() {
            assert_eq!(*stamp, after(base, written, rate), "{what}: block {n} is stamped where {written} frames at {rate} Hz put it");
            written += (bytes.len() / frame_bytes) as u64;
        }
    }

    /// **A seek starts the filter and the clock again**: after `reset` the output is a fresh converter's for the same
    /// input (nothing of the old position's tail is blended in), the first block is stamped at its own pts, and the
    /// stamps step by exactly the frames written over the output rate, in whole nanoseconds, however many blocks.
    #[test]
    fn a_seek_starts_the_filter_and_the_clock_again_and_stamps_step_by_the_frames_written() {
        // 192, 176.4 and 384 kHz run through the relaxed front and a FULL stage: a reset starts all of them again
        for (source_rate, rate) in [(96_000u32, 48_000u32), (88_200, 44_100), (192_000, 48_000), (176_400, 44_100), (384_000, 48_000)] {
            let shape = PcmOut { rate, bits: 32, channels: 2, source_rate };
            let block = (source_rate / 100) as usize; // 10 ms, a whole number of nanoseconds
            let input = noise(block * 12, 7);
            let mut conv = PcmConverter::resampled(shape);
            let first = run_blocks(&mut conv, &input, source_rate, block, 1_000_000_000);
            assert!(first.len() >= 11, "{source_rate}: a block that fits the filter writes");
            assert_eq!(first[0].0, 1_000_000_000, "{source_rate}: the first block is stamped at the anchor");
            assert_stamps_step_by_frames_written(&first, 1_000_000_000, rate, 8, "first run");
            conv.reset();
            let again = run_blocks(&mut conv, &input, source_rate, block, 60_000_000_000);
            let mut fresh = PcmConverter::resampled(shape);
            let want = run_blocks(&mut fresh, &input, source_rate, block, 60_000_000_000);
            assert_eq!(again, want, "{source_rate}: after a seek the output is a fresh converter's");
            assert_eq!(again[0].0, 60_000_000_000, "{source_rate}: the anchor restarts at the new position");
            assert_stamps_step_by_frames_written(&again, 60_000_000_000, rate, 8, "after the seek");
            let bytes = |v: &[(i64, Vec<u8>)]| v.iter().flat_map(|(_, b)| b.clone()).collect::<Vec<u8>>();
            assert_eq!(bytes(&first), bytes(&again), "{source_rate}: the old tail did not reach the new position");
            // a seek to where the clock already was (no gap to hide behind): the reset alone starts the filter and the anchor again
            let resume = after(60_000_000_000, (block * 12) as u64, source_rate);
            conv.reset();
            let contiguous = run_blocks(&mut conv, &input, source_rate, block, resume);
            let mut fresh = PcmConverter::resampled(shape);
            assert_eq!(contiguous, run_blocks(&mut fresh, &input, source_rate, block, resume), "{source_rate}: reset, not the gap check, starts it again");
            assert_eq!(contiguous[0].0, resume, "{source_rate}: and anchors at the new pts");
        }
    }

    /// **A jump in the content time re-anchors; jitter does not.** Matroska stamps in milliseconds, so a block that sits
    /// 5 ms from where the frames fed put it is the same run (its stamp still comes from the frames written); one that
    /// sits more than [`AU_GAP_TOLERANCE_NS`] away is a discontinuity: the clock re-anchors at its pts and the filter
    /// starts again.
    #[test]
    fn a_jump_past_the_gap_tolerance_re_anchors_and_millisecond_jitter_does_not() {
        let shape = PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 };
        let mut conv = PcmConverter::resampled(shape);
        let data = noise(960, 3);
        let bytes = s32(&data);
        let block = |conv: &mut PcmConverter, pts: i64| {
            let mut out = Vec::new();
            let stamp = conv.convert(Some(pts), 96_000, SampleFormat::S32, false, 2, 0, 960, &[&bytes], &mut out);
            (stamp, out.len() / 8)
        };
        let (s0, n0) = block(&mut conv, 0);
        assert_eq!(s0, Some(0));
        let (s1, n1) = block(&mut conv, 10_000_000 + 5_000_000);
        assert_eq!(s1, Some(after(0, n0 as u64, 48_000)), "5 ms off the clock is jitter: still stamped by the frames written");
        let (s2, _) = block(&mut conv, 20_000_000 - 5_000_000);
        assert_eq!(s2, Some(after(0, (n0 + n1) as u64, 48_000)), "and 5 ms early is too");
        let (s3, n3) = block(&mut conv, 2_000_000_000);
        assert_eq!(s3, Some(2_000_000_000), "2 s past it: a discontinuity, re-anchored at the block's own pts");
        assert!(n3 < 480, "the filter started again: {n3} frames, its lookahead held back");
        let (s4, _) = block(&mut conv, 2_010_000_000);
        assert_eq!(s4, Some(after(2_000_000_000, n3 as u64, 48_000)), "and stepping from the new anchor");
        let (s5, _) = block(&mut conv, 2_010_000_000 + 10_000_000 + AU_GAP_TOLERANCE_NS + 1);
        assert_eq!(s5, Some(2_010_000_000 + 10_000_000 + AU_GAP_TOLERANCE_NS + 1), "one nanosecond past the tolerance is a gap");
    }

    /// The decimator holds back its lookahead; the lossy path's stamps used to be the packet's, a half filter (about a
    /// millisecond) early for the audio that follows. They are now the anchor plus the frames written: the first block of
    /// a contiguous stream is stamped exactly at the anchor, the second at the anchor plus the first's frame count.
    #[test]
    fn the_lossy_clock_stamps_from_the_anchor_and_the_frames_written() {
        let mut conv = PcmConverter::lossy();
        let block = flt(&vec![0.25; 960 * 2]);
        let mut written = 0u64;
        for n in 0..4 {
            let mut out = Vec::new();
            // the packets' own stamps are a millisecond early and late alternately, as Matroska's are
            let pts = after(123_456_789, n * 960, 96_000) + if n % 2 == 0 { -1_000_000 } else { 1_000_000 };
            let pts = if n == 0 { 123_456_789 } else { pts };
            let stamp = conv.convert(Some(pts), 96_000, SampleFormat::Flt, false, 2, 0, 960, &[&block], &mut out).expect("a block of 10 ms writes");
            assert_eq!(stamp, after(123_456_789, written, 48_000), "lossy block {n}");
            written += (out.len() / 4) as u64;
        }
        assert_eq!(conv.report().frames_out, written);
    }

    /// **Full scale saturates, it never wraps.** A +-1.0 square wave at 96 kHz comes out of the low-pass overshooting full
    /// scale at its edges: the S32 output stops at exactly `i32::MAX` / `i32::MIN` (no sign flip anywhere in a flat half),
    /// the report counts the clipped samples and a peak above 1.0; a half-scale one clips nothing.
    #[test]
    fn full_scale_saturates_to_the_i32_range_and_is_counted() {
        let shape = PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 };
        let square = |level: i32| -> Vec<i32> {
            (0..960 * 20).flat_map(|i| { let v = if (i / 480) % 2 == 0 { level } else { level.wrapping_neg().max(i32::MIN + 1) }; [v, v] }).collect()
        };
        // +1.0 is not an i32, -1.0 is exactly i32::MIN
        let full: Vec<i32> = (0..960 * 20).flat_map(|i| { let v = if (i / 480) % 2 == 0 { i32::MAX } else { i32::MIN }; [v, v] }).collect();
        let mut conv = PcmConverter::resampled(shape);
        let got = run_blocks(&mut conv, &full, 96_000, 960, 0);
        let out: Vec<i32> = got.iter().flat_map(|(_, b)| s32_le(b)).collect();
        let left: Vec<i32> = out.iter().step_by(2).copied().collect();
        assert_eq!((left.iter().max(), left.iter().min()), (Some(&i32::MAX), Some(&i32::MIN)), "saturated at the i32 limits");
        let report = conv.report();
        assert!(report.clipped > 0 && report.peak > 1.0, "{report:?}: the overshoot at the edges is counted");
        assert_eq!(report.frames_out as usize, left.len());
        // the same stream through the resampler alone, saturated by the definition: the S32 bytes are exactly that, so the
        // overshoot is clamped where a wrapped one would have flipped its sign
        let as_f64: Vec<[f64; 2]> = full.chunks(2).map(|c| [f64::from(c[0]) / 2_147_483_648.0, f64::from(c[1]) / 2_147_483_648.0]).collect();
        let mut reference = Vec::new();
        Resampler::new(96_000, 48_000).run(&as_f64, &mut reference);
        let scaled: Vec<f64> = reference.iter().flatten().map(|v| (v * 2_147_483_648.0).round()).collect();
        let beyond = scaled.iter().filter(|&&v| v > 2_147_483_647.0 || v < -2_147_483_648.0).count() as u64;
        assert!(beyond > 0, "the overshoot reaches past full scale in the reference too");
        assert_eq!(report.clipped, beyond, "every sample past the i32 range is counted once");
        let want: Vec<i32> = scaled.iter().map(|v| v.clamp(-2_147_483_648.0, 2_147_483_647.0) as i32).collect();
        assert_eq!(out, want, "saturated, never wrapped");
        // and the middle of every flat half keeps its sign (a half is 240 frames at 48 kHz, its edges ring for 41 either side)
        let mut checked = 0;
        for half in 1..left.len() / 240 - 1 {
            let slice = &left[half * 240 + 60..half * 240 + 180];
            let positive = slice[0] > 0;
            assert!(slice.iter().all(|&v| (v > 0) == positive), "half {half}: no sign flip in a flat half");
            checked += 1;
        }
        assert!(checked >= 10);
        // half scale: no clipping, nothing near full scale
        let mut conv = PcmConverter::resampled(shape);
        run_blocks(&mut conv, &square(i32::MAX / 2), 96_000, 960, 0);
        let report = conv.report();
        assert_eq!(report.clipped, 0, "{report:?}");
        assert!(report.peak > 0.5 && report.peak < 0.6, "{report:?}: the overshoot of a half-scale square wave is under 10%");
    }

    /// At a table rate the bit-exact path stays: the declared shape asks for no converter at all, so the decoder's
    /// bytes reach the lane unchanged (`interleave_into`, with the tests above and `ff_pcm_decode_tests`).
    #[test]
    fn a_shape_at_the_declared_rate_needs_no_converter() {
        for out in [PcmOut::exact(48_000, 16, 2), PcmOut::exact(44_100, 32, 1), PcmOut::exact(8_000, 16, 1), STEREO_48K] {
            assert!(!out.resamples(), "{out:?}");
        }
        for (rate, source_rate) in [(48_000, 96_000), (48_000, 192_000), (44_100, 88_200), (44_100, 705_600), (48_000, i32::MAX as u32)] {
            assert!(PcmOut { rate, bits: 32, channels: 2, source_rate }.resamples(), "{source_rate} to {rate}");
        }
        assert_eq!(PcmOut::exact(44_100, 32, 1), PcmOut { rate: 44_100, bits: 32, channels: 1, source_rate: 44_100 });
    }

    /// **A 16-bit source through the resampling shape is S32 at scale 2^31**: a DC of 0x4000 (half scale on the 16-bit grid) comes out
    /// at 0x40000000 give or take a least significant bit, not as a 16-bit value, and a mono stream keeps its one channel, equal to the
    /// left channel of the stereo run (the stereo kernel's other channel carries a copy that is not written).
    #[test]
    fn a_16_bit_source_resamples_to_s32_at_scale_2_pow_31_and_mono_keeps_one_channel() {
        let left = vec![0x4000i16; 4000];
        let stereo: Vec<i16> = left.iter().flat_map(|&l| [l, 0x1000]).collect();
        let run = |channels: usize, data: &[i16]| {
            let mut conv = PcmConverter::resampled(PcmOut { rate: 48_000, bits: 32, channels: channels as u32, source_rate: 96_000 });
            let bytes = s16(data);
            let mut out = Vec::new();
            conv.convert(Some(0), 96_000, SampleFormat::S16, false, channels, 0, data.len() / channels, &[&bytes], &mut out);
            s32_le(&out)
        };
        let two = run(2, &stereo);
        let mono = run(1, &left);
        assert!(two.len() > 1_000 && two.len() % 2 == 0);
        for pair in two[200..two.len() - 200].chunks(2) {
            assert!((i64::from(pair[0]) - 0x4000_0000).abs() <= 1, "left: {:#x}", pair[0]);
            assert!((i64::from(pair[1]) - 0x1000_0000).abs() <= 1, "right: {:#x}", pair[1]);
        }
        let stereo_left: Vec<i32> = two.iter().step_by(2).copied().collect();
        assert_eq!(mono, stereo_left, "mono is the stereo run's left channel, one sample a frame");
        // planar input reads the same
        let mut conv = PcmConverter::resampled(PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 });
        let (l, r) = (s16(&vec![0x4000i16; 4000]), s16(&vec![0x1000i16; 4000]));
        let mut planar = Vec::new();
        conv.convert(Some(0), 96_000, SampleFormat::S16, true, 2, 0, 4000, &[&l, &r], &mut planar);
        assert_eq!(s32_le(&planar), two, "planar and packed are the same samples");
    }

    #[test]
    fn only_the_decoders_integer_formats_are_accepted() {
        assert_eq!(sample_width(AV_SAMPLE_FMT_S16), Some(2));
        assert_eq!(sample_width(AV_SAMPLE_FMT_S32P), Some(4));
        assert_eq!(sample_width(3), None); // FLT
    }
}
