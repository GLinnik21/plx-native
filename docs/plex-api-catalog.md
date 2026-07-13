# Plex API Catalog — reconciled against the official OpenAPI 3.1 spec

Source of truth: `docs/plex-openapi.json` — **Plex Media Server, OpenAPI 3.1.0, spec version
1.2.2, 205 operations**. This document reconciles every PMS endpoint the app currently calls
(from a code audit of `rust-modules/src/`) against that spec: the authoritative operationId,
method, canonical path, parameters, and the `MediaContainer` response shape — plus every
**divergence** between current usage and the spec, and the spec operations that would let the
app **stop hand-rolling** things.

> How to read a section: **operationId** is the codegen/reference name. **Canonical path** is
> what the spec documents; the app's raw path is shown when it differs. Params tables mark
> `in` (path/query/header) and `required`. Response shapes reference the shared schemas below
> so they aren't repeated 13 times.

---

## Shared response schemas (defined once)

Every list/detail endpoint wraps its payload in a `MediaContainer`. The base and the reused
element schemas:

### `MediaContainer` (base, schema `MediaContainer`)
`identifier: string`, `size: integer`, `totalSize: integer`, `offset: integer`. Concrete
responses add one of `Directory[]`, `Metadata[]`, `Hub[]`, `UltraBlurColors[]`, etc. via
`allOf`.

### `metadata` (schema `metadata`, 67 properties) — the movie/show/season/episode item
Fields the app reads, all **present** in the spec unless flagged:

| app reads | in spec `metadata`? | note |
|---|---|---|
| `type`, `subtype` | yes | movie / show / season / episode / clip … |
| `ratingKey`, `key` | yes | `key` is the child/leaf path e.g. `/library/metadata/123/children` |
| `title`, `titleSort`, `originalTitle` | yes | |
| `year`, `index`, `parentIndex`, `absoluteIndex` | yes | `index` = season/episode number; `parentIndex` = season number on an episode |
| `originallyAvailableAt` | yes | date string |
| `duration`, `viewOffset`, `viewCount`, `lastViewedAt` | yes | ms; `viewOffset` = resume point |
| `contentRating` | yes | |
| `summary`, `tagline`, `studio` | yes | |
| `thumb`, `art`, `banner`, `hero`, `theme`, `composite` | yes | image paths → feed to `imageTranscode` |
| `grandparentThumb`, `parentThumb`, `grandparentArt`, `grandparentTitle`, `parentTitle` | yes | show/season art + titles on an episode |
| `leafCount`, `viewedLeafCount` | yes | episode counts for shows/seasons |
| `Media[]` | yes → `media` | |
| `Genre[]`, `Country[]`, `Director[]`, `Writer[]`, `Role[]`, `Guid[]`, `Rating[]` | yes → `tag` | |
| `UltraBlurColors` | **NOT in the `metadata` schema** — but IS returned inline by PMS | see divergence D-1 below |

### `media` (schema `media`) — a Media variant
`id`, `videoCodec`, `audioCodec`, `audioChannels`, `container`, `bitrate`, `width`, `height`,
`aspectRatio`, `videoResolution`, `videoProfile`, `videoFrameRate`, `duration`,
`optimizedForStreaming`, `Part[] → part`. App reads `Media[0].videoCodec` / `audioCodec`
(direct-play vs transcode decision) — both **confirmed**.

### `part` (schema `part`) — a physical file/part
`id`, `key` (e.g. `/library/parts/1/1531779263/file.mov`), `file`, `container`, `duration`,
`size`, `videoProfile`, `audioProfile`, `Stream[] → stream`. App reads `Part[0].key` (the
direct-play URL) — **confirmed**.

### `stream` (schema `stream`, 29 properties) — an audio/video/subtitle track
| app reads | in spec `stream`? | note |
|---|---|---|
| `id` | yes | stream id used as `audioStreamID` / `subtitleStreamID` |
| `streamType` | yes | 1=video, 2=audio, 3=subtitle |
| `codec` | yes | |
| `language` | yes | (also `languageCode`) |
| `audioChannelLayout` | yes | e.g. `5.1(side)` |
| `selected`, `default`, `displayTitle`, `extendedDisplayTitle` | yes | prefer `displayTitle` for the UI |
| `channels` | **MISSING** from schema | real PMS returns it; spec omits it (see D-2) |
| `title` | **MISSING** | use `displayTitle`/`extendedDisplayTitle` instead |
| `hearingImpaired` | **MISSING** | real PMS attribute; spec omits it |
| `audioDescription` | **MISSING** | real PMS attribute; spec omits it |
| `forced` | **MISSING** | real PMS attribute; spec omits it |

