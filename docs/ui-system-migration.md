# Shared UI System — Migration Plan

> **Status (2026-07-18): EXECUTED — both halves are done and merged to main.** The token/
> component sweep landed as planned; the **view-tree half below is stale planning-tense** (it
> predates the executed work and e.g. describes a 5-field `DetailView` that is now ~17 fields).
> The accurate executed record for the view-tree migration — including the later Step 8
> (shared scroll/cull/hero infra, stopped after 8.5) — is `docs/ui-viewtree-plan.md` §G. Only
> the explicitly optional 7b (unified FocusPath) remains unbuilt, by design. This file is kept
> for the design rationale (token values, carve-outs, player-as-reference stance).

Extract a shared UI system from the **player screen** (the one screen with a coherent
design language: `widgets.rs` / `table.rs` / `label.rs` / `icons.rs` / `mod.rs`) and
migrate the two drifted client screens (`home.rs`, `detail.rs`) onto it — first onto
**tokens + components + cap-band text**, then onto the **retained `View` tree** (`mod.rs`
`update → layout → draw`).

**Correctness signal:** there is no host runtime. The *only* build/regression check is
`make` (webOS-NDK ARM cross-build). Every numbered step below must leave `make` green, and
must not change the C-ABI entry-point *signatures* that `app.rs` / `route.rs` / the
dev-trigger headless harness call (`home_init/update/draw/move_focus/pointer_focus/wheel`;
detail `open/open_rk/open_rk_season/close/update/draw/move_focus/on_ok`) until the final
thin-wrapper phase.

**Design stance (synthesized).** The user chose the *full retui migration* — home **and**
detail onto the retained tree. We commit to that, taking the player substrate's literals as
the canonical token values and its component APIs as the shared APIs (player-as-reference),
staged so the low-risk token/text sweep lands first and the structural tree work lands last
behind its own checkpoints. Three carve-outs stay immediate-mode on purpose (§D) because a
`View` wrapper there breaks a load-bearing contract for zero payoff.

---

## (A) TOKEN TABLE

New module `rust-modules/src/ui/theme.rs`. Colors are `pub const [f32; 4]` unless noted.
`ACCENT` / `ACCENT_INK` move here from `mod.rs:28-29` and are **re-exported** from `mod.rs`
(`pub use theme::{ACCENT, ACCENT_INK};`) so existing `crate::ui::ACCENT` imports in
`widgets.rs` / `table.rs` keep compiling unchanged.

Two collapses are deliberately **lossy** (many near-identical values → one token). They are
invisible until on-TV capture; each is gated behind its own `make` + capture checkpoint.

### Text
| Token | Value | Replaces (file:line) |
|---|---|---|
| `TEXT_PRIMARY` | `[0.97, 0.98, 0.99, 1.0]` | home hero title `home.rs:140`, hub title `:291` (`[0.93,0.95,0.98]`, keep `env.sp` alpha), focused-card title `:310` (`[0.96,0.97,0.98]`); detail `:306/:447/:453/:478/:525/:640` (`[0.98,0.99,1.0]` variants); table row `table.rs:220` (`[0.98,0.98,1.0]`); `player_hud.rs:212`; `info_panel.rs:268` (`[0.97,0.98,1.0]`); `chapters_panel.rs:156` focused |
| `TEXT_HEADING` | `[0.92, 0.94, 0.97, 1.0]` | detail "Related"/"Cast & Crew" `detail.rs:561/:607` (`[0.90,0.92,0.95]`), About headings/MORE `:857` (`[0.95,0.96,0.98]`), related title `:592` (`[0.85,0.87,0.90]`), badge glyph `:847` |
| `TEXT_SECONDARY` | `[0.72, 0.75, 0.80, 1.0]` | home hero meta/synopsis `home.rs:141` (`[0.70,0.73,0.78]`); detail meta/synopsis `detail.rs:307` (`[0.74,0.77,0.82]`), episode/cast idle title `:525/:640` (`[0.80,0.82,0.86]`); `player_hud.rs:213` (`[0.72,0.74,0.80]`); `info_panel.rs:269` (`[0.69,0.69,0.71]`); `chapters_panel.rs:156` idle |
| `TEXT_TERTIARY` | `[0.58, 0.60, 0.64, 1.0]` | detail runtime `detail.rs:308`, inactive tab `:478`, ep kicker `:505/:527`, cast role `:646` (`[0.56,0.58,0.62]`), About labels `:859` (`[0.55,0.57,0.62]`), genres `:860` (`[0.66,0.68,0.72]`); table dim/header `table.rs:221` (`#8a8a8e [0.541,0.541,0.557]`), empty-state `:211` (`[0.60,0.62,0.68]`); `chapters_panel.rs:145` (`[0.62,0.64,0.68]`) |
| `INK_ON_ACCENT` | `[0.03, 0.03, 0.04, 1.0]` | = existing `ACCENT_INK` (`mod.rs:29`); detail DARK_INK `detail.rs:410` (`[0.05,0.06,0.08]`); PillButton play ink `widgets.rs:109` (`[0.05,0.06,0.08]`) |

