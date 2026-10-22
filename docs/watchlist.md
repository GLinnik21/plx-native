# Watchlist

The Plex Watchlist as it ships: the plex.tv list, the strip tab and page that show it, the bookmark
disc and hold-menu rows that change it, and the catalog page for a title no server holds. The design
record is `docs/watchlist-plan.md` (the plan and its appendices); this file is what the CODE does,
so where the two differ this one is right.

**The rule that frames everything: the Watchlist is plex.tv account data, not PMS data.** It lives
on `discover.provider.plex.tv` (and `metadata.provider.plex.tv` for one item's state), is keyed by a
24-hex catalog id, and is written with the active profile's plex.tv token. A PMS ratingKey and a
catalog id name the same film in two unrelated id spaces; the only join between them is the guid
(below). Nothing here is read from, or written to, a server.

## Where it lives

| piece | module | role |
|---|---|---|
| client | `plex::watchlist` (`impl AccountClient`) | paged list, `released` list, `user_state`, `catalog_item`, add/remove `PUT`; injectable request closure |
| guid lookup | `Client::find_by_guids` (`plex/library.rs`) | which guids does this server hold |
| ids and ledger | `data::watchlist` (`model`, `ledger`, `order`) | rows, match table, `TileState`, intent ledger, comparator |
| store | `stores::watchlist::WatchlistStore` | main-thread state plus a mailbox adapter; `StoreId::Watchlist` |
| catalog | `data::catalog`, `stores::catalog::CatalogStore` | one Discover-only item; `StoreId::Catalog`, appended last |
| screens | `screens::watchlist`, `screens::catalog`, the Detail and Home hero discs, `item_menu/watchlist.rs` | the page, the catalog page, the disc, the menu rows |
| app | `app/watchlist.rs`, `app/input.rs`, `app/chrome.rs` | feed, presses, the tab pulse, the routes |

Layers: `data` names `plx_plex` and below, never `net` or `media`; screens never name a sibling
screen or the app; the application owns the pulse and the routes (`docs/module-layers.md`).

## Wire facts (redacted: no ids, tokens or addresses)

Measured with a restricted managed profile's own token; nothing was written to a household account
outside one add/check/remove/check pair on that profile.

- **List:** `GET /library/sections/watchlist/all?includeCollections=1&includeExternalMedia=1&sort=watchlistedAt:desc`,
  paged with `X-Plex-Container-Start` / `-Size`. Size above **100 answers 400**, so the client pages at
  100 and follows the container's `totalSize` (or a short or empty page) to the end: there is no page
  cap. The response carries `X-Plex-Container-Total-Size` and a weak `ETag`; a request with
  `If-None-Match` answers 304.
- **ETag (NET1, shipped).** `net::Resp::etag` is the final response's `ETag`, trimmed and otherwise
  verbatim (the weak `W/` prefix and quotes stay), read by a `CURLOPT_HEADERFUNCTION` that keeps only
  that header; a redirect hop's validator never survives into the final answer. The store keeps the
  page-0 `ETag` of the last COMPLETE read of the list (every page arrived) and the next read sends it
  as `If-None-Match` on **page 0 only**: a 304 on a later page is UNVERIFIED against the service, so
  later pages and the `released` variant never send it. A 304 has an empty body, reads no page and
  keeps the rows held (the same commit a whole read makes at its end); the `released` set is still
  re-read, since a title's release moves it without moving the list. The validator is dropped on a
  profile switch, sign-out or store reset (a new epoch), when a write is spawned (the next read is a
  whole one, so a write's own re-read never gets a 304 for the list it just changed), and for a read
  that was cut short or that a write overtook. A read the service answers without an `ETag` is always
  read in full.
- **Released:** the same path with `released` in place of `all`. `unreleased` answers 404 and is never
  used. "Coming soon" is "listed and not in the released set".
- **List rows carry no `watchlistedAt`**; the date-added order is the server's order, and the reverse
  is its reverse (the client never sends `watchlistedAt:asc`).
- **Per-item state:** `GET /library/metadata/<hex>/userState` answers an ARRAY under `UserState` on
  discover and an OBJECT on `metadata.provider`. `watchlistedAt` is present only while the title is on
  the list. The client tolerates both shapes.
- **Item record:** `GET /library/metadata/<hex>` carries title, year, summary, genres, art, `guid`
  (`plex://movie/<hex>` or `plex://show/<hex>`), a `Guid[]` of external ids and a
  `metadata-static.plex.tv` poster. **No `UltraBlurColors`** was observed on a Discover item.
- **Writes:** `PUT /actions/addToWatchlist?ratingKey=<hex>` and `.../removeFromWatchlist?ratingKey=<hex>`,
  both a body-less `PUT`, both 200 with an empty container. They are idempotent, which is why an
  unknown membership may show the Add face.
- **The `ratingKey` of a Discover item is the 24-hex id**, not a PMS key, and `guid` ends in the same
  24 hex digits. `catalog_hex_of_guid` decodes that tail once into `[u8; 12]`; any other guid form
  (`imdb://`, `local://`, a non-hex tail) gives no id, and such a title has no disc and no menu row.
- Responses are `Cache-Control: no-store`.

## The token rule

The Watchlist is written and read with **the active profile's own plex.tv token**, never the owner's
`account_token` and never a per-server PMS access token (a PMS token is refused on plex.tv, 401). It
comes from `session::plex_tv_credential(&UserRef)`, captured on the MAIN thread at spawn into a
`WatchlistAuth { client_id, credential, user, generation }` that the worker owns. New sessions carry
the profile's token explicitly; the one fallback is a proven legacy owner session. A managed or
unknown legacy profile therefore has **no credential**: the page shows the account read-out
(`Status::NoAccount`), `CatalogStore` reports `Unreachable`, and nothing is fetched.

A landing stamped with a stale generation or another profile's identity is dropped; a profile switch
rotates the epoch and resets the store (the catalog store re-opens under the new identity). Logs name
the status and the endpoint SHAPE only: no token, and no id beyond what is scrubbed.

**A debug build does not write unless someone switched it on.** In a `devtriggers` build a `PUT` is
refused and logged (`WatchlistError::Refused`) unless the loopback stand-in is armed (the stand-in then
takes the write) or the owner's live switch is on. "Armed" is `account::discover()`, the ONE Discover base
the person page shares: `/tmp/plxnative-discover`, else the loopback `/tmp/plxnative-plextv` already
redirects plex.tv to (the mock answers both on one listener). The UI flips and then reverts, and the page header says `Debug: watchlist write blocked` until a write
goes out (`header::dev_blocked_text`, compiled only into a `devtriggers` build, so nightly and stable keep Q8's
silence). Release builds write. Host tests never reach curl: they drive the request closure.

**Reads are refused on a dev-token boot too.** A dev-token boot (`plxnative-token`) with neither
stand-in armed has its READS of the provider refused and logged once per endpoint, the person page's profile
and credits included, rather than reaching the real host with a server token. That is a fixed decision
(fail closed), and the cost is accepted: such a boot shows no person biography and no filmography
credits; arm the stand-in to see them. A refused person read settles its page at once (no retry). A
refused Watchlist or catalog read (reachable only after an in-process sign-in: a dev-token boot with no
account seated makes no request at all) lands once as the unavailable read-out, never as an empty list,
and is not re-asked on a timer.

**The live switch** is `plxnative-watchlist-live` in the install's runtime root (on the television
`/tmp/com.beb.plxnative.debug/plxnative-watchlist-live`). It is read once at boot, which logs
`watchlist: LIVE writes enabled by trigger`; each write that goes out then logs
`watchlist: LIVE PUT <endpoint shape> -> HTTP <status>`. The owner creates it from their own terminal
and relaunches the app; a reboot clears `/tmp`, and `tools/tv-session.sh up` and `tests/run.py` sweep
every `plxnative-*` trigger, so any agent run turns it off again. **No agent creates it unless the owner
asks in that session** (AGENTS.md); `.claude/hooks/watchlist-live-guard.py` refuses a Bash command that
would, unless it carries the owner's `PLX_WATCHLIST_LIVE_OWNER=1` prefix.

## Matching a title to a server (guid matching)

A Watchlist row is a Discover guid. To play it, the app asks each Reachable server whether it holds
that guid: `Client::find_by_guids(kind, guids)` issues one
`GET /library/all?type=T&guid=a,b,c` per chunk of at most **40** guids per server per kind (`T` 1 for a
movie, 2 for a show). Only the primary `plex://` guid matches (`imdb://` and `tmdb://` answer 0), the
guids are percent-encoded individually and joined with a literal comma (a repeated `guid=` param keeps
only the last), and a guid the server lacks is silently absent. An error is "could not ask" and is
never read as "absent". See `docs/pms-api.md` for the request.

The result is a per-profile match table held in memory (it is not persisted across launches; owner
question Q5). It is incremental for new guids and fully recomputed when the server roster changes. The
copy a tile opens is chosen current server first, then owned, then shared, Reachable only.

**The other direction.** A `PmsMovie` carries `guid` and `show_guid` (an episode's `grandparentGuid`, a season's `parentGuid`: the server sends a season no `grandparentGuid`),
each decoded once to `[u8; 12]`. The Detail and Home hero discs and the hold-menu rows use them to name the catalog
id. `resolve_catalog_hex(known, fetch)` (`plex/models.rs`) is the helper for a row that lacks one: it
fetches `client.metadata(rk or show_rk).guid` only when the id is not already known. A hold-menu card
whose row carries no guid asks for it (`WatchlistStore::want_hex`, a worker reading the item's own
server record, an episode's SHOW record): the menu being opened omits the row, the next one offers it
once the record named a portable id (tests in `app/watchlist_press_tests.rs`). The Detail disc reads
the guid of the loaded record; the Home hero's disc reads the hero row's own (`PmsMovie::watchlist_subject`) and, unlike the menu, does not ask the server record for a missing one: its presence moves the focus row, so it appears only for a row that already names a title. Real
PMS guids are hex; the mock's default placeholder guids are not, which is why `mock_pms` has a
`{"guids":"all"}` mode for menu captures.

## The six states

`TileState` (`data/src/watchlist/model.rs`), derived at commit and again when the registry's
`facts_gen` or a server's reachability moves, never per frame:

| state | meaning | mark | OK opens | hold menu |
|---|---|---|---|---|
| Available | released, matched on a Reachable server | PMS tick / progress | Detail of the chosen copy | card kind plus Remove |
| ComingSoon | not released, unmatched | clock | Catalog | Catalog rows plus Remove |
| ComingSoonMatched | not released, but a Reachable server has it | clock kept | Detail | card kind plus Remove |
| NotOnAnyServer | released, every server on the roster answered "absent" | circle-slash; a 55% wash at rest, 25% on focus | Catalog | Catalog |
| ServerOffline | matched earlier, every matching server now Unreachable | server-slash, cached progress | Catalog | Catalog (no Play or Mark watched) |
| Unknown | no match and some server has not answered | none | Catalog | Catalog; excluded from "On your servers" |

OK and hold read the drawn, committed `row.state`. An open Detail whose server dies keeps today's
failure path. "Absent" needs the whole roster to have answered, so a silent server never produces
"Not on your servers".

## The write ledger

Writes are the Watchlist's own (not view-state: `ViewLedger` is keyed on `(ServerId, ratingKey)` and
this id lives on another service with another token). `IntentLedger`:

- A press flips the row at once (optimistic).
- One ordered write worker per epoch; presses on the same id coalesce and the newest intent wins.
- An intent beats any list read **started before** the write's 200: a landed write renumbers the entry
  above the press, so an older read is overridden and a later one is not.
- A refused or unreachable write reverts the flip to the last known membership. **No toast anywhere**:
  the button face is the only feedback.
- A burst of landed writes owes one list re-read two seconds later.
- Membership is read through `WatchlistStore::view().is_member` and `status()`: **On** when the title
  is a member (intents in flight included), **Off** when the list is `Ready` and it is absent,
  otherwise **Unknown**, which every surface draws as the Add face.

**Surfaces.** The Detail hero's bookmark disc is second after Play/Resume (`ELEM_WATCHLIST`, a circle that never unfurls and carries no name, focused or not — owner, 2026-10-20: an icon-only control is its glyph and its focus fill —
outline when off the list, filled when on; the fill rises from the bottom with a 3 px ribbon drop on a
flip, not on the first answer). It exists only for an item whose guid is `plex://movie|show/<hex>`
(`HeroSet::watchlist`).
The **Home hero** carries the same disc, second after Play/Resume and before Info (Play, disc, Info, with
`HERO_CTRL_GAP` = `CONTROL_GAP`, 20 px, between controls; the 12 px `HERO_ICON_GAP` is inside the Play pill). It is present when the hero row stands for a title
(`PmsMovie::watchlist_subject`: a movie or show itself, a season or episode its SHOW, "Add Show"
semantics) with a known id. The application feeds it the membership of the title the hero shows
(`HomeScreen::watchlist_hex` / `set_watchlist`, `app::watchlist::catalog_feed`); it flips only on a real
membership change of that title, never on open or when the hero rotates to another title. A press is the
same `AppFx::ItemMenu` write as Detail's. Face, glyphs, flip and the `Disc` state that decides when the flip plays are `plx_appkit::watchlist_disc`, shared.
The item menu offers `Add to Watchlist` / `Remove from Watchlist` for a movie and `Add Show to
Watchlist` / `Remove Show...` for an episode or season, with the identity captured when the menu
opened. A catalog page's primary pill reads `Add to Watchlist` / `On Watchlist`. An add from a menu
pulses the strip's bookmark tab (about 1.35x and a glow, decorative, no page image held): the
application does it (`ChromeSnapshot::pulse`), never a screen.

