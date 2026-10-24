# ui/ — the shared UI system (read before touching any screen)

This directory is the workspace crate `plx_ui` (`rust-modules/ui/`, `rust-modules/ui/src/` here):
the application names its items as `plx_ui::theme`, `plx_ui::widgets`, ... (it was `crate::ui::...`
before Split 6, `docs/module-layers.md`). `lib.rs` is what was `mod.rs`.

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
   `theme.rs` is **two layers**, so "add a token" has two halves: a new token is a **role** (a job:
   what is this colour *for*) and it resolves to a **primitive** (a stop on the palette, private to
   that module) — reach for an existing stop first, and only add one when the palette genuinely has
   no such shade, as an exact 8-bit code via `rgb8`. Two roles landing on one stop is expected and
   stays two roles (`ACCENT` and `TEXT_PRIMARY` are both Cool 0): retuning the focus fill must not
   restyle every title on screen. Overlays are `with_a(WHITE, a)` on the measured alpha ramp — a new
   overlay is a weight, never a new hue. The layering mirrors the `PlxNative Design System` project
   (`tokens/primitives.css` + `tokens/colors.css`), so a palette decision is one edit in each.

2. **Never write a raw text size — every text size in the UI is a `theme::size` token.** The named
   rungs are `size::HERO` 72 / `size::DISPLAY` 48 / `size::TITLE` 40 / `size::HEADLINE` 32 /
   `size::BODY` 28 / `size::LABEL` 26 / `size::CAPTION` 24 / `size::MICRO` 22 /
   `size::DIAGNOSTIC` 20 — the *size* axis of the design system.
   `CAPTION` (24) is the **couch legibility floor for ordinary product content**; `MICRO` exists
   solely for one-line de-emphasized labels — an episode's air date, a rating row's provider
   caption, a track's file path in the tracks panel — never for content. `DIAGNOSTIC` belongs only to the
   deliberately opened `app::diagnostics` engineering instrument: its fixed schema and complete values
   matter more than couch-copy scale, and it must never migrate into chrome or prose. The hero
   SYNOPSIS is the case that proves
   the rule and it is no longer here: a blurb is the longest run of prose on its page, so it is
   reading copy, and both heroes now draw it at `LABEL` 26 through `ui::hero_synopsis`. (The hero
   META line this parenthetical used to name is `BODY` 28 and always was.) Pass a rung to `Painter::text`/`Label`/`TextView`/`text::elide` instead of a bare
   `24`/`28`/… — a size is a role, not a magic number; pick the nearest rung, and if no rung fits a
   genuinely new role, **add a documented rung to `pub mod size` in `gfx/tokens.rs`** (`theme::size`
   re-exports it, so call sites still spell `theme::size::X`), don't inline a literal. Exactly two
   carve-outs live outside the scale (the player-HUD display title `HUD_TITLE_SZ` and the subtitle
   caption); both are **named + commented at their call site**, never bare literals — don't add a
   third; new roles go on the scale. `anim.rs` is a dev-diagnostic overlay, not chrome.

