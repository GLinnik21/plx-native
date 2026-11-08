//! The ACB video-plane owner: ONE backend-neutral state for the one binding the app allows across
//! its ACB handles, and a release that waits for the firmware's completion callback.
//!
//! **Why.** A handle is created and initialized once per process (`AcbAPI_create` +
//! `AcbAPI_initialize`; a repeat initialize returns 0). The boot makes up to two, one per backend
//! ([`PlaneHandle`]: Feed's, player type 10, and the URI engine's, player type 0), each created once
//! and never destroyed. Two cores in one process are independent (each has its own anonymous bus
//! client, callback, appId, sink and type; `/Volumes/DAS/lgfw/reports/acb-two-cores/report.md`), so
//! this owner keeps the invariant that matters on top of them: ONE binding at a time, across both.
//! `setMediaId` may be called again on the same
//! handle after UNLOADED, but only once the previous binding is really gone, and the only thing
//! that says so is the completion callback of `AcbAPI_setState(h, FG, UNLOADED, &task)`:
//! it carries the SAME task id that call returned, event type 1 (`-2` timeout, `-1` error,
//! `0` cancel, `1` complete). LOADED, PLAYING, window and video-data tasks also complete with
//! event 1, so the fence is matched on the TASK ID of the handle that holds the binding (each core numbers
//! its own tasks, and the callback's first argument is the id `AcbAPI_create` returned for the issuing core:
//! `/Volumes/DAS/lgfw/reports/acb-callback-id/report.md`), never on the event alone, and the callback's
//! playState argument (a cached value) is never read. If `setState` answers `-2` (not registered)
//! nothing was sent and no callback will come. The evidence for both rules (client and service
//! addresses, the device log, and what is not determined) is
//! `docs/measurements/acb-release-fence-2026-11-04.md`.
//!
//! The plane owner also tracks the other half of a URI bind: the full-screen window and
//! `setState(PLAYING)` that end it. [`PlaneOwner::show`] dispatches them and [`Shown`] says whether
//! the firmware has completed BOTH (event 1, matched on each call's own task id, never the event
//! alone). That is necessary evidence for a picture, not proof of one; the URI backend combines it
//! with the service's `playing` and a clock advance (`play-uri`'s `picture_gate`).
//!
//! **Two handles.** The machine knows only roles ([`PlaneHandle`]); the owner maps a role to the id
//! the sink's `plane_create` returned and, when it grants a bind, makes that handle the one the sink's
//! verbs address ([`DisplaySink::plane_select`]). A bind of one handle while the other holds the
//! plane is refused, and while the other's release is in flight it waits for that release's UNLOADED,
//! exactly as a second bind of one handle always did. Both handles' sinks are pinned to MAIN by the
//! bind's first verb before their first bind (`setSinkType` in `acb_media_id`): the firmware
//! recomputes the sink of every un-pinned object of an app on a foreground change, so an un-pinned
//! handle could have its sink rewritten by the other's foreground transition. The released handle
//! stays registered (UNLOADED) while the other is bound, as the owner has always left a handle
//! between titles.
//!
//! **Shape.** [`Plane`] is the pure machine (no clock, no global, no IO: time arrives as `now_ms`
//! in events, work leaves as [`Fx`]). [`PlaneOwner`] is its runner: the single holder of the
//! machine, the only consumer of the sink's completion mailbox ([`CompletionQueue`], the shape of
//! `uri::ReplyQueue`), and the one monotonic clock the machine's deadlines are measured on. Both
//! backends (Feed's `plane_bind`/`plane_unload`, the URI probe's ACB experiment) go through it, so
//! "one binding at a time" holds across them. The mailbox static itself lives in the webOS port
//! (`webos::video_sink`), which is the only place that has a library thread to receive from.
use super::sink::{DisplaySink, PlaneFinish, PlaneUnload};
use plx_base::task::MainThread;
use std::os::raw::c_long;
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
/// The player type the URI engine's handle is initialized with: the one its product binding is gated
/// on (`play-uri`'s `acb_bind::gate`), the sequence the device measurements ran.
pub const URI_PLAYER_TYPE: i32 = 0;

/// Which of the boot's ACB handles a binding runs over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneHandle {
    /// Buffer-feed's handle: [`DEFAULT_PLAYER_TYPE`] unless `/tmp/plxnative-ptype` says otherwise.
    Feed,
    /// The URI engine's handle: [`URI_PLAYER_TYPE`].
    Uri,
}

impl PlaneHandle {
    const fn slot(self) -> usize {
        match self {
            PlaneHandle::Feed => 0,
            PlaneHandle::Uri => 1,
        }
    }
}

impl std::fmt::Display for PlaneHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PlaneHandle::Feed => "feed",
            PlaneHandle::Uri => "uri",
        })
    }
}

/// What the boot learned about one ACB handle. Owned by the [`PlaneOwner`] from the `Boot` event on
/// (the Feed handle's is what [`PlaneOwner::facts`] answers); it replaces the old `ACB_OK`/`PTYPE`
/// statics.
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

/// Whether the firmware has completed the two calls that put a bound plane on the panel: the
/// full-screen `setDisplayWindow` and `setState(PLAYING)` of [`PlaneOwner::show`]. Each call
/// completes with event 1 on its own task id; a completion on any other id is not one of them.
/// Reset by a granted bind and by an unload, so a previous binding's tasks never count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// No accepted show is outstanding on this binding.
    No,
    /// Both calls were accepted; each id is still `Some` until its event-1 completion arrives.
    Waiting { display: Option<i64>, playing: Option<i64> },
    /// Both calls completed with event 1.
    Done,
    /// One of the two completed with this event (`-2` timeout, `-1` error, `0` cancel): final.
    Failed { ev: i32 },
}

