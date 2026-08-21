# Verdict: no per-region damage tracking

> **Field names in this document predate 2026-08-01 and the old name was REUSED.** Where it says
> `FPS=`, today's heartbeat says **`loop=`** (loop iterations); where it says `pres=`, today's says
> **`fps=`** (frames actually presented). The manifest gates moved too: `floor`→`loop_floor`,
> `present_floor`→`fps_floor`, `present_ceiling`→`fps_ceiling`, and `fps_stats`→`rate_stats`. The
> text below is left as written, with the line numbers of its day, because it is a dated record of
> an investigation rather than live guidance — see `CLAUDE.md` for the current names.

> **RESOLVED 2026-08-19 on the device. Read this box before the verdict below.** §6 named the
> experiment that could overturn this note, and it was run, along with §5's surviving lever. Four
> results, all measured:
>
> 1. **§6's flip condition is NOT met: animating frames are not fill-bound.** Removing 37.2% of a
>    frame's rasterized quads moved `GPU_ACTIVE` by −0.81%, with `ARITH_WORDS` −0.08% and
>    `TEX_WORDS` exactly 0. The arithmetic pipe sits at 89.5% occupancy against load/store at 44.8%.
>    **Anything that pays on this part removes ALU or texture work, not fragments.**
> 2. **§5's "cheap lever that survives" is dead.** Declaring the surface opaque was armed and
>    proven armed: `GPU_ACTIVE` +0.03%, `surface-manager` inside its own 6.3% on-leg spread, and
>    `TEX_WORDS` **identical to the byte** — LSM's full-screen blit does not go away. Captures
>    differ in 0 of 6,220,800 bytes. By this note's own go/no-go rule that closes the compositor
>    branch permanently: its ~28–34% of every frame is not addressable from inside this process.
> 3. **§4's second blocker is confirmed and stronger than stated.** `EGL_SWAP_BEHAVIOR` is
>    `BUFFER_DESTROYED`, the config carries no `SWAP_BEHAVIOR_PRESERVED_BIT`, and
>    `eglSurfaceAttrib(EGL_BUFFER_PRESERVED)` returns **EGL_BAD_MATCH**. Every buffer-preservation
>    scheme is dead on this device, exactly as §4 argued.
> 4. **But the direction §4 priced is NOT dead, because it priced the wrong mechanism.** §4 costs a
>    persistent full-screen FBO at ~19 MB/frame against ~8, which is right — and irrelevant, because
>    `EGL_KHR_partial_update` needs no FBO and no blit: the driver keeps the untouched tiles.
>    **This driver does not ADVERTISE the extension and implements it anyway.** A 480x270 damage
>    rect declared while still drawing everything takes `GPU_ACTIVE` 7,531,684 → 542,012 and
>    `FRAG_NUM_TILES` 4096 → 151. That is the mechanism's ceiling under a 6.25% rect, not a saving:
>    the real damage distribution extrapolates to roughly half an animating frame and near zero on
>    a cross-fade, and an unadvertised extension is not a portability contract. Full account:
>    `egl-partial-update-and-damage.md`.
>
> One factual correction to the table in §3: it reports the sysroot's `libEGL.so.1.4` as exporting
> zero `egl*` symbols. That is true of `sysroot/`, which holds libraries pulled off the TELEVISION.
> The NDK's link stub of the same name exports 44 — and linking it would still be fatal, for a
> different reason: its SONAME is `libEGL.so.1`, while webOS 2.2.3–5.3.1 ship `libEGLfk.so.2` and
> their own `libEGL.so.1.5` exports nothing, so a `DT_NEEDED` on it kills the process at `exec()`.
> The probe resolves through `RTLD_DEFAULT` instead; the `fwcompat` matrix is byte-identical.

**No — and not because immediate mode forbids it.** Every number we have is CPU (`/proc` jiffy deltas, `idle.rs:13-19`), and per-region damage removes GPU fragments, which appear in none of them; on the axis we measured, its ceiling is bounded by the 0.37 ms swap, and an honest hour-integration puts the whole prize under **0.25% of one core** — smaller than the 2 s keepalive we deliberately spend on insurance (`idle.rs:75`). The premise ("we're changing this anyway") is false in a specific, checkable way: the motion capability's coverage claim is *"there is nowhere else to get a time-varying value from — not anything keeping a list"* (`retui-invalidation-design.md:290`), and a damage rect is exactly a list, attached at a site (a scalar `Spring`) that structurally does not know its own geometry. What survives is one compositor-side piece of work that is not damage tracking at all: **we have declared our surface fully non-opaque for the app's entire lifetime, on six screens with nothing behind them** (`system.rs:77`).

