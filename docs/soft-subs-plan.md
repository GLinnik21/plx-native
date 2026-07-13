# Soft WebVTT subtitles during transcode (never burn)

**Goal.** When the user selects a subtitle track *while the item is transcoding*, render it
**client-side as soft text** — the same GLES text overlay direct-play already uses — instead
of asking Plex to **burn** it into the H264 video plane. We do this by opening a **second HTTP
connection on the same transcode session** to Plex's soft-subtitle endpoint, which returns
WebVTT streamed in lock-step with the video transcode, parsing it incrementally, and pushing
the cues into the **existing `SHARED.sub_cues` store** so `ui::player_hud::draw_subtitles` is
reused unchanged.

**Status.** This is a design + a landed, tested parser (`rust-modules/src/webvtt.rs`). Nothing
here is wired into the app yet, and burn is **not** removed — §5 is the diff sketch to apply
later. Keep burn as an explicit fallback (§6).

**Verified endpoint.**

```
GET /video/:/transcode/universal/subtitles?<same universal-transcode params as start.mkv>
        &subtitleStreamID={id}&subtitles=auto
→ HTTP 200, Content-Type: text/vtt
```

The body is a WebVTT document, **streamed progressively in sync with the active video
transcode session** (bytes flow only while the video transcode is being consumed). Because it
carries the *same* `session=plxnative-{rk}` id and the *same* `offset=` as the concurrent
`start.mkv`, its cue timeline is identical to the fed video timeline (§4). `subtitles=auto`
(not `burn`) tells PMS to emit sidecar text, not to bake pixels.

---

## 1. Where it plugs into today's code

Two subtitle mechanisms exist today (see `route.rs` header comment and `track_menu::on_ok`):

| Mechanism | Selected by | Produced by | Rendered by |
|---|---|---|---|
| **Soft, client-rendered** (direct-play only) | `desired_sub_idx` (index, `-1`=off) via `player::request_subtitle` | MKV demuxer `active_sub_track` → `push_subtitle_cue` → `SHARED.sub_cues` | `player_hud::draw_subtitles` ← `active_subtitle(playpos_ns())` |
| **Burn, server-side** (transcode) | `route::CUR_SUB_SID` via `route::set_subtitle` | PMS bakes it into the H264 plane (`&subtitles=burn`) | (already pixels) |

This plan adds a **third producer for the *same* soft render sink**: a WebVTT stream thread
that fills `SHARED.sub_cues` during a transcode. The render side (`active_subtitle` +
`draw_subtitles`) and the on/off gate (`desired_sub_idx >= 0`) are **unchanged** — we only add
a new cue *source* that runs when the demuxer can't (the transcode's `start.mkv` carries no
text-sub track).

Key existing anchors (all verified):

- Store: `SubCue { start_ns, end_ns, text }` — `player/shared.rs:18`; the vec
  `SHARED.sub_cues: Mutex<Vec<SubCue>>` — `shared.rs:60`; reset in `reset_session()` —
  `shared.rs:124`.
- Producer sink + 24-cue ring: `player::push_subtitle_cue` — `player/mod.rs:64`.
- Selector: `player::active_subtitle(now_ns)` — `mod.rs:78` (returns `None` when
  `desired_sub_idx < 0`; else newest cue covering `now_ns`).
- Renderer: `player_hud::draw_subtitles` — `ui/player_hud.rs:86`; call site `app.rs:765`.
- Display clock: `sf_on_event_inner` sets `playpos_ns = fed_pts - pts_shift + disp_base` —
  `mod.rs:146-147`. `disp_base` — `shared.rs:37`.
- Thread + socket primitives: `stream_thread` / `cues_thread` — `player/threads.rs:60,148`;
  `SendPtr` — `threads.rs:14`; `http_open/http_read/http_close/http_stream_boxed` —
  `stream.rs:88,217,290,356`; close-to-interrupt handles `hs_ptr`/`hs2_ptr` — `shared.rs:75`.
