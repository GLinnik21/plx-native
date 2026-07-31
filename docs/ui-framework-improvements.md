# retui — framework improvements

> **Field names in this document predate 2026-08-01 and the old name was REUSED.** Where it says
> `FPS=`, today's heartbeat says **`loop=`** (loop iterations); where it says `pres=`, today's says
> **`fps=`** (frames actually presented). The manifest gates moved too: `floor`→`loop_floor`,
> `present_floor`→`fps_floor`, `present_ceiling`→`fps_ceiling`, and `fps_stats`→`rate_stats`. The
> text below is left as written, with the line numbers of its day, because it is a dated record of
> an investigation rather than live guidance — see `CLAUDE.md` for the current names.

Audit of `rust-modules/src/ui/` (10 dimensions, adversarially verified: 53 findings confirmed of 106
raised; 3 competing layering designs scored by a 3-lens judge panel; a completeness critic over the
result). This is the corrected merge — the critic's fixes are folded in, not appended.

Headline: **layering is implicit draw order, and it should be a declaration.** That is Part 1.
Everything else is Part 2.

---

## Verdict

**What this framework gets right, and what any change must preserve.** `Painter` is a small `Copy`
value folding a translate and an alpha into every primitive with zero allocation and zero
indirection (`ui/mod.rs:141-145`, `:164-173`). `ScrollColumn` is generic over `impl Column`, so the
detail flow monomorphises with no vtable (`mod.rs:298-321`). Culling is by index, not scissor
(`on_axis`, `mod.rs:284-287`). The expensive composites are already folded: `tex_carded` issues
texture + 1px rim + pop-scaled drop shadow in **one** pass (`mod.rs:229-236` → `gfx.rs:584-613`), and
`draw_rect` has an AA-free fast path (`gfx.rs:317`). Painter discipline is near-total — exactly one
`gfx::draw_*` call escapes `mod.rs` in the whole UI (`app.rs:2023`). The token system and the
cap-band text contract are real and load-bearing.

**The three structural gaps.**

1. **Z is statement order**, spread across `app.rs:1971-2069` and eight screen modules — and it has
   already drifted into opposite conventions for the same two widgets (`home.rs:743-748` vs
   `library.rs:836-889`).
2. **Hit-testing is a second, hand-maintained ordering that disagrees with the draw.** With the Info
   card open, `app.rs:1981` passes `transport:false` so the transport is *not drawn*, while
   `app.rs:1363-1364` hit-tests it from compile-time constants anyway. Only `Overlay::Menu` is modal
   for clicks (`app.rs:1365`) though the key path is modal for all three (`:948`, `:967`, `:1014`).
   A click on the Info card's "Go to Show" starts a blind scrub-seek. *(Verified by hand.)*
3. **The frame has two hand-copied tails** (`app.rs:1993`, `:2030` are the only `SDL_GL_SwapWindow`
   sites) and they diverged: `/tmp/plxnative-framedrop` timings are collected at `app.rs:1967/1969`
   and thrown away by the `continue` at `:2006`. The documented judder tool is **dead on the player
   route**.

---

# Part 1 — Layering: a z-keyed frame command list

## 1.1 Why not just name the existing order

Draw order *is* z in an alpha-blended renderer with no depth buffer. The cheap answer — add a
`Layer` field, assert it never decreases — only *labels* the band a statement is already in. It
cannot hoist. And a strict-ascending law is incompatible with the tree we have: `ScrollColumn::draw`
(`mod.rs:344-358`) dispatches children in document order, and detail's section order is hero(0),
tabs(1), episodes(2), **cast(4), related(3)**, about(5) (`detail.rs:154-180`). The checker would fire
on its own prescribed migration.

So: **defer the exceptions, composite by band.** (Not "record everything and sort" — see §1.4.)

The standard objection — "commands capture `Art<'a>` and `*const c_char` from `CString`s built in
loop bodies, so you need a frame string arena" — is **false here**, and that is what makes this
tractable:

- `Painter::text` → `text::draw_text` (`text.rs:395`) calls `text_tex` at `:405`, which renders,
  ink-scans, uploads and caches a **GL texture id** before any GL draw call. Everything after `:405`
  touches only `tex/w/h`.
- `Art<'a>` is resolved to a `u32` inside `widgets::card` (`widgets.rs:41,66`) before any `Painter` call.
- Every other primitive takes `[f32;4]`/`u32` by value into a synchronously-copying `glUniform*fv`.

**No `Painter` primitive retains a borrow past its own body.** Split `text::draw_text` into
`resolve` + `emit` and there is nothing to keep alive. As a bonus this shrinks `ui/CLAUDE.md:62-64`'s
dangling-`c_char` gotcha from *"keep the `CString` alive for the whole draw frame"* to *"until the
call returns."*

## 1.2 `ui/layer.rs` (new)

```rust
//! Explicit z for the retui frame. Draw order is no longer the z model: every op carries a
//! (band, rank) key, the frame is composited in key order, and within one key source order still
//! decides (insertion order within a bucket).
//!
//! LAYERS REORDER; THEY DO NOT MOVE PIXELS. `card_row::title_lift` (card_row.rs:168-175),
//! `ROW_PITCH`'s +144 (consts.rs:11) and `SECTION_GAP` are layout CLEARANCES — a heading at
//! `Chrome` still reads badly inside a popped poster's 30px penumbra.

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Layer {
    /// The scroll flow: backdrops, hero, posters, list rows, section flow, video-plane subtitles.
    /// Everything is here today, and an unmigrated screen stays here forever.
    Content = 0,
    /// Pinned furniture above the scroll flow: top bands, tab tracks, toolbar chips, the A-Z rail,
    /// detail's compact title.
    Chrome = 1,
    /// Non-modal dismissible overlays: the player transport HUD, a future toast.
    Overlay = 2,
    /// A panel that owns input, and its scrim.
    Modal = 3,
    /// Diagnostics only: the FPS number, `anim::draw_overlay`. Never product UI.
    System = 4,
}
```

