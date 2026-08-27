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

## 2a. [RESOLVED] There is no estimator for `O₀` and `τ`, because nothing needs them

§8 listed "an estimator for `O₀` and `τ`" as the **first** thing blocking Phase 4 — "nothing
downstream is implementable without it". That was the wrong question. The two coefficients are
never separately needed, and the quantity that *is* needed has a closed form that estimates
neither.

**The derivation.** Physics supplies exactly two constraints on the model `A = O₀ + bytes·τ`: a
fixed cost cannot be negative (`O₀ ≥ 0`), and more bytes cannot cost less (`τ ≥ 0`). Take any one
observation `(b_i, A_i)` and ask what a different byte count `b_j` costs on the same link:

```
A_j = A_i + (b_j − b_i)·τ

b_j ≥ b_i :  τ ≤ A_i/b_i   (because O₀ ≥ 0)   ⟹   A_j ≤ A_i · b_j/b_i
b_j < b_i :  τ ≥ 0                            ⟹   A_j ≤ A_i

both      :  A_j  ≤  A_i · max(1, b_j/b_i)                    the TRANSFER BOUND
```

Both halves are **tight** — the first is attained at `O₀ = 0`, the second at `τ = 0`. So the bound
is the exact worst case over every split of `A_i` between the two coefficients, which is precisely
the split R7 proved this corpus cannot identify (ten effective degrees of freedom, `bytes`
collinear with rung). **The identification problem is dissolved rather than solved**, and the three
irreconcilable `(O₀, τ)` fits tabulated above stop being a blocker: none of them is used.

**One asymmetry decides how much of the ladder needs a size prediction at all.** A downshift bound
carries no `b_j`: acquisition cannot rise when the byte count falls, so `A_j ≤ A_i` holds whatever
the candidate's segments turn out to weigh. Only **upshifts** need §3's `σ`, which is why the three
rungs where `σ` has no usable ceiling (320, 720, 2000 — `p2h` §2/§2a) do not matter: they are
downshift targets.

### The single-observation form is REFUTED, and its failure shape is the evidence

Graded over every ordered pair in the committed device logs, `A_j ≤ A_i·max(1, b_j/b_i)` is
violated on **36.6%** of pairs (`tools/abr-transfer-bound.py --grade pairs`). The derivation is
deterministic; real acquisitions are not. **How it fails is what licenses the fix:**

| corpus | worst overshoot | reading |
|---|---:|---|
| the five steady-link pin cases | 1.05 – 1.06 | noise around the model |
| `brief_dropout`, `steady_modest_link` | 1.41, 2.00 | the link moved a little |
| `oscillating_link`, `slow_start_then_fast` | 8.99, 20.35 | the "same link" precondition genuinely broken |

A 5% overshoot on a settled link is dispersion, not a wrong model. A 20× overshoot is a different
link, and no per-segment bound of any shape survives that — it is `CapacityEstimate`'s regime-change
reset that owns it.

### The shipped form: the k-th order statistic of the transferred window

Over a trailing window of `n` observations, transfer each to the candidate byte count and take the
`k`-th largest:

```
Â_j  =  k-th largest of  { A_i · max(1, b_j/b_i)  :  i in the last n segments }
```

**Why this carries a guarantee.** The transferred values are a *fixed measurable function* of the
pairs `(b_i, A_i)`. A fixed function of exchangeable variables is exchangeable, so the order-
statistic result §4 already relies on applies unchanged:

```
P( T_next > k-th largest of the window )  =  k/(n+1)          exactly
```

`ε = k/(n+1)` is therefore (4), an explicit SLO choice, and `n = k/ε − 1` follows from it (R28's
corrected theorem — `k/ε − 1`, not `1/ε − 1`). Nothing else is chosen.

**Measured against real acquisitions it is conservative at every setting tried**, which is the
evidence that the exchangeability assumption is not being strained in practice — and a better
answer than R9's proposed AR(1) correction table, since `p2h` §4 refuted a *stable* autocorrelation
(ρ₁ spans −0.376 to +0.764 with no consistent sign), so no fixed ρ is estimable to correct with:

