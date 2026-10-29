//! **Playback speed, the audio half: decoded sound in, pitch-corrected 48 kHz stereo PCM out.**
//!
//! LG's own rate control cannot be used on this pipeline (`docs/parity-gaps.md`, Playback Speed):
//! `SetPlayRate` is a flushing rate seek that the buffer-feed source's next SEGMENT resets to 1x,
//! and libpf mutes the audio sink at any rate but 1x by design. So speed is done the way a browser
//! does it. The engine feeds every timestamp divided by the speed (`player::to_sink_ns`), the
//! pipeline plays that at its own 1x, and the sound — which normally reaches the TV still
//! compressed — is decoded in the app, time-stretched here, and fed as PCM (`"audio":"PCM"` +
//! `contents.pcmInfo` in the Load payload, proven on a 3.9.3 set 2026-10-15).
//!
//! Pure DSP and nothing else: no FFmpeg, no TV, no clock. The demuxer converts each decoded frame
//! to stereo with [`downmix_by_mask`], hands it to [`SpeedAudio::push`] with the frame's content PTS, and
//! pushes what comes back onto the audio lane as ordinary AUs.
//!
//! **Time-stretch is WSOLA** (waveform-similarity overlap-add): the output is built from 20 ms
//! windows taken from the input at `speed` times the output pace, each one shifted by up to
//! ±10 ms to where it best continues the previous one, and cross-faded in. Pitch is untouched
//! because no sample is played faster than it was recorded; only which ones are kept changes.

/// The output rate. `pcmInfo.sampleRate` is matched in KILOHERTZ against libpf's table, so the
/// Load payload says `48` for this (`player::engine`).
pub const OUT_RATE: u32 = 48_000;

/// Output frames per hop: half a 20 ms window at 48 kHz.
const HOP: usize = 480;
/// How far a window may move from its nominal position to line up with the previous one.
const SEARCH: usize = 480;
/// Frames per emitted AU (~21 ms, the size of the AAC AUs the lane is used to).
const CHUNK: usize = 1024;

/// One FFmpeg sample format, as the integer `AVSampleFormat` the decoder reports. The values are
/// FFmpeg's public enum and are proven against the bundled headers in `ci/ffabi-assert.c`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    U8,
    S16,
    S32,
    Flt,
    Dbl,
}

impl SampleFormat {
    /// `(format, planar)` for an `AVSampleFormat`, or `None` for one this does not convert.
    pub fn from_av(fmt: i32) -> Option<(SampleFormat, bool)> {
        Some(match fmt {
            0 => (Self::U8, false),
            1 => (Self::S16, false),
            2 => (Self::S32, false),
            3 => (Self::Flt, false),
            4 => (Self::Dbl, false),
            5 => (Self::U8, true),
            6 => (Self::S16, true),
            7 => (Self::S32, true),
            8 => (Self::Flt, true),
            9 => (Self::Dbl, true),
            _ => return None,
        })
    }

    pub(crate) fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::S16 => 2,
            Self::S32 | Self::Flt => 4,
            Self::Dbl => 8,
        }
    }

    /// Sample `i` of `bytes`, as -1.0..1.0.
    fn read(self, bytes: &[u8], i: usize) -> f32 {
        let at = i * self.bytes();
        let b = &bytes[at..at + self.bytes()];
        match self {
            Self::U8 => (b[0] as f32 - 128.0) / 128.0,
            Self::S16 => i16::from_ne_bytes([b[0], b[1]]) as f32 / 32768.0,
            Self::S32 => i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            Self::Flt => f32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
            Self::Dbl => f64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
        }
    }
}

/// **Any channel count down to stereo**, in FFmpeg's native order (FL FR FC LFE then the
/// surrounds). Mono is duplicated; the centre goes to both sides at -3 dB, the surrounds to their
/// own side at -3 dB, LFE is dropped, and anything past the first six is ignored. The mix is
/// scaled by the gain it can reach so a full-scale 5.1 cannot clip.
///
/// `planes` is one slice per channel for a planar format, or a single interleaved slice.
pub fn downmix(fmt: SampleFormat, planar: bool, channels: usize, frames: usize, planes: &[&[u8]]) -> Vec<[f32; 2]> {
    let sample = |ch: usize, i: usize| -> f32 {
        if planar {
            fmt.read(planes[ch], i)
        } else {
            fmt.read(planes[0], i * channels + ch)
        }
    };
    const M3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let mut out = Vec::with_capacity(frames);
    match channels {
        0 => out.resize(frames, [0.0, 0.0]),
        1 => out.extend((0..frames).map(|i| {
            let s = sample(0, i);
            [s, s]
        })),
        2 => out.extend((0..frames).map(|i| [sample(0, i), sample(1, i)])),
        n => {
            let has_c = n >= 3;
            let (sl, sr) = match n {
                3 | 4 => (None, None),
                5 => (Some(3), Some(4)), // FL FR FC BL BR: no LFE
                _ => (Some(4), Some(5)),
            };
            let gain = 1.0 / (1.0 + if has_c { M3DB } else { 0.0 } + if sl.is_some() { M3DB } else { 0.0 });
            out.extend((0..frames).map(|i| {
                let c = if has_c { sample(2, i) * M3DB } else { 0.0 };
                let l = sample(0, i) + c + sl.map_or(0.0, |s| sample(s, i) * M3DB);
                let r = sample(1, i) + c + sr.map_or(0.0, |s| sample(s, i) * M3DB);
                [l * gain, r * gain]
            }));
        }
    }
    out
}

/// **Mix by channel ROLE**, not by count: `mask` is the frame's `AVChannelLayout` native mask, whose
/// channels come in ascending bit order (`AV_CH_*`). [`downmix`] guesses the roles from the count,
/// which is right for mono, stereo, 3.0, 5.0 and 5.1 and wrong for the rest (a quad's surrounds, an
/// LFE beside four channels, a 3/1 layout's centre-back, 7.1). A mask that does not name exactly
/// `channels` channels (an unspecified or custom layout) falls back to the count.
pub fn downmix_by_mask(
    fmt: SampleFormat, planar: bool, mask: u64, channels: usize, frames: usize, planes: &[&[u8]],
) -> Vec<[f32; 2]> {
    if mask == 0 || mask.count_ones() as usize != channels {
        return downmix(fmt, planar, channels, frames, planes);
    }
    // One (left, right) coefficient pair per channel, in the mask's ascending-bit order, scaled so
    // the louder side's coefficients sum to 1: a full-scale signal on every channel cannot clip.
    let mut gains: Vec<[f32; 2]> = (0..64).filter(|b| mask >> b & 1 == 1).map(role_gain).collect();
    let peak = gains.iter().map(|g| g[0]).sum::<f32>().max(gains.iter().map(|g| g[1]).sum::<f32>());
    if peak <= 0.0 {
        return downmix(fmt, planar, channels, frames, planes);
    }
    for g in &mut gains {
        *g = [g[0] / peak, g[1] / peak];
    }
    (0..frames)
        .map(|i| {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for (ch, g) in gains.iter().enumerate() {
                if g[0] == 0.0 && g[1] == 0.0 {
                    continue;
                }
                let s = if planar { fmt.read(planes[ch], i) } else { fmt.read(planes[0], i * channels + ch) };
                l += s * g[0];
                r += s * g[1];
            }
            [l, r]
        })
        .collect()
}

/// The stereo coefficients of one `AV_CH_*` bit before scaling: the fronts at 1, the centre and
/// the side/back/top channels at -3 dB toward their own side, a centre-back or top-centre split
/// to both sides at -6 dB, the LFEs and anything unnamed dropped.
fn role_gain(bit: u32) -> [f32; 2] {
    const M3: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match bit {
        0 | 29 => [1.0, 0.0],                       // FL, STEREO_LEFT
        1 | 30 => [0.0, 1.0],                       // FR, STEREO_RIGHT
        2 => [M3, M3],                              // FC
        4 | 6 | 9 | 12 | 15 | 31 | 33 | 36 | 39 => [M3, 0.0], // BL FLC SL TFL TBL WL SDL TSL BFL
        5 | 7 | 10 | 14 | 17 | 32 | 34 | 37 | 40 => [0.0, M3], // BR FRC SR TFR TBR WR SDR TSR BFR
        8 | 11 | 13 | 16 | 38 => [0.5, 0.5],        // BC TC TFC TBC BFC
        _ => [0.0, 0.0],                            // LFE (3), LFE2 (35), the rest
    }
}