### Accent / control
| Token | Value | Replaces |
|---|---|---|
| `ACCENT` | `[0.914, 0.902, 0.878, 1.0]` (#e9e6e0) | keep (`mod.rs:28`); **replaces** detail's inverted cool-white focus fill `detail.rs:409` (`WHITE [0.97,0.98,0.99]`) — a deliberate design unification (see §Tradeoffs) |
| `CONTROL_IDLE_FILL` | `[0.145, 0.145, 0.153, 0.92]` | the 3 verbatim dups `widgets.rs:256/:298/:338` (TransportButton/TabPill/Button idle disc); detail DARK_FILL `detail.rs:411` (`[0.16,0.17,0.20,0.55]` — small darken); CircleButton face `widgets.rs:142` (`[0.42,0.44,0.50,0.5]`) |
| `CONTROL_IDLE_INK` | `[1.0, 1.0, 1.0, 1.0]` | white glyph over idle disc `widgets.rs:256/:298/:338`; detail LIGHT_INK `detail.rs:412` (`[0.95,0.96,0.98]`); CircleButton ink `widgets.rs:142` (`[0.92,0.94,0.97]`) |
| `FILL_PRIMARY` | `[0.97, 0.98, 0.99, 1.0]` | hero primary-CTA cool-white control fill (semantically a *fill*, not text): PillButton play fill `widgets.rs:109` |
| `INK_ON_PRIMARY` | `[0.05, 0.06, 0.08, 1.0]` | near-black ink over `FILL_PRIMARY`: PillButton ink `widgets.rs:109` |

### Surfaces / backgrounds
| Token | Value | Replaces |
|---|---|---|
| `SURFACE_APP` | `[0.10, 0.10, 0.115, 1.0]` | flat shelf/app base `home.rs:86` (`g`, both gradient stops) |
| `CLEAR_RGB` | `(0.03, 0.03, 0.045)` **3-float tuple** | GL clear `home.rs:428 frame_clear` — kept as a 3-float form (`frame_clear` takes `r,g,b`, no alpha), distinct from `SURFACE_APP` which overdraws it |
| `SURFACE_PANEL` | `[0.133, 0.133, 0.141, 1.0]` | opaque menu panel / fade mask / badge knockout `table.rs:81`; meta_badge interior `info_panel.rs:135` |
| `PANEL_TOP` | `[0.129, 0.129, 0.137, 0.985]` | near-opaque sheet gradient top `track_menu.rs:327`; info card bg `info_panel.rs:218` |
| `PANEL_BOT` | `[0.106, 0.106, 0.114, 0.985]` | sheet gradient bottom `track_menu.rs:328` (keep distinct — the gradient is deliberate) |
| `CARD_PLACEHOLDER` | `[0.12, 0.13, 0.16, 1.0]` | card skeleton `widgets.rs:52 draw_card`; related poster `detail.rs:587`; info still `info_panel.rs:239` |
| `SKELETON_TOP` | `[0.13, 0.14, 0.17, 1.0]` | poster skeleton top `widgets.rs:18` (SK_T); cast disc top `detail.rs:634` (`[0.16,0.17,0.20]` — accept slight lighten) |
| `SKELETON_BOT` | `[0.08, 0.09, 0.11, 1.0]` | poster skeleton bottom `widgets.rs:19` (SK_B); cast disc bottom `detail.rs:634` (`[0.10,0.11,0.13]`) |

### Scrims (near-black; alpha supplied per call via helpers)
| Token / helper | Value | Replaces |
|---|---|---|
| `SCRIM_INK` + `fn scrim(a) -> [f32;4]` | rgb `[0.02, 0.02, 0.03]` | home hero scrim `home.rs:104/:105` (`sa = 0.30+0.64*hero_a`); detail hero scrim `detail.rs:293` + scroll dim `:300` |
| `SCRIM_BLACK` + `fn scrim_black(a) -> [f32;4]` | rgb `[0.0, 0.0, 0.0]` | HUD bottom scrim `player_hud.rs:208/:209` (0.0→0.86); subtitle outline `:86` (0.85); modal scrim `track_menu.rs:318` (0.58·appear) |

### Rails / overlays / accents
| Token | Value | Replaces |
|---|---|---|
| `RAIL_TRACK` | `[1.0, 1.0, 1.0, 0.20]` | ProgressBar track `widgets.rs:367`; scrubber track `player_hud.rs:214` (merge 0.24 near-dup) |
| `RAIL_BUFFERED` | `[1.0, 1.0, 1.0, 0.28]` | ProgressBar buffered `widgets.rs:385`; home resume track `home.rs:67`; detail ep resume track `detail.rs:520` |
| `RAIL_FILL` | `[1.0, 1.0, 1.0, 0.95]` | ProgressBar fill `widgets.rs:367`; detail ep resume fill `detail.rs:521` |
| `RESUME_FILL` | `[0.98, 0.72, 0.18, 0.95]` | warm amber Continue-Watching fill `home.rs:68` (unique Plex-progress accent; no player equivalent — hosted so `Card`/`ProgressBar.fill` can import it) |
| `HAIRLINE` | `[1.0, 1.0, 1.0, 0.10]` | section divider `table.rs:243` |
| `OVERLAY_FOCUS_PILL` | `[1.0, 1.0, 1.0, 0.14]` | detail hand-rolled tab focus pill `detail.rs:481` (becomes `ACCENT` when `TabPill` is adopted — kept as a token until then) |
| `OVERLAY_FOCUS_SOFT` | `[1.0, 1.0, 1.0, 0.07]` | About selection panel `detail.rs:876` |
| `OVERLAY_BORDER` | `[1.0, 1.0, 1.0, 0.55]` | meta_badge border `info_panel.rs:132` |
| `TINT_WHITE` | `[1.0, 1.0, 1.0, 1.0]` | no-op texture tint (structural, not drift): `home.rs:97`, `detail.rs:282/:581/:628`, `info_panel.rs:233` |

### Non-tokenizable (documented so a sweep leaves them alone)
| Name | Value | Note |
|---|---|---|
| `FOCUS_GLOW` (doc only) | ring `vec3(1.0)`, glow `vec3(0.85,0.9,1.0)` @0.40 | **Shader-baked** in `gfx.rs:12` (FS_SRC). `draw_rect`/`Painter::ring` expose only a `focus:f32` scalar. An ACCENT-tinted or reshaped ring requires a shader uniform edit — out of scope. Recorded so no one re-adds it as a literal. |
| `CARD_RING_PAD_GRID` = `48.0`, `CARD_RING_PAD_STRIP` = `6.0`, `CARD_RING_RAD` = `14.0`, `FOCUS_ON` = `1.0`, `FOCUS_OFF` = `0.0` | — | Ring **geometry** constants (not colors). Home grid ring uses pad 48 + focus `(s-1)/0.055` (`home.rs:307`); `draw_card`/tiles use pad 6 + focus 1.0 (`widgets.rs:55`). These are visibly different glows — expose the pad as a parameter so home migrates byte-identically; do **not** unify blindly. |

---

## (B) COMPONENT CATALOG

All components draw through `Painter` over the C `gfx`/`text` primitives (no GL). Label-family
structs hold a non-owning `*const c_char` (Copy), so every call site must keep its `CString`
alive for the draw frame — the existing discipline (`table.rs` already does this inline).

### `theme` module — **new** (`rust-modules/src/ui/theme.rs`)
```rust
// tokens from §A as `pub const [f32; 4]` (+ CLEAR_RGB: (f32,f32,f32))
pub const fn scrim(a: f32)       -> [f32; 4] { [0.02, 0.02, 0.03, a] }
pub const fn scrim_black(a: f32) -> [f32; 4] { [0.0,  0.0,  0.0,  a] }
// splat a token's rgb with an overridden alpha (for env.sp-baked hub title, etc.)
pub const fn with_a(c: [f32; 4], a: f32) -> [f32; 4] { [c[0], c[1], c[2], a] }
```

### `Label` — **exists** (`label.rs:40`), keep verbatim
```rust
Label::new(text: *const c_char, sz: c_int, col: [f32;4]) -> Label
    .bold() .h(HAlign::{Left,Center,Right}) .v(VAlign::{Middle,CapTop,Baseline})
    .draw(&self, p: Painter, frame: Rect) -> f32   // painted width
```
The **layout ≠ paint** cap-band primitive (`text::text_cap_band`, `text.rs:315`). Every
single-run `p.text()` with a hand-tuned y in home/detail migrates onto this. Because it
centers the **cap band** (not the top-anchored y), each migrated run needs a derived frame:
`VAlign::CapTop` puts cap-top on `frame.y`, closely matching `draw_text`'s top-left `y`
(verify the small `cap_top` offset on-device).

### `TextView` — **SHIPPED** (`text_view.rs`; was planned here as `TextBlock` in `widgets.rs`)
```rust
TextView::new(text: &str, sz: c_int, col: [f32;4]) -> TextView
    .bold() .leading(px: f32) .h(HAlign) .max_lines(n: usize)
    .measure_h(&self, width: f32) -> f32
    .draw(&self, p: Painter, frame: Rect) -> f32   // consumed height; frame.w = wrap width
```
The one thing `Label` cannot express: multi-line flow. It went further than the planned
`TextBlock`: instead of taking pre-wrapped `*const c_char` lines, it takes a raw `&str` and does
its **own pixel word-wrap** (measured, not char-count), cap-band `Label` per line, ellipsis at
`max_lines`, and reports height. So it **replaced** the char-count `wrap_two`/`wrap_ep` helpers
(deleted) at the hero synopses (`home.rs`, `detail.rs`), episode summaries, and About columns
(`draw_pair`/Languages/Accessibility). Wrapping is memoized (`WRAP_CACHE`, `Rc`-shared) so the
per-frame cost is a hash + refcount bump. Not a reflow engine; the About summary's inline `MORE`
tail still uses `wrap_lines` pending a trailing-run hook.

### `Button` — **generalize** (`widgets.rs:310`)
```rust
enum ControlStyle { Accent, Primary, Custom { fill: [f32;4], ink: [f32;4] } }
Button::new(label: *const c_char, sz: c_int, frame: Rect) -> Button
    .icon(Icon) .leading_tri(bool) .focused(bool) .style(ControlStyle)
impl View for Button   // update noop; draw()
```
Already centers `[icon + gap + label]` via `text_vcenter_y` and encodes ACCENT-vs-idle by
`.focused()`. Add `.style()` (default `Accent` = `ACCENT`/`INK_ON_ACCENT` focused,
`CONTROL_IDLE_FILL`/`CONTROL_IDLE_INK` idle; `Primary` = `FILL_PRIMARY`/`INK_ON_PRIMARY`) and
`.leading_tri()` to absorb `PillButton::play`'s triangle. Replaces detail's `draw_buttons`
(`detail.rs:406`) wholesale — its four fn-local consts (WHITE/DARK_INK/DARK_FILL/LIGHT_INK)
become `Primary`/`Accent` + tokens. `PillButton` (`widgets.rs:99`) and `CircleButton`
(`:133`, keep for circular +/i glyphs) fold their raw-y text onto `Label` internally so the
last hand-guessed baselines in the substrate die.

### `TabPill` — **exists** (`widgets.rs:271`), already token-aware
```rust
TabPill::width(chars: usize, sz: c_int) -> f32
TabPill::new(label: *const c_char, sz: c_int, frame: Rect) -> TabPill .focused(bool)
impl View for TabPill   // ACCENT focus / CONTROL_IDLE_FILL idle; label via Label::Center.bold()
```
Replaces detail's hand-rolled `draw_tabs` (`detail.rs:458`: manual measure pass +
`[1,1,1,0.14]` highlight pill). **Adoption is a visible change** — detail's active tab goes
from near-white-text-on-faint-pill to a filled ACCENT pill (intended unification; call it out
in the PR).

