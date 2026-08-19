# Backdrop blur: baseline graph and HWCNT measurement

This note describes the `backdrop-blur` branch before the direct-render experiment. It is the
baseline against which that experiment must be compared; none of the profiling changes below
alter the release render path. Development timing and counter runs use separate triggers.

## Current render graph

The renderer is immediate-mode. The opaque UI page is drawn into framebuffer 0 first. The first
glass surface then calls `gfx::draw_blur_backdrop`, which may refresh the one shared cached blur
chain before drawing that surface.

```text
framebuffer 0: normal full-resolution UI (1920x1080 authored and drawable on the target)
    |
    | glCopyTexSubImage2D: aligned requested region only, copied into (0,0)
    v
grab texture: RGBA, full drawable allocation 1920x1080, no FBO
    |
    | reduction 1: fs_img, bilinear exact 2x, clear + region viewport
    v
mid texture/FBO: RGBA 960x540 allocation, live viewport rw/2 x rh/2
    |
    | reduction 2: fs_img, bilinear exact 2x, clear + region viewport
    v
a texture/FBO: RGBA 480x270 allocation, live viewport rw/4 x rh/4
    |
    | Kawase 1: fs_blur, offset 1.5 quarter-resolution texels
    v
b texture/FBO: RGBA 480x270 allocation, live viewport rw/4 x rh/4
    |
    | Kawase 2: fs_blur, offset 3.5 quarter-resolution texels
    v
a texture/FBO
    |
    | up-filter: fs_blur, offset 1.25 half-resolution texels
    v
mid texture/FBO: final cached blurred snapshot at half axis resolution
    |
    | fs_glass: one full-resolution panel quad, including refraction/rim/dither
    v
framebuffer 0
    |
    | fs_src frost gradient, then widget foreground (text/icons/rows)
    v
framebuffer 0 -> swap
```

The two reductions use `fs_img`, not `fs_blur`; the Kawase and up-filter passes use `fs_blur`.
There is no separate full-resolution FBO for the UI. `grab` is a full-size texture populated from
framebuffer 0, and all allocated targets are reused for the process lifetime. Region limiting is
implemented with smaller lower-left viewports and UV windows; it does not reallocate targets.

The twelve phases this note is about are `profile.empty`, `frame.ui`, `main.ui`, `blur.copy`,
`blur.reduce1`, `blur.reduce2`, `blur.tap1`, `blur.tap2`, `blur.up`, `glass.composite`,
`glass.frost` and `glass.foreground`. They are **not the whole set** — there are 25, the rest being
per-section phases on Home and the detail page — and `ui::profile::PHASES` is the authority. A
trigger naming something outside that list is now refused with the valid names, rather than arming
a profiler that silently never matches. `profile.empty` issues no GL work and measures the
per-query floor, which is not zero on this driver and must be quoted beside every result.

## Region and coordinate rules

Each panel's resting bounds are expanded by `BLUR_MARGIN = 88` authored pixels: `BLUR_REACH = 68`
plus the maximum 20-pixel popover entry slide. `BLUR_REACH` itself is 38 pixels of maximum lens
displacement plus approximately 24.5 pixels of filter support, rounded up. Cached containment tests
need only the 68-pixel sampling reach because the 20-pixel slack exists to keep the entry animation
inside the first capture.

Every drawn glass surface contributes its expanded bounds to `BLUR_WANT_CUR`. At frame end that
union becomes `BLUR_WANT_PREV`; the next frame's first refresh unions the previous complete set
with the current caller. Adjacent surfaces therefore converge to one capture, while far-apart
surfaces enlarge the rectangle through the space between them.

The authored union is mapped to drawable pixels, rounded outward and aligned to four pixels so two
integer halvings remain registered. `glCopyTexSubImage2D` flips the authored top-origin Y into GL's
bottom-origin window Y. Each FBO pass flips storage orientation once through `vs_img`; the five-pass
chain ends top-down (`bottom_up == false`).

