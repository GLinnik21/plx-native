# On-device regression harness

Headless regression tests for the native webOS Plex player. There is no host-side runtime
(the code only runs on the TV), so every test drives the **real app on the real TV** via the
`/tmp/plxnative-*` dev triggers and asserts on the on-device event log (`/tmp/plxnative-events.log`).

- `manifest.json` — the test matrix: real PMS items (rk), the triggers each case needs, and
  the expected log signals.
- `run.py` — the runner (Python 3 stdlib only; macOS system `python3` is fine).
- `README.md` — this file.

## Security

The PMS **X-Plex-Token is secret and is never committed**. `run.py` reads it from the
gitignored `src/config.local.h` (`#define PMS_TOKEN "..."`) at runtime and never prints,
logs, or writes it — progress URLs are redacted to `<token>` in output. The TV ssh
credentials are already in the committed `Makefile`, so the runner shells out to `make` /
`sshpass` for device I/O (no new secret is introduced).

## Test identity — runs as Guest (no watch-history pollution)

By default the harness plays **as the Plex Home managed user in `manifest.json` → `test_user`**
(Guest, id 762808621), so test playback + timeline scrobbles land on *that* user's history and
your real account stays clean. It works without storing any new secret:

- `run.py` uses the owner token (from `config.local.h`) to look up that user's **per-server
  access token** from `GET https://plex.tv/api/servers/<machineId>/shared_servers` (keyed by
  `userID`). The managed user must already have the libraries shared with it (Guest here does).
- That token is used for the `/:/progress` resume seed **and** written to `/tmp/plxnative-token` on the
  TV. The app reads `/tmp/plxnative-token` at boot and overrides its compiled-in owner token with it
  (see `plex_run`), so the **app itself** plays and scrobbles as the managed user — not just the
  seed. The token value is never printed (redacted to `<Guest …>`), and `plxnative-token` is cleared
  between cases like every other trigger.
- Pass **`--owner`** to run as the `config.local.h` owner token instead (history *will* be
  affected). If the manifest has no `test_user`, the runner falls back to owner with a warning.

## Prerequisites

- The same toolchain the main dev loop needs: the webOS NDK (`make setup-env`), `sshpass`,
  and (for `--build`) the Rust nightly + `rust-src` (see the repo `Makefile` / `CLAUDE.md`).
- The TV powered on and reachable (`root@192.168.0.114`, default from the manifest).
- The PMS reachable at `http://192.168.0.3:32400` (host/port in the manifest `pms` block).
- `src/config.local.h` present with `PMS_TOKEN`.

## Running

```bash
# list every case and what it covers (offline; no TV needed)
./tests/run.py --list

# build (cargo + make + make deploy), then run the whole matrix
./tests/run.py --build

# run one case (or a family) by name substring; assumes the app is already deployed
./tests/run.py --filter morning
./tests/run.py --filter seek

# run everything against an already-deployed build
./tests/run.py

# point at a different TV
./tests/run.py --tv 192.168.0.50 --filter substance

# run as the OWNER token instead of the Guest test_user (history WILL be affected)
./tests/run.py --owner --filter substance
```

The runner prints per-assertion PASS/FAIL with the failing evidence line, then a final
summary table. **Exit code is nonzero if any selected case fails** (CI-friendly).
Add `--verbose` to print evidence for passing assertions too.

## FPS regression suite (`--fps`)

A separate mode that guards **UI framerate**, not playback correctness. The app logs a once/sec
`FPS=<n> route=<home|detail|player> [overlay=<info|chapters|menu|none>]` heartbeat; each *scene* in
the manifest's `fps_scenes` sets its `plxnative-*` triggers (profiler **off**), runs, and asserts the
steady framerate for that screen stays above a floor. This is the automated form of the by-hand FPS
hunting that found the hero / cast+about / info-panel regressions.

```bash
# UI tier only — home hero, home grid, detail scroll transition. No video, no PMS token needed.
./tests/run.py --fps

# add the player tier (info panel, track menu) — these decode video as the Guest test_user, slower.
./tests/run.py --fps-player

# build first, or list the scenes:
./tests/run.py --build --fps-player
./tests/run.py --list          # scenes print as `fps:<name>`
```

- **Three assertions, and picking the wrong one is how a frozen animation ships.** Since the present
  gate (`ui::idle`) landed, `FPS=` counts **loop iterations**, not swaps — a skipped frame is a 16 ms
  sleep, so the loop reads ~60 whether or not anything reached the panel:
  - `floor` grades `FPS=`. It proves the **app is alive**. It cannot see a stopped animation, and on
    a settled screen it grades nothing at all (three scenes carry an `_idle_gate_note` saying so).
  - `present_floor` grades `pres=` on the **median** — "is this screen still animating, at rate".
    The median and not the 2nd-lowest, because a present rate is now intermittent *by design*: on a
    scene that bounces rather than animates continuously, a 1 s window can land wholly inside the
    settled gap and read 0 with a perfectly healthy animation (measured: `home-detail-nav` min=0,
    median=15). A frozen animator reads ~1/s — the keepalive alone — so the two are far apart.
  - `present_ceiling` grades `pres=` on the **2nd-highest** — "does this screen actually STOP".
    This is the only guard on over-reporting, which silently gives back the whole ~38-points-of-a-
    core saving while every floor in the suite still passes.
- **`drift`** (last-third minus first-third mean) is reported on every scene and asserted on none.
  `fps_stats` used to sort and discard sample ORDER, so a monotone 60→53 decay and a flat 53
  produced byte-identical output. 18–36 s is far too short to gate a thermal ramp on; this is a
  breadcrumb pointing at when a real soak is worth running.
