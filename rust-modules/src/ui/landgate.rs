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
//! **The wait is REPLAY-ONLY, bounded and taken at most once per (frame, store)**, so a store
//! whose landing is due at a site that will not produce it cannot spend the budget twice on one
//! frame. It is a stop-gap for one specific reason, worth saying plainly: the honest end state is
//! §5.5's closed replay, where the recorded payload is INJECTED and no fetch happens at all. That
//! needs every store's result codec and its client bindings, which this lane does not build; until
//! it exists, a replay's stores fetch live and this is what keeps their frames comparable.
//!
//! The gate is per STORE ORDINAL: the schedule is that store's `(frame, count)` pairs, and every
//! arrival at any of its sites consumes one unit. The COUNT is load-bearing and a boolean was
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
//! **Cost outside a recording or a replay is one relaxed `AtomicBool` load** ([`ARMED`]): the
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
static ARMED: AtomicBool = AtomicBool::new(false);

static STATE: Mutex<State> = Mutex::new(State {
    mode: Mode::Off,
    frame: 0,
    lands: BTreeMap::new(),
    sched: BTreeMap::new(),
    waited: BTreeSet::new(),
    diffs: Vec::new(),
});

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// Arm the RECORDING half: from here every consumed landing is stamped with its frame.
pub fn arm_recording() {
    with(|s| {
        *s = State { mode: Mode::Recording, frame: 0, lands: BTreeMap::new(), sched: BTreeMap::new(),
            waited: BTreeSet::new(), diffs: Vec::new() };
    });
    ARMED.store(true, Ordering::Relaxed);
}

/// Arm the REPLAY half with the recorded schedule: `sched[ord]` is that ordinal's recorded
/// `(frame, arrivals on it)` pairs, in order.
#[cfg(test)]
pub fn arm_replay(sched: Vec<Vec<(u64, u32)>>) {
    arm_sparse_replay(sched.into_iter().enumerate().map(|(i,queue)| (i as u32,queue)).collect());
}

/// Schedule storage is proportional to recorded entries, never the numeric ordinal. This is
/// the same landing policy as the dense fixture spelling, with full-width identities retained.
pub fn arm_sparse_replay(sched: BTreeMap<u32,Vec<(u64,u32)>>) {
    with(|s| {
        *s = State {
            mode: Mode::Replaying,
            frame: 0,
            lands: BTreeMap::new(),
            sched: sched.into_iter().map(|(ord,queue)| (ord,VecDeque::from(queue))).collect(),
            waited: BTreeSet::new(),
            diffs: Vec::new(),
        };
    });
    ARMED.store(true, Ordering::Relaxed);
}

/// Disarm: the ordinary state, and what a host test restores.
pub fn disarm() {
    ARMED.store(false, Ordering::Relaxed);
    with(|s| {
        *s = State { mode: Mode::Off, frame: 0, lands: BTreeMap::new(), sched: BTreeMap::new(),
            waited: BTreeSet::new(), diffs: Vec::new() };
    });
}

/// The frame the loop is about to run. Called once per iteration, before any landing site.
pub fn begin_frame(f: u64) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    with(|s| {
        s.frame = f;
        s.lands.clear();
        s.waited.clear();
    });
}

/// May this store consume a landing on this frame? `true` unless a replay is still waiting for
/// the frame the recording delivered this store's next landing on.
pub fn held(ord: StoreOrd) -> bool {
    phase(ord) == Phase::Hold
}

/// The gate's whole answer for one site: hold, wait, or take. Claims the store's per-frame WAIT
/// budget as a side effect, so only the first site of a store to find itself due can spend it.
fn phase(ord: StoreOrd) -> Phase {
    if !ARMED.load(Ordering::Relaxed) {
        return Phase::Free;
    }
    with(|s| {
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
pub fn landed(ord: StoreOrd) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    with(|s| {
        let i = ord.0;
        if s.mode == Mode::Recording {
            *s.lands.entry(i).or_default() += 1;
            return;
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
pub fn take<T>(ord: StoreOrd, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    if !ARMED.load(Ordering::Relaxed) {
        return f();
    }
    let got = match phase(ord) {
        Phase::Hold => return None,
        Phase::Due => wait_for(f),
        Phase::Free => f(),
    };
    if got.is_some() {
        landed(ord);
    }
    got
}

/// The same for a site that drains a QUEUE: an empty answer is not a landing.
pub fn take_all<T>(ord: StoreOrd, mut f: impl FnMut() -> Vec<T>) -> Vec<T> {
    if !ARMED.load(Ordering::Relaxed) {
        return f();
    }
    let got = match phase(ord) {
        Phase::Hold => return Vec::new(),
        Phase::Due => wait_for(|| {
            let v = f();
            if v.is_empty() { None } else { Some(v) }
        })
        .unwrap_or_default(),
        Phase::Free => f(),
    };
    if !got.is_empty() {
        landed(ord);
    }
    got
}

/// RECORDING: `(ordinal, arrivals)` for the frame just finished, for the recorder's records.
pub fn take_frame_lands() -> Vec<(StoreOrd, u32)> {
    if !ARMED.load(Ordering::Relaxed) {
        return Vec::new();
    }
    with(|s| {
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
pub fn take_diffs() -> Vec<(u64, u32, Diff)> {
    if !ARMED.load(Ordering::Relaxed) {
        return Vec::new();
    }
    with(|s| std::mem::take(&mut s.diffs))
}

/// REPLAY, at the end: every recorded landing this run never produced.
#[cfg(test)]
pub fn unmatched() -> Vec<(u32, u64)> {
    unmatched_counts().into_iter().flat_map(|(ord,frame,count)| std::iter::repeat_n((ord,frame),count as usize)).collect()
}

/// Missing counts remain aggregated: a corrupt large count cannot request that many entries.
pub fn unmatched_counts() -> Vec<(u32,u64,u32)> {
    if !ARMED.load(Ordering::Relaxed) {
        return Vec::new();
    }
    with(|s| {
        let mut out = Vec::new();
        for (i, q) in s.sched.iter_mut() {
            while let Some((at, left)) = q.pop_front() {
                out.push((*i,at,left));
            }
        }
        out
    })
}

/// A test's promise that the gate is disarmed again even if its assertions panic — the state is
/// process-global, so an armed gate left behind would hold another module's pumps.
#[cfg(test)]
pub(crate) struct Armed;
#[cfg(test)]
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
        let _g = crate::testlock::serial();
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
        let _g = crate::testlock::serial();
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
        let _g = crate::testlock::serial();
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
        let _g = crate::testlock::serial();
        let _armed = Armed;
        arm_replay(vec![vec![], vec![(1, 1)]]);
        begin_frame(1);
        assert_eq!(phase(A), Phase::Due);
        assert_eq!(phase(A), Phase::Free, "the budget is claimed, not shared");
        assert_eq!(phase(B), Phase::Free, "another store is unaffected");
    }

    #[test]
    fn an_unrecorded_landing_is_extra_and_a_never_arriving_one_is_missing() {
        let _g = crate::testlock::serial();
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
        let _g = crate::testlock::serial();
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
        let _g = crate::testlock::serial();
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

    #[test]
    fn a_disarmed_gate_is_a_pass_through() {
        let _g = crate::testlock::serial();
        disarm();
        assert!(!held(A));
        assert!(take(A, || Some(9)).is_some());
        assert!(take(A, || Some(())).is_some(), "and a third");
        assert!(take_frame_lands().is_empty());
        assert!(take_diffs().is_empty());
        assert!(unmatched().is_empty());
    }
}
