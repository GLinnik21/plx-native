# Plex API migration map — raw calls → typed `crate::plex` client

> **Status (2026-07-18 — EXECUTED, both layers).** The **read layer** landed first (P1–P3 /
> M1–M5 / D1 below, `plex::install` at boot, all-lenient `de_i64`/`de_f64` DTOs). The
> **playback layer is now migrated too** — rebuilt FROM the live `route.rs` (not this doc's
> original R-list, which had gone stale). The executed shape, where it differs from the plan
> below:
>
> - **Typed ops** live in `plex/transcoder.rs` (`mde_decision`, `transcode_decision`,
>   `transcode_start_url`, `transcode_stop`, the capability profile + `is_dp_audio`),
>   `plex/timeline.rs` (`timeline`, `machine_identity`, `create_play_queue`),
>   `plex/library.rs` (`select_streams`, `direct_play_url`), with request params in
>   `plex/params.rs` (`TranscodeSpec` — rk/session/remux-flavor/audio/sub/offset —,
>   `StreamSelection`, `TimelineReport`, `TimelineState`).
> - **Both `/decision` flavors return the parsed body** (not the planned `get_void`):
>   `route::server_decision` reads `Part.decision` + the verdict codes off `mde_decision`,
>   and `route::apply_decision_codecs` reads the OUTPUT codecs (`Part.Stream[].codec`) off
>   `transcode_decision` — the Load payload must describe what the server actually sends.
> - **Two endpoints the plan never listed** (added to route.rs after it was written) migrated
>   with it: `GET /identity` (`machine_identity`) and `POST /playQueues` (`create_play_queue`,
>   via a new `Client::post_json`; `post_void` carries the timeline).
> - **The playback identity moved into `Client`** (`playback_identity` + the field values that
>   were `route::DEVICE_ID`/`identity_qs`) — the old dead "com.beb.plxnative/Generic" field
>   values are gone; PMS playback keys on the fixed device UUID as before.
> - **`TBASE` died differently than planned**: instead of a stored offset-free query string,
>   `route` keeps a `CUR_REMUX` flavor flag and rebuilds the identical `TranscodeSpec` from
>   (CUR_RK, SESS, CUR_*_SID, flavor) on every seek/retranscode. `offset_secs < 0` = fresh
>   start (no `&offset=`), `>= 0` = encode restart — matching the old obase append.
> - **`route::CFG`/`set_config`/`config()` are deleted entirely** (nothing keeps a playback
>   copy of host/port/token; `plex::client_opt()` preserves the old None-guard semantics for
>   pre-login boots). `timeline_path` is gone from `threads.rs`; both timeline sites collapse
>   onto `route::report_timeline` → `Client::timeline` (POST, the spec verb — D-8a resolved:
>   the old code already POSTed).
> - **Preserved as-is (quirk, verified harmless against the live PMS log 2026-07-19):**
>   `retranscode` still sets `TSESSION = "plxnative-{rk}"` while the transcoder QUERY keeps
>   riding `sess()` — stop/is_transcoding key off TSESSION, the server correlation stays on
>   sess(). The server log shows the **stopped-timeline POST is what actually frees the
>   encoder** (PMS kills the transcoder job on it, ~200ms before our explicit
>   `/universal/stop` arrives), so the explicit stop 404s on EVERY teardown — with the
>   correct uuid or the synthetic id alike. It only matters if playback dies before any
>   timeline fired; no encoder leak either way.
>
> The R1–R6 / T1 / M / P / D sections below are the ORIGINAL plan, kept as history; line
> numbers reference the pre-migration tree.

Call-site-by-call-site plan to move every hand-built Plex path/query in the app onto the typed
`Client` in `rust-modules/src/plex/` (surface: `docs/plex-api-design.md`). Scope: `pms.rs`,
`metadata.rs`, `route.rs`, `posters.rs`, `player/threads.rs` (plus the two sibling sites in
`player/engine.rs` that share the same migration and must move together).

Line numbers are as of the current tree. All paths below are relative to `rust-modules/`.