/// **The one resampler: `from` Hz in, `to` Hz out, f64 frames, stateful across calls so chunk edges are
/// seamless.** The speed path (`SpeedAudio`) and the lossy app-decoded path
/// ([`PcmConverter`](crate::ff_pcm::PcmConverter), lossy shape) run it to [`OUT_RATE`]; `to` is any rate the caller needs.
///
/// * `from == to`: the frames are copied.
/// * `from < to` (44.1, 32, 22.05 kHz to 48): linear interpolation. Nothing folds back when the rate is
///   raised, only a faint image remains.
/// * `from > to`: a cascade of [`Decimator`]s, the one Kaiser-windowed-sinc low-pass of this file, every
///   stage with its cutoff at its own output's Nyquist. Plain interpolation would fold everything above the
///   output's Nyquist back into the band at full strength.
///
/// **The cascade is three parts, and bounds the converter whatever the rate.** All three read `from` as an integer
/// (a `u64`), halving it as they go.
/// 1. *Relaxed halvings from the top*: while `from >= 4 * to` and `from` is even, one exact 2:1 stage of
///    [`Kernel::RELAXED`] takes `from` to `from / 2` (192 kHz to 48 kHz takes one, 384 kHz two, 2.8224 MHz to 44.1 kHz five). A
///    relaxed stage is 25 taps instead of 83 and exists only where the rate is far above what the last stage keeps (see the
///    kernel for why it may be that cheap there).
/// 2. *Stage 0*: `k` is the smallest integer >= 0 with `from <= 8 * (to << k)`. A [`Kernel::FULL`] stage takes `from` to
///    `to << k`, a ratio of at most 8 (so at most 328 frames of lookahead and 656 taps),
/// 3. then `k` FULL stages of exactly 2:1 (one phase, 83 weights) take it down to `to` by halving UP FROM THE OUTPUT: the
///    boundaries are `to * 2^j`, not `from / 2^j`.
///
/// Part 3 halves from the output because halving an odd rate from the top would hand 1 000 003 Hz a fresh 1024-phase
/// table at every stage. Part 1 brings that back for no rate: it stops at the first odd `from` (192 001 and 1 000 003 Hz build
/// none), and every stage it builds is an exact 2:1 of one phase, so the only stage with a phased table is still stage 0.
/// Zero group delay composes, so output frame 0 is still centred on input frame 0.
///
/// Worst-case filter-table memory for ANY `i32` source rate (`TABLE_BYTES_BOUND`, held by a test through `Resampler::table_bytes`):
/// one final-ratio-<= 8 stage (656 taps * 1024 phases * (8 B weight + 2 B index) + 4 KB of offsets, about 6.7 MB,
/// reached only by an inexact ratio of that size; every real rate is far below it, and 96, 192, 384, 88.2,
/// 176.4, 352.8 and 705.6 kHz are one phase) plus FULL 2:1 stages of under 1 KB each (`k` of them, at most 14 for
/// any `i32` rate and an output of 16 384 Hz or more) plus RELAXED ones of under 500 B (at most 31). The work per input
/// frame is bounded: the relaxed front costs under 25 multiply-adds a channel per source frame (25 taps per output,
/// the first stage's output being half the source rate, the next half of that), stage 0 about 82 (2 * 41 taps per step
/// of the ratio) and the 2:1 tail at most the same again, those two seeing at most half the source's rate once there is a
/// front. A 192 kHz source to 48 kHz is 25 * 96 000 + 83 * 48 000 = 6.4 M multiply-adds a second a channel, against the
/// 247 * 48 000 = 11.9 M of the single stage of ratio 4 it used to be; 384 kHz is 11.2 M against 27.6 M.
/// The cascade departs from "halve from the top" for the part that is not exact on purpose, and there is no rate above which it refuses.
pub(crate) struct Resampler {
    from: u32,
    to: u32,
    /// Position of the next output frame (linear path), in input frames from the start of `last` + the new block.
    pos: f64,
    last: Option<[f64; 2]>,
    /// Non-empty for a source above the output rate: the relaxed 2:1 stages (none for an odd rate or one under 4x the
    /// output), then stage 0, then the FULL 2:1 stages. The linear path is then unused.
    stages: Vec<Decimator>,
    /// The two buffers a block ping-pongs between as it crosses the stages.
    ping: Vec<[f64; 2]>,
    pong: Vec<[f64; 2]>,
}

/// The widest stage's filter bytes: ratio 8 is `2 * 328` taps, each of `DECIMATOR_MAX_PHASES` phases a weight (f64) and a tap
/// index (u16), plus a row offset (u32) per phase and one more.
#[cfg(test)]
const WIDEST_STAGE_TABLE_BYTES: usize = 2 * 328 * DECIMATOR_MAX_PHASES * (8 + 2) + (DECIMATOR_MAX_PHASES + 1) * 4;
/// A FULL 2:1 stage keeps 83 of its 164 taps (and two row offsets); the dense bound is used here.
#[cfg(test)]
const HALVING_STAGE_TABLE_BYTES: usize = 164 * (8 + 2) + 2 * 4;
/// The FULL 2:1 stages after the first an `i32` source rate needs at most (`to >= 16 384`, and the relaxed front takes its share of the halvings).
#[cfg(test)]
const HALVING_STAGES: usize = 14;
/// A RELAXED 2:1 stage keeps 25 of its 48 taps (and two row offsets); the dense bound is used here.
#[cfg(test)]
const RELAXED_STAGE_TABLE_BYTES: usize = 48 * (8 + 2) + 2 * 4;
/// The relaxed halvings a source needs at most: each halves an even `from` that is at least `4 * to`, so a 32-bit rate
/// and an output of 1 Hz take 30.
#[cfg(test)]
const RELAXED_STAGES: usize = 31;
/// What [`Resampler::table_bytes`] cannot exceed for any source rate (about 6.7 MB): see [`Resampler`].
#[cfg(test)]
pub(crate) const TABLE_BYTES_BOUND: usize =
    WIDEST_STAGE_TABLE_BYTES + HALVING_STAGES * HALVING_STAGE_TABLE_BYTES + RELAXED_STAGES * RELAXED_STAGE_TABLE_BYTES;

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Self {
        let mut stages = Vec::new();
        if from > to && to > 0 {
            let (mut from64, to64) = (from as u64, to as u64);
            while from64 >= 4 * to64 && from64 % 2 == 0 {
                stages.push(Decimator::new(2, 1, Kernel::RELAXED));
                from64 /= 2;
            }
            let mut k = 0u32;
            while from64 > 8 * (to64 << k) {
                k += 1;
            }
            stages.push(Decimator::new(from64, to64 << k, Kernel::FULL));
            stages.extend((0..k).map(|_| Decimator::new(2, 1, Kernel::FULL)));
        }
        Resampler { from, to, pos: 0.0, last: None, stages, ping: Vec::new(), pong: Vec::new() }
    }

    /// Bytes the filter tables of every stage hold (what `TABLE_BYTES_BOUND` bounds).
    #[cfg(test)]
    pub(crate) fn table_bytes(&self) -> usize {
        self.stages.iter().map(Decimator::table_bytes).sum()
    }

    pub(crate) fn run(&mut self, input: &[[f64; 2]], out: &mut Vec<[f64; 2]>) {
        if self.from == self.to {
            out.extend_from_slice(input);
            return;
        }
        if let Some((first, rest)) = self.stages.split_first_mut() {
            self.ping.clear();
            first.run(input, &mut self.ping);
            for stage in rest {
                self.pong.clear();
                stage.run(&self.ping, &mut self.pong);
                std::mem::swap(&mut self.ping, &mut self.pong);
            }
            out.extend_from_slice(&self.ping);
            return;
        }
        if self.from == 0 || self.to == 0 {
            return;
        }
        let step = self.from as f64 / self.to as f64;
        // index -1 is `last`, so position p reads between frame floor(p) and floor(p)+1 of that view
        let at = |i: isize| -> Option<[f64; 2]> {
            if i < 0 {
                self.last
            } else {
                input.get(i as usize).copied()
            }
        };
        let offset = if self.last.is_some() { -1.0 } else { 0.0 };
        loop {
            let p = self.pos + offset;
            let i = p.floor() as isize;
            let (Some(a), Some(b)) = (at(i), at(i + 1)) else { break };
            let f = p - i as f64;
            out.push([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]);
            self.pos += step;
        }
        if let Some(&tail) = input.last() {
            // re-base so the next block's index 0 follows `tail`
            self.pos -= input.len() as f64 + if self.last.is_some() { 1.0 } else { 0.0 } - 1.0;
            self.last = Some(tail);
        }
    }
}

/// One Kaiser-windowed-sinc low-pass design for a [`Decimator`] stage: how many zero crossings of the sinc it keeps, where it
/// turns over and how hard it is windowed. The two designs below are the only ones; a stage names which it is built with.
#[derive(Clone, Copy)]
struct Kernel {
    /// Zero crossings of the sinc kept on each side of its centre, in units of the cutoff's own period.
    zeros: f64,
    /// Where the low-pass turns over, as a fraction of the stage's output Nyquist frequency: AT it, so the response is half
    /// (-6 dB) there.
    cutoff: f64,
    /// The Kaiser window's beta: it sets the depth of the stop band (about `beta / 0.1102 + 8.7` dB, 125 dB at 12.8).
    beta: f64,
}