- **Floors have margin** (50 for the steady home scenes, 45 for the transition/player scenes) so
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
  pass. `detail-transition`'s `rk` must be an **in-home-catalog (recently-added / on-deck) movie**.
- Validated by injection: reverting the glyph-cache fix (`TCACHE 160→48`) makes `detail-transition`
  fail (~34fps) while the unaffected home scenes still pass.
- Same nonzero-exit-on-failure contract as the case suite. Tune floors / rks / scenes in
  `manifest.json → fps_scenes`; the harness stays library-agnostic.

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

## Coverage — the 8 real matrix items + operations

Base playback (decision + codec + not-stuck), one case each:

| Case | rk | Covers |
|------|----|--------|
| `substance_h264_ac3` | 4 | H264 + AC3 direct-play, 1080p, embedded SRT |
| `morning_show_hdr10_eac3` | 1804 | HEVC 4K **HDR10** + E-AC3 direct-play, TV episode |
| `toy_story3_smart_dp` | 1926 | **smart direct-play** (TrueHD default → AC3 sibling), HEVC 4K |
| `project_hail_mary_dovi` | 1900 | HEVC 4K **Dolby Vision P8** + E-AC3 direct-play |
| `hannah_montana_mp4_aac` | 1816 | HEVC + AAC, **mp4 container**, sidecar subs |
| `phineas_h264_aac` | 72 | H264 + AAC direct-play, TV episode, no subs |
| `home_alone_manyaudio` | 3 | H264 + AC3 direct-play, 8 audio tracks (DTS/vorbis present) |
| `toy_story4_av1_transcode` | 1945 | **must-transcode** (AV1 + no DP audio) → **HEVC 4K HDR10**/AC3 (needs server pref `TranscoderHEVCEncodingMode=always`; else video drops to audio-only) |

Operation cases (each also re-checks not-stuck / no-error afterward):

| Case | rk | Asserts |
|------|----|---------|
| `substance_seek_inplace` | 4 | in-place seek to 140s (`seek(in-place)` + `sendSegment=1`, **no** `reload_at`), timeline reaches ~140s |
| `substance_seek_rapid` | 4 | rapid tap-burst seek (6 requests @300ms, fwd+back — exercises coalescing): ≥2 in-place seeks, **no** `reload_at`, post-burst timeline reaches ~130s **and keeps climbing**, audio lane resumes (`feed a#` after the last seek) |
| `morning_show_seek_rapid` | 1804 | rapid 10s-**back**-tap burst on 4K HEVC HDR10 (the historical stale-audio-silence shape): same assertions, final ~160s |
| `toy_story4_seek_transcode` | 1945 | transcode seek (`seek(transcode)` **or** `reload_at: fresh Load at 140s`), timeline reaches ~140s |
| `substance_resume_directplay` | 4 | viewOffset 600s honored — first `timeline` near 600s, not 0 |
| `toy_story4_resume_transcode` | 1945 | `resume(transcode): restart at offset 600s`, first timeline near 600s |
| `morning_show_audio_native` | 1804 | native audio switch (eac3→eac3) — `audio switch (native)`, codec **stays 174** |
| `home_alone_audio_transcode` | 3 | English (DTS) audio → transcode — `re-transcode` + `reload_transcode`, codec 174 (HEVC target; the video is re-encoded H264→HEVC — an audio-only/video-copy transcode is a future improvement) |
| `substance_subtitle_srt` | 4 | embedded subtitle soft-render on the **default `ff.rs` demuxer** — `sub cue [..] "text"` lines |
| `toy_story2_subtitle_pgs` | 1919 | **PGS image subtitle** client-render on HEVC 4K direct-play — `ff.rs` software-decodes the bitmap and logs `image cue [..] WxH at X,Y rects=N canvas=WxH` (op flagged `"image": true`) |

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
our in-process buffer-feed — see project memory). Verified on Toy Story 2 (rk 1919, 6× PGS).

The case targets `The Substance` (rk 4) — the local copy is Russian-dubbed with four text
tracks `[RU-forced, RU, EN, EN-SDH]`, so it picks **row 3 = the English track** (row 0 is Off;
`desired_sub_idx = row − 1`) and seeds a `viewOffset` of 843 s so playback lands in the dense
opening monologue and cues appear within the run window. (For a transcode item, soft subs ride a
WebVTT sidecar, which per project memory delivers 0 bytes on this pipeline — direct-play is the
only reliable sub path.)

## Adding a case

Append an entry to `manifest.json` → `cases`:

```json
{
  "name": "my_case",
  "rk": "1234",
  "kind": "movie",
  "title": "…",
  "covers": ["…"],
  "run_secs": 60,
  "setup": { "viewOffset_ms": 600000 },        // optional — seeds resume
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
labeled "synthetic" and secondary to the 8 authoritative real items:

- **Video:** VP9, MPEG-2, VC-1, MPEG-4-ASP; **8-bit HEVC** (every HEVC is Main10); **4K H.264**
  (all 4K is HEVC/AV1); interlaced. (These would exercise the transcode-fallback path from the
  client side.)
- **Audio:** FLAC, PCM/LPCM, MP3; a DTS-only file to force an audio-only transcode without
  depending on Home Alone's track ordering.
- **Subtitles:** ASS/SSA, VobSub/dvd_subtitle; mov_text/tx3g soft-render.
- **HDR:** HLG, HDR10+, Dolby Vision P5 (IPT, no HDR10 fallback — only on Wicked, mp4).
