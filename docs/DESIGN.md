# PlxNative design system

This is an index, not a specification. Every claim below points at the file that actually enforces
it, because those files are checked by the compiler, by the lint gate, or by a test, and prose is
not. If this file and a pointer disagree, the pointer wins and this file is stale.

The full design and migration status lives in [docs/ui-system-migration.md](docs/ui-system-migration.md).
The rules you must follow when writing UI code live in
[rust-modules/ui/src/CLAUDE.md](rust-modules/ui/src/CLAUDE.md). Read that before touching a screen.

## The surface

One fixed surface: 1920x1080, `ui::consts::SCR_W` / `SCR_H`. There is no responsive work here and no
second viewport. Input is a webOS remote, so focus is a first-class state and hover is not a
discovery mechanism. Text is rasterized by the TV's own text engine, so there is no font stack to
pick and no web typography to tune.

Distance changes what legible means. `CAPTION` 24 is the stated floor for ordinary content, and
that number is a couch measurement rather than a taste call.

## Colour is two layers

From `ui/CLAUDE.md` rule 1, enforced in `rust-modules/ui/src/theme.rs`:

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

From `ui/CLAUDE.md` rule 2, defined in `gfx/tokens.rs`'s `size` and named as `theme::size`. Nine rungs, and a size is a role rather than a
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

Alert titles and reading text share the left padding edge (`theme::alert::TEXT_ALIGN`). This
applies to both decision dialogs and read-only panels. Button labels and paired value columns
keep their control-specific alignment; centering the panel does not center its prose.

## Legibility over artwork is a graded contract, not a vibe

This is the part most easily broken by accident, so it is the part worth knowing before you change a
backdrop.

Catalog protects hero text with a shade that follows the content.
[`HeroShade`](../rust-modules/ui/src/hero_shade.rs) builds a feathered block from each piece of
drawn content, using its measured ink width. The blocks follow the title, copy and controls as
their positions change.

- Home uses one edge-anchored field that stays still through a carousel flip. It eases in from
  the upper left and fades toward the center of the artwork. The field joins the full-width
  shelf ramp with a soft maximum to keep the crossing from darkening like two overlays.
  The shelf ramp follows the first tile's live top and reaches `SHELF_FOOT_A` at the
  Continue Watching caption line. The whole field fades with the hero.
- Home's movie row shows type, year, runtime, up to two genres, quality, and the content rating.
  Episodes show their season and episode number, air date, runtime, quality, and rating. Shows
  use the series runtime or season count when runtime is missing, plus year, genres, and rating.
  Movies and shows read this band whether or not they have been played; only a season or a
  collection falls back to the first sentence of its summary.
  The Resume pill shows the play glyph, progress rail, and localized time remaining.
- Detail uses the same continuous renderer. Its wider left fade supports the synopsis and the
  full-width lower ramp supports the credits. The left fade shifts up with the preview logo and
  both strengths ease out for the full-screen trailer.
- The horizontal wedge and feathered corner that stood behind the heroes (`widgets::hero_scrim` and its
  closed forms) are deleted; the catalog page (`screens::catalog`), the last to draw them, builds a
  `HeroShade` from the rects it draws (the title, the copy under it, the pill) and lays it over its art
  as a scrim layer.

The shade scales by the screen's own hero fade, so it leaves with the hero instead of lingering over
the shelves.

**Anything that changes what sits behind hero text must re-grade the legibility tables**
(`screens/src/home/hero_shade_tests.rs` for Home, `widgets_hero_scrim_tests.rs` for Detail). Still artwork
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

Card rows come from `plx_ui::cards::Shelf` with a named `RowStyle`:

- `HOME` for poster shelves, and the single source of shelf motion
- `EPISODE` 420x236, gap 28, `focus_scale` 1.09, the 16:9 landscape still
- `STILL` is `EPISODE` at `theme::STILL_RADIUS` (12) and `theme::STILL_GAP` (40): the Continue Watching
  row's tile. The focused tile only pops (1.09, in place); no neighbour moves, so the spacing between
  tiles never changes (the pop grows a 420 px tile ~19 px a side, inside the 40 px gap). Its slot pitch
  is `STILL.w + gap` (460) and the row stands whole at `consts::PEEK_Y` + `consts::CARD_DY` (768) on the
  hero's page
- `CAST` 190 circles, gap 40, `focus_scale` 1.13
- `PROFILES` big circles, `focus_scale` 1.18

