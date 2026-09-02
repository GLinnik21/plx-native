# J3b — bounding a candidate transfer by the reserve it spends

*Device capture: `docs/measurements/j3b-logs/`. Binary: dev features, `FLAVOR=debug`, on the
LG 49SM9000PLA (webOS 4.5). Synthetic pipeline tier — no PMS, no token, no library.*

**What this measures.** Whether the deadline that landed in J3b changes anything, and what it
costs. It is deliberately *not* a re-derivation of `H_ref`: that needs the deadline to be in the
binary while `E_tx_down` is measured, which this capture is the first half of.

---

## 1. The baseline was wrong, and correcting it strengthens the case

The commit that opened J3b quoted the `Down`/commit distribution as n=65, p50 916, p95 2 198,
max 36 164 — a 16x jump from p95. **Those figures pool two different quantities.** Before the
transaction leg split, `tx.finish("committed")` sat below the feed loop, so `decided` also carried
the post-commit backpressure. Seventeen of the sixty-five records are of that older kind, and this
is the same error the review board caught in `docs/measurements/i2-transaction-cost.md` ("true
upshift cost is 3 065 ms median, not 9 563"). ` prime=` is the marker for the split.

Restricted to records where `decided` means the decision cost:

| | n | min | p50 | p90 | p95 | max |
|---|---:|---:|---:|---:|---:|---:|
| `Down`/commit, pooled (**wrong**) | 65 | 26 | 916 | 1 801 | 2 198 | 36 164 |
| `Down`/commit, post-leg-split | **48** | 26 | **749** | 1 441 | **1 491** | **36 164** |

The six largest values on the clean set are **1 441, 1 451, 1 491, 1 502, 2 241, 36 164**. So the
gap is **24x from p95 and 16x from the second-largest value in the entire corpus**, and the
`H_ref` observation of 1 424 ms sits at the **85th** percentile rather than the 74th. The outlier
is post-leg-split and its `warmup=36156ms` says the cost was the media fetch itself, not the feed.

`tools/test_abr_calibrate_plant.py` now restricts on ` prime=`, and `tests/run.py` reports any
`abr: tx` line it could not parse instead of dropping it — silence there reads as "there were no
transactions", which is how this was missed in the first place.

## 2. The property, and why it replaced the distribution test

A landed deadline does not retire the counter-example: the corpus is append-only, so the 36 s
record stays in it forever and a test keyed on the spread can never flip. The gradeable statement
is a property each capture satisfies or fails on its own:

> **No candidate transaction may spend more than the reserve it started with.**

`warmup + graded` against `buf_start`, both already on the wire. It is the permissive form — the
control plane sits on top of those legs — which is what makes it the strongest claim a captured
log can support rather than a restatement of the code.

Across the **115** transactions in the corpus carrying a leg breakdown, **exactly one violates it**:
the 36 156 ms warm-up against a 5 793 ms reserve, **6.2x** what it was spending. It is grandfathered
by name in `NoTransactionOutspendsItsReserve`; every capture since is graded automatically.

## 3. What the deadline does on a healthy link: nothing, which is the point

**15 of 15 ABR cases pass, and the deadline never fired.** Across the 22 transactions in this
capture the worst `spent/reserve` is **0.74x** (`pipe_abr_down_collapse`, 1 628 ms of a 2 209 ms
reserve); the `Down` median is 0.15x. Every outcome is `committed`. That is the correct signature
for a bound meant to bite only in the tail — but it is **not evidence that the bound works**, and
this section exists to say so rather than to let a green run read as a validation.

| capture | binary | `Down` n | worst `spent/reserve` | outcomes |
|---|---|---:|---:|---|
| `p2-logs` | before | 8 | **0.64x** | all committed |
| `j3a-window-logs` | before | 4 | **6.24x** | all committed |
| `j3b-logs` | **with the deadline** | 16 | **0.74x** | all committed |

## 4. The pathology is INTERMITTENT — one run in three of the same case

`pipe_abr_down_collapse` has been run three times. It produced the 36-second transaction **once**,
and the two runs that did not include one taken *before* the deadline existed. So the case cannot
grade a transfer deadline: two runs in three never enter the state it bounds.

Reading the three timelines side by side says why, and the mechanism is exact. The profile has a
single cliff, 40 000 -> 500 kbps at t=25 s. **Which rate the sample straddling that cliff reports
decides everything:**

| capture | sample at the cliff | downshift chosen | warm-up |
|---|---|---|---:|
| `p2-logs` | `cur=14000 net=500` | 14000 -> **320** | 1 418 ms |
| `j3b-logs` | `cur=8000 net=506` | 8000 -> **320** | 1 628 ms |
| `j3a-window-logs` | `cur=14000 net=`**`9593`** | 14000 -> **8000** | **36 156 ms** |

A rung-320 segment is ~94 KB and fetches in ~1.4 s at the 500 kbps floor. A rung-8000 segment is
~2 MB and needs **32 s** at that floor. So the failure is not "a downshift is slow"; it is
**a downshift target chosen from a rate the link no longer has** — and the intermediate reading
that produces it is a coincidence of where the fetch boundary falls relative to the cliff. j3b did
not even reach rung 14000 before the cliff, which is a third distinct trajectory of the same case.

## 5. `pipe_abr_down_staircase` — making it deterministic

The fix for a test that enters its own state one time in three is not to run it more often. The
middle leg is added instead: **40 000 -> 9 600 -> 500**, with the 9 600 leg long enough to be
measured by a whole sample. That forces the intermediate reading rather than waiting for it, so
the downshift target is a rung the floor cannot carry and the drop lands inside its warm-up fetch.

Nothing about the case is invented: **9 600 is the rate the pathological run actually measured**,
and the two-step shape is the reconstruction of what happened, not a stress test designed backwards
from a threshold.

The case grades what its sibling grades — the descent completes, no `Playing error` — because the
deadline is deliberately **not** asserted. `warmup_dl=` on the `abr: tx` line and a
`warmup_deadline` outcome are what show it; asserting the outcome would pin a mechanism where the
behaviour is what matters, and the plan's own rule is that a replacement test must be differential
or structural rather than an echo of the implementation.

## 6. The bound is 18x tighter at the top of the ladder than at the bottom

`B_max ∝ 1/R`, so "the deadline is the reserve" is not one number. From this capture's census:

| rung | settled reserve = the deadline |
|---:|---:|
| 320 | 88 293 ms |
| 720 | 67 543 ms |
| 2 000 | 37 210 ms |
| 4 000 | 18 418 ms |
| 10 000 | 8 168 ms |
| 16 000 | 5 793 ms |
| 20 000 | 4 960 ms |

Worth stating because it cuts the right way and is easy to miss: the bound is **tightest exactly
where transactions are most expensive** (a 20 000 kbps segment is 5 MB) and loosest where they are
cheapest. At rung 320 an 88-second deadline is barely a bound at all — and honestly so, because at
376 kbps of media the reserve really can absorb 88 seconds of fetching.

## 7. What this capture also settled, and what it did not

* **The pins land.** All seven M4 census rungs now hold their pinned rung for the whole run
  (`pin_320`: 89 samples at 320). The whole plant table comes from **one** capture for the first
  time, where it previously drew from `p2-logs` and `p1b-logs` together.
* **The calibration tool's "newest capture wins" was reverse-alphabetical**, which silently put
  every `p*` directory ahead of every `j*` one — the stale-table failure that tool exists to
  prevent, recurring in the mechanism meant to prevent it.

  **[CORRECTED] The first fix was also wrong, and its justification was false.** It replaced the
  sort with a HAND-WRITTEN chronology, on the stated grounds that "git cannot rescue it either:
  this branch's captures all carry the same commit date." They do not. That came from reading
  `git log --format=%cs`, which is the DAY; `--diff-filter=A --format=%ct` separates them to the
  second. And the hand-written list was wrong in two places: `j3-decides-logs` is NEWER than
  `j3a-window-logs` (13:15 against 12:03 — the window became a decider after the shadow capture),
  and `p2-logs` is newer than `p2h-logs`. The order is derived from the commit that ADDED each
  capture now, which is the right question anyway, and an uncommitted capture sorts newest because
  that is what a capture being taken right now is.

  The wrong order changed no shipped number — `j3b-logs` won every rung either way — which is
  exactly why it would have survived.
* **R11 is unobserved on device.** Zero `buf=none` samples: no case seeks during Auto.
  `pipe_abr_seek_flat` is added for that, and until it runs R11 is host-proven only.
* **`Down`/reject has still never been observed** — n = 0 across every capture. `sim.rs` continues
  to refuse the leg rather than invent it.

---

# J3d — the deadline FIRES, and reveals a larger hole beside it

*`docs/measurements/j3d-logs/pipe_abr_down_outrun.log`. Same binary as J3b plus the
request-indexed shaper.*

## 8. The first observation of the deadline acting

```
Down 18000->2000  outcome=warmup_deadline  decided=2226ms  warmup_dl=2209ms  buf_start=2209ms
Down 18000->320   outcome=committed        decided=1792ms  warmup=1787ms
```

The link fell to 5 568 kbps under segment 9; the controller, at rung 18000, targeted rung 2000.
Rung 2000 carries 2 221 kbps of media, so two seconds of it is 4 442 kbit — **9.1 s at the 490 kbps
floor, against a 2 209 ms reserve**. The deadline aborted at 2 226 ms, and the controller then
**re-decided on evidence that now included the 490 kbps reading and chose rung 320**, which
committed in 1 787 ms.

That is the whole value proposition, observed: an unaffordable commitment to a rung chosen from a
rate the link no longer had, converted into a bounded abort and a correct second choice. Without
it the fetch runs to completion and commits to rung 2000 on a 490 kbps link — unsustainable, and
requiring another downshift from an empty reserve, which is the 36-second record's shape.

**The request-indexed shaper is what made it reachable, and it justified itself in the same run.**
The indices were derived from `j3c`, whose trajectory this run did not share at all: with no
wall-clock profile the link is ~100 Mbps, the seed is rung 20000 rather than 10000, and segments
arrive far faster. Segments 10 and 11 still measured 5 568 and 490 kbps. Index-keyed shaping is
invariant to the trajectory; a wall clock is not, which is the entire reason the previous two
attempts produced a clean descent instead of the event.

## 9. **Nothing bounds the CURRENT stream's fetch, and that is where the stall was**

The case stalled **76 seconds**. It is not the transaction, and it is not a regression:

```
hls: segment=10 bytes=4648488 ... open_probe_ms=75943
```

That is the segment of the rung already playing. 4 648 488 B is 37 188 kbit; at 490 kbps it is
**75.9 s**, against a logged 75 943 ms — agreement to 40 ms. Every leg checks out the same way:

| fetch | kbit | link | predicted | logged |
|---|---:|---:|---:|---:|
| current seg 9 (6000 leg) | 39 346 | 5 568 | 7.1 s | 7 127 ms |
| **current seg 10 (500 leg)** | **37 188** | **490** | **75.9 s** | **75 943 ms** |
| rung 320 segment | 767 | 490 | 1.6 s | 1 782 ms |

So J3b bounds a **candidate** transfer at 2 209 ms while the **current** transfer beside it runs
unbounded for 76 seconds. Stated plainly: the deadline closed the smaller of the two holes. On a
collapsing link the dominant cost is not the transaction — it is the app's refusal to abandon a
segment it has already started, on a rung the link can no longer carry.

The historical proposed remedy was the plan's R16 prefix-rate abort rule, and this was the first
measurement that priced it: rung 320 was 1.6 s away. Later real-PMS evidence invalidated the last
sentence's premise. PMS can snapshot `Content-Length` for a still-growing segment and wait for its
encoder inside that body, so the delivered prefix rate does not identify the unseen remainder even
when both values appear known. Current code turns only observed `B = 0` into a decision point.

`pipe_abr_down_outrun` remains differential for the behavior the replacement rule actually
claims: after an observed terminal reserve boundary, the HLS pipeline stays alive and settles on a
cheaper actuator without treating the abandoned prefix as capacity. It deliberately carries no
`max_stall_s`; `B = 0` is an observed rebuffer boundary, not a promise that a severe collapse is
invisible, and the removed prefix forecast cannot justify such a bound.

---

# J3e / J3f — a refused seek latched the read-out for the rest of the film

*`docs/measurements/j3e-logs/` (before) and `j3f-logs/` (after). Found by a case written for
something else entirely, which is the part worth keeping.*

## 10. What the case was for, and why it cannot do that here

`pipe_abr_seek_flat` was added to close the one gap §7 named: R11's `None` branch is host-proven
and device-**unobserved**, because no case seeks during Auto. It cannot close it on this tier, and
the reason is structural rather than incidental. A transcode seek restarts the encode at a new
`&offset`; `route::transcode_seek` builds that from a PMS ratingKey and client; a
`plxnative-playurl` playback has neither. So the seek is refused before a single segment is
fetched, and the reserve the seek would have disturbed is never touched.

`unreadable reserve: 0/38`. **R11 remains host-proven and device-unobserved**, and the
characterisation line is what says so rather than an argument.

## 11. What it found instead

```
207: loop=47 route=player pos=5s vtick=5 vgap=201ms fps=48
213: autoseek: step → 40s (0 left)
214: seek(transcode): rebuild failed
     … 84 further seconds, 37 segment acquisitions, four rung commits, fps=60,
       and no `pos=` on any heartbeat again
```

`player::request_seek` arms `SHARED.seeking`. `pump::set_state` publishes
`PlaybackState::Seeking` from that flag **ahead of every other arm** — deliberately, because the
frames on the panel during a seek are the pre-seek ones. And exactly one place ever cleared it: the
successful prime→Play in `engine::feed_stream`.

So every path that gives UP on a seek leaked it, and a leaked flag means a spinner over the
picture, the playhead frozen at the target, and `is_playing()` false **for the rest of the
playback** — while the pipeline goes on fetching, deciding and presenting underneath. The stream
was fine. Only the app's account of it was stuck.

Two such paths existed: the failed transcode rebuild above, and `reload_transcode`'s
`no url (ignored)` early return. Both now call `player::abandon_seek`, which disarms the flag and
returns `seek_display_ns` to `-1` so a stale target cannot keep the playhead pinned after the
spinner clears.

## 12. The differential, on the device

Same case, same tier, same shaping; only the binary changed.

| | before (`j3e`) | after (`j3f`) |
|---|---|---|
| position series | 0 s .. **5 s** over **6** samples | 0 s .. **83 s** over **83** samples |
| after the refusal | — | position advanced **77 s** over 77 samples |
| `seek_refused` | — (`seek_inplace` failed) | **PASS** |
| segments / commits | 37 / 4 | 38 / 5 |

The bottom row is the control: the pipeline was healthy in BOTH runs, which is what makes the top
row a read-out defect rather than a playback one.

**The assertion is differential by construction.** `seek_refused` requires the position series to
advance after the refusal line — precisely what a latched spinner prevents — so it cannot be
satisfied by the code that produced the bug. The structural half is
`EverySeekGiveUpPathDisarmsTheSpinner`, which checks the two known give-up paths and **says out
loud that it cannot check a future one**: the real guard is to derive the state rather than latch a
flag (`Seeking` iff a target is pending or the engine is priming after one). That is not done here
because `prime_play` is also set outside a seek, so deriving would change the startup read-out too,
and stacking that onto a bug fix is what the plan forbids.
