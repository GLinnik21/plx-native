//! Lossless audio decoded in the app and fed as PCM: FLAC, ALAC and integer LPCM
//! (`devcaps::APP_DECODED` is the set).
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
    AVCodecContext, AVCodecParameters, AVPacket, OFF_AFRAME_NB_SAMPLES, OFF_FRAME_DATA,
    OFF_FRAME_FORMAT,
};

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

pub struct PcmDecoder {
    ctx: *mut AVCodecContext,
    frame: *mut std::ffi::c_void,
    out: PcmOut,
    buf: Vec<u8>,
}

// Owned by the one demux thread that opened it.
unsafe impl Send for PcmDecoder {}

impl PcmDecoder {
    /// Open the FFmpeg decoder for `par`'s codec, refusing a stream that is not the declared shape
    /// or whose codec is not one of the app-decoded set.
    pub unsafe fn open(par: *const AVCodecParameters, out: PcmOut) -> Result<PcmDecoder, String> {
        let rate = (*par).sample_rate;
        let channels = (*par).ch_layout.nb_channels;
        if rate as u32 != out.rate || channels as u32 != out.channels {
            return Err(format!(
                "the file is {rate}Hz {channels}ch but the Load declared {}Hz {}ch",
                out.rate, out.channels
            ));
        }
        let name = std::ffi::CStr::from_ptr(crate::ff::avcodec_get_name((*par).codec_id)).to_string_lossy();
        if plx_platform::devcaps::app_decoded_by_ffmpeg_name(&name).is_none() {
            return Err(format!("{name} is not an app-decoded codec"));
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
        Ok(PcmDecoder { ctx, frame, out, buf: Vec::new() })
    }

    /// Decode one packet into interleaved little-endian PCM of the declared width. These codecs
    /// have no decoder delay: one packet is one frame, so the packet's pts is the PCM's.
    pub unsafe fn decode(&mut self, pkt: *const AVPacket) -> Result<&[u8], String> {
        self.buf.clear();
        let r = crate::ff::avcodec_send_packet(self.ctx, pkt);
        if r < 0 {
            return Err(format!("avcodec_send_packet failed ({r})"));
        }
        loop {
            let r = crate::ff::avcodec_receive_frame(self.ctx, self.frame);
            if r == crate::ff::AVERROR_EAGAIN || r == crate::ff::AVERROR_EOF {
                break;
            }
            if r < 0 {
                return Err(format!("avcodec_receive_frame failed ({r})"));
            }
            self.append_frame()?;
        }
        Ok(&self.buf)
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

    #[test]
    fn only_the_decoders_integer_formats_are_accepted() {
        assert_eq!(sample_width(AV_SAMPLE_FMT_S16), Some(2));
        assert_eq!(sample_width(AV_SAMPLE_FMT_S32P), Some(4));
        assert_eq!(sample_width(3), None); // FLT
    }
}
