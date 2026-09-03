# On-device regression harness

Headless regression tests for the native webOS Plex player. Nothing on the host decodes a frame or
talks to Starfish/ACB, so every test here drives the **real app on the real TV** via the
`plxnative-*` dev triggers and asserts on the on-device event log (`plxnative-events.log`) — both
of them inside that install's **runtime root**, which is `/tmp` for the stable install and
`/tmp/<app id>` for a flavoured one. See *Which install it drives*, below: it decides every path
on this page, and the default is the **debug** build.

## Two tiers, and which one you want

There are two on-device suites here and they grade different subjects. Running the wrong one is
how a green run means nothing.

| | **synthetic** — the DEFAULT | **server** (`--server`) |
|---|---|---|
| what it proves | **transport and decode**: raw-socket GET → `ff.rs` demux over the custom AVIO → the two-lane AU queues → the Starfish `Feed()` pump → the ACB bind, and the Load payload's codec/Dolby **declaration** | **selection**: plex.tv auth, `/decision`, direct-play vs transcode, the PlayQueue, track menus from PMS metadata, markers, resume, the `/:/timeline` reporter |
| what it needs | a TV address and `make fixtures-pipeline` | a Plex server, a token, a `manifest.local.json`, and a library holding the shapes the matrix names |
| media | generated clips, served off your Mac | real items on your PMS |
| who can run it | **anyone** | whoever owns that library |
| cost | ~0.9 GB, a few minutes to build; seconds per case | ~3 GB, ~20 minutes to build; ten to twenty minutes to run |

```sh
./tests/run.py                  # the synthetic tier — 20 cases, ~7 min, needs nothing
./tests/run.py --server         # the library-backed 21, through the whole Plex chain
```

**The synthetic tier is the default**, and that inversion (2026-08-22) is about what a bare
`./tests/run.py` should mean. The default has to be the thing that runs for everybody, needs no
credentials, touches nobody's watch history, and answers *"is the player broken"* — which is asked
far more often than *"is my library's metadata right"*. Charging a PMS, a token and a filled-in
overlay for the obvious command meant most people could not type it at all. `--server` is the
explicit opt-in; `--fps` / `--fps-player` imply it, because those scenes navigate a real signed-in
Home and would otherwise grade a QR screen. `--pipeline` still parses — it names the default — but
combining it with `--server`/`--fps` is refused rather than silently resolved.

They are complements, not a ladder. The synthetic tier is what isolates *"the player is broken"*
from *"the library layer is broken"* when a server case fails — and it is the only tier a stranger
can run at all. **What it cannot prove, ever:** that the declaration it feeds is the one a real item
would produce. It writes those five route fields itself, so the whole `metadata → plan → apply_plan`
half is bypassed and a regression there passes it green. Nor does it reach resume, markers, Up Next,
timeline reporting, subtitle or audio-track *selection*, or any transcode. **Never ship on the
default alone.**

### What the synthetic cases actually cover

The player direct-plays exactly `{h264, hevc}` × `{aac, ac3, eac3}` in `mkv`/`mp4`/`m4v` —
`route.rs`'s codec gate and `plex::DP_AUDIO_CODECS`. That is **2 of the 19 video codecs and 3 of the
19 audio codecs this television's own capability table
(`/etc/umediaserver/device_codec_capability_config.json`) claims to decode**; everything else the
panel can decode — VP9, MPEG-2, WMV, DivX, DTS, FLAC, Opus, PCM… — reaches it as a server transcode
by design, because the Starfish `Load` payload has only the strings `H264`/`H265` and
`AC3`/`AC3 PLUS`/`AAC`. The synthetic tier covers **all six** of those payload combinations:

| | AC3 | AC3 PLUS | AAC |
|---|---|---|---|
| **H264** | `pipe_h264_ac3_1080p` | `pipe_audio_lane_eac3` | `pipe_audio_lane_aac`, `pipe_h264_aac_mp4` |
| **H265** | `pipe_hevc_ac3_lane` | `pipe_hevc_eac3_4k_hdr10`, `pipe_hevc_4k_60fps` | `pipe_hevc_aac_mp4` |

plus Dolby Vision 8.1 (`pipe_hevc_eac3_4k_dovi_p8`), both containers, in-place seek in each of
them, the **frame-rate axis** — `pipe_h264_1080p5994` is the only fixture in the repo that reaches
`engine::fps_rational`'s 1001-denominator branch (`esInfo: videoFps 60000/1001`), and
`pipe_hevc_4k_60fps` is 4K60 HEVC, the most demanding thing the device table claims; every other
fixture in both packs is 24p — the **resolution × codec matrix** (SD/HD/FHD/UHD × h264/hevc, eight
cells, §*The resolution × codec matrix* below), and one clip played to its **end**
(`pipe_finish_eos`).

Still uncovered, and worth knowing before quoting a green run: **HLG, HDR10+, DV P5/P7, Atmos**
(the `atmos` declaration field exists and no case sets it), the **4096-wide edge** and any file
that must be *refused* for exceeding it, a **user-driven** replay (as opposed to the trigger-driven
one below — a Play control on a detail page is server-tier by construction), and the whole
**transcode input space** — three server cases on one AV1 item stand in for 17 video codecs. One of
those is an app gap rather than a test gap: `devcaps` now reads `maxFrameRate` into per-codec rows,
but only the Starfish Load's `adaptiveStreaming` ceiling clamps against it (`engine::sink_envelope`,
since 2026-09-03) — the profile sent to PMS still carries no frame-rate limitation at all.