| n | k | nominal ε | observed | tested |
|---:|---:|---:|---:|---:|
| 10 | 1 | 9.09% | 4.31% | 487 |
| 20 | 1 | 4.76% | **1.06%** | 377 |
| 20 | 2 | 9.52% | 5.04% | 377 |
| 29 | 1 | 3.33% | 1.08% | 278 |
| 29 | 3 | 10.00% | 5.76% | 278 |
| 40 | 2 | 4.88% | 2.53% | 158 |

### [MEASURED 2026-08-27] The guarantee holds on a stationary link and degrades ~2x off one

The table above is settled links. The `pipe_abr_band_*` sweeps put the first 18 samples ever into
`A/D ∈ [0.80, 1.05]` and walk the link down a ladder of legs while a pin holds the rung. **On those
two logs the bound is anti-conservative**: 9.23% against a 4.76% nominal at (20, 1), 16.92% against
9.52% at (20, 2), 14.89% against 10% at (29, 3).

**The cause is not ambiguous — all 6 violations fall within one window of a link-rate step.** The
window straddles a leg boundary and carries observations from a link that no longer exists, so
exchangeability fails by construction rather than by degree.

**A regime-change reset does not rescue it, and the reason is a number worth knowing.**
`CapacityEstimate`'s existing rule fires on a **4×** move; the sweep's largest step is 3.1×, so it
never fires and the exceedance is unchanged. A 2× threshold fires but starves the window — 65
gradable samples fall to 14 — and still leaves 7.14%. There is no stationary regime to reset *to*.

**Pooled over all 382 device acquisitions the bound still holds** — 3.14% against 4.76% at (20, 1),
6.64% against 10% at (29, 3). So the honest statement of the guarantee carries a precondition:
`ε = k/(n+1)` on a stationary link, ~2ε on one that is actively degrading. **Do not tighten `ε` to
compensate.** The exceedance is not a property of `ε`; it is the window describing a link that has
changed, and a smaller `ε` buys nothing against a non-stationary process. §4's condition (2) is the
layer that covers it, and measurably did: `A/D` reached **1.73** with no stall.

**What the guarantee does NOT cover, stated because §2's earlier draft is what happens when it is
not.** Exchangeability covers the statistics. It does not cover the model step: `A_j ≤ T_next` holds
only if the plant parameters really are those implied by the next observation. That assumption is
the one the refutation above breaks, and the `pairs` grade is kept in the tool precisely so its
failure stays visible rather than being tidied away.

**Integer form.** A multiply, a ceiling divide and a selection — no floats, no fit, no coefficient:

```rust
// A_i * max(1, b_j / b_i), rounded UP: this is a safety bound, so flooring is the wrong way.
let transferred = if query_bytes <= observed_bytes {
    acquisition_us
} else {
    (acquisition_us * query_bytes + observed_bytes - 1) / observed_bytes
};
```

`acquisition_us` ≤ 6e7 (a 60 s fetch) and `query_bytes` ≤ 2e7 give a product ≤ 1.2e15, against
`i64::MAX` = 9.2e18 — six thousand times the headroom. `observed_bytes` is `.max(1)` at the call
site; a malformed log line must not divide by zero on the demux worker (R20's failure, in a new
place).

## 3. How big is the next segment

This is what the plan's R1 killed the old admission rule over, and it is now answered without a
device.

Let `W_j` be the rate the server **declares** for rung `j` — `#EXT-X-STREAM-INF:BANDWIDTH` from the
master playlist. Three properties, measured over **1 560 segments on three items in five windows**:

* `W_j` **equals the `/decision` response's own bitrate**, exactly, in the wire shape the app
  sends (26/26 rung-window pairs). It is a target average, **not** the RFC 8216 peak.
* **[CORRECTED] It bounds the delivered rate at rungs 4000 and above** — not 2000, which is what
  this bullet said until a second item was run through the full ladder and refuted it. 0 of
  **1 440** segments at rungs ≥ 4000 exceed `0.85 · W_j`, max **0.8456**. At rung **2000**, 9 of
  120 exceed it, to **0.9175**.
