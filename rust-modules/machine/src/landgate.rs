//! **The landing schedule** (restructure spec §3.3 step 3, §5.5): during REPLAY, a live result
//! is delivered on the frame the recording delivered it on, not on whichever frame the worker
//! thread happened to finish.
//!
//! §3.3 step 3 says adapter results are keyed by `(completion_frame, adapter_rank,
//! arrival_index)` and that `completion_frame` is "recorded, so thread scheduling is
//! irrelevant". Nothing enforced that half: the stores that land OUTSIDE the dispatcher's drain
//! (`stores::metadata::pump_detail` and its siblings, `person`, `viewstate`, `browse`, `search`,
//! and Home's hubs through `bridge::take_live_results`) poll their own mailboxes every frame, so
//! the frame a landing was OBSERVED on was whatever the network and the scheduler produced.
//! Measured 2026-09-10: the one recorded `async` record of flow 12 sat on frame 1 in the
//! recording and on frame 0 in every replay, and because a spring started one frame earlier
//! never re-converges bit for bit, 927 of 928 frames diverged.
//!
//! **The mechanism, in one sentence:** a landing SITE asks this module before it consumes its
//! mailbox, and while the store's next recorded landing frame is still in the future the answer
//! is "not yet" and the record stays where it is.
//!
//! * A landing that arrives EARLY waits — it is consumed on its recorded frame instead.
//! * On the frame a landing is DUE, the site polls its mailbox for a bounded moment
//!   ([`WAIT_POLLS`]) before giving up, because a worker that was fast enough when the recording
//!   was taken can be a frame slower here. Holding alone fixes only one of the two directions,
//!   and the other is a coin flip: measured on flow 12 with the hold alone, Metadata landed a
//!   frame late and Person two, and every frame after that diverged.
//! * A landing that is still not there when the budget is spent is delivered whenever it does
//!   arrive and counted as `late` — that is the pre-phase-11 behaviour, kept as the fallback.
//! * A landing the recording never saw at all is delivered at once and counted as `extra`.
//! * A recorded landing that never arrives is counted as `missing` when the replay ends.
//!
//! **The due-frame wait is REPLAY-ONLY, bounded and taken at most once per (frame, owner, store)**
//! (dump mode's `take_owed` wait is a separate poll with its own fail-closed timeout, below), so a store
//! whose landing is due at a site that will not produce it cannot spend the budget twice on one
//! frame. It is a stop-gap for one specific reason, worth saying plainly: the honest end state is
//! §5.5's closed replay, where the recorded payload is INJECTED and no fetch happens at all. That
//! needs every store's result codec and its client bindings, which this lane does not build; until
//! it exists, a replay's stores fetch live and this is what keeps their frames comparable.
//!
//! Each [`Gate`] belongs to one `Stores` owner, and is keyed per STORE ORDINAL inside that owner:
//! the schedule is that store's `(frame, count)` pairs, and every arrival at any of its sites
//! consumes one unit. The COUNT is load-bearing and a boolean was
//! measured wrong first — `person` has one mailbox per fetch and took four answers over four
//! frames when the recording was made and over three when it was replayed, so a per-frame boolean
//! let one replay frame consume two arrivals for one cursor step, after which every later landing
//! of that store read as `late` for the rest of the run. With counts the second arrival on a
//! frame whose unit is spent is simply HELD to the next recorded frame, which is where it was.
//!
//! **It is a hold, never a spawn barrier.** The gate wraps the mailbox take alone, so the pumps'
//! other halves — the retry countdowns, `maybe_spawn`, the debounce, the roster sync — run on
//! their recorded frames exactly as they did. Gating a whole `pump()` would have suppressed the
//! spawn that produces the very landing being waited for.
//!
//! **Dump mode** ([`Gate::arm_dump`], the deterministic frame dump) is the third way to arm a
//! gate, and the one that needs no recording. The replay gate above keys a landing to a RECORDED
//! frame; a dump has no recording, so it aims to make the landing frame a function of the request
//! instead: a request issued in iteration k lands on the next execution of its site, whatever the
//! worker's timing, PROVIDED the request is not superseded before it lands (see the known hole
//! below). A site that asks through [`Gate::take_owed`] / [`Gate::take_all_owed`] says whether a
//! request it spawned is still unanswered (`owed`, the store's own single-flight claim); the take
//! polls until mail arrives, and the claim is released BY THE TAKE, never when the worker
//! finishes (for `stores::Fetch` that is how it already worked). The wait is a poll count of
//! [`WAIT_STEP_US`] sleeps (at least the timeout in real time) and fails closed: past it the gate
//! panics naming the store rather than let a frame be written without the answer.
//!
//! **Which sites are protected: only FOUR, Person, Collection, Search and SubtitleSearch.** The
//! claim is per MAILBOX (a store-wide claim would make one mailbox's pump wait on another's), and
//! only those four stores have one converted. Metadata (detail, alt-sources, season), Hubs,
//! Browse and ViewState still do a SINGLE poll in dump mode, as before, so a landing at any of
//! them still follows the worker's timing (`Bridge::take_live_results` makes the Hubs and Browse
//! discovery takes among them); so do that function's session adapter drains (results, capture,
//! commits, erase), which do not go through this gate at all. Arming dump mode therefore does NOT yet make a run
//! deterministic. S5a-2 and S5a-3 convert the rest; a plain `take` under dump mode is not yet
//! counted or refused.
//!
//! **KNOWN HOLE (fixed in S5a-2): a superseded request can still land the new one by timing.**
//! The converted sites supersede by `clear()` plus a generation bump while the old worker is
//! still out, and `stores::Fetch::take` releases the claim on mail of ANY generation. So if the
//! stale answer reaches the mailbox before the new request's, `take_owed` returns it (the site
//! drops it on its generation), the new request's claim is already released, and the new answer
//! lands on a later execution, by worker timing. `owed` cannot tell a stale answer from the one it
//! is waiting for. The sites a storyboard supersedes at are exactly the ones a video shows
//! (search typing, person-to-person navigation). The ignored test
//! `dump_mode_a_superseded_requests_stale_answer_does_not_release_the_new_claim` (plx_data,
//! `search_dump_tests.rs`) reproduces it.
//!
//! What the dump driver (the app layer, later) must also do, none of it possible down here:
//! within one iteration run the takes, then the busy/debt sample, then the draw; and override the
//! clocks the app owns that a held virtual clock freezes: the poster's `P_RETRY` backoff
//! (`src/app/adapters/poster.rs`, `retry_at`) and evict cooldown run on `app::clock::now()`, so
//! under a held clock a retry is never due. In a dump a failed image or fetch is a bug (the mock
//! is local), so the driver must fail fast on it instead of waiting for the retry.
//!
//! **Cost outside a recording, a replay or a dump is one relaxed `AtomicBool` load** (`Gate`'s armed flag): the
//! helpers return the closure's own answer and never take the lock.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::machine::StoreOrd;