pub enum Event {
    /// The Feed handle's boot.
    Boot { facts: PlaneFacts },
    /// The URI handle's boot.
    BootUri { facts: PlaneFacts },
    BindRequested { handle: PlaneHandle, media_id: String, policy: BindPolicy },
    /// `setState(UNLOADED)` was called: its answer, the task id it wrote (trusted only for `rv == 1`)
    /// and when.
    UnloadDispatched { rv: i32, task: i64, now_ms: u64 },
    /// `setDisplayWindow` + `setState(PLAYING)` were called: the answer (1 only when both were
    /// accepted) and the two task ids, trusted only for `rv == 1`.
    FinishDispatched { rv: i32, display: i64, playing: i64 },
    /// A completion callback. `from` is the handle the firmware's callback named (its first argument
    /// is the id `AcbAPI_create` returned, so the runner can name the handle), `None` for a
    /// completion that arrived with no handle named. Each core numbers its tasks from its own
    /// counter, so a task id means something only on the handle that issued it: a completion from
    /// any handle but the one whose binding is current is not part of this binding's fence or show.
    Completion { from: Option<PlaneHandle>, task: i64, ev: i32 },
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
    /// A completion of a handle whose binding is not the current one (the idle core's, or one before
    /// any bind): its task id is that core's own, so it is neither this binding's fence nor its show.
    Foreign { handle: PlaneHandle, task: i64, ev: i32 },
}

pub struct Plane {
    facts: PlaneFacts,
    booted: bool,
    uri: PlaneFacts,
    uri_booted: bool,
    /// The handle of the latest granted binding: whose binding holds the plane while it is
    /// bound, and whose unload is in flight or done after.
    bound_by: Option<PlaneHandle>,
    state: State,
    early: VecDeque<(i64, i32)>,
    shown: Shown,
}

impl Default for Plane {
    fn default() -> Self {
        Self::new()
    }
}

