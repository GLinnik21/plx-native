//! The **side subtitle reader**'s thread and state (the FFmpeg half is `ff_subside.rs`).
//!
//! While the player plays a server remux of a film, PMS cannot put a subtitle in the stream, so the
//! app reads the film's ORIGINAL Part a second time, subtitle packets only, and draws the track with
//! the client renderer. `start` is called once per engine run (a remux seek is always a reload, so a
//! new start at the new offset), `stop` once per teardown; there is no in-place seek.
//!
//! Started by the engine for a route whose presenter is `ClientOverRemux` for an embedded track
//! (`route::side_reader_target`), or, for diagnostics, by the dev trigger `plxnative-subside`
//! ([`dev_armed_ordinal`]) with no subtitle selected; the reader itself is not dev-gated. Everything is main-thread driven except the one reader thread, whose
//! only inputs are the [`Spec`], `SHARED`'s playhead and clock atomics, and the main demuxer's
//! published keyframe anchor.
//!
//! Logs: `subside: open|anchor|reopen|failed|stop …` — labels and numbers, never the URL, host,
//! token or session identifier.

use crate::ff::subside::{read_once, Anchor, ReadCfg, RunEnd, SideStop};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Reopens after a read error before the failure is raised.
const MAX_REOPENS: u32 = 3;
/// How long the reader waits between looks at the anchor, and between a failure and its reopen.
const POLL: Duration = Duration::from_millis(50);
const BACKOFF: Duration = Duration::from_millis(500);
/// How far before the playhead a reopen starts reading, so a cue already on screen comes back.
const REOPEN_LEAD_NS: i64 = 15_000_000_000;

/// Everything one run of the reader is started with. `url` carries the token: held, never logged.
pub struct Spec {
    /// The fresh-session URL of the original Part (`direct_play_url(part, "<live>-subs")`).
    pub url: String,
    /// Where the remux was started in the film, nanoseconds.
    pub offset_ns: i64,
    /// The Part's whole-file bitrate, for the log line only.
    pub part_kbps: u32,
    /// The 0-based position of the drawn track among the Part's subtitle streams.
    pub ordinal: i32,
}

struct Running {
    stop: Arc<SideStop>,
    thread: std::thread::Thread,
    handle: std::thread::JoinHandle<()>,
    ordinal: i32,
    /// `desired_sub_idx` before the reader set it, restored on stop.
    prev_selection: i32,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);

fn running() -> std::sync::MutexGuard<'static, Option<Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Is a reader running? The draw gate (`route::subtitles_burned`) reads this only for the dev
/// trigger's run, where no subtitle is selected in the route to give the presenter anything to say.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Did the reader give up (every reopen failed, or the Part has no such track)? Reads and clears the
/// flag, so a failure is reported once.
pub fn take_failure() -> bool {
    FAILED.swap(false, Ordering::AcqRel)
}

/// The subtitle ordinal `plxnative-subside` asks to draw, or `None` when the trigger is absent or
/// holds no non-negative number. Read at each playback start (a live trigger: no relaunch needed).
/// Always `None` in a build without the dev triggers.
pub fn dev_armed_ordinal() -> Option<i32> {
    plx_base::devtrig::read("subside")?.trim().parse::<i32>().ok().filter(|n| *n >= 0)
}

/// Start the reader for this engine run. Replaces a reader still running (it is stopped first).
/// Selects `spec.ordinal` as the drawn track and marks the main demuxer's subtitle handling off.
/// Returns `false` when the thread could not be spawned.
pub fn start(spec: Spec) -> bool {
    stop("restart");
    FAILED.store(false, Ordering::Release);
    let stop_handle = SideStop::new();
    let prev_selection = super::desired_sub_idx();
    crate::player::SHARED.side_subs_owner.store(true, Ordering::Release);
    super::request_subtitle(spec.ordinal);
    let ordinal = spec.ordinal;
    let thread_stop = Arc::clone(&stop_handle);
    // `plx_base::task::spawn` hands back std's handle, whose `thread()` is what `stop` unparks.
    let Some(handle) = plx_base::task::spawn("subside", move || run(spec, thread_stop)) else {
        crate::player::SHARED.side_subs_owner.store(false, Ordering::Release);
        super::request_subtitle(prev_selection);
        return false;
    };
    ACTIVE.store(true, Ordering::Release);
    *running() = Some(Running { stop: stop_handle, thread: handle.thread().clone(), handle, ordinal, prev_selection });
    true
}

/// Stop the reader and join it, then clear the cue and bitmap stores it filled and give the
/// subtitle selection back. Call it BEFORE the demux join in teardown. Idempotent; a no-op when no
/// reader runs. `reason` is a label for the log line.
pub fn stop(reason: &str) {
    let Some(r) = running().take() else { return };
    let started = Instant::now();
    r.stop.trigger(); // flag, then the socket / curl wake
    r.thread.unpark(); // a reader parked on pacing or the anchor wait
    plx_base::task::join("subside", r.handle);
    ACTIVE.store(false, Ordering::Release);
    crate::player::SHARED.side_subs_owner.store(false, Ordering::Release);
    *crate::player::SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if super::desired_sub_idx() == r.ordinal {
        super::request_subtitle(r.prev_selection);
    }
    crate::player::SHARED.sub_cues.lock().unwrap_or_else(|e| e.into_inner()).clear();
    crate::player::SHARED.sub_bitmaps.lock().unwrap_or_else(|e| e.into_inner()).clear();
    plx_base::eventlog::log(&format!(
        "subside: stop reason={reason} join_ms={}",
        started.elapsed().as_millis()
    ));
}