### `Row` / `TableView` — **exists** (`table.rs`), keep bespoke ABI
```rust
TableView::new(); set_sections(Vec<Section>, sel: i32, slide: bool);
    move_sel(i32); update(dt: f32, visible_h: f32); draw(p: Painter, frame: Rect);
    measured_height() -> f32;  field .sel
Section::new(header).accessory(a).row(Row)
Row::new(label).checked(b).detail(s).badge(Badge).chevron(b).dim(b)   // ROW_H=60 / _TALL=92
Badge::{ Ad, Forced, Sdh, Cc, Text(String) }
```
The cleanest realized pattern — owns `hl_top`/`hl_bot`/`scroll` springs and a sliding ACCENT
pill; `track_menu` delegates 100%. **Do not** force it onto the `View` trait for the PlxNative
(reconciling its `update(dt,visible_h)`/`draw(p,frame)` ABI with `View::update(&Env)`/
`draw(&Env,p)` is churn with zero payoff). Only sweep its literals onto tokens
(`:220`→`TEXT_PRIMARY`, `:221`→`TEXT_TERTIARY`, `:81`→`SURFACE_PANEL`, `:243`→`HAIRLINE`;
`:232/:259/:260` already ACCENT). Reuse `Row` for detail's About key/values (`draw_pair`,
`detail.rs:833`).

