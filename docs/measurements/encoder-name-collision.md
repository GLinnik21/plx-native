# The client stopped its own encoder, and it read as a server fault

**Found 2026-08-29**, while investigating a playback failure the maintainer hit by hand: a seek
during Auto, then `warmup_unreachable`, then a run of `curlio: status=404`, then
`hls: demux failed: HLS segment was not produced in time`. Every visible symptom points at the
Plex Media Server. None of them was the server.

## 1. The mechanism

A candidate encoder is named `<logical_session>-abr-<n>`. The two halves had **different
lifetimes**:

* `logical_session` is `sess()`, and it **survives a seek**. At the time of this incident,
  `route::transcode_seek` also reused the physical session id deliberately — stopping it before a
  replacement existed would have cut the stream the demux worker was still reading.
* `n` was a `u64` **local to the demux worker**, initialised at the `abr.map(...)` that builds the
  adaptive tuple. Every `Load` — and a seek is a `Load` — reset it to zero.

So a playback that committed one switch and was then scrubbed came back with
`ACTIVE_ENCODER = <sess>-abr-1` and a counter at zero, and its next transaction primed a candidate
named `<sess>-abr-1` — **the live session's own id**. `prime` checked only
`is_active_encoder(expected)`; nothing compared the candidate's name to the live one.

Both exits then kill the playback:

| exit | call | effect when the names are equal |
|---|---|---|
| rollback | `abandon(candidate)` | `transcode_stop` on the **live** encoder |
| commit | `replace_active_encoder(expected, candidate)` then `retire(previous)` | the compare succeeds trivially, `previous == candidate`, so `retire` stops the session the commit just **switched to** |

The demuxer then polls a stopped session. **404 is `NotReady` by design** — PMS answers it for a
segment it has not produced yet — so the retry loop waits out its whole budget
(`clamp(3*duration + 2s, 3..15s)`) and reports "not produced in time". The client cannot tell a
stopped session from a slow one, which is why the failure reads as the server's.

## 2. Why no test tier caught it

Three conditions have to coincide: a **committed** switch before a seek, a `transcode_seek` (the
`retranscode_as` fallback path swaps the id back to `sess()` and does not collide), and a
transaction after the seek. That is the degraded-link path, and almost never the gigabit dev path.

The pipeline tier is blind by **construction**: `abandon` and `retire` both early-return on
`fixture_base`, so the two calls that do the damage are exactly the two a fixture playback never
makes.

## 3. Graded, two builds, one scenario

Host simulator (`tools/abr-scenario.sh`), `movie_av1_no_dp_audio` — the AV1/no-direct-play-audio
shape, so Original is infeasible and Auto must run Fixed HLS, which is what guarantees there are
transactions to collide. Unshaped loopback link (`0:pass`); the collision needs no conditioning at
all, only a commit before the seek, and an unshaped link supplies that by climbing. Seek to 300 s at
~87 s, via the `delay=` token added for this (see §5).

| | before | after |
|---|---:|---:|
| `abr: committed` | 2 | **5** |
| reloads (the seek) | 1 | 1 |
| `not produced in time` | **1** | **0** |
| `demux failed` | **1** | **0** |
| last `pos=` | **314 s** | **472 s** |

The before run dies 14 seconds after the seek. The sequence is legible line by line:

```
abr: committed Up to 2000kbps …      <- the pre-seek commit; live encoder becomes <sess>-abr-1
abr: retired previous encoder ok=1
autoseek: step → 300s
reload_transcode: fresh Load at offset 300s   <- the seek; generation restarts at 0
abr: seed rung=720kbps prior=11403kbps … n=25
abr: committed Up to 2000kbps …      <- generation 1 again: the candidate IS the live session
abr: retired previous encoder ok=1   <- and this stopped it
hls: demux failed: HLS segment was not produced in time
```

The after run reaches the same point and simply carries on, committing again to 4000 kbps.
Nothing else differs: the same scenario, the same binary but for two files.

## 4. The fix

The counter is now a **process-global monotonic**, allocated inside `prime` rather than passed in,
so there is no value a worker could hand it that repeats one. That makes the collision
unrepresentable rather than merely unlikely — a guard comparing the candidate against the live name
would also work, but it would leave the namespace able to express the bug.

`route.rs`'s regression test drives the naming through the fixture path, which shares the line that
formats the name, and fails as `sess-42-abr-1` against `sess-42-abr-1` when the counter is made to
restart.

**Lifecycle follow-up, 2026-08-31.** The maintainer's PMS archive showed the same physical
`abr-N` registered twice, about two minutes apart, with stale Streaming Resource state surviving
between starts. Ghidra confirmed that the exact opaque `session` is the encoder-map key; a seek is
not an operation on that encoder, it is another Universal Transcoder decision/start. The seek path
now allocates a fresh process-global name too, publishes it only after the replacement decision is
accepted, and retires the previous exact key after publication. A loopback PMS regression grades
all three wire/state facts: fresh decision id, new active id, and `/stop` on the old id.

## 4b. On the television

Device-verified the same day, dev set (webOS 4.10.2, debug install), panel off and audio muted,
binary md5-matched against the local build. `./tests/run.py --server --filter auto_` — **7 of 7**,
including both new cases, over the harness's own conditioning proxy:

