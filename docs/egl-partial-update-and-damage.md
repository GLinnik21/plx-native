# EGL partial update, Transaction Elimination, and the opaque region

**Device**: LG 49SM9000PLA, webOS 4.10.0, Mali-T820 MP2 (Midgard), DDK **r12p0-04rel0**, GLES2
context on a 1920x1080 drawable. Everything below was measured on that television on 2026-08-19,
in eight locked batches. Branch `blur/e7-opaque`.

---

## 0. The headline, in four lines

1. **This driver advertises none of the three damage extensions** — no `EGL_KHR_partial_update`,
   no `EGL_KHR_swap_buffers_with_damage`, no `EGL_EXT_buffer_age`. The extension string is 17
   entries long and settles a question this project has carried unanswered for its whole life.
2. **It implements them anyway.** All three entry points resolve, both damage calls return
   `EGL_SUCCESS`, `EGL_BUFFER_AGE_KHR` answers **2** after real presents, and — measured, not
   inferred — declaring a damage region **actually restricts what the GPU renders**. A 480x270
   rect on a 1920x1080 panel takes a scrolling library grid from **7,531,684 to 542,012**
   GPU_ACTIVE cycles per frame (**-92.8%**) and from 46-53 fps to a flat 60, with the picture
   outside the rect visibly frozen. See §3 and the two captures.
3. **Transaction Elimination is already collecting the bandwidth half of the prize.** On a settled
   Home **79% of presented frames are 100% eliminated** — bit-identical to two frames earlier —
   and external write traffic is ~1.3% of a full framebuffer. On a genuinely animating scene
   (library grid focus sweep) only **8.7%** of frames are fully eliminated and the median frame
   still has **54% of the screen unchanged**. TE happens *after* shading, so it removes the
   writeback and none of the arithmetic; a damage region removes both.
4. **Declaring the surface opaque does nothing here.** GPU_ACTIVE **+0.03%**, `TEX_WORDS`
   identical to the byte, `surface-manager` CPU 450.3 -> 463.3 jiffies/20 s with a wider spread
   than the difference, fps 60 either way, captures bit-identical. By
   `docs/perf-damage-tracking-verdict.md` §5's own decision rule ("**Flat =>** LSM's charge is
   per-commit"), **the compositor branch closes permanently.**

---

## 1. The EGL capability probe

`rust-modules/src/egl.rs`, called once from `app.rs` after `SDL_GL_CreateContext`. Read-only,
always on, ~5 lines in the event log.

### 1a. It does not link libEGL, and must not

The task brief said the NDK sysroot's `libEGL.so.1.4.0` exports 44 `egl*` symbols and therefore
`-lEGL` would link. Both halves are true and the conclusion is still wrong, in the way this
project's linking policy exists to catch. That file is a **link stub** — all 44 symbols are
aliased to one empty body at `0x00000d30` — and its SONAME is `libEGL.so.1`, which is a
`DT_NEEDED` the loader must satisfy at `exec()`. `tools/fwcompat.py` says it cannot:

```
release   libEGL                   libEGLfk
2.2.3     libEGLfk.so.2.3.0        libEGLfk.so.2.3.0
4.4.2     libEGLfk.so.2.4.0        libEGLfk.so.2.4.0
4.10.0    libEGLfk.so.2.4.0        libEGLfk.so.2.4.0     <- this television
5.3.1     libEGLfk.so.2.4.0        libEGLfk.so.2.4.0
6.4.0     libEGL.so.1.4.0          -
```

webOS 2.2.3 through 5.3.1 carry `libEGLfk.so.2`; their `libEGL.so.1.5` is a forwarder onto
`libmali.so` with **zero exported symbols**. `-lEGL` would have killed the process before `main`
on the only firmware family this app has ever run on — a black screen with no log line.