The focus pop values differ on purpose and each carries the reasoning at its definition: a lone row
of widely spaced circles needs a bigger pop than a tight poster shelf to read as selected at all.

### The Continue Watching still

A Continue Watching deck is ONE object, so it has one look wherever it appears: Home's row and the
Library > Recommended page's landscape Continue Watching row are both a row of `RowStyle::STILL` stills
with **no label block under them** (`captions::NOTHING_UNDER_STILLS`, band 0, one policy both screens
hand their shelf; Recommended's `Row::still_deck` picks it, `hub_style` the style and `shelf_pitch` the
pitch from it, `tile_facts::deck_overlay` draws the footer for both): everything a still says it says on
itself. The Recommended page's other landscape rows (Recently Released Episodes) are not decks and keep
the episode strip's `EPISODE` look, its caption band and `StillFooter::Shelf`.

- **Clean art, one footer line off it.** The deck's tile has no scrim (`card_named_led` skips
  `still_ground` for a clean tile); the footer is one `still_footer::FOOTER_LINE_H` (24 px) line UNDER
  the tile (`Place::Below`): its top `theme::STILL_FOOTER_DROP` (8 px) below the tile's foot, inset
  `FOOTER_BELOW_INSET` (4 px) each side, the text group and the mark at its right `FOOTER_GAP` (12 px)
  apart, standing on the backdrop with the shelf fade darkening it to near-black behind (no plate, no
  block). The Detail strip's footer keeps the old place in
  the art (`Place::InArt`): `widgets::art_scrim` darkens the bottom `STILL_SCRIM_H` (112 px), and the line
  stands `FOOTER_RISE` (12 px) above the card's bottom edge, inset `widgets::STILL_LINE_INSET` (16 px).
- **What the line says**: `S2 E4 · 38m left` and the amber bar `FOOTER_BAR_W` (100 px) wide,
  `FOOTER_BELOW_BAR_H` (6 px, radius 3) high under the art (`FOOTER_BAR_H`, 4 px, in it); an unstarted episode `S2 E1 · New episode` (the same `S2 E1` tag a started
  one wears, `core.footer_tag`, then the catalogue's `widgets.card.new_episode`) and no bar, since nothing is
  played yet. The fit rule is one pure function,
  `still_footer::footer_geometry`: the bar shrinks from 100 toward `FOOTER_BAR_MIN_W` (64 px) before
  anything is cut, and the tag fades only when even 64 px does not fit; the time unit is never cut. With no
  bar the tag is kept while the whole text fits the line, cue included; `New episode` is never cut or dropped
  (a test fits the worst tag, S12 E24, in every shipped language: all keep the tag, at rest and with the cue open).
- **The row's heading** has a small baseline rise and uses the same focus lift as the other shelves.
  `card_row::heading_lift_of` takes the larger of the two. The gap below the footer keeps the
  Continue Watching heading clear of the nav when the next shelf is focused, while
  `ShelfColumn::block_overrun` keeps that shelf's caption inside overscan.
- **The focus cue** is a white play glyph (OK plays, `DeckPress::Play`) or an info glyph (OK opens the
  page) at the left of the focused card's line, in a `FOOTER_CUE_SLOT` (28 px) that is collapsed at rest
  and opened by the card's own pop progress (`cards::Tile::pop`, never a timer of its own): the text
  moves right by exactly the slot and the bar shrinks toward 64. The Detail episode strip's footer has
  no cue (its OK always opens or plays the episode). **There is no amber triangle anywhere**: the old
  caption mark was removed from the code (`TileLabel::glyph` and its draw path), not only from the
  captions.
- **The art falls back cover -> baked composite -> title in type** (`card_art::pick`). Plex's own 16:9
  cover has the title baked in; our composite is the server's background with its clearLogo baked in
  once by the poster worker; with neither, the title is set in type at the logo slot
  (`card_art::needs_type_title`, drawn by `still_overlay` through `hero_logo::draw_title`). Baked card
  art is a bitmap drawn as an ordinary still, **never a live logo** (twenty cards would each hold a
  clearLogo texture and a per-frame blend). An opted-in card (`tile_facts::for_deck`) carries its title
  in its art, so the footer repeats none.
- **Home's page scroll.** Home rests at the hero's page or the grid's (`HomeScreen::on_grid_page`,
  `holds_page`): focus coming onto the Continue Watching row, by key, hover or click, scrolls nothing;
  Down from the row reaches the grid with the stills still on top, and the grid's shelves rest as every
  shelf does (`ui_cards::grid_rest_scroll`, `consts::GRID_TOP_Y`). The pointer's edge-glide bands are
  `app::input::EDGE_BAND` (= `consts::MARGIN_Y`, 54 px), and one wheel notch is one Up/Down key
  (`app::bridge::wheel_input`). **The billboard rotates only while focus is on the hero** (or on nothing):
  the 8 s countdown (`HERO_AUTO_S`) counts while the page rests at the hero and no shelf or shelf heading
  holds focus (`HomeScreen::row_holds_focus`), and it restarts at its full length when focus returns to the
  hero. A page that rests at the hero while a Continue Watching card is focused (the row scrolls nothing)
  therefore keeps its billboard still and presents nothing once settled, as it always did when focus on a
  row scrolled the page off the hero (the `home-idle` scene grades it).