- Transcode session/base/seek: `route::transcode_session/transcode_base/transcode_seek/
  retranscode/switch_audio` — `route.rs:56,155,114,308,336`.
- The dead-but-ready URL builder: `plex::transcode_subtitles_url` — `plex/transcoder.rs:71`.

---

## 2. The WebVTT subtitle-stream thread

Mirrors the cue-preflight second connection (`cues_thread`), but instead of parsing MKV Cues
into `SHARED.cues`, it parses a streamed WebVTT body into `SHARED.sub_cues`. It is the third
HTTP connection in a session (`hs`=demux, `hs2`=cue-preflight, **`hs3`=subs**).

### 2.1 New `SHARED` state (`player/shared.rs`)

Add, modeled 1:1 on `hs2_ptr` / `cues_abort` / `next_url` / `seek_byte`:

```rust
// --- soft WebVTT subtitle sidecar (transcode only) ---
// close-to-interrupt handle for the subs socket (like hs_ptr/hs2_ptr)
pub hs3_ptr: AtomicPtr<HttpStream>,
// teardown flag for the subs thread (like cues_abort)
pub subs_abort: AtomicBool,
// a seek/retranscode re-points the subs stream at a new subtitles?…&offset= URL
// (Some => the thread re-opens on it; taken on re-open, like next_url)
pub subs_next_url: Mutex<Option<String>>,
```

Init in `Shared::new()` (`hs3_ptr = null`, `subs_abort = false`, `subs_next_url = None`) and
reset in `reset_session()` (same three). No `subs_seek` integer is needed — a subtitle
re-open always uses a fresh URL (never a byte Range), so `subs_next_url.is_some()` *is* the
re-open signal (simpler than the demux path, which multiplexes byte-Range vs URL seeks).

The **desired** track is expressed as a Plex stream id the main thread owns:

```rust
// desired soft-sub sid for the CURRENT transcode; 0 = none/off. Set by track_menu,
// reconciled by the pump (spawn/re-point/stop the subs thread). (main-thread reads,
// track_menu writes — an AtomicI64 is enough; no lock.)
pub subs_want_sid: AtomicI64,
```

### 2.2 New `Engine` fields (`player/engine.rs`)

```rust
pub hs3: Box<HttpStream>,                       // subs socket (M owns; subs thread uses via raw ptr)
pub subs_th: Option<std::thread::JoinHandle<()>>,
pub subs_active_sid: i64,                        // sid the subs thread is CURRENTLY streaming (0 = none); main-thread-confined
```

`hs3` is allocated in `start_bufferfeed` exactly like `hs`/`hs2`
(`crate::stream::http_stream_boxed()` → `SHARED.hs3_ptr.store(&mut *hs3, Release)`), RAII —
held alive for the worker, freed only after join. The thread is **not** spawned at start
(subtitles are off on a fresh item — `play_movie` resets `CUR_SUB_SID = 0`); the pump spawns
it lazily when a sub is selected (§2.4).

### 2.3 The thread body (`player/threads.rs`)

Patterned on `stream_thread`'s re-open loop (`threads.rs:60-144`) but with a WebVTT parser in
place of the MKV demuxer, and `subs_next_url`-only re-open:

```rust
use crate::webvtt::VttParser;

/// soft-subtitle sidecar: open the /subtitles URL on the SAME transcode session, read
/// the WebVTT body incrementally, push cues into SHARED.sub_cues (rebased by disp_base).
/// Loops for seek/retranscode — the pump publishes subs_next_url + closes hs3 to
/// interrupt the blocked recv; we re-open on the new offset URL and flush stale cues.
pub(crate) fn subs_thread(host: String, port: c_int, path: String, hs3: SendPtr<HttpStream>) {
    let host_c = std::ffi::CString::new(host).unwrap_or_default();
    let mut path_c = std::ffi::CString::new(path).unwrap_or_default();
    let hs3_p = hs3.0;
    loop {
        if crate::stream::http_open(hs3_p, host_c.as_ptr(), port, path_c.as_ptr(),
                                    std::ptr::null(), "GET") != 0 {
            super::log(&format!("subs: http_open FAILED status={}", crate::stream::hs_status(hs3_p)));
        } else {
            let mut parser = VttParser::new();
            let mut buf = vec![0u8; 65536];
            loop {
                let r = crate::stream::http_read(hs3_p, buf.as_mut_ptr(), buf.len() as c_int);
                if r <= 0 { break; }                        // EOF, or unblocked by http_close on seek/teardown
                if SHARED.subs_abort.load(Ordering::Acquire) { break; }
                for cue in parser.push(&buf[..r as usize]) { push_vtt_cue(cue); }
            }
            for cue in parser.finish() { push_vtt_cue(cue); }
        }
        crate::stream::http_close(hs3_p);
        if SHARED.subs_abort.load(Ordering::Acquire) { break; }
        // re-open on the URL the pump published (seek / retranscode / track switch)
        if let Some(nu) = SHARED.subs_next_url.lock().unwrap().take() {
            let (_, _, pa) = super::engine::parse_stream_url(&nu);
            path_c = std::ffi::CString::new(pa).unwrap_or_default();
            continue;
        }
        break; // real EOF, no pending re-open
    }
    super::log("subs: thread ended");
}

/// push one parsed WebVTT cue into the shared store, rebased onto content time and
/// respecting the same 24-cue ring the demux path uses (see §4 for +disp_base).
fn push_vtt_cue(cue: crate::webvtt::VttCue) {
    if cue.text.trim().is_empty() { return; }
    let base = SHARED.disp_base.load(Ordering::Relaxed);   // §4 alignment
    super::push_subtitle_text(cue.start_ns + base, cue.end_ns + base, cue.text);
}
```

`host`/`port` never change across a re-open (same PMS), so only `path_c` is rebuilt — the new
`subtitles?…&offset=` path. (`parse_stream_url` already returns `(host, port, path)`; we keep
host/port fixed and take only the path, which matches how the URL is always the same server.)

Add a sibling sink `push_subtitle_text` in `player/mod.rs` so the WebVTT path inherits the
empty-drop + 24-cap + logging **without** re-running `sub_text` (the parser already produced
clean text). Refactor the existing producer to share it:

```rust
/// push a ready (already-clean) subtitle cue; keeps the last ~24 (ring buffer).
pub(crate) fn push_subtitle_text(start_ns: i64, end_ns: i64, text: String) {
    if text.is_empty() { return; }
    let mut cues = SHARED.sub_cues.lock().unwrap();
    if cues.len() >= 24 { cues.remove(0); }
    cues.push(SubCue { start_ns, end_ns, text });
}
pub(crate) fn push_subtitle_cue(start_ns: i64, end_ns: i64, payload: &[u8], is_ass: bool) {
    let text = sub_text(payload, is_ass);      // strip tags/override codes (demux path)
    if text.is_empty() { return; }
    log(&format!("sub cue [{}..{}ms] {:?}", start_ns / 1_000_000, end_ns / 1_000_000,
        text.chars().take(34).collect::<String>()));
    push_subtitle_text(start_ns, end_ns, text);
}
```

**24-cue ring is fine.** The endpoint throttles the VTT body to the transcode's consumption,
so cues arrive incrementally, slightly ahead of `playpos` — exactly the order the ring assumes
(same as the demux). No need to raise the cap. (If a future full-sidecar `/library/streams/{id}.vtt`
load is added instead, *that* would dump the whole file at once and must bypass/raise the cap —
not this path.)

### 2.4 Pump reconciliation (`player/pump.rs`) — spawn / re-point / stop

The pump already owns every mid-session pipeline transition on the main thread. Add one
reconcile step that drives the subs thread from `subs_want_sid` vs `eng.subs_active_sid`.
Call it once per `pump()` tick (after the seek/retranscode arms so a same-tick offset change
is already reflected in `SHARED.disp_base`/`TBASE`):

