//! One feed attempt's side-reader IO seam, and the **side subtitle reader**'s thread and state.
//! The common runner owns the reader's concrete source and lifecycle ([`SideReaderPort`]); the
//! physical feed preserves its start and stop order around worker joins. The reader thread itself
//! (below) is the Feed's, and reaches the common caption runner only through [`SideReaderJobPort`].
//!
//! While the player plays a server remux of a film, PMS cannot put a subtitle in the stream, so the
//! app reads the film's ORIGINAL Part a second time, subtitle packets only, and draws the track with
//! the client renderer. `start` is called once per engine run (a remux seek is always a reload, so a
//! new start at the new offset), `stop` once per teardown; there is no in-place seek.
//!
//! Started by the engine for a route whose presenter is `ClientOverRemux` for an embedded track
//! (`route::side_reader_target`), or, for diagnostics, by the dev trigger `plxnative-subside`; the
//! reader itself is not dev-gated. Everything is main-thread driven except the one reader thread,
//! whose only inputs are the [`Spec`], `FEED_SHARED`'s playhead and clock atomics, and the main
//! demuxer's published keyframe anchor.
//!
//! **A SIDECAR gets the same reader CLOCK-ONLY** ([`SIDE_READER_CLOCK_ONLY`]): the app fetches that
//! subtitle itself, so the reader reads no cue, owns no subtitle selection (`side_subs_owner` stays
//! false, `desired_sub_idx` is untouched), fills no store, and never ends in the server's burn:
//! neither its give-up path nor a mid-play start failure raises a failure ([`failed_reading`]), and
//! `route::side_subtitles_failed` ignores the flag for a sidecar anyway.
//! It runs the keyframe scan only, to learn where in the Part the remux's first keyframe is: a
//! sidecar is in file time, the remux is labelled with the REQUESTED offset, and PMS starts it on
//! a keyframe earlier than or at that offset (not always the last one before it). A byte-exact
//! match is published to [`exact_delta_ns`], which
//! `player::sidecar_clock_ns` adds to the sidecar's render clock; without one nothing is shifted
//! (the assumed mapping is not trusted for this: it was measured 2.002 s wrong). The run is bound
//! by the same gate as an embedded one (`route::side_reader_target`): off it there is no reader
//! and a sidecar is drawn on the remux's clock, as before.
//!
//! Logs: `subside: open|anchor|reopen|failed|clock-only failed|stop …` — labels and numbers, never
//! the URL, host, token or session identifier.

use crate::shared::FEED_SHARED;
use plx_playback_contract::subtitle::{Anchor, SIDE_READER_CLOCK_ONLY};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The player's side-subtitle reader as one Engine owns it and drives it from the main thread.
pub trait SideReaderPort: Send {
    /// The demuxer must leave embedded subtitles to this reader from its first instruction.
    fn embedded_owner(&self) -> bool;
    /// Start only after both demux and native Load workers have spawned.
    fn start_after_load_spawn(&mut self);
    /// Stop and join before the paired demux worker is joined, including a failed Load spawn.
    fn stop_before_demux_join(&mut self, reason: &str);
}

/// One open's reader, built from what the route decided ([`SideReaderRequest`]) over the common
/// caption runner the composition hands the Feed. It remains attached to the Engine through
/// teardown, so a failed Load spawn and a later dynamic track switch get the same
/// stop-before-join barrier.
pub struct SideReaderOpen {
    request: SideReaderRequest,
    job: &'static dyn SideReaderJobPort,
}

impl SideReaderOpen {
    pub fn new(request: SideReaderRequest, job: &'static dyn SideReaderJobPort) -> Self {
        Self { request, job }
    }
}

impl SideReaderPort for SideReaderOpen {
    fn embedded_owner(&self) -> bool { self.request.selected && !self.request.clock_only }

    fn start_after_load_spawn(&mut self) {
        if !self.request.selected { return; }
        start_run(self.request.spec.take(), self.request.clock_only, self.job);
    }

    fn stop_before_demux_join(&mut self, reason: &str) {
        stop(reason);
    }
}

