---
name: ui-sim
description: >
  Verify a UI or Plex-data-layer change on the macOS desktop simulator — build it, boot it to a
  screen, drive it with keys, screenshot it, and know when the answer does NOT count and you must
  finish on the television. Use whenever a change needs to be SEEN and a TV is not required or not
  free: "check this looks right", "screenshot the detail page", "does the library grid still lay
  out", "iterate on this spacing", "test my change", "run the app locally", "I don't have the TV",
  "someone else is using the TV", or when several agents need to verify work AT THE SAME TIME.
  Also covers what the simulator provably cannot answer — frame rates, text rasterization, video
  playback, the video plane — and hands those to the `tv-session` skill. Prefer this over
  `tv-session` for ordinary UI work; the television is a single shared resource and this is not.
---

# ui-sim — verify UI work on macOS, finish on the TV

`plxnative-sim` is the same application core the television runs, linked against desktop SDL2 and
desktop GL. It draws the real interface against your real Plex Media Server, on this Mac.

**Why it exists:** the TV serializes the entire dev loop. One set, one app instance, and two
`tests/run.py` jobs kill each other's app — so every UI change queues behind every other. Several
simulators run at once, each with its own instance root, so parallel agents never collide.

**It is not a replacement for the device.** See "What does not count" before reporting any result.

## Build and boot

```sh
make sim                       # build (cargo, ~seconds; no NDK, no cross toolchain)
make sim-token                 # stage the PMS token into the instance root (once per root)
make sim-shot                  # headless: boot, settle, write ONE png, exit
make sim-run                   # interactive: opens a window, Ctrl-C to quit
```

**Ask for `SIM_W=1920 SIM_H=1080` on any shot you intend to JUDGE.** The window is otherwise sized
to fit the display (`app.rs::desktop_window_size`), and on a 1x screen that divisor lands on 2, so
every screenshot comes back 960x540 — half the canvas the UI is authored at. A 1px edge-sheen, a
hairline, a snapped glyph and a specular rim are exactly the things that do not survive that
halving, which makes a shot at the default size evidence about layout and nothing else. The window
may then be larger than the display; for a headless grab that is fine, because the drawable is the
window's own framebuffer, not the part of it a compositor happens to show.

```sh
make sim-shot SIM_DIR=$D SIM_W=1920 SIM_H=1080 SIM_SHOT=$D/home.png
```

`SIM_DIR` is the **instance root** — the whole point of the design. Give every concurrent
simulator its own:

```sh
make sim-shot SIM_DIR=/tmp/sim-a SIM_SHOT=/tmp/sim-a/home.png
make sim-shot SIM_DIR=/tmp/sim-b SIM_SHOT=/tmp/sim-b/home.png     # safe, simultaneously
```

Inside a root live that instance's dev triggers, its remote FIFO and its event log — the same
names and the same contents as on the TV, so **every `tv-session` recipe transfers verbatim**.
With no `SIM_DIR` the root is `/tmp`, exactly as on the device.

`SIM_PMS`/`SIM_PORT` default to `src/config.local.h`. The host must be a **numeric IP**:
`stream.rs` has no DNS resolver here either.

**A checkout on a mounted volume needs `SIM_TDIR`.** Network shares, SMB mounts and some external
disks do not implement `flock`, and a cargo target dir on one fails before compiling anything —
`could not create session directory lock file (os error 45)`. The message blames incremental
compilation, but the cause is the filesystem. Point the build somewhere local and leave the
checkout where it is:

```sh
export SIM_TDIR=$HOME/plxnative-sim-target      # SIM_BIN follows it
```

Only the simulator is rescuable this way. `make` and `make check` build under
`rust-modules/$(RUST_TDIR)`, which is rooted inside the checkout by construction, so the ARM build
and the host suite still need the repo on a local filesystem. That is usually fine — the whole
point of the simulator is that it needs neither.

### What a second machine needs

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

Identical to the TV: arm a trigger in the instance root. The catalog is the source —
`grep -rhoE '/tmp/plxnative-[a-z0-9]+' rust-modules/src src | sort -u` — and `tv-session` owns the
screen-to-trigger recipes.

```sh
touch $SIM_DIR/plxnative-library            # boot into the browse grid
echo 3 > $SIM_DIR/plxnative-library         # ...on section 3
make sim-shot SIM_DIR=$SIM_DIR
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
the location — the `shot` token works in any session without it, including `make sim-run`. Then look at them — that is the whole point; a shot
nobody opens has verified nothing.

Tokens: `up`/`down`/`left`/`right`, `ok`, `back`, `play`/`pause`/`stop`, `ck:X,Y` (clicks in
authored 1920×1080 coords), `okdown`/`okup` (split halves — the only way to drive a press-and-hold
past `press::LONG_MS`), and `shot` (simulator only).

A desktop keyboard also works directly in `make sim-run`: arrows, RETURN, ESC (`is_ok`/`is_back`
have always accepted keyboard keys), plus space=pause, `p`=play, `s`=stop, backspace=BACK.

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

## What the simulator ANSWERS

Layout and spacing · focus and navigation · route transitions and the page cross-fade · every
screen (home, library grid, detail, person, menus, popovers, the failure read-out) · the entire
Plex data layer against a real PMS — browse, metadata, seasons, cast, images, sort/filter · idle
and repaint behaviour · anything reachable by keys or clicks.

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
- **Anything about video.** There is no playback: the 29-symbol Starfish/ACB seam does not exist
  off-device, and `player::ffi`'s host arm reports the seam's own "no video path" failure. Pressing
  Play lands on the app's real failure read-out — which is correct behaviour, not a bug, and is
  also a convenient way to look at that screen.
- **The video plane and UI transparency.** The wayland non-opaque trick is webOS-only.
- ~~**plex.tv sign-in.**~~ **This one is FIXED as of 2026-08-16 and is no longer a limitation.**
  The candidate list gained macOS's `libcurl.4.dylib` (which the dyld shared cache answers with no
  install), and `dynlib!` learned to bind a variadic C function correctly — Apple's ARM64 ABI puts
  variadic arguments on the stack, so the old non-variadic `curl_easy_setopt` binding SIGSEGV'd
  inside libcurl the instant it could open one. The QR flow, discovery and the who's-watching
  roster all run here now. `make sim-token` is still the faster way into a known server, and is
  what headless recipes should keep using.
- **Anything that smelled like a platform difference.** If a bug appears only in the simulator,
  suspect the simulator first — the seam is real code and it has had real bugs (a missing core-
  profile VAO drew nothing at all; a synthetic key layout mismatch swallowed every FIFO token).

## The expected workflow

1. Iterate on the simulator: change → `make sim-shot` → **look at the image** → repeat. No TV, no
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
- No video means no `player` route to screenshot beyond the failure read-out and the HUD's busy
  states.
