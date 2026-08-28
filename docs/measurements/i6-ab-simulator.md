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

## What this does NOT establish, and it matters

**The observation windows are not matched.** Both legs got 90 s of wall clock; pre-I6 produced
**59** heartbeats and 9 segments, head **87** and 52. So the stall totals are counts over unequal
windows and are not directly comparable — 1/4 against 1/5 is not a measurement of anything.

What survives that unevenness is the **rung** result, because a 720-versus-8000 difference is not
something a 28-beat window difference produces. The disqualifier is also safe: it asks whether head
stalls WORSE, and head's maximum is 1 s over the LONGER window.

Why the windows differ is not established either. The obvious candidate is that a controller parked
at the seed rung fetches differently from one that climbed, but that is a hypothesis and this run
does not separate it from a slower start.

**A matched-window re-run is owed** — same beat count, not same wall clock — before this is quoted
as a stall comparison. Quoting the rung result needs nothing further.

Neither leg is a device measurement: every heartbeat here carries `sim=1`, and nothing decodes.
