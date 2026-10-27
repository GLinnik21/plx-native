//! Audio decoded in the app and fed as PCM: the lossless FLAC, ALAC and integer LPCM, bit-exact,
//! and DTS, MP3, Vorbis and TrueHD, the lossy entries, converted to stereo 48 kHz S16 ([`Stereo48`]) and
//! only on a `Caps::app_decodes_lossy` value (`devcaps::APP_DECODED` is the set).
//!
//! libpf has no FLAC or ALAC input — its Load vocabulary is AAC / AC3 / AC3 PLUS / DTS / MP3 / MPEG
//! / OPUS / VORBIS / WMA / PCM (read from `libpf-1.0` on a webOS 4.4.3 OLED55B8) — but it does take
//! raw PCM through `contents.pcmInfo` (`player::engine::with_pcm_info`). The PCM decoded from these
//! is bit-exact: at 16 bits as S16, and at 24 bits too, carried left-justified in S32 exactly as
//! FFmpeg's decoders produce it. A decode that would need narrowing is refused, never truncated.
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

/// The PCM the Load declared: rate, sample width (16 or 32) and channels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PcmOut {
    pub rate: u32,
    pub bits: u32,
    pub channels: u32,
}

/// The shape the lossy decode outputs, whatever the source: stereo, 48 kHz, 16 bits
/// (`route::PcmFormat::STEREO_48K`).
pub const STEREO_48K: PcmOut = PcmOut { rate: speed_audio::OUT_RATE, bits: 16, channels: 2 };

pub struct PcmDecoder {
    ctx: *mut AVCodecContext,
    frame: *mut std::ffi::c_void,
    out: PcmOut,
    buf: Vec<u8>,
    /// Some for a lossy codec (DTS, MP3, Vorbis, TrueHD): the frame is converted to [`STEREO_48K`] instead of passed
    /// through bit-exact.
    lossy: Option<Stereo48>,
    /// `core_only` took on a lossy decoder (FFmpeg accepted the option). Reported in the log line
    /// and held by a host test; the decode works without it, only costlier.
    core_only: bool,
    errors: ErrorBudget,
}

/// PURE (no FFmpeg): any channel layout and rate to interleaved stereo S16 LE at 48 kHz. The
/// downmix is [`speed_audio::downmix_by_mask`], the resample its [`Resampler`] (linear up to 48 kHz,
/// a windowed-sinc low-pass decimator from above it; the bundled FFmpeg is built without swresample),
/// kept across frames so their edges are seamless.
pub struct Stereo48 {
    rate: u32,
    resampler: Resampler,
}

impl Stereo48 {
    pub fn new() -> Self {
        Stereo48 { rate: speed_audio::OUT_RATE, resampler: Resampler::new(speed_audio::OUT_RATE) }
    }

    /// Forget the resampler's carried tail (a seek).
    pub fn reset(&mut self) {
        self.resampler = Resampler::new(self.rate);
    }

