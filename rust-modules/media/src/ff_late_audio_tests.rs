//! A Matroska file whose audio track starts seconds after its video, demuxed through the real
//! `ff::demux` and the real `aq` lanes — the OBSERVABLE half of "a film whose audio starts late
//! no longer buffers forever at the start".
//!
//! **The failure under test.** The two lanes are filled from ONE sequential read of the file, and
//! each has a byte cap (the video lane's is `engine::aq_caps().0`, 10 MiB). An MKV whose audio
//! carries a container `Delay` of ~11 s has no audio block for the first 11 s, so more video than
//! the cap precedes the first audio packet. The demuxer fills the video lane, parks in `aq_push`,
//! and never reads the audio packet — while the pipeline, which will not start its clock without
//! audio, drains no video. Neither side can move: a spinner at 0:00 forever.
//!
//! **What is real here and what is not.** Real: the file (written by the `ffmpeg` CLI, then read
//! back with `ffprobe` to prove the fixture has the shape it claims), the HTTP transport (a
//! loopback server honouring `Range`, as the AVIO's `seek_cb` needs), the bundled FFmpeg that
//! `ff.rs` ships (the host build `make check-ffmpeg` stages — see `ff_image_subtitle_tests.rs`),
//! `ff::demux` itself, and `AuQueue` with the production video cap. Not real: the consumer. Nothing
//! pops the video lane, exactly as on the television before the clock starts, and the test pops the
//! AUDIO lane itself. The second stall the fix addresses (`feed_audio_lane` dropping the late AU as
//! stale) lives in the pump, which needs a Starfish sink and a clock, and is not reachable from a
//! file; the PR's `feed_both_lanes` tests cover it.
//!
//! **The fixture.** H.264 from a noise source at 12 Mbit/s: noise is how ~13 MB of video is made
//! to precede the first audio packet in a few seconds of encoding (the real film got there with
//! 9.5 Mbit/s of near-black frames over 11 s; black would need the same bitrate to be forced, and
//! noise forces it without depending on an encoder's rate control). E-AC-3 audio, `-itsoffset`
//! 11.008 s — the delay of the reported film. The control is the same file with the audio at 0.
//!
//! Like `ff_image_subtitle_tests`, it needs the host FFmpeg libraries, so it is `#[ignore]`d in the
//! plain suite. It skips (returns, loudly) when the `ffmpeg` / `ffprobe` command line tools are
//! absent. Run it with:
//! `cd rust-modules && PLX_FFMPEG_DIR=<dir holding libav*-plx.*.dylib> CARGO_INCREMENTAL=0
//! cargo +nightly test --lib -p plx_media ff::late_audio_tests -- --ignored`
//! (`make check-ffmpeg` stages the libraries into `pkg/`, which is the default directory).

use super::*;
use super::image_subtitle_tests::bind_host_ffmpeg;
use std::io::{Read, Write};
use std::os::raw::c_long;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the first audio AU gets to reach the audio lane. Healthy, it takes well under a
/// second (a loopback read of ~20 MB); broken, it never comes, and this is how long the failing
/// run waits before it says so. Long enough that a loaded CI host cannot flake it, short enough
/// that red is seconds, not a hung test binary.
const FIRST_AUDIO_DEADLINE: Duration = Duration::from_secs(10);

/// The delay the reported film carried on its E-AC-3 track, in seconds.
const LATE_AUDIO_DELAY_S: f64 = 11.008;

/// Whether a command line tool runs at all. A missing tool is a skip, never a failure.
fn tool_present(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A scratch directory removed on drop, however the test leaves.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("plx-late-audio-{}-{tag}", std::process::id()));
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