/// Why a landing did not match its recorded frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Diff {
    /// The recording had this store landing on an EARLIER frame; the worker was slower here.
    Late,
    /// The recording had no landing left for this store at all.
    Extra,
    /// The recording had a landing this run never produced (reported when the replay ends).
    Missing,
}

impl Diff {
    pub fn name(self) -> &'static str {
        match self {
            Diff::Late => "late",
            Diff::Extra => "extra",
            Diff::Missing => "missing",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    /// Stamp every consumed landing with the frame it was consumed on.
    Recording,
    /// Hold an early landing to its recorded frame and grade the rest.
    Replaying,
    /// DUMP MODE: a site that asks through [`Gate::take_owed`] waits (bounded, fail-closed) for
    /// the answer its own request owes instead of taking whatever has arrived.
    Dumping(std::time::Duration),
}

struct State {
    mode: Mode,
    frame: u64,
    /// Ordinals that consumed a landing on the current frame (recording: the frame's record).
    lands: BTreeMap<u32,u32>,
    /// Replay: each ordinal's recorded `(frame, arrivals left on it)`, oldest first.
    sched: BTreeMap<u32,VecDeque<(u64, u32)>>,
    /// Replay: the ordinal already spent its due-frame wait budget on the current frame.
    waited: BTreeSet<u32>,
    /// Replay: mismatches observed since the last drain, `(frame, ordinal, why)`.
    diffs: Vec<(u64, u32, Diff)>,
}

/// A bound on the mismatch buffer, so a replay of a build whose stores land continuously cannot
/// grow this without limit between drains. The COUNT is kept whole in the driver.
const MAX_DIFFS: usize = 64;

/// How many times a site polls its mailbox on the frame a landing is DUE, and how long it waits
/// between polls: 400 x 500 µs, so at most ~200 ms per (frame, store). A count rather than a
/// deadline deliberately — `ci/check-deps.sh`'s `wall` rule keeps `Instant::now` out of `ui/`,
/// and a replay's budget has no business reading a clock the recording does not own.
const WAIT_POLLS: usize = 400;
const WAIT_STEP_US: u64 = 500;

/// What the gate says about one site's take on this frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// The recorded frame is still ahead: leave the mailbox alone.
    Hold,
    /// This IS the recorded frame: poll, and wait a bounded moment if nothing is there yet.
    Due,
    /// Nothing recorded, or this store already landed on this frame: take it as it comes.
    Free,
}

/// The fast path's only cost: `false` in every ordinary build and every ordinary boot.
pub struct Gate {
    armed: AtomicBool,
    state: Mutex<State>,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            state: Mutex::new(State { mode: Mode::Off, frame: 0, lands: BTreeMap::new(),
                sched: BTreeMap::new(), waited: BTreeSet::new(), diffs: Vec::new() }),
        }
    }
}

