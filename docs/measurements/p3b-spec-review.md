# P3b — the second adversarial review, and it found four blocking errors

**Run 2026-08-27** against `docs/adaptive-playback-spec.md` after the first review's six blockers
were answered. Five seats, each on a scoped reading budget, every finding then handed to an
independent refuter told to kill it and to default to refuted when uncertain.

**5/5 seats returned** (no partial run), 14 findings raised,
**5 survived and 9 were killed.** Four of the five survivors are blocking, and all four
are errors in work landed the same day — the transfer bound's guarantee, the admission rule's
reassurance about step size, the quality formula, and the claim that nothing but ε is chosen.

The reading discipline is why this run produced anything: the FIRST attempt at this review
returned an empty result that looked exactly like a clean review, because every seat was told to
read a 94 KB plan plus 200 KB of Rust and hit the usage limit with nothing in the journal. Seats
now get named, small file lists and are told to grep; the workflow returns `seatsRun` so a partial
run cannot masquerade as a clean one.

## Survived

### S1 — [blocking] §2a  (transfer-bound seat)

**Claim attacked.** The transferred values are a *fixed measurable function* of the pairs `(b_i, A_i)`. A fixed function of exchangeable variables is exchangeable, so the order-statistic result §4 already relies on applies unchanged: P( T_next > k-th largest of the window ) = k/(n+1) exactly

**Why it is wrong.** Both the argument and the equality are false. (i) The map is NOT fixed: it is g_{b_j}(b_i,A_i)=A_i*max(1,b_j/b_i), indexed by the byte count of the TEST observation. Swap the test point with window member m and the map applied to the whole bag becomes g_{b_m}, a different function — so the transformed vector is not exchangeable. The test point is the only element that is never inflated (its own factor is exactly 1). (ii) Because every factor is >= 1, the k-th largest transferred value is >= the k-th largest untransferred A_i, so {A_j > transferred bound} is a SUBSET of {A_j > raw k-th largest}. The correct statement is therefore P <= k/(n+1), STRICT whenever any window byte count is below b_j — which is 44.6% of in-window transfers at n=20 (p90 factor 1.195, max 48.3). This also invalidates the very next sentence: 'conservative at every setting tried, which is the evidence that the exchangeability assumption is not being strained in practice.' The conservatism is mechanically forced by the >=1 inflation and would appear under any degree of non-exchangeability; the diagnostic that actually bears on exchangeability is the untransferred order statistic, which the tool never computes. And 'n = k/eps - 1 follows from it. Nothing else is chosen' inherits the defect: with an inequality, n is a conservative sample count whose realized coverage is 3-4x tighter than the SLO it is named after — an unexplained sample count under the document's own classification rule.

**Evidence.** docs/adaptive-playback-spec.md:186-195. Recomputation using the tool's own `transfer`/`grade_order` over the identical glob (docs/measurements/p1b-logs/*.log), re-run with the factor pinned to 1 (no bytes, no model) as a control — transferred vs raw vs nominal: (10,1) 4.31% / 9.24% / 9.09%; (20,1) 1.06% / 3.98% / 4.76%; (20,2) 5.04% / 10.34% / 9.52%; (29,1) 1.08% / 4.32% / 3.33%; (29,3) 5.76% / 11.15% / 10.00%; (40,2) 2.53% / 5.06% / 4.88%. The control sits at nominal at all six settings; the transferred grade is 2-4x under. Binomial against the claimed EXACT rate: P(<=4 exceedances in 377 | p=1/21) = 6.7e-5, so the spec's own corpus rejects the equality at (20,1).

### S2 — [major] §2a  (transfer-bound seat)

**Claim attacked.** **The cause is not ambiguous — all 6 violations fall within one window of a link-rate step.** The window straddles a leg boundary and carries observations from a link that no longer exists, so exchangeability fails by construction rather than by degree.

**Why it is wrong.** The predicate is satisfied by 100% of the samples, violating and non-violating alike, so it discriminates nothing and cannot be evidence for a cause. The band cases shape the link in 20-second legs, while a 20-sample window spans 27-41 s of wall time in these logs — every window straddles at least one leg boundary, and no gradable sample in either log is more than 10 s (~5 segments) from one. The experiment contains no control leg, so 'not ambiguous' is unfalsifiable here rather than established. The violation shape argues against the diagnosis as well: only 2 of the 6 sit at/next to a step, and 4 of the 6 are marginal mid-leg overshoots of 1.005-1.10x, i.e. exactly the 'dispersion around the model' the section itself classifies as noise for the 1.05-1.06 pin cases. A 2.69x overshoot at one step plus five near-ties is not 'exchangeability fails by construction'.