It is also unnecessary. SDL created the GLES2 context **through EGL**, so whichever EGL the
firmware has is already mapped with its symbols in the global scope.
`dynlib::Handle::self_handle()` (`RTLD_DEFAULT`) resolves them, with `SDL_GL_GetProcAddress` and a
SONAME candidate list as fallbacks. **`tools/fwcompat.py` is byte-identical before and after this
work: OK on 4.4.2 through 11.2.0.**

### 1b. What the television said

```
GL: Mali-T820 / OpenGL ES 3.2 v1.r12p0-04rel0.cb5901e2e52329f9302428bb2e5885c3
egl: display=0x651240 draw_surface=0x68a108
egl vendor: ARM
egl version: 1.4 Midgard-"r12p0-04rel0"
egl client_apis: OpenGL_ES
egl extensions: EGL_KHR_config_attribs EGL_KHR_image EGL_KHR_image_base EGL_KHR_fence_sync
  EGL_KHR_wait_sync EGL_KHR_gl_colorspace EGL_KHR_get_all_proc_addresses
  EGL_ARM_pixmap_multisample_discard EGL_WL_bind_wayland_display EGL_IMG_context_priority
  EGL_ARM_pixmap_multisample_discard EGL_KHR_gl_texture_2D_image EGL_KHR_gl_renderbuffer_image
  EGL_KHR_create_context EGL_KHR_surfaceless_context EGL_KHR_gl_texture_cubemap_image
  EGL_EXT_create_context_robustness EGL_KHR_cl_event2
egl procs: eglSetDamageRegionKHR=1 eglSwapBuffersWithDamageKHR=1 eglSwapBuffersWithDamageEXT=1
           eglQuerySurface=1 eglSurfaceAttrib=1
egl surface: 1920x1080 swap_behavior=0x3095 BUFFER_DESTROYED buffer_age=0 err=EGL_SUCCESS
egl config: id=1 surface_type=0x0007 SWAP_BEHAVIOR_PRESERVED_BIT=0
egl preserve: eglSurfaceAttrib(BUFFER_PRESERVED) ok=0 err=0x3009 EGL_BAD_MATCH readback=DESTROYED
egl damage: eglSetDamageRegionKHR(0,0,64,64)        ok=1 err=0x3000 EGL_SUCCESS
egl damage: eglSwapBuffersWithDamageKHR(0,0,64,64)  ok=1 err=0x3000 EGL_SUCCESS
egl surface (after 120 presents): buffer_age=2 ok=1 err=EGL_SUCCESS
```

Line by line, because each one closes something:

- **No `EGL_KHR_partial_update`, no `EGL_KHR_swap_buffers_with_damage`, no `EGL_EXT_buffer_age`**
  in the string. On the extension string alone the whole direction is closed.
- **`EGL_KHR_get_all_proc_addresses` IS advertised**, which is exactly the condition under which
  `eglGetProcAddress` returning non-NULL proves nothing: EGL 1.4 permits an implementation to
  answer for entry points it does not support. So the three `=1`s above are *not* evidence.
- **`EGL_SWAP_BEHAVIOR = EGL_BUFFER_DESTROYED`.** This is a *precondition* of
  `EGL_KHR_partial_update` (it is an error to set a damage region on a preserved surface), so it
  is the right value — and it is simultaneously what makes any *other* buffer-preservation scheme
  impossible.
- **`SWAP_BEHAVIOR_PRESERVED_BIT` is absent from the config**, and asking for it anyway returns
  **`EGL_BAD_MATCH`**. `eglSurfaceAttrib(EGL_BUFFER_PRESERVED)` is refused by the config, not by
  policy. `docs/perf-damage-tracking-verdict.md` §4's second blocker ("you cannot know what is in
  the back buffer") is **confirmed on the device** for every route except partial update itself.
- **`buffer_age` is 0 at boot and 2 after 120 presents.** The boot reading is 0 by construction —
  before the first swap the buffer has no history — which is why the probe asks again later. The
  answer **2** is a real double-buffered age from an extension that is not advertised. A correct
  damage implementation must therefore union **two frames** of damage.
