# Adaptive playback: the Phase 3 specification

**Status: DERIVATION, REVIEWED, NOT READY FOR PHASE 4.** Nothing here has landed. This is the
document the plan's §6 calls for. Where it and `docs/adaptive-playback-plan.md` disagree about a
formula, this one is later and cites its evidence; where it is silent, the plan governs.

> **It has now had the adversarial review the plan mandates, and it did not pass.** Five seats,
> each attacked by an independent refuter: **13 findings survived, 7 were killed**, and **6 of the
> survivors block Phase 4**. The full record, including the killed attacks and the refuters'
> corrections to their own seats, is `docs/measurements/p3-spec-review.md`. Every surviving
> finding is answered in place below, in the section it lands on, marked **[R-blocked]** where it
> is still open. **§3, §4, §5 and §7 all changed. Do not implement from the pre-review text.**

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
host-side (`docs/measurements/p2h-pms-ladder.md`) — with the two limits §3 now carries. The TIME
half — the per-segment intercept and the per-byte transport cost on a real link — is not, because
the measurement apparatus has the PMS on the same machine and no link at all.

**[R-blocked]** An earlier draft of this paragraph said §4's two parameters were written as
"named parameters whose *estimation* is specified". **That was false and is withdrawn.** No
estimator for `o0_us` or `tau_ps` appears anywhere in this document or the plan — no form, no
window, no update law, no cold-start value, no bound — and `O₀` is not even a scalar (§2 measures
three regimes for it while §4's arithmetic takes one number with no regime selector). Until §2a
exists, **§4 cannot be implemented**, and every shape decision an implementer would have to invent
is exactly the class of number the directive bans. What is already available to build it from:
`SegmentSample` carries `total_fetch_us` **and** `active_fetch_us` beside `bytes`
(`abr/estimate.rs`), so `total − active` and `active / bytes` are a two-parameter decomposition
that exists today and nobody has specified an estimator over.

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

**[corrected]** An earlier draft said "the fixture tier's `O₀ = 17.76 ms` ... is **12× low** for
deployment". **Both halves were wrong and the pairing that produced them was the real defect.**
`17.76 ms` does not appear anywhere in this repository — it comes from the planning file, from a
fit on a corpus the repo's own measurement doc says is not comparable. The repository's actual
joint fit, post-fixture-rebuild, is **`τ` = 21.40 ns/byte with `O₀` = 687.95 ms**
(`docs/measurements/p1-transaction-anatomy.md:203`) — so the fixture tier's intercept is **larger**
than the real PMS's ~214 ms, not 12× smaller. The draft had taken `τ` from that joint fit and
`O₀` from a different one, which is not a combination either fit supports: in `A = O₀ + bytes·τ`
the two are estimated together and only travel together.

**Three intercept/slope estimates exist. None of them may be mixed, and none is the deployment
value:**

| source | `O₀` | `τ` | apparatus |
|---|---:|---:|---|
| planning file | 17.76 ms | 58.51 ns/byte | pre-rebuild fixture corpus, 10 effective points |
| `p1-transaction-anatomy.md:203` | 687.95 ms | 21.40 ns/byte | post-rebuild fixture corpus |
| `p2h-pms-ladder.md` §5 | k · 108 ms, k ∈ {1,2,3} | not measurable | real PMS, no link |

`p1` says outright that its own `τ` is unsettled — 33.30 on the first five logs against 21.40 on
eleven, a 1.56× swing, with a cluster-robust SE of 15.29 that puts it ~1.4σ from zero.

**`τ` is not measurable on the host apparatus and no value is specified here.** The device tier
owns it. It ships in **picoseconds per byte** (`i64`): in ms/byte the value is 5.85e-5, which
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

**Classification, and [R-blocked] the availability split the earlier draft got wrong.** `D` is
(2). `σ` is (2). `W_j` is (2) — but the draft's "the transaction already fetches and already logs
it before deciding anything" is true of **one** decision and false of the other, and the two are
the two halves of admission:

| decision | where | is `W_j` available? |
|---|---|---|
| **validate** — commit this primed candidate, or reject it | `Controller::candidate_ready`, after `ff.rs`'s `hls_cursor_open(candidate_url, …)` | **yes.** The candidate's master playlist has been fetched and its `BANDWIDTH` parsed and logged. |
| **select** — which rung to prime at all | `Controller::observe`, at the `Proposal { rung: target }` sites | **no.** A rung's `BANDWIDTH` cannot be read without first creating a PMS encoder session for it. |

So §4's rule **can** be evaluated at the validation point and **cannot** be evaluated over the
ladder at selection time. Worse, the number is not even *retained* for the rung that does have
one — it is formatted into a log string and dropped.

Two consequences, and the second is not resolved:

* The rule below belongs at `candidate_ready`, which is where the transaction already grades a
  real segment. That is a good home for it, not a compromise.
* **Selection still needs a per-rung rate and this document does not supply one.** The catalog's
  `expected_wire_kbps` is the input R1 killed (`p2h` §6: +5.2% to +31.6% error, item-dependent,
  and non-injective — 18000 and 20000 both declare 16 150). The obvious repair is to **memoise
  `W_j` per rung as each is primed**, with the catalog as a cold-start prior, so a rung's rate is
  right from its second visit onward — but that never reaches a rung this playback has never
  visited, which is exactly the selection case. Alternatively `/decision` can be probed per
  candidate for 13–18 ms warm (`p2h` §5), which does reach it, at the cost of a control-plane
  round trip per candidate per evaluation. **Neither is specified here. Phase 4 cannot pick one
  by itself without inventing policy.**

**How much evidence `σ` actually rests on**, stated properly because the earlier draft's "1 120
segments" did the rhetorical work of a large sample:

* The 1 120 are **28 (window, rung) cells over ~160 media indices**, not 1 120 trials. Adjacent
  rungs are strongly rank-correlated by index (Pearson r 0.60–1.00), and rungs 18000 and 20000 are
  the same encoder session — all 40/40 `(bytes, duration, declared)` triples identical in both
  movie windows, so **80 rows are literal copies of 80 others** (1 040 non-duplicate).
* **The per-window maximum is usually one scene**, not forty draws: media index 11 sets it at 10
  of 11 movie-opening rungs, index 1201/1208 at 7 of 11 movie-40min rungs.
* **Rung coverage is uneven where it matters most.** Rungs 4000, 12000 and 22000 have 4 windows
  and 3 items; the other eight rungs ≥ 2000 have 2 windows and **one film** — 640 of the 1 120
  segments (57%). **Rung 2000 — the boundary of the rule — is one item, 80 segments**, and it
  holds the corpus's third-largest `s` (0.8424). The episode is the item that breaks the bound at
  720 (1.155, the worst overshoot in the corpus) and it is **absent from rung 2000 entirely**.

None of that moves `σ` — the maximum and the exceedance count are unchanged, and an attack arguing
`σ` is merely an extreme order statistic whose expectation grows like `ln n` was **tested and
killed** (`p3-spec-review.md`). It changes what may be claimed for it. §8 carries the remedy, and
it is not the one the earlier draft named.

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

**[corrected] There is no `prod ≤ 1000` test in the shipped code.** An earlier draft called the
rewrite below "free" on the strength of one, taking the plan's `prod ≤ 1000 ⟺ A ≤ D` — which is a
true statement about the *quantity* — and reading it as a shipped *comparison*. Grepped, the three
real boundaries are:

| site | gate |
|---|---|
| `abr/controller.rs:328` | `ratio_pm <= production_safe_pm` (**750**) |
| `abr/controller.rs:363` | `sample.production_ratio_pm() <= 800` — **a bare literal**, in `candidate_ready`, the one comparison that admits or refuses every upshift |
| `abr/ladder.rs:341` | `ratio <= production_safe_pm` (**750**) |
| `abr/viability.rs:42-43` | `> production_max_pm` (**1100**), and `> production_safe_pm` (750) |

So replacing any of them with `A ≤ D` is a **threshold change**, not a rewrite, and the 800 in
`candidate_ready` is an undocumented literal that §7's table never audited because it is not a
field of `AbrPolicy`. Whichever site is meant, the identity still argues for spelling the
comparison without division —

```rust
total_fetch_us <= media_duration_ms * 1_000
```

— because integer division floors, so a `≤ 1000` form admits `A < 1.001·D`, a 2 ms drain per
segment and 3.6 s of reserve per hour. But it is a behaviour change and must land as one.

### [R-blocked] The rule as stated cannot upshift, because the reserve is not in it

`A_j ≤ D` charged **per segment at the worst case** is stateless in `B`. The reserve is the only
physical reason a buffered player may run a rung whose *peak* exceeds the link, and it appears
nowhere on the admission side — only in §5's downshift deadline. Since `W_j` is a target **average**
and `σ·W_j` is a **peak**, requiring every segment's peak to fit inside `D` demands the link carry
the peak continuously, which is strictly stronger than sustainability.

The measured gap is large: at high rungs on easy content the *median* delivered rate is 0.14–0.26
of declared, so a rule keyed on the peak refuses rungs the link would carry comfortably for long
stretches.

**Two overstatements from the seat that raised this are withdrawn on the refuter's recomputation**
and should not be repeated: "settles three ladder steps low" matches neither measured window (the
mean-affordable rung at a 6 Mbit/s link is the ladder *top* on the easy window and rung 10000 on
the hard one), and the 2.80× is **not** deliverable headroom being wasted — the same film's 40-min
window aggregates 12 674 kbps at rung 22000 against that same link, so a rule that spent it would
guarantee a collapse at the difficulty change.

That is the real shape of the problem, and it is why the fix is not simply "use the median": the
correct condition has to admit on the **average** while proving the **peak** is survivable *out of
the reserve* for the length of a hard passage. Writing that condition needs the `B_after`
relaxation model §1 records as unavailable in closed form. **It is not written here, and until it
is, §4 admits far too little.**

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

**Trigger — when to reconsider at all.** **[R-blocked] The earlier draft listed only
`¬sustainable(current)` and `draining()`. Both are distress conditions, so from a healthy state —
current rung sustainable, reserve flat — neither fires, the target rule is never evaluated, and
the controller can never climb.** The section opened by blaming the plan for making the emergency
path look like the only path (R23) and then made it literally the only one.

The trigger set has **three** members and the third was missing:

| trigger | fires when | direction |
|---|---|---|
| unsustainable | `¬sustainable(current)` under §4 | down |
| draining | `draining()` — the magnitude test, not a sign test | down |
| **periodic review** | **the steady state: a healthy rung, on a cadence** | **up** |

The third is what makes climbing reachable at all, and this document does **not** specify it — not
its cadence, not its reserve precondition, not its interaction with the anti-flap cost. The plan's
N7 does keep a `safe_budget`-driven upshift proposal with a reserve gate and `&& !draining`, and
this document's precedence rule says the plan governs where this is silent, so **Phase 4 built
from the plan will still climb**. But §5 claims to be the exhaustive statement of "when to
reconsider at all" and is not, and an implementer reading it as exhaustive would ship a controller
that boots at the bootstrap rung and stays there on any comfortably fast link — with no distress
trace for a test to catch, because nothing distressing happens.

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

**[R-blocked] This table audits the eighteen fields of `AbrPolicy::measured()`, and that is not
the same set as "the quantities that decide".** At least **33 more** decision constants sit inside
the very utility sum this table is adjudicating, none of them fields of that struct and none of
them audited here:

* `hls_quality_score`'s bucket table — 8 boundaries and 9 values (`abr/mode.rs:127-135`). **This
  is the numeraire every row below is denominated in**, and it is unclassified.
* Original's risk ladder — `{2,10,25,60}`, `/2`, `+20`, `×4`, `min 15` (`abr/mode.rs:181-194`).
* HLS's risk ladder — `{0,1,4,12,40}`, `+20`, `+30`, and the `uncertainty_pm >= 500` gate
  (`abr/viability.rs:43-59`).
* The bare `800` in `candidate_ready` (§4), which is not in any struct at all.

So the directive's test has been applied to 18 of at least 51, and the omitted ones are inside the
sum. **Executing this table in full would leave every term of `total` at `abr/mode.rs:164`
classified while its unit of account is undefined.** The audit below stands as far as it goes; it
does not go as far as it claimed.

**The census in the next sentence was also wrong.** "Six derive or delete, five need a lease, seven
are product choices" sums to 18 but does not partition the table: only two rows carry a device
verdict, and three rows carry a verdict in none of the three named classes — and those three are
precisely the coefficients of the sum. Read the table, not the summary.

| field | today | verdict |
|---|---:|---|
| `production_safe_pm` | 750 | **Derive.** A 25% margin on the `A ≤ D` boundary. Under §4 the margin is the conformal quantile on `A`, not a constant. |
| `production_max_pm` | 1 100 | **Delete — but it is load-bearing, and this table did not say where.** It admits a rung 10% *past* real time, which provably drains, so the verdict stands; but it is read at `abr/viability.rs:42` inside `candidate_risk`, so deleting it is a change to the risk score and not a dead-constant removal. Phase 4 must handle that, and §4's replacement boundary is a threshold change at whichever of the three real sites is meant (§4). |
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
| `risk_weight` | 2 | **[R-blocked] The "delete" verdict is WITHDRAWN.** A probability is dimensionless and the other terms of the sum are quality points, so a probability is precisely *not* commensurable with them — it needs a price in points, which is the coefficient being deleted. And this specification constructs no probability anywhere: turning §4's per-segment exceedance into a *stall* probability needs the `B_after` relaxation model §1 records as unavailable. `risk_weight` is also the exchange rate for **both** ledgers (`abr/mode.rs:157` and `:206`), not just the HLS one this table quotes. Verdict: **keep, reclassified as a product choice** — the price of risk in quality points — until something produces a probability. |
| `server_cost_weight` | 4 | **Keep, and re-measure — but not on the premium quoted here.** "2.1× the work for 4% more bits" is the ratio of two catalog entries (`P1080High` 20 011 and `Uhd` 20 895), and `p2h` §6 measures that the app never obtains the first of them: under the request the app really sends, rung 20000 declares 16 150, so the real trade is **2.1× work for 29% more bits**. The premium is off by 6.6×, and it is the entire justification for the weight. `production_load_pm` is separately a per-item quantity stored as a per-server constant, inert below its own floor for the modal item at every rung. |

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

* **A second ITEM at the thin rungs, on THIS server** — which the earlier draft got wrong by
  asking only for a second *server*. Eight of the eleven rungs ≥ 2000 rest on one film, including
  rung **2000**, the boundary of the rule, and the item that breaks the bound one rung below it
  (the episode, 1.155 at 720) is absent from rung 2000 entirely. `tools/pms-rung-sweep.py`
  defaults to the full 13-rung ladder, so this needs no `--rungs` and no second server: it is one
  host-only command per additional item. **Do this first — it is cheaper than the second server
  and it closes the hole that a second-server run would faithfully reproduce.**
* **`σ` on a second server**, after that. Falsified by any segment above `0.85·W` at a rung ≥ 2000.
* **An estimator for `O₀` and `τ`** (§2a, which does not exist). Without it §4 is not
  implementable at all, whatever the device measures.
* **A bound for rungs 320 and 720**, where §3 does not hold.
* **The `B_after` relaxation model** from the queue's actual byte list, graded on R18's residuals.
* **λ and `P(revert)` are not available at all** (R6): four events, dispersion index 10.8, and a
  95% CI of width 0.6. The mode-comparison rule must degrade gracefully without them rather than
  wait for them.

**Process — done, and the answer was no.** The review the plan requires has run: five seats, each
attacked by an independent refuter, 13 findings surviving and 7 killed
(`docs/measurements/p3-spec-review.md`). I predicted the weakest points would be §3's
single-server `σ`, §7's product choices, and §4's two missing numbers. **`σ` itself survived** — an
attack arguing it is merely an extreme order statistic was recomputed and killed. What actually
broke was structural and I had not anticipated any of it: §5 could not climb, §4 had no reserve
term, §3's key input is unavailable at the decision that needs it, §7 audited one struct instead of
the decision surface, and two of my "delete" verdicts were wrong.

**Six findings block Phase 4** and are marked **[R-blocked]** above. In order of what has to be
answered first:

1. **§2a — an estimator for `O₀` and `τ`.** Nothing downstream is implementable without it.
2. **§4's reserve term.** Needs the `B_after` relaxation model from §1, which is unwritten.
3. **§5's periodic upshift trigger** — cadence, reserve precondition, anti-flap interaction.
4. **§3's selection-time rate** — memoise per rung, or probe `/decision` per candidate. Pick one.
5. **§7's real scope** — the 33 constants inside the utility sum, starting with the quality
   bucket table that is its unit of account.
6. **A probability, or `risk_weight` stays** — and on current evidence it stays.

A second review is warranted once those are answered, on the same terms.
