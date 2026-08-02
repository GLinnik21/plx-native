# Plex Media Server API — reference for the native webOS client

Verified live against a real PMS (`<pms-host>:32400`) by a research workflow, 2026-07.
Sections: Movies=1, TV Shows=2, Music=3. Default response is XML; send `Accept: application/json`.

## Auth / client identity headers (or query params)
- `X-Plex-Token` — required on every request.
- `X-Plex-Client-Identifier` — stable per-install UUID; REQUIRED for transcode sessions + timeline.
- `X-Plex-Product`, `X-Plex-Version`, `X-Plex-Device-Name` — identify the client (session UI).
- **`X-Plex-Platform` — REQUIRED (with a recognized value, e.g. `Generic`) to unlock the `/video/:/transcode/universal/*` endpoints.** Without it they 400. (This was the missing piece.)

## Library browsing + home (Apple-TV layout)
- `GET /library/sections` → `Directory[]`: key(=section id), type, title.
- `GET /library/sections/{id}/all?type=1&sort=titleSort:asc&<tagfilters>` → `Metadata[]`. Filters by id: `genre=`, `year=`, `collection=`, `contentRating=`, `unwatched=1`, `originallyAvailableAt>=-1y`, etc. Pagination: `X-Plex-Container-Start`/`Size` (header OR query). `totalSize` only populated when paginating.
- **`GET /hubs`** (global home) / **`GET /hubs/sections/{id}`** (per-library) → `Hub[]`, each with `title`, `style` (`hero` = the big Continue-Watching banner; `shelf` = poster row), `hubIdentifier`/`context`, `more` (has "see all"), `key` (full-row query), and inline `Metadata[]`. GOTCHA: skip hubs with `size=0` / no Metadata (empty genre/person rows come back empty).
- `GET /library/onDeck` — global next-up. `/library/sections/{id}/collections` + `/library/collections/{rk}/children`. `/library/sections/{id}/filters` + `/sorts` + `/library/sections/{id}/genre` (value lists; tag ids are per-server — discover live, never hardcode).

