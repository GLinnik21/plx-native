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
//! **What is asserted.** Not only that the first audio AU arrives: the engine feeds a late
//! track's first AU and the next `AUDIO_LEAD_IN_NS` of audio ahead of a clock that cannot start
//! without them, so the audio lane must receive that much audio SPAN while the video lane stays
//! full and undrained. A third test pins the bound: video past the pre-audio headroom with no audio
//! fails the open instead of parking.
//! A fourth pins the other side of the grant: an ORDINARY interleaved file, whose video lane never
//! fills before its first audio packet, never has its cap raised at all.
//!
//! **The mirror image (issue #600, the tests at the end of the file).** Audio stored AHEAD of the
//! first video block fills the audio lane instead. The audio lane may run past its soft cap while
//! the video lane is starved ([`AUDIO_AHEAD_HEADROOM`], `aq::PushGate`): the same harness pins that
//! the first video AU still arrives for an audio-only PREFIX (`tests/mkv_audio_lead.py`), that a
//! CONSTANT skew (every audio block stored beside the video 48 s earlier, the likelier real layout)
//! primes and then plays on without the demuxer parking, that the ceiling fails the open even when
//! the pipeline has popped some audio, and that an interleaved file never queues audio past its
//! soft cap. The skew test drives the real lanes with a stand-in consumer ([`drive_consumer`]).
//!
//! **The fixture.** H.264 from a noise source at 12 Mbit/s: noise is how ~13 MB of video is made
//! to precede the first audio packet in a few seconds of encoding (the real film got there with
//! 9.5 Mbit/s of near-black frames over 11 s; black would need the same bitrate to be forced, and
//! noise forces it without depending on an encoder's rate control). E-AC-3 audio, `-itsoffset`
//! 11.008 s — the delay of the reported film. The control is the same file with the audio at 0.
//!
//! Like `ff_image_subtitle_tests`, it needs the host FFmpeg libraries, so it is `#[ignore]`d in the
//! plain suite. It skips (returns, loudly) when the `ffmpeg` / `ffprobe` command line tools are
//! absent, except under `CI`, where that is a failure. Run it with:
//! `cd rust-modules && PLX_FFMPEG_DIR=<dir holding libav*-plx.*.dylib> CARGO_INCREMENTAL=0
//! cargo test --lib -p plx_media ff::late_audio_tests -- --ignored`
//! (`make check-ffmpeg` stages the libraries into `pkg/`, which is the default directory).

use super::*;
use super::image_subtitle_tests::bind_host_ffmpeg;
use crate::aq::AuNode;
use std::io::{Read, Write};
use std::os::raw::c_long;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the audio lane gets to deliver the first AU AND the engine's whole lead-in after it
/// ([`AUDIO_LEAD_IN_NS`](crate::player::engine::AUDIO_LEAD_IN_NS)). Healthy, it takes well under a
/// second (a loopback read of ~20 MB); broken, it never comes, and this is how long the failing
/// run waits before it says so. Long enough that a loaded CI host cannot flake it, short enough
/// that red is seconds, not a hung test binary.
const FIRST_AUDIO_DEADLINE: Duration = Duration::from_secs(10);

/// The fixture's video bitrate, bits per second (the `-b:v` of [`generate`]).
const FIXTURE_VIDEO_BPS: i64 = 12_000_000;

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

