//! Lossless audio through the REAL bundled FFmpeg and `ff_pcm::PcmDecoder`: the decoded bytes of
//! an ALAC, FLAC or LPCM track equal the samples that were encoded, bit for bit, at the width the
//! Load would declare. `make check` has no FFmpeg, so the host tests only pin the arithmetic
//! (`interleave_into`) and the tables; this is the half that proves the decoders the tables name
//! exist in `ci/build-ffmpeg.sh`'s component list and produce the sample format the arithmetic
//! assumes (S16 / S16P / S32 / S32P, 24-bit left-justified in 32).
//!
//! Each case writes a deterministic raw signal, has the `ffmpeg` command line tool encode it into
//! the codec and container under test, and reads the file back through `avformat_open_input` +
//! `av_read_frame` into `PcmDecoder`. Lossless means the expected output is the source itself, so
//! nothing here trusts the decoder to grade itself.
//!
//! `#[ignore]`d in the plain suite (it needs the host FFmpeg and brew's `ffmpeg`); run by
//! `make check-ffmpeg`. Skips, loudly, when the `ffmpeg` tool is absent, except under `CI`.

use super::image_subtitle_tests::bind_host_ffmpeg;
use super::*;
use crate::ff_pcm::{PcmDecoder, PcmOut};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("plx-pcm-decode-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Source depth in bits and the ffmpeg raw format name for it.
#[derive(Clone, Copy)]
struct Raw {
    bits: usize,
    fmt: &'static str,
}

const S16: Raw = Raw { bits: 16, fmt: "s16le" };
const S24: Raw = Raw { bits: 24, fmt: "s24le" };
const S32: Raw = Raw { bits: 32, fmt: "s32le" };

/// Deterministic full-scale-ish samples: every bit of the depth is exercised, the extremes appear,
/// and the channels differ, so a swapped or truncated sample cannot hide.
fn signal(raw: Raw, channels: usize, frames: usize) -> Vec<i32> {
    let mut x: u32 = 0x1234_5678;
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for c in 0..channels {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = (x as i32) >> (32 - raw.bits);
            out.push(match (i, c) {
                (0, 0) => ((1i64 << (raw.bits - 1)) - 1) as i32,
                (0, _) => (-(1i64 << (raw.bits - 1))) as i32,
                _ => v,
            });
        }
    }
    out
}

fn to_raw(raw: Raw, samples: &[i32]) -> Vec<u8> {
    let w = raw.bits / 8;
    samples.iter().flat_map(|s| s.to_le_bytes()[..w].to_vec()).collect()
}

/// What the Load's declared width makes of the source: S16 as is, anything deeper left-justified
/// in 32 bits (the value S32 means).
fn expected(raw: Raw, out_bits: usize, samples: &[i32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&s| {
            if out_bits == 16 {
                (s as i16).to_le_bytes().to_vec()
            } else {
                (s << (32 - raw.bits)).to_le_bytes().to_vec()
            }
        })
        .collect()
}

fn tool_present() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Encode `samples` as `codec` into `file`. `extra` carries per-case flags.
fn encode(dir: &Path, file: &str, raw: Raw, rate: u32, channels: usize, samples: &[i32], codec: &str, extra: &[&str]) -> PathBuf {
    encode_layout(dir, file, raw, rate, channels, None, samples, codec, extra)
}

/// [`encode`] with the input's channel layout named (`5.1(side)`, `2.1`...): the channels of `samples`
/// are then those of FFmpeg's layout of that name, in its order.
fn encode_layout(
    dir: &Path, file: &str, raw: Raw, rate: u32, channels: usize, layout: Option<&str>, samples: &[i32], codec: &str,
    extra: &[&str],
) -> PathBuf {
    let src = dir.join(format!("{file}.raw"));
    std::fs::write(&src, to_raw(raw, samples)).expect("write the source");
    let out = dir.join(file);
    let input_channels = match layout {
        Some(l) => ["-ch_layout".to_string(), l.to_string()],
        None => ["-ac".to_string(), channels.to_string()],
    };
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", raw.fmt, "-ar", &rate.to_string()])
        .args(input_channels)
        .arg("-i")
        .arg(&src)
        .args(["-c:a", codec])
        .args(extra)
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write {file}");
    out
}

/// Open `path`, hand every packet of its first audio stream to a [`PcmDecoder`] declared as `out`,
/// and return everything it produced. `Err` is whatever refused: the open or a decode.
unsafe fn decode_file(path: &Path, out: PcmOut) -> Result<Vec<u8>, String> {
    decode_file_with(path, out, |_| {})
}

/// [`decode_file`], showing `inspect` the opened decoder first.
unsafe fn decode_file_with(path: &Path, out: PcmOut, inspect: impl FnOnce(&PcmDecoder)) -> Result<Vec<u8>, String> {
    decode_blocks_with(path, out, inspect).map(|blocks| blocks.into_iter().flat_map(|b| b.pcm).collect())
}

/// One packet's worth of what a [`PcmDecoder`] returned: the PCM, the content time (ns) it was stamped with, and the
/// pts (ns) of the packet it came from.
struct Block {
    packet_pts: i64,
    stamp: i64,
    pcm: Vec<u8>,
}