impl Gate {
fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
    let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// Arm the RECORDING half: from here every consumed landing is stamped with its frame.
pub fn arm_recording(&self) {
    self.with(|s| {
        *s = State { mode: Mode::Recording, frame: 0, lands: BTreeMap::new(), sched: BTreeMap::new(),
            waited: BTreeSet::new(), diffs: Vec::new() };
    });
    self.armed.store(true, Ordering::Relaxed);
}

/// Arm the REPLAY half with the recorded schedule: `sched[ord]` is that ordinal's recorded
/// `(frame, arrivals on it)` pairs, in order.
#[cfg(any(test, feature = "test-support"))]
pub fn arm_replay(&self, sched: Vec<Vec<(u64, u32)>>) {
    self.arm_sparse_replay(sched.into_iter().enumerate().map(|(i,queue)| (i as u32,queue)).collect());
}

/// Schedule storage is proportional to recorded entries, never the numeric ordinal. This is
/// the same landing policy as the dense fixture spelling, with full-width identities retained.
pub fn arm_sparse_replay(&self, sched: BTreeMap<u32,Vec<(u64,u32)>>) {
    self.with(|s| {
        *s = State {
            mode: Mode::Replaying,
            frame: 0,
            lands: BTreeMap::new(),
            sched: sched.into_iter().map(|(ord,queue)| (ord,VecDeque::from(queue))).collect(),
            waited: BTreeSet::new(),
            diffs: Vec::new(),
        };
    });
    self.armed.store(true, Ordering::Relaxed);
}

/// Arm DUMP MODE (the deterministic frame dump): from here a landing site that consumes its
/// mailbox through [`Gate::take_owed`] / [`Gate::take_all_owed`] blocks until mail arrives for the
/// claim it owes (it has been TAKEN), or a poll-count timeout of `timeout` passes, which panics
/// naming the store (fail closed: a frame written past a missing answer would be a
/// nondeterministic frame). Only the sites converted to `take_owed` wait (Person, Collection,
/// Search, SubtitleSearch); every other site still takes as before. [`Gate::disarm`] ends it.
///
/// **Panics if a recording or a replay is armed on this gate**, the way the recorder and replayer
/// refuse to be armed together: arming dump over one would silently wipe its schedule or its
/// landings. Disarm first. Arming dump again over a dump just resets it.
///
/// This is a runtime switch, off by default, armed only by the dump driver. Nothing in a shipping
/// loop calls it, and an unarmed gate stays one relaxed load.
pub fn arm_dump(&self, timeout: std::time::Duration) {
    self.with(|s| {
        assert!(matches!(s.mode, Mode::Off | Mode::Dumping(_)),
            "landgate: arm_dump on a gate with a recording or a replay already armed; disarm it first");
        *s = State { mode: Mode::Dumping(timeout), frame: 0, lands: BTreeMap::new(), sched: BTreeMap::new(),
            waited: BTreeSet::new(), diffs: Vec::new() };
    });
    self.armed.store(true, Ordering::Relaxed);
}

fn dump_timeout(&self) -> Option<std::time::Duration> {
    self.with(|s| match s.mode { Mode::Dumping(t) => Some(t), _ => None })
}

/// A site that OWES an answer: `owed` says whether a request this site spawned has not been taken
/// yet. The claim is the store's own in-flight state, marked when the request is spawned on the
/// main thread and released by the take that consumes its mail (`stores::Fetch::busy` / `take`),
/// never when the worker finishes, so it is per MAILBOX; a store-wide claim would make the
/// detail pump wait on a season request's mailbox.
///
/// Off dump mode this is [`Gate::take`]. In dump mode it polls `f` until it answers, or `owed`
/// reads false after a poll (the request was superseded or abandoned, so nothing will land), or
/// the dump timeout passes (panic naming `name`).
///
/// **KNOWN HOLE (fixed in S5a-2):** it returns on the FIRST `Some`, and cannot tell a superseded
/// request's stale answer from the one it is waiting for. With `stores::Fetch` that stale answer
/// also releases the NEWER request's claim, so the new answer lands on a later call by worker
/// timing. The guarantee holds only for a request that is not superseded before it lands.
pub fn take_owed<T>(&self, ord: StoreOrd, name: &str, owed: impl Fn() -> bool,
    mut f: impl FnMut() -> Option<T>) -> Option<T> {
    if !self.armed.load(Ordering::Relaxed) {
        return f();
    }
    let Some(timeout) = self.dump_timeout() else { return self.take(ord, f) };
    let mut polls = 0u64;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if !owed() {
            return None;
        }
        Self::dump_wait(name, &mut polls, timeout);
    }
}

