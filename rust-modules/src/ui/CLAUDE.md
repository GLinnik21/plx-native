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
   `size::LABEL` 26 / `size::CAPTION` 24 / `size::MICRO` 22 — the *size* axis of the design system.
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

This table is the map of the directory — **every** `.rs` file under `ui/` has a row, so a missing
entry means the table is stale, not that the module is unimportant. Keep it that way when you add
one: an undocumented module is one the next person re-implements.

| File | Owns |
|---|---|
| `theme.rs` | **all color tokens** + the **`size` type scale** (HERO…MICRO, legibility floor CAPTION 24) + the **`space` gap scale** (`XS` 8 / `SM` 16 / `MD` 24 / `LG` 40 / `XL` 64 — the sibling axis of `size`; a gap between stacked blocks comes from a rung, never a hand-tuned offset) + `scrim`/`scrim_black`/`with_a`/`dim` helpers + focus-ring geometry consts. The single palette + type/space ladder. |
| `mod.rs` | the retui core: `Painter` (cascading alpha/translate — draw through it, never call `gfx::*` directly from a screen), `Rect`/`Size`/`Spring`/`Env`, the `View` trait, and the shared screen primitives `on_axis` (the ONE off-screen cull test the scroll flow uses to skip off-frame children — culling, not the `Painter::clip` scissor) / `hero_alpha` (the ONE hero-fade curve both screens call) / `ScrollColumn`+`Column` (the scroll-into-content container detail's below-hero flow is composed from). |
| `label.rs` | `Label` — single-run cap-band text (the layout ≠ paint primitive). Also `HAlign`/`VAlign`. |
| `text_view.rs` | `TextView` — multi-line cap-band text: pixel word-wrap + ellipsis + `measure_h`, wrap-cached. |
| `widgets.rs` | reusable leaves: `Button` (+`ControlStyle`), `TabPill` (+`TabStyle`), `TransportButton`, `CircleButton`, `Spinner`, `PageDots`, `StatusOverlay` (+`StatusKind` — the Working/Failed treatment the player draws over Connecting/Buffering/Seeking/Error; the *caller* supplies the caption so the state machine stays the single source of that string), the shared art-tile core (`card`/`draw_card` + `Art`), plus the poster-resolve helper `resolve_tex`. **There is no `ProgressBar` type** — the HUD scrubber is immediate-mode inside `player_hud.rs` (see the deliberately-immediate-mode gotcha below); if you need a bar elsewhere, that is the moment to promote one here, not to assume it exists. |
| `card_row.rs` | `CardRow` + `RowStyle` — the animated shelf component shared by the home grid, detail Related, the library grid, and (circular, via `RowStyle::PROFILES`) the who's-watching avatars. `RowStyle::HOME` is the single source of shelf motion+geometry; the row owns per-cell scale springs + the scroll spring and exposes `draw_tile`/`draw_focused` plus the `reveal`/`scroll_into_view` scroll math. Callers keep their own x/scroll/z-order loop (home's cross-row focused-last stays in `Grid`). |
| `table.rs` | `TableView`/`Section`/`Row`/`Badge` — the animated list (settings/track-menu look). |
| `icons.rs` | `Icon` enum + antialiased SVG rasterizer; color is the `tint` you pass. |
| `popover.rs` | `Popover` — the ONE open/appear choreography every modal panel shares (track menu, Info card, Chapters strip, profile menu): an OPEN flag + a critically-damped 0→1 appear spring driving fade + slide, with an optional full-screen scrim, handed out as a ready-made `painter(scrim_a, rise)`. Each panel used to hand-wire its own `static OPEN + APPEAR` pair, so any motion change was a four-file edit — **do not re-fork it.** |
| `press.rs` | The shared tvOS-style **click**: OK-down dips the focused control, OK-up releases with an overshoot bounce and commits the activation a beat later (so the bounce is actually on screen). Genuinely event-driven, so a held OK is a measurable long-press (`is_long`), not a tap. ONE control is pressed at a time (focus can't move mid-press — navigation `cancel`s it), which is why it is a global: `begin`/`release`/`cancel`/`tick`/`take_commit` + the `scale()` the renderer multiplies the focused tile by. Resolves stuck presses three ways because the Magic Remote drops key-ups. |
| `consts.rs` | Layout + input + animation constants — card/gap/margin geometry, `ROW_PITCH`, and the webOS **remote wcodes** with the `is_ok`/`is_back` predicates (see the raw-`SDL_KeyboardEvent` gotcha in the root `CLAUDE.md`). One source so hand-tuned pixel offsets can't drift between widgets. |
| `fmt.rs` | The shared display formatters — `dur_short` ("2h 15m") / `dur_long` / `time_left` / `clock`. ONE home for these, replacing the "2 hr 15 min" vs "2h 15m" vs "0 hr 45 min" drift across screens. Format a duration here, never inline. |
| `profile.rs` | draw profiler (diagnostic). `profile::phase("name", \|\| draw_x())` brackets a phase with `glFinish` to log its real per-frame GPU cost; on via `/tmp/plxnative-profile`, zero-overhead off. Use it to find fill/overdraw before guessing. FPS is also logged once/sec (grep `FPS=`). |
| `anim.rs` | spring diagnostic (not chrome). `anim::probe(name, pos, vel, target, dt)` right after a `Spring::step` logs that spring's settle metrics (frames, ms, overshoot %) and draws a live approach-curve overlay; on via `/tmp/plxnative-anim` or the ANIM toggle, zero cost off. Reach for it before hand-tuning a stiffness. |
| `home.rs` / `detail.rs` / `player_hud.rs` / `info_panel.rs` / `track_menu.rs` / `chapters_panel.rs` / `library.rs` / `login.rs` / `profiles.rs` / `account_menu.rs` | **screens** — compose the above; hold their own springs + input. Should contain almost no color literals. `library.rs` is the Library browse screen (shared top tab row + server-driven Sort/Filter/Unwatched chips + a 6-across grid of `card_row` tiles, menus built from `Popover`+`TableView`); `login.rs` is the QR / short-code sign-in driven by `crate::auth`'s phase; `profiles.rs` is the "who's watching" picker + PIN keypad, built on the **shared** `CardRow` with a circular `RowStyle::PROFILES` so avatars get the same springs as the poster shelves; `account_menu.rs` is the top-left profile popover (change profile / sign out) on the same `TableView` as the track menu. |

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

Three signals, in ascending cost. **`make check`** first — the host unit suite (`cargo test --lib`,
~0.3s) includes nine UI tests: `home.rs`'s focus packing round trips, row stepping staying
inside the shelf array, and the pointer hit column matching the drawn card at every snap phase; and
`card_row.rs`'s heading-clearance behaviour driven frame by frame (the regression that the heading
must hold still while focus walks the slots beneath it). If you touch focus navigation, hit-testing,
or shelf motion, **add a test here** — that math is host-testable and these caught real bugs.
Note the asymmetry, because it tells you where to put a new test: `card_row.rs` drives a **local**
`CardRow`, so its tests are ordinary and parallel; `home.rs` keeps focus in `static mut fr`/`fc`, so
its tests must take the module's `FOCUS` mutex or they race each other rather than the code. That
lock is the cost of screen-level singleton state — hold it, don't work around it.

Then **`make`** must stay green (ARM cross-build). Then the device: there is no host *runtime*, so
nothing above draws a single pixel. For anything that moves pixels, capture the panel on the TV
(`tools/capture-screen.sh out.png DISPLAY|GRAPHIC`) and eyeball it — the token collapses and
cap-band re-centering are invisible until deployed.
