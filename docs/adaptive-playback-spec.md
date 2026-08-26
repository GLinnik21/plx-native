# Adaptive playback: the Phase 3 specification

**Status: DERIVATION, not yet implemented.** Nothing here has landed. This is the document the
plan's §6 calls for and the one its "independent adversarial review before Phase 4" is meant to
attack. Where it and `docs/adaptive-playback-plan.md` disagree about a formula, this one is later
and cites its evidence; where it is silent, the plan governs.

**The rule this document is written under**, from the standing directive: every quantity used to
make a decision must be classifiable as one of

1. **physical / structural** — a queue size, a segment duration, an ABI constant
2. **directly measured** — with the measurement named
3. **mathematically derived** — from (1) and (2)
4. **an explicit product or SLO choice** — stated as a choice, with the trade named

and there must be no unexplained sample counts, buffer thresholds, multipliers, margins,
cooldowns, utility weights or rung-walking rules. §7 applies that test to all eighteen shipped
tunables, one at a time, and it is the part of this document most likely to be wrong.

**What is derivable today and what is not.** The SIZE half of the admission problem is settled
host-side (`docs/measurements/p2h-pms-ladder.md`). The TIME half — the per-segment intercept and
the per-byte transport cost on a real link — is not, because the measurement apparatus has the PMS
on the same machine and no link at all. §4 is therefore written with those two as named parameters
whose *estimation* is specified and whose *values* are a device job. That is deliberate: a
specification that hid the gap behind a plausible constant would be the thing this whole effort
exists to stop.

---

## 1. The plant

Facts about the machine, independent of any controller.

**A segment** carries `D` milliseconds of media and `bytes` bytes. `D` is measured per segment
from `#EXTINF` — never assumed to be 2000, because a playlist's last segment is short and a
different `secondsPerSegment` is one query parameter away.

**The reserve ceiling.** The AU queues bound how much media can be held:

```
B_max_ms(R) = min over lanes of ( lead_ms + queue_BITS / R_ES.max(1) )
```

`lead_ms` is `MAX_FEED_AHEAD_NS`, `queue_BITS` is `AQ_VIDEO_BYTES`/`AQ_AUDIO_BYTES` in **bits**,
`R_ES` is the lane's measured elementary rate. Validated at five rungs to ≤2.9%. Three consequences
are load-bearing and only the first is currently respected:

* `B_max ∝ 1/R`, so the ceiling **falls** as the rung rises. This is why an upshift guard that is
  `Ω(D)` cannot be satisfied at the top of the ladder (`docs/measurements/p0-plant-sizing.md`).
* **`B_max(R) ≥ D` must be asserted.** Below it a single segment does not fit the queue and
  `aq_push` blocks forever — a silent hang, not a stall. Nothing checks this today.
* `.max(1)` on the divisor is mandatory; `R_audio = 0` is reachable.

**The transaction identity**, exact and already validated (7/7, median 26 ms):

```
buf_decided = buf_start − decided_ms + n·D
```

What is **not** available in closed form is the relaxation from there toward `B_max(R_new)` over
following segments. The substituted form `B_after = min(B + n·D − E_tx, B_max)` misses by up to
6 238 ms and by +1 241 ms in the *unsafe* direction (R18). The instantaneous ceiling is the PTS
span of whatever fits in `Q` bytes given the **actual per-segment byte list in the queue**, which
`hls: segment bytes=` supplies. Modelling it from that list, and grading the model on R18's
residuals, is Phase 3 work that has not been done. **No guard in §5 depends on it.**

## 2. What a segment costs

```
A = O₀ + bytes · τ
```

`A` is total acquisition time, `O₀` the fixed per-segment cost, `τ` the per-byte cost. The
*structure* survived every review pass: byte-proportional and link-independent at fixed size
across a 15× link range. The coefficients did not, and this is the sharpest correction the
measurements make to the plan:

**`O₀` is not one number.** Measured against a real PMS over 1 426 fetches
(`docs/measurements/p2h-pms-ladder.md` §5), the server's contribution to `O₀` is **quantized**:
best-fit quantum **108.13 ms**, median residual 3.47 ms against ~27 ms if the grid were unrelated.
It takes three regimes:

| regime | `O₀` server term | evidence |
|---|---|---|
| steady state, source the encoder handles | k · 108 ms, k mostly ∈ {1,2,3} | flat from rung 2000 to 22000 across a 69× bitrate range; unchanged by pacing |
| after a seek | ≈ 6 quanta, ≈ 660 ms, **exactly one segment** | index 900 and 1800 cost 654 and 663 ms; the next segments cost 111 and 208 |
| source the encoder does not handle | rises with the rung, to 862 ms median / 1 306 ms max | the 4K HEVC item, rungs ≥ 12000 |