### `Icon` — **exists** (`icons.rs:13/:96`), theme-safe, no change
```rust
enum Icon { Cc, Audio, Check, Chevron, Play, Pause, Info /* + Plus, Info-glyph as needed */ }
icons::draw(p: Painter, id: Icon, r: Rect, tint: [f32;4])
```
Color is entirely the `tint` (mask rendered white), so passing any token just works. Add
`Icon::Plus` so detail's `+`/`i` glyph `CircleButton`s (`detail.rs:422/:426`) can become real
AA icons matching the transport buttons (optional polish). Caveat: `static mut CACHE`,
main-GL-thread only.

### `Card` + focus-ring — **new**, unifies the two inline cards + `draw_card` + `draw_poster`
Retained View owning the scale-pop spring (the `ProgressBar` pattern, `widgets.rs:355`):
```rust
enum Art<'a> { Poster(Option<&'a PmsMovie>), Thumb { key: &'a str, res: (c_int,c_int) }, None }
struct RingStyle { pad: f32, rad: f32 }
impl RingStyle { const GRID: RingStyle = RingStyle { pad: 48.0, rad: 14.0 };
                 const TILE: RingStyle = RingStyle { pad: 6.0,  rad: 14.0 }; }

struct Card { frame: Rect, scale: Spring, focused: bool, radius: f32,
              art: Art, ring: RingStyle, resume: f32 /* 0 = none */, resume_fill: [f32;4] }
Card::poster(m: Option<&PmsMovie>) -> Card
Card::thumb(key: &str, res: (c_int,c_int)) -> Card
    .focused(bool) .radius(f32) .ring(RingStyle) .resume(frac: f32)
impl View for Card {
    fn update(&mut self, e: &Env) { self.scale.step(if self.focused { TARGET } else { 1.0 }, K_SCALE, e.dt); }
    fn draw(&self, _e: &Env, p: Painter) {
        let r = if self.focused { self.frame.scaled(self.scale.pos) } else { self.frame };
        // draw_poster / tex-or-CARD_PLACEHOLDER; resume rail (RAIL_BUFFERED / resume_fill);
        // if focused: p.ring(r, self.ring.pad, self.ring.rad * self.scale.pos, focus_scalar)
    }
}
```
Replaces home `Card` (`home.rs:194`, currently owns only the spring — visuals decomposed
inline in `Shelf::draw_cells`/`Grid::draw`), the free `draw_card` (`widgets.rs:39`), and
detail's inline Related tiles (`detail.rs:574-594`). Grid keeps `RingStyle::GRID` (pad 48,
focus `(s-1)/0.055`); episode/chapters/related keep `RingStyle::TILE`.
**Z-order caveat:** the parent still owns depth — `Grid` draws non-focused Cards, then the
focused Card **last** (`home.rs:296-311`); `Card::draw` alone cannot reproduce the
focused-last overlay. Cast headshots stay inline (circular, `detail.rs:614-648`) — not worth a
Card variant for one call site. `draw_card`/`draw_poster` stay as thin free-fn shims over
`Card` during migration so `chapters_panel`/`info_panel` callers are untouched.