/// Park for `d`, waking early when `stop` is triggered (its `unpark` reaches this thread).
fn nap(stop: &SideStop, d: Duration) {
    if !stop.is_set() {
        std::thread::park_timeout(d);
    }
}

/// Wait for the main demuxer's first keyframe. `None` when stopped first.
fn wait_for_anchor(stop: &SideStop) -> Option<Anchor> {
    loop {
        if stop.is_set() {
            return None;
        }
        if let Some(a) = *crate::player::SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) {
            return Some(a);
        }
        nap(stop, POLL);
    }
}

fn playpos() -> i64 {
    crate::player::SHARED.playpos_ns.load(Ordering::Relaxed)
}

fn run(spec: Spec, stop: Arc<SideStop>) {
    // The anchor first: the heavy reads (the Part's own bytes) wait for the remux to have started.
    let Some(anchor) = wait_for_anchor(&stop) else { return };
    let cfg = ReadCfg { url: &spec.url, offset_ns: spec.offset_ns, ordinal: spec.ordinal.max(0) as usize, kbps: spec.part_kbps };
    let mut delta: Option<i64> = None;
    let mut reopens = 0;
    loop {
        // First pass reads from the start offset; a reopen from the playhead, mapped to Part time.
        let first = delta.is_none();
        let resume = |d: i64| {
            if first {
                cfg.offset_ns - crate::ff::subside::SCAN_NS
            } else {
                playpos().saturating_add(d).saturating_sub(REOPEN_LEAD_NS)
            }
        };
        let clock = || {
            let s = &crate::player::SHARED;
            (s.pts_shift.load(Ordering::Relaxed), s.disp_base.load(Ordering::Relaxed))
        };
        match read_once(&cfg, &stop, &anchor, &mut delta, &resume, &playpos, &clock) {
            RunEnd::Stopped => return,
            RunEnd::Unusable => {
                plx_base::eventlog::log("subside: failed");
                FAILED.store(true, Ordering::Release);
                return;
            }
            RunEnd::Failed => {
                reopens += 1;
                if reopens > MAX_REOPENS {
                    plx_base::eventlog::log("subside: failed");
                    FAILED.store(true, Ordering::Release);
                    return;
                }
                plx_base::eventlog::log(&format!("subside: reopen n={reopens}"));
                nap(&stop, BACKOFF);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener that accepts connections and never answers: a stalled server. Held connections
    /// are dropped when the test ends.
    fn stalled_server() -> (std::net::TcpListener, u16) {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        (srv, port)
    }

    fn spec(port: u16) -> Spec {
        Spec { url: format!("http://127.0.0.1:{port}/library/parts/1/file.mkv"), offset_ns: 0, part_kbps: 0, ordinal: 0 }
    }

    /// `stop` returns and the reader is gone even when the server never answers a single byte,
    /// whether the reader is parked waiting for the anchor or blocked reading the open's headers.
    #[test]
    fn start_then_stop_joins_promptly_with_a_stalled_server() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let (srv, port) = stalled_server();
            srv.set_nonblocking(true).unwrap();
            // 1. no anchor yet: the reader parks in its anchor wait
            *crate::player::SHARED.side_anchor.lock().unwrap() = None;
            assert!(start(spec(port)));
            assert!(active());
            assert!(crate::player::SHARED.side_subs_owner.load(Ordering::Acquire));
            stop("test-no-anchor");
            assert!(!active());
            assert!(!crate::player::SHARED.side_subs_owner.load(Ordering::Acquire));
            // 2. an anchor exists: the reader opens the Part and blocks in the stalled server
            *crate::player::SHARED.side_anchor.lock().unwrap() = Some(Anchor::of(0, &[1, 2, 3]));
            assert!(start(spec(port)));
            // wait until the reader has actually connected, so the stop lands on a blocked read
            let mut conn = None;
            while conn.is_none() {
                match plx_base::testnet::accept(&srv) {
                    Ok((s, _)) => conn = Some(s),
                    Err(_) => std::thread::yield_now(),
                }
            }
            stop("test-stalled");
            assert!(!active());
            assert!(!take_failure(), "a stop is not a failure");
            drop(conn);
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("subside: stop reason=test-stalled"), "{log}");
            for banned in ["http://", "127.0.0.1", "token", "Token", "/library"] {
                assert!(!log.contains(banned), "the log names {banned:?}: {log}");
            }
            *crate::player::SHARED.side_anchor.lock().unwrap() = None;
        });
    }

    #[test]
    fn stop_without_a_reader_is_a_no_op() {
        let _g = plx_base::testlock::serial();
        stop("nothing");
        assert!(!active());
    }

    #[test]
    fn an_unarmed_trigger_asks_for_no_track() {
        plx_base::devtrig::with_private_triggers(|| {
            assert_eq!(dev_armed_ordinal(), None, "no file, no reader");
            std::fs::write(plx_base::devtrig::path("subside"), "2\n").unwrap();
            assert_eq!(dev_armed_ordinal(), Some(2));
            std::fs::write(plx_base::devtrig::path("subside"), "-1").unwrap();
            assert_eq!(dev_armed_ordinal(), None);
            std::fs::write(plx_base::devtrig::path("subside"), "").unwrap();
            assert_eq!(dev_armed_ordinal(), None, "an empty trigger names no track");
        });
    }
}