## Summary

- **17 network call sites** across the 5 files migrate to a typed `plex::` method
  (3 in `pms.rs`, 5 in `metadata.rs`, 7 in `route.rs`, 1 in `posters.rs`, 1 in `player/threads.rs`).
- **3 URL-parse sites** move to `plex::StreamUrl::parse` (`threads.rs:120`, `engine.rs:101` def,
  `engine.rs:167` call).
- **1 sibling network site** in `engine.rs:289` (final `state=stopped` `/:/timeline`) migrates with
  the timeline reporter (the timeline is hand-built in **two** places).
- **2 helpers are deleted** as their callers migrate: `pms::urlenc_str` (→ `plex::client::enc`,
  already reimplemented there) and `metadata::get_json` (→ `Client::get_json`, internal).
- **Stays raw transport by design** (NOT gaps): the 3 demux/cue `http_open` calls
  (`threads.rs:90,159,184`) and the poster-image `http_get` (`posters.rs:285`). Only the
  host/port *source* for the poster fetch changes (Store fields → `client().host()/port()`).
- **Every raw call is covered** by an existing method — no missing endpoint. Flags below are
  behavioral divergences to verify, not gaps.

### Prerequisite wiring (not one of the 5 files, but required first)

**DONE** — `plex::install` (which took `(host, port, token)` when this was written and takes
`(&Origin, token)` since `plex/origin.rs`) is called in the boot/login paths (`app.rs`,
`auth.rs`) before the first PMS read. Remaining follow-ons when the playback layer migrates:

- `route::CFG` keeps only `demo_url` (host/port/token move to the client; `route::config()` callers
  read `plex::client()` instead).

---

## pms.rs (3 sites)

### P1 — `fetch_section` section listing — `pms.rs:160-161`
Current:
```rust
let path = format!("/library/sections/{sec}/all?X-Plex-Token={token}");
let body = match crate::stream::http_get(host, port, &path, Some("Accept: application/json\r\n")) { … };
let json: Value = serde_json::from_slice(&body)…;
let meta = json.get("MediaContainer")…get("Metadata")…as_array()…;
```
Replacement:
```rust
let mc = match crate::plex::client().section_items(sec) { Some(m) => m, None => return start };
for item in &mc.metadata { parse_item(m, item); … }   // parse_item now takes &plex::Metadata
```
`section_items(section_key: i64) -> Option<MediaContainer>`; consume `.metadata` (`Vec<Metadata>`).

### P2 — `pms_fetch_movies` section discovery — `pms.rs:202-204`
Current:
```rust
let secpath = format!("/library/sections?X-Plex-Token={token_s}");
if let Some(body) = crate::stream::http_get(&host_s, port, &secpath, Some("Accept: application/json\r\n")) {
    …json.get("MediaContainer").get("Directory").as_array()… jstr(d.get("type"))… d.get("key").parse::<i64>()…
```
Replacement:
```rust
if let Some(mc) = crate::plex::client().sections() {
    for is_show in [false, true] {
        let want = if is_show { "show" } else { "movie" };
        for d in &mc.directory {
            if d.kind != want { continue; }
            if let Ok(key) = d.key.parse::<i64>() { sections.push((key, is_show)); }
        }
    }
}
```
`sections() -> Option<MediaContainer>`; consume `.directory` (`Vec<LibrarySection { key, kind, title }>`).
The two-pass movies-then-shows ordering stays at the call site.

### P3 — `pms_fetch_hubs` home hubs — `pms.rs:275-276`
Current:
```rust
let path = format!("/hubs?count=12&excludeContinueWatching=0&X-Plex-Token={token_s}");
let body = match crate::stream::http_get(&host_s, port, &path, Some("Accept: application/json\r\n")) { … };
let hubs = json.get("MediaContainer").get("Hub").as_array()…;
```
Replacement:
```rust
let mc = match crate::plex::client().home_hubs(12) { Some(m) => m, None => return 0 };
for hub in &mc.hub {
    if SKIP.contains(&hub.kind.as_str()) { continue; }
    if hub.metadata.is_empty() { continue; }
    for item in &hub.metadata { … parse_item(m, item); … }
    …v.push(HubRow { title: hub.title.clone(), start, len: n - start });
}
```
`home_hubs(count: i64) -> Option<MediaContainer>`; consume `.hub` (each `Hub { kind, title, metadata }`).
Note `home_hubs` **drops `excludeContinueWatching=0`** (D-4, it was a no-op) — behavior-equivalent.