- `manifest.json` — the test matrix, and **installation-independent**: the triggers each case
  needs, the expected log signals, and the *shape* of the item it needs (`item`, a symbolic key
  like `movie_h264_ac3_1080p`) rather than any ratingKey.
- `manifest.local.json` — **gitignored, one per installation**, and required. Maps each `item`
  key to a ratingKey on *your* server, and carries your `pms` host, `tv` address, `test_user`,
  which `flavour` to drive, and (optionally) the `shared_server` a two-source case needs.
  Copy it from `manifest.local.json.example` and fill it in; `run.py` merges it over the manifest
  at load and refuses to run without it. An `item` key it cannot resolve is **not** fatal — see
  *Running it against your own library*.
- `run.py` — the runner (Python 3 stdlib only; macOS system `python3` is fine).
- `fixtures/` — **`make_fixtures.py`, which SYNTHESIZES the media those `item` shapes name** (and
  its own README). If your library has no TrueHD-default-with-AC-3-sibling, no Dolby Vision 8.1
  and no PGS track — nobody's does by accident — this builds all nine shapes from ffmpeg/lavfi on
  the host, no television involved, and tells you exactly what it proved about each file.
  `make fixtures` / `make fixtures-quick`.
- `README.md` — this file.

**The runner takes the television's lock** (`tools/tv-lock.sh`) at the point it commits to driving
the set, and releases it after the teardown. There is one dev TV and no OS-level mutex: a second
job during a run does not fail cleanly, it produces plausible wrong results — a `timeline_climb`
that failed because the other lane closed the app, an fps sample taken while a deploy was landing.
If the set is held, the run refuses and names the holder; queue for it with
`tools/tv-lock.sh acquire --wait 540`, or wrap the whole run in
`tools/tv-lock.sh with --why "…" -- ./tests/run.py …`. See the `tv-lock` skill.

The split is not just anonymisation. The symbolic key keeps "five cases deliberately share one
item" visible in the tracked file — which is the fact the per-case resume reset exists for — and
it makes a mis-set item a named setup error instead of a mystery failure.

## Which install it drives (`--flavor`)

Two builds can sit on one television: **`com.beb.plxnative`**, the app users install, and
**`com.beb.plxnative.debug`**, the developer build beside it — its own launcher tile, its own
sign-in, its own runtime files. They are separate apps to SAM, and a run has to drive exactly one
of them end to end.

The flavour is resolved once, before anything is touched: **`--flavor`**, else the overlay's
**`flavour`** key, else the Makefile's own default (`debug` — the dangerous id has to be typed).
Everything downstream is then **asked for**, never restated: `run.py` reads the app id, the runtime
root and the event-log path back from `make -s print-appid / print-rundir / print-eventlog
FLAVOR=<f>`, and every `make` it shells out to carries the same `FLAVOR=`. So the flavour it
resolved and the flavour it kills, launches, arms triggers in and greps for cannot drift apart —
closing install A while launching install B reproduces SAM's stale-"running" no-op and then grades
the other app's log, which is *plausible wrong data*, not a clean failure.

- **The runtime root moved; the names did not.** Every `plxnative-*` trigger, the `plxnative-remote`
  FIFO and the three `*.log` files are named exactly as before — only the directory they sit in
  changed. Ask for it (`make -s print-rundir FLAVOR=debug`) rather than writing it out.
