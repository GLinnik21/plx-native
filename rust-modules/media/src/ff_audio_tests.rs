//! The audio converter of "Convert audio: on the TV" (`ff_audio.rs`).
//!
//! Two halves. The pure one runs in `make check`: which AC3 a source becomes, the sample queue the
//! encoder is fed from, the frame clock, and which codecs are never converted. The other needs the
//! FFmpeg the app ships, because what it grades is a property of `ci/build-ffmpeg.sh`'s component
//! list (the decoders, libswresample, the `ac3` encoder) and of the bytes those produce: it writes
//! real files with the `ffmpeg` command line tool (TrueHD 5.1, FLAC 7.1 and 96 kHz, Opus 5.1, PCM
//! 7.1 in Matroska), runs every packet of each through an [`AudioConverter`] over the host build of the
//! bundled libraries, and reads the AC3 that comes out frame by frame: sync word, sample rate,
//! bit rate and channel mode from each frame's own header, and a clock that advances exactly one
//! frame per frame. Like the other FFmpeg tests it is `#[ignore]`d in the plain suite and skips,
//! loudly, without the command line tools; `make check-ffmpeg` runs it.

use super::audio::{ac3_pts_ns, fed_natively, Ac3Target, AudioConverter, SampleQueue};
use super::*;

#[test]
fn surround_becomes_5_1_at_640_and_the_rest_stereo_at_256() {
    for (channels, want_channels, want_rate) in [(0, 2, 256_000), (1, 2, 256_000), (2, 2, 256_000), (3, 6, 640_000), (6, 6, 640_000), (8, 6, 640_000)] {
        let t = Ac3Target::for_source(channels, 48_000);
        assert_eq!((t.channels, t.bit_rate), (want_channels, want_rate), "{channels} channels");
    }
    assert_eq!(Ac3Target::for_source(8, 48_000).layout_name(), "5.1");
    assert_eq!(Ac3Target::for_source(2, 48_000).layout_name(), "stereo");
}

#[test]
fn ac3_rates_pass_through_and_the_others_resample() {
    for (source, want) in [(48_000, 48_000), (44_100, 44_100), (32_000, 32_000), (88_200, 44_100), (176_400, 44_100),
        (96_000, 48_000), (192_000, 48_000), (24_000, 48_000), (22_050, 48_000), (0, 48_000)] {
        assert_eq!(Ac3Target::for_source(2, source).sample_rate, want, "{source} Hz");
    }
}

#[test]
fn the_queue_hands_out_whole_frames_in_order() {
    let mut q = SampleQueue::new(2);
    let left: Vec<f32> = (0..1000).map(|i| i as f32).collect();
    let right: Vec<f32> = (0..1000).map(|i| -(i as f32)).collect();
    q.push(&[&left, &right], 1000);
    let (mut a, mut b) = (vec![0.0; 1536], vec![0.0; 1536]);
    assert!(!q.pop_into(1536, &mut [&mut a, &mut b]), "1000 samples are not a frame");
    q.push(&[&left, &right], 1000);
    assert!(q.pop_into(1536, &mut [&mut a, &mut b]));
    assert_eq!((a[0], a[999], a[1000], a[1535]), (0.0, 999.0, 0.0, 535.0));
    assert_eq!((b[999], b[1535]), (-999.0, -535.0));
    assert_eq!(q.len(), 2000 - 1536, "the rest stays queued");
    q.clear();
    assert_eq!(q.len(), 0);
}

#[test]
fn the_frame_clock_is_the_anchor_plus_the_samples_encoded() {
    assert_eq!(ac3_pts_ns(5_000_000_000, 0, 48_000), 5_000_000_000);
    assert_eq!(ac3_pts_ns(5_000_000_000, 1536, 48_000), 5_032_000_000);
    assert_eq!(ac3_pts_ns(0, 1536, 44_100), 34_829_931);
    assert_eq!(ac3_pts_ns(7, 1536, 0), 7, "a rate of 0 cannot place anything");
}

#[test]
fn the_four_codecs_the_pipeline_decodes_are_never_converted() {
    for codec in ["aac", "ac3", "eac3", "dts"] {
        assert!(fed_natively(codec), "{codec}");
    }
    for codec in ["truehd", "mlp", "flac", "alac", "opus", "vorbis", "pcm_s16le", "pcm_s24le", "pcm_bluray"] {
        assert!(!fed_natively(codec), "{codec}");
    }
}

// ---- the bundled FFmpeg --------------------------------------------------------------------------

/// One AC3 frame's header, read with ATSC A/52 §5.3's field widths.
#[derive(Debug, PartialEq, Eq)]
struct Ac3Header {
    sample_rate: u32,
    bit_rate_kbps: u32,
    /// Full-bandwidth channels plus the LFE: 2 for stereo, 6 for 5.1.
    channels: u32,
    frame_bytes: usize,
}

