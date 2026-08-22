# View buffers, and whether the 50 is heat

> **Field names in this document predate 2026-08-01 and the old name was REUSED.** Where it says
> `FPS=`, today's heartbeat says **`loop=`** (loop iterations); where it says `pres=`, today's says
> **`fps=`** (frames actually presented). The manifest gates moved too: `floor`→`loop_floor`,
> `present_floor`→`fps_floor`, `present_ceiling`→`fps_ceiling`, and `fps_stats`→`rate_stats`. The
> text below is left as written, with the line numbers of its day, because it is a dated record of
> an investigation rather than live guidance — see `CLAUDE.md` for the current names.

**Q1 — view buffers: no, and not marginally.** On this stack a cached-quad composite *is* the most expensive fragment path we already have (`fs_img.frag:32`, one `texture2D` + a tint multiply), while the passes it would replace — full-screen scrims, washes, backdrops — are the *cheapest* one (`fs_src.frag:35-38`, a single `mix()` with zero memory traffic), so the trade runs the wrong way before you add the FBO round trip or the Midgard tile flush the tree already documents at `gfx.rs:851-852`. **Q2 — the drop is probably neither heat nor fill as stated, because the number quoted no longer means what it used to:** since the gate landed, `FPS=` counts *loop iterations*, not swaps (`app.rs:252-260`, `idle.rs:173-180`), and a settled screen reports the ~62 Hz idle-poll rate, so a post-gate "50" has to be re-read off `pres=` before any hypothesis is entertained. The existing 10-scene ranking supports **fill, weakly** (the two scenes below cap are exactly the two with the most full-screen passes, and their suite *position* is 4th and 8th, so nothing is ordered by elapsed time), but it cannot support or refute thermal at all, because no scene runs longer than 36 s and `fps_stats` sorts the samples and throws the order away (`tests/run.py:1078-1084`). And the honest headline: **the present gate already took most of the thermal win that exists here** — it changed the duty cycle from 60 presents/s to 0.5, which is a change in *joules per hour*, and no per-frame optimisation can repeat that.

---

## Q1 — view buffers

### The per-pixel comparison, which decides it

A "view buffer" ends up on screen as one textured quad. On this renderer that quad goes through `fs_img.frag`, and at radius 0 it takes the flat path — `fs_img.frag:32`:

```glsl
vec4 c = texture2D(u_tex, v_cuv);
vec3 tex = c.rgb*u_tint.rgb;  float ta = c.a*u_tint.a;
if (u_iradius < 0.5) { gl_FragColor = vec4(tex, ta); return; }
```

Here is what that costs against everything it could replace. Every row verified in the shader source.

| per fragment | ALU | texture read |
|---|---|---|
| **cached-layer composite** (`fs_img.frag:32`) | 1 fetch + 4 mul | **4 B** |
| flat rect / scrim / wash / backdrop tint (`fs_src.frag:35-38`) | `mix()` = 4 MAD | **0 B** |
| card **interior**, ~85% of a card's pixels (`fs_img.frag:34`) | 1 fetch + 4 mul | 4 B |
| card **edge**, ~15% (`fs_img.frag:35-42`) | sdBox + 3 smoothstep + 2 mix | 4 B |
| cached glyph quad (`text.rs:451-476`) | 1 fetch | ~1-4 B |

The composite is **identical** to the card-interior path, **strictly more expensive** than every flat pass, and cheaper only than the card *edge* — 15% of the pixels of a tile that is already **one draw call** (`ui/mod.rs:333` → `gfx.rs:684` → `draw_tex_impl`, a single `glDrawArrays` at `gfx.rs:663`, texture + 1px sheen + drop shadow folded into that one pass).

That is the fill-rate pass you already did, working against you. It made the primitives so cheap that there is nothing left for a cache to be cheaper *than*.

### The whole-screen arithmetic

The following arithmetic used the old serialized `glFinish` profiler and is retained only as the
historical argument that led to this design. Its `÷2.3` conversion is not a valid measured GPU
baseline; rerun the comparison with asynchronous timer queries before using these values:

- One **opaque full-screen rect** on this GPU: the redundant `SURFACE_APP` pass deleted from `Backdrop::draw` took `hm.backdrop` **4.5 → 0.13 ms** profiled ⇒ that single pass ≈ **4.4 ms profiled ≈ 1.9 ms real**.
- A whole **Home grid** frame's content: `hm.backdrop 0.13` + `hm.grid 8.0` ≈ **8.1 ms profiled ≈ 3.5 ms real** for 14 card composites plus the wash.