## Item metadata + hierarchy
- `GET /library/metadata/{ratingKey}` → full fields: title, year, summary, contentRating, `duration`(ms), `thumb`(poster), `art`(backdrop), `viewOffset`(ms, resume point), viewCount, lastViewedAt, `Media[].Part[].key`(part path) + `Media[].Part[].Stream[]`(codecs of video/audio/subtitle).
- Types: Movie=1, Show=2, Season=3, Episode=4. `GET .../children` (seasons, then episodes), `GET .../allLeaves` (all episodes). Episodes carry `grandparentTitle`(show), `parentIndex`(season#), `index`(ep#), `grandparentThumb`.

## Artwork (posters / backdrops / logos)
- `GET /photo/:/transcode?width=W&height=H&url={URL-encoded server-relative thumb/art}&minSize=1` → resized JPEG. (200×300 poster ≈ 17 KB vs 1.3 MB raw.) Raw `GET {thumb}?X-Plex-Token=` also works. Use the transcode for grid thumbnails.
- **Title logo (Apple-TV title treatment):** `/library/metadata/{rk}` returns an `Image[]` with `type`s `coverPoster`(=thumb), `background`(=art), `clearLogo`, `backgroundSquare`. The clearLogo is at `/library/metadata/{rk}/clearLogo/{ts}` — but the raw source may be **JPEG (opaque)**, so fetch it through `GET /photo/:/transcode?format=png&url={URL-encoded clearLogo}` to get a **transparent RGBA PNG** (verified: color_type 6). Use it as the hero/detail title; **fall back to text** when a title has no clearLogo. Because logos are PNG, the vendored image decoder must handle PNG (not JPEG-only).

## Playback — direct-play vs transcode (THE key decision)
Decide per item from `Media[0]` codecs: if `videoCodec` is one the TV decodes (h264) AND `audioCodec` is ac3 AND `container` is mkv → **direct-play** the part URL:
`http://{host}:32400{Part.key}?X-Plex-Token=` (Content-Length delimited, byte-Range seek — the existing pipeline).

Otherwise (HEVC/AV1, MP4/AAC/EAC3/TrueHD, etc.) → **TRANSCODE, and request it AS PROGRESSIVE MKV so the EXISTING MKV+H264+AC3 pipeline plays it unchanged — no HLS/TS/DASH demuxer needed:**
```
GET /video/:/transcode/universal/start.mkv
  ?path=/library/metadata/{ratingKey}
  &protocol=http                 # progressive, single Matroska stream (NOT hls)
  &mediaIndex=0&partIndex=0
  &directPlay=0&directStream=1    # copy audio/video where possible, transcode the rest
  &videoResolution=1920x1080&maxVideoBitrate=20000
  &session={stable-uuid}         # ONE per playback, reuse it
  &X-Plex-Client-Identifier={uuid}
  &X-Plex-Platform=Generic       # REQUIRED, else 400
  &X-Plex-Client-Profile-Extra=add-transcode-target(type=videoProfile&context=streaming&protocol=http&container=matroska&videoCodec=h264&audioCodec=ac3)
  &X-Plex-Token=...
```
Verified: start.mkv bytes are `V_MPEG4/ISO/AVC`(H264) + `A_AC3` (6ch). The `Client-Profile-Extra add-transcode-target(...audioCodec=ac3)` is what forces **AC3** (with `X-Plex-Platform=Generic`); the default/Chrome profile yields AAC which the pipeline can't decode. Do NOT trust the response `Content-Type` (varies).
- Resume/seek within a transcode: pass `&offset={seconds}` (fastSeek) on start; on quality/position change, **stop the old session then start a new one**.
- **Teardown/free the encoder:** `GET /video/:/transcode/universal/stop?session={id}&X-Plex-Client-Identifier={uuid}`.
- Session gotcha: after rapid start/stop cycling the universal endpoints can 400 for ~60–90s, and `/transcode/sessions` size / `transcoderActiveVideoSessions` are NOT reliable "free" gates → mint one stable session id, reuse it, and retry the start with backoff.
- **VERIFIED ON-DEVICE (2026-07-04) — the actual working recipe (start.mkv 400s without these):**
  1. **Handshake first:** `GET /video/:/transcode/universal/decision?<same params>` to REGISTER the session, THEN open `start.mkv`. A cold `start.mkv` → **400 Bad Request** (bare HTML).
  2. **`X-Plex-Session-Identifier` (== `session`) is REQUIRED** (400 without it). Also send `mediaIndex=0&partIndex=0&directPlay=0&X-Plex-Product&X-Plex-Version`.
  3. **The reply is `Transfer-Encoding: chunked`** (no Content-Length). The HTTP client MUST **de-chunk** it — a Content-Length-only reader feeds the demuxer chunk-size lines as garbage and nothing decodes.
  4. **Skip any Cue/byte-index preflight** for a transcode: a 2nd connection to the same live session makes the server cut the main demux stream. Seeking uses `&offset={sec}`, not byte-Cues.
  Result (this app): The Hobbit HEVC/EAC3 → H264/AC3 Matroska plays on the video plane via the existing MKV pipeline.

## Progress / resume (server-side)
- `GET /:/timeline?ratingKey={rk}&key=/library/metadata/{rk}&state=playing|paused|stopped&time={ms}&duration={ms}&X-Plex-Client-Identifier={uuid}` — report progress so the server stores `viewOffset`; comes back on metadata for "resume from where you left off".
- Mark watched: `PUT /:/scrobble?key={rk}&identifier=com.plexapp.plugins.library` / `/:/unscrobble`.

## Client sequencing
1. Startup: `GET /library/sections` → ids/types.
2. Home: `GET /hubs` → hero (Continue Watching) + shelf rows (skip empty).
3. Section grid: `GET /library/sections/{id}/all?type=1&sort=titleSort:asc` (+ `/photo/:/transcode` posters).
4. Detail: `GET /library/metadata/{rk}` (+ `/children` for shows).
5. Play: decide direct-play vs `start.mkv` transcode from `Media[0]` codecs; on stop call transcode `stop` + `/:/timeline?state=stopped`.
