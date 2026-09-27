# Poster request scheduling: typed state, explicit demand, bounded speculation

Design proposal, 2026-09-21. Source observations below are from this live worktree, not from the historical filenames in the July audit. No implementation or performance verification accompanies this document. The reported TV sequence—visible art queued 47th and dropped frames increasing from 68 to 247—is the task's supplied incident evidence, not a measurement reproduced here.

## 1. Problem and scope

Replace the poster adapter's implicit work protocol with a small, poster-specific request state machine. Keep two ordinary worker threads, the existing thread-spawn seam, and the SDL loop's ownership of application scheduling and result application. Make urgency and arrival order explicit data, independently of cache allocation. Do not migrate working one-shot mailboxes, playback services, or persistence into a common scheduler.

The immediate inversion has already been patched in [poster.rs](../rust-modules/src/app/adapters/poster.rs): `next_wanted` selects a visible request before a warm; `warm_admissible` rejects speculation while visible work is outstanding and limits non-visible outstanding work to one. Ordering *within* either group still follows slot index. `Pslot` still combines identity, integer lifecycle state, a sticky `visible` bit, retry fields, raw pixel addresses, and residency-related bookkeeping. The issue is now the implicit contract and remaining ordering ambiguity, not an unpatched absence of visible priority.

### Survey: actual thread shapes

I searched `rust-modules/src/**/*.rs` for `task::spawn*` calls and read their surrounding implementations, including delegated launchers. The snapshot contains **43 non-comment lexical calls: 41 non-test call sites and two test-fixture calls**. This counts source locations, not thread instances, work kinds, or simultaneously enabled features. The brief's “~20” and the decision log's historical fourteen are not current counts. Line numbers below are snapshot locators; symbols and labels are the durable references.

All paths in this table are relative to `rust-modules/src/`. `small` means `spawn_small`, `keep` means `spawn_small_keeping`, and `normal` means `spawn`.

| Source location(s) | Calls / labels | Shape and disposition |
|---|---|---|
| `app/adapters/poster.rs:992` | normal `poster`, called twice | Two interchangeable long-lived workers competing for images. **This proposal's scheduling domain.** |
| `browse/mod.rs:994,1975,2630` | small `directory`, `page`, `sources` | One-shot directory/page/source discovery, adapter mailboxes and caller-owned epochs/latches. Leave alone. `directory` serves genres and letters. |
| `browse/section_hubs.rs:406` | small `libhubs` | One-shot result with section/table/client validity checks. Leave alone. |
| `metadata.rs:2753,3188,3582` | small `detail`, `altsrc`, `season` | Distinct request/landing protocols; season failure is separate from an empty answer. Leave alone. |
| `person.rs:1472,1512` | small `person` at two locations | Profile/credits and per-server resolve/media/roles; per-slot claims and generation-tagged results. Leave alone. |
| `auth.rs:2345,2399` | small `probe` at two launcher definitions | Deadline-bounded connection-candidate races, including profile-resource probing. Not a reusable image-style worker pool. |
| `app/adapters/session.rs:229,259` | two small-spawn function-pointer installations | `start_work` / `launch_correlated` dispatch `login`, `rediscover`, `roster`, `roster-srv`, `switch`, `endpoint`; addressed progress/completions, admission reservations, cancellation. Not six additional lexical spawn calls. |
| `capture.rs:154,155` | normal `cap-listen`, `cap-encode` | Two different long-lived services, not two consumers of an interchangeable job queue. |
| `lab/upload.rs:116`; `lab/control.rs:117` | small `labup`; small `labctl` | One-shot upload versus long-lived polling service. Small stack does not imply short lifetime. |
| `player/adapter.rs:59` | small `jail repair` | One-shot repair with an attempt token and a receiver polled on app frames. |
| `player/engine.rs:1285,1306,1340` | normal `demux`, `media`, `timeline` | Session-owned pipeline/service threads with distinct lifetime and teardown rules. |
| `player/sidecar.rs:120` | small `sidecar` | A single worker loops over a coalesced latest pending subtitle request, then exits. Neither thread-per-request nor a permanent pool. |
| `route/decision.rs:2052,3742,3764,4202,4504,5257,5499,6150` | small `abr-cleanup`; keep `abr-original-stop`, `abr-original-physical-stop`, `scrobble`, `seek-stop`; small `resolve-abandoned-stop`, `resolve`; keep `retranscode-stop` | Resolve is a generation-guarded request; the others retire server resources / preserve stop ordering. Some refusal paths deliberately fall back synchronously. They must not become droppable low-priority art work. |
| `pms.rs:1589` | small `hubs` | Request object captures source/client; worker publishes completion. |
| `search.rs:1207` | small `search` | Per-server request/response with generation and captured favourite projection. |
| `search/recents.rs:86` | small `recents-save` | Persistence flush, not a visual response. |
| `plex/serverinfo.rs:159` | small `serverinfo` | Per-server single-flight facts refresh, retaining old facts on failure. |
| `viewstate.rs:434` | small `viewstate` | Serial queued writes; requeues at the head on refused spawn. FIFO expresses mutation ordering, not visual urgency. |
| `telemetry/mod.rs:236,248`; `telemetry/oneoff.rs:104` | small `telemetry`, `telemetry-retry`, `oneoff` | Coalesced flush, delayed retry worker, and bounded fallback delivery. No shared poster-capacity competition. |
| `storage_worker.rs:170` | normal, parameterized name (`persistence` in shared use) | Bounded serial persistence FIFO, capacity eight plus executing work, typed replies. Already has its own required ordering. |
| `dev/scenarios.rs:191` | small `logintest` | Diagnostic one-shot. |

