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
- **ETag is not used.** `net::Resp` carries no headers and there is no header callback, so reading
  an ETag is a curl FFI addition (plan item NET1) that has not shipped. The client has the
  `If-None-Match` seam, always `None`; every read is a full read.
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

**A debug build never writes.** In a `devtriggers` build a `PUT` is refused and logged
(`WatchlistError::Refused`) unless the loopback stand-in named by `/tmp/plxnative-discover` is armed;
the UI flips and then reverts. Release builds write. Host tests never reach curl: they drive the
request closure.

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

**The other direction.** A `PmsMovie` carries `guid` and `show_guid` (an episode's `grandparentGuid`),
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
| NotOnAnyServer | released, every server on the roster answered "absent" | none; a 30% wash at rest, 10% on focus | Catalog | Catalog |
| ServerOffline | matched earlier, every matching server now Unreachable | server-slash, cached progress | Catalog | Catalog (no Play or Mark watched) |
| Unknown | no match and some server has not answered | none | Catalog | Catalog; excluded from "On your servers" |

OK and hold read the drawn, committed `row.state`. An open Detail whose server dies keeps today's
failure path. "Absent" needs the whole roster to have answered, so a silent server never produces
"On no server".

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

**Surfaces.** The Detail hero's bookmark disc is second after Play/Resume (`ELEM_WATCHLIST`, unlabeled,
outline when off the list, filled when on; the fill rises from the bottom with a 3 px ribbon drop on a
flip, not on the first answer). It exists only for an item whose guid is `plex://movie|show/<hex>`
(`HeroSet::watchlist`).
The **Home hero** carries the same disc, second after Play/Continue and before Info (Play, disc, Info, with
the shipped 12 px between controls). It is present when the hero row stands for a title
(`PmsMovie::watchlist_subject`: a movie or show itself, a season or episode its SHOW, "Add Show"
semantics) with a known id. The application feeds it the membership of the title the hero shows
(`HomeScreen::watchlist_hex` / `set_watchlist`, `app::watchlist::catalog_feed`); it flips only on a real
membership change of that title, never on open or when the hero rotates to another title. A press is the
same `AppFx::ItemMenu` write as Detail's. Face, glyphs and flip are `plx_appkit::watchlist_disc`, shared.
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

**The count (Q7, decided: option D).** The Watchlist header carries the list's count as plain text
just left of the Sort & Filter pill: LABEL 26 regular in `TEXT_SECONDARY`, centred on the pill's line,
right-aligned `space::MD` (24 px) clear of it, so the pill does not move and nothing sits beside the
title. It reads "{n} titles" (plural forms in every shipped language) while no Type or Show filter
narrows the list (Sort alone does not), and "{shown} of {total}" when one does, including "0 of N"
beside the filtered-empty read-out. It is derived from the list state (no `SCREEN_SHAPES` change),
absent until the list has landed and on the empty-list and unavailable read-outs, and follows a Remove
when the grid's landing STARTS (after the hold menu has gone and the landing's beat), not at the
commit, so the number and the tiles move on one frame. It stays under the Sort & Filter menu's scrim; only the pill is lifted.

**The Remove landing (DS4).** Remove from the hold menu commits at once but the grid waits for the
menu: between the commit and the landing's first step nothing on the grid changes (tile slots and
scales, the caption under the focused row, the count). Then, on one frame, the removed tile's ghost
fades, the survivors move, and focus transfers to the next title, whose pop RISES from rest into the
focus pop as the ghost sinks. That rise is the intended look (the design Motion board's "takeover"
spring), not a departure from the design.

## The catalog page

`CatalogScreen` (`ContentArg::Catalog { hex }`) is the page for a Discover-only title. It is not a
Detail mode: it never reads `metadata::current()`, so it has no Play, Restart, Also available, Mark
watched, extras, seasons, cast or Related. `CatalogStore` owns the one `GET discover
/library/metadata/<hex>`, captures the same `WatchlistAuth`, and reads membership from the
WatchlistStore, keeping no copy. It draws the art ground (proxied through a reachable server's photo
transcoder, since `metadata-static.plex.tv` art needs a PMS; else the neutral ground), title, meta,
a line-clamped summary, genres, a status line (clock for coming soon, server-slash for on no server or
server offline, nothing otherwise), the `Coming 12 March` line and the primary pill.

Status: when the title is a row of the list read, its tile state dictates the line (coming soon, on
no server, server offline; an Unknown tile and an Available one say nothing, so a silent server never
reads "On no server"), re-derived while the page is open when a match lands or a server goes away.
Only a title not on the list read falls back to the date rule: a dated title whose date is after today
is coming soon; one already out is on no server; an undated title has none; with the clock unknown a
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
- **Debug builds refuse writes unless the stand-in is armed (Q11).**
- The "Coming" line is drawn at the 32 px rung where the board says 36: the type ladder has no 36.
- The Detail ribbon drop is a named constant (`RIBBON_DROP`, 3 px) because no theme token is 3 px.

### A record the service does not know

A discover 404 (`Load::NotFound`) draws the "On no server" verdict (`catalog::verdict_text`). The shipped
locales have no separate not-found string; the words are accurate, since a title the service has never
heard of is on none of the household's servers. The page has no focus stop, so BACK is the way out.

## Live-update gaps

PMS pushes nothing about plex.tv membership, so the account side is always a re-read: on tab entry
and page mount, on resume while the Watchlist or catalog is live (`reread.rs`: `WatchlistRead`, `CatalogRead`),
two seconds after a burst of writes, and on a profile switch. A landed watched or unwatched mark on a
matched item is not a list read: its `ViewChange` patches that tile in place. There is no polling.

What the PMS notification feed (`docs/pms-api.md` section 9) does and does not carry:

- **(a) Library adds and removes do not flip a tile live.** The feed subscribes to
  `filters=playing` only and the parser ignores every other type, so the timeline events a scan emits
  are not received. A title added to or removed from a server is noticed on the next re-match
  (tab entry, resume, a cover re-read, a roster change). The optional timeline subscription (plan item
  W-notify, gated on a passive capture) has not shipped.
- **(b) Watched and progress on a matched tile ride the existing path.** `WatchlistStore::held_keys()`
  is the page's hold window; `Shown.watchlist` adds it to what notify_map holds, so a session on a
  matched item owes a quiet read-back and the tile updates in place. A watched toggle on another client
  emits nothing (scrobble and unscrobble are silent), shared servers have no feed, and a session names
  the episode, so a matched SHOW tile's progress refreshes on re-read only.
- **(c) A server going offline or back** comes from reachability probes, not the feed. A feed loss is
  not offline; it only triggers a resync.
- **(d) Shared servers and owned servers without the timeline subscription** are re-matched when their
  last answer is older than the spacing, on the same triggers as (a). An idle visible page may stay
  stale; that is accepted.
