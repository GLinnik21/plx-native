# screens/ — the application's owned screens (read before adding or changing one)

Every `Screen` impl the dispatcher mounts, steps, focuses and draws lives here, across
this directory and the `detail/`, `home/`, `library/`, `player/` and `search/` families. This is
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
  Library's grid is usually prepared before its shelves land, so its first seat was the grid's
  heading; the shelves then committed above it and the first DOWN went on into the grid
  (`LibraryScreen::provisional`). A restore is only what the reader chose: the engine remembers
  the page's own seats too, so the page keeps their provenance (`LibraryScreen::placed`). Home already has this shape (the hero seats when its action row
  arrives, while focus is still on the strip); Search seats its field, which never arrives async,
  and Person and Detail keep no self-seat for a landing to strand.
- **The grid absorbs an index landing, the page shifts its scroll.** The Library's All grid is a
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
  `reveal_with`, `draw_heading`, `custom_draw`.
- Hold the `Stack` in the screen, feed it every event with `stack.on(&page, ev, cx, fx)` and draw
  through `stack.view(&page)`. Get `Focusable` from `plx_ui::focusable_via_view!`
  (`focusable_via_view!(Screen, H: [Bounds], view)` for a screen generic over its host).
- Opt into the hold hint with `Stack::hold_hint(Kind)`; `card_has_menu` says which cards a hold
  actually opens a menu on.
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
