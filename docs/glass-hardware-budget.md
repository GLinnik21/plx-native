# The frame budget — what this television will actually give you

**Who this is for:** whoever is designing screens for this app. It is written so you can decide,
without asking an engineer, whether an idea fits. Everything in it was measured on the real set
(LG 49SM9000PLA, webOS 4.5, Mali-T820 MP2) on 2026-08-19, in one session, with the legs interleaved.

The engineering companion is `docs/backdrop-blur-profiling.md`, which prices things in GPU cycles.
This note prices them in **frames and milliseconds**, which is the currency you spend.

---

## 1. The one-page answer

**You have 16.7 milliseconds per frame at 60 fps. The Home page with its grid moving already
spends 8.9 of them. About 7.8 ms are left, and that is the whole budget.**

Every ornament is charged by the **screen area it covers**, at a price per pixel that depends on
what kind of ornament it is:

| what you draw | price per screen pixel | 7.8 ms buys you |
|---|---|---|
| a flat photograph (the hero image) | **~4 ns** | the whole screen twice over |
| a poster card (art + rim + shadow) | **~15 ns** | ~4 poster tiles, or ¼ of the screen |
| **backdrop glass** (blur + frost + rim) | **~16–30 ns**, plus a refresh charge (§3.2) | ~1/8 of the screen |

**Glass is the most expensive thing this app can draw** — about **2x a poster card and 4–7x a
photograph, per pixel** (the two instruments used bracket it in that range) — and it is charged
**every frame it is on screen**, not once when it appears, plus a refresh charge on top.

Three consequences you can act on immediately:

* **A glass panel about a quarter of the screen (608x396) costs you 60 → 45 fps.** That is the
  shipped Account panel's size. It is affordable *because it is a modal you are looking at*, not
  because it is cheap.
* **A full-screen glass blur costs 60 → 24 fps.** That is 32.8 ms of work against a 7.8 ms budget —
  four times over — and no refresh setting changes it: refreshed every third frame or captured once
  and frozen, it measured 24 fps either way. At that size the only lever is **area** (§5).
* **The number of glass surfaces is free; their total area is everything.** One panel, two panels
  or four panels covering the same footprint all measured 45 fps. Draw as many as the layout wants.

**Nothing broke.** Pushed to four large panels with a full-screen blur refreshed every frame, the
set ran at 19 fps and kept running: no crash, no stutter cliff, no thermal collapse. There is no
edge to fall off — there is a **slope, and it starts at the first glass surface you add**.

---

## 2. How to read the numbers

`fps` is frames actually put on the panel, from the app's once-per-second heartbeat. The panel is
60 Hz, so **60 fps means "it fits" and tells you nothing about how much room is left**; that is why
§3's table also gives milliseconds.

Milliseconds are `1000 / fps` — the true average time one frame took. A **cost** is that minus the
16.67 ms a 60 fps frame is allowed. The base frame (8.9 ms) is not measured directly; it is the
intercept of the poster-card ramp in §3.3, whose straight line fits its four loaded points to within
0.5 ms and then predicts the fifth — the last card count that still holds 60 fps — to within 0.1 ms.
Treat it as good to about ±1 ms.

**Everything here was measured on Home with the grid scrolling continuously and the app's
repaint-skipping turned off**, i.e. the worst honest case for a browsing screen. A settled screen
that has stopped repainting costs nothing at all.

---

## 3. The budget

### 3.1 Backdrop glass

The "region" column is the rectangle the renderer has to snapshot and blur: **your panel grown by
88 pixels on every side**, because the glass rim bends in pixels from outside itself. You do not
control it, but it is why a small panel is not as cheap as its own size suggests.

Refresh cadence is how often the blurred snapshot behind the glass is retaken. `every 3rd frame`
is what the app ships today. `never` means it was captured once and then reused — a truly static
backdrop under a still page.

