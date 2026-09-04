# Liquid glass — what the hardware will and will not give us

The backdrop-blur material (`gfx.rs`'s blur chain + `shaders/fs_glass.frag`, drawn through
`Popover::panel` / `Painter::backdrop_blur`) is real and cheap enough to ship. This note is the
**envelope it can be designed inside** — the limits that come from the television rather than from
taste, so a design can be drawn against them instead of into them.

Everything below is either **measured** on the dev set (LG 49SM9000PLA, webOS 4.10.2, Mali Midgard,
GLES 2) or **derived from a measured constant**, and each is marked. Nothing here is an opinion
about how glass should look.

---

## 1. The one thing that is impossible

**Glass cannot sit over playing video.** Not "expensive" — impossible on this platform.

The decoded picture lives on the TV's hardware **video overlay plane**, composited by the set
*outside* our GL context. `glCopyTexSubImage2D` reads our own framebuffer, where the video shows
through as punch-through alpha, i.e. *nothing*. A glass panel over playback would blur transparency
and come out as a smear of whatever the UI plane happened to hold. There is no API to get the frame
back from Starfish/ACB either.

So the player's panels — track menu, chapters, info card, the `…` overflow, stats — keep their
near-opaque sheet (`theme::PANEL_TOP`/`PANEL_BOT`) **permanently**. Design has to accept one
material split in the product: menus over the UI are glass, menus over video are solid. It cannot be
designed away, only designed *with*.

*(Measured indirectly: `system.rs` documents the plane as slaved to our wayland surface, and the
in-app capture stream — same `glReadPixels` path — has never been able to see video. `docs/` and
`player/CLAUDE.md` carry the full account.)*

---

## 2. One glass REGION per frame — which is not the same as one element

There is **one** snapshot chain and **one** live region in it. But the unit that costs money is the
*region*, not the element, and that distinction decides whether a layout is affordable.

**Elements that sit close together are the affordable way to have more than one.** They share the
same five-pass snapshot chain; a neighbour grows its region and adds a composite, rather than
starting another chain. Exact small-region cost is not claimed—the final measurements disproved
the first two-point model's supposed fixed per-pass floor.

**Elements far apart are the expensive case**, because their union is most of the screen — a bar at
the top plus a sheet in the middle is the whole-screen grab again. That is why the glass tab bar
drops to its flat material the moment any popover opens (`widgets.rs`, `popover::any_open`).

**Implemented, and verified at runtime.** A snapshot is taken at the union of every region the
*previous* frame asked for (`blur_region_union`, `blur_frame_end`). Once that union is known, a
frame holding two glass surfaces takes one capture. The first frame that discovers a non-contained
second surface may still take a second capture. Measured on the simulator with the glass tab bar
and the account panel both live over the Home hero, 644 frames:

```
  1 x  reg=592,0  736x200      <- frame 1: the bar, nothing yet on record
  1 x  reg=0,56   608x344      <- frame 1: the panel, not covered by the bar's grab
642 x  reg=0,0   1328x400      <- every frame after: ONE grab serving both
```

It converges in exactly one frame and, just as importantly, **shrinks back in one frame** when a
surface goes away — the union is taken from the last frame's requests, not accumulated forever, so a
closed menu does not leave the bar grabbing half the screen for the rest of the session. Both halves
are pinned by a host test (`i_two_glass_surfaces_share_one_grab_and_give_it_back`).

Note what that measurement also shows about **distance**: the bar and the account panel are diagonal
from one another, and their union (0.53M px) is 1.5x the sum of the two regions (0.36M). Neighbours
would union to near the sum; opposite corners union to the screen. Which is exactly why the glass
tab bar still drops to flat under an open popover — not because it would thrash, but because that
particular pair is the far-apart case.

*(The earlier version of this file said this was unimplemented and that a containment miss replaced
the region. That was true when it was written; a second element only fitted inside the first's grab
if `gap + width ≤ 20 px`, i.e. never.)*

**Design consequence:** glass belongs to things that are *together*. A row of glass controls is a
reasonable ask; a glass control at one corner and a glass sheet at the other is not.

### Neighbours normally do not refract each other