> There is deliberately **no `Lifted` band.** "Float above my later siblings" is `rank`, not a band —
> see below. A separate band would give two ways to say one thing, and would only work for lifts
> originating in `Content`.

### z is inherited, not per-op — and `lift` must be relative

The key is **`(band, rank)`**, and both are carried on the `Painter`, so they flow down the tree
through the existing `Copy` cascade exactly like `alpha` and `translate`. **Almost no code mentions z
at all**: a screen draws with the painter it was handed, and its whole subtree lands in whatever band
its parent established.

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Z { band: Layer, rank: u8 }   // 2 bytes
```

There are exactly **two verbs**, and the distinction between them is the load-bearing part:

```rust
/// Enter a band. Absolute: sets the band and RESETS rank. This is a stacking context —
/// only the composition root and the shared modal choreography call it (~6 sites).
#[inline] pub fn layer(self, l: Layer) -> Self { Self { z: Z { band: l, rank: 0 }, ..self } }

/// Float this subtree above its own LATER SIBLINGS, staying inside the current band.
/// RELATIVE, so it composes: a lifted row inside a Modal panel is still Modal.
#[inline] pub fn lift(self) -> Self { Self { z: Z { rank: 1, ..self.z }, ..self } }
```

**Why `lift` cannot be absolute.** An earlier draft made `.layer(Layer::Lifted)` the escape hatch and
deleted the rank entirely, on the reasoning that `Popover`'s scrim-under-panel already works via
source order in one function (`popover.rs:48` then `:50`) and a stable sort preserves that. That
reasoning covers the scrim and nothing else. It breaks the moment a *reusable* component needs to
float inside a band above `Content`: a focused row inside a `Modal` panel calling
`.layer(Layer::Lifted)` would drop from band 4 to band 1 and paint **behind its own scrim**. A
component's z would then depend on where it was mounted — which is precisely the implicit-ordering
bug being removed, relocated into the fix.

With `lift` relative, one idiom works at every depth in every band:

- home's focused grid card — `p.lift()` inside `Content` (§1.6)
- the chapters strip's popped focused card — `p.lift()` inside `Modal`, which **fixes a latent bug
  that exists today**: `chapters_panel.rs:132-152` draws every card in one loop with the focused one
  scaled by `SCALE`, so its pop is currently clipped by the next chapter's card. Same call as the
  home grid, no focused-last pass, no second loop.

**Rank is not a number the caller picks.** It is one boolean bump — `.lift()`, nothing else. Two
ranks per band is enough for every case in this tree, and if a third level is ever genuinely needed
that is a signal to add a *named band* in this file, which is a reviewed change. No `z + 1`, no
`i32`, no arithmetic at call sites.

**Does depth-2 actually scale?** Walk the plausible futures rather than conceding in the abstract:

| case | works at depth 2? |
|---|---|
| Confirm dialog over the account menu | **yes** — the confirm draws scrim-then-panel through `p.lift()` while in `Modal`, landing in `(Modal,1)` in source order, above the menu's `(Modal,0)` |
| Toast over everything | yes — one new named band, one reviewed line |
| Drag/lift a card | yes — `rank` inside `Content` |
| A *third* stacked modal, or a `lift()` inside an already-lifted subtree | **no** — the inner lift saturates |

The first real break is depth 3, and note the failure mode: it **degrades to source order — i.e. to
exactly today's behaviour, locally.** Not corruption, not inversion. You lose an escape hatch you do
not have anywhere right now.

An earlier draft conceded this objection and proposed widening the key to a 4-level path packed in a
`u32` (CSS stacking-context semantics). **That was the wrong trade** and is rejected: arbitrary keys
kill bucket-append and drag back a real sort; per-component path values are `z + 1` with better
typography; and it optimises for unbounded nesting that this platform contradicts — a TV app's
stacking vocabulary is closed and enumerable (four modals, one HUD, one diagnostic layer).

Tree *position* contributes only the tiebreak: within one `(band, rank)`, source order decides, via
the stable sort. Depth and child index never affect the band — deriving z from tree position is what
makes a nested component's layering depend on its mount point.

## 1.3 `Painter` — one field, two methods, no signature churn

```rust
// ui/mod.rs:141-145
#[derive(Clone, Copy)]
pub struct Painter { dx: f32, dy: f32, a: f32, z: Z }
// `layer` / `lift` as defined in §1.2. Both KEEP the parent's translate and alpha — unlike
// `Painter::root()`, today's only "float above" idiom (popover.rs:48/50), which discards both.
```

`alpha`, `translate`, `c` are **unchanged**, so the alpha cascade, `ambient`'s deliberate opt-out
(`mod.rs:237`) and `icons.rs:122-123`'s `p.dx` snap trick survive verbatim. Only each primitive's
tail changes — from calling `gfx::*` to pushing a command.

## 1.4 `ui/frame.rs` (new) — deferred buckets, **not** a sorted recorder

> **Revised after a second-opinion review.** An earlier draft recorded *every* op for the whole frame
> and stable-counting-sorted at flush. That was over-built. With only 10 possible keys, the key is a
> **bucket index at record time**, not a sort key at flush time — which is what Dear ImGui
> (`ImDrawListSplitter`, how its Tables draw) and egui (`LayerId → PaintList`) both do. Nobody sorts.

The decisive simplification: **`(Content, 0)` is not a bucket — it is the immediate stream.** It
executes exactly as today, straight through to `gfx::*`. Only opted-in content (`.layer()` /
`.lift()`) is deferred:

```rust
// Buckets, replayed after the immediate stream, in this fixed order:
//   (Content,1) (Chrome,0) (Chrome,1) (Overlay,0) (Overlay,1) (Modal,0) (Modal,1) (System,0) (System,1)
pub(crate) fn flush() {
    for b in buckets_in_order() {
        crate::gfx::clip_clear();   // a clip pair may not span buckets
        b.replay();                 // insertion order — the "stable tiebreak", for free
        b.clear();
    }
    crate::gfx::clip_clear();
}
```

Each primitive's tail becomes one predictable branch:

```rust
pub fn rect(self, r: Rect, rad: f32, top: [f32; 4], bot: [f32; 4]) {
    let (t, b) = (self.c(top), self.c(bot));
    match self.z.bucket() {          // None for (Content, 0) — ~95% of every frame
        None => crate::gfx::draw_rect(r.x + self.dx, r.y + self.dy, r.w, r.h, 0.0, rad,
                                      t.as_ptr(), b.as_ptr(), 0.0),
        Some(k) => frame::push(k, Op::Rect, &[r.x + self.dx, r.y + self.dy, r.w, r.h, rad], &t, &b),
    }
}
```

```rust
const CAP:    usize = 64;    // PER BUCKET, not per frame
const STRIDE: usize = 24;    // f32 slots/command; widest op needs 21