/// Write an MKV: 640x360 H.264 noise at 12 Mbit/s for 14 s, plus 3 s of E-AC-3 whose first packet
/// lands `audio_delay_s` into the file. Returns its path.
fn generate(dir: &Path, audio_delay_s: f64) -> PathBuf {
    let out = dir.join("late_audio.mkv");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        // Grey frames with temporal + uniform noise on every frame: incompressible, so the
        // encoder spends the whole 12 Mbit/s budget on every picture.
        .args(["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=25:d=14,noise=alls=100:allf=t+u"])
        // The container delay: `-itsoffset` shifts the next input's timestamps, so the muxer
        // writes this track's first block at that time — what mkvmerge's `Delay` does.
        .args(["-itsoffset", &format!("{audio_delay_s}")])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3"])
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "25"])
        .args(["-b:v", "12M", "-maxrate", "12M", "-bufsize", "12M"])
        .args(["-c:a", "eac3", "-b:a", "192k"])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write the fixture");
    out
}

/// Read the fixture back: `(first audio packet's time in seconds, video bytes in packets that
/// precede it)`. The test is only meaningful if this says the file has the claimed shape, so the
/// shape is MEASURED from the file rather than trusted from the command line that made it.
fn first_audio_and_video_bytes_before_it(path: &Path) -> (f64, u64) {
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "packet=stream_index,codec_type,pts_time,size"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(probe.status.success(), "ffprobe could not read the fixture");
    let mut packets: Vec<(bool, f64, u64)> = Vec::new(); // (is_audio, pts_s, size)
    for line in String::from_utf8_lossy(&probe.stdout).lines() {
        // csv field order follows the muxer's section order: codec_type,stream_index,pts_time,size
        let f: Vec<&str> = line.trim().split(',').collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(pts), Ok(size)) = (f[2].parse::<f64>(), f[3].parse::<u64>()) else {
            continue;
        };
        packets.push((f[0] == "audio", pts, size));
    }
    let first_audio = packets
        .iter()
        .filter(|p| p.0)
        .map(|p| p.1)
        .fold(f64::INFINITY, f64::min);
    assert!(first_audio.is_finite(), "the fixture has no audio packets");
    let video_before: u64 = packets
        .iter()
        .filter(|p| !p.0 && p.1 < first_audio)
        .map(|p| p.2)
        .sum();
    (first_audio, video_before)
}

/// A loopback HTTP server over one file that honours `Range: bytes=a-[b]` — what the AVIO's
/// `seek_cb` reopens with when libavformat reads the Cues at the end of the file. One thread per
/// connection; everything stops when the value drops.
struct FileServer {
    port: u16,
    stop: std::sync::Arc<AtomicBool>,
}