`fs_glass` maps a panel rect into the live subregion of the half-resolution `mid` texture. Its lens
distance remains authored pixels. `u_uvpx = live_texture_span / authored_region_size`, with the V
sign changed only when pass parity leaves the target bottom-up, converts the 38-pixel displacement
to the correct cropped-texture UV offset.

## Cache and refresh policy

`Glass::CACHED` invalidates on activation and then reuses the blurred `mid` texture until explicit
page invalidation or a region containment miss. `Glass::DYNAMIC` applies its modal dim to the opaque
Home source render, keeps drawing the glass material every presented UI frame, and invalidates a
dirty backdrop at most every third successful present. Skipped idle-loop iterations do not advance
that clock. The current Account panel is 440 pixels wide and 120..440 pixels tall; its exact height
depends on the measured row set.

## Measurement modes, and what each one can actually measure

Both modes were run on the dev television (Mali-T820 MP2 r1p0, DDK r12p0, webOS 4.5) on
2026-08-19. Everything below is measured, not predicted, and it revises what the first draft of
this note assumed.

### The timer-query path works, with one hard structural limit

`GL_EXT_disjoint_timer_query` **is** advertised by this driver, with all six entry points and
`GL_QUERY_COUNTER_BITS_EXT = 64`. The extension string and the entry points are present in
`/usr/lib/libmali.so.0.1`; the app resolves them and collects results with no disjoint intervals
across thousands of samples. Put one exact phase name in `/tmp/plxnative-profile` (empty selects
`frame.ui`) and retrieve `/tmp/plxnative-gputime.jsonl`:

```sh
make fetch-profile
tools/analyze-gputime.py pkg/plxnative-gputime.jsonl --phase blur.up --discard 60
```

**The limit: it can only see work rendered into an FBO, never work rendered into framebuffer 0.**
Midgard defers a render target's fragment work until that target's pass is flushed. An FBO pass is
flushed when the next target is bound, so it lands inside its own interval; framebuffer 0 is not
resolved until the swap, which is always outside the phase. Measured, on one scene (Account panel
over a Home grid swept by `plxnative-homeosc`, `plxnative-noidle` armed), p50 in ms:

| phase | target | before the flush fix | after |
|---|---|---|---|
| `blur.reduce1` | FBO | 0.001 | **0.329** |
| `blur.reduce2` | FBO | 0.129 | **0.127** |
| `blur.tap1` | FBO | 0.153 | **0.154** |
| `blur.tap2` | FBO | 0.158 | **0.160** |
| `blur.up` | FBO | 0.486 | **0.482** |
| `blur.copy` | reads fb 0 | 0.001 | 0.001 |
| `glass.composite` | fb 0 | 0.001 | 0.001 |
| `glass.frost` | fb 0 | 0.001 | 0.001 |
| `glass.foreground` | fb 0 | 0.001 | 0.001 |
| `profile.empty` | none | 0.323 | 0.131 |

`gpu_timer::phase` now issues a `glFlush` before `glEndQueryEXT`. That is what moved `blur.reduce1`
from 0.001 to 0.329 — it was the one FBO pass whose flushing bind fell outside its own phase. It
does nothing for the framebuffer-0 rows and cannot: `glFlush` submits queued commands but does not
make a tiler resolve a render pass that is still open. **A 0.001 ms reading is the signature of an
unmeasurable phase, not of a free one.** `profile.empty` is the noise floor and must be quoted
beside any phase result; a phase whose p95 is under the floor measured nothing.

### `frame.ui` measures the frame PERIOD, not GPU time

Do not read whole-frame timer numbers as GPU cost. The same scene, with and without the glass
panel, `plxnative-noidle` armed so presents are continuous:

| leg | `fps=` (profiler ARMED — see below) | `frame.ui` p50 |
|---|---|---|
| Home grid, no glass | **60** | 16.63 ms |
| Account panel over it | **45–50** | 20.7 ms |