* **It does not bound it at 320, 720 or 2000.** Overshoot to 1.285, 1.155 and 0.917. Max `σ` decays
  *monotonically* across the whole ladder — 1.285, 1.155, 0.917, 0.846, … 0.798 — so this is one
  curve crossing a threshold, not a bound that holds and then breaks: the encoder cannot go below a
  content-dependent **quality floor**, and a small enough rate target loses to it.

What makes the bound a *ceiling* rather than a lucky quantile: between two 80 s windows of one film
the **median** of `delivered/declared` moves 4.3× while the **max** stays inside `[0.77, 0.85]`.

So, for rungs at or above **4000** kbps:

```
bytes_j  ≤  σ · W_j · D / 8000            σ, W in bit/s, D in ms, bytes in bytes
```

**`σ = 0.90`, and the margin is derived rather than picked.** Five item-windows now reach the same
rungs, which makes the *cross-item* spread of the max measurable: at rungs ≥ 4000 an unseen item
moved it by at most **5.6%** (at 2000 by 13.4%, at 720 by 22.7% — the floor regime is not merely
higher but far more item-dependent, which is what makes a shared constant wrong there
specifically). So `σ = max_observed × spread = 0.846 × 1.056 = 0.893`, both factors (2) and the
product (3). Shipping the observed max of 0.8456 as "0.85" leaves **0.5%**, which is a rounding
artefact of the measurement that produced it and not a safety margin.

**This is a seed, not a guarantee, and §2a is why that is survivable.** The spread is five windows
on one server; it bounds a sixth item only under an exchangeability assumption that rung 2000 has
already been seen to violate at the pooled level. Two things contain the damage. `σ` is verified
against every fetched segment at run time — the app already logs `bytes=`. And **it is needed only
on the upshift path**: §2a's transfer bound needs no `b_j` at all when the byte count falls, so the
three floor-regime rungs, which are downshift targets, never consume it.

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

**[SUPERSEDED — and the last bullet is why this paragraph was worth writing.]** The four bullets
above describe the corpus as it stood before the episode was run through the full ladder. That run
has happened, it was the remedy §8 named, and it **refuted the bound at exactly the rung this
analysis flagged as thinnest**: rung 2000 was one item and 80 segments, the episode was the item
that broke 720 and was absent from 2000, and putting it there put 9 of 40 segments above `0.85·W`.

So the honest verdict on this paragraph reverses. It closed with "none of that moves `σ`", which
was true of the corpus that produced it; the correct reading was that an evidence base this uneven
cannot support a claim about the rung it does not cover. The attack arguing `σ` is merely an
extreme order statistic whose expectation grows like `ln n` remains **tested and killed**
(`p3-spec-review.md`) — `σ` did not die of sampling, it died of an unmeasured item. Current
numbers are at the head of this section; the mechanism is `p2h` §2/§2a.

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

### [RESOLVED] `W_j` at selection time is the REQUESTED rate, and that is structural

The availability split above is real: the declared rate for a candidate rung is not known when the
rung is chosen, because the master playlist is fetched only after `control.prime()` has created an
encoder session (`ff.rs:3466`). §8 posed the remedy as a choice — "memoise per rung, or probe
`/decision` per candidate" — and both are worse than the answer.

**The parameter the app sends is `maxVideoBitrate` (`plex/transcoder.rs:268`), which is a CAP.** A
server cannot answer a rate-capped request with a higher declared rate and still be answering it,
so

```
W_j  ≤  R_j          R_j = the rung's REQUEST rate, known at selection time by definition
```

is classification **(1)**, physical/structural — a property of the protocol, not of this server.
It costs nothing, needs no memoisation and adds no round trip, because `R_j` *is* the thing being
chosen.

**Measured, it holds with room and the slack is informative.** Across 49 rung-item pairs the ratio
`declared/requested` never exceeded **0.951**:

| rungs | `declared / requested` | spread across items |
|---|---|---|
| 8000 – 16000 | 0.938 – 0.951 | ≤ **1.006** — effectively a constant 0.945 |
| 320, 4000, 6000, 22000 | 0.873 – 0.950 | ≤ 1.081 |
| 18000, 20000 | 0.807 – 0.951 | 1.148 — the two-rungs-one-session degeneracy of §6 |
| **720, 2000** | **0.590 – 0.915** | **1.287, 1.254** |

