# PMS API Reference (verified against live server)

> **Authoritative source: [`plex-openapi.json`](plex-openapi.json)** — the official Plex Media
> Server OpenAPI 3.1 spec (205 operations). Check it FIRST for any endpoint (method, path,
> params, response); this file is a hand-curated, live-verified subset for the paths we use.
> The spec is where `transcodeSubtitles` (soft WebVTT during transcode), `PUT /library/parts`
> (audio/subtitle stream selection), and `/library/streams/{id}.{ext}` live — reach for it before
> reverse-engineering.
>
> **One exception, and it is a live one: `/hubs/search`.** The spec's worked example has the
> response shape wrong (it files shows under `Directory`), and believing it renders two of the
> five search shelves as nothing, silently. §3b is the probed answer; prefer it there.

Server: `http://YOUR_PMS_HOST:32400` — PMS apiVersion 1.2.2.
Token: `X-Plex-Token=YOUR_PLEX_TOKEN` (query param or `X-Plex-Token` header). Get yours from Plex → any item → Get Info → View XML.
All endpoints below were verified live on 2026-07-03 with read-only GETs, **except** §7 (timeline), which is documented from community sources only.

**JSON instead of XML:** send `Accept: application/json`. Every response is wrapped in a top-level `MediaContainer` object.