### pms.rs supporting rewrites (data-model, same change)
- `parse_item(m: &mut PmsMovie, item: &Value)` → `parse_item(m: &mut PmsMovie, it: &plex::Metadata)`.
  Field reads become typed: `it.title`, `it.year`, `it.content_rating`, `it.duration*1_000_000`,
  `it.view_offset`, `it.grandparent_thumb`/`it.thumb`, `it.art`, `it.summary`, `it.rating_key`,
  `it.media.first()` → `.video_codec`/`.audio_codec`/`.part.first().key`.
- Blur: `it.ultra_blur_colors` (`Option<UltraBlurColors>` of `HexColor([f32;3])`) replaces the inline
  `hex3` + object read → `m.blur = [ub.top_left.0, ub.top_right.0, ub.bottom_right.0, ub.bottom_left.0]`.
  **This also fixes D-1** (PMS returns `UltraBlurColors` as an array; the current object read misses it).
- **Delete** `pms::jstr`, `pms::hex3`, and `pms::urlenc_str` (encoder lives in `plex::client::enc`).
  `urlenc_str` is currently also imported by `posters` and `route`; both lose their use after P/D/C
  migrations below, so the deletion is safe once all three files are migrated.

---

## metadata.rs (5 sites, all via the `get_json` helper)

The private helper `metadata::get_json(host,port,path)` (`metadata.rs:119-122`) and the
`(host,port,token)` threading from `route::config()` (used at `load_detail:323`, `load_season:357`)
are **deleted**; each fetch calls the singleton directly. Helpers `meta0`/`metas`/`first_part`/
`jstr`/`jint`/`tags` are dropped in favor of typed field access.

### M1 — `fetch_detail` — `metadata.rs:147`
Current: `get_json(host,port,&format!("/library/metadata/{rk}?X-Plex-Token={token}"))?` then `meta0(&json)?`.
Replacement:
```rust
let it = crate::plex::client().metadata(rk)?;   // Option<Metadata>, already the single item
let mut d = Detail {
    is_show: it.kind == "show", title: it.title.clone(), year: it.year,
    rating: it.content_rating.clone(), summary: it.summary.clone(), tagline: it.tagline.clone(),
    studio: it.studio.clone(), aired: it.originally_available_at.clone(),
    dur_ms: it.duration, resume_ms: it.view_offset, art: it.art.clone(), thumb: it.thumb.clone(),
    genres: it.genre.iter().map(|t| t.tag.clone()).collect(),
    countries: it.country.iter().map(|t| t.tag.clone()).collect(),
    directors: it.director.iter().map(|t| t.tag.clone()).collect(),
    writers: it.writer.iter().map(|t| t.tag.clone()).collect(),
    cast: it.role.iter().map(|r| Cast{ tag:r.tag.clone(), role:r.role.clone(), thumb:r.thumb.clone() }).collect(),
    …
};
parse_streams(&it, &mut d);
```
`metadata(rating_key: &str) -> Option<Metadata>` (returns `.metadata[0]` already, so `meta0` is gone).

### M2 — `fetch_item_streams` (show first-episode backfill) — `metadata.rs:226`
Current: `get_json(host,port,&format!("/library/metadata/{rk}?…"))` then `meta0` + `parse_streams`.
Replacement:
```rust
if let Some(it) = crate::plex::client().metadata(rk) { parse_streams(&it, d); }
```
Same `metadata()` method.