- **`GL_EXTENSIONS`** (91 entries, logged in full) carries `GL_EXT_discard_framebuffer`,
  `GL_EXT_disjoint_timer_query`, `GL_ARM_shader_framebuffer_fetch`,
  `GL_EXT_shader_pixel_local_storage`, and the whole ES 3.1/3.2 pack. The driver reports
  **OpenGL ES 3.2** while the app runs an ES2 context. None of that is in scope here; it is
  logged because nothing in this tree had ever logged it.

---

## 2. `FRAG_TRANS_ELIM`: how much of the screen is already unchanged

### 2a. The counter

Shader-core block, **word 21**. Cross-checked against four independent tables that also agree on
words 4 / 14 / 20 / 26 / 27, which are already in `hwcnt::COUNTERS` — that agreement is the check:

| source | shader word 21 |
|---|---|
| Arm r7p0 `mali_kbase_gator_hwcnt_names.h` | `T82x_FRAG_TRANS_ELIM` |
| Khadas/Amlogic vendor tree copy | `T82x_FRAG_TRANS_ELIM` |
| gator `hardware_counter_names`, T820 block 2 | `T820_FRAG_TRANS_ELIM` |
| modern libmali `gen.h` | `T820_FRAG_TRANS_ELIM` |

Added to **both** `rust-modules/src/hwcnt.rs` and `tools/analyze-hwcnt.py`; the host test that
asserts the two tables are identical still passes (756 tests).

**The `PRFCNT_EN` check.** The profiler logs `jm=0xff tiler=0x1f l2=0xffff sc0=0xffff sc1=0xffff`.
Each bit enables four counters, so `sc*=0xffff` enables words 0..63 and word 21 is live — unlike
the tiler, whose `0x1f` is what makes `TILER_ACTIVE` (word 22) a structural zero. Verified before
believing any reading.

**Scale, established empirically.** `FRAG_TRANS_ELIM` never exceeded **16,320** in any run, and
`FRAG_NUM_TILES` sits at **4,096** on a full-screen frame. 16,320 = 4 x 4,080 = 4 x (60 x 68), i.e.
four CRC blocks per 32x32 tile over 1920x1088. So **16,320 means "the entire screen was
eliminated"**, and `FRAG_TRANS_ELIM / 16320` is a directly readable *fraction of the screen
unchanged since the buffer's previous contents* — which, at `buffer_age = 2`, means **unchanged
since two frames ago**. That is exactly the quantity a damage scheme would have to compute.

### 2b. The distributions

`frame.ui` HWCNT, `plxnative-noidle` armed so presents are continuous, first 60 samples discarded,
~1,400 samples per leg.

| scene | frames 100% unchanged | p50 unchanged | p10 unchanged | GPU_ACTIVE p50 | L2_EXT_WRITE_BEATS p50 |
|---|---|---|---|---|---|
| Home, settled (hero auto-flip only) | **79.0%** | 1.00 | 0.35 | 8,741,941 | 6,464 |
| Home + `homeosc` focus sweep | 78.6% | 1.00 | 0.49 | 8,745,464 | 6,489 |
| Home + `homeosc`, repeat | 77.1% | 1.00 | 0.13 | 8,745,688 | 6,489 |
| Home <-> Library route cross-fade (`navosc`) | 28.4% | 0.99 | 0.11 | 7,371,436 | 10,972 |
| Library grid + `libswitch` | 43.3% | 1.00 | 0.32 | 7,308,733 | 7,431 |
| **Library grid + `libosc` focus sweep** | **8.7%** | **0.54** | 0.27 | 7,531,681 | 239,215 |

Three things to take from this.

**The bandwidth half of the damage prize is already collected, for free, by hardware.** A full
1920x1080 RGBA8 writeback is ~518,000 external write beats at 16 B/beat. A settled Home writes
**6,464** — about **1.3%** — while still shading a full 17.4M-fragment frame with 15.5M arithmetic
words. The GPU is computing an identical picture and then declining to store it. That is precisely
the state Arm says this counter exists to reveal.

