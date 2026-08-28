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

**Defect 4 — marking it incomplete was not enough, because the RATE still entered.** `abandoned()`
raised `uncertainty_pm`, and `conservative_kbps` discounts by uncertainty — but a prefix four times
the history trips `is_regime_change`, which RESTARTS the estimate at the prefix's own value with one
sample's confidence. So each abort reset the estimate *upward*: 5 632 → 28 744 → **101 078 kbps**
across successive aborts, and a 50% discount on 101 Mbit/s is still two orders of magnitude out.

The rule that follows is not a margin, it is what the observation means. **An abandoned transfer
may lower the estimate and may never raise it.** A fetch is abandoned because its projected
remainder did not fit the reserve — the event is evidence of INSUFFICIENCY. Its bytes are the
receive buffer's opening burst over a few hundred microseconds, so as an estimate of sustained
capacity they are biased upward by construction, and reading them as "the link is fast" inverts the
meaning of the event that produced them. A slower-than-history prefix is still kept: that is the
abort's actual message, and not keeping it would be a one-way ratchet blind to a real collapse.

**Defect 5 — and then it stopped learning at all, which is the same failure one level back.** With
the ratchet in place the fast prefixes were correctly ignored, and the estimate FROZE at its
pre-collapse value: no fetch ever completed, so nothing new ever entered. The controller chose the
same unaffordable target 36 times.

The cause is that the abort fires on the FIRST read once the reserve is small — measured
`prod=2pm`, a fetch abandoned **4 ms** in, which carries no observation of anything. So an abort now
waits until its own fetch is measurable, at `MEASURABLE_OBSERVATION_US`: already measured, already
used by `CapacityObservation::quality`, and already meaning exactly this — below it a transfer
reports latency rather than capacity. That bounds an abort's cost at a quarter second of an
already-stalled picture, and above it the sample is `Weak` at worst, which on a collapsed link is a
slower-than-history observation — the one direction the ratchet admits.

**The shape of the whole chain is worth stating once.** Every one of these five is the same mistake
in a different place: a quantity was used for a purpose its derivation did not support. A reserve
bound was applied to a direction whose benefit it did not model; a central estimate was used as a
deadline; a burst rate was used as a capacity; a regime-change rule was applied to an event that is
not a regime change; and an instrument was allowed to fire before it could measure. None was a
tuning error and none would have been fixed by changing a constant.

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

### The floor's A/B — ANSWERED 2026-08-28, and it stays

Defects 4 and 5 raised a fair question: with the estimator no longer corrupted and the abort no
longer firing blind, is the deadline floor still doing anything? The first passing run suggested
not — both successful downshifts had `warmup_dl == buf_start`, i.e. the RESERVE bound won and the
floor never bound.

Measured directly, one leg with the floor reverted to the pre-J3b `min(cold_start, reserve)` and
everything else identical: **the case FAILS.** 22 aborts, 22 deadline rejects, and the absorbing
state in its purest form —

```
abr: tx Down 18000->320kbps  warmup_dl=126ms  decided=138ms  buf_start=126ms
```

`320` is the LADDER FLOOR. With a 126 ms reserve as the only bound, even the cheapest rung on the
ladder cannot be reached, so there is no escape at any price. That is the property the floor
exists to remove, and nothing else added since removes it: the ratchet stops the estimate being
wrong and the measurability gate stops the abort being blind, but neither gives a transaction the
time it physically needs.

So all FOUR are load-bearing — both A/Bs were taken and both fail — and the reason they LOOK
redundant in a passing run is that they act at different points: the ratchet and the gate keep the reserve from collapsing in the first
place, and the floor is what makes the collapse survivable when it happens anyway.

- ~~The abort rule's own contribution is unmeasured.~~ **ANSWERED, and it stays too.** One leg
  with `StallGuard::arm` returning `None` and everything else identical: the case FAILS, by a
  DIFFERENT mechanism from the no-floor leg. With no abort the active fetch at 18000 kbps simply
  runs against a 500 kbps link, so no segment ever completes, no sample ever enters, and the
  estimate stays frozen at its pre-collapse value — `slow=98750kbps` while the shaper holds 500 —
  with the rung parked at 18000 and one failed downshift. The guard is what converts a fetch that
  cannot finish into a decision point; the floor is what gives the resulting decision enough time
  to act. Neither alone is sufficient and the two failures do not resemble each other.
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