/// [`decode_file_with`], keeping each packet's block and stamp apart.
unsafe fn decode_blocks_with(path: &Path, out: PcmOut, inspect: impl FnOnce(&PcmDecoder)) -> Result<Vec<Block>, String> {
    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    let mut fmt = avformat_alloc_context();
    let r = avformat_open_input(&mut fmt, c_path.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut());
    assert!(r >= 0, "avformat_open_input({}) = {r}", path.display());
    assert!(avformat_find_stream_info(fmt, std::ptr::null_mut()) >= 0);
    let streams = (*fmt).streams;
    let ai = (0..(*fmt).nb_streams as usize)
        .find(|&i| (*stream_codecpar(*streams.add(i))).codec_type == 1)
        .expect("an audio stream");
    let result = (|| {
        let mut dec = PcmDecoder::open(stream_codecpar(*streams.add(ai)), out)?;
        inspect(&dec);
        let mut pkt = av_packet_alloc();
        let mut all = Vec::new();
        let mut failure = None;
        while av_read_frame(fmt, pkt) >= 0 {
            if (*pkt).stream_index as usize == ai {
                let packet_pts = pts_ns(pkt, *streams.add(ai));
                match dec.decode(pkt, packet_pts) {
                    Ok((stamp, pcm)) => all.push(Block { packet_pts, stamp, pcm: pcm.to_vec() }),
                    Err(why) => failure = Some(why),
                }
            }
            av_packet_unref(pkt);
            if failure.is_some() {
                break;
            }
        }
        av_packet_free(&mut pkt);
        failure.map_or(Ok(all), Err)
    })();
    avformat_close_input(&mut fmt);
    result
}

struct Case {
    name: &'static str,
    file: &'static str,
    raw: Raw,
    rate: u32,
    channels: usize,
    codec: &'static str,
    extra: &'static [&'static str],
    /// the width the Load declares for this source (`route::pcm_format_of`)
    out_bits: u32,
}

const CASES: &[Case] = &[
    Case { name: "ALAC 16-bit 44.1 kHz stereo", file: "a.m4a", raw: S16, rate: 44_100, channels: 2, codec: "alac", extra: &[], out_bits: 16 },
    Case { name: "ALAC 24-bit 48 kHz stereo", file: "b.m4a", raw: S24, rate: 48_000, channels: 2, codec: "alac", extra: &[], out_bits: 32 },
    Case { name: "ALAC 16-bit mono", file: "c.m4a", raw: S16, rate: 48_000, channels: 1, codec: "alac", extra: &[], out_bits: 16 },
    Case { name: "FLAC 24-bit in MKV (the existing path, unchanged)", file: "d.mkv", raw: S24, rate: 48_000, channels: 2, codec: "flac", extra: &[], out_bits: 32 },
    Case { name: "pcm_s16le in MKV", file: "e.mkv", raw: S16, rate: 44_100, channels: 2, codec: "pcm_s16le", extra: &[], out_bits: 16 },
    Case { name: "pcm_s16be in MKV", file: "f.mkv", raw: S16, rate: 48_000, channels: 2, codec: "pcm_s16be", extra: &[], out_bits: 16 },
    Case { name: "pcm_s24le in MKV", file: "g.mkv", raw: S24, rate: 48_000, channels: 2, codec: "pcm_s24le", extra: &[], out_bits: 32 },
    Case { name: "pcm_s24be in MKV", file: "h.mkv", raw: S24, rate: 44_100, channels: 2, codec: "pcm_s24be", extra: &[], out_bits: 32 },
    Case { name: "pcm_s32le in MKV", file: "i.mkv", raw: S32, rate: 48_000, channels: 2, codec: "pcm_s32le", extra: &[], out_bits: 32 },
    Case { name: "pcm_s32be in MKV", file: "j.mkv", raw: S32, rate: 48_000, channels: 2, codec: "pcm_s32be", extra: &[], out_bits: 32 },
    Case { name: "pcm_s16le in MOV (sowt)", file: "k.mov", raw: S16, rate: 48_000, channels: 2, codec: "pcm_s16le", extra: &[], out_bits: 16 },
    Case { name: "pcm_s24le in MOV (in24)", file: "l.mov", raw: S24, rate: 48_000, channels: 2, codec: "pcm_s24le", extra: &[], out_bits: 32 },
    Case { name: "a 16-bit source declared wide (unstated depth)", file: "m.mkv", raw: S16, rate: 48_000, channels: 2, codec: "pcm_s16le", extra: &[], out_bits: 32 },
];

fn skip() -> bool {
    if tool_present() {
        return false;
    }
    assert!(std::env::var_os("CI").is_none(), "CI must have the ffmpeg command line tool");
    eprintln!("SKIPPED: the ffmpeg command line tool is not installed");
    true
}

#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn every_lossless_codec_decodes_bit_exact_through_the_bundled_ffmpeg() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("exact");
    for c in CASES {
        let samples = signal(c.raw, c.channels, 9_000);
        let file = encode(&scratch.0, c.file, c.raw, c.rate, c.channels, &samples, c.codec, c.extra);
        let out = PcmOut::exact(c.rate, c.out_bits, c.channels as u32);
        let got = unsafe { decode_file(&file, out) }.unwrap_or_else(|why| panic!("{}: {why}", c.name));
        let want = expected(c.raw, c.out_bits as usize, &samples);
        assert_eq!(got.len(), want.len(), "{}: decoded length", c.name);
        let first_diff = got.iter().zip(&want).position(|(a, b)| a != b);
        assert!(first_diff.is_none(), "{}: first differing byte at {:?}", c.name, first_diff);
    }
}

#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn a_file_that_contradicts_the_declared_load_is_refused_not_played_wrong() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("refuse");
    let s24 = signal(S24, 2, 2_000);
    let wide = encode(&scratch.0, "w.m4a", S24, 48_000, 2, &s24, "alac", &[]);
    // rate and channels are checked at the open
    for out in [
        PcmOut::exact(44_100, 32, 2),
        PcmOut::exact(48_000, 32, 1),
    ] {
        let why = unsafe { decode_file(&wide, out) }.expect_err("a mismatched shape");
        assert!(why.contains("Load declared"), "{why}");
    }
    // 24-bit audio declared as 16 would be truncated: refused at the first frame
    let why = unsafe { decode_file(&wide, PcmOut::exact(48_000, 16, 2)) }.expect_err("truncation");
    assert!(why.contains("24") || why.contains("32-bit"), "{why}");
    // a codec outside the app-decoded set never opens a PcmDecoder
    let ac3 = encode(&scratch.0, "x.mkv", S16, 48_000, 2, &signal(S16, 2, 9_000), "ac3", &[]);
    let why = unsafe { decode_file(&ac3, PcmOut::exact(48_000, 16, 2)) }.expect_err("ac3");
    assert!(why.contains("not an app-decoded codec"), "{why}");
}

