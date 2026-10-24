# screens/ — the application's owned screens (read before adding or changing one)

Every `Screen` impl the dispatcher mounts, steps, focuses and draws lives here, across
this directory and the `detail/`, `home/`, `item_menu/`, `library/`, `player/`, `search/` and `watchlist/` families. This is
the APPLICATION half of the restructure; the library half is `../ui/`, whose
[`CLAUDE.md`](../../ui/src/CLAUDE.md) carries the token rules, the architecture and the rendering
policy. Read that one first — its four rules bind here too.

**`lib.rs` (the crate root, `mod.rs` before Split 13) and `registry.rs` already document themselves.** Both carry substantial `//!` docs and
they are the authority on the module list and the application bundle. This guide holds only what
they do not: the rules that span screens.

## The layer rule is the thing that will bite you

A screen may name `ui/`, `appkit/` (the widgets several screens share — `player_hud`,
`track_menu`, `source_list` and the rest of the player's panels), `stores/`, the data crates and
this directory's `registry` — **never `app/`, and never a sibling screen module**. `ci/check-deps.sh`'s `layer` and `sibling` gates have
held this since restructure phase 10, so a violation fails CI rather than review. Since Split 13
this directory is the crate `plx_screens`, which cannot name the application at all (cargo has no
edge from it to `plxnative-modules`, and the application depends on it); the `layer` gate still
greps for the spelling `crate::app::`, and the `sibling` gate reads a sibling as `crate::<screen>`
because the crate root is the module `screens`. What the screens crate needs from the build
(`PLX_BUILD_SHA`, for About and Source) the application hands in at boot
(`legal::set_build_sha`, the way `plex::identity::set_version` is handed the version).

Two consequences worth stating, because both look like the obvious thing to do:

- **Reaching another screen is a message, not a call.** *Also available* reports its destination
  rather than navigating: it delivers `AppMsg::AltSourceOpen` to the Detail instance named on its
  argument, and the mounted `DetailScreen` turns that into a `ContentReq::Present`.
- **A screen keeps no focus of its own.** The `FocusEngine` in `Input` holds THE current focus and
  every group's remembered cursor (spec §7.3). There is no per-screen `move_focus`, `top_focus` or
  `key(sym)` ladder any more, and adding one back is how the old twelve-ladder tangle started.

No `static mut` here either: a screen's state is a field of the instance the container owns (§6.1).

## Adding a screen

The spec's done-criterion 5 is a measured claim, not an aspiration: *a new screen touches its own
`screens/<name>.rs`, `screens/registry.rs`, `dev/scenarios.rs` and `tests/manifest.json` and
nothing else.* Two conversions were run to prove it, and
[`docs/ui-system-migration.md` §(E)](../../../docs/ui-system-migration.md) records what
`git diff --name-only` actually said for each — the real answer is those four plus `screens/lib.rs`
(`screens/mod.rs` before the crate split), `rust-modules/ui/src/lib.rs` and `ci/allow/statics-migration.txt`.

If your change is reaching further than that list, the design says stop and ask why: the variant,
the screen id, the `mount` arm and the recorded shape (`SCREEN_SHAPES`) all live in `registry.rs`
precisely so they do not spread.

A screen composes the shared mechanisms and never invents one: it does not hand-route direction
keys between regions (the `FocusEngine` does, from the groups and edges the screen declares) and
does not hand-roll a transition or a menu. The ready-made ones, and what to do when none fits, are
rule 4 of [`../../ui/src/CLAUDE.md`](../../ui/src/CLAUDE.md).

A screen mounted as a modal surface is the same trait with a different `Style`; the Settings family
instantiates the same screens a second time for its own inner stack, which is how one
`OnboardScreen` mounts twice (§6.2).

## A landing must not move ground somebody is looking at

The Library's seven reported bugs in 2026-09 had one cause, and the shape recurs on every screen
that mixes async content with a live cursor:

- **Stage, never self-commit.** `browse::section_hubs`' `land_ok` ALWAYS stages. The first paint
  used to self-commit "because there is no layout yet to protect" — but the window is four
  SECONDS, and two presses reach the grid inside it.
- **A reloading control is not a missing control.** `browse::requery` empties the store
  synchronously on the press, so for the length of the fade the heading, the Sort/Filter row and
  the rail are all controls acting on nothing, and the async focus clamp *correctly* moved focus
  off an undrawn zone. The clamp cannot tell "this control is gone" from "the thing it acts on is
  reloading" — so `Layout::grid_head` stays true while a grid-scoped transition is in flight.
- **A seat nobody chose follows the head.** A page that seats its own focus on its document's
  first block must re-seat when a landing changes what that block is, until the user moves. The
  Library's grid was prepared before its hub shelves landed (they have since moved to the
  Recommended page), so its first seat was the grid's heading; the shelves then committed above it
  and the first DOWN went on into the grid (`LibraryScreen::provisional` keeps the rule for any
  block that lands late). A restore is only what the reader chose: the engine remembers
  the page's own seats too, so the page keeps their provenance (`LibraryScreen::placed`). Home already has this shape (the hero seats when its action row
  arrives, while focus is still on the strip); Search seats its field, which never arrives async,
  and Person and Detail keep no self-seat for a landing to strand.