**Paging:** either headers `X-Plex-Container-Start` / `X-Plex-Container-Size` or same-named query params. A paged response carries `size`, `totalSize`, `offset`; an unpaged one carries neither total nor offset (see [Paging, observed](#paging-observed)).

---

## 1. Library sections

```
GET /library/sections?X-Plex-Token=...          (Accept: application/json)
```

Verified response (trimmed):

```json
{"MediaContainer":{"size":3,"Directory":[
  {"key":"1","type":"movie", "title":"Movies",   "uuid":"66b73dbd-..."},
  {"key":"2","type":"show",  "title":"TV Shows", "uuid":"62897fae-..."},
  {"key":"3","type":"artist","title":"Music",    "uuid":"332590ad-..."}]}}
```

**Section keys on this server: Movies = `1`, TV Shows = `2`, Music = `3`.** Select sections by
`Directory[].type`, never by hard-coded key — and note the app no longer filters that type down to
`movie`/`show`: `artist` and `photo` are kept too (`browse::SecKind`), because the top strip's
projection can only give a friend's shared MUSIC library a pill if the type reaches the table at
all. "Ignore music for now" was true while the strip named one server's movie and show sections.

---

## 2. Section item listing (gallery)

```
GET /library/sections/1/all?X-Plex-Container-Start=0&X-Plex-Container-Size=50&X-Plex-Token=...
GET /library/sections/2/all?...        # shows; totalSize: Movies=23, Shows=12
```

Verified movie entry (trimmed to gallery-relevant fields):

```json
{"ratingKey":"1","key":"/library/metadata/1","type":"movie","title":"Example Movie",
 "contentRating":"PG","summary":"A one-paragraph plot synopsis ...",
 "rating":8.9,"audienceRating":8.5,"viewCount":1,"year":2013,
 "thumb":"/library/metadata/1/thumb/1778526065",
 "art":"/library/metadata/1/art/1778526065",
 "duration":6133056,
 "Media":[{"id":738,"duration":6133056,"bitrate":16248,"width":1920,"height":858,
   "audioChannels":6,"audioCodec":"ac3","videoCodec":"h264","videoResolution":"1080",
   "container":"mkv",
   "Part":[{"id":738,"key":"/library/parts/738/1767473373/file.mkv",
            "size":12456162321,"container":"mkv"}]}]}
```

Show entries differ: `type:"show"`, `key` ends in `/children`, and they add
`leafCount`, `viewedLeafCount`, `childCount` (season count); **no `Media`** on shows.

### Minimal JSON fields the gallery needs per item

| field | type | notes |
|---|---|---|
| `ratingKey` | string | item id; use for detail fetch + timeline |
| `key` | string | `/library/metadata/{rk}` (movie) or `.../children` (show) |
| `type` | string | `movie` \| `show` \| `season` \| `episode` |
| `title` | string | |
| `year` | int | may be absent |
| `thumb` | string | poster path (portrait) — feed to /photo transcoder |
| `art` | string | background/landscape path — feed to /photo transcoder |
| `duration` | int | ms |
| `viewOffset` | int | ms; **only present when partially watched** |
| `viewCount` | int | only present when watched ≥1× ; absent = unwatched |
| `contentRating` | string | e.g. "PG", "TV-MA"; may be absent |
| `rating` / `audienceRating` | float | 0–10; either may be absent |
| `summary` | string | can be empty |
| shows only: `leafCount`, `viewedLeafCount`, `childCount` | int | unwatched badge = leafCount − viewedLeafCount |
| episodes only: `grandparentTitle`, `parentIndex`, `index`, `grandparentThumb` | | "Show – S1E8" labels; `grandparentThumb` = show poster |

All optional numeric fields must be treated as absent-able in the C parser (default 0).

---

## 2b. Library browse — sort / filter / letter index (verified live 2026-07-19, PMS 1.43.2)

The Library screen's surface. Everything here was probed against the live server; the OpenAPI
spec's "Media Queries" section documents the full query language.

- **Browse views menu:** `GET /library/sections/{key}` → `viewGroup:"secondary"` + `Directory[]`
  of relative keys (`all`, `unwatched`, `recentlyAdded`, `genre`, `year`, `decade`, `collection`,
  `firstCharacter`, `folder`, …). All are canned queries over `/all` — the client synthesizes
  them with params instead.
- **Server-driven menus:** `GET /library/sections/{key}/all?includeMeta=1` → `MediaContainer.Meta.Type[]`
  with `Sort[]` (`{key, descKey, defaultDirection, title, firstCharacterKey?}` — movies: 9 entries)
  and `Filter[]` (`{filter, filterType, key=value-list URL, title}` — movies: 27). With header
  `X-Plex-Container-Size: 0` it's a menus+totalSize-only probe. Standalone `/filters` and `/sorts`
  endpoints exist too. **The client hardcodes no menu contents** (`browse.rs` consumes these).
- **Sorting:** `sort=key:asc|desc` (comma-chain for multi-level, `nullsLast` supported);
  `sort=random` works. Composes with filters + paging.
- **Filter values:** `GET /library/sections/{key}/genre` (also `/year`, `/decade`, `/contentRating`,
  `/collection`, …) → `Directory[]` `{key: tag id, title, fastKey}` where **`fastKey` is the
  ready-made listing URL** (`/library/sections/1/all?genre=150`). Apply as `/all?genre={id}`.
- **Media-query operators** (URL-encode the key side): string `=` contains / `==` equals /
  `<=` begins-with; int+date `>>=` gt/after, `<<=` lt/before; bool `=1/=0`; relative dates
  `addedAt>>=-3w`; OR via comma; parens via `push=1`/`pop=1`/`or=1`. Episode scoping
  `type=4&show.id={rk}` works; scoped exact `show.title==` does NOT (use `show.id`).
- **Unwatched:** movies `unwatched=1`; **shows use `unwatchedLeaves=1`** (`unwatched=1` on
  type=2 has odd semantics — returned 1 of 10 live); episodes `unwatched=1` normal.
- **Show granularity:** `?type=2|3|4` on `/all` lists shows/seasons/episodes flat (no
  children-walking). Type ids: movie=1, show=2, season=3, episode=4.
  Rechecked live 2026-09-26: `includeMeta=1` returns all three `Type` entries and marks the
  requested one `active`. Seasons and episodes advertise a compound Show sort and its `descKey`;
  use that descending expression as supplied. Neither advertises a Genre filter. Typed
  `/firstCharacter?type=2|3|4` counts match the corresponding flat listing totals.
  Seasons accept `unwatchedLeaves=1`; episodes use `unwatched=1`.
- **Letter index:** `GET /library/sections/{key}/firstCharacter` → per-letter `Directory[]` with
  `size` counts in titleSort order (articles stripped); `/firstCharacter/{L}` returns the items.
  **`?firstCharacter=X` on `/all` is silently IGNORED** — jump = prefix-summed
  `X-Plex-Container-Start` offset into the titleSort listing. (Spec's plural `/firstCharacters`
  404s.) `titleSort<=`/`>=` behave as a lexicographic range, not begins/ends-with.
- **Collections — two id spaces:** `/library/sections/{key}/collections` → `Metadata[]` with
  **ratingKey** (browse via `/library/collections/{rk}/children`); the `/collection` filter dir
  returns **tag ids** for `/all?collection={tag}`. Don't mix. A collection row's `index` is that
  tag id, while its `guid` is shared with search hits and member `Collection[]` tags. Adding
  `includeCollections=1` to `/hubs/search` yields full collection `Metadata[]` rows rather than
  tag-shaped `Directory[]` hits.
- **Collection routes and hubs:** `/library/collections/{rk}/children` and `/items` return the same
  paged members. A promoted `custom.collection.*` hub embeds the members and carries the collection
  ratingKey in its identifier; a member's `collection.related.*` hub lists the whole collection.
  Automatic art is `/library/collections/{rk}/composite/{stamp}` (possibly with a query), while a
  custom poster is `/library/metadata/{rk}/thumb/{stamp}`. An unshared section answers 403 from its
  collections listing; preserve that as authorization denial rather than an empty result.
- **Paging gotcha:** a query-param `X-Plex-Container-Size` WITHOUT `Start` is silently ignored —
  always send both (the client does), or use the headers. Header `Size: 0` = count-only probe.
  `totalSize` is only present on paged responses.
- **Payload slimming:** `excludeElements=Media` etc. per the spec (not used yet).

---

## 2c. People — cast/crew tags and the person page (verified live 2026-07-29, PMS 1.43.2)

The tag arrays on an item (`Role[]`, `Director[]`, `Writer[]`) are the entry point, and they
carry **six** attributes, not the three the app used to parse:

```json
{ "id": 465, "filter": "actor=465", "tag": "<person name>",
  "tagKey": "5d776c4b7a53e9001e73f56d", "role": "<character name>",
  "thumb": "https://metadata-static.plex.tv/b/people/b575cd9….jpg" }
```

`id` and `tagKey` are what make a person page reachable (`plex::Tag`). `count` also appears in
the spec but **this server does not emit it on `Role[]`** — treat 0 as unknown, not as none.

- **The person record:** `GET /library/people/{personId}` → `Directory[0]` =
  `{id, filter, tag, tagType, tagKey, thumb}`. `personId` accepts **either** the numeric `id`
  **or** the `tagKey` hex guid — both verified against the same record (`161` /
  `5d77682aeb5d26001f1de4b0`). **There is no biography, no birth date and no social handle
  here** — the record is only the tag. (`GET /library/metadata/{tagKey}` 404s; that is the wrong
  endpoint.) Because the record adds nothing to what `Role[]` already gave the caller, the app does
  **not** issue this request: the person page is opened on the cast row's own name + thumb.
- **The biography is NOT on PMS — it is on plex.tv, and it does exist.** "PMS has no biography" is
  true and was once written down as "Plex has none", which is not. It lives at
  `GET https://discover.provider.plex.tv/library/people/{tagKey}` (verified live 2026-07-29) and
  carries `summary`, `bornAt`, `diedAt`, `birthPlace`, `knownFor`, `CreditType[]` (the "Actor,
  Producer" roles line + the counts Plex's Filmography tabs show) and `External[]` (social handles).
  Three traps, all measured: **only the `tagKey` guid works** there — the numeric `id` PMS accepts
  for `/media` returns `404 "Invalid value provided for metadataId!"`; **`Accept: application/json`
  is required** or you get XML; and **an unknown person is a `200` with `totalSize:0`**, not a 404,
  so "no such person" and "the request failed" are different answers. Being a different HOST, it
  needs DNS+TLS and therefore `net.rs`/libcurl, never the raw `stream.rs` socket. The typed client is
  `plex/discover.rs`.
- **The filmography IS built** — `…/library/people/{tagKey}/credits` → `CreditGroup[]` of Discover
  items, NOT local library rows. `AccountClient::person_credits`, drawn by the independently
  mounted `screens/filmography.rs`.
  **The shape was measured 2026-09-06 and is not what the names suggest.** The container is
  `MediaContainer.CreditGroup[]` (never `Metadata`, which the DTO also accepted until a real
  response settled it). A group is `{title, type, Credit[]}` — `title` is "Actor"/"Producer"/
  "Appearances", `type` is `actor`/`producer`/`appeared`, and there is **no `size`**, so a group's
  count IS its row count. A credit is `{order, role, Metadata}` where `Metadata` is ONE item, whose
  whole key set is `art`, `key`, `originallyAvailableAt`, `publicPagesURL`, `ratingKey`, `slug`,
  `thumb`, `title`, `type`, `year`.
  **There is NO `guid` on that item**, and this is the trap: PMS states the same identity as
  `plex://movie/5d7768295af944001f1f7477` while this endpoint states it as a bare `ratingKey` of
  `5d7768295af944001f1f7477`. The join is therefore on the guid's **last path segment**
  (`person::guid_tail`), not on the whole string. Modelling a `guid` field here — it defaults to
  empty on every row — made the availability join match NOTHING while every count around it read
  healthy: `joinable=5`, 544 rows drawn, not one markable.
  `thumb` is an **absolute URL** on `image.tmdb.org` or `metadata-static.plex.tv`. That is not a
  reason to skip the artwork: `posters::poster_key` URL-encodes exactly such a URL into
  `/photo/:/transcode?url=…` and the SERVER fetches it, the same path Search's `actor` headshots
  take. And its group counts are **not** `CreditType`'s — that
  record says 1745 actor credits where this returns 222 — so the tabs and the person page's entry
  row spend the group's own, never the profile's. The one thing NOT settled live is which key the
  container puts the groups under (`CreditGroup` or `Metadata`); the DTO accepts both, logs which
  one answered, and treats a body carrying NEITHER as a failure to be retried rather than as a
  person with no career.
- **Unlike the profile beside it, `/credits` REQUIRES a token** (measured 2026-09-05): with no
  `X-Plex-Token` it answers `401 {"error":"Unauthorized","message":"You must provide a token!"}`,
  where `/library/people/{tagKey}` answers 200 unauthenticated. And the token it wants is the
  plex.tv **account** token, not a PMS server token — which is what puts the whole filmography out
  of reach of every automated boot in this repo: those sign in with `/tmp/plxnative-token`, a
  server token, and leave `Session::account_token` empty. It is the same wall
  `/tmp/plxnative-personbio` exists for one step further along — the biography degrades to a blank
  line, the filmography to nothing at all — and it is why `/tmp/plxnative-personcredits` had to be
  written. Reaching either with real data needs a signed-in account (`make sim` with a real
  session, or the debug install with `--no-token`).
- **The person's titles:** `GET /library/people/{personId}/media` → `Metadata[]`, everything the
  person appears in **across EVERY library section in one request** (person 161 → 3 items;
  person 6059 → 6). This is the right call for a person page; `?actor=<id>` below is the right
  call when you want one section.
- **`viewGroup` on that container is unreliable — group by each row's own `type`.** Verified:
  person 6059's response carries `viewGroup:"movie"` over five `movie` rows *and* one `show`.
- **Per-section listing:** `GET /library/sections/{key}/all?actor=<id>` (the `filter` string on
  the tag is exactly this query, ready-made).
- **The whole actor list of a section:** `GET /library/sections/{key}/actor` → `Directory[]` of
  `{key: "<id>", title, thumb, fastKey: "/library/sections/2/all?actor=691"}` (51 on Movies, 56
  on TV Shows here). NB the rows key the id as **`key`/`title`**, not `id`/`tag` — a different
  shape from the tag arrays above. This is the *Categories → by Actor* browse axis.
- **Headshots are absolute `https://metadata-static.plex.tv/…` URLs**. They still render through
  the PMS photo transcoder rather than through the app's PMS-origin HTTP client: `image_transcode_path`
  passes the whole URL as `url=` to `/photo/:/transcode` and **the server does the TLS** — the
  same path posters take. Never invent another image route for them.

---

## 3. Hubs (home shelves)

```
GET /hubs?count=12&X-Plex-Token=...             # classic global hubs
GET /hubs/promoted?count=12&excludeContinueWatching=0&X-Plex-Token=...
```

Verified hub list (`MediaContainer.Hub[]`), each hub has
`hubIdentifier`, `title`, `type`, `size`, `more`, `key`, and inline `Metadata[]` items:

| hubIdentifier | title | items key |
|---|---|---|
| `home.continue` | Continue Watching | `/hubs/home/continueWatching` |
| `home.ondeck` | On Deck | `/hubs/home/onDeck` |
| `home.movies.recent` | Recently Added Movies | `/hubs/home/recentlyAdded?type=1` |
| `home.television.recent` | Recently Added TV | `/hubs/home/recentlyAdded?type=2` |
| `movie.recentlyadded.1` (promoted) | Recently Added in Movies | `/library/sections/1/all?sort=addedAt:desc` |
| `custom.collection.*` | collection shelves | `/library/collections/{id}/children` |

**The spec gives `/hubs` no paging** (`docs/plex-openapi.json`: its only parameters are `count`, `onlyTransient`
and `identifier`), but a windowed request does page its hub list on a live server (see [Paging, observed](#paging-observed)).
Home reads all rows, in windows of `HUB_WINDOW` hubs (`home_hubs` is the whole-list fallback), with 12 preview cards per row. A row pages
through its provider's listing `key`, with both `X-Plex-Container-Start` and
`X-Plex-Container-Size`, when `plx_plex::plex::is_pageable_hub_key` admits the key: `/hubs/…`,
`/library/sections/…`, `/library/metadata/{id}/similar` and `/library/collections/{id}/children`
(the listing the Collection page pages). The client preserves the key's type,
library, and sort parameters. A row behind any other key keeps its 12-card preview and logs that
once per hub; the Home, Library-section and Detail related rows share this one allowlist.

A PMS response observed on 2026-10-09 returned `offset=0`, `size=36`, and `totalSize=50`.
The next response returned `offset=36`, `size=14`, and `totalSize=50`. This confirms the
required offset fields on that server. Other server versions remain unverified. The client
rejects a response whose offset differs from the requested start.

Home keeps a moving window of at most 24 cards per pageable row, with an overlap of up to 12 eligible cards.
Server offsets are kept separately from visible card positions. A page retains the overlap
and skips items from hidden libraries. One paging worker scans at most eight server chunks.
If those chunks are hidden, it publishes the advanced cursor and keeps the current cards.
A refresh scans the previous raw range so that hidden items do not shorten the retained window.
It requests another window near the visible end and fetches an earlier window when the user
scrolls back. Requests run outside the drawing loop. Failed page requests retain the current
cards and stop after three attempts. Leaving the row cancels its paging demand. Returning to
the row can start another attempt. A normal refresh reloads the current window. A profile change
starts from the new profile's preview. Both operations reject old results.
The shared shelf widget draws only visible cards. Paging preserves the banner's current items.

Home holds cards for at most `HOLD_ROWS` (16) rows at a time, each a window of at most
`MAX_SHELF_ITEMS` (24) cards, plus at most eight retained banner items. Every row keeps a
descriptor, so none is dropped; a row outside the held range publishes placeholders and is
read again at its remembered offset when it returns.

Verified Continue Watching item (movie, trimmed):

```json
{"ratingKey":"2029","key":"/library/metadata/2029","type":"movie","title":"Obsession",
 "year":2026,"thumb":"/library/metadata/2029/thumb/1783020218",
 "art":"/library/metadata/2029/art/1783020218",
 "duration":6543120,"viewOffset":131703,"audienceRating":7.2}
```

**Hub `title` is PMS-owned text, localized server-side by `X-Plex-Language` — and PMS's own
per-string translation coverage for a tag is partial, which this app now papers over for every
STANDARD hub rather than forwarding it as-is** (issue #12, investigated 2026-09-28: a Belarusian
UI showed some hub titles in Belarusian and others in Russian/English on one Home screen).
`plex::client::headers`/`pms_headers` still send the literal selected UI tag (`en`/`es`/`be` —
`identity::language()` → `i18n::current().language().tag()`) on **every** PMS operation, hubs
included (`rust-modules/plex/src/plex/client.rs`'s
`pms_headers_carry_the_literal_selected_ui_language_be_included` test pins that for all three
shipped tags) — that part of the earlier record stands. What changed is that neither
`screens/home/mod.rs` nor a library's own browse grid renders `hub.title` unconditionally:
`plex::hub_title::localized_hub_title` sits between a PMS `Hub` and the row the screen draws, at
BOTH call sites — Home's whole-catalog merge (`pms.rs::project`, one row per source, `/hubs`) and
a library's own shelves (`browse::section_hubs::parse_hubs`, `/hubs/sections/{id}`) — one shared
table so the two cannot drift apart. It overrides the title, **unconditionally** (not gated on
whether the selected tag happens to be one PMS translates), for a hubIdentifier this catalog
recognizes, and the override differs by **scope** because PMS itself titles the "Recently Added"
family differently at the two endpoints (§3 vs §3a):

| scope | hubIdentifier | client-side override | locale key |
|---|---|---|---|
| Home | `home.ondeck` / `home.onDeck` | "On Deck" | `browse.home.hub.on_deck` |
| Home | `home.playlists` | "Recent Playlists" | `browse.home.hub.recent_playlists` |
| Home | `home.movies.recent` / `.television.recent` / `.music.recent` / `.videos.recent` / `.photos.recent`, when the identifier names the household's ONLY hub of that type in the response | "Recently Added Movies" / "…TV" / "…Music" / "…Videos" / "…Photos" | `browse.home.hub.recently_added_movies` / `_tv` / `_music` / `_videos` / `_photos` |
| Home | the same 5 `home.*.recent` identifiers when PMS mints MORE THAN ONE hub under the same identifier (two same-type libraries), and any `movie.recentlyadded.<id>` / `show.recentlyadded.<id>` / `tv.recentlyadded.<id>` | "Recently Added in {library}" (library = the hub's own `librarySectionTitle`) | `browse.home.hub.recently_added_in` |
| Section (`/hubs/sections/{id}`) | `movie.recentlyadded.<id>` / `show.recentlyadded.<id>` / `tv.recentlyadded.<id>` | "Recently Added" (no library name — the section page already is that library, and PMS itself drops the qualifier here) | `browse.library.hub.recently_added` |

The per-type Home wording ("Recently Added Movies") is right only when nothing needs
disambiguating: `home_keeps_recently_added_rows_for_two_same_type_libraries`
(`pms_multi_source_merge_tests.rs`) measured PMS minting one `home.television.recent` hub PER TV
library when a household owns more than one, each with the library folded into `title`
("Recently Added in TV" vs "…in TV HDR") — so `pms.rs::project` counts occurrences of each
`hubIdentifier` in the response BEFORE choosing a wording (`hub_identifier_counts`,
`identifier_is_unique`), and only a hub that is the sole one under its identifier gets the
per-type form (`one_movie_and_one_tv_library_get_the_natural_per_type_recently_added_titles`). A
household with exactly one library of a type gets a `home.movies.recent` hub that is really that
ONE library's shelf wearing the whole-server identifier — the per-type form, not "Recently Added
in Movies", which reads oddly when there is nothing to disambiguate. `home.continue` never reaches
this table at all: Continue Watching's `HubRow.title` has never come from PMS — it is set from
`i18n::msg::browse_home_continue_watching()` where the dedicated `/hubs/continueWatching` deck is
merged into `HubRow`s (`pms.rs::merge_with_scope`) — and the standard-hub override above is the
SAME rule (an unconditional client-side string, not one gated on PMS's per-tag coverage) applied
to the shelves Continue Watching's own fix never touched. A per-section deck
(`movie.inprogress.<id>` / `tv.inprogress.<id>`, §3a) and every other section-hub family this
catalog has not specifically enumerated — a rotating genre/actor rail, a collection
(`custom.collection.*`) — keep drawing `hub.title` verbatim at BOTH scopes, exactly as before:
there is still no substitute catalog for arbitrary server-owned text, and no live evidence (this
file, `docs/plex-openapi.json`, or the repo's own fixtures) that those families need one. Tests:
`pms_multi_source_merge_tests.rs`'s `a_recently_added_library_hub_renders_the_be_catalog_string_under_a_be_ui`,
`one_movie_and_one_tv_library_get_the_natural_per_type_recently_added_titles`,
`a_lone_movie_library_renders_the_be_per_type_catalog_string_under_a_be_ui`, and
`an_unrecognized_hub_identifier_keeps_the_pms_title_verbatim` (Home scope); `section_hubs.rs`'s
`a_be_ui_localizes_the_section_recently_added_hub_and_leaves_an_unknown_one_alone` (Section scope).
This cannot be verified further against a live PMS from here (the mock server under
`tests/mock_pms.py` does not model per-string translation coverage, by design — see its own header
comment); do not infer a different PMS-side fallback chain (e.g. "falls back to the account's
region") from the one field report that started this.

Hub items **do include full `Media[].Part[]`**, so Continue Watching can direct-play
without a second metadata fetch. Resume position = `viewOffset` ms.
`home.ondeck` items are episodes with `grandparentTitle`, `parentIndex`, `index`,
`grandparentThumb` present.

Recommendation for the app: use `/hubs/promoted?count=12` for the home screen
(one request → Continue Watching + On Deck + Recently Added + collections),
and hub `key` + paging for "see all".

---

## 3a. Per-library hubs — `/hubs/sections/{id}` (verified live 2026-09-05, PMS 1.43.3)

```
GET /hubs/sections/{sectionId}?count=12&X-Plex-Token=...
```

**This is the library's own shelf list, and it is the SERVER OWNER's setting, not ours.** It is
the `Library Recommended` column of *Plex Web → Manage → Libraries*, in the order the owner
arranged by dragging. Nine rows there is nine hubs here, in that order — verified by comparing a
live response against the owner's own table on 2026-09-05, row for row.

Measured on this server, and every one of these is a thing the OpenAPI spec does not say:

* **Items ARE embedded** in each hub's `Metadata[]`. `docs/plex-openapi.json`'s example shows
  hubs with a nonzero `size` and an EMPTY `Metadata` array, which would have meant a second
  request per shelf; that is an artifact of the example, not the endpoint.
* **Empty hubs are still returned**, with `size: 0` and no `Metadata`. On a sparse library that
  is most of them — one movie section here answered with 6 hubs of which 5 were empty. **A
  client that draws what it is given draws five headings over nothing**, so dropping an empty
  hub is required, not a nicety.
* **`count` defaults to 6** and is honoured up to at least 24. It is items-per-hub; it never
  changes how many hubs come back.
* **The spec gives the hub list no paging** (the route's parameters are `count`, `onlyTransient` and
  `identifier`); a windowed request pages it on a live server (see [Paging, observed](#paging-observed)).
  The Library reads it in windows of `HUB_WINDOW` hubs (`paging::read_hub_list`; `library_hubs` remains the
  whole-list fallback for a server that ignores the window), so a server with many promoted collections never
  answers with all of them at once. The Library keeps a descriptor for every hub the windows
  bring; cards are held for at most `HOLD_ROWS` (16) rows, each a window of at most
  `MAX_SHELF_ITEMS` (24) cards that pages through the hub's own key.
* **`onlyTransient` is a no-op on this server** — `0`, `1` and absent all returned the same nine
  hubs. Do not send it and do not rely on it.
* **`Accept: */*` returns XML here too**, like every other PMS route. The client's explicit
  `Accept: application/json` is what makes this parse at all.
* **A hub's `type` can be `mixed`** (`tv.recentlyadded.N`), so an item's own `type` is the only
  thing worth reading. `pms::parse_item` already does that.
* **Continue Watching is present, scoped to the section**, as `movie.inprogress.N` /
  `tv.inprogress.N` with `key=/hubs/sections/N/continueWatching/items`. **It is NOT
  `home.continue`**, so `pms::hub_is_continue`'s match does not carry over — a per-section
  shelf is identified by the `*.inprogress.*` id or that `key`.
* **A collection is a hub like any other** (`custom.collection.N.<id>.<id>`,
  `key=/library/collections/{id}/children`), which is why the owner's table lists
  "Toy Story Collection" in the same column as "Recently Added".
* **PMS titles a `*.recentlyadded.<id>` hub plain "Recently Added" here — no library name**,
  unlike the same hubIdentifier on the whole-server `/hubs` (§3's "Recently Added in Movies").
  This route is already scoped to one library, so there is nothing to disambiguate. This app's own
  client-side override (§3's table, Section row) reproduces that: `browse.library.hub.recently_added`,
  not `browse.home.hub.recently_added_in`.

**The trap worth carrying: two of these hubs CHANGE IDENTITY BETWEEN REQUESTS.** The genre and
the actor/director shelves rotate their subject on every call — six consecutive requests
returned `movie.genre.1.3897`, `.153`, `.48`, `.150`, `.152`, `.149`, and
`movie.by.actor.or.director.1.<id>` likewise, sometimes answering with zero items and therefore
vanishing from the drawn set entirely. So a refetch of one library can legitimately return a
different SHELF COUNT and different shelf IDs with no change on the server. Anything that
remembers a position by `hubIdentifier` needs a fallback for an id that simply is not there any
more, and any UI that lays out below these shelves must not let a refresh move the ground under
a viewer.

Example shape (trimmed; one populated hub and one empty one):

```json
{"MediaContainer":{"size":9,"librarySectionID":1,"librarySectionTitle":"Movies","Hub":[
  {"hubIdentifier":"movie.inprogress.1","title":"Continue Watching","type":"movie","size":2,
   "more":false,"key":"/hubs/sections/1/continueWatching/items","Metadata":[
     {"ratingKey":"1001","key":"/library/metadata/1001","type":"movie","title":"Alpha",
      "year":2001,"thumb":"/library/metadata/1001/thumb/1","duration":6124864,
      "librarySectionID":1,"viewOffset":1048421}]},
  {"hubIdentifier":"movie.by.actor.or.director.1.3932","title":"Top Movies with …",
   "type":"movie","size":0,"more":false,
   "key":"/library/sections/1/all?unwatched=1&actor=3932&sort=audienceRating:desc"}
]}}
```

**Not yet established**, and both need someone other than this server to answer: whether a
**shared-library token** authorizes this route at all and what it returns when it does not, and
a direct before/after proof that **reordering** a row in *Manage → Libraries* reorders the
response (the order matching the owner's table exactly is strong evidence, not a controlled
test).

---

## 3b. Search — `/hubs/search` (verified live 2026-08-14, PMS 1.43.3)

```
GET /hubs/search?query=wallace&limit=8[&sectionId=1]&X-Plex-Token=...
```

**The spec is right about the parameters and wrong about the response.** Take `plex-openapi.json`'s
`sectionId` and `limit` descriptions — both were checked live and both hold. Do **not** take its
`simpson` response example, which files the `show` hub under `Directory`: read that and you
conclude shows arrive as `Directory[]` and people as `Metadata[]`, the exact inverse of the truth.
That example is why the split below was probed rather than read.

Everything here was measured across six queries, three `sectionId` variants and four `limit`
variants. Where the spec and the server disagree, the server wins.

### The payload arrives in TWO containers, and the hub's type says which

| container | hubs |
|---|---|
| `Metadata[]` | `movie`, `show`, `episode`, `album`, `artist`, `track` |
| **`Directory[]`** | **`actor`, `director`, `collection`** |

This is the one fact the whole search screen rests on, and getting it wrong fails **silently**:
nothing errors, no field is missing, `Hub.size` still reports 3 — the Cast & Crew and Collections
shelves simply draw nothing, because the reader looked in `Metadata` and the rows were in
`Directory`. **Two** of the app's five search shelves, gone, with a green parse — three hub types,
but `search::Kind::hubs` folds `actor` + `director` into the single Cast & Crew shelf. (Modelled as
`plex::Hub.directory: Vec<Tag>`; pinned by `plex::models`'s fixture tests.)

A `Directory[]` row is the same `Tag` record the cast row on a detail page is built from:

```json
{"key":"/library/sections/1/all?actor=921","librarySectionID":1,"librarySectionTitle":"Movies",
 "reason":"section","reasonID":1,"reasonTitle":"Movies","score":"0.52000","type":"tag",
 "id":921,"filter":"actor=921","tag":"Wallace Shawn","tagType":6,
 "tagKey":"5d776827151a60001f24ab18",
 "thumb":"https://metadata-static.plex.tv/a/people/….jpg","count":5}
```

A **collection** row carries `tag`, `id`, `key`, `count`, `filter`, `librarySectionID`, `reason`,
`reasonTitle` and a `guid` (`collection://…`) — and **no `tagKey`, no `thumb`, no `ratingKey`**. So
`key` is the only handle a collection hit gives you, and a screen that keys tags by `tagKey`
silently drops every collection. **The app therefore sends `includeCollections=1`** on every
search (`plex::Client::search`): the `collection` hub then answers full collection **`Metadata[]`**
rows — `ratingKey`, `index` (== the tag `id`), `thumb`, `childCount`, `UltraBlurColors`, plus
`score` — and the table above holds only without the flag. `search::project` keeps the tag shape as
the fallback for a server that ignores it. A person's `thumb` is an **absolute** `metadata-static.plex.tv`
URL, not a PMS path (§5's transcoder still fetches it, but nothing may prepend the server host).

**The same person arrives once per library section.** Wallace Shawn comes back twice — section 1
`count: 5`, section 2 `count: 3` — with the same `id` and the same `tagKey`, because the hub is a
union of per-section tag listings (that is what `reason: "section"` means). Draw it raw and he
appears twice; keep the first row and you report 5 credits for a person who has 8. Dedupe on
`tagKey`/`id`, sum the counts.

Nested credit arrays on a search `Metadata` row are **trimmed to `{"tag": …}`** — no `id`, no
`tagKey`, no `thumb` — unlike the same arrays on `/library/metadata/{rk}`. Search results are not
a substitute for a detail fetch.

### `sectionId` RANKS; it does not filter

With `sectionId=1` (Movies) the `movie` hub moves ahead of `show`; with `sectionId=2` (TV Shows)
`show` and `episode` come first. **Every row from every other section is still returned** in all
three cases — same items, same counts, different hub order. It cannot scope a search to one
library; the client must filter if it wants that.

The spec says the same thing and is easy to read past: *"This gives context to the search, and can
result in re-ordering of search result hubs."* Re-ordering — not filtering. The name is the only
part that suggests otherwise.

An **unknown `sectionId` is a 400**, so never forward a section id the server did not hand you —
and never a placeholder either, per the zero rule below.

### Query and limit

* **A blank/whitespace query is a 400.** A **one-character** query is a 200 with every hub empty
  (`a` → nothing; `to` → seven populated hubs). The app's minimum is therefore 2 characters, and
  the first keystroke of a search costs no round trip.
* **`limit` caps each hub separately**, not the response: `limit=2` cut a 6-row `movie` hub to 2
  and left the 1-row `show` hub alone. Omitting it is legal; the server's own default is **3 rows
  per hub** (measured, and the spec agrees: *"The number of items to return per hub. 3 if not
  specified"*).
* **`Hub.size` is the number of rows RETURNED**, already capped by `limit` — never the total match
  count. A "6 results" caption built from it means "6 shown".
* **The wire's `more` attribute is `false` even on a truncated hub** (measured on the 6→2 cut
  above), so it cannot be used to offer a "see all". `plex::Hub` does not model it, for that reason.
* **Hub ORDER moves per query**: `sta` ranks `actor` first, `star` ranks `movie` first. A response
  carries ~17 hubs — every type the server knows — most with `size: 0`. The app fixes its own shelf
  order for this reason; honouring the server's would move a row under a typing user's focus.
* Every row of every hub carries `score`, a float **encoded as a string** (`"0.52000"`). It is not
  modelled; if it ever is, it needs `de_f64` or the whole `MediaContainer` fails.

### The zero rule: omit an optional number, never send `0`

| request | result |
|---|---|
| `limit` omitted | **200** (3 rows per hub) |
| `limit=0` | **500** |
| `sectionId` omitted | **200** (whole server) |
| `sectionId=0` | **400** (no server has section 0) |

Both zeros are errors, so `Client::search` puts both numbers on via `QueryBuilder::opt_int` — 0
means "send no parameter". Using `.int` for either builds a search that never works and blames the
network for it, which is the next paragraph.

### Errors are HTML, so a rejected request looks like a dead socket

Every error here — 400 (blank `query`, `sectionId=0`), 500 (`limit=0`), 404 (bad path) — comes back
as `text/html` (`<html><head><title>Bad Request</title>…`), **not** a JSON `MediaContainer`,
whatever the `Accept` header says. `Client::get_json` parses nothing and returns `None`, which is
the same `None` a dead socket gives — the layer cannot tell them apart. So the search store must
read any `None` as "the request failed", never as "no results"; and a malformed request is not
self-announcing here, it just looks like bad wifi forever.

---

## 4. Item detail & show → season → episode chain

### Movie detail (verified against a movie item, ratingKey 1)

```
GET /library/metadata/1?X-Plex-Token=...
```

Returns one `Metadata[0]` with everything from §2 **plus** `tagline`, `studio`,
`originallyAvailableAt`, `Genre[]/Director[]/Writer[]/Role[]` (`{"tag":"..."}`),
`Rating[]`, `Guid[]`, `chapterSource`, and `Media[].Part[].Stream[]`
(streamType 1=video, 2=audio, 3=subtitle; `codec`, `language`, `languageCode`,
`channels`, `displayTitle`).

#### The Dolby Vision fields on a video stream (verified live 2026-08-21)

**`docs/plex-openapi.json` does not model these.** Its `stream` schema carries 29 properties and
not one `DOVI*` among them, so the spec cannot settle their spelling and neither could this file
until now — they were read off the dev server directly, by sweeping **every movie and episode on
it**: 540 leaves, of which **28 carry Dolby Vision** (8 movies, 20 episodes) across 34 video
streams. A `streamType: 1` stream on a DV file carries **eight** keys, and all 34 send all eight —
no shape on this server reports a profile without also reporting a compatibility id:

```jsonc
"DOVIPresent": true, "DOVIProfile": 8, "DOVILevel": 6, "DOVIVersion": "1.0",
"DOVIBLPresent": true, "DOVIELPresent": false, "DOVIRPUPresent": true, "DOVIBLCompatID": 1
```

Numbers arrive as JSON numbers and the flags as **real JSON booleans**, so every one of them is
read through `de_i64` (which accepts both). A non-DV stream sends **none** of them, which is why
absence and a legitimate zero are indistinguishable field-by-field — see `metadata::Dovi`.

Three are load-bearing for playback, because the buffer-feed pipeline feeds one elementary stream
to a decoder that ignores the RPU, so the **base layer is what the panel gets**:

| field | meaning | measured on this library |
|---|---|---|
| `DOVIProfile` | 5 = single-layer IPT-PQ (**no HDR10 fallback**), 7 = dual-layer, 8 = HDR10/SDR/HLG-compatible base | 26 P8 (6 movies + all 20 episodes), one P7, one P5 |
| `DOVIBLCompatID` | base-layer cross-compatibility: 0 none, 1 HDR10, 2 SDR, 4 HLG | `1` on every P8, `0` on the P5 — and **`6` on the P7** |
| `DOVIELPresent` | an enhancement layer rides in the file | `true` only on the P7 |

**The P7's `DOVIBLCompatID` of 6 is the trap**: a "is it profile 5" test written as
`DOVIBLCompatID == 0` waves every dual-layer file straight through. `DOVIELPresent` is the only
field that identifies P7. Note also that the **P5 item sends no `colorTrc` at all**, so
`DOVIPresent` is the only thing marking it as HDR.

These are on the FULL item fetch only — `/library/sections/{id}/all` returns `Part[]` with no
`Stream[]` array, so no DOVI field is reachable from a grid listing.

#### What the server actually DOES with a Dolby Vision source (verified live 2026-08-21)

**Refusing direct play is not the same as getting a re-encode, and on the file that matters most
the difference is the whole bug.** `/video/:/transcode/universal/decision` was driven directly with
this app's own re-encode query — `directPlay=0&directStream=1&videoResolution=3840x2160&maxVideoBitrate=60000`
(those two values are what the query still sends at the ladder's default `Quality::Auto`; a selected rung replaces them)
plus the dev TV's capability profile — against each of the three profiles:

| item | `Part.decision` | the VIDEO stream's own `decision` | output carries DOVI? |
|---|---|---|---|
| P5, mp4, hevc 3840x1602 @ 24.7 Mbit | `transcode` | **`copy`** | **yes — `DOVIProfile: 5` intact** |
| P7, mkv, hevc 3840x2160 | `transcode` | `transcode` | no |
| P8.1, mkv, hevc 3840x2160 | `transcode` | `transcode` | no |

`Part.decision=transcode` says only that the *container* changes. `directStream=1` is standing
permission to **copy** a track, and PMS takes it whenever the source fits the caps the query
carries — resolution, bitrate, and the profile's `add-limitation` axes, which are width, height and
bit depth. **None of those axes can express "Dolby Vision"**, so the P5 file came back as the same
IPT-PQ bitstream one container down: identical pixels, identical wrong colours, at the cost of a
server transcode session. The P7 transcodes for its own reasons (an enhancement layer is not
copyable), which is why testing only the dual-layer item would have shown a clean result.

Withdrawing the permission is what makes the refusal real. Re-running the P5 with
**`directStream=0` + `directStreamAudio=1`** (video re-encoded, audio still copied):

```
generalDecisionCode 2000 / transcodeDecisionCode 2003
"File is unplayable. DoVi (Profile 5) color space is not supported."
```

So this PMS **cannot convert a Profile 5 file at all** — and says so, in a sentence the player's
read-out quotes verbatim (`route::refusal` fires on general code 2000). That is the honest end of
the road for this file on this server, and it is strictly better than a picture in the wrong
colours with nothing on any surface to explain it. `TranscodeSpec::no_video_copy` is the flag, set
only for a Dolby Vision refusal — a size or codec refusal is one the server's own caps already
express, and a copy that satisfies them is a free win worth keeping.

Two profile limitations were tried first and **both are ignored by PMS 1.43.2** — the decision was
byte-identical to the unmodified one:
`add-limitation(scope=videoCodec&scopeName=hevc&type=notMatch&name=video.dovi.profile&value=5)` and
the `lowerBound` form on `video.dovi.blCompatId`. Don't re-try them; the client profile has no
Dolby Vision axis.

### Review scores — `Rating[]` + the flat pair (verified live 2026-07-29, PMS 1.43.2)

An item carries its review scores **twice**, in two shapes:

```jsonc
"rating": 9.1,          "ratingImage":         "rottentomatoes://image.rating.ripe",
"audienceRating": 8.5,  "audienceRatingImage": "rottentomatoes://image.rating.upright",
"Rating": [
  { "image": "imdb://image.rating",                     "value": 7.4, "type": "audience" },
  { "image": "rottentomatoes://image.rating.ripe",      "value": 9.1, "type": "critic"   },
  { "image": "rottentomatoes://image.rating.upright",   "value": 8.5, "type": "audience" },
  { "image": "themoviedb://image.rating",               "value": 7.8, "type": "audience" }
]
```
(a movie item, ratingKey 2032, verbatim. Some items also carry a `count` per row.)

- **`image` names the provider AND the icon state**, and is the ONLY thing that may pick the badge
  art: `…rating.ripe` = fresh tomato, `…rating.rotten` = green splat, `…rating.upright` = standing
  popcorn, `…rating.spilled` = tipped popcorn. All four occur on this library. Do **not** threshold
  `value` to decide fresh-vs-rotten — RT's critic and audience cutoffs differ and move (on this
  library one item is 4.0 and **rotten** while another is 6.0 and **ripe**, so no threshold works).
- **`type` does not identify the provider.** IMDb and TMDB both arrive as `audience`; only
  Rotten Tomatoes' tomato is ever `critic`.
- `value` is normalised **0–10 for every provider**, including the ones that publish percentages —
  a 91% tomato arrives as 9.1, and a TMDB 78% as 7.8.
- **`Rating[]` is only on `/library/metadata/{rk}`.** A section listing (`/library/sections/{k}/all`)
  sends the flat `rating`/`ratingImage`/`audienceRating`/`audienceRatingImage` pair and no array, so
  a client that reads only the array shows nothing in a grid. Prefer the array (it is the superset
  and carries per-score provider identity); fall back to the pair.
- An absent score is an **omitted field**, not a 0 — so a lenient-parsed 0.0 means "no score".

### Intro / credits markers — `?includeMarkers=1` (verified live 2026-07-29, PMS 1.43.2)

`Marker[]` is **omitted from the default response**, exactly like `Chapter[]`; the app asks for
both on the one metadata GET (`plex::Client::metadata`).

```
GET /library/metadata/{rk}?includeMarkers=1&includeChapters=1
```

```jsonc
"Marker": [
  { "id": 3096, "type": "credits", "startTimeOffset": 3065648, "endTimeOffset": 3130720,
    "final": true,  "Attributes": { "id": 3096, "version": 4 } },
  { "id": 3096, "type": "intro",   "startTimeOffset": 990,     "endTimeOffset": 99625,
    "Attributes": { "id": 3096 } }
]
```

- `type` — `intro` | `credits` (PMS also emits `commercial` on DVR recordings). Offsets are **ms**.
- `final: true` marks a credits segment that runs to the end of the file. It arrives as a JSON
  **bool**, so it needs the lenient `de_i64` like every other Plex flag.
- **The array is NOT sorted by time** — the sample above is verbatim, credits before intro.
- **`id` is not an identity.** Every marker on every item came back as `id: 3096`; key markers by
  kind + offsets, never by id.
- Not every episode has both: of the episodes probed here, one carries intro + credits and two
  carry credits only. Movies generally carry none — coverage is uneven, so never assume either.
- A `final` marker's `endTimeOffset` equals the **container** duration, which the decoder's
  playhead routinely stops short of — treat it as open-ended rather than exclusive, or an
  end-of-item prompt blinks out over the last frames.

### Up Next — the `continuous=1` PlayQueue already carries it (verified live 2026-07-29)

There is no "what plays next" endpoint to call: the `POST /playQueues?continuous=1` the app makes
for **every** playback (§7) returns the show's remaining episodes after the selected one, each a
**full** `Metadata` row — `thumb`, `parentIndex`/`index`, `duration`, `viewOffset`, `summary`, and
`Media[0].videoCodec`/`audioCodec` + `Part[0].key`. That is everything both the Up Next card and a
subsequent direct-play need, so the feature costs zero extra round-trips.

- Rows carry `playQueueItemID`; the container carries `playQueueSelectedItemID`,
  `playQueueSelectedItemOffset` and `playQueueTotalCount`. Find the successor by **item id**, not
  `ratingKey` — a queue may hold the same item twice.
- The queue does **not** span past the available episodes: the last episode a server holds for a
  show returns `playQueueTotalCount: 1`, i.e. no successor. A movie behaves the same way.
- The POST needs an `X-Plex-Client-Identifier`; without it PMS answers with an empty body.

### Show chain (verified against a show, ratingKey 1857)

```
GET /library/metadata/1857/children      → seasons
GET /library/metadata/1858/children      → episodes of Season 1
```

Season entry (verified):

```json
{"ratingKey":"1858","key":"/library/metadata/1858/children","type":"season",
 "title":"Season 1","index":1,"leafCount":8,"viewedLeafCount":0,
 "parentRatingKey":"1857","thumb":"/library/metadata/1857/thumb/1782966880"}
```

Episode entry (verified, trimmed):

```json
{"ratingKey":"1859","key":"/library/metadata/1859","type":"episode",
 "title":"Example Episode","index":1,"parentIndex":1,
 "grandparentRatingKey":"1857","parentRatingKey":"1858",
 "grandparentTitle":"Example Show","parentTitle":"Season 1",
 "thumb":"/library/metadata/1859/thumb/1781586780",
 "grandparentThumb":"/library/metadata/1857/thumb/1782966880",
 "duration":3273248,
 "Media":[{"id":3082,"bitrate":14663,"width":3840,"height":1920,
   "videoCodec":"hevc","audioCodec":"eac3","videoResolution":"4k","container":"mkv",
   "Part":[{"id":3130,"key":"/library/parts/3130/1781467224/file.mkv","size":6001638904}]},
  {"id":3086,"bitrate":2372,"width":1920,"height":960,
   "videoCodec":"hevc","audioCodec":"eac3","videoResolution":"1080","container":"mkv",
   "Part":[{"id":3134,"key":"/library/parts/3134/1781468203/file.mkv"}]}]}
```

Note: episodes can have **multiple `Media[]` versions** (this one: 4K HDR + 1080p).
The picker must iterate `Media[]` and choose by codec/resolution, not take `[0]` blindly.
Episode container metadata also carries `grandparentTitle`/`grandparentThumb` at the
`MediaContainer` level for header rendering.

### Extras / trailers (verified live 2026-09-12, PMS 1.43)

Movie and show metadata can name a primary trailer and a list of extras. The Trailer control
reads **one playable trailer** from that list (`Detail::trailer()`). The detail page draws
every extras row, trailer included, as an Extras shelf (`Detail.extras`). `trailer()` only
picks which of those rows the background preview and Play Trailer use. The hero Trailer disc
is not drawn.

```
GET /library/metadata/{rk}?includeChapters=1&includeMarkers=1&includeOnDeck=1&includeExtras=1
GET /library/metadata/{rk}/extras
```

Verified against this household's PMS (no titles recorded here):

| | movie | show |
|---|---|---|
| metadata GET (today's flags, no extras) | ~25 KB | ~21 KB |
| same GET with `includeExtras=1` | ~45 KB (+20 KB) | ~29 KB (+8 KB) |
| dedicated `GET …/extras` | ~21 KB | ~9 KB |
| extras rows | 14 | 3 |
| playable `subtype=trailer` rows | 2 | 3 |

`?includeExtras=1` nests the **same playable rows** as `/extras` under `Metadata[0].Extras.Metadata[]`
(a dict with a `Metadata` array, not a bare array). Each extra is `type: "clip"` and already
carries `Media[]` / `Part[0].key` / `videoCodec` / `audioCodec` / `duration` — not identity-only.
`Part.key` is a PMS-absolute path that is **not** `/library/parts/…` (online trailer assets);
it still has a part id, a container (`mp4`), and a `Stream[]`. Empty-`Part` extras were not
observed on this server. Folding extras onto the existing metadata GET therefore adds no serial
hop; a dedicated `/extras` GET is the same payload as a second trip.

The client still issues `/extras` **in parallel with** `/related` on movie and show detail only,
rather than `includeExtras=1` on every `metadata()` call: episode and season pages must not pay
the extras blob, and a refused extras GET must not fail the whole page. Serial depth stays 2
(movie) / 5 (show).

**`primaryExtraKey`** is the path `/library/metadata/{rk}` (OpenAPI's spelling; a bare rk was
not observed). It matches both `extra.ratingKey` (the path tail) and `extra.key`. On this
library it named a trailer that was also in the extras list.

**`subtype` / `extraType`** seen together:

| subtype | extraType |
|---|---|
| `trailer` | 1 |
| `behindTheScenes` | 5 |
| `sceneOrSample` | 6 |

Either `subtype == "trailer"` or `extraType == 1` is enough to count as a trailer. Behind-the-
scenes / featurettes / interviews are not this control.

**PlayQueue for an extra.** `POST /playQueues?uri=/library/metadata/{extraRk}&type=video&continuous=1`
returned **HTTP 400** on the simple uri form; the app's real POST uses
`server://{machineIdentifier}/com.plexapp.plugins.library/library/metadata/{rk}`. Sibling extras
under `continuous=1` would be the Up-Next hazard, so trailer sessions **omit `continuous`**
rather than filtering a queue after the fact. `up_next_of` is already episode-gated (`kind ==
"episode"`), so a clip successor would not arm the tile even if a queue came back larger than 1.

Shows carry no `Media` of their own; a show-level trailer still has its extra's `Part`. Episode
and season `/extras` are not requested.

---

## 5. Images (poster / art)

### Transcoded (use this for all UI images)

```
GET /photo/:/transcode?width={w}&height={h}&minSize=1&url={urlencoded thumb-or-art path}
    [&format=png]&X-Plex-Token=...
```

This is exactly what `plex::Client::image_transcode_path` (`rust-modules/plex/src/plex/transcoder.rs`)
builds; no other image request exists. `url` is the URL-encoded value of `thumb`/`art`/
`grandparentThumb` (e.g. `%2Flibrary%2Fmetadata%2F1%2Fthumb%2F1778526065`). `format=png` is sent
only for a clearLogo, which needs its alpha. **No `upscale` parameter is sent.**

**`minSize=1` means COVER: the result fills the w×h box and keeps the SOURCE's aspect.** It is
not cropped to exactly w×h, so its long side can overshoot the box: a 2:3 portrait requested at
300×300 comes back about 300×450. The client depends on this. `ui::Rect::cover_uv` and
`widgets::art_uv` crop the texture to the tile at draw time using its decoded size, and
`img.rs`'s decode-budget limits assume an overshooting long side. *Unverified against a real
PMS:* this is the PMS photo-transcoder semantics the code is written to, and the model
`tests/mock_pms.py` implements (`scale=…:force_original_aspect_ratio=increase`). The results
below are consistent with it, but they cannot tell cover from crop. The poster's source is 2:3
(its raw size is 1920×2880, below), so it already had the box's shape, and the art's source
size was not recorded.

Verified live:

| request | result |
|---|---|
| `width=420&height=236&url=/library/metadata/1/art/...` | 200, `image/jpeg`, 420×236, 29 KB |
| `width=300&height=450&url=/library/metadata/1/thumb/...` | 200, `image/jpeg`, 300×450, 36 KB |

### Raw (no transcode wrapper) — verified, do NOT use for grids

```
GET /library/metadata/1/thumb/1778526065?X-Plex-Token=...
```

Returns 200 `image/jpeg` but at **full original size**: 1920×2880, **1.3 MB**
(vs 36 KB transcoded). ~40× the bytes and a GLES texture upload/downscale per cell —
always go through `/photo/:/transcode`.

### Boxes the app requests

Each image is requested at the size its tile draws, not a multiple of it (the TV panel is 1:1 at
1080p), so a source already at the tile's aspect uploads exactly at tile size. The table is
mirrored in `img.rs`'s decode-budget note.

| UI element | request (all `minSize=1`) | source aspect differs from the box → |
|---|---|---|
| Poster card | `width=250&height=375`, `url=thumb` | long side overshoots; cropped at draw (`art_uv`) |
| Landscape still (episode / shelf) | `width=420&height=236`, still → show art → poster | cropped at draw (`art_uv`) |
| Person headshot, profile avatar | `width=300&height=300`, `url=thumb` | cropped at draw, headshots riding high (`Crop::Headshot`) |
| Profile chip avatar | `width=128&height=128` | cropped at draw |
| Player info panel still | `width=480&height=270` | cropped to its 320×180 box at draw |
| Home hero backdrop | `width=1280&height=720`, `url=art` | overflowed off-panel (`Rect::cover`) |
| Detail backdrop | `width=1920&height=1080`, `url=art` | overflowed off-panel (`Rect::cover`) |
| Hero clearLogo | `width=600&height=240&format=png` | contained in its column (`hero_logo::fit`) |

---

## 6. Direct-play URL

Chain (verified end-to-end against a movie item):

1. `GET /library/metadata/{ratingKey}` → `Metadata[0].Media[i].Part[0].key`
   e.g. `"/library/parts/738/1767473373/file.mkv"`
2. Play `http://<pms-host>:32400{Part.key}?X-Plex-Token=...`

Verified with a range GET:

```
GET /library/parts/738/1767473373/file.mkv?X-Plex-Token=...   (Range: bytes=0-99)
HTTP/1.1 206 Partial Content
Accept-Ranges: bytes
Content-Range: bytes 0-99/12456162321
Content-Type: video/x-matroska
```

Byte-range serving works, so seek-by-range is available to the player.

### `Media[]` fields needed for the direct-play decision

| field | example | use |
|---|---|---|
| `container` | `mkv`, `mp4` | demuxer support check |
| `videoCodec` | `h264`, `hevc` | webOS decoder check |
| `audioCodec` | `ac3`, `eac3`, `aac` | audio passthrough/decode check |
| `width` / `height` | 1920/858, 3840/1920 | ≤ panel/decoder max (4K HEVC ok on most webOS) |
| `bitrate` | 16248 (kbps) | LAN is fine; cap for Wi-Fi profiles |
| `videoResolution` | `1080`, `4k` | coarse version picker among multiple `Media[]` |
| `videoProfile` | `high`, `main 10` | 10-bit HEVC HDR detection |
| `audioChannels` | 6 | downmix decision |
| `Part[].size`, `Part[].duration` | | progress math, buffering hints |

Auto first tries to avoid an encoder entirely. Local uses Original immediately. On a direct Remote,
PlxNative samples a bounded prefix of the actual Part and admits direct play or a codec-preserving
remux when a completed response delivers at least as fast as the source consumes. There is no
fixed bitrate-headroom multiplier: short-term VBR risk is observed later in the playable-buffer
derivative rather than guessed from the whole-file average. An incomplete response or HTTP/
transport failure means capacity is unknown, not zero; Relay and an unknown result begin on HLS
(`/video/:/transcode/universal/start.m3u8`). The official Chromium/webOS client's “Checking
connection speed” uses the same physical object—an ordinary uncapped raw Part GET before
`player.open()`, not a dedicated PMS speed-test endpoint. PlxNative bounds that read to one finite
Range response and gives it the playback's durable logical resource identity so the winning route
can reuse it. The progressive path continues measuring successful body-read time and normalized A/V buffer time, so
a later bandwidth collapse can replace Original with HLS at the current position — decided on a
starvation horizon in seconds (`T_starve = B·R/(R−C)`) rather than on a window count, and the
conservative estimate, not the last window's raw rate, selects the replacement rung directly however
far down the ladder it is. The
measured PMS exposes one fixed rendition per encoder session, so PlxNative performs HLS adaptation
by priming a separately named encoder and committing only after complete candidate media is
decodable and its direction-specific conservation/reserve transaction passes. Conservative body
delivery and reserve refill order experiments; the candidate's complete end-to-end acquisition
then grades wall-clock feasibility. That acquisition includes PMS wait, pacing and path transfer,
so it is not also charged as an independently identified production constraint. The calibrated
PMS-work class remains a recurring cost in Original/HLS utility. Hysteresis is a per-switch penalty
decaying over the playback's own transition
history rather than a hold-down counter, so a single slow PMS segment cannot flap 20→8→20 Mbit/s.
Returning to Original requires neither the top rung nor a fixed number of probes. A bounded Part
probe is admitted only when the current reserve can pay both finite probe phases while preserving
the next HLS acquisition (`B >= 2P + max(R_s,D)`). It runs under the exact active HLS resource id,
without stopping or closing that route. An insufficient or failed request is rearmed only when
confidence-separated stronger HLS evidence appears. After a completed terminal comparison, an
upward HLS commit instead re-scores retained source evidence, while a downshift retires that
old-regime result before a fresh bounded request. There is no spacing timer and a demand-capped
response is not treated as spare link capacity. Each completed source observation updates an
estimate whose own uncertainty decides whether source consumption is sustainable. The accepted
playlist subset, session-ID precedence, safe probe tooling
and redacted live evidence are documented in `docs/pms-hls-protocol-probe.md`, and the controller
itself in `docs/adaptive-playback.md`. Original and the five
user-selected fixed rungs remain on the progressive direct-play/start.mkv paths described above.

---

## 6a. Plex Pass audio DSP — `boostDialog` / `normalizeLoudness` (issue #266, measured against PMS 1.43.4.10903 with Plex Pass)

Two universal-transcoder query params, each `1` or absent (never `0` — omit to mean off). PMS
accepts them on the transcode leg only; `Stream.canNormalizeLoudness` (bool, lenient-decoded like
every other PMS bool) says per-track whether the server has the loudness analysis the DSP needs.
Every session opened for these measurements was stopped afterward; see `/tmp/plx266/measurements.md`
for the raw captures behind this table.

| # | Request | Result |
|---|---|---|
| M1 | MDE shape `directPlay=1&directStream=1&directStreamAudio=1` plus either param | direct play becomes a Part TRANSCODE: video copy, audio transcoded to ac3 with the same channel count. `directPlayDecisionText` never names the enhancement — there is no wire signal that a DSP-driven remux differs from an ordinary one. Both params `=0` reproduces the baseline (no remux). |
| M2 | Remux shape `directPlay=0&directStream=1&directStreamAudio=1` (profile matroska, hevc/h264, ac3/eac3) | AC3 2.0: the baseline COPIES the audio; adding a param TRANSCODES it (audio decision `copy`→`transcode` is directly observable — the wire test for "did the server honour the ask"). AAC 5.1: audio transcodes to ac3 6ch with or without the params (already transcoded at baseline, so the ask is unobservable there — this is `EnhancementOutcome::Unverified`). |
| M3 | Re-encode shapes (`directStream=1` with a quality ceiling; `directStream=0&directStreamAudio=1`) | Audio transcodes even at baseline, and a server-selected SRT is burned into the video. The params change nothing observable — the enhancement is never offered on a re-encode rung (I5) precisely because there is nothing here to verify. |
| M4 | Enhanced remux plus `subtitleStreamID` for an embedded SRT | `subtitles=embedded` or `=sidecar`: video copy, subtitle decision `unavailable` — PMS refuses to carry a text subtitle into the progressive MKV the enhancement produces. `subtitles=auto`: the server instead re-encodes the video and burns it. Either way a subtitle and the enhancement cannot share a route, which is I6. |
| M5 | Part GET (`Range: bytes=0-1023`) on a transcode session, after MDE, after MDE followed by an enhanced-remux decision on the same session, and after only an enhanced decision | 206 Partial Content in every case from a host, including with the remux encoder still live, stopped physically, stopped with `closeResourceSession=1`, or abandoned. PR 4's device run nonetheless met a **503** on the Part right after releasing an enhanced remux; what PMS keyed it on did not reproduce off the television, so a release asks for the Part before it trials it and falls to the plain remux on a refusal (`route::decision::admit_original_part`). |
| M6 | MDE `/decision` on a session whose transcoder is live | The MDE ENDS that transcoder: an HLS session's later segments answer 404 (200 without the MDE) and its stop 404; a progressive session's `start.mkv` at an offset on the same id answers 400 until it is re-decided. Re-issuing MDE is harmless only when nothing is live — never before a Part GET whose rollback needs the encoder still running. |
| M7 | Enhanced remux plus an explicit `subtitleStreamID=<id>&subtitles=burn` for an embedded subtitle, instead of the `=embedded`/`=sidecar`/`=auto` shapes M4 measured | **Verified live (PMS 1.43.4, host, Guest identity, 2026-09-29)**, against our own transcode query shape plus `normalizeLoudness=1`. Enhanced remux + `subtitles=embedded`: video copy, the text subtitle is absent from the decision entirely, and a PGS one decides `unavailable` — confirming M4's refusal generalizes past SRT. Enhanced + `subtitles=burn` at a re-encode flavour (`videoResolution=3840x2160&maxVideoBitrate=60000`) on HDR10 4K sources, both SRT and PGS: video transcodes to HEVC 10-bit with `colorTrc` kept at `smpte2084` (confirmed by `ffprobe` on the output: `yuv420p10le`/`smpte2084`), the hardware encoder runs it, throughput measured 2.0–2.6× realtime, source resolution is kept (no downscale), and the PGS case's own decision reads `burn`. The SRT case lists no subtitle Stream in the decision at all even though the video is re-encoded — the burn happens, but the decision body gives no wire signal of it for a text track; a visual (on-screen) confirmation that the burned text actually appears is still pending the device run. The SAME sources with the enhancement OFF cost the identical video transcode — the burn itself is the cost, not the DSP params riding along with it. A Dolby Vision Profile 8 source asked to burn: PMS copies the video (`DOVIPresent=1` in the decision, matching `EnhancementRoute::RemuxDropsDolbyVision`'s "video copy" half) and silently drops the subtitle — confirming `DisabledReason::DolbyVisionSubtitle`'s refusal is not just this app being conservative; the server itself cannot do both. Official Plex clients ask for `subtitles=burn` explicitly rather than relying on `=auto`'s refuse-then-decide path M4 found; this app now does the same: an embedded subtitle forces `remux: false` (a real re-encode, `EnhancementRoute::Burn`) carrying both the DSP params and the burn request in the one decision, so the subtitle is never silently dropped (superseding I6's "cannot share a route" for the embedded case). An external sidecar stays on the ordinary enhanced remux and is drawn by the app over it: a remux contract never names a subtitle (see M8), and picking or clearing a sidecar on a live remux rebuilds nothing. |
| M8 | **UNMEASURED** — a remux (plain or enhanced) whose subtitle the app draws sends `subtitleStreamID=0&subtitles=none` on the transcode leg (`route::plan::transcode_spec`, `TranscodeSpec::client_subtitles`) | The spelling is the MDE handshake's client-rendered mode (`Client::mde_decision`), which PMS has accepted before; it has not been sent on a `/video/:/transcode/universal/decision` or `start.mkv` request. Expected: video copy, subtitle absent from the decision, no re-encode. A TV or host run must confirm PMS does not answer `auto`'s refuse-then-decide path or burn the part's own selected subtitle instead. **A TV run on 2026-10-06** showed PMS serving the Part to a fresh session id (`<live>-subs`) during a live remux, which is what the side reader (`player::subside`, `route::side_reader_target`) reads for an embedded subtitle. **Measured on the same TV (2026-10-06):** a Part `Range` read during a live remux answers 503 when it carries that remux session id and 206 with a fresh or absent session id; so a release from a remux (or burn) back to Direct Play (`route::decision::OriginalRecoveryPlan::direct_session`) probes and opens the Part on a freshly minted session id and stops the old remux once decoded frames confirm, while a release from a live hls encoder keeps that encoder's id (the Part shares its Streaming Resource). Whether `subtitles=none` on the start leg keeps the video a copy is still UNMEASURED: that run sent the dev trigger's remux with no subtitle selected. |

**Known device-only gap on M5 (`audio_enhancement_normalize_reset`, `tests/manifest.json`, 2026-09-29).**
Admitting the Part before the trial (`admit_original_part`) does not close the 503 in every case (the admission and the trial now read the Part on a fresh session id rather than the remux's, per M8, which this entry predates; whether that closes the case below is unmeasured):
with the enhanced `start.mkv` actually **playing** first — timelines posted on that same session id,
not just decided — the admission's own Part GET still meets HTTP 503 on the television, and the
release honestly lands on the plain codec-copy Original remux (`enhancement: server refused the
Original Part (HTTP 503); restoring Original as a remux`) instead of Direct Play. Host probes as
Guest against the same server and Part identity could not reproduce this: every Part GET came back
200/206, with the remux encoder live, stopped, or abandoned, same session id, identical header keys.
The one variable a host cannot create is the posted timelines, so the leading untested hypothesis is
that PMS refuses a raw Part on a session it has already seen post `state=playing` timelines (a
"session lacking permission to direct play" style refusal) — untestable from a host because a
timeline post is a real watch-history write. The harness case is `known_gap` (XFAIL) until this is
understood or worked around; a release from a session that has posted playing timelines returns to
Direct Play only when PMS admits the Part, otherwise it returns to the plain Original remux.

**Client-side reading.** `route::plan::enhancement_availability` (I1-I7, M7) gates the ASK and its
flavour (`EnhancementRoute::Remux` / `RemuxDropsDolbyVision` / `Burn`, or a `DisabledReason` for
every gate but "no Plex Pass" — see M7); the wire params are never sent outside a Direct/Remux
target even when the viewer's preference is on (M3, I5).
`route::EnhancementOutcome` grades the ANSWER from the params' own observability in the table above:
`Applied` (M1/M2 AC3: the transcode happened and the source codec is in the profile's copy list),
`Unverified` (M2 AAC: transcoded regardless, so honoring the ask cannot be told apart from ignoring
it), `Refused` (the server declined the params outright, or the audio decision came back `copy`
despite them). The player's Audio tab ("Boost dialog"/"Normalize loudness", `appkit/track_menu.rs`)
and the diagnostics Audio-row suffix / `route_line`'s `enh=<word>` both read this outcome, never the
bare ask.

---

## 7. Progress reporting — `/:/timeline` (DOCUMENTED ONLY, not called live)

Sources: python-plexapi `plexapi/base.py` (`Playable.updateTimeline`,
`updateProgress`) and the Plex Web API community docs (Arcanemagus wiki /
plexapi.dev). **Not verified against the live server** to avoid mutating watch state.

```
GET /:/timeline
    ?ratingKey={ratingKey}                 # e.g. 2029
    &key={key}                             # e.g. /library/metadata/2029
    &identifier=com.plexapp.plugins.library
    &state={playing|paused|stopped|buffering}
    &time={positionMs}
    &duration={durationMs}
    [&playQueueItemID={id}]                # only when playing from a play queue
    &X-Plex-Token=...
```

python-plexapi builds exactly:

```
/:/timeline?ratingKey={rk}&key={key}&identifier=com.plexapp.plugins.library&time={ms}&state={state}&duration={ms}
```

Required headers (also accepted as query params) for the session to appear in
"Now Playing" and be attributed to this client:

- `X-Plex-Client-Identifier` — stable unique device id (generate once, persist)
- `X-Plex-Product`, `X-Plex-Version`, `X-Plex-Platform`, `X-Plex-Device-Name` — cosmetic but recommended
- `X-Plex-Token`

**The list above is the MINIMUM, and it is not what a real client sends.** It was written as
"what PMS needs to attribute a session" and then read for years as "the identity set", which is
how this app came to send a strict subset of it. The **official Plex webOS client** sends the
following on every request, and this is the set to match — each one is a separate answer PMS and
plex.tv can key on, not decoration:

| header | what it carries | why it is not cosmetic |
|---|---|---|
| `X-Plex-Client-Identifier` | stable device id | the identity every other field hangs off; generate once, persist forever |
| `X-Plex-Product` | the app's product name | groups sessions and devices in the account's device list |
| `X-Plex-Version` | the app's own version | what a server-side "please update" notice keys on |
| `X-Plex-Platform` | the platform name (`webOS`) | one half of the platform identity |
| **`X-Plex-Platform-Version`** | the **firmware** release (`4.5`, `6.5.2`, …) | the other half. Plex gates behaviour on FIRMWARE, not on platform — this is how a server tells a webOS 4 set from a webOS 11 one |
| `X-Plex-Device` | the device class | |
| `X-Plex-Device-Name` | the user-visible name in *Authorized Devices* | what the account owner reads when deciding what to revoke |
| **`X-Plex-Model`** | the panel model | |
| **`X-Plex-Device-Vendor`** | `LG` | |
| **`X-Plex-Device-Screen-Resolution`** | the screen geometry | a client that never reports it is invisible to any server-side sizing decision |
| **`X-Plex-Language`** | the UI language tag | selects the language of server-returned strings and of metadata where a server has several |
| **`X-Plex-Features: external-media,indirect-media`** | two capability opt-ins | a client OPTS IN to shapes the server will not otherwise offer it. **What each flag unlocks is not verified in this repo** — it is copied from the official webOS client's request, which is evidence that the string is right and not evidence of what it does. Treat it as: send exactly this, do not reason from it |
| `X-Plex-Provides` | what this client offers (`player`, …) | how it appears as a cast target |
| `X-Plex-Token` | the per-server access token | |

**Two things this table is not.** It is not a census of what this app sends, and the count of new
rows is not worth carrying — the **bolded** rows are the ones this reference omitted until
2026-08-23, and the bold is the list. That is a documentation gap rather than a discovery, and the
source is **the official Plex webOS client's own requests** —
tier: another client's observed behaviour, which proves what Plex's own app sends and does not by
itself prove what a server does with each field. Two are worth separating on that basis.
`X-Plex-Platform-Version` is the one with a mechanism you can reason about — Plex demonstrably gates
client behaviour on firmware, and platform alone cannot express "webOS 4.5 versus webOS 11".
`X-Plex-Features` is the opposite: it is the field whose absence is least visible from the client,
because nothing errors — the server simply never offers a shape you did not claim to handle — and
also the one whose exact effect this repo has **not** measured. Settling that needs a live A/B
against a PMS, which nobody has run.

**What THIS app sends is a subset of the table, and the two must not be read as one.**
`rust-modules/plex/src/plex/client.rs::playback_identity` (constants in `plex/identity.rs`) always emits
nine fields: `X-Plex-Client-Identifier`, `-Product`, `-Version`, `-Platform`, `-Platform-Version`,
`-Device`, `-Device-Name`, `-Model` and `-Provides`, plus the token. The central PMS request choke
point sends **`X-Plex-Language` as a header** on every operation, using the UI language resolved
at boot from the app preference and TV locale (English fallback). This is THIS app's own
System-preference resolution (webOS locale → shipped catalog, `i18n::mod.rs`); once resolved, the
literal tag is what goes on the wire, unchanged by whether PMS happens to have a full translation
for it — see §3's hub-title note for the issue #12 case (`be`) this distinction settles.
`X-Plex-Device-Vendor` is sent to
plex.tv's authorized-device surface but not PMS;
`X-Plex-Device-Screen-Resolution` and `X-Plex-Features` remain absent from both. The table is the
target; this paragraph is the state.

The nine playback fields and token are appended as **query parameters** because the raw-socket
transport in `stream.rs` writes its own request line; the language is a header on both transports.
PMS accepts either identity spelling, which is what the "(also accepted as query params)" above
means.

Client behavior expected by PMS (matches official clients):

- send `state=playing` every ~10 s with current `time`
- send on every pause (`state=paused`), resume (`playing`), and stop (`stopped`, final `time`)
- PMS derives `viewOffset` from `time`; when time/duration ≥ ~90% it marks the item watched and advances On Deck
  - Measured live 2026-10-10 (PMS 1.43.4.10903): a `stopped` position under ~1–2 min is silently NOT
    stored (60 s dropped, 120 s kept; watched or not). A `playing` report past the watched threshold
    bumps `viewCount` and clears `viewOffset`. Otherwise `stopped` stores its own `time`, even a lower
    one and even on an already-watched item. The detail read, `/children`, `allLeaves`, OnDeck and the
    PlayQueue row all return the same `viewOffset`.
- simpler alternative (also unverified live): `GET /:/progress?key={ratingKey}&identifier=com.plexapp.plugins.library&time={ms}&state=stopped` — sets progress only; note plexapi warns `time=0` is ignored
- mark watched/unwatched without a time: `GET /:/scrobble?key={ratingKey}&identifier=com.plexapp.plugins.library` / `/:/unscrobble?...`

---

## 8. Subtitle search & download — `/library/metadata/{rk}/subtitles` (verified live 2026-10-04)

The agent-backed subtitle search (the official client's "Search subtitles"). **`plex-openapi.json` is
wrong about this endpoint in three ways and silent in a fourth**, which is why everything below was
measured rather than read: the spec documents only `GET …/subtitles` with the summary *"Add a
subtitle to a metadata item"*, marks it **admin-scoped**, gives it **no response schema at all**,
and documents no download parameter. All four claims are contradicted or unanswered by the server.

Measured against a **shared, non-owned** server (the case the spec's `admin` scope implies should
fail). Both operations succeeded, so the admin scope is not enforced here for a shared library.

### Search — `GET /library/metadata/{rk}/subtitles`

| param | type | notes |
|---|---|---|
| `language` | string | **TWO-LETTER code only** — see the trap below |
| `hearingImpaired` | 0/1 | |
| `forced` | 0/1 | |

Answers a `MediaContainer` whose rows arrive in **`Stream[]`** (`size` = row count; 10 for `en`,
4 for `nl` on the sample item). A row is an ordinary subtitle `Stream` plus provider fields:

```json
{ "id": 1929523, "key": "/library/streams/1929523", "streamType": 3,
  "codec": "srt", "format": "srt", "canAutoSync": false,
  "language": "Nederlands", "languageCode": "nld", "languageTag": "nld",
  "providerTitle": "OpenSubtitles", "score": "2301",
  "sourceKey": "/library/streams/6923797",
  "title": "ALIVE.2020.KOREAN.1080p.WEBRip.AAC2.0.x264-NOGRP",
  "displayTitle": "Nederlands",
  "extendedDisplayTitle": "ALIVE… (Nederlands SRT OpenSubtitles)" }
```

Types observed across every row: `id`/`streamType` int, `canAutoSync` bool, **everything else a
string — `score` included**. `score` is `"2301"`, never `2301`, so it needs the lenient integer
adapter; a strict field here fails the whole container and the screen shows no results at all.

**TRAP — a 3-letter language code returns HTTP 500.** `language=en` answers 200; `language=eng` and
`language=nld` answer a **bare-HTML `500 Internal Server Error`** (an unhandled server exception,
not a Plex error body). The client must therefore send the 2-letter primary subtag, even though the
rows themselves report `languageCode: "nld"`. Fold with `metadata::two_letter_code` before
building the query, and never pass a stream's own code straight through. **Not `lang_key`**: its
grouping key is the lexicographically smallest spelling, which for Dutch is `"dut"`.

**TRAP — candidate `key`s are EPHEMERAL.** Each search mints new ids: the same four Dutch
candidates were `/library/streams/1929511…14` on one call and `…1929520…23` on the next. A download
issued with a key from an earlier search answers **200 and silently does nothing**. A key is only
valid for the search that produced it, so the app must never cache or persist one across searches.

### Download — `PUT /library/metadata/{rk}/subtitles?key={candidate key}`

Answers **HTTP 200 with a zero-byte body**. It therefore names nothing: the id of the stream it
creates is not returned, and the only way to identify it is to diff the item's subtitle streams.

**The install is ASYNCHRONOUS.** The 200 means *accepted*, not *done*. An item fetched immediately
after the PUT did **not** yet carry the new stream; it was present on a later read. A client that
refreshes once, straight after the call, will reliably see nothing — it must re-read until the
stream appears (or give up), never assume one round trip suffices.

**The created stream takes a NEW id, unrelated to the candidate key.** Downloading candidate
`/library/streams/1929514` produced stream **1929519**:

```json
{ "id": 1929519, "key": "/library/streams/1929519", "languageCode": "nld",
  "codec": "srt", "format": "srt", "providerTitle": "OpenSubtitles",
  "selected": true, "title": "ALIVE.2020.KOREAN.1080p.WEBRip.AAC2.0.x264-NOGRP" }
```

`title` carries the provider's release name, which is what lets a diff confirm *which* candidate
landed. It is also the only stable identity a subtitle has across downloads: the SAME subtitle
fetched again takes yet another id, so the client compares release name and language
(`Stream::same_subtitle`) — a Search hit the item already lists reads "Added" and is not offered
again, and a landing that matches a listed entry replaces it instead of adding a second row. Note
`external` is absent (`null`) on this endpoint — the app derives external from
`streamType == 3 && !key.is_empty()`, which holds here.

**PMS SELECTS the downloaded subtitle itself.** `1929519` came back `selected: true` while the
previously selected English stream dropped to `null`. So the server-side selection is already done;
a client still has to make its own client-side renderer pick it up, but it should not assume the
server needs telling.

The delivered file is reachable at the created stream's `key` through the ordinary sidecar route
(§`/library/streams/{id}`), so an OpenSubtitles `.srt` renders through the existing sidecar path.

**A downloaded subtitle is discarded the moment the part's selection moves off it (measured
2026-10-06, from the server's own log).** The stream a download installs is selected by PMS and
lives only while it stays selected. Sequence, ids as placeholders (`<S>` the downloaded stream,
`<A>` the audio stream, `<part>` the part id):

```
T+0     200 PUT /library/parts/<part>?allParts=1&subtitleStreamID=<S>&audioStreamID=<A>   "Selecting subtitle stream <S>"
T+0     200 GET /library/streams/<S>?encoding=utf-8&format=srt                            (the subtitle draws fine)
        ... playback stops; GET /library/metadata/<item> still reports "Subtitle Stream: <S>" ...
T+40s   200 PUT /library/parts/<part>?allParts=1&subtitleStreamID=0&audioStreamID=<A>     "Selecting subtitle stream 0"
T+40s   404 GET /library/streams/<S>?encoding=utf-8&format=srt
T+60s   400 PUT /library/parts/<part>?allParts=1&subtitleStreamID=<S>&audioStreamID=<A>
        (404 / 400 for every later request naming <S>)
```

So moving the selection to `0` (the viewer's Off) or to ANY other stream deletes the downloaded
subtitle for good: it 404s and cannot be re-selected (400), and the viewer has to search again.
That is server behaviour, not the client's. The consequence for a client is a rule: **send
`subtitleStreamID=0` in a selection PUT only when the viewer chose Off** (or nothing is selected).
When the app draws the subtitle itself (a sidecar, or an embedded track beside a remux) the PUT
names the real selected id and "do not burn" is carried only by the transcode decision / start URL
(`subtitleStreamID=0&subtitles=none`), which leaves the server-side selection untouched. The app
sent `0` at every transcode start until 2026-10-06, which is how a freshly searched subtitle was
lost at the next playback.

The same loss, second sequence (2026-10-06): a download installed and selected mid-playback, playback
stopped and restarted two seconds later, and the start's `PUT .../parts/<part>?allParts=1&subtitleStreamID=<E>&audioStreamID=<A>`
named the part's EMBEDDED English track `<E>` (re-picked from the page's pre-playback copy of the
item) while the server's own metadata said `Subtitle Stream: <S>` — moving the selection to a real
stream discards the download exactly as `0` does, so a start must plan from the server's current selection.

`tests/mock_pms.py` (`MockPms.subtitle_search`) models this section — the 3-letter 500, the empty
answer, single-use candidate keys, and an install that creates a NEW selected external stream with
a fetchable sidecar — and is the only place the download may be exercised end to end. Its install
is immediate; the real one is asynchronous, as above. It also models the discard above
(`discard_deselected_downloads`: the stream leaves the part, its `/library/streams/<id>` is a 404, selecting
it is a 400).

---

## Paging, observed

Observed on PMS 1.43.4 with a small library, by read-only GETs. Only the endpoints in the table were
probed; the other listings this file describes are not covered by this section.

A request pages when it carries the window's start (`X-Plex-Container-Start`, as a query parameter or
a header; see the paging gotcha in §2b). A paged answer echoes `offset`, carries the listing's
`totalSize`, and `size` is the number of rows returned. An unpaged answer carries no `totalSize` and
no `offset`: a `similar` listing without a window has none, and a library search without one carries
only `size`.

| Endpoint | Pages? | Carries `totalSize` and `offset` when paged | Notes |
|---|---|---|---|
| `/hubs/continueWatching/items` (the key of the Continue Watching hub) | yes | yes | Windows (0,3) and (3,3) are contiguous; rows are sorted by `lastViewedAt`, descending. The library held six items, so nothing past twelve was seen. |
| `/library/metadata/{id}/similar` (the key of `movie.similar`) | yes | yes | `totalSize` 24 to 26 across calls; windows are contiguous; the related hub's preview is the exact head of the listing; order is stable. |
| `/library/sections/{id}/all` with a `title=` filter | yes | yes | Pages as an unfiltered section listing does. |
| `/library/sections/{id}/collections?title=`, `/library/sections/{id}/all?type=18&title=`, `/library/all?type=18&title=` | yes | yes | Matches collections by title. For the same three-letter prefix, the collection hub of `/hubs/search` returned no rows, so the two match sets are not known to be equal. |
| `/library/search?searchTypes=movies`, `tv`, `people`, `music` | yes | yes | Each row is `{Metadata, score}`; a people row is `{Directory, score}`, with `tagKey` and `count`. `totalSize` is capped by `limit` (`tv` gave 30 at `limit=30`). `searchTypes=movies,tv` returns one mixed list, movies first, then shows, then episodes. |
| `/library/search` with a kind the server does not know (`shows`, `episodes`, `actors`, `bogus`) | not applicable | `totalSize=0` | Empty, with no error: an unsupported kind looks like no hits. |
| `/hubs?count=…` (the hub list) | yes, by hub | yes | A window of three over ten hubs returned three hubs, `totalSize=10`, `offset` echoed. |
| `/hubs/sections/{id}?count=…` (one library's hub list) | yes, by hub | yes | Windows (0,3) and (3,3) over nine hubs; order is stable across two calls. |
| `/hubs/search` with a window | no | not per hub | Not a per-hub offset: seven hubs shrank to one hub with two rows, with `size=7`, `totalSize=17`, `offset=10`. Search hubs carry no key, and `more` is false even on a hub cut at its limit. |
| Key of `movie.topunwatched` and `movie.by.actor.or.director` (random hubs) | yes | not recorded | The listing came back in the same order on two calls; the windows (0,6) and (6,50) together equal the whole listing; every preview card is in it. The library held only 13 items, so large lists are not covered. |


## App data-layer summary

- 3 startup requests: `/library/sections` (find movie/show keys), `/hubs/promoted` (home shelves, items include Media for instant resume), then lazy `/library/sections/{key}/all` pages of `stores::page_cache::PAGE` (60).
- One JSON shape covers movie/show/season/episode; parse the field table in §2 with all-optional semantics.
- Every image through `/photo/:/transcode` at exact card size; never raw `thumb`.
- Direct play = `Media[].Part[].key` + token; server supports byte ranges.
