---
name: ui-sim
description: >
  Verify UI and Plex data-layer changes in the macOS or Windows/WSLg desktop simulator. Use it for
  layout, focus, navigation, screenshots, local app runs, and simulator-based validation when a TV
  is unavailable or unnecessary. It also defines which results still require `tv-session`, including
  TV frame rate, LG decoding, text rasterization, and the video plane.
---

# ui-sim — verify UI work on a desktop, finish on the TV


> **This is also what you do while the television is locked.** One set, one lane at a time
> (`tools/tv-lock.sh`, the **`tv-lock`** skill): when a device command is refused because another
> lane holds it, the simulator is usually the answer rather than the queue — N instances run at
> once, each with its own `PLXNATIVE_RUNTIME_DIR`. Come back to the TV only for what the simulator
> provably cannot answer (frame rate, text rasterization, LG's decoder, the video plane) — a
> shorter list on macOS since the streaming pipeline moved onto that simulator on 2026-08-28.
> Windows/WSLg's UI-only path stops at the existing host no-video seam.

`plxnative-sim` is the same application core the television runs, linked against desktop SDL2 and
desktop GL. It draws the real interface against your real Plex Media Server on macOS or through
WSLg on Windows.

**Why it exists:** the TV serializes the entire dev loop. One set, one app instance, and two
`tests/run.py` jobs kill each other's app — so every UI change queues behind every other. Several
simulators run at once, each with its own instance root, so parallel agents never collide.

**It is not a replacement for the device.** See "What does not count" before reporting any result.

## Build and boot

```sh
make sim-macos                 # build with host FFmpeg (cargo; no NDK or cross toolchain)
make sim-macos-token           # stage the PMS token into the instance root (once per root)
make sim-macos-shot            # headless: boot, settle, write ONE png, exit
make sim-macos-run             # interactive: opens a window, Ctrl-C to quit
```

The historical `sim`, `sim-token`, `sim-shot`, and `sim-run` names remain macOS aliases.

### Windows 11 through WSLg

The Windows entry point is `tools/sim.ps1`. It builds an optimized UI/Plex simulator inside the
existing Ubuntu 22.04 WSL distribution, keeps Cargo output and runtime state on WSL's Linux
filesystem, and opens the SDL/OpenGL window through WSLg:

```powershell
powershell -ExecutionPolicy Bypass -File tools/sim.ps1 setup
powershell -ExecutionPolicy Bypass -File tools/sim.ps1 run
powershell -ExecutionPolicy Bypass -File tools/sim.ps1 run -StageToken
powershell -ExecutionPolicy Bypass -File tools/sim.ps1 shot -Output .\shot.png
powershell -ExecutionPolicy Bypass -File tools/sim.ps1 send right ok shot
```

`setup` installs stable Rust, SDL2/SDL2_ttf, Mesa OpenGL and diagnostics in WSL and refuses a
software renderer. `run` and `shot` default to a 1920×1080 drawable, request vsync, and apply a
60 Hz host frame cap because WSLg's X11/GLX swap does not block on that request. They use
`~/.cache/plxnative-sim/target`, `~/.local/state/plxnative-sim`, and an isolated asset directory;
override those with `-TargetDir`, `-RuntimeDir`, and `-AssetDir`; quoted paths containing spaces are
supported. These three overrides are absolute Linux paths inside WSL, keeping Cargo and runtime
files off the Windows-mounted checkout. `-StageToken` copies the token from gitignored
`src/config.local.h` without printing it.
The staged token persists in that runtime directory, so use a fresh `-RuntimeDir` for QR sign-in
after staging one. `-PmsHost` overrides the header's PMS host.

This path deliberately calls `make sim-wsl`, the Windows-facing compatibility alias for the native
Linux `make sim-linux` target. Both skip bundled host FFmpeg. UI, sign-in, Plex browsing,
screenshots and remote commands work; playback reaches the existing host "no video path" result.
Use macOS `make sim-macos` when the demux/clock-sink simulation is required.

Check `glxinfo -B` before interpreting local smoothness: `Device: D3D12 (<GPU>)` and
`Accelerated: yes` prove WSLg is using the GPU; `llvmpipe`, `softpipe`, or `Accelerated: no` do not.
The result is still host evidence and never a television FPS result.

Also check the last `RDP backend: use_gfxredir` line in `/mnt/wslg/weston.log`. `= 0` means WSLg
lost its shared-memory graphics transport and fell back to copying the window through RDP: the app
can report 60 swaps while Windows visibly updates at only a few FPS. The launcher now refuses that
state. Close other WSL work, run `wsl.exe --shutdown`, and retry; accept the run only after it says
`use_gfxredir = 1`.

**Ask for `SIM_W=1920 SIM_H=1080` on any shot you intend to JUDGE.** The window is otherwise sized
to fit the display (`app::boot::desktop_window_size`), and on a 1x screen that divisor lands on 2, so
every screenshot comes back 960x540 — half the canvas the UI is authored at. A 1px edge-sheen, a
hairline, a snapped glyph and a specular rim are exactly the things that do not survive that
halving, which makes a shot at the default size evidence about layout and nothing else. The window
may then be larger than the display; for a headless grab that is fine, because the drawable is the
window's own framebuffer, not the part of it a compositor happens to show.

```sh
make sim-macos-shot SIM_DIR=$D SIM_W=1920 SIM_H=1080 SIM_SHOT=$D/home.png
```

`SIM_DIR` is the **instance root** — the whole point of the design. Give every concurrent
simulator its own:

```sh
make sim-macos-shot SIM_DIR=/tmp/sim-a SIM_SHOT=/tmp/sim-a/home.png
make sim-macos-shot SIM_DIR=/tmp/sim-b SIM_SHOT=/tmp/sim-b/home.png # safe, simultaneously
```

Inside a root live that instance's dev triggers, its remote FIFO and its event log — the same
names and the same contents as on the TV, so **every `tv-session` recipe transfers verbatim**.
With no `SIM_DIR` the root is `/tmp/plxnative-sim` (the Makefile's default); `/tmp` is the *device's* root, not this one's.

**The flavour axis does not reach here.** The TV now carries three installs with three runtime
roots, so `tv-session` recipes name theirs via `make -s print-rundir FLAVOR=…`; an explicit
instance root outranks all of that in `paths::resolve_runtime_dir`, so the simulator is steered
by `SIM_DIR` alone and takes no `FLAVOR`. Translate a device recipe by substituting `$SIM_DIR`
for whatever runtime root it names — the trigger and log FILE names are identical either way,
which is what makes the recipes transfer at all.

`SIM_PMS`/`SIM_PORT` default to `src/config.local.h`. The host must be a **numeric IP**:
`stream.rs` has no DNS resolver here either.

**A checkout on a mounted volume needs `SIM_TDIR`.** Network shares, SMB mounts and some external
disks do not implement `flock`, and a cargo target dir on one fails before compiling anything —
`could not create session directory lock file (os error 45)`. The message blames incremental
compilation, but the cause is the filesystem. Point the build somewhere local and leave the
checkout where it is:

```sh
export SIM_TDIR=$HOME/plxnative-sim-target      # print-simbin follows it
```

Only the simulator is rescuable this way. `make` and `make check` build under
`rust-modules/$(RUST_TDIR)`, which is rooted inside the checkout by construction, so the ARM build
and the host suite still need the repo on a local filesystem. That is usually fine — the whole
point of the simulator is that it needs neither.

### What a second Mac needs

On Windows, run `tools/sim.ps1 setup`; use `-PmsHost` for an explicit server and `-StageToken`
when credentials should be copied from `src/config.local.h`.

Three things, and notably no webOS NDK and no nightly:

1. `brew install sdl2_ttf` — pulls `sdl2-compat`. These are the ONLY non-system dynamic
   dependencies (`otool -L` shows just those two plus OpenGL/iconv/libSystem), and they are linked
   by absolute Homebrew path, so a copied binary will not run on a machine without them. Build on
   the machine rather than copying: `build.rs` asks `brew --prefix`, so it is also correct on an
   Intel Mac where Homebrew lives at `/usr/local`.
2. **rustup + stable.** Not nightly. `rust-modules/.cargo/config.toml` carries `[unstable]
   build-std`, which looks like it forces nightly, but that table is gated and stable cargo ignores
   it — it only applies to the ARM cross-build, which passes `cargo +nightly` explicitly.
3. `src/config.local.h` for the PMS host and token. Gitignored, so a fresh clone has none: pass
   `SIM_PMS=<ip>` and write the token into `$SIM_DIR/plxnative-token` by hand.

## Boot into a specific screen

Identical to the TV: arm a trigger in the instance root. The catalog is the source, and the
command that produces it lives in `docs/agent-reference.md` (the "The catalog is the source, not this list"
bullet) — use that one, not a copy: it has **two** greps, because a single path grep silently
under-reports the four triggers named nowhere but their `dev::flag`/`dev::read` call. `tv-session`
owns the screen-to-trigger recipes.

```sh
touch $SIM_DIR/plxnative-library            # boot into the browse grid
echo 3 > $SIM_DIR/plxnative-library         # ...on section 3
make sim-macos-shot SIM_DIR=$SIM_DIR
```

Stale triggers change which screen you boot to, silently — same trap as on the device. `make
sim-clean SIM_DIR=…` resets a root.

## Drive it, then screenshot

Run it in the background, write tokens to the FIFO, ask for a shot:

```sh
PLXNATIVE_RUNTIME_DIR=$D PLXNATIVE_APP_DIR=$PWD/pkg \
  rust-modules/target-sim/debug/plxnative-sim "$PMS_IP" 32400 >/dev/null 2>&1 &   # numeric IP
sleep 7                                     # let Home land and posters arrive

exec 3<> $D/plxnative-remote                # SEE THE WARNING BELOW — `<>`, never `>`
printf 'shot ' >&3;  sleep 2                # -> shot-1.png
for t in down down right ok; do printf '%s ' "$t" >&3; sleep 0.9; done
sleep 3; printf 'shot ' >&3; sleep 2        # -> shot-2.png
exec 3>&-
```

Shots land in the instance root as **numbered** files (`shot-1.png`, `shot-2.png`, …) so a
sequence never overwrites one file or races whoever is reading it. `PLXNATIVE_SHOT` only overrides
the location — the `shot` token works in any session without it, including `make sim-macos-run`.
Then look at them — that is the whole point; a shot
nobody opens has verified nothing.

Tokens: `up`/`down`/`left`/`right`, `ok`, `back`, `play`/`pause`/`stop`, `ck:X,Y` (clicks in
authored 1920×1080 coords), `okdown`/`okup` (split halves — the only way to drive a press-and-hold
past `press::LONG_MS`), and `shot` (simulator only).

A desktop keyboard also works directly in `make sim-macos-run`: arrows, RETURN, ESC (`is_ok`/`is_back`
have always accepted keyboard keys), plus space=pause, `p`=play, `s`=stop, backspace=BACK.

**`back` at a ROOT is the platform's root press** (`webos::go_home`) — Home's root, the
who's-watching picker and the QR sign-in — and it does not end the process. **On the simulator it
is a LOG LINE and nothing else** (`gohome: no LS2 bus off-device …`): there is no webOS launcher to
hand a Mac window to, so the screen does not change and a `back` too many looks like a key that did
nothing. That is the one part of this behaviour the simulator is structurally blind to; whether the
television really shows its launcher and brings the SAME PROCESS back is a `tv-session` question —
and note the contract is the process, not the screen: workers keep running while backgrounded, so a
sign-in may legitimately have advanced by the time the app is foregrounded again.

### Three traps, each of which has already cost an hour

1. **Open the FIFO read-write.** `printf x > $D/plxnative-remote` blocks forever in `open(2)` if
   the app is not running — there is no reader, and the shell hangs with no output. Always
   `exec 3<> fifo` and write to `&3`.
2. **A settled screen stops presenting.** `ui::idle` skips the whole swap once nothing moves, so
   anything depending on a frame must invalidate first. The `shot` token does this for you; if you
   add another such path, call `ui::idle::invalidate()` or it will wait for a frame that never
   comes.
3. **Give the app time before driving.** Posters and hub data arrive asynchronously; a shot at 2 s
   catches a half-built screen and looks like a layout bug.

## Documentation screenshots

`make screenshots` regenerates every image under `docs/screenshots/` from a committed scene
manifest, against the mock server's demo library — no Plex account, no television, no gitignored
file:

```sh
make demo-library                     # fetch (sha256-pinned, ~120 MB once) + derive; screenshots runs it too
make screenshots                      # build the sim, render every scene into docs/screenshots/
make screenshots SCENES=home,ux-detail.jpg OUT=/tmp/shots   # a subset, somewhere else
make screenshots CHECK=1              # render each scene twice; fail unless within its bound
make screenshots HERO=sintel          # pin another film as the home hero for this run
make screenshots HERO_VARIANTS=1      # also home-hero-<film>.jpg for each hero candidate
```

- **Scenes are target STATES, reached by triggers.** `tests/screenshots/scenes.json` names each
  state, the triggers that reach it, the event-log lines that prove it was reached (`expect`)
  and the files it becomes. No key is sent. `tools/screenshots.py` arms `token`, `plextv` (plex.tv
  replaced by the mock) and `stillclock` itself on every scene, and refuses a run whose log shows
  a `BADTRIGGER`, a trigger that gave up, or a request the mock could not answer.
- **The capture is the app's, once the screen is at rest.** `PLXNATIVE_SHOT_SETTLE=<ms>` makes the
  simulator write one PNG after the screen has not changed for that long (and not before
  `PLXNATIVE_SHOT_AFTER`), then exit with `PLXNATIVE_SHOT_EXIT=1`. The driver waits on that
  process, under a ceiling timeout; there is no sleep anywhere. A screen that never comes to rest
  is a failure, not a capture. A popover freezes the page under it, so a menu scene opens its menu
  only after the page has rested (`acct=<ms>`, `libmenu=<kind>,<ms>`).
- **Everything that moves is pinned.** The mock's clock is the catalog's `now`; watch state,
  progress and added dates come from the catalog; `stillclock` holds free-running animation; the
  hero is pinned to slot 0 (`heropin=0`), and the mock puts the hero film at the head of Continue
  Watching, so its button reads Continue with progress.
- **Determinism.** `CHECK=1` renders every scene twice and compares pixel by pixel: each channel
  may differ by at most `max_delta` (default 1 — the GPU's run-to-run rounding), except inside a
  scene's `free_regions`, which must carry a `tolerance_reason`. Search, detail, sign-in and the
  failure read-out come back byte-identical; scenes with backdrop blur or glass differ by exactly
  1 in anything from a few pixels to ~120k of them. The player scene frees the playhead knob and elapsed-time read-out: the clock
  sink plays in real time until the pause is accepted, so where on the timeline it pauses moves
  by a second or two.
- **The library is openly licensed.** `tests/demo_library/assets.json` pins every source file (URL,
  sha256, licence, author); `catalog.json` is the library. Film title logos are never used (the
  Blender Studio terms reserve its trademarks); the app draws titles as text.
  `python3 tools/demo_library.py credits` rewrites `docs/screenshots/CREDITS.md`, and
  `python3 tools/demo_library.py check` validates both manifests offline. The cache lives outside
  the repository (`$PLXNATIVE_DEMO_CACHE`, default `~/.cache/plxnative-demo`).
- **Review before committing.** Open every image. A regenerated set is committed on its own,
  never in the same commit as a change to the pipeline.

## What the simulator ANSWERS

Layout and spacing · focus and navigation · route transitions and the page cross-fade · every
screen (home, library grid, detail, person, menus, popovers, the failure read-out) · the entire
Plex data layer against a real PMS — browse, metadata, seasons, cast, images, sort/filter · idle
and repaint behaviour · anything reachable by keys or clicks. **Modals are drawn with the frame cache
OFF** (`frame cache: CopyTexSubImage error=0x500 — cache off`), so a sim capture cannot show a
fault in the cached host path a modal card is served from; only the TV can.

That is most UI work, and it is the half that transfers.

## What does NOT count — finish on the television

Report these ONLY from the device, via the **`tv-session`** skill (and `wake-tv` first):

- **Frame rates, always.** The `--fps` gates are calibrated to the SM9000's Mali. A Mac is a
  different GPU, driver and compositor. Every simulator heartbeat carries **`sim=1`** and the log's
  first line says so — if you see either, the number is not a perf result. Never quote `fps=` or
  `loop=` from a simulator as evidence about performance.
- **Text rasterization truth.** Different FreeType, so stem/bar weight, hinting and the
  `theme::size` ladder stay device questions — `tools/font-hint-audit.py` and a device capture.
  The window is no longer at an arbitrary fitted scale (`scale≈0.86` was the old fitted size): it
  opens at an exact divisor of the 1920x1080 canvas and is not resizable, so on a Retina display
  the drawable is 1920x1080 and `scale` is exactly 1.0. Check the `surface:` line — it prints the
  drawable and the scale, and a 0.5 there means glyphs are downscaled and softer by construction.
- **Anything about LG's DECODER** — resource-allocation refusals, the Load payload's Dolby
  declaration, `SOUND_ERROR_019`, frame pacing, which codecs the panel takes. The 29-symbol
  Starfish/ACB seam does not exist off-device, so by default `player::ffi`'s host arm reports the
  seam's own "no video path" failure and pressing Play lands on the app's real failure read-out —
  correct behaviour, not a bug, and a convenient way to look at that screen.
- ~~**Anything about video.**~~ **Narrowed twice, and the second time is recent.** Arm
  **`plxnative-clocksink`** in the instance root and the seam becomes a plant: access units are
  accepted and discarded and a presentation clock advances at real time, clamped to the last fed
  PTS, reporting position at the television's own measured 5 Hz. And since **2026-08-28** the
  source of those AUs can be a real network stream: `make sim-macos` builds a HOST copy of the
  bundled FFmpeg (`HOST=1 ci/build-ffmpeg.sh`, staged into `pkg/` as `libavformat-plx.63.dylib`),
  so `ff.rs` demuxes here. What that makes runnable off-device is everything between the socket
  and the decoder — both AVIO transports, the HLS demux, the AU queues and their byte-cap
  backpressure, the feed-ahead throttle, the ABR controller's rung transactions, seek and PTS
  rebase. A 30 s host run against `tests/serve_fixtures.py` produces `abr:` lines and rung commits.
  **Nothing decodes**, every heartbeat still carries `sim=1`, and no number taken here is a device
  measurement.
- **The video plane and UI transparency.** The wayland non-opaque trick is webOS-only.
- ~~**plex.tv sign-in.**~~ **This one is FIXED as of 2026-08-16 and is no longer a limitation.**
  The candidate list gained macOS's `libcurl.4.dylib` (which the dyld shared cache answers with no
  install), and `dynlib!` learned to bind a variadic C function correctly — Apple's ARM64 ABI puts
  variadic arguments on the stack, so the old non-variadic `curl_easy_setopt` binding SIGSEGV'd
  inside libcurl the instant it could open one. The QR flow, discovery and the who's-watching
  roster all run here now. `make sim-macos-token` is still the faster way into a known server, and is
  what headless recipes should keep using.
- **Anything that smelled like a platform difference.** If a bug appears only in the simulator,
  suspect the simulator first — the seam is real code and it has had real bugs (a missing core-
  profile VAO drew nothing at all; a synthetic key layout mismatch swallowed every FIFO token).

## The expected workflow

1. Iterate on macOS: change → `make sim-macos-shot` → **look at the image** → repeat. On Windows,
   use `tools/sim.ps1 shot`. No TV, no
   queue, parallel-safe.
2. When it looks right, run `make check` (host suite + lint).
3. Then verify once on the device with **`tv-session`** — and run `./tests/run.py --fps` there if
   motion changed. A change is not done until the device has seen it.

Reporting rule: say which of the two produced each claim. "Looks right in the simulator, not yet
device-verified" is a useful, honest status. "Verified" without a TV is not.

## Gotchas that are simulator-specific

- Screenshots are written as **opaque RGB on purpose.** The app renders a non-opaque UI plane so
  the TV's video plane composites through it; carried into a PNG that alpha gets re-interpreted by
  the viewer and the whole interface blooms over a white page. RGB is the faithful image and the
  one comparable to a device capture.
- `screencapture` (the macOS tool) works for a quick manual look but is wrong for automation: it
  needs Screen Recording permission — a GUI prompt that silently yields black frames headlessly —
  and it grabs whatever is actually on screen, so an occluded window or two parallel simulators
  corrupt it. The in-app shot needs no permission, works occluded, and is deterministic.
- The window is not 1:1. `surface::probe` letterboxes 1920×1080 into whatever the drawable is, so
  shots come out at the viewport size (e.g. 1650×928). Fine for layout, wrong for pixel work.
- Without `plxnative-clocksink` there is no `player` route to screenshot beyond the failure
  read-out and the HUD's busy states. With it there is a real one, driven by a real stream — but
  the video PLANE is empty, because the app decodes nothing and the wayland overlay is webOS-only:
  the HUD sits over black. Arm `plxnative-simvideo` as well and a system `ffmpeg` child decodes
  the stream the clock sink accepts and composites it UNDER the UI (`player/sim_video.rs`). That
  is a screenshot facility for the documentation's player figure; it says nothing about LG's
  decoder, the video plane or picture timing on the television.
