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
`glass.frost` and `glass.foreground`. They are **not the whole set** — the rest are per-section
phases on Home and the detail page, and the count here has rotted twice, so take it from
`ui::profile::PHASES`, which is the authority. A
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
page invalidation or a region containment miss. `Glass::DYNAMIC` keeps drawing the glass material
every presented UI frame and invalidates a dirty backdrop on every changed successful present.
The modal dim is a page-drawn scrim rather than an input transform on the source render. Skipped
idle-loop iterations do not advance that clock. The Account panel's exact height depends on the
measured row set.

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

## Part 5 — the other 88.6%: what the MAIN UI submits, and why removing 37% of it bought 0.8%

> **The instruments this part describes ARE in the tree now** — `ui::overdraw`,
> `/tmp/plxnative-overdraw`, `/tmp/plxnative-drawmask` and `/tmp/plxnative-heroground`. They were
> hand-transplanted from `blur/e4-overdraw` rather than merged: that branch's history was rewritten
> with `filter-branch`, so it shares no ancestry with the baseline commits here, and a direct merge
> conflicts in fifteen files while sitting ~24,000 lines behind — resolving it in the branch's
> favour would delete the deliberately-kept nav-glass code among much else. Only the ledger, the
> mask, the one-pass ground and their host tests came across; everything else on that branch had
> already landed here by another route, in a later form.
>
> **Two things changed in the transplant and the recipes below reflect them.** The hero-ground
> program is linked LAZILY, at its first draw rather than at `init_image`, because `devtriggers` is
> compiled out of a release build and linking a shader that build can never reach was pure boot
> cost. And all three triggers are registered in `dev::DIAG`: they are measurement knobs whose
> method is an A/B against an unmasked control, and a non-DIAG trigger suppresses the boot
> who's-watching picker — so the control leg and the masked leg would have booted to different
> screens, making the difference between them the screen rather than the class being priced.
>
> **The paths below name the STABLE install's runtime root.** A flavoured install puts the same
> names under `$(make -s print-rundir FLAVOR=<f>)` — `/tmp/com.beb.plxnative.debug` at the tracked
> `FLAVOR ?= debug` default — so pasted verbatim they arm one install while `make run` launches the
> other, and every leg is then measured on an unarmed screen. See `docs/two-installs.md`.
>
> The numbers below were taken before all of this and stand on their own — every one is a
> whole-frame `frame.ui` A/B with three interleaved repeats.


Part 4 priced the glass at +11.4% of frame GPU cycles and recorded that the main UI is the rest,
"at 3.65x overdraw for 1080p". That sentence is true and it is the most misleading line in this
note, for two reasons this part settles with measurements: **most of the 3.65x is not ours**, and
**the part of it that is ours is very nearly free.**

### Two instruments, neither of which is a GPU counter

`FRAG_QUADS_RAST` is GPU-global, so it cannot say whose quads it counted. Two things were added to
answer that without guessing.

**`/tmp/plxnative-overdraw`** — a CPU-side ledger (`ui::overdraw`) that sums, per draw class, the
screen-VISIBLE area of every quad the app submits, clipped to the panel and to `Painter::clip`'s
live box. It is not `glFinish`-serialised and it cannot be billed for another process's work. It
runs in the desktop simulator too, and gives the same authored-pixel answer there, because it works
in authored coordinates.

**`/tmp/plxnative-drawmask=<classes>`** — refuse every draw of the named classes, so a whole-frame
`frame.ui` HWCNT A/B against the unmasked control prices that class **as the frame sees it**,
un-serialised. `all` draws nothing, and is therefore the **compositor floor**. Every leg it
produces except the control is a broken picture by construction; it is a measurement knob.

### The frame, decomposed

Scene: Home, `plxnative-homeosc` + `plxnative-noidle`, no glass. Three interleaved repeats per leg,
first 60 samples discarded, ~1,050 samples per run, within-leg spread 0.05–0.07%.

| leg | GPU_ACTIVE / frame | FRAG_QUADS_RAST | as pixels | tiles |
|---|---|---|---|---|
| control (the app draws) | 8,740,395 | 1,875,881 | 7,503,524 | 4,096 |
| `drawmask=all` (app draws nothing) | 3,007,196 | 519,120 | 2,076,480 | 4,080 |
| **difference = the app's own draw** | **5,733,199 (65.6%)** | **1,356,761** | **5,427,044** | 16 |