Excluded fixtures: `browse/mod.rs:2992` (`browse-owner-test`) and `app/run.rs:3038` (`lifecycle-fixture`). Calls inside `task.rs`'s own tests are unqualified helper calls, not production launch sites.

Two further corrections matter. [metadata.rs](../rust-modules/src/metadata.rs)'s `fetch_full` uses `Builder::spawn_scoped` for `detail-extras` around line 2565, and [player/ffi_host.rs](../rust-modules/src/player/ffi_host.rs)'s `Clock::start_ticker` uses `Builder::spawn` for the host clock sink around line 328. Both inspect the `Result`; neither is a `task::spawn*` call. Thus “every spawn site uses task.rs” is too broad for this snapshot. This proposal introduces no further exception and does not repair those unrelated sites.

Also, [storage_worker.rs](../rust-modules/src/storage_worker.rs) already calls its serial persistence wrapper `Executor` and its boxed closure `Job`. That is existing blocking FIFO infrastructure, not permission to introduce an async executor here. Posters remain the surveyed **multi-worker interchangeable pool**; they are not the only background queue. A single abstraction covering all these shapes would discard important semantics.

“Leave alone” means no demonstrated need for poster-style priority arbitration, not a claim that every error/lifetime path is flawless. In particular, mutation ordering, screen ownership, and connection racing are different problems from deciding which image gets one of two decoders next.

### The concrete boundaries we must preserve

* [task.rs](../rust-modules/src/task.rs) uses `Builder::spawn` and reports refusal, returning `Option<JoinHandle<()>>` or `bool`. Its documented reason is the panic from `std::thread::spawn` on OS refusal crossing the C entry seam. `spawn_small` chooses 256 KiB; its own contract excludes demux/decode/encode. Poster workers currently use normal `spawn` and must continue to do so. Caller-specific refusal cleanup stays with the caller.
* The poster store has 64 slots and 256-byte path storage, with `KEY_MAX = 255`; identity is server plus the complete built image path (including size/format and token). Fetch/decode happens off-lock; publication checks slot generation and `P_LOADING`. `P_READY` means handed to the render cache, **not resident**. `drain_decoded` currently copies C pixels into `tex::Decoded`, then `tex::accept` queues them; `prepare` uploads under the frame budget. See `poster.rs::{lookup,poster_worker,drain_decoded}` and [ui/tex.rs](../rust-modules/src/ui/tex.rs).
* The main loop calls `begin_frame` and `drain_decoded` before its texture `prepare` path ([app/run.rs](../rust-modules/src/app/run.rs), around lines 327–409). Main-thread retry deadlines use the app clock; workers park transient results without reading that clock. Retry waits are 1, 2, 4, 8, 16, then 30 seconds. Only a draw restarts a due retry; warm requests do not retry failures.
* `poster_worker` also reads/writes the existing avatar disk cache and may do a `refresh_after` fetch **after publishing pixels**. This still consumes a worker after the slot looks complete. Moreover, on its successful publication path the local `MutexGuard g` has no explicit drop before that refresh block; only the stale-result branch drops it. The new protocol must make the lock scope end before all subsequent I/O. Treat this as a source-level concern requiring a blocked-refresh regression, not a newly measured TV stall.
* The target constraints are real: [agent-reference.md](agent-reference.md)'s toolchain section specifies glibc 2.12, ARMv7-A / 32-bit ARM; `ui/tex.rs::TEX_RESIDENT_BYTES_MAX` is 44 MiB (scaled by render area at cache construction). The texture budget is not a bound on compressed input, decoder scratch, pending RGBA, or worker stacks.