**Evidence.** docs/adaptive-playback-spec.md:218-220; leg schedule at tests/manifest.json:1781-1786 and 1811-1816 (`network_profile` until_s 25/45/65/85, i.e. 20 s legs). Recomputation over docs/measurements/p2-logs/pipe_abr_band_{4000,20000}.log at (20,1), mapping segment index to wall time via the `abr: steady ... left=` countdown: 63/63 gradable windows contain a leg boundary (100%), window wall span 27 s and 41 s. The six violations, with overshoot A/bound: band_4000 seg21 t=27 s 2763/1029=2.69; seg37 t=60 s 3465/3161=1.10; band_20000 seg23 t=45 s 2488/2319=1.07; seg31 t=62 s 1.06; seg32 t=64 s 1.03; seg35 t=70 s 2889/2874=1.005.

### S3 — [blocking] §4  (admission seat)

**Claim attacked.** The ladder's own steps are ~1.4×, so a healthy link clears a one-rung upshift with room to spare and a loaded one (`A/D` ≈ 0.6) admits between 1.5 and 1.8 — a single rung, not three.

**Why it is wrong.** Both halves are false against the repository. (a) No step in the shipped ladder is 1.4×. `Rung::kbps` above 4000 is 4000→6000→8000→10000→12000→14000→16000→18000→20000→22000, i.e. ratios 1.500, 1.333, 1.250, 1.200, 1.167, 1.143, 1.125, 1.111, 1.100 (the measured `expected_wire_kbps` points are identical up to 20 011 / 20 895, giving a final step of 1.044). Geometric mean per step from 4000 up is 1.23, not 1.4. (b) The table directly above the sentence lists three rows near A/D ≈ 0.6 — `oscillating_link` 0.60 → **2.98**, `steady_modest_link` 0.59 → 1.77, `brief_dropout` 0.65 → 1.51 — so the range is 1.51–2.98, and the paragraph two lines earlier singles out the 2.98 case as the rule working correctly. Substituting the real ladder: from the 8000 rung, 1.77 lands on 14 000 (three rungs) and 2.98 lands on 20 895, the top of the ladder (seven rungs, and a raster change to 4K). The 'single rung, not three' reassurance is therefore off by 3–7×, and it cannot be recovered by rung-walking because `best_sustainable` is explicitly documented and coded to jump straight to the largest admissible candidate. This is the sentence the section rests its 'climbing is unreachable is closed with a number' conclusion on; the number says the opposite.

**Evidence.** rust-modules/src/abr/ladder.rs:95-108 (`Rung::kbps`) and :241-253 (measured `expected_wire_kbps` points); computed step ratios [2.25, 2.778, 2.0, 1.5, 1.333, 1.25, 1.2, 1.167, 1.143, 1.125, 1.112, 1.044]; 8000 × 2.98 = 23 840 → clamps to the 20 895 UHD point, 7 rungs up; ladder.rs:346-351 doc ('a jump from 8 Mbps to a 15 Mbit/s budget primes the 14 Mbps encoder once instead of walking 10, 12, 14')

### S4 — [blocking] §7 — "Quality is octaves below source"  (quality-risk seat)

**Claim attacked.** "`min(R, S)` is the **information bound**, classification (1): a transcode cannot carry more than its source, so paying for rate above `S` buys nothing. This is exactly R5's defect … fixed by construction" — and "Relative to `S`, so Original scores exactly **0** … Original wins ties because it *is* the source, not because it is handed 40 points."

**Why it is wrong.** The information bound is an INEQUALITY and the formula turns it into an EQUALITY. "A transcode cannot carry more than its source" gives Q(transcode) ≤ 0; it does not give Q = 0. A transcode is a re-encode of already-lossy content, so generation loss makes it strictly worse at every finite R, and on this app's path it can also lose the source's DV/Atmos entirely — which §7's own `original_feature_bonus` row concedes ("DV/Atmos must be represented explicitly, not through a bitrate proxy"). So R5's inversion is not fixed by construction, it is reduced from "transcode wins by three steps" to "transcode ties". That tie is decision-relevant, not cosmetic: with Q_orig = Q_hls = 0, `original_utility`'s total is `0 + features − risk − transition` and `hls_utility`'s is `0 − risk − server − transition`, so the entire Original-vs-top-rung decision is now carried by `server_cost_weight` (§7: "re-measure … the premium is off by 6.6×, and it is the entire justification for the weight") and `original_feature_bonus` (§7: "Open") — two constants this same document leaves unresolved. Whenever the link makes Original's risk exceed the transcode's (Original is scored against `source_requirement_kbps`, i.e. the source rate plus `vbr_allowance_pm`, so its horizon is systematically the shorter one), the ledger now picks a strictly worse artefact over the source, which the shipped +40 prevented. Second defect in the same line: `log₂(min(R,S)/S)` presumes R and S lie on ONE rate-distortion curve. PMS transcodes change codec and resolution (the target is a fallback chain hevc,h264 with downscaling), so for a 4K HEVC source at S = 25 000 transcoded to 1080p H.264 at R = 20 000 the formula returns −0.32·K — "a third of an octave" — for a 4× pixel reduction plus a codec change. Rate-ratio-is-quality across a resolution change is neither structural (1) nor measured (2); it is the same unclassified assumption the bucket table was making, restated in closed form.