### `Chip` — **new**, unifies the badge impls (low priority, do last)
```rust
enum ChipStyle { Outlined, Filled }
chip(p: Painter, x: f32, cy: f32, text: *const c_char, style: ChipStyle) -> f32   // width
```
Collapses `meta_badge` (`info_panel.rs:127`, border `OVERLAY_BORDER`, h34/sz22, cap-band),
`draw_badge` (`table.rs:389`, border=col, h26/sz19, `cy-sz*0.58` guess), and detail's
accessibility badges (`detail.rs:843`). Interior `SURFACE_PANEL`, text via `Label(Middle)` so
the 0.58 guess dies. Pick one metric (≈h30/sz20) and accept a few-px shift on the Info card +
track menu.

---

## (C) IMPLEMENTATION PLAN

Ordering principle: **additive first → values → text → components → tree → routing**;
one file per step where possible; `make` green after **every** step; home before detail;
the C-ABI signatures unchanged until the final wrapper phases. Steps 5-6 (the tree work) and
step 7 (routing) are the ones the user explicitly asked for; steps 1-4 de-risk them.

**Step 1 — add `ui/theme.rs` (additive, no call sites).**
Create `rust-modules/src/ui/theme.rs` with all §A tokens + `scrim`/`scrim_black`/`with_a`
helpers + the ring-geometry/`FOCUS_*` constants. Move `ACCENT`/`ACCENT_INK` into it; add
`pub mod theme;` and `pub use theme::{ACCENT, ACCENT_INK};` to `mod.rs`. Nothing consumes the
new tokens yet. *Checkpoint:* `make` (pure addition; `#![allow(dead_code)]` already present).