**But TE runs after shading, and a damage region runs before it.** The frame is arithmetic-bound
(a sibling measured the arith pipe at 89.5% occupancy); `ARITH_WORDS` is 15.5M whether TE
eliminates 100% of the tiles or none of them. This is the whole case for the damage direction and
the reason `FRAG_TRANS_ELIM` being high does *not* mean the prize is spent.

**On the frames that actually run, damage is large.** The one genuinely continuous-motion scene —
a library grid focus sweep — has a median of **54% of the screen unchanged**, an interquartile
range of roughly 34%..99%, and **zero frames below 25% unchanged**. The route cross-fade is the
opposite shape: mostly-static frames punctuated by dips where **89% of the screen changes**
(p10 = 0.11). So the honest summary is *about half the screen on a scroll, essentially all of it
on a cross-fade*, which is exactly the third objection in the brief, quantified.

### 2c. An observation to hand on, not a conclusion

**`/tmp/plxnative-homeosc` produced no measurable motion on Home.** The oscillator leg and a leg
with no oscillator at all agreed to within 0.02% on *every* counter — GPU_ACTIVE 8,745,464 vs
8,741,941, `TEX_WORDS` 4,597,564 vs 4,597,548, and the same 78-79% of fully-eliminated frames.
Focus *was* moving (53 `plxnative-focus` lines in a 34 s run, against the oscillator's 350 ms
cadence). The reading that fits is that a focus step on Home changes little enough, and settles
fast enough between 350 ms steps, that three quarters of presented frames are still bit-identical
to two frames earlier. `tests/manifest.json`'s `fps:home-grid` scene arms this oscillator to make
the screen move; whoever owns that gate may want to know it moves less than it looks.

---

## 3. The damage region is real, and it is enormous

The extension is not advertised, and a stub that accepts everything and returns `EGL_TRUE` is
indistinguishable from a working implementation by return code alone. So this was measured rather
than asked.

**`/tmp/plxnative-egldamage[=WxH]`** declares a damage rect of that size at the bottom-left, every
frame, **while still drawing the entire screen unchanged**. That is deliberately not a dirty-rect
renderer — a real one would draw only inside the rect, and then a wrong picture would prove
nothing about the driver. Drawing everything makes the driver's behaviour the only variable.
`eglSetDamageRegionKHR` is called as the first statement of the present block (its spec permits it
only before any rendering command since the last swap) and the buffer age is queried first (its
spec makes a sub-buffer region an error otherwise).

### 3a. Whole-frame A/B, interleaved

Scene: Library browse grid with `plxnative-libosc` sweeping focus, `plxnative-noidle`, HWCNT
`frame.ui`, 30 s legs, order off/on/off/on, first 60 samples discarded, ~1,380 (off) and ~1,680
(on) samples per leg. Medians of the sample distribution.

| counter | damage off | damage on (480x270) | delta |
|---|---|---|---|
| **GPU_ACTIVE** | 7,531,684 (+-0.08%) | **542,012** (+-0.08%) | **-92.80%** |
| FRAG_ACTIVE | 14,549,508 | 642,960 | -95.58% |
| FRAG_QUADS_RAST | 1,017,043 | 24,580 | -97.58% |
| **FRAG_NUM_TILES** | **4,096** | **151** | -96.31% |
| FRAG_TRANS_ELIM | 8,962 | 163 | -98.18% |
| ARITH_WORDS | 12,584,749 | 579,976 | -95.39% |
| LS_WORDS | 5,439,590 | 195,906 | -96.40% |
| TEX_WORDS | 3,516,964 | 98,208 | -97.21% |
| L2_EXT_READ_BEATS | 351,060 | 15,858 | -95.48% |
| L2_EXT_WRITE_BEATS | 238,974 | 17,136 | -92.83% |
| heartbeat `fps=` | 46-53 | **60** | at the vsync cap |

