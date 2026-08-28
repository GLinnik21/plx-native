# M3 — what a rung actually costs the server

**Measured 2026-08-28 on the dev Mac against the configured PMS.** No television, no lock.
Tool: `tools/abr-production-census.py`, which drives `tools/pms-hls-probe.py` once per request
ceiling and reads only its already-redacted `report.json`. Twelve consecutive segments per rung,
one short-lived transcode session each, run twice: back-to-back, and paced at one request per two
seconds of media (what a player actually does).

Two sources, because the answer turns out to depend on which one you use: the overlay shapes
`movie_h264_ac3_1080p` and `movie_hevc_4k_high_bitrate`.

`rho = total_fetch_ms / media_duration_ms`, per mille — the same expression
`SegmentSample::production_ratio_pm` evaluates at runtime, so the census and the controller cannot
mean different things by it.

## Why this measurement blocks an increment

`HlsActuatorCatalog::measured()` carries a `production_load_pm` per rung, and `ladder.rs` says
plainly that two of the thirteen are empirical and the other eleven are "an ordering assumption".
That table is one half of the two-constraint admission rule — the half that refuses 4K on a fast
link in front of a loaded PMS — so eleven unmeasured numbers decide a real behaviour.
`docs/adaptive-playback-plan.md` blocks I9 on this and states the falsification rule in advance:
residuals within 15% uphold the "inert argmax" finding, and any mid-ladder load off by more than
25% means the deferred argmax is a re-parameterisation on fresh numbers.

## Result 1 — against a 4K source, the top of the ladder is UPHELD and the mid-ladder is not

Residual `load_j = 1000 * (rho_j - rho_floor) / (rho_top - rho_floor)`, the table's own
normalisation. Paced leg; the back-to-back leg agrees to within 0.2 points everywhere, which is
itself worth noting — this server is fast enough that pacing does not change the answer.

| rung | request | table | measured resid | deviation |
|---|---|---|---|---|
| P240 | 320 | 90 | 0 | *floor-tied* |
| P480 | 720 | 180 | 0 | *floor-tied* |
| P720Low | 2000 | 420 | 321 | 23.6% |
| P720 | 4000 | 450 | 333 | 26.0% |
| **P1080M6** | **6000** | **900** | **353** | **60.8%** |
| P1080 | 8000 | 930 | 1026 | 10.3% |
| P1080M10 | 10000 | 950 | 1032 | 8.6% |
| P1080M12 | 12000 | 970 | 1032 | 6.4% |
| P1080M14 | 14000 | 980 | 1026 | 4.7% |
| P1080M16 | 16000 | 990 | 1019 | 2.9% |
| P1080M18 | 18000 | 995 | 1006 | 1.1% |
| P1080High | 20000 | 1000 | 1000 | *anchor* |
| **Uhd** | **22000** | **2100** | **2404** | **14.5%** |

**The entry that matters most is confirmed.** `Uhd = 2100` is the number that refuses 4K on a
loaded server, and against a real 4K source it measures 2404 — inside the 15% rule. So is the
whole 1080p block, every member within 10.3%. Those were the two empirical points and the
interpolation between them, and the interpolation holds.

**`P1080M6` does not.** The table says 900; it measures 353, off by 60.8% and consistently so in
both pacing legs. The 6000 kbps rung is charged roughly two and a half times the server work it
costs, which biases the admission rule against exactly the rung a mid-speed link should be
settling on in front of a loaded PMS. `P720Low` and `P720` sit either side of the 25% line
(23.6% / 26.0%) and are charged about a third too much on the same reading.

Per M3's stated rule this is **REFUTED at the mid-ladder**: the deferred argmax cannot be adopted
on the existing table and must be argued on these numbers.

## Result 2 — the table is indexed by the wrong variable, and a 1080p source shows it

Against `movie_h264_ac3_1080p` the ordering **inverts**: 58 of 75 rung pairs cost the opposite of
what the table orders them. Warm rho, paced:

| rung | raster | warm rho |
|---|---|---|
| P240 | 426x240 | 162 |
| P480 | 854x480 | 163 |
| P720Low | 1280x720 | 163 |
| P720 | 1280x720 | 113 |
| P1080M6 … P1080High | 1920x1080 | 105–109 |
| Uhd | (capped to 1080p) | 58 |