| panel (authored px) | its area | region blurred | refresh | fps | frame | **cost** |
|---|---|---|---|---|---|---|
| — none — | 0 | — | — | **60** | 16.7 ms | — |
| 300 x 200 | 60,000 | 179,000 | every 3rd | **60** | 16.7 ms | fits (≤7.8 ms) |
| 450 x 300 | 135,000 | 298,000 | every 3rd | **60** | 16.7 ms | fits, only just |
| **1148 x 76** (the tab bar) | 87,000 | 334,000 | never | **60** | 16.7 ms | fits (≤7.8 ms) |
| **1148 x 76** (the tab bar) | 87,000 | 334,000 | every 3rd | **45–46** | 21.7–22.2 ms | **+13 ms** |
| 608 x 396 (Account-panel size) | 241,000 | 448,000 | never | **60** | 16.7 ms | fits (≤7.8 ms) |
| 608 x 396 | 241,000 | 448,000 | every 6th | **52** | 19.2 ms | +10.4 ms |
| 608 x 396 | 241,000 | 448,000 | every 3rd | **45** | 22.2 ms | +13.4 ms |
| 608 x 396 | 241,000 | 448,000 | every 2nd | **40** | 25.0 ms | +16.1 ms |
| 608 x 396 | 241,000 | 448,000 | every frame | **47** | 21.3 ms | +12.4 ms |
| 960 x 540 (a quarter panel) | 518,000 | 813,000 | never | **44** | 22.7 ms | +13.9 ms |
| 960 x 540 | 518,000 | 813,000 | every 3rd | **36** | 27.8 ms | +18.9 ms |
| 960 x 540 | 518,000 | 813,000 | every frame | **36** | 27.8 ms | +18.9 ms |
| 1324 x 456 | 604,000 | 948,000 | every 3rd | **36** | 27.8 ms | +18.9 ms |
| **1920 x 1080** (full screen) | 2,074,000 | 2,074,000 | never | **24** | 41.7 ms | **+32.8 ms** |
| 1920 x 1080 | 2,074,000 | 2,074,000 | every 3rd | **24** | 41.7 ms | +32.8 ms |
| 1920 x 1080 | 2,074,000 | 2,074,000 | every frame | **19** | 52.6 ms | +43.8 ms |
| 4 panels of 900 x 520 | 1,872,000 | 2,074,000 | every frame | **19** | 52.6 ms | +43.8 ms |

**How many surfaces you draw does not matter.** One 608x396 panel, two 292x396 panels and four
140x396 panels — same footprint, same region — all measured **45 fps**, exactly. Cost follows area
and nothing else. Split a control into as many glass pieces as the design wants.

**Small is not cheap, and the reason is the 88-pixel margin.** The tab bar is only 87,000 pixels — a
third of the Account panel — yet at the shipped refresh cadence it costs the same. §8 tested this
directly and the answer is clean: **the shape of the surface does not matter at all; the size of the
rectangle that has to be blurred behind it does.** A 1148x76 bar and a 295x295 square cover the same
87,000 pixels, but grown by 88 on every side the bar's region is 334,000 pixels and the square's is
222,000 — and that difference alone is 60 fps against 46. **A wide thin surface is expensive because
its margin is nearly all of it.**

### 3.2 The blur's refresh rate is a weak and badly-behaved lever

Read the 608x396 rows again, in order: never **60**, every 6th **52**, every 3rd **45**, every 2nd
**40**, every frame **47**.

* **Not refreshing at all is free.** A glass surface over a page that is not moving costs nothing
  beyond its own composite, and at 608x396 that fits inside the budget. So does the tab bar (§8).
* **The first refresh is most of the price.** Never → every-sixth-frame costs 10.4 ms.
  Every-sixth → every-single-frame costs 2 ms more.
* **It is not monotone.** Every-second-frame is the *worst* setting measured, worse than refreshing
  every single frame.

**Why, and it is the most useful mental model in this document.** The panel hands out a slot every
16.7 ms. A frame either fits in one slot or takes two. Your frame rate is 60 divided by the average
number of slots a frame needs — and **a slot is charged whole**. Refreshing the blur every third
frame makes one frame in three heavy: that frame takes two slots, the other two take one, four slots
for three frames, and 60 x 3/4 is exactly the **45 fps** measured. The light frames cannot give their
spare time back.

So: **do not design around "we'll refresh it slowly to save money."** Spreading the same work over
fewer, heavier frames does not help, because the overrun is rounded up every time it happens. Decide
instead whether the backdrop needs to be live at all — *that* is worth 10 ms.