The bottom of the ladder is item-dependent again, and in the same place and for the same reason as
`σ`: below a certain rate target the encoder's quality floor decides the outcome, not the request.
So the two loosest inputs to the admission rule are loose *together*, on the same three rungs, and
those three rungs are downshift targets that §2a's asymmetry already exempts.

**A memoised `W_j` is strictly better where it exists and is a refinement, not a prerequisite.**
Once a rung has been visited the transaction has logged its real declared rate
(`hls: master one-variant bandwidth=`), which is tighter than `R_j` by the 5–40% above. Use it when
present; fall back to `R_j` otherwise. The fallback is what makes the *first* upshift to an unvisited
rung possible at all, which neither of §8's two options offered.

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

### [RESOLVED] The rule now admits on the average and survives the peak from the reserve

The blocker recorded here was that `A_j ≤ D` charged **per segment at the worst case** is stateless
in `B`. The reserve is the only physical reason a buffered player may run a rung whose *peak*
exceeds the link, and it appeared nowhere on the admission side. Since `W_j` is a target **average**
and `σ·W_j` a **peak**, the rule demanded the link carry the peak continuously — strictly stronger
than sustainability, and measurably so: at high rungs on easy content the *median* delivered rate
is 0.14–0.26 of declared.

**Two overstatements from the seat that raised it stay withdrawn** on the refuter's recomputation
and should not be repeated: "settles three ladder steps low" matches neither measured window, and
the 2.80× is **not** deliverable headroom being wasted — the same film's 40-min window aggregates
12 674 kbps at rung 22000 against that link, so a rule that spent it would guarantee a collapse at
the difficulty change.

§2a supplies what was missing. The transferred window is a *distribution*, not a single worst case,
so the two halves the finding asked for can be written separately against it:

```
admit(j)  ⟺   Σ_i T_i(b_j)  ≤  n · D                     (1) sustainability, on the AVERAGE
          ∧   B  ≥  Σ_i ( T_i(b_j) − D )⁺                (2) the reserve covers the excess
```

where `T_i(b_j)` is §2a's transferred value and `x⁺ = max(0, x)`.

**(1) is the sustainability condition and it is exact.** The reserve moves by `D − A` per segment —
one segment buys `D` of media and spends `A` of wall time — so `ΣA ≤ nD` is precisely "this rung
does not drain the buffer over the window", with no margin, no discount and nothing chosen. It
replaces the peak test with the mean test the finding asked for.

**(2) is the survivability condition.** Segments that exceed `D` drain the reserve by their excess;
summing every excess in the window and requiring `B` to cover it asks whether the reserve absorbs
the whole tail *at once*. Summing rather than taking the observed maximum drawdown is deliberate
and is the conservative choice: under exchangeability the ORDER of the window carries no
information, so the worst ordering — every hard segment consecutive — is the only one that can be
assumed.

**What (2) proves, and the boundary it does not cross.** It proves survival for **the span of the
evidence**, `n·D` of media, and not one segment further. A passage harder than the window is long
is not covered by it. That is sound only because the controller re-evaluates every segment, so it
never needs to survive longer than the interval to the next decision — but it is a real limit and
it is the reason §5's re-evaluation cadence is not a free parameter.

**Measured effect, against the same corpus** (`tools/abr-transfer-bound.py`): the pair is uniformly
more permissive than the order statistic alone, and most where it should be. On the easiest state
in the corpus — `slow_start_then_fast`, `A/D = 0.10` — the order statistic alone admits a byte
ratio of only **1.16**, because `k = 1` is the window maximum and one slow-start observation
dominates it; the pair admits **2.58**. On `oscillating_link` at `A/D = 0.60` with 25 s of reserve
it moves 1.00 → 2.98, which is the reserve doing exactly the job the finding said was missing.

| case | median `A/D` | median `B` | order statistic alone | (1) ∧ (2) |
|---|---:|---:|---:|---:|
| `slow_start_then_fast` | 0.10 | 50 126 ms | 1.16 | **2.58** |
| `oscillating_link` | 0.60 | 25 293 ms | 1.00 | **2.98** |
| `pin_4000` | 0.35 | 18 418 ms | 2.15 | 2.71 |
| the five steady pins | 0.36 | ~4 900 ms | 2.66 – 2.74 | 2.78 – 2.80 |
| `brief_dropout` | 0.65 | 6 335 ms | 1.16 | 1.51 |
| `steady_modest_link` | 0.59 | 37 210 ms | 1.31 | 1.77 |

