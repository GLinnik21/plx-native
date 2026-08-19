# Backdrop blur: what the refresh cadence costs

`Glass::DYNAMIC` refreshes its shared backdrop snapshot on every third successful present — about
20 Hz while the UI presents at 60. This note prices that number: what each cadence costs in GPU
cycles on both source paths, what it costs in frames, and what it buys on the panel. It is a
companion to `docs/backdrop-blur-profiling.md`, whose instruments, scene and cautions it uses
unchanged.

Everything below was measured on the dev television (LG 49SM9000PLA, Mali-T820 MP2, DDK r12p0,
webOS 4.5) on 2026-08-19.

## The instrument that had to be built first

`/tmp/plxnative-glasshz=<presents-per-refresh>` moves the shared cadence: `1` refreshes on every
present, `3` is what ships, `8` is the far end. Absent, nothing runs and the period is the
compiled-in `DEFAULT_DYNAMIC_PERIOD`, so a default build is unchanged. The value clamps to 1..=8 —
zero is a refresh every zero frames — and the boot log says what was asked for and what was
installed.

**A configured cadence and the cadence that ran are different claims.** An invalidation only
schedules a capture; a containment miss takes one nobody asked for; and a bug that made the blur
invalidate every frame once read as an 11% regression of an unrelated change. So `gfx` counts
chain executions at both entries — the capture `blur_snapshot` and the direct source pass — and
under the trigger the heartbeat carries ` glass=<n> glasshz_period=<n>` beside `fps=`. The field
appears only under the trigger; every other build logs the line it always logged.

## The frame is bimodal by construction, and this is the whole trap

At one refresh in three, one frame in three carries the blur chain and two do not. `frame.ui` is
therefore two populations, and `FRAG_NUM_TILES` separates them with no threshold and no clustering:

| frame | tiles |
|---|---|
| no snapshot taken | exactly **4096** |
| capture-path refresh (`glCopyTexSubImage2D` + two reductions) | exactly **6050** |
| direct-path refresh (culled source pass at 1/4) | exactly **4912** |

Nothing lands between, so every frame of every leg is classified exactly, the refresh fraction is
**counted** rather than inferred, and the marginal cost of one refresh is the difference of two
class means taken inside one run, at one thermal state, with the frame-level `glFinish` identical
on both sides:

| | cycles | as a share of a no-refresh frame |
|---|---|---|
| a frame with no refresh | 10,027,966 | — |
| **one capture-path refresh** | **+1,877,317** | **+18.7%** |
| **one direct-path refresh** | **+703,831** | **+7.0%** |

(means over 15 and 14 clean legs; leg-to-leg spread 1.2% and 1.5%.)

**The consequence for the existing note: its whole-frame direct-vs-capture A/B was read as a
MEDIAN, and at one refresh in three the median is a frame where the blur did not run.** That is
why it came out at −0.21%: the two paths draw the identical visible frame, so on non-refresh frames
they cost the same, and 71% of frames are non-refresh frames. Reading the same runs by the MEAN —
work per frame, which is what a frame rate is spent on — the direct path is worth **−3.55%** of the
frame at the shipped cadence. Both numbers are correct measurements of different questions; the
median answers "what does a typical frame cost", and the mean answers "what does the feature cost".
For pricing a change, the mean is the one that pays the bills.

## The cost curve

Whole-frame `frame.ui` HWCNT, scene `plxnative-acct` + `plxnative-homeosc` + `plxnative-noidle`
(the established baseline scene). Three interleaved rounds — ascending, descending, shuffled — of
ten legs each, ~700 samples per leg, 60 leading samples discarded. Each cell is the median across
rounds of that round's mean; `spread` is max−min across rounds.

| period | Hz at 60 fps | capture cycles/frame | vs shipped | spread | direct cycles/frame | vs shipped | spread |
|---|---|---|---|---|---|---|---|
| 1 | 60 | **11,555,452** | **+9.16%** | 0.57% | **10,592,685** | **+0.07%** | 0.31% |
| 2 | 30 | 10,822,512 | +2.24% | 0.17% | 10,312,892 | −2.58% | 0.19% |
| **3** | **20 (ships)** | **10,585,723** | **0.00%** | 0.27% | 10,210,411 | −3.55% | 0.28% |
| 4 | 15 | 10,438,039 | −1.40% | 0.18% | 10,193,612 | −3.70% | 0.30% |
| 8 | 7.5 | 10,227,803 | −3.38% | 0.11% | 10,112,223 | −4.47% | 0.02% |

The single most useful line in the table: **a 60 Hz backdrop on the direct path costs the same as a
20 Hz backdrop on the capture path** (+0.07%, inside the leg-to-leg spread). Raising the cadence
and changing the source path are the same size of lever pointing in opposite directions.