**Accepted deviation, action keying.** `Action::WatchlistAdd` carries the 12-byte `{ hex }`; the
`WatchlistRemove` and `GoToCatalog` actions carry the discover guid string and decode the hex from
its tail at press time, so a guid with no decodable tail does nothing instead of being refused at
open. The plan's `{hex}` keying for all three is not implemented; the owner has not yet confirmed
the difference. The Remove row's wording is `Remove Show...` for an episode (plan 1.8), not the
brief's `Remove Show from Watchlist`.

**plex.tv unreachable.** The page shows the full-screen read-out with Retry (a populated list is never
blanked). Detail, Home hero and catalog controls draw the outline face when membership is unknown, the item-menu
row is omitted when there is no face to offer, and a press flips then reverts. Reproduce with
`/tmp/plxnative-nowan` or the failing-closure tests.

**The count (Q7, decided: option D; reshaped onto the Library's header row).** The page header is the
Library's one row (`plx_ui::page_header::PageHeader`; `watchlist/header.rs`): the title "Watchlist" on
the selected plate at the left, and at the right the control "Sort & Filter · <value> ▾". The list's
count is a filter read-out and nothing else: "{shown} of {total}" (the Library's sentence,
`library.header.filtered_of`, numbers grouped as it groups them) stands ahead of the control while a
Type or Show filter narrows the list (Sort alone does not), including "0 of N" beside the
filtered-empty read-out, and there is no "{n} titles" at rest. It is derived from the list state (it
holds no state of its own), absent until the list has landed and on the empty-list and unavailable
read-outs, and follows the cards when the grid's landing STARTS MOVING: a Remove's on the first feed
after the hold menu's veil drops below `CLOSING_GONE`, a Sort & Filter change's on the frame after the
click (the click's own frame draws the frame before it), never at the commit, so the number and the
tiles move on one frame. It stays under the Sort & Filter menu's scrim; only the control is lifted.
The row's pills are the Library's, one component. With one focus stop the focus pill has nowhere to
travel from: it lands on the control and fades in on the top bar's alpha spring, then pops on the
page's `CtlPop`; the title is plain text, no plate (`HeaderItem::title`, design review S10; `plx_ui::page_header`; the page holds
the row's `TabStrip` and `CtlPop`, which `WatchlistScreen::SHAPE` carries and its tick steps), and the Sort & Filter
panel is as wide as its longest label or as the control's focused pill, whichever is wider
(`TableView::menu_panel_width_from`, the rule the Library's Sort & Filter hangs by too).

**The grid and its captions follow the Library.** Six columns of 250 x 375 posters from x 96, 285
apart (the last column ends on the row's right text edge, x 1771), the first row at y 244, a focused
row below the first resting at y 244 too. The caption under a poster is the global Under posters setting (Settings >
Libraries; `plx_ui::caption_policy`): the Title switch (default off) and the Details picker, and only
the focused row opens its band (0 / 80 / 112 px). A Watchlist row itself carries a title, a year and a
kind; the rest of what a caption words is the matched copy's, as the guid lookup's row said it
(`ListingFacts`, `app/watchlist.rs`), so a playable title reads like any poster ("2001 · 1h 40m",
"Drama", "4K · HDR", "PG-13 · 7.8", "2019 · 3 seasons") and a title with no copy falls to its year (or,
under a picker the row has nothing for, to its kind). An unplayable title's state sentence ("Coming soon", "Not on your servers", "Server
offline") takes the details line whatever the picker shows, except Nothing, which shows no line at all.
A focused row below the first rests with its posters at the first row's y, 244 (`GridSpec::resting`,
`poster_grid::rest_scroll`: the Library's rule, one function for both pages), and the last rows of a
long list rest lower because the document's end holds them. So the first scrolled rest is a whole row
down. The page owns one `plx_ui::scroll_chrome::Handoff` (the Library's, one rule for both pages): it is
decided on the key frame from the scroll TARGET (tucked once the target is within 60 px of one row
pitch down or past it, untucked once it is 120 px short of a pitch or less) and the top chrome leaves on the `xfade` clock, not on the scroll
spring's. The header row is the chrome's own alpha (`Handoff::chrome`), so it leaves and returns with
the bar and has no ramp of its own, and the eased 72 px fade of the page's ground takes the top
edge, as on the Library. There is NO capsule: the header names no
library, and the capsule restates the library the header names (`scroll_chrome::capsule_shown` is
true only for 2+ names).

**The Remove landing (DS4).** Remove from the hold menu commits at once but the grid waits for the
menu: between the commit and the landing's first step nothing on the grid changes (tile slots and
scales, the caption under the focused row, the count, the title it reads). Then, on one frame, the removed tile's ghost
fades, the survivors move, and focus transfers to the next title, whose pop RISES from rest into the
focus pop as the ghost sinks. That rise is the intended look (the design Motion board's "takeover"
spring), not a departure from the design.

**The page colour never passes through neutral.** The wash holds the key it has through any landing
(a Remove, a sort or a filter), and follows the focused title, not the index that title had (the Sort
& Filter menu holds the engine's focus while the cards reflow). A focused title whose colours have not
loaded yet (a matched copy whose facts have not arrived) leaves the wash on its old key until they do;
the flat surface is for a page that has no key and for a title that has none.

**The menus' glass (DS6).** The Sort & Filter panel and the hold menu are glass on frozen ground
(`ui::popover_glass`): the panel's body shows the modal dim's latched field and its rim refracts the
host snapshot taken once when the hold menu opened (the Sort & Filter panel's is retaken once when the
reflow a click started has stopped, below), with the frame's dim applied to both. Nothing behind
the hold menu is re-rendered or re-blurred while it opens, stands or closes, and the page under it
does not move until it has let go (the Remove landing above). **The Sort & Filter menu shows each
click**: its value reaches the grid at once and the cards reflow under the open panel (a card that
changes slot in its row slides, one that changes row crossfades, the longest mover on top, a new
card starts to fade in once the cards in flight cover less than half its cell, drawn under them). While they move the host is drawn live (no
per-frame capture); when they stop it is taken once more and the old look crossfades into the new
ground over about 150 ms. An opener lifted over the dim (the hold menu's
card, the Sort & Filter pill) is not in the lens.

## The catalog page

`CatalogScreen` (`ContentArg::Catalog { hex }`) is the page for a Discover-only title. It is not a
Detail mode: it never reads `metadata::current()`, so it has no Play, Restart, Also available, Mark
watched, extras, seasons, cast or Related. `CatalogStore` owns the one `GET discover
/library/metadata/<hex>`, captures the same `WatchlistAuth`, and reads membership from the
WatchlistStore, keeping no copy. It draws the art ground (proxied through a reachable server's photo
transcoder, since `metadata-static.plex.tv` art needs a PMS; else the neutral ground), title, meta,
a line-clamped summary, genres, a status line (clock for coming soon, circle-slash for not on your servers,
server-slash for server offline, nothing otherwise), the `Coming 12 March` line and the primary pill.

Status: when the title is a row of the list read, its tile state dictates the line (coming soon, not on your
servers, server offline; an Unknown tile and an Available one say nothing, so a silent server never
reads "Not on your servers"), re-derived while the page is open when a match lands or a server goes away.
Only a title not on the list read falls back to the date rule: a dated title whose date is after today
is coming soon; one already out is not on your servers; an undated title has none; with the clock unknown a
dated title is never called coming. The plan's "progress caption" on a server-offline title is not
drawn. An unreachable page has no Try again control: it reads again on resume, or after being covered for about two seconds.

**Defaults awaiting the owner.**

- **The catalog page has no Trailer button (Q4).** Board DetailNotOnServer draws one beside the pill.
  It is omitted in v1 because probe P5 (does a Discover item carry a playable trailer) is unverified;
  the string `watchlist.detail.trailer` is shipped and the control row is a slice, so adding it later
  is an addition, not a rework.
- **The match table is memory-only (Q5)**, so the first tab entry after launch re-matches.
- **Coming soon but matched keeps the clock and plays (Q6).**
- **A press while plex.tv is unreachable flips then reverts with no toast (Q8).**
- **Skeleton art when no PMS is reachable (Q9).**
- **Debug builds refuse writes unless the stand-in or the owner's live switch is armed (Q11).**
- The "Coming" line is drawn at the 32 px rung where the board says 36: the type ladder has no 36.
- The Detail ribbon drop is a named constant (`RIBBON_DROP`, 3 px) because no theme token is 3 px.

### A record the service does not know

A discover 404 (`Load::NotFound`) draws the "Not on your servers" verdict (`catalog::verdict_text`). The shipped
locales have no separate not-found string; the words are accurate, since a title the service has never
heard of is on none of the household's servers. The page has no focus stop, so BACK is the way out.

## Live-update gaps

PMS pushes nothing about plex.tv membership, so the account side is always a re-read: on tab entry
and page mount, on resume while the Watchlist or catalog is live (`reread.rs`: `WatchlistRead`, `CatalogRead`),
two seconds after a burst of writes, and on a profile switch. A landed watched or unwatched mark on a
matched item is not a list read: its `ViewChange` patches that tile in place. There is no polling. A re-read
of an unchanged list costs one conditional request for page 0 (see the ETag note under Wire facts).

What the PMS notification feed (`docs/pms-api.md` section 9) does and does not carry:

- **(a) Library adds and removes flip a tile live on an owned server.** The feed subscribes to
  `filters=playing,timeline`; a finished movie or show create or delete (`TimelineEntry` state 5 with no `metadataState`, and
  state 9, which is UNVERIFIED: no delete has been captured) owes the Watchlist one re-match of that server (`stores::notify_map`, plan §1.11 B(a)):
  a delete of the item a tile is matched to asks again about that guid, a create asks again about the
  titles of that kind the server was not seen to hold (40 guids to a request). It is made at most once
  per quiet-refetch spacing per server and kind, only once the list has loaded and only when a re-read
  is allowed, and the tile flips when the answer lands, with the page open or not. A feed that came back
  after a gap re-asks the server's titles whole. The `timeline` frame shape was captured from an owned server
  (`docs/pms-api.md` section 9, with the measured frame rates); this path is tested against frames
  built from that shape. A delete frame has NOT been observed, and a delete re-match also
  assumes the PMS guid search no longer returns the item once the delete timeline event fires;
  both are unverified. A change that arrives before the first load is not owed
  (the read to come reflects it); one that arrives while that read is out is kept. Shared servers have no feed (d).
- **(b) Watched and progress on a matched tile ride the existing path.** `WatchlistStore::held_keys()`
  is the page's hold window; `Shown.watchlist` adds it to what notify_map holds, so a session on a
  matched item owes a quiet read-back and the tile updates in place. A watched toggle on another client
  emits nothing (scrobble and unscrobble are silent), shared servers have no feed, and a session names
  the episode, so a matched SHOW tile's progress refreshes on re-read only.
- **(c) A server going offline or back** comes from reachability probes, not the feed. A feed loss is
  not offline; it only triggers a resync.
- **(d) Shared servers** (no feed) are re-matched when their last answer is older than the spacing, on
  the triggers of a roster change, tab entry, resume and a cover re-read. An idle visible page may stay
  stale; that is accepted.