/// **DTS, the lossy entry**: a 5.1 core goes in, stereo 48 kHz S16 comes out, for a 48 kHz and a
/// 44.1 kHz source alike, over the real `dca` decoder of the bundled component list with
/// `core_only` accepted. A lossy codec is not bit-exact, so the signal is a sine in the front-left
/// only: the output must carry it on the left (the downmix ran), be about as long as the source
/// (the resample kept the duration), and a file declared as anything but stereo 48 kHz is refused.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn dts_decodes_to_stereo_48k_through_the_bundled_ffmpeg() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("dts");
    let stereo = PcmOut::exact(48_000, 16, 2);
    for (file, rate) in [("a.mkv", 48_000u32), ("b.mkv", 44_100)] {
        let frames = rate as usize * 2;
        let mut samples = Vec::with_capacity(frames * 6);
        for i in 0..frames {
            let v = ((i as f64 * 2.0 * std::f64::consts::PI * 440.0 / rate as f64).sin() * 12_000.0) as i32;
            samples.extend_from_slice(&[v, 0, 0, 0, 0, 0]);
        }
        let src = encode(&scratch.0, file, S16, rate, 6, &samples, "dca", &["-strict", "-2", "-b:a", "768k"]);
        let got = unsafe { decode_file_with(&src, stereo, |d| assert!(d.core_only(), "core_only was not accepted")) }
            .unwrap_or_else(|why| panic!("{rate} Hz: {why}"));
        let pcm: Vec<i16> = got.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let frames_out = pcm.len() / 2;
        assert!((95_000..=97_500).contains(&frames_out), "{rate} Hz: 2 s is ~96000 frames at 48 kHz, got {frames_out}");
        let energy = |ch: usize| pcm.iter().skip(ch).step_by(2).map(|&v| (v as f64).powi(2)).sum::<f64>();
        assert!(energy(0) > 100.0 * energy(1).max(1.0), "{rate} Hz: the front-left sine must stay on the left");
    }
    let src = scratch.0.join("a.mkv");
    for wrong in [PcmOut::exact(44_100, 16, 2), PcmOut::exact(48_000, 32, 2), PcmOut::exact(48_000, 16, 6)] {
        let why = unsafe { decode_file(&src, wrong) }.expect_err("DTS is only converted to stereo 48 kHz S16");
        assert!(why.contains("stereo 48 kHz"), "{why}");
    }
}

/// The LPCM flavours the app cannot decode (float, 8-bit) reach the demuxer under PMS's `pcm` and a
/// PCM Load: the real FFmpeg names them `pcm_f32le` / `pcm_s8` / `pcm_u8`, none of which is in
/// `APP_DECODED`, so `PcmDecoder::open` refuses them and the demuxer's `pcm_load_feeds_raw` guard
/// stops them being fed raw (`ff::pcm_load_guard_tests` holds the decision).
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn float_and_8_bit_lpcm_are_not_app_decodable() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("unsupported");
    let samples = signal(S16, 2, 2_000);
    for (file, codec) in [("f32.mkv", "pcm_f32le"), ("f32.mov", "pcm_f32le"), ("s8.mov", "pcm_s8"), ("u8.mov", "pcm_u8"), ("u8.mkv", "pcm_u8")] {
        let src = encode(&scratch.0, file, S16, 48_000, 2, &samples, codec, &[]);
        for bits in [16, 32] {
            let why = unsafe { decode_file(&src, PcmOut::exact(48_000, bits, 2)) }
                .expect_err(&format!("{file}: must not decode"));
            assert!(why.contains("not an app-decoded codec"), "{file}: {why}");
        }
    }
}

/// Output of the `span` audio packets from number `k` of `path`, decoded by a [`PcmDecoder`] declared
/// as `out`. The decoder is [reset](PcmDecoder::reset_stream) before packet `k`, as a seek does, after
/// it had decoded the `history` packets before `k` (0: none). A reset must make the output the same
/// whatever the history was. (Not compared with a decoder that never played: FFmpeg applies a
/// stream's start padding only at the start, so an MP3 decoder's first frame legitimately differs.)
unsafe fn packet_output(path: &Path, out: PcmOut, k: usize, span: usize, history: usize) -> Vec<u8> {
    packet_blocks(path, out, k, span, history).into_iter().flat_map(|b| b.pcm).collect()
}

