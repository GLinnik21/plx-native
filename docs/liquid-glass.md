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

**Elements that sit close together are nearly free to have side by side.** The fixed ~2.45 ms is paid
once for a shared region; a neighbour only adds its own fragments at ~2.7 ns each.

| | region | cost *(derived)* |
|---|---|---|
| one 200×60 button | 376×236 | ~2.7 ms |
| two of them, 24 px apart, sharing a grab | 600×236 | **~2.9 ms** |

**Elements far apart are the expensive case**, because their union is most of the screen — a bar at
the top plus a sheet in the middle is the whole-screen grab again. That is why the glass tab bar
drops to its flat material the moment any popover opens (`widgets.rs`, `popover::any_open`).

**Implemented, and verified at runtime.** A snapshot is taken at the union of every region the
*previous* frame asked for (`blur_region_union`, `blur_frame_end`), so a frame holding two glass
surfaces takes ONE capture. Measured on the simulator with the glass tab bar and the account panel
both live over the Home hero, 644 frames:

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

### Neighbours do not refract each other

The snapshot is taken before any glass is drawn, so A's rim displaces its sample into whatever the
*page* holds under B — never into B itself. Two adjacent capsules each bend the background
independently.

This differs from Apple's Liquid Glass, where neighbouring elements interact, and it is **not a bug
that can be fixed**: for A to see B, B would have to be in the snapshot, which means drawn before it,
which means drawn twice. Design around it rather than expecting the interaction.

---

## 3. Glass does not get cheaper by being small

This is the least intuitive limit and the one most likely to shape a layout.

Measured cost model: **≈0.49 ms fixed per render pass + ≈2.7 ns per output fragment.** The chain is
five passes, so **≈2.45 ms is paid before a single useful fragment**, whatever the element's size.

| element | region | snapshot cost (measured/derived) |
|---|---|---|
| Library Sort panel (470×630) | 630×790 | ~3.4 ms *(measured)* |
| tab bar track (~710×76) | ~870×230 | ~3.0 ms *(derived)* |
| a hypothetical 200×60 chip | 360×220 | ~2.7 ms *(derived)* |

A chip a twentieth the area of a panel costs about **four fifths** as much.

**Design consequence:** one large glass surface is close to free relative to one small one. **Many
small glass elements is the shape to avoid** — and since §2 allows only one live region anyway, the
two rules point the same way: glass belongs to whole surfaces (a bar, a sheet, a card), never to
scattered ornaments.

---

## 4. It is free at rest, and charged while things move

The snapshot is taken **once per opening** for anything over a still page, and **once per drawn
frame** for anything over a moving one. `ui::idle` gates the whole frame, so a settled screen does
not present and therefore does not blur at all.

Measured on Home, glass tab bar over the hero, grid oscillating: worst frame **21.2 ms** with glass
against **21.5 ms** without — indistinguishable. (The 3.4 ms above is an *upper bound* from the
profiler, which brackets each pass with `glFinish` and so serializes work that normally pipelines
with the rest of the draw.)

**Design consequence:** glass over static chrome is genuinely free. Glass over content that scrolls,
cross-fades or auto-flips is the case that costs, and it costs only while the motion lasts.

---

## 5. Geometry

| limit | value | why |
|---|---|---|
| **Clearance from the screen edge** | **68 px** (`BLUR_REACH`) | The rim samples up to 38 px outward (the lens) plus ~25 px of kernel spread. Past the panel edge there is nothing to sample; closer than this to the frame the rim reads as a one-colour smear on that side. *Derived from `GLASS_LENS` + the tap ladder.* |
| **Minimum short side** | **~60 px** | The bevel is 28 px from each edge, so below ~60 px an element is *all* rim and no interior — it still draws, and reads as a solid lozenge of refraction rather than a pane. It also loses the shader's interior early-out, so every fragment pays the full lens. The tab bar at 76 px has 20 px of interior and is close to the floor. *Derived from `GLASS_BEVEL`.* |
| **Maximum bevel** | **~40 px** | Past that the ramp covers enough of the panel to be read as *shading* rather than as an edge, and the object stops looking like glass and starts looking vignetted. *Judgement, recorded in `GLASS_BEVEL`'s doc.* |
| **Corner radius** | anything up to a capsule | The lens reuses the `sdBox` distance the rounding already computes, so a full capsule (`h * 0.5`, what the tab bar uses) costs nothing extra. |
| **Slide during appear** | **≤ 20 px** (`POPOVER_MAX_RISE`) | The grab is sized to swallow the appear travel so one opening costs one snapshot. A popover that rises further than this must raise the constant — a host test fails if it does not, so this cannot be broken silently. |

---

## 6. What the material can actually show

The snapshot is reduced to **quarter resolution** before the blur taps, then brought back up one
level. That is a deliberate design choice (`BLUR_REDUCTIONS`), not a limit — but it fixes what the
lens has to work with.

- **Structure finer than roughly 25–30 authored px does not survive.** Text, thin rules, a dense
  grid: all gone. What reaches the glass is **regions of colour**. *(Derived from the tap ladder:
  1.5 and 3.5 texels at quarter res, i.e. 6 and 14 authored px, dual-filtered.)*
- **So the lens bends colour, not detail.** A refraction that "reveals the shape of what is behind"
  is not available; a refraction that slides warm and cool areas past the rim is. This is why
  `GLASS_LENS` is 38 px and not the 14 it started at — a small displacement of a colour field moves
  nothing an eye can find.
- **Glass over a flat ground shows nothing.** On the Library and Search screens the tab bar sits
  over `SURFACE_APP`; blurring a uniform fill costs the full snapshot and produces the flat material
  it replaced. Glass needs something behind it to be worth its price.
- **The frost is 72 % opaque** (`PANEL_FROST_*`). More transparent shows more backdrop and costs
  text contrast; the couch legibility floor is the binding constraint, not the material.

---

## 7. Where it can go today

**Yes:** any popover or sheet over a UI page (Sort, Filter, Sources, item menu, account menu, alt
sources — all already on it via `Popover::panel`), and the shared top tab bar.

**The tab bar is all-or-nothing across Home, Library and Search.** It is *one control* — `nav`'s
`chrome_alpha` exists so it holds still while the pages swap under it — so a material that changes
when you cross between those three breaks the one illusion that code was written to preserve. Either
all three or none; "glass on Home only" is not an option, however tempting the frame budget makes it.
(It is still behind `/tmp/plxnative-glasstabs` and compiled out of a `RELEASE` build.)

**No:** anything on the player route (§1), and anything that would put a second glass surface in the
same frame (§2).

---

## 8. Quick reference for a design pass

- One glass CLUSTER per screen state — neighbours are nearly free, opposite corners are not.
- Never over video.
- Whole surfaces, not ornaments — small glass costs nearly as much as large.
- Adjacent glass does not refract adjacent glass, only the page behind it.
- Keep 68 px clear of the screen edge, 60 px minimum short side.
- Put it over something with colour in it, or it is an expensive way to draw the flat material.
- Over still chrome it is free; over moving content it costs ~3 ms a frame while the motion lasts.
- The tab bar is one object across three screens — it cannot be glass on some of them.

Implementation, the cost model's derivation and the two rejected optimisations are in
`gfx.rs`'s `blur_snapshot` doc.