## 2. Typed model

These are proposed Rust declarations and interface sketches, not compiled code. Keep them local to the poster adapter and its private worker/policy modules. There is no generic task type, arbitrary closure queue, future, runtime, reactor, actor framework, or dependency.

Use a fixed slot table for identity and LRU, but choose work by explicit request fields. A bounded linear minimum over 64 records is sufficient; a heap with stale promotion entries adds machinery without removing meaningful work.

```rust
const POSTER_SLOTS: usize = 64;
const POSTER_WORKERS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
struct SlotId(u8);                 // private checked constructor: < POSTER_SLOTS
#[derive(Clone, Copy, PartialEq, Eq)]
struct SlotGeneration(u64);
#[derive(Clone, Copy, PartialEq, Eq)]
struct RequestId(u64);             // unique per attempt, including retries
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct QueueOrder(u64);            // unique entry order into the current class
#[derive(Clone, Copy, PartialEq, Eq)]
struct ImageHandle { slot: SlotId, generation: SlotGeneration }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Priority { Visible, Speculative }
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkKind { AcquirePixels, RefreshAvatarFile }

#[derive(Clone, PartialEq, Eq)]
struct ImageIdentity {
    server: crate::plex::ServerId,
    path: Box<str>,                // validated full path, <= existing KEY_MAX
}
struct ImageRequest {
    id: RequestId,
    image: ImageIdentity,
    priority: Priority,
    order: QueueOrder,
    kind: WorkKind,
}

struct LruStamp { last_draw: u32, last_frame: u32 }
enum SlotState {
    Empty,
    Occupied(ImageSlot),
}
struct ImageSlot {
    handle: ImageHandle,
    image: ImageIdentity,
    lru: LruStamp,
    attempts: u8,
    phase: ImagePhase,
}
enum ImagePhase {
    Queued(ImageRequest),
    Running(RunningImage),
    Decoded { request: ImageRequest, pixels: crate::ui::tex::Decoded },
    Delivered,                    // replaces READY; makes no residency claim
    Failed(PermanentFailure),
    Retry { cause: TransientFailure, wait: RetryWait },
}
struct RunningImage {
    request: ImageRequest,         // authoritative, can be promoted under the lock
    worker: WorkerId,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct WorkerId(u8);               // exactly 0 or 1
enum RetryWait {
    AwaitingVisibleDraw,
    Scheduled { at_ms: u32, wake_sent: bool },
}
enum PermanentFailure { Http(u16), Decode }
enum TransientFailure { NoServer, NoResponse, EmptyBody, Http(u16), WorkerUnwind }
```

`Empty` cannot carry pixels; only `Decoded` owns pixels; retry deadlines cannot accidentally apply to queued work. `ImageRequest` carries priority even when its slot index is low or high. `Delivered` names the existing source/cache handoff truthfully. `Failed` is a negative result, not “no request.” The source keeps image identity through settled states for deduplication. Duplicating at most one short identity into an active request is bounded; use neither interned global strings nor per-draw allocation on hits.

The attempt ID is separate from slot generation: a retry is a new attempt for the same slot allocation. Counters are protected by the store mutex, not 64-bit atomics. Use checked increments; on exhaustion refuse new admission with a logged terminal capacity reason until reinitialization after workers have joined. Do not wrap an ID into a live identity.

Worker occupancy must be distinct from slot lifecycle, because cancellation, promotion, and file refresh need not release a physical worker:

```rust
struct Flight {
    id: RequestId,
    target: WorkTarget,
    started_speculative: bool,     // immutable reservation until this attempt exits
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkTarget { Image(ImageHandle), AvatarRefresh }
enum WorkerState { Unavailable, Idle, Busy(Flight) }

struct RefreshWork {
    request: ImageRequest,         // always Speculative + RefreshAvatarFile
    disk: crate::imgcache::DiskKey,
    cache_generation: u64,         // imgcache::generation() returns u64
    phase: RefreshPhase,
}
enum RefreshPhase { Offered, Queued, Running(WorkerId) }

struct PosterQueue {
    slots: [SlotState; POSTER_SLOTS],
    generations: [SlotGeneration; POSTER_SLOTS],
    workers: [WorkerState; POSTER_WORKERS],
    refresh: Option<RefreshWork>,  // at most one best-effort file refresh record
    next_request: u64,
    next_order: u64,
    uploads_pending: bool,         // last main-loop-published pressure snapshot
    retire_cache: u64,             // one pending main-thread disposal bit per slot
    stopping: bool,
}

struct WorkLease {                 // moved to one worker; no GL or screen references
    worker: WorkerId,
    id: RequestId,
    image: ImageIdentity,
    payload: WorkPayload,
}
enum WorkPayload {
    AcquirePixels { target: ImageHandle },
    RefreshAvatarFile { disk: crate::imgcache::DiskKey, cache_generation: u64 },
}
enum WorkOutcome {
    Pixels(crate::ui::tex::Decoded),
    Retry(TransientFailure),
    Failed(PermanentFailure),
    RefreshFinished,
}
```