impl Plane {
    pub const fn new() -> Self {
        Self { facts: PlaneFacts::ABSENT, booted: false, uri: PlaneFacts::ABSENT, uri_booted: false,
            bound_by: None, state: State::Unavailable, early: VecDeque::new(), shown: Shown::No }
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn shown(&self) -> Shown {
        self.shown
    }

    /// A granted bind starts a new binding: the previous one's completions and show are history.
    fn bound(&mut self, handle: PlaneHandle, media_id: String, fx: &mut Vec<Fx>) {
        self.bound_by = Some(handle);
        self.state = State::Bound { media_id: media_id.clone() };
        self.shown = Shown::No;
        self.early.clear();
        fx.push(Fx::Bind { media_id });
    }

    /// Account `(task, ev)` against the outstanding show. True when it was one of the show's two
    /// tasks (it is then consumed: it is not the unload fence, and not worth an `early` slot).
    fn complete_shown(&mut self, task: i64, ev: i32) -> bool {
        let Shown::Waiting { display, playing } = self.shown else { return false };
        let (display, playing) = if display == Some(task) {
            (None, playing)
        } else if playing == Some(task) {
            (display, None)
        } else {
            return false;
        };
        self.shown = if ev != 1 {
            Shown::Failed { ev }
        } else if display.is_none() && playing.is_none() {
            Shown::Done
        } else {
            Shown::Waiting { display, playing }
        };
        true
    }
    /// The Feed handle's facts.
    pub fn facts(&self) -> PlaneFacts {
        self.facts
    }
    pub fn facts_of(&self, handle: PlaneHandle) -> PlaneFacts {
        match handle {
            PlaneHandle::Feed => self.facts,
            PlaneHandle::Uri => self.uri,
        }
    }
    /// The handle the latest granted binding ran over (its unload is the one in flight or done).
    pub fn bound_handle(&self) -> Option<PlaneHandle> {
        self.bound_by
    }
    /// Whether either handle exists: the plane is unavailable only when neither does.
    pub fn any_handle(&self) -> bool {
        self.facts.handle || self.uri.handle
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
                self.state = if self.any_handle() { State::Idle } else { State::Unavailable };
            }
            Event::BootUri { facts } if !self.uri_booted => {
                self.uri_booted = true;
                self.uri = facts;
                self.state = if self.any_handle() { State::Idle } else { State::Unavailable };
            }
            Event::Boot { .. } | Event::BootUri { .. } => {}
            Event::BindRequested { handle, .. } if !self.facts_of(handle).handle => {
                fx.push(Fx::Refuse(Refusal::NoPlane));
            }
            Event::BindRequested { handle, media_id, policy } => match &self.state {
                State::Unavailable => fx.push(Fx::Refuse(Refusal::NoPlane)),
                State::Releasing { .. } => fx.push(Fx::Defer),
                State::Idle | State::Released => self.bound(handle, media_id, fx),
                State::Bound { .. } => match policy {
                    BindPolicy::Strict => fx.push(Fx::Refuse(Refusal::Bound)),
                    // Feed re-binds its own handle title after title; the other handle's binding is
                    // released first, never replaced.
                    BindPolicy::Rebind if self.bound_by == Some(handle) => self.bound(handle, media_id, fx),
                    BindPolicy::Rebind => fx.push(Fx::Refuse(Refusal::Bound)),
                },
                State::Quarantined { why } => match policy {
                    BindPolicy::Strict => fx.push(Fx::Refuse(Refusal::Quarantined(*why))),
                    BindPolicy::Rebind => {
                        fx.push(Fx::OverQuarantine(*why));
                        self.bound(handle, media_id, fx);
                    }
                },
            },
            Event::FinishDispatched { rv, display, playing } => {
                // Only a bound plane has a window and a play state to complete. A refused call, or
                // an id of 0 (the seam's "none"), leaves nothing to wait for and is never `Done`.
                if !matches!(self.state, State::Bound { .. }) {
                    return;
                }
                self.shown = Shown::No;
                if rv == 1 && display != 0 && playing != 0 && display != playing {
                    self.shown = Shown::Waiting { display: Some(display), playing: Some(playing) };
                    // The firmware may have been quicker than this bookkeeping, as for the unload.
                    for task in [display, playing] {
                        if let Some(at) = self.early.iter().position(|(t, _)| *t == task) {
                            let (_, ev) = self.early.remove(at).expect("position is in range");
                            self.complete_shown(task, ev);
                        }
                    }
                }
            }
            Event::UnloadDispatched { rv, task, now_ms } => {
                if matches!(self.state, State::Unavailable | State::Releasing { .. }) {
                    return;
                }
                // The binding is going: a show still outstanding is that binding's, and its ids
                // must never match a completion of the next one.
                self.shown = Shown::No;
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
            Event::Completion { from: Some(handle), task, ev } if self.bound_by != Some(handle) => {
                fx.push(Fx::Foreign { handle, task, ev });
            }
            Event::Completion { task, ev, .. } => {
                if self.complete_shown(task, ev) {
                    return;
                }
                match self.state {
                    State::Releasing { task: waiting, .. } if waiting == task => self.settle(ev, task, fx),
                    State::Releasing { .. } | State::Unavailable => {}
                    _ => {
                        if self.early.len() >= MAX_EARLY_COMPLETIONS {
                            self.early.pop_front();
                        }
                        self.early.push_back((task, ev));
                    }
                }
            }
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
    /// `id` is the first argument of the firmware's callback: the id `AcbAPI_create` returned for the
    /// core that issued `task` (0 = the sink named none).
    Done { id: i64, task: i64, ev: i32 },
    /// `count` completions did not fit the mailbox.
    Dropped { count: u64 },
}

/// The mailbox the library thread pushes `(id, task, ev)` into and the main thread drains, in the
/// shape of `uri::ReplyQueue`: bounded, never latest-wins, and a drop is counted and surfaces as a
/// [`PlaneCompletion::Dropped`] marker on the next drain, never silently.
pub struct CompletionQueue {
    sender: SyncSender<(i64, i64, i32)>,
    receiver: Mutex<Receiver<(i64, i64, i32)>>,
    dropped: std::sync::atomic::AtomicU64,
}

impl CompletionQueue {
    pub fn new(depth: usize) -> Self {
        let (sender, receiver) = sync_channel(depth);
        Self { sender, receiver: Mutex::new(receiver), dropped: std::sync::atomic::AtomicU64::new(0) }
    }
    /// Library thread: record a completion that names no handle. Never blocks.
    pub fn push(&self, task: i64, ev: i32) {
        self.push_from(0, task, ev);
    }
    /// Library thread: record a completion of the core `id` (the callback's first argument). Never blocks.
    pub fn push_from(&self, id: i64, task: i64, ev: i32) {
        if self.sender.try_send((id, task, ev)).is_err() {
            self.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    /// Main thread: everything that arrived, oldest first, then a marker when any was dropped.
    pub fn drain(&self) -> Vec<PlaneCompletion> {
        let receiver = self.receiver.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<_> = receiver.try_iter().map(|(id, task, ev)| PlaneCompletion::Done { id, task, ev }).collect();
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

/// What the runner holds of one boot handle beyond the machine's facts: the id the sink's
/// `plane_create` returned (0 = none, nothing to select) and the library's confirmation of
/// `AcbAPI_initialize`, read once at the boot while that handle was the sink's current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleSeat {
    pub id: c_long,
    pub initialized: bool,
}

impl HandleSeat {
    /// A handle with no identity to select by: a device without ACB, or a handle booted by facts alone.
    pub const NONE: Self = Self { id: 0, initialized: false };
}

struct Inner {
    plane: Plane,
    epoch: Option<Instant>,
    /// The current wait has been logged (a bind is retried every frame; say it once).
    defer_logged: bool,
    seats: [HandleSeat; 2],
    /// The handle the sink's verbs currently address, once the owner has made one so. The boot
    /// leaves the newest created handle current, so this starts unknown and the first grant selects.
    selected: Option<PlaneHandle>,
}

impl Inner {
    const fn new() -> Self {
        Self { plane: Plane::new(), epoch: None, defer_logged: false, seats: [HandleSeat::NONE; 2],
            selected: None }
    }
    /// The handle a completion's id names: `Ok(None)` for id 0 (the sink named none: a host sink, a
    /// handle booted without an id), `Err` for an id that is neither seat.
    fn handle_of(&self, id: i64) -> Result<Option<PlaneHandle>, ()> {
        if id == 0 {
            return Ok(None);
        }
        [PlaneHandle::Feed, PlaneHandle::Uri].into_iter()
            .find(|handle| self.seats[handle.slot()].id != 0 && i64::from(self.seats[handle.slot()].id) == id)
            .map(Some).ok_or(())
    }
    /// Make `handle` the one the sink's verbs address (a no-op for a handle with no id, or when it
    /// already is). The owner is the only caller: one binding at a time makes the current handle
    /// the bound one.
    fn select(&mut self, sink: &dyn DisplaySink, mt: &MainThread, handle: PlaneHandle) {
        let id = self.seats[handle.slot()].id;
        if id == 0 || self.selected == Some(handle) {
            return;
        }
        // SAFETY: main thread (the witness), and the owner is the only caller of the select verb.
        let done = unsafe { sink.plane_select(mt, id) };
        plx_base::eventlog::log(&format!("plane: {handle} handle {id} selected={done}"));
        if done {
            self.selected = Some(handle);
        }
    }
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
                Fx::Foreign { handle, task, ev } => plx_base::eventlog::log(&format!(
                    "plane: ignored a completion of the {handle} handle, which holds no binding (task={task} ev={ev})")),
                Fx::Defer if !self.defer_logged => {
                    self.defer_logged = true;
                    plx_base::eventlog::log("plane: a bind waits for the previous release's completion");
                }
                Fx::Defer | Fx::Bind { .. } | Fx::Refuse(_) => {}
            }
        }
        if fx.iter().any(|effect| !matches!(effect, Fx::Defer | Fx::Lost { .. } | Fx::Foreign { .. })) {
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
        Self { inner: Mutex::new(Inner::new()) }
    }
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record what the boot learned of the Feed handle. Taken once; a later call is ignored.
    pub fn boot(&self, facts: PlaneFacts) {
        self.boot_handle(PlaneHandle::Feed, facts, HandleSeat::NONE);
    }
    /// Record what the boot learned of `handle`, and the `seat` the sink knows it by. Taken once per
    /// handle; a later call is ignored.
    pub fn boot_handle(&self, handle: PlaneHandle, facts: PlaneFacts, seat: HandleSeat) {
        let mut inner = self.lock();
        let first = !match handle {
            PlaneHandle::Feed => inner.plane.booted,
            PlaneHandle::Uri => inner.plane.uri_booted,
        };
        inner.run(match handle {
            PlaneHandle::Feed => Event::Boot { facts },
            PlaneHandle::Uri => Event::BootUri { facts },
        });
        if first {
            inner.seats[handle.slot()] = seat;
        }
    }
    /// The Feed handle's facts.
    pub fn facts(&self) -> PlaneFacts {
        self.lock().plane.facts()
    }
    pub fn facts_of(&self, handle: PlaneHandle) -> PlaneFacts {
        self.lock().plane.facts_of(handle)
    }
    /// The library confirmed `AcbAPI_initialize` for `handle` at the boot (a created handle alone is
    /// not that); false for a handle that was never booted.
    pub fn initialized(&self, handle: PlaneHandle) -> bool {
        self.lock().seats[handle.slot()].initialized
    }
    /// Whether this boot created either ACB handle (Feed's or the URI engine's): the device has an
    /// ACB plane at all, which a build with only the URI engine has with Feed's handle absent.
    pub fn any_handle(&self) -> bool {
        self.lock().plane.any_handle()
    }
    /// The old `ACB_OK`: this device's video plane is an initialized ACB handle (Feed's).
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
                PlaneCompletion::Done { id, task, ev } => match inner.handle_of(id) {
                    Ok(from) => Event::Completion { from, task, ev },
                    Err(()) => {
                        plx_base::eventlog::log(&format!(
                            "plane: ignored a completion of id={id}, which is not a boot handle (task={task} ev={ev})"));
                        continue;
                    }
                },
                PlaneCompletion::Dropped { count } => Event::Dropped { count },
            };
            inner.run(event);
        }
        let now_ms = inner.now_ms();
        inner.run(Event::Tick { now_ms });
        inner.plane.state().phase()
    }