So caching the entire Home grid:

```
composite the cached layer   ≥ 1.9 ms real   (a full-screen pass, PLUS an 8.29 MB texture read the flat rect didn't do)
content it replaces            3.5 ms real
─────────────────────────────────────────────
best-case saving             ≤ 1.6 ms of a 16.6 ms frame  (≤10%)
```

…and that is **before** the rasterisation into the FBO (another ~3.5 ms, amortised over however many frames the layer stays valid) and before the tile flush. Amortised cost per frame is `1.9 + 3.5/N + flush`; break-even against 3.5 needs N ≥ 3 *with the flush at zero*.

DRAM, same frame:

```
1920×1080 RGBA8                       = 8,294,400 B = 8.29 MB
cached layer: FBO write + tex read    = 16.6 MB per frame
uncached: 14 cards × 93,750 px × 4 B  =  5.25 MB per frame
                                        → ~3× the traffic
```

**The absolute bandwidth of this SoC is unverified** — no part of the tree states it and no sysfs exposes it. But the conclusion does not rest on it: 16.6 MB > 5.25 MB is arithmetic, and it holds at any bus speed. What *is* unverified in ms is the tile cost: `gfx.rs:851-853` records the obligation from the capture path — *"glClear before each pass spares Midgard the tile preserve-load of the stale FBO contents (a full-screen quad does NOT relieve that obligation)"* — but nobody has ever timed a mid-frame FBO bind against a direct draw on this device. That is the one number that could move the estimate, and it can only move it the wrong way.

### The structural reason, which is stronger than the arithmetic

A view buffer wants a subtree that is **static while a sibling animates**. The gate has already removed every frame in which *nothing* animated (`idle.rs:11-19`: 39.6% → 1.67% of a core; 60 → 0.5 presents/s). What still presents is, by construction, frames where something moved — and in this UI the moving quantity is almost always either *inside* every subtree (a focus-pop scale spring per cell) or a transform/alpha applied to *all* of them:

- `detail-transition`: `art_a = 1.0 - sf` (`detail.rs:1571`) and `hero_vis` are functions of the scroll spring, and the oscillator steps focus every frame reversing every 450 ms (`app.rs:3362`), so the composite differs on **every** presented frame.
- `library-switch`: the presented frames are exactly the cross-fade (`p.alpha(xf().alpha())`, `library.rs:1050`) and the commit frame, where the item set itself is replaced.

The granularity at which content is static is precisely the granularity at which the gate already skipped the frame. There is close to nothing left in that residual.

Two more facts worth knowing before anyone tries anyway:

- **The FBO capability is proven** — `gfx.rs:778-793` (`cap_target`, with `glCheckFramebufferStatus` and latch-off) and `gfx.rs:825-906` run 1080p→960→480 RGBA FBOs on this Mali today at ~29 fps while the UI holds 60. So this is a cost verdict, not a feasibility one. But note the app has **never blended into an FBO**: `gfx.rs:329` sets one global `glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)`, there is no `glBlendFuncSeparate` anywhere, and every shader emits straight (non-premultiplied) alpha. A *transparent* layer needs both, which is a second global GL regime.
- **It would be group opacity, not element opacity.** `Painter::c` (`ui/mod.rs:274-277`) multiplies the cascade alpha into each primitive; `Painter::ambient` (`ui/mod.rs:354-364`) carries a bespoke mix-toward-`SURFACE_APP` hack that exists *because* an opaque full-screen field cannot be alpha-faded. A snapshot silently converts both. That is a look change, not a pure optimisation.
- **Memory**: 8.29 MB per full-screen layer = **+23%** of the measured 35.8 MB Mali footprint of a Home grid, competing with the 64-slot poster store (`posters.rs:24`) and the 160-slot glyph cache (`text.rs:131`) — the two caches that *are* paying for themselves, and whose thrash is the documented cause of scroll judder (reverting `TCACHE 160→48` drops `detail-transition` to ~34 fps, `tests/README.md:99-100`).

### What would have to be true for a view buffer to pay here

Three conditions, all of which currently fail:

1. A subtree whose replaced content is **more than one texture fetch per pixel** — i.e. genuine multi-pass overdraw, not single-pass tiles. The only place in the app with that shape is the detail hero band, where four near-full-screen passes stack (`detail.rs:1592`, `:1595`, `:1617`, `:1631`).
2. That composite must be **invariant** across frames — today `art_a` and `hero_vis` both change every frame, so it is re-rendered anyway.
3. Memory to spare, which means not while a 64-poster LRU and a 160-glyph cache are what's holding the frame rate up.

Concretely: **if** the detail backdrop were re-authored so the hero band's composite were fixed under a pure translate (parallax rather than a ground↔art cross-fade), then a hero-band layer would replace ~4 passes with 1 and would be worth measuring. That is the whole list.

The nearest thing to a payoff today is a *whole-screen* snapshot for `ui::nav`'s route dip — the page is opaque (`theme::CLEAR_RGB` **is** `theme::SURFACE_APP`, `ui/theme.rs:171,175`) so it composites correctly with no blend surgery, and the exact grab already runs on device (`glCopyTexSubImage2D`, `gfx.rs:837`). But its cacheable half is the OUT leg only — `OUT_MS = 70` (`xfade.rs:33`), ~4 frames — the IN leg cannot be cached because the destination's mount springs are the animation, and **that fade currently reports no motion to the gate at all** (`Xfade` is `{phase, t: f32}` ramping at `xfade.rs:134`/`:151`, no spring, so `note_spring` never sees it — `docs/retui-invalidation-design.md:171-179`). So its cost is not merely unoptimised, it is unmeasured, and `home-detail-nav`'s 62 is a loop rate.

---

## Q2 — is it thermal?

> **ANSWERED 2026-08-19, and the answer is no: it was the INSTRUMENT.** A control leg reads
> **60 fps, min 60, max 60** across six independent runs on a set that had been up 1 h 42 m at the
> first measurement and 2 h 15 m at the last, under continuous load, with per-run drift 0.00–0.50
> fps on every configuration. Arming the HWCNT profiler drops that same control leg from 60 to 45
> and compresses the legs toward each other; `frame.ui` brackets every frame with two `glFinish`es.
> **Every archived 50 fps reading in this project's performance notes came from a profiled run.**
> The unmeasured-hypothesis verdict below was right to withhold judgement; what it could not know
> is that the number it was reasoning about belonged to the measuring apparatus. Caveat kept: a
> genuinely COLD leg was never obtained — the set was warm and busy throughout, and it exposes no
> temperature sensor and no GPU clock anywhere in sysfs. What can be said is that a set this warm
> shows no decay and holds a clean 60. See `glass-hardware-budget.md` §7.

### You may be right. The evidence does not currently say either way, for three separate reasons.

**1. The metric changed meaning under you.** `fps_tick` increments `frames_ct` unconditionally (`app.rs:252-260`); a skipped frame is `SDL_Delay(16)` (`app.rs:3541`, `idle.rs:84`). So an idle loop reports ~1000/16.x ≈ 60–62 and a vsync-paced loop reports 60 — **these are not distinguishable from `FPS=` alone**. A mixed second lands anywhere between. `pres=` (`app.rs:3608`) is the present count and the only fill-relevant number now. Three scenes already carry `_idle_gate_note` saying exactly this (`tests/manifest.json:26`, `:78`, `:94`), and only **one** scene grades `pres=` at all — `home-idle`, and as a *ceiling* (`manifest.json:46`, `run.py:1140-1158`). Eight of ten UI scenes currently have no valid fill gate.

**2. What the ranking actually supports: fill, weakly.** Restrict to the scenes that genuinely present continuously and the order is inverse to full-screen passes, not to elapsed time:

- **detail-transition 53** — `frame_clear` + opaque `AmbientWash` + full-bleed blended backdrop + full-screen scrim ramp + corner wedges. Critically, **the wash and the art skips are not mutually exclusive**: the wash draws while `art_a < 0.99` (`detail.rs:1592`) and the art draws whenever `art_tex != 0`, which is `art_a > 0.01` (`detail.rs:1574`, `:1595`). With `SCROLLED = 800` (`detail.rs:269`), **both** full-screen passes run for scroll ∈ (8, 792) px — and `detailosc` parks the page in exactly that band, reversing every 450 ms (`app.rs:3362`). ~4× overdraw.
- **library-switch 57** — grid + a 3-quad top scrim (`library.rs:1128-1141`) + a full-screen popover scrim when a menu is open (`library.rs:1188` → `popover.rs:57-60`).
- **home-grid / library-scroll 60** — one wash + cards.