/// [`packet_output`], keeping each packet's block, its stamp and its pts apart.
unsafe fn packet_blocks(path: &Path, out: PcmOut, k: usize, span: usize, history: usize) -> Vec<Block> {
    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    let mut fmt = avformat_alloc_context();
    assert!(avformat_open_input(&mut fmt, c_path.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
    assert!(avformat_find_stream_info(fmt, std::ptr::null_mut()) >= 0);
    let streams = (*fmt).streams;
    let ai = (0..(*fmt).nb_streams as usize)
        .find(|&i| (*stream_codecpar(*streams.add(i))).codec_type == 1)
        .expect("an audio stream");
    let mut dec = PcmDecoder::open(stream_codecpar(*streams.add(ai)), out).expect("open");
    let mut pkt = av_packet_alloc();
    let mut n = 0;
    let mut result = Vec::new();
    while av_read_frame(fmt, pkt) >= 0 {
        if (*pkt).stream_index as usize == ai {
            if n == k {
                dec.reset_stream();
            }
            if n >= k {
                let packet_pts = pts_ns(pkt, *streams.add(ai));
                let (stamp, pcm) = dec.decode(pkt, packet_pts).expect("decode");
                result.push(Block { packet_pts, stamp, pcm: pcm.to_vec() });
            } else if n + history >= k {
                dec.decode(pkt, pts_ns(pkt, *streams.add(ai))).expect("decode");
            }
            n += 1;
            if n >= k + span {
                av_packet_unref(pkt);
                break;
            }
        }
        av_packet_unref(pkt);
    }
    av_packet_free(&mut pkt);
    avformat_close_input(&mut fmt);
    assert!(n >= k + span, "the file has packets {k}..{}", k + span);
    result
}

/// **A seek leaves no history in the lossy decoder.** The first frame decoded after
/// `reset_stream` equals the same frame decoded by a decoder that never saw what came before it
/// (the DTS synthesis filter bank carries state from one frame to the next, so without
/// `avcodec_flush_buffers` it would not). Run on a 48 kHz and a 44.1 kHz source: the second also
/// proves the resampler's tail is dropped.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn a_reset_leaves_no_history_of_the_stream_before_it() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("dts-reset");
    let stereo = PcmOut::exact(48_000, 16, 2);
    for (file, rate) in [("a.mkv", 48_000u32), ("b.mkv", 44_100)] {
        let frames = rate as usize * 2;
        let mut samples = Vec::with_capacity(frames * 6);
        for i in 0..frames {
            let t = i as f64 / rate as f64;
            let v = ((t * 2.0 * std::f64::consts::PI * 440.0).sin() * 9_000.0
                + (t * 2.0 * std::f64::consts::PI * 3_100.0).sin() * 6_000.0) as i32;
            samples.extend_from_slice(&[v, -v / 2, v / 3, 0, v / 4, -v / 5]);
        }
        let src = encode(&scratch.0, file, S16, rate, 6, &samples, "dca", &["-strict", "-2", "-b:a", "768k"]);
        let fresh = unsafe { packet_output(&src, stereo, 20, 1, 0) };
        let after_seek = unsafe { packet_output(&src, stereo, 20, 1, 20) };
        assert!(!fresh.is_empty() && fresh.iter().any(|&b| b != 0), "{rate} Hz: the packet decodes to sound");
        assert!(fresh == after_seek, "{rate} Hz: the first frame after a reset must not depend on what was decoded before");
    }
}

/// One lossy stereo or mono fixture: `codec` is the host encoder (`libmp3lame`, `vorbis`), `left_only`
/// puts the sine on the left channel alone (so a downmix that crossed the channels shows).
struct LossyCase {
    file: &'static str,
    codec: &'static str,
    extra: &'static [&'static str],
    rate: u32,
    channels: usize,
    left_only: bool,
}

const LOSSY_CASES: &[LossyCase] = &[
    LossyCase { file: "mp3-48k.mkv", codec: "libmp3lame", extra: &["-joint_stereo", "0", "-b:a", "192k"], rate: 48_000, channels: 2, left_only: true },
    LossyCase { file: "mp3-44k.mkv", codec: "libmp3lame", extra: &["-joint_stereo", "0", "-b:a", "192k"], rate: 44_100, channels: 2, left_only: true },
    LossyCase { file: "mp3-32k.mkv", codec: "libmp3lame", extra: &["-joint_stereo", "0", "-b:a", "128k"], rate: 32_000, channels: 2, left_only: true },
    LossyCase { file: "mp3-mono.mkv", codec: "libmp3lame", extra: &["-b:a", "96k"], rate: 44_100, channels: 1, left_only: false },
    LossyCase { file: "vorbis-48k.mkv", codec: "vorbis", extra: &["-strict", "-2", "-b:a", "192k"], rate: 48_000, channels: 2, left_only: true },
    LossyCase { file: "vorbis-44k.mkv", codec: "vorbis", extra: &["-strict", "-2", "-b:a", "192k"], rate: 44_100, channels: 2, left_only: true },
];

fn lossy_source(scratch: &Path, c: &LossyCase) -> PathBuf {
    let frames = c.rate as usize * 2;
    let mut samples = Vec::with_capacity(frames * c.channels);
    for i in 0..frames {
        let t = i as f64 / c.rate as f64;
        let v = ((t * 2.0 * std::f64::consts::PI * 440.0).sin() * 9_000.0
            + (t * 2.0 * std::f64::consts::PI * 3_100.0).sin() * 6_000.0) as i32;
        match (c.channels, c.left_only) {
            (1, _) => samples.push(v),
            (_, true) => samples.extend_from_slice(&[v, 0]),
            _ => samples.extend_from_slice(&[v, v]),
        }
    }
    encode(scratch, c.file, S16, c.rate, c.channels, &samples, c.codec, c.extra)
}