The ladder's own steps are ~1.4×, so a healthy link clears a one-rung upshift with room to spare
and a loaded one (`A/D` ≈ 0.6) admits between 1.5 and 1.8 — a single rung, not three. **This is the
finding that "climbing is unreachable" being closed with a number rather than an argument.**

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

## 4a. [TESTED, NEGATIVE] A stall probability is still not constructible

§7 keeps `risk_weight` "until something produces a probability". §4 looked like it might: an
ε-level admission with a reserve condition is closer to a probability than anything the plan had.
It does not get there, and the route is recorded because a tested dead end is worth more than an
untried one.

**The construction.** The reserve moves by `D − A` per segment, so the accumulated drain over `m`
segments is `Σ(A_i − D)⁺` and Markov's inequality gives a genuine bound with no free parameter:

```
P( stall within m segments )  ≤  m · E[(T − D)⁺] / B
```

`E[(T−D)⁺]` is the mean transferred excess over the window — already computed for §4's condition
(2) — so this costs nothing extra and is (3), mathematically derived.

**It is valid and it is vacuous.** Measured on the band sweeps, which are the only samples that
have ever had a non-zero excess term at all, it **saturates at certainty** (1000 pm) at the worst
moments of both runs — and neither run stalled. A bound that says "certain" where nothing happened
carries no information.

The reason is structural rather than a matter of tuning. Markov assumes the current excess rate
persists for the whole horizon, and at `benefit_horizon_ms = 120 000` that is 60 segments. Both
runs recovered instead, because their last leg returns the link to full speed. Knowing that
requires a model of how the link EVOLVES, which is exactly what R6 killed: four events, dispersion
index 10.8 against Poisson, a 95% CI of width 0.6 on `P(revert)`.

**Where it stops being vacuous is itself the finding:**

| case | worst `B` | worst excess | m=3 | m=5 | m=10 | m=20 | m=60 |
|---|---:|---:|---:|---:|---:|---:|---:|
| `band_20000` | 2 210 | 572 | 0.78 | 1.00 | 1.00 | 1.00 | 1.00 |
| `band_4000` | 16 960 | 282 | 0.05 | 0.08 | 0.17 | 0.33 | 1.00 |

The horizon over which a stall probability can be constructed is set by `B / excess`, not by the
product's benefit horizon — and `B / excess` is a **time to empty**, in segments. Which is
`starvation_horizon`, already in `abr/plant.rs`, already the input to the risk score.

**So the conclusion is that the risk term's natural unit is a horizon and not a probability**, and
`risk_weight` is the price of a horizon in quality points. That is a product choice (4) and it
stays. What §7's table should stop claiming is that a probability is one derivation away; it is one
*link model* away, and that model is not identifiable from anything this project can measure.

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

### [RESOLVED] The periodic review has no cadence parameter, and its precondition is derived

The third trigger was unspecified — not its cadence, not its reserve precondition, not its
interaction with the anti-flap cost. §2a and §4 between them remove the need to choose any of the
three.

**Cadence: every segment.** There is nothing to schedule. The admission rule of §4 is a sum and a
selection over a window of `n ≤ 32`, evaluated where the controller already runs once per segment,
so a cadence parameter would only be a way of doing *less often* something that costs nothing. The
window needs no reset across a **commit**: §2a transfers by **bytes**, so observations taken at the
old rung remain valid evidence about the new one — which is the property R3 identified and the
reason the τ-form was worth keeping.

**[CORRECTED 2026-08-27] That is a claim about a RUNG change and it does not extend to a LINK
change, which is what the band sweeps measured.** Across a shifting link the window describes a
link that no longer exists and the ε guarantee runs ~2× loose (§2a). A rung commit remains
untested — both band cases pin the rung — so the sentence above is still unrefuted in its own
scope and was over-read the moment it was written down without one.