The curve is the two-population model and nothing else — `cycles/frame = plain + refresh_fraction ×
marginal` predicts every measured leg to within 0.2%. Measured refresh fractions were 0.80 / 0.41 /
0.29 / 0.22 / 0.11 against an ideal 1/N; the shortfall is the presents on which the underlay
reported clean and no refresh was due.

## What it costs in frames

The counter runs cannot answer this — HWCNT brackets every frame with `glFinish` and its `fps=` is
the serialized rate (~40). Two further interleaved rounds ran with no profiler armed at all, eleven
legs each, including a control with the glass absent. Mean `fps=` per leg, first three heartbeats
discarded:

| period | capture fps | direct fps |
|---|---|---|
| 1 | **54.8 / 54.7** | 60.0 / 60.1 |
| 2 | 59.5 / 59.9 | 59.9 / 59.9 |
| 3 | 60.1 / 59.8 | 59.9 / 60.0 |
| 4 | 60.1 / 59.9 | 60.0 / 60.0 |
| 8 | 59.8 / 60.1 | 59.9 / 59.7 |
| — | glass absent: **59.9 / 60.1** | |

**Nine of the ten configurations hold 60 fps, including the glass itself.** The one that does not
is the capture path at one refresh per present, which loses about a tenth of its frames (54.7)
whether it runs first in a round or last. So the frame cliff sits between 30 Hz and 60 Hz on the
capture path, and nowhere at all on the direct path.

This also revises a line in the existing note. "The Account glass costs 60 fps → 45–50 fps" was
measured with the timer profiler armed; with nothing armed, the Account glass at the shipped
cadence costs **zero frames** — 60.1 and 59.8 against a glass-absent control of 59.9 and 60.1, in
the same interleaved rounds.

## What the faster cadence buys, honestly

**A still capture cannot answer this, and the reason is worth writing down.** The panel capture
service takes seconds per still, and `homeosc` reverses on a 3 s period, so four stills at 20 Hz
and four at 60 Hz aliased onto the same phase of the sweep: two of them are pixel-identical
across different cadences and different app launches, and every same-phase pair diffs to zero.
That is a statement about the sampling, not about the cadence.

The app's own UI capture stream is the instrument that can see it: ~20–26 frames a second of the
UI plane, which is the plane the glass and its backdrop live on. 120 frames at each cadence, over
the same scene, differencing consecutive frames inside the panel and over the page beside it:

| | period 1 (60 Hz) | period 3 (20 Hz, ships) |
|---|---|---|
| sampled steps where the PAGE moved | 21 / 119 | 20 / 119 |
| …of those, the BACKDROP also moved | **21 (100%)** | **10 (50%)** |
| backdrop step size when it moves (mean abs luma over the panel) | 1.40 (max 4.62) | 2.02 (max 5.95) |
| page step size on the same frames | 15.03 (max 31.69) | 14.87 (max 30.72) |

Three things follow. The cadence does what it says: at 60 Hz the backdrop tracks every page step,
at 20 Hz it tracks about half of the ones this sampling rate sees. The step is small where it
matters — a page step that moves the sharp page by 15/255 moves the blurred, source-dimmed backdrop
by 2/255, which is what a quarter-resolution Kawase blur plus a 0.5 dim does to motion. And the
scene is mostly still: the page moved in only ~17% of sampled steps, because the app's one dynamic
glass surface today is a popover over a hero that does not itself scroll.

What this cannot say: the stream samples 20–26 of the app's 60 frames a second, so it counts how
often the backdrop tracks the page but cannot resolve the exact three-frame stepping pattern, and
no still frame can show judder. The bound that *is* certain is arithmetic — at 60 fps and one
refresh in three, the backdrop a frame shows is at most 33 ms old, and each skipped step is worth
about 2/255 mean-abs inside the panel.

## Recommendation

**Keep three. Do not raise the cadence on the capture path, and do not lower it.**

- 60 Hz on the capture path is the only configuration measured here that costs frames: +9.2% GPU
  work per frame and 60 → 54.7 fps, to make a backdrop track a page that moves in bursts and whose
  motion the blur attenuates eightfold. That is the worst trade in the table.
- 30 Hz on the capture path (+2.24%, no frames lost) is affordable but buys the same small thing;
  15 Hz (−1.40%) and 7.5 Hz (−3.38%) save real cycles and are the interesting direction if this
  scene ever gets tighter, but 7.5 Hz is a visibly stepping backdrop the moment a page does scroll
  behind glass, and nothing here measured that case.
- **If the direct source path ships, 60 Hz becomes free** — +0.07% against today's shipped
  configuration, and 60 fps held. That is the honest way to state the direct path's value: it does
  not save much at the cadence we run today, it removes the reason not to raise it.

The trigger stays in the tree because the curve above is a property of one scene on one television,
and the next scene with a genuinely moving underlay behind glass will want it re-measured rather
than re-argued.
