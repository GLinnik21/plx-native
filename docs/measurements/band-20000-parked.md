# Rung 20000, held on purpose — what the top of the ladder actually costs

> **This document was wrong when first written (2026-08-28) and is rewritten here.** It opened
> "the last bad case in the tier" and described `pipe_abr_band_20000` as *parked on a rung its own
> estimator says it cannot afford*, i.e. as a controller defect. It is not. **The case sets
> `abr_pin: 20000`** — it pins the ladder to that rung deliberately, which is the whole point of a
> `band`/`pin` case (M4: "a rung held long enough to read a settled reserve at it"). The controller
> returns `Decision::Stay` from the pin block, which is exactly why every parked sample carries
> `reason=None`, and why no downshift is proposed.
>
> The error is instructive and cheap to avoid: I read `observe`'s downshift path line by line,
> proved the guard should fire, called the result "one contradiction", and never checked the
> case's own manifest entry for a pin. **Read what the case ASKED FOR before diagnosing what the
> controller DID.**

## What the case actually measures, which is worth having

Pinned to rung 20000 on the dev television, over four tier runs:

| run | stall max/total | lumpy | rate |
|---|---|---|---|
| tier3 | 29 / 29 | 0 | 698 pm |
| tier5 | 29 / 29 | 0 | 698 pm |
| tier6 | 27 / 27 | 0 | 719 pm |
| tier7 | 29 / 29 | 0 | 698 pm |

Stable to ±1 across runs spanning several controller changes, so it is a property of the rung and
the plant rather than of any policy touched this session. The case **passes** — it declares an
empty `abr_shape` and no rate bound, so the tier is 19/19 with this inside it.

## The settled state

```
abr: sample current=20000kbps media=9437kbps net=74596kbps buf=126ms vbuf=126ms abuf=279ms
             dur=2000ms prod=206pm n=111 decision=stay target=0kbps reason=None
```

**The link is not the constraint.** 74.6 Mbit/s delivered against a 9.4 Mbit/s media rate, with
server production at 20.6% of the segment duration. Nothing upstream is starved. (An earlier
version of this note offered `safe=8375` as the explanation — that is the CONSERVATIVE estimate,
and the instantaneous rate is eight times it. Also wrong, also corrected.)

**The reserve sits at 126 ms and the film runs at 698 pm.** Held at this rung the pipeline cannot
present in real time, and the queue never builds, because it is drained as fast as it fills.

## This is R2, measured

The plan's R2: *"the top of the ladder is unreachable for ANY guard of this shape. The guard is
Ω(D); `B_max ∝ 1/R`. They cross at ~15.7 Mbit/s of media at the shipped `AQ_VIDEO_BYTES = 8 MiB`."*
Phase 0 raised the queue to 10 MiB, which moves the crossing but does not remove it.

At a 9.4 Mbit/s media rate the reserve available at this rung is a small multiple of one frame, not
of one segment — so there is nothing to absorb any jitter, and the 30 % rate deficit follows. That
is the ladder's top being *nominally* selectable and *practically* unusable, which is what R2 says
and what a `band` case exists to expose.

## What this does NOT say

* **Nothing about the CONTROLLER.** A pinned rung bypasses selection entirely. This case cannot
  fail a downshift trigger, an admission rule or a deadline, because none of them run.
* **Nothing about whether Auto would ever CHOOSE 20000.** `pipe_abr_pin_20000` and
  `pipe_abr_band_20000` both pin it; the unpinned cases climb to 18000 at most on this link.
* **Whether the deficit is decode, feed-ahead, or the queue geometry** is not separated here.
  `MAX_FEED_AHEAD_NS`, `AQ_VIDEO_BYTES` and the decoder are three candidates and this run
  distinguishes none of them.