/// [`Gate::take_owed`] for a site that drains a QUEUE: every batch is collected until the
/// requests the site owes have all been taken. No site calls it yet, and its `owed` cannot see
/// the batch taken so far, so a site whose done-marker is written after the take cannot use it
/// as is. It carries the same stale-answer KNOWN HOLE as [`Gate::take_owed`].
pub fn take_all_owed<T>(&self, ord: StoreOrd, name: &str, owed: impl Fn() -> bool,
    mut f: impl FnMut() -> Vec<T>) -> Vec<T> {
    if !self.armed.load(Ordering::Relaxed) {
        return f();
    }
    let Some(timeout) = self.dump_timeout() else { return self.take_all(ord, f) };
    let mut polls = 0u64;
    let mut out = Vec::new();
    loop {
        out.extend(f());
        if !owed() {
            return out;
        }
        Self::dump_wait(name, &mut polls, timeout);
    }
}

/// One poll interval of a dump wait, or the fail-closed panic once `timeout` has passed. The
/// timeout is a poll COUNT of [`WAIT_STEP_US`] sleeps, like [`WAIT_POLLS`] and for the same reason
/// (`ci/check-deps.sh`'s `wall` rule keeps wall-clock reads out of this crate), so the real wait
/// is the timeout or a little more, never less.
fn dump_wait(name: &str, polls: &mut u64, timeout: std::time::Duration) {
    if *polls >= (timeout.as_micros() as u64 / WAIT_STEP_US).max(1) {
        panic!("landgate dump mode: the {name} store still owes an answer after {timeout:?}; failing closed");
    }
    *polls += 1;
    std::thread::sleep(std::time::Duration::from_micros(WAIT_STEP_US));
}

/// Disarm: the ordinary state, and what a host test restores.
pub fn disarm(&self) {
    self.armed.store(false, Ordering::Relaxed);
    self.with(|s| {
        *s = State { mode: Mode::Off, frame: 0, lands: BTreeMap::new(), sched: BTreeMap::new(),
            waited: BTreeSet::new(), diffs: Vec::new() };
    });
}

/// The frame the loop is about to run. Called once per iteration, before any landing site.
pub fn begin_frame(&self, f: u64) {
    if !self.armed.load(Ordering::Relaxed) {
        return;
    }
    self.with(|s| {
        s.frame = f;
        s.lands.clear();
        s.waited.clear();
    });
}

/// May this store consume a landing on this frame? `true` unless a replay is still waiting for
/// the frame the recording delivered this store's next landing on.
pub fn held(&self, ord: StoreOrd) -> bool {
    self.phase(ord) == Phase::Hold
}

/// The gate's whole answer for one site: hold, wait, or take. Claims the store's per-frame WAIT
/// budget as a side effect, so only the first site of a store to find itself due can spend it.
fn phase(&self, ord: StoreOrd) -> Phase {
    if !self.armed.load(Ordering::Relaxed) {
        return Phase::Free;
    }
    self.with(|s| {
        if s.mode != Mode::Replaying {
            return Phase::Free;
        }
        let i = ord.0;
        match s.sched.get(&i).and_then(|q| q.front()).map(|&(at, _)| at) {
            // still ahead of us: leave the mailbox alone until the frame comes round. A second
            // arrival on a frame whose recorded arrivals are all spent is held here too — that is
            // the count doing its work.
            Some(at) if at > s.frame => Phase::Hold,
            Some(at) if at == s.frame => {
                if !s.waited.insert(i) {
                    Phase::Free // this store already spent its wait on this frame
                } else {
                    Phase::Due
                }
            }
            // the recorded frame has already passed (a late arrival), or nothing is recorded at
            // all: take it as it comes and grade it when it lands
            _ => Phase::Free,
        }
    })
}

/// The due-frame poll: the mailbox for a bounded moment, never holding this module's lock.
fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..WAIT_POLLS {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(std::time::Duration::from_micros(WAIT_STEP_US));
    }
    None
}

/// This store consumed one batch on this frame. Each consumed batch advances its count once;
/// supplied-result ingress reports the same batch boundary without polling a live mailbox.
pub fn landed(&self, ord: StoreOrd) {
    if !self.armed.load(Ordering::Relaxed) {
        return;
    }
    self.with(|s| {
        let i = ord.0;
        match s.mode {
            Mode::Recording => {
                *s.lands.entry(i).or_default() += 1;
                return;
            }
            // dump mode records nothing and grades nothing
            Mode::Off | Mode::Dumping(_) => return,
            Mode::Replaying => {}
        }
        let frame = s.frame;
        let why = match s.sched.get_mut(&i).and_then(|q| q.front_mut()) {
            Some((at, left)) => {
                let late = *at < frame; // `*at > frame` cannot happen: `phase` held the site
                *left -= 1;
                if *left == 0 {
                    s.sched.get_mut(&i).expect("current schedule").pop_front();
                }
                if late { Some(Diff::Late) } else { None }
            }
            None => Some(Diff::Extra),
        };
        if let Some(why) = why {
            if s.diffs.len() < MAX_DIFFS {
                s.diffs.push((frame, ord.0, why));
            }
        }
    });
}

