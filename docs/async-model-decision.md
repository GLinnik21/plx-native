# Async model — decision (2026-07-27)

Companion to `docs/async-model-review.md` (the 84-finding audit). This records **which
concurrency model the project adopts and why**, from a structured five-position debate:
opening positions → cross-rebuttals → three independent judges → moderator synthesis.

Positions argued: **A** extend the house idiom (no new abstraction) · **B** one reusable
`task::Job<T>` primitive · **C** async/await + a hand-written frame executor · **D** actors over
channels · **E** single-threaded readiness reactor.

**Scores** (three judges, C1-C7 out of 70): **B 52/55/53** · A 45/44/44 · E 37/40/39 ·
C 32/33/34 · D 32/34/33. All three judges picked B **as the vehicle only** — none endorsed it as
posted.

---

## The decision

Adopt **`task::Job<T>` as the concurrency vehicle, on position A's sequencing and transport
discipline**, with D/E's three-line race fix and the audit's Phase D folded into the first two
commits.

- No executor, no actor topology, no readiness reactor, **no new dependency**.
- Workers stay plain `std::thread`. **The SDL loop remains the only scheduler.**
- Every off-loop operation becomes a `Job<T>`: generation guard, monotone one-slot mailbox,
  single-flight, typed `Fail`, failure backoff, and a `Cancel { flag: AtomicBool, sock: Mutex<c_int> }`
  whose `shutdown(2)`/`close(2)` happen **under the lock**, hooked inside `stream::http_open` /
  `close_owned` so all three transports are covered by one hook.
- The two `player::pump` network arms (`pump.rs:42`, `pump.rs:108`) are **explicitly not
  migrated** — they land last as a hand-written two-state enum.

Seams: **A** owns commit 1 (transport hardening) and the `plan.rs` sibling-module structure ·
**D/E** own the `ff::demux(…, acodec: String)` by-value capture · **B** owns `task.rs` and the
caller migrations · **D** owns the accessor-demotion rule.

---

## What decided it (all verified against the repo, not taken from self-ratings)

**1. The board's most valuable finding is not an async-model answer.** A, B, C *and* the audit's
own Phase C3 all claimed that making `apply_plan` the sole writer removes the `ff.rs:1358`
`STREAM_ACODEC` race. **It does not.** Verified: every writer is *already* main-thread —
`route.rs:401/424/426` (build_stream), `route.rs:580` (`retranscode`, reached from `pump.rs:42`),
`player/mod.rs:92` (`request_audio_track`, from the track menu). Single-writer does not fix a
write/read race. Only D and E found the actual fix, and it is three lines and behaviourally free:
`ff::demux` already takes `host: String, path: String` by value (`ff.rs:1278`), and every codec
writer is followed by `teardown(true) + start_bufferfeed()`, which respawns the thread with the
new value. **Three positions claimed a C2 win they did not have; two lower-scored positions found
the only real one.**

**2. Only one cancellation design survives the transport.** `http_get`/`put`/`post` each
`Box::new(zeroed())` their `HttpStream` internally and never expose it. That makes C's and D's
`AtomicPtr<HttpStream>` a use-after-free against a Box the worker may have just dropped. A's
`Cancel(Mutex<usize>)` has no cancelled flag, so a BACK landing *between* two round trips of a 4-6
request chain is silently dropped. B publishes the **fd value** under a Mutex from inside
`http_open` and closes inside the same lock — no dereference, no drop-order coupling, no
recycled-fd window.

**3. B is nevertheless unsound as posted, and A supplies the fix.** Verified: `http_open` closes
the fd directly on **five** early-return paths — `stream.rs:200` (bad dotted-quad), `:205`
(connect failed), `:240` (send failed), `:254` (recv ≤ 0 mid-headers), `:272` (no header
terminator) — none through `close_owned`, and `hs.set_fd(fd)` only happens at `:293` after the
header parse. Publish the fd at `socket()` without fixing those five, and a dead descriptor stays
armed in the token while `posters.rs:334` opens new sockets on two continuously-live workers.
**This is the hard seam between the two leading positions, and it is why commit 1 is A's.**