Suite *positions* for those five are 2, 4, 7, 8, 9 → 60, 53, 60, 57, 60. Non-monotone in time, monotone in fill. That is evidence against cumulative thermal accumulation — weakened, not destroyed, by the `make kill` + relaunch between scenes (`run.py:1099`).

**3. The suite could not see a thermal ramp even if there were one.** `run_secs` is 18–36 s with 5–8 s of warmup discarded — the window in which a ramp would *start* (`manifest.json:15-148`). And the only thermal claim anywhere in the tree is `tests/README.md:96` — *"Floors have margin … because the panel GPU thermally throttles"* — asserted as floor rationale with **no measurement cited anywhere**. That sentence is where "the TV throttles" became common knowledge in this project. Mark it a hypothesis.

**4. There is a third hypothesis nobody has ruled out.** 50 is not on the 60/n ladder (60/30/20/15), so a clean 50 cannot come from frame-doubling on a missed deadline — but it *is* exactly the European panel refresh, and this is an SM9000**PLA**. We cannot outrun the compositor: `SwapInterval(0)` does not raise the rate because the wayland frame callback overrides it (measured; the diagnostic and its intent are at `app.rs:326`). If the callback follows a 50 Hz display mode, we read exactly 50 with nothing throttled and nothing over budget. **Unverified** — no panel-mode reading exists in the tree — and it is the cheapest of the three to eliminate.

### The one experiment that settles it

Fixed scene, vary only thermal history, and log the **ordered** `pres=` series. No code changes.

**Arm** (clear first — `make run` clears only the event log, so a stale trigger silently changes the screen):

