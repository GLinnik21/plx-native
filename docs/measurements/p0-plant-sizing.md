# Phase 0 — is the actuator ladder physically climbable?

**Host-only. No television, no lease, no controller.** Reproduce with
`tools/abr-plant-sweep.py` (add `--verbose`, `--json`, `--ladder`, `--reject-delivers`).

## Why this ran before any controller work

Two ABR upshift guards have now been written that the top of the ladder cannot satisfy: the
shipped `buffered >= 3 * segment` (6 000 ms against a 5 421 ms ceiling), and the replacement
proposed in the adaptive-playback plan. Both were derived carefully; both failed for the same
reason, and it is not a control-law reason:

```
an upshift guard is Omega(D)          it must cover a transaction
B_max(R) = lead + queue_bits / R_ES   and the ceiling falls as 1/R
```

They cross. Above the crossing rate **no guard of that shape can be satisfied**, so "which guard"
is downstream of "is this ladder climbable on this queue".

## Three ceiling conditions, not one

Earlier passes checked only the first and concluded the ladder was fine.

| condition | question |
|---|---|
| `B_max(R_j) >= up-guard(j-1 -> j)` | can the rung be **reached** from below |
| `B_max(R_j) >= D + E_tx_down(j)` | can **one wrong admission** be survived |
| `B_max(R_j) >= D` | does **one segment** fit — else `aq_push` blocks forever, a *silent hang*, not a stall |

Evaluated at the **worst admissible** corner: `A_j = D` is admissible by definition, since
`sustainable <=> A <= D`. A configuration that passes only at typical `A/D ~ 0.6` is not passing.

Compared against the **low end** of the ceiling model, never the model itself: measured p10 where
the census has it, `model x 0.984` elsewhere (the worst over-prediction observed, 5335/5421). At
rung 16000 the model says 6 064 ms and the device says 5 960 against a 6 000 ms threshold — a sweep
run on the model alone certifies a guard the television cannot satisfy.

## Result

Worst margin over every rung transition, per configuration:

| ladder | graded-reject feed | queue / lead | verdict | worst margin | at |
|---|---|---|---|---:|---|
| no-ties | yes | lead 3.8 s | climbable | +2 270 | 14000->18000 |
| no-ties | yes | aq 12 MiB | climbable | +2 103 | 14000->18000 |
| full | yes | lead 3.8 s | climbable | +1 798 | 18000->20000 |
| full | yes | aq 12 MiB | climbable | +1 513 | 18000->20000 |
| no-ties | yes | **aq 10 MiB** | climbable | **+1 104** | 14000->18000 |
| **full** | **yes** | **aq 10 MiB** | **climbable** | **+572** | 18000->20000 |
| no-ties | no | aq 10 MiB | climbable | +313 | 14000->18000 |
| no-ties | yes | shipped | climbable | **+105** | 14000->18000 |
| full | no | aq 10 MiB | climbable | **+17** | 18000->20000 |
| full | yes | shipped | **blocked** | -408 | 18000->20000 |
| no-ties | no | shipped | **blocked** | -686 | 14000->18000 |
| **full** | **no** | **shipped** *(today)* | **blocked** | **-963** | 18000->20000 |

## The finding that changes the recommendation

**The noise floor is 167 ms.** Measured within-rung reserve spread (max - p10) on the settled leg:
167 ms at rung 4000, 167 ms at rung 20000. (Rung 720's 9 000 ms spread is queue fill-in, not
dispersion — an 8 MiB queue at 1 202 kbps takes ~50 s to fill and the leg never fully settled.)

So **every single-lever configuration is inside the measurement noise**:

- `no-ties + graded-reject feed` on the shipped queue: **+105 ms** — this was the plan's
  recommendation, and it is 0.6x the noise floor. Not safe.
- `full + aq 10 MiB` with today's reject behaviour: **+17 ms**. Not safe.

**Recommendation: full ladder + graded-reject feed + `AQ_VIDEO_BYTES` 8 -> 10 MiB.** Margin +572 ms,
3.4x the noise floor, every rung retained, +2 MiB RSS.

## Why not the no-ties ladder, which scores better

Dropping rungs 12000/16000/20000 removes exactly the four ties in `hls_quality_score`
(`abr.rs:1715-1727`), where the selector currently spends +20%, +14.3%, +11.2% and +4.4% more bits
for an identical score. That looks free — but the scoring function **saturates at 76 for everything
above 17 000 kbps**, which is a known defect of that function rather than a statement about
perception. Deleting the top rung on the authority of a model that cannot see above it would be
circular.

**The ladder question is deferred to a perceptual measurement, not decided here.** If 18000 and
20000 are later shown indistinguishable, `no-ties + graded-reject feed + aq 10 MiB` (+1 104 ms,
6.6x noise) is strictly better and costs a rung nobody can see.

## Landed 2026-08-28

The recommendation is implemented, as two changes that are one decision:

* **The graded-reject feed** — `ff.rs`'s `not_ready` arm now feeds its already-fetched,
  already-demuxed, already-raster-checked segments before abandoning the candidate session, and
  advances the current cursor and the fed timeline past them. Outcome token `not_ready_fed`.
  The other six reject outcomes feed nothing, deliberately: `raster_refused` was rejected for the
  raster of these very AUs, and the rest never completed a fetch.
* **`AQ_VIDEO_BYTES` 8 -> 10 MiB** (`player/engine.rs`), mirrored into `sim.rs`'s
  `VIDEO_QUEUE_BYTES` — the mismatch gate fired, which is how it got there. `B_max` at the top rung
  moves **5002 -> 5852 ms**, and the audio-bound rungs do not move at all, which is the check that
  the enlargement went to the lane it was aimed at.