### `tag` (schema `tag`) — a Genre/Director/Role/Country entry
`id`, `tag`, `tagKey`, `role`, `thumb`, `filter`, `context`, `ratingKey`. App reads `.tag`
everywhere, plus `Role[].role` and `Role[].thumb` — **all confirmed**.

### `hub` (schema `hub`) — a home/related shelf
`type`, `subtype`, `title`, `hubIdentifier`, `context`, `key`, `hubKey`, `more`, `size`,
`totalSize`, `promoted`, `random`, `style`, `Metadata[] → metadata`. App reads `type`,
`title`, `Metadata[]` — **confirmed**.

### `librarySection` (schema `librarySection`) — a library entry in the Directory list
`key → key`, `type → type`, `title → title`, `art`, `thumb`, `composite`, `agent`, `scanner`,
`language`, `refreshing`, `filters`, `directory`, `Location[]`. App reads `Directory[].type`
and `Directory[].key` — **confirmed** (Directory items are `librarySection`, not the generic
`directory` schema).

---

## Operations the app uses

### 1. `libraryGetSections` — list libraries
- **Method / canonical path:** `GET /library/sections/all`
- **App calls:** `GET /library/sections`  ← **path divergence (D-3)**
- **Params:** none required (X-Plex-Token via header/query for auth).
- **Response:** `MediaContainer.Directory[]` of `librarySection` (+ `allowSync`, `title1`).
- **App reads:** `Directory[].type` (select `movie`, then `show`), `Directory[].key` → i64
  section id. Both fields exist on `librarySection`.
- **Divergences:**
  - **D-3:** App hits bare `/library/sections`; the spec documents this list under
    `/library/sections/all` (operationId `libraryGetSections`). Both resolve to the library
    list on a real PMS, so this is cosmetic, but `/library/sections/all` is the spec-canonical
    path. (Note: real PMS `/library/sections/all` can also mean "all items"; verify on-device
    before switching — the current `/library/sections` is known-working.)
  - App parses `key` as an i64. `librarySection.key` is a string (e.g. `"3"`); parsing the
    numeric section key is fine, but keep the raw string if any library key is non-numeric.

### 2. `librarySectionGetAll` — items in a section
- **Method / canonical path:** `GET /library/sections/{sectionId}/all`  ✅ matches app
- **Params:**

  | name | in | required | type | note |
  |---|---|---|---|---|
  | `sectionId` | path | yes | string | app passes the i64 section key |
  | `mediaQuery` | query | no | object | free-form filter/sort/pagination bag (`type=`, `sort=`, `X-Plex-Container-Start/Size`, …); app sends none |

- **Response:** `mediaContainerWithMetadata` → `MediaContainer.Metadata[]` of `metadata`.
- **App reads:** the full `metadata` field set listed in the shared schema above (title/year/
  contentRating/duration/viewOffset/thumb/art/grandparentThumb/summary/ratingKey +
  `Media[0].videoCodec`/`audioCodec` + `Media[0].Part[0].key` + `UltraBlurColors.*`). All
  confirmed except `UltraBlurColors` (D-1).
- **Divergences:** none on the request. Consider using the `mediaQuery` pagination params
  (`X-Plex-Container-Start` / `X-Plex-Container-Size`) for large libraries instead of pulling
  the whole section at once.

### 3. `hubsGetSlash` — global home hubs
- **Method / canonical path:** `GET /hubs`  ✅ matches app
- **Params:**

  | name | in | required | type | note |
  |---|---|---|---|---|
  | `count` | query | no | integer | app sends `count=12` ✅ |
  | `onlyTransient` | query | no | integer(0/1) | app doesn't send it |
  | `identifier` | query | no | array | filter to specific hub identifiers |