/// **MP3 and Vorbis decode to the one lossy shape** (S4-4) through the real `mp3` and `vorbis`
/// decoders of the bundled component list, at the rates they ship in: stereo 48 kHz S16, about as
/// long as the source, a left-only sine still on the left, a mono source on both sides equally, and a
/// file declared as anything else refused. Neither accepts `core_only` (a DTS option), which is
/// recorded and harmless.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn mp3_and_vorbis_decode_to_stereo_48k_through_the_bundled_ffmpeg() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("lossy-mp3-vorbis");
    let stereo = PcmOut::exact(48_000, 16, 2);
    for c in LOSSY_CASES {
        let src = lossy_source(&scratch.0, c);
        let got = unsafe { decode_file_with(&src, stereo, |d| assert!(!d.core_only(), "{}: core_only is a DTS option", c.file)) }
            .unwrap_or_else(|why| panic!("{}: {why}", c.file));
        let pcm: Vec<i16> = got.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let frames_out = pcm.len() / 2;
        assert!((92_000..=97_500).contains(&frames_out), "{}: 2 s is ~96000 frames at 48 kHz, got {frames_out}", c.file);
        let energy = |ch: usize| pcm.iter().skip(ch).step_by(2).map(|&v| (v as f64).powi(2)).sum::<f64>();
        assert!(energy(0) > 1_000.0, "{}: the sine is audible", c.file);
        if c.left_only {
            assert!(energy(0) > 30.0 * energy(1).max(1.0), "{}: the left sine must stay on the left ({} vs {})", c.file, energy(0), energy(1));
        } else {
            let ratio = energy(0) / energy(1).max(1.0);
            assert!((0.9..1.1).contains(&ratio), "{}: a mono source is centred, got {ratio}", c.file);
        }
        let peak = pcm.iter().map(|&v| (v as i32).abs()).max().unwrap();
        assert!(peak < 32_000, "{}: the downmix did not clip ({peak})", c.file);
    }
    for c in [&LOSSY_CASES[0], &LOSSY_CASES[4]] {
        let src = scratch.0.join(c.file);
        for wrong in [PcmOut::exact(44_100, 16, 2), PcmOut::exact(48_000, 32, 2), PcmOut::exact(48_000, 16, 1)] {
            let why = unsafe { decode_file(&src, wrong) }.expect_err("converted to stereo 48 kHz S16 only");
            assert!(why.contains("stereo 48 kHz"), "{}: {why}", c.file);
        }
    }
}

/// A seek leaves no history in the MP3 (bit reservoir, overlap) or Vorbis (overlap window) decoder:
/// the first eight packets after `reset_stream` are the same whether the decoder had played 7 or 20
/// packets before it.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn a_reset_leaves_no_history_in_mp3_and_vorbis() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("lossy-mp3-vorbis-reset");
    let stereo = PcmOut::exact(48_000, 16, 2);
    for c in [&LOSSY_CASES[1], &LOSSY_CASES[5]] {
        let src = lossy_source(&scratch.0, c);
        let short = unsafe { packet_output(&src, stereo, 20, 8, 7) };
        let after_seek = unsafe { packet_output(&src, stereo, 20, 8, 20) };
        assert!(short.iter().any(|&b| b != 0), "{}: the packet decodes to sound", c.file);
        assert!(short == after_seek, "{}: the first frame after a reset must not depend on what was decoded before", c.file);
    }
}

/// One TrueHD layout the encoder can write and the decoder reports as a native mask. The count alone
/// cannot name these (2.1 and 3.1 look like 3.0 and 4.0, 4.1 like 5.0): only the layout can say where
/// the LFE is, so the downmix must use it. `roles` is `(channel index, expected on L, expected on R)`.
struct TrueHdLayout {
    name: &'static str,
    channels: usize,
    /// (channel, expected left, expected right): `Some(true)` = the sound is there, `Some(false)` = silent.
    hot: &'static [(usize, bool, bool)],
}

const TRUEHD_LAYOUTS: &[TrueHdLayout] = &[
    // 2.1: FL FR LFE
    TrueHdLayout { name: "2.1", channels: 3, hot: &[(0, true, false), (1, false, true), (2, false, false)] },
    // 3.1: FL FR FC LFE
    TrueHdLayout { name: "3.1", channels: 4, hot: &[(0, true, false), (1, false, true), (2, true, true), (3, false, false)] },
    // 5.1(side): FL FR FC LFE SL SR
    TrueHdLayout {
        name: "5.1(side)",
        channels: 6,
        hot: &[(0, true, false), (1, false, true), (2, true, true), (3, false, false), (4, true, false), (5, false, true)],
    },
];

/// **TrueHD decodes to the one lossy shape, mixed by the layout it reports** (S4-5): for each layout the
/// bundled `truehd` decoder outputs, the sound of one channel lands where its role says (a front on its
/// own side, the centre on both, a side surround on its side, the LFE nowhere), at 48, 96 and 192 kHz,
/// stereo 48 kHz S16 of the right duration.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn truehd_decodes_to_stereo_48k_mixed_by_its_layout_through_the_bundled_ffmpeg() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("truehd");
    let stereo = PcmOut::exact(48_000, 16, 2);
    for rate in [48_000u32, 96_000, 192_000] {
        for layout in TRUEHD_LAYOUTS {
            for &(hot, want_left, want_right) in layout.hot {
                let frames = rate as usize;
                let mut samples = vec![0i32; frames * layout.channels];
                for i in 0..frames {
                    let t = i as f64 / rate as f64;
                    samples[i * layout.channels + hot] = ((t * std::f64::consts::TAU * 440.0).sin() * 12_000.0) as i32;
                }
                let file = format!("th-{}-{rate}-{hot}.mkv", layout.name.replace(['(', ')'], ""));
                let src = encode_layout(
                    &scratch.0, &file, S16, rate, layout.channels, Some(layout.name), &samples, "truehd", &["-strict", "-2"],
                );
                let got = unsafe { decode_file(&src, stereo) }
                    .unwrap_or_else(|why| panic!("{} @ {rate}: {why}", layout.name));
                let pcm: Vec<i16> = got.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                let frames_out = pcm.len() / 2;
                assert!((47_000..=48_200).contains(&frames_out), "{} @ {rate}: 1 s is ~48000 frames, got {frames_out}", layout.name);
                let energy = |ch: usize| pcm.iter().skip(ch).step_by(2).map(|&v| (v as f64).powi(2)).sum::<f64>();
                let (l, r) = (energy(0), energy(1));
                let on = |e: f64| e > 1.0e8;
                let off = |e: f64| e < 1.0e5;
                assert!(
                    (if want_left { on(l) } else { off(l) }) && (if want_right { on(r) } else { off(r) }),
                    "{} @ {rate}: channel {hot} should be on (L {want_left}, R {want_right}), energy L {l:.3e} R {r:.3e}",
                    layout.name
                );
            }
        }
    }
}