Neither is worth landing alone: +17 ms and +105 ms respectively, against a 167 ms noise floor.

**The census is now a measurement of a configuration the app is not.** `census_buf_ms` is `buf=`
off the television at 8 MiB, so `the_calibration_reproduces_the_device_census` was moved onto an
explicit `CENSUS_VIDEO_QUEUE_BYTES` plant rather than `Plant::default()`. The model is still
validated at the only operating point evidence exists for; it is no longer validated at the one
that ships. Re-censusing is a device job and the prediction to check is `1600 + 83886080/R_v`,
about 25% above every video-bound row of the current table.

**Host evidence, such as it is.** A 200 s shaped simulator run (`make sim` + `plxnative-clocksink`
through `tools/netcond.py`, unshaped then `rate:8300`) climbed 720 -> 2000 -> 20000, walked down to
4000 and back to 6000, and peaked at **`qbytes=8481329`** — past the old 8 388 608 cap, so the
enlargement is live in the real pipeline and not merely in the model. That run produced **six
commits and zero rejects**, so the graded-reject feed was unexercised *in that window*.

**SUPERSEDED 2026-08-28 — it is exercised, heavily, on the DEVICE, and no rig was needed.** The
paragraph above sent the next reader to build a shaped `pipe_abr_reject_up_4000` recipe for an
experiment the ordinary tier had already been running all along. `not_ready_fed` IS this outcome,
and a full `--filter pipe_abr` tier fires it **37 times across 7 cases**, feeding **2** graded
segments each (not one), with **zero** errors and all 7 cases passing:

| case | transaction |
|---|---|
| `pipe_abr_pin_2000` | 720 -> 2000 |
| `pipe_abr_band_4000`, `pipe_abr_pin_4000` | 720 -> 4000 |
| `pipe_abr_pin_10000` | 720 -> 10000 |
| `pipe_abr_pin_16000` | 720 -> 16000 |
| `pipe_abr_band_20000`, `pipe_abr_pin_20000` | 720 -> 20000 |

**All 37 cross a catalog raster band** — 720's box is 854x480 and every target above is 1280x720 or
1920x1080 — so the excursion-and-return is what LG's decoder actually did, 37 times, without a
`Playing error` and with `pos_climb` satisfied in every case. The device half of this leg is
DISCHARGED, and it was discharged by evidence already on disk rather than by a session.

**One thing these logs cannot settle**, stated rather than glossed: they predate `out=WxH` on the
commit line, so they prove the *catalog* bands were crossed and cannot prove the DECODED raster
differed on each one — against a 1080p source several rungs produce an identical picture
(`m3-production-census.md`). The conclusion is unaffected either way: whatever the decoder was
handed, it handled it.

**ANSWERED 2026-08-28, and the answer is that this tier cannot answer it.** The first device run
carrying `out=` (19/19) shows the decoded raster equal to the catalog box on **every one of the 13
distinct commit lines** — `Up to 2000kbps 1280x720 out=1280x720`, and so on for all of them — and
`abr_raster_changes` reads 15 on both the catalog and the decoded series. That is not evidence the
two agree in general; it is a property of the FIXTURE PACK. `serve_fixtures.py`'s `ABR_RASTER` maps
every rung to exactly its catalog box, because the renditions are pre-encoded static files rather
than a transcoder's output. There is no PMS here choosing a raster, so there is nothing for the box
to disagree with.

**The synthetic tier is therefore structurally blind to the box-vs-decoded divergence**, and the
divergence is real: M3 measured a live PMS producing 1280x720 for BOTH `P720` and `P1080M6` against
a 4K source, and 1918x802 for every rung from `P1080M6` up against a 1080p one. Confirming that
`out=` changes what `raster_changes` counts needs the SERVER tier (`./tests/run.py --server`)
against a real PMS. Until then the change is correct-by-construction and unexercised, which is a
different and weaker claim than "verified".

**What the simulator can and cannot say about the feed.** `make sim` + `plxnative-clocksink` now
runs the whole path — queues, backpressure, the PTS timeline, the cursor step, the reserve the
controller reads next — so "does our own pipeline handle a fed reject" is host-answerable and was
answered here. What is NOT answerable off-device is LG's decoder: this is a one-segment raster
excursion followed by a return, and while a commit changes raster too and is fine, the change BACK
is new. That was the device leg this owed. **It is discharged** — see the superseding note above: 37
raster-crossing excursions across 7 cases on the television, zero errors. The host half was
answered here; the device half was answered by the ordinary tier, which had been exercising it
before anyone went looking.

## What this does not settle

- `cold_start_ms = 250` is **unmeasured off-fixture**; on a remote TLS PMS it carries a handshake.
- `E_tx_down` medians come from 17 committed down-legs on a static file server: no JIT, control
  plane 6 ms. Both are optimistic against a real PMS.
- The `/1.04` TS-to-ES divisor is an assumption (`sim.rs:246-250`), not a measurement, and the
  per-lane ES byte counters that replace it are Phase 1 work.
- ~~The audio lane has never bound in 366 samples, so its term in `B_max` is pure arithmetic.~~
  **Measured 2026-08-27 and no longer true**: the audio lane binds at rungs 320 and 720, within
  +3.2% and +5.6% of `lead + AQ/R_a`, and the ratio test agrees (`R_v/R_a` = 5.1 at rung 720,
  below the bare `VQ/AQ` = 8). The crossover sits between 720 and 2000, so the audio term is
  measured rather than arithmetic. `docs/measurements/p2-census.md` §2.
- The graded-reject feed needs **device verification** that a one-segment raster excursion through
  `hls_feed_segment` is benign — the AUs are self-contained, but the encoder session is abandoned
  immediately afterwards and Starfish's reaction is unproven.