Leg-to-leg spread within a configuration is 0.01-0.36%, against a 92.8% effect.

**The tile count is the proof that this is the driver and not an artefact.** 480x270 at 32x32
tiles is 15 x 9 = 135 tiles; the measurement is **151**, the extra ~16 being the small
quarter-resolution blur FBO passes, which are separate render targets and unaffected. The driver
is skipping exactly the tiles outside the declared region — not loading them, not rasterizing
them, not shading them, not storing them — while the application submits the same geometry it
always did. `FRAG_QUADS_RAST` falling 97.6% says the fragment stage never saw them.

The `fps=` figures are from legs with the HWCNT profiler armed, whose per-phase `glFinish` costs
this scene ~10 fps; the same scene without the profiler runs at 60 either way, so **the frame-rate
line above is evidence that the work was removed, not a claim about a shippable 60->60 win.**

### 3b. What it looks like on the panel

`/tmp/plxnative-egldamage` declares **180 frames of full damage first**, then narrows. That
warm-up is not a nicety and was arrived at from the wrong end: with a sub-rect declared from the
very first frame, **the panel showed the boot splash forever** and the app's own picture never
appeared at all, because no frame ever declared the whole surface valid. That is the "default must
be full damage" rule of any shippable version, discovered by violating it.

With the warm-up, the captures (`tools/capture-screen.sh … DISPLAY`, hero pinned with
`plxnative-heroidx`) are unambiguous:

- **damage = 1920x540** (bottom half): the top half of the panel is frozen on the hero the app was
  showing 20 seconds earlier, the bottom half is live — current hero caption, current card row —
  and the seam is a hard horizontal line at exactly y=540.
- **damage = 480x270**: the whole panel is frozen except a 480x270 rectangle in the bottom-left
  corner, which alone tracks the live UI.

(`scratchpad/e7/w-warmhalf.png`, `w-warmquad.png`. Bottom-left origin, per the damage specs, is
why the live region is at the bottom of the screen.)

### 3c. What this does and does not license

It licenses the claim that **`EGL_KHR_partial_update` is fully implemented in this Mali r12p0
blob and simply not advertised on this display**, and that its effect scales with declared area:
a rect covering 6.25% of the panel left 7.2% of the frame's GPU cycles.

It does **not** license a projected shipping win. The -92.8% is the mechanism's ceiling under an
absurdly small damage rect. Applying the measured damage distribution from §2b instead — a median
of 46% of the screen changed on a library scroll — the extrapolation is roughly **half the GPU
work of an animating scroll frame**, and near zero on a route cross-fade, where nearly everything
changes. And on Home, where 78% of frames are bit-identical, `ui::idle` already removes those
frames entirely and removes the compositor's share with them, which no damage scheme can.

It also does not touch the compositor. `eglSwapBuffersWithDamageKHR` — the *other* extension, whose
own spec says it changes nothing about rendering and is purely a hint so the compositor "can avoid
recomposing parts of the surface that haven't really changed" — was called once and returned
`EGL_SUCCESS`, and was **not measured**, because SDL owns the swap (`SDL_GL_SwapWindow`) and
replacing it is a separate piece of work. The two extensions attack the app's 65.6% and the
compositor's 34.4% respectively and must be priced separately; only the first is priced here.

---

## 4. The opaque region: flat, and the compositor branch closes

`docs/perf-damage-tracking-verdict.md` §5's lever, never tried before. `/tmp/plxnative-opaque`
binds `wl_compositor` from a registry bind, creates one full-surface `wl_region` at boot, and
asserts it on every route that is not the player — edge-triggered, so an unchanged route costs one
static read. The player route keeps `set_opaque_region(NULL)` and gets it back on the transition,
because a claim of opacity over a slaved video plane is exactly the failure `system.rs:36-38`
warns about.

**Proof it armed** (this is the "verify only one path ran" check, and the first attempt at these
numbers had no such proof):