- **Response:** `MediaContainer.Hub[]` of `hub`, each with `Metadata[]` of `metadata`.
- **App reads:** `Hub[].type` (skips album/artist/track/photo/clip/playlist), `Hub[].title`,
  `Hub[].Metadata[]` (same parse as section-all). Confirmed.
- **Divergences:**
  - **D-4:** App sends `excludeContinueWatching=0`. **Not a documented param** on `hubsGetSlash`.
    PMS ignores unknown params, so harmless, but it's a no-op; the documented knob is
    `onlyTransient`. Drop it, or move to the dedicated `hubsGetContinueWatching` hub.

### 4. `libraryMetadataGetSlash` — one item's full metadata (detail page)
- **Method / canonical path:** `GET /library/metadata/{ids}`  (path param is **`ids`**)
- **App calls:** `GET /library/metadata/{rk}` with a single ratingKey
- **Params:**

  | name | in | required | type | note |
  |---|---|---|---|---|
  | `ids` | path | yes | **array** (CSV) | app passes a single id — a valid singleton |
  | `checkFiles`, `skipRefresh`, `asyncCheckFiles`, `includeElements`, … | query | no | integer(0/1) | optional; app sends none |

- **Response:** `mediaContainerWithMetadata` → `MediaContainer.Metadata[]` (app reads `[0]`).
- **App reads (detail):** title/year/contentRating/summary/tagline/studio/
  originallyAvailableAt/duration/viewOffset/art/thumb + `Genre[].tag`/`Country[].tag`/
  `Director[].tag`/`Writer[].tag`/`Role[].tag,role,thumb` + `Media[0].Part[0].Stream[]`
  (id/language/codec/audioChannelLayout/streamType + `channels`/`title`/`hearingImpaired`/
  `audioDescription`/`forced`). All confirmed except the five `stream` fields flagged in D-2.
- **Divergences:**
  - Path param is named `ids` and is an **array** (comma-separated) — the app can batch several
    ratingKeys in one call (`/library/metadata/1,2,3`) if ever useful.
  - **D-2** applies to the streams it reads (see below). No request-side divergence.
- **Second call site** (`fetch_item_streams`, show → first episode) uses the same operation,
  consuming only the `Stream[]` subset. Same notes.

### 5. `libraryMetadataGetRelated` — related shelves for an item
- **Method / canonical path:** `GET /library/metadata/{ids}/related`  ✅ matches app
- **Params:** `ids` (path, required, **string** here — single id). Optional `count`/`excludeFields`
  not read.
- **Response:** `MediaContainer.Hub[]` of `hub` (each with `Metadata[]`).
- **App reads:** flattens `Hub[].Metadata[]`, de-dups by `ratingKey`, caps 20; reads
  `ratingKey`, `title`, `thumb`, `year`, `type`. All confirmed.
- **Divergences:** none.

### 6. Children (seasons / episodes) — **UNDOCUMENTED endpoint**
- **App calls:** `GET /library/metadata/{rk}/children` (seasons of a show) and
  `GET /library/metadata/{season_rk}/children` (episodes of a season).
- **Spec status:** **No such operation.** `/library/metadata/{ids}/children` is **not in the
  spec.** The only `{ids}/{element}` path is a POST/PUT/DELETE artwork setter
  (`libraryMetadataPostElement` / `…PutElement` / `…DeleteElement`, `element` ∈
  thumb/art/clearLogo/squareArt/banner/poster/theme) — **not** a children GET.
- **Closest documented alternative:** `libraryMetadataGetAllLeaves` —
  `GET /library/metadata/{ids}/allLeaves` returns **all episodes** of a show in one call
  (flattened; group client-side by `parentIndex`). This can replace the per-season
  `.../children` episode loop (one request instead of one per season), at the cost of the
  natural per-season grouping.
- **Divergences:**
  - **D-5:** The app depends on `/library/metadata/{rk}/children`, a **real but undocumented**
    PMS endpoint. It works today; just know the official spec doesn't cover it. Response is a
    normal `MediaContainer.Metadata[]` of `metadata` (seasons: `type=season`, with
    `index`/`leafCount`; episodes: `index`/`parentIndex`/`Media[0].Part[0].key`, etc.), which
    the app already parses correctly. Keep it, or migrate season-episode listing to
    `libraryMetadataGetAllLeaves`.