**4. Two positions were refuted by compiling a counterexample.** D's entire C2 differentiator does
not exist: a `static mut` of a `!Sync` type with a free `addr_of!` accessor **is** readable from
`thread::spawn` — it builds, runs, prints. `static mut` carries no `Sync` bound. C's `!Send` `Task`
guarantee is voided by C's own `static mut PLAY` storage, and C's claimed exclusive advantage
(main-thread work *between* two network steps) is zero once you read both pump arms — each is
network → terminal ACB step → `return`. **Both authors conceded, unforced.**

*This is why step 4 uses the token-as-argument form, not a marker field on a state type.*

**5. C5 is empirical and decisive here.** `cargo test` passes **28 tests in 0.30s**, including
`shutdown_wakes_a_reader_that_is_already_blocked_in_recv`, `exactly_one_caller_can_claim_the_fd`
and two connect-deadline tests. The machinery to host-test cancellation-with-teeth **already
exists and already passes**. On a target with no debugger and a ~30s deploy cycle, one tested file
carrying the monotone-slot / single-flight / failure-vs-empty / backoff / cancel invariants beats
five hand-written copies. A scored 4 here and conceded it.

**Where the judges split.** All three picked B but disagreed on ordering: the shipper put Phase D
at commit 2, the correctness judge at commit 8. Moderator sided with the shipper, and the code
settles it — `player::loading()` is literally `SHARED.seeking.load()` (`mod.rs:79`), false during
the initial resolve, so the `Spinner` already sitting in `player_hud.rs` **has never fired on
first play**. Roughly half the reported bug is "there is nothing to draw," it depends on nothing
in `task.rs`, and it is the part the user sees this week.

**Why not A, despite the best fit score (9/9/9 on C7).** A's own arithmetic refuted it: A set its
threshold at ~8 call sites, recounted the real number as fourteen, and honoured its own
disqualifier. Its two flagship cheap wins also fail on inspection — deleting `let _ =
c.transcode_decision(&sp)` (`route.rs:184`) is a **PMS protocol change** (`transcoder.rs:8`: the
decision "registers a transcode/remux session before `start.mkv` will stream"), and `resolve` is
not a mechanical hoist because `metadata::load_playing` writes `static mut PLAYING` that
`pick_dp_audio` reads back one line later (`route.rs:377` → `:463`).

The decisive evidence is empirical: **the codebase has already re-derived this pattern five times
and gotten a different invariant wrong each time.** `metadata.rs`'s season worker applies a failed
fetch as `unwrap_or_default()` (blanks the episode row); `browse.rs`'s comment records "one wifi
hiccup blanked a populated grid permanently" as a review-confirmed bug; `browse::pump()` ends in
`maybe_spawn()` so it only schedules while Library is mounted; the monotone-slot comment at
`metadata.rs:552` exists because someone already lost the newest season to a late landing. That is
A's own disqualifier #3, satisfied twice before the sixth site is written.

---

## The plan — nine steps, each independently shippable

| # | Step | Files |
|---|---|---|
| 1 | **Phase A+ — transport hardening + the two real races** | stream.rs, ff.rs, player/{engine,threads,shared}.rs |
| 2 | **Phase D — a loading state and something to draw** | player/{shared,mod}.rs, ui/{widgets,player_hud}.rs, app.rs |
| 3 | ~~**`task.rs` — `Job<T>`, zero callers, host-tested**~~ **DECLINED** — see the log below; `task.rs` shipped as the spawn, not the mailbox | task.rs (new), lib.rs |
| 4 | **`MainThread` token on the ACB/Starfish seam** — LANDED, and it grew the `ENGINE` slot | player/{ffi,engine,pump,mod}.rs, task.rs, app.rs |
| 5 | **metadata onto `Job`** — season, then detail | metadata.rs, ui/detail.rs, app.rs |
| 6 | **Split `load_playing`** — the prerequisite everyone under-priced | metadata.rs, route.rs |
| 7 | **The reported bug — resolve off the key handler** | plan.rs (new), route.rs, app.rs, ui/detail.rs |
| 8 | **browse.rs onto `Job`** — partially, and say so | browse.rs, app.rs, ui/library.rs |
| 9 | **The pump's two network arms — LAST, hand-written** | player/pump.rs, route.rs |