After the shared union has converged, its snapshot is taken before any glass is drawn, so A's rim
displaces its sample into whatever the *page* holds under B. Two adjacent capsules then bend the
background independently. On the first discovery frame, a non-contained second caller can recapture
after the first glass was drawn; production avoids that transition for the far-apart tab/modal pair
by disabling tab glass while a popover is open.

This differs from Apple's Liquid Glass, where neighbouring elements deliberately interact. The
steady-state shared snapshot contains the host page, not the other glass widgets; design around that
rather than depending on the incidental first-frame ordering above.

---

## 3. Glass does not get cheaper by being small

This is the least intuitive limit and the one most likely to shape a layout.

The first full-screen measurements suggested **≈0.49 ms per pass + ≈2.7 ns per output fragment**,
but that was a two-point sizing fit, not a hardware invariant. The final region-limited run measured
the two-pass `blur.taps` phase at 0.74 ms, below the model's claimed 0.98 ms fixed floor. What the
data does establish is qualitative: five passes carry enough overhead that cost does not fall in
proportion to panel area.

| element | region | snapshot cost |
|---|---|---|
| Library Sort panel (470×630) | 630×790 | ~3.4 ms *(measured)* |
| Account dynamic panel | 608×396 | ~3.9 ms *(measured in the dynamic profile leg)* |
| tab bar track (~710×76) | ~870×230 | not isolated on the final chain |

The smaller Account region was not proportionally cheaper than Sort; run-to-run context and the
serialized profiler matter more than an area-only prediction.

**Design consequence:** do not estimate cost from area alone. **Many scattered glass elements is the
shape to avoid**: five passes still have material overhead, while their union can grow through empty
space. Prefer coherent surfaces (a bar, a sheet, a card) and measure each new geometry on the TV.

---

## 3b. The page under a panel is not redrawn, and the panel's own ground is not either

**A modal costs its host page every frame, and on this GPU that is the whole cost of having one up.**
Measured 2026-09-02 on the debug install, detail page 2012 with the track-information panel open and
paging with DOWN: every presented frame `draw≈83 ms`, `loop=41`, `fps=9`. The same page with no
panel presents at 60 with worst frames ≈29 ms. The draw-class bisect (`/tmp/plxnative-drawmask`)
priced it as fill and nothing else.

`ui::popover::host` is the answer, and it applies to every popover that asks for it rather than to
one — `Popover::caching_host()`. The page is drawn ONCE into a snapshot and served from it as a
single textured quad; the freeze is a REFUSAL at `gfx::culled`, the renderer's one shared gate, so
the page's draw code still runs (layout, hit rects, poster uploads) and only the fill is removed.
That is what lets it serve the four panels which draw from inside their page, which "skip the page
and draw the panel after it" never could.

**The snapshot has two stages, and the second is where the frames are.** Freezing the page alone got
83 → 58 ms; the bisect on that build said the survivors were `glass` and `rect` at ~25 ms each — the
popover's own full-screen modal scrim and its own glass composite, redrawn every frame although
NEITHER CHANGES once the appear spring has settled. So the snapshot grows to page + scrim + lifted
`Opener` + the panel's own ground, taken by `Popover::panel`/`sheet` on the first settled frame, and
only the panel's foreground stays live:

| | draw / presented frame | `loop=` | `fps=` |
|---|---|---|---|
| before | 83 ms | 41 | 9 |
| page frozen | 58 ms | 34–51 | — |
| + panel ground frozen | **40 ms** | **60** | **37–38** |
| `drawmask=all` control, shipped build | 40 ms | — | — |

That last row is the point: with the app submitting no quad at all the frame still costs 40 ms, so
nothing the app draws remains in it. The person page's bio panel is the same shape — `draw≈75 ms` /
`loop=26–52` before, `draw≈24–33 ms` / `loop=61–62` after — and it keeps its DYNAMIC backdrop,
because the scrim is drawn live above the snapshot and a re-source therefore still sees the ramp it
is meant to. A blur SOURCE pass is never served the ground stage; it contains the panel's own frost,
so a backdrop re-sourced from it would frost a picture of itself.

---

## 3c. Banding is an output problem, and there is one dither