impl Kernel {
    /// **The design of every stage but the relaxed front, and always of the last: the one that sets the converter's pass
    /// band and stop band.** Cutoff 1.0 of the output's Nyquist, so the response is half (-6 dB) AT it, flat (within 0.01 dB,
    /// measured under 1e-5) to 90% of it (21.6 kHz at 48 kHz, 19.845 kHz at 44.1 kHz) and at least 120 dB down from
    /// `fout - 0.9 Nyquist` (26.4 kHz at 48 kHz) up: what can fold into the pass band is gone. The 90..100% of Nyquist
    /// is the transition band, neither flat nor alias-free (content from 24 to 26.4 kHz folds into it), the SoX `-a`
    /// convention. 41 zero crossings and beta 12.8 (about 125 dB) give the 120 dB (this file's tests hold it over tones
    /// from 96, 192, 64, 384, 88.2, 176.4 and 705.6 kHz sources; they measure -124.5 dB at worst, at the stop band's edge).
    const FULL: Kernel = Kernel { zeros: 41.0, cutoff: 1.0, beta: 12.8 };

    /// **The cheap design of the exact 2:1 stages in front of the rest, for a source of 4x the output or more.**
    ///
    /// *What it must do.* O is the final output rate. The last stage ([`Kernel::FULL`]) is flat to 0.45 * O and rejects
    /// from 0.55 * O up (its transition band is 0.45..0.55 * O), so whatever a relaxed stage R -> R/2 lets fold below
    /// 0.55 * O is never removed afterwards, while everything it leaves above that is. A relaxed stage therefore has to be
    /// flat to 0.45 * O (within the limits of this file) and to reject `[R/2 - 0.55 * O, R/2]` by 120 dB: for 192 kHz to
    /// 48 kHz, 69.6..96 kHz must not fold into 0..26.4 kHz. Nothing else is asked of it: 48..69.6 kHz folds to
    /// 26.4..48 kHz, which the last stage removes.
    ///
    /// *The worst case is R = 4 * O*, the least a stage is built for (`Resampler::new`). Its output Nyquist is O, the
    /// cutoff is that Nyquist (1.0: a half-band, -6 dB there) and the transition runs from 0.55 * O to 1.45 * O, a
    /// symmetric half-width of 0.45 of the Nyquist about the cutoff: 0.9 * cutoff, which is 0.225 of the stage's input
    /// rate. A higher R is easier: the cutoff, and with it the rejection edge 1.45 * R/4, grows with R while the band to
    /// keep stays 0.45 * O.
    ///
    /// *Length.* Kaiser's estimate for A dB of rejection over a transition of 0.225 of the sample rate is
    /// `(A - 7.95) / (14.36 * 0.225)` taps, and the sinc of a half-band has a zero crossing every 2 taps, so
    /// `zeros >= (A - 7.95) / (14.36 * 0.9)`: 9.05 for the 125 dB of beta 12.8, 9.9 for the 136 dB of beta 14. Measured
    /// on this stage alone at 192 kHz, 9 crossings with beta 12.8 leave 118.6 dB at 69.6 kHz (short of 120) and 10 give
    /// 129.5 dB. 12 crossings with beta 14 keep two crossings of margin: 24 frames each side, 25 nonzero taps (the
    /// centre and the odd offsets 1..23; the even ones are zeros of the sinc) against the 83 of a FULL 2:1 stage, and
    /// 136.0 dB at worst above 69.6 kHz, 140.9 dB at 69.6 kHz itself, flat to 2.5e-6 dB at 21.6 kHz.
    const RELAXED: Kernel = Kernel { zeros: 12.0, cutoff: 1.0, beta: 14.0 };

    /// Input frames on each side of the centre a stage of `step` input frames per output frame reaches.
    fn half(self, step: f64) -> usize {
        (self.zeros / (self.cutoff / step)).ceil() as usize
    }
}

/// The finest fractional input position the weights are tabulated for. A rational ratio with a smaller
/// denominator (96 and 192 kHz: 1; 88.2 kHz to 48 kHz: 80; 50 kHz: 24) is exact; any other is rounded to the
/// nearest 1/1024 of an input frame.
const DECIMATOR_MAX_PHASES: usize = 1024;

/// A windowed-sinc low-pass and decimator for one stage of the [`Resampler`]: the output at input position `x` is
/// the Kaiser-windowed sinc ([`Kernel`]: its cutoff is a fraction of the output's Nyquist) centred on `x` dotted with the input
/// around it, the weights for every fractional position the ratio visits tabulated once, in f64, and only the
/// nonzero ones (the sinc is exactly zero at every integer multiple of its period: half of the 164 taps of a 2:1
/// stage). No group delay (the kernel is symmetric about the output instant); it needs `half` frames of
/// lookahead, so what it holds back is `half` input frames (82 at a ratio of 2: 0.85 ms at 96 kHz, 0.93 ms at 88.2 kHz;
/// 24 for a relaxed one)
/// rather than a time shift. Nothing flushes them: the last of those frames at the end of a stream is not
/// emitted, which is the length of one filter half-width of audio, not heard.
///
/// The ratio is the reduced rational `num / den` input frames per output frame, and the position of the next
/// output is the integer `pos_i` plus the exact fraction `pos_f / den`: no float accumulates, so the output is the
/// same for any way the input is cut into blocks.
struct Decimator {
    num: u64,
    den: u64,
    /// Input frames on each side of the centre the kernel reaches.
    half: usize,
    phases: usize,
    /// `phases + 1` offsets: row `p` of `idx` and `w` is `starts[p]..starts[p + 1]`.
    starts: Vec<u32>,
    /// The tap (0..2 * half) each nonzero weight belongs to.
    idx: Vec<u16>,
    /// The nonzero weights of every row, each row normalised to sum to 1 (DC gain exactly 1).
    w: Vec<f64>,
    /// `half` frames of silence ahead of the stream, then what has not been consumed yet.
    buf: Vec<[f64; 2]>,
    /// The next output's centre: frame `pos_i` of `buf` plus `pos_f / den` of a frame.
    pos_i: usize,
    pos_f: u64,
}

impl Decimator {
    fn new(num: u64, den: u64, kernel: Kernel) -> Self {
        let g = gcd(num, den);
        let (num, den) = (num / g, den / g);
        let step = num as f64 / den as f64;
        // The kernel's cutoff in cycles per input frame, doubled: sinc(u) with u counted in
        // 1/(2*cutoff) input frames.
        let scale = kernel.cutoff / step;
        let half = kernel.half(step);
        let phases = (den as usize).min(DECIMATOR_MAX_PHASES);
        let mut starts = Vec::with_capacity(phases + 1);
        let (mut idx, mut w) = (Vec::new(), Vec::new());
        let i0_beta = bessel_i0(kernel.beta);
        for p in 0..phases {
            let frac = p as f64 / phases as f64;
            starts.push(w.len() as u32);
            let row_start = w.len();
            let mut sum = 0.0f64;
            for t in 0..2 * half {
                // tap t reads input frame i + 1 - half + t, at distance (that - (i + frac)) from the centre
                let u = ((t as f64 + 1.0 - half as f64) - frac) * scale;
                let r = u / kernel.zeros;
                let weight = if r.abs() >= 1.0 {
                    0.0
                } else {
                    sinc(u) * bessel_i0(kernel.beta * (1.0 - r * r).sqrt()) / i0_beta
                };
                if weight != 0.0 {
                    sum += weight;
                    idx.push(t as u16);
                    w.push(weight);
                }
            }
            for weight in &mut w[row_start..] {
                *weight /= sum;
            }
        }
        starts.push(w.len() as u32);
        idx.shrink_to_fit();
        w.shrink_to_fit();
        Decimator { num, den, half, phases, starts, idx, w, buf: vec![[0.0; 2]; half], pos_i: half, pos_f: 0 }
    }

    #[cfg(test)]
    fn table_bytes(&self) -> usize {
        self.starts.len() * 4 + self.idx.len() * 2 + self.w.len() * 8
    }

    /// The table row for a fractional position of `pos_f / den`; a position that rounds up to the next whole frame
    /// is row 0 of the next one (`carry`).
    fn phase(&self) -> (usize, bool) {
        let p = if self.den as usize <= self.phases {
            self.pos_f as usize
        } else {
            ((self.pos_f * self.phases as u64 * 2 + self.den) / (2 * self.den)) as usize
        };
        if p == self.phases { (0, true) } else { (p, false) }
    }