> **Those two `fps=` values belong to the INSTRUMENT, not to the app.** `frame.ui` brackets every
> frame with two `glFinish`es. With nothing armed, the same scene holds **60 fps in both legs** —
> measured later against a glass-absent control interleaved in the same rounds, and reproduced
> independently three times. Arming HWCNT drops a 60 fps control leg to 45 on its own. **Never
> quote `fps=` from a run with either profiler armed**; take pacing in a separate, unarmed run.

16.63 ms is one 60 Hz period to within 0.04 ms, held to ±0.01 across consecutive windows; 20.7 ms
is one period at the frame rate that leg actually achieved. The query spans the vsync wait, so it
reports whatever the frame period happens to be. `main.ui` behaves the same way and additionally
*rose* from 15.7 to 25.6 ms when the flush was added, because the flush breaks the pipeline inside
an interval that was already dominated by waiting.

**The honest whole-frame measurement is the heartbeat's `fps=`, not the timer.** On this scene the
Account glass costs 60 fps → 45–50 fps, i.e. about 4 ms of real frame time. The five FBO blur
passes account for 1.25 ms of that (0.66 ms of which is five instances of the query floor), so the
majority sits in the parts the timer cannot see: the `glCopyTexSubImage2D` capture, the render-pass
split it forces on the tiler, and the full-resolution `fs_glass` and frost quads.

### HWCNT is the attribution instrument, and its unit is CYCLES

Put one exact phase name in `/tmp/plxnative-hwcnt`, launch the same warmed scene, retrieve
`/tmp/plxnative-hwcnt.jsonl`. Never arm both triggers in one run — `app.rs` refuses if both are
present.

```sh
tools/analyze-hwcnt.py pkg/plxnative-hwcnt.jsonl --phase blur.copy --discard 10
```

The reader is validated independently by `tools/mali-hwcnt-probe.c` (`make mali-hwcnt-probe`); its
target contract is UK 10.2, API 1, layout 5, 1280-byte dumps, 16 buffers, a 20480-byte mapping that
is exactly five 4096-byte target pages. `dump_size = (2 + nr_l2 + fls64(core_mask)) * 64 * 4`, and
on this MP2 part the five dump blocks are job manager, tiler, one MMU/L2 slice, shader core 0,
shader core 1 — confirmed against `patch_dump_buffer_hdr_v5` in Arm's `mali_kbase_vinstr.c`. Arm's
vinstr zeroes each client's accumulation buffer after every user-visible dump, so a sample is
already the delta since the previous `DUMP` ioctl and must not be differenced again.

Four things to hold onto when reading counter output:

- **Report cycles.** There is no GPU clock node anywhere in this TV's sysfs — no
  `/sys/class/devfreq` entry, no frequency file under `/sys/devices/platform/mali.0`. Cycles cannot
  be converted to milliseconds here, so compare cycles between legs and do not invent a clock.
- **Words 0..3 of every block are the block header, not counters.** Word 2 is `PRFCNT_EN`, and each
  of its bits enables a group of four counters. The profiler logs all five masks once per run.
  On this television they are jm `0xff`, tiler `0x1f`, l2 `0xffff`, sc0/sc1 `0xffff` — so **tiler
  words 20..63 are switched off in hardware and `TILER_ACTIVE` (word 22) is a structural zero**, not
  an idle tiler. Without the mask line those two are indistinguishable.
- **The counters are GPU-global.** There is no context filter in the ABI, and `surface-manager`
  composites on the same GPU every frame. A phase interval attributes the compositor's work to the
  phase. Design runs as a control-leg difference, not an absolute.
- **Shader counters are summed across both cores**, so shader cycles can exceed elapsed cycles.

Counter names are the reviewed T82x subset from Arm's r12p0 table, cross-checked against three
independent kernel trees and Arm's own gator daemon. `L2_EXT_READ` and `L2_EXT_WRITE` were wrong in
the first implementation (words 45 and 49; the correct words are 48 and 50 — 45 is reserved and 49
is `L2_EXT_READ_LINE`, a read counter that was being reported as writes). `tools/analyze-hwcnt.py`
carries the same table so archived JSONL can be re-decoded, and a host test asserts the two lists
are identical, because the raw words outlive the build that captured them.