The frost is a slow gradient over a broad area, which is the exact case an 8-bit framebuffer turns
into flat plateaus. Reported as "too discreet colours and strange patterns"; measured on Settings
over Home as a 700-row column spanning luma 55.7 → 59.1 in **four** levels, treads of 158, 157 and
146 rows.

Two rules, both of which this material got wrong on its own before 2026-09-02:

- **It is not a precision problem.** Promoting a mix to fp32 changes nothing visible and costs ~4.5
  arithmetic words a fragment. The cure is noise at the OUTPUT.
- **The noise is a texture fetch behind a uniform branch, never a hash.** `fs_glass.frag` carried
  `fract(sin(dot(p,k))*43758.5)` and evaluated it on EVERY fragment of every glass surface — the
  same construction `fs_ambient.frag`'s own header had been recording as a mistake that cost 38% of
  a Home frame. A sine hash is also structured, which is the "strange patterns" half of the report.

`shaders/dither.glsl` is now the one answer for the three programs whose ramp is a blur or a
whole-screen wash (`fs_ambient`, `fs_modal_ground`, `fs_glass` — the per-rect `fs_src`/`fs_shadow`
carried it for two days and cost the hero paging scene 57→50 fps for a branch that answered "no" on
every draw, 2026-09-04), `gfx::dither_for_field` the one policy for which draws pay. Measured on the panel afterwards: the flat runs that ARE the
staircase fall from 9.6 px / 15.7 px to 2.2 px, the horizontal autocorrelation at lag 64 from
+0.144 to −0.010, and the paging numbers in §3b are unchanged.

---

## 4. It is free at rest; moving glass uses the saved 3-present policy

`widgets::Glass::CACHED`, the default, takes **one snapshot per opening** over a still page. A modal
over moving opaque UI opts into `widgets::Glass::DYNAMIC`; a non-modal widget uses
`Glass::DYNAMIC_BACKDROP` to get the same cadence without dimming its host. The widget itself is
still drawn on every presented frame, while a dirty shared backdrop is refreshed on **at most every
third successful present**. `DYNAMIC`'s source dim also changes during the opening fade, so it may
refresh during appear even when the host itself is still. The name in code is
`EveryThirdPresent`, not “20 Hz”, because 20 Hz is only the result while the UI is actually
presenting at 60 Hz. `ui::idle` still gates the whole frame, so settled content creates no private
blur clock and burns no presents.

The cadence and pending-damage state are global, like the snapshot chain: several neighbouring
dynamic widgets cannot stagger their phases into a refresh on every frame. Each owner keeps only
its visibility/source state and reports whether its **host underlay** moved; foreground modal
motion is deliberately excluded. A one-shot data/texture landing between sample slots buys at most
two follow-up presents to reach the next slot, while a pure 2-second compositor keepalive does not
recapture a clean backdrop.

Measured on the dev television over the moving Home hero:

| policy | presented rate | pacing result |
|---|---:|---|
| no recurring snapshot, no scrim | 60.1 fps | clean test reference; not the complete cached preset |
| snapshot every present | 52.6 fps | sustained overload |
| snapshot every second present | 52.7 fps | rejected; 35–42 ms burst every measured second |
| snapshot every third present | ~60 fps | historical cadence experiment; periodic 33–36 ms update frames |

The shipping dynamic preset uses a **page-drawn scrim** between the unchanged host page and the
glass panel. Source-RGB dimming was an experiment and is no longer part of the material API. The
historical A/B measured **40.8 fps with the blended scrim** (`n=19` valid heartbeat buckets) and
**59.84 fps with source dim** (`n=38`), but the later direct source path removed the reason to make
host pages participate in a special dimming mode.

**Current design consequence:** cached glass over static chrome is genuinely free. Dynamic glass
uses the direct source path and refreshes on every changed successful present; an idle page still
takes no recurring snapshots. The reusable `Glass::DYNAMIC` preset remains the product API rather
than an Account-only trigger.

---

## 5. Geometry

