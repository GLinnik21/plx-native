# Watchlist — implementation plan (final, critic-approved)

## Context
The Watchlist design session (cloud) handed over the owner-approved design and asked this
session to plan the implementation in code: a planner and an architect critic, talking
directly, until no blocking objections. Done in 3 rounds (21 points, all accepted, no
residual disputes). Base: `origin/main` adf73d81a. Inputs (handoff + live Guest probe) are
kept as appendices at the end of this file; corrections since: icons from commit 40b4cd14,
read-out anchor 540 (#337), bookmark tab = shipped 60×60 Search-pill geometry.

---

## Design source (owner decision 2026-10-15: the canvas is the visual source of truth)
Canvas: https://claude.ai/artifact/QBFG5q83M4r9U4u83uhApw (Design artifact "Watchlist", version 42
or later). Each board is `project/<Board>.dc.html`; `project/canvas.json` lists them. The board
HTML carries the exact values inline: px positions and sizes, colours (DS tokens), copy, icon
paths and DS component props. The prose in this plan and in INPUT 1 paraphrases these boards;
where the two differ, the board wins.

| row | board | what it is |
|---|---|---|
| 1 | Main | Home with the bookmark tab focused, next to Search |
| 1 | Watchlist | the page: title, one Sort & Filter pill, grid, ground in the focused title's colours |
| 1 | WatchlistOptions | Sort & Filter open on glass |
| 1 | WatchlistFiltered | filters narrowed the list to nothing (read-out) |
| 1 | WatchlistEmpty | empty list (read-out) |
| 1 | WatchlistHeaderB | REJECTED alternative header (title + three chips) — reference only, build nothing from it |
| 2 | Detail | Detail, title not on the list (outline bookmark disc after Resume) |
| 2 | DetailOnList | Detail, on the list (filled disc) |
| 2 | DetailAdded | Detail right after OK on the bookmark |
| 2 | DetailNotOnServer | coming soon: key art, the date, Trailer, labelled pill |
| 2 | DetailOffline | server offline: "On Watchlist" |
| 3 | HoldMenu | hold menu with "Add Show to Watchlist" |
| 3 | WatchlistHold | Watchlist grid hold menu with "Remove from Watchlist" |
| 3 | WatchlistHoldUnplayable | hold menu on an unplayable title |
| 3 | WatchlistRemoved | the grid after a removal |
| 3 | TileStates | unplayable tile marks (clock, server-slash, no-server wash, veil) |
| 3 | Motion | animated: the Remove card replays the #737 landing springs; the Sort & Filter card holds the morph spring values (numbers also in INPUT 1 "Motion") |

Version 42 or later: every board's `renderVals()` TABS matches the drawn strip (Home, Movies, TV
Shows, bookmark icon, search icon).

### Hard rule for every implementing agent (UI PRs)
1. Before writing UI code, read that PR's boards from the artifact (Artifact tool, `action:"read"`
   with `path: "project/<Board>.dc.html"`).
2. Take every value from them: geometry, sizes, colours, states, copy, and the icon paths (they
   match `assets/icons` from 40b4cd14).
3. Map each value onto the existing theme tokens and constants; never paste a raw literal
   (ui/src/CLAUDE.md rules 1–4).
4. Where a board and the shipped code disagree, the shipped component wins; record the difference
   in the PR body for the owner (known: TAB_GAP, the 60×60 icon pill, FULL_ANCHOR_TOP 540).
5. Each UI PR body pairs a simulator (or TV) screenshot with its board, side by side.
6. Motion values come from the Motion board.

# PLAN
Every file:line is on origin/main adf73d81a.

## 0. What already exists
- **plex.tv client.** `AccountClient` (plex/src/plex/account.rs:203) already sends
  `Accept: application/json` (:240-266). Its `get` is pub(super) (:270), so op files add their
  own `impl AccountClient` blocks. `discover.rs` already talks to discover.provider.plex.tv
  (discover.rs:63); parity-gaps.md:500's "new plex/discover.rs" is stale.
- **PUT and the test seam.** A body-less PUT works:
  `request_evidence(..,"PUT",Some(b""),..)` (account/preferences.rs:110-111). Host tests inject
  a `request` closure (preferences.rs:116-160, :330-365); the watchlist client copies that seam.
- **No response headers.** `net::Resp` carries only status, body and peer_pin
  (net.rs:659-667), and there is no CURLOPT_HEADERFUNCTION. Reading an ETag is a curl FFI
  addition.
- **Token.** Use `session::plex_tv_credential(&UserRef)` (session.rs:~236-258): the active
  profile's own plex.tv token, falling back to the owner token only for a proven legacy owner
  session. Capture it with `current_snapshot()`, as route/plan.rs:1585-1596 does. Do NOT copy
  person.rs:1744, which uses the owner's `account_token`.
- **Finding a title on a server.** `Client::find_by_guid` (library.rs:36-71) takes one guid and
  sets `type` from its prefix. Also-available (data/metadata.rs:4380-4470) and the view-state
  fan-out (viewstate.rs:912-1020, `resolved_guid` :983) use it one guid at a time. Registry:
  `server_ids`, `client_for` (servers.rs:492), `probe_result` (:665), `roster_gen` (:608),
  `facts_gen` (:623), `ServerId::UNSET` (:120).
- **PMS rows lack the guid.** `PmsMovie` (data/pms.rs:78-150) has no guid. `Metadata.guid`
  exists (models.rs:328); there is no grandparentGuid field.
- **External art needs a PMS.** metadata-static.plex.tv URLs are fetched through a PMS
  `/photo/:/transcode?url=` proxy (data/search.rs:183-195; filmography.rs:737-755).
- **Tab strip.** `HomeTab{Home,Movies,Shows,Search}` (registry.rs:241-247). Keys
  `STRIP_BASE+0..4`, +4 = account chip (home/mod.rs:88-93). The renderer assumes "the last label
  is the Search icon" (widgets.rs ~6417-6423, ~6861-6871; app/chrome.rs:24-37, :91-93). Library
  and Search map strip positions to tabs (library/mod.rs:890-893, search/mod.rs:267-268). Icon
  pill: `TAB_ICON_PILL_W = TAB_PILL_H` (60×60), `TAB_ICON_D` 32, `TAB_GAP` 16
  (widgets.rs:5582-5585).
- **Landings.** `cards/landing.rs` is 1-D (`Scene{pitch, margin_x}`, :63-78), Shelf only; ui
  CLAUDE.md: "Grid has none yet". `K_SCALE` 320, `K_SCROLL` 170 (consts.rs:492-493).