### Run design

Run one phase per leg, on a warmed scene, and discard the leading samples. The blur chain only
executes on a **refresh**, and `Glass::EveryThirdPresent` only invalidates when the underlay
actually changed — so on a settled Home the blur phases never sample at all. Pair `plxnative-acct`
with `plxnative-homeosc` (a moving underlay) and `plxnative-noidle` (continuous presents), which
yields about 20 refreshes a second. Collect production frame pacing, p50/p95/worst frame and
presented FPS in a separate run with both triggers absent.


## The direct-render experiment: result

`/tmp/plxnative-blurdirect` (empty = 1/4 per axis; a power of two ≥ 4 selects the divisor) replaces
the capture path's `glCopyTexSubImage2D` + two reductions with a second render of the page, drawn
directly into the quarter-resolution tap target. The capture path stays the default, so the two are
an A/B on one binary.

### How the scene reaches a cropped, scaled target

No shader changed. Both vertex shaders map an authored pixel with `ndc = px / u_screen * 2 - 1`,
which does not depend on the target, so a scaled, negative-origin viewport places any sub-rectangle
of the canvas anywhere in any target:

```text
glViewport(-rx/S, -(gh - ry - rh)/S, gw/S, gh/S)   glScissor(0, 0, rw/S, rh/S)
```

The scene shaders' Y flip makes the direct render bottom-up — the same orientation a window copy
produces — and the chain that follows is three passes where the capture path runs five. Both are
odd, so the snapshot is top-down either way and `bottom_up` stays `false`.

### What had to change for the page to be drawn twice

The page draw is a pure function of UI state: it steps no spring (`home_draw` builds its `Env` with
`dt = 0`), starts no fetch, and every cache it touches hits on the second call. What is not safe is
the **global GL and renderer state its callees assume**, and each of these is a correctness fix, not
a nicety:

- `gfx::clip_set` built its scissor from `surface::viewport()`/`scale()` — the default framebuffer's
  geometry — and `clip_clear` was a bare `glDisable(GL_SCISSOR_TEST)`. Inside the source pass every
  `Painter::clip` would clip a different part of the picture and then throw away the region clamp.
  Both now consult a render-target override, which is the same `(vx, vy, scale)` triple the viewport
  took.
- `home_draw` contains a glass owner of its own (the tab track). `draw_blur_backdrop` now refuses
  outright during a source pass, so it cannot re-enter `blur_snapshot` — which would copy
  framebuffer 0 while an FBO is bound, then rebind framebuffer 0 mid-pass.
- `home_draw` opens with `ui::guard`, which **catches** a panic and returns normally. The restore is
  therefore a `Drop` guard: nothing else in the app ever binds framebuffer 0, so a panic that
  skipped it would leave every later frame rendering into a 480x270 texture, with no crash and no
  log line.
- The page's own profiler phases and the framedrop card counters are suppressed during the pass, or
  they record twice per frame under one name.

### Measured on the television

