# `pipe_abr_band_20000` — parked on a rung its own estimator says it cannot afford

**Device, 2026-08-28, tier7 (19/19 pass).** After the downshift warm-up abort landed, this is the
**only** case in the ABR tier that still misbehaves: 18 of 19 show zero stall at ~1000 pm, and this
one holds one position for **30 beats** and runs the film at **698 pm** — 70% of real time.

It **passes**, which is the point worth carrying: `abr_shape` does not bound the rate here, and
this is the exact shape `playback_rate`'s doc was written for.

## Not a regression

| run | stall max/total | lumpy | rate |
|---|---|---|---|
| tier3 | 29 / 29 | 0 | 698 pm |
| tier5 | 29 / 29 | 0 | 698 pm |
| tier6 | 27 / 27 | 0 | 719 pm |
| **tier7** (warm-up abort live) | 29 / 29 | 0 | 698 pm |

Stable across four runs spanning today's transport changes. The 27-vs-29 is run-to-run noise.

## The settled state, which is the whole diagnosis

```
abr: steady current=20000kbps safe=8375kbps slow=16750kbps unc=500pm n=93
             buf=126ms slope=0ms/s prod=194pm/194pm risk=70 starve=0 dwell=0ms
```

Read it field by field:

* `current=20000` — it committed UP to the top of the ladder, once, after four `not_ready_fed`
  rejects.
* `safe=8375` — its OWN conservative capacity is **41%** of the rung it is sitting on.
* `buf=126ms` — the reserve is essentially gone. One segment is 2000 ms.
* `slope=0ms/s` — and it is *not draining*.
* `starve=0` — so nothing fires.

**`slope=0` at `buf=126ms` is the failure, not evidence against it.** The reserve is media time
measured against the playhead. When the playhead slows to match the fetch rate, the reserve stops
draining — so a buffer pinned just above empty reads as *stable* to every signal keyed on its
derivative. The picture is being delivered just-in-time at 70% speed and the controller sees a flat
line. This is `[[reserve-cannot-see-a-slow-film]]` exactly: **a healthy metric is not evidence of
health when the failure moved its own denominator.**

## What is NOT explained

Why it does not downshift. `safe=8375` against `current=20000` should make the current rung
unaffordable by the admission rule's own arithmetic, and `risk=70` is elevated — yet no downshift
is proposed across 30 beats. Either the emergency predicate is keyed on the drain (which is zero,
per above) rather than on the level, or the admission test is not evaluated for the CURRENT rung
once committed. **This has not been read out of the code and should not be guessed at**; the log
establishes the symptom and the two candidate mechanisms, and nothing here distinguishes them.

That is also why no fix is attempted in this document. The plan's own downshift TRIGGER (R23 —
*"trigger, target and deadline are three different things"*, and the ordinary trigger "was never
written down at all") is the section this belongs to.

## The instrument note

`max_stall_s` sees this one (29 s of held clock), so it is not invisible — but it would NOT have
been caught by `min_buf_ms`, `slope`, `starving()` or `dip_max_kbps`, all of which read fine. The
rate is the only signal that names what is wrong, and it is reported and never asserted, for I0's
reason: asserting it would pin today's behaviour as desirable.