The floor leg's 519,120 quads are 2,076,480 pixels — **exactly one 1920x1080 composite** — with
`TEX_WORDS` 2,076,480, i.e. exactly one texel per pixel. That is the wayland compositor blitting our
surface, and it is **34.4% of the frame's GPU cycles for work this app cannot remove**. Tiles barely
move between the legs because both still resolve two full-screen render passes (2,040 tiles each at
this part's 32x32 tiling); drawing nothing does not save the pass, only its fragments.

So the app's own overdraw is **2.62x**, not 3.65x. The missing 1.0x is the compositor.

### Where the app's 5.43M pixels go — and it is not the cards

The ledger, on the same scene (television and simulator agree to the pixel; the HWCNT difference
above puts the app at 5,427,044 against the ledger's 5,386,592, **0.75% apart**, which is what
validates the ledger):

| class | px / frame | draws | share of the app |
|---|---|---|---|
| `image` — the full-bleed hero photograph (+ 6 icons) | 2,150,733 | 7 | 39.9% |
| `rect` — the two atmospheric-ramp bands (+ 34 chrome rects) | 1,446,368 | 36 | 26.9% |
| `grad` — the hero corner wedge, two quads | 1,410,048 | 2 | 26.2% |
| `card` — the peek row's four tiles | 279,480 | 4 | 5.2% |
| `text` | ~75,000 | 8 | 1.4% |
| `shadow` | 25,364 | 2 | 0.5% |
| **total** | **5,386,592** | ~58 | **2.60x the panel** |

**The established scene is the HERO, not the grid.** `plxnative-homeosc` moves the grid's focus
indices; it does not dive the snap, so Home stays on its billboard. Three stacked full-panel layers
— the photograph, the atmospheric ramp and the corner wedge — are **90% of everything the screen
submits**. The card composites everyone assumes are the expensive part are 5%.

### The experiment: fold the hero's whole ground into one pass

`/tmp/plxnative-heroground` (`ui::widgets::hero_ground` + `shaders/fs_hero.frag`) draws the
photograph and BOTH scrim fields in **one** quad instead of the art plus four blended gradient
quads over it. Both fields are closed forms of the authored pixel position — `home::base_scrim_a`
and `hero_scrim_a` feathered over `[HERO_SCRIM_TOP, HERO_SCRIM_KNEE]` — and both are
`theme::SCRIM_INK`, so two straight-alpha layers of one ink compose exactly as `a1 + a2 - a1*a2`
and the art folds into the same single blend:

```text
want   dst' = mix(mix(dst, art, A), ink, B)
s      = 1 - (1-A)*(1-B)
src    = (art*A*(1-B) + ink*B) / s
```

Exact in real arithmetic; what differs is 8-bit rounding, because the shipped path quantises the
framebuffer three times where this quantises once. The screen owns the preconditions (one art layer
at a time, so a hero flip falls back; no photograph yet, so the scrims still have the wash).

**It is the same picture.** Simulator, hero pinned with `plxnative-heroidx=0`, 960x540: **maximum
absolute difference 1/255**, mean 0.081/255, over 101,018 of 518,400 pixels — and **not one pixel
differs by 2**. That is the double-rounding, and nothing else. Ledger, same pair: 5,387,031 px
(x2.60) to 2,608,407 px (x1.26), `grad` 2 quads to 0, `rect` 36 draws to 34; on the television the
same pair reads 5,362,974 px (x2.59) to 2,584,350 px (x1.25).

**The first on-panel capture pair was thrown away, and the reason is worth writing down**:
`plxnative-heroidx` JUMPS the billboard to a pool page, it does not stop it rotating. `HERO_AUTO_S`
is 8 s, so a capture taken 16 s after launch had already advanced two pages, and the control frame
was caught mid-FLIP — which is also exactly the state the fold declines. The diff was 249/255 and
said nothing about the shader. A capture comparison on this screen has to be taken inside the first
rotation window, with `plxnative-homeosc` absent (its 350 ms focus step moves the peek row under
the shutter).

Retaken that way — no oscillator, shot 6 s after launch — the ON-PANEL pair
(`tools/capture-screen.sh … DISPLAY`, 1920x1080) is **max 2/255, mean 0.096/255, with exactly ONE
pixel of 2,073,600 differing by more than one code**. A second shot of each leg ~5 s later is
254/255 apart, which is the rotation doing precisely what it did the first time and is why the
first pair was thrown away rather than reported.

**And it is worth almost nothing.** Television, three interleaved repeats per leg, ~925 samples per
run, first 60 discarded, within-leg spread 0.04% (control) and 0.01% (folded):

| counter | control | folded | delta |
|---|---|---|---|
| GPU_ACTIVE / frame | 8,746,572 | 8,676,083 | **−70,489 (−0.81%)** |
| FRAG_QUADS_RAST | 1,875,660 | 1,177,742 | −697,918 (**−37.21%**) |
| LS_WORDS | 7,762,728 | 4,984,257 | −2,778,471 (−35.79%) |
| ARITH_WORDS | 15,527,280 | 15,515,117 | −12,163 (**−0.08%**) |
| TEX_WORDS | 4,597,344 | 4,597,344 | 0 |
| FRAG_NUM_TILES | 4,096 | 4,096 | 0 |
| heartbeat `fps=` | 60 | 60 | 0 |

**Removing 37% of the frame's rasterized fragments bought 0.81% of its GPU cycles**, and the
counters say exactly why. Each removed fragment cost **one LS word and no arithmetic**: the driver
folds `mix(uniform, uniform, varying)` and the four-corner bilinear field into varying
interpolation, which runs on the load/store pipe. That pipe was at **44.8%** occupancy
(7.76M of 17.34M tripipe cycles) while the arithmetic pipe was at **89.5%** (15.53M). The frame is
**arithmetic-bound**, and the overdraw carried none of it. 130,299 fragment core-cycles for
2,791,672 removed pixels is **0.047 core-cycles per pixel** — the removed layers were, to a very
good approximation, free.

The 2,778,471 LS words removed against the ledger's 2,778,624 predicted pixels — **0.006% apart** —
is also the tightest available check that the ledger and the counters are measuring one thing.

**So "3.65x overdraw" is not headroom.** A third of it belongs to the compositor, and most of the
rest is blended varying interpolation on a half-idle pipe. Culling app overdraw on this part is
worth roughly 0.02% of the frame per percent of fragments removed. Anything that pays here has to
remove ARITHMETIC, not fragments.

### What this means for anyone optimising this app

1. **Stop reading `FRAG_QUADS_RAST` as a budget.** On this part a rasterized fragment costs between
   nothing and a great deal depending on which pipe its shader uses, and the cheapest ones are
   exactly the big full-screen ones. Price a change by a whole-frame `frame.ui` A/B or not at all.
2. **34.4% of the frame is the compositor** and is not addressable from inside this process. The
   app's own share of a hero frame is 5,733,199 cycles; that is the whole size of the prize.
3. **The bottleneck is the arithmetic pipe at 89.5% occupancy.** The lever is arith words per
   fragment on the quads that carry them, not the number of quads.
4. **`ui::overdraw` is worth keeping** whichever way the fold goes. It is compiled out of a release
   build entirely, it runs in the simulator, and it is the only instrument here that can attribute a
   fragment to a draw class — the counters cannot, because they are GPU-global.
5. The fold itself is **worth having in the tree behind its flag and not worth switching on**: 0.81%
   does not pay for a GLSL copy of two design curves that `theme.rs` and two screens own, and every
   future retune of either curve would have to be made in both places. It becomes interesting the
   day the surface is not 1080p — at 4K the fragment and load/store work scale by four while the
   arithmetic per fragment does not, which is precisely the condition under which the pipe it
   unloads becomes the one that binds.

### What a fragment costs, by draw class — the table the ledger exists to produce

Same instrument, same scene: refuse ONE class and take a whole-frame `frame.ui` A/B against the
control. Two interleaved repeats per leg, ~850 samples per run, within-leg spread 0.03–0.13%.
Control 8,756,698 GPU_ACTIVE per frame.

| class refused | px removed | Δ GPU_ACTIVE | Δ % of frame | **cycles / px** | Δ ARITH_WORDS |
|---|---|---|---|---|---|
| `card` — 4 peek-row tiles, `fs_img` full path (SDF + rim + penumbra) | 281,924 | −851,188 | **−9.72%** | **3.02** | −1,652,307 |
| `image` — the full-bleed hero photograph, `fs_img` FLAT path | 2,153,440 | −2,102,996 | **−24.02%** | **0.98** | −4,288,786 |
| `text` — 8 glyph strings | 84,216 | −60,018 | −0.69% | 0.71 | −89,628 |
| the ramp + the wedge (via `heroground`) | 2,791,672 | −70,489 | −0.81% | **0.025** | −12,163 |

**A card-composite fragment costs 120x what a gradient fragment costs, and a textured full-screen
one costs 39x.** Every delta tracks `ARITH_WORDS / 2` to within 10% (two shader cores, one
instruction word per cycle each) — which is the same statement as "the frame is arithmetic-bound",
arrived at from the other side.

So the ranking, on a hero frame, is: **the wayland compositor 34.4%, the hero photograph 24.0%,
four card composites 9.7%, the glass 11.4%** (Part 4, same scene), everything else under 1% each.
Two consequences worth carrying:

* **The photograph is the app's single most expensive object**, at nearly one GPU cycle per screen
  pixel, and it is one draw call with the simplest shader in the app. There is no overdraw to
  remove there; it is a 1280x720 texture read magnified to the panel, and `L2_EXT_READ_BEATS` falls
  56% when it goes.
* **These deltas are a RANKING and a per-pixel price, not an additive budget.** They sum to 35.2%
  against the 65.6% that `drawmask=all` removes, because `GPU_ACTIVE` is an OR across concurrently
  active units: the app's arithmetic partly hides behind the compositor's texture stalls, so
  removing one class frees less than removing it in isolation would. Price a real change by its own
  A/B, exactly as this note has said since Part 4 — do not build a budget by adding these rows up.

## 2026-09-02: the Home motion regression — a census, and what a full-screen fragment costs

Reported: Hero paging ~46 fps and the Hero→first-shelf fold ~38 fps against a 50 fps gate (the
`home-hero` / `home-fold` scenes, `plxnative-heroosc` / `plxnative-homefoldosc`). The previous
diagnosis blamed the profile chip's second glass surface and the top bar's blur source pass. Both
were measured and both were wrong; what follows is the record, because every step of it
contradicted a reasonable expectation.

**1. The frame-drop detector reads as CPU and is not.** `plxnative-framedrop=1` on the hero scene:
`draw=24.0 ms p50, swap=0.3 ms`. A new profiler mode, **`/tmp/plxnative-cpuprof`** — the render
thread's own inclusive wall time per `ui::profile::phase`, every phase at once, no `glFinish` —
put 26 ms of that in **`hm.clear`**, the frame's first framebuffer-0 command, and ~2 ms in the whole
of Home's real work (`hm.hero` 0.9, `hm.grid` 0.5, `hm.tabs` 0.2, the blur source pass 0.8 CPU
including its three FBO passes). On this driver the wait for the GPU lands in the first command
that needs the back buffer, so a fat `draw=` is a GPU-bound frame until this mode says otherwise.

