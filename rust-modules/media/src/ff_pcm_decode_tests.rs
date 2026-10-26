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
    let src = dir.join(format!("{file}.raw"));
    std::fs::write(&src, to_raw(raw, samples)).expect("write the source");
    let out = dir.join(file);
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", raw.fmt, "-ar", &rate.to_string(), "-ac", &channels.to_string()])
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
        let mut pkt = av_packet_alloc();
        let mut all = Vec::new();
        let mut failure = None;
        while av_read_frame(fmt, pkt) >= 0 {
            if (*pkt).stream_index as usize == ai {
                match dec.decode(pkt) {
                    Ok(pcm) => all.extend_from_slice(pcm),
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
        let out = PcmOut { rate: c.rate, bits: c.out_bits, channels: c.channels as u32 };
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
        PcmOut { rate: 44_100, bits: 32, channels: 2 },
        PcmOut { rate: 48_000, bits: 32, channels: 1 },
    ] {
        let why = unsafe { decode_file(&wide, out) }.expect_err("a mismatched shape");
        assert!(why.contains("Load declared"), "{why}");
    }
    // 24-bit audio declared as 16 would be truncated: refused at the first frame
    let why = unsafe { decode_file(&wide, PcmOut { rate: 48_000, bits: 16, channels: 2 }) }.expect_err("truncation");
    assert!(why.contains("24") || why.contains("32-bit"), "{why}");
    // a codec outside the app-decoded set never opens a PcmDecoder
    let ac3 = encode(&scratch.0, "x.mkv", S16, 48_000, 2, &signal(S16, 2, 9_000), "ac3", &[]);
    let why = unsafe { decode_file(&ac3, PcmOut { rate: 48_000, bits: 16, channels: 2 }) }.expect_err("ac3");
    assert!(why.contains("not an app-decoded codec"), "{why}");
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
            let why = unsafe { decode_file(&src, PcmOut { rate: 48_000, bits, channels: 2 }) }
                .expect_err(&format!("{file}: must not decode"));
            assert!(why.contains("not an app-decoded codec"), "{file}: {why}");
        }
    }
}
