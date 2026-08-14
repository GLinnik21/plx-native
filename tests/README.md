# On-device regression harness

Headless regression tests for the native webOS Plex player. There is no host-side runtime
(the code only runs on the TV), so every test drives the **real app on the real TV** via the
`/tmp/plxnative-*` dev triggers and asserts on the on-device event log (`/tmp/plxnative-events.log`).

- `manifest.json` — the test matrix, and **installation-independent**: the triggers each case
  needs, the expected log signals, and the *shape* of the item it needs (`item`, a symbolic key
  like `movie_h264_ac3_1080p`) rather than any ratingKey.
- `manifest.local.json` — **gitignored, one per installation**, and required. Maps each `item`
  key to a ratingKey on *your* server, and carries your `pms` host, `tv` address, `test_user` and
  (optionally) the `shared_server` a two-source case needs.
  Copy it from `manifest.local.json.example` and fill it in; `run.py` merges it over the manifest
  at load and refuses to run without it, naming any key it cannot resolve.
- `run.py` — the runner (Python 3 stdlib only; macOS system `python3` is fine).
- `README.md` — this file.

The split is not just anonymisation. The symbolic key keeps "five cases deliberately share one
item" visible in the tracked file — which is the fact the per-case resume reset exists for — and
it makes a mis-set item a named setup error instead of a mystery failure.

## Security

The PMS **X-Plex-Token is secret and is never committed**. `run.py` reads it from the
gitignored `src/config.local.h` (`#define PMS_TOKEN "..."`) at runtime and never prints,
logs, or writes it — progress URLs are redacted to `<token>` in output. The TV ssh
credentials are already in the committed `Makefile`, so the runner shells out to `make` /
`sshpass` for device I/O (no new secret is introduced).

Every other token the harness uses is **derived from that one at run time and stored nowhere**: the
managed user's per-server token (below), and a second server's access token (further below). Both
are written to a `/tmp/plxnative-*` file on the TV and are cleared by the same glob wipe — before
every case, and again by `teardown()` on *every* exit path, including Ctrl-C and a crash.

## Test identity — runs as a managed user (no watch-history pollution)

By default the harness plays **as the Plex Home managed user in `manifest.local.json` →
`test_user`**, so test playback + timeline scrobbles land on *that* user's history and your real
account stays clean. It works without storing any new secret:

- `run.py` uses the owner token (from `config.local.h`) to look up that user's **per-server
  access token** from `GET https://plex.tv/api/servers/<machineId>/shared_servers` (keyed by
  `userID` — which is what `test_user.id` is). The managed user must already have the libraries
  shared with it.
- That token is used for the `/:/progress` resume seed **and** written to `/tmp/plxnative-token` on the
  TV. The binary carries **no** token, so this file is the only way an automated run gets PMS access
  at all (see `plex_run`); the **app itself** then plays and scrobbles as the managed user, not just
  the seed. The token value is never printed (redacted to `<…, redacted>`), and `plxnative-token` is
  cleared between cases like every other trigger.
- Pass **`--owner`** to run as the `config.local.h` owner token instead (history *will* be
  affected). If the overlay has no `test_user`, the runner falls back to owner with a warning.

## A second server (a friend's shared one)

`/tmp/plxnative-token` carries exactly **one** token, and a shared server is a **separate
authority**: its own `machineIdentifier`, its own per-(user,server) access token, and a 401 for
anybody else's. So a screen that shows two sources at once could only ever be checked by hand, one
capture at a time. `/tmp/plxnative-servers` is the second credential channel — **purely additive**:
the primary server is still `plxnative-token` against the compiled-in host, unchanged, and a run
that names one server behaves exactly as it always did.

**Configure it once**, in the gitignored `manifest.local.json` (the block is optional — delete it if
you have no such server):

```json
"shared_server": {
  "machine_id": "aaaabbbbccccddddeeeeffff0000111122223333",
  "name": "nas-home",
  "host": "10.0.0.9",
  "port": 32400
}
```

