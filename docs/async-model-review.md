# Async model review (2026-07-27)

Audit of the app's concurrency model, prompted by the observed bug: **while the player loads the
stream, the HUD is frozen until it completes.**

Method: six parallel readers over the concurrency surfaces (main loop, play/route path, player
engine, the existing async idioms, the stream/demux pipeline, UI state), each followed by an
adversarial verifier that re-read the cited code and tried to refute the claims. **97 findings
raised, 84 confirmed, 13 refuted.** Every claim below is cited to `file:line`.

---

## 1. The verdict in one paragraph

There is no async model. There is a **strictly serial single-threaded frame loop** in which any
operation that needs the network is performed inline, and the frame simply does not present until
it returns. `plex_run` has exactly one `SDL_GL_SwapWindow` (`app.rs:2075`); everything from the
poll loop (`app.rs:799`) onward runs before it. So a blocking call anywhere in event handling, the
per-frame pump, or draw is a **directly visible freeze**.

The codebase already contains the *correct* idiom — invented for `metadata::load_season`
(`metadata.rs:527-561`), hardened in `browse.rs` (single-flight + generation + failure sentinel +
retry backoff). Posters, browse pages, season fetches and auth all use it properly. **The two
paths that matter most — playback resolution and detail loading — never adopted it.** That
asymmetry is the whole defect. This is not an engine problem.

---

## 2. Why the HUD freezes — the reported bug, root-caused

Two independent causes stack.

### 2a. The play ritual runs inside the SDL key handler

`route::play_movie` / `play_episode` → `build_stream` (`route.rs:335-444`) performs serial,
blocking PMS round trips *inside the event handler*, then `metadata::load_detail`
(`app.rs:651`) adds more. Neither yields a frame.

Per keypress, in order:

| # | Call | Site | Notes |
|---|---|---|---|
| 1 | `ensure_playqueue` → `GET /identity` | `route.rs:354`→`:300` | memoized after first play |
| 2 | `ensure_playqueue` → `POST /playQueues` | `route.rs:319` | every play, ~100-300 ms |
| 3 | `metadata::load_playing` → `GET /library/metadata` | `route.rs:358`→`metadata.rs:345` | skipped on a detail-store hit |
| 4 | `server_decision` → `GET /decision` | `route.rs:390`→`:196` | rare branch only |
| 5 | `put_selection` → `PUT /library/parts` | `route.rs:438`→`:273` | transcode branch only |
| 6 | `transcode_decision` → `GET /decision` | `route.rs:440` | transcode branch only |
| 7 | `metadata::load_detail` | `app.rs:651`→`metadata.rs:472` | **2 GETs (movie) / 5 (show)** |
| 8 | `engine::resume_at` → `/decision` | `engine.rs:379` | on a resume |

`load_detail`'s own doc comment already admits it: *"Blocks on several HTTP round-trips… called
synchronously when opening the detail page"* (`metadata.rs:470-471`).

Every one of these bottoms out in `plex/client.rs:100/116/121` → `stream.rs`'s blocking raw
socket, and **each opens a brand-new TCP connection** — `Connection: close` (`stream.rs:233`).
There is no keep-alive and no pooling, so every round trip pays a fresh handshake.

**Measured bounds from the code's own constants:** typical direct-play MKV is ~4 serial round
trips (**≈300 ms–2 s**); the transcode path ~6; the Home→show→season arm ~9-11. When PMS is
degraded the ceiling is `CONNECT_TIMEOUT_MS = 2000` (`stream.rs:119`) plus a 15 s `SO_RCVTIMEO`
(`stream.rs:212`) **per request** — a 12 s to 150 s hard freeze.

### 2b. There is no loading state to draw even if the loop were free

- `Route::Player { overlay }` (`app.rs:537`) has **no loading substate**. The route flips straight
  to "playing" and the draw dispatch has nothing to show.
- `player::loading()` is `SHARED.seeking` (`mod.rs:79`), written **only** by `request_seek`
  (`mod.rs:73`). It is false during the initial load, so the `Spinner` branch that already exists
  at `player_hud.rs:298-307` never fires on first play.
- The HUD reads position/duration straight off the not-yet-loaded engine and draws a
  live-looking transport at **0:00 / -0:00** (`player_hud.rs:257`).
- **The auto-hide deadline is computed from a pre-block timestamp.** `app.rs:1191` passes
  `last_input + HUD_LINGER_MS` (4500 ms), where `last_input` is stamped at the keypress, *before*
  the blocking chain. A load longer than 4.5 s ends in a completely blank screen — the HUD expires
  before it is ever shown.