- **The grid absorbs an index landing, the page shifts its scroll.** The Library's grid is a
  `cards::Grid` in `ScrollMode::External`: when the focused element's index changes with no
  `FocusMoved` (an insert or reorder before it) the grid moves its pop with the element and
  `Grid::landed_shift()` is the document shift that keeps the tile where it was on screen;
  `LibraryScreen` adds it to BOTH `scroll` and `scroll_target` on that tick, so the spring has
  nothing to chase. The publication of the landing (`GridPart::refresh`, staging, commit) stays
  the Library's.
- **Derive re-entry position, don't store it.** `Layout::seat_for_scroll` derives the focus seat
  from the restored SCROLL rather than a saved grid index: the server's hubs change subject
  between requests, so a stored shelf index can name a different shelf on return.

A press cancels on hover in the engine, not in a screen: `Input` cancels an armed pointer press
whose `hit` leaves its arm (`input_tests.rs`, `a_pointer_press_is_cancelled_when_the_hit_leaves_its_arm`),
so a screen reports nothing about its focus stop moving and none keeps a path for it.

A screen that builds a `TableView` ships a `fit_report` test over its REAL builder (extract a pure
builder that takes its inputs as arguments, as `preferences::field_form` and
`settings::root_form` do) across `i18n::SHIPPED`, asserting `TableView::app_fit_failures` is empty.
Text that comes from a server or a user is marked with the `server_*` builders so it is exempt;
never mark the app's own fallback strings. See `rust-modules/ui/src/CLAUDE.md`, "Localization and shared reading
layout", for what to do when a string does not fit.

A page on a `ui::form::FormTable` (the Settings root, Playback / Audio & Subtitles, Language, the Legal index, Privacy & data and the item / account / more / track menus, the source list, the Alternate-sources panel and the Library menu)
focuses by IDENTITY: the element the engine holds, the `Fx::Remember` seat and the page's canon are
the row's `RowKey`, never its table index, so reordering the form moves no focus key. Such a page
answers `FocusMoved` with `family::form_focus` and draws through `TableScreen::keyed`; no screen keeps a parallel rows/actions vector beside its `TableView` any more (the Alternate-sources panel keys its rows by position, which is a dynamic list's documented key). A menu outside the Settings family has no `Dest` (an
uninhabited `Infallible`) and activates with `FormTable::activate`; it rebuilds with `set_or_open`, so a
vanished focused row reopens on the safe opening row instead of sliding onto a destructive neighbour.

Every Settings drill-down is a family-stack push through `family::form_activate` (a `Nav` row emits
`NavOp::Push`); a page never owns a private submenu or a `RoutePush` (grep-gated in
`settings_nav_structure_tests.rs`). A picker is its own page, `SettingsPage::Picker(PickerKind)`.

## The Watchlist surfaces (page, catalog page, disc, menu rows)

`docs/watchlist.md` is the map; these are the rules that bind a screen here.

- **A screen asks, the application writes and pulses.** The Watchlist page, the Detail bookmark disc,
  the item menu and the catalog page name no store: a press is an `Action` / `AppFx`
  (`WatchlistAdd { hex }`, `WatchlistRemove(guid)`, `GoToCatalog(guid)`, `CatalogOut`) and
  `app::input::apply_item_action` queues the write on the Watchlist store and pulses the strip's
  bookmark tab. A screen flips nothing itself: the store's intent ledger flips the face and puts it
  back when plex.tv refuses, and the app hands the screen its new view-model either way. Never call
  `ChromeSnapshot::pulse` from here.
- **The catalog page is not a Detail mode** (`catalog.rs`). It never reads `metadata::current()`
  (a source test asserts it), has no Play, Restart, Also available, Mark watched, extras, seasons,
  cast or Related, and holds ONE focus element, the primary pill (`Add to Watchlist` outline /
  `On Watchlist` filled). Its view-model (`CatalogViewModel`, local `Membership`, `Status`, `Load`)
  is the screen's own, with `From` impls from the data types, so it is drawn and tested without a
  store. It shares hero geometry through `plx_appkit::hero_layout` (the same rung constants and pill
  width the Detail action row spends); do not copy them, and do not name `detail` from here. The
  Trailer button the board DetailNotOnServer draws is **omitted in v1**, an owner default (Q4).