### 7. `imageTranscode` — poster/art image transcode
- **Method / canonical path:** `GET /photo/:/transcode`  ✅ matches app
- **Params (all query):**

  | name | required | type | app sends | note |
  |---|---|---|---|---|
  | `url` | no | string | yes (url-encoded upstream image path) | ✅ |
  | `width` | no | integer | yes | ✅ |
  | `height` | no | integer | yes | ✅ |
  | `minSize` | no | integer(0/1) | yes (`1`) | ✅ |
  | `format` | no | string(jpg/jpeg/png/ppm) | yes (`png` when `png=1`, else default JPEG) | ✅ |
  | `quality`, `upscale`, `blur`, `opacity`, `background`, `blendColor`, `saturation`, `rotate`, `chromaSubsampling` | no | — | no | available knobs |

- **Response:** raw `image/jpeg` / `image/png` / `image/x-portable-pixmap` bytes (not JSON) →
  decoded to RGBA. Confirmed.
- **Divergences:** none. Everything the app sends is documented. (`upscale=1` could sharpen
  small source art; `quality` could trim bandwidth on the TV.)

### 8. `libraryPutPartsPart` — server-side stream selection
- **Method / canonical path:** `PUT /library/parts/{partId}`  ✅ matches app (correct verb!)
- **Params:**

  | name | in | required | type | app sends | note |
  |---|---|---|---|---|---|
  | `partId` | path | yes | **integer** | yes (parsed from part key) | ✅ |
  | `audioStreamID` | query | no | integer | only when audio switched | ✅ |
  | `subtitleStreamID` | query | no | integer | always (0 = subs off) | ✅ |
  | `allParts` | query | no | integer(0/1) | yes (`1`) | ✅ |

- **Response:** no body (200) / 400 if the stream doesn't belong to the part.
- **App reads:** only the HTTP status. Confirmed.
- **Divergences:** none. This is the **correct** mechanism for stream selection (the app also
  redundantly appends `audioStreamID`/`subtitleStreamID` to the transcode GET — those are
  undocumented no-ops there; see D-7). `partId` is an integer in the spec — keep parsing it as
  numeric.

### 9. `transcodeDecision` — register/handshake a transcode session
- **Method / canonical path:** `GET /{transcodeType}/:/transcode/universal/decision`
  with `transcodeType` ∈ {video, music, audio, subtitles}
- **App calls:** `GET /video/:/transcode/universal/decision` (transcodeType=`video`) ✅
- **Documented params (subset the app touches):**

  | name | in | required | type | app sends | verdict |
  |---|---|---|---|---|---|
  | `transcodeType` | path | yes | enum | `video` | ✅ |
  | `path` | query | no | string | `%2Flibrary%2Fmetadata%2F{rk}` | ✅ |
  | `mediaIndex` | query | no | integer | `0` | ✅ |
  | `partIndex` | query | no | integer | `0` | ✅ |
  | `protocol` | query | no | enum(http/hls/dash) | `http` | ✅ |
  | `directPlay` | query | no | integer(0/1) | `0` | ✅ |
  | `directStream` | query | no | integer(0/1) | `1` | ✅ |
  | `videoResolution` | query | no | string | `1920x1080` | ✅ |
  | `offset` | query | no | number | seconds, on seek/retranscode | ✅ |
  | `subtitleSize` | query | no | integer | `100` | ✅ |
  | `subtitles` | query | no | enum(auto/burn/none/sidecar/embedded/segmented/unknown) | `burn` | ✅ |
  | `videoBitrate` / `peakBitrate` | query | no | integer | — | app instead sends `maxVideoBitrate` (D-6a) |
  | `transcodeSessionId` | query | no | string | — | app instead sends `session` (D-6b) |
  | `audioStreamID` / `subtitleStreamID` | — | — | — | app sends them | **not documented on this op** (D-7) |
  | `X-Plex-Client-Identifier` | **header** | **yes** | string | app sends as **query** | (D-6c) |
  | `X-Plex-Session-Identifier` | **header** | no | string | app sends as **query** | (D-6c) |
  | `X-Plex-Platform` | **header** | no | string | app sends as **query** | (D-6c) |
  | `X-Plex-Client-Profile-Extra` | **header** | no | string | app sends as **query** | (D-6c) |