**Step 2 — migrate color literals to tokens, screen by screen (no structure change).**
Pure literal→const substitution; byte-identical except the deliberate merges.
- **2a substrate** (`widgets.rs` + `table.rs`): the 3 `CONTROL_IDLE_FILL` dups
  (`:256/:298/:338`), PillButton fill/ink (`:109`), ProgressBar rails (`:367/:385`),
  `draw_poster` SK_T/SK_B (`:18/:19`), `draw_card` placeholder (`:52`), `table.rs` PANEL_BG
  (`:81`)/row text (`:220/:221`)/hairline (`:243`). *Checkpoint:* `make` — player screen now
  fully tokenized, zero behavior change.
- **2b player surfaces** (`player_hud.rs` / `track_menu.rs` / `info_panel.rs` /
  `chapters_panel.rs`): scrims → `scrim_black`, panel gradients → `PANEL_TOP/BOT`, text →
  `TEXT_*`, meta_badge interior → `SURFACE_PANEL`. *Checkpoint:* `make`.
- **2c home** (`home.rs`): `:86`→`SURFACE_APP`, `:67`→`RAIL_BUFFERED`, `:68`→`RESUME_FILL`,
  `:104/:105`→`scrim(sa)`, `:140/:291/:310`→`TEXT_PRIMARY` (hub title keeps `env.sp` alpha via
  `with_a(TEXT_PRIMARY, env.sp)`), `:141`→`TEXT_SECONDARY`, `:428`→`CLEAR_RGB`. Keep
  Backdrop's per-element alphas (`:80-108`) **explicit** — do not fold into `p.alpha()`
  (ambient writes opaque pixels; art/scrim fade on independent curves. The cascade still reaches
  the whole `Backdrop` from above — see the amended carve-out 1 below.)
  *Checkpoint:* `make` + on-TV capture (3 near-whites collapse to one — intended).
- **2d detail** (`detail.rs`): reconcile the two fn-local palettes (`draw_buttons` `:409-412`,
  `draw_about` `:857-860`) + scattered runs onto `TEXT_PRIMARY/HEADING/SECONDARY/TERTIARY` +
  `INK_ON_ACCENT`; backdrop/scrim `:282/:293/:300`→`TINT_WHITE`/`scrim`; placeholders
  `:587/:634`→`CARD_PLACEHOLDER`/`SKELETON_*`; overlays `:481/:520/:521/:876`→`OVERLAY_*`/
  `RAIL_*`. Leave `draw_buttons`' focus **inversion** as literals for now (pure color-merge, no
  design decision yet). *Checkpoint:* `make` + capture.

**Step 3 — sweep `Label` / cap-band across all raw `p.text()` sites (text positioning).**
The high-risk-per-pixel, low-structural-value step — sequence it isolated so a regression
bisects to one screen. `Label` centers the cap band, so each run needs a derived frame that
reproduces its current top-anchored draw-y (`VAlign::CapTop` at `frame.y = current_y`).
- **3a** add `TextBlock` to `widgets.rs` (additive; `make`).
- **3b home**: hero title/meta (`home.rs:163/:169`) → `Label`; synopsis (`:174-183`) →
  `TextBlock` (leading 30 to match the +128/+158 step); hub title (`:291`) and focused-card
  title (`:310`) → `Label`. *Checkpoint:* `make` + capture (first step that can shift pixels
  vertically — eyeball hero + card titles vs a pre-change capture).
- **3c detail**: the 22 single runs → `Label`; the ~10 multi-line runs (synopsis, episode
  summary `:532-542`, About values) → `TextBlock`; kill the invisible-measure-then-place
  patterns (right-aligned "Starring" `:399`, tab measure `:479`) via `Label::Right` /
  `TabPill::width`. *Checkpoint:* `make` + capture.

*Fallback gate:* if a `Label`/`TextBlock` run shows unacceptable drift on-device that the
frame derivation can't dial out, revert **just that run** to the tokenized raw `p.text()`
(color token from step 2 survives). Keeps every checkpoint shippable.

**Step 4 — promote shared components + adopt in home/detail.**
One component swap per sub-step, `make` between each.
- **4a** generalize `Button` (`ControlStyle` + `leading_tri`); fold `PillButton`/`CircleButton`
  text onto `Label` internally. `make`.
- **4b** land `Card`/`RingStyle` in `widgets.rs`; re-express `draw_card`/`draw_poster` as
  shims over it. `make` (player/chapters/info unaffected).
- **4c home** cards: replace `Shelf::draw_cells` non-focused draw (`home.rs:230-246`) and the
  `Grid::draw` focused block (`:297-311`) with `Card` (`RingStyle::GRID`, resume frac). Keep
  the focused-last overlay pass and the skip at `:232`. `resume_bar` + inline ring + card title
  disappear into `Card`. *Checkpoint:* `make` + capture (ring pad/scale-pop unchanged).
