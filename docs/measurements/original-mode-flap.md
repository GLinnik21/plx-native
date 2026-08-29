# Auto abandons Original on a link that is carrying the film, three ways

**Device-observed 2026-08-29** on the dev television (webOS 4.10.2, debug install), reported by the
maintainer as *"I artificially dropped the speed. It dropped to an optimal resolution. OK. I
released the speed. It returned to original and then back to dropped quality — and because of that
the experience was poor and playback blinked 2 times."*

Reproduced and graded on the **host simulator** through a loopback `tools/netcond.py`
(`tools/abr-scenario.sh`). Nothing here is a device measurement of frame rate or decode: every
simulator heartbeat carries `sim=1`. What the simulator does run is the real controller, the real
demux, the real AU queues and the real transactions — which is all this finding is about.

## 1. The device observation

Source: **25 264 kbps, 3840x2160, Dolby Vision P8 + Atmos**, direct-playable, so Auto chose
Original. Requirement `R = 1.35 x 25 264 = 34 106 kbps`.

| | evidence |
|---|---|
| throttle on | `tx Down 4000->2000`, then `tx Down 2000->720kbps 854x480` — correct |
| throttle released | between probe #4 (`2378kbps`) and #5 (`44583kbps`) |
| recovered, ~13 s / 3 probes later | `probe #7 measured=45151kbps verdict=Some(Recover)` -> `reload_at: fresh Load at 943s` — **blink 1** |
| held Original **8 seconds** | `pos=943s..951s`, `play=985..1031pm` (full speed), reserve **749 -> 4814 ms** |
| abandoned | `auto: Original -> HLS ImminentStarvation measured=31037kbps safe=23932kbps need=34106kbps buf=4814ms slope=1020ms/s starve=16 held=2358ms` -> `reload_transcode: fresh Load at offset 951s` — **blink 2** |
| landed worse | `seed rung=16000kbps prior=4694kbps n=48`, one segment, then `tx Down 16000->2000kbps` |
| never returned | later probes at `33944` and `42012 kbps`, both `Insufficient` |

The two blinks are the two `Load`s. A starvation verdict is printed on the same line as a reserve
filling at over a second of media per second of wall clock.

## 2. Why it was not bad luck: the condition was permanently armed

`T = B*R/(R - C)` was computed on `conservative_kbps`. The live link measured 31 037 and the
discount published 23 932, so the model saw a **10 174 kbps** deficit where the true one was
**3 069** — 92 % composition rather than observation, the product of `vbr_allowance_pm` inflating
`R` and `uncertainty_pm` discounting `C`.

`T` is increasing in `B`, and `B` is bounded above by the plant ceiling
`B_max = lead + queue_bytes*8/R`, about **5.0 s** for a source this size. The device log confirms
the ceiling directly: `qbytes` pinned at the 10 MiB video cap four seconds in, and the exit firing
at `buf=4814ms` — 96 % of it. So

```
T_max = 5.0 s x 34 106 / 10 174 = 16.8 s   <   starvation_fallback_secs = 20 s
```

**The horizon half of the imminent test was satisfied on every window that playback could ever
produce, a completely full buffer included.** The reserve could not buy its way out at any level it
was physically able to reach. The stint's length was therefore determined, not unlucky: it is the
fill time, ~8.4 s predicted against 8 s observed.

That is also why the derivative guards were necessary and not sufficient. They close the CHANNEL a
permanently-armed condition fires through; the condition stays armed, and the next channel takes
over. Three did, in turn — `ImminentStarvation`, then the `SustainedDeficit` tally, then
`EmergencyLowBuffer`.

## 3. The fix already existed, for the other mode

`controller.rs`, at the HLS emergency horizon:

> Conservatism belongs to ADMISSION — a rung you have not tried might be dearer than you think, so
> plan against a lower bound. It does not belong to EVICTION, where the claim is that the link in
> front of you cannot carry what is already playing, and the evidence for that has to be observed
> rather than discounted into existence.

`OriginalModeController::observe` violated it. On the measured rate the same ceiling gives
`T_max ~ 55 s`, and the branch becomes reachable only when the link genuinely stops covering the
file.

## 4. Graded, three builds, one scenario

`movie_hevc_4k_dovi_p8` (source 15 633 kbps), legs `0:pass 45:x0.25 105:x1.5` — squeeze to a
quarter of the source, release to **1.5x**. The multiple is the point: Auto's recovery gate needs
`conservative >= 1.35 x source` and `conservative <= 0.8 x slow`, so it wants **1.6875x**, and the
interesting band is `1.0x < link < 1.69x` — fast enough to carry the film, too slow for the model
to say so. Releasing to `pass` on a gigabit LAN jumps clean over it, which is why neither shipped
`link_profile` case reaches this.