- **Response:** `mediaContainerWithDecision` (a decision `MediaContainer`); app discards it
  (only the server-side session registration side-effect matters).
- **Divergences:**
  - **D-6a:** `maxVideoBitrate=20000` — the spec's bitrate knobs are `videoBitrate` and
    `peakBitrate`; `maxVideoBitrate` is a legacy Plex param (still honored by PMS) not in the
    spec. Consider `videoBitrate=20000`.
  - **D-6b:** `session=plxnative-{rk}` — the spec's session query param is **`transcodeSessionId`**.
    `session` is the legacy param PMS actually honors; the spec name is `transcodeSessionId`
    (plus the `X-Plex-Session-Identifier` header).
  - **D-6c:** The app passes `X-Plex-Client-Identifier`, `X-Plex-Session-Identifier`,
    `X-Plex-Product`, `X-Plex-Version`, `X-Plex-Platform`, `X-Plex-Client-Profile-Extra` as
    **query params**. The spec declares these as **HTTP headers** (`X-Plex-Client-Identifier`
    is a **required header**). PMS accepts X-Plex-* as either, so it works, but header form is
    spec-correct and avoids leaking identity into cached URLs.
  - **D-7:** `audioStreamID` / `subtitleStreamID` on the transcode GET are **not documented**
    params of `transcodeDecision`/`transcodeStart`; server-side selection is done via
    `libraryPutPartsPart` (which the app already does). The GET copies are no-ops — safe to drop.

### 10. `transcodeStart` — the actual transcoded MKV stream
- **Method / canonical path:** `GET /{transcodeType}/:/transcode/universal/start.*`
- **App calls:** `GET /video/:/transcode/universal/start.mkv` ✅ (`start.*` → `video/x-matroska`)
- **Params:** identical set to `transcodeDecision` (same divergences **D-6a/b/c**, **D-7**).
- **Response:** binary stream — `text/html` (DASH MPD), `application/vnd.apple.mpegurl` (HLS),
  or **`video/x-matroska`** (progressive, what the app uses). Consumed by the MKV demuxer
  (`mkv.h`), not by `http_get`. Confirmed.
- **Divergences:** same as `transcodeDecision`. `start.mkv` correctly selects the matroska
  progressive variant.

### 11. Direct-play part fetch — `libraryGetPartsPartChangestampFilename`
- **Method / canonical path:** `GET /library/parts/{partId}/{changestamp}/{filename}`
- **App calls:** the verbatim `part.key` from metadata (e.g.
  `/library/parts/{id}/{changestamp}/file.mkv?...`), opened by the demuxer with a byte
  `Range:` header for seeks. ✅ matches the canonical shape.
- **Params:** `partId` (path, int), `changestamp` (path, int), `filename` (path, string),
  `download` (query, 0/1, not used).
- **Response:** raw media bytes. Notable error codes: **503/509** = "requested the part without
  a decision and no decision could be inferred" — i.e. some server configs require a
  `transcodeDecision` call even for direct-play. The app's h264+ac3 direct-play path works
  today without one; keep this in mind if a direct-play 503 ever appears.
- **Divergences:** none (the key comes straight from `part.key`). The middle path segment is a
  **changestamp** (part updatedAt), not a byte offset — the app treats it as opaque, which is
  correct.

### 12. `timelinePostSlash` — playback progress report
- **Method / canonical path:** **`POST /:/timeline`**
- **App calls:** **`GET /:/timeline`**  ← **method divergence (D-8a)**
- **Params:**

  | name | in | required | type | app sends | verdict |
  |---|---|---|---|---|---|
  | `ratingKey` | query | no | string | yes | ✅ |
  | `key` | query | no | string | yes (`%2Flibrary%2Fmetadata%2F{rk}`) | ✅ |
  | `state` | query | no | enum(stopped/buffering/playing/paused) | `playing`/`paused` | ✅ |
  | `time` | query | no | integer | yes (ms) | ✅ |
  | `duration` | query | no | integer | yes (ms) | ✅ |
  | `playQueueItemID` | query | no | string | no | — |
  | `X-Plex-Client-Identifier` | **header** | **yes** | string | app sends as **query** | (D-8b) |
  | `X-Plex-Session-Identifier` | header | no | string | no | — |

