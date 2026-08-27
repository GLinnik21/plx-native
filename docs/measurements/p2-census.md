# P2 — the M4 rung census, measured on the device for the first time

**Device session 2026-08-27.** LG 49SM9000PLA, webOS 4.10.0, `com.beb.plxnative.debug`, panel off
(`tools/tv-session.sh screen off` — a panel state, not an app state; it delivers no SDL background
event and does not suspend the buffer-feed). Pipeline tier: no Plex, no PMS, a static fixture
server on the dev Mac. Seven `pipe_abr_pin_*` cases, all `--no-early`. Scrubbed logs in
`p2-logs/` beside this file.

## 0. Why the previous census measured nothing, and how that stayed invisible

`docs/measurements/p1-transaction-anatomy.md` reports an eleven-point census. **Four of its seven
pinned rungs were never reached.** `pin_320`, `pin_2000`, `pin_10000` and `pin_16000` all ran at
`rung=20000` and logged byte lists byte-identical to `pin_20000`'s, so the census recorded the top
rung five times and read as ladder-wide coverage.

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

## 4. What this run does not answer

* **Every rung here is on a comfortably fast link.** Median `A/D` runs 0.01 to 0.37, so the
  admission rule's excess term is identically zero at every sample and the rule's *refusals* are
  untested. `docs/adaptive-playback-spec.md` §4 (2) prices exactly that term.
* **`A/D ∈ [0.80, 1.05]` is still unobserved**, as it was across all 366 prior samples. The
  `pipe_abr_band_*` cases exist for it.
* **The residual anomaly of §3**, above.
* **One server, one client profile, one television**, as always.
