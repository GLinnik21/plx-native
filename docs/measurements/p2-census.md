# P2 — the M4 rung census, measured on the device for the first time

**Device session 2026-08-27.** LG 49SM9000PLA, webOS 4.10.0, `com.beb.plxnative.debug`, panel off
(`tools/tv-session.sh screen off` — a panel state, not an app state; it delivers no SDL background
event and does not suspend the buffer-feed). Pipeline tier: no Plex, no PMS, a static fixture
server on the dev Mac. Seven `pipe_abr_pin_*` cases, all `--no-early`. Scrubbed logs in
`p2-logs/` beside this file.

## 0. Why the previous census measured nothing, and how that stayed invisible

`docs/measurements/p1-transaction-anatomy.md` reports an eleven-point census. **Four of its seven
pinned rungs were never reached**, and in the corpus before it, five.

* **P1b** (post-fixture-rebuild, the corpus every fit in that document is computed from):
  `pin_320`, `pin_2000`, `pin_10000` and `pin_16000` all ran at `rung=20000` and logged byte lists
  byte-identical to `pin_20000`'s, so the census recorded the top rung **five times** and read as
  ladder-wide coverage.
* **P1**: the same four, plus `pin_4000`, which ran its whole 124 samples at `current=6000kbps`.
  It is not a fifth instance of the same failure — rungs 2000 and 4000 shared one clip until the
  fixture rebuild gave 4000 its own, so the pin landed on the neighbouring rung the shared clip
  actually served. The rebuild fixed that half; the reserve gate below is what was left.

**The count therefore differs by corpus and both numbers are correct.** Anywhere this repository
says "five of seven" it means P1; this document is P1b.