**2. Glass ON is faster than glass OFF, three times running.** Same scene, interleaved:

| leg | hero fps | fold fps | GPU_ACTIVE / frame (hero) |
|---|---|---|---|
| shipped: glass, refresh every present (`glasshz=1`) | 46 | 38 | 14.44M |
| `plxnative-flattabs` — no glass, no source pass | 35 | 30 | 13.81M |
| `plxnative-glasshz=8` — glass, refresh 1 present in 8 | 36 | 30 | — |

More GPU work, seven milliseconds less frame period. The only structural difference is that the
direct source pass submits FBO render passes immediately after the swap, before anything touches
framebuffer 0. Whether that is Mali kbase DVFS reacting to a gap-free submission
(`/sys/devices/platform/mali.0/dvfs_period` exists; `power_policy` is `demand`) or the driver
starting the frame's fragment work earlier is NOT settled — but the consequence is: **lowering
the glass cadence or removing the source pass to "save work" costs 20% of the frame rate, and any
change to that path has to be re-measured on the set.**

**3. The census: `plxnative-hwcnt` (`frame.ui`) + `plxnative-drawmask=<class>`, hero paging,
flat tabs.** Control 13.81M GPU_ACTIVE per frame — against 8.76M for a hero frame on 2026-08-22,
which is the regression stated in cycles.

