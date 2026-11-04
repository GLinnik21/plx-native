//! The ACB video-plane owner: ONE backend-neutral state for the one binding the television's
//! `libAcbAPI` handle can hold, and a release that waits for the firmware's completion callback.
//!
//! **Why.** The handle is created and initialized once per process (`AcbAPI_create` +
//! `AcbAPI_initialize`; a repeat initialize returns 0). `setMediaId` may be called again on the same
//! handle after UNLOADED, but only once the previous binding is really gone, and the only thing
//! that says so is the completion callback of `AcbAPI_setState(h, FG, UNLOADED, &task)`:
//! it carries the SAME task id that call returned, event type 1 (`-2` timeout, `-1` error,
//! `0` cancel, `1` complete). LOADED, PLAYING, window and video-data tasks also complete with
//! event 1, so the fence is matched on the TASK ID, never on the event alone, and the callback's
//! playState argument (a cached value) is never read. If `setState` answers `-2` (not registered)
//! nothing was sent and no callback will come. The evidence for both rules (client and service
//! addresses, the device log, and what is not determined) is
//! `docs/measurements/acb-release-fence-2026-11-04.md`.
//!
//! **Shape.** [`Plane`] is the pure machine (no clock, no global, no IO: time arrives as `now_ms`
//! in events, work leaves as [`Fx`]). [`PlaneOwner`] is its runner: the single holder of the
//! machine, the only consumer of the sink's completion mailbox ([`CompletionQueue`], the shape of
//! `uri::ReplyQueue`), and the one monotonic clock the machine's deadlines are measured on. Both
//! backends (Feed's `plane_bind`/`plane_unload`, the URI probe's ACB experiment) go through it, so
//! "one binding at a time" holds across them. The mailbox static itself lives in the webOS port
//! (`webos::video_sink`), which is the only place that has a library thread to receive from.
use super::sink::{DisplaySink, PlaneUnload};
use plx_base::task::MainThread;
use std::collections::VecDeque;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// How long a release may wait for its completion callback before the plane is declared
/// quarantined. The device logs show the callback within ~100 ms of the dispatch
/// (`acb unloaded rv=1 task=5` then `acb callback task=5 ev=1`), so this is generous; it is also
/// the worst-case latency one rebind adds on a firmware whose callback never arrives (Feed then
/// binds anyway, see [`BindPolicy::Rebind`]).
pub const RELEASE_FENCE_MS: u64 = 2_000;
/// Completions that arrived while no release was waiting for them, kept so a callback that beat
/// the main thread's bookkeeping still matches. LOADED, PLAYING, pause, window and video-data
/// tasks complete too, so the oldest are forgotten; the race this covers spans one frame.
pub const MAX_EARLY_COMPLETIONS: usize = 8;
/// `setState`'s answer when the handle holds no registration: nothing was sent, no callback comes.
pub const UNLOAD_NOT_REGISTERED: i32 = -2;
/// The `PLAYER_TYPE_MSE` the boot passes to `AcbAPI_initialize` unless `/tmp/plxnative-ptype` says otherwise.
pub const DEFAULT_PLAYER_TYPE: i32 = 10;

/// What the boot learned about this device's ACB handle. Owned by the [`PlaneOwner`] from the
/// `Boot` event on; it replaces the old `ACB_OK`/`PTYPE` statics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneFacts {
    /// `acb_create` produced an initialized handle (ACB is this device's video plane).
    pub handle: bool,
    /// The player type `AcbAPI_initialize` was given.
    pub player_type: i32,
}

impl PlaneFacts {
    /// Before the boot has run, and on a device whose plane is not ACB.
    pub const ABSENT: Self = Self { handle: false, player_type: DEFAULT_PLAYER_TYPE };
}