### M3 — `fetch_seasons` — `metadata.rs:234`
Current: `get_json(…&format!("/library/metadata/{rk}/children?…"))` then `metas()` filter `type=="season"`.
Replacement:
```rust
let mc = crate::plex::client().children(rk)?;   // or return Vec::new() on None
mc.metadata.iter().filter(|x| x.kind == "season").map(|x| Season {
    rk: x.rating_key.clone(), index: x.index, title: x.title.clone(), leaf_count: x.leaf_count,
}).collect()
```
`children(rating_key: &str) -> Option<MediaContainer>`; consume `.metadata`.

### M4 — `fetch_episodes` — `metadata.rs:251`
Current: `get_json(…&format!("/library/metadata/{season_rk}/children?…"))` then `metas()`.
Replacement:
```rust
let mc = crate::plex::client().children(season_rk)?;
mc.metadata.iter().map(|x| {
    let m0 = x.media.first();
    Episode {
        rk: x.rating_key.clone(), index: x.index, season: x.parent_index, title: x.title.clone(),
        summary: x.summary.clone(), aired: x.originally_available_at.clone(), dur_ms: x.duration,
        thumb: x.thumb.clone(), resume_ms: x.view_offset,
        part: m0.and_then(|m| m.part.first()).map(|p| p.key.clone()).unwrap_or_default(),
        rating: x.content_rating.clone(),
        vcodec: m0.map(|m| m.video_codec.clone()).unwrap_or_default(),
        acodec: m0.map(|m| m.audio_codec.clone()).unwrap_or_default(),
    }
}).collect()
```
Same `children()`. (Optional future optimization: replace the per-season loop with a single
`all_leaves(show_rk)` grouped by `parent_index` — method already exists but is unused; keeping
per-season `children` is the smallest diff.)

### M5 — `fetch_related` — `metadata.rs:279`
Current: `get_json(…&format!("/library/metadata/{rk}/related?…"))` then `.get("Hub").as_array()`.
Replacement:
```rust
let mc = crate::plex::client().related(rk)?;
for h in &mc.hub { for x in &h.metadata { … x.rating_key / x.title / x.thumb / x.year / (x.kind=="show") … } }
```
`related(rating_key: &str) -> Option<MediaContainer>`; consume `.hub[].metadata[]`. The 20-item cap
and dedup `HashSet` logic stay at the call site.

### metadata.rs supporting rewrite
- `parse_streams(item: &Value, d)` → `parse_streams(item: &plex::Metadata, d)`:
  `item.media.first().and_then(|m| m.part.first())` → `&part.stream`; each stream reads `s.id`,
  `s.language`, `s.codec`, `s.channels`, `s.audio_channel_layout`, `s.hearing_impaired!=0`,
  `s.audio_description!=0 || s.title.to_lowercase().contains("descri")`, `s.forced!=0`,
  `match s.stream_type { 2 => audio, 3 => subs, _ => {} }`.

---

## route.rs (7 sites)

`route` keeps its **playback state** (`CUR_RK`, `CUR_AUDIO_SID`, `CUR_SUB_SID`, `CUR_PART_ID`,
`TSESSION`, `URL`) — that is app state, not a Plex op. The client is stateless about the current
selection; `route` builds a `TranscodeSpec`/`StreamSelection` from that state per call.
**`TBASE` is eliminated** (`transcode_start_url` is rebuilt from a `TranscodeSpec` each time), and
**`transcode_base()` (`route.rs:155-178`) is deleted** — its query is now `Client::transcode_query`.

### R1 — `stop_transcode` — `route.rs:96-100`
Current:
```rust
let sp = format!("/video/:/transcode/universal/stop?session={sess}&X-Plex-Client-Identifier={sess}&X-Plex-Token={}", cfg.token);
let _ = crate::stream::http_get(&cfg.host, cfg.port, &sp, None);
```
Replacement: `crate::plex::client().transcode_stop(&sess);`
`transcode_stop(session: &str)` (get_void). Match: it sends `session=` + `X-Plex-Client-Identifier=session`.