**Step 1** publishes the fd at `socket()`, replaces the whole-struct memset with a tail-only zero
(the atomic `fd` must never be transiently 0), **routes all five bare closes through
`close_owned`**, condvars the timeline reporter, and lands the `ff::demux(acodec)` capture.
BACK during a load goes from 0.5-17s to ~1 frame.

**Step 7** puts `resolve` in `plan.rs` as a **sibling** of `route` (not a child), so Rust's privacy
rules make touching route's sixteen private statics a compile error **at zero machinery cost**.

**Step 9's escape hatch:** if either pump arm cannot be expressed as a two-state enum without
breaking the seek coalescing guard or `SEEK_STUCK_MS`, **leave it blocking**. It is a few hundred
ms behind a spinner the HUD already draws, and that code has self-DoS'd before.

---

## Dissent — the failure mode we are accepting

**`Job<T>` is a library, not a safety property, and the strongest fix on this board came from
outside it.**

After all nine steps `route.rs` still has sixteen `static mut`s and twenty-eight `pub(crate) fn`s,
six of them outright setters. `F: Send` **cannot see a `static mut` write** — a static is a path,
not a capture — and cannot see a `pub(crate)` setter call at all. Sibling-module privacy closes
one door; step 7's accessor demotion closes six more; nothing closes the rest. The guarantee we
are buying is: *the play worker, specifically, cannot reach the statics it used to write.* **The
next worker somebody adds can, and it will compile clean.**

D's diagnosis — these are state-location bugs, not scheduling bugs — is correct. We decline its
remedy on cost and because its specific mechanism was inert, **not because the diagnosis is wrong.**

The proof is in the evidence: the single most valuable correctness fix in this exercise is a
three-line by-value capture at `engine.rs:294`, which no proposal's architecture produced and
which the winning position both missed and then wrongly claimed to deliver. **Read B's C2 score as
near-zero. The decision buys C3 (real cancellation) and C5 (host-testable invariants), and
nothing else.**

Second: centralising the mailbox invariants does not centralise the judgement that produces these
bugs. B's own posted design ships three defects at exactly the call sites `Job` does not cover — a
swallowed `Builder::spawn` `io::Result` (an EAGAIN wedges `pending()` true forever behind a
spinner that can never resolve and never logs), `poll()` returning `Pending` when `pending()` is
false after a cancel, and "moving `browse::pump()` into app.rs is a one-line change" (wrong:
`pump()` ends in `maybe_spawn()` and would schedule page GETs against a stale window for an
unmounted screen). Each is one line to fix. Together they are the argument A was making.

Third, the honest accounting: **steps 1, 2 and 7 deliver almost all the user-visible
improvement.** Steps 3, 5, 8 are ~400 lines whose payoff is deleting duplication we already have
and preventing duplication we have not written yet. If the roadmap stalls after step 7 — and
roadmaps stall — the engineer has paid for a generic primitive serving two callers.

---

---

## Implementation log

### Step 1 — landed at `a940884`, headline change REVERTED (2026-07-27)

**The fd-publication reversal trigger fired.** Publishing the socket fd before `connect` — the
change that lets a teardown/BACK interrupt a stalled open — was implemented, host-tested (two new
regression tests, both verified to fail on the pre-fix code), and then **reverted after device
runs**.

Why: it makes *every* reopen interruptible, and the demuxer reopens that socket from **two**
places — its outer-loop head and the AVIO `seek_cb` — while the pump fires `http_shutdown` on the
same stream to service a seek. Cost `substance_seek_inplace`, confirmed by bisect (fails with the
change, passes without). A first patch (retry an interrupted reopen) made it worse — 16/18 — and
the logs showed the demuxer was not even dying; the seek was being rejected by the pump's guard
and never firing.