The low rungs are the *expensive* ones. That is not noise and it is not a server anomaly — it is
what a transcoder does: **against a source at raster R, a target below R must be downscaled, while
a target at R is a near-copy.** Requesting 240p from a 1080p master is real scaling work;
requesting 1080p is close to a remux. The `Uhd` row is the same effect at the other end — PMS
never upscales, so a 4K request against a 1080p source produces 1080p, and it is the cheapest
column in the table.

`production_load_pm` is indexed by the **target rung alone**. It cannot express a cost that
depends on the *distance between source and target raster*, so there is no single set of thirteen
numbers that is right for both a 4K library item and a 1080p one. The 4K column above is the one
the current table approximates; against a 1080p source the same table is not merely mis-calibrated
but ordered backwards.

## An alternative explanation for Result 2 that these numbers cannot rule out

Result 2 reads the 1080p column as the table being indexed by the wrong VARIABLE. **A second
explanation fits the same numbers and nothing recorded here can separate them**, which has to be
said before anyone re-parameterises a table on it.

The `Uhd` row is what raises it. Against a 1080p source, a 4K request produced **58 pm** where
`P1080High` produced **105** — the same output raster (PMS never upscales), the same source, half
the work, from two requests that differ only in a bitrate ceiling that binds in neither case. Two
unbinding ceilings should not differ by 2x. The obvious reason they might is that **at a ceiling
above what the source needs, PMS stops re-encoding and copies the video** — in which case the
cheapest column is not a cheap transcode at all but a REMUX, and a remux does not belong on a curve
of encoder cost. If that is what happened, part of the "inversion" is two different operations
plotted on one axis rather than a mis-indexed table.

A latency floor was the other candidate and it is **refuted by these numbers**: a fixed cost per
request would be a lower bound, and the 1080p rungs sit *below* the low ones (210–218 ms against
324–326 ms). Whatever the low rungs are paying, it is not a floor.

`tools/abr-production-census.py` now records the **output** per rung — codec, raster and delivered
rate — beside the cost, which is the one column that separates the two readings. It was not
recorded the first time. Until it is, Result 2's *direction* is measured and its *cause* is not,
and I9's choice between "index by (source, target)" and "declare the table correct for one source
class" should not be made on the cause being assumed.

## What this does and does not settle

- **Settles:** `Uhd = 2100` and the 1080p block are real, within 15%, measured against a source
  that exercises them. The two empirical points the table always claimed are confirmed.
- **Settles:** `P1080M6 = 900` is wrong by 61% and cannot be inherited into I9's argmax.
- **NARROWED since first written:** "the table's variable is wrong, not just its values" is what
  the ordering shows, and the section above gives a second reading of the same evidence that has
  not been excluded. What is settled is that **the target rung alone does not predict the cost**;
  whether the missing term is the source raster or the transcode/remux distinction is open. Any re-parameterisation has to
  decide whether to index by (source, target) or to accept being right for one source class.
- **Does NOT settle:** anything about a *loaded* server. Every reading here is against an idle
  PMS, where warm rho tops out at 432 pm against a `production_max_pm` of 1100 — a factor of 2.5
  of headroom, so the production gate never approaches firing. The table's job is to rank rungs
  when the server is behind, and this census cannot create that condition. Stated rather than
  inferred past.
- **Does NOT settle:** the cold/warm split. Cold rho (the first segment, which the encoder has not
  run ahead of) is roughly double warm on 1080p targets and equal on smaller ones; that is a real
  startup term and it is recorded in `census.json` but not modelled anywhere.

## Reproducing

```sh
tools/abr-production-census.py --item movie_h264_ac3_1080p     --segments 12 --pace 2.0
tools/abr-production-census.py --item movie_hevc_4k_high_bitrate --segments 12 --pace 2.0
```

Both write a `census.json` carrying every per-segment rho. The overlay keys are shape names, not
identifiers on anybody's server; no address, token, session, rating key or title reaches any
artifact, which `pms-hls-probe.py` enforces rather than merely intending.