/// Consume a one-slot mailbox under the gate: `None` while held, and a `Some` answer is the
/// frame's landing for this store.
pub fn take<T>(&self, ord: StoreOrd, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    if !self.armed.load(Ordering::Relaxed) {
        return f();
    }
    let got = match self.phase(ord) {
        Phase::Hold => return None,
        Phase::Due => Self::wait_for(f),
        Phase::Free => f(),
    };
    if got.is_some() {
        self.landed(ord);
    }
    got
}

/// The same for a site that drains a QUEUE: an empty answer is not a landing.
pub fn take_all<T>(&self, ord: StoreOrd, mut f: impl FnMut() -> Vec<T>) -> Vec<T> {
    if !self.armed.load(Ordering::Relaxed) {
        return f();
    }
    let got = match self.phase(ord) {
        Phase::Hold => return Vec::new(),
        Phase::Due => Self::wait_for(|| {
            let v = f();
            if v.is_empty() { None } else { Some(v) }
        })
        .unwrap_or_default(),
        Phase::Free => f(),
    };
    if !got.is_empty() {
        self.landed(ord);
    }
    got
}

/// RECORDING: `(ordinal, arrivals)` for the frame just finished, for the recorder's records.
pub fn take_frame_lands(&self) -> Vec<(StoreOrd, u32)> {
    if !self.armed.load(Ordering::Relaxed) {
        return Vec::new();
    }
    self.with(|s| {
        let mut out = Vec::new();
        for (i, landed) in s.lands.iter_mut() {
            let n = std::mem::take(landed);
            if n > 0 {
                out.push((StoreOrd(*i), n));
            }
        }
        out
    })
}

/// REPLAY: the mismatches observed since the last drain, and whether any were dropped by the
/// buffer's bound (the count the driver keeps is its own).
pub fn take_diffs(&self) -> Vec<(u64, u32, Diff)> {
    if !self.armed.load(Ordering::Relaxed) {
        return Vec::new();
    }
    self.with(|s| std::mem::take(&mut s.diffs))
}

/// REPLAY, at the end: every recorded landing this run never produced.
#[cfg(any(test, feature = "test-support"))]
pub fn unmatched(&self) -> Vec<(u32, u64)> {
    self.unmatched_counts().into_iter().flat_map(|(ord,frame,count)| std::iter::repeat_n((ord,frame),count as usize)).collect()
}

/// Missing counts remain aggregated: a corrupt large count cannot request that many entries.
pub fn unmatched_counts(&self) -> Vec<(u32,u64,u32)> {
    if !self.armed.load(Ordering::Relaxed) {
        return Vec::new();
    }
    self.with(|s| {
        let mut out = Vec::new();
        for (i, q) in s.sched.iter_mut() {
            while let Some((at, left)) = q.pop_front() {
                out.push((*i,at,left));
            }
        }
        out
    })
}
}

// Compatibility owner for focused gate tests and data-layer fixtures that do not construct a
// production `Stores` aggregate. The application never routes Bridge landings through this value.
#[cfg(any(test, feature = "test-support"))]
static FIXTURE_GATE: std::sync::LazyLock<Gate> = std::sync::LazyLock::new(Gate::default);
#[cfg(any(test, feature = "test-support"))]
pub fn fixture_gate() -> &'static Gate { &FIXTURE_GATE }

