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
}

/// Build the fixture, prove its shape, serve it and start demuxing it. `None` when the tools are
/// missing (a skip).
fn prepare(tag: &str, audio_delay_s: f64, video_s: u32) -> Option<Prepared> {
    prepare_with(tag, |dir| generate(dir, audio_delay_s, video_s))
}

/// [`prepare`] over a fixture `make` writes into the scratch directory.
fn prepare_with(tag: &str, make: impl FnOnce(&Path) -> PathBuf) -> Option<Prepared> {
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
    Some(Prepared { run, first_audio_s, video_before, _server: server, _scratch: scratch })
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
    let lane = p.run.video_lane() as usize;
    let max_cap = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let sampler = {
        let (max_cap, stop) = (max_cap.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let cap = i64::from(crate::aq::aq_cap(lane as *mut AuQueue));
                max_cap.fetch_max(cap, Ordering::AcqRel);
                std::thread::yield_now();
            }
        })
    };
    let until = Instant::now() + FIRST_AUDIO_DEADLINE;
    while !p.run.finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Release);
    sampler.join().expect("sampler");

    assert!(p.run.finished(), "the demuxer must reach the end of a four second file");
    assert!(!SHARED.demux_failed.load(Ordering::Acquire), "the open must not fail");
    let held = i64::from(crate::aq::aq_bytes(&mut *p.run.video));
    assert!(held > 0 && held < base, "the whole file sits in the video lane: {held} bytes");
    assert_eq!(
        max_cap.load(Ordering::Acquire),
        base,
        "a file whose video never fills the lane must keep the lane's base cap throughout \
         (a grant was taken although no push ever parked)"
    );
}