/// Why a plane is not usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// `setState(UNLOADED)` was refused (`rv` was neither accepted nor "not registered").
    UnloadRefused(i32),
    /// The unload's completion arrived and was not event 1.
    Completion(i32),
    /// No completion by the deadline.
    Deadline,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Why::UnloadRefused(rv) => write!(f, "unload-refused rv={rv}"),
            Why::Completion(ev) => write!(f, "completion ev={ev}"),
            Why::Deadline => f.write_str("no completion by the deadline"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// No ACB handle on this device (webOS 5 exported window, nothing, or the boot has not run).
    Unavailable,
    Idle,
    Bound { media_id: String },
    Releasing { task: i64, deadline_ms: u64 },
    Released,
    Quarantined { why: Why },
}

/// [`State`] without the payloads a caller does not need: what a backend asks the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Unavailable,
    Idle,
    Bound,
    Releasing,
    Released,
    Quarantined(Why),
}

impl State {
    pub fn phase(&self) -> Phase {
        match self {
            State::Unavailable => Phase::Unavailable,
            State::Idle => Phase::Idle,
            State::Bound { .. } => Phase::Bound,
            State::Releasing { .. } => Phase::Releasing,
            State::Released => Phase::Released,
            State::Quarantined { why } => Phase::Quarantined(*why),
        }
    }
}

/// What a bind does when the previous release is not proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindPolicy {
    /// The URI probe: wait for Released, refuse once Quarantined, refuse a second binding.
    Strict,
    /// Buffer-feed, which re-binds A->B->A on one handle title after title and has never waited
    /// on the library: wait for Released, but once the deadline has passed (or the release failed)
    /// bind anyway and say so. A firmware whose callback never arrives must still get a picture.
    Rebind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No ACB handle on this device.
    NoPlane,
    /// A binding already holds the plane.
    Bound,
    Quarantined(Why),
}