### R2 — `build_stream` direct-play URL — `route.rs:210-212`
Current:
```rust
return (format!("http://{}:{}{}?X-Plex-Token={}", cfg.host, cfg.port, part, cfg.token), String::new());
```
Replacement:
```rust
return (crate::plex::client().direct_play_url(part).to_url(), String::new());
```
`direct_play_url(part_key: &str) -> StreamUrl`; `.to_url()` gives the stored `URL` string.

### R3 — `build_stream` transcode decision + start — `route.rs:220-222`
Current:
```rust
let base = transcode_base(rk, cfg);
unsafe { *addr_of_mut!(TBASE) = base.clone() };
put_selection(cfg);
let dpath = format!("/video/:/transcode/universal/decision?{base}");
let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{base}", cfg.host, cfg.port);
(url, session)
```
Replacement:
```rust
let spec = crate::plex::TranscodeSpec {
    rating_key: rk, audio_stream_id: cur_audio_sid(), subtitle_stream_id: cur_sub_sid(),
    offset_secs: 0, max_video_bitrate: 20000, video_resolution: "1920x1080",
};
put_selection(cfg);                          // R5, now select_streams
let c = crate::plex::client();
c.transcode_decision(&spec);
(c.transcode_start_url(&spec).to_url(), format!("plxnative-{rk}"))
```
`transcode_decision(&TranscodeSpec)` (get_void) + `transcode_start_url(&TranscodeSpec) -> StreamUrl`.

### R4 — `transcode_seek` decision + start — `route.rs:130-135`
Current:
```rust
let obase = format!("{base}&offset={}", offset_secs.max(0));
let dpath = format!("/video/:/transcode/universal/decision?{obase}");
let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{obase}", cfg.host, cfg.port);
set_url(&url);
```
Replacement:
```rust
if transcode_session().is_empty() { return None; }   // was: base.is_empty() (TBASE) guard
let rk = unsafe { (*addr_of!(CUR_RK)).clone() };
let spec = crate::plex::TranscodeSpec {
    rating_key: &rk, audio_stream_id: cur_audio_sid(), subtitle_stream_id: cur_sub_sid(),
    offset_secs: offset_secs.max(0), max_video_bitrate: 20000, video_resolution: "1920x1080",
};
let c = crate::plex::client();
c.transcode_decision(&spec);
let url = c.transcode_start_url(&spec).to_url();
set_url(&url);
```
The `TranscodeSpec.offset_secs` field carries the seek (`transcode_query` appends `&offset=` when >0).
The "is this a transcode?" guard moves from `TBASE.is_empty()` to `TSESSION.is_empty()`.

### R5 — `put_selection` server-side stream selection — `route.rs:192-197`
Current:
```rust
let mut p = format!("/library/parts/{part}?allParts=1&subtitleStreamID={sub}");
if aud > 0 { p.push_str(&format!("&audioStreamID={aud}")); }
p.push_str(&format!("&X-Plex-Token={}", cfg.token));
let st = crate::stream::http_put(&cfg.host, cfg.port, &p);
```
Replacement:
```rust
let sel = crate::plex::StreamSelection {
    part_id: part, audio_stream_id: aud, subtitle_stream_id: sub, all_parts: true,
};
let st = crate::plex::client().select_streams(&sel);
```
`select_streams(&StreamSelection) -> i32` (HTTP status, used in the log). Sends `subtitleStreamID`
always (0 = off) and `audioStreamID` only when `>0` — identical to today.

### R6 — `retranscode` decision + start — `route.rs:314-324`
Current: builds `transcode_base(&rk,cfg)` → sets TBASE/TSESSION → `put_selection` → `&offset=` →
decision GET → `start.mkv` URL. Replacement mirrors **R4** (same `TranscodeSpec` with the current
`offset_secs`), plus:
```rust
unsafe { *addr_of_mut!(TSESSION) = format!("plxnative-{rk}"); }
put_selection(cfg);      // R5
// then transcode_decision + transcode_start_url as in R4; drop the TBASE write
```
`switch_audio` (`route.rs:336`) is unchanged apart from calling the migrated `retranscode`.