- **4d detail** components: `draw_buttons`→`Button{Primary/Accent}`+`CircleButton` — this
  **commits the focus-inversion decision** (cool-white → warm ACCENT); `draw_tabs`→`TabPill`
  (active tab → ACCENT pill); Related tiles→`Card{RingStyle::TILE}`; episode still already uses
  `draw_card`→now `Card`; About key/values→`Row`/`TableView`; badges→`Chip`. `make` + capture
  after 4d (design-visible).

*After step 4 the visible-consistency goal is met on every screen.* Steps 5-7 are the
structural retui migration the user asked for.

**Step 5 — home.rs onto the View tree (mostly already done).**
`Home` already owns `bg/hero/grid` as typed `View` fields with a `static mut SCENE:
Option<Home>` (`home.rs:398`). Finish the contract:
- Move the per-cell scale stepping out of `Shelf::update` into `Card::update` (each `Card` now
  owns its spring, `ProgressBar`-style). `Shelf` keeps `scroll_x`; `Grid` keeps `scroll_y` +
  `nav`/`vert`/`hit_test`/`wheel`.
- Rename `Shelf::draw_cells` → conform to a `draw(&Env, Painter)` shape where practical, but
  **keep** the two-pass overlay in `Grid::draw` (non-focused then focused-last) — a naive
  depth-first draw would paint the ring under neighboring cards.
- Backdrop stays a draw-only `View` managing its own per-element alphas (do not route through
  `p.alpha()`). Focus stays the three `static mut` (`fr/fc/snapTarget`) read live via `Env`
  each frame; the `app.rs` accessor bridge (`row/col/snap_target/set_*`, `app.rs:116-124`) is
  unchanged. *Checkpoint:* `make` + capture. No new C-ABI.

**Step 6 — detail.rs onto the View tree (the real lift — no view instance exists today).**
- **6a** introduce `struct DetailView { section: c_int, col: c_int, scroll: Spring,
  card_scale: Spring, ep_hscroll: Spring }` absorbing the module statics
  (`detail.rs:19-24`), with `static mut DETAIL: Option<DetailView>` mirroring home's `SCENE`.
  Realize `View::update/layout/draw` by moving the bodies of the existing free
  `update/draw/move_focus` onto methods. Make the existing pub fns `open/open_rk/
  open_rk_season/close/update/draw/move_focus/on_ok/last_resume_ns/selected_ptr` **thin
  wrappers** forwarding to methods — so `app.rs` routing, `route.rs`, and the dev-triggers
  (`plxnative-detail*`, `app.rs:886-1053`) compile **unchanged**. *Checkpoint:* `make`.
- **6b** section rows (episodes/related/cast) become child sub-views composed by
  `DetailView::draw` under the translated `ps = p.translate(0,-scroll)` painter. Unify the two
  horizontal-scroll models: adopt the sprung `ep_hscroll` glide for Related/Cast too (kill the
  instant-snap asymmetry `detail.rs:566/:612`) via a shared `HScrollRow` helper
  (`scroll: Spring`, pins focus to 2nd slot; callers still cull off-frame cells by index —
  `Painter` has no clip). **Keep** the absolute `section_top()` Y table
  (`TAB_Y..ABOUT_Y`, `detail.rs:37-52/:108-125`) — do **not** build a flow/`VStack`; About's
  dynamic height doesn't justify a layout engine in a PlxNative, and a naive reflow risks section
  collisions. *Checkpoint:* `make` + capture.

**Step 7 — align app.rs routing/input (the convergence — touches shared state, do last).**
- **7a** replace the flat route bools (`playing`/`detail_open`/`menu_open`/`info_open`/
  `chapters_open`, `app.rs:329-334`) with one `enum Route { Home, Detail, Player }` + the modal
  flags, so update-dispatch becomes exclusive (today `home_update` runs every frame regardless,
  `app.rs:1151`). Draw-dispatch is already exclusive. *Checkpoint:* `make`.
- **7b** optional purist convergence: a single `ui::focus::FocusPath { screen, section, col,
  snap }` replacing home's `fr/fc/snapTarget` and detail's `SECTION/COL`; `Env` carries it;
  each screen's nav writes it; the `g_fr/g_fc/g_snap/set_*` shims (`app.rs:116-124`) re-point at
  it so the snap split-ownership (target set in `app.rs:588-594`, spring chased in home) and the
  headless dev-triggers keep resolving. **This is the only step that mutates app.rs-shared focus
  state** — if it destabilizes the snap edge-trigger or the harness, **stop at 7a**: home+detail
  are already full `View` structs on tokens+components by then (~90% of the win at a fraction of
  the risk). Preserve the raw LG-SDL byte-offset decode (`+16/+20/+24`) and webOS wcodes exactly
  — CLAUDE.md flags them as fragile. *Checkpoint:* `make` + capture + run the dev-trigger
  headless flows.