**Evidence.** docs/adaptive-playback-spec.md:798-807 (the four bullets and the recomputed R5 counterexample); rust-modules/src/abr/mode.rs:196 (Original's quality is `original_quality_bonus + hls_quality_score(P1080High)`), :158-164 (HLS total subtracts `server`), :204-210 (Original total has `server: 0` and adds `features`), :175 (`source_requirement_kbps(inputs.source_kbps, policy)` is what Original's horizon is scored against); spec table rows `server_cost_weight` and `original_feature_bonus` at docs/adaptive-playback-spec.md:754-755.

### S5 — [blocking] §2a / 5  (numerics seat)

**Claim attacked.** `ε = k/(n+1)` is therefore (4), an explicit SLO choice, and `n = k/ε − 1` follows from it (R28's corrected theorem — `k/ε − 1`, not `1/ε − 1`). Nothing else is chosen.

**Why it is wrong.** ε pins only the RATIO k/(n+1); the pair (n,k) has two degrees of freedom, so k is a second free parameter that is chosen and is classified nowhere as (1)-(4). It is not a neutral choice: it sets the window length n = k/ε − 1, which is (a) the estimator's exposure to non-stationarity — the failure mode §2a itself measures, where 'all 6 violations fall within one window of a link-rate step' — and (b) via §4 condition (2), the span of media the reserve condition proves survival over, stated in the document as exactly `n·D`. The spec's own table proves the choice moves behaviour at fixed ε: (n=20,k=1) has nominal ε 4.76% and observed exceedance 1.06%, while (n=40,k=2) has essentially the same nominal ε (4.88%) and observed 2.53% — 2.4× the exceedance, with 80 s of coverage instead of 40 s. Worse, §5 introduces a second unclassified quantity that silently constrains the first: 'a window of `n ≤ 32`' appears once, in passing, with no derivation. At n ≤ 32 the achievable ε is bounded BELOW by k/33, so k=2 forces ε ≥ 6.06% and k=3 forces ε ≥ 9.09% — the cap forecloses three of the six settings §2a measured, including the (40,2) row it quotes as evidence. The document therefore ships an SLO whose attainable range is set by an unexplained sample count, which is the exact shape the design rule bans.

**Evidence.** docs/adaptive-playback-spec.md:195 ('Nothing else is chosen') vs :629 ('a window of `n ≤ 32`'); the (n,k) table at :198-207. Recomputation: k/ε − 1 at ε=0.0488, k=2 gives n = 39.98 ≈ 40 > 32, so the (40,2) setting measured at :205 is unreachable under §5's own cap; and ε ≥ k/33 for every k under that cap.

## Killed — do not re-raise

**K1 §2a (transfer-bound).** The exceedance is not a property of `ε`; it is the window describing a link that has changed, and a smaller `ε` buys nothing against a non-stationary process.

Refuted: VERIFIED THE EVIDENCE FIRST. All six of the seat's numbers reproduce exactly from `python3 tools/abr-transfer-bound.py --logs 'docs/measurements/p2-logs/pipe_abr_band_*.log' --window N --k K --grade order`: (20,3) 24.62%/14.286% [1.72]; (20,2) 16.92%/9.524% [1.78]; (20,1) 9.23%/4.762% [1.94] on 65 gradable samples; (29,3) 14.89%/10% [1.49]; (29,2) 12.77%/6.667% [1.92]; (29,1) 8.51%/3.333% [2.55] on 47. The spec's three quoted rows at /Users/gleblinnik/Developer/plex/plex-native-poc/.claude/worktrees/bridge-cse_01R2JV4ZysHtYjfRKUdRGTKi/docs/adaptive-playback-spec.md:215-217 are also correct. So the seat's arithmetic is sound; its MODEL is not, and the same tool kills it.

1. "The observed/nominal ratio is pinned at 1.5-1.9 across the whole sweep" is false, and false on the seat's own numbers. It computed (29,1) = 2.55 and then wrote a range excluding it. Extending the sweep to k=1..5 at n

**K2 §4 (admission).** **(1) is the sustainability condition and it is exact.** The reserve moves by `D − A` per segment [...] so `ΣA ≤ nD` is precisely "this rung does not drain the buffer over the window", with no margin,

Refuted: Every line the seat cites is real (ladder.rs:372 max_by_key; controller.rs:314 `safe_budget * 4 / 5`, :345 `stable_samples < 3`, :372 `<= 800`, :387 `Direction::Down => 8`), but the argument built on them fails six ways, four of them independently fatal.

(1) SELF-CONTRADICTION. Admission is a conjunction of (1) and (2); a max over the conjunction is tight in at most one of them generically. The seat claims BOTH that the loop settles where (1) is tight (E[D−A]=0) and that on `oscillating_link` "(2) is the binding constraint". If (2) binds then (1) is slack — ΣT < nD — which is strictly positive drift, i.e. the seat's own boundary example refutes its own mechanism. If instead (1) is tight, then Σ(T−D)⁺ < B strictly and the worst-ordering drawdown never reaches 0. Both tight at once is a codimension-2 coincidence.

(2) DISCRETE LADDER. `best_sustainable` calls `self.feasible()` (a `.filter

**K3 §4a (admission).** **It is valid and it is vacuous.** [...] A bound that says "certain" where nothing happened carries no information. [...] The horizon over which a stall probability can be constructed is set by `B / e

Refuted: The seat's load-bearing evidentiary claim — that the §4a table substitutes a maximum for the mean E[(T−D)⁺] and that this "is what drives it to 1" — is false against the underlying data, and its other two claims are already stated in the section it attacks.

(1) MAX-FOR-MEAN IS REFUTED BY THE SOURCE LOGS. Reconstructing per-segment excess from docs/measurements/p2-logs/pipe_abr_band_20000.log and pipe_abr_band_4000.log as (prod_pm − 1000)⁺ · dur/1000 (prod_pm = 1000·A/D; dur = 2000 ms): band_20000's per-segment excess maximum is 888 ms and band_4000's is 1464 ms. The published "worst excess" values are 572 and 282 — far BELOW those maxima, so no maximum was substituted. They match trailing-WINDOW MEANS at the worst moment: band_20000's k=12 rolling mean peaks at 573.3 ms with buf = 2251/2210 (published worst B = 2210), and band_4000's k=10 rolling mean is 288.0 ms at buf = 16960 (publish

**K4 §5 (trigger-r2).** §4's condition (2) is not such a guard: it is `Ω(observed excess)`, which is **zero** on a link that is keeping up. […] the rule admits **35 of 35** states at rung 20000 […] because the summed excess 

Refuted: Reproduced the seat's arithmetic exactly from docs/measurements/p2-logs/pipe_abr_band_20000.log (49 samples, all current=20000kbps dur=2000ms): prod 385-1444 pm, 24/49 >1000; buf min 2000 / median 3751 / max 5210; Sigma(T-D)+ peak trailing-32 = 9414 ms (sample 39), peak any-20 = 8576; fails 23/49 in situ, 17/49 at B=5421, 13/49 while trailing-32 mean prod <= 1000. prod confirmed as 1000*A/D at estimate.rs:313-317. The numbers are right; the claim is not, for three reasons.

(1) The spec already measures what the seat says it never entered. Section 4a — which the seat did not cite and evidently did not read — says outright "Measured on the band sweeps, which are the only samples that have ever had a non-zero excess term at all", and tabulates band_20000 at worst B 2210 ms, worst excess 572 ms. Section 5's own next paragraph names pipe_abr_band_* as the cases that exist to settle the untes

**K5 §5 (trigger-r2).** A marginal upshift that would leave the reserve unable to sustain the rung it just bought is refused **by the same rule that admitted it**, which is what a cooldown was approximating. […] admit_up(j) 

Refuted: The finding rests on three legs and all three fail against the cited evidence.

(1) THE 623 ms PREMISE IS CONTRADICTED BY THE REPOSITORY'S OWN MEASUREMENT OF THE SAME TRANSACTION. The seat treats "623 ms of post-transaction reserve" as the state the rule admits. It is not a measured state; it is 5 421 − 4 798 = 623 to the digit, i.e. B_max(20000) minus the gross wall clock of both transaction legs. docs/measurements/p1-transaction-anatomy.md §2 and §3 establish that this over-charges by construction: "buf_fed = buf_decided + n*D - feed_ms is an IDENTITY" ("conservation, not a model", drained/feed ratio 1.00–1.07, n=5), and p3-spec-review.md:238 cites the decided-leg twin "buf_decided = buf_start − decided_ms + n·D" as validated 7/7 with median error 26 ms. Charging feed_ms as pure reserve loss discards the n·D of staged media the feed leg delivers. The device measured the post-transactio

**K6 §7a — "The loose adders" (quality-risk).** "`+20` if `production_risk` / `samples == 0` | `viability.rs`, `mode.rs` | **Delete — it is the estimator's job.** "No measurement of this request" is low `n`, and `ε = k/(n+1)` already widens the bou

Refuted: Verified all cited lines directly. The seat's CODE reading is correct — plant.rs:107-109 returns None at samples==0, so viability.rs:40-44's production_risk (predicted.is_some_and(...)) can never fire on "no measurement", and only mode.rs:187 is genuinely n-based. But the finding dies on three independent legs, all in spec text the seat did not read.

(1) The seat's central mechanism claim is false. It says "ε = k/(n+1) — a quantile on the DELIVERY/capacity bound — does not widen with server load at all." §2:83 defines A = O₀ + bytes·τ as TOTAL ACQUISITION, and §2's regime table makes O₀'s server term the PMS production cost explicitly (quantized 108 ms steady-state; ≈660 ms after a seek; rising to 862 ms median / 1306 ms max for "source the encoder does not handle"). §2a:191 takes the order statistic on T_next, the transferred acquisition, and §7:747 states it outright: "Under §4 the ma

**K7 §7a — "Above the constraint, risk is a price" (quality-risk).** "`K_r` is quality points per **halving** of the safety horizon — one constant, in a stated unit, commensurable with `Q` by construction because both are octaves." and "**And `risk_weight` disappears i

Refuted: All three legs fail against the cited lines.

(1) The seat's physics is right and irrelevant: the spec never adds two bare logarithms. Line 875-876 states K_r's unit as "quality points per halving of the safety horizon", which IS the price the seat says is needed to make the addition legal. The clause "commensurable with Q by construction because both are octaves" does not claim log(times) equals log(rates); it claims the risk term takes the quality scale's form, so its coefficient is "points per octave" like K, rather than "points per row of an unnamed nine-value table" like the ladder it replaces. The seat's proposed remedy is the sentence it attacks.

(2) "risk_weight has been reparameterised, not deleted" is the document's own claim, not a defect it concedes under pressure. Lines 883-885: "They were always the same quantity — the price of risk in quality points — split across a coeff

**K8 §3 (numerics).** **`σ = 0.90`, and the margin is derived rather than picked.** … So `σ = max_observed × spread = 0.846 × 1.056 = 0.893`, both factors (2) and the product (3).

Refuted: I recomputed the two factors from the raw per-segment CSVs in docs/measurements/p2h-logs/ (s = (bytes*8/duration_s)/declared_bps, per-item max per rung). The spec's table reproduces exactly: pooled max over rungs >=4000 is 0.84563 (movie-opening at rung 4000), the largest cross-item spread (max_i/min_i of per-item maxima) at rungs >=4000 is 1.05562 (rung 12000), and the product is 0.89266. Both factors are directly measured and the multiplication is correct.

The seat's central mathematical assertion is false: "multiplying a sample maximum by the sample range does not change the exceedance probability for a new exchangeable item at all". Since {S6 > 1.056*M5} is a strict subset of {S6 > M5}, the exceedance probability is strictly smaller for any non-degenerate distribution. What is true is only that the distribution-free BOUND remains 1/6 - and the spec never claims a probabilistic guara

**K9 §4 / 6 (numerics).** Every term above is `i64`, and no subtraction of unsigned quantities appears in the rule.

Refuted: REFUTED on four independent grounds, each checked against the cited lines.

1. SCOPE. The attacked sentence (:538-539) is the last line of the subsection `### The shipped integer form` (:513), and it directly follows the per-term overflow table (:527-534) for exactly one expression: `1_000_000 * o0_us + bytes_worst * tau_ps <= 1_000_000_000 * d_ms`, plus `bytes_worst = (sigma_pm * declared_bps * d_ms + 7_999_999) / 8_000_000`. "Every term above" has that table as its antecedent, and those terms contain NO subtraction of any signedness. The seat reaches 78 lines back, past a subsection break, to `admit(j)` at :460-461 to find one.

2. THE CLAIM IS STILL TRUE UNDER THE SEAT'S WIDE READING. §6 :705 is a specification clause, not an aspiration: "Integer `i64` throughout. No floats anywhere in the ABR path." And :462 defines the operator in the very next line of the formula the seat attacks: 