The fixture tier's `O₀ = 17.76 ms` describes a static file server and is **12× low** for
deployment. Any margin calibrated against it is calibrated against the wrong apparatus.

**`τ` is not measurable on the host apparatus and is not specified here.** The device tier owns
it. It ships in **picoseconds per byte** (`i64`): in ms/byte the measured value is 5.85e-5, which
truncates to integer **0** and divides by zero in the demux worker (R20).

## 3. How big is the next segment

This is what the plan's R1 killed the old admission rule over, and it is now answered without a
device.

Let `W_j` be the rate the server **declares** for rung `j` — `#EXT-X-STREAM-INF:BANDWIDTH` from the
master playlist. Three properties, all measured over 1 440 segments on three items:

* `W_j` **equals the `/decision` response's own bitrate**, exactly, in the wire shape the app
  sends (26/26 rung-window pairs). It is a target average, **not** the RFC 8216 peak.
* **It bounds the delivered rate at every rung the ladder uses above 720 kbps.** Not one of
  **1 120** segments at rungs ≥ 2000 exceeded `0.85 · W_j`. The maximum anywhere was 0.846.
* **It does not bound it at 320 or 720.** Overshoot to 1.285 and 1.155 respectively; at rung 320
  the peak segment carries 1.91× the video rate the server said it was targeting, because a
  minimum-quality floor beats a rate target that small.

What makes the first bound a *ceiling* rather than a lucky quantile: between two 80 s windows of
one film the **median** of `delivered/declared` moves 4.3× while the **max** stays inside
`[0.77, 0.85]`.

So, for rungs at or above 2000 kbps:

```
bytes_j  ≤  σ · W_j · D / 8000            σ = 0.85, W in bit/s, D in ms, bytes in bytes
```

**Classification.** `W_j` is (2) directly measured, per transaction, from a playlist the
transaction **already fetches and already logs** (`ff.rs`, `hls: master one-variant bandwidth=`).
`D` is (2). `σ` is (2) — a measured structural ceiling, and the single most fragile number in this
document: one server, three items, four windows. §8 says what would falsify it.

**Two alternatives were tested and rejected**, which is why this one is not merely the tidiest:

* **The declared *ratio* as a bound on the delivered ratio** — refuted, 36/40 paired segments break
  it at one step, overshoot to 1.180.
* **A statistical bound over segment size** — the lag-1 autocorrelation of the size series ranges
  **−0.376 to +0.764** across four windows with no consistent sign. No fixed `ρ` is estimable, so
  neither an AR(1) correction nor an exchangeability claim is available. A structural bound needs
  neither.

**Rungs 320 and 720 are outside this rule and must be handled as their own case.** They are also
the rungs a controller reaches when everything has already gone wrong, so "we have no bound there"
is not academic. §8 carries it as open.

## 4. The admission rule

A rung is **sustainable** when one segment's acquisition fits inside the media it provides:

```
A_j ≤ D
```

The existing `prod ≤ 1000` test is exactly this — `production_ratio_pm` is
`total_fetch_us·1000 / (media_duration_ms·1000)`, the two thousands cancel, and the comparison is
`total_fetch_us / media_duration_ms ≤ 1000`. It should be spelled without the division at all:

```rust
total_fetch_us <= media_duration_ms * 1_000
```

Integer division currently floors, so `prod ≤ 1000` admits `A < 1.001·D` — a 2 ms drain per
segment, 3.6 s of reserve per hour. The rewrite is free and removes the rounding question.

### The shipped integer form

Substituting §2 and §3, with `o0_us` the intercept in microseconds and `tau_ps` picoseconds per
byte:

```rust
// bytes the candidate could demand, worst case. CEILED: flooring a safety bound is the
// wrong direction, and this is the term the whole rule is a bound on.
let bytes_worst = (sigma_pm * declared_bps * d_ms + 7_999_999) / 8_000_000;

// A_j <= D, multiplied through by 1e6 so no division survives into the comparison.
1_000_000 * o0_us + bytes_worst * tau_ps  <=  1_000_000_000 * d_ms
```

**Why this association and not another.** The obvious substitution overflows. Folding `σ·W·D·τ`
into one product before dividing reaches 7.6e17 on the left and **1.6e19** on the right at rung
22000 — past `i64::MAX` — so the candidate association is not a style question. Computing
`bytes_worst` first keeps every intermediate under 1e14:

| term | value at rung 22000, D = 2000 | headroom to `i64::MAX` |
|---|---:|---:|
| `sigma_pm * declared_bps * d_ms` | 3.6e13 | 2.6e5 × |
| `bytes_worst` | 4.5e6 | — |
| `bytes_worst * tau_ps` | 9.5e10 | 9.7e7 × |
| `1_000_000 * o0_us` | 2.1e11 steady, 6.6e11 after a seek | 1.4e7 × |
| `1_000_000_000 * d_ms` | 2.0e12 | 4.6e6 × |