---

## (D) WHERE FULL VIEW MIGRATION IS **NOT** WORTH IT (stays immediate-mode)

Honest carve-outs — a `View` wrapper here breaks a load-bearing contract for no payoff (these are
about a specific contract, NOT about "it's only a throwaway" — that is never the reason):

1. **home `Backdrop`**: four elements fade on independent curves and `Painter::ambient` intentionally
   ignores the cascade alpha. Folding into an alpha'd subtree visibly breaks the hero fade — that
   contract is intact and is *why* the component is shaped the way it is.
   **AMENDED** — it is no longer *draw-only*. The per-element alphas are now **springs** (the two art
   reveals that cross-fade a backdrop in when it decodes, plus a whole `AmbientWash` for the page
   ground), and springs need a `View::update`, which `home_update` calls explicitly beside
   `grid.update`. The carve-out that stands is the one about the cascade: those alphas are still
   passed per element and must never be routed through `p.alpha()`, so the art and the wash keep
   fading on their own curves. `update` is also where each layer's texture is resolved once, so the
   draw cannot disagree with the springs about whether there is anything to reveal.
   **AMENDED AGAIN (`ui::nav`)** — "a wash cannot be alpha-faded at all" was true only of fading one
   ITEM's colours into ANOTHER'S, which is still twelve springs. Fading a wash toward the app's own
   GROUND is now the cascade's job: `Painter::ambient` mixes its corners toward `theme::SURFACE_APP`
   at an alpha below 1, which is what lets a whole page — `Backdrop` included — dip for the
   route-level page transition. The pixels it writes are still opaque; only the corner rgb moves.
2. ~~**detail absolute section layout**: keep the hard-coded Y table; no reflow engine.~~
   **SUPERSEDED** — detail is now a single computed vertical flow: `section_y()` stacks the present
   blocks' `block_h()` heights from `CONTENT_TOP` with one `SECTION_GAP` (tabs→episodes hug via
   `TAB_EP_GAP`), and both the draws and `scroll_target` read it. Block heights are content-derived
   (Related tracks `REL_H`=`CARD_H`), so resizing a block reflows everything below with no magic
   constants. The original carve-out was wrong — the manual `CAST_Y`/`ABOUT_Y` bump needed when
   Related grew was exactly the drift a flow removes.
3. **player surfaces** (`player_hud.rs` / `track_menu.rs` / `info_panel.rs` /
   `chapters_panel.rs`): out of scope for this migration. `player_hud`'s absolute `SCR_H-offset`
   geometry is a **shared pointer-hit-test contract** with `app.rs` (`icon_hit`/`sb_w`) — a tree
   rewrite would break the Magic-Remote pointer. `draw_clock` (`player_hud.rs:173`) and
   subtitles are bespoke and not `Label`-expressible. These already consume the component
   substrate; leave them immediate-mode. The player transport/scrub state machine (~12 `app.rs`
   locals) is a separate future effort.

---

## Tradeoffs (called out honestly)

- **Two token collapses are real, un-previewable visual changes.** 3-4 near-white primaries →
  one `TEXT_PRIMARY`; ~7 dim greys → `SECONDARY`/`TERTIARY`. Only on-TV capture shows the
  luminance nudge — hence the per-step capture gates. The alternative (a token per observed
  value) defeats the purpose.
- **Adopting ACCENT on detail is a design change, not a refactor.** Detail's hero-button focus
  and season tabs currently use an *inverted* cool-white treatment; steps 4d make them warm
  ACCENT to match the player. `ControlStyle::Custom` is the escape hatch if that reads wrong
  on-device.
- **The focus ring is shader-locked** (`gfx.rs:12`): cool-blue glow + white stroke, driven only
  by a `focus:f32` scalar. A token cannot retint it; home's pad-48 glow genuinely differs from
  `draw_card`'s pad-6. "Unify the ring" is a deliberate, param-guarded, deferred change — not a
  mechanical sweep.
- **The focused-last z-order can't live inside `Card::draw`** — the parent (`Grid`) must keep
  the two-pass overlay, so "Card is a View" is only half true; the parent still hand-orders.
- **`Painter` has no clip/scissor** (`mod.rs`): scrolling rows (`Card`/`HScrollRow`) still cull
  by index and hand-roll edge masks. Fine for the fixed 1080p target; a ceiling if overflowing rows
  ever need real edge-fades.
- **`CString` keep-alive:** every `Label`/`Button`/`TextBlock` holds a non-owning
  `*const c_char`; migrated home/detail call sites (which build Rust `String`s) must keep the
  `CString` in scope for the draw frame — no new hazard, but a per-site cost, already the
  `table.rs` pattern.
