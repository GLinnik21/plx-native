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

## Narrowed by code reading, 2026-08-28 — to one contradiction

The earlier text offered "two candidate mechanisms" and said the log distinguishes neither. Reading
the controller narrows it much further, and what is left is sharper than a choice between two
stories.

**There is exactly ONE path that proposes a downshift** (`Controller::observe`), and its outer
guard is

```rust
if horizon_bad || (!cold_start && (buffer_bad || production_bad)) { … }
    buffer_bad   = buffered < segment || self.buffer.starving()
    cold_start   = self.samples_on_rung == 0
```

On the parked samples the log reports `buf=126ms`, `dur=2000ms`, `onrung=97`. So `buffer_bad` is
`126 < 2000` = **true** and `cold_start` is **false**, the guard should fire, and inside it
`buffered < segment / 2` also holds — which selects `best_for_budget(safe).min(current.below())`,
i.e. a rung around 8000 against a `current` of 20000. A proposal should be made.

**It is not.** Every one of those lines carries `decision=stay target=0kbps reason=None`, and
`reason=None` is the strong part: `last_reason` is cleared at the top of `observe` and set by every
decision path, so `None` means the function returned before reaching any of them.

Only three early returns precede that guard — a transaction already pending, an unreadable reserve,
and the dev rung pin. The log rules out all three on these samples: `pending=0kbps`, `pin=none`,
and the reserve is plainly readable (`buf=126ms vbuf=126ms abuf=279ms`, and `buffered_ms()` is an
`Option` whose `None` is what R11 made explicit).

**So either a fourth early return exists that this reading missed, or the `buffered` the guard
tests is not the quantity `buf=` prints.** That is the whole remaining question, it is answerable
by reading rather than by another device run, and it is a much smaller question than the one this
document opened with.

## Also worth carrying: the link is NOT the problem here

`abr: sample current=20000kbps media=9437kbps net=74596kbps buf=126ms … prod=206pm`. The network is
delivering **74.6 Mbit/s** against a media rate of 9.4, and production is 20.6% of the segment
duration. Nothing upstream is starved. The reserve sits at 126 ms anyway, and the film runs at
698 pm. Whatever is wrong, "the link cannot carry rung 20000" is not it, and the earlier reading of
`safe=8375` as the explanation was too quick — that is the CONSERVATIVE estimate, and the
instantaneous `net=` is an order of magnitude above it.

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
