# ACB release fence: the evidence, 2026-11-04

This note is the evidence behind two rules in `rust-modules/platform/src/tv/plane.rs` (the plane
owner) and `src/starfish.h` (`acb_unload`): the ACB plane is released when the completion callback
of the `setState(UNLOADED)` call itself arrives, matched on that call's task id; and a `setState`
answer of `-2` means nothing was sent and no callback will come. It cites the vendor binaries by
name and address and quotes none of their code.

Scope: one development set, firmware W19H, webOS 4.10.2 (the dev television that
`uri-acb-dass-2026-11-02.md` calls the "webOS 4.5 dev set"). Nothing here was measured on webOS 5 or later, where `libAcbAPI` is
absent. Addresses are the ones recorded by the decompilation session that produced them and were not
re-derived while writing this note.

## Binaries

| Binary | SHA-256 prefix |
|---|---|
| `libAcbAPI` | `d9627a2f` |
| `libAcb` | `d5c79be1` |
| `AcbService` | `46ea3225` |

## What the code does (client side: `libAcbAPI` / `libAcb`)

1. **`AcbCore::setState` @0x1e9dc answers `-2` without sending when its registered latch
   (`this[9]`) is 0.** The caller's task id is not written on that path, which is why the seam
   (`acb_unload` in `src/video_plane.c`) reads the id from its own zeroed slot (not the shared one) and reports it only for an
   accepted call, and why the plane owner treats `-2` as "released, no callback to wait for".
2. **Otherwise it calls `createTask(type 3, &taskId)` and returns 1.** `createTask` writes a
   client-local id, taken from the counter at `+0x15c`, synchronously, before the `/setState`
   request is posted. The id the call returned is therefore the id the completion will carry.
3. **The completion arrives through `AcbCore::onLsReply` @0xd760 → `fireEvent` @0xd468**, which
   invokes the registered callback with `(acbId, taskId, eventType, appState, playState, reply)`.
   `eventType` is `-2` timeout, `-1` error, `0` cancel, `1` complete. The `playState` argument is
   the cached value at `+0x9c`, not a report of what the pipeline did, so the plane owner never
   reads it.
4. **`AcbCore::setMediaId` @0x13bf8 unregisters a previous binding and then registers.** It has no
   "already bound" guard: a second `setMediaId` on a handle that was not unloaded silently replaces
   the first binding.

## What the code does (service side: `AcbService`)

5. **The service answers `/setState` only from `StateTask::decreaseCountToWaitReply` @0x540e0**,
   when the count of renderer replies it waits for is 0 and the target state has been reached. A
   completion with event type 1 for the UNLOADED task is therefore sent after the service has
   finished the state change, not when it received the request.
6. **The service's register handler @0x972b4 replaces an object that has the same pipelineId.**
   A bind over an unreleased binding is not refused by the service either.

## Device log (the dev set)

`acb unloaded rv=1 task=5` was followed by `acb callback task=5 ev=1 app=1 play=0` in 5 of 5
unloads. An earlier probe run printed `need-relaunch` one line before that callback: that line was
the old design's verdict, made at the dispatch of the unload, and the callback it had not waited for
arrived immediately after. The other state and display calls complete with event 1 too: in
`uri-acb-dass-2026-11-02/control.txt` the LOADED, video-data, window and PLAYING calls are tasks 1 to
4 and each has an `acb callback task=N ev=1` line, which is why the fence compares the task id and
never the event alone.

## What the code does with it

- `PlaneOwner::unload` dispatches `setState(UNLOADED)` and records the task id from an accepted
  call (answer 1). The plane is released when `Completion { task, ev: 1 }` for that id is drained
  from the callback mailbox. A completion for another task is not the fence; one that arrives
  before the dispatch is recorded is held (`MAX_EARLY_COMPLETIONS`) and matched when it is.
- The window and PLAYING calls that end a URI bind complete the same way. `acb_finish` returns each
  call's task id (only for a call that answered 1) and `PlaneOwner::show` / `shown` track both:
  `Shown::Done` once each id has an event-1 completion, `Shown::Failed` on any other event. A
  completion that beat the dispatch is matched from the early queue; the early queue is dropped at a
  granted bind and `Shown` is reset by a bind and by an unload, so a previous binding's task ids never
  count. This is the plane half of the URI `Observation::Presented` rule (`docs/uri-first-migration.md`).
- An answer of `-2` releases at once (point 1); any other refusal quarantines.
- A completion that is not event 1, or none within `RELEASE_FENCE_MS` (2000 ms; the device logs
  show the callback within about 100 ms), quarantines the plane. A late completion does not undo a
  verdict already given.
- `BindPolicy::Strict` (the URI backend and probe) refuses to bind over a bound or quarantined
  plane and waits for a release in flight; `BindPolicy::Rebind` (Feed) waits the same way but, after
  a failed fence, binds anyway and says so, because Feed has always re-bound on one handle without
  waiting and a firmware that never calls back must still get a picture. Points 4 and 6 are why
  `Strict` exists: nothing below the app stops a second binding.

## Not determined

- Whether `/unregister` inside `setMediaId` is ordered before `/register` on every transport. The
  client posts both; this note found nothing that orders them on the wire.
- The 18O and 18A unload chains were not re-decompiled for this note.
- Anything about firmware other than W19H / webOS 4.10.2, and any set whose `AcbService` build
  differs from the hash above.