// The job seam's types are contract vocabulary (`plx_playback_contract::subtitle`), where the
// common caption runner names them without naming this crate; the old paths stay for this crate's
// own code.
pub use plx_playback_contract::feed::SideReaderRequest;
pub use plx_playback_contract::subtitle::{ReadCfg, RunEnd, SideReaderJobPort, SideStop, SCAN_NS, Spec};

// ---- the reader thread and its state ----------------------------------------------------------

/// Reopens after a read error before the failure is raised.
const MAX_REOPENS: u32 = 3;
/// How long the reader waits between looks at the anchor, and between a failure and its reopen.
const POLL: Duration = Duration::from_millis(50);
const BACKOFF: Duration = Duration::from_millis(500);
/// How far before the playhead a reopen starts reading, so a cue already on screen comes back.
const REOPEN_LEAD_NS: i64 = 15_000_000_000;

struct Running {
    stop: Arc<SideStop>,
    thread: std::thread::Thread,
    handle: std::thread::JoinHandle<()>,
    ordinal: i32,
    /// `desired_sub_idx` before the reader set it, restored on stop.
    prev_selection: i32,
    /// The common caption runner this reader was started with; `stop` gives its selection and
    /// stores back through the same one.
    job: &'static dyn SideReaderJobPort,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The reader's give-ups, counted, and the count a stop has voided: `FAILED` as a level could be
/// cleared by a stop, which the player's counted edge cannot do, so the count it reads is
/// [`failures`] (the raises no stop has voided).
static RAISED: AtomicU32 = AtomicU32::new(0);
static VOIDED: AtomicU32 = AtomicU32::new(0);
/// Serialises every start and stop (the engine's, and the worker `sync` hands its change to).
static LIFECYCLE: Mutex<()> = Mutex::new(());
/// Bumped by every engine-level `start`/`stop`: a `sync` queued before one is stale after it.
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// The clock delta the last reader measured (or was given), read by the reader that replaces it.
static LAST_DELTA: Mutex<Option<i64>> = Mutex::new(None);
/// What [`EXACT_DELTA_NS`] holds while no byte-exact match has been measured for this engine run.
const NO_EXACT: i64 = i64::MIN;
/// The Part-to-playhead clock delta (`delta`, nanoseconds) of THIS engine run, published only when
/// the keyframe scan matched the run's first keyframe byte for byte ([`publish_exact`]); read lock-free
/// every frame by the sidecar draw ([`exact_delta_ns`]). Cleared by `start` and `stop`, so a run the
/// reader is not admitted for (or one not measured yet) never inherits another run's delta.
static EXACT_DELTA_NS: AtomicI64 = AtomicI64::new(NO_EXACT);
/// The newest change `sync` was asked for, with the epoch it was asked in. Latest wins.
static WANT: Mutex<Option<(u64, Option<Spec>, &'static dyn SideReaderJobPort)>> = Mutex::new(None);
static SYNC_WORKER: AtomicBool = AtomicBool::new(false);

fn lifecycle() -> std::sync::MutexGuard<'static, ()> {
    LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner())
}

fn running() -> std::sync::MutexGuard<'static, Option<Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Publish the exact clock delta the reader measured for this engine run: `part_time − playhead`
/// at the run's first keyframe, matched byte for byte. Never called with an ASSUMED delta.
pub fn publish_exact(delta_ns: i64) {
    EXACT_DELTA_NS.store(delta_ns, Ordering::Release);
}

/// The run's exact clock delta, or `None` when none has been measured (the scan has not finished,
/// did not match, or no reader is admitted for this run). Lock-free: the sidecar draw reads it per frame.
pub fn exact_delta_ns() -> Option<i64> {
    Some(EXACT_DELTA_NS.load(Ordering::Acquire)).filter(|d| *d != NO_EXACT)
}