- **The root is created `1777` — `mkdir` then an explicit `chmod`** (umask masks `mkdir`'s mode),
  in the same round-trip that writes the first trigger. Two uids write into it and neither can be
  made to go second: the harness arms triggers over ssh **as root** before the app has ever booted,
  and the app then runs jailed under its own uid and creates its logs there. An owner-only mode
  locks the other one out, and a root-owned event log the app cannot write stays 0 bytes — which
  every assertion here reports as "no line found", i.e. exactly like a total regression.
- **`--flavor stable` normally aborts, by design** — see the boot-line check below.

### The `install:` boot line is a precondition, not a log line

The app's **first** log line names the install that wrote it, before anything can fail:

```
install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug features=dev APPID_env=<value|unset>
appdir: /media/developer/apps/usr/palm/applications/com.beb.plxnative.debug (from current_exe)
```

`run.py` grades it as the log arrives and **aborts the whole run**, once and by name, if `id=` is
not the app id it drove or `features=` is not `dev` (`check_install`). Nothing else can answer that
question: both binaries are named `plxnative`, and `pkg/plxnative` is a path every flavour *and*
every configuration writes, so an md5 against the local build proves only that *some* build
matches. An absent boot line is refused too — it means a deployed binary that predates the line,
and an unattributable log.

Uncaught, a **release** build fails like a catastrophe rather than like a mistake. `devtriggers` is
compiled out, so it reads nothing under the runtime root: the injected token is ignored, the app
parks on the who's-watching picker having played nothing, and every assertion fails as "the line
has not appeared **yet**" — which `failed_for_good` deliberately never settles. Every case then
burns its full `run_secs` and the summary reads as a total regression, for a build that is working
perfectly. This is also why `--flavor stable` aborts in normal use: `make deploy` refuses to put a
dev build on the stable id without `ALLOW_DEV_ON_STABLE=1`, so that install *is* a release build.

### Three ways to name a running app, and they are not equivalent

| | scope | matches |
|---|---|---|
| `fuser <appdir>/plxnative` | **inode** | exactly the install at that path |
| `luna-send … closeByAppId {"id":…}` | **app id** | exactly that install |
| `pidof plxnative` | **name** | **both installs** — both binaries are called `plxnative` |

`make kill` uses the first two and carries `FLAVOR=`, which is what leaves the *other* install
alone — including its running app. **`pidof plxnative` is no longer a liveness test**: it returns
two pids, in an order busybox does not promise. Use `fuser <appdir>/plxnative`, or resolve
`readlink /proc/<pid>/exe` per pid. And any match on the app id must be **anchored on a delimiter**:
`com.beb.plxnative` is a prefix of `com.beb.plxnative.debug`.

## Security

The PMS **X-Plex-Token is secret and is never committed**. `run.py` reads it from the
gitignored `src/config.local.h` (`#define PMS_TOKEN "..."`) at runtime and never prints,
logs, or writes it — progress URLs are redacted to `<token>` in output. The TV ssh
credentials are already in the committed `Makefile`, so the runner shells out to `make` /
`sshpass` for device I/O (no new secret is introduced).

Every other token the harness uses is **derived from that one at run time and stored nowhere**: the
managed user's per-server token (below), and a second server's access token (further below). Both
are written to a `plxnative-*` file in the TV's runtime root and cleared by the same glob wipe —
before every case, and again by `teardown()` on *every* exit path, including Ctrl-C and a crash.

## Test identity — runs as a managed user (no watch-history pollution)

By default the harness plays **as the Plex Home managed user in `manifest.local.json` →
`test_user`**, so test playback + timeline scrobbles land on *that* user's history and your real
account stays clean. It works without storing any new secret:

- `run.py` uses the owner token (from `config.local.h`) to look up that user's **per-server
  access token** from `GET https://plex.tv/api/servers/<machineId>/shared_servers` (keyed by
  `userID` — which is what `test_user.id` is). The managed user must already have the libraries
  shared with it.
- That token is used for the `/:/progress` resume seed **and** written to `plxnative-token` in the
  TV's runtime root. The binary carries **no** token, so this file is the only way an automated run
  gets PMS access at all (see `plex_run`); the **app itself** then plays and scrobbles as the
  managed user, not just the seed. The token value is never printed (redacted to
  `<…, redacted>`), and `plxnative-token` is cleared between cases like every other trigger.
- Pass **`--owner`** to run as the `config.local.h` owner token instead (history *will* be
  affected). If the overlay has no `test_user`, the runner falls back to owner with a warning.

## A second server (a friend's shared one)

`plxnative-token` carries exactly **one** token, and a shared server is a **separate
authority**: its own `machineIdentifier`, its own per-(user,server) access token, and a 401 for
anybody else's. So a screen that shows two sources at once could only ever be checked by hand, one
capture at a time. `plxnative-servers` is the second credential channel — **purely additive**:
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
  `plxnative-servers` for those cases only — a JSON array of
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

## Running it against your own library

**The matrix is a superset of what any one library can exercise.** It was derived from a survey of
the maintainer's server, so it names shapes — 4K Dolby Vision P8, a TrueHD default with an AC-3
sibling, a PGS subtitle track, AV1 with no direct-playable audio — that most Plex libraries simply
do not contain. Until 2026-08-22 that made the suite unrunnable by anyone else: a single
unresolvable key called `_die_no_overlay` and killed all 21 cases, so the honest answer to "can I
test my change?" was no.

Now an `item` key the overlay cannot resolve — **absent, or left as the example's `<ratingKey>`
placeholder** — skips the cases and fps scenes that need it and runs the rest:

```
  [SKIP] dp_hevc_eac3_dovi_p8  <- `items.movie_hevc_4k_dovi_p8` is still the template placeholder
16 passed, 0 failed, 0 known-gap of 16, 5 skipped
```

Skips are printed in the summary and by `--list` (which is offline — it needs neither the TV nor
the server, so you can see your own coverage before touching anything). **The pass count is never
quotable without them**: `16 passed` means sixteen of the shapes you have, not sixteen of twenty-one.

What that buys, concretely. One ordinary **h264/AC-3 1080p movie with embedded SRT** — the shape
nearly every library has — is enough for `dp_h264_ac3_1080p`, `seek_inplace_h264`,
`seek_rapid_h264`, `resume_directplay` and `subtitle_text_srt`, i.e. the direct-play open, both
seek tiers and the soft-subtitle renderer. Adding `movie_in_home_catalog` (any movie reachable from
Home's recently-added or on-deck row) brings the UI fps tier to 13 of 16 scenes. The rest of the
matrix unlocks shape by shape as your library supplies them.

Three things are worth knowing before you read a green run as a portable claim:

- **A skipped case is coverage you did not get**, not a pass. The four `covers` tags on each case in
  `manifest.json` say what a skip actually costs; losing `episode_hevc_4k_hdr10_eac3` alone takes
  six cases with it, including the whole marker/up-next tier.
- **Two shapes are not portable and never will be.** VC-1 has no free encoder anywhere, and Dolby
  Vision profile 5 / dual-layer profile 7 cannot be authored with free tooling — see *Library gaps*.
- **A mis-mapped item fails as a player bug.** The harness cannot tell "this ratingKey is the wrong
  shape" from "the player regressed": map an mp4 to a case expecting mkv direct play and you get a
  transcode assertion failure that reads exactly like a routing regression. When a case fails on a
  fresh installation, confirm the item's shape in Plex before believing the failure.

**Watch history:** every case clears the item's resume point (`/:/unscrobble`) before it runs and
may seed a fake one. That is intentional and documented below, but it is destructive against a
library you care about — which is what `test_user` is for.

## Prerequisites

- The same toolchain the main dev loop needs: the webOS NDK (`make setup-env`), `sshpass`,
  and (for `--build`) the Rust nightly + `rust-src` (see the repo `Makefile` / `docs/agent-reference.md`).
- `tests/manifest.local.json` present — `cp tests/manifest.local.json.example` and fill in the
  PMS host/port, the TV address, `test_user`, and **as many `item` ratingKeys as your library can
  actually supply**; leave the rest bracketed and the cases that need them are skipped (below).
  If your library cannot supply them, **generate them**: `make fixtures` builds every shape from
  ffmpeg and writes a `fixtures.json` keyed by the same symbolic names — see
  `tests/fixtures/README.md`, including the three `marker_*` cases it cannot solve. Its
  `shared_server` block is optional; delete it unless a second server is shared with your account
  (see below). Its `flavour` key is optional too — see above; the fallback is the Makefile's own
  default.
- **The install being driven must already exist on the TV.** `make deploy` scp's into an app
  directory SAM already knows about; a flavour is registered once with
  `make FLAVOR=<f> install`, which builds its .ipk, installs it and then deploys into it.
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

# drive the OTHER install (overrides the overlay's `flavour`; read the boot-line check first)
./tests/run.py --flavor stable --filter dp_h264

# run as the OWNER token instead of the overlay's test_user (history WILL be affected)
./tests/run.py --owner --filter dp_h264

# hand the app a SECOND server's credentials for every case of this run (see below)
./tests/run.py --shared-server --filter dp_h264
```

The runner prints per-assertion PASS/FAIL with the failing evidence line, then a final
summary table. **Exit code is nonzero if any selected case fails** (CI-friendly).
Add `--verbose` to print evidence for passing assertions too.

**Every playback case in both tiers grades `presented_rate` (since 2026-09-03):** the frame rate
the video sink actually presented — the app's `sink: displayed=<n>` line, libpf's 200 ms poll of
the sink's `non-flushable-displayed-frames` (raw callback 47 on webOS 4, 49 on 5+, normalised by
the app), armed by the Load payload's `streamQualityInfoNonFlushable` —
against the rate the app declared on the `load:` line, as a median over the healthy wall seconds
(position advancing, `play=` at real time) within ±6 %. It is the only assertion that can see a
24p stream presented at 13 fps with every other instrument reading healthy, which is exactly what
a 4K H.264 stream declared at `maxFrameRate: 60` did on the dev set. A transcode declares no rate
and is skipped (the evidence says so); `expect.presented_rate: false` opts a case out, and
`presented_rate_tol_pct` widens the band. Each case also prints a `presented:` characterisation
line from the same counter.

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
# UI tier only — every scene whose `tier` is "ui". No video; it implies --server and (since
# 2026-09-02) resolves the test identity first, so it needs src/config.local.h and the overlay —
# without a token every route scene but the login spinner (which WANTS the sign-in screen) boots
# to QR sign-in and grades a screen it never reached.
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
    the fix that note asks for. The remaining `loop_floor`-only scenes are `info-panel`, `chapters-panel` and
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
  justified here by "the panel GPU thermally throttles" — that was an unmeasured hypothesis, and it
  is now MEASURED AND REFUTED**: a control leg holds 60/60/60 across six runs on a set up 2 h 15 m
  under continuous load, while arming the HWCNT profiler drops that same leg to 45. The 50 fps
  readings in the archived notes belong to the instrument, not to the panel. Keep the margin for
  jitter; do not keep the story. Nothing has ever measured a
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
3. Create the runtime root (`mkdir` + `chmod 1777`), clear every `plxnative-*` trigger in it,
   then write only the ones this case needs.
4. `make run TV=<tv> RUN_SECS=<n> FLAVOR=<f>` — relaunch, wait, and cat that install's
   `plxnative-events.log` back.
5. Filter the `smp_cb type=43 num=0 str=` flood and evaluate the assertions.

## The `plxnative-play=<rk>` trigger (added for this harness)

Tests use `plxnative-play=<ratingKey>` instead of the fragile `plxnative-detail`. `plxnative-detail`
only *plays* if the rk is in the home catalog (Continue Watching / hubs); off-catalog it loads
data-only and never plays. `plxnative-play` fetches the item's metadata fresh (`metadata::load_detail`,
works for **any** rk) and drives the same field-based play path the detail Play button uses
(`route::play_episode` — generic over movie/episode — + `player::resume_at` + `start_bufferfeed`),
bypassing the catalog lookup entirely. It honors the server `viewOffset` for resume and logs
`plxnative-play: rk=<rk> server=<slot> start` so the harness can confirm both halves of the item
identity that fired. A bare `plxnative-play` keeps the historical current-server behaviour;
`plxnative-server=<slot>` makes a by-hand/direct-screen run target a registered secondary PMS and
fails closed when that slot does not exist. `tools/tv-session.sh up --screen player=<rk> --server
<slot>` writes the pair without navigating the UI, boots from the signed-in stored roster (rather
than the singular injected-token server), and exits nonzero unless the log confirms the exact
`ratingKey + server slot` that started.

## Coverage — the 8 matrix item shapes + operations

The `item` column is the symbolic key `manifest.local.json` maps to a ratingKey on your server;
it is also the specification of what that item has to be.

Base playback (decision + codec + not-stuck), one case each:

| Case | item | Covers |
|------|------|--------|
| `dp_h264_ac3_1080p` | `movie_h264_ac3_1080p` | H264 + AC3 direct-play, 1080p, embedded SRT |
| `dp_hevc_eac3_4k_hdr10` | `episode_hevc_4k_hdr10_eac3` | HEVC 4K **HDR10** + E-AC3 direct-play, TV episode |
| `dp_hevc_truehd_ac3_sibling` | `movie_hevc_4k_hdr10_truehd` | **smart direct-play** (TrueHD default → AC3 sibling), HEVC 4K |
| `dp_hevc_eac3_dovi_p8` | `movie_hevc_4k_dovi_p8` | HEVC 4K **Dolby Vision P8** + E-AC3 direct-play. Grades that the stream **transports** (route, demux, bind, timeline, no error) — **not** that the panel engages Dolby Vision, which no assertion here can see. See the case's `_dovi_note` in `manifest.json` |
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
| `audio_switch_native` | `episode_hevc_4k_hdr10_eac3` | native audio switch (eac3→eac3) — `route transition: native audio idx=` (older logs: `audio switch (native)`), codec **stays 174** |
| `audio_switch_transcode` | `movie_h264_ac3_many_audio` | English (DTS) audio → transcode — `re-transcode` + `reload_transcode`, codec 174 (HEVC target; the video is re-encoded H264→HEVC — an audio-only/video-copy transcode is a future improvement) |
| `subtitle_text_srt` | `movie_h264_ac3_1080p` | embedded subtitle soft-render on the **default `ff.rs` demuxer** — `sub cue [..] len=<n>` lines |
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
- **audio switch:** `route transition: native audio idx=` / `route transition: user retranscode` + `reload_transcode:` (older logs: `audio switch (native)` / `re-transcode:`; both spellings grade).
- **subtitles (text):** `sub cue [<a>..<b>ms] len=<n>` — the cue's character COUNT, never its
  text; `len>0` means a text cue for the selected track arrived. The obvious "improvement" to
  this line is to put the text back, and it must not be made: subtitle text is LG Content
  Viewing Information and the event log gets photographed into issue threads.
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
ASS/SSA, mov_text) and emits `sub cue [..] len=<n>` lines. It pushes cues for **all** text tracks (tagged by
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
    // or: {"op":"pause_resume","delay_ms":25000,"hold_ms":6000,
    //      "min_climb_after_s":20}
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
`pause_resume`→one `plxnative-autopause=delay=<ms>,hold=<ms>` script;
`audio_switch`/`subtitle`→`plxnative-menupick`) and picks the
per-op assertions from the `op`/`mode`. Track-menu row semantics: **audio tab** row = the
metadata audio index (0-based, file order); **subtitles tab** row 0 = *Off*, row *r* = subtitle
index *r−1*.

A **synthetic** case is a `pipeline_cases` entry instead, and its `expect` keys are the ones listed
under *Assertions* below. Two are worth naming here because they are recent and easy to miss:
**`video_size`** (`"1920x1080"`) grades the demuxed raster EXACTLY and is what the resolution
matrix is built on — prefer it to `min_video_width` in any case where the resolution is the point;
and **`reaches_eos`** turns on the `finished` assertion, which only `pipe_finish_eos` may set,
because it needs a fixture short enough to actually run out.

## The synthetic tier (the default) — the player, with no Plex behind it

```sh
make fixtures-pipeline          # ~0.9 GB into $FIXTURES_OUT/pipeline; ~4 min, once
./tests/run.py                  # runs them on the TV
./tests/run.py --list           # offline: what would run, at what resolution, what is missing
```

Nothing else is required — no `manifest.local.json`, no PMS, no token, no ratingKey, no library, no
sharing. The TV address comes from the overlay's `tv` if you have one, else from the gitignored
`.tv-host`, else `--tv`. That is the whole configuration.

**How it works.** `run.py` starts `serve_fixtures.py` on this machine, then arms
`/tmp/plxnative-playurl` per case — one JSON object carrying the clip's URL **and the Load payload
declaration to play it with**:

```json
{"url":"http://192.0.2.10:50605/pipe_hevc_eac3_4k_dovi_p8.mkv","vcodec":"hevc","acodec":"eac3",
 "fps":23.976,"dovi":{"profile":8,"bl_compat":1,"el_present":false},"atmos":false}
```

The app enters the player straight from boot on that trigger alone — no home grid to press OK on,
which matters because with no session there isn't one. Everything downstream is the same engine,
byte for byte; only the *choosing* is bypassed.

**Why the declaration is carried separately, and why it is the interesting half.** The Starfish
`Load` payload takes its codecs from `route::stream_vcodec`/`stream_acodec` and its Dolby nodes from
`stream_dovi`/`stream_immersive` — five fields normally installed together by `route::apply_plan`
from a PMS decision and replaced together by later route transitions. The older `plxnative-url`
trigger hands over a URL and nothing else, so a URL-fed 4K HEVC file was declared to the television
as whatever the route happened to hold: on a fresh boot, the empty string, which falls through the
engine's `_ =>` arm to an H264 payload with `"AC3"` audio.
The declaration is precisely what governs HEVC-vs-H264 payload selection, LG's `"AC3 PLUS"`
renaming of E-AC-3, and both Dolby nodes — so a tier that cannot set it cannot test any of them.
`plxnative-playurl` sets all five in one write (`route::set_stream_declaration`).

That fallthrough is also the tier's main false-PASS risk, and the manifest is shaped against it: an
unread trigger produces exactly the right payload for the AC-3 baseline case. So the matrix carries
cases whose expected `load_audio` is `"AC3 PLUS"` and `"AAC"` — values the fallthrough cannot
produce by accident — and the `load_decl` assertion grades the app's new `load:` event-log line.

**Assertions.** `stream_path` (the demuxer opened *this* case's fixture, not something a stale
trigger pointed it at), `load_decl` (above), `codec` and `audio_stream_index` (what the demuxer
*found*, independent of what was *declared* — the two can only ever agree on the integration tier),
`video_bound`, `pos_climb` (the 1 Hz heartbeat only; there is no `/:/timeline` fallback here and
accepting its absence would make a broken assertion read as a pass), `no_error`, `finished` (below),
the seek assertions, and **`server_wire`** — the counters from the fixture server itself, which is
the one assertion no log line can give: the pump logs its seek intent whether or not the demuxer
ever reached the AVIO, so a counted `206` is the only proof the `Range` reopen actually happened.

### Auto under a changing link, without Plex

`pipe_auto_original_slow_recover` is the synthetic equivalent of changing the TV's router limit
while a movie is already playing. One fixture-server response follows a wall-clock schedule whose
clock begins on its first body byte: 40 Mbit/s, then 4 Mbit/s, then 40 Mbit/s again. The app starts
an 8 Mbit/s Original, must decide that the shortfall will outlast its content reserve, replace it
with the best HLS rung its measured capacity sustains, and later — once the link recovers and a
bounded probe of the actual Original fixture clears the source's declared average bitrate with
enough confidence — perform a third fresh Load back onto Original and re-arm the progressive
controller. The assertion requires that ORDER: a fallback without recovery, a probe logged before
the collapse, a probe that does not clear the bar, or a requested transition the route never
committed all fail.

The probe gives connection/header setup and body measurement separate bounded windows. That keeps
one-off DNS/TLS/header latency from shortening the interval which measures the uncapped Original
body. On PMS, the bounded raw Part GET exact-reuses the active HLS resource identity without a
client-side stop, close or replacement of the working encoder. This prevents the known second
AdHoc admission path, but does not prove that PMS preserves the prior HLS cursor while serving the
raw Part. A successful recovery therefore publishes its handoff on the same completed media
boundary and performs no subsequent HLS GET. The reserve gate still funds the two separately
bounded probe phases plus HLS continuity when the result retains HLS:

```text
B >= 2P + max(R, D)
```

The synthetic fixture has no PMS resources, so it exercises the source setup/body and controller
half; the exact-identity lifecycle is pinned by the loopback protocol test and the server/device
tier.

The remote-device acceptance run on 2026-08-31 exercised the complete physical sequence with the
panel off and hardware audio muted. A 4 Mbit/s whole-link cap drove Auto from Original to 2 Mbit/s
HLS. Releasing the cap produced a setup-bearing 4K object in 2.504 s, the next ordinary object in
1.407 s, and a commit to the response actually delivered: 20.895 Mbit/s at 3840x2160. Sixteen
successive complete active objects then arrived in 1.197–1.449 s without a false terminal abort.
Once the exact reserve funded the serial source transaction, its finite body measured 49.6 Mbit/s
against a 25.264 Mbit/s source requirement; Auto selected Original and continued Dolby
Vision/Atmos playback for more than 40 s without a playing error. This is device evidence for the
slow→HLS→4K HLS→Original arc; the private server log and item identity are intentionally not
committed.

**What it deliberately does not grade is the rung the ladder happened to be on when the probe
fired.** It used to require the 20 Mbit/s top rung plus two spaced probes, and that measured the
wrong resource: PMS producing 20 Mbit/s of H.264 says the SERVER can encode and says nothing about
whether the link can carry the remux (`docs/adaptive-playback.md` §7). The current gate has no
spacing timer or fixed upshift dwell: HLS frontier exhaustion, a non-draining reserve and exact
serial affordability can become true in either order. Requiring a particular current rung first
would therefore grade incidental ordering rather than the recovery. The old "three healthy
segments" and "ladder's five" counts are historical; neither number exists now.

The HLS master/media playlists are generated by `serve_fixtures.py`, and their independent
two-second H.264/AAC MPEG-TS segments come from **one rate-targeted clip per rung, each cut into
six real segments**. What a rung asks PMS for is a bitrate CEILING and the raster is the
consequence, so two rungs may share a raster — 2000 and 4000 are both 1280×720 — but never a
clip. No PMS decision, real token, library item, transcoder, or Internet path participates.

**Both halves of that arrangement were wrong until 2026-08-26 and the fix is what makes this tier
able to identify anything.** The server advertised 90 segments per rung and *discarded the sequence
number*, so all 90 returned one file: 593 segments in a device run carried exactly **ten** distinct
byte sizes, each of them a file size on disk, which made `bytes` an exact function of `rung` and
left a transport-model fit with ten effective data points however long the suite ran. And the four
low rungs were encoded to a QUALITY rather than a rate, so they delivered 1.57×–1.90× of the rung
they name against 1.14× for the rest — most of the 2.4× nominal/delivered spread that refuted the
admission rule. The pack now holds **72 distinct segment sizes** with a within-rung spread of
1.08×–1.35×, and delivered/requested inside 1.16× across the whole ladder.
`docs/measurements/p1-transaction-anatomy.md` §6 is the measurement.
The host suite separately drives the same response body through fast → slow → fast rate phases;
the TV case proves the real transport, FFmpeg, queue watchdog, pipeline reload, and HLS controller.

### The resolution × codec matrix (LG App Self Checklist #50 / #51)

That item is graded as a **matrix**, and until 2026-08-23 this repo could only answer it as
*"pieces are covered"*: every fixture was 1080p except one 4K HEVC, so SD and HD had never been
played at all and 4K H.264 existed in neither pack (`library_gaps` lists it — every 4K item in the
maintainer's library is HEVC or AV1, which is a fact about one library and not about the app).
Eight cells now, all direct-play, all 24p, **one audio codec per column** so a row-to-row
difference is the resolution and nothing else:

| | h264 / AC-3 5.1 mkv | hevc / E-AC-3 5.1 mkv |
|---|---|---|
| **SD** 720×480 | `pipe_res_h264_sd` | `pipe_res_hevc_sd` |
| **HD** 1280×720 | `pipe_res_h264_hd` | `pipe_res_hevc_hd` |
| **FHD** 1920×1080 | `pipe_h264_ac3_1080p` | `pipe_res_hevc_fhd` |
| **UHD** 3840×2160 | `pipe_res_h264_uhd` | `pipe_hevc_eac3_4k_hdr10` |

Two cells predate the matrix and keep their names; `covers` carries `resolution-matrix` on all
eight, so grepping that tag finds the lot. Every cell grades **`expect.video_size`** — an exact
`WxH` out of the `ff:` line's `AVCodecParameters`, not a `min_video_width`, which cannot tell
720×480 from 720×576 and is exactly how a matrix ends up answered with *"at least 1900 wide"*.
`./tests/run.py --list` prints the resolution as its own column.

Three things it deliberately does **not** cover, so nobody reads a uniform grid that is not there:
the UHD/HEVC cell is HDR10 Main10 where the rest are SDR 8-bit (HDR is a third axis, that cell is
the one already device-verified, and an SDR twin would differ only in `trc=`/`pri=`, which nothing
here grades); the 4096-wide edge the device table claims, and any refusal above it; and every
non-24p rung — the frame-rate axis is `pipe_h264_1080p5994` and `pipe_hevc_4k_60fps`, at 1080p and
4K only.

### Playing to the END, and then again (LG #46)

`pipe_finish_eos` and `pipe_replay_after_eos` are **the only cases in either tier that are supposed
to run out**. Every other clip in both packs is sized so it *cannot* hit EOF inside a case's window
(`make_fixtures.py` trap 9's second half), because a finish tears the session down under assertions
that were only ever about playing — so those two share a 20 s fixture, `pipe_h264_ac3_short.mkv`,
which only a case declaring `reaches_eos` may name. The `finished` assertion wants two lines **in order**: `EOS reached: … → ended` (the pump
gates that on `eos_pushed && pos >= dur - 1s`, so a truncated transfer does not reach it) and then
`stop_bufferfeed: torn down`. The order is the assertion — every stop tears the engine down, the
harness's own close included.

**And the second half — starting the same content *again* — is `pipe_replay_after_eos`**, which
plays the same clip, lets it end, and restarts it. That needed an app change and got the smallest
one that works: `/tmp/plxnative-replay[=N]` re-arms `app.rs`'s one-shot `auto_tried` latch N times
when a `plxnative-playurl` playback reaches EOS, so the next frame goes back through the entry it
booted through. `teardown` clears the URL and the `ended` flag on a real stop, and
`engine::start_bufferfeed` re-reads `dev::playurl()` whenever `route::url()` is empty. One more
piece was NOT in place until 2026-09-02: the fixture entry asks the route reducer for a start
owner directly (no `begin_playback_request` resolve in front of it), and a completed stop used to
leave the reducer latched in `Stopping`, which refused it — one Load where the case needs two. The
stop now publishes `Idle` (`route::finish_engine_teardown`, called from both `teardown` exits), and
`begin_route_start` mints the replay its owner from there.

A **counter**, not a lifted latch: `auto_tried` also guards the `autoplay`+`playidx` arm, which
does a `request_play_movie` + `load_detail_now`, so an unconditionally re-armable latch would
re-fetch a catalog item on every player exit and loop a real playback forever. An absent trigger is
0, which leaves every other boot byte-identical, and `replay_budget` (host-tested) is what turns
the file's contents into that number.

The `replayed` assertion wants **three** things, because each is worthless alone: the `replay:`
line exactly N times (counted — more often than asked is a loop, and a loop satisfies everything
else); at least N+1 `load:` lines (one per session, so the second says the declaration really was
re-read); and the media position **falling and then climbing again** (a replay that resumed where
the first run stopped would produce both lines above and no second viewing). `server_opens_min: 2`
is the wire-side half: the second viewing has to fetch the clip again, which no log line can prove.

Two things it still does not cover: **replay driven by a user** rather than by a trigger — that is
a Play control on a detail page, so it belongs to the server tier — and replay of a **transcode**,
which has a server session behind it that a synthetic clip has no equivalent for.

**`serve_fixtures.py`, and why not `python3 -m http.server`.** The demuxer seeks by closing the
socket and reopening with `Range: bytes=<n>-`, and `stream.rs` accepts any 2xx. A server that
ignores `Range` answers `200` with the file *from byte zero*; the AVIO believes it is positioned at
`n`, and every byte after that is offset garbage — with no error anywhere, presenting as a corrupt
bitstream or a hang. `SimpleHTTPRequestHandler` is exactly that server. Ours answers `206` or `416`
and never `200`-with-a-`Range`, and `--selftest` proves the ranged body is the tail of the whole
body.

**The macOS trap, and it costs an afternoon every time.** The application firewall silently drops
the TV's connections to an ad-hoc python listener — no refusal, no log line, the app's open just
reads empty, and every assertion then fails as "no line found", which is what a total regression
looks like. Accept the *allow incoming connections* prompt for this python **once, with a human at
the keyboard**, before any headless run. If the server saw no request at all, `run.py` says so.

**Skips.** A fixture the pack does not hold skips its cases with the reason named, exactly as an
unresolvable `item` does on the integration tier — and so does a fixture generated *shorter* than
the case seeks into it (checked with `ffprobe`), which would otherwise fail as though the player
had regressed. `pipe_finish_eos` takes the opposite bound and skips when its fixture is **longer**
than `0.6 × run_secs`, since it has to play the whole clip at 1× inside the cap; a pack regenerated
with `--secs`/`--quick` is the realistic way to trip that, and `finished` failing on a 300 s clip
would read as the app freezing on the last frame. As always: the pass count is meaningless without
the skip count beside it.

## Library gaps — combos NO real item can cover

The library survey found these have no exercising item; the harness can't test them with real
media (see `library_gaps` in `manifest.json`). **The pipeline tier above is the machinery for closing them** — synthetic
clips, served from the host, fed by boot trigger — and a gap becomes a case by adding a shape to
`make_fixtures.py` and an entry to `pipeline_cases`. Two corrections to how this paragraph used to
read, both of which would send you down a dead end: the server is **`tests/serve_fixtures.py`**, not
`python3 -m http.server`, which has no `Range` support and silently corrupts every seek; and the
trigger is **`plxnative-playurl`**, not `plxnative-url`, because the latter carries no declaration
and several of these gaps (HLG, HDR10+, 8-bit HEVC) are *about* what the payload declares. Still
secondary to the real item shapes, and still to be labelled synthetic:

- **Video:** VP9, MPEG-2, VC-1, MPEG-4-ASP; interlaced. (These would exercise the
  transcode-fallback path from the client side.) **4K H.264 came off this list on 2026-08-23** —
  `pipe_res_h264_uhd` generates it — and **8-bit HEVC was already off it and nobody had noticed**:
  `pipe_hevc_aac_mp4` carries no `hdr` key, so it is Main / `yuv420p`, and has been since that
  fixture landed; the matrix widened the raster spread rather than closing the gap. Both caveats
  are the same one: these are generated clips, so what neither reaches is the half the entries were
  really about — a **PMS decision** on such an item. That still needs a real one.
- **Audio:** FLAC, PCM/LPCM, MP3; a DTS-only file to force an audio-only transcode without
  depending on the many-audio movie's track ordering.
- **Subtitles:** ASS/SSA, VobSub/dvd_subtitle; mov_text/tx3g soft-render.
- **HDR:** HLG, HDR10+. **Dolby Vision P5 and P7 are a different kind of gap — the items exist and
  the missing thing is a CASE.** This entry used to read "the only item carrying [P5] is mp4, which
  the container gate sends to the server anyway", and that was stale from **2026-08-11**, the day
  `.mp4`/`.m4v` joined `.mkv` on the direct-play side (issue #22). For the ten days after it, a
  Profile 5 file — single-layer IPT-PQ, no HDR10 base — direct-played and was described to Starfish
  as bare `H265`, which displays in visibly **wrong colours**, and nothing in this suite could
  notice: the codec assertion reads `avcodec_get_name`, and that answers `hevc` for a P5, a P7, a
  P8 and an ordinary SDR file alike. Since **2026-08-21** the route refuses both non-self-displayable
  shapes (P5, and dual-layer P7) — see `metadata::Dovi::base_layer_unusable` — and `ff.rs` logs what
  the demuxer actually found (`ff: … dovi=P5 level=6 bl_compat=0 rpu=1 el=0 bl=1 …`) at every open.
  **A case here must grade the OUTPUT, not the decision**, and that is the trap this gap now
  carries in writing: measured against the dev PMS the same day, refusing direct play produced
  `Part.decision=transcode` while the VIDEO stream's own decision was **`copy`** — the identical
  IPT-PQ bitstream one container down, with `DOVIProfile: 5` still on it. A case asserting
  `decision: transcode` would have passed over a completely unfixed bug. The route now also
  withdraws the copy permission (`TranscodeSpec::no_video_copy`), and this PMS answers **general
  code 2000 — "File is unplayable. DoVi (Profile 5) color space is not supported."** — so the P5
  item's honest on-device expectation is the **failure read-out quoting that sentence**, not a
  playing stream. The P7 does transcode for real (an enhancement layer is not copyable), which is
  why a case built on the dual-layer item alone would also have looked clean. What still needs a
  human in front of the television is whether the panel engages Dolby Vision at all on the P8 file
  that does direct-play.
  **And since 2026-08-21 the P5 half of that refusal is CONDITIONAL, so read the paragraph above as
  the behaviour with `plxnative-dv` absent — which is what this suite runs and what a
  `RELEASE=1` build compiles in.** Arming that trigger makes the Load payload declare the stream
  (`contents.DolbyHdrInfo`, the node LG's own pipeline has parsed all along), and a declared
  single-layer Profile 5 direct-plays *correctly* — so with it armed the P5 item's expectation is a
  playing stream, one `dv: sourceInfo contents.DolbyHdrInfo …` line in the event log, and a picture
  only a human can grade. The dual-layer P7 is refused either way. `metadata::Dovi::presentation` is
  the single predicate behind both the gate and the node; `base_layer_unusable` is now specifically
  the COPY question (a remux carries no declaration), which is why `no_video_copy` still rides a
  declared P5 that ends up on the transcode branch for some other reason.
