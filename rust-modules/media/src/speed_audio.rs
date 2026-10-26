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
//! to stereo with [`downmix`], hands it to [`SpeedAudio::push`] with the frame's content PTS, and
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

/// Linear-interpolating resampler to [`OUT_RATE`], stateful across calls so chunk edges are seamless.
pub(crate) struct Resampler {
    from: u32,
    /// Position of the next output frame, in input frames from the start of `last` + the new block.
    pos: f64,
    last: Option<[f32; 2]>,
}

impl Resampler {
    pub(crate) fn new(from: u32) -> Self {
        Resampler { from, pos: 0.0, last: None }
    }

    pub(crate) fn run(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        if self.from == OUT_RATE {
            out.extend_from_slice(input);
            return;
        }
        let step = self.from as f64 / OUT_RATE as f64;
        // index -1 is `last`, so position p reads between frame floor(p) and floor(p)+1 of that view
        let at = |i: isize| -> Option<[f32; 2]> {
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
            let f = (p - i as f64) as f32;
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
        let resample = self.resample.get_or_insert_with(|| Resampler::new(rate));
        self.scratch.clear();
        resample.run(frames, &mut self.scratch);
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