| build | visible reloads | what happens after the release |
|---|---:|---|
| unfixed | **3** | recovers into Original, leaves 5 s later via `EmergencyLowBuffer` on `buf=1181ms slope=1113ms/s starve=none` |
| derivative guards only | **2** | recovers into Original and **stays** to the end of the run |
| + measured eviction basis | **1** | correct downshift only; climbs HLS to 18000 kbps 1080p and settles |

The single remaining reload is the one that must happen: `ImminentStarvation measured=4328kbps
safe=2164kbps need=21104kbps buf=13531ms slope=-535ms/s starve=17` — a **negative** slope, a real
four-fold deficit, evicted correctly. The guards do not block a genuine collapse.

## 5. What is NOT fixed, and it is the reason row 3 does not reach Original

The **entry** gate is untouched. It requires `conservative_kbps >= 1.35 x source` while
`uncertainty_pm` has a hard floor of 200 pm, so admission needs `slow >= 1.6875 x source`
permanently — 42.6 Mbps for the device's 25.3 Mbps film. The device link was ~38 Mbps. Row 3's run
shows the refusal in one line, now that the probe prints its basis:

```
abr: Original probe #5 measured=23731kbps ... slow=27987kbps unc=304pm n=2 cons=19478kbps
     need=21104kbps verdict=Some(Insufficient)
```

`cons` is what is compared, not `measured`. Before this line carried its basis, the same event read
`measured=42012kbps ... verdict=Insufficient` against a 25 Mbit/s film — which invites exactly one
reading, and it is the wrong one.

Two aggravators worth recording because neither is obvious:

* **The probe competes with the stream it wants to replace.** It runs over the same link as the
  live HLS session, so it measures the RESIDUAL, and the residual shrinks as the ladder climbs. In
  row 3 the controller reached 18000 kbps and the probes fell with it. `ProbeBlock::NoSpareCapacity`
  already documents this for the GATE ("a controller that keeps upshifting is consuming the very
  headroom this condition looks for"); the MEASUREMENT has the same problem and nothing corrects
  for it.
* **Entry and exit are measured on different instruments.** Entry uses isolated burst probes
  (3 083 KiB in 559 ms on the device); exit uses steady-state reads under backpressure. Observed
  ratio 45 151/31 037 = **1.455**, against a nominal hysteresis margin of about 1.09. The loop's
  sign is set by an instrument bias rather than by policy.

## 6. A retraction, and the rule it buys

`docs/measurements/local-original-blind.md` §6 argued against exactly the guard this work landed —
"a guard written against `draining()` would have blocked this exit" — from a correct reading of a
line that said `slope=8446ms/s` at a correct fallback. The reasoning was sound; the premise was a
**fabricated first sample**, and `da4a245b` removed it the day after that measurement was taken.
Re-measured on the same item and profile, a correct fallback now reads `slope=-876ms/s`.

**A number read out of a log is evidence about the code that produced it, and that code has a
date.** `[[silent-instrument-trap]]`'s sibling: prove the instrument was sound *when the reading was
taken*, not merely that it is sound now.

**And one claim made during this work was itself unsupported, which is recorded here rather than
quietly dropped.** The commit message for the `EmergencyLowBuffer` guard says that fixing the
imminent branch and the deficit tally "moved the blink rather than removing it", citing the
simulator. No build with only those two fixes was ever run. The trace it cites is the UNFIXED
binary, where the emergency branch fired alongside the others rather than after them. The guard is
justified by that trace — `buf=1181ms`, `slope=+1113ms/s` and `starve=none` are all in it — but the
causal story is not, and the table in §4 is the measurement that was missing.

## 7. Still open

* The entry gate's 1.6875x floor (§5). A policy change; needs its own branch and a device leg.
* `starving()`'s `6_000` arm, plan increment I3b(a), never done: it is degenerate at the top two
  rungs, where `B_max <= 6 000` makes every reachable reserve satisfy the level test, collapsing the
  arm to `draining_samples >= 2` alone.
* Separating network acquisition from PMS production in the acquisition window: `acquisition_us` is
  end to end, and on a healthy LAN more than half of it is not link transfer.
* A device leg for the eviction-basis change. Rows 1-3 above are simulator runs; the shipped
  behaviour was verified on the television only through the 25-case server tier and the four Auto
  cases, which do not condition the link.