    fn run(&mut self, input: &[[f64; 2]], out: &mut Vec<[f64; 2]>) {
        self.buf.extend_from_slice(input);
        loop {
            let (phase, carry) = self.phase();
            let i = self.pos_i + carry as usize;
            // the last frame the kernel reads is i + half
            if i + self.half >= self.buf.len() {
                break;
            }
            let (from, to) = (self.starts[phase] as usize, self.starts[phase + 1] as usize);
            let window = &self.buf[i + 1 - self.half..i + 1 + self.half];
            let (idx, w) = (&self.idx[from..to], &self.w[from..to]);
            let (idx4, w4) = (idx.chunks_exact(4), w.chunks_exact(4));
            let (idx_rest, w_rest) = (idx4.remainder(), w4.remainder());
            // four independent accumulator pairs: one dependent chain would run at the FPU's latency
            let (mut l0, mut r0, mut l1, mut r1, mut l2, mut r2, mut l3, mut r3) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for (k, c) in idx4.zip(w4) {
                let (a, b, d, e) = (window[k[0] as usize], window[k[1] as usize], window[k[2] as usize], window[k[3] as usize]);
                l0 += c[0] * a[0];
                r0 += c[0] * a[1];
                l1 += c[1] * b[0];
                r1 += c[1] * b[1];
                l2 += c[2] * d[0];
                r2 += c[2] * d[1];
                l3 += c[3] * e[0];
                r3 += c[3] * e[1];
            }
            for (&k, &c) in idx_rest.iter().zip(w_rest) {
                let a = window[k as usize];
                l0 += c * a[0];
                r0 += c * a[1];
            }
            out.push([(l0 + l1) + (l2 + l3), (r0 + r1) + (r2 + r3)]);
            // advance by num/den exactly
            self.pos_f += self.num % self.den;
            self.pos_i += (self.num / self.den) as usize;
            if self.pos_f >= self.den {
                self.pos_f -= self.den;
                self.pos_i += 1;
            }
        }
        // drop what no later output reads, keeping the kernel's reach behind the next centre
        let consumed = self.pos_i.saturating_sub(self.half).min(self.buf.len());
        self.buf.drain(..consumed);
        self.pos_i -= consumed;
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `sin(pi x) / (pi x)`, in plain arithmetic (`ci/allow/libm.txt` keeps transcendentals to render code):
/// the argument is folded to `[-1/2, 1/2]` of a half turn and the sine is its Taylor series there.
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        return 1.0;
    }
    let n = x.round();
    let y = (x - n) * std::f64::consts::PI;
    let (y2, mut term, mut sum) = (y * y, y, y);
    for k in 1..12 {
        term *= -y2 / ((2 * k) as f64 * (2 * k + 1) as f64);
        sum += term;
    }
    let sign = if (n as i64) % 2 == 0 { 1.0 } else { -1.0 };
    sign * sum / (x * std::f64::consts::PI)
}