- Key events queued during the stall are consumed immediately afterwards **against the new
  route** (`app.rs:1146`) — a BACK meant to cancel lands after the player is already up.

---

## 3. Blocking is not confined to the play path

### 3a. The per-frame pump makes blocking HTTP round trips

`player::pump` runs every frame from the main loop and calls, on the main thread:

- `route::transcode_seek` (`pump.rs:108` → `route.rs:184` `/decision`) — **every transcode seek**
- `route::switch_audio` / `retranscode` (`pump.rs:42` → `route.rs:581` PUT + `:584` `/decision`) —
  every audio-track switch and subtitle-burn refresh

So the HUD freezes *while it is on screen*, mid-interaction. Note `transcode_seek` **discards** the
`/decision` result (`let _ = c.transcode_decision(&sp)` at `route.rs:184`) — the URL build at
`:185` is pure string work. That round trip is pure cost.

### 3b. Teardown joins three workers on the main thread, unbounded

`engine::teardown` (`engine.rs:480-510`) joins the demux, load and timeline threads with no
timeout, then does **two more blocking PMS calls** (`report_timeline` + `stop_transcode`,
`engine.rs:508`). This runs on BACK, Stop, EOS, app-switch, and every reload (transcode seek,
audio switch). Three verified stalls:

1. **The shutdown interrupt is a no-op during the demuxer's open window.** `http_open` sets
   `hs.set_fd(-1)` at `stream.rs:177` and only publishes the real fd at `stream.rs:293`, *after*
   headers are parsed. For the whole connect+send+header-read window `http_shutdown`
   (`engine.rs:480`) does nothing, and the join that follows blocks for the full 2 s connect or
   15 s recv timeout. Worst case ~27 s of parked SDL loop.
2. **The timeline reporter only checks its stop flag between whole-second sleeps**
   (`threads.rs:30-35`). Every teardown pays a deterministic **0-1000 ms** join.
3. `http_open` also `write_bytes`-memsets the whole `HttpStream` including the atomic `fd`
   (`stream.rs:176`), racing another thread's load — the interrupt can target fd 0.

### 3c. Other main-thread network calls

- Post-playback hub refresh: large blocking GET + serde parse (`app.rs:1791`)
- Profile pick: installs the client and fetches hubs + sections synchronously (`app.rs:1942`)
- Detail page open: 2-5 blocking fetches from the key handler (`detail.rs:1190`)
- Library entry / login completion (`library.rs:179`)
- Track-menu commit: blocking PUT from the OK handler while the menu is open (`route.rs:623`)
- Draw path rasterizes glyphs/SVGs and uploads synchronously — the ticking HUD clock guarantees
  one cache miss per second (`text.rs:267`)

---

## 4. Correctness bugs the audit surfaced along the way

Not stalls — real races, hangs and leaks.

**Data races on `static mut` read across threads**
- The demux worker clones `static mut STREAM_ACODEC` while the main thread reassigns it
  (`ff.rs:1358`) — data race / use-after-free.
- The timeline reporter reads route's `static mut` playback state the main thread mutates
  (`threads.rs:51`).
- The main thread polls `SMP_isLoadCompleted()` on the same non-thread-safe C++ object the load
  thread is concurrently inside `SMP_Load()` on (`pump.rs:174`).
- `pump` resets `SHARED.frames` with a plain store while the library thread `fetch_add`s it
  (`pump.rs:168`).

**Hangs / dead ends with no error path**
- A failed stream open silently ends the demuxer; with `duration_ns` still 0 the EOS path can
  never fire → **black screen forever, no error, no exit** (`ff.rs:1293`, `engine.rs:624`).
- Every demux failure exits the thread silently — the pump can never learn the producer died.
- A 15 s recv timeout is mapped to `AVERROR_EOF` (`ff.rs:949`), so **one network hiccup
  permanently ends playback**.
- An unserviceable seek latches `TX.seek_to_ns`, and the feed gate keys off it — the pump stops
  feeding *both* lanes until `duration_ns` is published (`pump.rs:221`).
- `INPLACE_SEEK_OK` is a process-lifetime latch (`mod.rs:28`) — one transient failure permanently
  downgrades every future seek to the slow reload path.
- `posters_shutdown` joins two workers that may be in a 15 s read — app exit hangs
  (`posters.rs:370`).

**Idiom gaps in the code that *does* use the house pattern**
- Neither idiom can **cancel**: "supersede" invalidates the *result*, never the worker, which
  keeps a socket for up to 17 s (`metadata.rs:527`).
- A failed season fetch is indistinguishable from an empty one and is applied — the episode row
  silently blanks with no error and no retry (`metadata.rs:552`).
