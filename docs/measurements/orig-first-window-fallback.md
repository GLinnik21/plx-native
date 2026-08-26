# The Original path abandons direct play on its FIRST measurement window

**Found 2026-08-26, on a real film, not on the harness.** Observed on the debug install running
`5a8ef2ef` + I0. Raw log beside this file (`orig-first-window-fallback.log`, third-party server
details replaced with placeholders).

This is a THIRD instance of the collision class in `docs/adaptive-playback-plan.md` §0.3, and it is
not recorded there: §0.3(1) is the HLS first SEGMENT, §0.3(2) is the top-rung threshold pair. This
one is the **Original path's first WINDOW**, it is more expensive than either, and no test on any
tier covers it.

## What happened

A 4K Dolby Vision Profile 8 + Atmos movie (source 25 264 kbps) started in **direct play**. The
pipeline primed and bound cleanly — `primed: v=749ms a=817ms -> Play`, `SMP ACB bound`,
`acb setMediaAudioData rv=1`, `"hdrType":"DolbyVision"`, `contents.immersive=ATMOS`. Roughly one
second later:

```
auto: Original -> HLS ImminentStarvation measured=42365kbps safe=21182kbps
      need=34106kbps buf=85ms slope=113ms/s starve=0 windows=1 target=22000kbps
auto: Original became unsustainable at 21182kbps; switching to 14000kbps 1920x1080 HLS
```

Direct play of 4K DV + Atmos was replaced by a 1080p H.264/AAC transcode, permanently for that
playback.

## Why, term by term

| term | value | what it is |
|---|---|---|
| `measured` | 42 365 kbps | the link, actually measured |
| `need` | 34 106 kbps | `source_requirement_kbps` = 25 264 x 1.35 (`vbr_allowance_pm`) |
| `safe` | 21 182 kbps | **exactly half of measured** — first sample, so `uncertainty_pm` is at its 500 pm floor (`abr.rs:264-268`) and `conservative_kbps` halves it (`abr.rs:283-287`) |
| `buf` | **85 ms** | playback had just started; the 749 ms prime had been consumed by the first 750 ms window |
| `slope` | **+113 ms/s** | **the reserve was GROWING** |
| `windows` | **1** | the FIRST measurement window |

The link carried 1.24x what the file needed. Halving it manufactured a deficit that did not exist;
`T = B*R/(R-C)` with B = 85 ms then gives a horizon of ~0, which trips
`secs <= starvation_fallback_secs` and returns `OriginalExit::ImminentStarvation`
(`abr.rs:875-880`). That branch is a **hard guard**: no utility check, no anti-flapping veto, and
no persistence requirement — unlike `SustainedDeficit` below it, which requires
`ORIGINAL_DEFICIT_WINDOWS` windows. It can therefore fire on window 1, and did.

## The same 1.35 is then applied a second time, inversely

`original_fallback_rung` divides the ALREADY-discounted rate by `vbr_allowance_pm` again:

```
42 365  measured
x 0.50  first-sample uncertainty floor      -> 21 182
/ 1.35  vbr_allowance_pm, applied inversely -> 15 690
-> best rung at or below                    -> 14 000
```

A 42 Mbit/s link became a 14 Mbit/s rung: **3.0x below what was measured**, none of it from an
observed problem. (The log line's `target=22000` is the pre-fallback intent; the committed rung is
the 14 000 the second line names.)

## Why it never came back

`observe_probe` requires `conservative_kbps() >= source_requirement_kbps`, so recovery needed
34 106 / (1 - u). The probes measured 33 623 and 32 521 kbps and were discounted to 16 811 and
22 764 — refused by 17 295 and 11 342 kbps. At the 200 pm asymptote this link would have to measure
**42 632 kbps, 1.69x the source**, to be allowed to play a file it was already carrying. Probes 3-5
returned `0kbps complete=0` (the server had begun refusing the part) and were correctly discarded
rather than folded into the estimate (`abr.rs:609-614`).

## Why it matters beyond this one film

* It is the **most expensive** instance of the class: §0.3(1) costs one rung and is recoverable;
  this abandons a playback MODE, takes 4K DV and Atmos with it, and the recovery gate then makes
  the decision effectively irreversible on any link under 1.69x the source.
* The shared root of all three is one sentence: **the first measurement window is taken while the
  buffer is definitionally near-empty, and a hard guard reads that as an emergency.**
* The reserve was RISING. Any test of sustainability that looked at the buffer's direction rather
  than at a discounted rate against an inflated requirement would have stayed.

## Status

Evidence only. No code changed. No threshold proposed here — the numbers above are measurements,
not a recommendation, and the plan of record's rule stands: the next controller change is derived
from the calibrated plant model, not from this incident.
