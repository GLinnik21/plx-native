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
  every `p*` directory ahead of every `j*` one. It is a stated chronology now, and an unlisted
  capture is an error rather than a silent placement.
* **R11 is unobserved on device.** Zero `buf=none` samples: no case seeks during Auto.
  `pipe_abr_seek_flat` is added for that, and until it runs R11 is host-proven only.
* **`Down`/reject has still never been observed** — n = 0 across every capture. `sim.rs` continues
  to refuse the leg rather than invent it.
