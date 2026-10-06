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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
#[derive(Clone)]
pub struct Spec {
    /// The fresh-session URL of the original Part (`direct_play_url(part, "<live>-subs")`).
    pub url: String,
    /// Where the remux was started in the film, nanoseconds.
    pub offset_ns: i64,
    /// The Part's whole-file bitrate, for the log line only.
    pub part_kbps: u32,
    /// The 0-based position of the drawn track among the Part's subtitle streams.
    pub ordinal: i32,
    /// Start reading at the playhead instead of the run's start offset: the reader begins mid-play
    /// (a pick on a live remux), not with the engine run.
    pub from_playhead: bool,
    /// The Part-to-playhead clock delta a reader of this same run already measured, so a reader
    /// that replaces it (a track switch) does not scan for the anchor again.
    pub delta: Option<i64>,
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
/// Serialises every start and stop (the engine's, and the worker `sync` hands its change to).
static LIFECYCLE: Mutex<()> = Mutex::new(());
/// Bumped by every engine-level `start`/`stop`: a `sync` queued before one is stale after it.
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// The clock delta the last reader measured (or was given), read by the reader that replaces it.
static LAST_DELTA: Mutex<Option<i64>> = Mutex::new(None);
/// The newest change `sync` was asked for, with the epoch it was asked in. Latest wins.
static WANT: Mutex<Option<(u64, Option<Spec>)>> = Mutex::new(None);
static SYNC_WORKER: AtomicBool = AtomicBool::new(false);

fn lifecycle() -> std::sync::MutexGuard<'static, ()> {
    LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner())
}

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

/// Raise the failure flag from outside the reader thread: the reader could not even be started.
pub fn raise_failure() {
    FAILED.store(true, Ordering::Release);
}

/// The subtitle ordinal `plxnative-subside` asks to draw, or `None` when the trigger is absent or
/// holds no non-negative number. Read at each playback start (a live trigger: no relaunch needed).
/// Always `None` in a build without the dev triggers.
pub fn dev_armed_ordinal() -> Option<i32> {
    plx_base::devtrig::read("subside")?.trim().parse::<i32>().ok().filter(|n| *n >= 0)
}

/// The reader's [`Spec`] for a route's [`crate::route::SideReaderTarget`]: the fresh-session URL
/// shape PMS serves a second reader of the Part (the live session's own identifier is refused while
/// the remux runs). It carries the token and is handed to the reader, never logged. `None` when the
/// target's server is gone.
pub fn spec_for(target: &crate::route::SideReaderTarget) -> Option<Spec> {
    let client = plx_plex::plex::client_for(target.sid)?;
    let su = client.direct_play_url(&target.part, &format!("{}-subs", target.session));
    Some(Spec {
        url: format!("{}{}", su.origin.base(), su.path),
        offset_ns: crate::player::SHARED.disp_base.load(Ordering::Relaxed),
        part_kbps: target.part_kbps,
        ordinal: target.ordinal,
        from_playhead: false,
        delta: None,
    })
}

/// Start the reader for this engine run. Replaces a reader still running (it is stopped first).
/// Selects `spec.ordinal` as the drawn track and marks the main demuxer's subtitle handling off.
/// Returns `false` when the thread could not be spawned.
pub fn start(spec: Spec) -> bool {
    let _l = lifecycle();
    EPOCH.fetch_add(1, Ordering::AcqRel);
    *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner()) = spec.delta;
    stop_locked("restart", false);
    start_locked(spec)
}