> **CORRECTED 2026-08-19 by a five-agent study; two load-bearing claims below are WRONG.** They are
> left in place because they are a dated record of how the error was made, and the error is
> instructive. **(1) The direct path is worth −3.3% of the frame, not −0.21%.** The −0.21% is a
> MEDIAN of `frame.ui`, and on this scene the median is structurally blind to the blur: the chain
> runs on ~28% of presents, so the median reports frames in which it never executed. Classify frames
> exactly by `FRAG_NUM_TILES` — 4096 = no refresh, 6050 = capture refresh, 4912 = direct refresh at
> 1/4, nothing in between — and report the MEAN. Three agents in three sessions got −3.46% / −3.25% /
> −3.55%, and the marginal cost of one capture refresh agrees to 0.2% across four independent
> sessions (1,877,317 cycles). **(2) "The cap is not the GPU" is true at 50 fps and FALSE at 60.**
> Every leg supporting it had a profiler armed, and `frame.ui` brackets each frame with two
> `glFinish`es; with nothing armed the same scene holds 60 fps. Pairing each leg's cycles/frame with
> its profiler-free `fps=` brackets the sustainable ceiling to **(646M, 693M] GPU_ACTIVE cycles per
> second** — a 60 fps budget of **10.8M–11.6M cycles per frame** — with no leg in between: every leg
> holding ≥59.5 fps needed ≤646M/s, and both legs that fell needed ≥693M/s. The shipped glass
> configuration sits at **92–98% of that budget**, so real headroom is 1.7–8.4%, not 14%. A
> capture-path refresh frame costs 11.9M cycles, i.e. **111% of a vsync**, which is why refreshing
> every present drops to 54.7 fps; the same frame on the direct path costs 10.7M (99.5%) and holds
> 60.0. **Never quote `fps=` from a run with either profiler armed.**


**Correctness first.** With the hero pinned (`plxnative-heroidx`) so rotation cannot confound it,
the two paths are pixel-near-identical: mean absolute difference 0.01/765 over the frame, **maximum
3/255, confined entirely to the glass panel**, and every pixel outside the blur region bit-identical.

**Whole-frame GPU work**, HWCNT `frame.ui`, medians over the sample distribution (not over log
lines), three interleaved repeats per leg. Run-to-run spread within a leg is 0.06–0.09%:

| scene (region) | capture | direct | delta |
|---|---|---|---|
| Account panel (608x396) | 10,107,885 | 10,086,395 | −0.21% ← **WRONG, a median** |
| Account + glass tab bar (1324x456) | 10,453,654 | 10,428,664 | −0.24% ← **WRONG, a median** |

**Both rows are medians, and on this scene the median is structurally blind to the blur.** The
chain runs on ~28% of presents, so the median reports frames in which it never executed at all.
Classify frames EXACTLY by `FRAG_NUM_TILES` — 4096 = no refresh, 6050 = capture refresh, 4912 =
direct refresh at 1/4, nothing in between — and take the MEAN. Three agents in three sessions then
get **−3.46% / −3.25% / −3.55%**, and the marginal cost of one capture refresh agrees to 0.2%
across four independent sessions (1,877,317 cycles). **The direct path is worth −3.3% of the
frame.** The rows are left here because the error is the instructive part: an earlier revision
claimed 3.7% from reading `PROFILE` log tails, this revision claimed 0.21% from the right file with
the wrong statistic, and only the third attempt was both. The whole-frame quads and
tiles are identical on both paths (1,953,471 and 4,096), which is the simplest statement of why:
the source stage is a small enough slice of the frame that replacing it does not move the total.

The source stage itself really is 4.5x cheaper (see below); it is simply a small slice. The
per-phase HWCNT figures are `glFinish`-serialized and therefore overstate what an un-serialized
frame saves — by about 8x here, which is worth remembering before pricing any pass from a phase
number alone.

**What the glass costs at all**, same scene, control leg with the panel absent:

| leg | GPU_ACTIVE / frame | delta |
|---|---|---|
| Home scrolling, no glass | 8,955,222 | — |
| + Account glass panel | 10,104,204 | +1,148,982 (**+11.4%**) |
| + glass tab bar as well | 10,453,654 | +349,450 (**+3.4%**) |

The main UI is the other 88.6%, at **3.65x overdraw** for 1080p.

> **"The cap is not the GPU" was FALSE, and the 50 fps was the instrument.** All three legs above
> ran with a profiler armed. Unarmed, a control leg reads 60/60/60 across six independent runs on a
> set that had been up 2 h 15 m under continuous load, so it is not thermal either. Pairing each
> leg's cycles/frame against its own unarmed `fps=` brackets the sustainable ceiling to **(646M,
> 693M] `GPU_ACTIVE` cycles per second** — a 60 fps budget of **10.8M–11.6M cycles per frame** —
> with no leg in between: every leg holding ≥59.5 fps needed ≤646M/s and both that fell needed
> ≥693M/s. The shipped glass configuration sits at **92–98% of that budget**. Real headroom is
> 1.7–8.4%, not 14%. What design should be handed instead of this paragraph is
> `glass-hardware-budget.md`, which restates the whole thing as a region-area law.