- **Unknown membership draws the Add face.** Detail, the item menu and the catalog page all treat
  `Membership::Unknown` (list unread, plex.tv unreachable) as Off, because the add is idempotent; an
  item-menu card with no `Face` offers no Watchlist row at all.
- **The menu captures identity at open.** `item_menu/watchlist.rs` builds a `Face { hex, member,
  kind, from_episode }` when the menu opens, so a membership flip behind it cannot change which title
  a row acts on. `ItemMenuKind::Catalog` rows are `Go to ...`, then Add or Remove; the menu opens on
  the go row, and Remove is `destructive(true)`. `Action::has_subject()` (not `!rk().is_empty()`) is
  the activation guard, because `WatchlistAdd` has no ratingKey.
- **The hero disc is second and unlabelled** (`detail/hero.rs`: `ELEM_WATCHLIST`, `HeroCtl::Watchlist`;
  the face, glyphs, `flip_frame` and the `Disc` that owns the membership, flip spring and press flag are
  `plx_appkit::watchlist_disc`, shared with Home's hero). There is one element id for both faces so focus survives a flip,
  it is always the circle width so it never unfurls, and it is not a focus anchor in full-trailer
  mode. Its guid must be `plex://movie|show/<24 hex>` or it is absent: `HeroSet::watchlist` is
  set from `catalog_hex_of_guid` and `hero_ctls` omits the disc otherwise. When the fill snaps and
  when it flips is `Disc::set`'s one rule (the first answer snaps; a change from a known face, or one
  answering the user's own press, plays the flip); a screen never copies it.
- **Home's hero carries the same disc** (`home/mod.rs`: `HERO_WATCHLIST_ELEM`, `HeroDisc`). A hero
  control's SLOT (`Located::Hero(slot)`: 0 Play, 1 Info, 2 disc, the `CtlPop` index) is not its
  POSITION in the row (`hero_slots`: Play, disc, Info); neighbours, the focus extent and the hit
  stops all go through `hero_slots` / `hero_rects`. The disc exists only for a hero row with a known
  catalog id; the application feeds `set_watchlist(hex, membership)` for the title shown, and `HeroDisc`
  keeps only which title the shared `Disc` is for: a rotation `reset`s it onto the new face, and the
  same title's change is `Disc::set`'s rule.
- **New widths, rungs and ids come from the existing systems.** The catalog page's 36 px board line
  is drawn at `HEADLINE` because the type ladder has no 36 rung; where a board and the shipped
  tokens disagree the tokens win and the difference is reported, not patched in with a literal.

## Building a card page

A new screen that shows media cards is a `plx_ui::cards::Stack` page; `collection.rs` is the
smallest worked example.

- Implement `CardSource<H>` (`len`, `elem`, `index_of`, `art`, `label`; the rest default) once per
  section's content, over a borrowed view of the store.
- Implement `StackPage<H>` on the page's content type. Required: `type Key`, `type Cards<'a>`,
  `revision` (a counter that moves whenever anything the sections read moves), `sections` (the
  `SectionSpec`s, in order), `fallback` (the order focus falls back through when a section empties)
  and `cards` (a section's `CardSource`, `None` while it has no content). Optional hooks: `pending`,
  `recover`, `card_has_menu`, `elem_of`, `plain_len`, `plain_elem`, `plain_step`, `focus_rect`,
  `element_rect`, `seat_override`, `reveal_margin`, `shelf_foot`, `wide_extent`, `plain_group`,
  `reveal_with`, `draw_heading`, `custom_draw`. Stacked shelves are adjacent sections and the air
  under one is its `shelf_foot` (Search, Person), never a `Custom` gap section between two: a shelf's
  caption stops short of the next shelf's lifted heading, which `Stack` reads off the section that follows.
- Hold the `Stack` in the screen, feed it every event with `stack.on(&page, ev, cx, fx)` and draw
  through `stack.view(&page)`. Get `Focusable` from `plx_ui::focusable_via_view!`
  (`focusable_via_view!(Screen, H: [Bounds], view)` for a screen generic over its host).
- Opt into the hold hint with `Stack::hold_hint()`; `card_has_menu` says which cards a hold
  actually opens a menu on. The hint is once per INSTALL per surface (`plx_ui::hold_hint::Surface`: a card, and Detail's season pill, which takes a shorter dwell); the screen only names the surface and focus in `HintInput`, and the application persists what was taught (`app/hint_taught.rs`, the `hold_hint_taught` Install preference).
- Register a conformance mount: a `cards_harness.rs` child module of the screen exposing
  `mount` (see `collection/cards_harness.rs`), and a row for it in `table()` of
  `cards_conformance_tests.rs`; the shared drivers live in `ui/src/cards/conformance.rs`.
- A screen never names L0 (`CardRow`, `GridPop`, `GridBands`, `cards::paint_visible`, ...): the
  `cards` gate in `ci/check-deps.sh` fails it. Everything a layout needs is re-exported from
  `plx_ui::cards`; which cards the grid actually paints (the Library's poster lookahead needs the
  rows it does NOT) is `Grid::painted`.

## Verifying a screen change

Captures are the check — see the `ui-sim` and `which-tier` skills, and `../../ui/src/CLAUDE.md`'s
"When you're done". Two traps specific to this directory:

- A screen's `name()` is the heartbeat word and must stay byte-identical to the route word the
  test manifest selects on (spec §15.3). Renaming it silently unselects every fps scene for that
  screen.
- The person page's bio provider 401s on an injected token, so a headless boot cannot reach it —
  sign in, or drive that leg a different way.

## Under posters settings page (library redesign)

`SettingsPage::UnderPosters` (ScreenId 113, `settings_under_posters.rs`) is pushed from Settings >
Libraries, which now has two rows (Favorite libraries, Under posters). The page has three rows (Title switch, Details › with the current line on the right, Reset to default) and a framed preview (a still row of three posters from the library the Library last showed, `cards::SampleRow`, the middle one lifted and captioned, a blank face carrying no word); Details pushes `SettingsPage::UnderPostersDetails` (ScreenId 114, `DetailsPage` in the same file), the picker of the one details line (Nothing first, a sample on each row), which pops on a durable write. `settings::caption_policy_of`
maps the stored `UnderPosters` to the ui `CaptionPolicy`. **EVERY poster tile reads that one policy through
`registry::captions`** (`policy()` with the one `TEST_UNDER_POSTERS` test override, `label()` the one builder of a focused
poster's `[title, details]`, `shelf_label()` for Home's and Recommended's shelves): the Library grid, the Collections page, the Watchlist,
Home's shelves, Recommended's poster shelves, **Search's poster rows** (Movies, Shows, Collections; a result from a server that is not the
household's own is credited after its details line, `CaptionFacts::source`), **the person page's filmography**, **Detail's Related and
collection shelves** and **the Collection screen's members** (an episode member's poster already prints its "S3 · E4" mark, so its caption names the show instead: `CaptionFacts::address_on_art`). A grid page hands its grid the policy through `StackPage::caption_policy`
(Stack hands it to a `Body::Shelf` as well as a `Body::Grid`); Detail owns its shelves and sets the policy on Related and the
collection each step. **Landscape tiles and headshots are not posters and do not read it**: the Library's Episodes listing, Search's Episode stills and Person
headshots, Detail's Extras and Cast, and Recommended's landscape rows keep the legacy block under every setting (the Episodes listing's grid is handed
`None` for its policy, so its band is the legacy 114 px one the two-line label needs; every landscape block is made by `captions::still_block`: `still_label` words an episode's, Extras hand theirs in). A shelf is
handed the policy (`Shelf::set_caption_policy`) and reserves,
opens and reveals its band from `caption_policy::band_h`; a tile that carries a state keeps it (the Continue Watching deck's
time left is its details line, a filmography poster's character is its details line, the amber play mark leads the first line drawn while OK plays, a Watchlist title's release date or
"Server offline"), and with Title Off and Details set to Nothing no band opens (a state line is dropped with the details line), **except the Continue Watching deck** (the owner's 2026-10-22 call): its progress line is the tile's state and is always shown, Nothing included, so the deck shelf is handed `captions::deck_policy(policy)` (Home's and Recommended's alike; `shelf_label` words its tiles by the same value) and keeps a one-line band (two with the Title On), per shelf so it never changes as focus walks the row.
**One rule for every kind of tile: a line the setting turns on is never drawn empty** (`caption_policy::caption_lines`): the title line is the name a reader
knows the tile by (`tile_facts::caption_facts`: a season by its SHOW, an episode by its own title, a show or movie by its title), and a
details kind the tile has no facts for (a season has no length, often no genre or rating) says the tile's identity instead: a season "Season 6" (its own title,
else "Season N", else "Specials" for the season numbered 0), an episode "S1 · E8" (the app's ONE address spelling, `fmt::episode_address`), a show or movie its year, a collection its size, and for an episode, movie, show or collection
with not even that the KIND word ("Episode", "Movie", "Show", "Collection") as the last fallback, so no band opens over nothing and no per-tile band is needed (a per-tile band
would move the rows below it as focus walks a row). Home's season tiles ("Recently Added TV") and the Library grid's seasons read it the same way; the
Library grid's episodes keep their own wording (title and release date). **A collection follows one rule on every surface**: its details line is its
size under every details kind but Nothing, the Title switch governs its title line, and a name its face cuts (`registry::tile_facts::face_cuts_name`) rides the details line
("Name · 7 films") when the title line is off; the Collections page has no caption transform of its own. The Library page is the full grid under one
header row (`library/header.rs`); the hub shelves live on the Recommended page (`library/recommended.rs`), which the header's Recommended control pushes.