**Reserve precondition: self-consistency after the transaction.** An upshift is admissible when the
rung would still be admissible *having paid for the move*:

```
admit_up(j)  ⟺  admit(j) evaluated with B replaced by ( B − E_tx_up )
```

No cooldown, no `stable_samples` counter, no hysteresis constant. A marginal upshift that would
leave the reserve unable to sustain the rung it just bought is refused **by the same rule that
admitted it**, which is what a cooldown was approximating. And because §4 (2) charges the *measured*
excess rather than a multiple of `D`, the precondition is cheap exactly when it should be: on a
healthy link no segment exceeds `D`, so `Σ(T−D)⁺ = 0` and the term vanishes.

**This is also what dissolves R2.** "The top of the ladder is unreachable for ANY guard of this
shape" was true of guards that are `Ω(D)` **by construction** — the shipped `buffered ≥ 3·segment`,
the plan's successor, and every variant — because `B_max ∝ 1/R` crosses a constant multiple of `D`
at ~15.7 Mbit/s of media. §4's condition (2) is not such a guard: it is `Ω(observed excess)`, which
is **zero** on a link that is keeping up. Measured against the top-rung corpus, the rule admits
**35 of 35** states at rung 20000 even after subtracting the full measured upshift transaction
(`decided` 2 933 + `feed` 1 865 = 4 798 ms), leaving 623 ms of reserve — because the summed excess
over the window is 0 ms.

**So Phase 0's plant sizing is no longer load-bearing and must not be shipped on R2's authority.**
The sweep recommended `AQ_VIDEO_BYTES` 8 → 10 MiB plus the graded-reject feed to buy +572 ms of
ceiling. That was bought to clear a guard this specification no longer contains. The levers may
still be worth having for other reasons — a bigger reserve absorbs a longer hard passage, which is
exactly what §4 (2) prices — but "the ladder top is otherwise unreachable" is no longer one of them.

**What is NOT closed by any of this**, and what the `pipe_abr_band_*` cases exist to settle: every
number above is measured on links that were comfortably fast, where `A/D ≈ 0.35` and the excess
term is identically zero. The rule's *refusals* are therefore untested. The one state in the corpus
that resembles a marginal link — `oscillating_link`, `A/D = 0.60` with 25 s of reserve — is admitted
a **2.98×** byte ratio by (1) ∧ (2), which is either the reserve correctly doing its job or the
condition being too permissive when the reserve is large and the link is unreliable. **The corpus
cannot tell those apart**, because `A/D ∈ [0.80, 1.05]` is 0 of 366 samples. That is the band the
sweeps enter.

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

### [RESOLVED for the numeraire] Quality is octaves below source, and R5's inversion goes with it

§7's audit covered 18 struct fields and not the ≥51 quantities that decide, and the omitted ones
are inside the utility sum — starting with `hls_quality_score`'s bucket table
(`abr/mode.rs:127-135`), which is **the unit every other term is denominated in**. Nine values and
eight boundaries, none classified. This answers that table and the ledger it feeds; the two risk
ladders are scoped below and are not answered here.

**The table's own doc comment states the right principle and the numbers do not obey it.** It
argues concavity — "2 to 4 Mbit/s is a transformation of the picture, 18 to 20 is not" — which is
the statement that equal *ratios* are equally visible, i.e. that quality is logarithmic in rate.
That is also what rate-distortion theory says. A logarithmic score has a constant number of points
per octave. This one does not:

| from | to | Δ score | Δ octave | points/octave |
|---:|---:|---:|---:|---:|
| 500 | 1 000 | 10 | 1.000 | 10.0 |
| 1 000 | 2 500 | 15 | 1.322 | 11.3 |
| 2 500 | 5 000 | 15 | 1.000 | 15.0 |
| 5 000 | 7 000 | 10 | 0.485 | **20.6** |
| 7 000 | 9 000 | 8 | 0.363 | **22.1** |
| 9 000 | 13 000 | 8 | 0.531 | 15.1 |
| 13 000 | 17 000 | 6 | 0.387 | 15.5 |

It runs 10.0 → 22.1 → 15.5, so the table is an ad-hoc curve wearing a concavity argument.