### Draw-call culling, and the two modes

A scissor bounds what a source pass may write but does not stop a tile-based GPU binning geometry or
walking tiles, so `gfx::culled` skips any quad whose authored bounds miss the region, at every draw
primitive and both text sites. It is exact rather than conservative: the backdrop is a crop of the
page, so a quad that misses the region cannot contribute a fragment to it.

`blur.scene` measures **bimodally**, and the two modes are now identified. Sample by sample across
three runs, 2.8% / 4.5% / 5.7% of samples land in an expensive mode of ~2.0M cycles and 2076 tiles —
about what processing the whole 480x270 target costs — and the rest sit in a steady mode of ~155k
cycles and 36 tiles. The expensive samples are **the first ten of a run, plus roughly one in a
hundred thereafter**: warm-up, and the frames where new artwork lands and the page genuinely redraws
more than the region. There is nothing in between, so a **mean over a window containing both names a
value that never occurred** — which is exactly how two opposite and equally wrong conclusions were
drawn from single log lines. Both profiler summaries now print `SPREAD=<n>x` beside the mean whenever
the window's max exceeds twice its min, and say in the line itself that the mean is not
representative.

Steady-state source stage, medians, floor `profile.empty` = 73,410 cycles:

| stage | GPU_ACTIVE | net of floor | tiles |
|---|---|---|---|
| capture: `blur.copy` + `reduce1` + `reduce2` | 696,242 | 476,012 | 892 |
| direct, before culling | 231,414 | 158,004 | 36 |
| direct, after culling | **155,200** | **81,790** | **36** |

So culling is worth about a third, and the direct source pass is **4.5x cheaper gross and 5.8x
cheaper net** than the capture and two reductions it replaces. That is a far larger factor than the
0.21% whole-frame result, and reconciling the two is the single most important lesson in this note.
Naively: the source stage runs on one present in three, so `(696,242 - 155,200) / 3 = 180k` cycles
per frame, or **1.7%** of a 10.6M-cycle frame — eight times the 0.21% actually measured. The gap is
not a mystery, it is the serialization: every per-phase HWCNT figure is bracketed by `glFinish`,
which drains the pipeline and bills the phase for work an un-serialized frame overlaps with
everything else. **A phase-level cycle count is an attribution instrument, not a budget.** Price a
change by a whole-frame `frame.ui` A/B; use per-phase numbers only to say where the work sits.

**One earlier run (`f-scene.jsonl`) had 81% of its samples in the expensive mode and is the outlier
that produced the "2.8x worse" reading this note previously carried.** Its Home evidently never
settled. Grade a source-pass run by its expensive-sample fraction before quoting its median.

### Scale

Only divisors that are powers of two ≥ 4 are accepted: the taps ping-pong through `a`/`b`, which are
allocated at a quarter of the canvas, so 1/2 would need a second half-size target and 2 MB more.
Tap offsets are scaled by `4/S` so the authored blur radius is held fixed as the source resolution
moves — without that a scale sweep changes two variables at once.

**1/8 runs; 1/16 never has, on this television.** 1080 % 16 = 8, so the divisibility guard refuses
it and logs `blur direct: canvas 1920x1080 does not divide by 16`. Any earlier text listing 1/16 as
usable is wrong. The grading that was outstanding here is done and lives in
`glass-hardware-budget.md`: the whole usable ladder spans 0.96% of a mean frame and zero frames,
1/4 is the MATCHED sampling rate for this kernel rather than a compromise, and both neighbours are
worse — 1/8 loses 2.2% of large-scale contrast to save 0.38%, and 1/2 costs more *and* looks
rougher, because the Kawase tap offsets scale with the source while the bilinear box does not.