| class refused | Δ GPU_ACTIVE | Δ ARITH_WORDS | px removed | cycles / px |
|---|---|---|---|---|
| `grad` — the hero corner scrim, `draw_grad4` | **−5.30M (38%)** | −10.6M | 1.43M | **3.7** |
| `rect` — the two atmospheric ramps, track, buttons | −2.65M | −5.0M | 1.44M | 1.8 |
| `image` — the photograph | −2.17M | −4.3M | 2.15M | 1.0 |
| `card` — the peek row's tiles | −1.41M | −2.7M | 0.46M | 3.1 |
| `text` | −0.13M | −0.1M | 0.10M | 1.3 |
| `ambient` — hidden under opaque art on this scene | −0.00M | — | — | — |

The Aug-22 table priced "the ramp + the wedge" at 0.025 cycles/px. `dec32f2e` (2026-09-01) had
rewritten `fs_ambient.frag` — the ONE program behind both `draw_grad4` and the page wash — with a
`fract(sin(dot()))` dither hash evaluated on every fragment (`u_noise` only scaled it to zero for
a scrim) and a highp coordinate. `sin` on Midgard is a range reduction plus a polynomial; the
scrim went to ~7 arithmetic words a fragment.

**4. Three fixes, each measured on its own (hero frame, flat tabs, GPU_ACTIVE per frame):**

