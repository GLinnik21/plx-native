# ui/ — the shared UI system (read before touching any screen)

This is a **real product built for production quality.** "Not worth it for a throwaway" is never a
reason to skip a proper component, leave a bespoke `draw_*` in place, or half-finish a primitive.
If a shared piece is missing (a real text-flow view, a chip, a list),
**build it** — a reusable primitive that pays off across screens is exactly the work worth doing.

This directory is a **design system**, not a pile of per-screen draw code. Home, detail, and the
player HUD are all *compositions of the same tokens + components*. When you add or change UI,
**reach for the shared pieces and improve them in place** — do not paste a new `draw_*` function
with hard-coded colors and hand-tuned text offsets. That is exactly the drift this system exists to
kill. Full design + migration status: `docs/ui-system-migration.md`.

## The four rules

1. **Never write a raw color literal.** Every color comes from `theme.rs` as a named token
   (`theme::TEXT_PRIMARY`, `theme::CONTROL_IDLE_FILL`, `theme::scrim(a)`, …). Need a shade that
   doesn't exist yet? **Add a token to `theme.rs`** (with a doc line saying what it's for) and use
   that — don't inline `[0.9, 0.9, 0.9, 1.0]`. If your shade is within a hair of an existing token,
   use the existing one; the point is one value per role, not a value per call site.

2. **Never write a raw text size — every text size in the UI is a `theme::size` token.** The named
   rungs are `size::HERO` 72 / `size::TITLE` 40 / `size::HEADLINE` 32 / `size::BODY` 28 /
   `size::LABEL` 26 / `size::CAPTION` 24 / `size::MICRO` 16 — the *size* axis of the design system.
   `CAPTION` (24) is the **couch legibility floor for anything that must be read**; `MICRO` exists
   solely for one-line de-emphasized kickers beside outsized titles (the hero meta line), never for
   content. Pass a rung to `Painter::text`/`Label`/`TextView`/`text::elide` instead of a bare
   `24`/`28`/… — a size is a role, not a magic number; pick the nearest rung, and if no rung fits a
   genuinely new role, **add a documented rung to `theme.rs`**, don't inline a literal. Exactly two
   carve-outs live outside the scale (the player-HUD display title `HUD_TITLE_SZ` and the subtitle
   caption); both are **named + commented at their call site**, never bare literals — don't add a
   third; new roles go on the scale. `anim.rs` is a dev-diagnostic overlay, not chrome.

3. **Never hand-place text with a magic y.** `y - sz*0.58` guesses are banned — they mis-center the
   moment a string has a descender (g j y p). Text is positioned by its **cap band** (layout ≠
   paint): use `ui::label::Label` (single run) or `ui::text_view::TextView` (multi-line, pixel-
   wrapped), or if you must call `Painter::text` directly, derive the y from `text::text_vcenter_y` /
   `text::text_cap_band`. See `label.rs`'s module docs for the rule.

4. **Improve a component before forking one.** If a shared widget almost does what you need, add a
   builder method / style variant to it (e.g. `Button::style(ControlStyle)`), don't copy it. A new
   bespoke widget is only justified when nothing here is close — and then it lands *here*, as a
   reusable `View`, so the next screen gets it for free.

## What lives where

