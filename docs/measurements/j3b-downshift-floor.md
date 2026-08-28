# J3b — the exhausted reserve was an ABSORBING state

**Measured 2026-08-28 on the dev television**, `./tests/run.py --filter pipe_abr_down_outrun
--no-early`, panel off, lock held. Both logs are beside this file, scrubbed:
`j3b-downshift-floor-logs/before-absorbing.log` and `after-floor.log`.

## What was being looked for, and what was actually there

The session was verifying the **abort rule** (the plan's R16 + R12) — `ff::StallGuard`, which
abandons a segment fetch its own projection says cannot land inside the reserve. The rule fired,
and the case still failed: 74 s of stall, the rung pinned at 18000 kbps, the film running at
`play=617` (two thirds of real time), **321 aborts on one segment**.

The first hypothesis was that the abort's own measurement was poisoning the estimator — an
abandoned fetch is measured over its own prefix, and the log line really does read
`bytes=1448 ... at 42277kbps`, which is one TCP segment out of the receive buffer rather than a
link rate. That hypothesis is **wrong**, and a host fixture refuted it in one run before any device
time was spent on it: fed that exact prefix, `Controller::observe` returns
`Prime(P1080M10, Down)`. The estimator was never fooled.

**The log says what actually happened, and it is not in the controller at all.** Every one of the
321 aborts produced a correct decision, and every resulting transaction died the same way:

```
abr: stall abort seq=4 bytes=133968 of 6084ms reserve at 6086kbps
abr: sample current=18000kbps ... buf=6084ms decision=prime_down target=2000kbps reason=Some(Hls(StarvationHorizon))
abr: tx Down 18000->2000kbps  outcome=warmup_deadline  warmup_dl=5876ms  buf_start=6084ms  buf_end=168ms

abr: stall abort seq=4 bytes=1448 of 168ms reserve at 25292kbps
abr: sample current=18000kbps ... buf=168ms  decision=prime_down target=12000kbps reason=Some(Hls(BufferConstraint))
abr: tx Down 18000->12000kbps outcome=warmup_deadline  warmup_dl=168ms   buf_start=168ms   buf_end=168ms
```

`candidate_warmup_budget` bounds a candidate transfer by **the reserve it is paid out of**. The
first downshift spent the whole 6 084 ms reserve on its warm-up and missed the deadline by 31 ms.
From then on every downshift was issued with `warmup_dl=168ms` — a deadline no transfer can meet —
so it was refused, so the reserve was never refilled, so the next one got 168 ms too.

**The exhausted reserve is absorbing.** The controller decided correctly 321 consecutive times and
could not act on any of them.

## Why the bound was wrong, in one sentence

The reserve bound expresses *abandon a transaction that can no longer do what it exists to do.*
For an **upshift** that is exactly right — an upshift buys more quality on a picture that is still
playing, so when the reserve is gone the benefit is gone with it. For a **downshift** the same
sentence is false: a downshift's benefit is the picture RESTARTING, which is available precisely
when the reserve is exhausted. The function's own doc had written the correct premise ("a
transaction starting with no reserve has already stalled") and drawn a conclusion that holds for
one direction only.

## The correction

A downshift's deadline is floored at the transfer's own physical requirement:

```text
bits for one segment at rung R over D ms of media  =  R * D          (kbps * ms = bits)
time to move them over a link measured at C        =  R * D / C      (bits / kbps = ms)
```

`R` is the catalog's **observed** output for the target (`expected_wire_kbps`, not the request
ceiling) and `C` is the delivery estimate's conservative reading, which is built only from
completed segments. No margin, no multiplier: refusing a transfer less time than it physically
needs is not bounding it, it is refusing it. Zero capacity yields zero, which is the identity
element of the `max`, so an unmeasured link restores the previous behaviour exactly rather than
inheriting an invented one.

**It does not loosen the 36-second runaway the reserve bound was written for.** That record was a
14000 → 8000 downshift on a link measured at 9 593 kbps, and `8000 * 2000 / 9593` = **1 667 ms** —
tighter than the reserve that transaction ran against. The floor binds only where the reserve has
collapsed below what any transfer needs, which is the absorbing state and nothing else. Both
properties are host tests (`a_downshift_gets_at_least_the_time_its_transfer_physically_needs`,
`the_floor_is_below_the_reserve_on_the_runaway_it_was_written_for`), and the first is differential
in both directions: with the floor absent the same call returns the 168 ms the device measured, and
the upshift leg pins that the floor is scoped to `Down` so it cannot be satisfied by raising the
bound generally.

## Two more defects behind it, and the third is the root cause

