# ui/ — the shared UI system (read before touching any screen)

This is a **real product built for production quality — not a POC.** "It's a POC" / "not worth it
for a POC" is never a reason to skip a proper component, leave a bespoke `draw_*` in place, or
half-finish a primitive. If a shared piece is missing (a real text-flow view, a chip, a list),
**build it** — a reusable primitive that pays off across screens is exactly the work worth doing.

This directory is a **design system**, not a pile of per-screen draw code. Home, detail, and the
player HUD are all *compositions of the same tokens + components*. When you add or change UI,
**reach for the shared pieces and improve them in place** — do not paste a new `draw_*` function
with hard-coded colors and hand-tuned text offsets. That is exactly the drift this system exists to
kill. Full design + migration status: `docs/ui-system-migration.md`.

## The three rules

1. **Never write a raw color literal.** Every color comes from `theme.rs` as a named token
   (`theme::TEXT_PRIMARY`, `theme::CONTROL_IDLE_FILL`, `theme::scrim(a)`, …). Need a shade that
   doesn't exist yet? **Add a token to `theme.rs`** (with a doc line saying what it's for) and use
   that — don't inline `[0.9, 0.9, 0.9, 1.0]`. If your shade is within a hair of an existing token,
   use the existing one; the point is one value per role, not a value per call site.

2. **Never hand-place text with a magic y.** `y - sz*0.58` guesses are banned — they mis-center the
   moment a string has a descender (g j y p). Text is positioned by its **cap band** (layout ≠
   paint): use `ui::label::Label` (single run) or `ui::text_view::TextView` (multi-line, pixel-
   wrapped), or if you must call `Painter::text` directly, derive the y from `text::text_vcenter_y` /
   `text::text_cap_band`. See `label.rs`'s module docs for the rule.

3. **Improve a component before forking one.** If a shared widget almost does what you need, add a
   builder method / style variant to it (e.g. `Button::style(ControlStyle)`), don't copy it. A new
   bespoke widget is only justified when nothing here is close — and then it lands *here*, as a
   reusable `View`, so the next screen gets it for free.

## What lives where

| File | Owns |
|---|---|
| `theme.rs` | **all color tokens** + `scrim`/`scrim_black`/`with_a` helpers + focus-ring geometry consts. The single palette. |
| `mod.rs` | the retui core: `Painter` (cascading alpha/translate — draw through it, never call `gfx::*` directly from a screen), `Rect`/`Size`/`Spring`/`Env`, the `View` trait. |
| `label.rs` | `Label` — single-run cap-band text (the layout ≠ paint primitive). Also `HAlign`/`VAlign`. |
| `text_view.rs` | `TextView` — multi-line cap-band text: pixel word-wrap + ellipsis + `measure_h`, wrap-cached. |
| `widgets.rs` | reusable leaves: `Button` (+`ControlStyle`), `TabPill`, `TransportButton`, `CircleButton`, `ProgressBar`, `Spinner`, `PageDots`, the shared art-tile core (`card`/`draw_card` + `Art`), plus the poster-resolve helper. |
| `card_row.rs` | `CardRow` — the animated poster-shelf component shared by the home grid and detail Related (`RowStyle::HOME` = the single source of shelf motion+geometry; owns per-cell scale springs + scroll spring + `draw_tile`/`draw_focused`). Callers keep their own x/scroll/z-order loop (home's cross-row focused-last stays in `Grid`). |
| `table.rs` | `TableView`/`Section`/`Row`/`Badge` — the animated list (settings/track-menu look). |
| `icons.rs` | `Icon` enum + antialiased SVG rasterizer; color is the `tint` you pass. |
| `profile.rs` | draw profiler (diagnostic). `profile::phase("name", \|\| draw_x())` brackets a phase with `glFinish` to log its real per-frame GPU cost; on via `/tmp/poc-profile`, zero-overhead off. Use it to find fill/overdraw before guessing. FPS is also logged once/sec (grep `FPS=`). |
| `home.rs` / `detail.rs` / `player_hud.rs` / `info_panel.rs` / `track_menu.rs` / `chapters_panel.rs` | **screens** — compose the above; hold their own springs + input. Should contain almost no color literals. |

## Gotchas that bite

- **`Label`/`Button` hold a non-owning `*const c_char`.** Keep the `CString` alive for the whole
  draw frame (bind it to a `let` in the same scope) or you'll draw freed memory. (`TextView` is the
  exception — it borrows a `&str` and builds its own `CString`s internally, so it's memory-safe.)
- **The focus ring/glow is shader-baked** (`gfx.rs` `FS_SRC`): `Painter::ring` exposes only a
  `focus: f32` scalar and the ring *geometry* (`theme::CARD_RING_PAD_*`, `CARD_RING_RAD`). Its color
  cannot be tokenized — don't try, and don't re-add it as a literal.
- **`detail.rs` below-hero layout is a computed flow, not magic constants.** Every section's Y comes
  from `section_y()`, which stacks the *present* blocks' `block_h()` heights from `CONTENT_TOP` with
  one `SECTION_GAP`. To resize/space a section, change its `block_h` (content-derived — e.g. Related
  tracks `REL_H`=`CARD_H`) or `SECTION_GAP` — never reintroduce a hard-coded per-section Y. Both the
  draws and `scroll_target` read `section_y`, so they can't drift apart.
- **A few things are deliberately immediate-mode** (documented in `docs/ui-system-migration.md` §D):
  home's `Backdrop` per-element alphas, `player_hud`'s `SCR_H`-offset geometry (shared with `app.rs`
  pointer hit-tests), and the subtitle renderer. Leave them; wrapping them in a `View` breaks a
  load-bearing contract.
- **`Painter` has no clip/scissor.** Scrolling rows cull off-frame cells by index and hand-roll edge
  fade masks — follow that pattern rather than assuming a clip rect exists.

## When you're done

`make` is the only correctness signal (ARM cross-build; no host runtime). After a UI change, `make`
must stay green, and for anything that moves pixels, capture the panel on the TV
(`tools/capture-screen.sh out.png DISPLAY|GRAPHIC`) and eyeball it — the token collapses and
cap-band re-centering are invisible until deployed.