/// Write an MKV: 640x360 H.264 noise at 12 Mbit/s for `video_s` seconds, plus 3 s of E-AC-3 whose
/// first packet lands `audio_delay_s` into the file. Returns its path.
fn generate(dir: &Path, audio_delay_s: f64, video_s: u32) -> PathBuf {
    let out = dir.join("late_audio.mkv");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        // Grey frames with temporal + uniform noise on every frame: incompressible, so the
        // encoder spends the whole 12 Mbit/s budget on every picture.
        .args(["-f", "lavfi", "-i", &format!("color=c=gray:s=640x360:r=25:d={video_s},noise=alls=100:allf=t+u")])
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

/// Write an ORDINARY interleaved MKV: 640x360 H.264 noise at 1 Mbit/s for 4 s, plus 2 s of E-AC-3
/// whose first packet lands 0.2 s in — a handful of video packets (25 fps: five) precede the first
/// audio packet, as in any MP4 with B-frames or a muxer that leads with video. The whole file is a
/// few hundred KB, far under the video lane's base cap, and the audio is shorter than the engine's
/// lead-in so a grant taken at the first video push would still be standing at the end.
fn generate_ordinary(dir: &Path) -> PathBuf {
    let out = dir.join("ordinary.mkv");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=25:d=4,noise=alls=100:allf=t+u"])
        .args(["-itsoffset", "0.2"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2"])
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "25"])
        .args(["-b:v", "1M", "-maxrate", "1M", "-bufsize", "1M"])
        .args(["-c:a", "eac3", "-b:a", "192k"])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write the ordinary fixture");
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
    /// pts (ns) of the last AU popped from the AUDIO lane by the deadline; minus the first, the
    /// span of audio the demuxer managed to push.
    last_audio_pts_ns: Option<i64>,
    /// How many audio AUs were popped.
    audio_aus: usize,
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

    /// Whether the demux thread has returned (end of file, a failure, or a stop).
    fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// The video lane, for a probe that watches it from another thread.
    fn video_lane(&mut self) -> *mut AuQueue {
        &mut *self.video as *mut AuQueue
    }

    /// The audio lane, likewise.
    fn audio_lane(&mut self) -> *mut AuQueue {
        &mut *self.audio as *mut AuQueue
    }

    /// Pop the audio lane until it has delivered `want_span_ns` of audio (last AU's pts minus the
    /// first's) or the deadline passes. **The video lane is never popped**, exactly as on the
    /// television before the clock starts: the engine feeds video only up to `MAX_FEED_AHEAD_NS`
    /// past a presented position that cannot move until audio has been fed.
    fn observe(&mut self, deadline: Duration, want_span_ns: i64) -> Observed {
        let until = Instant::now() + deadline;
        let mut first_audio_pts_ns = None;
        let mut last_audio_pts_ns = None;
        let mut audio_aus = 0;
        while Instant::now() < until {
            let node = crate::aq::aq_pop(&mut *self.audio, std::ptr::null_mut());
            if !node.is_null() {
                let (es, _key, pts, _len, _data) = unsafe { crate::aq::au_fields(node) };
                unsafe { libc::free(node as *mut c_void) };
                assert_eq!(es, 2, "the audio lane carries es=2 AUs");
                first_audio_pts_ns.get_or_insert(pts);
                last_audio_pts_ns = Some(pts);
                audio_aus += 1;
                if pts - first_audio_pts_ns.unwrap_or(pts) >= want_span_ns {
                    break;
                }
                continue;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Observed {
            first_audio_pts_ns,
            last_audio_pts_ns,
            audio_aus,
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

/// A fixture on disk, served, with a demuxer running over it. Field order is drop order: the
/// demuxer is stopped before its server and its files go.
struct Prepared {
    run: DemuxRun,
    first_audio_s: f64,
    video_before: u64,
    _server: FileServer,
    _scratch: Scratch,
    _ffmpeg: super::image_subtitle_tests::HostFfmpeg,
}

/// Build the fixture, prove its shape, serve it and start demuxing it. `None` when the tools are
/// missing (a skip).
fn prepare(tag: &str, audio_delay_s: f64, video_s: u32) -> Option<Prepared> {
    prepare_with(tag, |dir| generate(dir, audio_delay_s, video_s))
}

/// [`prepare`] over a fixture `make` writes into the scratch directory.
fn prepare_with(tag: &str, make: impl FnOnce(&Path) -> PathBuf) -> Option<Prepared> {
    if !tool_present("ffmpeg") || !tool_present("ffprobe") {
        // A skip is for a developer machine; CI installs the tools, and a gate that skips there
        // is a gate that is not running (`CI` is set by GitHub Actions).
        assert!(std::env::var_os("CI").is_none(), "{tag}: ffmpeg / ffprobe are missing under CI");
        eprintln!("SKIP {tag}: the ffmpeg / ffprobe command line tools are not installed");
        return None;
    }
    let ffmpeg = bind_host_ffmpeg();
    SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
    SHARED.desired_audio_idx.store(-1, Ordering::Relaxed);
    SHARED.dg_load_v.store(0, Ordering::Relaxed);
    SHARED.side_subs_owner.store(false, Ordering::Relaxed);
    SHARED.demux_failed.store(false, Ordering::Relaxed);
    SHARED.demux_io_failed.store(false, Ordering::Relaxed);
    FEED_AUDIO.store(true, Ordering::Relaxed);

    let scratch = Scratch::new(tag);
    let file = make(&scratch.0);
    let (first_audio_s, video_before) = first_audio_and_video_bytes_before_it(&file);
    eprintln!(
        "{tag}: fixture first audio packet at {first_audio_s:.3} s, {video_before} bytes of video before it"
    );
    let server = FileServer::start(&file);
    let run = DemuxRun::start(server.port);
    Some(Prepared { run, first_audio_s, video_before, _server: server, _scratch: scratch, _ffmpeg: ffmpeg })
}

/// Watches one lane's cap and queued bytes from another thread while the demuxer runs.
///
/// A demux of a four second fixture ends in milliseconds, so the test thread can see it finished
/// before the OS has scheduled this thread at all (a loaded CI runner did, twice: "0 bytes").
/// The thread therefore reads the stop flag BEFORE each sample and stops only after a sample that
/// followed it, so one sample is always taken after the demuxer has ended. The lanes the tests
/// watch are never popped, so that last sample is also the peak.
struct LaneProbe {
    stop: std::sync::Arc<AtomicBool>,
    thread: std::thread::JoinHandle<(i64, i64)>,
}

impl LaneProbe {
    fn start(lane: *mut AuQueue) -> LaneProbe {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let lane = lane as usize;
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let (mut max_cap, mut max_queued) = (0i64, 0i64);
                loop {
                    let last = stop.load(Ordering::Acquire);
                    max_cap = max_cap.max(i64::from(crate::aq::aq_cap(lane as *mut AuQueue)));
                    max_queued = max_queued.max(i64::from(crate::aq::aq_bytes(lane as *mut AuQueue)));
                    if last {
                        return (max_cap, max_queued);
                    }
                    std::thread::yield_now();
                }
            })
        };
        LaneProbe { stop, thread }
    }

    /// Stop the probe; `(largest cap seen, most bytes queued seen)`.
    fn finish(self) -> (i64, i64) {
        self.stop.store(true, Ordering::Release);
        self.thread.join().expect("lane probe")
    }
}

/// The probe samples at least once however soon it is finished (no ffmpeg needed).
#[test]
fn a_lane_probe_finished_at_once_still_samples_the_lane() {
    let mut q = crate::aq::aq_new(4096);
    let au = [0u8; 100];
    assert_eq!(crate::aq::aq_push(&mut *q, au.as_ptr(), au.len() as std::os::raw::c_int, 0, 1, 1), 0);
    for round in 0..200 {
        let probe = LaneProbe::start(&mut *q);
        assert_eq!(probe.finish(), (4096, 100), "round {round}");
    }
}

/// [`prepare`], then report what reached the audio lane within the deadline.
fn demux_fixture(tag: &str, audio_delay_s: f64) -> Option<(Observed, f64, u64)> {
    let mut p = prepare(tag, audio_delay_s, 14)?;
    let seen = p.run.observe(FIRST_AUDIO_DEADLINE, crate::player::engine::AUDIO_LEAD_IN_NS);
    Some((seen, p.first_audio_s, p.video_before))
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
    // Above the base cap by MORE than the pump could have popped: on the television the pump feeds
    // video up to MAX_FEED_AHEAD_NS (1.6 s) before the clock, which at this fixture's rate is
    // ~2.4 MB leaving the lane. The fixture must park the demuxer even after that much has gone.
    let throttle_pop = FIXTURE_VIDEO_BPS / 8 * crate::player::engine::MAX_FEED_AHEAD_NS / 1_000_000_000;
    assert!(
        video_before as i64 > video_cap + throttle_pop,
        "fixture: {video_before} bytes of video before the audio must exceed the video lane's \
         {video_cap}-byte cap plus the {throttle_pop} bytes the pump's feed-ahead throttle could \
         pop, or this file cannot park the demuxer"
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
    assert_lead_in_reached(&seen, pts, video_cap);
}

/// **The first audio AU is not enough: the engine feeds a late track's first AU and the next
/// [`AUDIO_LEAD_IN_NS`](crate::player::engine::AUDIO_LEAD_IN_NS) of audio ahead of a clock that
/// cannot start without them.** So the demuxer must keep reading, and pushing both lanes, until
/// that much audio is on the audio lane — with the video lane, already past its base cap, still
/// not drained by anyone. Releasing the headroom the instant the first audio AU lands drops the
/// video cap BELOW what the lane holds; the next video push parks and the audio lane starves after
/// one or two packets (device log: `feed a#1`, `feed a#2`, nothing more, no clock, no position).
fn assert_lead_in_reached(seen: &Observed, first_pts: i64, video_cap: i64) {
    let last = seen.last_audio_pts_ns.unwrap_or(first_pts);
    let span = last - first_pts;
    assert!(
        span >= crate::player::engine::AUDIO_LEAD_IN_NS,
        "only {} audio AU(s) spanning {} ms reached the audio lane within {FIRST_AUDIO_DEADLINE:?}; \
         the engine needs {} ms of lead-in before its clock can start. The demuxer is parked behind \
         the video lane (holds {} bytes, base cap {video_cap}) and cannot read the rest of the audio",
        seen.audio_aus,
        span / 1_000_000,
        crate::player::engine::AUDIO_LEAD_IN_NS / 1_000_000,
        seen.video_lane_bytes
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
    // The same bounded wait for the lead-in: an on-time track has all of it long before any lane fills.
    assert_lead_in_reached(&seen, pts, crate::player::engine::aq_caps().0);
}

/// **The bound.** Audio so late that the video ahead of it (~36 MB at 12 Mbit/s) outruns even the
/// pre-audio headroom (the base cap plus `PRE_AUDIO_VIDEO_HEADROOM`, 34 MiB). Nothing pops the video
/// lane and no audio will ever be read, so parking would be a spinner nobody can leave: the demuxer
/// must fail the open (`demux_io_failed`, which the pump turns into a route recovery or the failure
/// read-out) instead, with the audio lane still empty.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn a_video_lane_filled_to_the_headroom_with_no_audio_fails_the_open() {
    let _g = plx_base::testlock::serial();
    // 30 s of delay at 12 Mbit/s is ~45 MB of video before the first audio packet; 32 s of video
    // so the file does not end before the bound is reached.
    let Some(mut p) = prepare("exhausted", 30.0, 32) else {
        return;
    };
    let cap = crate::player::engine::aq_caps().0 + PRE_AUDIO_VIDEO_HEADROOM as i64;
    assert!(
        p.video_before as i64 > cap,
        "fixture: {} bytes of video before the audio must exceed the {cap}-byte bound",
        p.video_before
    );
    let until = Instant::now() + FIRST_AUDIO_DEADLINE;
    while !SHARED.demux_io_failed.load(Ordering::Acquire) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let seen = p.run.observe(Duration::from_millis(50), i64::MAX);
    assert!(
        SHARED.demux_io_failed.load(Ordering::Acquire) && SHARED.demux_failed.load(Ordering::Acquire),
        "the demuxer must report the exhausted headroom within {FIRST_AUDIO_DEADLINE:?} (video lane holds {} bytes, \
         cap {cap}), not park on the full video lane for good",
        seen.video_lane_bytes
    );
    assert_eq!(seen.audio_aus, 0, "no audio was ever read");
    assert!(
        seen.video_lane_bytes <= cap + 1024 * 1024 && seen.video_lane_bytes > crate::player::engine::aq_caps().0,
        "the lane filled past its base cap into the headroom: {} bytes",
        seen.video_lane_bytes
    );
}

/// **F2: an ordinary file never sees the headroom.** Video leads the first audio packet by five
/// packets (any MP4 with B-frames, many MKVs), the whole file is a few hundred KB, and nothing ever
/// parks the video lane. The pre-audio headroom exists for the file whose video lane FILLS before
/// its first audio packet; granting it at the first video push of every open charged every such
/// file 24 MiB of cap (18-30 MiB of real queue on a 60-100 Mbit/s UHD stream) on a RAM-tight
/// 32-bit set. The lane's cap must never leave its base, sampled continuously while the demuxer
/// runs to the end of the file.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn an_ordinary_file_never_raises_the_video_lane_cap() {
    let _g = plx_base::testlock::serial();
    let Some(mut p) = prepare_with("ordinary", generate_ordinary) else {
        return;
    };
    let base = crate::player::engine::aq_caps().0;
    assert!(
        p.first_audio_s > 0.15 && p.first_audio_s < 0.3,
        "fixture: the first audio packet must be at ~0.2 s, got {}",
        p.first_audio_s
    );
    assert!(
        p.video_before > 0 && (p.video_before as i64) < base / 4,
        "fixture: video must lead the audio, by far less than the base cap, got {} bytes",
        p.video_before
    );

    // Watch the cap from outside while the demuxer runs.
    let probe = LaneProbe::start(p.run.video_lane());
    let until = Instant::now() + FIRST_AUDIO_DEADLINE;
    while !p.run.finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let (max_cap, _) = probe.finish();

    assert!(p.run.finished(), "the demuxer must reach the end of a four second file");
    assert!(!SHARED.demux_failed.load(Ordering::Acquire), "the open must not fail");
    let held = i64::from(crate::aq::aq_bytes(&mut *p.run.video));
    assert!(held > 0 && held < base, "the whole file sits in the video lane: {held} bytes");
    assert_eq!(
        max_cap, base,
        "a file whose video never fills the lane must keep the lane's base cap throughout \
         (a grant was taken although no push ever parked)"
    );
}

// ---------------------------------------------------------------------------------------------
// The mirror image: audio stored AHEAD of the video (issue #600).
// ---------------------------------------------------------------------------------------------
//
// A muxer that leads with audio (issue #600's film: ~48 s of E-AC-3, 4.6 MB, before the first
// video block, whose timestamp is still 0) fills the AUDIO lane (`engine::aq_caps().1`, 1 MiB,
// ~10.9 s of 768 kbit/s audio) before the first video packet is read. The demuxer parked in
// `aq_push` on the full audio lane, and nothing drains audio before video has started: a start
// that buffered forever with `feed a#1..4` in the log and no `ff: AU#0`. The fixture here is a
// normally interleaved file whose audio blocks below a lead are re-stored in front of the first
// video block by `tests/mkv_audio_lead.py` (timestamps untouched), so the layout is the muxer's.

/// How far ahead of the video the audio is stored in the first fixture: 20 s of 640 kbit/s E-AC-3
/// is ~1.6 MB, over the audio lane's 1 MiB base cap and far under the headroom.
const EARLY_AUDIO_LEAD_S: f64 = 20.0;

/// E-AC-3 bitrate of the early-audio fixtures, bits per second (the most the encoder will write).
const EARLY_AUDIO_BPS: i64 = 640_000;

/// Write an interleaved MKV (`audio_s` seconds of E-AC-3, 4 s of low-rate H.264 noise) and re-store
/// every audio block with a timestamp below `lead_s` ahead of the first video block with
/// `tests/mkv_audio_lead.py`. Asserts the resulting layout: at least `min_audio_bytes` of audio
/// precede the first video packet (file order) and that packet is still at pts 0.
fn generate_early(dir: &Path, audio_s: u32, lead_s: f64, min_audio_bytes: u64) -> PathBuf {
    let interleaved = dir.join("interleaved.mkv");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=25:d=4,noise=alls=100:allf=t+u"])
        .args(["-f", "lavfi", "-i", &format!("sine=frequency=440:sample_rate=48000:duration={audio_s}")])
        .args(["-map", "0:v", "-map", "1:a"])
        // B-frames on (ultrafast turns them off): decode order differs from pts order, which is what
        // `assert_video_order_kept` needs to see a fixture that re-sorts a track's blocks.
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-bf", "2", "-pix_fmt", "yuv420p", "-g", "25"])
        .args(["-b:v", "1M", "-maxrate", "1M", "-bufsize", "1M"])
        .args(["-c:a", "eac3", "-b:a", &format!("{}k", EARLY_AUDIO_BPS / 1000)])
        .arg(&interleaved)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write the interleaved fixture");
    let out = dir.join("early_audio.mkv");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/mkv_audio_lead.py");
    let status = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg(&interleaved)
        .arg(&out)
        .arg(format!("{lead_s}"))
        .stdout(Stdio::null())
        .status()
        .expect("run python3");
    assert!(status.success(), "tests/mkv_audio_lead.py could not reorder the fixture");
    assert_video_order_kept(&interleaved, &out);
    let (audio_before, first_video_s) = audio_bytes_before_first_video(&out);
    eprintln!("fixture: {audio_before} bytes of audio precede the first video packet (pts {first_video_s:.3} s)");
    assert!(first_video_s.abs() < 0.1, "fixture: the first video packet must still be at pts 0, got {first_video_s}");
    assert!(
        audio_before >= min_audio_bytes,
        "fixture: {audio_before} bytes of audio must precede the first video packet, wanted at least {min_audio_bytes}"
    );
    out
}

/// The video packets' pts (as ffprobe prints them) in FILE order. A reordering fixture may change
/// the interleave of audio against video but never a track's own order: video with B-frames is
/// stored in decode order, and a file that sorts it by pts does not decode (#600).
fn video_pts_in_file_order(path: &Path) -> Vec<String> {
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v", "-show_entries", "packet=pts", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(probe.status.success(), "ffprobe could not list the video packets of {}", path.display());
    String::from_utf8_lossy(&probe.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Assert `out` keeps the video packet sequence of `src` exactly, in order.
fn assert_video_order_kept(src: &Path, out: &Path) {
    let (a, b) = (video_pts_in_file_order(src), video_pts_in_file_order(out));
    assert!(!a.is_empty(), "the source fixture has no video packets");
    assert_eq!(a, b, "tests/mkv_audio_lead.py must keep the video packets in their original (decode) order");
}

/// Read the fixture back in FILE order: `(bytes of audio packets before the first video packet,
/// that video packet's pts in seconds)`.
fn audio_bytes_before_first_video(path: &Path) -> (u64, f64) {
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "packet=stream_index,codec_type,pts_time,size"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(probe.status.success(), "ffprobe could not read the early-audio fixture");
    let mut audio = 0u64;
    for line in String::from_utf8_lossy(&probe.stdout).lines() {
        let f: Vec<&str> = line.trim().split(',').collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(pts), Ok(size)) = (f[2].parse::<f64>(), f[3].parse::<u64>()) else {
            continue;
        };
        if f[0] == "audio" {
            audio += size;
        } else {
            return (audio, pts);
        }
    }
    panic!("the early-audio fixture has no video packets");
}

/// Wait until the video lane holds an AU, then pop it: `(pts_ns, key)`. `None` at the deadline.
fn first_video_au(run: &mut DemuxRun, deadline: Duration) -> Option<(i64, i32)> {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        if crate::aq::aq_bytes(&mut *run.video) > 0 {
            let node = crate::aq::aq_pop(&mut *run.video, std::ptr::null_mut());
            if !node.is_null() {
                let (es, key, pts, _len, _data) = unsafe { crate::aq::au_fields(node) };
                unsafe { libc::free(node as *mut c_void) };
                assert_eq!(es, 1, "the video lane carries es=1 AUs");
                return Some((pts, key));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// **Issue #600.** ~1.6 MB of audio is stored before the first video block, over the audio lane's
/// base cap, and nothing drains audio. The first video AU (pts 0, a keyframe) must still reach the
/// video lane: the demuxer must not park on the full audio lane in front of it.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn an_early_audio_track_does_not_park_the_demuxer_before_the_first_video_au() {
    let _g = plx_base::testlock::serial();
    let (_, audio_cap) = crate::player::engine::aq_caps();
    let Some(mut p) = prepare_with("early", |d| {
        generate_early(d, 30, EARLY_AUDIO_LEAD_S, audio_cap as u64 + 256 * 1024)
    }) else {
        return;
    };
    let au = first_video_au(&mut p.run, FIRST_AUDIO_DEADLINE);
    let (pts, key) = au.unwrap_or_else(|| {
        panic!(
            "no video AU reached the video lane within {FIRST_AUDIO_DEADLINE:?}: the demuxer is parked \
             behind a full audio lane (holds {} bytes, base cap {audio_cap}) and never read the first \
             video packet",
            crate::aq::aq_bytes(&mut *p.run.audio)
        )
    });
    assert_eq!(pts, 0, "the first video AU is the one at pts 0");
    assert_eq!(key, 1, "the first video AU is a keyframe");
    assert!(!SHARED.demux_failed.load(Ordering::Acquire), "the open must not fail");
}

/// **The bound.** Audio so far ahead that it outruns even the audio headroom: no video will ever be
/// read and the pipeline has accepted only a few audio AUs, so the demuxer must fail the open (`demux_io_failed`, the
/// same way the late-audio path does) instead of parking, with the video lane still empty.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn an_audio_lane_filled_to_the_headroom_with_no_video_fails_the_open() {
    let _g = plx_base::testlock::serial();
    let cap = crate::player::engine::aq_caps().1 + AUDIO_AHEAD_HEADROOM as i64;
    // Audio for the whole headroom and then some, at the encoder's top rate: 640 kbit/s is 80 KB/s.
    let lead_s = (cap / (EARLY_AUDIO_BPS / 8)) as f64 + 20.0;
    let Some(mut p) = prepare_with("early-exhausted", |d| {
        generate_early(d, lead_s as u32 + 5, lead_s, cap as u64 + 1024 * 1024)
    }) else {
        return;
    };
    // The engine pops the audio lane BEFORE any video exists (`feed_audio_lane` feeds audio until the
    // pipeline answers BufferFull; device log `feed a#1 ... qbytes=568320`). Do what it does: the
    // pipeline accepts `AUDIO_ACCEPTED_AUS` AUs, then stops. A "spent, so the consumer is draining"
    // rule reads those pops as a healthy consumer and parks untimed, which is the original hang.
    // The pops must land WHILE the lane is filling (the demuxer pushes 17 MiB in tens of
    // milliseconds), so wait for the lane to pass its base cap and then pop at once, a pop per
    // 50 us, rather than after the demuxer has already reached the end of the headroom.
    let base_cap = crate::player::engine::aq_caps().1 as i64;
    let wait = Instant::now() + FIRST_AUDIO_DEADLINE;
    while i64::from(crate::aq::aq_bytes(&mut *p.run.audio)) <= base_cap && Instant::now() < wait {
        std::hint::spin_loop();
    }
    for _ in 0..AUDIO_ACCEPTED_AUS {
        let node = crate::aq::aq_pop(&mut *p.run.audio, std::ptr::null_mut());
        assert!(!node.is_null(), "the lane holds more than its base cap");
        unsafe { libc::free(node as *mut c_void) };
        std::thread::sleep(Duration::from_micros(50));
    }
    let until = Instant::now() + FIRST_AUDIO_DEADLINE;
    while !SHARED.demux_io_failed.load(Ordering::Acquire) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let held = i64::from(crate::aq::aq_bytes(&mut *p.run.audio));
    assert!(
        SHARED.demux_io_failed.load(Ordering::Acquire) && SHARED.demux_failed.load(Ordering::Acquire),
        "the demuxer must report the exhausted headroom within {FIRST_AUDIO_DEADLINE:?} (audio lane holds {held} \
         bytes, cap {cap}), not park on the full audio lane for good"
    );
    assert_eq!(crate::aq::aq_bytes(&mut *p.run.video), 0, "no video was ever read");
    assert!(
        held <= cap + 1024 * 1024 && held > crate::player::engine::aq_caps().1,
        "the lane filled past its base cap into the headroom: {held} bytes"
    );
}

/// **An ordinary file never sees the audio headroom.** Audio leads the first video packet by
/// 0.3 s (a few KB, as in many muxers), far under the audio lane's base cap: the lane must never
/// queue more than that soft cap, and its cap never moves, while the demuxer runs to the end of
/// the file.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI — see the module doc"]
fn an_interleaved_file_never_queues_audio_past_the_soft_cap() {
    let _g = plx_base::testlock::serial();
    let Some(mut p) = prepare_with("early-ordinary", |d| generate_early(d, 4, 0.3, 1024)) else {
        return;
    };
    let base = crate::player::engine::aq_caps().1;
    let probe = LaneProbe::start(p.run.audio_lane());
    let until = Instant::now() + FIRST_AUDIO_DEADLINE;
    while !p.run.finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let (max_cap, queued) = probe.finish();

    assert!(p.run.finished(), "the demuxer must reach the end of a four second file");
    assert!(!SHARED.demux_failed.load(Ordering::Acquire), "the open must not fail");
    assert!(crate::aq::aq_bytes(&mut *p.run.video) > 0, "the video was read");
    assert_eq!(max_cap, base, "the audio lane's cap never moves");
    assert!(
        queued > 0 && queued < base,
        "a file whose audio never fills the lane keeps it under its soft cap throughout: {queued} bytes"
    );
}

/// How many audio AUs the stand-in pipeline accepts before it answers BufferFull. The device log
/// shows ~568 KB queued behind `feed a#1`; 40 AUs of E-AC-3 are ~1.3 s of audio, deliberately fewer
/// than the engine's prime needs of video-side time, so a rule that leans on audio pops fails.
const AUDIO_ACCEPTED_AUS: usize = 40;

// ---------------------------------------------------------------------------------------------
// The constant-skew layout: audio stays ahead of the video for the WHOLE file (issue #600).
// ---------------------------------------------------------------------------------------------
//
// A muxer that wrote its audio 48 s ahead keeps that skew: after the audio-only prefix, audio at
// pts t is stored beside video at pts t-48. The audio lane then holds ~the whole skew for the whole
// film (3.8 MB at 640 kbit/s), far over its 1 MiB base cap, and the demuxer must neither park on it
// while the consumer is short of video (before Play: under the prime; during play: under the pump's
// feed-ahead target) nor let the video fall behind mid-film.

/// The audio lead of the skew fixture, seconds (issue #600's film: ~48 s).
const SKEW_LEAD_S: f64 = 48.0;

/// Seconds of video in the skew fixture, and how much of it the consumer must read.
const SKEW_VIDEO_S: u32 = 65;
const SKEW_READ_TO_S: i64 = 60;

/// Write the constant-skew MKV: [`SKEW_VIDEO_S`] s of 1 Mbit/s H.264 noise and `SKEW_VIDEO_S +
/// SKEW_LEAD_S` s of E-AC-3, every audio block re-stored beside the video `SKEW_LEAD_S` earlier.
/// Proves the layout from the file in FILE order: the audio-only prefix is at least the lead's worth
/// of bytes and, at the video block at pts 30 s, the most recently stored audio block is ~48 s ahead.
fn generate_skew(dir: &Path) -> PathBuf {
    let interleaved = dir.join("interleaved.mkv");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("color=c=gray:s=640x360:r=25:d={SKEW_VIDEO_S},noise=alls=100:allf=t+u")])
        .args(["-f", "lavfi", "-i", &format!("sine=frequency=440:sample_rate=48000:duration={}", SKEW_VIDEO_S as f64 + SKEW_LEAD_S)])
        .args(["-map", "0:v", "-map", "1:a"])
        // B-frames on (ultrafast turns them off): decode order differs from pts order, which is what
        // `assert_video_order_kept` needs to see a fixture that re-sorts a track's blocks.
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-bf", "2", "-pix_fmt", "yuv420p", "-g", "25"])
        .args(["-b:v", "1M", "-maxrate", "1M", "-bufsize", "1M"])
        .args(["-c:a", "eac3", "-b:a", &format!("{}k", EARLY_AUDIO_BPS / 1000)])
        .arg(&interleaved)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg could not write the interleaved skew fixture");
    let out = dir.join("skew_audio.mkv");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/mkv_audio_lead.py");
    let status = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg(&interleaved)
        .arg(&out)
        .arg(format!("{SKEW_LEAD_S}"))
        .arg("skew")
        .stdout(Stdio::null())
        .status()
        .expect("run python3");
    assert!(status.success(), "tests/mkv_audio_lead.py could not skew the fixture");
    assert_video_order_kept(&interleaved, &out);

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "packet=stream_index,codec_type,pts_time,size"])
        .args(["-of", "csv=p=0"])
        .arg(&out)
        .output()
        .expect("run ffprobe");
    assert!(probe.status.success(), "ffprobe could not read the skew fixture");
    let (mut prefix_bytes, mut seen_video, mut last_audio_pts, mut mid_skew) = (0u64, false, 0.0f64, None);
    for line in String::from_utf8_lossy(&probe.stdout).lines() {
        let f: Vec<&str> = line.trim().split(',').collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(pts), Ok(size)) = (f[2].parse::<f64>(), f[3].parse::<u64>()) else {
            continue;
        };
        if f[0] == "audio" {
            last_audio_pts = pts;
            if !seen_video {
                prefix_bytes += size;
            }
        } else {
            seen_video = true;
            if mid_skew.is_none() && pts >= 30.0 {
                mid_skew = Some(last_audio_pts - pts);
            }
        }
    }
    let mid_skew = mid_skew.expect("the skew fixture has video past 30 s");
    eprintln!("skew fixture: {prefix_bytes} audio bytes before the first video packet; audio is {mid_skew:.1} s ahead at video pts 30 s");
    let audio_cap = crate::player::engine::aq_caps().1 as u64;
    assert!(prefix_bytes > audio_cap, "fixture: the audio-only prefix must outrun the audio lane's base cap");
    assert!(
        (mid_skew - SKEW_LEAD_S).abs() < 2.0,
        "fixture: the skew must persist after the first video block (audio {mid_skew:.1} s ahead mid-file)"
    );
    out
}