```rust
fn reconcile_soft_subs(eng: &mut Engine, now_secs: i64) {
    let is_transcode = !crate::route::transcode_session().is_empty();
    let want = if is_transcode { SHARED.subs_want_sid.load(Relaxed) } else { 0 };

    // OFF (sub=Off, or switched to direct-play): stop the thread, drop cues.
    if want == 0 {
        if eng.subs_th.is_some() {
            SHARED.subs_abort.store(true, Release);
            let p = SHARED.hs3_ptr.load(Acquire);
            if !p.is_null() { crate::stream::http_close(p); }   // interrupt the recv
            if let Some(t) = eng.subs_th.take() { let _ = t.join(); }
            SHARED.subs_abort.store(false, Release);
            SHARED.sub_cues.lock().unwrap().clear();
            eng.subs_active_sid = 0;
        }
        return;
    }

    // ON, not running yet: spawn on the current session at the current offset.
    if eng.subs_th.is_none() {
        if let Some(url) = crate::route::transcode_subtitles_url(want, now_secs) {
            let (h, p, pa) = crate::route::_split(&url); // = parse_stream_url
            let hs3_raw = &mut *eng.hs3 as *mut HttpStream;
            SHARED.hs3_ptr.store(hs3_raw, Release);
            SHARED.subs_abort.store(false, Release);
            let hs3p = super::threads::SendPtr(hs3_raw);
            eng.subs_th = Some(std::thread::spawn(move || super::threads::subs_thread(h, p, pa, hs3p)));
            eng.subs_active_sid = want;
        }
        return;
    }

    // ON, running, but the user switched to a DIFFERENT track: re-point (same as a seek).
    if eng.subs_active_sid != want {
        if let Some(url) = crate::route::transcode_subtitles_url(want, now_secs) {
            *SHARED.subs_next_url.lock().unwrap() = Some(url);
            SHARED.sub_cues.lock().unwrap().clear();            // drop the old track's cues
            let p = SHARED.hs3_ptr.load(Acquire);
            if !p.is_null() { crate::stream::http_close(p); }   // → thread re-opens on subs_next_url
            eng.subs_active_sid = want;
        }
    }
}
```

`now_secs` is the current content position in whole seconds
(`(SHARED.playpos_ns / 1e9).max(0)`), matching the `offset` granularity the video uses.

**Seek / retranscode / audio-switch re-open.** These arms already flush Starfish, drain the
AQ, re-point the video demux at a new `start.mkv?…&offset=`, set `disp_base`, and close `hs`.
Add the symmetric subs re-point *inside those same arms* when the subs thread is running:

```rust
// (inside the transcode-seek arm, after SHARED.disp_base.store(secs*1e9,…); and inside
//  the audio-switch / retranscode arm, after SHARED.disp_base.store(secs*1e9,…))
if eng.subs_th.is_some() && eng.subs_active_sid > 0 {
    if let Some(surl) = crate::route::transcode_subtitles_url(eng.subs_active_sid, secs) {
        *SHARED.subs_next_url.lock().unwrap() = Some(surl);
        SHARED.sub_cues.lock().unwrap().clear();               // post-seek cues are stale
        let p = SHARED.hs3_ptr.load(Acquire);
        if !p.is_null() { crate::stream::http_close(p); }      // unblock → re-open at new offset
    }
}
```

Order matters and already holds: `disp_base` is stored **before** we close `hs3`, so by the
time the subs thread re-opens and parses its first post-seek cue, `push_vtt_cue` reads the new
`disp_base` (§4). Because an audio switch keeps the same `session=plxnative-{rk}` id and the same
selected subtitle, the subs thread simply re-opens on the new-offset URL of the *same* session
— satisfying requirement (4) "the WebVTT thread must restart too on the same new session".

Note the existing `request_audio_switch` (`mod.rs:45`) and `request_transcode_refresh`
(`mod.rs:52`) both `sub_cues.lock().clear()`. That is still correct — the clear happens, then
the subs re-open re-seeds. (Once burn is gone, `request_transcode_refresh` is deleted entirely,
§5; `request_audio_switch` keeps its clear.)