Note the coupling that forced an all-or-nothing revert: with the fd unpublished, `close_owned`'s
`take_fd()` returns -1 and **leaks** the descriptor, so the five-bare-close routing cannot land
without the publication.

**Consequence for step 3: `Cancel` must ship flag-only** — it can set the flag and have the worker
check it between round trips, and the generation guard still makes a late landing harmless, but it
**cannot wake a worker already blocked in `recv(2)`**. The decision's C3 (cancellation with teeth)
is not delivered. Re-read the scorecard as buying **C5 + the state machine**, and little else.

The likely fix, unimplemented: a `reading: AtomicBool` set once headers are parsed, with two
interrupt entry points — "wake a blocked read" (seek) vs "kill this stream" (teardown). Design and
host-test it before it goes near the device again.

### Step 1's headline change — RE-LANDED (2026-07-29)

Not via the two-entry-point design above: that was solving "distinguish a seek interrupt from a
teardown interrupt," and **there is no seek interrupt any more.** `5938b5f`/`71929ee` moved seeking
into the demux thread's own `av_seek_frame`, so the only `http_shutdown` left in the tree is
teardown's (`player/engine.rs`), where cutting an open short is exactly the intent. The reversal
trigger dissolved rather than being worked around.

So `http_open` now publishes the fd at `socket()` and all five early returns retire it through
`close_owned`. What that buys: `teardown` joins the demux thread, and that thread could be inside
`http_open` — where `http_shutdown` was a no-op — so a stop during a stalled open parked the **main
loop** for the full 15 s `SO_RCVTIMEO` (or 2 s of connect). Now it costs one syscall.

**The design was decided by a device probe, and my reasoning going in was wrong.** I argued the fd
should be published *after* `connect`, on the grounds that Linux fails `shutdown` on an unconnected
socket with ENOTCONN and so the handshake window is uninterruptible anyway. `tools/sockprobe.c`
(new) says otherwise on this kernel: `shutdown` returns 0 and the handshake dies at 200 ms instead
of running its 1200 ms deadline. The plan's original `socket()` publication was right. Note this
could not have been settled on the host — `cargo test` runs on Darwin, where the same call returns
0 **and makes `connect_timeout` report success on a socket that never connected.** Three platforms'
worth of divergence in one syscall.

Host coverage: `an_open_stalled_in_the_header_read_is_interruptible` — verified to fail on the
pre-fix code by taking exactly 15.00 s (the `SO_RCVTIMEO`), and to pass in 0.3 s after.

Device: 18/18 + 5/5 FPS, and the seek tier 12/12 over three runs — `substance_seek_inplace`, the
case that bisected the original revert, among them.

**Still open, and NOT closed by this:** `http_shutdown` reads the fd and then acts on it, so it can
race the owner's `close_owned` and shoot a number that has since been recycled by another thread's
`socket()`. That race pre-dates this change; publishing earlier widens its window from the
body-read phase to the whole open. The decision's own remedy is the right one — a `Mutex` under
which both `shutdown(2)` and `close(2)` happen — deliberately not folded in here, because bundling
a struct-layout change into the re-land of a previously-reverted change is how you earn a second
revert. Tracked separately.

### Step 3 — `Job<T>` DECLINED on re-evaluation (2026-07-28)

The board picked B as the vehicle. By the time step 3's turn came, steps 1, 2, 5, 6 and 7 had all
landed **by hand**, device-verified — so `Job<T>` was no longer "the way the callers get written,"
it was a rewrite of five working ones. Re-evaluated against the decision's own reversal triggers:

**"Signals A was right" — partly fired.** The trigger reads *"if the migration stalls after step 7
with `task.rs` serving two callers and five hand-rolled mailboxes still alive."* The migration did
not stall; it finished without the primitive. Same end state, arrived at from the other direction,
and the doc's own accounting already conceded it: *"steps 1, 2 and 7 deliver almost all the
user-visible improvement. Steps 3, 5, 8 are ~400 lines whose payoff is deleting duplication we
already have."*