```
opaque: bound wl_compositor v1 (advertised v3) proxy=0x69ecc8
opaque: ARMED — full-surface opaque region 1920x1080 at (0,0)
opaque: set_opaque_region(full) for route player=false
```

3 `opaque:` lines in every experiment leg, 0 in every control leg.

**GPU**, `frame.ui` HWCNT on a settled Home with `noidle`, six legs interleaved off/on/off/on/off/on,
~1,160 samples each, medians:

| counter | opaque off | opaque on | delta |
|---|---|---|---|
| GPU_ACTIVE | 8,741,608 (spread 0.04%) | 8,744,474 (spread 0.02%) | **+0.03%** |
| **TEX_WORDS** | **4,597,344** | **4,597,344** | **+0.00%** (identical to the byte) |
| ARITH_WORDS | 15,529,516 | 15,533,434 | +0.03% |
| FRAG_NUM_TILES | 4,096 | 4,096 | 0 |
| L2_EXT_READ_BEATS | 316,167 | 315,997 | -0.05% |

`TEX_WORDS` is the sharp instrument here: a sibling identified one full-screen textured blit of our
surface inside it, so if LSM had promoted an opaque surface to a plane the counter had 2,073,600
texels to lose. It lost none.

**CPU**, `/proc/<pid>/stat` utime+stime over 20 s windows, three interleaved repeats, app at a
steady 60 fps with `noidle`:

| leg | `surface-manager` | app |
|---|---|---|
| baseline, app closed | 81 jiffies | — |
| opaque off | 451 / 452 / 448 -> **450.3** (spread 0.9%) | 381 / 381 / 382 |
| opaque on | 475 / 446 / 469 -> **463.3** (spread 6.3%) | 380 / 383 / 380 |

The +2.9% mean difference is smaller than the on-leg spread and has the wrong sign for a win.
Our presents cost `surface-manager` about 369 jiffies per 20 s (18.5% of one A53) and the opaque
region does not reduce it.

**Quality**: `DISPLAY` captures with the hero pinned are **byte-identical** — 0 of 6,220,800 bytes
differ, and the same md5 as four unrelated control captures from other batches. No repaint
artefact, and none expected, since nothing changed.

**Recommendation: do not ship.** By the verdict's own rule, flat means LSM's charge is
per-commit, not per-blended-pixel, and **the entire compositor branch is closed**: the whole-frame
present gate (`ui::idle`) remains the only thing in this app that ever reaches the compositor's
share. The ~120 lines in `system.rs` are kept behind the trigger only because they are the
evidence; there is no reason to enable them.

---

## 5. What a shippable dirty-rect renderer would still need

The prototype in this branch is a calibration probe, not a feature. Between it and something
shippable:

1. **It rests on an unadvertised extension.** `EGL_KHR_partial_update` is absent from
   `EGL_EXTENSIONS` on this exact driver, and the app supports webOS 4.4.2 through 11.2.0 across
   at least three different EGL implementations (`libEGLfk.so.2`, `libEGL.so.1`, and a PowerVR
   stack at 10.2.0). Any use must be gated on a **runtime probe that proves the behaviour**, not
   on the string and not on the entry point resolving — because here the string says no and the
   entry point says nothing.
2. **The first frames must declare full damage**, and so must any frame after a resize, a route
   change, a context loss or an app-switch return. This is not defensive: §3b is what happens
   without it.
3. **Two frames of damage must be unioned**, because `buffer_age` is 2. A damage ring, and a
   correct answer when the age changes or comes back 0.
4. **The clip stack fights it.** `Painter::clip` *replaces* the scissor at 7 sites and
   `clip_clear` *disables* it at 6 more plus `ui::guard`'s panic arm. This prototype sidesteps the
   issue entirely by not scissoring anything — it declares damage and still draws everything, so
   the clip stack is irrelevant to the measurement and would be fatal to a real implementation.
   B5's intersecting clip stack (`ui-framework-improvements.md:460`) is a hard prerequisite.