| state | hero GPU/frame | hero fps (glass) | fold fps (glass) |
|---|---|---|---|
| as found | 13.81M | 46 | 38 |
| dither behind a uniform branch, cheap hash | 11.68M | 54 | 41 |
| + highp→mediump coordinate, flat rects routed to the ambient program | 12.30M | 52 | 39 |
| − that routing, + bilinear as ONE fragment mix (corner mixes as varyings, `vs_ambient.vert`) | 10.63M | 59 | 43 |
| + the wash's dither as a 64x64 noise TEXTURE fetch (`gfx::noise_tex`) | 10.63M | 59 | 45 |
| + the wash undithered while Home's hero is in motion | 10.63M (fold 11.15M) | **59** | **53** |

Three of those rows are lessons rather than steps. **A three-mix bilinear fragment costs ~2.3
cycles a pixel on this part even with no hash** — the two horizontal corner mixes are linear in
`u`, a varying interpolates a linear function exactly, so they moved into the vertex shader and
the fragment keeps one mix. **Routing a flat rect to that program made the frame 0.6M cycles
DEARER** than `fs_src`'s one-mix early-out, even though `fs_src` itself now prices a flat pixel at
~1.8 cycles (Midgard sizes the register file for the whole grown shader — capsule arcs, glow —
and the early-out runs at that occupancy). And **an interleaved-gradient hash in highp still cost
the fold's full-screen wash ~4M cycles**: `gl_FragCoord` is highp, so the hash was fp32 on 2M
pixels. The texture fetch moved it to the idle pipe (`TEX_WORDS` +1.7M, `ARITH_WORDS` −5M), and
the wash then still cost 2.5M for one mix plus the blend — which is why it is now undithered while
the hero is mid-fold or mid-slide, the only times it is visible on Home and the only times it is
behind a moving translucent picture nobody reads as a gradient. At rest, with the photograph absent
or still arriving, it dithers as before.

**The rule this leaves behind:** on the T820 the arithmetic pipe binds, and a 60 fps frame is about
11M cycles ≈ 22M words for EVERYTHING on the panel, compositor included — roughly ten words per
screen pixel, total. A full-screen quad therefore cannot afford more than a couple of operations
per fragment. Uniform-branch every optional term, push anything linear into a varying, put lookups
on the texture pipe, and price the result with `hwcnt` + `drawmask`, never by reading the GLSL.

## 2026-09-02 (later): the ambient wash's dither — a 64px plaid, and what an overlay actually costs

Two reported items, measured on the dev set (webOS 4.5, Mali-T820) in one session. The first was
a picture bug with three candidate causes and the measurement killed two of them; the second was a
performance question whose answer turned out to be "there is no problem", which is only worth
anything because the numbers are written down.

### Item 6 — "banding and strange visual patterns" on the Person page

The wash is `route_screen`'s full-screen `AmbientWash`, which — unlike Home's and Detail's, both of
which pass `dither=false` while the hero moves — dithers on EVERY frame it draws. Three hypotheses
were on the table: (a) the 64px noise tile repeating visibly, (b) ±½ LSB being too little dither to
break a contour, (c) the mediump `v_uv` quantising over a 1080px span.

**The instrument first, because two of these are invisible to it.** A `tools/tv-session.sh shot` is
the composited panel output, so it shows the app's own 8-bit result and NOT what LG's picture
processing does to it afterwards. It can therefore see a tile repeat and a contour; it cannot see
sharpening amplifying either. It does resolve the dither — residual std 0.47 LSB against a clean
region — so its silence would have meant something, which is the precondition for reading it.

Region: `y 450–1050, x 1500–1900` of the person page, right of the poster shelf and below the bio
panel, i.e. wash and nothing else. The gradient there is **4.6 LSB over 600 rows — one 8-bit step
per ~130 rows**, which is the slowest ramp in the app and the worst case for contouring.