    /// Append `frames` frames (`planes`: one slice per channel when `planar`, else one) to `out`.
    /// `mask`: the layout's native channel mask (0 = not stated, mixed by count).
    /// A frame at a different `rate` than the last starts a new resampler.
    pub fn convert(
        &mut self, rate: u32, fmt: SampleFormat, planar: bool, channels: usize, mask: u64,
        frames: usize, planes: &[&[u8]], out: &mut Vec<u8>,
    ) {
        if rate == 0 || frames == 0 {
            return;
        }
        if rate != self.rate {
            self.rate = rate;
            self.resampler = Resampler::new(rate);
        }
        let stereo = speed_audio::downmix_by_mask(fmt, planar, mask, channels, frames, planes);
        let mut at_48k = Vec::with_capacity(stereo.len() * 48_000 / rate as usize + 2);
        self.resampler.run(&stereo, &mut at_48k);
        out.reserve(at_48k.len() * 4);
        for [l, r] in at_48k {
            for v in [l, r] {
                let s = (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                out.extend_from_slice(&s.to_le_bytes());
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
            if rate as u32 != out.rate || channels as u32 != out.channels {
                return Err(format!(
                    "the file is {rate}Hz {channels}ch but the Load declared {}Hz {}ch",
                    out.rate, out.channels
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
        Ok(PcmDecoder { ctx, frame, out, buf: Vec::new(), lossy: entry.lossy.then(Stereo48::new), core_only, errors: ErrorBudget::default() })
    }

    /// The batcher for this decoder's output: blocks shorter than [`MIN_AU_MS`] are gathered into one AU.
    pub fn au_batcher(&self) -> AuBatcher {
        AuBatcher::new(self.out)
    }

    /// Whether the decoder is a lossy one running its core only (see the field).
    pub fn core_only(&self) -> bool {
        self.core_only
    }

    /// Decode one packet into interleaved little-endian PCM of the declared width. The lossless
    /// codecs have no decoder delay: one packet is one frame, so the packet's pts is the PCM's. (A DTS,
    /// MP3 or TrueHD frame is the same; the resampler holds back one input frame (the decimator, about half a
    /// millisecond of them). Vorbis overlaps
    /// its blocks, so its first packet yields nothing and the rest come out about half a block, tens
    /// of milliseconds, after their packet's pts: not corrected, and not determined whether that
    /// matters.) The pts is content time and stays so: the set renders a PCM Load through a mixer
    /// channel that lacks the lip sync its own decoder gets (`player::engine::pcm_output_lead` is the
    /// table: on LG's `liblxa` stack 400 ms, less the output's own delay on a wireless one, the one
    /// stack that was filmed; 2018's `audiosink_new` the same figures and `lgaudiosink` 400 ms on every
    /// output, both read from the firmware; none on 2016/2017's `adecsink`, Realtek and MStar), and
    /// the correction is NOT made here but at the audio lane's one Feed (`player::engine`,
    /// `PCM_SINK_LEAD_NS`), so every producer of PCM gets it once and none stamps its own. The
    /// app-decoded codecs here are not filmed.
    ///
    /// A LOSSY stream (DTS, MP3, Vorbis, TrueHD) skips a packet or frame that fails, as the speed decoder does, and
    /// ends only after [`MAX_CONSECUTIVE_DECODE_ERRORS`] in a row; a lossless one is bit-exact or
    /// nothing, so its first failure ends it.
    pub unsafe fn decode(&mut self, pkt: *const AVPacket) -> Result<&[u8], String> {
        self.buf.clear();
        match self.decode_packet(pkt) {
            Ok(()) => self.errors.ok(),
            Err(why) if self.lossy.is_some() && self.errors.skip() => {
                crate::player::log(&format!("ff: a packet was skipped — {why}"));
            }
            Err(why) => return Err(why),
        }
        Ok(&self.buf)
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
    /// to frame, which is a click at every seek), the frame buffer emptied, the error count started again and a lossy
    /// decode's resampler tail forgotten.
    pub fn reset_stream(&mut self) {
        unsafe { crate::ff::avcodec_flush_buffers(self.ctx) };
        self.buf.clear();
        self.errors = ErrorBudget::default();
        if let Some(conv) = self.lossy.as_mut() {
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
        if let Some(conv) = self.lossy.as_mut() {
            let rate = *(base.add(OFF_AFRAME_SAMPLE_RATE) as *const c_int);
            if rate <= 0 {
                return Err(format!("decoder reported sample rate {rate}"));
            }
            let channels = (*(base.add(OFF_AFRAME_NB_CHANNELS) as *const c_int)).max(0) as usize;
            let Some((fmt, planar)) = SampleFormat::from_av(format) else {
                return Err(format!("unexpected decoder sample format {format}"));
            };
            // `data[]` holds eight planes: a DTS core has at most six channels, TrueHD's 7.1 bed eight.
            if channels == 0 || channels > 8 {
                return Err(format!("unexpected decoder channel count {channels}"));
            }
            let width = fmt.bytes();
            let planes: Vec<&[u8]> = if planar {
                (0..channels).map(|c| std::slice::from_raw_parts(*data.add(c), nb * width)).collect()
            } else {
                vec![std::slice::from_raw_parts(*data, nb * width * channels)]
            };
            let mask = crate::ff::frame_channel_mask(base);
            conv.convert(rate as u32, fmt, planar, channels, mask, nb, &planes, &mut self.buf);
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
        let mut lossless = AuBatcher::new(PcmOut { rate: 96_000, bits: 32, channels: 2 });
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

    /// **Lossy mode's one conversion**: any layout in, interleaved stereo S16 LE at 48 kHz out.
    /// A planar 5.1 of silence plus a full-scale front-left becomes a stereo frame whose left is
    /// the scaled front-left and whose right is silent, so the downmix really ran.
    #[test]
    fn a_planar_51_is_downmixed_to_interleaved_stereo_s16() {
        let mut conv = Stereo48::new();
        let one = flt(&[1.0, 1.0]);
        let zero = flt(&[0.0, 0.0]);
        let planes: Vec<&[u8]> = vec![&one, &zero, &zero, &zero, &zero, &zero];
        let mut out = Vec::new();
        conv.convert(48_000, crate::speed_audio::SampleFormat::Flt, true, 6, 0, 2, &planes, &mut out);
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
        let mut conv = Stereo48::new();
        let sl_only = [flt(&[0.0]), flt(&[0.0]), flt(&[0.0]), flt(&[0.0])];
        let (fl, fr, sl, sr) = (sl_only[0].clone(), sl_only[1].clone(), flt(&[0.5]), sl_only[3].clone());
        let planes: Vec<&[u8]> = vec![&fl, &fr, &sl, &sr];
        let quad = (1u64 << 0) | (1 << 1) | (1 << 9) | (1 << 10);
        let mut out = Vec::new();
        conv.convert(48_000, crate::speed_audio::SampleFormat::Flt, true, 4, quad, 1, &planes, &mut out);
        let v = s16_le(&out);
        assert!(v[0] > 1_000 && v[1] == 0, "the left surround is on the left only: {v:?}");
    }

    #[test]
    fn a_44100_source_comes_out_at_48000_in_the_same_duration() {
        let mut conv = Stereo48::new();
        let block = flt(&vec![0.25; 4410]);
        let mut out = Vec::new();
        for _ in 0..10 {
            conv.convert(44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 4410, &[&block], &mut out);
        }
        let frames = out.len() / 4;
        assert!((47_990..=48_010).contains(&frames), "1 s of 44.1 kHz is ~48000 frames, got {frames}");
    }

    #[test]
    fn a_rate_change_mid_stream_is_followed_not_played_at_the_old_pitch() {
        let mut conv = Stereo48::new();
        let mut out = Vec::new();
        let a = flt(&vec![0.1; 480]);
        conv.convert(48_000, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 480, &[&a], &mut out);
        assert_eq!(out.len() / 4, 480);
        let b = flt(&vec![0.1; 441]);
        let before = out.len();
        conv.convert(44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 441, &[&b], &mut out);
        let got = (out.len() - before) / 4;
        assert!((470..=482).contains(&got), "10 ms of 44.1 kHz is ~480 frames, got {got}");
    }

    #[test]
    fn samples_past_full_scale_clamp_instead_of_wrapping() {
        let mut conv = Stereo48::new();
        let src = flt(&[2.0, -2.0, 1.0, -1.0]);
        let mut out = Vec::new();
        conv.convert(48_000, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 4, &[&src], &mut out);
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
        let mut conv = Stereo48::new();
        let mut out = Vec::new();
        let a = flt(&vec![1.0; 441]);
        conv.convert(44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 441, &[&a], &mut out);
        conv.reset();
        out.clear();
        let b = flt(&vec![-1.0; 441]);
        conv.convert(44_100, crate::speed_audio::SampleFormat::Flt, false, 1, 0, 441, &[&b], &mut out);
        let first = i16::from_le_bytes([out[0], out[1]]);
        assert_eq!(first, -32_768, "the first frame after a seek is the new position's, not a blend with the old tail");
    }

    #[test]
    fn only_the_decoders_integer_formats_are_accepted() {
        assert_eq!(sample_width(AV_SAMPLE_FMT_S16), Some(2));
        assert_eq!(sample_width(AV_SAMPLE_FMT_S32P), Some(4));
        assert_eq!(sample_width(3), None); // FLT
    }
}