3. **Never hand-place text with a magic y.** `y - sz*0.58` guesses are banned — they mis-center the
   moment a string has a descender (g j y p). Text is positioned by its **cap band** (layout ≠
   paint): use `ui::label::Label` (single run) or `ui::text_view::TextView` (multi-line, pixel-
   wrapped), or if you must call `Painter::text` directly, derive the y from `text::text_vcenter_y` /
   `text::text_cap_band`. See `label.rs`'s module docs for the rule.
   Tracking (DS8): a string that needs letter-spacing goes through `Painter::text_tracked` with a
   named constant (`theme::TRACKING_TITLE_TIGHT`, -0.5 px, the DS8 title tracking the Watchlist header wore before it took the Library's header row); at `0.0` it is
   exactly `text`. The gaps are n-1 between characters (CSS adds one after the last too, a
   sub-pixel difference shipped code keeps), and no screen adopts it yet.

4. **Improve a component before forking one, and never write an ad-hoc mechanism.** If a shared
   widget almost does what you need, add a builder method / style variant to it (e.g.
   `Button::style(ControlStyle)`), don't copy it. A new bespoke widget is only justified when
   nothing here is close — and then it lands *here*, as a reusable `View`, so the next screen gets
   it for free.

   A mechanism is forked the same way and more quietly, because it looks like three lines of glue.
   Find the ready-made one first; if it almost fits, extend it in place for every caller; if it
   truly cannot do the job, say why before writing a new one, which then lands here. PR #843
   hand-routed the Up key in a screen and gave its header pills no motion while all of these existed:

   - **Focus travel between regions** (nav, header, content, rail): `FocusEngine` (`focus.rs`) over
     the owner's `Focusable` contract (`screen.rs`: `neighbour`, `Link`, `EdgeRule`, `Seat`). A
     screen declares its groups and edges; it never matches a direction key to move focus between
     regions.
   - **A row of pills** (selection and focus capsules that travel): `widgets::TabStrip` and
     `SelMark` (`widgets.rs`), the nav bar's own motion.
   - **Menus**: `Popover` (`popover.rs`) around a `TableView` (`table.rs`). **Strips and grids of
     tiles**: `cards::Shelf` / `cards::Grid` (`cards/`, next section).
   - **A row of items an equal gap apart** (a header row, a crumb, a name packer): `hstack::HStack`
     (`hstack.rs`), UIStackView's spacing for widths the caller already measured. It places, sums
     (`width`) and budgets (`budget_w`); it never measures, so a replay gains no `Width` key, and a
     slot sizes the budget, never the air. A run whose items are not in a slice (their count is the
     data's, or each is measured as it is painted, or it stops lazily at the first item that will not
     fit) uses `hstack::Flow`: the same gap rule, placing while measuring/painting
     (`push`/`place`), `next_x` for the lazy break. Never write `x += w + GAP` for a new row.
     The same stack top to bottom is `vstack::VStack`/`VFlow` (`Anchor::{Top,Bottom,Centre}`, `Item::before` for a
     gap that belongs to the item below it, `AlertHead` for the alert family's eyebrow/title/path ladder); never `y += h + GAP`.
   - **Springs**: `Spring` (`lib.rs`) stepped at the shared stiffness (`consts::K_SCALE`,
     `K_SCROLL`, `K_SNAP`; `popover::K_APPEAR`), not a new constant or a hand-made easing.

## Card shelves and grids: `cards::Shelf` / `cards::Grid`

A new screen that shows a strip or grid of media tiles uses `plx_ui::cards::{Shelf, Grid}` and
implements `CardSource` for its content; it does not assemble `CardRow`, `GridPop` or `GridBands`
itself. `card_row` and `poster_grid` are `pub(crate)`: what a screen's own layout reads from them
(`RowStyle`, `TileLabel`, `under_band`, `reveal`, the heading painters, `GRID_STYLE`...) is
re-exported from `plx_ui::cards`, and the profile picker's circular avatar row is
`cards::AvatarRow`. The `cards` gate in `ci/check-deps.sh` is the backstop (`ci/allow/cards.txt` is
empty).
The component reads focus from the engine, adopts a focus no deliberate move announced at full pop,
takes the press from the frame, registers the stops it draws and reports one `CardEvent`
(Activate / Hold / Want) from `on(ev, cx, src, fx)`, which the screen calls with EVERY event. (Three screens are exceptions: Home feeds Tick and FocusMoved only and keeps its own press paths, because a Continue Watching press is account-affecting; Library and Detail feed every event but discard the returned `CardEvent` and handle Activate / PressCommit / PressHold themselves.) The
contract is the `//!` of `cards/mod.rs`; `cards/tests.rs` is its Tier 1 conformance, and
`cards/conformance.rs` the Tier 2 drivers each real screen runs. Three rules a screen can break by
reaching around the component: a `SectionFrame`'s `y` (and a `Grid`'s `top`) is SCREEN space and
`draw` undoes the painter's own translate, so hand it the painter you have, never a pre-offset
one; a content landing that moves the focused element's index is the component's to absorb (the
pop and the scroll follow the element, `Shelf::landed` / `Grid::landed` report it, at any distance
and whether or not a key, a restore or a reconcile moved focus in the same tick: `pool::slid`) so the
screen does not re-seat anything (a scroll TARGET it sets against content that already landed is held
in the scroll's own coordinates, `Grid::pending_shift`); and read a shelf through `heading_lift()` / `under_band()`, not through
the L0 `CardRow`.

The `Grid` asks for its FOCUSED card's art first even though that card paints last (over its
neighbours): the source serves the newest frame's requests in the order they were made, so a seated
first poster used to arrive after every sibling (`card_row::lead_focused` resolves it once, before
the others, and `draw_focused_led` draws with that answer; one `card_motion` occurrence per card per
walk, never a second `resolve_card_art`). A `Shelf` still asks in paint order.

A `Shelf` built with `animate_landings(true)` (Home's Continue Watching row) PLAYS a landing that no
move announced instead of cutting it (`cards/landing.rs`): a removed card fades out as a ghost, named by its
element (`ghost_keys()`) so the owner keeps only the cards that left (`Arc` clones, while `landing_pending()`) and paints them
with `paint_ghosts`; survivors slide, new cards fade in, each on its own alpha. The focused tile is held
at its slot by the landing rule (its offset is 0), but the scroll is clamped to its range, so at an end
of the strip even it can move on screen. There is no cap on the cards diffed (a map, not a scan); only cards
on-axis at either end get a spring, a fade or a ghost, the rest cut. A slide of the window the source holds
(`global` / `page_epoch` moved) is adopted whole. These springs are layout motion, so a landing seen while the page
is a held transition image starts at once (the first live frame shows the start state) and steps once the
image is released (`containers::transition::page_image_held`). Off by default. A `Grid` built from a `GridSpec` with `.animate_landings(true)` plays the same landing over 2-D slots (`Scene::slot` is index -> (x, y), the scroll shift is an (x, y) pair): a removed card is a ghost (`ghost_keys()`, `paint_ghosts`) that fades in its cell on `style.k_scale`, easing from its lifted scale when it was the focused card; every card that keeps its row slides to its new cell on `style.k_scroll`, ALL on one frame (no stagger); a card that crosses a line break never flies over the row between: it crossfades on `style.k_scale`, a leaving copy (`Landing::leaving`, painted by `Grid::draw` under the sliding cards; a new card or a row-changer fading in is drawn after the still cards and before every mover, `DrawRole::Arriving`, and a new card begins to fade once the cards in flight cover less than half its cell: `Occupancy`) fading out where it stood while the card fades in at its new cell, `out = 1 - in` (a card that only moves with the scroll shift keeps the 2-D spring); the card focus reconciles to after the focused card left rises from rest (`GridPop::rise`) as the ghost sinks, so two lifted tiles never stand side by side (the app reconciles a frame late: until it does the pop belongs to no card, `GridPop::vacate`); the focused card's ghost keeps `Tile::focused`, and when a hold menu removed it the opener lift carries it above the dim while the menu fades (`Screen::opener_departing`, `DrawFrame::lifting`; the page pass leaves it out), so it never drops into the dim on the OK frame; and nothing of a landing steps while a menu veils the page (`containers::modal::page_veiled`: a surface opening, open, or closing above `CLOSING_GONE`, i.e. still holding the host, stated by the dispatcher before the pages tick; the springs start on the first frame after, no beat), and the app's Watchlist feed hands the page nothing either (the Remove's tile commits on that same frame), and while veiled it reports no motion, so the menu's fade-out stays on its cached host, and until the landing steps NOTHING on the grid changes (slots, scales, the new focus card's rest, the caption band under the focused row, the title the focus caption reads (the removed card's ghost keeps it, and it reads the next card's on the landing's first moving frame, the frame the owner's count follows), the owner's count: the vacated pop draws the reconciled card at rest, `Grid::landing_waiting` is the owner's cue); the takeover rising from rest into the focus pop as the ghost sinks is the INTENDED look (design Motion board "takeover" spring), not a departure from the board; a card with neither end on screen snaps (no cap: a map, not a scan); focus lands on the card that took the removed card's index, the next card (the engine's reconcile; `conformance::removal_focuses_next`). Slots are measured under the caption bands as they stand NOW, but where a card stood is measured under the bands the last frame drew (`Scene::was`; a focus the reader did not move settles the caption band on its new row at once, and the layout that moves with it plays on the landing's springs, the caption with its card: `Landing::rebase` does the same for a band that settles while a landing plays), and the page shift a landing needs (`landed_shift`) is carried into the ghost, which stays where it stood on screen. The owner must keep the removed card for the ghost, as Home does for the shelf.

A row that goes on past an edge (`CardSource::more_after`, `more()` unless the source overrides it, / `more_before`, `false`
unless the source says its window slid off the start: Home, Library and Search do) paints the skeleton of an unloaded poster in the cells past that edge, a screenful at
most, so a row still loading does not look finished (`Shelf::paint_pending`). They are cells of the
same grid: no stop, no pool entry, no request, no caption, so focus never lands on one and the
page that lands takes them over in place. Gate `more_after` on nothing focus-dependent (Home's
`more()` is gated on focus being near the edge, because that is when it asks; its `more_after` is not).
A skeleton in view is a promise the row keeps: `Shelf::page_ask` asks for the trailing page while
focus is elsewhere whenever the cell past the last card falls within the screen's width at the row's
scroll (`pending_in_view`; horizontal only, so a short Detail Related row below the fold asks on
the page's open too), so a row
shorter than the screen — Detail's Related head, cut to a card or two when a member movie's
collection takes its previews — does not stand there as a screenful of skeletons until somebody
focuses it. A row whose `more()` is gated on focus (Home, the Library) emits no `Want` unfocused and
so still asks only from focus; Detail's Related and Search's rows are not gated.

`GridSpec::new(top, edge)` is the six-column `cards::GRID_STYLE` grid from `MARGIN_X`;
`.columns(cols, style, left)` sets the column count, card style and left edge (a rail-aware or
episode grid), and `.external()` selects `ScrollMode::External`, for a grid inside a document whose page scroll
another owner drives: the Library's grid runs under the Library's scroll and every grid section
of a `cards::Stack` under the stack's. Both shipped grids are external; `ScrollMode::Own` is the
default a spec is built with and no screen runs it, so a scroll fault on a stack page is in
`cards/stack.rs`, not in `Grid`. An external grid never steps a scroll: the owner calls
`set_page(top, scroll)` before each
`on` / `draw` / `place`, may read the scroll the focused row wants from `reveal_target()` for a
snapping grid (`GridSpec::new`; a grid built with `GridSpec::resting` is read through
`rest_target(end)` instead, because `reveal_target()` has no document end in it and `Stack::wanted`
passes its own; the Library applies the same rule through `Layout::row_reveal` ->
`poster_grid::rest_scroll`, and Home's shelf grid through `home::rest_scroll` -> `cards::grid_rest_scroll`,
so the focused shelf rests with its heading at `GRID_TOP_Y - TITLE_DY` whatever the Under posters band
and whichever way focus arrived) and applies the document
shift a content landing needs, `landed_shift()`, to its scroll and scroll target; the grid does
not home when focus is elsewhere. An owner whose columns, style or document top change at run time
(the Library's episode listing, its shelf run) hands them over with `set_columns` / `set_page`;
`settle_band` adopts a restored focus row's caption band before the next tick so a layout built in
between sizes the document from it.

## Localization and shared reading layout

App-owned text comes from typed `i18n::msg` accessors. Keep protocol identifiers and server-returned
metadata untouched. Measure translated labels with the injected `Measure`; complete questions must
remain inside their safe frames at the shared theme sizes. Tile labels are the exception: they
belong to their card, never to the safe area. `card_row::anchor_label` (the pure decision;
`place_label` feeds it the card's edges and the screen's room) places a focused tile's block
**centred** on its card while the centred block sits on the panel; where it would not, **leading**
(every line starts at the card's left edge) while the room to the right shows the whole block; and
where the room to the right is a sliver (the last card of a row) **trailing** — the window ends on
the card's own right edge and reaches LEFT over the room the neighbours leave (never past the row's
`margin_x`), and every line is right-aligned to that edge, so a caption does not run off the card
into the margin. Only a trailing block may be wider than `under_budget` (it is given what it
wants, up to that room); a block that still does not fit glides inside its window, in lockstep
through the one `marquee::Block`. A label that names a tile whose face cut the name (the
Collections page) is held to its tile instead: `TileLabel::within_tile` gives the block a
tile-wide window (never the neighbours' room) and the run dissolves at BOTH of the window's edges
over `card_row::LABEL_EDGE_FADE` px (`Painter::text_fade_edges`: out into the right edge, in from the
left edge once the run has glided past it, a band that grows with the travel and is none at rest)
as the shared marquee glides it. All of it is judged where the tile will REST
(`CardRow::settle_lag`, passed through `TileLabel::settling`), so a scroll glide never flips the
block's alignment or width mid-flight. A headshot's credit label is not placed this way: it is a
box inside its own slot (`draw_credit_label`), which cannot cross the margin, and it never moves.

**Focused text that does not fit marquees; it is never left ending in an ellipsis.** The scope is
every line of a FOCUSED card's label block: a poster's title AND its caption line (year, character
or credit role, "Show · S1 · E4", a server handle — every shelf, the Library and Collection grids,
Person credits and Search draw it through `card_row::draw_label_block`), and a headshot's name and
role. All go through the one `marquee::Marquee::glide_in`. A headshot's label (a cast shelf names
EVERY tile, focused or not) is `card_row::draw_credit_label`: a name line and ONE role line on the
same two baselines in every state — never a role wrapped to two lines at rest and one clipped line
on focus, which made the block change height and jump with focus. Unfocused, a line that does not
fit ends in an ellipsis and stays still; focused, the same lines glide. The
lines of one block are ONE `marquee::Block`: one clock, one cycle (the longest overflowing line's),
so title and sub-line leave their rest beat together and loop together; a line that fits stays
plain and still, and the shared clock is released only when no line overflows. An UNFOCUSED card
draws no sub-line at all (the block belongs to the focus), and the persistent on-artwork lines
(`widgets::poster_label`, the still overlay's show and address) stay elided: they are on every card
at once and are part of the art, not of the focus. A new focused sub-line on a card goes through
`TileLabel::caption` or a `marquee::Block`, never a screen-local clip-and-offset.

Alert titles, eyebrows and reading text use `theme::alert::TEXT_ALIGN`: the common left padding
edge. Decision dialogs follow the same rule as the About, biography and track-information panels.
Button labels and paired trailing values retain their own control/column alignment.

`RouteLayout::screen_for_title` measures translated titles; `screen_for_reading` allocates space
for complete first-run consent disclosures and one row of concise answers. Exceptional overflow
uses the event-time measured `DocumentReader` bounds while the answer retains visible focus.
Do not add a separate reader outline or keycap hint. Route crumbs already name the BACK destination;
read-only alert panels retain their dismissal hint. Contribution caption and URL lines are centered
on the QR allocation.

**Text fit is a rule about who owns the text, and it is tested, not eyeballed.** App-owned text
(`i18n::msg`, `plex::languages` names, `Quality::label`) never elides in any shipped language
(`i18n::SHIPPED`, generated from `locales/languages.json`; a new language is a catalog directory and one registry line, `docs/localization.md#adding-a-language`) at the shipped sizes; server- and user-owned text (machine names,
handles, profile names, titles) may. `ui::table` types that at the call site with
`Origin { App, Server }` per text slot, defaulting to App; `Row::server_label`/`server_detail`/
`server_value` and `Section::server_header`/`server_accessory` mark a slot Server. There is no
string classifier — a fallback the app itself invents ("Plex server") is App and must not be
marked. What a row does when its text does not fit is decided by its role:

| role | app text that doesn't fit | server text |
|---|---|---|
| trailing value | a `*.short` readout key (`settings.playback.forced.short`, `forced_readout`); the picker keeps the long string. Never stacked into the grey sub-line: it would read as a description | elides |
| label, detail, header | rewrite the translation shorter (all three languages say the same thing) | elides; the FOCUSED row's label scrolls instead (`marquee::ROW`) |
| prose (alerts, consent) | wraps | wraps |

`ui::fit::two_runs` is THE declared priority for a line with two runs: the primary is guaranteed
its natural width up to its share of the span, the secondary always keeps the rest (never zero
unless the span is tiny), and when both fit the secondary takes exactly its natural width. Rows
use it (label primary, `ROW_PRIMARY_SHARE`, hugged by `HUG_MARGIN`); so do section headers against
their accessory (uppercased header primary, accessory pre-capped at `ACCESSORY_W`). Sequential
flows, drop cascades and single-run elisions (`card_row` headings, hero facts, track names) are
deliberately not migrated — see the note at each site.

`TableView::fit_report(frame_w, &Measure, HEADROOM) -> Vec<FitIssue>` (test-only) resolves every
label, detail, value, header and accessory column and reports what would end in an ellipsis. Label
and Detail use the draw's own column, shrunk by the headroom; Value, Header and Accessory are
re-resolved at natural widths inflated by `1/HEADROOM`, since a fitting secondary sits at exactly
its natural width. A `Section::accessory_app_prefix` declares the app-owned lead of a Server
accessory (`Not reachable ·` before a handle) so it is still checked. Tests call
`TableView::app_fit_failures(frame_w, tag)`, which runs it with
`fontcov::advances::ShippedMeasure` (whole-pixel advances, the way the device sums them; the
simulator's fractional advances hide overflows) and keeps the `Origin::App` findings, then
`assert_no_fit_failures`.

Adaptive text size or kerning is not used to make text fit. Type sits on named size rungs, the
CAPTION 24 rung is the legibility floor at TV distance, and every extra size costs another glyph
cache. A string that does not fit is rewritten or given a short key.

**Known unchecked surfaces** (no `fit_report` covers them yet): action pills, crumbs and tabs in
`app/chrome.rs`, the player HUD, `up_next`, chapters (all now in the `plx_appkit` crate, `rust-modules/appkit/src/`), and card-row names such as profile names.
Do not read their absence from a test as a pass.

## The architecture (restructure spec v4)

The owner's goal, verbatim from the spec: *"the application's behaviour is described by several
linked, DETERMINISTIC state machines — session, navigation, screen instances, playback — each with
an explicit owner, interacting only through typed events, never by writing another machine's
state."* Phases 0–12 have landed. The full design record is
[`docs/ui-restructure-spec-v4.md`](../../../docs/ui-restructure-spec-v4.md); `lib.rs` cites it by
section number on nearly every `mod` line, so `// RESTRUCTURE (spec §6.2)` is a live reference you
can follow.

**The layer rule (§2.1), gated by `ci/check-deps.sh`.** This is why the screens are not here:

| layer | may name |
|---|---|
| `ui/` — the LIBRARY | `plx_gfx::{gfx,text}` plus `plx_platform::i18n` and `plx_base::{paths,task,tile}` — **never an application type**. Except the `plx_machine` crate (`machine`, `present`, `idle`, `landgate`, `landing`, `motion`; `rust-modules/machine/`), which `ci/module-layers.ini` places BELOW the library (`docs/module-layers.md`): it names only `plx_base` and itself, never `gfx`/`text`/`i18n` or anything in `ui/` |
| `appkit/` — widgets several screens share (`player_hud`, `track_menu`, `more_menu`, `info_panel`, `up_next`, `chapters_panel`, `timing_capsule`, `skip_pill`, `source_list`) | `ui/`, `stores/`, `plex/` types, `player/`, `route/` — never `screens/` or `app/`. They are compositions of `ui/` components over application types; they cannot live under `screens/` because the `sibling` gate forbids one screen family naming another |
| `screens/` — the application's screens | `ui/`, `appkit/`, `stores/`, `plex/` types, `player/` — never a sibling screen |
| `stores/` | data crates and `plx_machine::machine` only — never `screens/` |
| `app/` | everything |

`ci/check-module-cycle.py` holds which top-level modules sit on the crate's module cycle, not the
references between them: a module outside the cycle (or a new one) that gains a path into it fails
with `file:line` (see "Module-cycle ratchet" in `docs/agent-reference.md`). A `ui` ->
`screens`/`app` reference no longer compiles (`plx_ui` cannot see the application, and a `gfx`/`text` -> `ui` one cannot either: they are `plx_gfx`); between application modules already on the cycle another upward reference still passes that
gate, so `ci/check-module-layers.py`, `ci/check-deps.sh` and review are what stop it; a green `module-cycle: ok` is not permission.

`ui/` is generic over one application bundle and is compiled and tested against `FixtureHost`
with no Plex type in scope (`fixture.rs`). If you find yourself reaching for `crate::plex` or
`crate::browse` from a file in this directory, the design says the code belongs in `screens/`.

A widget that needs a fact about an application row takes it as a plain value the caller fills, and
does not name the row. A catalog row reaches the poster and still tiles, their captions and the
page wash as `ui::tile::TileFacts`, built by `screens::registry::tile_facts::of` (the resume rule,
the composite-thumb test and the kind table stay with the layer that owns them and arrive already
answered); a server is the raw `u16` id `ui::tex` already takes (`ServerId::raw()`); a review score's
units are `fmt::RatingScale`. Never add an `impl ui::Trait for pms::Type` in a third layer to get
around this: after the crate split that impl has neither its trait nor its type in its own crate,
which the orphan rule forbids.

**One owner per state, event and resource (§2.2).** `App` owns a containment tree — never
references: `Session` (auth, profile, the one `ProfileScope`), `Consent`, `Input` (press, key-repeat
edges, pointer visibility, the double-buffered hit map, and the `FocusEngine` that holds THE
current focus — screens keep no copy), `Present` (the gate), `Navigation` (the container tree and
every entry/instance), `Player`, `Stores`, `Adapters`. Ownerships that used to be double are now
single: the press belongs to `Input`, the modal phase to `ModalStack`, the present gate to
`Present`, `ProfileScope` to `Session`.

**Navigation is a model, not a route word (§6.2).** `containers/` is the fold:
TabContainer → NavStack → ModalStack. A screen is an owned INSTANCE with logical state inside and
render resources created lazily. `input_owner()` resolves over the shared ModalStack first, then
the top page's own stack, then the page.

**One frame algorithm (§3.3, `dispatch.rs`)** — ten steps, and `frame/` expands only steps 8–10:
ls2_pump → ingest → adapter results in a deterministic `(completion_frame, adapter_rank,
arrival_index)` order → one `Tick` broadcast containers-before-pages → expired timers → a bounded
drain (`MAX_STEPS_PRE = 192`) → **exactly one nav commit per frame**, with `MAX_STEPS_POST = 64`
reserved so a key that opens a page still mounts it in the same frame → present decision →
prepare under the `frame::Budget` → draw. Nothing is ever dropped from that queue; an overrun is
CARRIED to the next frame and rides the heartbeat as `carried=`.

**Frames must not wait on storage or synchronous LS2 calls.** The app loop, bridge and dispatcher
enter `task::FrameScope`; `task::assert_may_block` guards session I/O, helper transactions,
Keymanager and LS2 calls. Tests panic (catchably). In developer builds (`threadcheck`), an unallowed call logs
`main-thread block: <label> at <file:line> (fatal; aborting)`, then aborts on the frame thread before the call runs.
Release builds retain elapsed-time logging once per (label, call site):
`main-thread block: <label> <ms>ms at <file:line> [under <exception label> at <file:line>]`. Use cached reads and the bounded `storage_worker` queue, then observe the landing
on the frame thread and call `idle::invalidate()` only when visible content changed. Boot's synchronous loads run before the frame scope; a new blocking exception
must be explicit and justified through `task::allow_blocking`, never added to a tick — and every production
`allow_blocking(` site is a line in `ci/allow/blocking.txt`, which the `blocking` gate holds exact (a
new site or a stale line fails; the list only shrinks). Move the work to a worker (`task::spawn_off_frame`; a PMS half takes
`&OffFrame`) instead of adding a line. The ledger holds two sites, both a refused worker thread's fallback rather than a user
action: `task::spawn_or_inline` (OS refused a thread; a server-side stop that must not be dropped runs inline) and
`route::decision::scrobble_stop` (refused `spawn_small_keeping`; its reporter handle and stop fence must stay ordered).
To see it fire on the TV, hold the TV lock and, after the first present in a `threadcheck` + `devtriggers` build, run `tools/tv-session.sh key hang:1000` for the guard abort, or `key hang-raw:1000` for the watchdog's unlabeled warning path (both capped at 5000 ms).

The release-enabled `task::watchdog` independently checks loop progress every 100 ms. It reports
once after more than 250 ms without progress and once on recovery, using the innermost static
`BlockingLabel` scope or `unlabeled`. The frame entrance increments one atomic counter; the
watchdog owns the clock, so durations are sampled rather than exact frame timings. Static label
descriptors keep scope publication allocation-free. The host harness disables the observer;
tests opt into private signals and drive the detector with synthetic timestamps. Timing starts
only after the first present completes, excluding initial shader/font warm-up. A freeze after
that point is reported once even if no further frame presents; after the loop resumes, the
watchdog reports the total stall duration and the label captured when the stall was detected.
With `threadcheck`, the first hang also publishes a purple runtime warning.
Controlled recording/replay boots suppress the warning's forced presents and pixels, so real-time
linger cannot change per-frame replay grades. Logging and fatal enforcement remain active. The FRAME thread
paints it, so it appears when the stall ENDS and lingers for about three seconds. It never appears
WHILE the stall is happening: the stuck main thread cannot draw. At >=2000 ms the observer sends SIGABRT once per stall to the main pthread
captured at loop start, so crashtrace records the interrupted main thread, not the observer.
Developer draw and swap phases publish label-only scopes (`frame draw`, `gl present`): these
neither invoke the blocking guard nor grant permission for guarded calls. On hardware GL (the TV,
a desktop GPU) a legitimately slow GPU frame still hits the >=2000 ms fatal threshold by design:
a two-second main-thread stall is a bug regardless of cause; use the `guard=log` escape hatch
below when investigating it. The one exception is a CPU rasterizer: when the boot's `GL_RENDERER`
is a software renderer (Apple Software Renderer, llvmpipe, softpipe, swrast, SwiftShader, WARP —
the CI simulator), time sampled in a GL-work phase — `frame draw`, `gl readback` (the capture
stream's and the simulator screenshot's `glReadPixels`), `gl present` — logs and warns but does
not count toward the kill, because that time is the rasterizer's. Two seconds outside those
phases, `unlabeled` included, stays fatal there, and so does the blocking guard.
`threadcheck: software GL renderer` in the log says the exception is armed.
If the watchdog itself misses more than four poll intervals (>400 ms), it cannot distinguish
process freeze, suspend, debugger stop or observer starvation from a main-thread stall. It
rebases the stall origin, clears any warning, resets the fatal latch and reports nothing for
that sample. A fresh uninterrupted two-second stall can still become fatal after the rebase.
A guard's own `abort()` instead records the abort path; its log label identifies the guarded call.
To investigate without termination, write `log` into `/tmp/plxnative-guard` before launch
(`/tmp/plxnative-guard=log` notation; simulator: instance runtime directory). This DIAG trigger
is latched at loop start and downgrades both fatal paths to log plus warning. It requires
`devtriggers`; it never suppresses the profile picker. Release builds contain neither warning nor
fatal policy and send no `SIGABRT`, and keep the existing guard/watchdog logs; they do send one
signal, the freeze probe (`SIGUSR2` to the main thread after 5 s without loop progress in the
foreground, at most four per process), which `src/crashtrace.c`'s `freeze_handler` answers with a
`*** FREEZE` record. That probe can cause one `EINTR` in a blocking call in closed library code.

**Damage is not an effect in the queue.** `fx.invalidate(provenance)` calls `Present::note` at
once, because a draw-phase report has to survive into the frame it belongs to. `Provenance` is
recorded, so a replay diff can say *why* a frame presented. Workers get exactly one documented
atomic door, `present::wake_from_worker()`.

**Time and measurement are capabilities, not free functions (§4.1, §4.3).** `Tick{ms, dt_us}`
arrives on the event; `Clock` is `SdlClock` or `VirtualClock`. `Measure` has three
implementations — `TtfMeasure` (device/sim), `TableMeasure` (replay, from the recording's metrics
table) and `FixtureMeasure` (host tests) — and travels on `Cx`/`DrawFrame`. The point of both is
§5: real scenarios record and replay deterministically.

**What has NOT landed** — the guide says so rather than letting `lib.rs` imply otherwise:

- §11 lists `nav.rs`, `trail.rs` and `press.rs` for deletion. Only `trail.rs` is gone; `nav.rs`
  (164 lines) and `press.rs` (619) are still here and still used.
- §11 says `present.rs` REPLACES `idle.rs`. Both exist (in `rust-modules/machine/src/`, since the
  machine split) — `idle.rs` at 1127 lines against `present.rs`'s 242 — so the gate has two homes
  and `idle.rs` is still the larger one.
- §0's done-criterion 9 (`doc_claim_auditor` clean against §11's prose list) is not met.

Modules with no product caller yet carry `#![allow(dead_code)]` with a one-line reason; that is
the spike, not dead code to delete.

## Product idiom (settled owner verdicts)

The reference clients are a checklist of CAPABILITIES, never a mockup. Plex says *what* a screen
must be able to do; Apple TV is what it should look like; the presentation stays ours. Each rule
below is a verdict the owner has already given — satisfy the capability, and do not re-open the
presentation in a design pass.

1. **Menus are popovers, never full-screen sheets.** A new menu — playback settings, a quality
   ladder, an item context menu, the play-queue list — is a `popover.rs` container over the live
   screen with a `table.rs` `TableView` inside, anchored near its trigger, the way `appkit/track_menu.rs`
   and the library sort/filter menus already work. Plex's full-screen modals were rejected by name.
   A menu never opens with its focus on a destructive action: tag such rows
   `Row::destructive(true)` and open the table with `TableView::open_sections`, whose
   `opening_row` is also the focus fallback when a focused row disappears.

   **Popover open styles.** Two exist. `Appear` (default) is the shared `K_APPEAR` fade+slide.
   `Morph` (`Popover::morphing()` is the one selector; open with `open_from(opener.rect)`) is the glass opener's pill-to-panel
   morph in `popover_morph.rs`: width spring k 620 and height spring k 950 critically damped, radius
   pill -> THE CALLER'S panel radius with a bulge (so settle and the start of a close do not snap),
   rows 0.82 -> 1 at zeta 0.6 k 720, and a 0.92 -> 1 fade+scale fallback when the opener has no
   rect. The glass is laid whole at the morphing rect and the rows come in over it (from 30%,
   opaque by 60%, clipped to the current rect), all on one clock (the slower rect spring).
   **The header menus morph the glass ONLY (design review S11/S12):** Library Sort & Filter, the
   Library's `+N` list and the Watchlist's Sort & Filter all open this way, and their opener is
   never part of the morph. A morphing surface names its opener (`Screen::opener`), the page
   leaves it out of its pass (`DrawFrame::lifted`: a pill left on the page sits under the menu's
   glass, and the rim's lens refracts its label into the panel's edge), and the dispatcher's lift
   pass (`Dispatcher::draw_with`, `Z::OPENER`: ONE place, after the last surface, on every page
   plan; the account chip, a `Screen::bar_opener`, is lifted by the same pass through `Rig::lift_bar_member`) calls `Screen::redraw_focused`
   -> `PageHeader::draw_opener`, the ONE paint of a header opener, which draws it focused above the
   scrim AND the panel, so the glass grows out from under a pill that does not change and a close
   shrinks back into it (`the_page_openers_lift_is_drawn_after_the_surfaces_on_both_plans`). The shared painters are
   `Popover::panel` (the glass at the morphing rect) and `Popover::morph_rows` (the rows, scaled
   about the panel anchor by the row spring). The morph draws no opener face: every header
   opener, the profile chip included, is the lift pass's, and the morph starts and ends on the opener's LIVE drawn rect (`Screen::opener_rect` -> `Popover::track_opener`, so the chip follows the bar's tuck). The morph is not
   faded by the container's appear: the surface begins and ends as the opener. A held surface is ticked with no time, so a morph starts on the first
   frame anyone sees. The item hold menu keeps lift+appear. A new morph constant is named in
   `popover_morph.rs`.

2. **Clickable text marks are ALL CAPS.** `MORE` and every other pressable text mark is
   capitalised, overriding the design system's sentence-case rule for app-written text: the mark is
   a control, not prose, and caps is how the app says *pressable*. Use the shared
   `text_view::more_mark()` rather than a screen-local literal — the drift that caused this rule was
   two screens each holding their own copy. A tab pill reading "More" is a pill, not a text mark.

3. **The player HUD gets a state glyph, not a transport row.** No `<<` `||` `>>` `■` buttons. The
   HUD keeps the small transport *state indicator* past the elapsed clock: a pause glyph while
   paused, a seek spinner while the transport owns the busy signal, nothing at all while playing.
   Transport is driven by REMOTE KEYS, which must genuinely work. Any `<<` / `>>` added must be a
   small glyph in that same indicator slot at the clock's cap height — never a control, and never a
   fourth entry in the HUD control row (`appkit/player_hud.rs`'s `BTN_N`, currently 3).

4. **Failure read-outs are never red — the app does not scold.** A read-out's verdict (DS3) is inked `theme::TEXT_PRIMARY`: bright, because it is the line the eye lands on — a `Failed` read-out's verdict, and an `Empty` read-out's caption, which is not bold. Its body, the reason under it, is `theme::TEXT_SECONDARY`. Emphasis is ink only: never a warning colour, a rule, a capsule or a tint. A `Failed` verdict is bold `size::TITLE` (per the design system's `StatusOverlay` contract). Home, Library, sign-in and the player's failure read-outs share ONE placement: the verdict hangs
   from `FULL_ANCHOR_TOP` with the reason and action row stacked directly under it, and a 112 px
   `theme::TEXT_SECONDARY` glyph above the verdict (`StatusOverlay::page`'s `glyph` argument). A
   read-out centred in its own container instead lands visibly off that verdict line and reads as a
   different component.

5. **Read the DS contract before inventing a control state.** Focus, hover and selected treatments
   come from the component's `.d.ts` in the design system, not from a lane's judgement. The Search
   field is the standing example: its focus is carried by INK ALONE — no capsule, rim, rule, plate
   or focus pop — and an accent underline was rejected on sight. A punch-list line like "make focus
   more visible" is satisfied WITHIN the contract (widen the ink step), never with a new primitive.

6. **An icon tab and a bookmark disc are both a mark, never a label.** The strip's faces are
   `TabFace::{Label, Icon}` (the Watchlist bookmark and Search are `Icon` faces: square, 60 x 60,
   the mark at `TAB_ICON_D`), and a destination is found by its KEY (`TAB_KEY_WATCHLIST` is
   `STRIP_BASE + 5`, drawn before Search), never by "the last face". The strip PULSES a mark once
   after a title is put on the list: `StripRender::pulse(idx)` (`TAB_PULSE_S` 0.4 s, the GLYPH
   grown to `TAB_PULSE_SCALE` about 1.35x with a soft white `TAB_PULSE_GLOW_A` glow around it,
   `tab_pulse_look`) is time-driven, ends at exactly rest, reports through `idle::note_motion`, and
   runs inside `idle::decorative` so a page image is never held for it. **No plate behind the
   mark**: a disc there is the strip's selected plate and reads as "this tab is selected". The APPLICATION asks
   (`ChromeSnapshot::pulse`); no screen does, and a screen that wants one reports an `AppFx`. A
   hero disc that swaps a glyph for its filled twin (the Detail and Home hero bookmark) uses
   `CircleButton::icon_fill(filled, fill, dy)`: the filled glyph is revealed from the box's bottom
   up with a downward nudge, the two icons share one silhouette, and a settled control passes the
   resting icon alone so nothing of the animation lingers. The 3 px ribbon drop is a named constant
   in `plx_appkit::watchlist_disc` (`RIBBON_DROP`), since no theme token is 3 px.

## The route family says where BACK goes ONCE, as a crumb

Every Settings-family route — Settings, Privacy & data, Legal notices, a legal document, About, the
Home-sources editor, the first-run consent pair — draws a caption line above its title: a
left-pointing chevron (`Icon::ChevronLeft`, the mirror of the row chevron and sharing its `ink_x`
entry) and the name of the place BACK returns to. `RouteLayout::draw_narrative` takes it as
`Option<&str>` so a new route cannot forget to answer. **`None` means "BACK does not go anywhere
inside the app"** — three routes qualify today (the Settings modal, the first first-run consent
question, and the QR sign-in), but take the census with
`git grep -A2 'draw_narrative(' -- 'rust-modules/ui/src/*.rs'` rather than trusting that number.

**One crumb rule, keyed on where the page's heading lives.** A page whose heading is in its header
row reads `Parent › Current` in that row (the Library family, `page_header`'s `Crumb`). A page whose
heading is its own big title carries `‹ Parent` above that title, and both forms say the parent
page's own title string (one catalog entry, never a second spelling). The big-title form has ONE
painter, `route_screen::draw_back_crumb`: `draw_narrative` draws the Settings family's through it,
and the Collection screen draws its `‹ Parent` through it in the title column, in the page's
top margin (`route_screen::CRUMB_RISE` above the title's cap top, so nothing below moved). Neither
is a focus stop; BACK already returns. A page that wears the second form calls the painter rather
than drawing a chevron of its own.

**`‹ Parent` names the entry BENEATH the page on the navigation stack, read from the container
once.** A page with ONE opener may name it (the Settings family, filmography: correct by
construction), but the Collection screen is opened from the Collections page, a Home hub,
Recommended, a library page, Search and an item's Detail, and a constant named the wrong place
whenever BACK went elsewhere. So the dispatcher builds each page's context with the screen id of the
entry beneath it (`NavStack::beneath_id`, folded in by `Host::views_over` through
`CxParts::page_cx`; `None` for the root), and the host turns that kind into the title that page
wears (`app::page_title`: Home, the library's name, Collections, Recommended, Watchlist, Search, an
item's title; a page it does not name gets no caption). The screen reads it
(`CollectionLike::back_to`), is never told by its opener, and draws no caption when BACK leaves the
page stack or the page beneath has no name. A new big-title page that can be opened from several
places reads the same answer rather than carrying a parent of its own.

**It replaced `Press [BACK] to return` in the bottom action band across the whole family**, and the
trade is the point: the hint spent a 60px band restating a key the remote already has and could not
say where the key WENT, which on a three-deep push is the only part anybody needs. Freeing the band
is what let first-run consent put its two answers there. The sweep is complete —
`git grep -n 'KeyHint::new'` finds no call under a route any more, and the consent document
reader's was the one that survived the first pass, so that route said where BACK went twice.
`KeyHint` is unchanged and still correct where it lives: the read-only ALERT panels
(`screens::about_panel`, `screens::person_bio` and `screens::tracks_panel`), which hold no control at all, so
there the line really is the whole affordance. **One ROUTE draws one since 2026-09-17 and it is the
opposite object** — `screens::detail::trailer`'s `[^] Full screen`, under the hero's
control row while a trailer is playing behind the page. What the sweep removed was a line naming a
key the viewer already knows (BACK) for a destination it could not name; this one names a key that
does something the page gives NO other sign of, and it is drawn only in the seconds the trailer's
picture is up, fading with it. That is the test to apply to the next one: does the hint teach an
affordance that is otherwise invisible, or restate the remote? It reaches the cap through
`KeyHint::glyph`, the `CapFace::Glyph` face added with it — a cap whose legend is an `Icon` centred
in the same keyline rather than a `MICRO` word, because the chevron IS the key's face.

## Where things live

**`lib.rs` is the map.** Every module is declared there with its purpose and, where it came from
the restructure, its spec section. It cannot go stale the way a hand-written table does: rename a
module and the build breaks.

**The `//!` doc at the top of each file is the detail** — every file here should carry one, and
they are the authority on their own module. Read the file's `//!` before changing it.

This guide deliberately does NOT carry a per-file table. One existed, drifted 37 files out of
date, and duplicated the `//!` docs verbatim down to their typos; `docs/ui-framework.md` already
states the rule — *"do not fix this file into a second map of the directory; one map is the
point."* If you add a module, write its `//!` doc and its `lib.rs` line. That is the documentation.

**Adding a SCREEN is a different question**: screens live in `rust-modules/screens/src/` (workspace crate `plx_screens`) — see
[`../../screens/src/CLAUDE.md`](../../screens/src/CLAUDE.md).

## Glass, dim and dither — rendering policy you must not re-derive

Each of these was measured on the television and each has a cheaper-looking wrong answer that was
tried first. None of it is reconstructable from the code without repeating the measurement.

- **Live-backdrop glass: call it, don't orchestrate it.** To add a surface, call
  `Glass::DYNAMIC_BACKDROP.backdrop` from its normal `Painter` draw. Do **not** prepare,
  invalidate, check for modals, or skip a source pass in the widget. The dispatcher supplies the
  layer order and `ui::frame::backdrop` (`gfx/backdrop.rs`, owned by `GlassPlan`) declares geometry, excludes the
  glass's own and higher layers, checks occlusion and compares the ordered draw arguments beneath
  each sampling region.

- **Two live glasses whose blur footprints meet never share a frame; a page that hands a place from
  one to another sequences them.** The upper glass has a lower live glass inside its footprint, so
  the engine drops it from `jobs()` (`composite_capture`: a "composite" band) and it captures
  INLINE in the visible pass: a mid-frame `glCopyTexSubImage2D` and the six-pass chain, where a
  direct job replays the page at 1/4 in three. Measured on the television (2026-10-19): the inline
  band costs about **1.4M GPU cycles per frame over its own direct job** (12.91M against 10.73M on
  the Library's tuck ramp with the capsule up; 25.6% of those frames late against 2.4% without it),
  and a direct job plus an inline capture on one frame is the FRAG_NUM_TILES class above 6300 that
  the 4096 / ~5000 / ~6050 bins never named. The engine keeps composite capture as the correct
  fallback (the pixels are right, and the composite tests stay); it is the page's job not to need
  it. The Library's chrome band and its scroll capsule are the case: the page owns one
  `scroll_chrome::Handoff`, decided on the key frame from the scroll target and run on the `xfade`
  clock, in sequence in both directions (the departing glass fades to exactly 0 and only then does
  the arriving one start), so no frame has both. Home keeps its nav and chip shown and owns no
  `Handoff`; a heading that rides under them fades by where it stands through the same spatial rule
  (`scroll_chrome::clearance_under_shown_chrome`). The chip and the
  tab track (`GLASS_TRACK_MAX`, `BAND_AIR`) and the hold hint and the band (`hold_hint.rs`) keep
  apart the same way. Any new live glass that can stand within a band's blur reach of another
  (68 px past the visible members) must be sequenced with it, or it prices ~1.4M a frame.

- **A MENU is glass on the frozen ground (DS6); nothing else is glass.** The scope is
  MENUS ONLY (owner decision, wave 2 review): Sort & Filter, the account menu, the item hold menu,
  the Library menu (the player's "…" menu stands over the video plane and keeps its own ground).
  Every other `panel_ground` user — the alerts, About, Track information, the person bio, the
  Also-available sheet, the decision alert — keeps the pre-DS6 ground, the latched underlay field
  under the frost. The choice is explicit at each call, `widgets::Ground::Menu` or
  `Ground::Sheet` (`panel_ground`'s last argument, `Popover::panel`'s too); there is no global
  switch. A `Menu` ground's first act is `popover_glass::draw`: `fs_glass.frag`'s composite through
  `gfx::draw_glass_frozen`, wearing a gradient face (`FACE_TOP`/`FACE_BOT`), the 1 px
  `theme::GLASS_RIM`, the lit top edge `GLASS_RIM_LIGHT` and `GlassRim::Menu`, then a soft shadow
  cut out around the panel's rounded shape. **It reads no framebuffer** (owner decision,
  2026-10-17: menus stand on main's frozen ground and never live-refract a re-rendered page). Its
  body shows the latched underlay field (`ui::underlay`, the 60x32 reconstruction the modal dim
  draws, reaching a surface as `DrawFrame::underlay`); its rim's lens refracts the host snapshot
  (`popover::host::page_tex`, the one capture of the undimmed page) with the SAME rounded-rect SDF
  that rounds the panel, at `tokens::MENU_SHARP`; the frame's one modal dim (noted by
  `Painter::dim`) is applied to both at composite (`u_dim`, and `u_dim_sharp` for the snapshot).
  So a menu requests no backdrop entry, runs no blur source pass (`src`, `pagex2`) and copies no
  band (`glass.sharp`): its open, stay and close cost one panel quad (profiler phase
  `glass.frozen`) and its shadow, and a morph or a slide moves that quad over pictures that do not
  change. What the pictures do not hold, the glass does not show: an opener lifted over the dim
  (the item menu's card, the account chip) is absent from the lens, and a menu stacked over
  another surface's dim, or drawn before the field has latched (the first frames of a modal), or
  over the video plane, falls back to the field ground (`draw` returns `false`), windowed to the
  panel's rect and graded under `PANEL_LUMA_MAX`. The opener lift is frozen too: the account
  menu's profile chip, lifted above the dim and the panel (`widgets::lift_profile_chip`, called by the
  lift pass through `Rig::lift_bar_member`), is a `glass_frozen`
  capsule on the stack's latched field (`ModalStack::lift_field`, no dim, the host snapshot behind its
  rim) and requests no backdrop entry or `src` pass; before the field latches the lift draws
  nothing (the bar's own chip is already on screen at that rect). Only the chip on the tab strip, with no menu up, is live glass. The scrim behind a compact menu is
  `theme::underlay::DIM_COMPACT`, 30%. The face colours and shadow are theme roles
  (`MENU_GLASS_TOP/BOT`, `MENU_SHADOW*`); the face fades with the panel's cascaded alpha.
  `GlassRim::Menu` over the LIVE path (`draw_blur_backdrop`) would turn on a per-frame sharp band
  copy; no menu takes that path any more. A source-grep test (`widgets_glass_budget_tests`) keeps `Glass` and
  `Painter::backdrop_blur` out of every file except the chrome list — no popover and no screen
  names the blur path — and pins the scope: the menu files name `Ground::Menu`, the sheet files
  `Ground::Sheet`, and none names the other.

- **Glass over a page stands on the frozen host; the page does not move while a menu is up, except
  under a menu that lets it reflow.**
  A menu (opening, open or closing, until it has let go of the host at `CLOSING_GONE`) stands on
  the page as the host snapshot holds it (`popover.rs` module doc, "A modal stands on a page that is
  not moving"): nothing on the page animates, reflows or lands under it. A layout motion a menu's
  choice starts (the Watchlist's Remove) and the data that would change the page (the app's
  `watchlist_feed`) wait until the menu is gone and start on the first frame after
  (`containers::modal::page_veiled`). **The one exception is a surface whose body answers
  `Screen::lets_host_reflow` (the Watchlist's Sort & Filter menu):** it casts no veil, the grid
  reflows under it, the host is drawn live while the cards move (`popover::host::set_reflow`: no
  per-frame capture, `page_epoch` does not move) and is retaken ONCE when they stop, the old look
  fading into the new ground over `host::CROSSFADE_S` from a second retained copy. The menu's glass samples only that frozen ground (above), so
  even a page that did move under it would rebuild nothing for the glass. **Do not add new
  live-refracting glass over a page unless the owner asks.** Live glass measured about 2 to 2.6M
  Mali GPU cycles per frame more than frozen ground (2026-10-17, release build, mock, panel off),
  with the page re-drawn at quarter resolution on each frame its draws changed (the `src` and
  `pagex2` spans).

- **A modal's dim is DRAWN, never applied to the source.** The variant that multiplied the host
  page's RGB going into the backdrop was measured and removed: dimming the source destroys the
  modulation the frost is layered over, so the panel arrives flat however dense the material is.
  Do not re-add it from this note. And the scrim is part of the PAGE — the direct source path
  re-renders the page closure before any popover draws, so a scrim drawn with the panel reaches
  the visible frame but not the snapshot, and the panel's ground then comes out brighter than the
  screen around it. Since phase 10 the pairing is the container's (`Screen::scrim` +
  `ModalStack::draw_scrims`), not each panel's. **The dim's ink is the page's own light, not
  black**: a surface names a ROLE (`theme::underlay::DIM_COMPACT`/`DIM_PANEL`/…), never a literal,
  and a test greps for one. **Drawn, yet a glass whose LAST source paint is the dim applies it at
  composite** (`backdrop::Dim`, `Painter::dim`, `fs_glass.frag`'s `u_dim`): that glass's retained
  source is the page without the dim, and its composite mixes this frame's dim ink over the
  filtered page — the same result by linearity (`blur(mix(P, ink, a)) = mix(blur(P), ink, a)`),
  so a ramping or re-latched dim re-blurs nothing. That is NOT the removed source multiply: the
  page's modulation is still filtered, and the dim is still a scrim drawn in the visible frame.
  Anything drawn above the dim under the glass (an opener lift, a lower glass in the DIM band,
  as the account menu's chip is) keeps the dim in the source, and that glass re-captures while
  the dim moves.
  **The page's WASH is deferred the same way, as a DELTA** (`backdrop::Ground`,
  `Painter::ambient` / `ambient_inked`, `fs_glass.frag`'s `u_wash_*`): `AmbientWash` eases each
  corner on its own spring, so a library switch or an art-less focus move moves the exact 8-bit
  codes every frame, and the wash is full-screen, so it sat in the prefix of every live glass
  under it (the tab bar's band, the hold hint) and re-sourced each one on every frame of the ease.
  Now a glass whose first draw under its region THAT CAN SHOW is a marked wash (the only one
  there, covering the whole region, no dim and no lower glass under it) keys on the wash's rect and
  ink and not its corners; the frame clear every page records first is overwritten by the opaque
  wash and does not count (a test list that omits it passes while the feature never runs: the wash
  tests record `gfx::frame_clear` as a page does); its source keeps the wash it was drawn under and the composite adds `wash now - wash
  captured`, at the refracted uv, weighted by what the ink ramp lets through. **The marker is
  DECLARED** (`declare_as`, never the tag): `ambient_art` (tag 22, a photograph in the ground) is
  never marked, and a ramp knee inside the footprint (two washes) keeps the wash in the source.
  **Bounded, not exact:** over card content the term adds the ease as if the wash showed
  everywhere, so the error is at most the ease's amplitude times the content's coverage under the
  filter, and it is gone when the wash comes to rest, where the source is re-taken ONCE. **Rest is
  the wash's OWN statement**, not a reading of its codes or of the page: `AmbientWash::step` lands a
  spring on its target the moment it settles, `AmbientWash::at_rest` (all twelve velocities exactly
  zero) rides `Painter::ambient`'s `resting` to the glass, and the re-take fires on the frame it is
  true and the held wash differs. A critically damped tail holds an 8-bit code for whole frames
  while it moves, and a card can keep the page moving long after the wash is still, so neither
  "the codes did not change" nor "nothing moves" is rest. A page dip (the painter's alpha mixing the
  wash toward the app ground) is not rest either: `resting` counts only through an identity
  cascade. A card moving under the band keeps every frame owed, as before.

- **Freezing the host page is a separate, shared policy** — `Popover::caching_host()` +
  `ui::popover::host`. The mechanism is a REFUSAL at the renderer's one shared gate
  (`gfx::culled`), not a skipped call tree: the page's draw still runs, so layout, hit rects and
  texture uploads are untouched and only the fill goes away. An owned surface asks for it through
  its `Style` instead.

- **Every slow FIELD takes ONE dither whenever it is drawn, and it is not a per-shader decision.**
  An 8-bit framebuffer quantises: Settings over Home measured a 700-row column spanning luma 55.7
  to 59.1 in FOUR levels, treads of 158, 157 and 146 rows. `shaders/dither.glsl` is the answer and
  `gfx::glsl_dithered!` is how a program gets it — for the four whose ramp is a blur or a
  whole-screen wash, and deliberately NOT the per-rect `fs_src`/`fs_shadow`, which carried it for
  two days and cost the hero paging scene 57→50 fps for a branch that answered "no" on every draw.
  `gfx::dither_for_field` is an AREA test only: a field dithers whenever it is drawn, moving or
  not, and every page wash dithers on every frame.

## Card marks — three marks, three meanings, and no card may blur them

Library All uses the shared caption-band policy for movies, shows, seasons and collections (so does every other POSTER tile: Home's shelves,
Recommended's poster shelves, Search's poster rows, the person page's filmography, Detail's Related and collection shelves and the Collection
screen's members; `Shelf::set_caption_policy(Some(policy))` sizes the band a shelf reserves and the caption block it reveals by
`caption_policy::band_h`, and a `Stack` page hands it to a shelf or grid section through `StackPage::caption_policy`; a shelf or grid with
no policy (`None`: a landscape still row or a headshot row, and the Library's Episodes listing, a grid of stills) keeps the legacy 114 px
band and its two-line block, under every setting; the band must follow the label, never the setting):
only the focused row opens its caption; a bounded set of springs closes rows left behind, and
each LINE of a row's caption is drawn only once the room under its card holds the line's box, and
never beyond its band's own expansion (`card_row::line_reveal_for`, one rule for a shelf and a grid), so
a move hands the title over from the departing row to the arriving one with no line over what is under
it AS DRAWN: a grid measures to the drawn top of the tile below (its live pop and press), and the owner
of stacked shelves hands each one the lift of the next shelf's heading (`SectionFrame::below_rise`;
`Stack`, Home, Recommended and Detail do; a `Stack` page keeps its shelves adjacent, the air under one its
`StackPage::shelf_foot`, never a `Custom` gap section between two); `GridBands::closing` names the card a closing band's caption belongs to.
Live and target geometry include these bands, so painting, paging, scrolling and restored
viewports agree. Episode rows use landscape stills. `Art::Still` supplies the label ground;
`widgets::still_overlay` draws its labels and resume bar afterward. Both the artwork and
its ground participate in backdrop discovery, including texture changes and UV cropping.

- An **amber ▶** means **this press starts the video**. It is drawn where that is true and nowhere
  else: the detail page's episode filmstrip always, and a Continue Watching deck only while
  Settings > Playback > Continue Watching is "Play" (`DeckPress::Play`; the default opens the page
  and draws none). On a poster deck (Home's, Recommended's) the mark leads the FIRST line the Under
  posters setting draws (`registry::captions::label`): the title when Title is On, else the details
  line ("8m left"), hanging to the left of a line that is centred as if it were not there; with the
  title and the details both off (Nothing) no line is drawn and so no mark, EXCEPT on the deck: its
  progress line ("S3 · E4 · 18m left") is the tile's state and is always shown, Nothing included (the
  owner's 2026-10-22 call; `captions::deck_policy`), so the deck's band keeps one line (two with the
  Title On) and the mark leads that line. The band is per shelf, so it never changes as focus walks
  the row. Watch state does not
  withdraw it — `widgets::still_glyph` is the whole rule, graded by an exhaustive table, because
  the clause that failed first was the absent one: the glyph used to be decided by `PosterMark`
  FIRST, so an in-progress tile drew nothing and a Continue Watching deck — almost entirely
  in-progress tiles — announced nothing and then played.
- An **amber resume BAR** means **how far in you already are**. It is a fact about the item, not a
  promise about the press, so it is drawn wherever there is progress to report, including on a
  shelf that navigates. Hiding it there would hide something true; the triangle beside it was
  always the mark that read as "this will play".
- A **`✓`** means **finished** — likewise a state, not a promise.

**A card with no play indicator NAVIGATES** (a poster deck set to Play with the title and details both Off draws no
line to carry the mark, the one exception), and an episode navigates to its own detail page —
except on Continue Watching, where it opens the SHOW on the card's season
(`app::input::activate_deck_card`; the card menu's Go to Episode is the way to the episode).
Until 2026-09-05 `want_play` was `from_deck || kind == 3` on both Home and the Library, so an
episode played immediately from ANY shelf and every landscape tile drew the triangle whatever its
press did. Behaviour and mark both read `is_continue && route::deck_press().press_plays()`
(`Shelf::is_continue` / `pms::hub_is_continue` alone is no longer the answer to either), so they
cannot disagree. A poster wears nothing at all when it has never
been started — most of a server is unstarted, so a clean shelf is the common case and a mark is
information.

## Placeholders are counted where they are DRAWN (`placeholder.rs`)

A skeleton, a loading ground, a spinner or a "Loading" read-out says "the real thing has not
arrived". Every site that paints one calls `placeholder::note` (or one of its wrappers: `skeleton`,
`ground`, `flat`, `face`, `ink`) at the DRAW, with the painter that paints it — never at a resolve
(Home's `prefetch` resolves heroes it never draws), and a recording pass counts nothing. Absence is
not a placeholder: a person the server has no headshot of and a collection with no artwork wear
`theme::CARD_ABSENT` (declared with a `placeholder-exempt:` reason), a profile with no picture keeps
its ordinary `CONTROL_IDLE_FILL` disc, and a hero whose clearLogo has settled as a miss
(`tex::logo_failed`, asked only when `placeholder::accounting_visible()`: armed, or the sentinel build)
types its title as plain ink and is reported in `Frame::absent`; none is counted
(a profile whose picture path has not resolved, and a logo still on its way, ARE counted). The
feature `placeholder-sentinel` (off in every default, shipping and TV build; `make check` compiles
it) additionally paints the FILLED placeholders (skeletons, grounds, spinner dots, the stale-strip
cover) in the one exact `theme::PLACEHOLDER_SENTINEL` at alpha 1 with no sheen, so a frame scan can
find them by pixel. The two oracles fail differently (the counter misses a site nobody hooked, the
pixels miss a colour nobody overrode), which is why both exist.

**Art that lands late dissolves in over its skeleton, and that skeleton is not counted**: a card whose
texture arrives after it drew a skeleton (`card_motion::Scope::arrival`, the card's render history, not
`LogicalState`) draws the art at `ArtImage::shown` over `placeholder::skeleton_under` / `ground_under`
(exempt: the picture is here) on the page fade's own duration and curve (`xfade::arrived`); art that
was resident at the card's first draw shows whole. The count must not include that sheet, or a frame
dump (which holds on any counted placeholder at a frozen clock) would never write the dissolve's frame.
**The history advances once per card per frame, whatever the number of walks**: the frame's first walk
that draws a card (discovery) works its verdict and art phase out, and every later walk (a glass source
replay, the visible walk) reuses that answer, so the picture a glass filters is the picture the panel
shows (`card_motion` module docs; `backdrop::walk_serial` names the walk). Keyed by the draw's place in
the whole frame, a replay that started with the landing moved the visible draw to an occurrence with no
history, and the art showed whole in one frame (DEBUG-5).

**The count is zero or non-zero, never "how many"**: the page closure runs once per walk (discovery,
each blur source job, the visible pass) and each walk ticks. **A page shown as a held image** (a
`PageDip` push's IN half and its quiescence hold) draws nothing, so its capture's placeholders are
recorded (`placeholder::since`) and re-noted on every frame the image is painted; the pixel oracle
cannot see a placeholder inside a blended image, so the counter alone vouches for those frames
(`Dispatcher::held_page_image`). A frame dump holds no image (every dip frame re-captures the page
live, `PageImage::step_filmed`), so there the capture's own draw is the debt.

**The demo-video driver's contract** (full text: `placeholder.rs` module docs, "What the dump driver
(S5b) must do"): `reset` before the frame's first walk and `take` after the last, on the UI thread;
force a live draw on every written frame; a held page image is not a hold reason, only its captured
placeholders are; read the count as zero or
non-zero only; wait out reveal springs and the dip alpha after arrival (an `Xfade` In ramp plays in a
dump as in the product, under the dip: a dump films a pushed page live, never as a held image,
`plx_gfx::dump`'s one picture-changing page rule); cover
late-arriving sections (Home shelves, Detail's related/cast/extras/ratings, Recommended-page shelves, an
`Xfade` Hold) with the landing predicate, since neither oracle sees an absent section; fail fast on
`Frame::absent` and on an entry whose key is `""` instead of holding; scan connected components
against exactly (255,0,254), never scanline runs, and report as COUNTER-ONLY the reasons no pixel
can show (Home and Detail backdrops, every caption, the hero-logo fallback, the spinner under a
run scan, anything under a scrim or glass, a held image); build the sentinel in CI; keep `pin_hero`.

Unarmed (every shipping build) a hook is one relaxed atomic load and a branch before any
thread-local access; do not put a hook on a resident-texture path, and read anything a hook needs
once per draw, not once per cell (`season_loading()` is two `SeqCst` loads).

`ci/check-placeholders.py` (in `make check`) fails on a placeholder reference with no counting call
in the SAME FUNCTION, or `// placeholder-exempt: <reason>`, pairs each spinner with its own call or
marker, and fails when a `Reason` loses its last hook; a new loading surface needs one or the other.
Its docstring states what it cannot see.

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
  `CONTENT_TOP` with one of THREE gaps, and which one is `section_gap`'s whole job: `SECTION_GAP`
  between ordinary sections, `TAB_EP_GAP` where the season tabs hug their episodes, and
  `consts::UNDER_LABEL_AIR` after a SHELF section (Related, Cast, Extras), which already carries its
  own label band — stacking a region gap on top of that band is the double-count that made every gap
  below the hero read as a hole. To resize/space
  a section, change its `block_h` (content-derived — e.g. Related tracks `REL_H`=`CARD_H`) or the gap —
  never reintroduce a hard-coded per-section Y. `ScrollColumn::draw` culls off-screen sections with
  `on_axis` and pre-translates each child painter to its origin, so each `draw_*` draws from local
  y=0; the same `child_top` feeds `scroll_target` (via `lift_target`), so draws and scroll can't drift.
- **A few things are deliberately immediate-mode** (documented in `docs/ui-system-migration.md` §D):
  home's `Backdrop` **per-element alphas** (the alphas, not the component — those alphas are springs
  now and it has a real `update`; what must never happen is routing the per-LAYER reveals through
  the cascade `p.alpha()`, which would fade the art and the wash as one object. The cascade still
  reaches the whole `Backdrop` from above, which is how `nav`'s page dip covers it),
  `appkit::player_hud`'s `SCR_H`-offset geometry
  (shared with `app.rs` pointer hit-tests), and the subtitle renderer. Leave them; wrapping them in a
  `View` breaks a load-bearing contract.
- **Clipping: prefer culling; use a scissor clip only for a hard-bounded panel.** Two spellings
  today. The legacy one is `Painter::clip(rect)` / `clip_clear()` (backed by `gfx::clip_set`/
  `clip_clear`) — **global GL state**, so you MUST pair set/clear inside the same frame (the panels,
  the document reader, the glass cut in `home`/`library`/`filmography`, the card underline are its
  users). A screen drawing through a `DrawFrame` uses `f.clip(p, rect)` instead: an RAII scope that
  restores the previous scissor when dropped AND narrows the painter's `clip` cascade value, so a
  `stop` registered inside it is intersected with the visible part (spec §7.6). Do NOT reach for either in
  the big scroll flow: `ScrollColumn`/the shelves deliberately **cull** off-frame children by index
  (`on_axis`) instead, which avoids per-frame scissor churn and needs no clean-up. So: bounded list/panel
  → `clip`; long scrolling document → cull. (The old edge-fade-mask trick is gone — a linear fade can't
  cut a tall two-line row evenly, which read as a broken clip; scissor replaced it.) `TableView` is the
  one place rows still fade, and only WHOLE rows by their ink position, never a per-pixel mask: the top
  edge dissolves rows (`top_edge_alpha`), a PANEL's (`draw`) bottom edge fades them before the scissor,
  and a PAGE's (`draw_page`, what `TablePart` and the Filmography page call) bottom edge has neither —
  its clip runs on to the bottom of the screen, so rows simply slide off it.

## When you're done

Three signals, in ascending cost. **`make check`** first — the host unit suite (`cargo test --lib`,
~28 s for 3,639 tests, measured 2026-09-17; do not re-quote that number, `time` it) includes the UI
geometry and focus tests: `screens/home/tests.rs`'s destination round trips,
row stepping staying inside the shelf array, and the pointer hit column matching the drawn card at
every snap phase; and
`card_row.rs`'s heading-clearance behaviour driven frame by frame (the regression that the heading
must hold still while focus walks the slots beneath it). If you touch focus navigation, hit-testing,
or shelf motion, **add a test here** — that math is host-testable and these caught real bugs.
Note the asymmetry, because it tells you where to put a new test: `card_row.rs` drives a **local**
`CardRow`, so its tests are ordinary and parallel; an owned screen's tests hold no focus of their
own (the `FocusEngine` does). Tests that seed the shared registry or the remaining global stores
take `testlock::serial()`; tests using separate production BrowseStore owners isolate their state
and landings. **`xfade.rs` is
the cautionary case**: its tests were ordinary and parallel until `tick` started reporting to
`plx_machine::idle`'s process-global flag, at which point driving a fader began mutating state *another
module's* assertions read. They all take `testlock::serial()` now. Anything you make report to the
frame gate inherits that obligation — and the lock is ENFORCED rather than merely documented since
2026-09-10: it records the holding thread, and the shared stores (plus the app frame trunk, which
pumps all of them) call `testlock::assert_held`, so an unguarded write panics in the test that made
it rather than in whichever bystander it happened to land on.

**If you add anything that ANIMATES, it owes two tests**, because the failure modes are opposite and
each is invisible to the other's gate. Host: it reports while running **and** goes quiet at rest —
`machine/src/idle.rs`'s and `ui/xfade.rs`'s test modules are the pattern, including a settled-tree case that
steps 439 springs and asserts *nothing* is requested. Device: an `fps_floor` scene proving it
still animates under the gate, and (if its screen settles) an `fps_ceiling` proving it stops. An
over-reporting animator costs the entire ~38-points-of-a-core saving while every `floor` in the
suite still passes, so the ceiling is not optional politeness — it is the only thing watching.

Then **`make`** must stay green (ARM cross-build). Then the device: there is no host *runtime*, so
nothing above draws a single pixel. For anything that moves pixels, capture the panel on the TV
(`tools/capture-screen.sh out.png DISPLAY|GRAPHIC`) and eyeball it — the token collapses and
cap-band re-centering are invisible until deployed.

Note (library redesign): `cards::draw_label` is re-exported so screens can draw a poster caption
through the shared label path; Settings > Libraries has two rows (Favorite libraries, Under posters).