**(c) is arithmetic, and it is not close.** An fp16 `v_uv` steps by 1/2048 of the quad; across a
span of 4.6 LSB that is 0.0045 LSB per step, **445x below one 8-bit quantum**. The shader header
already argued this and the measurement agrees with it. Nothing was changed here, and a `highp`
coordinate remains the wrong fix — it was priced at 3.2M cycles a frame earlier the same day.

**(a) is the defect, and it is unambiguous.** Autocorrelation of the residual after removing a
fitted ramp, on the panel capture:

| lag | 63 | **64** | 65 | 128 |
|---|---|---|---|---|
| horizontal, 64px tile | +0.131 | **+0.570** | +0.134 | +0.277 |
| vertical, 64px tile | +0.186 | **+0.366** | +0.185 | **+0.703** |
| horizontal, 256px tile | +0.089 | +0.083 | +0.089 | +0.016 |
| vertical, 256px tile | +0.144 | +0.141 | +0.141 | +0.124 |

A spike four times its own neighbours at exactly the tile period, and a bigger one at twice it: the
tile was repeating **30 times across the panel** as a plaid. That is the "strange visual patterns".
The old justification — "64 is well past the eye's ability to see a repeat at ±½ LSB amplitude" —
confuses two thresholds: a PERIODIC signal is found far below the contrast at which its own grain
is resolved. At 256 the curve is a smooth monotone decay with no spike anywhere.

**(b) is real but second-order, and the simulation says so plainly.** On this measured ramp the
existing ±½ LSB uniform dither already flattens the staircase about tenfold, and triangular dither
is marginally WORSE on absolute blurred error because it is more noise. What ±1 LSB TPDF removes is
noise MODULATION — under uniform dither the quantisation error's variance still tracks the signal,
which reads as the wash breathing or clumping rather than as grain. Per-row error-variance
coefficient of variation falls **0.44 → 0.08**. The amplified-residual crops show it: the 64px
image has visible horizontal clumping, the 256px one is structureless.

The measured residual std moved **0.4692 → 0.5502 LSB**. That is not a loose "it got noisier": the
capture's own noise floor solves to 0.369 LSB from the first number, and TPDF at ±1 LSB then
predicts 0.550 — an independent confirmation that the dither really is triangular at the intended
amplitude, from a number nobody tuned.

**The triangle is baked into the TILE, and that is the whole trick.** Forming it in the shader would
be a second channel plus an add on 2M fragments; storing the mean of two independent hashes leaves
the fragment expression byte-for-byte identical and moves the entire difference into `u_noise`'s
scale (`1/255` → `2/255`). **Zero shader change.** The only cost that exists is the tile's footprint.

**Cost, `fps:settings-root` with `plxnative-hwcnt`, phase `frame.ui`, n=60 per sample, steady state
(the first sample after launch is a 13.7M settle frame and is discarded):**

| counter, per frame | 64px RPDF | 256px TPDF | Δ |
|---|---|---|---|
| GPU_ACTIVE | 8.761M | 8.766M | **+0.06%** |
| ARITH_WORDS | 15.637M | 15.639M | +0.01% |
| TEX_WORDS | 4.308M | 4.312M | +0.08% |
| L2_EXT_READ_BEATS | 160k | 256k | **+60%** |
| L2 read hit rate | 82.6% | 74.1% | −8.5 pt |

The cost lands exactly and only where theory puts it — a 256 KB tile misses L2 far more often than a
16 KB one and pulls ~96k more external read beats a frame — and **none of it reaches GPU_ACTIVE**,
because on this part the arithmetic pipe binds and the memory pipe beside it has headroom. The
+0.06% is inside the 64px leg's own ±17k sample spread. Pacing, taken in a separate run with NO
profiler armed: `fps:settings-root` **60 fps median (60–61) against its floor of 50**, and the whole
tier 25/25. The old "small enough to live in the texture cache whole" argument is genuinely given
up, and it was worth less than it read: the mapping is 1:1 in screen space under `GL_NEAREST`, so
each fragment fetches a distinct texel in tile order — a coherent streaming read, which is the
pattern a texture cache is best at, not the random re-reads a resident tile protects against.

### Item 12 — every popover/modal with a blur backdrop, measured