/// Is a reader running? The draw gate (`route::subtitles_burned`) reads this only for the dev
/// trigger's run, where no subtitle is selected in the route to give the presenter anything to say.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// How many times the reader gave up (every reopen failed, or the Part has no such track, or it
/// could not start) that no stop has voided; `0` when none stands. A counter, not a level: the
/// player's readout holds how many it has taken (`FeedRaw::side_failures`), so a failure is reported
/// once. Each raise makes the count differ from every count taken before it; a stop voids what was
/// raised and not yet taken (a failure the stopped reader raised names a reader that is gone) and
/// reads `0`.
pub fn failures() -> u32 {
    let raised = RAISED.load(Ordering::Acquire);
    if raised == VOIDED.load(Ordering::Acquire) { 0 } else { raised }
}

fn raise_failure() {
    RAISED.fetch_add(1, Ordering::AcqRel);
}

/// A stop voids every failure raised so far.
fn void_failures() {
    VOIDED.store(RAISED.load(Ordering::Acquire), Ordering::Release);
}

/// Start the reader for this engine run. Replaces a reader still running (it is stopped first).
/// Selects `spec.ordinal` as the drawn track and marks the main demuxer's subtitle handling off —
/// unless it is [`SIDE_READER_CLOCK_ONLY`], which takes neither. Returns `false` when the thread could not be
/// spawned.
pub fn start(spec: Spec, job: &'static dyn SideReaderJobPort) -> bool {
    let _l = lifecycle();
    EPOCH.fetch_add(1, Ordering::AcqRel);
    stop_locked("restart", false);
    *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner()) = spec.delta;
    // A new engine run: the last run's measurement describes a stream that is gone.
    EXACT_DELTA_NS.store(NO_EXACT, Ordering::Release);
    start_locked(spec, job)
}

fn start_locked(spec: Spec, job: &'static dyn SideReaderJobPort) -> bool {
    void_failures();
    let stop_handle = SideStop::new();
    let prev_selection = job.desired_sub_idx();
    let clock_only = spec.ordinal == SIDE_READER_CLOCK_ONLY;
    if !clock_only {
        FEED_SHARED.side_subs_owner.store(true, Ordering::Release);
        job.request_subtitle(spec.ordinal);
    }
    let ordinal = spec.ordinal;
    let thread_stop = Arc::clone(&stop_handle);
    // `plx_base::task::spawn` hands back std's handle, whose `thread()` is what `stop` unparks.
    let Some(handle) = plx_base::task::spawn("subside", move || run(spec, thread_stop, job)) else {
        if !clock_only {
            FEED_SHARED.side_subs_owner.store(false, Ordering::Release);
            job.request_subtitle(prev_selection);
        }
        return false;
    };
    ACTIVE.store(true, Ordering::Release);
    *running() = Some(Running { stop: stop_handle, thread: handle.thread().clone(), handle, ordinal, prev_selection, job });
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
    void_failures();
    // The engine run is over: its anchor names a stream that no reader will match again, whether
    // or not a reader was running (the sync worker may have stopped it already, keeping the anchor).
    *FEED_SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // The same reason: whether or not a reader ran, a run that gets none (off the gate) must not
    // inherit the clock of the run before it, which would shift its sidecar by a stranger's snap.
    *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner()) = None;
    EXACT_DELTA_NS.store(NO_EXACT, Ordering::Release);
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
    void_failures();
    if !keep_anchor {
        *FEED_SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    // A clock-only run took no subtitle selection, owned no ownership flag and filled no store:
    // there is nothing of it to give back.
    if r.ordinal != SIDE_READER_CLOCK_ONLY {
        FEED_SHARED.side_subs_owner.store(false, Ordering::Release);
        if r.job.desired_sub_idx() == r.ordinal {
            r.job.request_subtitle(r.prev_selection);
        }
        r.job.clear_bitmaps();
    }
    plx_base::eventlog::log(&format!(
        "subside: stop reason={reason} join_ms={}",
        started.elapsed().as_millis()
    ));
    *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner())
}