> **The `/tmp/plxnative-…` paths in this section predate the two-install split: they are the STABLE
> install's runtime root.** A flavoured install puts the same names under `$(make -s print-rundir
> FLAVOR=<f>)` — `/tmp/com.beb.plxnative.debug` at the tracked `FLAVOR ?= debug` default — so
> pasted as bare `/tmp/…` this arms one install while `make run` launches the other, and the ordered
> `pres=` series comes off an unarmed screen. The block below is therefore scoped with
> `R=$(make -s print-rundir)`, which is also what keeps its `rm -f` from reaching across and wiping
> the other install's triggers; the legs and the numbers are unchanged. See `docs/two-installs.md`.

```
R=$(make -s print-rundir)                  # this install's runtime root, never bare /tmp
rm -f $R/plxnative-*                       # keeps the 3 append-only *.log files
printf '2012' > $R/plxnative-detail        # detail-transition's rk, manifest.json:62
touch  $R/plxnative-detailosc              # permanent scroll, app.rs:484 / :3362
printf '0.01' > $R/plxnative-framedrop     # log EVERY frame — 0 is filtered out at app.rs:526
touch  $R/plxnative-noidle                 # pin present==loop so pres= is unambiguous
```

`plxnative-noidle` is in the DIAG list (`app.rs:386`) so it does **not** suppress the boot picker — `plxnative-detail` is what does that. Arming noidle alone lands you on the profiles picker.

**Run** four legs, teeing to a host file (`run-stream` tails forever; `make run` truncates the log each launch at `src/main.c:103`, so the host tee is the *only* place the time axis can survive):

| leg | condition | duration |
|---|---|---|
| **0 — control** | same, but `/tmp/plxnative-login` instead of `plxnative-detail` (near-empty screen, ~zero fill) | 3 min |
| **A — cold** | TV in standby ≥30 min, WoL, launch | 40 min |
| **B — hot restart** | relaunch within ~30 s of A ending | 3 min |
| **C — recovered** | `make kill`, app closed 15 min, relaunch | 3 min |

```
timeout 2400 make run-stream TV=<tv> | tee /tmp/soak-A.log
```

**Log**: `pres=` and `worstframe=` off each 1 Hz heartbeat, plus the `FRAMEDROP total= pump= up= px=` lines.

**Read** — and this matters: **do not use the draw/swap split** the way `app.rs:517-521` documents it. Measured on this stack, `swap` returns in ~0.4 ms and the pipeline syncs *inside* the draw calls, so `draw` reads ~16.6 ms whenever the GPU keeps up — it is throttle-pinned, not GPU cost. Read `total`, `pump`, `up=`, `px=`.

**Result map:**

| observation | verdict |
|---|---|
| Leg 0 reads exactly 50, `worstframe ≈ 20.0 ms`, flat — on a screen with no fill | **50 Hz display mode.** Neither heat nor fill. Investigation ends; check the TV's own picture/motion settings and any active 50 Hz source. |
| A starts ~60, decays over minutes to ~50; **B starts already low; C starts back at ~60** | **Thermal.** History-dependent with recovery and a minutes-scale constant — nothing else produces that shape. |
| A is flat 53–57 from second 1 through minute 40; B and C identical | **Fill.** Content-determined, no time axis, no recovery. |
| `FPS≈50` but `pres` is 0–3 | Not a rendering problem at all — the screen was settled and the *loop* slowed. |

That last row is worth checking first and costs one log line: read `pres=` beside `FPS=` at the moment you next see a 50. (One correction to a hypothesis you may hear: `poster_pump(3)` at `app.rs:3414` runs above the gate, but it `break`s immediately when no slot is `P_DECODED` (`posters.rs:373-380`), so on a settled screen it is one lock plus a 64-slot scan — it can only lengthen iterations *while posters are actively landing*, not in steady state.)

**A box fan across the vents during a repeat of leg A is a legitimate instrument.** If the knee disappears, it is thermal and the discussion is over.

If you want a real thermometer afterwards, the device exposes no `thermal_zone`, no `cpufreq`, and a Mali runtime-PM node reporting `unsupported` — but frequency is measurable by its effect. A fixed-iteration CPU kernel timed against `SDL_GetPerformanceCounter` (`CLOCK_MONOTONIC`, frequency-independent, so rising ms **is** a falling clock) plus a fixed GPU pass bracketed by `gfx::gl_finish()` — the bracket already exists verbatim at `ui/profile.rs:32-44` — sampled at 1 Hz, is ~40 lines behind a new DIAG trigger. Build it as a standalone probe in the shape of `tools/sockprobe.c` / `tools/threadprobe.c` (`Makefile:253-263`) if you'd rather not touch the app.

### The harness cannot answer this today, and the fix is ~30 lines of Python

`fps_stats` **sorts** and returns only `{n, min, median, robust_min}` (`tests/run.py:1078-1084`); `run_fps_scene` folds those into a string and lets the ordered list fall out of scope (`:1124-1138`); `lines` is captured via `make(..., capture=True)` and **never written anywhere** (`:1117-1122`, `:143-146`). On the TV, `src/main.c:103` opens the event log `"w"`, so each scene's launch destroys the previous scene's. **A monotone 60→53 decay and a flat 53 produce byte-identical harness output.** The one surviving hint is `median` (a decaying series has `median >> robust_min`), and it is printed but never asserted.

Four cheap fixes, in order of value:

1. **Tee** each scene's filtered log to `tests/out/fps-<scene>.log`. Two lines. Without this the time axis does not exist anywhere.
2. **`trend`** beside `robust_min`: `median(first third) − median(last third)` of the *ordered* post-warmup samples, printed always, flagged `THERMAL-SUSPECT` past ~3 fps of decline. That flag alone would have made this whole question answerable from an ordinary run.
3. **`present_floor`** — the mirror of the `present_ceiling` block at `run.py:1140-1158` (same `parse_pres`, same `len<5` guard, `sorted(pres)[1]`, `>=`) — and grade the continuously-presenting scenes on `pres=`, not `FPS=`. `docs/retui-invalidation-design.md:244` already specifies this exactly.
4. **`--soak <scene> <minutes>`**, so the experiment above doesn't have to be driven by hand next time.

---

## The duty-cycle point, stated plainly

The gate's win was not that frames got cheaper — it is that **there stopped being frames**. 39.6% → 1.67% of a core, ours 15.4 → 1.05, `surface-manager` 24.2 → 0.62, presents 60/s → 0.5/s. On a passively cooled SoC the metric that matters is joules per hour, and a TV parked on Home for hours now enters a browsing session **cold** instead of at the steady-state temperature of a 60 Hz composite loop. If the drop you're seeing is thermal, that single change is most of the available fix, and it is already shipped.

It also reprioritises everything else. The compositor charge is **per-present and content-independent** — 23.8% flat picker / 23.8% Home grid / 24.8% hero / 26% during playback with our plane fully transparent and zero draw calls — and it is *larger than our entire process*. Drawing less cannot touch it. Only presenting less can. Which means the largest remaining duty-cycle item in the product is **playback**, which the gate deliberately excludes (`app.rs:3436`, because `system.rs:36-38` documents the video plane as slaved to our wayland surface) and which runs for two hours at a stretch at 26% compositor + 25% tvservice while a HUD-hidden frame issues *zero draw calls*. A detail page a user looks at for eight seconds is thermally irrelevant no matter how many full-screen passes it draws.

---

## Ranked: what's actually worth doing, cheapest first

| # | Do | Cost | Expected win |
|---|---|---|---|
| 1 | **Read `pres=` next to `FPS=`** the next time you see a 50 (`app.rs:3608-3613`). | Free, one log line | Tells you which of three problems you have. Can invalidate the entire premise. |
| 2 | **Harness: tee + `trend` + `present_floor`** (`run.py:1078-1084`, `:1117-1126`, `:1140-1158`). | ~30 lines Python, host-only, no device | Makes a thermal ramp visible in every future run; restores a real fill gate to 8 of 10 UI scenes that currently have none. |
| 3 | **The soak A/B above** (legs 0/A/B/C). | ~1 h device, no code | Settles Q2 definitively, including the 50 Hz alternative. |
| 4 | **Declare a wayland opaque region on non-player routes.** `clear_opaque_region` sets it to NULL once at boot (`system.rs:30-40`, `:77`) and re-asserts NULL only per player frame (`app.rs:3456`), so on Home/Library/Detail the compositor alpha-blends a 1080p plane every pixel of which `gfx::frame_clear` wrote opaque (`gfx.rs:150-155`). | ~10-15 lines against the same `wl_proxy_marshal` seam; must be cleared before the player mounts | Attacks the *largest measured single consumer* at the place it is spent — the same class of lever as the gate, which is the only thing that has actually worked. **Unverified** whether webOS's surface-manager honours it; grade it exactly as the gate was graded (`/proc` jiffy deltas for `surface-manager`, 60 s, A/B on one build). |
| 5 | **Make the fade report motion** — the 2-line `Xfade::is_swapping` stopgap beside `app.rs:3239` and `library.rs:474` (`docs/retui-invalidation-design.md:171-179`). | 2 lines + the 3-line ordering fix | Fixes a *live* freeze (a BACK dip largely doesn't play today; the panel holds the outgoing page until the 2 s keepalive hard-cuts) and makes the two nav scenes measure something. Prerequisite for ever costing the route dip. |
| 6 | **Shorten detail's double-pass window.** For scroll ∈ (8, 792) of `SCROLLED = 800` both the opaque wash and the blended full-bleed art draw (`detail.rs:1592`/`:1595`/`:1571`, `:269`) — ~2.07 M extra fragments/frame. This is inherent to cross-fading an opaque ground into full-bleed art; the only lever is the *width* of the overlap. | A look decision, cheap to try | Removes roughly one full-screen pass (~1.9 ms real) from the slowest scene. Note `detail-transition`'s 53 is largely an artefact of `detailosc` parking in the most expensive band — a real user crosses it in a fraction of a second. |
| 7 | ~~View buffers~~ | — | **Not on this list.** They would have to satisfy all three conditions in "What would have to be true" above, and today none holds. Revisit only if (a) the detail hero band is re-authored so its composite is invariant under a pure translate, and (b) someone has actually timed a mid-frame FBO bind on this Mali against a direct draw. |

---

## What I could not verify

- **Every Mali hardware number.** DRAM width and clock, achievable bandwidth, per-fragment energy — none of it is in the tree and no sysfs exposes it. The Q1 conclusion deliberately rests on the *ratio* (16.6 MB vs 5.25 MB), which is arithmetic, not on any bus figure.
- **The cost in ms of a mid-frame FBO bind** (the tile flush + preserve-load). The obligation is documented at `gfx.rs:851-853` from the capture path; the magnitude has never been measured. It is the single number that could change the estimate, and only in the direction of making view buffers worse.
- **The 50 Hz panel-mode hypothesis.** No refresh-mode reading exists anywhere in the tree; the SKU-to-region inference is from the model name only.
- **Whether the compositor honours a wayland opaque region here** (item 4), and whether the video plane survives a route change back into the player afterwards. Both are device questions.
- **`idle.rs:28`'s ~439 springs/frame**, and the profiler's ~2.3× inflation factor — both project-memory figures, not re-derivable from source.
- Nothing below has been compiled, deployed or run: the device is handed back, and every claim above is either a line of source I opened or a labelled measurement from the project's own record.