### 3.3 Poster cards and photographs

| what | area drawn | fps | frame | cost |
|---|---|---|---|---|
| 4 poster tiles (300x450) | 540,000 | **60** | 16.7 ms | fits exactly |
| 5 poster tiles | 675,000 | **53** | 18.9 ms | +2.2 ms |
| 6 poster tiles | 810,000 | **48** | 20.8 ms | +4.2 ms |
| 7 poster tiles | 945,000 | **45** | 22.2 ms | +5.6 ms |
| 8 poster tiles | 1,080,000 | **40** | 25.0 ms | +8.3 ms |
| 12 poster tiles | 1,620,000 | **35** | 28.6 ms | +11.9 ms |
| 12 poster tiles, all focused | 1,620,000 | **34** | 29.4 ms | +12.7 ms |
| one full-screen card | 2,074,000 | **35** | 28.6 ms | +11.9 ms |
| one full-screen flat photograph | 2,074,000 | **57** | 17.5 ms | +0.9 ms |

**Cards are linear in area and the line is clean**: `frame = 8.87 ms + 14.7 ns x (card pixels)`,
which fits every loaded point above to within 0.5 ms and correctly predicts, to within 0.1 ms, that
four tiles is the last count that still holds 60. So:

* **A poster tile of 300x450 costs about 2 ms.** You can have four before the frame slips.
* **Focus does not change the price.** Twelve focused tiles cost 0.8 ms more than twelve resting
  ones — the focus glow and grown shadow are within measurement noise of free.
* **A photograph is nearly free by comparison** — a full-screen one costs 0.9 ms, about a
  twentieth of a full screen of cards. Big art is not what costs; *card treatment* is.
* **A poster wall the size of the panel costs about 12 ms** and lands the app at 35 fps.

**How many cards can this hardware carry, and what happens at the limit?** Directly: **four
poster tiles is the last count that holds 60 fps**, and every tile after that costs about 2 ms, so
five is 53, six is 48, seven is 45, eight is 40 and twelve is 35. The relationship is a straight
line with no knee in it — the panel's whole area in cards is 2 million pixels, which lands at 35 fps
and 96% arithmetic-pipe occupancy. **Nothing "happens" at the limit**: there is no stall, no dropped
input, no thermal event, no visible tearing. The frame rate just keeps falling in proportion to the
area you cover. The prediction that filling the panel with cards "would roughly double the app's
arithmetic and be the first thing capable of missing vsync" turns out to be right about the
arithmetic — a full panel of cards adds **12.7 million instruction words** to a frame that already
issues 15.5 million, so +82%, close to a doubling — and right that it is the first thing design
controls that can miss vsync. What it gets wrong is the shape of the failure: not a cliff, a slope.

### 3.4 Everything on one scale

Per screen pixel covered, on this hardware, measured:

```
flat photograph          4 ns/px    ▏
poster card (large)     10 ns/px    ▍
poster card (300x450)   15 ns/px    ▋      <- the penumbra ring costs more on small tiles
glass, full screen      16 ns/px    ▋
glass, quarter screen   27 ns/px    █▎     <- fewer pixels, so the fixed parts weigh more
```

You have **7.8 ms**. Multiply and see if it fits.

For **glass that refreshes**, add the blur of the region behind it. That is not a per-pixel price —
it is a whole extra slot on the frames it happens, which is why §3.2 says what it says. As a
planning rule: **a blurred region up to about 300,000 pixels (your panel + 88 a side) still holds 60
fps at the shipped cadence; past that you drop to the mid-40s, and it falls from there.**

---

## 4. Things the hardware simply will not do

These are not budget items. They are unavailable at any frame rate.

1. **Two pages cannot be on screen at once.** Every screen's draw begins by clearing the frame, and
   the app holds no picture of the page it is leaving. A route transition can dissolve *through*
   something (grey today, blur if you want — §5), but it can never cross-dissolve page A into page
   B. Designing a transition that shows both is designing something that cannot be built without
   rebuilding the renderer.