### 2.5 Teardown (`player/engine.rs::stop_bufferfeed`)

Alongside the `hs`/`hs2` closes and the cue/stream/load/report joins:

```rust
SHARED.subs_abort.store(true, Release);            // with cues_abort/report_stop (step 1)
let p3 = SHARED.hs3_ptr.load(Acquire);
if !p3.is_null() { crate::stream::http_close(p3); } // with the hs/hs2 closes (step 1)
…
if let Some(t) = eng.subs_th.take() { let _ = t.join(); }   // in the join block (step 2)
```

`reset_session()` then clears `hs3_ptr` / `subs_abort` / `subs_next_url` / `subs_want_sid`
(and already clears `sub_cues` + `desired_sub_idx`). `hs3` (the box) drops with the Engine
after the join, like `hs`/`hs2`.

### 2.6 The route helper (`route.rs`)

Add a burn-free subtitles-URL builder. It reuses the offset-free `TBASE` (which, after the §5
burn removal, no longer contains any subtitle block) and appends the explicit soft-sub
selection + offset:

```rust
/// Build the soft-WebVTT sidecar URL for `sub_sid` at `offset_secs` on the CURRENT
/// transcode session. Same universal params as start.mkv (TBASE) + &subtitleStreamID=…
/// &subtitles=auto (NOT burn). None if not transcoding. The subs thread opens this.
pub(crate) fn transcode_subtitles_url(sub_sid: i64, offset_secs: i64) -> Option<String> {
    if transcode_session().is_empty() || sub_sid <= 0 { return None; }
    let base = unsafe { (*addr_of!(TBASE)).clone() };
    if base.is_empty() { return None; }
    let cfg = unsafe { (*addr_of!(CFG)).as_ref()? };
    let q = format!("{base}&subtitleStreamID={sub_sid}&subtitles=auto&offset={}", offset_secs.max(0));
    Some(format!("http://{}:{}/video/:/transcode/universal/subtitles?{q}", cfg.host, cfg.port))
}
```

The dead `plex::transcode_subtitles_url` (`plex/transcoder.rs:71`) is a ready reference, but its
`transcode_query` still appends `&subtitles=burn` when `subtitle_stream_id>0`
(`transcoder.rs:33-38`); that burn param **must not** leak into the subtitles request. The
helper above sidesteps it by building from the burn-free `TBASE` and setting `subtitles=auto`
explicitly. (`_split` in §2.4 is just `engine::parse_stream_url` re-exported.)

---

## 3. The parser (`rust-modules/src/webvtt.rs`) — landed + tested

Self-contained, pure-`std`, **streaming** (byte-chunk in, completed cues out), UTF-8-boundary
safe (buffers raw bytes, splits only on `\n`). Public surface:

```rust
pub struct VttCue { pub start_ns: i64, pub end_ns: i64, pub text: String }

pub struct VttParser { /* … */ }
impl VttParser {
    pub fn new() -> Self;
    /// feed a body chunk; returns cues COMPLETED by this chunk (document order)
    pub fn push(&mut self, data: &[u8]) -> Vec<VttCue>;
    /// flush a trailing cue not terminated by a blank line (call at EOF/close)
    pub fn finish(&mut self) -> Vec<VttCue>;
}

pub fn parse_timing(line: &str) -> Option<(i64, i64)>;   // "START --> END [settings]" → ns
pub fn parse_timestamp(s: &str) -> Option<i64>;          // HH:MM:SS.mmm | MM:SS.mmm → ns
pub fn clean_text(s: &str) -> String;                    // strip <tags>, decode entities
pub fn parse_all(input: &str) -> Vec<VttCue>;            // whole-doc convenience (used by tests)
```