impl FileServer {
    fn start(path: &Path) -> FileServer {
        let body = std::sync::Arc::new(std::fs::read(path).expect("read the fixture"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set_nonblocking");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let accept_stop = stop.clone();
        std::thread::spawn(move || {
            while !accept_stop.load(Ordering::Acquire) {
                match plx_base::testnet::accept(&listener) {
                    Ok((mut s, _)) => {
                        let (body, stop) = (body.clone(), accept_stop.clone());
                        std::thread::spawn(move || serve(&mut s, &body, &stop));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => break,
                }
            }
        });
        FileServer { port, stop }
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn serve(s: &mut std::net::TcpStream, body: &[u8], stop: &AtomicBool) {
    let _ = s.set_nonblocking(false);
    let mut head = Vec::new();
    let mut tmp = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&tmp[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let n = body.len();
    let range = text.lines().find_map(|l| l.strip_prefix("range: bytes=")).map(|r| {
        let (a, b) = r.trim().split_once('-').unwrap_or((r.trim(), ""));
        let start = a.parse::<usize>().unwrap_or(0).min(n);
        let end = b.parse::<usize>().ok().map_or(n, |e| (e + 1).min(n));
        (start, end.max(start))
    });
    let (start, end) = range.unwrap_or((0, n));
    let header = if range.is_some() {
        format!(
            "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{}/{n}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            end.saturating_sub(1),
            end - start
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n"
        )
    };
    if s.write_all(header.as_bytes()).is_err() {
        return;
    }
    // In slices, so a client that has gone away (the test ended) stops the transfer promptly.
    for chunk in body[start..end].chunks(64 * 1024) {
        if stop.load(Ordering::Acquire) || s.write_all(chunk).is_err() {
            return;
        }
    }
    let _ = s.flush();
}

/// What `ff::demux` left in the lanes by the deadline.
struct Observed {
    /// pts (ns) of the first AU popped from the AUDIO lane, if one arrived in time.
    first_audio_pts_ns: Option<i64>,
    /// Bytes sitting in the video lane at that moment — the demuxer's read-ahead of video.
    video_lane_bytes: i64,
}

/// A running `ff::demux` over the lanes, torn down however the test leaves: the lanes are aborted
/// (which is what `engine::teardown` does first) and the thread is joined with a bound, so a
/// demuxer that will not stop fails the test instead of hanging the binary.
struct DemuxRun {
    video: Box<AuQueue>,
    audio: Box<AuQueue>,
    thread: Option<std::thread::JoinHandle<()>>,
    hs: Box<HttpStream>,
}

impl DemuxRun {
    fn start(port: u16) -> DemuxRun {
        let (video_cap, audio_cap) = crate::player::engine::aq_caps();
        let mut video = crate::aq::aq_new(video_cap as c_long);
        let mut audio = crate::aq::aq_new(audio_cap as c_long);
        let mut hs = plx_net::stream::http_stream_boxed();
        let (vp, ap, hp) = (
            SendPtr(&mut *video as *mut AuQueue),
            SendPtr(&mut *audio as *mut AuQueue),
            SendPtr(&mut *hs as *mut HttpStream),
        );
        let origin = plx_plex::plex::Origin::http("127.0.0.1", port as i32);
        let thread = std::thread::spawn(move || {
            let off = plx_base::task::OffFrame::for_test();
            // `acodec` is the Load payload's audio codec: the demuxer feeds the track that matches.
            demux(&off, origin, "/library/parts/1/file.mkv".into(), "eac3".into(), None, None, vp, ap, hp)
        });
        DemuxRun { video, audio, thread: Some(thread), hs }
    }

    /// Pop the audio lane until its first AU or the deadline.
    fn observe(&mut self, deadline: Duration) -> Observed {
        let until = Instant::now() + deadline;
        let mut first_audio_pts_ns = None;
        while Instant::now() < until {
            let node = crate::aq::aq_pop(&mut *self.audio, std::ptr::null_mut());
            if !node.is_null() {
                let (es, _key, pts, _len, _data) = unsafe { crate::aq::au_fields(node) };
                unsafe { libc::free(node as *mut c_void) };
                assert_eq!(es, 2, "the audio lane carries es=2 AUs");
                first_audio_pts_ns = Some(pts);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Observed {
            first_audio_pts_ns,
            video_lane_bytes: i64::from(crate::aq::aq_bytes(&mut *self.video)),
        }
    }
}

impl Drop for DemuxRun {
    fn drop(&mut self) {
        crate::aq::aq_abort(&mut *self.video);
        crate::aq::aq_abort(&mut *self.audio);
        plx_net::stream::http_shutdown(&mut *self.hs);
        if let Some(t) = self.thread.take() {
            let until = Instant::now() + Duration::from_secs(10);
            while !t.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            } else if !std::thread::panicking() {
                panic!("the demuxer did not stop within 10 s of its lanes being aborted");
            }
        }
    }
}

/// Build the fixture, prove its shape, serve it, demux it, and report what reached the audio lane.
fn demux_fixture(tag: &str, audio_delay_s: f64) -> Option<(Observed, f64, u64)> {
    if !tool_present("ffmpeg") || !tool_present("ffprobe") {
        eprintln!("SKIP {tag}: the ffmpeg / ffprobe command line tools are not installed");
        return None;
    }
    bind_host_ffmpeg();
    super::ABI_OK.store(true, Ordering::Relaxed);
    SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
    SHARED.desired_audio_idx.store(-1, Ordering::Relaxed);
    SHARED.dg_load_v.store(0, Ordering::Relaxed);
    SHARED.side_subs_owner.store(false, Ordering::Relaxed);
    SHARED.demux_failed.store(false, Ordering::Relaxed);
    FEED_AUDIO.store(true, Ordering::Relaxed);

    let scratch = Scratch::new(tag);
    let file = generate(&scratch.0, audio_delay_s);
    let (first_audio_s, video_before) = first_audio_and_video_bytes_before_it(&file);
    eprintln!(
        "{tag}: fixture first audio packet at {first_audio_s:.3} s, {video_before} bytes of video before it"
    );
    let server = FileServer::start(&file);
    let mut run = DemuxRun::start(server.port);
    let seen = run.observe(FIRST_AUDIO_DEADLINE);
    Some((seen, first_audio_s, video_before))
}

/// **The regression.** Audio 11 s late, and ~13 MB of video ahead of it — more than the video
/// lane's 10 MiB cap. The first audio AU must still reach the audio lane: the demuxer must not
/// park on the full video lane in front of it. Before the fix this fails after the deadline with
/// an empty audio lane and a video lane pinned at its cap.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn a_late_audio_track_is_reached_although_the_video_lane_fills_first() {
    let _g = plx_base::testlock::serial();
    let Some((seen, first_audio_s, video_before)) = demux_fixture("late", LATE_AUDIO_DELAY_S) else {
        return;
    };
    let (video_cap, _) = crate::player::engine::aq_caps();

    // The fixture has the claimed shape; without this the test could be red or green for no
    // reason that has anything to do with the demuxer.
    assert!(
        (first_audio_s - LATE_AUDIO_DELAY_S).abs() < 0.1,
        "fixture: the first audio packet must be at ~{LATE_AUDIO_DELAY_S} s, got {first_audio_s}"
    );
    assert!(
        video_before as i64 > video_cap,
        "fixture: {video_before} bytes of video before the audio must exceed the video lane's \
         {video_cap}-byte cap, or this file cannot park the demuxer"
    );

    let pts = seen.first_audio_pts_ns.unwrap_or_else(|| {
        panic!(
            "no audio AU reached the audio lane within {FIRST_AUDIO_DEADLINE:?}: the demuxer is \
             parked behind a full video lane (video lane holds {} bytes, cap {video_cap}) and \
             never read the first audio packet at {first_audio_s:.3} s",
            seen.video_lane_bytes
        )
    });
    let at_ms = pts / 1_000_000;
    assert!(
        (at_ms as f64 - LATE_AUDIO_DELAY_S * 1000.0).abs() < 100.0,
        "the first audio AU is the one at ~{LATE_AUDIO_DELAY_S} s, got {at_ms} ms"
    );
}

/// **The control.** Same fixture shape, audio NOT delayed: the first audio packet is within the
/// first video frames, so it is read long before any lane fills. Must pass with and without the
/// fix — if it fails, the harness (fixture, server, bundled FFmpeg, lane reading) is broken, and
/// the regression test's red would mean nothing.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn an_on_time_audio_track_reaches_the_audio_lane_at_once() {
    let _g = plx_base::testlock::serial();
    let Some((seen, first_audio_s, _)) = demux_fixture("ontime", 0.0) else {
        return;
    };
    assert!(
        first_audio_s < 0.1,
        "fixture: the control's audio must start with the video, got {first_audio_s}"
    );
    let pts = seen.first_audio_pts_ns.unwrap_or_else(|| {
        panic!(
            "no audio AU reached the audio lane within {FIRST_AUDIO_DEADLINE:?} on a file whose \
             audio starts on time — the harness itself is broken (video lane holds {} bytes)",
            seen.video_lane_bytes
        )
    });
    assert!(pts / 1_000_000 < 100, "the control's first audio AU is at ~0 ms, got {} ms", pts / 1_000_000);
}