- **Response:** `MediaContainer` (empty); app discards it.
- **Divergences:**
  - **D-8a:** Spec verb is **POST**; app issues a **GET** (its own doc-comment says POST but the
    code calls `http_get`). PMS accepts a GET timeline in practice, but POST is spec-correct.
  - **D-8b:** `X-Plex-Client-Identifier` should be a **required header**; app sends it as a
    query param. Works, but header form is canonical.
  - App does not send `state=stopped` on teardown — sending a final `stopped` timeline (POST)
    is what marks the session ended server-side.

### 13. Stop transcode session — **UNDOCUMENTED endpoint**
- **App calls:** `GET /video/:/transcode/universal/stop?session={id}&X-Plex-Client-Identifier=…`
- **Spec status:** **No such operation.** The documented `/{transcodeType}/:/transcode/universal/*`
  operations are only `decision`, `start.*`, `subtitles`, and `fallback` (POST). There is **no
  `/stop`.**
- **Divergences:**
  - **D-9:** `/video/:/transcode/universal/stop` is a **real but undocumented** PMS endpoint the
    app relies on to free the encoder. No spec-blessed drop-in exists. The nearest documented
    session-control op is `statusPostTerminate` (`POST /status/sessions/terminate?sessionId=…&reason=…`),
    which terminates a **playback session** (different id namespace — `Session.id` from
    `/status/sessions`, not the transcode `session=` token). Keep using `/stop`; optionally also
    send a final `stopped` timeline.

---

## Operations to adopt — stop hand-rolling

These documented operations cover things the app currently improvises or doesn't do yet:

| operationId | method / path | replaces / enables |
|---|---|---|
| `hubsGetSearch` | `GET /hubs/search?query=&sectionId=&limit=` | A real search screen (movies/shows/people/episodes) instead of client-side filtering. |
| `hubsGetContinueWatching` | `GET /hubs/continueWatching?count=` | Dedicated Continue Watching hub — cleaner than the `excludeContinueWatching` hack on `/hubs` (D-4). |
| `hubsGetPromoted` | `GET /hubs/promoted?count=` | The server's promoted/featured rows. |
| `hubsGetSection` | `GET /hubs/sections/{sectionId}?count=&onlyTransient=` | Per-library hubs (Recently Added, On Deck for one section) for a library landing page. |
| `libraryMetadataGetAllLeaves` | `GET /library/metadata/{ids}/allLeaves` | Fetch **all episodes of a show in one call** — replaces the per-season `.../children` loop (D-5); group by `parentIndex`. |
| `libraryMetadataGetSimilar` | `GET /library/metadata/{ids}/similar?count=` | A "More like this" rail (alongside/instead of `related`). |
| `transcodeSubtitles` | `GET /{transcodeType}/:/transcode/universal/subtitles` | Fetch **text (SRT) subtitles** for client-side rendering instead of burning them into the video (`subtitles=burn`), avoiding a re-transcode on every subtitle change. |
| `libraryGetStreamsStream` | `GET /library/streams/{streamId}.{ext}` | Download a **sidecar subtitle stream** directly (e.g. `.srt`/`.vtt`) by stream id — pairs with soft-sub rendering. |
| `putScrobble` / `putUnscrobble` | `PUT /:/scrobble` / `PUT /:/unscrobble?identifier=com.plexapp.plugins.library&key={rk}` | Explicit **mark-watched / mark-unwatched** (the app only reports progress via timeline). |
| `putRate` | `PUT /:/rate?key={rk}&rating={0-10}&identifier=…` | Star-rating an item. |
| `librarySectionGetSection` | `GET /library/sections/{sectionId}?includeDetails=1` | Library detail incl. **Pivots** (built-in filter/category tabs) for a proper library browse UI. |
| `statusGetSlash` | `GET /status/sessions` | Inspect active sessions (get the real `Session.id` for `statusPostTerminate`). |
| `statusPostTerminate` | `POST /status/sessions/terminate?sessionId=&reason=` | Spec-blessed session teardown (complements the undocumented transcode `/stop`, D-9). |