#[cfg(any(test, feature = "test-support"))]
pub fn arm_recording() { FIXTURE_GATE.arm_recording(); }
#[cfg(any(test, feature = "test-support"))]
pub fn arm_replay(sched: Vec<Vec<(u64, u32)>>) { FIXTURE_GATE.arm_replay(sched); }
#[cfg(any(test, feature = "test-support"))]
pub fn arm_sparse_replay(sched: BTreeMap<u32,Vec<(u64,u32)>>) { FIXTURE_GATE.arm_sparse_replay(sched); }
#[cfg(any(test, feature = "test-support"))]
pub fn arm_dump(timeout: std::time::Duration) { FIXTURE_GATE.arm_dump(timeout); }
#[cfg(any(test, feature = "test-support"))]
pub fn take_owed<T>(ord: StoreOrd, name: &str, owed: impl Fn() -> bool, f: impl FnMut() -> Option<T>) -> Option<T> {
    FIXTURE_GATE.take_owed(ord, name, owed, f)
}
#[cfg(any(test, feature = "test-support"))]
pub fn take_all_owed<T>(ord: StoreOrd, name: &str, owed: impl Fn() -> bool, f: impl FnMut() -> Vec<T>) -> Vec<T> {
    FIXTURE_GATE.take_all_owed(ord, name, owed, f)
}
#[cfg(any(test, feature = "test-support"))]
pub fn disarm() { FIXTURE_GATE.disarm(); }
#[cfg(any(test, feature = "test-support"))]
pub fn begin_frame(f: u64) { FIXTURE_GATE.begin_frame(f); }
#[cfg(any(test, feature = "test-support"))]
pub fn held(ord: StoreOrd) -> bool { FIXTURE_GATE.held(ord) }
#[cfg(test)]
fn phase(ord: StoreOrd) -> Phase { FIXTURE_GATE.phase(ord) }
#[cfg(any(test, feature = "test-support"))]
pub fn landed(ord: StoreOrd) { FIXTURE_GATE.landed(ord); }
#[cfg(any(test, feature = "test-support"))]
pub fn take<T>(ord: StoreOrd, f: impl FnMut() -> Option<T>) -> Option<T> { FIXTURE_GATE.take(ord, f) }
#[cfg(any(test, feature = "test-support"))]
pub fn take_all<T>(ord: StoreOrd, f: impl FnMut() -> Vec<T>) -> Vec<T> { FIXTURE_GATE.take_all(ord, f) }
#[cfg(any(test, feature = "test-support"))]
pub fn take_frame_lands() -> Vec<(StoreOrd, u32)> { FIXTURE_GATE.take_frame_lands() }
#[cfg(any(test, feature = "test-support"))]
pub fn take_diffs() -> Vec<(u64, u32, Diff)> { FIXTURE_GATE.take_diffs() }
#[cfg(any(test, feature = "test-support"))]
pub fn unmatched() -> Vec<(u32, u64)> { FIXTURE_GATE.unmatched() }
#[cfg(any(test, feature = "test-support"))]
pub fn unmatched_counts() -> Vec<(u32,u64,u32)> { FIXTURE_GATE.unmatched_counts() }

