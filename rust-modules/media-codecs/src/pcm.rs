//! Pure PCM output shape, conversion and access-unit batching.
use crate::speed_audio::{self, Resampler, SampleFormat};
use std::os::raw::c_int;

/// FFmpeg `AVSampleFormat` values the lossless decoders produce (FLAC, ALAC, `pcm_s*`). The enum has been stable since
/// FFmpeg 1.x (`S16 = 1`, `S32 = 2`, `S16P = 6`, `S32P = 7`).
pub const AV_SAMPLE_FMT_S16: c_int = 1;
pub const AV_SAMPLE_FMT_S32: c_int = 2;
pub const AV_SAMPLE_FMT_S16P: c_int = 6;
pub const AV_SAMPLE_FMT_S32P: c_int = 7;

/// The PCM the Load declared: the rate the decoder emits, the sample width (16 or 32), the channels, and the
/// rate the track itself is encoded at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PcmOut {
    /// What the Load declared (a rate of `devcaps::PCM_SAMPLE_RATES`), and what the decoder emits.
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
    /// The rate of the file; `PcmDecoder::open` refuses a lossless one that is not this. Equal to `rate` at a table
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
    /// `out.channels` (1 or 2) kept, S32. (`bits` is 32 by `route::pcm_format_of`'s rule; `PcmDecoder::open` refuses
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

/// PURE: feeding `in_width`-byte samples as `out_width`-byte ones loses nothing (equal, or widening).
pub fn width_is_exact(in_width: usize, out_width: usize) -> bool {
    out_width >= in_width
}

pub fn sample_width(format: c_int) -> Option<usize> {
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
pub const MIN_AU_MS: u32 = 10;

/// How far a block's stamp may sit from where the length of what is gathered puts it before the content
/// time counts as having a gap. Matroska stamps in milliseconds, so the blocks of a stream shorter than
/// a millisecond jitter by about one around their true time.
pub const AU_GAP_TOLERANCE_NS: i64 = 20_000_000;

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