/// The modified Bessel function of the first kind, order 0 (the Kaiser window's), by its power series.
fn bessel_i0(x: f64) -> f64 {
    let (q, mut term, mut sum) = (x * x / 4.0, 1.0, 1.0);
    for k in 1..60 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// WSOLA time-stretch at `speed` (input frames consumed per output frame).
struct Stretch {
    speed: f64,
    input: Vec<[f32; 2]>,
    /// Nominal input position of the next window, relative to `input[0]`.
    pos: f64,
    /// Start of the previous window in `input`, so its natural continuation is at `prev + HOP`.
    prev: Option<usize>,
    /// The second half of the previous window, to cross-fade out under the next.
    tail: Vec<[f32; 2]>,
    fade: Vec<f32>,
}

impl Stretch {
    fn new(speed: f64) -> Self {
        // smoothstep: the same S-shaped equal-sum crossfade as a Hann half-window, in plain
        // arithmetic (`ci/allow/libm.txt` keeps transcendentals to render code)
        let fade = (0..HOP)
            .map(|i| {
                let x = (i as f32 + 0.5) / HOP as f32;
                x * x * (3.0 - 2.0 * x)
            })
            .collect();
        Stretch { speed, input: Vec::new(), pos: 0.0, prev: None, tail: Vec::new(), fade }
    }

    /// Mono correlation of `input[a..a+HOP]` against `input[b..b+HOP]`, every 4th frame,
    /// normalised by the candidate's energy so a loud window does not win on loudness alone.
    fn similarity(&self, a: usize, b: usize) -> f32 {
        let (mut xy, mut xx) = (0.0f32, 1e-9f32);
        let mut i = 0;
        while i < HOP {
            let x = self.input[a + i][0] + self.input[a + i][1];
            let y = self.input[b + i][0] + self.input[b + i][1];
            xy += x * y;
            xx += x * x;
            i += 4;
        }
        xy / xx.sqrt()
    }

    fn run(&mut self, more: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        self.input.extend_from_slice(more);
        loop {
            let nominal = self.pos.round() as usize;
            if nominal + SEARCH + 2 * HOP > self.input.len() {
                break;
            }
            let start = match self.prev {
                None => nominal,
                Some(prev) => {
                    let target = prev + HOP;
                    let lo = nominal.saturating_sub(SEARCH);
                    let mut best = (f32::MIN, nominal);
                    let mut s = lo;
                    while s <= nominal + SEARCH {
                        let v = self.similarity(s, target);
                        if v > best.0 {
                            best = (v, s);
                        }
                        s += 2;
                    }
                    best.1
                }
            };
            if self.tail.is_empty() {
                out.extend_from_slice(&self.input[start..start + HOP]);
            } else {
                for i in 0..HOP {
                    let (fi, fo) = (self.fade[i], 1.0 - self.fade[i]);
                    let a = self.tail[i];
                    let b = self.input[start + i];
                    out.push([a[0] * fo + b[0] * fi, a[1] * fo + b[1] * fi]);
                }
            }
            self.tail.clear();
            self.tail.extend_from_slice(&self.input[start + HOP..start + 2 * HOP]);
            self.prev = Some(start);
            self.pos += HOP as f64 * self.speed;
            // drop what no future window or continuation can reach
            let keep_from = start.min((self.pos as usize).saturating_sub(SEARCH));
            if keep_from > 0 {
                self.input.drain(..keep_from);
                self.pos -= keep_from as f64;
                self.prev = Some(start - keep_from);
            }
        }
    }
}

/// **One playback's audio at one speed**: stereo at the source rate in, PCM AUs out, each stamped
/// with the CONTENT time its first frame stands for. The feed divides that by the speed like every
/// other timestamp, so audio and video stay on one clock.
pub struct SpeedAudio {
    permille: i32,
    resample: Option<Resampler>,
    stretch: Stretch,
    first_pts: Option<i64>,
    /// Output frames already handed out as AUs.
    emitted: u64,
    pending: Vec<[f32; 2]>,
    /// The block widened to f64 for the resampler, and what it returned.
    wide: Vec<[f64; 2]>,
    resampled: Vec<[f64; 2]>,
    scratch: Vec<[f32; 2]>,
}

impl SpeedAudio {
    pub fn new(permille: i32) -> Self {
        SpeedAudio {
            permille,
            resample: None,
            stretch: Stretch::new(permille as f64 / 1000.0),
            first_pts: None,
            emitted: 0,
            pending: Vec::new(),
            wide: Vec::new(),
            resampled: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Feed `frames` (stereo, `rate` Hz) whose first frame is at content time `pts_ns`. Returns
    /// complete AUs as `(content pts ns, s16le interleaved bytes)`.
    pub fn push(&mut self, pts_ns: i64, rate: u32, frames: &[[f32; 2]]) -> Vec<(i64, Vec<u8>)> {
        if rate == 0 {
            return Vec::new();
        }
        self.first_pts.get_or_insert(pts_ns);
        let resample = self.resample.get_or_insert_with(|| Resampler::new(rate, OUT_RATE));
        // the resampler works in f64; the stretch (and the S16 AUs it feeds) stay f32
        self.wide.clear();
        self.wide.extend(frames.iter().map(|f| [f[0] as f64, f[1] as f64]));
        self.resampled.clear();
        resample.run(&self.wide, &mut self.resampled);
        self.scratch.clear();
        self.scratch.extend(self.resampled.iter().map(|f| [f[0] as f32, f[1] as f32]));
        self.stretch.run(&self.scratch, &mut self.pending);
        let mut aus = Vec::new();
        while self.pending.len() >= CHUNK {
            let chunk: Vec<[f32; 2]> = self.pending.drain(..CHUNK).collect();
            aus.push((self.content_pts(self.emitted), to_s16le(&chunk)));
            self.emitted += CHUNK as u64;
        }
        aus
    }

    /// Content time of output frame `n`: each output frame stands for `speed` input frames.
    fn content_pts(&self, n: u64) -> i64 {
        let ns = (n as i128 * self.permille as i128 * 1_000_000) / OUT_RATE as i128;
        self.first_pts.unwrap_or(0) + ns as i64
    }
}

fn to_s16le(frames: &[[f32; 2]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames.len() * 4);
    for f in frames {
        for s in f {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `freq` Hz test tone. A triangle wave rather than a sine: it has the same zero crossings,
    /// which is all [`pitch`] counts, and needs no transcendental (`ci/allow/libm.txt`).
    // AV_CH_* bit positions (FFmpeg's public layout masks; ci/ffabi-assert.c is the ABI half).
    const FL: u64 = 1 << 0;
    const FR: u64 = 1 << 1;
    const FC: u64 = 1 << 2;
    const LFE: u64 = 1 << 3;
    const BL: u64 = 1 << 4;
    const BR: u64 = 1 << 5;
    const BC: u64 = 1 << 8;
    const SL: u64 = 1 << 9;
    const SR: u64 = 1 << 10;

    /// One interleaved float frame of `mask`'s channels, `hot` set to 1.0 and the rest silent,
    /// mixed by role.
    fn mix(mask: u64, hot: u64) -> [f32; 2] {
        let n = mask.count_ones() as usize;
        let frame: Vec<u8> = (0..64)
            .filter(|b| mask >> b & 1 == 1)
            .flat_map(|b| (if hot >> b & 1 == 1 { 1.0f32 } else { 0.0 }).to_ne_bytes())
            .collect();
        downmix_by_mask(SampleFormat::Flt, false, mask, n, 1, &[&frame])[0]
    }

    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    /// DTS core layouts the count cannot tell apart from a 5.1: a quad (FL FR SL SR) sends each
    /// surround to its own side, never the left one to the centre; the LFE is dropped whatever
    /// channels it sits beside; a 3/1 layout's back-centre goes to both sides; mono+LFE keeps the
    /// dialogue centred.
    #[test]
    fn layouts_the_count_cannot_name_are_mixed_by_role() {
        let quad = FL | FR | SL | SR;
        let g = 1.0 / (1.0 + std::f32::consts::FRAC_1_SQRT_2);
        assert!(near(mix(quad, SL), [std::f32::consts::FRAC_1_SQRT_2 * g, 0.0]), "SL: left only: {:?}", mix(quad, SL));
        assert!(near(mix(quad, SR), [0.0, std::f32::consts::FRAC_1_SQRT_2 * g]), "SR: right only");
        assert!(near(mix(FL | FR | LFE | SL | SR, LFE), [0.0, 0.0]), "2/2+LFE: the LFE is dropped");
        let c31 = FL | FR | FC | LFE | BC;
        assert!(near(mix(c31, LFE), [0.0, 0.0]), "3/1+LFE: the LFE is dropped, not sent left");
        let bc = mix(c31, BC);
        assert!(bc[0] > 0.1 && (bc[0] - bc[1]).abs() < 1e-6, "3/1+LFE: back-centre on both sides: {bc:?}");
        assert!(near(mix(FC | LFE, FC | LFE), [1.0, 1.0]), "mono+LFE: dialogue centred, LFE dropped");
    }

    /// 5.1 (back or side surrounds) and 7.1 mix every channel to its own side, the LFE out, and a
    /// full-scale signal on every channel cannot clip.
    #[test]
    fn a_71_goes_to_the_right_sides_and_cannot_clip() {
        let l71 = FL | FR | FC | LFE | BL | BR | SL | SR;
        for left in [SL, BL, SL | BL] {
            let a = mix(l71, left);
            assert!(a[0] > 0.1 && a[1] == 0.0, "left surrounds are left ({left:#x}): {a:?}");
        }
        for right in [SR, BR, SR | BR] {
            let b = mix(l71, right);
            assert!(b[0] == 0.0 && b[1] > 0.1, "right surrounds are right ({right:#x}): {b:?}");
        }
        assert!(near(mix(l71, LFE), [0.0, 0.0]));
        let all = mix(l71, l71 & !LFE);
        assert!(all[0] <= 1.0 + 1e-6 && all[1] <= 1.0 + 1e-6 && all[0] > 0.99, "full scale is exactly full scale: {all:?}");
        let l51 = FL | FR | FC | LFE | SL | SR;
        let frame: Vec<u8> = [0.3f32, -0.2, 0.5, 0.9, 0.1, 0.4].iter().flat_map(|v| v.to_ne_bytes()).collect();
        assert!(
            near(
                downmix_by_mask(SampleFormat::Flt, false, l51, 6, 1, &[&frame])[0],
                downmix(SampleFormat::Flt, false, 6, 1, &[&frame])[0],
            ),
            "the 5.1 the count already got right is unchanged"
        );
    }

    /// **Every channel a TrueHD stream can carry** (FFmpeg's `thd_channel_order`) has a role: the lefts on
    /// the left only, the rights on the right only, the centres on both, the two LFEs nowhere. The 7.1
    /// bed the decoder reports (FL FR FC LFE BL BR SL SR) is mixed at full scale without clipping.
    #[test]
    fn every_channel_a_truehd_stream_can_carry_has_a_role() {
        // AV_CH_* bits not named above: FLC 6, FRC 7, TC 11, TFL 12, TFC 13, TFR 14, WL 31, WR 32, SDL 33,
        // SDR 34, LFE2 35.
        let roles: &[(u32, bool, bool)] = &[
            (0, true, false),  // FL
            (1, false, true),  // FR
            (2, true, true),   // FC
            (3, false, false), // LFE
            (9, true, false),  // SL
            (10, false, true), // SR
            (12, true, false), // TFL
            (14, false, true), // TFR
            (6, true, false),  // FLC
            (7, false, true),  // FRC
            (4, true, false),  // BL
            (5, false, true),  // BR
            (8, true, true),   // BC
            (11, true, true),  // TC
            (33, true, false), // SDL
            (34, false, true), // SDR
            (31, true, false), // WL
            (32, false, true), // WR
            (13, true, true),  // TFC
            (35, false, false), // LFE2
        ];
        let all = roles.iter().fold(0u64, |m, (bit, _, _)| m | 1 << bit);
        for &(bit, left, right) in roles {
            let got = mix(all, 1 << bit);
            assert_eq!((got[0] > 0.0, got[1] > 0.0), (left, right), "AV_CH bit {bit}: {got:?}");
        }
        let bed = FL | FR | FC | LFE | BL | BR | SL | SR;
        let a = mix(bed, bed & !LFE);
        assert!(a[0] > 0.99 && a[0] <= 1.0 + 1e-6 && a[1] > 0.99 && a[1] <= 1.0 + 1e-6, "the 7.1 bed at full scale: {a:?}");
    }

    /// A mask that does not name `channels` channels (unspecified, custom, or wrong) is not trusted.
    #[test]
    fn a_mask_that_does_not_fit_the_channel_count_falls_back_to_the_count() {
        let frame: Vec<u8> = [0.3f32, -0.2, 0.5, 0.9, 0.1, 0.4].iter().flat_map(|v| v.to_ne_bytes()).collect();
        for mask in [0, FL | FR, 0xdead_beef_0000_0000] {
            assert_eq!(
                downmix_by_mask(SampleFormat::Flt, false, mask, 6, 1, &[&frame]),
                downmix(SampleFormat::Flt, false, 6, 1, &[&frame]),
                "mask {mask:#x}"
            );
        }
    }

    /// sin(2 pi t) for `t` turns, from [`sinc`] (no transcendental call: `ci/allow/libm.txt`).
    fn sin_turns(t: f64) -> f64 {
        sinc(2.0 * t) * std::f64::consts::PI * 2.0 * t
    }

    fn cos_turns(t: f64) -> f64 {
        sin_turns(t + 0.25)
    }

    /// A pure `freq` Hz sine of amplitude `amp` on both channels, in f64 and with the phase taken exactly
    /// (`i * freq` is an exact integer in f64, so the fraction of a turn is correctly rounded): an f32 tone
    /// cannot show a -120 dB alias, and a 190 kHz source needs the phase to stay exact.
    fn tone64(freq: f64, rate: u32, secs: f64, amp: f64) -> Vec<[f64; 2]> {
        (0..(rate as f64 * secs).round() as usize)
            .map(|i| {
                let v = sin_turns((i as f64 * freq).rem_euclid(rate as f64) / rate as f64) * amp;
                [v, v]
            })
            .collect()
    }

    /// Resample `input` from `from` to `to` Hz in `block`-sized pieces.
    fn resample(input: &[[f64; 2]], from: u32, to: u32, block: usize) -> Vec<[f64; 2]> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        for b in input.chunks(block) {
            r.run(b, &mut out);
        }
        out
    }

    /// Frames skipped at each end of an output before it is measured: the filter's start-up and its
    /// lookahead are well inside it (a stage reaches at most 41 of its own output frames each side).
    const SETTLE: usize = 400;

    /// The result of fitting `a cos + b sin` (at `freq` Hz) to the left channel of `out`.
    struct Fit {
        /// `sqrt(a^2 + b^2)`.
        amplitude: f64,
        /// RMS of what is left after subtracting the fit, relative to the fit's own RMS.
        residual: f64,
    }

    /// Least-squares fit of one tone to the settled middle of `out` (sampled at `rate`): [`SETTLE`] frames
    /// are dropped from each end and the span is trimmed to a WHOLE number of periods of `freq`, so the
    /// fit neither leaks nor depends on the phase. `freq` is an integral number of Hz.
    fn fit_tone(out: &[[f64; 2]], rate: u32, freq: f64) -> Fit {
        assert_eq!(freq.fract(), 0.0, "a whole number of Hz");
        let period = (rate as u64 / gcd(freq as u64, rate as u64)) as usize;
        let avail = out.len().checked_sub(2 * SETTLE).expect("the output is longer than its settling");
        let n = avail / period * period;
        assert!(n >= 2 * period, "{freq} Hz at {rate} Hz needs two periods ({period} frames) of settled output, has {avail}");
        let from = SETTLE + (avail - n) / 2;
        let at = |i: usize| {
            let t = (i as f64 * freq).rem_euclid(rate as f64) / rate as f64;
            (cos_turns(t), sin_turns(t))
        };
        let (mut cc, mut ss, mut cs, mut xc, mut xs) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (i, f) in out.iter().enumerate().skip(from).take(n) {
            let (c, s) = at(i);
            cc += c * c;
            ss += s * s;
            cs += c * s;
            xc += f[0] * c;
            xs += f[0] * s;
        }
        let det = cc * ss - cs * cs;
        let (a, b) = ((xc * ss - xs * cs) / det, (xs * cc - xc * cs) / det);
        let (mut res, mut sig) = (0.0f64, 0.0f64);
        for (i, f) in out.iter().enumerate().skip(from).take(n) {
            let (c, s) = at(i);
            let fit = a * c + b * s;
            res += (f[0] - fit) * (f[0] - fit);
            sig += fit * fit;
        }
        Fit { amplitude: (a * a + b * b).sqrt(), residual: (res / sig).sqrt() }
    }

    fn db(ratio: f64) -> f64 {
        20.0 * ratio.log10()
    }

    /// Where a tone at `hz` lands in the output of `fout` Hz: folded into `0..=fout/2`.
    fn folded(hz: u64, fout: u64) -> u64 {
        let r = hz % fout;
        r.min(fout - r)
    }

    /// **The pass band is flat to 90% of the output's Nyquist: within 0.01 dB** (it measures under 1e-5: 4.7e-6 from
    /// 96 kHz, 4.8e-6 from 192 kHz, 6.6e-6 from 384 kHz through the relaxed stages) from
    /// 96, 192, 64 and 384 kHz to 48 kHz and from 88.2 and 176.4 kHz to 44.1 kHz, and a tone inside it is
    /// not shifted. The old filter was 0.82 dB down at 21.6 kHz.
    #[test]
    fn the_pass_band_from_a_high_rate_source_is_flat_to_90_percent_of_nyquist() {
        let cases: [(u32, u32, &[f64]); 6] = [
            (96_000, 48_000, &[100.0, 1_000.0, 8_000.0, 16_000.0, 20_000.0, 21_600.0]),
            (192_000, 48_000, &[100.0, 1_000.0, 8_000.0, 16_000.0, 20_000.0, 21_600.0]),
            (64_000, 48_000, &[100.0, 1_000.0, 8_000.0, 16_000.0, 20_000.0, 21_600.0]),
            (384_000, 48_000, &[100.0, 1_000.0, 8_000.0, 16_000.0, 20_000.0, 21_600.0]),
            (88_200, 44_100, &[100.0, 1_000.0, 8_000.0, 16_000.0, 19_800.0]),
            (176_400, 44_100, &[100.0, 1_000.0, 8_000.0, 16_000.0, 19_800.0]),
        ];
        for (from, to, freqs) in cases {
            for &hz in freqs {
                let out = resample(&tone64(hz, from, 0.05, 0.5), from, to, 1000);
                let gain = db(fit_tone(&out, to, hz).amplitude / 0.5);
                assert!(gain.abs() <= 0.01, "{from} -> {to} Hz: {hz} Hz comes out at {gain:.5} dB");
            }
        }
    }

    /// **Everything that can fold into 0..90% of the output's Nyquist is at least 120 dB down.** The named
    /// cases first (27 and 30 kHz from 96 kHz fold to 21 and 18 kHz; 62 kHz from 176.4 kHz to 17.9 kHz;
    /// 60 kHz from 192 kHz to 12 kHz), then a sweep of the whole region from `fout - 0.9 Nyquist` up to the
    /// source's own Nyquist, for the rates that decimate by an integer and one that does not (64 kHz). It measures
    /// -124.5 to -126.8 dB at worst, at the full stage's own stop-band edge (the relaxed stages in front of it
    /// are held to it in `the_relaxed_front_is_flat_and_alias_free_through_the_whole_cascade` and, on their own,
    /// in `a_relaxed_stage_is_flat_to_045_o_and_rejects_everything_that_would_fold_below_055_o`).
    /// The old filter let 26.7 kHz through at -92 dB.
    #[test]
    fn a_tone_that_folds_into_the_pass_band_is_rejected_by_120_db() {
        for (from, to, hz) in [
            (96_000, 48_000, 27_000.0),
            (96_000, 48_000, 30_000.0),
            (176_400, 44_100, 62_000.0),
            (192_000, 48_000, 60_000.0),
        ] {
            let alias = folded(hz as u64, to as u64) as f64;
            let out = resample(&tone64(hz, from, 0.06, 0.5), from, to, 4096);
            let leaked = db(fit_tone(&out, to, alias).amplitude / 0.5);
            assert!(leaked <= -120.0, "{from} -> {to} Hz: a {hz} Hz tone folds to {alias} Hz at {leaked:.1} dB");
        }
        // (from, to, step in Hz, first source frequency): the first is the edge of the stop band and every
        // step keeps the alias a multiple of a few hundred Hz, so its period fits in the settled output.
        for (from, to, step, first) in [
            (96_000u32, 48_000u32, 300u64, 26_400u64),
            (192_000, 48_000, 1_800, 26_400),
            (64_000, 48_000, 300, 26_400),
            (88_200, 44_100, 450, 24_300),
            (176_400, 44_100, 4_500, 24_300),
            (384_000, 48_000, 9_000, 26_400),
        ] {
            let worst = worst_alias(from, to, first, from as u64 / 2, step);
            assert!(worst.0 <= -120.0, "{from} -> {to} Hz: {} Hz folds in at {:.1} dB", worst.1, worst.0);
        }
    }

    /// **Between 90% of the Nyquist and the Nyquist the filter is neither flat nor alias-free** (the SoX `-a`
    /// convention): a tone there is attenuated, by more than the pass band allows and by less than the
    /// stop band, and the further up the more. No 120 dB claim is made, or checked, here. (23.9 kHz stands for
    /// the Nyquist itself: a tone at exactly fs/2 is sampled at its zero crossings, whatever its level.)
    #[test]
    fn the_transition_band_attenuates_more_the_further_up_and_claims_nothing_else() {
        let mut prev = 0.0f64;
        for hz in [22_000.0, 23_000.0, 23_500.0, 23_900.0] {
            let out = resample(&tone64(hz, 96_000, 0.06, 0.5), 96_000, 48_000, 4096);
            let gain = db(fit_tone(&out, 48_000, hz).amplitude / 0.5);
            assert!(gain <= prev + 1e-6, "{hz} Hz comes out at {gain:.4} dB, louder than the tone below it ({prev:.4} dB)");
            if hz >= 23_000.0 {
                assert!(gain < -0.01 && gain > -120.0, "{hz} Hz is in the transition band: {gain:.4} dB");
            }
            prev = gain;
        }
    }

    /// **24-bit quality**: a 1 kHz sine at -1 dBFS comes out with everything that is not that sine at least 135 dB
    /// below it (f64 frames, weights and accumulators: it measures -277 dB, so 200 is asserted too). The fit is over a whole number of
    /// periods of the settled middle.
    #[test]
    fn a_decimated_sine_carries_no_noise_above_minus_135_db() {
        for (from, to) in [(96_000, 48_000), (192_000, 48_000), (88_200, 44_100)] {
            let out = resample(&tone64(1_000.0, from, 0.25, 0.8913), from, to, 4096);
            let fit = fit_tone(&out, to, 1_000.0);
            assert!((fit.amplitude - 0.8913).abs() < 1e-4, "{from} -> {to} Hz: amplitude {}", fit.amplitude);
            let thdn = db(fit.residual);
            assert!(thdn <= -135.0, "{from} -> {to} Hz: THD+N {thdn:.1} dB");
            // the f32 accumulation of the old filter measured -137.9 dB, which the target above would let through; f64 is at -277
            assert!(thdn <= -200.0, "{from} -> {to} Hz: THD+N {thdn:.1} dB is not f64 arithmetic");
        }
    }

    /// **The same output whatever the block size**, bit for bit (the position is an exact rational, the sums
    /// run in one order), the right length, and a constant stays the constant: at the integer ratios, the
    /// ones with a phase table (88.2 kHz: 80 phases; 50 kHz: 24), one cascaded with a tail (64 kHz is
    /// 4:3), one whose denominator is past the table (96001/48000) and the ones with a relaxed front: one relaxed
    /// stage and a FULL 2:1 (192 and 176.4 kHz), two (384 kHz), one before a phased stage 0 (200 kHz is 25:12 after
    /// it; 705.6 kHz two before 147:40).
    #[test]
    fn a_decimated_stream_is_the_same_in_any_block_size_and_the_right_length() {
        for rate in [88_200u32, 96_000, 176_400, 192_000, 64_000, 50_000, 96_001, 384_000, 200_000, 705_600] {
            let input = tone64(1_234.0, rate, 0.1, 0.5);
            let whole = resample(&input, rate, OUT_RATE, input.len());
            for block in [1, 7, 480, 1152, 4096] {
                let got = resample(&input, rate, OUT_RATE, block);
                assert_eq!(got.len(), whole.len(), "{rate} Hz, block {block}: length");
                let worst = got.iter().zip(&whole).map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs())).fold(0.0f64, f64::max);
                assert!(worst <= 1e-12, "{rate} Hz, block {block}: differs by {worst}");
            }
            // the filter holds back its lookahead (at most 41 output frames at a stage); it never makes frames up
            let expect = (input.len() as u64 * OUT_RATE as u64 / rate as u64) as i64;
            let len = whole.len() as i64;
            assert!(len <= expect + 1 && len + 100 >= expect, "{rate} Hz: {len} frames, expected ~{expect}");
            let dc = resample(&vec![[0.25f64, -0.25]; rate as usize / 5], rate, OUT_RATE, 1152);
            let mid = dc[dc.len() / 2];
            assert!((mid[0] - 0.25).abs() <= 1e-12 && (mid[1] + 0.25).abs() <= 1e-12, "{rate} Hz: DC gain: {mid:?}");
        }
    }

    /// **There is no rate ceiling and the converter is bounded**: the filter tables of any source rate fit
    /// [`TABLE_BYTES_BOUND`] (705.6 kHz, 2.8224 MHz and 5.6448 MHz are the DSD-derived multiples of 44.1 kHz, 1 000 003 Hz is
    /// an odd one, `i32::MAX` is the largest FFmpeg can state), and where audio is pushed through the unit gain holds.
    #[test]
    fn any_source_rate_is_converted_within_the_table_bound() {
        for (from, to) in [(705_600u32, 44_100u32), (2_822_400, 44_100), (1_000_003, 48_000), (5_644_800, 44_100), (i32::MAX as u32, 48_000), (1 << 30, 48_000), (1 << 30, 1), (1 << 31, 1)] {
            let r = Resampler::new(from, to);
            assert!(r.table_bytes() <= TABLE_BYTES_BOUND, "{from} -> {to} Hz: {} bytes of tables", r.table_bytes());
            assert!(r.stages.len() <= RELAXED_STAGES + HALVING_STAGES + 1, "{from} -> {to} Hz: {} stages", r.stages.len());
        }
        // an even rate is halved by relaxed stages all the way down to the output's 4x, then the bounded cascade
        assert_eq!(Resampler::new(1 << 30, 1).stages.len(), 29 + 1, "2^30 Hz to 1 Hz: 29 relaxed halvings (to 2) and a FULL 2:1");
        for (from, to) in [(705_600u32, 44_100u32), (2_822_400, 44_100), (1_000_003, 48_000)] {
            let out = resample(&tone64(1_000.0, from, 0.05, 0.5), from, to, 100_000);
            let gain = fit_tone(&out, to, 1_000.0).amplitude / 0.5;
            assert!((gain - 1.0).abs() <= 1e-4, "{from} -> {to} Hz: gain {gain}");
        }
        // 2e9 frames a second is not pushed through it; a short block shows the chain of 14 stages runs and cannot overflow
        // (and a long relaxed front: 2^30 Hz is halved 13 times, to 2^17 Hz, before the 375-phase stage 0)
        for from in [i32::MAX as u32, 1 << 30] {
            let mut out = Vec::new();
            Resampler::new(from, 48_000).run(&vec![[0.5, 0.5]; 4096], &mut out);
            assert!(out.len() <= 1, "{} frames out of 4096 at {from} Hz", out.len());
        }
    }

    /// The documented figures are the real ones: the widest stage is 656 taps by 1024 phases (6.7 MB, only
    /// reached by an inexact ratio just under 8), a FULL 2:1 stage keeps 83 of its 164 taps and a RELAXED one 25 of
    /// its 48 (the others are exact zeros of the sinc), and 96 kHz and 192 kHz are one phase.
    #[test]
    fn the_filter_tables_are_the_size_the_doc_says() {
        assert_eq!(Resampler::new(96_000, 48_000).table_bytes(), 83 * (8 + 2) + 2 * 4);
        assert_eq!(Resampler::new(192_000, 48_000).table_bytes(), (25 + 83) * (8 + 2) + 2 * 2 * 4);
        let widest = Resampler::new(383_999, 48_000).table_bytes();
        let dense = 656 * DECIMATOR_MAX_PHASES * (8 + 2);
        assert!(widest <= TABLE_BYTES_BOUND && widest * 100 >= dense * 99, "{widest} bytes against {dense} dense");
        assert!(Resampler::new(192_000, 48_000).table_bytes() < 10_000);
        assert_eq!(Resampler::new(48_000, 48_000).table_bytes(), 0);
        assert_eq!(Resampler::new(44_100, 48_000).table_bytes(), 0, "raising the rate is the linear path");
    }

    /// A stage's design, by the span it reaches: the two 2:1 designs differ in `half`, and any ratio's FULL one is known.
    const FULL_2_1: (u64, u64, usize) = (2, 1, 82);
    const RELAXED_2_1: (u64, u64, usize) = (2, 1, 24);

    /// The cascade `Resampler::new(from, to)` builds, stage by stage: (input frames, output frames, reach) per stage.
    fn shape(from: u32, to: u32) -> Vec<(u64, u64, usize)> {
        Resampler::new(from, to).stages.iter().map(|s| (s.num, s.den, s.half)).collect()
    }

    /// **Which stages a rate pair builds**: the relaxed halvings from the top (an even `from` of at least 4x the output),
    /// then today's bounded cascade of FULL stages. 96 -> 48 and 88.2 -> 44.1 build no relaxed stage, so those paths are the
    /// ones they always were; an odd rate builds none either, so it cannot be handed a table per halving.
    #[test]
    fn a_source_of_four_times_the_output_or_more_halves_through_relaxed_stages_first() {
        assert_eq!(Kernel::FULL.half(2.0), FULL_2_1.2);
        assert_eq!(Kernel::RELAXED.half(2.0), RELAXED_2_1.2);
        let full_ratio = |num: u64, den: u64| (num, den, Kernel::FULL.half(num as f64 / den as f64));
        let cases: [(u32, u32, Vec<(u64, u64, usize)>); 11] = [
            (96_000, 48_000, vec![FULL_2_1]),
            (88_200, 44_100, vec![FULL_2_1]),
            (64_000, 48_000, vec![full_ratio(4, 3)]),
            (192_000, 48_000, vec![RELAXED_2_1, FULL_2_1]),
            (176_400, 44_100, vec![RELAXED_2_1, FULL_2_1]),
            (384_000, 48_000, vec![RELAXED_2_1, RELAXED_2_1, FULL_2_1]),
            (705_600, 44_100, vec![RELAXED_2_1, RELAXED_2_1, RELAXED_2_1, FULL_2_1]),
            (200_000, 48_000, vec![RELAXED_2_1, full_ratio(25, 12)]),
            (192_001, 48_000, vec![full_ratio(192_001, 48_000)]),
            (1_000_003, 48_000, vec![full_ratio(1_000_003, 192_000), FULL_2_1, FULL_2_1]),
            (2_822_400, 44_100, vec![RELAXED_2_1; 5].into_iter().chain([FULL_2_1]).collect()),
        ];
        for (from, to, want) in cases {
            assert_eq!(shape(from, to), want, "{from} -> {to} Hz");
        }
    }

    /// Multiply-adds a second a channel: every stage's nonzero weights per output (the mean over its phases) times the
    /// stage's output rate, for a source of `from` Hz.
    fn madds_per_second(stages: &[Decimator], from: u32) -> f64 {
        let mut rate = from as f64;
        stages
            .iter()
            .map(|s| {
                rate *= s.den as f64 / s.num as f64;
                s.w.len() as f64 / s.phases as f64 * rate
            })
            .sum()
    }

    /// **The relaxed front halves the cost** of a source four times the output or more. The yardstick is the one stage
    /// that was built for these rates before it (a FULL stage of the whole ratio: 247 nonzero taps at ratio 4, 575 at 8).
    #[test]
    fn a_192_khz_source_costs_at_most_six_tenths_of_the_one_stage_it_replaced() {
        assert_eq!(Resampler::new(192_000, 48_000).stages[0].w.len(), 25, "a relaxed 2:1 stage keeps 25 taps");
        assert_eq!(RELAXED_STAGE_TABLE_BYTES, 2 * Kernel::RELAXED.half(2.0) * (8 + 2) + 2 * 4, "the table bound is for this reach");
        for (from, to, ratio) in [(192_000u32, 48_000u32, 4u64), (176_400, 44_100, 4), (384_000, 48_000, 8)] {
            let single = madds_per_second(&[Decimator::new(ratio, 1, Kernel::FULL)], from);
            let cascade = madds_per_second(&Resampler::new(from, to).stages, from);
            assert!(cascade <= 0.6 * single, "{from} -> {to} Hz: {cascade} multiply-adds a second a channel against {single}");
        }
    }

    /// **A relaxed stage on its own meets the spec derived in [`Kernel::RELAXED`]**, at the worst case R = 4 * O (192 kHz
    /// in, 96 kHz out, O = 48 kHz; the kernel is the same shape at any rate): flat within 0.01 dB to 0.45 * O (21.6 kHz) and
    /// at least 120 dB down over `[R/2 - 0.55 * O, R/2]` (69.6..96 kHz, which would fold into 0..26.4 kHz). The cascade
    /// test above cannot see the 69.6..74.4 kHz end of that: it folds into the full stage's transition band (21.6..26.4 kHz),
    /// where no claim is made. Measured: -140.9 dB at 69.6 kHz, -136.0 dB at worst above it, 2.5e-6 dB off flat.
    #[test]
    fn a_relaxed_stage_is_flat_to_045_o_and_rejects_everything_that_would_fold_below_055_o() {
        let through = |hz: u64| {
            let mut stage = Decimator::new(2, 1, Kernel::RELAXED);
            let mut out = Vec::new();
            stage.run(&tone64(hz as f64, 192_000, 0.05, 0.5), &mut out);
            (out, folded(hz, 96_000))
        };
        let mut hz = 600u64;
        while hz <= 21_600 {
            let (out, _) = through(hz);
            let gain = db(fit_tone(&out, 96_000, hz as f64).amplitude / 0.5);
            assert!(gain.abs() <= 0.01, "{hz} Hz comes out at {gain:.6} dB");
            hz += 600;
        }
        let mut hz = 69_600u64;
        while hz < 96_000 {
            let (out, alias) = through(hz);
            let leaked = db(fit_tone(&out, 96_000, alias as f64).amplitude / 0.5);
            assert!(leaked <= -120.0, "{hz} Hz folds to {alias} Hz at {leaked:.1} dB");
            hz += 300;
        }
    }

    /// The loudest alias of the cascade `from` -> `to` among tones from `lo` up to (not including) `hi` Hz in `step`s that
    /// fold into 0..90% of the output's Nyquist, in dB below the tone, and the tone's frequency. A tone whose alias is in
    /// the last tenth of the Nyquist, or at 0 Hz, is the transition band's (no claim is made there).
    fn worst_alias(from: u32, to: u32, lo: u64, hi: u64, step: u64) -> (f64, u64) {
        let mut worst = (f64::NEG_INFINITY, 0u64);
        let mut hz = lo;
        while hz < hi {
            let alias = folded(hz, to as u64);
            if alias > 0 && alias * 20 <= to as u64 * 9 {
                let out = resample(&tone64(hz as f64, from, 0.03, 0.5), from, to, 4096);
                let leaked = db(fit_tone(&out, to, alias as f64).amplitude / 0.5);
                if leaked > worst.0 {
                    worst = (leaked, hz);
                }
            }
            hz += step;
        }
        worst
    }

    /// **The relaxed stages lose nothing the pass band keeps and let nothing through that the full stage cannot remove**,
    /// measured end to end through the whole cascade. For each source: the pass band flat to 90% of the output's Nyquist,
    /// then every tone from the output's Nyquist to the source's, in steps fine enough to land on the band the relaxed
    /// stage passes and the full one must remove (48..72 kHz of a 192 kHz source) and on the band nearest the relaxed stage's
    /// fold (69.6..96 kHz: it folds into 0..26.4 kHz, below where the full stage rejects), at least 120 dB down.
    #[test]
    fn the_relaxed_front_is_flat_and_alias_free_through_the_whole_cascade() {
        // (from, to, step in Hz: an alias of a few hundred Hz keeps its period inside the settled output)
        for (from, to, step) in [(192_000u32, 48_000u32, 600u64), (176_400, 44_100, 450), (384_000, 48_000, 600), (705_600, 44_100, 450), (200_000, 48_000, 400)] {
            let nyquist = to as u64 / 2;
            let (db_worst, at) = worst_alias(from, to, nyquist + step, from as u64 / 2, step);
            assert!(db_worst <= -120.0, "{from} -> {to} Hz: {at} Hz folds in at {db_worst:.1} dB");
            let mut flat = 0.0f64;
            let mut hz = step;
            while hz * 10 <= nyquist * 9 {
                let out = resample(&tone64(hz as f64, from, 0.05, 0.5), from, to, 1000);
                flat = flat.max(db(fit_tone(&out, to, hz as f64).amplitude / 0.5).abs());
                hz += step;
            }
            assert!(flat <= 0.01, "{from} -> {to} Hz: the pass band is {flat:.6} dB off");
        }
    }

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<[f32; 2]> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| {
                let phase = (i as f32 * freq / rate as f32).fract(); // 0..1, one cycle
                let tri = if phase < 0.5 { 4.0 * phase - 1.0 } else { 3.0 - 4.0 * phase };
                let s = tri * 0.5;
                [s, s]
            })
            .collect()
    }

    /// Upward zero crossings per second of the left channel.
    fn pitch(samples: &[[f32; 2]], rate: u32) -> f32 {
        let ups = samples.windows(2).filter(|w| w[0][0] < 0.0 && w[1][0] >= 0.0).count();
        ups as f32 * rate as f32 / samples.len() as f32
    }

    fn run(permille: i32, input: &[[f32; 2]], rate: u32, block: usize) -> (Vec<(i64, Vec<u8>)>, Vec<[f32; 2]>) {
        let mut sa = SpeedAudio::new(permille);
        let mut aus = Vec::new();
        let mut t = 0i64;
        for b in input.chunks(block) {
            aus.extend(sa.push(t, rate, b));
            t += b.len() as i64 * 1_000_000_000 / rate as i64;
        }
        let pcm = aus
            .iter()
            .flat_map(|(_, b)| b.chunks(4).map(|c| {
                let l = i16::from_le_bytes([c[0], c[1]]) as f32 / 32767.0;
                let r = i16::from_le_bytes([c[2], c[3]]) as f32 / 32767.0;
                [l, r]
            }))
            .collect();
        (aus, pcm)
    }

    /// **The point of the module**: 1.5x and 2x play the material in 1/1.5 and 1/2 of the time,
    /// and a 440 Hz tone is still 440 Hz.
    #[test]
    fn faster_audio_is_shorter_and_keeps_its_pitch() {
        let input = sine(440.0, 48_000, 4.0);
        for permille in [500, 750, 1250, 1500, 1750, 2000] {
            let (_, out) = run(permille, &input, 48_000, 1024);
            let want = input.len() as f32 * 1000.0 / permille as f32;
            let got = out.len() as f32;
            // the stretch holds back up to a window and a search at the end, and AUs are whole
            assert!((got - want).abs() < (SEARCH + 2 * HOP + CHUNK) as f32 * 1.5, "{permille}: {got} frames for {want}");
            let p = pitch(&out, 48_000);
            assert!((p - 440.0).abs() < 8.0, "{permille}: pitch {p} Hz");
        }
    }

    /// Each AU's PTS is the content time its first frame stands for: consecutive AUs advance by
    /// `CHUNK * speed` frames of content, so after the feed divides by the speed they abut exactly.
    #[test]
    fn au_timestamps_advance_in_content_time() {
        let (aus, _) = run(1500, &sine(440.0, 48_000, 2.0), 48_000, 960);
        assert!(aus.len() > 10);
        let step = (CHUNK as i64 * 1500 * 1_000_000) / 48_000;
        for w in aus.windows(2) {
            assert!(((w[1].0 - w[0].0) - step).abs() <= 1, "{} then {}", w[0].0, w[1].0);
        }
        assert_eq!(aus[0].0, 0, "the first AU starts at the first input frame");
        assert!(aus.iter().all(|(_, b)| b.len() == CHUNK * 4));
    }

    /// 44.1 kHz sources are resampled, not mislabelled: the output is still 48 kHz and in tune.
    #[test]
    fn a_44k1_source_comes_out_at_48k_in_tune() {
        let input = sine(440.0, 44_100, 3.0);
        let (_, out) = run(1500, &input, 44_100, 1024);
        let want = input.len() as f32 * 48_000.0 / 44_100.0 / 1.5;
        assert!((out.len() as f32 - want).abs() < 4000.0, "{} for {want}", out.len());
        assert!((pitch(&out, 48_000) - 440.0).abs() < 8.0);
    }

    /// Block boundaries of any size change nothing: the same input gives the same output.
    #[test]
    fn block_size_does_not_change_the_output() {
        let input = sine(330.0, 48_000, 1.5);
        let (_, a) = run(1250, &input, 48_000, 1024);
        let (_, b) = run(1250, &input, 48_000, 173);
        assert_eq!(a.len(), b.len());
        let diff = a.iter().zip(&b).map(|(x, y)| (x[0] - y[0]).abs()).fold(0.0, f32::max);
        assert!(diff < 1e-3, "max sample difference {diff}");
    }

    #[test]
    fn downmix_keeps_stereo_duplicates_mono_and_never_clips_5_1() {
        let s16 = |v: &[i16]| v.iter().flat_map(|x| x.to_ne_bytes()).collect::<Vec<u8>>();
        let st = s16(&[16384, -16384]);
        assert_eq!(downmix(SampleFormat::S16, false, 2, 1, &[&st]), vec![[0.5, -0.5]]);
        let mono = s16(&[16384]);
        assert_eq!(downmix(SampleFormat::S16, false, 1, 1, &[&mono]), vec![[0.5, 0.5]]);
        // 5.1 planar float, every channel at full scale
        let one = 1.0f32.to_ne_bytes().to_vec();
        let planes: Vec<&[u8]> = (0..6).map(|_| one.as_slice()).collect();
        let m = downmix(SampleFormat::Flt, true, 6, 1, &planes);
        assert!(m[0][0] <= 1.0001 && m[0][1] <= 1.0001, "{m:?}");
        assert!(m[0][0] > 0.99, "full scale stays full scale: {m:?}");
    }

    #[test]
    fn every_av_sample_format_maps() {
        for (fmt, want) in [(1, (SampleFormat::S16, false)), (8, (SampleFormat::Flt, true)), (7, (SampleFormat::S32, true))] {
            assert_eq!(SampleFormat::from_av(fmt), Some(want));
        }
        assert_eq!(SampleFormat::from_av(12), None);
    }
}