/// A 96 kHz or 192 kHz TrueHD with ultrasonic content comes out without it folded into the band: a
/// 30.123 kHz tone (60.123 kHz at 192 kHz), through the real decoder and the real resampler, leaves
/// its alias (17.877 kHz, 12.123 kHz) more than 85 dB below the tone's own level, and a 1 kHz tone in a
/// second file passes at its level. Two files, not a sum: the 16-bit rounding of a two-tone SOURCE has
/// intermodulation lines of its own in the band, about 80 dB down, which is the test signal and not the
/// resampler (the same tone alone measures about 105 dB down). The frequencies are not round on
/// purpose, for the same reason (60 kHz at 192 kHz is 5/16 of the rate, a periodic rounding error).
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn high_rate_truehd_does_not_alias_ultrasound_into_the_band() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("truehd-alias");
    let stereo = PcmOut::exact(48_000, 16, 2);
    let amplitude = |left: &[f64], hz: f64| {
        let (from, to) = (2_000, left.len() - 2_000);
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in left.iter().enumerate().take(to).skip(from) {
            let ph = i as f64 * hz / 48_000.0 * std::f64::consts::TAU;
            re += v * ph.cos();
            im += v * ph.sin();
        }
        2.0 * (re * re + im * im).sqrt() / (to - from) as f64
    };
    let decode_tone = |rate: u32, hz: f64, file: &str| -> Vec<f64> {
        let frames = rate as usize / 2;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = ((i as f64 / rate as f64 * std::f64::consts::TAU * hz).sin() * 8_000.0) as i32;
            samples.extend_from_slice(&[v, v]);
        }
        let src = encode(&scratch.0, file, S16, rate, 2, &samples, "truehd", &["-strict", "-2"]);
        let got = unsafe { decode_file(&src, stereo) }.unwrap_or_else(|why| panic!("{file}: {why}"));
        got.chunks(4).map(|b| i16::from_le_bytes([b[0], b[1]]) as f64).collect()
    };
    for (rate, ultrasound_hz, alias_hz) in [(96_000u32, 30_123.0f64, 17_877.0f64), (192_000, 60_123.0, 12_123.0)] {
        let audible = decode_tone(rate, 1_000.0, &format!("audible-{rate}.mkv"));
        let tone = amplitude(&audible, 1_000.0);
        assert!((tone - 8_000.0).abs() < 100.0, "{rate}: the 1 kHz tone passes at 8000, got {tone}");
        let ultra = decode_tone(rate, ultrasound_hz, &format!("ultra-{rate}.mkv"));
        let rejection = 20.0 * (amplitude(&ultra, alias_hz) / 8_000.0).log10();
        assert!(rejection < -85.0, "{rate}: a {ultrasound_hz} Hz tone folds to {alias_hz} Hz at {rejection:.1} dB");
    }
}

/// **The speed decoder mixes by layout too.** A TrueHD 2.1 whose only sound is on the LFE, decoded for a
/// 1.25x session, comes out silent: mixing by the count (three channels read as FL FR FC) would put it in
/// the centre, on both sides.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn the_speed_decoder_drops_the_lfe_of_a_layout_the_count_cannot_name() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("speed-lfe");
    let rate = 48_000u32;
    let mut samples = vec![0i32; rate as usize * 2 * 3];
    for i in 0..rate as usize * 2 {
        samples[i * 3 + 2] = ((i as f64 / rate as f64 * std::f64::consts::TAU * 60.0).sin() * 12_000.0) as i32;
    }
    let src = encode_layout(&scratch.0, "lfe.mkv", S16, rate, 3, Some("2.1"), &samples, "truehd", &["-strict", "-2"]);
    let c_path = CString::new(src.to_str().unwrap()).unwrap();
    unsafe {
        let mut fmt = avformat_alloc_context();
        assert!(avformat_open_input(&mut fmt, c_path.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
        assert!(avformat_find_stream_info(fmt, std::ptr::null_mut()) >= 0);
        let par = stream_codecpar(*(*fmt).streams);
        let mut dec = crate::ff::SpeedDecoder::open(par, 1250).expect("the speed decoder opens truehd");
        let mut pkt = av_packet_alloc();
        let (mut loud, mut total) = (0usize, 0usize);
        let mut t = 0i64;
        while av_read_frame(fmt, pkt) >= 0 {
            for (_, au) in dec.decode(pkt, t) {
                for v in au.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])) {
                    total += 1;
                    loud += (v.unsigned_abs() > 50) as usize;
                }
            }
            t += 1_000_000;
            av_packet_unref(pkt);
        }
        av_packet_free(&mut pkt);
        avformat_close_input(&mut fmt);
        assert!(total > 10_000, "the stream decoded ({total} samples)");
        assert!(loud == 0, "the LFE must not reach either side ({loud} of {total} samples are audible)");
    }
}

// ---------------------------------------------------------------------------------------------
// Hi-res lossless: a stereo FLAC above the PCM input's table is resampled by the app (`PcmOut::source_rate`).
// The signal is not the LCG of the bit-exact cases (noise says nothing about a filter): it is sines with f64
// phases, and the analysis is correlation in f64 over whole periods, so a level or a line 110 dB down can be read.
// ---------------------------------------------------------------------------------------------

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Interleaved stereo `bits`-bit samples of the sum of `tones` (frequency Hz, amplitude as a fraction of full
/// scale), the right channel at half the left's amplitude so a crossed or swapped channel shows. The amplitudes
/// are whole numbers of LSBs (`0.5` and `0.25` of a power of two), so a tone's level is known exactly.
fn tone_signal(bits: usize, rate: u32, frames: usize, tones: &[(u32, f64)]) -> Vec<i32> {
    let full = (1i64 << (bits - 1)) as f64;
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let mut sum = 0.0;
        for &(hz, amp) in tones {
            // the phase reduced exactly (integer modulo), so a long file does not drift
            let cycle = (i as u64 * u64::from(hz) % u64::from(rate)) as f64 / f64::from(rate);
            sum += amp * (cycle * std::f64::consts::TAU).sin();
        }
        let left = (sum * full).round() as i32;
        let right = (sum * full * 0.5).round() as i32;
        out.extend_from_slice(&[left, right]);
    }
    out
}