`./tests/run.py --fps --fps-player`, no profiler armed, medians over post-warmup 1 Hz samples.
`fps=` is frames swapped and `loop=` is loop iterations; a settled overlay is SUPPOSED to read ~0
`fps` (the present gate), so an idle number near zero beside a healthy `loop=` is a pass, not a
stall. Player-tier scenes carry no `fps` gate at all by design — `ui::idle` excludes the player
route, so `fps=` there grades nothing.

| overlay (scene) | glass | loop/s | fps median | fps range | verdict |
|---|---|---|---|---|---|
| Settings root (`settings-root`) | route ground | 60 | 60 | 60–61 | PASS ≥50 |
| Settings privacy (`settings-privacy`) | route ground | 60 | 60 | 60–60 | PASS ≥50 |
| Settings home picker (`settings-home`) | route ground | 60 | 60 | 60–60 | PASS ≥50 |
| Legal (`settings-legal`) | route ground | 60 | 60 | 60–60 | PASS ≥50 |
| Settings idle (`settings-idle`) | route ground | 62 | 0 | 0–1 | PASS ≤5 |
| Consent crash (`consent-crash`) | route ground | 60 | 60 | 60–60 | PASS ≥50 |
| Consent product (`consent-product`) | route ground | 60 | 60 | 60–60 | PASS ≥50 |
| Account menu (`home-acct-glass`) | `CACHED` | 60 | 60 | 60–60 | PASS ≥50 |
| Item context menu (`item-menu`) | `CACHED` | 62 | 0 | 0–1 | PASS ≤5 |
| Person page + bio row (`person-page`) | route ground | 62 | 0 | 0–1 | PASS ≤5 |
| Library sort/filter/tab (`library-switch`) | `CACHED` + nav glass | 61 | 18 | 0–60 | PASS ≥8 |
| Search shelves (`search-type`) | nav glass | 62 | 43 | 17–60 | PASS ≥20 |
| Search idle (`search-idle`) | nav glass | 62 | 0 | 0–1 | PASS ≤5 |
| Player info panel (`info-panel`) | `CACHED` | 59 | 60 | 58–60 | PASS ≥45 loop |
| Player track menu (`track-menu`) | `CACHED` | 60 | — | — | PASS ≥45 loop |
| Player chapters (`chapters-panel`, NEW) | `CACHED` | 59 | — | — | PASS ≥45 loop |

**25/25 with the new scene added, and there is no blur performance problem to fix.** Every glass
surface holds the panel rate while something animates over it and falls to the keepalive when it
settles — which is the pair of properties the floors and ceilings exist to pin, and passing both is
the thing a single number cannot show. The optimisation this item anticipated (a cached glass being
re-sourced every frame, a scrim drawn twice, a full-resolution source pass) was looked for and is
not present. Note also the standing measured warning that still applies: **the glass source pass
refreshing every present runs FASTER than a rarer refresh on this GPU** (the 2026-09-02 section
above), so nothing here should be "saved" by lowering `DEFAULT_DYNAMIC_PERIOD` without an fps A/B.

**Three overlays remain without a scene, and all three are blocked on something outside this file.**
`more_menu` (`overlay=more`) and `alt_sources` have no boot trigger at all — reaching them needs a
new `dev::flag` in `app.rs`. `tracks_panel` has `plxnative-tracks` but emits no `overlay=` tag, so a
scene naming one would fail as "never entered this screen"; it needs the tag added beside the other
five in `app.rs`'s heartbeat match. `person_bio` is the interesting one — the ONLY
`Glass::DYNAMIC_BACKDROP` popover in the app, so it is the only surface where the refresh cadence is
live — and it is opened by a key press on the person page, which no boot trigger expresses.

## 2026-09-04: the shared dither's branch, the host cache's ledger, and a spinner that never stopped

Four reported frame-rate items were one renderer census: hero paging at ~50 fps, the hero→shelf
fold under its floor, the Settings entry ramp with 89 ms frames and a grey pause, the Library
popovers at ~20 fps beside a smooth account menu, and the Cast & Crew row dropping frames. Every
number below is from the debug install on the dev set (`tests/run.py --fps`, `plxnative-hwcnt=frame.ui`,
`plxnative-cpuprof`, `plxnative-framedrop=1`), and the "before" column is `f3bdbdce`.