- `machine_id` (the server's `clientIdentifier`) is the match key; `name` alone also works,
  case-insensitively, but is not stable. **No token here** — `run.py` looks the server up in
  `GET https://plex.tv/api/v2/resources` with the owner token from `config.local.h` and takes the
  `accessToken` plex.tv returns for it, exactly like it does for `test_user`.
- `host`/`port` are **optional but usually right to set**. Without them the runner picks a
  connection from plex.tv and prints which — and for a *shared* server plex.tv's `local` flag means
  the **owner's** LAN, not yours (a real one here advertises `10.9.9.5:32400 local=true`, which
  the TV can never reach), so the public address is preferred instead, dotted quads before
  hostnames (the app's transport has no DNS). Anything but a LAN address prints a NOTE saying so.
- **Watch history:** there is no managed-user token for someone *else's* server, so a case that
  plays from it plays as **you** on your friend's server. `test_user` isolation does not extend
  there.

**Ask for it per case**, in `manifest.json` (installation-independent — it says *that* a second
server is needed, never *which*):

```json
{ "name": "shared_home_shelf", "needs_shared_server": true, ... }
```

- With `shared_server` configured, the runner resolves it **before touching the TV** and writes
  `/tmp/plxnative-servers` for those cases only — a JSON array of
  `{name, machine_id, host, port, token}` — beside `plxnative-token`. Value never on stdout; the
  printed line is `plxnative-servers: <nas-home @ 10.0.0.9:32400, token redacted>`.
- Without it, those cases are **SKIPPED**, with the reason, and appear as `[SKIP]` in the summary —
  an installation with no friend's server is a normal installation. Anything unresolvable *is* a
  loud exit that names it (server no longer shared, no `accessToken`, no address).
- `./tests/run.py --shared-server` injects it into **every** case/scene of one run — for bringing a
  second-source screen up by hand. It exits if the overlay has no `shared_server` block.
- `./tests/run.py --list` marks such entries `[+2nd server]` and says whether one is configured.
  It is offline: nothing is resolved, plex.tv is not called.

On the device the app parses the file in `dev::servers()` (`rust-modules/src/dev.rs`) and logs

```
servers: #0 name="nas-home" 10.0.0.9:32400 mid=a348a464.. creds=ok
servers: 1 extra server(s) injected, 1 usable
```

— never a token (`DevServer` has no `Debug`, and `describe()` prints everything but). That pair of
lines is the headless proof the credentials arrived, and is what a shared-server case can assert on
before any of its screen exists. `plxnative-servers` is deliberately **not** on `dev.rs`'s `DIAG`
exemption list: it names a host *and* the token to trust it with, so like `plxnative-token` it marks
the boot automated and skips the who's-watching picker — a run that landed on the picker would grade
the wrong screen.

## Prerequisites

- The same toolchain the main dev loop needs: the webOS NDK (`make setup-env`), `sshpass`,
  and (for `--build`) the Rust nightly + `rust-src` (see the repo `Makefile` / `CLAUDE.md`).
- `tests/manifest.local.json` present — `cp tests/manifest.local.json.example` and fill in every
  `<placeholder>` (PMS host/port, TV address, `test_user`, and a ratingKey per `item` key). Its
  `shared_server` block is optional; delete it unless a second server is shared with your account
  (see below).
- The TV powered on and reachable (`root@<tv>`, the overlay's `tv`).
- The PMS reachable at `http://<pms-host>:32400` (the overlay's `pms` block).
- `src/config.local.h` present with `PMS_TOKEN`.

## Running

```bash
# list every case and what it covers (offline; no TV needed)
./tests/run.py --list

# build (cargo + make + make deploy), then run the whole matrix
./tests/run.py --build

# run one case (or a family) by name substring; assumes the app is already deployed
./tests/run.py --filter marker
./tests/run.py --filter seek

# run everything against an already-deployed build
./tests/run.py

# point at a different TV (overrides the overlay's `tv`)
./tests/run.py --tv 10.0.0.50 --filter dp_h264

# run as the OWNER token instead of the overlay's test_user (history WILL be affected)
./tests/run.py --owner --filter dp_h264

# hand the app a SECOND server's credentials for every case of this run (see below)
./tests/run.py --shared-server --filter dp_h264
```

The runner prints per-assertion PASS/FAIL with the failing evidence line, then a final
summary table. **Exit code is nonzero if any selected case fails** (CI-friendly).
Add `--verbose` to print evidence for passing assertions too.

## FPS regression suite (`--fps`)

A separate mode that guards **UI framerate**, not playback correctness. The app logs a once/sec
`loop=<n> route=<login|profiles|account|itemmenu|library|detail|person|search|player|home>
[overlay=<info|chapters|menu|none>] fps=<n>` heartbeat; each
*scene* in the manifest's `fps_scenes` sets its `plxnative-*` triggers (profiler **off**), runs, and
asserts its gates. This is the automated form of the by-hand FPS hunting that found the hero /
cast+about / info-panel regressions.

> **The heartbeat carries two rates and they are not interchangeable.** `loop=` counts **loop
> iterations** — liveness only; a settled screen reports ~62 while swapping nothing. `fps=` counts
> **frames actually swapped** and is the only real frame rate. They were **renamed 2026-08-01 and
> the old name was reused**: a pre-rename log's `FPS=` is today's `loop=`, and its `pres=` is
> today's `fps=`. Both regexes match the new names only, so an old log fails loudly as "no samples"
> instead of grading a loop rate as a frame rate.

```bash
# UI tier only — every scene whose `tier` is "ui". No video, no PMS token needed.
./tests/run.py --fps

# add the player tier (info panel, track menu) — these decode video as the test_user, slower.
./tests/run.py --fps-player

# build first, or list the scenes:
./tests/run.py --build --fps-player
./tests/run.py --list          # scenes print as `fps:<name>`
```

- **Three assertions, and picking the wrong one is how a frozen animation ships.** Since the present
  gate (`ui::idle`) landed, a skipped frame is a 16 ms sleep, so `loop=` reads ~60 whether or not
  anything reached the panel:
  - `loop_floor` grades `loop=`. It proves the **app is alive**. It cannot see a stopped animation,
    and on a settled screen it grades nothing at all — `home-hero` carries an `_idle_gate_note`
    saying so. It is the only one left: this line said "three scenes" long after the other two
    (`home-grid`, `library-scroll`) were given oscillators and real `fps_floor`s, which is exactly
    the fix that note asks for. The two remaining `loop_floor`-only scenes are `info-panel` and
    `track-menu`, and they need no such note — the present gate **excludes the player route**
    (`ui/idle.rs:57`), so their `loop_floor` still grades a fill rate the way it always did.
  - `fps_floor` grades `fps=` on the **median** — "is this screen still animating, at rate".
    The median and not the 2nd-lowest, because a frame rate is now intermittent *by design*: on a
    scene that bounces rather than animates continuously, a 1 s window can land wholly inside the
    settled gap and read 0 with a perfectly healthy animation (measured: `home-detail-nav` min=0,
    median=15). A frozen animator reads ~1/s — the keepalive alone — so the two are far apart.
  - `fps_ceiling` grades `fps=` on the **2nd-highest** — "does this screen actually STOP".
    This is the only guard on over-reporting, which silently gives back the whole ~38-points-of-a-
    core saving while every floor in the suite still passes.
- **`drift`** (last-third minus first-third mean) is reported on every scene and asserted on none.
  `rate_stats` used to sort and discard sample ORDER, so a monotone 60→53 decay and a flat 53
  produced byte-identical output. 18–36 s is far too short to gate a thermal ramp on; this is a
  breadcrumb pointing at when a real soak is worth running.
- **`loop_floor`s have margin** (50 for the steady home scenes, 45 for the transition/player) so
  normal 55–60 jitter passes while a real regression drops well below. **This margin used to be
  justified here by "the panel GPU thermally throttles" — that is an unmeasured hypothesis, not a
  finding**, and it is the only place in the repo the claim appears. Nothing has ever measured a
  temperature on this device (no `thermal_zone`, no `cpufreq`, Mali runtime-PM reports
  `unsupported`), the observed slow scenes are exactly the two with the most full-screen passes,
  and 50 fps is also precisely the European panel refresh on this SKU. Discriminate with a soak
  (same scene cold vs hot vs recovered), never from one sample. See
  `docs/perf-view-buffers-and-thermal.md`.
- **False-negative guard:** a scene with <5 post-warmup samples for its route FAILs (it never reached
  that screen — app crash, or a `detail`/`play` rk that isn't in the home catalog), never a vacuous
  pass. `detail-transition`'s item (`movie_in_home_catalog`) must be an **in-home-catalog
  (recently-added / on-deck) movie**.
- **The Search pair only means something together.** `search-type` and `search-idle` are the same
  screen with and without its oscillator: the first asserts an `fps_floor` (the shelves still move
  under a travelling focus), the second an `fps_ceiling` (a settled result set stops presenting).
  Run one without the other and half the question goes unasked, which is how a screen that repaints
  forever passes a floor. Two things to know before reading a result:
  - **Their `fps_floor` is the one number in this file that is not a device measurement.** They were
    written while the search screen was still being built, so `search-type` carries a floor picked
    only to separate a frozen animator (~0.5/s, `ui::idle`'s keepalive) from a running one. Raise it
    to a real median the first time it runs green on a television — the scene's own
    `_fps_floor_note` says so, and the neighbours all quote a date.
  - **`plxnative-search`'s value is a literal query, not a symbolic key.** `run.py` resolves `item`
    keys against your overlay; it has no notion of a query, so the manifest carries the text. If
    your library matches nothing for it there are no shelves, and `search-type` degrades to grading
    the tab strip. Change the literal, never the floor.
- Validated by injection: reverting the glyph-cache fix (`TCACHE 160→48`) makes `detail-transition`
  fail (~34fps) while the unaffected home scenes still pass.
- Same nonzero-exit-on-failure contract as the case suite. Tune floors / scenes in
  `manifest.json → fps_scenes` and point their `item` keys at your own library from
  `manifest.local.json`; the harness stays library-agnostic.

### What each case does (per case, automatically)

1. `make kill` — close the app (luna-send `closeByAppId` + `fuser -k`) **first**.
2. If the case sets `viewOffset_ms`: `PUT /:/progress` to seed the resume point — done
   **after** the close so the app's `timeline_thread` can't re-scrobble over it.
3. Clear every `/tmp/plxnative-*` trigger, then write only the ones this case needs.
4. `make run TV=<tv> RUN_SECS=<n>` — relaunch, wait, and cat `/tmp/plxnative-events.log` back.
5. Filter the `smp_cb type=43 num=0 str=` flood and evaluate the assertions.

## The `/tmp/plxnative-play=<rk>` trigger (added for this harness)

Tests use `/tmp/plxnative-play=<ratingKey>` instead of the fragile `/tmp/plxnative-detail`. `plxnative-detail`
only *plays* if the rk is in the home catalog (Continue Watching / hubs); off-catalog it loads
data-only and never plays. `plxnative-play` fetches the item's metadata fresh (`metadata::load_detail`,
works for **any** rk) and drives the same field-based play path the detail Play button uses
(`route::play_episode` — generic over movie/episode — + `player::resume_at` + `start_bufferfeed`),
bypassing the catalog lookup entirely. It honors the server `viewOffset` for resume and logs
`plxnative-play: rk=<rk> start` so the harness can confirm the trigger fired.

## Coverage — the 8 matrix item shapes + operations

The `item` column is the symbolic key `manifest.local.json` maps to a ratingKey on your server;
it is also the specification of what that item has to be.

Base playback (decision + codec + not-stuck), one case each:

| Case | item | Covers |
|------|------|--------|
| `dp_h264_ac3_1080p` | `movie_h264_ac3_1080p` | H264 + AC3 direct-play, 1080p, embedded SRT |
| `dp_hevc_eac3_4k_hdr10` | `episode_hevc_4k_hdr10_eac3` | HEVC 4K **HDR10** + E-AC3 direct-play, TV episode |
| `dp_hevc_truehd_ac3_sibling` | `movie_hevc_4k_hdr10_truehd` | **smart direct-play** (TrueHD default → AC3 sibling), HEVC 4K |
| `dp_hevc_eac3_dovi_p8` | `movie_hevc_4k_dovi_p8` | HEVC 4K **Dolby Vision P8** + E-AC3 direct-play |
| `dp_mp4_container` | `movie_hevc_aac_mp4` | HEVC + AAC, **mp4 container** direct-play (mov demuxer over HTTP, AAC→ADTS), sidecar subs |
| `dp_h264_aac_episode` | `episode_h264_aac` | H264 + AAC direct-play, TV episode, no subs |
| `dp_h264_ac3_many_audio` | `movie_h264_ac3_many_audio` | H264 + AC3 direct-play, 8 audio tracks (DTS/vorbis present) |
| `transcode_av1_no_dp_audio` | `movie_av1_no_dp_audio` | **must-transcode** (AV1 + no DP audio) → **HEVC 4K HDR10**/AC3 on this Plex-Pass server (the target chain ends in h264 since issue #22, so a server that cannot encode HEVC re-encodes to h264 instead of dropping video) |

Operation cases (each also re-checks not-stuck / no-error afterward):

| Case | item | Asserts |
|------|------|---------|
| `seek_inplace_h264` | `movie_h264_ac3_1080p` | in-place seek to 140s (`seek(in-place)` + `sendSegment=1`, **no** `reload_at`), timeline reaches ~140s |
| `seek_rapid_h264` | `movie_h264_ac3_1080p` | rapid tap-burst seek (6 requests @300ms, fwd+back — exercises coalescing): ≥2 in-place seeks, **no** `reload_at`, post-burst timeline reaches ~130s **and keeps climbing**, audio lane resumes (`feed a#` after the last seek) |
| `seek_rapid_hevc_4k` | `episode_hevc_4k_hdr10_eac3` | rapid 10s-**back**-tap burst on 4K HEVC HDR10 (the historical stale-audio-silence shape): same assertions, final ~160s |
| `seek_transcode` | `movie_av1_no_dp_audio` | transcode seek (`seek(transcode)` **or** `reload_at: fresh Load at 140s`), timeline reaches ~140s |
| `resume_directplay` | `movie_h264_ac3_1080p` | viewOffset 600s honored — first `timeline` near 600s, not 0 |
| `resume_transcode` | `movie_av1_no_dp_audio` | `resume(transcode): restart at offset 600s`, first timeline near 600s |
| `audio_switch_native` | `episode_hevc_4k_hdr10_eac3` | native audio switch (eac3→eac3) — `audio switch (native)`, codec **stays 174** |
| `audio_switch_transcode` | `movie_h264_ac3_many_audio` | English (DTS) audio → transcode — `re-transcode` + `reload_transcode`, codec 174 (HEVC target; the video is re-encoded H264→HEVC — an audio-only/video-copy transcode is a future improvement) |
| `subtitle_text_srt` | `movie_h264_ac3_1080p` | embedded subtitle soft-render on the **default `ff.rs` demuxer** — `sub cue [..] "text"` lines |
| `subtitle_image_pgs` | `movie_hevc_4k_pgs_subs` | **PGS image subtitle** client-render on HEVC 4K direct-play — `ff.rs` software-decodes the bitmap and logs `image cue [..] WxH at X,Y rects=N canvas=WxH` (op flagged `"image": true`) |

### Key log signals asserted (filter `smp_cb type=43 num=0 str=$` first)

- **decision:** `decision: part=<d> ... -> DIRECT PLAY | TRANSCODE`
- **codec/res:** `ff: v=#0 codec_id=<N> <W>x<H>` — 28 = H264, 174 = HEVC. Transcode is always
  28 at 1920x1080; direct-play HEVC is 174 at native (≥3000 px wide for 4K — some 4K is
  3840×1920, so the harness asserts a **width floor**, not an exact size).
- **video plane bound:** `setMediaVideoData sent`
- **not stuck:** ≥2 `timeline playing t=<S>s/` reports whose `<S>` climbs; and **no**
  `smp_cb type=18` / **no** `Playing error`.
- **seek:** `seek(in-place)` / `in-place seek: ... sendSegment=1` / `seek(transcode)` /
  `reload_at: fresh Load at 140s`.
- **audio switch:** `audio switch (native)` / `re-transcode:` + `reload_transcode:`.
- **subtitles (text):** `sub cue [<a>..<b>ms] "<text>"`.
- **subtitles (image PGS/VobSub):** `image cue [<t>ms] <W>x<H> at <x>,<y> rects=<N>
  canvas=<W>x<H>` (a decoded display-set pushed to the render store — `rects` is how many bitmaps
  it carries, `canvas` the stream's authoring canvas the renderer scales them from, `0x0` when the
  decoder declares none). The assertion matches the prefix, so the tail can grow.

## Gotchas the harness handles for you

- **Close-before-progress.** A live `timeline_thread` re-scrobbles every ~10 s and would
  overwrite a seeded `viewOffset`; the runner does `make kill` before every `PUT /:/progress`.
- **`make` runs from the repo root** (via `make -C <root>`), and `--build` shells out to it
  (the Makefile owns the cargo invocation), so cwd/toolchain flags can't drift.
- **Type=43 flood** is filtered on every log read.
- **RUN_SECS** per case clears the trigger arm time + reporter cadence (not-stuck ≥ ~25 s,
  seek ≥ ~45 s); the manifest uses 60–90 s.
- **Unicode arrows** in some log lines (`setMediaVideoData sent → …`, `→ reload`) are matched
  on their stable ASCII prefix.

## Subtitle soft-render

The **demuxer (`ff.rs`) demuxes embedded text subtitles** (SRT/subrip,
ASS/SSA, mov_text) and emits `sub cue [..] "text"` lines. It pushes cues for **all** text tracks (tagged by
index) and the renderer filters by the selected `desired_sub_idx`, so a mid-play track switch is
instant (no ~10-20s buffer-gap wait). Image subs (PGS/VobSub/DVB) are now client-rendered too:
`ff.rs` software-decodes the selected bitmap track (`avcodec_decode_subtitle2`), converts each
display-set to RGBA, and `player_hud::draw_subtitle_bitmap` composites it over the video as a GL
texture (the webOS pipeline's own HW subtitle engine is only reachable in URI/demuxer mode, not
our in-process buffer-feed — see project memory). Verified on a 4K HEVC movie carrying 6 PGS
tracks (`movie_hevc_4k_pgs_subs`).

The text case targets `movie_h264_ac3_1080p`, which has four text tracks
`[RU-forced, RU, EN, EN-SDH]`, so it picks **row 3 = the English track** (row 0 is Off;
`desired_sub_idx = row − 1`) and seeds a `viewOffset` of 843 s so playback lands in the dense
opening monologue and cues appear within the run window. (For a transcode item, soft subs ride a
WebVTT sidecar, which per project memory delivers 0 bytes on this pipeline — direct-play is the
only reliable sub path.)

## Adding a case

Append an entry to `manifest.json` → `cases`:

```json
{
  "name": "my_case",
  "item": "movie_h264_ac3_1080p",     // symbolic; map it in manifest.local.json → items
  "kind": "movie",
  "title": "movie · h264/ac3 · 1080p — …",   // the item SHAPE, not a library title
  "covers": ["…"],
  "run_secs": 60,
  "setup": { "viewOffset_ms": 600000 },        // optional — seeds resume
  "needs_shared_server": true,                 // optional — also inject a SECOND server's
                                               // credentials (see "A second server" above);
                                               // SKIPPED where none is configured
  "operations": [
    { "op": "play" },
    { "op": "seek", "mode": "inplace", "target_s": 140 }
    // or: {"op":"audio_switch","tab":0,"row":1,"mode":"native"|"transcode"}
    // or: {"op":"subtitle","tab":1,"row":1}
    // or: {"op":"resume","mode":"directplay"|"transcode","offset_s":600}
  ],
  "expect": {
    "decision": "directplay",       // or "transcode"
    "codec_id": 28,                 // 28 = H264, 174 = HEVC
    "min_video_width": 1900,        // resolution floor (4K → 3000)
    "min_timeline_climb_s": 15,
    "no_playing_error": true,
    "require_video_bound": true
  }
}
```

`run.py` derives the triggers from `operations` (`play`→`plxnative-play`, `seek`→`plxnative-autoseek`
— for `"mode":"rapid"` the op's `script` becomes the trigger content: optional `gap=<ms>` +
comma-separated steps, absolute `120` or tap-relative `+10`/`-10`, fired one per gap;
`audio_switch`/`subtitle`→`plxnative-menupick`) and picks the
per-op assertions from the `op`/`mode`. Track-menu row semantics: **audio tab** row = the
metadata audio index (0-based, file order); **subtitles tab** row 0 = *Off*, row *r* = subtitle
index *r−1*.

## Library gaps — combos NO real item can cover

The library survey found these have no exercising item; the harness can't test them with real
media (see `library_gaps` in `manifest.json`). A small set of **synthetic** `ffmpeg` clips
(10–30 s), served from the host (`python3 -m http.server`) and fed via the `/tmp/plxnative-url` boot
trigger, would close them deterministically — recommended as an optional supplement, kept clearly
labeled "synthetic" and secondary to the 8 authoritative real item shapes:

- **Video:** VP9, MPEG-2, VC-1, MPEG-4-ASP; **8-bit HEVC** (every HEVC is Main10); **4K H.264**
  (all 4K is HEVC/AV1); interlaced. (These would exercise the transcode-fallback path from the
  client side.)
- **Audio:** FLAC, PCM/LPCM, MP3; a DTS-only file to force an audio-only transcode without
  depending on the many-audio movie's track ordering.
- **Subtitles:** ASS/SSA, VobSub/dvd_subtitle; mov_text/tx3g soft-render.
- **HDR:** HLG, HDR10+, Dolby Vision P5 (IPT, no HDR10 fallback — the only item carrying it is
  mp4, which the container gate sends to the server anyway).