/// A stretch of `x[margin..len - margin]` that holds a whole number of periods of every tone in `hzs` (sampled at
/// `rate`): `(start, length)`. The correlation below then sees no leakage of one of them into another, which a
/// partial period of a tone 100 dB above the one being read would put at -90 dB.
fn whole_periods(len: usize, margin: usize, rate: u32, hzs: &[u32]) -> (usize, usize) {
    let common = hzs.iter().fold(1usize, |l, &hz| {
        let period = (rate / gcd(rate, hz)) as usize;
        l / gcd(l as u32, period as u32) as usize * period
    });
    let n = (len - 2 * margin) / common * common;
    assert!(n >= 4 * common, "{len} samples hold fewer than 4 common periods of {hzs:?} Hz at {rate} Hz");
    (margin, n)
}

/// The amplitude of the `hz` component of `x[start..start + n]` (sampled at `rate`) by correlation with a sine and a
/// cosine in f64. `(start, n)` is [`whole_periods`].
fn tone_amplitude(x: &[f64], hz: u32, rate: u32, (start, n): (usize, usize)) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x[start..start + n].iter().enumerate() {
        let cycle = (i as u64 * u64::from(hz) % u64::from(rate)) as f64 / f64::from(rate);
        let phase = cycle * std::f64::consts::TAU;
        re += v * phase.cos();
        im += v * phase.sin();
    }
    2.0 * re.hypot(im) / n as f64
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The two channels of interleaved little-endian S32 stereo as f64.
fn s32_stereo(pcm: &[u8]) -> [Vec<f64>; 2] {
    let side = |ch: usize| {
        pcm.chunks_exact(8).map(|f| i32::from_le_bytes([f[4 * ch], f[4 * ch + 1], f[4 * ch + 2], f[4 * ch + 3]]) as f64).collect()
    };
    [side(0), side(1)]
}

/// Stamps of the non-empty blocks advance by exactly the frames before them at `rate`, from the first packet's pts.
fn assert_stamps_follow_the_frames(blocks: &[Block], rate: u32, what: &str) {
    let first = blocks.iter().find(|b| !b.pcm.is_empty()).expect("some block has sound");
    assert_eq!(first.stamp, first.packet_pts, "{what}: the first block is stamped with its packet's pts");
    let mut frames_before = 0i128;
    for b in blocks.iter().filter(|b| !b.pcm.is_empty()) {
        let due = first.stamp + (frames_before * 1_000_000_000 / i128::from(rate)) as i64;
        assert_eq!(b.stamp, due, "{what}: a block after {frames_before} frames is stamped by the frames, not the packet");
        frames_before += (b.pcm.len() / 8) as i128;
    }
}

const HIRES_TONES: &[(u32, f64)] = &[(1_000, 0.5), (30_000, 0.25)];

/// **96 kHz / 24-bit FLAC through the Load's resampled shape {48 kHz, S32, stereo, source 96 kHz}.** The output is
/// half as long (less the filter's lookahead), the 1 kHz tone is at unity (0.01 dB) on both channels (a crossed
/// channel would show: the right is half the left), and the 30 kHz tone, which folds to 18 kHz at 48 kHz, is
/// at least 110 dB below it (the filter is meant for 124). The stamps follow the output frames, not the packets'
/// millisecond-rounded pts, and dropping the decoder (which logs the stream's clipping report) does not panic.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn a_96k_24_bit_flac_is_resampled_to_48k_s32_at_unity_with_the_ultrasound_rejected() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("hires-96k");
    let frames = 96_000;
    let samples = tone_signal(24, 96_000, frames, HIRES_TONES);
    let file = encode(&scratch.0, "hires.mkv", S24, 96_000, 2, &samples, "flac", &[]);
    let out = PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 };

    let blocks = unsafe { decode_blocks_with(&file, out, |_| {}) }.expect("a 96 kHz FLAC decodes as the resampled shape");
    let pcm: Vec<u8> = blocks.iter().flat_map(|b| b.pcm.iter().copied()).collect();
    assert_eq!(pcm.len() % 8, 0, "whole S32 stereo frames");
    let frames_out = pcm.len() / 8;
    assert!(
        frames_out.abs_diff(frames / 2) <= 300,
        "half of {frames} frames less the lookahead (41 output frames), got {frames_out}"
    );
    assert_stamps_follow_the_frames(&blocks, 48_000, "96 kHz");

    // the 30 kHz tone is in the source, at -6 dB of the 1 kHz one: the rejection below is not a missing tone
    let source: Vec<f64> = samples.iter().step_by(2).map(|&v| f64::from(v)).collect();
    let window = whole_periods(source.len(), 2_000, 96_000, &[1_000, 30_000]);
    let in_1k = tone_amplitude(&source, 1_000, 96_000, window);
    let in_30k = tone_amplitude(&source, 30_000, 96_000, window);
    assert!((db(in_30k / in_1k) + 6.02).abs() < 0.01, "the source holds the 30 kHz tone at half the 1 kHz one: {in_1k} {in_30k}");

    let channels = s32_stereo(&pcm);
    let window = whole_periods(channels[0].len(), 1_000, 48_000, &[1_000, 18_000]);
    for (ch, (side, gain)) in channels.iter().zip([1.0, 0.5]).enumerate() {
        let want = 0.5 * gain * 2_147_483_648.0;
        let tone = tone_amplitude(side, 1_000, 48_000, window);
        let level = db(tone / want);
        assert!(level.abs() <= 0.01, "channel {ch}: the 1 kHz tone is {level:+.4} dB from unity (amplitude {tone}, wanted {want})");
        let alias = db(tone_amplitude(side, 18_000, 48_000, window) / tone);
        eprintln!("96 kHz -> 48 kHz channel {ch}: 1 kHz {level:+.6} dB from unity, the 30 kHz tone at {alias:.1} dB of it ({frames_out} frames)");
        assert!(alias <= -110.0, "channel {ch}: the 30 kHz tone folds to 18 kHz at {alias:.1} dB of the 1 kHz tone");
    }
}

