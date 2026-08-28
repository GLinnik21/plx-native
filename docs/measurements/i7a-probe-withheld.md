# I7a — the Original-recovery probe never fires, and now the log says why

**Host, 2026-08-28, `pipe_auto_original_slow_recover`.** This case starts in Original mode, is
collapsed to 4 Mbit/s so it falls back to HLS, and then given 40 Mbit/s back so it can recover. It
covers `original-to-hls-handoff`, `original-recovery` and `runtime-original-watchdog` — the two
legs I7a and I7b owe.

**It was never run today, and the reason is a naming accident**: every tier this session used
`--filter pipe_abr`, and this case is `pipe_auto_*`. The plan's I7b row asks whoever gets there to
*"name the case that can"* reach the Original->HLS transition. This is it, and it is not a
`pipe_abr_*` case, which is exactly why the row could be written.

## What happens

The handoff works. The recovery does not:

```
loads: 2                (the case declares load_count_exact: 3)
a_auto_network_recovery -> False
  "Original fell at 3841kbps/5104ms (ImminentStarvation) and HLS reached 18000kbps,
   but Original was never requested again"
abr: mode lines: 0
```

Not a time bound: at 80 s HLS reaches 8000 and at 180 s it reaches 18000, and Original is never
re-requested in either.

## Why the log could not answer this before

`probe_due` is a conjunction — a deep reserve, a reserve that is not draining, measurable spare
capacity over the CURRENT rung — plus a spacing timer, plus `worth_probing`. **Any one of the five
failing produced the same output: nothing.** `abr: mode` is only emitted on a probe RESULT, so a
gate that never opens logs identically to one that was never constructed.

That is `[[silent-instrument-trap]]`: *prove the instrument can see the thing before reading its
silence.* `ProbeBlock` + `abr: probe withheld reason=…` names the first unmet condition, rate
limited to CHANGES so a steady refusal costs one line rather than one per segment. **It changes
nothing about when a probe fires.**

## The answer

```
abr: probe withheld reason=shallow_reserve rung=2000kbps  buf=2000ms  safe=2321kbps
abr: probe withheld reason=too_soon        rung=2000kbps  buf=6939ms  safe=2940kbps
abr: probe withheld reason=draining        rung=6000kbps  buf=24246ms safe=19173kbps
abr: probe withheld reason=too_soon        rung=18000kbps buf=6451ms  safe=24447kbps
abr: probe withheld reason=not_worth_it    rung=18000kbps buf=6202ms  safe=24176kbps
```

The gate reaches healthy-and-spaced and then **`worth_probing` says a successful probe would not
change the decision**: with HLS at 18 000 kbps, the mode comparison prefers it to the **8 000 kbps
source it is a transcode of**.

## This is R5, measured

The board's R5 said the ledger *"inverts the mode preference for the modal library item … an
8.5 Mbit/s 1080p source scores three steps below a 20 Mbit/s transcode of itself"*, and it has been
an analysis of the scoring function until now. This is the same inversion observed end to end, in a
running controller, with the refusal it causes.

It is also **§7.B's defect from the other side**. §7.B is blocked on quality being scored against a
fabricated baseline (N14 site 3, `original_utility`); this is what that scoring DOES when the
ladder climbs above the source rate.

## What this does NOT establish

* **Whether the device agrees.** This case has never run on the television — it is outside the
  `pipe_abr` filter — so there is no device leg to compare. On a loopback link the ladder reaches
  18 000 against an 8 000 kbps source, and a real link may not, which would change the comparison
  without changing the scoring. **Run it on the device before treating the refusal as universal.**
* **Whether `not_worth_it` is WRONG here.** Preferring an 18 Mbit/s transcode to an 8 Mbit/s source
  is defensible on bitrate alone; it is wrong only if Original's structural advantages
  (no generation loss, DV, Atmos — `original_quality_bonus` and the N16 split) outweigh it, and
  those are the magnitudes §7.B exists to put on a measured footing. **This measurement does not
  settle the scoring; it shows the scoring is what decides.**
* Nothing here decodes and every heartbeat carries `sim=1`.

## Owed

One device run of `pipe_auto_original_slow_recover` — which is now one command, and which no tier
in this repository's default filters will do for you.


## Postscript — Original recovery is impossible from the TOP rung, and that is structural

Found while trying to raise the fixture's recovery link from 40000 to 48000 so the probe could
clear its bound. It could not, and the reason is worth more than the bound was.

`probe_due`'s first condition is `deep_reserve` — `buffered_ms >= 3 * segment`, i.e. 6000 ms at the
2 s segments this pipeline requests. `B_max(R) = lead + queue_bits/R_ES` is inversely proportional
to the rung. At rung 20000 on this plant the reserve simply cannot reach three segments: the run at
48000 climbs there and then reports `reason=shallow_reserve` for the rest of its life, having never
once opened the gate.

So **whenever the ladder has climbed to the top, Original recovery cannot even be considered** — not
declined on the comparison, not spent and lost, but never evaluated. A faster link makes this MORE
likely rather than less, which is the counter-intuitive part: it raises the rung, which lowers
`B_max`, which withholds the probe.

This is plan R2 (*"the top of the ladder is unreachable for ANY guard of this shape … `B_max ∝
1/R`"*) meeting the recovery gate, and it is a property of the plant rather than of any case.
`docs/measurements/band-20000-parked.md` is the same crossing measured directly, with the rung
pinned.

**Not addressed here.** The candidates are Phase 0's remaining levers (`AQ_VIDEO_BYTES` beyond
10 MiB, `MAX_FEED_AHEAD_NS`, deleting or re-spacing the top rungs — R15 notes four quality ties up
there already) and re-deriving `deep_reserve`'s three segments against `B_max` rather than against
the segment duration. Which of those is right is a plant-sizing decision, which is where the plan
puts it, and none of them should be picked to make one case green.