Tile text is `TileLabel::title(t)` or `TileLabel::titled(t, caption)`, with the focused-label band
driven by `line_reveal_for` and `under_band` (a line shows once the room under its card holds its
box, for a shelf and a grid alike, so no caption line is drawn over the tile or heading under it as it is
drawn, pop and heading lift included). Row headings are `theme::size::HEADLINE` at the SHARED
shelf pitch, `consts::TITLE_DY + CARD_DY` (see `related.rs`, `cast.rs`, `extras.rs`) — the same
heading-to-card distance Home and the Library use, quoted as the sum rather than as a number this
file would then have to keep in step.

Menus over a page are glass on frozen ground (DS6: `popover_glass`, gradient face, 1 px rim, soft shadow, 30% scrim; the body shows the dim's latched field and the rim refracts the once-captured host snapshot, so nothing is re-blurred while a menu is up). Every other surface stands on the page's own latched light: `widgets::panel_ground` draws the
underlay field the modal dim latched, windowed to the panel's rect and graded toward the panel
material under its frost. Backdrop-blur glass (`Glass::DYNAMIC_BACKDROP`) is chrome-only — the top
bar's track and the profile chip — plus two non-interactive standing notes in the page layer: the "Hold OK for options"
capsule (`ui::hold_hint`: standing once per install on the card screens, and once on the Detail season pill) and the Library's
scroll capsule (`ui::scroll_capsule`: the library's name, size and sort ("Films 1,284 · Date viewed") once the chrome has tucked away on a page whose header names two
or more libraries; the nav pill's own glass, asked for by the owner in place of its frozen-ground form). The guard is the file
list in `widgets_glass_budget_tests`, not this paragraph. See `docs/liquid-glass.md`.

## Navigation says where BACK goes, once

Every Settings-family route draws a crumb above its title: a left chevron and the name of the place
BACK returns to. `RouteLayout::draw_narrative` takes it as `Option<&str>` so a new route cannot
forget to answer, and `None` means BACK leaves the app entirely.

This replaced "Press [BACK] to return" across the family. The old hint spent a 60 px band restating
a key the remote already has, and could not say where the key went, which on a three-deep push is
the only part anybody needs. Keep the BACK-return `KeyHint` on read-only ALERT panels, where
it supplies their dismissal affordance; do not repeat it beside a route's return crumb.

First-run consent sizes its narrative from the complete disclosure and the related-link column.
Its contextual answer verbs share one horizontal row, separated from BODY-sized reading copy by
`space::MD`. Exceptional overflow scrolls with UP/DOWN while the selected answer stays focused.

## Where to look when you are changing something

| Changing | Read first |
|---|---|
| any screen | `rust-modules/ui/src/CLAUDE.md`, then `docs/ui-system-migration.md` |
| adding a screen | `docs/ui-system-migration.md` section (E) |
| a colour | `rust-modules/ui/src/theme.rs` |
| a size | `rust-modules/gfx/src/gfx/tokens.rs` (`mod size`, re-exported as `theme::size`) |
| anything behind hero text | `rust-modules/ui/src/landing_hero.rs` (Home and Detail), `hero_shade.rs` (Catalog), `theme::shade` and `gfx/scrim.rs`, then re-grade `widgets_hero_shade_tests.rs` and `widgets_hero_scrim_tests.rs` |
| a horizontal row | `rust-modules/ui/src/card_row.rs` |
| playback UI | `rust-modules/media/src/player/CLAUDE.md` |
| Plex data feeding UI | `rust-modules/plex/src/plex/CLAUDE.md` and `docs/pms-api.md` |
| known gaps against official clients | `docs/parity-gaps.md` |