2. **There is exactly one blur cache.** Every glass surface in a frame samples the *same* blurred
   snapshot, taken once, of one rectangle that is the union of what all of them asked for. Two
   glass surfaces far apart therefore drag that rectangle out toward the whole screen and get
   charged for the space between them. Adjacent glass is nearly free; scattered glass is not.
3. **Glass on top of glass shows nothing new.** Because of (2), a glass surface sitting over an
   area that is already blurred samples the identical pixels: you get its tint, its rim and its
   edge refraction, but no additional blur. Giving the upper surface its own backdrop needs a
   second cache, which was built and measured: **60 → 16 fps** (§5). It is not affordable.
4. **The blur cannot see video.** Over the player, the app's own frame is transparent where the
   hardware video plane shows through, so a backdrop blur there would smear transparency, not
   picture. Glass is unavailable on the player's panels, permanently.
5. **No new render targets per frame, and no GLES3.** The renderer allocates its buffers once at
   boot; anything that would need a fresh full-screen buffer while running is out. The GPU is
   OpenGL ES 2 only — no compute, no multiple render targets, no fancy filtering.
6. **A settled screen stops repainting.** This is why the app idles at ~2% of a CPU core. Anything
   that animates forever — a shimmer, a drifting gradient, a breathing glow — turns that off and
   costs the whole frame, continuously, for as long as it is on screen.

---

## 5. The blurred route transition, answered

**The idea:** today a route change (Home → a library) dips the outgoing page to the app's grey
background, flips at the bottom, and fades the new page up. Could that grey trough be a **blur** of
the outgoing page instead, with the tab bar's glass sitting on top of it?

**It was built and measured.** `/tmp/plxnative-navblur` holds the page at full brightness and
cross-fades a full-bleed blur slab over it, with a tab-track-shaped glass capsule composited above.

**The answer is yes, at a real and quantified cost.**

| | fps | frame |
|---|---|---|
| the composition held at full strength | **27** | 37.0 ms |
| the same, with a private second blur cache for the capsule | **16** | 62.5 ms |
| a real route bounce every 1.4 s (the transition is ~210 ms of it) | **49–60**, mean 52 | — |

* **During the transition the app runs at 27 fps.** A 210 ms transition therefore plays in about
  **6 frames instead of 13**. Whether that reads as a soft blur bloom or as a stutter is a
  judgement to make on the panel, not from this table — but it is half frame rate, and you should
  expect to *see* it in the ramp.
* **Averaged over normal navigation it costs about 7 fps** — a second containing one route change
  measured 49–54 fps against 60 with nothing happening. Navigation is not continuous, so this is
  not a standing cost.
* **The tab bar's glass must ride the same cache.** Mode 2, which gives the capsule its own
  backdrop, costs 60 → 16 fps. Do not design around it.
* **What that means visually:** with one cache the capsule over the blur reads as a *tinted, rimmed
  capsule*, not as a second layer of glass, because everything under it is already blurred to the
  same degree. The capture below, taken off the television's own panel output, shows this working:
  the rim light, the lens bending at the capsule's edge and the darker scrim all read clearly, and
  the composition looks deliberate. It just is not "glass over blur" in the sense of two different
  amounts of blur — that is unavailable.

![the blurred route transition, held still, photographed off the panel](screenshots/navblur-transition.jpg)

*The prototype on the television: the whole page blurred, with the tab-track capsule composited over
it. The capsule's rim light, its edge refraction and its darker scrim are all doing their job; what
it has no room to do is blur, because what is under it is already blurred.*

**If you want it, here is how to make it cheaper**, in the order of how much it buys:

1. **Blur less than the whole screen — this is the only large lever.** Cost is close to linear in
   area, and the measured points are: a quarter of the panel (960x540) costs 13.9 ms and runs at
   **44 fps**; the whole panel costs 32.8 ms and runs at 24–27. Half the panel interpolates to about
   20 ms and **34 fps**. Note what that means: a "content band" spanning the full width with only the
   top chrome left sharp is still about two thirds of the panel, so it buys you almost nothing —
   the saving has to come from blurring a *region*, not a *band*.
2. **Shorten it.** The cost is per frame, so a 140 ms transition pays it for four frames instead of
   six. Given the frame rate during it, a shorter, more decisive move is also the safer look.