| limit | value | why |
|---|---|---|
| **Clearance from the screen edge** | **68 px** (`BLUR_REACH`) | The rim samples up to 38 px outward (the lens) plus ~25 px of kernel spread. Past the panel edge there is nothing to sample; closer than this to the frame the rim reads as a one-colour smear on that side. *Derived from `GLASS_LENS` + the tap ladder.* |
| **Minimum short side** | **~60 px, for a SHEET** | The bevel is 28 px from each edge, so below ~60 px an element is *all* rim and no interior — it still draws, and reads as a solid lozenge of refraction rather than a pane. It also loses the shader's interior early-out, so every fragment pays the full lens. *Derived from `GLASS_BEVEL`.* **A standing container is not bound by this**, because it does not take the ramp — see §5b. |
| **Maximum bevel** | **~40 px** | Past that the ramp covers enough of the panel to be read as *shading* rather than as an edge, and the object stops looking like glass and starts looking vignetted. *Judgement, recorded in `GLASS_BEVEL`'s doc.* |
| **Corner radius** | anything up to a capsule | The lens reuses the `sdBox` distance the rounding already computes, so a full capsule (`h * 0.5`, what the tab bar uses) costs nothing extra. |
| **Slide during appear** | **≤ 20 px** (`POPOVER_MAX_RISE`) | The grab swallows the full travel, so the slide itself forces no extra capture. Dynamic cadence may still refresh during the appear. A larger rise must raise the constant; a host test fails otherwise. |

---

## 5b. The rim is per-surface, and a 76 px bar does not get a sheet's

A SHEET is a thick object: a perimeter line, a specular hairline on its top edge, a chamfer shade
under it, and a **28 px ramp inside each** so the thickness reads. That is what `GlassRim::Bevelled`
draws and it is tuned on the panel.

Give the same treatment to the **tab track** and the arithmetic runs out. The bar is 76 px tall, so
its half-height is 38 against a 28 px bevel: the un-ramped interior is `|dy| < 10`, a 20 px band —
**56 of 76 px, 74 % of the object, is inside the ramp.** Measured on the corrected render (column
x=1150, uniform hero behind): a lit chamfer y=42..47, the material untouched y=48..100, a
multiplicative shade ramping 0.97x → 0.62x over y=101..109. The rim had become most of the bar.

The lens has the same problem twice over: `GLASS_LENS` is 38 px, which on this object is **half its
height**. It is invisible over a flat hero — a lens on a uniform field is the identity — so no
capture of the dev set's own Home screen can show it.

So the rim is a **per-draw** parameter (`gfx::GlassRim`), and the track takes `Line`: bevel collapsed
onto the perimeter, lens off, and the shader's own specular off with them. Its rim is then drawn
**over** the material — `theme::GLASS_RIM` .14 round the perimeter, `GLASS_RIM_LIGHT` .28 along the
top — which is where a rim belongs and is the only way a stated weight means anything. The shader's
hairline is part of the BACKDROP, so the darkening lands on top of it: measured pre-scrim it clips
to `(255, 255, 255)` 1.5 px in from both caps, where the design asks for white .14.

Two consequences worth carrying:

- **The lit and shaded halves are not symmetric.** `GLASS_LIGHT` is `(-0.35, -0.94, 0.45)`; the lit
  side is *additive* at `GLASS_EDGE`'s .14 and the shaded side is a *multiply* at .45 — 3.2:1 in
  favour of dark. On a sheet that reads as thickness. On a bar it reads as a shadow cast on it.
- **A cap gets neither, and that is geometry, not an omission.** The chamfer is
  `dot(normal, light.xy)`: a vertical normal scores 0.94, a cap's horizontal normal 0.35, and each
  cap *contains the zero crossing* (20.4° past the apex), so it sweeps `+0.94 → 0 → -0.94` across a
  119 px arc instead of holding one value for 468 px. The ends are treated — just split, three times
  weaker, and cancelling. A `Line` rim removes the question.

---

## 6. What the material can actually show