    /// Ask to bind `media_id` over `handle`. On [`BindVerdict::Granted`] `handle` is the sink's
    /// current one and the caller runs the bind verbs now.
    pub fn bind(&self, sink: &dyn DisplaySink, mt: &MainThread, handle: PlaneHandle, policy: BindPolicy,
        media_id: &str) -> BindVerdict {
        self.pump(sink, mt);
        let mut inner = self.lock();
        let fx = inner.run(Event::BindRequested { handle, media_id: media_id.to_owned(), policy });
        for effect in fx {
            match effect {
                Fx::Bind { .. } => {
                    inner.select(sink, mt, handle);
                    return BindVerdict::Granted;
                }
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
        // The unload is the bound handle's, whichever the sink last addressed.
        if let Some(handle) = inner.plane.bound_handle() {
            inner.select(sink, mt, handle);
        }
        // SAFETY: main thread (the witness), and the owner is the only caller of the unload verb.
        let PlaneUnload { rv, task } = unsafe { sink.plane_unload(mt) };
        let now_ms = inner.now_ms();
        inner.run(Event::UnloadDispatched { rv, task: i64::from(task), now_ms });
        inner.plane.state().phase()
    }

    /// The full-screen `setDisplayWindow` and `setState(PLAYING)` through the sink, recorded: the
    /// last step of the paced URI bind. True only when the library accepted both. Does not wait:
    /// [`shown`](Self::shown) says when the firmware has completed them. The sink is called even
    /// when the plane is not bound (the bind sequence is the caller's), but only a bound plane
    /// tracks the completions.
    pub fn show(&self, sink: &dyn DisplaySink, mt: &MainThread) -> bool {
        self.pump(sink, mt);
        let mut inner = self.lock();
        // SAFETY: main thread (the witness), and the owner is the only caller of the finish verb.
        let PlaneFinish { rv, display_task, playing_task } = unsafe { sink.plane_finish(mt) };
        inner.run(Event::FinishDispatched { rv, display: i64::from(display_task), playing: i64::from(playing_task) });
        rv == 1
    }

    /// Drain the completion mailbox, then say how the outstanding [`show`](Self::show) stands.
    pub fn shown(&self, sink: &dyn DisplaySink, mt: &MainThread) -> Shown {
        self.pump(sink, mt);
        self.lock().plane.shown()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reset_for_test(&self) {
        *self.lock() = Inner::new();
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
        step(m, Event::BindRequested { handle: PlaneHandle::Feed, media_id: "m1".into(), policy })
    }
    fn unload(m: &mut Plane, rv: i32, task: i64, now_ms: u64) -> Vec<Fx> {
        step(m, Event::UnloadDispatched { rv, task, now_ms })
    }
    fn finish(m: &mut Plane, rv: i32, display: i64, playing: i64) -> Vec<Fx> {
        step(m, Event::FinishDispatched { rv, display, playing })
    }
    /// Booted, bound, window and PLAYING accepted as tasks 3 and 4 (the device log's ids).
    fn showing() -> Plane {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        finish(&mut m, 1, 3, 4);
        m
    }
    const WAITING_BOTH: Shown = Shown::Waiting { display: Some(3), playing: Some(4) };
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
        assert_eq!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
        assert_eq!(m.state(), &State::Released);
        // A repeated completion is inert.
        assert!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }).is_empty());
        assert_eq!(m.state(), &State::Released);
    }

    #[test]
    fn a_completion_of_another_task_is_not_the_fence() {
        let mut m = releasing(5);
        for other in [4, 6, 0] {
            assert!(step(&mut m, Event::Completion { from: None, task: other, ev: 1 }).is_empty(), "task {other}");
            assert!(matches!(m.state(), State::Releasing { task: 5, .. }), "task {other}");
        }
        assert_eq!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
    }

    #[test]
    fn a_completion_that_is_not_complete_quarantines() {
        for ev in [-2, -1, 0] {
            let mut m = releasing(5);
            assert_eq!(step(&mut m, Event::Completion { from: None, task: 5, ev }),
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
        assert!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }).is_empty());
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
        assert!(step(&mut m, Event::Completion { from: None, task: 7, ev: 1 }).is_empty());
        assert_eq!(unload(&mut m, 1, 7, 100), [Fx::Released { task: 7 }]);
        assert_eq!(m.state(), &State::Released);
        // An early FAILED completion is as final as a late one.
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        step(&mut m, Event::Completion { from: None, task: 7, ev: -1 });
        assert_eq!(unload(&mut m, 1, 7, 100), [Fx::Quarantined { why: Why::Completion(-1) }]);
    }

    #[test]
    fn early_completions_are_bounded_and_the_oldest_is_forgotten() {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        for task in 1..=(MAX_EARLY_COMPLETIONS as i64 + 1) {
            step(&mut m, Event::Completion { from: None, task, ev: 1 });
        }
        // Task 1 was pushed out, so its unload waits for a completion that will not come again.
        assert!(unload(&mut m, 1, 1, 100).is_empty());
        assert!(matches!(m.state(), State::Releasing { task: 1, .. }));
    }

    #[test]
    fn a_show_is_done_only_when_both_of_its_own_tasks_complete_with_event_1() {
        let mut m = showing();
        assert_eq!(m.shown(), WAITING_BOTH);
        // Another task (a pause, the video data) is no part of it; neither is a task 0.
        for other in [1, 2, 5, 0] {
            step(&mut m, Event::Completion { from: None, task: other, ev: 1 });
            assert_eq!(m.shown(), WAITING_BOTH, "task {other}");
        }
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        assert_eq!(m.shown(), Shown::Waiting { display: None, playing: Some(4) }, "one of two is not done");
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::Done);
        // A repeat is inert, and the show never touches the release state.
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::Done);
        assert_eq!(m.state(), &State::Bound { media_id: "m1".into() });
    }

    #[test]
    fn the_order_of_the_two_show_completions_does_not_matter() {
        for order in [[3, 4], [4, 3]] {
            let mut m = showing();
            step(&mut m, Event::Completion { from: None, task: order[0], ev: 1 });
            assert!(matches!(m.shown(), Shown::Waiting { .. }), "{order:?}");
            step(&mut m, Event::Completion { from: None, task: order[1], ev: 1 });
            assert_eq!(m.shown(), Shown::Done, "{order:?}");
        }
    }

    #[test]
    fn a_show_completion_that_is_not_complete_fails_for_good() {
        for (task, ev) in [(3, -2), (3, -1), (3, 0), (4, -2), (4, -1), (4, 0)] {
            let mut m = showing();
            step(&mut m, Event::Completion { from: None, task, ev });
            assert_eq!(m.shown(), Shown::Failed { ev }, "task {task} ev {ev}");
            // The other half completing cleanly does not undo it.
            step(&mut m, Event::Completion { from: None, task: 7 - task, ev: 1 });
            assert_eq!(m.shown(), Shown::Failed { ev }, "task {task} ev {ev}");
        }
    }

    #[test]
    fn show_completions_that_beat_the_dispatch_are_matched_from_the_early_queue() {
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::No, "nothing was waiting yet");
        finish(&mut m, 1, 3, 4);
        assert_eq!(m.shown(), Shown::Done);
        // One early and one late.
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        finish(&mut m, 1, 3, 4);
        assert_eq!(m.shown(), Shown::Waiting { display: Some(3), playing: None });
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        assert_eq!(m.shown(), Shown::Done);
        // An early failure is as final as a late one.
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        step(&mut m, Event::Completion { from: None, task: 3, ev: -1 });
        finish(&mut m, 1, 3, 4);
        assert_eq!(m.shown(), Shown::Failed { ev: -1 });
    }

    #[test]
    fn a_completion_from_before_the_bind_never_counts_toward_the_show() {
        // The previous binding's tasks 3 and 4 sit in the early queue when it is unloaded.
        let mut m = booted();
        bind(&mut m, BindPolicy::Strict);
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        unload(&mut m, 1, 5, 100);
        step(&mut m, Event::Completion { from: None, task: 5, ev: 1 });
        assert_eq!(m.state(), &State::Released);
        // A new binding whose tasks reuse the ids does not inherit those completions.
        bind(&mut m, BindPolicy::Strict);
        finish(&mut m, 1, 3, 4);
        assert_eq!(m.shown(), WAITING_BOTH);
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::Done, "its own completions do");
    }

    #[test]
    fn a_show_with_nothing_to_wait_for_is_never_done() {
        // Refused, task-less, a repeated id, and a plane that is not bound.
        for (rv, display, playing) in [(0, 3, 4), (1, 0, 4), (1, 3, 0), (1, 3, 3), (-2, 3, 4)] {
            let mut m = booted();
            bind(&mut m, BindPolicy::Strict);
            finish(&mut m, rv, display, playing);
            assert_eq!(m.shown(), Shown::No, "rv {rv} tasks {display}/{playing}");
            step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
            step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
            assert_eq!(m.shown(), Shown::No, "rv {rv} tasks {display}/{playing}");
        }
        let mut m = booted();
        finish(&mut m, 1, 3, 4);
        assert_eq!(m.shown(), Shown::No, "an idle plane has no window to show");
    }

    #[test]
    fn an_unload_and_a_new_bind_both_end_the_show() {
        let mut m = showing();
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        unload(&mut m, 1, 5, 100);
        assert_eq!(m.shown(), Shown::No);
        // The stale half cannot complete anything now, and the unload fence is untouched.
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::No);
        assert!(matches!(m.state(), State::Releasing { task: 5, .. }));
        assert_eq!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
        // Feed re-binds over a bound plane without an unload: that is a new binding too.
        let mut m = showing();
        bind(&mut m, BindPolicy::Rebind);
        assert_eq!(m.shown(), Shown::No);
    }

    #[test]
    fn the_unload_fence_still_releases_after_a_show_consumed_its_tasks() {
        let mut m = showing();
        step(&mut m, Event::Completion { from: None, task: 3, ev: 1 });
        step(&mut m, Event::Completion { from: None, task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::Done);
        unload(&mut m, 1, 5, 100);
        assert_eq!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
        assert_eq!(m.state(), &State::Released);
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
        step(&mut m, Event::Completion { from: None, task: 5, ev: 1 });
        assert_eq!(bind(&mut m, BindPolicy::Strict), [Fx::Bind { media_id: "m1".into() }]);
        assert_eq!(m.state(), &State::Bound { media_id: "m1".into() });
        // A second binding while one holds the plane is refused.
        assert_eq!(bind(&mut m, BindPolicy::Strict), [Fx::Refuse(Refusal::Bound)]);
        // And a quarantined plane is refused for good.
        let mut m = releasing(5);
        step(&mut m, Event::Completion { from: None, task: 5, ev: -1 });
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
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Strict, "m1"), BindVerdict::Granted);
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        // A second unload while one is in flight is not sent again.
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        assert_eq!(sink.unload_count(), 1);
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Strict, "m2"), BindVerdict::Deferred);
        sink.mailbox.push(4, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Releasing);
        sink.mailbox.push(5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Strict, "m2"), BindVerdict::Granted);
    }

    #[test]
    fn the_owner_matches_a_completion_that_beat_its_bookkeeping() {
        use super::super::sink::ScriptedPlaneSink;
        let owner = PlaneOwner::new();
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        sink.complete_at_once.store(true, std::sync::atomic::Ordering::Relaxed);
        let mt = mt();
        owner.boot(PlaneFacts { handle: true, player_type: 0 });
        owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Strict, "m1");
        // The callback is in the mailbox before `unload` has recorded its task; the next pump
        // still finds it.
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
    }

    #[test]
    fn the_owner_shows_through_the_sink_and_reports_done_after_both_pumped_completions() {
        use super::super::sink::{ScriptedPlaneSink, SCRIPTED_DISPLAY_TASK, SCRIPTED_PLAYING_TASK};
        let owner = PlaneOwner::new();
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        let mt = mt();
        owner.boot(PlaneFacts { handle: true, player_type: 0 });
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Strict, "m1"), BindVerdict::Granted);
        assert_eq!(owner.shown(&sink, &mt), Shown::No, "nothing was shown yet");
        assert!(owner.show(&sink, &mt));
        assert_eq!(sink.calls(), ["plane_finish"]);
        let waiting = Shown::Waiting { display: Some(SCRIPTED_DISPLAY_TASK.into()), playing: Some(SCRIPTED_PLAYING_TASK.into()) };
        assert_eq!(owner.shown(&sink, &mt), waiting, "dispatched is not completed");
        sink.mailbox.push(SCRIPTED_DISPLAY_TASK.into(), 1);
        assert_eq!(owner.shown(&sink, &mt),
            Shown::Waiting { display: None, playing: Some(SCRIPTED_PLAYING_TASK.into()) });
        sink.mailbox.push(SCRIPTED_PLAYING_TASK.into(), 1);
        assert_eq!(owner.shown(&sink, &mt), Shown::Done);
        // The unload fence is a different task and still resolves after the show.
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        assert_eq!(owner.shown(&sink, &mt), Shown::No);
        sink.mailbox.push(5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
    }

    #[test]
    fn a_show_the_library_refused_is_not_accepted_and_waits_for_nothing() {
        use super::super::sink::ScriptedPlaneSink;
        let owner = PlaneOwner::new();
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        sink.failing.lock().unwrap().push("plane_finish");
        let mt = mt();
        owner.boot(PlaneFacts { handle: true, player_type: 0 });
        owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Strict, "m1");
        assert!(!owner.show(&sink, &mt));
        assert_eq!(owner.shown(&sink, &mt), Shown::No);
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

    // ---- two handles: Feed's (type 10) and the URI engine's (type 0) ----

    const FEED_ID: c_long = 11;
    const URI_ID: c_long = 22;

    fn both_booted() -> Plane {
        let mut m = Plane::new();
        step(&mut m, Event::Boot { facts: PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE } });
        step(&mut m, Event::BootUri { facts: PlaneFacts { handle: true, player_type: URI_PLAYER_TYPE } });
        m
    }
    fn bind_of(m: &mut Plane, handle: PlaneHandle, policy: BindPolicy) -> Vec<Fx> {
        step(m, Event::BindRequested { handle, media_id: "m".into(), policy })
    }

    #[test]
    fn a_device_with_only_the_uri_handle_has_a_plane() {
        let mut m = Plane::new();
        step(&mut m, Event::Boot { facts: PlaneFacts::ABSENT });
        step(&mut m, Event::BootUri { facts: PlaneFacts { handle: true, player_type: URI_PLAYER_TYPE } });
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(bind_of(&mut m, PlaneHandle::Feed, BindPolicy::Rebind), [Fx::Refuse(Refusal::NoPlane)],
            "Feed has no handle on this boot");
        assert_eq!(bind_of(&mut m, PlaneHandle::Uri, BindPolicy::Strict), [Fx::Bind { media_id: "m".into() }]);
        // The order of the two boots does not matter, and a device with neither has no plane.
        let mut m = Plane::new();
        step(&mut m, Event::BootUri { facts: PlaneFacts { handle: true, player_type: URI_PLAYER_TYPE } });
        step(&mut m, Event::Boot { facts: PlaneFacts::ABSENT });
        assert_eq!(m.state(), &State::Idle);
        let mut m = Plane::new();
        step(&mut m, Event::Boot { facts: PlaneFacts::ABSENT });
        step(&mut m, Event::BootUri { facts: PlaneFacts::ABSENT });
        assert_eq!(m.state(), &State::Unavailable);
    }

    #[test]
    fn a_bind_of_either_handle_is_refused_while_the_other_holds_the_plane() {
        for (holder, other) in [(PlaneHandle::Feed, PlaneHandle::Uri), (PlaneHandle::Uri, PlaneHandle::Feed)] {
            for policy in [BindPolicy::Strict, BindPolicy::Rebind] {
                let mut m = both_booted();
                assert_eq!(bind_of(&mut m, holder, BindPolicy::Strict), [Fx::Bind { media_id: "m".into() }]);
                assert_eq!(bind_of(&mut m, other, policy), [Fx::Refuse(Refusal::Bound)],
                    "{other} over {holder}, {policy:?}");
                assert_eq!(m.bound_handle(), Some(holder), "the refused bind took nothing");
            }
        }
    }

    #[test]
    fn feed_still_rebinds_its_own_handle_over_its_own_binding() {
        let mut m = both_booted();
        bind_of(&mut m, PlaneHandle::Feed, BindPolicy::Rebind);
        assert_eq!(bind_of(&mut m, PlaneHandle::Feed, BindPolicy::Rebind), [Fx::Bind { media_id: "m".into() }]);
    }

    #[test]
    fn a_bind_of_the_other_handle_waits_for_the_release_then_goes_ahead() {
        let mut m = both_booted();
        bind_of(&mut m, PlaneHandle::Feed, BindPolicy::Rebind);
        unload(&mut m, 1, 5, 100);
        for (handle, policy) in [(PlaneHandle::Uri, BindPolicy::Strict), (PlaneHandle::Feed, BindPolicy::Rebind)] {
            assert_eq!(bind_of(&mut m, handle, policy), [Fx::Defer], "{handle} waits on the other's UNLOADED");
        }
        assert_eq!(step(&mut m, Event::Completion { from: None, task: 5, ev: 1 }), [Fx::Released { task: 5 }]);
        assert_eq!(bind_of(&mut m, PlaneHandle::Uri, BindPolicy::Strict), [Fx::Bind { media_id: "m".into() }]);
        assert_eq!(m.bound_handle(), Some(PlaneHandle::Uri));
    }

    /// Reachable in the app only if a Start were admitted after a quarantined URI release: today the
    /// playback contract rejects every later Start while the session is not Vacant, so this pins the
    /// owner's rule, not a route a user can take.
    #[test]
    fn feed_binds_over_an_unproven_release_of_the_uri_handle_and_the_uri_engine_does_not() {
        let mut m = both_booted();
        bind_of(&mut m, PlaneHandle::Uri, BindPolicy::Strict);
        unload(&mut m, 1, 5, 100);
        step(&mut m, Event::Tick { now_ms: 100 + RELEASE_FENCE_MS });
        assert_eq!(bind_of(&mut m, PlaneHandle::Uri, BindPolicy::Strict),
            [Fx::Refuse(Refusal::Quarantined(Why::Deadline))]);
        assert_eq!(bind_of(&mut m, PlaneHandle::Feed, BindPolicy::Rebind),
            [Fx::OverQuarantine(Why::Deadline), Fx::Bind { media_id: "m".into() }]);
    }

    /// The owner over a scripted sink with both handles booted under the ids the sink returned.
    fn owner_of_two(sink: &super::super::sink::ScriptedPlaneSink) -> PlaneOwner {
        let _ = sink;
        let owner = PlaneOwner::new();
        owner.boot_handle(PlaneHandle::Feed, PlaneFacts { handle: true, player_type: DEFAULT_PLAYER_TYPE },
            HandleSeat { id: FEED_ID, initialized: true });
        owner.boot_handle(PlaneHandle::Uri, PlaneFacts { handle: true, player_type: URI_PLAYER_TYPE },
            HandleSeat { id: URI_ID, initialized: true });
        owner
    }

    #[test]
    fn the_owner_addresses_the_handle_it_granted_and_the_one_it_unloads() {
        use super::super::sink::ScriptedPlaneSink;
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        let owner = owner_of_two(&sink);
        let mt = mt();
        assert_eq!(owner.facts_of(PlaneHandle::Feed).player_type, DEFAULT_PLAYER_TYPE);
        assert_eq!(owner.facts_of(PlaneHandle::Uri).player_type, URI_PLAYER_TYPE);
        assert!(owner.initialized(PlaneHandle::Uri));
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Uri, BindPolicy::Strict, "m1"), BindVerdict::Granted);
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        sink.mailbox.push(5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Rebind, "m2"), BindVerdict::Granted);
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Rebind, "m3"), BindVerdict::Granted);
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Uri, BindPolicy::Strict, "m4"),
            BindVerdict::Refused(Refusal::Bound), "Feed holds the plane");
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        // Selected once per change of handle, never for a refused bind, and the unload addressed
        // the bound handle without a second select.
        assert_eq!(sink.calls(), [format!("plane_select {URI_ID}"), "plane_unload".to_owned(),
            format!("plane_select {FEED_ID}"), "plane_unload".to_owned()]);
    }

    #[test]
    fn a_select_the_sink_refused_is_retried_before_the_unload_and_does_not_refuse_the_bind() {
        use super::super::sink::ScriptedPlaneSink;
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        sink.failing.lock().unwrap().push("plane_select");
        let owner = owner_of_two(&sink);
        let mt = mt();
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Feed, BindPolicy::Rebind, "m1"), BindVerdict::Granted,
            "the machine granted it; a refused select is logged");
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        assert_eq!(sink.calls(), [format!("plane_select {FEED_ID}"), format!("plane_select {FEED_ID}"),
            "plane_unload".to_owned()], "a handle the owner never saw selected is selected again before the unload");
    }

    // ---- completions are matched on (handle, task): each core numbers its own tasks ----

    #[test]
    fn an_idle_cores_completion_sharing_the_active_tasks_id_does_not_settle_the_fence() {
        for (active, idle) in [(PlaneHandle::Uri, PlaneHandle::Feed), (PlaneHandle::Feed, PlaneHandle::Uri)] {
            let mut m = both_booted();
            bind_of(&mut m, active, BindPolicy::Strict);
            unload(&mut m, 1, 5, 100);
            // Both cores issue task 5 (the TV's callbacks showed task=1..4 on each).
            assert_eq!(step(&mut m, Event::Completion { from: Some(idle), task: 5, ev: 1 }),
                [Fx::Foreign { handle: idle, task: 5, ev: 1 }], "{idle} while {active} is releasing");
            assert!(matches!(m.state(), State::Releasing { task: 5, .. }), "the idle core's task 5 is not the fence");
            assert_eq!(step(&mut m, Event::Completion { from: Some(active), task: 5, ev: 1 }),
                [Fx::Released { task: 5 }]);
        }
    }

    #[test]
    fn an_idle_cores_completions_do_not_complete_the_active_show() {
        let mut m = both_booted();
        bind_of(&mut m, PlaneHandle::Uri, BindPolicy::Strict);
        finish(&mut m, 1, 3, 4);
        for task in [3, 4] {
            assert_eq!(step(&mut m, Event::Completion { from: Some(PlaneHandle::Feed), task, ev: 1 }),
                [Fx::Foreign { handle: PlaneHandle::Feed, task, ev: 1 }]);
        }
        assert_eq!(m.shown(), WAITING_BOTH, "neither of the idle core's tasks counted");
        step(&mut m, Event::Completion { from: Some(PlaneHandle::Uri), task: 3, ev: 1 });
        step(&mut m, Event::Completion { from: Some(PlaneHandle::Uri), task: 4, ev: 1 });
        assert_eq!(m.shown(), Shown::Done);
    }

    #[test]
    fn a_completion_of_a_handle_before_any_bind_is_foreign_and_one_naming_none_is_the_bound_ones() {
        let mut m = both_booted();
        assert_eq!(step(&mut m, Event::Completion { from: Some(PlaneHandle::Uri), task: 7, ev: 1 }),
            [Fx::Foreign { handle: PlaneHandle::Uri, task: 7, ev: 1 }]);
        // Nothing was kept as an early completion either: a dispatch that names task 7 waits.
        bind_of(&mut m, PlaneHandle::Uri, BindPolicy::Strict);
        assert!(unload(&mut m, 1, 7, 100).is_empty());
        assert!(matches!(m.state(), State::Releasing { task: 7, .. }));
        assert_eq!(step(&mut m, Event::Completion { from: None, task: 7, ev: 1 }), [Fx::Released { task: 7 }]);
    }

    #[test]
    fn the_owner_attributes_a_completion_to_its_handle_by_the_id_the_firmware_named() {
        use super::super::sink::ScriptedPlaneSink;
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        let owner = owner_of_two(&sink);
        let mt = mt();
        assert_eq!(owner.bind(&sink, &mt, PlaneHandle::Uri, BindPolicy::Strict, "m1"), BindVerdict::Granted);
        assert_eq!(owner.unload(&sink, &mt), Phase::Releasing);
        sink.mailbox.push_from(FEED_ID.into(), 5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Releasing, "the idle Feed core's task 5 is not the fence");
        sink.mailbox.push_from(99, 5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Releasing, "an id that is neither boot handle is ignored");
        sink.mailbox.push_from(URI_ID.into(), 5, 1);
        assert_eq!(owner.pump(&sink, &mt), Phase::Released);
    }

    #[test]
    fn the_owner_reports_an_acb_plane_when_either_handle_exists() {
        let seat = HandleSeat { id: 1, initialized: true };
        let facts = |handle| PlaneFacts { handle, player_type: 0 };
        for (feed, uri, any) in [(false, false, false), (true, false, true), (false, true, true), (true, true, true)] {
            let owner = PlaneOwner::new();
            owner.boot_handle(PlaneHandle::Feed, facts(feed), seat);
            owner.boot_handle(PlaneHandle::Uri, facts(uri), seat);
            assert_eq!((owner.handle(), owner.any_handle()), (feed, any), "feed {feed}, uri {uri}");
        }
    }

    #[test]
    fn a_handle_with_no_id_is_never_selected() {
        use super::super::sink::ScriptedPlaneSink;
        let sink = ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 });
        let owner = PlaneOwner::new();
        owner.boot(PlaneFacts { handle: true, player_type: 0 });
        assert_eq!(owner.bind(&sink, &mt(), PlaneHandle::Feed, BindPolicy::Strict, "m1"), BindVerdict::Granted);
        assert!(sink.calls().is_empty());
    }

    #[test]
    fn the_completion_queue_keeps_order_and_reports_what_it_dropped() {
        let q = CompletionQueue::new(2);
        q.push(1, 1);
        q.push(2, 1);
        q.push(3, 1);
        q.push(4, -1);
        assert_eq!(q.drain(), [
            PlaneCompletion::Done { id: 0, task: 1, ev: 1 },
            PlaneCompletion::Done { id: 0, task: 2, ev: 1 },
            PlaneCompletion::Dropped { count: 2 },
        ]);
        assert!(q.drain().is_empty());
        q.push(9, 1);
        assert_eq!(q.drain(), [PlaneCompletion::Done { id: 0, task: 9, ev: 1 }]);
    }
}