pub enum Event {
    Boot { facts: PlaneFacts },
    BindRequested { media_id: String, policy: BindPolicy },
    /// `setState(UNLOADED)` was called: its answer, the task id it wrote (trusted only for `rv == 1`)
    /// and when.
    UnloadDispatched { rv: i32, task: i64, now_ms: u64 },
    Completion { task: i64, ev: i32 },
    /// The mailbox lost `count` completions to overflow.
    Dropped { count: u64 },
    Tick { now_ms: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fx {
    /// Run the bind verbs now; the plane is this media id's.
    Bind { media_id: String },
    /// The previous release is still in flight: ask again later.
    Defer,
    Refuse(Refusal),
    /// A `Rebind` goes ahead over a plane whose release was never proven.
    OverQuarantine(Why),
    Released { task: i64 },
    Quarantined { why: Why },
    /// Completions the mailbox dropped. State is unchanged: only an explicit match releases.
    Lost { count: u64 },
}

pub struct Plane {
    facts: PlaneFacts,
    booted: bool,
    state: State,
    early: VecDeque<(i64, i32)>,
}

impl Default for Plane {
    fn default() -> Self {
        Self::new()
    }
}

impl Plane {
    pub const fn new() -> Self {
        Self { facts: PlaneFacts::ABSENT, booted: false, state: State::Unavailable, early: VecDeque::new() }
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn facts(&self) -> PlaneFacts {
        self.facts
    }

    fn settle(&mut self, ev: i32, task: i64, fx: &mut Vec<Fx>) {
        if ev == 1 {
            self.state = State::Released;
            fx.push(Fx::Released { task });
        } else {
            self.state = State::Quarantined { why: Why::Completion(ev) };
            fx.push(Fx::Quarantined { why: Why::Completion(ev) });
        }
    }

    pub fn step(&mut self, event: Event, fx: &mut Vec<Fx>) {
        match event {
            Event::Boot { facts } if !self.booted => {
                self.booted = true;
                self.facts = facts;
                self.state = if facts.handle { State::Idle } else { State::Unavailable };
            }
            Event::Boot { .. } => {}
            Event::BindRequested { media_id, policy } => match &self.state {
                State::Unavailable => fx.push(Fx::Refuse(Refusal::NoPlane)),
                State::Releasing { .. } => fx.push(Fx::Defer),
                State::Idle | State::Released => {
                    self.state = State::Bound { media_id: media_id.clone() };
                    fx.push(Fx::Bind { media_id });
                }
                State::Bound { .. } => match policy {
                    BindPolicy::Strict => fx.push(Fx::Refuse(Refusal::Bound)),
                    BindPolicy::Rebind => {
                        self.state = State::Bound { media_id: media_id.clone() };
                        fx.push(Fx::Bind { media_id });
                    }
                },
                State::Quarantined { why } => match policy {
                    BindPolicy::Strict => fx.push(Fx::Refuse(Refusal::Quarantined(*why))),
                    BindPolicy::Rebind => {
                        fx.push(Fx::OverQuarantine(*why));
                        self.state = State::Bound { media_id: media_id.clone() };
                        fx.push(Fx::Bind { media_id });
                    }
                },
            },
            Event::UnloadDispatched { rv, task, now_ms } => {
                if matches!(self.state, State::Unavailable | State::Releasing { .. }) {
                    return;
                }
                if rv == UNLOAD_NOT_REGISTERED {
                    self.state = State::Released;
                    fx.push(Fx::Released { task: 0 });
                } else if rv == 1 && task != 0 {
                    if let Some(at) = self.early.iter().position(|(t, _)| *t == task) {
                        let (_, ev) = self.early.remove(at).expect("position is in range");
                        self.settle(ev, task, fx);
                    } else {
                        self.state = State::Releasing { task, deadline_ms: now_ms.saturating_add(RELEASE_FENCE_MS) };
                    }
                } else {
                    self.state = State::Quarantined { why: Why::UnloadRefused(rv) };
                    fx.push(Fx::Quarantined { why: Why::UnloadRefused(rv) });
                }
            }
            Event::Completion { task, ev } => match self.state {
                State::Releasing { task: waiting, .. } if waiting == task => self.settle(ev, task, fx),
                State::Releasing { .. } | State::Unavailable => {}
                _ => {
                    if self.early.len() >= MAX_EARLY_COMPLETIONS {
                        self.early.pop_front();
                    }
                    self.early.push_back((task, ev));
                }
            },
            Event::Dropped { count } => fx.push(Fx::Lost { count }),
            Event::Tick { now_ms } => {
                if let State::Releasing { deadline_ms, .. } = self.state {
                    if now_ms >= deadline_ms {
                        self.state = State::Quarantined { why: Why::Deadline };
                        fx.push(Fx::Quarantined { why: Why::Deadline });
                    }
                }
            }
        }
    }
}

/// One item of the sink's completion mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneCompletion {
    Done { task: i64, ev: i32 },
    /// `count` completions did not fit the mailbox.
    Dropped { count: u64 },
}

/// The mailbox the library thread pushes `(task, ev)` into and the main thread drains, in the
/// shape of `uri::ReplyQueue`: bounded, never latest-wins, and a drop is counted and surfaces as a
/// [`PlaneCompletion::Dropped`] marker on the next drain, never silently.
pub struct CompletionQueue {
    sender: SyncSender<(i64, i32)>,
    receiver: Mutex<Receiver<(i64, i32)>>,
    dropped: std::sync::atomic::AtomicU64,
}

impl CompletionQueue {
    pub fn new(depth: usize) -> Self {
        let (sender, receiver) = sync_channel(depth);
        Self { sender, receiver: Mutex::new(receiver), dropped: std::sync::atomic::AtomicU64::new(0) }
    }
    /// Library thread: record a completion. Never blocks.
    pub fn push(&self, task: i64, ev: i32) {
        if self.sender.try_send((task, ev)).is_err() {
            self.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    /// Main thread: everything that arrived, oldest first, then a marker when any was dropped.
    pub fn drain(&self) -> Vec<PlaneCompletion> {
        let receiver = self.receiver.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<_> = receiver.try_iter().map(|(task, ev)| PlaneCompletion::Done { task, ev }).collect();
        let dropped = self.dropped.swap(0, std::sync::atomic::Ordering::Relaxed);
        if dropped > 0 {
            out.push(PlaneCompletion::Dropped { count: dropped });
        }
        out
    }
}

/// The verdict of a bind request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindVerdict {
    /// Run the bind verbs now.
    Granted,
    /// The previous release is still in flight: skip this frame, ask again.
    Deferred,
    Refused(Refusal),
}

struct Inner {
    plane: Plane,
    epoch: Option<Instant>,
    /// The current wait has been logged (a bind is retried every frame; say it once).
    defer_logged: bool,
}

impl Inner {
    fn now_ms(&mut self) -> u64 {
        let epoch = *self.epoch.get_or_insert_with(Instant::now);
        u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn run(&mut self, event: Event) -> Vec<Fx> {
        let mut fx = Vec::new();
        self.plane.step(event, &mut fx);
        for effect in &fx {
            match effect {
                Fx::Released { task } => plx_base::eventlog::log(&format!("plane: released task={task}")),
                Fx::Quarantined { why } => plx_base::eventlog::log(&format!("plane: quarantined ({why})")),
                Fx::OverQuarantine(why) => plx_base::eventlog::log(&format!(
                    "plane: binding over an unproven release ({why}); Feed re-binds as it always has")),
                Fx::Lost { count } => plx_base::eventlog::log(&format!("plane: {count} completion(s) lost to mailbox overflow")),
                Fx::Defer if !self.defer_logged => {
                    self.defer_logged = true;
                    plx_base::eventlog::log("plane: a bind waits for the previous release's completion");
                }
                Fx::Defer | Fx::Bind { .. } | Fx::Refuse(_) => {}
            }
        }
        if fx.iter().any(|effect| !matches!(effect, Fx::Defer | Fx::Lost { .. })) {
            self.defer_logged = false;
        }
        fx
    }
}

/// The runner of [`Plane`]: the one owner of the ACB plane in this process, reached by every
/// backend through the same `&'static` instance (`installed()` in composition code, injected into
/// drivers). Its methods take the sink the caller already holds, so a fenced sink stays fenced.
pub struct PlaneOwner {
    inner: Mutex<Inner>,
}

static OWNER: PlaneOwner = PlaneOwner::new();

/// The process's plane owner. Resolve it in composition code and inject it, as with the sink.
pub const fn installed() -> &'static PlaneOwner {
    &OWNER
}

impl Default for PlaneOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaneOwner {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(Inner { plane: Plane::new(), epoch: None, defer_logged: false }) }
    }
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record what the boot learned. Taken once; a later call is ignored.
    pub fn boot(&self, facts: PlaneFacts) {
        self.lock().run(Event::Boot { facts });
    }
    pub fn facts(&self) -> PlaneFacts {
        self.lock().plane.facts()
    }
    /// The old `ACB_OK`: this device's video plane is an initialized ACB handle.
    pub fn handle(&self) -> bool {
        self.facts().handle
    }
    pub fn phase(&self) -> Phase {
        self.lock().plane.state().phase()
    }

    /// Drain the sink's completion mailbox into the machine and advance its clock. Call it every
    /// frame a backend waits on the plane; it is the ONLY consumer of the mailbox.
    pub fn pump(&self, sink: &dyn DisplaySink, mt: &MainThread) -> Phase {
        let completions = sink.plane_completions(mt);
        let mut inner = self.lock();
        for completion in completions {
            let event = match completion {
                PlaneCompletion::Done { task, ev } => Event::Completion { task, ev },
                PlaneCompletion::Dropped { count } => Event::Dropped { count },
            };
            inner.run(event);
        }
        let now_ms = inner.now_ms();
        inner.run(Event::Tick { now_ms });
        inner.plane.state().phase()
    }

    /// Ask to bind `media_id`. On [`BindVerdict::Granted`] the caller runs the bind verbs now.
    pub fn bind(&self, sink: &dyn DisplaySink, mt: &MainThread, policy: BindPolicy, media_id: &str) -> BindVerdict {
        self.pump(sink, mt);
        let fx = self.lock().run(Event::BindRequested { media_id: media_id.to_owned(), policy });
        for effect in fx {
            match effect {
                Fx::Bind { .. } => return BindVerdict::Granted,
                Fx::Defer => return BindVerdict::Deferred,
                Fx::Refuse(why) => return BindVerdict::Refused(why),
                _ => {}
            }
        }
        BindVerdict::Refused(Refusal::NoPlane)
    }

    /// `setState(UNLOADED)` through the sink, recorded. Does not wait: the verdict arrives through
    /// later [`pump`](Self::pump) calls. A plane that is unavailable, or already releasing, is not
    /// unloaded again.
    pub fn unload(&self, sink: &dyn DisplaySink, mt: &MainThread) -> Phase {
        self.pump(sink, mt);
        let mut inner = self.lock();
        if matches!(inner.plane.state().phase(), Phase::Unavailable | Phase::Releasing) {
            return inner.plane.state().phase();
        }
        // SAFETY: main thread (the witness), and the owner is the only caller of the unload verb.
        let PlaneUnload { rv, task } = unsafe { sink.plane_unload(mt) };
        let now_ms = inner.now_ms();
        inner.run(Event::UnloadDispatched { rv, task: i64::from(task), now_ms });
        inner.plane.state().phase()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reset_for_test(&self) {
        *self.lock() = Inner { plane: Plane::new(), epoch: None, defer_logged: false };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(m: &mut Plane, event: Event) -> Vec<Fx> {
        let mut fx = Vec::new();
        m.step(event, &mut fx);
        fx
    }
    fn booted() -> Plane {
        let mut m = Plane::new();
        step(&mut m, Event::Boot { facts: PlaneFacts { handle: true, player_type: 0 } });
        m
    }
    fn bind(m: &mut Plane, policy: BindPolicy) -> Vec<Fx> {
        step(m, Event::BindRequested { media_id: "m1".into(), policy })
    }
    fn unload(m: &mut Plane, rv: i32, task: i64, now_ms: u64) -> Vec<Fx> {
        step(m, Event::UnloadDispatched { rv, task, now_ms })
    }
    /// Booted, bound, unload accepted as `task` at t=100.
    fn releasing(task: i64) -> Plane {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        unload(&mut m, 1, task, 100);
        m
    }

    #[test]
    fn the_completion_of_the_unload_task_releases() {
        let mut m = releasing(5);
        assert_eq!(m.state(), &State::Releasing { task: 5, deadline_ms: 100 + RELEASE_FENCE_MS });
        assert_eq!(step(&mut m, Event::Completion { task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
        assert_eq!(m.state(), &State::Released);
        // A repeated completion is inert.
        assert!(step(&mut m, Event::Completion { task: 5, ev: 1 }).is_empty());
        assert_eq!(m.state(), &State::Released);
    }

    #[test]
    fn a_completion_of_another_task_is_not_the_fence() {
        let mut m = releasing(5);
        for other in [4, 6, 0] {
            assert!(step(&mut m, Event::Completion { task: other, ev: 1 }).is_empty(), "task {other}");
            assert!(matches!(m.state(), State::Releasing { task: 5, .. }), "task {other}");
        }
        assert_eq!(step(&mut m, Event::Completion { task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
    }

    #[test]
    fn a_completion_that_is_not_complete_quarantines() {
        for ev in [-2, -1, 0] {
            let mut m = releasing(5);
            assert_eq!(step(&mut m, Event::Completion { task: 5, ev }),
                [Fx::Quarantined { why: Why::Completion(ev) }], "ev {ev}");
            assert_eq!(m.state(), &State::Quarantined { why: Why::Completion(ev) });
        }
    }

    #[test]
    fn no_completion_by_the_deadline_quarantines() {
        let mut m = releasing(5);
        assert!(step(&mut m, Event::Tick { now_ms: 100 + RELEASE_FENCE_MS - 1 }).is_empty());
        assert!(matches!(m.state(), State::Releasing { .. }));
        assert_eq!(step(&mut m, Event::Tick { now_ms: 100 + RELEASE_FENCE_MS }),
            [Fx::Quarantined { why: Why::Deadline }]);
        assert_eq!(m.state(), &State::Quarantined { why: Why::Deadline });
        // The completion that comes too late does not undo a verdict already given.
        assert!(step(&mut m, Event::Completion { task: 5, ev: 1 }).is_empty());
        assert_eq!(m.state(), &State::Quarantined { why: Why::Deadline });
    }

    #[test]
    fn an_unload_that_found_nothing_registered_releases_at_once() {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        assert_eq!(unload(&mut m, UNLOAD_NOT_REGISTERED, 0, 100), [Fx::Released { task: 0 }]);
        assert_eq!(m.state(), &State::Released);
    }

    #[test]
    fn an_unload_the_library_refused_quarantines() {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        assert_eq!(unload(&mut m, 0, 0, 100), [Fx::Quarantined { why: Why::UnloadRefused(0) }]);
        // rv 1 with no task id is no fence either.
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        assert_eq!(unload(&mut m, 1, 0, 100), [Fx::Quarantined { why: Why::UnloadRefused(1) }]);
    }

    #[test]
    fn a_completion_queued_before_the_dispatch_was_recorded_still_matches() {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        // The library thread was quicker than the main thread's bookkeeping.
        assert!(step(&mut m, Event::Completion { task: 7, ev: 1 }).is_empty());
        assert_eq!(unload(&mut m, 1, 7, 100), [Fx::Released { task: 7 }]);
        assert_eq!(m.state(), &State::Released);
        // An early FAILED completion is as final as a late one.
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        step(&mut m, Event::Completion { task: 7, ev: -1 });
        assert_eq!(unload(&mut m, 1, 7, 100), [Fx::Quarantined { why: Why::Completion(-1) }]);
    }

    #[test]
    fn early_completions_are_bounded_and_the_oldest_is_forgotten() {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        for task in 1..=(MAX_EARLY_COMPLETIONS as i64 + 1) {
            step(&mut m, Event::Completion { task, ev: 1 });
        }
        // Task 1 was pushed out, so its unload waits for a completion that will not come again.
        assert!(unload(&mut m, 1, 1, 100).is_empty());
        assert!(matches!(m.state(), State::Releasing { task: 1, .. }));
    }

    #[test]
    fn a_dropped_marker_never_releases_and_the_deadline_still_quarantines() {
        let mut m = releasing(5);
        assert_eq!(step(&mut m, Event::Dropped { count: 3 }), [Fx::Lost { count: 3 }]);
        assert!(matches!(m.state(), State::Releasing { task: 5, .. }));
        assert_eq!(step(&mut m, Event::Tick { now_ms: 100 + RELEASE_FENCE_MS }),
            [Fx::Quarantined { why: Why::Deadline }]);
    }

    #[test]
    fn one_binding_at_a_time_strict() {
        let mut m = releasing(5);
        assert_eq!(bind(&mut m, BindPolicy::Strict), [Fx::Defer]);
        assert!(matches!(m.state(), State::Releasing { .. }));
        step(&mut m, Event::Completion { task: 5, ev: 1 });
        assert_eq!(bind(&mut m, BindPolicy::Strict), [Fx::Bind { media_id: "m1".into() }]);
        assert_eq!(m.state(), &State::Bound { media_id: "m1".into() });
        // A second binding while one holds the plane is refused.
        assert_eq!(bind(&mut m, BindPolicy::Strict), [Fx::Refuse(Refusal::Bound)]);
        // And a quarantined plane is refused for good.
        let mut m = releasing(5);
        step(&mut m, Event::Completion { task: 5, ev: -1 });
        assert_eq!(bind(&mut m, BindPolicy::Strict),
            [Fx::Refuse(Refusal::Quarantined(Why::Completion(-1)))]);
    }

    #[test]
    fn feed_proceeds_over_an_unproven_release_and_says_so() {
        // The deadline passing is the case the decompile cannot rule out on another firmware:
        // refusing here would be a black picture on every title after the first.
        let mut m = releasing(5);
        assert_eq!(bind(&mut m, BindPolicy::Rebind), [Fx::Defer]);
        step(&mut m, Event::Tick { now_ms: 100 + RELEASE_FENCE_MS });
        assert_eq!(bind(&mut m, BindPolicy::Rebind), [
            Fx::OverQuarantine(Why::Deadline),
            Fx::Bind { media_id: "m1".into() },
        ]);
        assert_eq!(m.state(), &State::Bound { media_id: "m1".into() });
        // A bound plane is rebound by Feed (it re-binds A->B->A on one handle).
        assert_eq!(bind(&mut m, BindPolicy::Rebind), [Fx::Bind { media_id: "m1".into() }]);
    }

    #[test]
    fn a_device_without_acb_has_no_plane_to_bind_or_release() {
        let mut m = Plane::new();
        step(&mut m, Event::Boot { facts: PlaneFacts { handle: false, player_type: 10 } });
        assert_eq!(m.state(), &State::Unavailable);
        assert_eq!(bind(&mut m, BindPolicy::Rebind), [Fx::Refuse(Refusal::NoPlane)]);
        assert!(unload(&mut m, 1, 5, 100).is_empty());
        assert_eq!(m.state(), &State::Unavailable);
        assert_eq!(m.facts(), PlaneFacts { handle: false, player_type: 10 });
        // Before boot nothing is known either, and nothing is bound.
        let mut fresh = Plane::new();
        assert_eq!(bind(&mut fresh, BindPolicy::Strict), [Fx::Refuse(Refusal::NoPlane)]);
    }

    #[test]
    fn boot_facts_are_taken_once() {
        let mut m = booted();
        step(&mut m, Event::Boot { facts: PlaneFacts { handle: false, player_type: 3 } });
        assert_eq!(m.facts(), PlaneFacts { handle: true, player_type: 0 });
        assert_eq!(m.state(), &State::Idle);
    }

    fn mt() -> MainThread {
        // SAFETY: a unit test is its own main thread; nothing here reaches a native object.
        unsafe { MainThread::assume() }
    }

    #[test]
    fn the_owner_unloads_then_waits_for_the_pumped_completion() {
        use super::super::sink::ScriptedPlaneSink;
        let owner = PlaneOwner::new();
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        let mt = mt();
        owner.boot(PlaneFacts { handle: true, player_type: 0 });
        assert_eq!(owner.bind(&sink, &mt, BindPolicy::Strict, "m1"), BindVerdict::Granted);
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        // A second unload while one is in flight is not sent again.
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        assert_eq!(sink.unload_count(), 1);
        assert_eq!(owner.bind(&sink, &mt, BindPolicy::Strict, "m2"), BindVerdict::Deferred);
        sink.mailbox.push(4, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Releasing);
        sink.mailbox.push(5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
        assert_eq!(owner.bind(&sink, &mt, BindPolicy::Strict, "m2"), BindVerdict::Granted);
    }

    #[test]
    fn the_owner_matches_a_completion_that_beat_its_bookkeeping() {
        use super::super::sink::ScriptedPlaneSink;
        let owner = PlaneOwner::new();
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        sink.complete_at_once.store(true, std::sync::atomic::Ordering::Relaxed);
        let mt = mt();
        owner.boot(PlaneFacts { handle: true, player_type: 0 });
        owner.bind(&sink, &mt, BindPolicy::Strict, "m1");
        // The callback is in the mailbox before `unload` has recorded its task; the next pump
        // still finds it.
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
    }

    #[test]
    fn the_owner_does_not_unload_a_device_without_a_handle() {
        use super::super::sink::ScriptedPlaneSink;
        let owner = PlaneOwner::new();
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        let mt = mt();
        assert_eq!(owner.unload(&sink, &mt), Phase::Unavailable);
        assert_eq!(sink.unload_count(), 0);
        assert!(!owner.handle());
    }

    #[test]
    fn the_completion_queue_keeps_order_and_reports_what_it_dropped() {
        let q = CompletionQueue::new(2);
        q.push(1, 1);
        q.push(2, 1);
        q.push(3, 1);
        q.push(4, -1);
        assert_eq!(q.drain(), [
            PlaneCompletion::Done { task: 1, ev: 1 },
            PlaneCompletion::Done { task: 2, ev: 1 },
            PlaneCompletion::Dropped { count: 2 },
        ]);
        assert!(q.drain().is_empty());
        q.push(9, 1);
        assert_eq!(q.drain(), [PlaneCompletion::Done { task: 9, ev: 1 }]);
    }
}