**The bisect.** `fps:home-hero` / `fps:home-fold` medians across the last five commits:
`a0a682af` 57/53, `2365a525` 57/53, `f361b776` 57/–, `becb4e56` **50/48**, `f3bdbdce` 50/48. One
commit, and a binary-swap bisect (the same worktree, four binaries) said the same. Hardware
counters on the hero paging scene: the fast builds spend 20.5M arithmetic words a frame (12.0M GPU
cycles), the slow ones **24.6M (14.0M)**, with rasterised quads and texture words unchanged — so
the +4M words were per-fragment ALU on the same pixels. `becb4e56` had put `shaders/dither.glsl`
behind a uniform branch on FIVE programs, two of them the per-rect `fs_src` and per-shadow
`fs_shadow`. Rule 1 of that file ("a uniform branch is resolved per draw, so a draw with
`u_dither = 0` pays nothing") is true of the fetch and the add and false of the branch: on this
Midgard it cost ~2 words a fragment on every rect, pill, scrim and shadow in the frame, and the
ramp policy had answered 0 for nearly all of them.

**What changed, as shared mechanisms.**

- `fs_src.frag` and `fs_shadow.frag` are plain again (`glsl!`); `dither_for_ramp` and its two
  thresholds are gone. The three slow-field programs keep the prelude, and the glass amplitude is
  per draw rather than a link-time constant. (The same day's GLOBAL motion gate — no field dithered
  while any spring was in flight — lasted one day: it flickered the wash's bands in and out on every
  focus spring on Settings, the picker and first run, screens that were at 60 fps with the noise on.
  Only the two page washes under moving artwork keep a motion gate, `gfx::page_wash_dither`.)
- `ui::idle::should_present` presents one **settle frame** after motion stops, so the LIVE picture left
  on the panel is the dithered one.
- The ambient field has an in-flight twin program (`fs_ambient.frag` linked behind
  `shaders/dither_stub.glsl`, `gfx::ambient_program`), with no uniform, sampler or branch at all;
  `draw_grad4` always takes it. That alone moved the fold from 50 to 55.
- A flat-colour program (`shaders/fs_flat.frag`) draws every uniform, square, unfocused rect —
  the full-screen scrims — with no interpolation.
- The popover host cache (`ui::popover::host`) is on the Library menus, Settings and the decision
  alert, and its refresh decision was rebuilt three times under review: page damage is now
  **attributed at the source and counted** (`idle::OwnScope` around a panel's update, its
  drawing, the input it holds, and the host page pass itself; `idle::take_page_damage`), page
  motion is a refresh reason only while every holder is fading, and the fps scenes' oscillators
  `wake` instead of `invalidate`.

**The account menu, three wrong answers deep.** `fps:home-acct-glass` read 47 before this pass
and **26** through most of it: first the two-bit ledger stopped masking the panel's own per-frame
appear/marquee invalidate; then page MOTION was a refresh reason while Home's decorations kept
moving under the frozen page; then, with both fixed, a simulator backtrace at the unscoped
`invalidate` named `home::draw_status`'s `Spinner::draw` — a spinner on the host reports damage
from its own draw, so any refresh frame (the page drawn for real) re-armed the next, forever.
`host::PagePass` now scopes the page draw: what the page reports while being drawn into the
snapshot IS the snapshot. Simulator: 799 host refreshes in 14 s → 2. Device: 60 fps.

| scene / boot | before | after |
|---|---|---|
| `fps:home-hero` median | 50 | **60** |
| `fps:home-fold` median / robust_min | 48 / 46 | **55 / 53** |
| `fps:home-acct-glass` median / robust_min | 47 / 43 | **60 / 60** |
| `fps:settings-root` | 60 | 60 |
| account menu over Home under `acctosc`, GPU cycles per frame (mean) | 23.1M | **4.3M** |
| `fps:library-switch` (Sort menu, p50 draw) | 33 ms | 16 ms |
| hero paging, GPU cycles / ARITH words per frame (mean) | 14.0M / 24.6M | 11.2M / 17.8M |
| Settings boot (entry ramp, Privacy, Delete alert), GPU per frame mean / p50 | 12.2M / 9.5M | 8.4M / 6.2M |
| Settings entry, worst draw frames | 3 × 89 ms | 1 × 69 ms |
| Cast & Crew scroll (Depeche Mode: 101), GPU per frame mean / p50 | 10.7M / 10.6M | 8.4M / 7.5M |
| Cast & Crew scroll, worst steady frames | ~50 ms | ~42 ms |

Still open from the same census, and not renderer work: the show detail's entry pays two ~80 ms
CPU frames rasterising the episode list and the hero text (`dt.eps`, `dt.hero`); the Cast & Crew
row's remaining drops are the page's fill (a full-screen wash plus fifteen shadowed circle
composites) crossing the budget on the frames the ambient twin does not reach.
