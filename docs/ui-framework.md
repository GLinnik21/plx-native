# retui — the retained UI framework

`rust-modules/src/ui/` is a small **retained-mode, UIKit-style view tree** in Rust.
It backs the home screen today and is designed to back the detail page, settings,
and player HUD next. Design synthesized from a 3-way design workflow (ergonomic /
minimal-risk / extensible priorities → one spec).

## The idea

A `View` is `update(dt) → layout → draw` each frame. It **never touches GL** — it
draws only through the crate's existing `gfx`/`text` primitives. Three small types
carry the whole contract:

- **`Painter`** (Copy value) folds a **cascading alpha** (+ optional translate) into
  every draw op. The hero fade is `p.alpha(hero_a)` over the whole subtree instead
  of baking `heroA` into ~12 colors by hand.
- **`Spring{pos,vel}`** wraps the existing C `spring()` so motion is byte-identical;
  a view owns one per animated value ("springs live in views").
- **`Env`** is the per-frame context, bridged **once** from the C globals
  `fr`/`fc`/`snapTarget` — which stay the single source of truth (main.c writes them
  too, incl. the autoplay path). The tree reads them live and writes back via nav;
  it never caches focus.

## Ownership (chosen by data shape, not dogma)

- **Screens** (`Home`, later `Player`) own children as concrete typed fields — one
  each, static dispatch, zero indirection.
- **The grid is a collection view**: `Grid` is one `View` holding fixed arrays
  `[Shelf;5]` × `[Card;10]` — no 50 boxed widgets, **zero per-frame heap allocation**.
- **`Stack{Vec<Box<dyn View>>}`** is the only `dyn` escape hatch, for heterogeneous
  linear layouts (the future HUD transport row). Single-threaded → no `Rc/RefCell`.

## Modules

| file | contents |
|------|----------|
| `ui/mod.rs` | `Rect`, `Size`, `Spring`, `Env`, the `View` trait, the `Painter` |
| `ui/consts.rs` | layout/input/spring constants mirroring `ui_home.h` (one source of truth) |
| `ui/widgets.rs` | reusable leaves: `PillButton`, `CircleButton`, `PageDots`, `ProgressBar` + poster/text helpers |
| `ui/home.rs` | `Backdrop`, `Hero`, `Grid`/`Shelf`/`Card`, `Home`, and the `#[no_mangle]` C ABI |
| `ui/player.rs` | *(later)* the HUD, built from `ProgressBar`+`Stack`+`Label` — proves cross-screen reuse |

## Pixel-identity rule

Where `ui_home.c` used hand-tuned absolute offsets (`titleY+92/+128/+158/+200`,
`14*s`, `(s-1)/0.055`), the port uses the **same literals** — never a stack with
"about right" spacing. Every step is capture-diffed on the TV against a baseline.

## Build-order status

- **Done + verified on device:** the core (`mod.rs`), constants, widgets, and the
  full `home.rs` tree. Hero view is **pixel-identical** to the pre-framework render;
  grid view (focused-card scale pop + glow ring + centered title + scroll springs)
  verified by booting into it. C ABI unchanged; `ui_home.rs` retired.
- **Next:** memoize the hero meta/synopsis in a `Text` type (drop the last per-frame
  `format!`/`CString` for zero steady-state alloc), then add a `Screen` router +
  the player HUD as the first second screen (task #13's payoff).