#[derive(Clone, Copy)]
struct Hdr { op: Op, tex: u32 }   // tex is c_uint-wide: see below
```

**What this buys over sort-everything:**

- **Inert by construction, not by proof.** Unmigrated code never enters the recorder at all, so
  "the enabling commit is pixel-identical" stops being a theorem needing a freeze trigger +
  `pixdiff.py` + four extra captures, and becomes a tautology. The harness leaves the critical path.
- **`profile::phase` stays honest.** Under a global sort, a lifted card recorded inside the "grid"
  phase bracket replays *outside* its own `glFinish` brackets — the fill-attribution tool would
  silently lie about exactly the content that moved. With an immediate stream, attribution is
  preserved, and each bucket's replay can carry its own phase (`lifted`, `chrome`, `modal`) — which
  is *more* attribution than exists today.
- **The texture-lifetime invariant shrinks** to only deferred ops. The subtitle in-place re-upload
  (`player_hud.rs:116/127`) draws in `Content` and needs no carve-out. (Commit 6's deferred glyph
  deletion still stands: an immediate string's cache miss can evict a *deferred* string's texture.)
- **The `frame_clear` hoisting stops being load-bearing**, and memory drops from a 768-slot frame
  buffer to ~64 slots per bucket.

**The honest cost:** each primitive now has two code paths, and a bug in the record path only shows
on lifted/chrome/modal content. One-path-always is a real simplicity argument — but it buys that
one path by imposing whole-frame invariants on *everything*, forever. Take the branch.

> **Correction the audit caught:** an earlier draft packed `tex: u16`. Every GL texture id in this
> codebase is `c_uint` (`gfx.rs:556`, `posters.rs`, `text.rs:93`), and this app creates/deletes
> textures continuously (glyph LRU `text.rs:274`, poster eviction `posters.rs:211`, subtitle
> re-upload `player_hud.rs:116/127`, `capture.rs`). Nothing guarantees ids stay under 65536, and
> truncation renders the *wrong texture*. Use `u32`.

`Op` must be **enumerated explicitly** in this file — including `Op::Clip`, `Op::ClipClear` and
`Op::Mark` (the `profile::phase` bracket). That list is the one thing a reviewer of the recorder
commit needs.

**Why the enabling commit cannot change a pixel.** With nothing opting in, every op takes the
`None` arm and calls `gfx::*` directly, at the same point in the same order as today. Not "the sort
happens to be the identity" — *the recorder is not on the path at all.* Every `Op` maps 1:1 onto an
existing `gfx` function: `Op::TexCarded` → `gfx::draw_tex_carded` keeps the folded pass, `Op::Rect` →
`gfx::draw_rect` keeps its `aa = 0` fast path (the fast path lives *inside* `draw_rect`).

**Two semantics to write down in `layer.rs`'s module doc:**

- **Bucket overflow does not degrade to "wrong z."** An op executed immediately on overflow draws
  *before the buckets flush* — i.e. **underneath everything**, not merely mis-ordered. An overflowing
  modal's tail becomes invisible behind the screen below it. Per-bucket `CAP` makes this local, but
  it must be counted into the `FRAMEDROP` line, not merely logged.
- **A `lift()` escapes an enclosing scissor.** `flush` clears clip at bucket boundaries, so a lift
  inside a `Painter::clip` region (a future lifted row inside `TableView`'s hard clip) replays
  unclipped. Usually *desirable* — a pop escaping its panel is the point — but it is a real change to
  `clip`'s contract (`mod.rs:255-261`) and belongs next to "layers reorder; they do not move pixels."

**Cost.** ~64 slots x 9 buckets ≈ 14 KB static BSS. Zero heap, zero `dyn`, zero vtables, **no sort**.
Draw calls, fill, and the folded composites are **unchanged** — one op still means one `glDrawArrays`.
~95% of every frame never touches the recorder.

## 1.5 The composition root

```rust
/// THE composition. Every band is named here; no other file decides what is on top.
/// ACCEPTANCE TEST: shuffle the arms of this match in a scratch build — the image must not change.
/// (The clears are hoisted OUT of the screens: they are raw gfx calls and would otherwise execute
/// at record time and wipe the recorded frame.)
fn compose_frame(route: Route, hud: HudState, now: u32, fps_shown: i32) {
    let player = matches!(route, Route::Player { .. });
    ui::frame::begin();
    compose_head(player);                                   // clear + clear_opaque_region

    match route {
        Route::Player { overlay } => {
            player_hud::draw_subtitle_bitmap();                              // Content
            player_hud::draw_subtitles(hud.up || overlay == Overlay::Menu);  // Content
            if hud.up || overlay != Overlay::None {
                player_hud::draw_hud(..., !matches!(overlay, Overlay::Info | Overlay::Chapters));
            }                                                                // Overlay
        }
        Route::Login => login::draw(),
        Route::Profiles => profiles::draw(),
        Route::Detail => detail::draw(),
        Route::Library => library::draw(),
        _ => home::home_draw(),
    }
    match modal_of(route) {                                                  // Modal
        Modal::None => {}
        Modal::Account => account_menu::draw(),
        Modal::Menu => track_menu::draw(),
        Modal::Info => info_panel::draw(),
        Modal::Chapters => chapters_panel::draw(),
    }
    system_chrome(player, fps_shown);                                        // System
    ui::frame::flush();
}