Behavior: recognizes the `WEBVTT` header block, cue blocks (optional id line + timing line +
text lines, ended by a blank line), ignores `NOTE`/`STYLE`/`REGION` blocks and cue-setting
suffixes (`line:` / `position:` / `align:` …), strips inline tags (`<i>`, `<c.class>`,
`<v Name>`, `<00:00:01.000>` timestamps), decodes the common entities (`&amp; &lt; &gt; &nbsp;
&lrm; &rlm;`, `&amp;` last so `&amp;lt;` → literal `&lt;`), joins multi-line text with `\n`,
accepts `.` or `,` decimals, `HH:MM:SS` or `MM:SS`, CRLF or LF, and a leading UTF-8 BOM; drops
cues whose text is empty after cleaning; rejects out-of-range minutes/seconds and non-digit
fields.

**Times are stream-relative ns** (relative to the transcode `offset` = 0-point) — the caller
adds `disp_base` (§4). The full source + 18 `#[test]`s are in `rust-modules/src/webvtt.rs`.

### 3.1 Running the tests

`cargo test --lib webvtt` **does not link** in this crate: the staticlib test executable can't
resolve the SDL/GLES/Starfish `extern "C"` symbols the rest of the crate references (they only
exist on the TV via the stub `.so` SONAMEs). The parser has **zero** dependencies on the crate
(pure `std`), so validate it standalone:

```sh
rustc --test --edition 2021 rust-modules/src/webvtt.rs -o /tmp/webvtt_test && /tmp/webvtt_test
# → test result: ok. 18 passed; 0 failed
```

(Verified: 18/18 pass — timestamp variants, cue-setting suffixes, NOTE/STYLE/REGION + id lines
ignored, tag strip, entity decode ordering, multi-line join, empty-cue drop, CRLF, BOM,
malformed-timing skip, `finish()` trailing flush, and streaming equivalence — byte-by-byte,
arbitrary chunk sizes, and multi-byte UTF-8 split across chunk boundaries — all matching the
one-shot parse.)

To wire it into the crate later, add `mod webvtt;` to `rust-modules/src/lib.rs`. (Deliberately
omitted now so nothing is wired.)

---

## 4. Timeline alignment (the exact formula)

**On-screen `playpos_ns()` is true content-time-ns in every mode** — direct-play, initial
transcode, and after any transcode seek/retranscode — because the fed-PTS rebase cancels:
`playpos = fed_pts - pts_shift + disp_base = (demux_pts + pts_shift) - pts_shift + disp_base =
demux_pts + disp_base`, and the transcode is requested 0-based at content `T` so
`demux_pts = content − T` while `disp_base = T·1e9` ⇒ `playpos = content`. (`mod.rs:146-147`,
`engine.rs:388-396`, `route.rs:130-133`.)

The WebVTT sidecar is fetched with the **same** `session=` and **same** `offset=T` as the
concurrent `start.mkv`, so PMS rebases its cue times to **0 at T** exactly like the fed video
PTS. Therefore a parsed cue time `vtt_ns` relates to content time as `vtt_ns = content − T`,
identical to `demux_pts`. To land on the store's expected clock (content-time ns, the clock
`active_subtitle(playpos_ns())` compares against), add the same `disp_base` the video uses:

```
store_start_ns = vtt_start_ns + SHARED.disp_base      (disp_base = offset_secs · 1e9)
store_end_ns   = vtt_end_ns   + SHARED.disp_base
```

This is the `+ base` in `push_vtt_cue` (§2.3). Read `disp_base` at **push time** (Relaxed;
single writer = main thread). It is stable for a given subs-stream connection (set once per
seek), and the pump always stores the new `disp_base` **before** closing `hs3`, so the first
post-seek cue already sees the new value.

Covered cases:

| Mode | `offset T` | `disp_base` | `vtt_ns` | `store_ns` | vs `playpos` |
|---|---|---|---|---|---|
| **Initial transcode** | 0 | 0 | content | content | ✓ aligned |
| **After transcode seek → T** | T | T·1e9 | content−T | content | ✓ aligned |
| **Retranscode / audio-switch @ T** | T | T·1e9 | content−T | content | ✓ aligned |
| Direct-play (demuxer path, unchanged) | — | 0 | content | content | ✓ aligned |