**"if any caller needs a per-site flag to fit" — fired outright.** `browse.rs` gates its spawn on
`done` — a landed-empty list is an answer, not an absence — which is screen state, not in-flight
state, and `Job` cannot own it. `browse::kick_directory` is *already* a local generic over `T`
serving its two callers. Meanwhile the three in-flight representations across the five sites
(`PLAY_BUSY` bool, `GEN`/`DONE` counter pair, a single-flight `AtomicBool`) are not
interchangeable: each drives a different spinner with different semantics.

**C3 was the last argument, and it is worth less than it was.** The step-1 log says cancellation
retreated to flag-only, leaving "C5 + the state machine, and little else." Two things since:
`5938b5f`/`71929ee` deleted the pump's seek-time `http_shutdown` and the demuxer's outer-loop
reopen, so the coupling that forced step 1's all-or-nothing revert **is gone** — the only
`http_shutdown` left is teardown's (`engine.rs`), where interrupting a reopen is the intent. That
re-opens fd publication as a *separate* job (below). But it no longer argues for `Job`: every
socket phase is already deadline-bounded (2 s connect, 15 s recv, 10 s send), and `route.rs`'s own
comment records why flag-only costs nothing here — the freeze was fixed by getting the resolve off
the loop, and a lingering worker is invisible once the UI has moved on.

**What the re-evaluation did find, and what shipped instead.** The five copies disagree on one
invariant: `std::thread::spawn` **panics** when the OS refuses a thread, and all but two sites used
it, from the SDL loop — so an EAGAIN unwinds out of `plex_run` through the C shim and kills the
app. Worse, each site had
just armed an in-flight flag, so the survivable version is a spinner that can never resolve —
exactly the shape of `browse.rs`'s already-fixed `reset()` latch bug. **The piece worth sharing was
the spawn, not the mailbox.** `task.rs` exists, at ~50 lines instead of ~200: two entry points that
report a refusal instead of panicking, plus the 256 KB stack the network workers want. Every one of
the fourteen spawn sites in the crate is on it; each caller still releases its own latch, because a
latch belongs to the screen. Host-tested (`a_refused_spawn_reports_instead_of_panicking`, forced
with an unsatisfiable stack size).

**How likely is the refusal? MEASURED, not estimated** — `tools/threadprobe.c` (new), run on the
TV under the app's own uid with the app closed. The first write-up guessed from `/proc` arithmetic
and got the binding limit wrong:

| stack | refused at | `VmSize` there | binding limit |
|---|---|---|---|
| 2 MB (platform default) | **2043 threads** | 4188 MB | `RLIMIT_AS` = 4294967295, the full AArch32 4 GB space |
| 256 KB (`spawn_small`) | **3745 threads** | 963 MB | `RLIMIT_NPROC` = 3746, exactly |

Both are EAGAIN(11) — precisely what `std::thread::spawn` unwraps
(`library/std/src/thread/functions.rs`: `Builder::new().spawn(f).expect("failed to spawn thread")`,
verified in the local rust-src). Against those ceilings the app runs **31 threads at playback
peak**, 13 at Home, `VmSize` 363 MB: ~66x and ~11x headroom. RSS is a non-issue either way
(~12 kB/thread; 31 MB at 3746 threads).

So `task.rs` is not fixing something that happens; it deletes an unreachable-but-unrecoverable
branch for the price of a return value — still the right trade (a panic across the FFI boundary
has no recovery) but NOT the urgent defect the paragraph above would suggest on its own. Two
things worth keeping: **which limit binds depends on the stack size**, and the crossover sits
between 256 KB and 2 MB, so `spawn_small`'s choice is real and not cosmetic; and the one process
limit anywhere near reach is `Max open files` (1024 soft, 70 in use with 20 sockets at playback) —
a different subsystem, worth its own look before the fd-publication work below.