| File | Owns |
|---|---|
| `theme.rs` | **all color tokens** + the **`size` type scale** (HERO…CAPTION, floor 24) + `scrim`/`scrim_black`/`with_a`/`dim` helpers + focus-ring geometry consts. The single palette + type ladder. |
| `mod.rs` | the retui core: `Painter` (cascading alpha/translate — draw through it, never call `gfx::*` directly from a screen), `Rect`/`Size`/`Spring`/`Env`, the `View` trait, and the shared screen primitives `on_axis` (the ONE off-screen cull test the scroll flow uses to skip off-frame children — culling, not the `Painter::clip` scissor) / `hero_alpha` (the ONE hero-fade curve both screens call) / `ScrollColumn`+`Column` (the scroll-into-content container detail's below-hero flow is composed from). |
| `label.rs` | `Label` — single-run cap-band text (the layout ≠ paint primitive). Also `HAlign`/`VAlign`. |
| `text_view.rs` | `TextView` — multi-line cap-band text: pixel word-wrap + ellipsis + `measure_h`, wrap-cached. |
| `widgets.rs` | reusable leaves: `Button` (+`ControlStyle`), `TabPill`, `TransportButton`, `CircleButton`, `ProgressBar`, `Spinner`, `PageDots`, the shared art-tile core (`card`/`draw_card` + `Art`), plus the poster-resolve helper. |
| `card_row.rs` | `CardRow` — the animated poster-shelf component shared by the home grid and detail Related (`RowStyle::HOME` = the single source of shelf motion+geometry; owns per-cell scale springs + scroll spring + `draw_tile`/`draw_focused`). Callers keep their own x/scroll/z-order loop (home's cross-row focused-last stays in `Grid`). |
| `table.rs` | `TableView`/`Section`/`Row`/`Badge` — the animated list (settings/track-menu look). |
| `icons.rs` | `Icon` enum + antialiased SVG rasterizer; color is the `tint` you pass. |
| `profile.rs` | draw profiler (diagnostic). `profile::phase("name", \|\| draw_x())` brackets a phase with `glFinish` to log its real per-frame GPU cost; on via `/tmp/plxnative-profile`, zero-overhead off. Use it to find fill/overdraw before guessing. FPS is also logged once/sec (grep `FPS=`). |
| `home.rs` / `detail.rs` / `player_hud.rs` / `info_panel.rs` / `track_menu.rs` / `chapters_panel.rs` | **screens** — compose the above; hold their own springs + input. Should contain almost no color literals. |

## Gotchas that bite

- **`Label`/`Button` hold a non-owning `*const c_char`.** Keep the `CString` alive for the whole
  draw frame (bind it to a `let` in the same scope) or you'll draw freed memory. (`TextView` is the
  exception — it borrows a `&str` and builds its own `CString`s internally, so it's memory-safe.)
- **The focus ring/glow is shader-baked** (`gfx.rs` `FS_SRC`/`FS_IMG`, folded into the card
  composite pass): callers drive it only through a `focus: f32` scalar and the geometry consts
  (`theme::CARD_RING_RAD`, `consts::GLOW_PAD`). Its color cannot be tokenized — don't try, and
  don't re-add it as a literal.
- **`detail.rs` below-hero layout is a computed flow, not magic constants.** The below-hero sections
  are the children of a shared `ScrollColumn` (`impl Column for DetailView`): the container's
  `child_top(i)` stacks the *present* blocks' `block_h()` heights (via `Column::height`) from
  `CONTENT_TOP` with one `SECTION_GAP` (season tabs → episodes hug with `TAB_EP_GAP`). To resize/space
  a section, change its `block_h` (content-derived — e.g. Related tracks `REL_H`=`CARD_H`) or the gap —
  never reintroduce a hard-coded per-section Y. `ScrollColumn::draw` culls off-screen sections with
  `on_axis` and pre-translates each child painter to its origin, so each `draw_*` draws from local
  y=0; the same `child_top` feeds `scroll_target` (via `lift_target`), so draws and scroll can't drift.
- **A few things are deliberately immediate-mode** (documented in `docs/ui-system-migration.md` §D):
  home's `Backdrop` per-element alphas, `player_hud`'s `SCR_H`-offset geometry (shared with `app.rs`
  pointer hit-tests), and the subtitle renderer. Leave them; wrapping them in a `View` breaks a
  load-bearing contract.
- **Clipping: prefer culling; use `Painter::clip` only for a hard-bounded panel.** A real GL scissor
  clip exists now — `Painter::clip(rect)` / `clip_clear()` (backed by `gfx::clip_set`/`clip_clear`). It
  is **global GL state**, so you MUST pair set/clear inside the same frame (see `TableView::draw`, its
  one user — a fixed panel whose overflowing list is cut cleanly at the frame). Do NOT reach for it in
  the big scroll flow: `ScrollColumn`/the shelves deliberately **cull** off-frame children by index
  (`on_axis`) instead, which avoids per-frame scissor churn and needs no clean-up. So: bounded list/panel
  → `clip`; long scrolling document → cull. (The old edge-fade-mask trick is gone — a linear fade can't
  cut a tall two-line row evenly, which read as a broken clip; scissor replaced it.)

## When you're done

`make` is the only correctness signal (ARM cross-build; no host runtime). After a UI change, `make`
must stay green, and for anything that moves pixels, capture the panel on the TV
(`tools/capture-screen.sh out.png DISPLAY|GRAPHIC`) and eyeball it — the token collapses and
cap-band re-centering are invisible until deployed.
