# PMS API Reference (verified against live server)

Server: `http://YOUR_PMS_HOST:32400` — PMS apiVersion 1.2.2.
Token: `X-Plex-Token=YOUR_PLEX_TOKEN` (query param or `X-Plex-Token` header). Get yours from Plex → any item → Get Info → View XML.
All endpoints below were verified live on 2026-07-03 with read-only GETs, **except** §7 (timeline), which is documented from community sources only.

**JSON instead of XML:** send `Accept: application/json`. Every response is wrapped in a top-level `MediaContainer` object.

**Paging:** either headers `X-Plex-Container-Start` / `X-Plex-Container-Size` or same-named query params. Response carries `size`, `totalSize`, `offset`.

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

**Section keys on this server: Movies = `1`, TV Shows = `2`** (Music = `3`, ignore for now).
Select sections by `Directory[].type == "movie" | "show"`, not by hard-coded key.

---

## 2. Section item listing (gallery)

```
GET /library/sections/1/all?X-Plex-Container-Start=0&X-Plex-Container-Size=50&X-Plex-Token=...
GET /library/sections/2/all?...        # shows; totalSize: Movies=23, Shows=12
```

Verified movie entry (trimmed to gallery-relevant fields):

```json
{"ratingKey":"1","key":"/library/metadata/1","type":"movie","title":"Frozen",
 "contentRating":"PG","summary":"Fearless optimist Anna teams up with ...",
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

Verified Continue Watching item (movie, trimmed):

```json
{"ratingKey":"2029","key":"/library/metadata/2029","type":"movie","title":"Obsession",
 "year":2026,"thumb":"/library/metadata/2029/thumb/1783020218",
 "art":"/library/metadata/2029/art/1783020218",
 "duration":6543120,"viewOffset":131703,"audienceRating":7.2}
```

Hub items **do include full `Media[].Part[]`**, so Continue Watching can direct-play
without a second metadata fetch. Resume position = `viewOffset` ms.
`home.ondeck` items are episodes with `grandparentTitle`, `parentIndex`, `index`,
`grandparentThumb` present.

Recommendation for the app: use `/hubs/promoted?count=12` for the home screen
(one request → Continue Watching + On Deck + Recently Added + collections),
and hub `key` + paging for "see all".

---

## 4. Item detail & show → season → episode chain

### Movie detail (verified with Frozen, ratingKey 1)

```
GET /library/metadata/1?X-Plex-Token=...
```

Returns one `Metadata[0]` with everything from §2 **plus** `tagline`, `studio`,
`originallyAvailableAt`, `Genre[]/Director[]/Writer[]/Role[]` (`{"tag":"..."}`),
`Rating[]`, `Guid[]`, `chapterSource`, and `Media[].Part[].Stream[]`
(streamType 1=video, 2=audio, 3=subtitle; `codec`, `language`, `languageCode`,
`channels`, `displayTitle`).

### Show chain (verified with "Every Year After", ratingKey 1857)

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
 "title":"Every Summer After","index":1,"parentIndex":1,
 "grandparentRatingKey":"1857","parentRatingKey":"1858",
 "grandparentTitle":"Every Year After","parentTitle":"Season 1",
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

---

## 5. Images (poster / art)

### Transcoded (use this for all UI images)

```
GET /photo/:/transcode?width={w}&height={h}&minSize=1&upscale=1
    &url={urlencoded thumb-or-art path}&X-Plex-Token=...
```

`url` is the URL-encoded value of `thumb`/`art`/`grandparentThumb` (e.g.
`%2Flibrary%2Fmetadata%2F1%2Fthumb%2F1778526065`). `minSize=1` = fill (crop to
exact w×h), `upscale=1` = allow upscaling small sources so returned size is exact.

Verified live:

| request | result |
|---|---|
| `width=420&height=236&url=/library/metadata/1/art/...` | 200, `image/jpeg`, exactly 420×236, 29 KB |
| `width=300&height=450&url=/library/metadata/1/thumb/...` | 200, `image/jpeg`, exactly 300×450, 36 KB |

### Raw (no transcode wrapper) — verified, do NOT use for grids

```
GET /library/metadata/1/thumb/1778526065?X-Plex-Token=...
```

Returns 200 `image/jpeg` but at **full original size**: 1920×2880, **1.3 MB**
(vs 36 KB transcoded). ~40× the bytes and a GLES texture upload/downscale per cell —
always go through `/photo/:/transcode`.

### Size recommendations for 1920×1080 UI

| UI element | request size | notes |
|---|---|---|
| Landscape shelf card 420×236 | `width=420&height=236&minSize=1&upscale=1`, `url=art` (or episode `thumb`) | exact-size JPEG, ~25–40 KB |
| Poster card 300×450 | `width=300&height=450&minSize=1&upscale=1`, `url=thumb` | ~30–45 KB |
| Detail background | `width=1920&height=1080&minSize=1&upscale=1`, `url=art` | fetch once, ~150–300 KB |
| Focus zoom headroom (optional) | request 1.25×: 525×295 / 375×563 | only if cards scale >1.1 on focus |

Requesting the exact card size (no devicePixelRatio multiplier — the TV panel is 1:1
at 1080p) keeps texture memory minimal: 420×236 RGBA = ~400 KB VRAM per card.

---

## 6. Direct-play URL

Chain (verified end-to-end with Frozen):

1. `GET /library/metadata/{ratingKey}` → `Metadata[0].Media[i].Part[0].key`
   e.g. `"/library/parts/738/1767473373/file.mkv"`
2. Play `http://192.168.0.3:32400{Part.key}?X-Plex-Token=...`

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

If direct play fails, the fallback is the HLS transcode endpoint
(`/video/:/transcode/universal/start.m3u8`) — out of scope for this doc.

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

Client behavior expected by PMS (matches official clients):

- send `state=playing` every ~10 s with current `time`
- send on every pause (`state=paused`), resume (`playing`), and stop (`stopped`, final `time`)
- PMS derives `viewOffset` from `time`; when time/duration ≥ ~90% it marks the item watched and advances On Deck
- simpler alternative (also unverified live): `GET /:/progress?key={ratingKey}&identifier=com.plexapp.plugins.library&time={ms}&state=stopped` — sets progress only; note plexapi warns `time=0` is ignored
- mark watched/unwatched without a time: `GET /:/scrobble?key={ratingKey}&identifier=com.plexapp.plugins.library` / `/:/unscrobble?...`

---

## App data-layer summary

- 3 startup requests: `/library/sections` (find movie/show keys), `/hubs/promoted` (home shelves, items include Media for instant resume), then lazy `/library/sections/{key}/all` pages of 50.
- One JSON shape covers movie/show/season/episode; parse the field table in §2 with all-optional semantics.
- Every image through `/photo/:/transcode` at exact card size; never raw `thumb`.
- Direct play = `Media[].Part[].key` + token; server supports byte ranges.