**The replacement is one line and it deletes seventeen numbers:**

```
Q(R)  =  K · log₂( min(R, S) / S )          S = the source's own rate; Q ≤ 0 always
```

* `min(R, S)` is the **information bound**, classification (1): a transcode cannot carry more than
  its source, so paying for rate above `S` buys nothing. This is exactly R5's defect —
  "quality step on a bitrate ladder does not respect `transcode ≤ source`" — fixed by construction
  rather than by a correction term.
* `log₂` is (3), from the concavity the table already claims and rate-distortion supplies.
* **Relative to `S`**, so Original scores exactly **0** and every transcode scores negative. The
  ledger stops needing `original_quality_bonus` as a thumb on the scale; Original wins ties because
  it *is* the source, not because it is handed 40 points.
* `K` is the single surviving constant: **quality points per octave**, an explicit product choice
  (4), and the numeraire §7 said was undefined. One number, stated in a unit, instead of nine
  values whose unit was never named.

**R5's counterexample, recomputed.** An 8.5 Mbit/s 1080p source against a 20 Mbit/s transcode of
itself: `min(20000, 8500) = 8500`, so `Q = K·log₂(1) = 0` — **equal to Original**, where the shipped
ladder put the source three steps below. A 4 Mbit/s transcode of the same item scores
`K·log₂(4000/8500) = −1.087·K`, about one octave down, which is what a viewer would actually see.

**Integer form.** `log₂` to a sixteenth of an octave: `i64::ilog2` for the exponent plus a 16-entry
mantissa table, error ≤ 1/16 octave — far below the resolution at which a quality difference is a
decision. No floats, per §6.

**What this does NOT answer**, since §7's whole complaint was a scope claim that outran its audit:
Original's risk ladder (`{2,10,25,60}`, `/2`, `+20`, `×4`, `min 15` — `abr/mode.rs:181-194`), HLS's
risk ladder (`{0,1,4,12,40}`, `+20`, `+30`, the `uncertainty_pm ≥ 500` gate —
`abr/viability.rs:43-59`), and the bare `800` in `candidate_ready`. Those are ~33 further constants
and they price **risk**, not quality — and §4 has changed what risk means, since an ε-level
admission with a reserve condition replaces most of what the starvation-horizon buckets were
approximating. They should be re-derived against the new rule rather than classified against the
old one, which is work this section does not do.

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

* ~~**A second ITEM at the thin rungs, on THIS server**~~ — **DONE, and it refuted the bound.**
  Rung 2000 puts 9 of 40 segments above `0.85·W` (max 0.9175), so §3's claim now reads **rungs
  ≥ 4000**. Five item-windows also make the cross-item margin *derivable*: at rungs ≥ 4000 an
  unseen item moves max `σ` by at most 5.6%, giving `σ = 0.90` with a reason rather than 0.85 with
  0.5% of rounding slack. `docs/measurements/p2h-pms-ladder.md` §2/§2a. The original wording,
  which the earlier draft got wrong by asking only for a second *server*, was: Eight of the eleven rungs ≥ 2000 rest on one film, including
  rung **2000**, the boundary of the rule, and the item that breaks the bound one rung below it
  (the episode, 1.155 at 720) is absent from rung 2000 entirely. `tools/pms-rung-sweep.py`
  defaults to the full 13-rung ladder, so this needs no `--rungs` and no second server: it is one
  host-only command per additional item. **Do this first — it is cheaper than the second server
  and it closes the hole that a second-server run would faithfully reproduce.**
* **`σ` on a second server**, after that. Falsified by any segment above `0.85·W` at a rung ≥ 2000.
* ~~**An estimator for `O₀` and `τ`**~~ — **DISSOLVED, not built.** The coefficients are never
  separately needed: one observation bounds acquisition at any other byte count in closed form
  (§2a), tightly, over exactly the split the corpus cannot identify. Graded three ways in
  `tools/abr-transfer-bound.py`.
* **A bound for rungs 320 and 720**, where §3 does not hold.
* **The `B_after` relaxation model** from the queue's actual byte list, graded on R18's residuals.
  Still open, but **no longer blocking §4**: the reserve now enters admission as a level test
  against the transferred window (§4 (2)), which needs `B` itself and not its trajectory.
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