---

## 1. Is "we're changing it anyway, so do it now" true?

It is a reasonable thing to have assumed, and it would be true in UIKit. It is false here, and the reason is worth stating precisely because it is not "we don't do that".

**UIKit's dirty tracking is not free-standing — it rides the retained layer tree.** `setNeedsDisplay` marks a `CALayer` whose backing store is a cached bitmap. The win is not scissoring a redraw; it is *not re-rasterizing content into the cache*, after which a separate compositor recomposites cached bitmaps. The rect is free because the layer already has a frame and an identity.

retui has neither. `mod.rs:406-416` is explicit: *"every frame clears + redraws the whole tree, so the way to 'avoid drawing' is to CULL what isn't visible, not to dirty-track what changed."* There is no layer to hang a rect on, no backing store to keep clean, no identity. Porting the optimisation means porting the substrate.

**Overlap with the accepted motion work — the honest accounting:**

| | motion capability | per-region damage |
|---|---|---|
| touch the 48 `.step(` sites (verified: exactly 48) | yes | yes — **the only overlap** |
| the animator's *geometry* | never needed | required at every site |
| painted-extent inflation on 11 `gfx::draw_*` (`gfx.rs:337,360,382,402,414,434,457,554,669,676,684`) | no | required |
| intersecting clip stack (7 replace-semantics sites) | no | required |
| back-buffer preservation | no | required, **no API exists** (§4) |
| a pixel-correctness gate | no | required, **deliberately deleted** (`ui-framework-improvements.md:492-500`) |

And the deeper problem is not size, it is direction. To get a rect you need one of two things:

- **Hand-attribute geometry per animator.** `snap.step(target, K_SNAP, dt)` (`home.rs:1196`) is three scalars; nothing at that call knows it moves the whole grid. Hand-authoring "what does this spring move" at 48 sites *is* the hand-maintained list the motion design exists to kill — and that list has already failed once: `idle.rs:132-138`'s census was false the day it shipped (`idle.rs:134` names a route-change `invalidate` that does not exist; confirmed at `retui-invalidation-design.md:19`).
- **Record the whole frame and diff it against the last one.** Both accepted designs refuse 100% recording — `ui-framework-improvements.md:52` ("**Not** 'record everything and sort'"), `:192` ("`(Content, 0)` is not a bucket — it is the immediate stream"), `:232` ("Unmigrated code never enters the recorder at all"), `:279` ("~95% of every frame never touches the recorder"), with `const CAP: usize = 64; // PER BUCKET` at `:223`. `ui/layer.rs`, `ui/frame.rs`, `ui/hit.rs` do not exist.

So damage is not the next step past the motion capability. **It is the opposite architecture on the one axis that matters**, and deferring it forecloses nothing, because nothing is converging on it. (`retui-invalidation-design.md:23` already says this in those words.)

---

## 2. The numbers

From the A/B (39.6 → 1.67 points of one A53; 60 → 0.5 presents/s):

```
one present = (39.6 − 1.67)/59.5      = 6.38 ms of one A53
  compositor  (24.20 − 0.62)/59.5     = 3.96 ms   62%   ← no retui rect reaches this
  ours        (15.43 − 1.05)/59.5     = 2.42 ms   38%
     swap                              = 0.37 ms   ← the only place a GPU stall surfaces
     pumps                             = 0.004 ms
     remainder (poll + ~439 springs
       + tree walk + GL submission)    ≈ 2.05 ms   ← CPU. A scissor does not touch it.
```

**What is left after the gate, and what damage can reach:**

| segment | residual | damage's reach |
|---|---|---|
| playback (gate excluded, `app.rs:3436`) | **26 points**, measured | **0** — `app.rs:3434-3435`: *"Playback also spends ~99% of its time with the HUD auto-hidden, where the frame is already 0 draw calls."* |
| settled UI | **1.67 points**, measured | **0** — on an empty union the gate removes 6.38 ms; damage removes at most the fill inside 2.42 |
| animating UI | 38 points *while animating* | ≤ the swap term |