`WorkLease`'s payload enum makes file-refresh data exclusive to refresh work. Private constructors enforce that an image request has `AcquirePixels` kind and that a refresh request is Speculative; `finish` matches payload and outcome before publication. No `Clone` for pixel ownership or work leases. `Flight` remains in the store until worker completion even if an image slot is retired. The current two handles are retained for shutdown; failed starts mark workers `Unavailable`.

Use owned `tex::Decoded` across the mailbox. Copy C-decoder pixels into a validated `Box<[u8]>` on the worker and immediately free the C allocation, instead of retaining an integer pointer in shared state. Validate dimensions and checked `width * height * 4` arithmetic on ARM32 before copying. This moves the existing copy off the loop; it is not a zero-copy claim and temporarily holds both allocations. Keep this resource-ownership change separately reviewable from queue ordering. No new `unsafe impl Send` or “pointer as usize makes it safe” argument is needed.

### Admission, promotion, claim, and landing

```rust
enum Admission {
    Queued(ImageHandle),
    Existing(ImageHandle),         // may also have promoted it
    Deferred(AdmissionRefusal),
    InvalidImage,
}
enum AdmissionRefusal { BusyVisible, SpeculationLimit, NoSlot, NoWorker, Stopping, IdExhausted }
enum Promotion { Changed, AlreadyVisible, Settled, Stale }

impl PosterQueue {
    fn request(&mut self, image: ImageIdentity, priority: Priority,
               frame: u32, now_ms: u32) -> Admission;
    fn promote(&mut self, image: ImageHandle) -> Promotion;
    fn claim_next(&mut self, worker: WorkerId) -> Option<WorkLease>;
    fn finish(&mut self, lease: WorkLease, result: WorkOutcome);
    fn offer_refresh(&mut self, image: ImageIdentity,
                     disk: crate::imgcache::DiskKey, cache_generation: u64) -> bool;
    fn pump_admission(&mut self, uploads_pending: bool);
}

// Main-loop bridge, outside the pure queue implementation:
fn drain_to_cache(mt: &crate::task::MainThread);
```

These are pure state operations under one mutex; no network, disk, decode, joins, GL calls, or app-clock reads while holding it. `now_ms` is supplied by the main-loop caller. `claim_next` is private poster-queue arbitration used by the existing sleeping workers, not a second application scheduler: workers can take admitted image work and report results, but cannot schedule screen actions, run continuations, or advance app state. The SDL loop remains the only application scheduler; this preserves the current worker-pull mechanism without adding a frame of dispatch latency.

`request` validates the path, deduplicates by full identity, records draw LRU protection for Visible demand, and calls `promote` on a matching outstanding image. Only misses allocate. It returns typed deferral without occupying a slot when admission fails. The existing `Source` facade maps this to `Warm::{Known,Claimed,Full}` and the draw's optional key; failure never looks like a successfully queued request. Preserve the current LRU victim rule: empty first, then settled entries not drawn this frame. Queue arrival time never affects that rule.

`claim_next` computes `(priority_rank, order)`, with Visible rank zero, and changes Queued to Running while assigning the idle worker in the **same critical section**. Each claim is unique; simultaneous workers cannot take the same attempt. Slot index is used only to retrieve storage, never to break scheduling ties. Unique `QueueOrder` removes ties. The returned lease captures the full server/path identity; execution uses the existing client/art-fetch and decoder paths.

`finish` validates worker, request ID, slot generation, and Running phase before replacing the phase. Use the **current stored request**, not the lease's original priority, so a concurrent promotion survives publication. A stale result drops its owned pixels and releases exactly its own worker reservation; it cannot overwrite a recycled slot or release a newer flight. A worker-side completion guard / unwind boundary publishes failure and releases the flight for a Rust panic; native process crashes are outside this protocol. Notify waiting workers after releasing the lock. Condvar waits always recheck stopping and eligibility.