/// **Make the running reader match `desired`** while the stream plays on: start one for a track
/// picked on a live remux, run it clock-only for a sidecar ([`SIDE_READER_CLOCK_ONLY`]), stop it when the pick
/// is Off, or switch it to another
/// track (the old track's cues and bitmaps are cleared with it). Never blocks the caller: stopping
/// a reader joins its thread, which a stalled server can hold up, so the change runs on a worker.
/// The newest request wins; a request older than an engine `start`/`stop` is dropped.
pub fn sync(desired: Option<Spec>, job: &'static dyn SideReaderJobPort) {
    if desired.is_none() && !ACTIVE.load(Ordering::Acquire) && !SYNC_WORKER.load(Ordering::Acquire) {
        return;
    }
    *WANT.lock().unwrap_or_else(|e| e.into_inner()) = Some((EPOCH.load(Ordering::Acquire), desired, job));
    kick_worker();
}

fn kick_worker() {
    if SYNC_WORKER.swap(true, Ordering::AcqRel) {
        return; // the running worker takes the newer request on its next turn
    }
    let spawned = plx_base::task::spawn("subside-sync", || loop {
        let want = WANT.lock().unwrap_or_else(|e| e.into_inner()).take();
        match want {
            Some((epoch, desired, job)) => apply(epoch, desired, job),
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

fn apply(epoch: u64, desired: Option<Spec>, job: &'static dyn SideReaderJobPort) {
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
                // A sidecar's clock-only run must not raise the burn, even when its thread is
                // refused: the flag would be read as an EMBEDDED pick's if the viewer switched
                // before the pump polled it.
                let clock_only = spec.ordinal == SIDE_READER_CLOCK_ONLY;
                spec.from_playhead = true;
                spec.delta = delta;
                *LAST_DELTA.lock().unwrap_or_else(|e| e.into_inner()) = delta;
                if !start_locked(spec, job) {
                    failed_reading(clock_only, "subside: could not start the reader thread");
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
        if let Some(a) = *FEED_SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) {
            return Some(a);
        }
        nap(stop, POLL);
    }
}

fn playpos() -> i64 {
    FEED_SHARED.playpos_ns.load(Ordering::Relaxed)
}

/// The reader cannot go on, or could not start: one log line, and the failure the route answers
/// with the server's burn. `embedded_label` is the line a TRACK reader leaves.
///
/// A clock-only run has no track to lose: its failure is delta 0 (the sidecar keeps the remux's
/// clock, as before the reader existed), it gets its own label, and it never raises the burn,
/// which would re-encode the picture for a subtitle the app fetches itself.
pub fn failed_reading(clock_only: bool, embedded_label: &'static str) {
    if clock_only {
        plx_base::eventlog::log("subside: clock-only failed");
    } else {
        plx_base::eventlog::log(embedded_label);
        raise_failure();
    }
}

/// Start the reader of an engine run once both workers have spawned, or give up: with no `spec`
/// to start, or a thread the system refused, the run is [`failed_reading`], and a track reader
/// (not a clock-only one) hands the demuxer its embedded subtitles back.
pub fn start_run(spec: Option<Spec>, clock_only: bool, job: &'static dyn SideReaderJobPort) {
    if spec.is_some_and(|spec| start(spec, job)) { return; }
    failed_reading(clock_only, "subside: could not start the reader thread");
    if !clock_only {
        FEED_SHARED.side_subs_owner.store(false, Ordering::Release);
    }
}

fn run(spec: Spec, stop: Arc<SideStop>, job: &dyn SideReaderJobPort) {
    let clock_only = spec.ordinal == SIDE_READER_CLOCK_ONLY;
    // A clock-only run that is handed a delta (a track switch within this engine run) has nothing
    // left to find: the mapping is the run's, whether it was matched exactly (then `exact_delta_ns`
    // already holds it) or not (the same anchor, Part and window would miss again).
    if clock_only && spec.delta.is_some() {
        return;
    }
    // The anchor first: the heavy reads (the Part's own bytes) wait for the remux to have started.
    let Some(anchor) = wait_for_anchor(&stop) else { return };
    let cfg = ReadCfg {
        url: &spec.url,
        offset_ns: spec.offset_ns,
        ordinal: spec.ordinal.max(0) as usize,
        kbps: spec.part_kbps,
        clock_only,
    };
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
                cfg.offset_ns - SCAN_NS
            } else {
                playpos().saturating_add(d).saturating_sub(REOPEN_LEAD_NS)
            }
        };
        let clock = || {
            let s = &FEED_SHARED;
            (s.pts_shift.load(Ordering::Relaxed), s.disp_base.load(Ordering::Relaxed))
        };
        let retention = || {
            let seeking = FEED_SHARED.seeking.load(Ordering::Acquire);
            let target = FEED_SHARED.seek_display_ns.load(Ordering::Relaxed);
            plx_playback_contract::subtitle::CaptionRetention {
                playhead_ns: Some(playpos()),
                seek_target_ns: (seeking && target >= 0).then_some(target),
            }
        };
        let end = job.read_once(&cfg, &stop, &anchor, &mut delta, &resume, &playpos, &clock, &retention, &publish_exact);
        save(delta);
        match end {
            RunEnd::Stopped | RunEnd::Measured => return,
            RunEnd::Unusable => {
                failed_reading(clock_only, "subside: failed");
                return;
            }
            RunEnd::Failed => {
                reopens += 1;
                if reopens > MAX_REOPENS {
                    failed_reading(clock_only, "subside: failed");
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

    /// The player's side of the counted failure: the readout's edge rule (`counted != 0 && counted
    /// != taken`) over [`failures`], taking what it reads.
    fn take_failure() -> bool {
        static TAKEN: AtomicU32 = AtomicU32::new(0);
        let counted = failures();
        counted != 0 && TAKEN.swap(counted, Ordering::AcqRel) != counted
    }
    use plx_playback_contract::subtitle::CaptionRetention;
    use std::sync::atomic::AtomicI32;

    /// The common caption runner, stood in for: `read_once` parks until the stop is set (a reader
    /// blocked on a server that never answers), and the selection is one atomic.
    struct StubJob { selected: AtomicI32 }
    static JOB: StubJob = StubJob { selected: AtomicI32::new(-1) };

    impl SideReaderJobPort for StubJob {
        fn read_once(
            &self, _cfg: &ReadCfg, stop: &Arc<SideStop>, _anchor: &Anchor, _delta: &mut Option<i64>,
            _resume_part_ns: &dyn Fn(i64) -> i64, _playpos: &dyn Fn() -> i64,
            _clock: &dyn Fn() -> (i64, i64), _retention: &dyn Fn() -> CaptionRetention,
            _on_exact_delta: &dyn Fn(i64),
        ) -> RunEnd {
            while !stop.is_set() {
                std::thread::park_timeout(Duration::from_millis(20));
            }
            RunEnd::Stopped
        }
        fn desired_sub_idx(&self) -> i32 { self.selected.load(Ordering::Acquire) }
        fn request_subtitle(&self, ordinal: i32) { self.selected.store(ordinal, Ordering::Release); }
        fn clear_bitmaps(&self) {}
    }

    fn spec(port: u16) -> Spec {
        Spec { url: format!("http://127.0.0.1:{port}/library/parts/1/file.mkv"), offset_ns: 0, part_kbps: 0, ordinal: 0, from_playhead: false, delta: None }
    }

    fn clock_only_spec(port: u16, delta: Option<i64>) -> Spec {
        Spec { ordinal: SIDE_READER_CLOCK_ONLY, delta, ..spec(port) }
    }

    /// Wait until the reader thread has returned on its own (not been stopped).
    fn wait_until_the_reader_ends() {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !running().as_ref().is_some_and(|r| r.handle.is_finished()) {
            assert!(Instant::now() < deadline, "the reader did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A clock-only run is a sidecar's: it takes no subtitle selection and no ownership of the
    /// demuxer's subtitles, and what it measured does not outlive the engine run (`stop`) or leak
    /// into the next one (`start`).
    #[test]
    fn a_clock_only_run_takes_nothing_and_its_measurement_does_not_outlive_the_run() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let port = 1;
            *FEED_SHARED.side_anchor.lock().unwrap() = None;
            let selected = JOB.desired_sub_idx();
            assert!(start(clock_only_spec(port, None), &JOB));
            assert!(active(), "`sync`'s early return reads this: a clock-only run must stop through it");
            assert!(!FEED_SHARED.side_subs_owner.load(Ordering::Acquire), "the demuxer keeps its subtitles");
            assert_eq!(JOB.desired_sub_idx(), selected, "the viewer's selection is not the reader's to change");
            publish_exact(-2_169_000_000);
            assert_eq!(exact_delta_ns(), Some(-2_169_000_000));
            stop("test-clock-only");
            assert_eq!(exact_delta_ns(), None, "the engine run is over: so is its measurement");
            assert_eq!(JOB.desired_sub_idx(), selected);
            assert!(!take_failure());
            // a measurement left by another run is gone when the next one starts
            publish_exact(1);
            assert!(start(clock_only_spec(port, None), &JOB));
            assert_eq!(exact_delta_ns(), None);
            stop("test-clock-only");
        });
    }
    /// A clock-only run that cannot go on measures nothing and says so once; unlike a track reader
    /// it never raises the failure the route answers with the server's burn, and its line is its own
    /// (the harness grades `subside: failed` / `subside: could not start` as the TRACK reader
    /// failing).
    #[test]
    fn a_clock_only_run_that_gives_up_is_never_the_server_burn() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let _ = take_failure();
            failed_reading(true, "subside: failed");
            assert!(!take_failure(), "a sidecar's clock reader must never end in the server burn");
            assert_eq!(exact_delta_ns(), None, "delta stays 0: the sidecar keeps the remux's clock");
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("subside: clock-only failed"), "{log}");
            assert!(!log.contains("subside: failed"), "a clock-only failure must not read as the track reader's: {log}");
            failed_reading(false, "subside: failed");
            assert!(take_failure(), "fixture: a track reader that gives up IS the burn");
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert_eq!(log.matches("subside: failed").count(), 1, "one line per give-up: {log}");
        });
    }
    /// The policy both start-failure call sites use (`apply` for a mid-play pick, `engine` for the
    /// run's own start): a clock-only run never raises the burn (the flag could otherwise be read
    /// against an embedded pick the viewer switched to), and a track reader still does. The call
    /// site's only input is `spec.ordinal == SIDE_READER_CLOCK_ONLY`, computed before `spec` moves.
    #[test]
    fn a_clock_only_start_failure_is_never_the_server_burn() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let _ = take_failure();
            failed_reading(true, "subside: could not start the reader thread");
            assert!(!take_failure(), "a sidecar's clock reader must never end in the server burn");
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("subside: clock-only failed"), "{log}");
            assert!(!log.contains("subside: could not start"), "a clock-only failure must not read as the track reader's: {log}");
            failed_reading(false, "subside: could not start the reader thread");
            assert!(take_failure(), "fixture: a track reader's refused thread IS the burn");
        });
    }
    /// A track switch within one engine run hands the clock-only run the delta already measured: it
    /// has nothing to scan and ends at once, whether or not that delta was exact.
    #[test]
    fn a_clock_only_run_handed_a_delta_ends_without_a_scan() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let port = 1;
            *FEED_SHARED.side_anchor.lock().unwrap() = None; // no anchor: a scan could not even start
            assert!(start(clock_only_spec(port, Some(-1_500_000_000)), &JOB));
            wait_until_the_reader_ends();
            assert!(!take_failure());
            assert_eq!(exact_delta_ns(), None, "an assumed delta is never published");
            stop("test-handed");
        });
    }
    /// The production path for a carried delta is `apply` (a track switch mid-play), not `start`:
    /// an exact delta a TRACK run measured is the same engine run's mapping, so a pick that moves
    /// the reader to the clock-only role keeps it — and the clock-only run has nothing left to scan.
    #[test]
    fn a_delta_measured_by_a_track_run_survives_the_switch_to_the_sidecar() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let port = 1;
            *FEED_SHARED.side_anchor.lock().unwrap() = Some(Anchor::of(0, &[1, 2, 3]));
            assert!(start(spec(port), &JOB));
            // what `read_once` publishes on a byte-exact match, and what the run hands on
            publish_exact(-2_169_000_000);
            *LAST_DELTA.lock().unwrap() = Some(-2_169_000_000);
            let epoch = EPOCH.load(Ordering::Acquire);
            apply(epoch, Some(clock_only_spec(port, None)), &JOB);
            assert_eq!(running().as_ref().map(|r| r.ordinal), Some(SIDE_READER_CLOCK_ONLY), "the reader measures the clock only now");
            assert_eq!(exact_delta_ns(), Some(-2_169_000_000), "the run's measurement is the sidecar's too");
            wait_until_the_reader_ends();
            stop("test-carried");
            assert_eq!(exact_delta_ns(), None, "the engine run is over: so is its measurement");
        });
    }
    /// What `sync`'s worker does: a pick switches the reader to another track (keeping the main
    /// demuxer's anchor, which a replacement mid-play needs), Off stops it, and a request queued
    /// before an engine start/stop is dropped as stale.
    #[test]
    fn a_synced_pick_switches_then_stops_the_reader_and_a_stale_one_is_dropped() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let port = 1;
            let anchor = || *FEED_SHARED.side_anchor.lock().unwrap();
            *FEED_SHARED.side_anchor.lock().unwrap() = Some(Anchor::of(0, &[1, 2, 3]));
            assert!(start(spec(port), &JOB));
            let epoch = EPOCH.load(Ordering::Acquire);
            // another track: replaced, still active, anchor kept
            apply(epoch, Some(Spec { ordinal: 3, ..spec(port) }), &JOB);
            assert!(active());
            assert_eq!(running().as_ref().map(|r| r.ordinal), Some(3));
            assert!(anchor().is_some(), "a replacement reader needs the anchor again");
            // the same track again: nothing to do
            apply(epoch, Some(Spec { ordinal: 3, ..spec(port) }), &JOB);
            assert_eq!(running().as_ref().map(|r| r.ordinal), Some(3));
            // Off / a sidecar: stopped, anchor still kept for a later pick
            apply(epoch, None, &JOB);
            assert!(!active());
            assert!(anchor().is_some());
            // an engine stop retires everything queued before it
            stop("engine");
            assert!(anchor().is_none(), "the engine's stop clears the anchor");
            apply(epoch, Some(spec(port)), &JOB);
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
            let port = 1;
            *FEED_SHARED.side_anchor.lock().unwrap() = None;
            // engine stop, with and without a reader still registered
            assert!(start(spec(port), &JOB));
            raise_failure();
            stop("test-engine");
            assert!(!take_failure(), "stop clears a pending failure");
            raise_failure();
            stop("test-nothing-running");
            assert!(!take_failure(), "even with no reader left to stop");
            // a sync that stops the reader (the viewer picked a sidecar or Off)
            assert!(start(spec(port), &JOB));
            let epoch = EPOCH.load(Ordering::Acquire);
            raise_failure();
            apply(epoch, None, &JOB);
            assert!(!active());
            assert!(!take_failure(), "a sync that stops the reader clears its failure");
            stop("test-cleanup");
        });
    }
    /// What the player reads of the reader (the draw gate, the sidecar clock, the failure edge) is
    /// the feed's sample, not a call into this module.
    #[test]
    fn the_feed_sample_carries_the_readers_facts() {
        let _g = plx_base::testlock::serial();
        stop("test-before");
        let raw = || crate::feed_pump::native_feed_facts().raw;
        let quiet = raw();
        assert_eq!((quiet.side_reader_active, quiet.side_failures, quiet.side_exact_delta_ns), (false, 0, None));
        publish_exact(-5);
        raise_failure();
        let seen = raw();
        assert_eq!(seen.side_exact_delta_ns, Some(-5));
        assert_ne!(seen.side_failures, 0, "a raise nobody stopped stands");
        stop("test-after");
        let gone = raw();
        assert_eq!((gone.side_failures, gone.side_exact_delta_ns), (0, None), "a stop voids both");
    }

    #[test]
    fn stop_without_a_reader_is_a_no_op() {
        let _g = plx_base::testlock::serial();
        stop("nothing");
        assert!(!active());
    }
}