Bound it generously: assume **6 continuous minutes of animation per hour** (≈21,600 presents), and assume 100% of the 0.37 ms swap is fill and 100% of that fill is redundant. That is 8.0 s of core time per hour = **0.22% of one core**. The keepalive we chose to pay for insurance is 0.5 × 6.38 ms = **0.32%**. *The entire prize is smaller than the insurance policy.*

**The frame-time argument is the only one that could survive — and it has never been measured.** The fps ladder is 50/45 (`tests/manifest.json:20,32,58,72,112,125,139,154,168`), and `:14` says a floor is a pass threshold ("2nd-lowest steady FPS >= floor"), not a recorded rate. The `_floor_note`s do reason in fill terms (`:71`, `:138`) — but **nothing in the repo records what any scene actually runs at**, so "the 45 tier goes back to 60" has no baseline. Two things point away from fill:

- Pre-gate, a **still** Home sustained 60 presents/s under compositor pacing. In immediate mode a still Home draws the *same tree* an animating Home draws. So Home's full draw already fits inside 16.7 ms *alongside* LSM's composite.
- `item-menu` — the scrim'd popover over a live grid, the canonical "damage obviously wins here" scene — carries the STALE-GATE note at `tests/manifest.json:78`: *"this scene's screen SETTLES, so for most of the window the app presents ~0 frames."* It presents ~0/s today. Same note on `home-hero` (`:26`) and `person-page` (`:94`).

---

## 3. Disputed facts, resolved

| claim | verdict |
|---|---|
| `Painter::clip` set-sites | **7**, not 6: `table.rs:315`, `widgets.rs:220/224/334/1654/1797/2060`. The "six" everyone cited are the `clip_clear()` lines. B5's own sizing (`ui-framework-improvements.md:460`) cites `card_row.rs:293/298/301` — **stale**, `card_row.rs` has no clip. `ui/CLAUDE.md`'s "`TableView::draw`, its one user" is also stale. |
| `sysroot/usr/lib/libEGL.so.1.4` exports `eglSetDamageRegionKHR` | **FALSE.** Checked with both `nm -D` and `objdump -T`: 14 dynamic symbols, **all CRT** (`_init`, `_fini`, `__cxa_finalize`, bss markers). Zero `egl*` symbols. 5140 bytes, only real `DT_NEEDED` is `libmali.so`. **Nothing about which EGL extensions this device has is knowable from this tree.** |
| device SDL's EGL entry points | **18**, from its `"Could not retrieve EGL function …"` strings. `eglGetProcAddress` and `eglQueryString` are present; **no** `*WithDamage*`, `eglSurfaceAttrib`, or `buffer_age`. |
| `/tmp/plxnative-nodraw` exists | **No.** 45 triggers in the tree; `nodraw` is not one. |
| `capture.rs:344` is a libEGL dlopen precedent | It dlopens **`libturbojpeg.so.0`** (`capture.rs:337-353`). The precedent is real; the target was mis-described. |
| Home's hero "already pays an opaque full-screen wash every frame" | **Backwards.** `home.rs:444-447` is the *skip*, and its comment says why: *"without the first the hero view pays an extra ~2M-fragment pass it does not need."* |
| wayland damage is a union / opcodes | **Confirmed verbatim.** `WL_SURFACE_DAMAGE 2` (`wayland-client-protocol.h:3096`), `SET_OPAQUE_REGION 4` (`:3098`), *"the new pending damage is the union of old pending damage and the given rectangle"* (`:3252-3253`). A hand-marshalled rect can only **add**. |
| Midgard preserve-load | **Confirmed verbatim**, `gfx.rs:850-852`: *"glClear before each pass spares Midgard the tile preserve-load of the stale FBO contents (a full-screen quad does NOT relieve that obligation)."* All six screens open with `frame_clear`. |
| no EGL anywhere in the crate | **Confirmed** — `grep -rniE '\begl\|buffer_age\|BUFFER_PRESERVED\|SwapBuffers\|wl_surface_damage' rust-modules/src src` returns nothing. |
| 1920×1080 as an FBO **attachment** | **Unverified.** `cap_tex(1920,1080)` (`gfx.rs:804`) is only ever a `CopyTexSubImage` target; only 960×540 and 480×270 go through `cap_target` (`gfx.rs:805-806`). |
| `HERO_AUTO_S` | `home.rs:41` (not `:40`); the auto-flip loop is `home.rs:1185-1192`. |