3. **Keep the frost sheet off the slab.** The prototype already does; it is worth roughly 4–5 ms at
   full screen. A transition slab is not a popover and does not need a frosted sheet over it.
4. **Freezing the blur during the fade saves nothing at full screen** — and this is worth saying,
   because it is the obvious economy and it does not work here. Captured-once and
   refreshed-every-third-frame both measured 24 fps at full screen: once the surface is that big the
   per-frame composite is the whole bill and the refresh disappears into it. Freezing *does* pay at
   panel sizes (worth ~5 ms at 960x540 and ~5.5 ms at 608x396), so it is a lever for a partial-screen
   transition, not for a full-bleed one.

None of these change the fact that a full-screen blur is over budget while it is up. **The honest
recommendation: yes, if you accept ~27 fps for the length of it.** There is no arrangement of
cadence, caching or frost that makes a full-bleed blur cheap; only shrinking it does, and shrinking
it enough to matter (down to a quarter of the panel, 44 fps) stops it being the effect you asked
for. So the real choice is between a **short full-bleed blur at 27 fps** and **no blur**. Six frames
of a low-frequency image ramping is not obviously a bad six frames — a blur is exactly the kind of
picture that hides temporal steps — but that is a judgement to make in front of the set, not in this
table.

---

## 6. Where the cliff is, and what is actually binding

**There is no cliff.** Load was escalated from nothing to four large glass panels over a
full-screen blur refreshed every single frame. The frame rate fell continuously — 60, 45, 36, 24,
19 — and nothing else changed: no crash, no freeze, no thermal event, no discontinuity anywhere on
the curve. The set simply runs slower.

The only edge that matters is the **60 fps boundary**, and it sits at about **7.8 ms of extra
work** — roughly a quarter of the screen in poster cards, or an eighth of it in glass.

**What binds is shader arithmetic — not memory, not the CPU, not the display.** Four independent
measurements say so:

* Frame time tracks the GPU's **arithmetic instruction count**, exactly: every leg's extra GPU
  cycles equal its extra arithmetic words divided by two (there are two shader cores, each retiring
  one word per cycle). The relationship holds within 10% across a 5x range of load.
* The arithmetic pipe is already **89.6% occupied on a plain Home frame**, and glass pushes it to
  **92.8%** (a quarter-screen panel) and **95.8%** (full screen). There is almost nothing left.
* **External memory traffic does not rise with glass at all** — a full-screen glass panel *reduces*
  external reads slightly while adding 157% to GPU cycles. Bandwidth is not the wall.
* When the frame overruns, the time is spent **inside the app's own drawing calls** (the driver
  blocking on a busy GPU), not waiting at the display hand-off, which stays at 0.2–0.4 ms
  throughout. The CPU-side work — input, data, layout — never appears.

Per-pixel arithmetic, measured directly:

| | arithmetic words per pixel | GPU cycles per pixel |
|---|---|---|
| a gradient or scrim | ~0.004 | 0.025 |
| flat photograph | 1.94 | 1.12 |
| poster card | 6.12 | 3.35 |
| **backdrop glass** (its composite plus its frost sheet) | **14.77** | **7.41** |