- **Popover glass policy.** One shared appear spring `K_APPEAR` 300 (popover.rs:74); host page
  captured once (`popover::host` :675). ui CLAUDE.md: panels stand on `widgets::panel_ground`,
  and a grep test keeps `Glass` out of other files. Glass menus are a policy change.
- **Read-outs.** ui CLAUDE.md idiom #4: verdict in TEXT_SECONDARY. Anchor `FULL_ANCHOR_TOP` = 540
  (widgets.rs:3866-3869; #337 moved it from 372). Watchlist uses 540, no glyph.
- **Replay.** `StoreId` is append-only (data/stores/mod.rs:469-486). `ARG_SHAPE`
  (registry.rs:1650) feeds `SCREEN_SHAPES_PIN` (:2232); a fixture recorded against another pin
  cannot load (:2121-2125). 3 committed fixtures.
- **i18n.** `SHIPPED = [En, Es, Be, Fr]` (platform/i18n/mod.rs:22-23). Dates only as
  `YMD::short()`. No collation/case-mapping/normalization crate in Cargo.lock.
- **Text tracking.** gfx/src/text.rs has no letter-spacing.
- **Icons.** Commit 40b4cd14 (branch claude/zen-tesla-pqhbu8): `bookmark`, `bookmark-fill`,
  `clock`, `server-slash` .svg, baked by tools/readout-glyphs.py INLINE at stroke 2.2. No `Icon`
  variant yet.

## 1. Architecture

### 1.1 Modules and layers
| module | crate/layer | role |
|---|---|---|
| `plex::watchlist` (plex/src/plex/watchlist.rs) | plex | `impl AccountClient`: paged list, released list, `user_state(hex)`, `catalog_item(hex)`, add/remove PUT. Drift-tolerant DTOs; injectable `request` closure. Base URL `account::discover()` (moved from this file; `discover.rs`'s person routes share it): DISCOVER, or `/tmp/plxnative-discover` (else the `plxnative-plextv` loopback) behind the `loopback_http` guard as `plex_tv()`. Logs status + endpoint shape only. |
| `Client::find_by_guids` (plex/library.rs) | plex | One `/library/all?type=T&guid=a,b,c` per server per kind (1 movie, 2 show), ≤40 guids/request; other guid kinds not matched. |
| `watchlist` (data/src/watchlist.rs) | data — ADD to `[data] members` + docs/module-layers.md line. Names `plx_plex` (incl. `plex::servers` facts_gen/roster_gen/probe_result, `plex::session`), base, machine, platform. NOT net, NOT media. | Rows, released set, match table, `TileState`, intent ledger, client-side sort/filter, comparator. |
| `stores::watchlist` | data | `WatchlistStore`: main-thread state + `Arc<Adapter>` mailbox (viewstate ownership pattern). `StoreId::Watchlist` APPENDED. |
| `catalog` (data/src/catalog.rs) + `stores::catalog` | data — ADD to members | Discover-only item page fetch/state. `StoreId::Catalog` appended. |
| `screens::watchlist` | screens | The page. |
| `screens::catalog` | screens | Lightweight page for a Discover-only item (§1.7). |
| `appkit::hero_layout` | appkit | Hero geometry/meta-line helpers moved out of detail/hero.rs so Detail and catalog share them without breaking the sibling rule. |
| `ui::icons`, `ui::cards::{landing,grid}`, `ui::popover`, gfx `fs_glass.frag`, gfx text | ui/gfx | DS PRs |
| `platform::i18n` | platform | Month-name dates, strings |
| app (chrome.rs, bridge.rs, content.rs, effects.rs, input) | app | Strip vocabulary, routing, `AppArg::Watchlist`, tab pulse |

Nothing names webOS. Screens never name a sibling screen or app.

### 1.2 Data flow and threading
- Worker `spawn_small("wlist")` → Adapter mailbox slot tagged {profile generation, store
  epoch, request id} → main-thread `pump` behind the landing gate → store commit →
  `StoreChanged`. Screens (Watchlist, Detail, item_menu, catalog, chrome) read via `Cx`.
- Spawn-site capture: owned `WatchlistAuth{client_id, credential, UserRef, generation}` built on
  the main thread; stale generation/uuid landings dropped (`AudioPreferencesKey::is_current`,
  account.rs:96-104). Profile switch rotates the epoch (store reset).
- One list worker + one write worker per epoch. Refused spawn retries after N frames, never
  blocks. Matching runs on the list worker after each page lands.

### 1.3 Reads
- ONE list in default order (`sort=watchlistedAt:desc&includeCollections=1&includeExternalMedia=1`),
  paged Start/Size=100 (101 → 400), remaining pages in the background until the server's own end (`totalSize`, a short or empty page; no page cap); skeleton
  cells past the loaded tail. Plus the `released` list as a guid set. Membership from the
  default list only.
- Sort/type/Show filtering client-side over stored rows: no requests on a Sort & Filter change.
  Date Added asc = reverse of server order (never send `watchlistedAt:asc`).
- Coming soon = listed, not in released set; if released read fails →
  `originallyAvailableAt > today`, else "unknown".
- Re-read on: tab entry/page mount; resume while Watchlist/Detail/catalog is live; 2 s after a
  write burst (one read per burst); a landed watched/unwatched ViewChange on a matched item
  (makes "watched auto-removes" irrelevant to correctness); profile switch. No polling; PMS
  notifications not used for membership.
- Detail before the list loads: one `GET discover /library/metadata/<hex>/userState` (array
  form), merged as a point fact stamped with its read sequence; a later list read supersedes.
- ETag (NET1, shipped; see `docs/watchlist.md`): page-0 If-None-Match; 304
  short-circuits page 0 only (UNVERIFIED for later pages).

### 1.4 Writes — own store (decided)
- Not view-state: ViewLedger is keyed on `(ServerId, ratingKey)` with per-server queues
  (viewstate.rs:1-250); watchlist identity is the catalog hex on another service with another
  token.
- Copied discipline: optimistic flip; one ordered write worker; same-id presses coalesce,
  newest intent wins; INTENT LEDGER beats list reads started before the write's 200; failed
  write reverts, no toast; logs carry no token, no ids beyond the scrubbed guid.
- Watch marks on matched tiles DO join view-state (store consumes ViewChange for its matched
  `(sid, rk)`).
- Ids from PMS rows: `plex://movie|show/<hex>` → hex. W6 adds `guid` and `show_guid` (new
  `grandparent_guid` in models.rs) to `PmsMovie`, decoded once to `[u8;12]`; if absent, the write
  worker resolves via `client.metadata(rk or show_rk).guid` (viewstate.rs:983 precedent).
  Non-plex:// guids: no row, no disc.
- Failsafe: in a `devtriggers` build add/remove are REFUSED and logged unless the discover
  stand-in is armed; UI flips and reverts. Release builds write. Host tests never reach curl (Q11).
- plex.tv unreachable (`account::plex_tv_recently_unreachable`, account.rs:781): page shows
  full-screen read-out at 540 with Retry (a populated list is never blanked); Detail/catalog
  controls draw the outline face when membership unknown; item-menu row omitted; a press flips
  then reverts (Q8). Repro: `/tmp/plxnative-nowan` (devtrig.rs:147-170) + failing-closure tests.

### 1.5 GUID → server matching
- Per server × kind, `find_by_guids` chunks ≤40, over Reachable servers. Cost
  S × 2 × ceil(N_kind/40) (200 items, 2 servers ≈ 10–12 GETs).
- Match table per profile, in memory for v1 (Q5); incremental for new guids; full re-match when
  `roster_gen` moves. Copy choice: current server, then owned, then shares, Reachable only.
- `TileState` derived at commit AND when `facts_gen`/reachability moves (one stamped epoch):
  playable (released, matched on ≥1 Reachable); coming soon (unmatched, or matched — Q6); on no
  server (released, every server answered "absent"); server offline (matched earlier this
  session, every matching server now Unreachable; cached progress kept); unknown (no match, some
  server silent; no mark, no wash, excluded from "On your servers").
- State, "On your servers" set, sorted/filtered index and count computed at commit, never per
  frame; membership `HashSet<[u8;12]>`.

### 1.6 Ground / UltraBlur
Discover items showed no UltraBlurColors. Use the matched PMS item's blur (from guid-batch rows
if present — probe P3 — else lazily from that item's metadata, focused tile only); else flat
`SURFACE_APP` (`AmbientWash::flat`, detail/mod.rs:495). Nothing computed from the poster.

### 1.7 Catalog page (Discover-only item; not a Detail mode)
- `CatalogStore` owns `GET discover /library/metadata/<hex>`, captures `WatchlistAuth`;
  membership read from WatchlistStore.
- `CatalogScreen`, `ContentArg::Catalog{hex}`, SHAPE string, conformance mount, scenario,
  manifest row. Never touches `metadata::current()` (no MORE, no Tracks; summary line-clamped).
- Draws: art ground (proxied, else neutral), title, meta, summary, genres, status line with icon
  (clock / circle-slash / server-slash / none), 36 px "Coming 12 March" line, primary pill "Add to Watchlist"
  (outline) / "On Watchlist" (filled). Suppressed: Play, Restart, Also available, Mark watched,
  extras, seasons, episodes, cast, Related, Trailer (Q4). One focus group. A match landing while
  open re-derives only the status line.
- Estimate: data ~350 + 300 tests; screen ~700 + 400 tests; ~250 moved to appkit.

### 1.8 Detail and menus (PMS items)
- Detail hero: bookmark disc SECOND after Play/Resume, new ELEM id in the hero range
  (hero.rs:39-44); outline/filled; fills from the bottom with a 3 px ribbon drop on flip; no
  labels.
- Item menu: movie "Add to Watchlist"/"Remove from Watchlist"; episode "Add Show to
  Watchlist"/"Remove Show…"; unknown membership → Add face (PUT idempotent); closes on commit.
- Tab pulse: item_menu reports `AppFx::ItemMenu(Action::WatchlistAdd{..})`;
  `app::input::apply_item_action` queues the write and calls `ChromeSnapshot::pulse(STRIP_BASE+5)`
  (app-owned). Pulse ~1.35× + glow, `idle::decorative`, never holds a page image.

### 1.9 Hold-menu identity and state × press table
- `ItemMenuKind::Catalog`; `ItemMenuArg.catalog: String` (24-hex; empty for PMS kinds). Catalog:
  sid `ServerId::UNSET`, rk empty, no PMS rows. Actions carry identity captured at open:
  `WatchlistAdd{hex}`, `WatchlistRemove{hex}`, `GoToCatalog{hex}`. RowKeys
  `ItemRow::{WatchlistAdd, WatchlistRemove, GoToCatalog}`; Remove `destructive(true)`;
  `opening_row` = go row.

| state | mark / caption | OK | hold |
|---|---|---|---|
| playable | PMS tick/progress | Detail{sid,rk} of chosen copy | Card kind (sid,rk) + Remove |
| coming soon, unmatched | clock, "Coming 12 Mar" | Catalog{hex} | Catalog: Go to … — Remove |
| coming soon, matched Reachable (Q6) | clock kept (proposal) | Detail{sid,rk} | Card kind + Remove |
| on no server | circle-slash; 55% wash rest / 25% focus | Catalog{hex} | Catalog |
| server offline | server-slash, cached progress | Catalog{hex} (server-slash status + progress caption) | Catalog (no Play/Mark-watched) |
| unknown | none | Catalog{hex} | Catalog |

OK/hold read the drawn, committed `row.state`. An open Detail whose server dies keeps today's
failure path. Remove on the grid: menu closes, store flips, DS4 landing plays, focus → next,
count drops.

### 1.10 Title sort (v1)
Comparator in `data::watchlist`: `titleSort` else `title`; Unicode lowercase + small fold table
stripping only marks that are not letters in any shipped language (é→e, à→a, ç→c, œ→oe…); ñ, ў,
й stay distinct (after n, у, и); then unfolded string, then hex. No new dependency
(`icu_collator` later, once sized). Tests: Es/Be/Fr titles, "the"/"The". Rating / Release Date:
missing field last in both directions.

### 1.11 PMS live updates (amendment; critic: no blocking objections)
**A. What the stack received before W-notify** (the feed now subscribes `filters=playing,timeline`, B(a)). The feed subscribes to `filters=playing` only
(plex/src/plex/notify.rs:294); `classify` (:160-185) ignores every type but "playing"
(`Notification = Play | Ping | Ignored`, :149-156). Timeline (library add/update/delete),
activity/scanner and status events are NOT received. Reachability comes from probes
(`probe_result` :665, `facts_gen` :623), not the feed. Feeds open for OWNED servers only
(data/src/stores/notify.rs:22, :422-434; `NOTIFY_ENABLED = true` :38). pms-api.md §9: scrobble /
unscrobble emit nothing. notify_map (data/src/stores/notify_map.rs): a playing/paused/stopped
session on a HELD (sid, rk) owes a read-back (:258-281); `stopped` also owes a quiet Home refetch
(+ `HubsInvalidateServer` when unheld); held = `Stores::held(pages, Shown)` (:205),
`Shown{home, library, search}` (:174-182) built in src/app/run.rs:2452-2461; owed work spent only
when `reread_allowed()` (:293; stores/mod.rs:303); facts → `publish_view` → `ViewChange`;
resync → `forget_server` (:253) → `app.reread.resync()` (run.rs:2462-2463). src/app/reread.rs
`action()` (:121-131): list pages `ReadBack` via `bridge.held_keys` → `owe_read_back`
(:150-153); unlisted → `_ => Nothing`.

**B(b) Watched/progress on matched tiles — rides the existing path (W2 + W45):**
`WatchlistStore::held_keys()` returns matched (sid, rk) in the hold window (visible + margin);
new `Shown.watchlist` (also in `Shown::ALL`), folded from `AppArg::Watchlist` in run.rs, added in
`held()` (runtime-only, in no replay shape — no pin bump); reread.rs maps `AppArg::Watchlist` →
`ReadBack` + owed re-match, and `ContentArg::Catalog` gets an EXPLICIT arm owing the re-match for
its guid (keeps its status line fresh while covered). A session on a matched item → quiet read
→ `ViewChange` → the store's watched/progress patch updates the tile in place. Accepted, documented
gaps: watched toggles on another client emit nothing (§9); shared servers have no feed; sessions
carry the episode rk, so a matched SHOW tile's progress refreshes only on re-read (as Home show
cards today).

**B(a) Library add/remove → tile flips live: BUILT in PR W-notify (Q12 answered yes); the timeline frame shape and scan rate are CAPTURED (pms-api.md §9); a delete frame is not yet observed.**
Gated on a passive capture: a read-only GET stream on the owner's own PMS while the owner changes
the library out of band (the app never triggers a scan); token kept out of capture/log/commit,
only scrubbed frame shapes committed; frame rate during a scan measured. Subscribe
`filters=playing,timeline` (the encoded comma list is measured to work; fallback: no `filters`, its cost measured
too). Parser `Notification::Library(Vec<LibraryEntry{item_id, kind, state, section}>)`, keeping
finished create + delete for movies/shows (fields UNVERIFIED until capture). Timeline carries an
itemID, not a guid: DELETE where itemID = a matched tile's rk on that sid → re-match that guid
across servers; ADD → re-match on that sid/kind over still-unmatched watchlisted guids (one
`/library/all?type=T&guid=…` per ≤40). Owed set per (sid, kind), deduped per pump, at most once per
quiet-refetch spacing, only when `reread_allowed`, only once the store has loaded; classify on the
feed worker, no per-frame work. Tile flips at commit via §1.5. Home untouched. Only a query value
changes (no net/FFI/curl change, no fw-compat review). Tier: host + sim.

**B(c) Server offline/back:** unchanged (§1.5). A feed loss/resync is NOT offline; it only
triggers `reread.resync()`.

**B(d) Shared servers / owned without W-notify:** re-match servers whose last match is older than
the spacing on tab entry, resume (`Reread::resume`, reread.rs:68), a cover re-read, or a
`roster_gen` move. No polling while visible; an idle visible page may stay stale (accepted).

**C. Membership:** no PMS signal for plex.tv watchlist membership; §1.3's plex.tv re-read
triggers stand.

**D. Tests (writes always mocked):** notify_map_tests (watchlist-held key owes a quiet read on
playing and stopped; `Shown.watchlist=false` owes nothing; `!reread_allowed` keeps it queued);
store test (`ViewChange` patches progress/watched); reread.rs (`Watchlist` → `ReadBack`, `Catalog`
owes re-match). W45 sim: mock_pms + `--mock-plextv`, Watchlist visible,
`POST /_mock/notify {ratingKey:<matched>, state:"stopped", viewOffset}` (tests/mock_pms.py:1956)
updates the tile. W-notify: classify tests on scrubbed timeline frames; `/_mock/notify` accepts
`{"type":"timeline",…}` + a test-only library add/remove hook; sim scenario where an add flips a
coming-soon tile to playable without leaving the tab.

## 2. PR series
Every PR: `make check` (layer gate, check-deps) + doc-claim audit on docs it touches; docs that
describe a PR's code land in that PR.

### 2.1 Shape pin
- Bump A (W45): `AppArg::Watchlist`, WatchlistScreen SHAPE, `ItemMenuKind::Catalog` +
  `ItemMenuArg.catalog`, Sort & Filter menu arg.
- Bump B (W6): Detail disc ELEM / DetailScreen SHAPE, `ContentArg::Catalog`, CatalogScreen SHAPE.
- Each bump re-records all three fixtures (1-boot-home-chip-grid, 12-filmography-detail-return,
  6-settings-family) via `tools/plxnative-rec rerecord` + `check` and `make check-replay`, with a
  pin-history comment. No other PR moves the pin (test-asserted). UNVERIFIED: whether
  app/recorder.rs's pin moves with the strip — W45 checks; bumps in the same PR if so.

### 2.2 Read-only probes (GETs only)
P1 before W1: type=2 batch, mixed batch, 40-guid URL, absent guids. P2 before W2: list-row fields
(titleSort, originallyAvailableAt, rating vs audienceRating, year, thumb, type), cross-page order
stability. P3 before W45: UltraBlurColors on guid-batch rows / Discover items. P4 before W6: do
PMS hub/section/search/CW rows carry guid/grandparentGuid without includeGuids? P5 before W6:
Discover trailer playable?

### 2.3 Design-system PRs (each its own decision + PR)
- **DS1 Icons** (boards: TileStates; icon paths also in Main/Detail): cherry-pick 40b4cd14; `Icon::{Bookmark, BookmarkFill, Clock, ServerSlash}` +
  ink/band. Host + ui-sim at 28/112 px.
- **DS2 Tick veil** (boards: TileStates): rgba(0,0,0,.55) as a theme role token, DS-wide. ui-sim + TV capture.
- **DS3 Bright verdict** (boards: WatchlistEmpty, WatchlistFiltered): TEXT_PRIMARY verdict, secondary body, everywhere; amend idiom #4.
  ui-sim captures of every read-out.
- **DS4 Grid landings** (boards: WatchlistRemoved + Motion's Remove card): `Scene` → slot→(x,y); `GridSpec::animate_landings`; ghost on K_SCALE,
  survivors K_SCROLL, no stagger; a card that changes row crossfades on K_SCALE (the 2-D spring only for a scroll-only move); off-screen snap; focus
  next. Cards Tier-1 + conformance; update "Grid has none yet". Host + ui-sim + TV feel.
- **DS5 Strip N icon tabs (neutral)** (board: Main, strip geometry only — the bookmark tab itself lands in W45): `TabFace::{Label, Icon}` replaces "last label is Search";
  shipped 60×60 pill, 32 px mark, 16 px gap, `TabPill::mixed_ink`; positional maps → key lookups
  (Search +3, chip +4 unchanged); reword widgets.rs docs (~5575-5585, ~6405) and docs/search.md
  §1. Pin untouched; overscan_rects test (no scroll at 5 stops). Host + ui-sim + check-replay.
- **DS6 Glass menus + 30% scrim** (boards: WatchlistOptions, HoldMenu, WatchlistHold): gradient panel, rims, shadow; fs_glass rim refraction from the
  captured host, same SDF; menus only (owner decision, wave 2 review); amend ui CLAUDE.md Glass section + grep test. Landed as a
  live `Glass::DYNAMIC_BACKDROP` surface with a per-frame sharp band copy; the
  2026-10-17 owner decision moved it onto frozen ground (`gfx::draw_glass_frozen`: the latched dim field as the body, the
  once-captured host snapshot as the rim's sharp source, the dim applied at composite), so a menu requests no backdrop
  entry, runs no source pass and copies no band.
  ui-sim + TV (profile-tv, LG pixels).
- **DS7 Morph open style** (board: Motion's Sort & Filter card; end state WatchlistOptions): pill→panel rect (w k≈620, h k≈950, critically damped), radius
  pill→22 with bulge, rows 0.82→1 (zeta 0.6, k≈720), ~190/150 ms, cross-dissolve, never empty
  glass; fade+scale fallback const. TV mandatory (profile-tv). The growing glass rect re-took a live
  blur source on each frame (fixed first by requesting the morph's whole envelope); on frozen ground
  (DS6 note) the morph moves one quad over pictures that do not change and that machinery is gone.
- **DS8 Text tracking** (board: Watchlist header title): −0.5 px in gfx text (Q10). Host + TV LG raster.

### 2.4 Feature PRs
- **NET1 (optional):** CURLOPT_HEADERFUNCTION → ETag in `Resp`. fw-compat + bind-tv-lib-abi
  (curl floor 7.53.1); loopback fixture tests. Host + TV smoke.
- **W1 client + `find_by_guids` + discover trigger:** closure tests for URLs/params (Size ≤100,
  released, type), 200/304/400/401/404, userState array vs object, PUT verbs/URLs, no token in
  log, dev-build refusal; `find_by_guids` vs loopback PMS fixture. Zero real-account traffic.
  Host + fw-compat review.
- **W2 data `watchlist` + store** (ini/docs diff naming its uses, incl. `plex::servers` epochs):
  stale generation dropped; press during read survives; failed write reverts; coalescing;
  re-match on roster change; all six states; reachability flip re-derives in one epoch; press in
  the flip tick routes on drawn state; comparator. Host.
- **W3 i18n:** `MD` medium/long ("12 Mar"/"12 March"), Be genitive, Fr; new strings En/Es/Be/Fr;
  measure icu_datetime name-data size (UNVERIFIED). Host.
- **W45 strip destination + page (Bump A)** (boards: Main, Watchlist, WatchlistOptions, WatchlistFiltered, WatchlistEmpty, WatchlistRemoved; NOT WatchlistHeaderB): `HomeTab::Watchlist`, `STRIP_BASE+5`,
  `TabFace::Icon(Bookmark)` before Search, route word `"watchlist"`; 6-col 250×375 grid; header
  (DISPLAY 48 bold; tracking after DS8) + "Sort & Filter · <current sort>" pill → popover (morph if
  DS7 landed): Show / Sort+direction word / Filter; empty + filtered read-outs at 540; marks/states;
  PageGround wash; focus caption; hold menu with Remove + DS4 landing. Text-fit test for SHIPPED;
  cards_harness + conformance, scenario, manifest. tests/mock_pms.py gains discover paths (list,
  released, userState, metadata, in-memory add/remove); `tools/tv-session.sh up
  --mock-plextv=PORT` also arms `plxnative-discover`. Host + check-replay + ui-sim (mock) + TV.
  ANY bookmark/Remove press on sim or TV only under the mock with a mock sign-in; real-account
  boots are view-only.
- **Live-update additions (§1.11):** W2 adds `WatchlistStore::held_keys` + the ViewChange
  patch test (its docs name `facts_gen`). W45 adds `Shown.watchlist`, the run.rs fold, the
  `held()` arm, reread.rs `Watchlist` + `Catalog` arms, the notify sim scenario, pms-api.md §9
  "What a hint does" listing the Watchlist page, parity-gaps.md notes on the (b)/(d) gaps.
- **W-notify (Q12 answered yes; BUILT, capture DONE except a delete):** timeline subscription, parser,
  itemID→re-match mapping, mock + sim; pms-api.md §9 gains the measured timeline frames and scan
  rate; doc-claim audit. The owner's passive capture replaced the community-documented shape: the fixtures
  (`rust-modules/plex/src/plex/notify/fixtures.rs`) are the captured shape, scrubbed, and pms-api.md §9
  carries the measured rates. A delete frame (state 9) is still UNVERIFIED: none occurred in the capture.
- **W6a appkit::hero_layout extraction** (boards: Detail for reference; no visual change): behaviour-neutral; host geometry test, Detail
  conformance row, unchanged pin.
- **W6 Detail + catalog + menus (Bump B)** (boards: Detail, DetailOnList, DetailAdded, DetailNotOnServer, DetailOffline, HoldMenu, WatchlistHold, WatchlistHoldUnplayable): P4 first; `PmsMovie` guid/show_guid; Detail disc;
  CatalogStore + CatalogScreen; item_menu rows + Catalog kind; tab pulse. Tests: hero control
  tables, menu row tables, the press table. Same mock-only rule; TV for fill, ribbon, pulse.
- **W7 doc sweep:** docs/search.md (line 8, §6); docs/parity-gaps.md (:8, :80, :271-274, :348,
  :418, :488, :498-501, :529, :534, :657-660, :702-705, :847-864, :879, :945-948, :1132-1140,
  :1203-1206); docs/module-layers.md; new docs/watchlist.md (wire facts, token rule, matching,
  states); docs/pms-api.md (guid batching); ui + screens CLAUDE.md. Doc-claim auditor, incl. the
  stale person.rs:1760-1770 claim that host tests cannot link libcurl.

### 2.5 Order
DS1, DS5, W1, W2, W3, NET1 in parallel → W45 (needs DS1, DS5, W2, W3, DS4) with DS2, DS3, DS6,
DS7, DS8 before or alongside → W6a → W6 → W7.

## 3. Risks and costs
- Glass morph on Mali: clipped rounded rect + text blur per frame; blur only in flight, fade+scale
  fallback, profile-tv measurement.
- DS6 reverses the popover glass policy for every menu; re-measure popover frame numbers
  (popover.rs:8-14). Live menu glass measured about 2 to 2.6M Mali cycles a frame over the field
  ground (2026-10-17); the frozen-ground glass is expected near main's menu frame, unmeasured on
  the TV.
- Grid reflow row-change crossfade and the Remove landing on the first feed after the menu's veil drops below `CLOSING_GONE` need a TV feel check.
- Tab bar: vocabulary + positional maps only; two pin bumps re-record fixtures twice.
- Icon-only tab accessibility UNVERIFIED (Q3). i18n: Be genitive, four languages, size delta
  UNVERIFIED. No reachable PMS → skeleton Discover posters (Q9). plex.tv offline → flip+revert
  (Q8). Dev failsafe → no persistence on debug installs without mock (Q11). PMS URL length
  (chunk 40) and real list sizes UNVERIFIED. Managed user without plex.tv token → read-out (Q2).

## 4. Open questions for the owner
1. Watched auto-removes? Design makes it irrelevant. Verifying needs consent for a new Guest-only
   live write (add → mark watched → re-read), or a read-only search for a plex.tv setting.
2. Managed user without Lists / plex.tv token: proposed tab present, read-out "Watchlist isn't
   available for this profile".
3. Audio Guidance name for the icon-only tab: is any accessibility channel wired? If not, ship
   without and record the gap.
4. Trailer on the catalog page: omitted in v1 unless P5 shows it plays.
5. Persist the match table across launches (per profile, `ACCOUNT_BOUND_PREFERENCES`,
   storage/state.rs:184)?
6. Coming soon but matched: clock + playable (proposed), or unplayable?
7. Where the count sits (e.g. beside the title)?
8. Offline press flips then reverts, no toast — accept?
9. Skeleton art when no PMS is reachable — OK for v1?
10. −0.5 px title tracking worth a gfx change (DS8)?
11. Dev-build write failsafe blocks add/remove on any debug install incl. the owner's TV unless the
    mock is armed — accept, or allow an explicit opt-out trigger for the owner's set?
12. Subscribe owned servers' feeds to library (timeline) events too (W-notify), so a title
    added to / removed from the library flips its tile live? Cost: extra frames on EVERY owned
    feed during scans, even with the Watchlist closed (cheaply dropped); the capture gives the
    measured frames-per-minute during a scan. If declined: re-match on tab entry, resume, and
    re-show after being covered. **Answered YES (owner); built as W-notify.**

Settled by measurement: morph frame cost (DS7, TV); UltraBlur on Discover (P3). ICU month names
settled in W3.

## Verification (of this plan's execution)
Per PR as listed in §2 (make check, check-replay, ui-sim under mock, TV tiers via tv-lock with
mock sign-in, fw-compat for NET1/W1, doc-claim audit). No PR is written now.

## Review log
- Round 1: 5 blocking + 11 non-blocking, all accepted (write isolation on devices, replay pin
  cost batched into two bumps, catalog kind/actions, catalog page instead of Detail mode,
  per-kind batches; client-side sort/filter, plex.tv-offline states, commit-time derivation,
  PmsMovie probe, sign-out scope, doc sweep, layer uses, tracking as gfx PR).
- Coordinator corrections: icons 40b4cd14; anchor 540 (#337); bookmark tab = shipped Search pill.
- Round 2: 1 blocking (state × OK/hold table); non-blocking: locale-aware comparator, Q11 reworded,
  W6a host gate, strip overscan test.
- Round 3: NO BLOCKING OBJECTIONS; fold table keeps ñ/ў/й, W2 names `plex::servers` epochs.
  Residual disputes: none.
- Amendment (owner: "What about live updates from PMS?"): §1.11 + PR additions + Q12; one critic
  round, NO BLOCKING OBJECTIONS; notes folded (explicit Catalog reread arm, `Shown` not in replay
  shape, capture hygiene + scan rate in Q12).

---

# APPENDIX — inputs

---

## INPUT 1 — Design handoff (owner-approved, from the Watchlist design session)

Plan only: no feature code, no push, no PR unless the owner asks.

### Hard constraints
- Never write to a real person's Plex account. Every test that writes uses a mocked server.
  (The only live writes ever allowed: Guest profile, one add/check/remove/check pair — done.)
- Public repo: no tokens, account ids, addresses, gitignored-file values in the plan.
- Module layers: references only point down; only the webOS port names webOS
  (`ci/check-module-layers.py`; `ci/allow/layers.txt` only shrinks). Place every new module.
- No client-made shelves: Home shows only server hubs; no Watchlist row on Home.
- `docs/search.md` and `docs/parity-gaps.md` say Watchlist is out of scope → plan updates them,
  then the doc-claim audit runs.
- Tiers per phase (`which-tier`): pixels, LG text, motion feel, frame cost need the TV; any
  FFI/curl/net change needs the fw-compat review.

### Settled decisions (do not relitigate; may cost them)
**Entry point.** Tab bar becomes Home · Movies · TV Shows · [bookmark icon] · [search icon];
bookmark is an icon tab grouped with Search, no label/caption even on focus. Code today:
`HomeTab { Home, Movies, Shows, Search }` (`rust-modules/screens/src/registry.rs`) → needs a
Watchlist destination. Icon tabs are pill-shaped; bookmark 18×24 viewBox, stroke 3, sized to
match Search; path `M3 1.5h12A1.5 1.5 0 0 1 16.5 3v19.5L9 17.5l-7.5 5V3A1.5 1.5 0 0 1 3 1.5z`.
New DS icons: bookmark, plain clock, server-with-slash.

**Watchlist page.** One grid (Library-style, 6 columns, 250×375 tiles), not sections/rows.
Default order = server's `watchlistedAt` order (list items carry no `watchlistedAt`).
Header: title "Watchlist" (48 px bold, −0.5 px tracking) + ONE pill "Sort & Filter · <current sort>"
(default "Date added"; a filter shows only as the header's "N of M") opening a glass popover with three sections:
- Show: Movies & Shows / Movies / TV Shows (type=1/2)
- Sort: Date added / Title / Release date / Rating (watchlistedAt / titleSort /
  originallyAvailableAt / rating); the checked row's value is its direction word ("Newest first")
- Filter: All / Released / On your servers ("Released" = API `released` list; "On your servers"
  computed client-side from server matching)
Page ground: AmbientWash keyed to focused title's UltraBlur colours, as Home and Library
(`screens/src/home/mod.rs:347`; `TileFacts::blur` `ui/src/tile.rs:84`; Library `mod.rs:202`,
`draw.rs:105`). Unverified whether Discover items carry UltraBlur colours → needs fallback
(PMS item's colours when matched, else neutral ground).
Empty states: full-screen read-out (verdict y=372, no glyph, sentence case): empty list, and
"filter left nothing". DS rule change: verdict in PRIMARY text colour, body secondary,
everywhere. App text never elides (ui/src/CLAUDE.md).
Focus caption: TileLabel title + secondary line; unplayable → state sentence ("Coming 12
Mar"). ICU4X dates numeric only today (`platform/src/i18n/mod.rs:151-163`); need month-name
format for En/Es/Be (`SHIPPED`, `mod.rs:22`).

**Tile states.** Unplayable tiles visible+focusable; OK → Detail, hold → menu. State marks
are icons in the watched-tick slot (top-right, same slot/corner veil as `WatchedMark`), never
text chips. Coming soon: clock. Server offline: server-with-slash (24 px, stroke 2.7), keeps
progress bar. On no server (released): NO mark, 30% black wash at rest, 10% on focus, no rim.
Corner veil darkened to rgba(0,0,0,.55) DS-wide (watched tick too). Ruled out: desaturation,
duotone, frosted tiles.

**Detail.** With Play/Resume: icon-only bookmark disc SECOND after Play/Resume; outline when
not on list, filled when on. No label under a hero disc, at rest or focused (design-review S25's focused caption, "Add to Watchlist" / "On Watchlist", was removed by the owner on 2026-10-20). Without Play (coming soon / on no
server / server offline): labelled pill in primary slot "Add to Watchlist" (outline) / "On
Watchlist" (filled) + status line with icon; coming-soon also has a 36 px "Coming 12 March"
line and a Trailer button. NO toast anywhere (not `platform/src/tv/toast.rs`); button state is
the only feedback.

**Hold menus.** Episode menu rows today: Go to Episode, Go to Show, —, Mark as Watched, Play
from Start. Add one row "Add Show to Watchlist" / "Remove…". Movie: "Add to Watchlist".
`item_menu.rs` closes on every commit (stays). On the Watchlist page the hold menu includes
"Remove from Watchlist". After removal: grid closes up, focus → next title, count drops.

**Glass/popovers.** Glass panel: `linear-gradient(rgba(58,58,64,.78), rgba(40,40,46,.86))`,
1 px rim 14% white, 1 px top highlight 24% white, soft shadow, over 30% scrim. Blur NOT live:
host page captured once (`ui/src/popover.rs`, `host` module); `gfx/src/shaders/fs_glass.frag`
refracts at rim from the same rounded-rect SDF. DS-wide items, each its own decision+PR: glass
menus, 30% scrim, icon-tab pill, darker tick veil, bright-verdict rule, new icons.

**Motion (implement only these).**
1. Add from Detail: existing press spring (`ui::press`); new = bookmark FILLS from bottom on
   state flip + 3 px ribbon drop. The fill is the confirmation.
2. Add from hold menu: menu closes as today; tab bar bookmark pulses once (~1.35× + soft glow,
   back). Only confirmation.
3. Remove from Watchlist grid: the #737 landing (`ui/src/cards/landing.rs`), run AFTER the menu
   (the Remove lands on the first feed after the menu's veil drops below `CLOSING_GONE`): ghost fades in place on `K_SCALE`
   320 easing off its focus pop, carried above the dim by the opener lift; same-row survivors
   slide home together on `K_SCROLL` 170, no stagger; a card that changes row crossfades; the
   next card rises from rest once focus reconciles. GAP: landings only on `Shelf`
   (`animate_landings(true)`, Home CW only); ui CLAUDE.md "`Grid` has none yet". Need Grid
   landings incl. reflow across line breaks: a card that crosses a line break crossfades (out
   where it stood, in at its new cell, on K_SCALE); it never flies over the row between.
4. Page colour: existing AmbientWash crossfade.
5. Sort & Filter popover: glass MORPH (pill's glass becomes the panel, iOS 26 Liquid Glass).
   Rect animates pill→panel anchored top-right; panel hugs rows (8 px padding). Width k≈620,
   height k≈950, critically damped (height leads). Corner radius pill→22 with rounder bulge
   mid-flight. NO shape overshoot. Open ~190 ms, close ~150 ms. Rows keep own size, clipped by
   glass, rise scale 0.82→1 on underdamped spring (zeta 0.6, k≈720, ~1.5% give), opacity+blur
   resolve on landing; the pill's plate and label cross to glass over the first 30% of the rect's progress, THEN
   the rows come in (clipped to the surface); on close the rows leave first, while the rect is
   still well larger than the pill, and the surface ends as the pill with its NEW value; never
   an empty glass frame.
   Engine: today every popover opens with one shared fade-and-slide (`Popover`, `K_APPEAR`
   300, `ui/src/popover.rs`). Morph = second open style for glass openers (Sort & Filter pill,
   profile chip→account menu, player "…"). Item hold menu keeps lift+appear. Glass rect stands
   on frozen ground (`gfx::draw_glass_frozen`: the latched dim field as the body, the once-captured host snapshot as the rim's sharp source), so a moving rect re-grabs nothing. Menu TEXT blur is the Mali cost;
   fallback fade+scale; decide by TV measurement (profile-tv).
   Rejected: focus tilt, light sweep, release-day clock sweep, shrink-and-stagger remove.

### Data
- Watchlist = plex.tv account data on `discover.provider.plex.tv`, token of the ACTIVE profile
  (managed users included), not the PMS token. List `/library/sections/watchlist/all`,
  `released` variant (`unreleased` 404). Filter type=1/2; sort titleSort / originallyAvailableAt
  / rating / watchlistedAt. Pages hard-capped at 100 → paginate X-Plex-Container-Start. Weak
  ETag → If-None-Match 304. Add/remove `PUT /actions/addToWatchlist?ratingKey=<hex>` /
  `removeFromWatchlist`. Per-item `/library/metadata/<hex>/userState`: `watchlistedAt` only
  while on list; discover → ARRAY, metadata.provider → OBJECT.
- Discover ratingKeys are 24-char hex, guid `plex://movie/<hex>`, NOT PMS ratingKeys. Mapping to
  playable PMS item = GUID matching against user's servers. "On your servers", unplayable states
  and Detail's Play depend on it. Find how (whether) the app already resolves `plex://` GUIDs
  on its servers and per-item cost.
- HTTP: `net.rs` libcurl TLS already carries plex.tv (`plex/account.rs`). `docs/parity-gaps.md:499`
  proposes `plex/discover.rs` + a screen — starting point, predates the crate split; test it
  against the current layer map.
- Watched state: does marking watched remove from watchlist? Unverified; say how to verify
  without writing to a real account.
- Reconcile: view-state series (#719–#737, `data/src/viewstate.rs`, `publish_view`,
  ViewLedger) propagates a press to every store and survives in-flight reads. Watchlist
  membership is a new per-item fact read by Detail, item menu, grid, tab pulse. Does it belong
  in that system or its own store?
- Live updates: PMS notifications (#753/#755/#761) cover PMS only; watchlist changes elsewhere
  arrive only by re-reading (ETag cheap). Choose when: tab entry, resume, after write.

### Still open (to the owner)
UltraBlur on Discover + fallback; watched auto-removes?; webOS Audio Guidance reaching the app
(icon-only tabs need accessible name); what a managed user without Lists sees; ICU4X month-name
format En/Es/Be; morph frame cost on TV.

### Docs the agents must ground in
`AGENTS.md`, `docs/agent-reference.md` (relevant sections), `docs/module-layers.md`,
`ci/module-layers.ini`, `rust-modules/ui/src/CLAUDE.md`, `rust-modules/screens/src/CLAUDE.md`,
`rust-modules/plex/src/plex/CLAUDE.md`, `docs/pms-api.md`, `docs/ui-restructure-spec-v4.md`
(as cited), `docs/parity-gaps.md` (Watchlist ~:497, :656, :701; menu rows ~:270),
`docs/search.md` §6.

---

## INPUT 2 — Live API probe, Guest profile, 2026-10-14/15 (redacted)

Token: Guest = Home's built-in managed user (home=true, restricted=true, guest=true,
homeAdmin=false, protected=false, no Plex Pass). Obtained via `GET /api/v2/home/users` →
`POST /api/v2/home/users/{uuid}/switch` (uuid, not numeric id; numeric → 404) → 201 authToken.
A per-server PMS accessToken (shared_servers) is 401 on plex.tv.

discover.provider.plex.tv (Guest token, headers Accept: application/json, X-Plex-Token,
X-Plex-Client-Identifier, X-Plex-Product):
- `/library/sections/watchlist/all?includeCollections=1&includeExternalMedia=1&sort=watchlistedAt:desc&X-Plex-Container-Start=0&X-Plex-Container-Size=5`
  → 200; cache-control no-store; X-Plex-Container-Start / X-Plex-Container-Total-Size headers;
  weak ETag `W/"…"`. MediaContainer keys: identifier ("tv.plex.provider.discover"),
  librarySectionID ("watchlist"), librarySectionTitle ("Watchlist"), offset, size, totalSize.
- If-None-Match: <etag> → 304.
- `available` → 200, `released` → 200, `unreleased` → 404 {"Error":{…"Not Found"…}}.
- type=1 / type=2 → 200; sorts titleSort:asc, originallyAvailableAt:desc, rating:desc → 200
  (list was empty, ordering unproven).
- Container-Size 100 → 200; 101+ → 400 "Invalid value provided for x-plex-container-size!".
- `/library/metadata/<hex>/userState` on discover → 200 `UserState: [ {ratingKey,type,viewCount,viewOffset[,watchlistedAt]} ]`
  (ARRAY); on metadata.provider.plex.tv → 200 `UserState: {…}` (OBJECT). no-store + ETag.
- `/library/metadata/<hex>?includeUserState=1` → 200; UserState NOT inline; viewCount/viewOffset
  flat on item; no AvailabilityPlatform. Item keys incl. availabilityId, isTopWatchlisted, slug,
  streamingMediaId, publicPagesURL, playableKey (provider://tv.plex.provider.vod/…), ratingKey
  (24-hex), guid plex://movie/<hex>, type, thumb on metadata-static.plex.tv, Guid[] imdb/tmdb/tvdb,
  art, Image[] (UltraBlur colours NOT observed in the key list — `UltraBlurColors` absent).
- `/library/metadata/<hex>/availabilities` → 200 `Availability[]` {attribution, country,
  offerTitle, offerType, platform, platformColorThumb, platformInfo, platformUrl, playableKey,
  quality, startDate, streamingMediaId, title, metadataTitle}; cacheable (max-age 60,
  s-maxage 21600), Vary X-Plex-Language.
- WRITE pair (Guest only): `PUT /actions/addToWatchlist?ratingKey=<hex>` → 200
  `{"MediaContainer":{"size":0}}`; userState gains `watchlistedAt` (unix s); list totalSize 1,
  list item had NO `watchlistedAt`. `PUT /actions/removeFromWatchlist?ratingKey=<hex>` → 200 same
  body; state restored.

luma.plex.tv (Plex web UI backend): accepts a plain plex.tv token (header or query); no token →
401. `/api/screen/watchlist` → 200; `/api/screen/movie/<slug>` → 200 with actions
addToWatchlist PUT `/api/action/metadata/<hex>/watchlist`, removeFromWatchlist DELETE same,
rate PUT `/rating`, markAsWatched PUT / markAsUnwatched DELETE `/watched`. For the managed
Guest: `/api/screen/watchlist/my-lists` → 503 "User not found: <id>/<hash>"; no add-to-list
action; `/add-to-list` → 404. (Admin/full account not tested.) App will NOT use luma.

Local PMS 1.43.4 (per-server token, read-only):
- `GET /library/all?guid=plex://movie/<hex>[&type=1]` → hit with librarySectionID; absent → size 0.
- Comma-joined `guid=<a>,<b>,<c>` in ONE param → all present ones returned, absent silently
  dropped (batch works). Repeated `guid=a&guid=b` → last wins only.
- `guid=imdb://…` / `tmdb://…` (all/section, with/without type) → 0. Only the primary plex://
  guid matches. Items carry Guid[] (imdb/tmdb/tvdb) with includeGuids=1.

---

(end of appendices)