**Not confirmable without the TV, and none of it should decide this:** whether LSM honours client damage at all; whether the Mali blob exports `EGL_KHR_swap_buffers_with_damage` or `EGL_EXT_buffer_age`; whether LSM's charge scales with composited area; whether the video plane survives a non-presenting or zero-damage commit; whether *any* 45/50-tier scene is fill-bound.

---

## 4. What damage would actually do to this stack

Three independent blockers, each sourced:

**The tiler punishes partial redraw.** Not clearing is the whole point of a damage scissor, and `gfx.rs:850-852` says not clearing costs a full-screen tile preserve-load. You trade a free clear for 8.3 MB of load traffic per frame, on LPDDR shared with the video decoder and LSM, to save fragments that `fs_img.frag:3-5` records as *"~85% of a card's fragments are strictly inside the rounded rect (d < −2)"* — a 2-instruction path.

**You cannot know what is in the back buffer.** SDL swaps through EGL with no `eglSurfaceAttrib` (`EGL_BUFFER_PRESERVED`) and no `EGL_EXT_buffer_age` in its 18 resolved entry points. At swap N you inherit N−2 or N−3 and have no way to tell which. The only correct answer is a persistent FBO that never rotates — which is then *two* passes: an FBO pass that must not clear (preserve-load + store) plus a back-buffer pass that clears and samples an 8.3 MB texture. ~19 MB/frame against today's ~8 MB.

**No rect you can marshal shrinks what the compositor is told.** `wl_surface.damage` is a union into pending state (`:3252-3253`), SDL's `eglSwapBuffers` posts full-surface damage and commits, and we deliberately do not own the commit (`system.rs:36-38`: *"a bare commit here presents a null-buffer surface and disrupts the slaved video plane"*). So the compositor's 3.96 ms — 62% of every present — is untouched by anything retui can do.