```
[PASS] auto_pin_and_back          [PASS] auto_link_squeeze
[PASS] auto_baseline              [PASS] auto_seek_after_switch          <- the collision
[PASS] auto_hls_pin_and_back      [PASS] auto_original_squeeze
                                  [PASS] auto_original_squeeze_released  <- the reported scenario
```

**Read that green with the next section in hand:** at the moment it was taken,
`auto_seek_after_switch` did not yet discriminate — it would have passed the broken build too. The
run above is honest about the fixed build's behaviour and says nothing about the case's power. It
was re-run after the assertions were strengthened.

One authoring error is worth recording, because it failed on the first pass and the failure looked
like the fix: `auto_seek_after_switch` first declared `mode: "inplace"`, copied from the direct-play
seek cases. This item transcodes, and a transcode seek is a **rebuild** — `route::transcode_seek`
into `reload_transcode` — so no `seek(in-place)` line can ever appear. The seek itself was healthy
throughout. The default arm (`op_seek_transcode`) is the one that grades this path, and
`test_harness.py` now pins the mode choice, since the mistake is invisible in review: every other
seek case in the suite is direct-play.

## 5. What this needed from the test suite, and what it exposed

Neither shipped ABR case could express the scenario, for a reason worth recording: the app fires
the first `plxnative-autoseek` step at a **fixed ~12 s** after the player route is entered, while an
ABR transaction needs tens of seconds of samples before it can commit. So every seek this suite
could ask for landed **before the controller had ever switched**.

`delay=<ms>` (a token distinct from `gap=`, which is the cadence *between* rapid steps) is what
makes the wait expressible without a throwaway first seek — a throwaway would appear in the very
log the case grades. `tests/manifest.json` gains `auto_seek_after_switch` on top of it.

### The regression case passed the broken build

Recorded here because it is the most transferable thing in this file, and because it was found by
being asked the obvious question — *why not write the test first and watch it fail?*

The case was written **after** the fix and was green on the television, which looked like enough.
Replayed against the saved log of the **broken** build — a run that died on `HLS segment was not
produced in time` fourteen seconds after the seek — it scored **five green assertions out of
five**. Two independent reasons, both of which generalise well past this bug:

* **The global `timeline_climb` counted the seek DISCONTINUITY as progress.** Position went 1 s to
  314 s, so the assertion reported "climb 313s, need >=30s" and passed — on a log whose last
  fourteen seconds are the whole of its post-seek life.
* **`no_playing_error` greps the Starfish error surface** (`smp_cb type=18` / `Playing error`). A
  death on the *acquisition* side never reaches it: `ff.rs` reports `hls: demux failed: …` and
  stops. On the host simulator there is no Starfish at all, so that assertion is structurally
  silent there — the same blindness the rest of this repo files under
  `[[silent-instrument-trap]]`.

What it took to make the case discriminate: **`no_demux_failure`** (the death line is its own
assertion, not a hoped-for side effect of another one) and **`min_climb_after_s`** on the seek
operation, which grades the position series from the target ONWARD — the only window in which a
post-seek death is visible at all. Replayed again: pre-fix **FAIL** on both, post-fix **PASS** on
all six.

The general form is now a rule at the head of `docs/agent-reference.md`'s testing section. The mechanical part
worth repeating: **keep the failing run's log**, because "would this test have caught it" is a
question a fixed build cannot answer, and replaying a case's real assertions over a saved
`plxnative-events.log` costs seconds.

The same pass added **`max_reloads`**, and it closes a hole the flap investigation left open:
`no_reload` cannot grade a mode-switching Auto case, which legitimately reloads twice — out of
Original and back. Zero is the wrong gate and absent is no gate, so the Original flap satisfied
every assertion in the suite while it was happening: a reload is brief, so climb, play rate and
no-error all hold through a flapping session. `auto_original_squeeze_released` is the maintainer's
own by-hand scenario — full link, drop, hold, release to full — with a budget of two Loads.

## 6. Still open

* **The pre-fix build has no device leg.** The A/B in §3 is the simulator; on the television only
  the fixed build was run, so the device evidence is "the regression case passes", not "it failed
  before and passes now". The before half is a deliberate omission — reproducing it needs a
  knowingly broken binary deployed to a set someone watches.
* **`transcode_seek` vs `retranscode_as` in the original incident.** The original excerpt could
  not distinguish them. The later PMS archive and binary audit close the lifecycle ambiguity for
  seeks in general (a repeated exact key is unsafe), but cannot retroactively prove which UI action
  produced that first excerpt.
* **The seek re-seed**, which is a different defect in the same sequence: `cur_ceiling` is frozen at
  plan time and re-entered after a seek on the stalest link evidence in the system, while the
  estimator carries the freshest. Not fixed here.
* **Aborted fetches contaminate the production estimator** — `production_ratio_pm` is fed
  unconditionally, so an abandoned fetch enters as "the server is comfortably ahead" at the moment
  it is failing to produce. `SegmentSample::abandoned()` fixed this inversion for delivery;
  production never got the equivalent.
* **The timeline reporter posts `state=playing` from a dead pipeline**, with a frozen position,
  every 10 s until BACK — it consults only `TX.paused` and never `PlaybackState`.