*(That is 7 typed sites in route.rs: R1, R2, R3, R4, R5, R6, plus the deletion of `transcode_base`
whose query is subsumed by `Client::transcode_query`.)*

---

## posters.rs (1 typed site; 1 transport site changes source only)

### D1 — `poster_key` transcode path build — `posters.rs:99-102`
Current:
```rust
let enc = crate::pms::urlenc_str(&cstr(src_path));
let tok = store().token.clone();
let fmt = if png != 0 { "&format=png" } else { "" };
let s = format!("/photo/:/transcode?width={w}&height={h}&minSize=1&url={enc}{fmt}&X-Plex-Token={tok}");
```
Replacement:
```rust
let s = crate::plex::client().image_transcode_path(&cstr(src_path), w as i64, h as i64, png != 0);
```
`image_transcode_path(src, w, h, png) -> String` — the one method that returns a path; posters uses
it as both the LRU **key** and the fetch path (design rule 10). The `store().token` read is gone.

### D2 — `poster_worker` image fetch — `posters.rs:285` (STAYS raw `http_get`, source changes)
Current:
```rust
let px = match stream::http_get(&host, port, &key_s, None) { … };   // host/port from Store
```
Replacement:
```rust
let c = crate::plex::client();
let px = match stream::http_get(c.host(), c.port(), &key_s, None) { … };
```
By design the poster worker keeps its own `http_get` (fetch + `img_decode` belong to the worker).
Only the host/port *source* moves from `Store` to `client()`; `Store` loses its `host`/`port`/`token`
fields (and `posters_init` loses those params).

---

## player/threads.rs (1 typed site; 3 transport sites unchanged; 1 URL-parse)