**And there is no gate in this project that can see a one-pixel seam.** `cargo test --lib` cannot link GL (`gfx.rs`/`text.rs` carry bare `extern "C"` with no `cfg(test)` stub, unlike `ff.rs`'s `cfg_attr(not(test))`-gated `#[link]`s). The FPS scenes grade a number, never an image. `pixdiff.py` + golden captures were deliberately deleted (`ui-framework-improvements.md:492-500`). A missed `invalidate` is stale-but-coherent and bounded at 2 s (`idle.rs:75`) and already gated (`present_ceiling: 3`, `manifest.json:46`). A missed damage rect is a smear only the television can show you — and the television is gone.

---

## 5. The cheap lever that survives — and it is not damage

### Declare the surface opaque on the six screens with nothing behind them

**What's there today.** `clear_opaque_region()` marshals `set_opaque_region(NULL)` (opcode 4, `system.rs:39`) and is called exactly **twice** in the whole app: once at boot inside `sys_grab_wayland` (`system.rs:77` ← `app.rs:336`), and once per frame on the player route (`app.rs:3456`). The opaque region is double-buffered but otherwise sticky — *"Otherwise, the pending and current regions are never changed"* (`wayland-client-protocol.h:3337-3339`). **So the surface has been declared fully non-opaque for the app's entire lifetime, including Home, Library, Detail, Person, Login and Profiles**, where nothing is beneath us.

**Why it might matter.** The protocol's own words: the opaque region *"is an optimization hint for the compositor that lets it optimize the redrawing of content behind opaque regions"* (`:3320-3324`). All four compositor measurements (23.8 / 23.8 / 24.8 / 26) held surface area fixed at 1920×1080 — content-independent is not area-independent, and nobody has varied the variable. If LSM's charge is blend/area-related rather than pure commit bookkeeping, this is the **only** lever that reaches it, with zero retui change, no EGL extension, no new library.

**Honest cost — not five lines.** A `wl_region` comes from `wl_compositor.create_region` (opcode 1, `wayland-client-protocol.h:1146`, a `wl_proxy_marshal_constructor` against `wl_region_interface`), and `sys_grab_wayland` captures only `wl_display` and `wl_surface` (`system.rs:71-73`). There is no compositor proxy in the app; getting one means a registry bind and a roundtrip on a display SDL owns. Call it ~100 lines in `system.rs`, one file, behind a trigger.

**Real risk to check.** Our destination alpha is not 1 everywhere. `frame_clear` clears to alpha 1.0 (`gfx.rs:152`), but the blend is non-separate `glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)` (`gfx.rs:329`), so a 0.5-alpha overdraw leaves dst alpha at 0.75 — every AA glyph edge, scrim and focus glow. The protocol warns *"marking transparent content as opaque will result in repaint artifacts"* (`:3326-3327`). Visually we *want* LSM to ignore alpha here; it still has to be seen on the panel. And it must be route-scoped and re-asserted the way the player path already re-asserts NULL every frame, or the first frame after leaving playback occludes a plane that has not torn down yet.

**Go/no-go, one boot.** Arm `plxnative-noidle` in this install's runtime root
(`$(make -s print-rundir)`, not a bare `/tmp` — at the tracked `FLAVOR ?= debug` default those are
different directories, and arming the wrong one leaves the present gate ON, so Home idles instead
of presenting and BOTH legs measure the same settled screen. That produces exactly the flat result
this section's rule reads as "the entire compositor branch closes permanently") so Home presents at
60 and the charge is visible. A/B `surface-manager`'s 60 s `/proc` jiffy delta with a full-screen region set vs today's NULL, `DISPLAY` capture each way. **Flat ⇒** LSM's charge is per-commit, the entire compositor branch closes permanently, and the present gate stays the only thing that ever reaches its 62%. **A drop ⇒** it is area/blend-related, and this is a free win on every presenting frame of every screen — still not damage tracking.

### Three others, all worth more than damage

1. **Extend the gate to the player route** (`app.rs:3436`) — ~26 points for however much of the day is playback. Also the single riskiest unverified change on the table: `idle.rs:49-53` and `system.rs:36-38` both record the plane as slaved. Triggered A/B with `DISPLAY` captures every 10 s, never a default.
2. **Cap the hero auto-flip** (`home.rs:1185-1192`, `HERO_AUTO_S = 8.0` at `:41`). A TV parked on Home's billboard *never* idles — every 8 s the gate re-opens for the flip's settle time, forever. ~5 lines to stop after N cycles without input. It is a product decision as much as a perf one, and it is what makes the parked-TV case actually do what the gate was built for.
3. **A settle predicate in pixels**, generalising `HERO_SLIDE_REST_PX = 0.5` (`home.rs:62`) over the scalar `MOTION_EPS = 1e-3` (`idle.rs:65`). Removes whole presents — including the compositor's 62%, which no damage scheme ever can.

---

## 6. The device experiment that overturns this verdict

**Question: does any animating frame spend its time in fill?** Nothing else can reopen the case.

> **The `/tmp/plxnative-…` paths in this section predate the two-install split: they are the STABLE
> install's runtime root.** A flavoured install puts the same names under `$(make -s print-rundir
> FLAVOR=<f>)` — `/tmp/com.beb.plxnative.debug` at the tracked `FLAVOR ?= debug` default — so
> pasted as bare `/tmp/…` this arms one install while `make run` launches the other, and every
> number here is then read off an unarmed screen. The block below is therefore scoped with
> `R=$(make -s print-rundir)`, which is also what keeps its `rm -f` from reaching across and wiping
> the other install's triggers; the experiment itself is unchanged. See `docs/two-installs.md`.

```
R=$(make -s print-rundir)                              # this install's runtime root, never bare /tmp
wake-tv; ssh root@TV "rm -f $R/plxnative-*"            # a stale trigger changes which screen you boot to
printf 'hm.grid' > $R/plxnative-profile                # one asynchronous timer-query phase per run
touch $R/plxnative-homeosc                             # perpetual grid focus sweep: never settles, gate never fires
echo "$TOKEN" > $R/plxnative-token
make run RUN_SECS=40      # then read the once-per-60-frames aggregate lines out of the event log
```

There are **11** brackets already in the tree: `hm.backdrop`/`hm.hero`/`hm.grid` (`home.rs:1127`/`:1129`/`:1131`) and eight `dt.*` (`detail.rs:1402-1493`). Select and run them one at a time. Repeat with `echo <ratingKey> > $R/plxnative-navosc` (`home-detail-nav` — by the manifest's own reasoning at `:138` the heaviest thing in the app) and with `$R/plxnative-libswitch`.

Then, separately and without either profiler armed: `echo 14 > $R/plxnative-framedrop` on the same three scenes. `app.rs:521` states the read verbatim — **"high `swap` with low pump/draw ⇒ GPU fill."**

**Flip condition.** If individual phase GPU time on a *sweeping* grid or a nav dip is a large fraction of 16.7 ms, or the framedrop lines show `swap` dominating with low `draw`, then animating frames are fill-bound and the question reopens — **as a fill project, not necessarily as damage.** Do not sum separately queried phases as normal frame time; use the whole-frame query for that. The cheaper fill levers are still unspent: `player_hud.rs:92-100` draws every subtitle line **five times** across the full panel at size 36 (`ui-framework-improvements.md` B7, *"biggest un-named fill item in the tree"*), and detail requests a 1920×1080 backdrop into a 64-slot LRU with no byte budget and no `glGetError` check (B10, `posters.rs:24`). Spend those before scissoring anything. If `draw` dominates `swap` and the phases come back small, the 45-tier is CPU, and this closes.

**Second, cheap, and settles the compositor argument permanently:** log `eglQueryString(EGL_EXTENSIONS)` once at boot beside `app.rs:336`, resolved by `dlopen("libEGL.so.1")` + `dlsym` (precedent: `capture.rs:337-353`). This cannot be answered off-device — the sysroot's `libEGL.so.1.4` exports no `egl*` symbol at all. Both "present with empty damage during playback" and every buffer-preservation scheme die without it.

---

## 7. If you still want it

The smallest honest version that could exist on this stack, so the decision is yours with real numbers:

**Scope.** One screen, one animation, over a ground that does not move — the HUD clock, `Button::progress`, or a popover's appear spring over a frozen grid. Not a general mechanism.

**Mechanism.** A persistent 1920×1080 FBO in a new `ui/damage.rs` plus a `[Rect; 8]` union. Render the tree into the FBO under one scissor, blit the FBO to the back buffer. No per-widget backing stores, no identity, no tree paths, no EGL — the FBO never rotates, so "the previous frame is still there" is true by construction. The **default is full damage**: a geometry-free `step`/`invalidate` marks the whole panel, so the enabling commit is pixel-identical and every conversion is one opt-in call site. A forgotten rect is a lost optimisation, never a smear.

**What it costs.**

- **Bandwidth: a regression, not a wash.** ~19 MB/frame (FBO preserve-load + store, then an 8.3 MB texture read + 8.3 MB store) against today's ~8 MB, to save fragments that are already ~85% a 2-instruction path.
- **Prerequisite in code:** B5's intersecting clip stack (`ui-framework-improvements.md:460`), because `Painter::clip` currently *replaces* the scissor at 7 sites and `clip_clear` *disables* it at 6 more plus `ui::guard`'s panic arm (`mod.rs:95`) — any one kills a damage scissor for the rest of the frame.
- **Painted-extent inflation** on the 11 `gfx::draw_*` entry points. Genuinely centralized (`pad = blur + 1.0` at `mod.rs:337`, `GLOW_PAD` at `consts.rs:17`, `AA_BLEED` at `gfx.rs:27`) — this is the *cheap* third.
- **Unverified prerequisite:** 1080p FBO attachability. One boot settles it, and `cap_target` (`gfx.rs:778-792`) already has the `glCheckFramebufferStatus` + latch-off-with-log failure path to copy.
- **Unbuildable-to-standard prerequisite:** a pixel gate. A freeze trigger + `pixdiff.py` + golden captures, all of which were deliberately removed, and none of which can be run without the TV.
- **Size:** ~1200–1500 lines across `ui/damage.rs`, the clip stack, the 11 primitives, the 48 step sites, and the harness. Two weeks *with* a device; not shippable to this project's standard without one.
- **Payoff:** bounded by §2 — under 0.25% of a core on the measured axis, and on the unmeasured axis, unknown until the §6 experiment runs.

**The zero-pixel version, if you want the data instead of the feature.** Put the `Rect` on the reporting seam during motion steps 1 and 8 (`step_in`/`invalidate_in`), feeding a **diagnostic-only** union whose area fraction lands on the FRAMEDROP line (`app.rs:3562`). The 48 sites get touched once instead of twice; nothing changes on screen. But be clear-eyed about what it measures: a union of hand-authored rects is only as good as the hand-authoring, and hand-authoring is the failure mode the motion capability exists to remove. **I'd take that histogram only after §6 comes back saying fill is real** — otherwise it is 60 lines of scaffolding measuring guesses, and it makes the migration's central claim ("nothing keeps a list") false on its first commit.