/// The ONE place "which panel owns the frame" is decided. Read by the draw composition AND by the
/// pointer arm, so they cannot drift.
fn modal_of(r: Route) -> Modal { /* ... */ }
```

## 1.6 Worked example — `home.rs` `Grid::draw`

**Before** (`home.rs:462-518`): pass 1 with a `continue` sentinel at `:480-482`, then pass 2 at
`:496-517` re-deriving `x`, `s`, `rect` and `movie_at`, adding a `.min(MAX_ITEMS - 1)` clamp pass 1
lacks, and `return`ing out of `Grid::draw` entirely on `if r >= nh`.

**After** — one loop, the lift declared in place:

```rust
for c in 0..crate::pms::hub_len(r) {
    let m = movie_at(r as c_int, c as c_int);
    let x = MARGIN_X + c as f32 * (CARD_W + GAP) - self.eff_scroll(r, env.sp);
    let focused = r == env.fr as usize && c == env.fc as usize && env.sp > 0.5;
    // pass 2 deliberately had NO horizontal cull and drew the Art::Poster(None)
    // placeholder; preserve both for the focused cell.
    if !focused {
        let Some(_) = m else { continue };
        if !on_axis(x, CARD_W, SCR_W, GLOW_PAD) { continue; }
    }
    let s = self.shelves[r].scale(c.min(MAX_ITEMS - 1))
          * if focused { crate::ui::press::scale() } else { 1.0 };
    let rect = Rect::new(x, row_y + CARD_DY, CARD_W, CARD_H).scaled(s);

    if focused {
        // lift(): composites over EVERY later shelf, staying in this painter's band.
        // No second pass, no sentinel, no early return.
        card_row::draw_focused(p.lift(), Art::Poster(m), rect, s, ...);
    } else {
        card_row::draw_tile(p, Art::Poster(m), rect, s, &RowStyle::HOME, m.and_then(PmsMovie::resume_frac));
    }
}
```

The same deletion repeats at `library.rs:816/872`, `detail.rs:981/993`, `profiles.rs:161/168`.

## 1.7 Hit-testing — `ui/hit.rs`, and it ships **with** the mechanism, not after

> **Revised.** An earlier draft deferred this as over-scoped, on the reasoning that it "explicitly
> does not cover the two grids, which is where most clicks land." **That premise is false.** Both
> grids draw each visible tile individually with its `rect` already in scope (`home.rs:491-493`,
> `library.rs:831-833`) — the grids are the *easiest* case, one line at the draw site. Deferring the
> only capability that addresses the bug *class* while shipping the cosmetic half was backwards.

The bug that motivated this whole effort is a **draw/hit divergence**. Layering alone does not fix
it. This does, and it is independent of the bucket recorder — it can land first:

```rust
// ~40-100 entries/frame, one array write each. No heap, no sort.
#[derive(Clone, Copy, PartialEq)]
pub struct HitId(pub u32);      // screen-packed: home = (row << 8) | col, player = a button enum, …

impl Painter {
    /// Register `r` (in this painter's space, at this painter's z) as hittable.
    pub fn hit(self, id: HitId, r: Rect) {
        hit::push(id, Rect::new(r.x + self.dx, r.y + self.dy, r.w, r.h), self.z);
    }
}

