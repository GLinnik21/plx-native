# PlxNative design system

This is an index, not a specification. Every claim below points at the file that actually enforces
it, because those files are checked by the compiler, by the lint gate, or by a test, and prose is
not. If this file and a pointer disagree, the pointer wins and this file is stale.

The full design and migration status lives in [docs/ui-system-migration.md](docs/ui-system-migration.md).
The rules you must follow when writing UI code live in
[rust-modules/src/ui/CLAUDE.md](rust-modules/src/ui/CLAUDE.md). Read that before touching a screen.

## The surface

One fixed surface: 1920x1080, `ui::consts::SCR_W` / `SCR_H`. There is no responsive work here and no
second viewport. Input is a webOS remote, so focus is a first-class state and hover is not a
discovery mechanism. Text is rasterized by the TV's own text engine, so there is no font stack to
pick and no web typography to tune.

Distance changes what legible means. `CAPTION` 24 is the stated floor for ordinary content, and
that number is a couch measurement rather than a taste call.

## Colour is two layers

From `ui/CLAUDE.md` rule 1, enforced in `rust-modules/src/ui/theme.rs`:

- A **role** says what a colour is for: `theme::TEXT_PRIMARY`, `theme::CONTROL_IDLE_FILL`,
  `theme::scrim(a)`. Roles are what call sites use.
- A **primitive** is a stop on the palette, private to `theme.rs`, added as an exact 8-bit code via
  `rgb8` and only when the palette genuinely has no such shade.
- Two roles landing on the same stop stay two roles. `ACCENT` and `TEXT_PRIMARY` are both Cool 0,
  and retuning the focus fill must not restyle every title on screen.
- Overlays are `with_a(WHITE, a)` on the measured alpha ramp. A new overlay is a weight, never a new
  hue.

No raw colour literal, anywhere. Need a shade that does not exist? Add a role with a doc line saying
what job it does. The two layers mirror the external PlxNative Design System project
(`tokens/primitives.css` plus `tokens/colors.css`), so a palette decision is one edit in each place.

## Size is a scale

From `ui/CLAUDE.md` rule 2, defined in `theme::size`. Nine rungs, and a size is a role rather than a
number:

| Rung | px | What it is for |
|---|---|---|
| `HERO` | 72 | the largest display type |
| `DISPLAY` | 48 | |
| `TITLE` | 40 | |
| `HEADLINE` | 32 | row and section headings |
| `BODY` | 28 | the hero meta line |
| `LABEL` | 26 | reading copy, including the hero synopsis via `ui::hero_synopsis` |
| `CAPTION` | 24 | the couch legibility floor for ordinary product content |
| `MICRO` | 22 | one-line de-emphasized labels only, never content |
| `DIAGNOSTIC` | 20 | `app::diagnostics` only, never chrome and never prose |

Exactly two carve-outs live outside the scale, both named and commented at their call site: the
player HUD's display title (`HUD_TITLE_SZ`) and the subtitle caption. Do not add a third.

Text is never hand-placed with a magic y. Use `ui::label::Label` for a single run,
`ui::text_view::TextView` for wrapped multi-line, or derive from `text::text_vcenter_y` /
`text::text_cap_band`. See `label.rs` for why: a guessed offset mis-centers the moment a string has
a descender.

## Legibility over artwork is a graded contract, not a vibe

This is the part most easily broken by accident, so it is the part worth knowing before you change a
backdrop.

Hero text is protected by a two-dimensional field, and the arithmetic is deliberately pure so a test
can grade it. `widgets.rs`'s `hero_scrim_a` doc says it plainly: the contract "is graded on it: this
is the arithmetic the anchor table in this module's tests reads, so the promise and the paint cannot
come from two different curves."

- Vertical ramp, [rust-modules/src/ui/landing_hero.rs](rust-modules/src/ui/landing_hero.rs):
  zero above `HERO_BASE_SCRIM_Y0` (0.34 of the screen, 367), `foot * MID_WEIGHT` at `KNEE_Y` (0.65
  of the screen, 702), and `foot = 0.30 + 0.64 * hero_a` at the bottom. The text stack builds upward
  from `TEXT_BOTTOM` 692 in a column of `COL_W` 660.