fn parse_ac3(frame: &[u8]) -> Ac3Header {
    assert!(frame.len() >= 8, "an AC3 frame is longer than its header");
    assert_eq!(&frame[..2], &[0x0B, 0x77], "AC3 sync word");
    let fscod = frame[4] >> 6;
    let frmsizecod = (frame[4] & 0x3F) as usize;
    let sample_rate = [48_000, 44_100, 32_000][fscod as usize];
    const KBPS: [u32; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];
    let bit_rate_kbps = KBPS[frmsizecod / 2];
    // Words per frame (A/52 table 5.18), as bytes.
    let frame_bytes = match fscod {
        0 => bit_rate_kbps as usize * 4,
        1 => (bit_rate_kbps as usize * 320 / 147 + (frmsizecod & 1)) * 2,
        _ => bit_rate_kbps as usize * 6,
    };
    // bsid(5) bsmod(3) then acmod(3); the cmixlev/surmixlev/dsurmod fields that may sit before
    // lfeon depend on acmod.
    let bits = u32::from_be_bytes([frame[5], frame[6], frame[7], 0]);
    let acmod = (bits >> 21) & 0x7;
    let mut pos = 11u32;
    if acmod & 1 != 0 && acmod != 1 {
        pos += 2;
    }
    if acmod & 4 != 0 {
        pos += 2;
    }
    if acmod == 2 {
        pos += 2;
    }
    let lfeon = (bits >> (31 - pos)) & 1;
    let full = [2, 1, 2, 3, 3, 4, 4, 5][acmod as usize];
    Ac3Header { sample_rate, bit_rate_kbps, channels: full + lfeon, frame_bytes }
}

#[test]
fn the_ac3_header_reader_reads_a_known_frame() {
    // 48 kHz, 640 kb/s (fscod 0, frmsizecod 36), bsid 8, bsmod 0, then acmod 7 (3/2) with its
    // two 2-bit mix levels, and lfeon 1 in the last bit of the seventh byte.
    let f = [0x0B, 0x77, 0, 0, 0x24, 0x40, 0b1110_0001, 0x00];
    let h = parse_ac3(&f);
    assert_eq!((h.sample_rate, h.bit_rate_kbps, h.channels, h.frame_bytes), (48_000, 640, 6, 2560));
}

/// Whether a command line tool runs at all. A missing tool is a skip, never a failure.
fn tool_present(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Write `seconds` of a sine as `codec` at `rate` with `channels` into a Matroska file.
fn write_fixture(dir: &std::path::Path, codec: &str, rate: u32, channels: u32, seconds: u32) -> std::path::PathBuf {
    let out = dir.join(format!("{codec}-{channels}ch-{rate}.mkv"));
    let status = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("sine=frequency=440:sample_rate={rate}:duration={seconds}")])
        .args(["-ac", &channels.to_string(), "-c:a", codec, "-strict", "-2"])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write the {codec} fixture");
    out
}