The loop drains decoded results by the same explicit priority/order, moves their pixels to `tex::accept`, and sets Delivered. The existing render cache still owns upload ordering and budgets; no GL work moves to a worker. Slot recycling sets its bit in `retire_cache`; `drain_to_cache` first takes that bounded disposal mask and frees old render-cache entries/pending pixels off-lock, then hands off replacements using the same external `PosterKey`. Source lookups can therefore remain free of GL calls and token plumbing. The existing rule against recycling a slot drawn this frame remains essential. Keep worker generation checking internal; do not pretend the existing slot-only `PosterKey` became generational without changing that seam.

Confinement must be honest. The pure queue may safely be locked from either thread; it owns no screen state or GL resources. Main-thread cache disposal/upload boundaries should require `&task::MainThread` as a **function argument**, through the app's existing token-owning path, with raw operations private. Do not add a marker field to `static mut` and claim a guarantee: [the decision](async-model-decision.md)'s compiled counterexample refutes it. Nor does `Send` prevent arbitrary global access. This design relies on owned messages, narrow private APIs and seam review, not a claim that workers cannot spell a global path. The existing `Source` trait need not become a generic job interface.

## 3. Scheduling policy

### Two classes, stable order

**Visible** means a real draw has demanded these pixels during this attempt. **Speculative** means a lookahead/hero warm or a best-effort avatar-file refresh. No focus-distance score, deadline estimator, hero override, or aging boost in the first version. All visible images are peers. Speculation may starve indefinitely during continuous visible demand; that is acceptable because it is optional.

FIFO is **time of entry into the current priority class**. A new visible miss receives a fresh order. A queued warm becoming visible receives a fresh *visible-class* order exactly once, behind already-waiting visible requests and ahead of every speculative request. Repeated draws and repeated promotion do not reset its order. This avoids giving a long-idle warm priority over work the user was already waiting for. A retry gets a new request ID and order after its deadline and renewed visible demand.

Promotion is monotone for the attempt, matching today's sticky bit: a warm cannot demote previously visible demand. Promote Queued, Running, or Decoded in place without another fetch. Running promotion cannot change when the in-flight fetch finishes. Delivered/Failed remain settled; a Retry schedules/requeues only through the existing visible-draw/backoff rule. A file-only refresh cannot turn into a pixel acquisition by promotion; a draw needing absent pixels creates/coalesces an AcquirePixels request instead.

This is not a live viewport tracker. Images once requested visibly can remain high priority after scrolling away. That is an explicit first-version limitation; adding visibility expiry requires an owner/scope protocol and evidence, not a hidden “last frame” heuristic that confuses idle frames or transitions with loss of interest.

### Admission is more important than preemption

**A worker cannot interrupt a network fetch already in flight off-lock.** It also cannot preempt a decoder invocation. Generation invalidation cancels interest in a result, not occupied capacity. Do not add socket shutdown hooks to this scheduling change; the historical transport work shows why that is a separate design and device-verification problem.

Retain the point fix's conservative behavior and make its reservations explicit:

1. No new speculative admission while any Visible acquisition is Queued, Running, or Decoded. A visible request remains eligible for whatever capacity and slots exist — **except when its drawn card has unknown placement or moves faster than the card admission threshold**, where `ui::card_motion`'s scoped verdict declines it in `poster.rs` before source admission. Featured hero/backdrop/logo images and the Info panel's single still are outside this card scope: translating those images reveals no scrolling tile identities. That refusal is deliberate and measured, and the typed queue must keep it rather than merely rank it: pacing the visible request instead (one in flight, perfect pacing — zero refusals recorded) still cost twice the ungated baseline's dropped frames, because by the time a request is being ordered its fetch and decode are already owed. See the measured table at that gate in `lookup`.
2. At most **one** speculative obligation exists across queued warms, still-unpromoted decoded warms, file refresh, and speculative-started busy workers. Count an attempt once, even if both its slot and flight describe it. Hold a speculative-start reservation until that worker actually finishes, even if the request is promoted, retired, or its cached pixels were published. A promotion therefore cannot accidentally authorize a second speculative fetch while the first worker is still busy.
3. A surviving-worker count below two disables speculation entirely. With zero workers, return `NoWorker` rather than parking forever in Queued. This improves the refusal behavior explicitly; it does not retry thread creation on each draw.
4. Before starting queued speculation, `claim_next` rechecks that no visible acquisition is outstanding. Admission and claim are distinct: visible work may have arrived in between. A queued speculative request occupies no worker and is bounded to one record. Do not spawn another worker to evade this policy.
5. A slot shortage returns `NoSlot`; it does not evict visible/in-flight work or allocate an overflow queue. The loop will retry a visible miss on a later draw. If all slots are busy, their completion/present path must still wake the UI. Add explicit invalidation for terminal failures too, so capacity becoming available cannot remain invisible indefinitely.