The source is at **quarter resolution** before the blur taps and is then brought back up one level.
Both snapshot paths land there — the direct path renders the page at 1/4, the capture path halves
twice (`BLUR_REDUCTIONS`, pinned to the direct path's divisor by a compile-time assertion) — and the
up pass is what stops the panel's own 2x magnification reading as enlarged pixels rather than as
blur. A cached popover on a half-res source with that pass gated off is what "the static blur on the
popup menu is too pixelated" was, 2026-08-20 to 2026-08-21.

- **Structure at the scale of a letterform partly survives, and finer than that does not.** *(Tap
  ladder: 0.35 and 0.75 texels at quarter res, i.e. 1.4 and 3.0 authored px, dual-filtered — much
  narrower than the 1.5/3.5 this section was first written against. Measured on `hbars`: a 24px
  period keeps 9% of the page's modulation, 48px 17%, 128px 65%.)* What reaches the glass is still
  mostly **regions of colour**.
- **So the lens bends colour, not detail.** A refraction that "reveals the shape of what is behind"
  is not available; a refraction that slides warm and cool areas past the rim is. This is why
  `GLASS_LENS` is 38 px and not the 14 it started at — a small displacement of a colour field moves
  nothing an eye can find.
- **Glass over a flat ground shows nothing.** On the Library and Search screens the tab bar sits
  over `SURFACE_APP`; blurring a uniform fill costs the full snapshot and produces the flat material
  it replaced. Glass needs something behind it to be worth its price.
- **The frost is 72 % opaque** (`PANEL_FROST_*`). More transparent shows more backdrop and costs
  text contrast; the couch legibility floor is the binding constraint, not the material.
- **The tab track's weight is SOLVED, not chosen** (`widgets::track_alpha_for`). A panel carries a
  page of copy at `TEXT_PRIMARY` and can hold one number; a 76px band whose idle labels are the
  dimmest ink in the app cannot, because the ground it sits on is a different picture every eight
  seconds. So the ink stands still and the material moves: five pixels are read back from under the
  bar every thirtieth drawn frame, and the scrim is set to the lightest weight at which those labels
  still clear 3:1 — floored at the design's `--glass-track-top` and capped at the flat capsule's own
  `.72`. Measured on the set: a dark hero leaves it at the floor, a bright cyan one takes it to .562.
  The readback is why the rate is low — it stalls a tiler — and the frame rate is unchanged at it.
  **Do not read the ground from `UltraBlurColors`.** That is Plex's derived palette for an ambient
  wash, not the top of the picture: for the hero that measures (0.00, 0.68, 0.91) on the panel it
  reports (0.30, 0.23, 0.18), and the bar sits at its floor while the labels drown.

---

## 7. Where it can go today

**Yes:** any popover or sheet over a UI page. Sort, Filter, Sources, item menu, alt sources and
Account use cached glass through `Popover::panel`. A moving host opts in explicitly through
`Popover::with_glass(Glass::DYNAMIC_BACKDROP)`. Account intentionally does not: its page state is
frozen and its completed host frame is copied once into `gfx::FrameCache`, then drawn as one quad
while the scrim and menu remain live. Cached remains the default, so making the policy reusable
does not silently turn every menu into a recurring capture workload.

**The tab bar is all-or-nothing across Home, Library and Search.** It is *one control* — `nav`'s
`chrome_alpha` exists so it holds still while the pages swap under it — so a material that changes
when you cross between those three breaks the one illusion that code was written to preserve. Either
all three or none; "glass on Home only" is not an option, however tempting the frame budget makes it.
It ships, and `/tmp/plxnative-flattabs` is the way back to the flat capsule for a comparison.
Its density is not a constant — see §6.

**No:** anything on the player route (§1), and a second far-away glass cluster whose union would
grow the snapshot toward the whole frame (§2). Neighbouring elements may share one cluster.

---

## 8. Quick reference for a design pass

- One glass CLUSTER per screen state — neighbours can share a chain; opposite corners grow its region.
- Never over video.
- Whole surfaces, not scattered ornaments — area alone does not predict the five-pass cost.
- In steady state, adjacent glass samples the page rather than refracting adjacent glass.
- Keep 68 px clear of the screen edge, 60 px minimum short side.
- Put it over something with colour in it, or it is an expensive way to draw the flat material.
- Over still chrome it is free; moving glass targets every changed successful present.
- Modal dim is a page-drawn scrim; source RGB dim is not a supported material axis.
- The tab bar is one object across three screens — it cannot be glass on some of them.

Implementation, the cost model's derivation and the two rejected optimisations are in
`gfx.rs`'s `blur_snapshot` doc.