**All six findings that blocked Phase 4 have now been answered or tested to a conclusion**, and
none by the route the specification expected. Four dissolved, one is half-done and says which half,
one was attempted and failed. What follows is the state, not a plan:

1. ~~**§2a — an estimator for `O₀` and `τ`.**~~ **Answered by dissolution.** There is nothing to
   estimate: the transfer bound is closed-form, tight, and parameter-free apart from the SLO `ε`.
   Validated conservative on 278–487 real device acquisitions at six `(n, k)` settings.
2. ~~**§4's reserve term.**~~ **Answered.** The transferred window is a distribution, so
   sustainability (`ΣT ≤ nD`) and survivability (`B ≥ Σ(T−D)⁺`) separate cleanly. It does *not*
   need the `B_after` relaxation model, which is what made this look blocked. Measured: it turns
   "climbing unreachable" into a 2.6–2.8× admitted byte ratio on a healthy link against a ladder
   that steps 1.4×.
3. ~~**§5's periodic upshift trigger.**~~ **Answered, with no parameter.** Cadence is *every
   segment* — the rule is a sum over `n ≤ 32` evaluated where the controller already runs, and the
   window survives a commit because §2a transfers by bytes. The reserve precondition is
   self-consistency, `admit(j)` re-evaluated at `B − E_tx_up`, which is what a cooldown was
   approximating. **This also dissolves R2**: the unreachable ladder top was a property of guards
   that are `Ω(D)` by construction, and §4 (2) is `Ω(observed excess)`, which is zero on a link
   that is keeping up — 35 of 35 top-rung states admit even after paying the full 4 798 ms
   transaction. **Consequence: Phase 0's plant sizing must not ship on R2's authority.**
4. ~~**§3's selection-time rate.**~~ **Answered, and neither option was needed.** The app sends
   `maxVideoBitrate`, a CAP, so `W_j ≤ R_j` — the request rate, known at selection time by
   definition — is structural rather than measured. Measured slack: `declared/requested` never
   exceeds 0.951 over 49 rung-item pairs. A memoised declared rate is a refinement where it exists;
   the fallback is what makes a first upshift to an unvisited rung possible at all, which neither
   "memoise" nor "probe" offered.
5. **§7's real scope** — the 33 constants inside the utility sum, starting with the quality bucket
   table that is its unit of account.
6. **A probability, or `risk_weight` stays.** **Tried, and it stays** — the route was tested
   rather than assumed, and the negative result is in §4a below. `risk_weight` is a product choice
   pricing a HORIZON, which is what `starvation_horizon` already computes.

**Device session 2026-08-27 settled three of the five items in the "needs the device" list above.**
The M4 census reached its pinned rungs for the first time (see the defect below), the settled
reserve is now measured at seven rungs across an 18x range, the audio/video crossover is
**confirmed** between rungs 720 and 2000 by the fixtures' own audio rates, and the `A/D ∈ [0.80,
1.05]` band holds **18 samples** where it held 0 of 366. Full record:
`docs/measurements/p2-census.md`. Still owed by a device: `E_tx(up, reject)` at n ≈ 10 and
`E_tx_down` under collapse — the two cases the plan ranks 4th and 5th, still unwritten, and
downshifts still have no deadline at all.

**A defect in the apparatus, found while grading the above, that the next device lease must not
repeat.** Five of the seven M4 pin cases never reached their pinned rung: `pin_320`, `pin_2000`,
`pin_10000` and `pin_16000` all logged `rung=20000`, and their byte lists are identical. The
census measured the top rung five times. `PIN_MIN_RESERVE_SEGMENTS = 6` demands 12 000 ms of
reserve while `B_max(20000) ≈ 5 421 ms`, so a pin can never land *downward* from the startup rung —
and its own derivation (warm-up + prime + `candidate_ready`'s residual) is an **upshift** argument,
while `candidate_prime_budget` and `candidate_warmup_budget` both return `None` for
`Direction::Down`. The effective byte-size support of the whole device corpus is **three clips**,
not eleven, which is R7 confirmed from a direction the board did not look.

A second review is warranted once those are answered, on the same terms.