The “one speculative obligation” count ends at handoff for a pixel result, as today's `P_DECODED` cap does; uploaded/resident art is not forever speculative work. Separately, block new speculation while `tex::has_pending()` is true, using a main-loop-published cache-pressure snapshot in the admission facade. This prevents a slow upload stage from collecting a new speculative image every frame. A snapshot can be conservative/stale by a frame; it is an extra gate, never evidence to override the hard flight cap. Worker-side claims use queue state and this last published gate, never read the main-thread cache directly.

### Account for the avatar refresh tail

An old avatar-cache hit may publish pixels quickly and then spend a network round trip refreshing its file. The current implementation does this inline in the same worker. Slot-based priority alone cannot account for it, and two visible avatar hits can otherwise turn both workers into background refreshers.

Split that tail into an optional `RefreshWork` offered after the image flight finishes. `Offered` retains one candidate's metadata, with no worker, pixels, or admitted capacity reservation. This distinction matters: immediately after publishing a visible avatar, its pixels are still Decoded, so trying to admit the refresh immediately would reject practically every refresh. The loop's `pump_admission`, after drain/prepare and also on idle iterations, updates cache pressure and moves Offered to Queued only when the same speculation gates permit it, assigning a fresh admission order. It notifies the worker condvar after unlocking. An Offered candidate is not an outstanding speculative obligation; Queued and Running refreshes are. No screen spinner or source-busy present requirement is attached to an Offered candidate.

Drop another offer if this one-record allowance is unavailable; the existing stale file can be reconsidered on a later image acquisition. There is no additional queue of rejected offers. A refresh performs fetch, decode validation and generation-guarded disk write off-lock, publishes no texture replacement, and releases its flight only after disk work ends. Visible work wins the next claim. Once refresh I/O has started it remains non-preemptible and counts toward the one speculative flight limit. Discard Offered/Queued candidates when their account-cache generation expires; a Running refresh still releases its worker and relies on the write fence.

Preserve `imgcache::write_at`'s account-generation fence; a request ID does not replace that authorization check. Do not let cache generation reset race a stale write. This changes refresh timeliness: the old “within a day” prose is no longer an unconditional promise under sustained foreground demand. Update that claim if/when this step ships.

### Memory and end-to-end limits

Keep 64 image slots, two workers, and at most one optional refresh record. No unbounded promotion nodes, completion channel, or rejected-request backlog. An image's pixels have one Rust owner after the C-copy boundary, and a stale landing is dropped outside the store lock. Keep destruction of larger buffers outside critical sections too.

Slot bounds are not byte bounds. The initial migration must preserve existing image-size validation and render budgets and measure pending RGBA / decoder high-water memory; it must not claim a 44 MiB total-memory ceiling. A new decoded-byte cap would require size-aware reservation before decoding and a policy for a single large backdrop; choosing that number without imgtrace is deferred. No extra decode stage, parallel decoder, or larger prefetch window is justified by this proposal.

Draining Visible decoded results first improves the next handoff, but `TexCache::accept` appends to a FIFO and `prepare` stops when the head's budget class is refused. Images already handed off may still precede a newly visible image. This design guarantees priority **at worker claim and adapter drain**, not earliest on-screen completion. Keep the upload-budget policy intact. If imgtrace identifies that FIFO as the dominant inversion, design a separate renderer-owned priority seam with promotion/expiry; do not bypass `Budget` here.

## 4. Migration and verification

This document was the entire change at `8eea8340`; `ce48077f` then added the fast-scroll request gate on top of it. The steps below are future work in isolated development, not authorization to edit this live tree. Each verified piece lands as one squash commit. A policy change is compared with the visible-first baseline **and** that gate — not the old broken index-only scheduler, and no longer visible-first alone. The distinction is operational: a moving card's new demand is declined before queueing, while known stationary cards and featured images can still produce visible work. Gather queue-ordering A/B evidence with known card placement below `ui/card_motion.rs`'s `MAX_SPEED` (120 px/s), or with the gate explicitly disarmed and reported as such. Unknown card placement also defers admission until a later sample; existing resident art remains drawable throughout.