---

## Divergence summary

| id | endpoint | divergence | severity |
|---|---|---|---|
| **D-1** | `metadata.UltraBlurColors` | Spec's `metadata` schema doesn't declare `UltraBlurColors`, **but PMS returns it inline** and the spec's own example shows it — as an **ARRAY**: `"UltraBlurColors": [{topLeft,topRight,bottomRight,bottomLeft}]`. The app reads it as an **object** (`.topLeft`). Verify the app indexes element `[0]`; object access on an array will miss the colors. Dedicated op: `ultraBlurGetColors` (`GET /services/ultrablur/colors?url=`). | **check — possible bug** |
| **D-2** | `stream.channels`, `.title`, `.hearingImpaired`, `.audioDescription`, `.forced` | App reads 5 `stream` fields the spec's `stream` schema omits (real PMS returns them). Spec incompleteness, not an app bug; prefer `displayTitle`/`extendedDisplayTitle` over `title`. | low |
| **D-3** | `GET /library/sections` | Bare path; spec-canonical is `/library/sections/all` (`libraryGetSections`). Both work on PMS. | cosmetic |
| **D-4** | `GET /hubs` | `excludeContinueWatching=0` is undocumented (no-op); documented knob is `onlyTransient`. Use `hubsGetContinueWatching` for that hub. | low |
| **D-5** | `GET /library/metadata/{rk}/children` | Undocumented endpoint (works). Documented alternative for episodes: `libraryMetadataGetAllLeaves`. | low |
| **D-6a** | transcode decision/start | `maxVideoBitrate` → spec name `videoBitrate` (or `peakBitrate`). | low |
| **D-6b** | transcode decision/start | `session=` → spec name `transcodeSessionId` (+ `X-Plex-Session-Identifier` header). | low |
| **D-6c** | transcode decision/start | `X-Plex-*` sent as query params; spec declares them as **headers** (`X-Plex-Client-Identifier` required). | low |
| **D-7** | transcode decision/start | `audioStreamID`/`subtitleStreamID` on the GET are undocumented no-ops; correct mechanism is `libraryPutPartsPart` (already used). Drop the GET copies. | low |
| **D-8a** | `/:/timeline` | App uses **GET**; spec verb is **POST** (`timelinePostSlash`). | medium |
| **D-8b** | `/:/timeline` | `X-Plex-Client-Identifier` sent as query; spec = required **header**. | low |
| **D-9** | `/video/:/transcode/universal/stop` | Undocumented endpoint (works); no spec drop-in. Nearest: `statusPostTerminate`. | low |

**Correct-as-is (no divergence):** `librarySectionGetAll` (2), `libraryMetadataGetRelated` (5),
`imageTranscode` (7), `libraryPutPartsPart` (8 — correct PUT + params),
`libraryGetPartsPartChangestampFilename` (11).

---

## OperationIds the app depends on

Documented operations currently exercised:

1. `libraryGetSections` — `GET /library/sections/all` (app uses `/library/sections`)
2. `librarySectionGetAll` — `GET /library/sections/{sectionId}/all`
3. `hubsGetSlash` — `GET /hubs`
4. `libraryMetadataGetSlash` — `GET /library/metadata/{ids}`
5. `libraryMetadataGetRelated` — `GET /library/metadata/{ids}/related`
6. `imageTranscode` — `GET /photo/:/transcode`
7. `libraryPutPartsPart` — `PUT /library/parts/{partId}`
8. `transcodeDecision` — `GET /video/:/transcode/universal/decision`
9. `transcodeStart` — `GET /video/:/transcode/universal/start.mkv`
10. `libraryGetPartsPartChangestampFilename` — `GET /library/parts/{partId}/{changestamp}/{filename}`
11. `timelinePostSlash` — `POST /:/timeline` (app issues GET)

Undocumented endpoints the app relies on (no operationId in the spec):

- `GET /library/metadata/{rk}/children` — seasons/episodes listing (alt: `libraryMetadataGetAllLeaves`)
- `GET /video/:/transcode/universal/stop` — free the transcode encoder (alt: `statusPostTerminate`)