/// **A seek (`reset_stream`) leaves no history in the resampled decode.** The blocks after a reset equal those of a
/// decoder that decoded nothing before it, whatever the history was, and the first one is stamped with its packet's
/// pts again (the clock re-anchors), the next by the frames written.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn a_reset_leaves_no_history_in_the_resampled_decode() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("hires-reset");
    let samples = tone_signal(24, 96_000, 96_000, HIRES_TONES);
    let file = encode(&scratch.0, "hires.mkv", S24, 96_000, 2, &samples, "flac", &[]);
    let out = PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 };
    let (k, span) = (4, 3);
    let fresh = unsafe { packet_blocks(&file, out, k, span, 0) };
    let after_seek = unsafe { packet_blocks(&file, out, k, span, k) };
    assert_eq!(fresh.len(), span);
    assert!(fresh.iter().all(|b| b.pcm.iter().any(|&v| v != 0)), "each packet decodes to sound");
    assert!(fresh[0].packet_pts > 0, "the packet is not the first of the stream, so the anchor is a real restart");
    assert!(
        fresh.iter().zip(&after_seek).all(|(a, b)| a.pcm == b.pcm),
        "the first blocks after a reset must not depend on what was decoded before it"
    );
    assert_stamps_follow_the_frames(&fresh, 48_000, "fresh");
    assert_stamps_follow_the_frames(&after_seek, 48_000, "after a reset");
    assert_eq!(
        after_seek.iter().map(|b| b.stamp).collect::<Vec<_>>(),
        fresh.iter().map(|b| b.stamp).collect::<Vec<_>>(),
        "the stamps restart from the packet after the seek"
    );
}

/// **The open refuses a file whose rate is not the Load's `source_rate`.** A wrong rate is wrong pitch (or a
/// filter built for another ratio), not something anyone would see as an error.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn a_file_that_is_not_at_the_loads_source_rate_is_refused() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("hires-refuse");
    let hi = encode(&scratch.0, "hi.mkv", S24, 96_000, 2, &signal(S24, 2, 4_000), "flac", &[]);
    let lo = encode(&scratch.0, "lo.mkv", S24, 48_000, 2, &signal(S24, 2, 4_000), "flac", &[]);
    // the 96 kHz file declared as a 48 kHz one, which is what the Load said before the app resampled
    let why = unsafe { decode_file(&hi, PcmOut::exact(48_000, 32, 2)) }.expect_err("96 kHz declared as 48 kHz");
    assert!(why.contains("Load declared"), "{why}");
    // a 48 kHz file declared as a 96 kHz source
    let why = unsafe { decode_file(&lo, PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 }) }
        .expect_err("48 kHz declared as 96 kHz");
    assert!(why.contains("Load declared"), "{why}");
    // a resampled Load is S32 and mono or stereo: 16 bits would need dither, which this path does not do
    let why = unsafe { decode_file(&hi, PcmOut { rate: 48_000, bits: 16, channels: 2, source_rate: 96_000 }) }
        .expect_err("a resampled S16 Load");
    assert!(why.contains("S32"), "{why}");
}

/// **88.2 kHz / 16-bit FLAC to {44.1 kHz, S32, stereo, source 88.2 kHz}**: a multiple of 44.1 kHz stays in the
/// 44.1 kHz family, a 16-bit source is carried as S32 (the resampled value is not on its grid), and the 1 kHz tone
/// is at unity on both channels.
#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg tool; run make check-ffmpeg"]
fn an_88k_16_bit_flac_is_resampled_to_44k_s32_at_unity() {
    let _g = plx_base::testlock::serial();
    if skip() {
        return;
    }
    let _ffmpeg = bind_host_ffmpeg();
    let scratch = Scratch::new("hires-88k");
    let frames = 88_200;
    let samples = tone_signal(16, 88_200, frames, &[(1_000, 0.5)]);
    let file = encode(&scratch.0, "hires.mkv", S16, 88_200, 2, &samples, "flac", &[]);
    let out = PcmOut { rate: 44_100, bits: 32, channels: 2, source_rate: 88_200 };

    let blocks = unsafe { decode_blocks_with(&file, out, |_| {}) }.expect("an 88.2 kHz FLAC decodes as the resampled shape");
    let pcm: Vec<u8> = blocks.iter().flat_map(|b| b.pcm.iter().copied()).collect();
    assert_eq!(pcm.len() % 8, 0, "whole S32 stereo frames");
    assert!((pcm.len() / 8).abs_diff(frames / 2) <= 300, "half of {frames} frames less the lookahead, got {}", pcm.len() / 8);
    assert_stamps_follow_the_frames(&blocks, 44_100, "88.2 kHz");
    let channels = s32_stereo(&pcm);
    let window = whole_periods(channels[0].len(), 1_000, 44_100, &[1_000]);
    for (ch, (side, gain)) in channels.iter().zip([1.0, 0.5]).enumerate() {
        let want = 0.5 * gain * 2_147_483_648.0;
        let level = db(tone_amplitude(side, 1_000, 44_100, window) / want);
        eprintln!("88.2 kHz -> 44.1 kHz channel {ch}: 1 kHz {level:+.6} dB from unity");
        assert!(level.abs() <= 0.01, "channel {ch}: the 1 kHz tone is {level:+.4} dB from unity");
    }
}
