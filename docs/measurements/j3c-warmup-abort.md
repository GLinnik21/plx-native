# J3c — a downshift warm-up that cannot land, and the ~30 s of judder after it

**Device, `pipe_abr_down_outrun`, 2026-08-28.** Synthetic fixture tier: no PMS, no library, no
account. Every number below is off the app's own event log.

## What was reported, and why no test had caught it

A user described playback that "stalls for a sec, then quickly plays what stalled for a sec, then
stalls again … until it eventually plays smooth", lasting "up to a minute", after a bitrate change.

The suite could not see it. `pipe_abr_down_outrun` **passed**, with `max_stall=8s` and
`play_rate_pm≈1000` — a mean rate at exactly real time. The reason is that the failure is neither
of the two shapes the harness grades:

| shape | instrument | reads |
|---|---|---|
| the clock STOPS | `abr_stalls` | 8 s — the real stall, before the burst |
| the clock runs SLOW | `playback_rate` | ~1000 pm — real time |
| the clock arrives in LUMPS | *nothing* | — |

`playback_lumpiness` was added for the third (`tests/run.py`, pinned by `test_harness.py`). It
scores 13–14 lumpy beats here and **0** on `pipe_abr_oscillating_link` and `pipe_abr_pin_4000`.

## The media clock, run-length encoded

```
0 1 2 … 119 120 121x9 122 123 125x2 127 129x2 131 133x2 135 137x2 139 140 141 143 …
                ^^^^^ 8 s stall    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ ~30 beats of +2/0
… 157 158 159 160 161 162 … 195          <- smooth 1:1 to the end
```

2 s is the fixture segment duration. The clock is advancing **one whole segment per arrival** —
playback running straight off the network with nothing queued.

## The three transactions that produced it

Immediately before, the estimator is confident and wrong:

```
current=18000kbps  slow=99555kbps  unc=200pm  n=22  buf=6168ms
```

```
tx Down 18000->2000  outcome=warmup_deadline  decided=5948ms  warmup_dl=5918ms
                     buf_start=5918ms  buf_decided=168ms  net=5798kbps
tx Down 18000->320   outcome=warmup_deadline  decided=1769ms  warmup_dl=1755ms
                     buf_start=168ms   buf_decided=168ms  net=363kbps  slow=1313kbps
tx Down 18000->320   outcome=committed        decided=2589ms  warmup_dl=8494ms  warmup=2583ms
```

1. **`warmup_dl == buf_start` exactly.** The deadline was the whole reserve. The
   `predicted_transfer` floor added in J3b is inert here: `2000 x 2000ms / 99555 ≈ 40 ms`, computed
   from an estimate **17x** above the link the transfer then measured (`net=5798`). A floor
   derived from a wrong `C` is a wrong floor, and no amount of uncertainty widening
   (`MAX_UNCERTAINTY_PM` = 500, i.e. x1.5) covers a 17x error.
2. The transfer ran the **full** 5918 ms and delivered nothing: `buf 5918 -> 168ms`. The reserve
   was spent discovering a fact that a projection off the first measurable 250 ms already implied.
3. The second attempt therefore ran on 168 ms. Here the floor *is* load-bearing —
   `warmup_dl=1755ms > buf_start=168ms` — but the estimate still lagged (`slow=1313` against a true
   `net=363`), and the true requirement was `320 x 2000 / 363 ≈ 1763 ms`. **It missed by 8 ms.**
4. The third had a converged estimate, an 8494 ms budget, and committed in 2583 ms — onto an
   empty queue. Everything after that is the lump burst above.

## The defect

The candidate warm-up carried a wall-clock deadline and **no abort rule**, on this argument:

> A candidate's bound is what the TRANSACTION can afford, which `candidate_deadline` already is.
> The abort rule is about what the PICTURE can survive, and the picture is being fed by the
> current rung throughout.

The first clause still holds. The second is true of an **upshift** — where the current rung is
affordable by construction, which is why a dearer one was proposed — and false of a **downshift**,
where the current rung being unaffordable IS the trigger. `buf_start=5918ms -> buf_decided=168ms`
is that clause failing, measured. It is the same asymmetry `candidate_warmup_budget` already turns
on, one level down.

A deadline says WHEN to give up. It cannot say that giving up now is already certain.

## The change

`candidate_warmup_is_guarded(proposal)` — `Direction::Down` and not at the ladder floor — arms the
existing `StallGuard` on the warm-up, with the budget it already had:

```text
abort iff  8 * bytes_remaining / C_measured  >  warmup_budget - elapsed
```

No new constant and no new threshold: the budget is `warmup_budget`, the arithmetic is R16's, and
`MEASURABLE_OBSERVATION_US` still forbids aborting before the fetch is measurable, so the capacity
sample is learned either way. The floor exclusion is R12 as `hls_read_loop` already applies it to
the active cursor — with nowhere cheaper to run to, an abort re-fetches the same bytes and buys a
loop instead of a picture. The outcome is named `warmup_unreachable` rather than
`warmup_deadline`, because the two differ in the thing worth reading off a log: one spent the whole
budget to learn the rung was unaffordable, the other proved it early and handed the rest back.

## What this does NOT claim

- It does not fix the stale estimate that mis-sized the first deadline. It bounds the **cost** of
  discovering the staleness; it does not make the first downshift after a collapse free.
- The 8 s stall at pos=121 precedes all three transactions and is not addressed here.
- One fixture, one link profile, one collapse shape. `down_outrun` is built so the link outruns the
  controller; whether the same sequence occurs on a gentler collapse is untested.
- Nothing here has been measured against a real PMS, only the synthetic fixture server. The
  capacity numbers are a shaped loopback link, not a household's internet.