- `load_season` spawns an unjoined thread per invocation with no in-flight guard
  (`metadata.rs:548`); `auth.rs:402` likewise (double-OK runs two `/switch_user` flows).
- `browse::maybe_spawn` has no zero-progress guard — a short page respawns a thread **every
  frame** (`browse.rs:508`).
- `browse::pump` only runs while Library is mounted, so a late landing holds the single-flight
  flag indefinitely (`browse.rs:453`).
- `engine()` hands out a `&'static mut Engine` that teardown drops out from under its caller —
  aliasing avoided only by comment convention (`engine.rs:101`).

**Leaks**
- `retranscode` sets `TSESSION` to a synthetic marker, so `stop_transcode` later stops a session
  that does not exist (`route.rs:573`).
- A registered transcode session is orphaned when playback never starts (`route.rs:529`).

---

## 5. Recommended fix

Two designs were generated independently and **converge on the same core**: decompose
`build_stream` into a pure worker-side `resolve` producing an owned plan, plus a main-thread
`pump` that installs it — exactly the `metadata::load_season` shape. They differ only in scope.

**Recommendation: do the minimal design's Step 1 first, then take the state-machine design's
primitive.** Step 1 is a prerequisite either way and is independently valuable; the primitive is
what stops five modules from re-implementing fractions of the same idiom.

### Phase A — make blocking I/O interruptible (prerequisite, small, independently shippable)
1. Publish the socket fd immediately after `socket()` instead of after header parse
   (`stream.rs:176→293`), and replace the whole-struct memset with a tail-only zero so the atomic
   `fd` is never transiently 0. Makes `http_shutdown` effective for the whole open window.
2. Timeline reporter waits on a condvar instead of ten 1-second sleeps (`threads.rs:35`);
   teardown notifies before joining.
3. Teardown's two trailing round trips (`engine.rs:508`) become fire-and-forget.
4. Abort check at the demux outer-loop head before `http_open` (`ff.rs:1292`).

Result: BACK during a load returns in ~1 frame instead of 0.5-17 s.

### Phase B — the primitive
Add `rust-modules/src/task.rs`: a `Job<T>` with generation guard, worker-written `settled` flag,
single-flight, failure backoff, `Poll { Idle, Pending, Ready(T), Failed(Fail) }`, and a `Cancel`
token that carries the in-flight `HttpStream` so cancellation can `shutdown(2)` a blocked socket.
Plus `detach` for fire-and-forget. No runtime, no executor — a thread plus four atomics plus a
one-slot mailbox. **Host-testable** (`cargo test` runs on the host; 22 tests exist today).

The key contract: `Failed` is **not** `Ready(empty)`. That distinction is what fixes the blanking
episode row.

### Phase C — collapse the callers
Migrate in this order, each independently verifiable on-device:
1. `metadata.rs` (detail + season) — kills the detail-page freeze
2. `browse.rs` — mechanical, fixes the respawn-every-frame and stuck-flag bugs
3. **`route::build_stream` → `resolve` + `apply_plan`** — the reported bug. The worker touches no
   `static mut` and no `SHARED`; the main thread's `apply_plan` becomes the sole writer of
   `URL`/`TSESSION`/`SESS`/`PQ_*`/`CUR_*`/`STREAM_*`, which also removes the `STREAM_ACODEC` race.
4. The pump's transcode-seek and audio-switch arms (§3a)
5. The remaining main-thread calls (§3c)

### Phase D — the state machine and the error paths
Replace the `seeking` boolean with an explicit `PlaybackState { Idle, Resolving, Connecting,
Buffering, Playing, Seeking, Error }` in `player/shared.rs`, derived once per frame in
`player::pump`. Render it through a new reusable `StatusOverlay` component built from existing
tokens + `Spinner` (per `ui/CLAUDE.md`). Stamp the HUD linger deadline at the **route flip**, not
the keypress. Then `ff::demux`'s silent failure exits each publish an error, so a failed open
becomes an Error card with a retry instead of a permanent black screen.

---

## 6. Verification

- `tests/run.py` (all 18, especially the two rapid-seek-burst cases — they exercise Phase A
  hardest) and `tests/run.py --fps --fps-player`.
- Proposed new regression gate: a `/tmp/plxnative-slowpms=<ms>` dev trigger injecting an
  artificial per-request delay in `stream.rs`'s one-shot wrappers, so "the HUD stays live through
  a slow resolve" becomes an assertable case rather than a manual observation.
- On device: `/tmp/plxnative-framedrop` armed at `22`, drive `ok` then `back` through
  `/tmp/plxnative-remote`, and confirm no long frame; `/tmp/plxnative-capture` +
  `tools/stream-screen.py` to watch the HUD appear within one frame of the press.