- Horizontal wedge plus a feathered bottom-right corner, `widgets.rs`: `hero_scrim_a` and
  `hero_scrim_right_a`, painted as three quads whose quad 0 to quad 1 seam is host-gradeable because
  it is the one structural bug the component can have.

Both halves scale by the screen's own hero fade, so the field leaves with the hero instead of
lingering over the shelves.

**Anything that changes what sits behind hero text must re-grade the anchor table.** Still artwork
has a knowable worst case, which is what the PMS `blur` hash is for. Video does not.

For text sitting directly on a piece of artwork, the answer is `art_scrim`: `STILL_SCRIM_H` 112 px
(or `STILL_SCRIM_H_1` 88 for one line) of near-black at `STILL_SCRIM_A` 0.78, fading upward and
clipped to the card's own rounded silhouette. Its doc records the decision and the reason: "a plain
label over an arbitrary video frame is a coin flip for legibility, and a capsule per label was the
alternative the design deliberately drops." Do not reintroduce per-label capsules or chips.

When the backdrop is something GL cannot read at all, controls use `ControlGround::Unkeyed`.

## Components, not per-screen draw code

`ui/` is the design system and screens are compositions of it. The table in `ui/CLAUDE.md` has a row
for every module, so a missing row means the table is stale.

Improve a component before forking one. If a shared widget almost fits, add a builder method or a
style variant. A genuinely new widget still lands in `ui/` as a reusable `View` so the next screen
gets it free.

Card rows come from `ui::card_row` with a named `RowStyle`:

- `HOME` for poster shelves, and the single source of shelf motion
- `EPISODE` 420x236, gap 28, `focus_scale` 1.09, the 16:9 landscape still
- `CAST` 190 circles, gap 40, `focus_scale` 1.13
- `PROFILES` big circles, `focus_scale` 1.18

The focus pop values differ on purpose and each carries the reasoning at its definition: a lone row
of widely spaced circles needs a bigger pop than a tight poster shelf to read as selected at all.

Tile text is `TileLabel::title(t)` or `TileLabel::titled(t, caption)`, with the focused-label band
driven by `band_reveal` and `under_band`. Row headings are `theme::size::HEADLINE` at the SHARED
shelf pitch, `consts::TITLE_DY + CARD_DY` (see `related.rs`, `cast.rs`, `extras.rs`) — the same
heading-to-card distance Home and the Library use, quoted as the sum rather than as a number this
file would then have to keep in step.

Surfaces over a page use the `Glass` policy, which has exactly one axis: how often the backdrop
snapshot refreshes. `Glass::CACHED` is the default, capture once on open. A surface over a moving
page opts into `Glass::DYNAMIC_BACKDROP`.

## Navigation says where BACK goes, once

Every Settings-family route draws a crumb above its title: a left chevron and the name of the place
BACK returns to. `RouteLayout::draw_narrative` takes it as `Option<&str>` so a new route cannot
forget to answer, and `None` means BACK leaves the app entirely.

This replaced "Press [BACK] to return" across the family. The old hint spent a 60 px band restating
a key the remote already has, and could not say where the key went, which on a three-deep push is
the only part anybody needs. `KeyHint` survives only on the read-only ALERT panels that hold no
control at all, where the line really is the whole affordance.

## Where to look when you are changing something

| Changing | Read first |
|---|---|
| any screen | `rust-modules/src/ui/CLAUDE.md`, then `docs/ui-system-migration.md` |
| adding a screen | `docs/ui-system-migration.md` section (E) |
| a colour or a size | `rust-modules/src/ui/theme.rs` |
| anything behind hero text | `rust-modules/src/ui/landing_hero.rs` and `widgets.rs`'s scrim section, then re-grade the anchor table |
| a horizontal row | `rust-modules/src/ui/card_row.rs` |
| playback UI | `rust-modules/src/player/CLAUDE.md` |
| Plex data feeding UI | `rust-modules/src/plex/CLAUDE.md` and `docs/pms-api.md` |
| known gaps against official clients | `docs/parity-gaps.md` |