fn start_locked(spec: Spec) -> bool {
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
/// reader runs. `reason` is a label for the log line. Also retires any `sync` still queued.
pub fn stop(reason: &str) {
    let _l = lifecycle();
    EPOCH.fetch_add(1, Ordering::AcqRel);
    *WANT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    stop_locked(reason, false);
    // A failure raised but never polled named a reader that is gone; it must not be consumed
    // against whatever the next engine run, or the next selection, draws.
    FAILED.store(false, Ordering::Release);
    // The engine run is over: its anchor names a stream that no reader will match again, whether
    // or not a reader was running (the sync worker may have stopped it already, keeping the anchor).
    *crate::player::SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// `stop`'s body, under the lifecycle lock. `keep_anchor`: leave the main demuxer's published
/// anchor in place for a reader that replaces this one mid-play. Returns the clock delta the
/// stopped reader had, when one ran.
fn stop_locked(reason: &str, keep_anchor: bool) -> Option<i64> {
    let r = running().take()?;
    let started = Instant::now();
    r.stop.trigger(); // flag, then the socket / curl wake
    r.thread.unpark(); // a reader parked on pacing or the anchor wait
    plx_base::task::join("subside", r.handle);
    ACTIVE.store(false, Ordering::Release);
    // A failure the stopped reader raised and nobody has polled yet belongs to it, not to the
    // reader (or the sidecar) that replaces it.
    FAILED.store(false, Ordering::Release);
    crate::player::SHARED.side_subs_owner.store(false, Ordering::Release);
    if !keep_anchor {
        *crate::player::SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    if super::desired_sub_idx() == r.ordinal {
        super::request_subtitle(r.prev_selection);
    }
    crate::player::SHARED.sub_cues.lock().unwrap_or_else(|e| e.into_inner()).clear();
    crate::player::SHARED.sub_bitmaps.lock().unwrap_or_else(|e| e.into_inner()).clear();
    plx_base::eventlog::log(&format!(
        "subside: stop reason={reason} join_ms={}",
        started.elapsed().as_millis()
    ));
    *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner())
}

/// **Make the running reader match `desired`** while the stream plays on: start one for a track
/// picked on a live remux, stop it when the pick is Off or a sidecar, or switch it to another
/// track (the old track's cues and bitmaps are cleared with it). Never blocks the caller: stopping
/// a reader joins its thread, which a stalled server can hold up, so the change runs on a worker.
/// The newest request wins; a request older than an engine `start`/`stop` is dropped.
pub fn sync(desired: Option<Spec>) {
    // A host test of the route asks what the route wanted; it must not start a real reader.
    #[cfg(test)]
    TEST_SYNCS.with(|l| l.borrow_mut().push(desired.map(|d| d.ordinal)));
    #[cfg(not(test))]
    {
        if desired.is_none() && !ACTIVE.load(Ordering::Acquire) && !SYNC_WORKER.load(Ordering::Acquire) {
            return;
        }
        *WANT.lock().unwrap_or_else(|e| e.into_inner()) = Some((EPOCH.load(Ordering::Acquire), desired));
        kick_worker();
    }
}

#[cfg(test)]
thread_local! {
    static TEST_SYNCS: std::cell::RefCell<Vec<Option<i32>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The changes `sync` was asked for on this thread since the last call (host tests of the route):
/// `Some(ordinal)` = draw this track, `None` = draw nothing.
#[cfg(test)]
pub(crate) fn take_test_syncs() -> Vec<Option<i32>> {
    TEST_SYNCS.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

#[cfg_attr(test, allow(dead_code))]
fn kick_worker() {
    if SYNC_WORKER.swap(true, Ordering::AcqRel) {
        return; // the running worker takes the newer request on its next turn
    }
    let spawned = plx_base::task::spawn("subside-sync", || loop {
        let want = WANT.lock().unwrap_or_else(|e| e.into_inner()).take();
        match want {
            Some((epoch, desired)) => apply(epoch, desired),
            None => {
                SYNC_WORKER.store(false, Ordering::Release);
                // A request that landed between the take and the store would otherwise wait for
                // the next `sync`.
                if WANT.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    kick_worker();
                }
                return;
            }
        }
    });
    if spawned.is_none() {
        SYNC_WORKER.store(false, Ordering::Release);
        plx_base::eventlog::log("subside: could not start the sync worker");
    }
}

fn apply(epoch: u64, desired: Option<Spec>) {
    let _l = lifecycle();
    if EPOCH.load(Ordering::Acquire) != epoch {
        return; // the engine restarted or stopped since: this change names a stream that is gone
    }
    let current = running().as_ref().map(|r| r.ordinal);
    match (current, desired) {
        (None, None) => {}
        (Some(a), Some(d)) if a == d.ordinal => {}
        (_, desired) => {
            let delta = stop_locked("pick", true);
            if let Some(mut spec) = desired {
                spec.from_playhead = true;
                spec.delta = delta;
                *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner()) = delta;
                if !start_locked(spec) {
                    plx_base::eventlog::log("subside: could not start the reader thread");
                    FAILED.store(true, Ordering::Release);
                }
            }
        }
    }
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
    let mut delta: Option<i64> = spec.delta;
    let from_playhead = spec.from_playhead;
    let save = |d: Option<i64>| *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner()) = d;
    let mut reopens = 0;
    loop {
        // First pass reads from the start offset (unless this reader began mid-play); a reopen
        // from the playhead, mapped to Part time.
        let first = delta.is_none() && !from_playhead;
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
        let end = read_once(&cfg, &stop, &anchor, &mut delta, &resume, &playpos, &clock);
        save(delta);
        match end {
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
        Spec { url: format!("http://127.0.0.1:{port}/library/parts/1/file.mkv"), offset_ns: 0, part_kbps: 0, ordinal: 0, from_playhead: false, delta: None }
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

    /// What `sync`'s worker does: a pick switches the reader to another track (keeping the main
    /// demuxer's anchor, which a replacement mid-play needs), Off stops it, and a request queued
    /// before an engine start/stop is dropped as stale.
    #[test]
    fn a_synced_pick_switches_then_stops_the_reader_and_a_stale_one_is_dropped() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let (_srv, port) = stalled_server();
            let anchor = || *crate::player::SHARED.side_anchor.lock().unwrap();
            *crate::player::SHARED.side_anchor.lock().unwrap() = Some(Anchor::of(0, &[1, 2, 3]));
            assert!(start(spec(port)));
            let epoch = EPOCH.load(Ordering::Acquire);
            crate::player::SHARED.sub_cues.lock().unwrap().clear();
            // another track: replaced, still active, anchor kept
            apply(epoch, Some(Spec { ordinal: 3, ..spec(port) }));
            assert!(active());
            assert_eq!(running().as_ref().map(|r| r.ordinal), Some(3));
            assert!(anchor().is_some(), "a replacement reader needs the anchor again");
            // the same track again: nothing to do
            apply(epoch, Some(Spec { ordinal: 3, ..spec(port) }));
            assert_eq!(running().as_ref().map(|r| r.ordinal), Some(3));
            // Off / a sidecar: stopped, anchor still kept for a later pick
            apply(epoch, None);
            assert!(!active());
            assert!(anchor().is_some());
            // an engine stop retires everything queued before it
            stop("engine");
            assert!(anchor().is_none(), "the engine's stop clears the anchor");
            apply(epoch, Some(spec(port)));
            assert!(!active(), "a request from before the stop names a stream that is gone");
        });
    }

    /// A failure nobody has polled yet belongs to the reader that raised it: tearing the engine
    /// down, or a sync that replaces or stops that reader, retires it, so it is never consumed
    /// against whatever the viewer selects next.
    #[test]
    fn a_pending_failure_is_retired_with_its_reader() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let (_srv, port) = stalled_server();
            *crate::player::SHARED.side_anchor.lock().unwrap() = None;
            // engine stop, with and without a reader still registered
            assert!(start(spec(port)));
            raise_failure();
            stop("test-engine");
            assert!(!take_failure(), "stop clears a pending failure");
            raise_failure();
            stop("test-nothing-running");
            assert!(!take_failure(), "even with no reader left to stop");
            // a sync that stops the reader (the viewer picked a sidecar or Off)
            assert!(start(spec(port)));
            let epoch = EPOCH.load(Ordering::Acquire);
            raise_failure();
            apply(epoch, None);
            assert!(!active());
            assert!(!take_failure(), "a sync that stops the reader clears its failure");
            stop("test-cleanup");
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