/// A stand-in consumer with the engine's contract, driven against the REAL lanes.
///
/// Before Play it pops video freely (the pipeline accepts video up to BufferFull, megabytes) and
/// audio only up to [`AUDIO_ACCEPTED_AUS`] (BufferFull, because the clock cannot start); Play fires
/// once the video it has popped spans `PRIME_VIDEO_MAX_NS`. After Play a virtual clock runs at
/// `speed` x real time; video is fed up to `MAX_FEED_AHEAD_NS` past it and audio up to
/// `MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS` past it (the pump's own budgets), each holding the one AU that is too far ahead, as `feed_stream` does.
/// Returns the pts (ns) of the last video AU popped. Panics, with the lane state, when the start
/// never primes or when video stops arriving for `stall` while the clock is ahead of it.
fn drive_consumer(run: &mut DemuxRun, read_to_ns: i64, speed: i64, stall: Duration) -> i64 {
    use crate::player::engine::{AUDIO_SLACK_NS, MAX_FEED_AHEAD_NS, PRIME_VIDEO_MAX_NS};
    let lanes = |run: &mut DemuxRun| {
        format!(
            "audio lane {} bytes, video lane {} bytes, demuxer finished={} failed={}",
            crate::aq::aq_bytes(&mut *run.audio),
            crate::aq::aq_bytes(&mut *run.video),
            run.finished(),
            SHARED.demux_failed.load(Ordering::Acquire)
        )
    };
    let pop = |q: *mut AuQueue| -> Option<(*mut AuNode, i64)> {
        let node = crate::aq::aq_pop(q, std::ptr::null_mut());
        if node.is_null() {
            return None;
        }
        Some((node, unsafe { crate::aq::au_fields(node) }.2))
    };
    let free = |n: *mut AuNode| unsafe { libc::free(n as *mut c_void) };

    // Before Play.
    let (mut first_v, mut max_v) = (None::<i64>, 0i64);
    let mut audio_left = AUDIO_ACCEPTED_AUS;
    let until = Instant::now() + FIRST_AUDIO_DEADLINE;
    while first_v.is_none_or(|f| max_v - f < PRIME_VIDEO_MAX_NS) {
        assert!(
            Instant::now() < until,
            "the start never primed: only {} ms of video reached the lane in {FIRST_AUDIO_DEADLINE:?} \
             (Play needs {} ms) - the demuxer is parked behind the audio lane ({})",
            first_v.map_or(0, |f| (max_v - f) / 1_000_000),
            PRIME_VIDEO_MAX_NS / 1_000_000,
            lanes(run)
        );
        while audio_left > 0 {
            let Some((n, _)) = pop(&mut *run.audio) else { break };
            free(n);
            audio_left -= 1;
        }
        let mut any = false;
        while let Some((n, pts)) = pop(&mut *run.video) {
            free(n);
            first_v.get_or_insert(pts);
            max_v = max_v.max(pts);
            any = true;
        }
        if !any {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    // Play: a virtual clock at `speed` x.
    let t0 = Instant::now();
    let clock0 = first_v.unwrap_or(0);
    let (mut held_v, mut held_a) = (None::<(*mut AuNode, i64)>, None::<(*mut AuNode, i64)>);
    let mut last_progress = (Instant::now(), max_v);
    let result = loop {
        let clock = clock0 + t0.elapsed().as_nanos() as i64 * speed;
        for (held, lane, ahead, is_video) in [
            (&mut held_v, &mut *run.video as *mut AuQueue, MAX_FEED_AHEAD_NS, true),
            (&mut held_a, &mut *run.audio as *mut AuQueue, MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS, false),
        ] {
            loop {
                let cur = match held.take() {
                    Some(x) => x,
                    None => match pop(lane) {
                        Some(x) => x,
                        None => break,
                    },
                };
                if cur.1 > clock + ahead {
                    *held = Some(cur);
                    break;
                }
                free(cur.0);
                if is_video {
                    max_v = max_v.max(cur.1);
                }
            }
        }
        if max_v >= read_to_ns {
            break max_v;
        }
        if max_v > last_progress.1 {
            last_progress = (Instant::now(), max_v);
        } else if last_progress.0.elapsed() > stall && clock > max_v + MAX_FEED_AHEAD_NS {
            panic!(
                "video stopped arriving at {} ms while the clock is at {} ms: the demuxer is parked ({})",
                max_v / 1_000_000,
                clock / 1_000_000,
                lanes(run)
            );
        }
        assert!(!SHARED.demux_failed.load(Ordering::Acquire), "the stream must not fail ({})", lanes(run));
        std::thread::sleep(Duration::from_millis(2));
    };
    for n in [held_v, held_a].into_iter().flatten() {
        free(n.0);
    }
    result
}

/// **Constant skew (the likelier real layout).** Audio stays 48 s ahead for the whole file. The
/// start must prime (>= `PRIME_VIDEO_MAX_NS` of video reaches the lane although the stand-in
/// pipeline takes only a few audio AUs) and a steady consumer must read to 60 s of video without the
/// demuxer ever parking: the audio lane has to hold the whole skew without starving the video lane.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg and the ffmpeg CLI - see the module doc"]
fn a_constant_audio_skew_neither_hangs_the_start_nor_starves_video_mid_film() {
    let _g = plx_base::testlock::serial();
    let Some(mut p) = prepare_with("skew", generate_skew) else {
        return;
    };
    let reached = drive_consumer(&mut p.run, SKEW_READ_TO_S * 1_000_000_000, 20, Duration::from_secs(5));
    assert!(reached >= SKEW_READ_TO_S * 1_000_000_000, "video read to {reached} ns");
    assert!(!SHARED.demux_failed.load(Ordering::Acquire), "the stream must not fail");
    let audio = i64::from(crate::aq::aq_bytes(&mut *p.run.audio));
    assert!(
        audio > crate::player::engine::aq_caps().1,
        "the audio lane carries the skew over its base cap: {audio} bytes"
    );
}
