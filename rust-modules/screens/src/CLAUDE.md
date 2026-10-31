# screens/ — the application's owned screens (read before adding or changing one)

Every `Screen` impl the dispatcher mounts, steps, focuses and draws lives here, across
this directory and the `detail/`, `home/`, `item_menu/`, `library/`, `player/`, `search/` and `watchlist/` families. This is
the APPLICATION half of the restructure; the library half is `../ui/`, whose
[`CLAUDE.md`](../../ui/src/CLAUDE.md) carries the token rules, the architecture and the rendering
policy. Read that one first — its four rules bind here too.

**`lib.rs` (the crate root, `mod.rs` before Split 13) and `registry.rs` already document themselves.** Both carry substantial `//!` docs and
they are the authority on the module list and the application bundle. This guide holds only what
they do not: the rules that span screens.

## Sans-IO

A screen is a machine: it starts no worker, reads no process-global registry and carries no
`cfg(test)` fork. It asks for work by pushing an `AppFx` and reads what the stores publish.

@../../../docs/sans-io-guide.md

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
Libraries, which now has two rows (Favorite libraries, Under posters). The page has four rows (Title switch, Details › with the current line on the right, Sort value switch, Reset to default) and a framed preview (a still row of three posters from the library the Library last showed, `cards::SampleRow` in a `cards::SampleFrame`, the middle one lifted and captioned, the outer two clipped by the frame and dimmed, a blank face carrying no word); Details pushes `SettingsPage::UnderPostersDetails` (ScreenId 114, `DetailsPage` in the same file), the picker of the one details line (Nothing first, a sample on each row), which pops on a durable write. `settings::caption_policy_of`
maps the stored `UnderPosters` to the ui `CaptionPolicy`. **EVERY poster tile reads that one policy through
`registry::captions`** (`policy()` with the one `TEST_UNDER_POSTERS` test override, `label()` the one builder of a focused
poster's `[title, details]`, `shelf_label()` for Home's and Recommended's shelves): the Library grid, the Collections page, the Watchlist,
Home's shelves, Recommended's poster shelves, **Search's poster rows** (Movies, Shows, Collections; a result from a server that is not the
household's own is credited after its details line, `CaptionFacts::source`), **the person page's filmography**, **Detail's Related and
collection shelves** and **the Collection screen's members** (an episode member's poster already prints its "S3 · E4" mark, so its caption names the show instead: `CaptionFacts::address_on_art`). The Library's movie, TV show, season and episode grids are the exception while they show their sort value (Sort value On, sorted by something that has a value for that kind of row): it draws the one value line described below instead. A grid page otherwise hands its grid the policy through `StackPage::caption_policy`
(Stack hands it to a `Body::Shelf` as well as a `Body::Grid`); Detail owns its shelves and sets the policy on Related and the
collection each step. **Landscape tiles and headshots are not posters and do not read it**: the Library's Episodes listing, Search's Episode stills and Person
headshots, Detail's Extras and Cast, and Recommended's landscape rows keep the legacy block under every setting (the Episodes listing's grid is handed
`None` for its policy, so its band is the legacy 114 px one the two-line label needs; the one thing that replaces that block is the Episodes view's
sort value line while it shows one, below; every landscape block is made by `captions::still_block`: `still_label` words an episode's, Extras hand theirs in). A shelf is
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
header row (`library/header.rs`); the hub shelves live on the Recommended page (`library/recommended.rs`), which the header's Recommended control pushes. The header's right cluster is the board's: Collections › and Recommended › are places (a chevron), a divider (no stop) separates them from the sort control, which is the sort icon, the chosen criterion and a down chevron; "Sort & Filter" is the control's name, the identity the row's swap matches it by, drawn nowhere: not on the control and not as a heading (the menus' own headings are Show, Sort and Filter). The criterion has no reserved slot (the control ends at the row's right edge and the places slide as its width changes while the control keeps its right edge and its chevron, the old criterion fading out at its own place and the new one in at its own; only the library names' budget is sized from the widest criterion), and the Collections page's Sort and the Watchlist's control are the same `HeaderItem::sort_control` (`plx_ui::page_header`; the Watchlist's criterion is its sort alone, a Type or Show filter shows as its "N of M" read-out, and its row begins the same `HeaderSwap` from `SharedRow::get_replacing`). Once the page is scrolled the shared capsule carries the sort too ("Films 1,284 · Date viewed": `LibraryScreen::sort_value`, the string the control shows, queued edit included, handed to `scroll_chrome::capsule_parts` as its criterion; library and criterion in the primary ink, the count and the dot at 50% white (`theme::COUNT_FILTERED_INK`), the capsule as wide as its pieces).

The **Sort value** switch is also in `UnderPosters` (`sort_value`, saved per profile, On by default and
stored as its Off state, so everyone starts On). While it is On and a movie or TV show library's movies, shows, seasons or episodes (never its Collections) are
sorted by something that has a value FOR THAT KIND OF ROW (`registry::sort_info::shows_value(kind, key)`: not Title, Random,
the compound Show order Seasons and Episodes default to, or a sort the table does not word for the kind), every card shows
the value it is sorted by on ONE line under it
(`registry::sort_info::label`, drawn by `plx_ui::card_row::draw_sort_value`) and no title or details
line: CAPTION 24 regular at 80% white centred on the card's axis at rest (a poster's or an episode still's), bold white under the lifted
card when focused (a two-texture crossfade on the lift's own pop factor, so it snaps when the lift
does), rows 60 px apart (40 otherwise) with the focused row opening 26 px. The Library reads the
switch and the sort in `sync` (`LibraryScreen::values`, from `shows_sort_values(kind, listed, ..)`: the section kind and the
listing map to the row kind the listing holds, `value_row_kind`) and gives the layout and the grid the same
answer (`Layout::with_value_line`, `Grid::set_value_line`); with the switch Off, or a Title/Random
sort, the grid draws as it did before (an Episodes view keeps its legacy two-line caption and 114 px band). The Title switch does not change a value's band: the
value replaces the title and the details line, and the focused row opens 26 px whatever the Title switch says. Other poster surfaces keep the Title/Details policy
above. The values are Plex's own facts from the listing (no request is added): ratings as a
percentage beside the provider's mark (the shared `captions::rating_mark`, hung left of the number so
the number stays on the axis), `2h 14m`, `4K` with `+1` for a second version, `62.0 Mbps`, plays with their noun ("14 plays", "1 play": the
catalogue's plural message), the content rating, the year, for a show the episodes left to play ("9 episodes", the
catalogue's `browse.detail.episodes`; a show with none left draws NO line, while its slot keeps the 60 px pitch; a season is worded for the same count, its dates added and viewed and its plays), and dates as Today, Yesterday, `27 Oct` this year or `Oct 2024` otherwise,
in the interface language's month names (an English TV numeric-format locale does not override
Russian month names). Every space in a value is a no-break space; an empty value says what it means at 50% white and never bold:
"Never" under Date viewed and Plays for something nobody played, the en dash only for data the server did not send
(a show's Last episode added, `episode.addedAt`, is such a gap: the listing carries no date of its latest episode, so that sort is not worded and
the grid keeps its captions). **A sort the listing carries no fact for, for that kind of row, is not worded and shows NO line at all** (its rows stay 40 px
apart, its captions stay): never a column of dashes. The dash is for the one item of a worded sort whose fact the server did not send. An
episode is worded for every term a movie is, from its own row (its air date, never the show's; the two scores; length; plays; dates; media), a season for
Date added, Date viewed, Plays and Unplayed, a collection for none. A real PMS lists for Episodes the Show order, Release date, Critic and Audience rating,
Date added, Date viewed and Random (all but the Show order and Random read), and for Seasons the Show order and Random alone (`docs/pms-api.md`, "Sort[]"). The
episode view's cards are 4 columns wide, wider than a poster, so no value gains a truncation risk there; the fit test runs both widths in every shipped language. A line that does not fit drops its `+1`, then its mark, and is never cut. **Today is an
input, not a read:** a screen reads no clock and fetches no date; the application's loop publishes the
calendar date once per frame (`Bridge::publish_today`, read from the platform at most once a minute through
`app::clock::today`, the loop its one caller) and `LibraryLike::today` hands it in from the frame's views,
and `CaptionCache` keys its labels on it so "Today" rebuilds at midnight. The settings preview
illustrates the mode as a library sorted by Year while the Sort value row has focus: each sample's own year
(through `registry::sort_info::year_value`, the Year sort's own wording) under its tile and a "Sorted by Year" caption at the frame's foot. The frame is the
board's (`cards::SampleFrame`: 660 x 556, Library cards 35 px apart so the outer two are clipped by it and drawn at 35%, their
value lines with them); the Details picker's frame says "The library you last opened" at its foot, and its Video choice
carries the one detail line the table can hold without a focus-only mechanism. It never adds sort choices.

## The Library's rail: letters for a title sort, bands for a value sort

`library/rail.rs` (`RailPart`) is ONE rail with one logic: an element is a place in the listing (`starts`, the index
of its first item), an element no item starts is dimmed and skipped by focus, moving focus inside the rail scrolls the
grid to the element's first card at once (`LibraryScreen::follow`, no OK), the grid has no focus while the rail has it,
and Left returns to the grid. A title sort (ascending) has the A-Z letters (`ListingView::rail_available`); a sort with
a value ladder has its BANDS (`ListingView::bands_available`: rating `95% 90% 85% 80% 75% <75%`, duration `3h+ 2½h 2h
1½h 1h <1h`, unplayed `12+ 10 8 6 4 2 1`, plays `10+ 5 3 2 1`, and the CALENDAR sorts: date added, date viewed and
last episode added (newest first only) read `Today Oct Sep ... Jan 2025 2024 ...`, release date reads `2026 2020s
2010s ...`), never both. A calendar rail needs the day, which the Library's ask hands the data layer
(`LibraryLike::today`), and its bands follow it: a day turn re-plans them, and the previous day's bands stay on screen
until the new day's are read (`SecState::shown_bands`), so a focus seated in the rail is not lost to a gap; a key the new
plan no longer holds is seated on the first band with films (`RailPart::reconcile`), never on an empty one. What differs:

- **Labels** are `library/rail_bands.rs`, built from the data layer's `Mark` tokens with `fmt::percent`, the locale's
  numbers and the catalog's `library_rail.hours` and `library_rail.hours_or_more` (the language's own short hour unit,
  spaced as `core.duration_hours` spaces it, `½` for a half hour). `<` is written by the code, and so is the `+` of the
  count ladders (`12+`, `10+`); the Duration ladder's `3h+` is `library_rail.hours_or_more`, a catalog string, so a
  language can put the `+` after its own unit. A calendar band is `library_rail.today` (the word under a poster, or
  its abbreviation where that is too wide for the column: French `Auj.`), a month is `LocaleContext::month_short`
  (the stand-alone abbreviation), a year is its plain digits (never the grouped `locale.number`) and a decade is
  `library_rail.decade` over the digits of its first year and the last two of its last (`{decade}` and `{end}`; the
  English entry lists `end` under `"optional"`, so a language reads the ones it needs): `2020s`, `2020er`, `años 2020`,
  `lata 2020.`, `2020-е`, and `2020–29` for a language whose phrase does not fit (French `années 2020` is 137 px;
  Hungarian's suffix alternates by vowel harmony, `2020-as` but `2010-es`). The widest label the column holds is
  the track limit less the least pill air each side (116 px at `MICRO`), so a new language's Today and decade
  strings are checked against it by `rail_bands`' fit test, which loops every family of every shipped language
  through the real measure. Focus identity is the token, not the text.
- **Geometry** is `layout::BandRows`: centre x 1834 (the letters keep 1844), rows 40 px apart (the letters' pitch is
  `layout::rail_pitch`: 32 at most, squeezed toward 26 for a long alphabet), the first
  at the grid's top (244), `BAND_STEP_GAP` extra before the rows a scale change names (`rail_bands::steps`, from the
  marks: between a month and a year, a year and a decade, in either direction of the sort; `Today` is on no scale, so
  `Today 2025` on the first of January has none), the
  track as wide as the widest label plus the pill's 12 px each side (never under the letters' 44, never into the
  posters: a label wider than the limit allows 12 each side gives air back, `layout::band_pill_pad`, down to 4).
  A calendar rail can hold some 35 rows (twelve months and every year back), more than the column's
  window: it scrolls in whole rows, the driving band one row clear of both fades, like a long alphabet. The grid
  does not move for it.
- **Ink**: a band that holds items is `TEXT_SECONDARY`, an empty one the same at 25%, the CURRENT band (the one the
  grid's card is in, none when the card is among the items the sort leaves without a value, which lie before the first
  band or after the last) `TEXT_PRIMARY`, and the focused band wears the white focus pill (38 high, built with
  `widgets::focus_pill`, filled as the letters' disc is). There is no position bar or marker.