| Piece | Scope | Evidence required before landing | Reversion |
|---|---|---|---|
| Prerequisites | Follow the agreed resident-eviction fix, then per-image imgtrace work. | Reproduce the residency fault against the old behavior; prove its repair separately. Trace queue wait, fetch, decode, handoff, upload, residency loss and first visible use without logging token-bearing paths. | Separate commits; neither is hidden inside the scheduler. |
| A: typed lifecycle | Replace `c_int` phases with payload enums and name Delivered truthfully; preserve patched selection/admission, transport, retries, cache handoff and worker count. | Pure transition tests plus existing poster regressions. Resource drop counters prove one owner / one free for stale, failed, drained and shutdown paths. If moving the C copy changes timing, keep that subchange as its own verified piece. | Independently revertible before B; after B, revert dependents first. Do not imply source-level refactors are order-independently revertible. |
| B: explicit request ordering | Add attempt IDs, class-entry order, typed admission and promotion; use the bounded minimum. Keep two workers and normal stacks. | First add an index-permutation/FIFO regression that fails the patched current implementation. Then verify visible-first, class FIFO, one-time promotion and retry order; deterministic two-worker claim races and refused starts. | B can be reverted to A while retaining typed phases. No flag making bare slot-order dispatch an alternative shipping architecture. |
| C: complete occupancy accounting | Add flight reservations, zero/one-worker admission behavior, cache-pressure gate, and separately admitted avatar refresh. | A blocked mock refresh must leave store admission responsive; a promoted or retired speculative flight must still reserve capacity. Check both workers cannot enter speculative I/O, stale account-generation writes are discarded, refresh produces no texture swap, and shutdown/refused spawn/unwind release ownership. | C can revert to B's existing refresh behavior and point-fix limits; document that the stronger occupancy guarantee is then withdrawn. |

Keep tests local to the pure poster policy and controlled mock transports; there is no need to change `tests/manifest.json` or `tests/run.py`. Required cases include: a visible slot at index 47 beats speculative low indices; earlier visible arrival beats a lower-index later arrival; duplicate draws do not refetch; promotion during fetch survives landing; generation mismatch and retry-attempt mismatch discard safely; no warm restarts Retry; wrapping app-clock deadlines wake once; full capacity and zero workers do not create phantom requests; shutdown closes admission before notifying/joining workers.

Following [which-tier](../.agents/skills/which-tier/SKILL.md), host tests prove ordering, ownership, boundedness and failure paths. For future Rust changes run the repository's host/lint gate, shipping `--no-default-features` check and ARM build on the committed, clean candidate, and verify cleanliness afterwards. Record the actual `test result:` line and skips where applicable. These gates are **not run for this document**.

Use the simulator against a mock art server to check that visible skeletons resolve, late results repaint and settled retries do not spin. Simulator timing does not prove ARM decode speed, GLES uploads, memory pressure or dropped-frame improvement. Final queue-policy acceptance needs a TV lease through `tv-lock` and `tv-session`, a confirmed guest identity, and mocked content so no household viewing state is written. Compare identical cold/warm scroll traces, art dimensions, server delays and instrumentation on/off conditions. Capture queue wait for Visible requests separately from fetch/decode time, upload latency, first-visible latency, pending bytes and dropped frames. A scheduler that reduces queue wait but worsens scrolling or memory high-water fails acceptance; no invented numerical improvement target replaces the matched baseline.

Before each behavior piece, audit prose made false by the change: `P_READY` terminology, worker-copy location, priority claims, avatar refresh timing, `idle` semantics, and any claim that every accepted image is resident. Review the decoder/copy ownership manually for ABI and allocator compatibility if that seam changes; this design adds no new FFI binding. Shutdown still joins ongoing work and can wait for network deadlines; do not label this work cancellation-with-teeth.

## 5. What this does not fix

**Two workers remain two workers. Their count is frozen until per-image imgtrace measurement exists.** The agreed order supplied for this task is **resident-eviction bug → imgtrace → lookahead/worker-count tuning from measurements → disk cache**. This design is preparation for that measured work, not permission to skip ahead or bundle those projects together.

A scheduler cannot make two workers decode faster, shorten a slow server response, remove texture-upload stalls, or make 64 logical identities fit under the texture byte budget. `Delivered` does not repair the source/cache residency contract: `TexCache::evict_for` can drop residency independently of the adapter slot. That boundary is why resident-eviction work comes first. The scheduler also does not add transport cancellation, cure playback's blocking control paths, or establish a process-wide network budget.