**Follow-on, not done here:** re-land step 1's fd publication now that its blocker is gone. The
payoff is no longer `Job`'s cancellation — it is that `teardown` currently joins the demux thread
while it may be inside `http_open`, so a stop during a stalled reopen blocks the **main loop** for
up to the 2 s connect or 15 s recv deadline. Same reversal trigger applies: bisect
`substance_seek_inplace` before and after.

### Step 4 — `MainThread` token, LANDED (2026-07-29)

Shipped in the form finding #4 argued for — **a `!Send` ZST passed as an argument**, minted once
at the top of `plex_run` — and *not* as a marker field on a state type, which that finding refuted
by compiling the counterexample (`static mut` carries no `Sync` bound, so a `!Send` field inside
one is still readable from `thread::spawn`).

**It covers more than the seam.** The step was scoped to the ACB/Starfish verbs, but the same
argument applies harder to `engine::ENGINE`: it is a `static mut` handed out as `&'static mut`,
with worker threads holding raw pointers into the boxes it owns, and two live `&mut` is instant
UB. Both are now gated, which is why `mod.rs`/`app.rs` are in the file list and `lib.rs` is not
(the token lives in `task.rs`, beside `spawn` — the module is now the thread seam in both
directions: where work leaves the main thread, and what cannot follow it).

What makes it a guarantee rather than a naming convention is **privacy**, the same lever step 7
used: `ffi.rs`'s `extern "C"` block moved into a private `mod sys`, so the token-taking wrappers
are the only way in, and `ENGINE`'s four raw `addr_of` touches became four token-taking accessors.
`start_bufferfeed` is the proof this mattered — it was the one function whose `mt` the compiler
flagged as unused, precisely because it reached the static directly instead of through `engine()`.

**Verified with teeth, both directions.** A temporary `task::spawn("evil", move || pause(mt))`
fails to compile (`*const () cannot be shared between threads safely`), and the host test
`the_main_thread_token_cannot_cross_a_spawn` asserts the `!Send`-ness the whole thing rests on —
an absent impl being invisible to ordinary code, it uses inherent-vs-trait const resolution to
detect it, with a `Send` control case so a probe that answered "never Send" would fail.

**Two deliberate holes, both documented where someone would hit them.** `sf_load` takes no token
because `load_thread` runs it off-main by design — the missing parameter *is* the documentation
that this one verb is not main-thread. And `MainThread::assume()` is callable, so `unsafe` inside
a worker still defeats it; that is the ceiling of the pattern. What it buys is that the mistake
must now be written, in an `unsafe` block, rather than made by forgetting a convention that was
documented in three other files and enforced by none.

The rule for new code, recorded in `engine.rs` and `player/CLAUDE.md`: **take the token iff you
reach the seam or the Engine.** `arm_seek`/`resume_at` are main-thread too and deliberately do not
take one — if it spreads to everything that merely runs on main, it stops carrying information.

### The audit round — all five findings closed (2026-07-29)

The nine-step plan finished, then an adversarial audit of the whole range (`4e7c4b0..HEAD`, 5 lenses
→ 18 raw → 9 verified → 5 confirmed) found what was left. **Every survivor was the same class: a
cancellation gap.** No data race, no UAF, nothing wrong in the primitives. Landed in order of value:

| Finding | Fix | Evidence |
|---|---|---|
| `ff::seek_cb` had no abort guard | mirror `read_cb`'s check | host test: **9 accepts for 8 hops** unguarded — one full `http_open` per hop, not the single wasted open static review predicted |
| a failed `/children` landed as an empty season | `Option` failure model, matching the detail sibling | host test fails on the old `pump_season` |
| teardown's 2 UNCONDITIONAL blocking POSTs | `route::scrobble_stop` onto a worker, drained at exit | `/status/sessions` empty 6 s after BACK — the stopped report lands from the worker |
| teardown joined the reporter's POST | per-session `ReportStop`; the join rides out with the scrobble worker | **6974 ms → 0.5 s**, measured with `tools/netcond.py` |
| `http_shutdown` vs `close_owned` on a recycled fd | one gate over both, fd re-read under it | structural; no host-reproducible test, and said so |