**Why sync survives keyframe snap.** `offset` is quantized to whole seconds and PMS's
`fastSeek` may snap the transcode 0-point to a nearby keyframe, so `playpos` can drift from
*true* content time by up to a GOP after a transcode seek. But the subtitle stream is the
**same session**, snapped to the **same** 0-point, so its cues drift *identically* — subtitle-
to-video sync is exact regardless of the snap; only the absolute content-time label drifts, and
it drifts for both together. No client-side correction is possible or needed.

**Do not fetch an independent whole-file (offset-0) VTT.** The endpoint streams alongside the
throttled video session; requesting offset-0 subtitles while the video session sits at offset T
would not stream in step (and would misalign). Always mirror the video's `offset` and add
`disp_base` — this keeps the sidecar in lock-step with the video and makes the math cancel.

---

## 5. How this replaces burn (diff sketch — NOT applied yet)

The goal: a selected sub while transcoding starts the **soft WebVTT thread** (`subtitles=auto`)
instead of the burn path; direct-play stays demuxer-rendered; burn survives only as an explicit
fallback (§6).

**(a) `ui/track_menu.rs::on_ok`, Subtitles tab (`track_menu.rs:139-149`)** — replace the burn
trigger with the soft-subs request:

```diff
     } else {
         addr_of_mut!(ACTIVE_SUB).write(sel - 1);              // row 0 = Off = -1
-        // direct-play: client-render the selected track from the demuxer (instant)
-        crate::player::request_subtitle(active_sub());
-        // transcode: remember the Plex sub stream id to BURN into any transcode of this item;
-        // if we're transcoding right now, re-burn immediately at the current pos.
-        crate::route::set_subtitle(sub_stream_id());
-        if !crate::route::transcode_session().is_empty() {
-            crate::player::request_transcode_refresh();
-        }
+        // gate the soft renderer on/off (direct-play demuxer path unchanged; also the
+        // on/off flag active_subtitle() checks for the transcode WebVTT path).
+        crate::player::request_subtitle(active_sub());          // desired_sub_idx = idx (-1 = off)
+        // transcode: fetch soft WebVTT for this sid instead of burning it. The pump
+        // spawns/re-points/stops the subs thread from subs_want_sid (0 = off).
+        if !crate::route::transcode_session().is_empty() {
+            if BURN_FALLBACK {                                  // §6 explicit fallback
+                crate::route::set_subtitle(sub_stream_id());
+                crate::player::request_transcode_refresh();
+            } else {
+                crate::route::set_subtitle(0);                  // keep the video burn-free
+                crate::player::request_soft_subs(sub_stream_id()); // 0 = off
+            }
+        }
     }
```

with, in `player/mod.rs`:

```rust
/// desired soft-WebVTT subtitle stream id during a transcode (0 = off). The pump
/// reconciles the subs thread (spawn / re-point / stop) from this.
pub(crate) fn request_soft_subs(sid: i64) { SHARED.subs_want_sid.store(sid, Relaxed); }
```

**(b) `route.rs::transcode_base` (`route.rs:161-168`)** — drop the burn block from the video
transcode so the video plane is never baked (subtitles ride the sidecar instead):

```diff
-    let sub_p = match cur_sub_sid() {
-        0 => String::new(),
-        s => format!("&subtitleStreamID={s}&subtitleSize=100&subtitles=burn"),
-    };
+    // subtitles are rendered soft (WebVTT sidecar, transcode_subtitles_url) — never
+    // burned into the video. (Burn survives only behind BURN_FALLBACK; when that is on,
+    // re-add this block gated on it.)
+    let sub_p = String::new();
```

**(c) `route.rs::put_selection` (`route.rs:186-198`)** — keep forcing the server-side selection
to *no burned sub* (`subtitleStreamID=0`), so PMS never default-selects+burns one. The chosen
sub id goes only to `transcode_subtitles_url`, not the PUT:

```diff
-    let (aud, sub) = (cur_audio_sid(), cur_sub_sid());
-    let mut p = format!("/library/parts/{part}?allParts=1&subtitleStreamID={sub}");
+    let aud = cur_audio_sid();
+    // always subtitleStreamID=0: the video transcode carries no burned sub (the comment
+    // above still holds — only the PUT suppresses a server default-selected+burned sub).
+    let mut p = format!("/library/parts/{part}?allParts=1&subtitleStreamID=0");
```

**(d) Remove the re-transcode-on-sub-change path** — a soft-sub change must **not** flush the
video pipeline. Delete `request_transcode_refresh` (`mod.rs:50-53`) and the `refresh` arm of
the pump (`pump.rs:29` swap + the `refresh` half of the `asid >= 0 || refresh` arm at
`pump.rs:30-54`), reducing that arm to the audio-switch case only. (Keep it if `BURN_FALLBACK`
is retained as a compile path — then gate.) The audio-switch arm stays; it legitimately
re-transcodes video, and §2.4 re-opens the subs stream on the new offset.

**Why Plex will now emit soft, not burn.** The advertised client profile
(`X-Plex-Client-Profile-Extra`, `route.rs:156-158`) declares only a video transcode target and
no soft-sub capability, so PMS defaults an *attached-to-the-video* subtitle to burn. With (b)+(c)
the video request carries no subtitle at all, and the explicit `/subtitles?…&subtitles=auto`
call asks specifically for the sidecar text stream — the endpoint that returns `text/vtt`.

---

## 6. Burn as explicit fallback

Keep burn reachable behind a single switch so a failure of the sidecar (e.g. the second
connection races the video stream on some server build — see §7) is one flag away from the
known-good burn path:

- A compile const `const BURN_FALLBACK: bool = false;` gating branch (a) above (and, if you
  keep the code, the pump `refresh` arm + `transcode_base` sub block). Flip to `true` to
  restore burn.
- Or, matching this repo's dev-trigger convention (boot-time `/tmp/plxnative-*` files), read
  `/tmp/plxnative-burn-subs` once at start and store it in an `AtomicBool` the `on_ok` branch checks —
  so burn-vs-soft is togglable on-device without a rebuild during bring-up.

Either way the soft path is the default; burn is the escape hatch, not the norm.

---

## 7. Thread lifecycle summary + the one risk to validate

**Lifecycle** (requirement 5):

- **Spawn** when `transcoding && subs_want_sid > 0` and no subs thread is running (pump §2.4).
- **Re-point** (not restart) on a *track switch* (`subs_active_sid != want`) and on any *seek /
  retranscode / audio-switch* — publish `subs_next_url` + close `hs3`; the thread re-opens on
  the new-offset URL and the pump `clear()`s the stale `sub_cues`.
- **Stop** on *sub=Off* / *switch to direct-play* (`want == 0`) and on *teardown*
  (`stop_bufferfeed`) — set `subs_abort`, close `hs3` to interrupt the blocked `recv`, join.
- **Interrupt** the blocking `http_read` the same way the demux does: the main thread
  `http_close(hs3_ptr)`s the fd; the in-flight `recv` fails, `http_read` returns ≤0, the read
  loop exits, the thread checks `subs_abort`/`subs_next_url` and either re-opens or ends. The
  15s `SO_RCVTIMEO` on every socket (`stream.rs:130-134`) is the backstop.

**The load-bearing risk.** `engine.rs:183-184` deliberately **skips the cue preflight for a
transcode** — "a 2nd conn cuts the stream": a concurrent second connection raced/killed the
primary MKV transcode stream. The subtitles sidecar is a *different* kind of second connection —
same `session=` id, an endpoint Plex designed to stream **alongside** the video — so it should
be tolerated where an independent second full stream was not. **Validate this on-device before
relying on it:** select a sub mid-transcode and confirm (a) `/tmp/plxnative-events.log` shows
`subs:` cues flowing *and* the video `feed v#…` cadence continuing uninterrupted, and (b) the
video doesn't stall/rebuffer when the subs connection opens. If it does cut the video stream on
this server build, fall back to burn (§6) — the design keeps that one flag away.