The floor made the case pass and **the full ABR tier then failed it again**, which is how the rest
of the chain came out. Recording all three because the order matters: each was invisible until the
one before it was fixed, and the last one is the only one that was causing the failure.

**Defect 2 — the floor was a central estimate, so it was a coin flip.** `R * D / C` is a *median*
prediction and is exceeded about half the time. `Down 18000->16000` came back
`warmup_dl=1314ms decided=1327ms` — missing by 13 ms, 53 consecutive times. Invisible in the first
device leg because its targets were far below the current rung (8000, 720, 320 out of 20000), where
the prediction is generous by a wide margin; it needs a NEAR target, where prediction and reality
meet. Fixed by widening the floor with the estimator's own published error
(`CapacityEstimate::uncertainty_pm`, the `unc=` on the steady line) rather than a chosen margin.

**Defect 3 — the estimator was confident in a rate the link could not carry, and the ABORT RULE was
what made it so.** With the floor widened the case *still* failed, and the numbers said why: the
deadline was computed from `C ≈ 16 427 kbps` while the shaper held the link at **500 kbps**.

`Controller::observe` built every `CapacityObservation` with a hardcoded `completed: true`. The
field was already modelled and already wired to `MAX_UNCERTAINTY_PM`; the one caller with something
else to say could not say it. So each abandoned prefix — 1448 bytes of receive buffer, timing at
42 277 kbps — entered as a completed measurement. **They agree with each other**, so the
estimator's dispersion term FELL and it became confident: `slow=48672kbps unc=500pm` against a real
500 kbps. Every downshift the controller correctly decided to make then chose a target thirty times
too dear, overran, aborted, and decided again.

The decision was never wrong. The number it was made from was — and the instrument was
manufacturing it.

`SegmentSample::abandoned()` marks it; the bytes still count and the estimate still moves, but with
maximum uncertainty attached, and `conservative_kbps` treats uncertainty as a discount. After it:
`slow=501kbps`, which is the shaped rate to within one per cent.

**A false trail worth recording.** The first hypothesis was exactly this — "the abandoned prefix is
poisoning the estimator" — and it was dropped after a host fixture showed `observe` returning
`Prime(Down)` when fed one. That fixture was answering the wrong question: the abort does produce a
downshift, and what the prefixes corrupt is the *target* it picks, which a single-sample test cannot
see. The mechanism is CONVERGENCE, so the differential test needs 24 repetitions; at 4 both legs
land on the same budget and the fix looks inert.

## Result

Same case, same shaped link, same manifest — `tests/` is byte-identical across the two runs.

| | before | after |
|---|---|---|
| verdict | **FAIL** (`settled 18000kbps, want <= 720`) | **PASS** |
| stall abort | 321 | 2 |
| `outcome=warmup_deadline` | 321 | 0 |
| `outcome=committed` | 0 after the first downshift | 6 |
| rung trajectory | `u2000 u8000 u20000 d18000` then stuck | `20000 → 8000 → 720 → 320` |
| stall | 74 s over 196 beats | none graded |
| `play=` | 617 pm | 992–1000 pm |
| segments fetched | 375 | 105 |

The trajectory in the right-hand column is what the case was written to grade: a link that falls
*under a downshift already in flight*, walked down the ladder one honest transaction at a time.

## What this does not settle

- **The abort rule is not what fixed this.** It fired twice in the passing run and its own
  contribution is unmeasured — the floor alone may be sufficient. An A/B with the guard disarmed is
  one run and has not been taken.
- **Only the `Down` direction was measured.** The upshift bound is unchanged and untested by this
  case by construction.
- **The floor's MAGNITUDE is bounded only by the target selection being sane**, and that is a
  coupling between two mechanisms rather than a property of this one. `R_target * D / C` grows
  without limit as `C` falls: an 8000 kbps target on a link measured at 100 kbps is a truthful
  160 s deadline. What keeps it small in practice is that the controller picks its target with
  `best_for_budget(C)`, so a 100 kbps link selects the ladder floor and the prediction comes back
  at 6.4 s. The measured run bears that out — warm-ups of 321 / 4 221 / 1 328 ms across the three
  commits — but nothing *enforces* the coupling, and a selection bug would now buy a long stall
  where before it bought a fast refusal. Capping it would be the unexplained constant the design
  rule forbids; the honest statement is that this floor assumes the selector.
- **The 31 ms miss on the first transaction is unexplained.** `warmup_dl=5876ms` against
  `decided=5907ms` is close enough that the deadline may be being computed from a reserve read
  slightly before the fetch starts; that is a real question and this run does not answer it.
