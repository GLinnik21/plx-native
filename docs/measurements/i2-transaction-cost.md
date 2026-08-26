# I2 — transaction cost, measured

**Device session 2026-08-26**, `5a8ef2ef` + I0 + the I2 instrumentation. Four shaped M2 cases,
`--verbose --no-early --save-logs`. Raw logs in `i2-logs/`. No policy changed.

25 candidate transactions recorded. This is the first time this project has measured what a
transaction costs; the only prior figure was the host plant's **derived** 4600 ms, which the plant
charged identically to all four legs.

## What a transaction costs

`decided` is elapsed to the commit/reject decision — the unrefilled cost. `total` runs to scope end
and also contains the post-commit feed, which blocks on a full queue and is backpressure rather
than transaction cost. Median post-commit feed: **0-1 ms**, so the two are nearly the same here.

| leg | n | decided ms (min/med/max) | control plane | warm-up acq | graded acq |
|---|---:|---:|---:|---:|---:|
| Down / commit | 17 | 284 / **1 792** / 2 246 | 5 | 1 220 | — (never fetched) |
| Up / commit | 7 | 2 383 / **9 563** / 28 998 | 6 | 1 533 | 1 524 |
| Up / reject | 1 | (unlabelled path — fixed after this run) | 6 | 1 412 | — |

**The single constant was wrong in both directions.** Against the plant's 4600 ms: an upshift costs
**2.1x** that at the median and **6.3x** at the maximum; a downshift costs **0.39x**. Simulating
both with one number over-charges recovery and under-charges experimentation — the opposite of the
bias you would choose.

## Two thirds of an upshift is in a leg nothing measures

`decided − (control + warmup + graded)`:

| transition | decided | instrumented legs | unaccounted |
|---|---:|---:|---:|
| 320 -> 720 | 2 760 | 2 757 | **3 ms (0%)** |
| 720 -> 2 000 | 2 383 | 2 379 | **4 ms (0%)** |
| 14 000 -> 18 000 | 4 808 | 2 889 | 1 919 ms (39%) |
| 4 000 -> 10 000 | 9 563 | 3 146 | 6 417 ms (67%) |
| 2 000 -> 10 000 | 13 885 | 3 111 | 10 774 ms (77%) |
| 4 000 -> 20 000 | 22 190 | 3 065 | 19 125 ms (86%) |
| 720 -> 20 000 | 28 998 | 3 088 | **25 910 ms (89%)** |

Median: **6 417 ms unaccounted of a 9 563 ms transaction.** It is ~0 for short hops and grows with
the target rung. The instrumented legs are flat at ~3 100 ms across every row, so the variable cost
is entirely outside them.

**Not yet attributed, and deliberately not guessed.** The operations between the warm-up demux and
the decision are: the media-playlist `hls_cursor_next` for the graded segment, the `NotReady` retry
loop inside it (`retry_budget = clamp(3d+2s, 3s, 15s)` = 8 s at d = 2 s, bounded independently of
any candidate deadline), `candidate_ready`, the raster check, and `control.retire`. The next
increment must bracket each; a 25.9 s leg on a 90 s case cannot stay unattributed.

Note this **refutes the board's hypothesis about which leg is uncovered**: it predicted the control
plane (`control.prime` + master + media playlist) would dominate because no deadline covers it.
Measured, that leg is **5-6 ms**. Something after it and outside both media legs takes the
majority. Caveat: the fixture server is static, so there is no PMS transcode decision and no JIT
production in these numbers — against a real PMS the control plane will not be 6 ms.

## The claimed admission rule, against measurement

`T_prime,max = B − A_i` would have admitted every one of these:

| transition | B | A_i | T_max | decided | reserve consumed |
|---|---:|---:|---:|---:|---:|
| 720 -> 20 000 | 30 334 | 120 | 30 214 | 28 998 | **25 000 ms** |
| 4 000 -> 20 000 | 23 542 | 268 | 23 274 | 22 190 | **18 208 ms** |
| 2 000 -> 10 000 | 24 126 | 450 | 23 676 | 13 885 | 9 958 ms |
| 4 000 -> 10 000 | 19 792 | 450 | 19 342 | 9 563 | 5 583 ms |
| 14 000 -> 18 000 | 6 293 | 1 188 | 5 105 | 4 808 | 834 ms |

Every row passes the rule while consuming most of the reserve, which is the board's fixed-point
objection with device numbers behind it. Note also that the two largest rows end at **5 334 ms** —
the new rung's own ceiling. Part of that collapse is not the transaction at all: upshifting
*reduces* `B_max`, so the reserve is truncated by the ceiling change independently of what the
experiment cost.

## Status

Measurement only. No threshold proposed, no policy changed. The four transaction legs now have
measured values for the fixture tier; against a real PMS they are unmeasured.