### T1 — `timeline_thread` progress report — `threads.rs:249-253`
Current:
```rust
let state = if super::TX.paused.load(Ordering::Relaxed) { "paused" } else { "playing" };
let path = format!("/:/timeline?ratingKey={rk}&key=%2Flibrary%2Fmetadata%2F{rk}&state={state}\
    &time={t}&duration={d}&X-Plex-Client-Identifier={CID}&X-Plex-Token={token}");
let _ = crate::stream::http_get(&host, port, &path, None);
```
Replacement:
```rust
use crate::plex::{TimelineReport, TimelineState};
let state = if super::TX.paused.load(Ordering::Relaxed) { TimelineState::Paused } else { TimelineState::Playing };
crate::plex::client().timeline(&TimelineReport { rating_key: &rk, state, time_ms: t, duration_ms: d });
```
`timeline(&TimelineReport)` internally does `key=/library/metadata/{rk}` (enc'd), the
`X-Plex-Client-Identifier`, and the token. The thread no longer needs `host`/`port`/`token` params
(only `rk` at spawn). **Divergence: GET → POST** (D-8a) — see flags.

### T2 — `parse_stream_url` call (post-seek transcode re-point) — `threads.rs:120`
Current: `let (h, p, pa) = super::engine::parse_stream_url(&nu);`
Replacement:
```rust
let su = crate::plex::StreamUrl::parse(&nu);
host_c = std::ffi::CString::new(su.host).unwrap_or_default();
path_c = std::ffi::CString::new(su.path).unwrap_or_default();
port = su.port;
```
`StreamUrl::parse(&str) -> StreamUrl` (same default-32400 behavior as `engine::parse_stream_url`).

### T3 — demux/cue `http_open` — `threads.rs:90, 159, 184` (STAY raw)
These open the raw demux + cue sockets and add byte-`Range:` headers via `http_open`'s `extra`
(design rules 4-6: the Plex layer produces targets, the player owns the socket + Range). **Not
migrated** — they already receive host/port/path (from `StreamUrl` fields, once `engine` passes a
`StreamUrl` instead of a parsed `String`). Not a gap.

---

## Sibling: player/engine.rs (migrates WITH the above)

Not in the 5 requested files, but these share the same two methods and must move together:

- **engine.rs:101 `parse_stream_url` def + engine.rs:167 call** → `plex::StreamUrl::parse`. Delete
  the local `parse_stream_url`; `start_bufferfeed` uses `let su = StreamUrl::parse(&url);` and passes
  `su.host/su.port/su.path` (or a `StreamUrl`) to the demux/cue threads.
- **engine.rs:289-294 final `state=stopped` `/:/timeline`** → `client().timeline(&TimelineReport {
  rating_key: &rk, state: TimelineState::Stopped, time_ms: pos, duration_ms: dur })`. This is the
  **second** hand-built timeline string; both T1 and this collapse onto `timeline()`.
- Boot (engine/app) obtains `(host,port,token)` from `route::config()` for the reporter (`engine.rs:206`).
  After migration the reporter needs no config (uses the singleton); `route::config()` can be removed
  once `metadata` + `engine` stop calling it.

---

## Coverage check — does the client cover every raw call?

**Yes — every raw Plex request in the 5 files maps to an existing method.** No endpoint is missing.

| raw call | typed method | status |
|---|---|---|
| `/library/sections` | `sections()` | covered |
| `/library/sections/{k}/all` | `section_items()` | covered |
| `/hubs?count=` | `home_hubs()` | covered |
| `/library/metadata/{rk}` | `metadata()` | covered |
| `/library/metadata/{rk}/children` | `children()` | covered |
| `/library/metadata/{rk}/related` | `related()` | covered |
| `/photo/:/transcode` | `image_transcode_path()` | covered |
| `…/transcode/universal/decision` | `transcode_decision()` | covered |
| `…/transcode/universal/start.mkv` | `transcode_start_url()` | covered |
| `http://…{part}?token` (direct play) | `direct_play_url()` | covered |
| `…/transcode/universal/stop` | `transcode_stop()` | covered |
| `PUT /library/parts/{id}` | `select_streams()` | covered |
| `/:/timeline` (thread + final) | `timeline()` | covered |
| URL split (`parse_stream_url`) | `StreamUrl::parse()` | covered |
| demux/cue `http_open` + poster `http_get` | (stays `stream::*` transport) | by design |

**Methods that exist but have NO current call site** (extra capacity, not required by this
migration): `section_items_paged`, `metadata_many`, `all_leaves`, `continue_watching`, `promoted`,
`search`, `transcode_subtitles_url`. Leave `#![allow(dead_code)]` until a UI feature adopts them.

## Divergences to verify on-device (behavioral, not gaps)

1. **Timeline GET → POST** (T1 + engine final). `Client::timeline` uses `Client::post` (spec-correct,
   D-8a). The current code uses GET and works. Verify PMS accepts the POST (watch
   `/tmp/plxnative-events.log` for the resume point + watched-state update). If it regresses, the design
   notes the one-line fallback: swap `post` → `get_void` inside `timeline()`; signature unchanged.

2. **Transcode `X-Plex-Client-Identifier`.** Today `route::transcode_base` sends
   `X-Plex-Client-Identifier={session}` (= `plxnative-{rk}`) on decision/start (route.rs:173).
   `Client::transcode_query` sends `X-Plex-Client-Identifier=self.client_id` (`com.beb.plxnative`)
   while `session`/`X-Plex-Session-Identifier` stay `plxnative-{rk}`. Confirm the transcode still
   registers and streams after R3/R4/R6 (this is the value the design deliberately changed; if PMS
   ties the session to the client-id, revert `transcode_query` to use `session` there).

3. **`home_hubs` drops `excludeContinueWatching=0`** (P3). It was a no-op default; confirm the home
   shelves (incl. Continue Watching) are unchanged.

4. **UltraBlurColors now parsed** (P1 blur). The old object read missed the array shape (D-1), so
   ambient gradients may *appear where they were previously blank* — this is the intended fix, but
   eyeball a card that has `UltraBlurColors` to confirm the corners map L/R correctly.

5. **`metadata()` returns `.metadata[0]`** — a response with an empty `Metadata` array yields `None`
   (today `meta0` also returned `None`). Equivalent, but M1/M2 must keep the `?`/early-return.
</content>
</invoke>