`PIN_MIN_RESERVE_SEGMENTS = 6` demands 12 000 ms of reserve before the pin will transact. The
reachable reserve at the ladder top is ~5 421 ms. So a pin could never transact **downward** from
the top rung — and on an unshaped LAN `startup_rung` picks exactly that rung, which makes every
pin in the census a downshift. The constant's own derivation is an upshift argument end to end
(`candidate_warmup_budget` + `candidate_prime_budget` + `candidate_ready`'s residual), and both of
those budgets return `None` for `Direction::Down`.

Nothing failed. Every case passed, every metric was plausible, and the corpus's real byte-size
support was **three distinct clips rather than eleven** — which is R7 ("the n=366 fit is ten data
points") confirmed from a direction the review board did not look at.

Fixed by splitting the constant in two and deriving the downward figure from what a downshift
actually costs: one segment for the warm-up fetch, bounded by the acquisition-transfer bound at a
current-rung segment since the candidate's byte count is lower by definition, plus one segment for
the three control-plane requests (~100 ms median, 1 306 ms max on a real PMS). Two segments —
4 000 ms — fits inside `B_max` at every rung including the top.

**Before and after, same binary except that constant:**

| case | raster before | raster now |
|---|---|---|
| `pin_320` | 58 × 1920x1080 | **82 × 426x240** |
| `pin_720` | 10 × 1280x720, 81 × 854x480 | 89 × 854x480 |
| `pin_2000` | 51 × 1920x1080 | **74 × 1280x720** |
| `pin_4000` | 67 × 1280x720 (P1b, landed) — 63 × 1920x1080 at rung 6000 in P1 | 67 × 1280x720 |
| `pin_10000` | 52 × 1920x1080 | **1920x1080, at rung 10000** |
| `pin_16000` | 49 × 1920x1080 | **1920x1080, at rung 16000** |

## 1. The settled reserve, per rung

Median over the settled half of each run, from the once-per-segment `abr: sample` line.

| rung | n | media kbps | settled `B` | `vbuf` | `abuf` |
|---:|---:|---:|---:|---:|---:|
| 320 | 83 | 380 | **88 397** | 88 397 | 88 418 |
| 720 | 90 | 800 | 67 585 | 67 585 | 67 585 |
| 2000 | 74 | 2 221 | 37 210 | 37 210 | 37 210 |
| 4000 | 66 | 4 397 | 18 418 | 18 418 | 18 418 |
| 10000 | 54 | 10 522 | 8 168 | 8 168 | 8 168 |
| 16000 | 54 | 16 530 | 5 793 | 5 793 | 5 793 |
| 20000 | 39 | 20 694 | **4 918** | 4 918 | 4 945 |

Monotone in the rung across a **18×** range, which is the `B_max ∝ 1/R` shape the plant model
predicts and which the previous census could not show at all.

**`vbuf` and `abuf` are equal at every rung, and that is structural rather than a coincidence.**
One demux worker fills both lanes from the same segment, and backpressure on *either* queue stops
it, so both lanes stop at the same media time. The observable is therefore `B` alone, and the
question a census can answer is not "which lane is deeper" but **which queue's byte cap was
reached**.

## 2. The audio/video crossover, confirmed

`abr/sim.rs`'s geometry is `B_max = min(lead + VQ/R_v, lead + AQ/R_a)` with `VQ = 8 MiB` and
`AQ = 1 MiB` (`player/engine.rs`). Audio therefore binds when `R_v/R_a < VQ/AQ = 8` — a pure ratio,
with no rate in it.

The fixtures' own audio rates, read with `ffprobe`, decide it without any fitting:

| rung | audio kbps | `AQ/R_a` | observed `B` | verdict |
|---:|---:|---:|---:|---|
| 320 | 97.97 | 85 624 | 88 397 | **audio binds** (+3.2%) |
| 720 | 131.02 | 64 027 | 67 585 | **audio binds** (+5.6%) |
| 2000 | 159.44 | 52 614 | 37 210 | observed is *below* the audio ceiling → video binds |
| ≥ 10000 | 192.19 | 43 647 | 8 168 and down | video binds |

And the ratio test agrees: rung 720 is `R_v/R_a = 5.1` (< 8, audio), rung 2000 is `13.0` (> 8,
video). **The crossover falls between rungs 720 and 2000**, which is where
`tests/test_harness.py::test_the_census_covers_both_sides_of_the_predicted_binding_crossover`
placed it when it said "~1.66 Mbit/s of wire". That test has existed since the census was written
and until this run there was no data that could satisfy the prediction it guards.

## 3. Where video binds, the residual is the feed-ahead lead — above rung 10000 only

| rung | video ES | `VQ/R_v` | observed | residual |
|---:|---:|---:|---:|---:|
| 2000 | 2 062 | 32 552 | 37 210 | **4 658** |
| 4000 | 4 238 | 15 837 | 18 418 | **2 581** |
| 10000 | 10 330 | 6 497 | 8 168 | 1 671 |
| 16000 | 16 338 | 4 108 | 5 793 | 1 685 |
| 20000 | 20 502 | 3 273 | 4 918 | 1 645 |

At rungs 10000 and above the residual is **1 645 – 1 685 ms**, flat across a 2× rate range, against
`MAX_FEED_AHEAD_NS = 1.6 s` (`player/engine.rs`). That is the lead term of the geometry, measured
rather than assumed, and it is the tightest confirmation of the plant model this project has.

**Below rung 10000 it is not flat, and that is unexplained.** 2 581 ms at rung 4000 and 4 658 ms at
rung 2000, growing as the rate falls. Three candidates, none tested: the video queue is not
actually reaching its byte cap at those rates; `media=` is a wire rate and the container overhead
it carries is not constant across fixtures; or a third limit takes over between the audio ceiling
and the video one. **Do not fit a correction to this** — it is a 2.8× spread in a term that is
otherwise a constant, so something structural is missing and a coefficient would hide it.

## 4. The unobserved band, entered — and the ε guarantee degrades in it

The two `pipe_abr_band_*` cases hold a rung while the shaper walks the link down a derived ladder
of legs. Both passed (`no_playing_error`, `min_pos_climb_s: 40`). **They put 18 samples into
`A/D ∈ [0.80, 1.05]`, a region that held 0 of 366 samples across every prior measurement**, and
both went past it into the draining regime:

| case | n | `A/D` range | samples in [0.80, 1.05] |
|---|---:|---|---:|
| `band_4000` | 56 | 0.24 – **1.73** | 15 |
| `band_20000` | 49 | 0.39 – **1.44** | 3 |

**Nothing stalled at `A/D = 1.73`**, sustained across several segments. That is the reserve
absorbing an unsustainable rung for the length of a hard passage, which is exactly what
`docs/adaptive-playback-spec.md` §4 (2) prices and what no previous run could demonstrate.

**And the ε guarantee does not hold here.** Grading §2a's order-statistic transfer bound on these
two logs alone:

| n | k | nominal ε | observed | tested |
|---:|---:|---:|---:|---:|
| 20 | 1 | 4.76% | **9.23%** | 65 |
| 20 | 2 | 9.52% | **16.92%** | 65 |
| 29 | 3 | 10.00% | **14.89%** | 47 |

About **1.9× anti-conservative**, against a corpus of settled links where the same bound ran
1.06–5.76% *under* nominal. **The cause is not ambiguous: all 6 violations fall within one window
of a link-rate step.** The window straddles the shaper's leg boundary and carries observations from
a link that no longer exists, so exchangeability fails by construction — which is the same
precondition failure the `pairs` grade documents, seen from the inside.

**A regime-change reset does not rescue it, and the reason is a number.** `CapacityEstimate`'s
existing rule resets on a **4×** move. The sweep's largest step is 20000 → 6446, which is **3.1×**,
so the reset never fires and the observed exceedance is unchanged at 9.23%. Dropping the threshold
to 2× does fire, but starves the window — 65 gradable samples become 14 — and still leaves 7.14%.
There is no stationary regime to reset *to*: the link is non-stationary by construction here, which
is what the case was built to produce.

**So the honest statement of the guarantee has a precondition, and the two-layer design is what
survives it.** Pooled over all **382** device acquisitions the bound still holds — 3.14% against a
4.76% nominal at (20, 1), and 6.64% against 10% at (29, 3). Restricted to an actively degrading
link it is ~2× loose. The reserve condition is the layer that catches that, and it did: `A/D` 1.73
with no stall. **Do not tighten ε to compensate** — the exceedance is not a property of ε, it is
the window describing a link that has changed, and a smaller ε buys nothing against a
non-stationary process.

**This refutes a claim made in the specification's §5 two commits before this run**, that "the
window needs no reset across a commit". That claim was about a RUNG commit and is still untested,
since a pin holds the rung here. What is now measured is the **link** regime, and there the window
is the problem rather than the rung.

## 5. `E_tx_down` under collapse, measured — 1 424 ms, out of a 2 209 ms reserve

`pipe_abr_down_collapse` holds the link at 500 kbps, where only rung 320 survives. It forced the
full descent and produced the first measurement of a downshift's cost:

| move | `decided` | `warmup` | `buf_start` → `buf_decided` | `cur_acq_before` |
|---|---:|---:|---:|---:|
| 10000 → 8000 | 745 ms | 738 ms | 1 920 → 1 920 | 916 ms |
| 8000 → 14000 (up) | 2 464 ms | 1 173 ms | 9 959 → 7 543 | 816 ms |
| **14000 → 320** | **1 424 ms** | 1 418 ms | **2 209 → 168** | **61 480 ms** |

**`E_tx_down` = 1 424 ms** on the collapse commit, and it is paid almost entirely by the warm-up
fetch (1 418 of 1 424) — the control plane is 6 ms, as `p2h` §5 says it is on this tier.

**The alarming column is the last one.** `cur_acq_before = 61 480 ms`: the rung the controller was
still on took **61.5 seconds** to fetch one 2-second segment before it moved. The escape then cost
1 424 ms out of a 2 209 ms reserve, leaving **168 ms** — under a tenth of a segment. It did not
stall (`no_playing_error` and `pos_climb` both passed), but it is not a margin either. §5's
deadline `B < A_i + E_tx_down` was violated by a factor of thirty before the controller acted,
which is the case for that deadline existing, measured.

It also gives §7a's `H_ref = E_tx_down + D` a value for the first time: **3 424 ms**.

## 6. A case that passed while measuring nothing, and what it took to see that

`pipe_abr_reject_up_4000` is built to produce `E_tx(up, reject)` — an upshift proposed and refused.
It **passed with every assertion green and produced zero rejections**, because it never reached
rung 4000 at all: it settled at 720/2000, and `settle_max_kbps: 4000` is satisfied by settling
*below* the rung the case is named for. The pipeline tier's standing warning about false PASSes,
in a new place.

**The first diagnosis was wrong and is recorded because the correction is the useful part.** The
obvious explanation is a ratchet: throughput measured as `bytes / A` is biased low when a segment
is small, so a low rung under-estimates the link that put it there and stays low. The arithmetic
works — at rung 720 on an 8 300 kbps link that model predicts 3 443 kbps, 41% of the truth. **It is
also refuted.** `network_kbps()` divides by `active_fetch_us`, the transfer window alone, so the
fixed cost is not in the denominator; and `clamped_to_evidence` binds only on Weak samples, while
rung 2000's 555 kB segment is Normal. Neither mechanism was operating.

What actually happened is visible in the log: the *instantaneous* observation was correct
throughout (`net` 6 426 – 7 908 kbps against a shaped 8 300), and it was `slow_kbps` — an 8-weight
EWMA — that lagged, **climbing** 4 830 → 5 031 across the window. `conservative` = 5 031 × 0.8 =
4 025 reached rung 4000's boundary exactly as the 120 s run ended. Not a measurement error, not a
ratchet: EWMA convergence from a low bootstrap, and a case one window too short.

Fixed by opening the profile at 40 000 kbps for 15 s so the estimate **descends** to 8 300 instead
of climbing to it. The lesson is in the case's own note, since it is not visible from the outside:
a flat leg at the target rate silently measures the estimator's convergence rather than the
behaviour under test.

### And the re-run found something better than the thing it was looking for

With the fast opening leg the controller reached rung 4000 as intended — and then **proposed
nothing for 62 consecutive samples**. Still zero rejections, because `E_tx(up, reject)` requires an
upshift to be *attempted*, and the shipped controller does not attempt one. The state at the end of
the run:

| quantity | value |
|---|---:|
| true shaped link | 8 300 kbps |
| `slow` (measured link) | 7 282 kbps |
| `unc` | 200 pm |
| `safe` budget | 5 825 kbps |
| **after `best_sustainable`'s `4/5` haircut** | **4 660 kbps** |
| rung actually played | **4 000 kbps** |
| reserve | 18 710 ms — **nine segments** |
| `slope` | **+33 ms/s** — filling |

Every gate in the upshift's `all_good` conjunction would have passed: the reserve is three times
the three-segment requirement, nothing is draining, production is fine. **The rung never gets
proposed at all**, because the target is chosen by `best_sustainable(safe_budget * 4/5, …)` and
4 660 kbps cannot reach rung 6000. Target resolves to the current rung, and the branch returns
`Stay` before any of the gates are consulted.

**So Auto plays 4 000 kbps on an 8 300 kbps link — 48% of capacity — with nine segments of
buffer, a rising buffer slope, and no distress signal anywhere.** This is the plan's central
complaint (stacked margins multiplying to 0.32–0.51 of the measured link) observed directly, with
numbers, on hardware, rather than derived from reading the constants.

**It also settles what `pipe_abr_reject_up_4000` is.** `E_tx(up, reject)` is not measurable against
the shipped controller in any configuration, because the transaction it prices is never started.
The case is a **Phase 4** case: it becomes productive the moment §4's admission rule replaces the
haircut, and until then its passing tells you nothing. That is worth more than the measurement it
failed to take — a case that cannot fire is evidence about the controller, not about the case.

## 7. What this run does not answer
* **Whether a rung COMMIT invalidates the transfer window**, as §4 above distinguishes from a link
  change. Both band cases pin the rung, so nothing here commits.
* **The residual anomaly of §3**, above.
* **One server, one client profile, one television**, as always.