/// A fixture's promise that its compatibility gate is disarmed again even if assertions panic.
#[cfg(any(test, feature = "test-support"))]
pub struct Armed;
#[cfg(any(test, feature = "test-support"))]
impl Drop for Armed {
    fn drop(&mut self) {
        disarm();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: StoreOrd = StoreOrd(1);
    const B: StoreOrd = StoreOrd(3);

    /// The whole promise of §3.3 step 3 in one test: a result the worker produced before its
    /// recorded frame is NOT observed until that frame comes round.
    #[test]
    fn a_landing_is_delivered_on_its_recorded_frame_during_replay() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(3, 1)]]);
        // ONE result, in the mailbox since frame 0 — the worker finished long before its frame
        let mailbox = std::cell::Cell::new(Some("result"));
        let mut delivered = None;
        for f in 0..=4u64 {
            begin_frame(f);
            if let Some(v) = take(A, || mailbox.take()) {
                delivered = Some((f, v));
            }
        }
        assert_eq!(delivered, Some((3, "result")), "delivered on the RECORDED frame");
        assert!(take_diffs().is_empty(), "an on-time landing is not a divergence");
        assert!(unmatched().is_empty());
    }

    #[test]
    fn a_late_landing_is_delivered_and_counted_as_a_divergence() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(3, 1)]]);
        for f in 0..=5u64 {
            begin_frame(f);
            // nothing arrives until frame 5 — holding cannot manufacture a result
            let got = take(A, || if f >= 5 { Some(()) } else { None });
            assert_eq!(got.is_some(), f == 5);
        }
        assert_eq!(take_diffs(), vec![(5, 1, Diff::Late)]);
        assert!(unmatched().is_empty(), "the cursor was consumed by the late arrival");
    }

    /// The other direction, and the one holding alone cannot fix: on the frame a landing is DUE
    /// the site waits a bounded moment for a worker that is slower here than it was when the
    /// recording was taken. Without it, flow 12 had Metadata a frame late and Person two, and
    /// every frame after that diverged.
    #[test]
    fn a_landing_that_arrives_during_its_due_frame_is_still_taken_on_that_frame() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(2, 1)]]);
        let mailbox = std::sync::Arc::new(Mutex::new(None::<&'static str>));
        let worker = std::sync::Arc::clone(&mailbox);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(15));
            *worker.lock().unwrap() = Some("result");
        });
        let mut delivered = None;
        for f in 0..=3u64 {
            begin_frame(f);
            if let Some(v) = take(A, || mailbox.lock().unwrap().take()) {
                delivered = Some((f, v));
            }
        }
        assert_eq!(delivered, Some((2, "result")), "the due frame waited for it");
        assert!(take_diffs().is_empty(), "a landing waited for is not a divergence");
    }

    /// …and the wait is claimed once per (frame, store), so a store whose landing is due at a
    /// site that will never produce it cannot spend the budget twice on one frame.
    #[test]
    fn the_due_frame_wait_is_spent_once_per_store_per_frame() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(1, 1)]]);
        begin_frame(1);
        assert_eq!(phase(A), Phase::Due);
        assert_eq!(phase(A), Phase::Free, "the budget is claimed, not shared");
        assert_eq!(phase(B), Phase::Free, "another store is unaffected");
    }

    #[test]
    fn an_unrecorded_landing_is_extra_and_a_never_arriving_one_is_missing() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(2, 1)], vec![], vec![(4, 1)]]);
        begin_frame(0);
        assert!(take(A, || Some(())).is_none(), "held: the recording says frame 2");
        begin_frame(2);
        assert!(take(A, || Some(())).is_some());
        begin_frame(3);
        assert!(take(A, || Some(())).is_some(), "nothing recorded left: delivered at once");
        assert_eq!(take_diffs(), vec![(3, 1, Diff::Extra)]);
        // B never lands at all
        assert_eq!(unmatched(), vec![(3, 4)]);
    }

    /// A store with several mailboxes: the COUNT decides how many of its sites may take on one
    /// frame. Two arrivals on a frame the recording gave one are not both delivered — the second
    /// is held to the frame the recording took it on, which is what keeps the cursor aligned for
    /// the rest of the run.
    #[test]
    fn a_frames_recorded_arrival_count_is_what_a_store_may_take_on_it() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(1, 2), (2, 1)]]);
        begin_frame(1);
        assert!(take(A, || Some(())).is_some());
        assert!(take(A, || Some(())).is_some(), "the frame recorded TWO arrivals");
        assert!(take(A, || Some(())).is_none(), "the third waits for frame 2");
        begin_frame(2);
        assert!(take(A, || Some(())).is_some());
        assert!(take_diffs().is_empty(), "three recorded arrivals, three taken");
        assert!(unmatched().is_empty());
    }

    #[test]
    fn recording_stamps_each_frames_landings_and_drains_them_once() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_recording();
        begin_frame(7);
        assert!(take(A, || Some(())).is_some(), "recording never holds");
        assert!(take_all(B, || vec![1, 2]).len() == 2);
        assert!(take_all(B, || Vec::<u32>::new()).is_empty());
        assert!(take_all(B, || vec![3]).len() == 1);
        let mut lands = take_frame_lands();
        lands.sort_by_key(|(o, _)| o.0);
        assert_eq!(lands, vec![(A, 1), (B, 2)], "arrivals are COUNTED, not flagged");
        assert!(take_frame_lands().is_empty(), "drained once");
    }

    // ---- dump mode (the deterministic frame dump) -------------------------------------------

    /// A fake one-slot store with the claim `stores::Fetch` has: marked at spawn on the main
    /// thread, released by the TAKE that consumes the mail (never by the worker finishing).
    #[derive(Default)]
    struct FakeStore {
        owed: AtomicBool,
        slot: Mutex<Option<u32>>,
    }

    impl FakeStore {
        fn spawn(self: &std::sync::Arc<Self>, answer: u32, delay: std::time::Duration) {
            self.owed.store(true, Ordering::SeqCst);
            let me = std::sync::Arc::clone(self);
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                *me.slot.lock().unwrap() = Some(answer);
            });
        }
        fn take(&self) -> Option<u32> {
            let v = self.slot.lock().unwrap().take()?;
            self.owed.store(false, Ordering::SeqCst);
            Some(v)
        }
        fn owed(&self) -> bool { self.owed.load(Ordering::SeqCst) }
    }

    /// One iteration's order is: takes, then the draw. Frame 1 issues a request AFTER its take; the
    /// answer is posted at a random point within the following iterations. In dump mode it must
    /// land on frame 2 for every one of 100 seeds, and the spring it starts must trace identically.
    #[test]
    fn a_request_lands_on_the_next_execution_of_its_site_whatever_the_worker_timing() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        let mut traces = Vec::new();
        for seed in 0..100u64 {
            arm_dump(std::time::Duration::from_secs(20));
            let store = std::sync::Arc::new(FakeStore::default());
            // a deterministic spread of 0..3 ms delays and 0..2 ms iteration jitter
            let mix = |n: u64| (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ n.wrapping_mul(0xBF58_476D_1CE4_E5B9)) >> 40;
            let delay = std::time::Duration::from_micros(mix(1) % 3000);
            let (mut landed_at, mut spring, mut trace) = (None, None::<(f32, f32)>, Vec::new());
            for f in 0..6u64 {
                begin_frame(f);
                if let Some(v) = take_owed(A, "fake", || store.owed(), || store.take()) {
                    assert_eq!(v, 7);
                    landed_at = Some(f);
                    spring = Some((0.0, 1.0)); // (position, velocity) from the landing's own frame
                }
                if let Some((x, v)) = spring.as_mut() {
                    *v += (1.0 - *x) * 0.25 - *v * 0.35;
                    *x += *v;
                    trace.push((*x).to_bits());
                }
                if f == 1 {
                    store.spawn(7, delay); // dispatched after this iteration's take
                }
                std::thread::sleep(std::time::Duration::from_micros(mix(10 + f) % 2000));
            }
            assert_eq!(landed_at, Some(2), "seed {seed}: the request must land on the next execution");
            traces.push(trace);
            disarm();
        }
        assert!(traces.windows(2).all(|w| w[0] == w[1]), "the spring trace is the same for every seed");
    }

    /// Outside dump mode the SAME site does not wait: the landing frame follows the worker. This
    /// is the failure dump mode exists to remove, so the contrast must be observable here. The
    /// worker is released by a channel after frame 0's take, so no clock decides the order.
    #[test]
    fn without_dump_mode_the_landing_frame_follows_the_worker() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_recording();
        let store = std::sync::Arc::new(FakeStore::default());
        store.owed.store(true, Ordering::SeqCst);
        let (release, held) = std::sync::mpsc::channel::<()>();
        let worker = std::sync::Arc::clone(&store);
        let join = std::thread::spawn(move || {
            held.recv().unwrap();
            *worker.slot.lock().unwrap() = Some(7);
        });
        begin_frame(0);
        assert!(take_owed(A, "fake", || store.owed(), || store.take()).is_none(), "nothing yet: no wait");
        release.send(()).unwrap();
        join.join().unwrap();
        begin_frame(1);
        assert_eq!(take_owed(A, "fake", || store.owed(), || store.take()), Some(7), "lands whenever it came");
    }

    /// A claim whose request was abandoned (superseded, or its screen left): the owner releases
    /// it, so the site's take returns at once instead of waiting for an answer nobody will post.
    #[test]
    fn an_abandoned_claim_causes_no_wait() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        // a short timeout: any wait for the abandoned request would fail closed (panic) here
        arm_dump(std::time::Duration::from_millis(100));
        let store = FakeStore::default();
        store.owed.store(true, Ordering::SeqCst);
        store.owed.store(false, Ordering::SeqCst); // the owner's supersede/clear
        begin_frame(0);
        assert!(take_owed(A, "fake", || store.owed(), || store.take()).is_none());
    }

    /// A claim that is never answered fails CLOSED, naming the store, instead of writing a frame.
    #[test]
    #[should_panic(expected = "the fake store still owes an answer")]
    fn an_unanswered_claim_fails_closed_naming_the_store() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_dump(std::time::Duration::from_millis(40));
        let store = FakeStore::default();
        store.owed.store(true, Ordering::SeqCst);
        begin_frame(0);
        let _ = take_owed(A, "fake", || store.owed(), || store.take());
    }

    /// Arming dump mode over a recording or a replay would silently wipe it: refused, loudly.
    #[test]
    #[should_panic(expected = "a recording or a replay already armed")]
    fn arm_dump_refuses_a_gate_that_is_recording() {
        let gate = Gate::default();
        gate.arm_recording();
        gate.arm_dump(std::time::Duration::from_secs(1));
    }

    #[test]
    #[should_panic(expected = "a recording or a replay already armed")]
    fn arm_dump_refuses_a_gate_that_is_replaying() {
        let gate = Gate::default();
        gate.arm_replay(vec![vec![(1, 1)]]);
        gate.arm_dump(std::time::Duration::from_secs(1));
    }

    /// Disarming first is the way through, and re-arming a dump is allowed.
    #[test]
    fn arm_dump_after_a_disarm_and_over_a_dump_is_allowed() {
        let gate = Gate::default();
        gate.arm_recording();
        gate.disarm();
        gate.arm_dump(std::time::Duration::from_secs(1));
        gate.arm_dump(std::time::Duration::from_secs(2));
    }

    /// A queue site collects every batch its claims owe, not just the first to arrive.
    #[test]
    fn a_queue_site_waits_for_every_request_it_owes() {
        let _g = plx_base::testlock::serial();
        let _armed = Armed;
        arm_dump(std::time::Duration::from_secs(20));
        let owed = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(2));
        let mail = std::sync::Arc::new(Mutex::new(Vec::<u32>::new()));
        for (n, delay) in [(1u32, 3u64), (2, 25)] {
            let mail = std::sync::Arc::clone(&mail);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                mail.lock().unwrap().push(n);
            });
        }
        begin_frame(0);
        let got = take_all_owed(B, "fake", || owed.load(Ordering::SeqCst) > 0, || {
            let v = std::mem::take(&mut *mail.lock().unwrap());
            owed.fetch_sub(v.len() as u32, Ordering::SeqCst);
            v
        });
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn a_disarmed_gate_is_a_pass_through() {
        let _g = plx_base::testlock::serial();
        disarm();
        assert!(!held(A));
        assert!(take(A, || Some(9)).is_some());
        assert!(take(A, || Some(())).is_some(), "and a third");
        assert!(take_frame_lands().is_empty());
        assert!(take_diffs().is_empty());
        assert!(unmatched().is_empty());
    }
}