/// Run every packet of the file's first audio stream through a converter: the frames, and the
/// converter's own label.
unsafe fn convert_file(path: &std::path::Path) -> (Vec<(i64, Vec<u8>)>, String) {
    let c = CString::new(path.to_str().unwrap()).unwrap();
    let mut fmt = std::ptr::null_mut();
    assert!(avformat_open_input(&mut fmt, c.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
    assert!(avformat_find_stream_info(fmt, std::ptr::null_mut()) >= 0);
    let ai = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, std::ptr::null_mut(), 0);
    assert!(ai >= 0, "the fixture has an audio stream");
    let st = *(*fmt).streams.add(ai as usize);
    let mut conv = AudioConverter::open(stream_codecpar(st), stream_time_base(st)).expect("a converter for the fixture");
    let pkt = av_packet_alloc();
    let mut out = Vec::new();
    while av_read_frame(fmt, pkt) >= 0 {
        if (*pkt).stream_index == ai {
            conv.push(pkt, &mut out);
        }
        av_packet_unref(pkt);
    }
    av_packet_free(&mut { pkt });
    avformat_close_input(&mut fmt);
    let label = conv.label().to_string();
    (out, label)
}

fn assert_ac3_stream(frames: &[(i64, Vec<u8>)], want: Ac3Header, seconds: u32, what: &str) {
    let per_frame_ns = 1536 * 1_000_000_000 / i64::from(want.sample_rate);
    let expected = i64::from(seconds) * 1_000_000_000 / per_frame_ns;
    let n = frames.len() as i64;
    assert!(n >= expected - 2 && n <= expected + 1, "{what}: {n} frames for {seconds} s, want about {expected}");
    for (i, (pts, bytes)) in frames.iter().enumerate() {
        let h = parse_ac3(bytes);
        assert_eq!(h, Ac3Header { frame_bytes: bytes.len(), ..want }, "{what}: frame {i}'s header");
        if i > 0 {
            let step = pts - frames[i - 1].0;
            assert!((step - per_frame_ns).abs() <= 1, "{what}: frame {i} is {step} ns after the one before");
        }
    }
    // Within one Opus pre-skip of 0 (FLAC, TrueHD and PCM start exactly there).
    assert!(frames[0].0.abs() < 10_000_000, "{what}: the first frame is at {} ns, not the start", frames[0].0);
}

#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg command line; run make check-ffmpeg"]
fn every_converted_codec_comes_out_as_the_ac3_its_source_calls_for() {
    if !tool_present("ffmpeg") {
        eprintln!("SKIP: no ffmpeg command line tool to write the fixtures with");
        return;
    }
    let _g = plx_base::testlock::serial();
    let _ffmpeg = super::image_subtitle_tests::bind_host_ffmpeg();
    let dir = std::env::temp_dir().join(format!("plx-audio-convert-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ac3 = |sample_rate, bit_rate_kbps, channels| Ac3Header { sample_rate, bit_rate_kbps, channels, frame_bytes: 0 };
    for (codec, rate, channels, want) in [
        // The CLI's TrueHD encoder stops at 5.1; 7.1 is graded through FLAC and PCM below.
        ("truehd", 48_000, 6, ac3(48_000, 640, 6)),
        ("truehd", 96_000, 2, ac3(48_000, 256, 2)),
        ("flac", 48_000, 8, ac3(48_000, 640, 6)),
        ("flac", 96_000, 2, ac3(48_000, 256, 2)),
        ("flac", 44_100, 6, ac3(44_100, 640, 6)),
        ("alac", 44_100, 2, ac3(44_100, 256, 2)),
        ("libopus", 48_000, 6, ac3(48_000, 640, 6)),
        ("vorbis", 48_000, 2, ac3(48_000, 256, 2)),
        ("pcm_s24le", 48_000, 8, ac3(48_000, 640, 6)),
        ("pcm_s16le", 88_200, 1, ac3(44_100, 256, 2)),
    ] {
        let path = write_fixture(&dir, codec, rate, channels, 3);
        let (frames, label) = unsafe { convert_file(&path) };
        let what = format!("{codec} {channels}ch {rate} Hz ({label})");
        assert_ac3_stream(&frames, want, 3, &what);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "requires the bundled host FFmpeg and the ffmpeg command line; run make check-ffmpeg"]
fn a_reset_reanchors_the_clock_on_the_next_frame() {
    if !tool_present("ffmpeg") {
        eprintln!("SKIP: no ffmpeg command line tool to write the fixture with");
        return;
    }
    let _g = plx_base::testlock::serial();
    let _ffmpeg = super::image_subtitle_tests::bind_host_ffmpeg();
    let dir = std::env::temp_dir().join(format!("plx-audio-reset-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_fixture(&dir, "flac", 48_000, 2, 6);
    unsafe {
        let c = CString::new(path.to_str().unwrap()).unwrap();
        let mut fmt = std::ptr::null_mut();
        assert!(avformat_open_input(&mut fmt, c.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
        assert!(avformat_find_stream_info(fmt, std::ptr::null_mut()) >= 0);
        let st = *(*fmt).streams;
        let mut conv = AudioConverter::open(stream_codecpar(st), stream_time_base(st)).unwrap();
        let pkt = av_packet_alloc();
        let mut out = Vec::new();
        // One second in, then a jump to four seconds: the packets between are read and never
        // given to the converter, which is what a seek looks like from its side (the fixture has
        // no index for `av_seek_frame` to land with).
        while out.len() < 30 && av_read_frame(fmt, pkt) >= 0 {
            conv.push(pkt, &mut out);
            av_packet_unref(pkt);
        }
        conv.reset();
        let tb = stream_time_base(st);
        let mut after = Vec::new();
        while after.is_empty() && av_read_frame(fmt, pkt) >= 0 {
            if av_rescale_q((*pkt).pts, tb, NS_TB) >= 4_000_000_000 {
                conv.push(pkt, &mut after);
            }
            av_packet_unref(pkt);
        }
        av_packet_free(&mut { pkt });
        avformat_close_input(&mut fmt);
        let first = after.first().expect("frames after the seek").0;
        assert!((3_900_000_000..=4_100_000_000).contains(&first), "the first frame after the seek is at {first} ns");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
