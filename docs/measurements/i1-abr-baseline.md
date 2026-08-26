# I1 — ABR baseline on unmodified policy

**Device session 2026-08-26.** LG 49SM9000PLA, webOS 4.10.0, `com.beb.plxnative.debug`.
Binary: `5a8ef2ef` + I0 (`88b5d738`) + I0 amendments (`3f9ce72b`) — instrumentation only, no ABR
policy change. Host suite at that SHA: 1272 Rust + 98 harness green.

Raw `abr: sample` traces for the census points are beside this file (`raw-<pin>.txt`).

## Purpose

Record what the CURRENT controller does, before any of increments I3-I8 change it. Nothing here
is a threshold and nothing here was graded — every ABR bound in the manifest is still absent by
design (`test_no_manifest_case_carries_a_new_abr_bound_yet`).

## M2 — the four shaped cases (all PASS)

| case | rungs | settled | commits | min_buf | max_stall | first segment |
|---|---|---|---|---|---|---|
| `pipe_abr_slow_start_then_fast` | — | — | — | — | — | buf=2000ms current=720 -> **prime_down 320** |
| `pipe_abr_steady_modest_link` | — | — | — | — | — | buf=2000ms current=2000 -> **prime_down 720** |
| `pipe_abr_brief_dropout` | — | — | — | — | — | buf=2000ms current=10000 -> **prime_down 8000** |
| `pipe_abr_oscillating_link` | — | — | — | — | — | buf=2000ms current=6000 -> **prime_down 4000** |

(The rung/commit columns were not captured on this pass: `report_case` prints the metric story
only under `--verbose`, and the first four runs were not verbose. Re-run with `--verbose` to fill
them; the characterisation lines above were printed unconditionally and are the load-bearing part.)

Every run also printed `history: switches=1 since_last=<ms> advanced=0ms`.

## M4 — the buffer census

Unshaped LAN measured 103-111 Mbit/s. Pins 720 and 4000 additionally carry a flat shaped leg
(6000 / 20000 kbps) — see the limitation note below; the shaper sets the FILL rate and is far
above the media rate, so the queue is still what bounds the reserve SIZE, which the agreement
below confirms.

Predictions are computed from queue geometry alone — `aq_caps()`, `MAX_FEED_AHEAD_NS`,
`AUDIO_SLACK_NS` and the MEASURED media and audio rates. No controller helper is involved. The
elementary rate is taken as `(media - audio) / 1.04`, because the AU queues hold demuxed
elementary bytes while `media=` is measured off the TS wire.

| pin | media kbps | audio kbps | video ES | pred video | pred audio | **prediction** | **observed median** | err | binding lane |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 720 | 1381 | 131 | 1202 | 57 431 | 67 635 | **57 431** | **59 001** | +2.7% | video 85/85 |
| 4000 | 3183 | 160 | 2907 | 24 685 | 56 028 | **24 685** | **24 835** | +0.6% | video 70/70 |
| 20000 | 18456 | 192 | 17 562 | 5 421 | 47 290 | **5 421** | **5 335** | -1.6% | video 57/57 |

Distributions (settled rung, first quarter discarded as queue fill-in):

| pin | n | p10 | median | p90 | max |
|---:|---:|---:|---:|---:|---:|
| 720 | 56 | 50 085 | 59 001 | 59 085 | 59 085 |
| 4000 | 53 | 24 751 | 24 835 | 24 918 | 24 918 |
| 20000 | 43 | 5 293 | 5 335 | 5 460 | 5 460 |

`dur=2000ms` on every sample at every rung — the server's `EXT-X-TARGETDURATION:2` was honoured
and never varied.

### The two guard collisions, measured

| rung | max reserve | `buffered >= 3*segment` (6000 ms) | `starving()` 6000 ms band |
|---:|---:|---|---|
| 720 | 59 085 | satisfiable | not armed |
| 4000 | 24 918 | satisfiable | not armed |
| **20000** | **5 460** | **UNSATISFIABLE** | **permanently armed** |

## Limitations of this session

* Only 3 of the 7 census points were measured. Unshaped, the controller starts at the TOP rung
  (the LAN measures 84-111 Mbit/s, so `startup_rung` picks 20000), and the pin cannot transact
  from there: `PIN_MIN_RESERVE_SEGMENTS` needs six segments of reserve, which is unreachable above
  ~11 Mbit/s of media. Pins 720 and 4000 were reached only by shaping the link so the run STARTS
  low. Measuring 320 / 2000 / 10000 / 16000 needs the pin applied at the ROUTE's starting ceiling
  rather than as a transaction — an I0 follow-up, not a policy change.
* Segment-duration sensitivity (I1-D) was NOT measured. `serve_fixtures.py` hard-codes
  `#EXT-X-TARGETDURATION:2` over 2 s clips; a 4 s leg needs new fixtures. Deferred, not inferred.
* Seek (I1-E) was NOT exercised: one `abr: seed` line per run, no seek operation. Deferred.
* Mode-switch decay (I1-F): the INPUT is confirmed zero (`advanced=0ms`, every run), but no
  HLS -> Original -> HLS round trip was produced, so the decay's behaviour over wall time is
  still unobserved.