/// Topmost wins: max (bucket, insertion index) among containing rects. Linear scan, n is small.
pub fn top_at(mx: f32, my: f32) -> Option<HitId>;
```

**Why this is correct by construction:** `p.hit()` is called *from the draw*, with the *draw's*
painter and the *draw's* rect. Visibility and hittability become the same fact. The transport that
isn't drawn (`app.rs:1981` passing `transport:false`) never registers, so it cannot be clicked. The
Info card's buttons, drawn in `Modal`, outrank the scrub band automatically.

It also kills the geometry duplication directly: `home.rs:613`'s `hit_at` currently re-derives the
card x **without** the `* env.sp` the draw applies (item A5) — under `p.hit()` there is no second
copy to diverge.

Policy stays in the handlers, applied to the returned id: the fly-away hover guard
(`home.rs:608-610`), `dpad_mode` gating, and the alpha threshold for a fading panel.

## 1.8 What this fixes, and what it doesn't

**Fixes.** Cross-file z becomes a declaration. Four two-pass loops collapse. The library
chrome-vs-focus inversion becomes a one-line band change instead of a 38-line block move. The
composition becomes one readable function with a falsifiable shuffle test.

**Does not fix.** No batching (§1.9). No fill or draw-call reduction. No layout — `title_lift`,
`ROW_PITCH`'s +144, `SECTION_GAP` are clearances, not order. Not a clip *stack* (see B5). Bucket
overflow and the scissor-escape semantics are in §1.4.

**The cheaper floor, stated honestly.** The two-pass duplication is also fixable *per site* with no
framework at all: a local `Option<LiftedTile>` POD filled in pass 1 and drawn after the loop — one
loop's math, ~5 lines per site, at `home.rs:496-517`, `library.rs:872-890`, `detail.rs` `draw_strip`,
`profiles.rs`. That plus `compose_frame`/`modal_of` (which fixes cross-*file* order) plus A1 is
roughly 90% of the payoff for ~25 lines. The mechanism here is worth building — the API is good and
it subsumes the stash — but be clear-eyed that it competes against 25 lines of plain code, not
against chaos.

## 1.9 Batching is a mirage here — but the state win is real and separable

Geometry batching is unachievable: every textured draw binds a **distinct** texture (`gfx.rs:607`) —
each poster its own slot, each string its own glyph texture (`text.rs:256`), each icon its own mask —
and GLES2 has no array textures, no bindless, no instancing. Batching the untextured SDF path means
moving ~23 uniform floats into vertex attributes: ~6 extra varyings on the hottest fragment shader,
on a fill-bound Midgard where this repo has already been burned by varying precision. Net negative.

**State batching needs none of that.** `draw_tex_impl` (`gfx.rs:596-611`) pays ~5 wasted GL calls on
*every* textured quad: `glUseProgram(IPROG)`, `glUniform2f(IL_SCREEN, 1920, 1080)` (compile-time
constants), `glActiveTexture(GL_TEXTURE0)`, `glUniform1i(IL_TEX, 0)`, and an unconditional
`glUseProgram(PROG)` restore. `draw_text` (`text.rs:414-424`) wastes ~5 of 8. `draw_ambient` and
`draw_shadow` switch and restore too. On the library grid's 18 consecutive `tex_carded` calls that is
**36 `glUseProgram` for one program**. A lazy `CUR_PROG` cache + hoisting the constant uniforms into
`init_*` removes several hundred driver calls per frame. Ships first, alone, measured.

---

# Part 2 — Everything else, ranked

Effort: **S** ≤ ~30 lines / one file; **M** one screen or a small cross-cut; **L** multi-commit.

## Tier A — do next (several are user-visible bugs)

| # | Problem | Evidence | Change | Eff |
|---|---|---|---|---|
| A1 | **Clicks fall through the Info/Chapters modals onto an undrawn transport.** Only `Overlay::Menu` is modal for clicks; `icon_hit`/`scrub_hit` are consulted with Info/Chapters open, so clicking "Go to Show" starts a scrub drag and commits a seek on button-up. *(verified by hand)* | `app.rs:1365` vs the key path at `:948/967/1014`; `:1981` passes `transport:false` | `modal_of(route)`; make the click arm modal on **any** non-`None` overlay before `icon_hit`/`scrub_hit` are computed. That is the whole bug — resist adding a `tab_hit` rect table one commit before §1.7 would delete rect tables. | S |
| A2 | **Framedrop is dead on the player route.** Timings collected then discarded by the `continue`. | `app.rs:1967/1969`, `:2006`; swaps at `:1993`/`:2030` | One frame tail. `tests/run.py:572` is `FPS=(\d+) route=(\w+)(?: overlay=(\w+))?` — `worstframe=` must stay **after** `overlay=`. | S |
| A3 | **Redundant GL state on every textured/text draw** (§1.9). | `gfx.rs:596-611`, `text.rs:414-424`, `gfx.rs:355/363`, `:415/424` | Lazy `CUR_PROG` + hoist `IL_SCREEN`/`glActiveTexture`/`IL_TEX` into `init_image`/`init_text`; delete the restore-to-`PROG` calls. Note `init_image` (`gfx.rs:518-540`) never binds `IPROG` — add an explicit `use_prog(IPROG)` first. | S |
| A4 | **The glyph cache can never hit for strings ≥96 bytes.** The key is stored truncated to 95 bytes + NUL (`cbuf::set_bytes_raw`) but compared against the **full** probe — an unsatisfiable predicate. Every frame: `TTF_RenderUTF8_Blended` + full-surface ink scan + `glTexImage2D` + an eviction. **Cyrillic wraps past 95 bytes at ~48 chars**, so this fires constantly on a Russian library. *(verified by hand)* | `text.rs:89`, `:194`, `cbuf.rs:7-18` | Add `klen: u32`; predicate becomes `hash == && klen == len && prefix ==`. Log once when a key exceeds the buffer. Same latent trap at `posters.rs:34-46` (`[u8;256]`, real keys ~135 B). | S |
| A5 | **Home's grid hit-test drops `* env.sp`.** Draw uses `- scroll_x() * env.sp` (`:486`, `:506`); `hit_at` uses `- scroll_x()` (`:613`). `Grid::vert` (`:583-596`) has the same hole. *(verified by hand)* Severity note: `app.rs:1344` only routes here when `snap_pos() >= 0.5` and `sp` converges within ~150 ms, so the error is **transient during the hero→grid snap**, bounded by `0.5 × max_scroll` — not a standing offset. | `home.rs:486/506` vs `:613`, `:590` | One `eff_scroll(&self, r, sp)` used by draw, `hit_at` and `vert`. Thread `sp` as an argument — do **not** call `snap_pos()` inside `hit_at` (it aliases the live `&mut Home`). Host-testable: `home.rs:818` already has a test module. | S |
| A6 | **New detail pages inherit the previous item's Related/Cast scroll.** `reset_view_state` zeroes three springs but not `related`/`cast`, whose `scroll_x` is frozen while unfocused so the stale value survives. | `detail.rs:303-311`; `card_row.rs:120-123`; used at `:978` | `CardRow::reset()`; call for both. Repro: OK a Related tile (`detail.rs:1164-1169` opens in place). | S |
| A7 | **`TextView` is the largest per-frame heap allocator in the UI**, against an explicit zero-alloc goal. `wrap()` SipHashes the entire string every frame; `CString::new` runs once per rendered line per frame; `measure()` allocates per call. *(verified by hand)* | `text_view.rs:112-118`, `:103-108`, `:206/213/223`; hot at `detail.rs:938/944` (per visible episode), `home.rs:326/349/358`, `info_panel.rs:222-223` | Store `Wrapped { lines: Vec<CString> }` (`text_view.rs:30-33`). The cache is already `Rc`-shared and generation-capped at `:41-43`, so a hit becomes zero-alloc. Cheaper than B6/B7 and strictly higher payoff. | S |
| A8 | **Docs actively mislead.** `CLAUDE.md:180` and `ui/CLAUDE.md:92` say there is no host test suite — **`cargo test` runs 22 tests in 0.30 s on macOS today, 3 of them UI** *(verified by hand)*. `ui/CLAUDE.md:53` advertises a deleted `ProgressBar`; `:83` says clip has one user (there are three `clip_set` calls across two fns — `card_row.rs:293/298/301`); `theme.rs:109` describes an overdraw removed in b1d999b. `docs/ui-framework.md` — the doc `mod.rs:7` points readers to — advertises `Stack{Vec<Box<dyn View>>}`, `PillButton` and `ui/player.rs`, none of which exist. | as cited | Line edits + a `make check` target (**not** an `all` dependency — host link success is a dead-code-elimination accident). | S |
| A9 | **Two screens pay a full-screen overpaint home already deleted** — `CLEAR_RGB == SURFACE_APP` bit-for-bit. ~2.07 M blended fragments of pure waste. | `login.rs:62`, `profiles.rs:140`; rationale at `home.rs:256-259`; `theme.rs:106/110` | Delete both lines. Provably pixel-identical. | S |
| A10 | **9 of 10 screen modules have no panic barrier.** `home.rs:692-695`'s `guard` is the only thing between a screen `draw` and the C caller; a panic in `detail`, `library` or any player overlay unwinds into C and kills a live playback session. | `home.rs:692-695` + `docs/ui-viewtree-plan.md:147` | Lift `guard()` into `ui/mod.rs` (`impl FnOnce`, no boxing); player overlays first. Put `gfx::clip_clear()` in the `Err` arm — it also fixes home's latent scissor leak. **This must precede the recorder** (see Part 3). | S |

## Tier B — worth doing

| # | Problem | Evidence | Change | Eff |
|---|---|---|---|---|
| B1 | **No error/offline state anywhere except login, and Home has none at all.** If `/hubs` fails, `hub_count()` → 0 → `n_hubs()` → 0 → `Grid::draw`'s loop runs zero times: a blank `#2C2C2E` screen, no copy, no retry, no focusable element — and BACK at Home root **exits the app**. This is the state a couch user hits when the PMS is asleep. | `home.rs:98`, `:465`; `metadata.rs:472` (blocking, no error surface); cf. `login.rs:68` `Phase::Error`, `library.rs:805`, `table.rs:244` | One shared `StatusView { icon, title, body, action }` in `ui/`, reused by home/detail/library — not five bespoke branches. Ranks above most of Tier B on user impact. | M |
| B2 | **Detail has no pointer path at all** — the Magic Remote cursor is visible and inert on the screen where you press Play. | `app.rs:1487` is the only Detail pointer handler; no arm at `:1341`/`:1391-1458` | `detail::click(mx,my) -> bool` first; hover later (cross-section hover scrolls the page under the cursor). Trap: `sections()` pushes cast(4) before related(3), so flow index ≠ section id. | M |
| B3 | **Popovers fade in over 200 ms and vanish in one frame**, because `app.rs` gates update *and* draw on `Route` and the route flips synchronously. | `popover.rs:23-26`, `:31-35`; `app.rs:1958-1966`, `:1983-1991` | `close()` springs to 0; add `is_visible()`; gate update/draw on it — **and move the panels' own `if !is_open() { return; }` early-outs to `is_visible()`**, or the change is a no-op. Use a stiffer `K_CLOSE` (~900-1200); reusing `K_APPEAR=300` gives a ~380 ms mushy tail. | M |
| B4 | **Related drill-down is a one-way door.** Home → detail → OK a Related poster → BACK lands on Home, and `metadata::CURRENT` is clobbered so re-opening costs a fresh blocking fetch. | `detail.rs:1164-1169` → `:1187`; BACK cascade `app.rs:1295-1299` | One-level `Option<Crumb>` + `detail::back()`, mirroring `library::back()` (`library.rs:464`). Verify `metadata::current().rk == crumb.rk` before consuming (a Wi-Fi blip must not eat the crumb); restore scroll with `jump()`. | M |
| B5 | **`Painter::clip` replaces instead of intersecting and disables instead of restoring**, with three `clip_set` calls across two users. | `mod.rs:259-265`, `gfx.rs:131-146`; `table.rs:251/366`, `card_row.rs:293/298/301` | `clip_set` intersects with a `static mut CLIP` and returns the previous; `clip_restore(prev)`. `Painter::clip(r) -> ClipSave` — plain `#[must_use]` `Copy`, **no `Drop`** (`Copy + Drop` is E0184, and a guard would restore before anything drew at both statement-form call sites). Intersect-on-set is the load-bearing half. Note `card_row.rs:296-297`'s comment ("the second clip simply replaces the first") becomes a lie under intersect — behaviour is identical only because it *is* a subset; say so. | S |
| B6 | **`focus: f32` is dead at all 34 `Painter::rect` call sites** and its shader branch is unreachable — yet `ui/CLAUDE.md:65-68` documents it as the live focus mechanism. | `mod.rs:174`, `gfx.rs:325/345/378/396`, `fs_src.frag:35/50-55` | Delete the parameter, the `LOC_FOCUS` uniform and the branch; AA test becomes `radius >= 0.5`. Compile-enforced. Don't miss `gfx.rs:499`. | S |
| B7 | **`player_hud` forks the text system.** `player_hud.rs:37-57` is a hand-rolled **character-count** word wrap (a fork of `TextView`'s pixel wrap, violating rule 4), and `draw_subtitles` hand-places at `top + 4.0` with a magic `lh = 48.0` (violating rule 3). Worse, `:92-100` draws **each line five times** (4 outline offsets + fill) at size 36 across the full panel — up to 15 text quads/frame of pure fill, on the route where A3's win lands. | `player_hud.rs:37-57`, `:87`, `:92-100` | Route through `TextView`; replace the 5× outline with a single pass (a pre-baked outline in the glyph texture, or a shadow op). Biggest un-named fill item in the tree. | M |
| B8 | **Two clamps on Home's focused column disagree**: draw clamps to `MAX_ITEMS-1`, `col()` (which OK dispatch uses) does not. Zero headroom — the max hub is exactly 24. Live sibling: detail's Cast row is uncapped, so past index 23 focus magnification silently stops. | `home.rs:666` vs `:55-57`; writes at `:570/593/612/640`; `card_row.rs:129-131`; `metadata.rs:237-241` | `CardRow::scale` → `.get(i).map(..).unwrap_or(1.0)`; bound `fc` at the five sites. Host-test a pure `col_of(fc, hub_len)`. | S |
| B9 | **CJK has no line breaking.** `wrap_uncached` splits on `split_whitespace()`, so a space-less script yields one "word" and the over-wide safety net at `:154-159` ellipsizes it to **one line** where Latin gets 3-5. *(verified by hand)* | `text_view.rs:123`, `:151-159` | A break-opportunity test on CJK codepoint ranges inside `wrap_uncached` — ~15 lines, no perf cost (the wrap is memoized). RTL/bidi and Arabic shaping are out of reach on SDL2_ttf without HarfBuzz — **name that as a known limitation** in `ui/CLAUDE.md` rather than leaving it silently absent. | S |
| B10 | **Poster texture memory has no byte budget and no failure path.** 64 slots evicted by *slot count*, with sizes spanning 130× — 250×375 posters (375 KB) up to a 1920×1080 backdrop (8.3 MB). Worst case is hundreds of MB of GL texture with no accounting; `upload_rgba` never checks `glGetError`. | `posters.rs:24`, `:179-188`, `:46`; `gfx.rs:556-570`; `detail.rs:660` vs `home.rs:293` | Shrink detail's backdrop request first (−55% upload/transcode/decode), then a byte-budgeted LRU — `px` is already tracked per slot, so it's ~20 lines. This is what keeps a 4-library server from OOMing. | S |
| B11 | **The player HUD owns none of its state** — focus/btn/tab/dismissed plus a 7-variable scrub machine live as `plex_run` locals, touched at 15+ sites, reset in three ad-hoc places. | `app.rs:475-487`, `:1981`; resets at `:792`, `:1727`, `:1829-1832` | **Phase A only:** move the four pure-UI locals into `player_hud` statics behind accessors (`info_panel.rs:24-25` is the template); add `player_hud::reset()`. Leave the scrub machine — it belongs beside the atomics in `player/shared.rs:181-184`. | M |
| B12 | **The `View` trait is vestigial.** 4 home types + 6 widget leaves implement it; `layout` is overridden **exactly once** (`home.rs:454`). detail, library, profiles, login, track_menu, info_panel, chapters_panel and player_hud are all free `pub fn draw()`. `mod.rs:1-7` and `docs/ui-framework.md` describe a retained tree ~60% of screens don't participate in. | as cited | Either finish it or delete it — but decide, in the same commit as A8. Given `compose_frame` calls eight free functions happily, deleting is the honest answer. | S |

## Tier C — only if it starts hurting

`C1` screen `draw()` takes no args so no caller can fade/offset a whole screen (the real defect
underneath is that `metadata::load_detail` blocks — `metadata.rs:468-472` — making Home→Detail a
synchronous freeze; fix that independently, async like `load_season`). `C2` motion has no token axis
(9 bare stiffness literals, three competing homes — seed `theme::motion` with the **existing values
verbatim**; invariant #9 is byte-identical motion). `C3` controls hard-cut between idle and accent.
`C4` `elide` clones a `String` on every cache hit. `C5` `tabs_layout` rebuilds a `Vec<CString>` up to
twice per frame. `C6` poster resolve does 2 allocs + a mutex + a 64-slot byte-walk per tile per
frame. `C7` `poster_pump(3)` budgets by slot count though slots vary 130× in cost. `C8` `anim::probe`
covers 6 of 21 springs (and `plxnative-anim.log` is missing from `DIAG`). `C9` `ScrollColumn` computes
the flow twice — but note `Column::height` for detail reaches `ep_meta_h()`, which fingerprints 4
fields per call and on a miss re-measures every episode, so "make draw call `child_top`" turns 6
`height()` calls into 21 on exactly the frame no FPS scene covers. Measure first.

---

# Part 3 — Sequence

**Ship the verified bug fixes first. Do not gate them behind framework infrastructure.** A1, A3, A4,
A5, A7 are each a confirmed bug or measured waste, each independent, and none requires a design
decision. They are commits 1-5 below.

**The harness is a prerequisite for two commits, not eight.** Because `(Content,0)` stays immediate
(§1.4), the mechanism commit is inert *by construction* rather than by pixel-proof. Only two later
commits genuinely move or risk pixels: the Home one-loop rewrite and the Library chrome flip. Build
the harness before *those* — a `/tmp/plxnative-freeze` trigger pinning every spring at its target
plus a ~30-line `tools/pixdiff.py`, since `tools/` today has no diff script, no golden images and no
way to freeze a deterministic frame. Add the four missing FPS scenes at the same time:
`route=player overlay=none` (the bare HUD + B7's 5x subtitle path — the route A2 makes framedrop live
on), profiles, login/account, chapters. The suite covers 7 of ~11 drawable states today, and this
plan moves pixels on three uncovered ones.

**Commit 1 — lazy program cache** (A3). `gfx.rs` + `text.rs` only. Verify with
`/tmp/plxnative-profile` before/after; expect the win in `FRAMEDROP`'s `draw=` term, not necessarily
in `FPS=`.

**Commit 2 — `modal_of` + pointer modality** (A1). Restructure `app.rs:1356-1390` so any non-`None`
overlay is handled *before* `icon_hit`/`scrub_hit` are computed. Verify with a
`/tmp/plxnative-remote` case: open Info, `ck:1600,830`, assert playback position unchanged.

**Commit 3 — one frame tail + `compose_head`** (A2). Replace the `continue` at `app.rs:2006` with
`if player { … } else { … }`, and factor the clear into `compose_head(player)` **now** so commit 5
only inserts `begin`/`flush` rather than rewriting the same 80 lines twice. Extend the `rn` match with
`Route::Player{..} => "player"`; append `worstframe=` after `overlay=`.

**Commit 4 — glyph-cache key fix** (A4). Must land **before** commit 5's deferred-deletion work: that
work assumes eviction is rare, and today every ≥96-byte string evicts a slot *every frame*. A4 is an
`S` and makes commit 5's heuristic tunable against a realistic workload.

**Commit 5 — panic barriers** (A10). ~15 lines, and it must precede the recorder: commit 6 rewrites
the tail of every `Painter` primitive and adds an indexed replay loop — the highest panic-risk change
in the plan — and it should not land with 9 of 10 screens unguarded.

**Commit 6 — `text::resolve`/`emit` split + deferred glyph deletion.** Still fully immediate. Split
`draw_text` (`text.rs:395-426`) at line 405. Push the eviction victim onto a pending list drained
after the swap; stamp entries with a frame counter and prefer an older-frame victim (`posters.rs:180`
is the model). Note `player_hud.rs:118/128` deletes **and re-uploads in place**, which a pending list
cannot save — the rule is: *no texture may be deleted or re-uploaded between `frame::begin` and
`frame::flush`*.

**Commit 7 — the mechanism, inert.** `ui/layer.rs` + `ui/frame.rs` (deferred buckets, §1.4) **plus
`ui/hit.rs`** (§1.7 — `Painter::hit` + `top_at`, which is independent of the buckets and can even
precede them); `Painter.z`; the primitives
record; `gfx::draw_number` ported to `Painter::rect` (it is the one `gfx` bypass and the `System`
band depends on it); `frame_clear` hoisted from the five screens into `compose_head`;
`profile::phase` pushes `Op::Mark` and `flush` calls `glFinish` at marks when armed (**not** a
follow-up — `phase` is the only fill-attribution tool and would otherwise report ~0 for everything);
`frame::stats()` into `FRAMEDROP`. **Acceptance:** pixel-diff zero on home / detail / library /
player+menu **plus** a cold-poster boot (skeleton `rect_sheened`), a fading-in About panel
(`text_fade`, `ambient`) and a chip-focused Home (`focus_shadow`) — the four obvious captures miss
four ops.

**Commit 8 — name the bands.** `Popover::open_in(p, Layer::Modal, scrim, rise)` replacing
`popover.rs:44-51`'s two bare `Painter::root()` calls (keep the separate scrim painter — routing it
through the content painter gives `scrim_a * a²`); `draw_hud` → `Overlay`; `system_chrome` →
`System`. **Acceptance: shuffle `compose_frame`'s match arms; the image must not change.**

**Commit 9 — Home** (§1.6). First real deletion. Pixel-diff zero.

**Commit 10 — Library, the visible fix.** Grid → `Content`, focused tile → `lift()`,
`library.rs:837-870` chrome → `Chrome`, rail → `Chrome`. Chrome then wins by declaration, matching
Home. **This moves pixels**: at rest the focused row sits at `GRID_TOP = 214`, and the 1.09 pop puts
the poster top at ~197 with a 31 px shadow pad reaching 166, under the 170→214 gradient — shorten the
gradient to end at ~197. The interleave is described as deliberate at
`docs/architecture-review-2026-07-26.md:651`; get the product call made first and keep it separable
from the mechanism.

**Commits 11+ —** `detail::draw_strip` + `profiles`; player surfaces (annotation only — do **not**
touch `player_hud`'s `SCR_H` geometry, a documented hit-test contract).

Tier A items A5-A9 are independent and can land in any gap.

---

# Part 4 — Deliberately not doing

- **Multi-pass traversal / demand-driven re-entry.** The guard sits at the primitive, but the cost is
  upstream: `widgets::resolve_tex` hashes a 352-byte key per tile per call, `card_row::under_label`
  heap-allocates twice per label per call. A skipped pass still pays all of it. Every screen also
  opens with a raw `gfx::frame_clear`, so pass 2 wipes pass 1. And `Painter::text` returning 0.0 on a
  skipped pass breaks the cursor-advance idiom (`library.rs:770-776`). Failure mode: silent 3× CPU.
- **A portal/callback registry.** Same re-entrancy cost, plus a hand-packed payload and a
  hand-maintained ordering — the exact failure being complained about, relocated.
- **A monotonicity assert as the whole answer.** It labels the band a statement is already in, cannot
  hoist, and forbids the legitimate `Content → Chrome → Content` interleave `ScrollColumn` produces.
- **`z: i32` / caller-chosen depths.** Numbers invite `z + 1`. Five named bands plus a boolean
  `lift()`, `Ord`-derived. A third level inside one band means a new *named* band, reviewed in
  `layer.rs` — not arithmetic at a call site.
- **Geometry batching, a texture atlas, a GL depth buffer.** §1.9; and the whole UI is alpha-blended
  (`gfx.rs:304-305`), so blended fragments need back-to-front, which a depth test does not provide.
- **A uniform `Screen` trait to collapse `app.rs`'s dispatch chains.** Refuted at
  `docs/architecture-review-2026-07-26.md:344-347`: the arms carry key-repeat arming, HUD timers,
  `hud_focus` and the press animation, which no return value expresses.
- **Converting the 13 `&'static mut` screen accessors to a `with()` scope.** `ScrollColumn::draw`
  takes `&self` and detail's `draw_child` re-enters `view()`. Do land the two-line fix at
  `detail.rs:1020/1053` (bind `view()` once).
- **`theme::space` sweeps, `Rect` helper expansion, a `Row`/`Flow` primitive, retiring
  `CARD_FOCUS_SCALE`, `Env` removal from leaf widgets.** Each verified as a non-defect, a
  pixel-moving design change dressed as a refactor, or a primitive with one user.