**The lesson worth keeping is about method, not any one bug.** Four of the five were fault-conditional
— invisible on a healthy LAN, where the measured teardown-join baseline is `demux 0ms media 0ms
timeline 0ms`. They were argued about for two sessions and settled in one, by building
`tools/netcond.py` and making the server misbehave on demand. Two things it corrected on contact:
the reporter freeze was real (7 s of parked frame loop, exactly `SO_RCVTIMEO` minus elapsed), and a
`POST /playQueues` failure that looked like a regression was the proxy's own artifact — confirmed by
re-running direct. **Condition the network before theorising about a network failure.**

The one fix that did not come from the audit's own proposal is the reporter's. The obvious remedy
was a cancellation path — thread a caller-owned socket through four layers. The real reason the join
existed was that the reporter's stop flag lived in `SHARED`, which `reset_session` clears at the end
of teardown, so a reporter still parked in its POST would resurrect. Per-session ownership removed
the need to join on the main thread at all, and deleted shared state instead of adding a mechanism.

### Pre-existing failure, not ours

`morning_show_seek_rapid` fails on **clean HEAD** with the identical message (`only 1
seek(in-place) fired, need >=2`). It was mis-attributed to step 1 at first. The suite is therefore
17/18 before and after; this case needs its own investigation and is unrelated to the async work.

---

## Reversal triggers

**Hard stops in step 1.** If the five bare closes cannot all route through `close_owned` without
changing its error semantics — **stop**. Every token-based cancellation on this board is then
unsound, and step 3's C3 claim must retreat to flag-only (discard the result, let the socket time
out), which costs most of the reason to centralise. If the `ff::demux(acodec)` capture changes any
observed codec in the Load payload on device (check the event log's `decision output: v= a=`
against the Load payload on a retranscode), the respawn assumption is wrong.

**Step 3.** If `cancel_interrupts_a_job_blocked_in_recv` cannot be made to pass through the real
`http_open`/`close_owned` path, or review finds any path closing an fd outside `close_owned`, the
publish/retire protocol has a hole — a wrong-socket `shutdown(2)` on a device with no debugger is
worse than the freeze we started with. **If the primitive plus corrections exceeds ~200 lines, or
any caller needs a per-site flag to fit, A's over-build argument is live again.**

**Step 5 is a fork, not a formality.** Measure `serde_json::from_slice`. If it exceeds ~8ms on
common payloads, the hub refresh needs a decode worker, and any future readiness-reactor idea is
definitively the wrong axis — record the number and close E's file. If recv dominates *and* a
keep-alive or `plex.direct` TLS transport ever lands, E's poll-set becomes live again and `Job`
can be re-implemented over it without touching a caller.

**Signals D was right.** A third cross-thread reader of route's state appearing anywhere, or any
new `unsafe impl Send`/`SendPtr` introduced to get a worker compiling. Either means the two races
deleted in step 1 were symptoms, not the disease — attempt D on a throwaway branch, count the
compile errors, drop it only if more than one or two need an escape hatch.

**Signals A was right.** If the migration stalls after step 7 with `task.rs` serving two callers
and five hand-rolled mailboxes still alive — finish steps 8-9 **or delete `task.rs` and inline
it. Do not leave both idioms standing.** Also: if any of B's three posted defects reaches device
before review catches it, that is evidence centralising the mailbox did not centralise the
judgement, and the remaining migrations should be done by hand.

**Runtime signals.** A `Builder::spawn` EAGAIN/ENOMEM on device (thread exhaustion in a 32-bit
address space) means thread-per-Job is wrong here and a bounded pool is required. Any sign that
the `sock_opened`/`sock_closing` hook changed behaviour on the demux or poster sockets means the
"no-op outside a Job" claim is false and the hook must move.
