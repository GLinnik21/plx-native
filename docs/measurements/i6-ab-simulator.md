# I6 A/B — the sample counters were pinning the ladder down

**Host, 2026-08-28, no television.** I6's grading requirement is *"closed-loop sim over the frozen
trace library, both parameter sets, a stall regression in any leg disqualifies"*. The "both
parameter sets" half had never run, and was read for weeks as blocked on device access. It was not:
it was blocked on there being no second policy path in the app.

**It does not need one.** `TransactionModel::measured()` (`ab0c6f7e`) landed BEFORE I6
(`f331dd4d`), so a checkout of `f331dd4d^` runs the same pipeline — and a checkout cannot grade a
strawman, which hand-reconstructing the deleted gates from a comment could. `tools/abr-sim-case.py`
takes `PLXNATIVE_SIM_BIN`, so both legs get **HEAD's manifest, HEAD's fixtures and HEAD's shaper**
and differ only in the app binary.

## Result

90 s wall per case, three cases, both legs, run concurrently.

| case | leg | stall max/total | lumpy | rate | commits |
|---|---|---|---|---|---|
| `pipe_abr_oscillating_link` | pre-I6 | 1 / 4 | 5 | 1018 | **0** |
| | head | 1 / 5 | 6 | 1012 | **2** |
| `pipe_abr_brief_dropout` | pre-I6 | 1 / 5 | 6 | 1018 | **0** |
| | head | 1 / 4 | 5 | 1012 | **2** |
| `pipe_abr_steady_modest_link` | pre-I6 | 1 / 3 | 4 | 1018 | **0** |
| | head | 1 / 5 | 6 | 1012 | **2** |

**I6 is not disqualified**: no leg's maximum stall is worse than its pre-I6 counterpart, and every
one of the six is 1 s — which at this instrument's ±1 s resolution means "nothing measurable".

**And the counters really were the binding constraint.** After 90 s on `oscillating_link`:

```
pre-I6   current=720kbps  n=7   buf=14000ms  stable=0 cool…      <- never left the seed rung
head     current=8000kbps n=13  buf=12140ms  dwell=0             <- climbed 720 -> 2000 -> 8000
```

`stable=` is pre-I6's own field, so the binary under test is unambiguously the old one and its
gates are live. On a link that carries 8000 it sat at 720 for the whole window.

## The matched-window re-run, which the first pass owed

The first pass gave both legs 90 s of wall clock and got **59** heartbeats from pre-I6 against
**87** from head, so its stall totals were counts over unequal windows and were recorded as not a
comparison. Re-run at 170 s per leg and truncated to the common beat count:

| case | beats | pre-I6 stall | head stall | pre-I6 lumpy | head lumpy | final rung |
|---|---|---|---|---|---|---|
| `pipe_abr_oscillating_link` | 58 | 1 / 4 | 1 / 2 | 5 | 3 | **720 -> 20000** |
| `pipe_abr_brief_dropout` | 58 | 1 / 2 | 1 / 3 | 3 | 4 | **720 -> 20000** |
| `pipe_abr_steady_modest_link` | 58 | 1 / 4 | 1 / 2 | 5 | 3 | **720 -> 20000** |

**I6 is not disqualified.** Maximum stall is 1 s — the instrument's own resolution, i.e. nothing
measurable — on every leg of both parameter sets. Totals and lumpiness favour head in two cases of
three and pre-I6 in the third, by one beat; at ±1 s per beat that is noise in both directions and
neither is a result.

**The rung is the result, and it is the same on all three.** Over an identical 58-beat window
pre-I6 never left the seed rung, while head reached the top of the ladder. The three sample
counters were not a safety margin that I6 spent — they were preventing the controller from using
a link it had already measured.

## What this still does not establish

* **It is not a device measurement.** Every heartbeat carries `sim=1`, nothing decodes, and this
  Mac's loopback is not a television's link to a PMS. The device A/B on `pipe_abr_oscillating_link`
  that I6's row asks for is still owed — but it is now owed as *confirmation*, against a host
  result that already exists, rather than as the only evidence.
* **Three cases, one link profile each.** The disturbance matrix in `abr/sim.rs` is broader and
  runs the shipped controller only; running it under both binaries would be the stronger form of
  this and is not what was done here.
* **Why the beat counts differed at equal wall clock** is still unexplained — pre-I6 produced 59
  beats where head produced 87. A controller parked at the seed rung plausibly fetches differently
  from one that climbed, but this run does not separate that from a slower start, and the
  truncation sidesteps rather than answers it.