5. **Painted extent, not logical bounds.** `pad = blur + 1.0` (`ui/mod.rs`), `GLOW_PAD`
   (`ui/consts.rs`), `AA_BLEED` (`gfx.rs`) — centralized, which makes this the cheap third.
6. **Where the rects come from is still the unsolved problem**, and it is the one
   `docs/perf-damage-tracking-verdict.md` §1 is right about: a `Spring` is three scalars that do
   not know what they move, and hand-attributing geometry at 48 `.step(` sites is the
   hand-maintained list the motion capability exists to remove. Nothing measured here changes that.
7. **There is no pixel gate in this project.** A missed rect is a smear only a television can
   show you. The two captures in §3b are the entire visual verification apparatus that exists.
8. **The player route must stay out**, for the reason `system.rs:36-38` gives.
9. **ARM's own caveat applies and should be quoted at whoever proposes this**: *"Do not use either
   `EGL_KHR_partial_update` or `EGL_KHR_swap_buffers_with_damage` for applications that always
   re-render the whole frame, as there is an extra cost in doing so."* §2b says roughly half of a
   scrolling frame and almost none of a cross-fading one is reusable, so the scheme pays on scroll
   and costs on transitions unless it can tell them apart.

---

## 6. What was not measured, and why

- **`eglSwapBuffersWithDamageKHR`'s effect on the compositor.** It returns `EGL_SUCCESS`, and
  that is all that is known. SDL owns `SDL_GL_SwapWindow`, so using it means intercepting the
  swap. Given §4 — LSM's charge does not respond to an opacity hint — the prior probability that
  it responds to a damage hint is low, but it is untested.
- **Whether any other firmware honours the unadvertised entry points.** One television, one DDK.
- **The end-to-end win of a real dirty-rect renderer.** §3a measures the mechanism's ceiling with
  the application still submitting every draw call; a real scheme also stops submitting, but
  damages much more than 6.25% of the screen. The two effects move in opposite directions and
  neither is measured.
- **Whether `FRAG_NUM_TILES = 4,096` on a full-screen frame is two passes or one.** 4,080 tiles
  is one 32x32 pass over 1920x1088, and the counter reads 4,096 with two shader cores summed. The
  ratio `FRAG_TRANS_ELIM / 16320` is used above as a *fraction*, which is insensitive to that
  question, but the absolute tile count is not fully explained.
- **The blur/glass interaction.** Every leg here ran with whatever glass the scene draws by
  default; no leg isolated it. The backdrop chain renders into its own FBOs, which a swapchain
  damage region does not touch — visible as the ~16 tiles that survive in §3a.

---

## 7. Reproducing

> **The `/tmp/plxnative-…` paths below predate the two-install split: they are the STABLE install's
> runtime root.** A flavoured install puts the same names under `$(make -s print-rundir
> FLAVOR=<f>)` — `/tmp/com.beb.plxnative.debug` at the tracked `FLAVOR ?= debug` default — so pasted
> verbatim the `ssh` lines arm one install while the `make deploy`/`make run` beside them drive the
> other, and the probe reports on a screen nothing armed. See `docs/two-installs.md`.

```sh
# EGL capability probe: on by default, in the boot log
make deploy && make run RUN_SECS=12 | grep -E '^(egl|gl extensions)'

# the mutating probe (config surface type, BUFFER_PRESERVED attempt, the two damage calls)
ssh root@TV 'touch /tmp/plxnative-eglprobe'

# the damage experiment: 180 frames of full damage, then a WxH rect, bottom-left origin
ssh root@TV 'printf "480x270" > /tmp/plxnative-egldamage'

# the opaque-region experiment
ssh root@TV 'touch /tmp/plxnative-opaque'

tools/analyze-hwcnt.py pkg/plxnative-hwcnt.jsonl --phase frame.ui --discard 60
```

Every trigger defaults to absent and every default path is unchanged: `make check` 756/756,
`make lint` clean, `tools/fwcompat.py` OK on 4.4.2 through 11.2.0 — the same matrix as before.