At those values the rule reads `3.09e11 ≤ 2.0e12` — the top rung is sustainable with the intercept
and the worst-case bytes both charged in full, which is the sanity check that the association is
not merely non-overflowing but usable. (`τ` there is the fixture tier's 21.4 ns/byte standing in
for a number the device still owes; see §2.)

**Overflow checking is asymmetric between host and device and that is the hazard**, not the
arithmetic: `overflow-checks` is **on** under `cargo test` and **off** in release, so an unsigned
underflow panics on the Mac and wraps silently on the television. Every term above is `i64`, and
no subtraction of unsigned quantities appears in the rule.

### The comparison between two rungs

The plan's `viable` predicate exists in two contradictory forms in its own text, and the one it
ships would divide `R_t/R_i` in integers — truncating to **0** for a 20000→720 downshift (making
every downshift unconditionally viable) and to **1** for 4000→6000 (admitting a 50% upshift on the
current rung's evidence). Re-associated, with `B` the **measured** bytes rather than any catalog
rate:

```
Ô₀·(B_i − B_j) + B_j·A  ≤  D·B_i
```

`i64`, no division, and `(B_i − B_j)` is signed — in `u32` an upshift makes it negative, which
panics under `cargo test` and wraps on the television.

**In this specification that predicate is not needed for admission**, because §3 gives an absolute
bound on `bytes_j` rather than a ratio to `bytes_i`. It is retained here only because the plan's
§5 ships it, and the note above is what a reviewer needs if it comes back.

## 5. Trigger, target and deadline are three different things

The plan conflates them, which is why the emergency path looked like the only path (R23).

**Trigger — when to reconsider at all.** `¬sustainable(current)` under §4, or `draining()`. This
was never written down anywhere.

**Target — which rung to move to.** The highest rung that is sustainable under §4 at the current
`Ô₀`/`τ` estimates. Chosen directly, never "one rung up": a jump from 8 to a 15 Mbit/s budget
primes the 14 Mbps encoder once instead of paying for three encoder creations.

**Deadline — the last point at which an affordable escape still exists.**

```
B < A_i + E_tx_down(k*)
```

This is `must_downshift`, and it is a deadline, not a trigger. Two defects in the shipped version
must be fixed with it:

* **`buffered_ms()` returns 0 for "unknown"** and `must_downshift` reads 0 as "empty", so a missing
  audio timestamp fires a deadline-free downshift on a full reserve. It must become
  `Option<i64>` — the Original path already encodes the same condition that way.
* **There is no terminal case** when no rung is viable. A predicate with no action in the region
  that motivates it.

`E_tx` is a **lower** bound, not a worst case: `decided − E_tx` reaches 1 231 ms, because the
`NotReady` retry (up to 8 s) and the post-commit block have no leg of their own. Any deadline built
on `E_tx_max` as "the sum of enforced deadlines" is unsound until those are bounded.

## 6. Numerical safety

Not a review pass — part of the specification.

* **Integer `i64` throughout. No floats anywhere in the ABR path.** `sim.rs` promises bit-identical
  results across machines and a soft-float `exp()` on ARM breaks that promise.
* **`τ` in ps/byte**, `O₀` in µs, `D` in ms, `W` in bit/s, per-mille for dimensionless ratios. Every
  formula states its units.
* **`.max(1)` on every divisor.**
* **Rounding direction is stated per branch, and the rule is sign-independent**: on a safety bound
  every LHS term ceils and the RHS floors. "Ceil on one branch, floor on the other" is backwards
  for a negative term — `floor(−100.5) = −101` makes the LHS *smaller*, i.e. permissive.
* **One rounding defect exists in shipped code today**: `BufferEstimate::update`'s EWMA truncates
  toward zero, so a *negative* slope rounds **up** — understating drain against `draining()`'s
  −50 ms/s threshold.
* **`starvation_horizon` returns milliseconds in a field named `drain_per_s`.** The arithmetic is
  right (`B·R/deficit` is time-to-empty in ms, and `/1000` gives seconds); the name asserts a rate.
  A dimension error waiting to be made by the next reader.

## 7. Every shipped tunable, against the classification rule

Eighteen fields in `AbrPolicy::measured()`. This is the table the directive demands, and the
honest summary is that **six can be derived or deleted now, five need one device lease, and seven
are product choices that must be argued as choices rather than tuned.**

| field | today | verdict |
|---|---:|---|
| `production_safe_pm` | 750 | **Derive.** A 25% margin on the `A ≤ D` boundary. Under §4 the margin is the conformal quantile on `A`, not a constant. |
| `production_max_pm` | 1 100 | **Delete.** It admits a rung 10% *past* real time, which provably drains. The boundary is 1000, exactly, and §4 spells it without division. |
| `production_floor_pm` | 250 | **Replace with a measurement.** This is `O₀` as a per-mille of `D`. Measured: 107 pm on an easy source (§2) — the constant is 2.3× high. It also stops being a "floor": it is the intercept. |
| `vbr_allowance_pm` | 1 350 | **Delete on the HLS path**, where §3's `σ` against the declared rate replaces it. **Keep on the Original path**, where the source file's own peak-over-average is still unmeasured. |
| `stale_half_life_ms` | 30 000 | **Device.** How fast confidence should decay is how fast link capacity actually changes. |
| `bootstrap_confidence_pm` | 1 350 | **Device.** One probe, no dispersion; the margin carries the uncertainty. Same number as `vbr_allowance_pm` for a different purpose, which is a smell. |
| `minimum_buffer_ms` | 2 500 | **Product choice, re-derived against `B_max`.** A reserve target above `B_max(R)` is unsatisfiable at that rung — the R2/R4/R22 condition, which nothing asserts. |
| `emergency_buffer_ms` | 2 000 | Same. |
| `starvation_fallback_secs` | 20 | **Product choice.** "A visible switch is worth it below this horizon." |
| `starvation_safe_secs` | 60 | **Product choice.** |
| `benefit_horizon_ms` | 120 000 | **Product choice**, and a defensible one: it prices a benefit against the interruption that buys it. |
| `visible_switch_cost` | 15 | **Product choice needing an exchange rate.** 15 is stated as "about one ladder step". That is the right *form* — quality steps per interruption — but the ladder step it is calibrated against is 2→4 Mbps, and §3's measurements say ladder steps are not equal. |
| `visible_switch_penalty` | 15 | Same, plus: this is the anti-flap mechanism and it is the one place a *derived* answer exists — the cost of a switch is `E_tx` plus a reload, both measurable. |
| `visible_switch_decay_ms` | 120 000 | **Product choice.** |
| `original_quality_bonus` | 40 | **Open, and the ledger it feeds is dead as specified** (R5): "quality step" on a bitrate ladder does not respect `transcode ≤ source`, so an 8.5 Mbit/s 1080p source scores three steps below a 20 Mbit/s transcode of itself. Quality must be relative to the source and concave. |
| `original_feature_bonus` | 25 | Same. DV/Atmos must be represented explicitly, not through a bitrate proxy. |
| `risk_weight` | 2 | **Delete.** Under §4 the risk term is a probability with units, not a score in `{1,4,12,40} + 20 + 30`. A weight exists only to make a score commensurable, and a probability already is. |
| `server_cost_weight` | 4 | **Keep, and re-measure.** The trade is real (2.1× the work for 4% more bits) but `production_load_pm` is a per-item quantity stored as a per-server constant, and is inert below its own floor for the modal item at every rung (`p2h` §6b). |

## 8. What blocks Phase 4

**Needs the device, and all of it fits one lease:**

* `O₀` and `τ` on a real link — §2 and §4's two parameters. Nothing in §4 can be evaluated without
  them.
* The `A/D ∈ [0.80, 1.05]` band, which is 0 of 366 samples in the entire existing corpus. The whole
  law is keyed on a boundary no measurement has ever been near.
* `E_tx(up, reject)` at n ≈ 10, and `E_tx_down` under collapse. Downshifts have no deadline at all
  today.
* The audio lane of `B_max`, which has never bound in 366 samples.
* The over-grant gate, on a binary carrying the hoisted `safe=` telemetry.

**Needs no device, and is not done:**

* **`σ` on a second server.** It is the load-bearing number of §3 and it rests on one PMS.
  Falsified by: any segment above `0.85·W` at a rung ≥ 2000 on another server. This is a host-only
  run and it should happen before anything is built on §3.
* **A bound for rungs 320 and 720**, where §3 does not hold.
* **The `B_after` relaxation model** from the queue's actual byte list, graded on R18's residuals.
* **λ and `P(revert)` are not available at all** (R6): four events, dispersion index 10.8, and a
  95% CI of width 0.6. The mode-comparison rule must degrade gracefully without them rather than
  wait for them.

**Process:** the plan requires an independent adversarial review of this document before any of
Phase 4 is written. The four seats that reviewed the plan — probability, control theory, decision
theory, numerics — found that the mathematics survived and everything estimated from the corpus did
not. This document has a different exposure: its structural claims are now measured, and its
weakest points are §3's single-server `σ`, §7's seven product choices, and the fact that §4 cannot
be evaluated at all until the device supplies two numbers.