(The card and photograph figures were arrived at independently, by a different person on a different
scene with a different instrument, as 5.86 and 1.99 words per pixel. Two methods agreeing to 5% is
why these are worth quoting. The gradient row is that other measurement's.)

**The design reading:** the price of an effect on this television is the number of *maths
operations per pixel* its shader performs, multiplied by the pixels it covers. Blur-and-refract is
the most arithmetic-heavy thing in the app. Gradients, scrims and flat fills are, by comparison,
free — the app's existing washes and ramps cost 0.025 cycles per pixel, three orders of magnitude
below glass. **If an effect can be expressed as a gradient, it is free. If it has to sample its
background and bend it, it is not.**

---

## 7. Was the 50 fps thermal? No — and the earlier reading was an instrument artefact

The record carried a worrying claim: the same scene had been seen at 60 fps early in a session and
50 fps late in it, with no workload change, which would mean the frame budget design gets to spend
is set by the enclosure rather than by the renderer.

**That is not what this set does.**

* The control leg — Home with the grid scrolling, repaint-skipping off — measured **60 fps, with a
  minimum of 60 and a maximum of 60**, in every run, across a session in which the set had already
  been up **1 h 42 m** when the first measurement ran and **2 h 15 m** when the last one did, under
  continuous load from five agents throughout.
* Within each 2-minute run the drift (last third minus first third) was **0.00 to 0.50 fps** on
  every configuration, loaded and unloaded alike. Nothing decays.
* The same configurations reproduce across runs taken 40 minutes apart: 608x396 glass at the
  shipped cadence measured 45 fps in five separate legs, and the control read 60 in six.

**Where the 50 came from.** Turning on the GPU counter profiler drops the *control* leg from 60 fps
to 45 and compresses every leg toward the middle — it inserts a full pipeline drain at each frame
boundary, which costs a cheap frame ~5 ms and an expensive one nothing. The archived "50 fps in all
three legs" numbers came from profiled runs. **Frame rates read off a profiled run are not frame
rates**; they belong to the instrument. This note's fps figures were all taken with both profilers
disarmed.

**Confidence, stated honestly.** I could not take a genuinely cold measurement — the set had been
running for hours and other agents were using it continuously, and it exposes **no temperature
sensor and no GPU clock at all** (there is no thermal zone and no frequency node anywhere in its
`/sys`; this was checked, read-only, at the start and end of every batch). So I cannot say what a
set that has been off overnight does. What I can say is that **a set this warm shows no decay and
holds a clean 60**, which removes the reason to design against a 50 fps ceiling.

---

## 8. Does the shape of a glass surface change its price?

The tab bar (87,000 px) costs what a 241,000 px panel costs, while a 135,000 px panel is free. Area
alone does not explain that, so it was tested directly: three glass surfaces of the **same area**
(~87,000 px) and different shapes, with the refresh turned off entirely so only the per-frame
composite is being measured.

| surface | area | region blurred | refresh | fps |
|---|---|---|---|---|
| — none — | 0 | — | — | **60** |
| 1148 x 76 (a bar) | 87,248 | 334,000 | never | **60** |
| 295 x 295 (a square) | 87,025 | 222,000 | never | **60** |
| 600 x 145 (in between) | 87,000 | 249,000 | never | **60** |
| 1148 x 76 (a bar) | 87,248 | 334,000 | every 3rd | **46** |
| 295 x 295 (a square) | 87,025 | 222,000 | every 3rd | **60** |

**Shape costs nothing.** All three shapes are free when the backdrop is not refreshing — the glass
material charges by pixels covered and does not care what outline they form.

**The blurred region is everything.** The same two surfaces, once the backdrop starts refreshing:
the square (222,000 px of region) stays at 60, the bar (334,000 px) falls to 46. A wide thin control
pays for a rectangle far larger than itself, because the renderer must blur 88 pixels beyond every
edge for the glass rim to have something to bend.

**Two rules fall out of this, and they are the ones most likely to change a layout:**

1. **Compact glass is cheap glass.** Prefer a chunky panel to a long thin strip. If a strip is what
   the design wants, its cost is set by its *length*, not its area.
2. **The shipped tab-bar glass is affordable exactly when the page under it is still.** Over a
   settled screen it is free; over a scrolling grid it costs 60 → 46 fps.

---

## 9. How this was measured

**The instrument.** A load dial (`rust-modules/src/ui/glassload.rs`, `/tmp/plxnative-glassload`)
draws N surfaces of a chosen size and kind — backdrop glass, poster card, or flat photograph — over
the real Home screen, at a chosen blur-refresh cadence. It **cycles its own configurations on a
timer inside one launch**, six seconds each, repeating for the length of the run. That is not a
convenience: the correct way to compare legs on a television that may drift is to interleave them,
and a dial that has to be re-armed between legs makes that a deploy per leg. Every heartbeat and
every counter sample is stamped with which configuration was live, and
`tools/analyze-loadsweep.py` splits one log into per-configuration distributions after the fact.

**The runs.** Five locked device batches on 2026-08-19 (the television is shared, so each batch was
a deploy plus its measurements inside one lock — a deploy in one lock and a measurement in another
would be measuring somebody else's binary). Thirteen measurement legs across nine distinct sweeps,
6-second steps, 2–4 full cycles each, 8–16 usable heartbeat samples per configuration after
discarding the two seconds around every step change. Scene throughout: Home, focus sweeping the grid
continuously (`plxnative-homeosc`), repaint-skipping disabled (`plxnative-noidle`), so every
configuration saw the same moving underlay and presented continuously.

**To reproduce any row.** Arm `/tmp/plxnative-token`, `/tmp/plxnative-noidle`,
`/tmp/plxnative-homeosc`, and put a sweep in `/tmp/plxnative-glassload` — for example
`hold=6;off,1x608x396@3,1x1920x1080@3` — then `make run RUN_SECS=128` and read the log with
`tools/analyze-loadsweep.py`. The transition prototype is `/tmp/plxnative-navblur` (`1p:3` pins it
for a capture, `1:3` rides a real route change, `2:3` gives the upper surface its own cache). Both
triggers are absent from a `RELEASE=1` build.

**Reproducibility.** The control read 60 fps in six independent legs. 608x396 glass at the shipped
cadence read 45 fps in five. The area curve reproduced identically — 60/45/36/24/19/19 — in two
separate runs eight minutes apart, one of them additionally instrumented with the frame-drop
detector. Within-leg spread was 0–1 fps on almost every configuration, and per-leg drift (last third
minus first third) was 0.00–0.50 fps.

**Attribution.** One counter run (Mali hardware counters, whole-frame, ~600–900 samples per
configuration) supplied §6's per-pixel arithmetic and the cycles-equal-arithmetic-halved
relationship. Counter runs were never used for frame rates, for the reason §7 gives.

**Corrections made along the way.** The dial's own layout is unit-tested so a configuration that
does not fit on screen reports what it actually drew rather than what was asked for; the log line
names the drawn count and drawn area for every step. A first attempt at the surface-count question
grew the blurred region along with the count and would have blamed area on count — the sweep was
rebuilt to hold the footprint constant.

---

## 10. What I could not measure — treat as unknown, not as free

* **A cold television.** No temperature sensor and no clock are exposed, and the set was warm and
  busy throughout. §7 shows no decay over hours of continuous load, which is strong, but a
  from-standby comparison was not possible.
* **How much room is left inside the 8.9 ms base frame.** That figure is an inference from the card
  ramp, good to about ±1 ms. It is not a direct measurement, and it is specific to Home with the
  grid moving; other screens will differ, probably downward (Home's hero photograph is the single
  most expensive object in the app).
* **Whether 27 fps for 210 ms looks acceptable.** That is a judgement about motion on a panel, and
  no number in this document settles it. It needs a person in front of the television.
* **The quality of a cheaper blur.** A coarser blur source is available and costs less to refresh,
  but its appearance next to the current one has never been graded on the panel, so "make it cheaper
  by blurring more crudely" is not yet a supported option. It would in any case only help the
  refresh, and §3.2 shows the refresh is the smaller half of the bill.
* **Why refreshing every second frame is worse than refreshing every frame.** The slot model in §3.2
  accounts for the shape of the cadence curve but not for that specific inversion, which reproduced
  in two runs. It is a real effect and it is unexplained; do not build a plan on any cadence's exact
  number without re-measuring it.
* **Any configuration below the 60 fps line.** Six rows in this document read "60 fps", which means
  only "it fits". Their true cost could be anything from nothing up to the full 7.8 ms, and the
  headroom they consume is invisible until something else is added. If a design stacks two such
  things, measure the pair; do not assume two free things are free together.
* **Anything about the player.** Every measurement here is on browsing screens. The player draws
  almost nothing, has no glass, and cannot have any (§4.4).
* **Screens other than Home.** The library grid, the detail page and Search were not swept. The
  per-pixel prices in §3.4 should carry across, since they are properties of the shaders rather
  than of a screen, but the base frame each screen starts from was not measured.
* **The real tab bar in its real position.** The transition prototype's capsule stands in for it at
  the true height and place but a fixed width, and it is composited *over* the page where the real
  strip is drawn *inside* it. The costs are representative; the exact pixels are not.