“Then a disk cache” must mean a broader poster-art cache: [imgcache.rs](../rust-modules/src/imgcache.rs) already caches avatars. Its module prose currently says posters “never will be” cached, which conflicts with the requested later roadmap. Record that contradiction rather than claiming no disk tier exists; changing that policy and its prose belongs to the later cache project.

**Is this worth doing on its own merits? Yes, as a bounded correctness and maintainability change; not as a throughput optimization.** Explicit FIFO within priority, typed impossible-state prevention, and accounting for hidden refresh occupancy are independently useful. The visible-first point fix already provides the main incident remedy, so a generic background-work rewrite would not earn its cost. If the implementation expands into cross-screen state ownership or a renderer rewrite, stop at the typed poster protocol and retain the patch. Performance claims wait for the measurements above.

## 6. The declined `Job<T>` and the reversal trigger

The binding entry is [async-model-decision.md, “Step 3 — `Job<T>` DECLINED on re-evaluation (2026-07-28)”](async-model-decision.md#step-3--jobt-declined-on-re-evaluation-2026-07-28):

> By the time step 3's turn came, steps 1, 2, 5, 6 and 7 had all landed **by hand**, device-verified — so `Job<T>` was no longer "the way the callers get written," it was a rewrite of five working ones.

And, specifically:

> **"if any caller needs a per-site flag to fit" — fired outright.** `browse.rs` gates its spawn on `done` — a landed-empty list is an answer, not an absence — which is screen state, not in-flight state, and `Job` cannot own it.

It also records that the three in-flight forms (`PLAY_BUSY`, `GEN`/`DONE`, single-flight `AtomicBool`) drive different spinners, and concludes: **“The piece worth sharing was the spawn, not the mailbox.”** The current `browse/mod.rs::kick_directory` still takes `done` and declines spawning when it is true; this objection remains concrete, not merely historical.

This design shares typed outcomes, generation rejection, explicit admission, caller-visible refusal, and main-loop result application with the declined proposal. Those are properties of the house idiom, not evidence that it needs a reusable `Job<T>`. The proposed queue has one domain, fixed payloads and one real contested resource: two poster workers. Priority is an image-request property; `started_speculative` is physical-capacity accounting, and retry/negative art results are image-domain state. None is an accommodation for a screen's loaded-empty list, spinner, navigation history, or mutation acknowledgment. The owner still decides when to draw/warm; the queue answers whether and when that requested image work can run.

Thus the per-site flag trigger does **not** fire for this narrowly scoped design. It **would** fire if browsing, metadata, auth or playback were made clients and required flags such as `done`, spinner variants, “must persist despite cancel,” or callbacks into screen state. That is an explicit stop condition, not a future generic extension point. Do not infer that every other mailbox is sound merely because it stays out of this queue.

### What the full historical documents complicate

The July review reports 97 findings, 84 confirmed, but its broad opening diagnosis and proposed Phase B/C are historical. The later implementation log overrides the initial “every off-loop operation becomes a Job” decision. It also refutes the review's claim that making `apply_plan` the sole writer fixed a cross-thread codec read and its suggestion that the transcode decision request was pure removable cost. Neither claim is a rationale for this scheduler.

More importantly, the **2026-09-10 addition under Step 3 explicitly reverses the broader premise** that the five hand-written mailboxes remain sound by construction: `NavStack` can retain multiple entries of the same screen kind and evict/remount their bodies, so “newest generation” and “the requesting entry” are no longer equivalent. The log says no misdelivery had been observed there; it does not reinstate `Job<T>`. This complicates the brief's “already working callers” shorthand. A later screen-ownership design may be warranted on its own evidence. This poster design neither dismisses it nor claims to solve it: shared images coalesce by full image identity, while `ImageHandle` and `RequestId` fence storage reuse and attempts, not navigation entries.

The Step 4 log also describes a later legitimate token **field** in an owned `PlayerAdapter`, after removal of `static mut ENGINE`. That is not the refuted marker-field trick on a global. Keep those cases distinct; new poster GL boundaries use token arguments and do not assert that a marker makes a static safe.

Finally, cancellation history is not a single permanent retreat to flags: Step 1's fd publication was reverted and later re-landed, and the audit-round entry subsequently closes the recycled-fd race and reporter-join issues. Conversely, Step 9 is recorded as **not done / not decided**, including a correction that the supposedly visible spinner did not represent those blocking waits. Those are historical playback/transport findings, not proof that poster fetches are interruptible today. The actual poster worker/shutdown path inspected here exposes no per-request interruption handle. Preserve that limit and scope this proposal to ordering work that has not started.
