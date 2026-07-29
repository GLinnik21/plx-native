# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A **real, native Plex client for LG webOS 4.5 TVs** — built toward production quality, not a
throwaway. **Build proper, reusable, well-factored components and finish them** — a shortcut is
never justified by "it's only a demo." See `rust-modules/src/ui/CLAUDE.md` for how the UI is
expected to be built. It's cross-compiled from macOS and sideloaded onto a rooted 32-bit ARM TV,
renders an Apple-TV-style gallery/shelf UI with SDL2 + OpenGL ES 2, and plays video from a Plex
Media Server (PMS) entirely in-app.

**Almost everything is Rust** in `rust-modules/src/` (UI, event loop, input, player orchestration,
the streaming/demux pipeline, and the Plex data layer), compiled to a static lib and linked in
(see the Makefile). Only two things stay C: `src/main.c` — a small **boot shim** (the async-
signal-safe crash tracer, the event-log handle, stderr capture, process bring-up) that then calls
the Rust `plex_run()` — and `src/starfish.c`, the **StarfishMediaAPIs C++/ACB seam** (`src/svg.c`
is the third C file, the nanosvg rasterizer).

Target device (per `Makefile`/memory): LG 49SM9000PLA, webOS 4.5, rooted, `root@192.168.0.114`
(ssh password `alpine`, already committed in the Makefile). App id `com.beb.plxnative`.

## Build / deploy / run

The `Makefile` is the entire dev loop. Requires the **webOS NDK** (install with `make
setup-env`), a **Rust nightly toolchain + `rust-src`** (for `-Z build-std`), and `sshpass`
(Homebrew, for deploy/run). See the **`setup-environment` skill** (`.claude/skills/`) for the
full one-time setup + troubleshooting.

- `make setup-env` — download + extract + `relocate-sdk.sh` the webOS NDK into `$(WEBOS_SDK)`
  (default `~/webos-ndk/…`). One-time; re-run `relocate-sdk.sh` if you move the SDK.
- `make` — build `pkg/plxnative` (the ARM binary). Also builds the FFmpeg/curl stub `.so` if stale.
- `make deploy` — scp the binary + `appinfo.json` (+ fonts if missing) to the TV app dir.
- `make run` — close any running instance, wipe `/tmp/plxnative-events.log`, launch, keep alive
  `RUN_SECS` (default 18s), then `cat` the on-device event log back to your terminal.
- `make test` — `deploy` then `run` (the normal iteration command).
- `make kill` — close the app on the TV.
- `make ipk` — repackage the installable `pkg/com.beb.plxnative_0.1.0_arm.ipk`.
- Override the TV IP with `make TV=1.2.3.4 …`; the run duration with `make run RUN_SECS=30`.

**Cross-compile toolchain:** the webosbrew **native-toolchain** buildroot NDK —
`arm-webos-linux-gnueabi-gcc` (GCC 12, **glibc 2.12, armv7-a soft-float**; default `cortex-a9`
codegen, so we do *not* pin `-mcpu`). It ships a **sysroot** with the TV's own SONAME'd libs,
which the Makefile links against. Rust is a static lib built with plain `cargo +nightly build -Z
build-std --target arm-unknown-linux-gnueabi` (a staticlib needs no linker, so no external
cross-linker — but `-Z build-std` + `-C target-cpu=cortex-a9` is load-bearing: the default
ARMv6 codegen emits the CP15 barrier that SIGILLs on the A53; see the Makefile comment). Headers
come from `include/` (the TV's SDL2 2.0.4-fork headers, kept ahead of the sysroot's newer copies
so we compile against the ABI the TV runs). The ipk uses the NDK's `ar` (GNU format; macOS BSD
`ar` won't work). The old `zig cc` path is gone.

## The stub `.so` linking trick (now just FFmpeg + curl)

Almost everything links against the **real sysroot libraries** (SDL2, SDL2_ttf, GLESv2,
wayland-client, glib-2.0, luna-service2, and even LG's proprietary `libAcbAPI` / `libplayerAPIs`
/ `libpf-1.0` — all bundled in the NDK), so the Starfish/ACB C++ calls get real link-time symbol
checking. Only **two families remain hand-written stubs in `stub/`**, because the sysroot can't
satisfy them: **FFmpeg** (`libavformat/avcodec/avutil` — absent from the sysroot) and **libcurl**
(sysroot ships `libcurl.so.4`, but the TV wants `libcurl.so.5`).

Each stub is a `.c` file of empty symbol bodies, compiled `-fPIC -shared -nostdlib` with
`-Wl,-soname,<the TV's real SONAME>` (e.g. `libavformat.so.57`). At link time it satisfies the
symbols; **at runtime the TV's own real library (matching that SONAME via `DT_NEEDED`) is loaded
instead.** So:

- Adding a call to a new **FFmpeg/curl** function means **adding its symbol to the matching
  `stub/*_stub.c`** or the link fails — only the *name* must match (empty `void foo(void){}`).
- Adding a dependency on any **other** TV library that the **sysroot also has**: link it real
  (add `-l<name>` to `LIBS_REAL`), don't write a stub. Only stub a lib the sysroot lacks or ships
  with a different SONAME than the TV.
- The C++ `StarfishMediaAPIs` methods are still called from C via `extern … __asm__("<mangled
  name>")` declarations (in `src/starfish.c`) — now resolved against the real `libplayerAPIs`.
- The gitignored `sysroot/usr/lib/` (a few real TV `.so` files pulled off the device) is only for
  reference/inspection; the build uses the **NDK's** sysroot, not that directory.

## Runtime architecture (big picture)

Two planes are composited by the TV: the app's **GLES/graphics plane** (UI, drawn by us) sits
over the hardware **VIDEO overlay plane** (decoded frames). The UI plane is made non-opaque so
video shows through.

**UI (the Rust app core — `app.rs`'s event loop, entered via `plex_run()`):** SDL2 window + GLES2
context. All UI is drawn with two tiny shaders — an SDF rounded-rect/triangle shader (cards, focus
glow, HUD widgets, seven-segment FPS) and a text shader that samples SDL2_ttf-rendered glyph
textures (cached by string+size). Critically-damped springs animate focus scale and shelf scroll.
Fonts are `appfont.ttf` / `appfont-bold.ttf` deployed next to the binary. (Wayland surface setup +
input decoding live in `system.rs` / `app.rs`, not the C shim — see the gotchas below.)

**Video playback (summary — `rust-modules/src/player/CLAUDE.md` is the deep-dive: pipeline,
threading model, the Starfish/ACB ABI + bind-order gotchas, seek/PTS rebase. Read it before
touching playback):** LG's in-process **StarfishMediaAPIs** (`libplayerAPIs.so`) in
`BUFFERSTREAM` **buffer-feed** mode, the decoded sink bound to the hardware video plane via
**`libAcbAPI` (ACB)** — in-process is what lets ACB bind the app-owned sink. The media pipeline
is all Rust: `PMS HTTP GET (raw TCP, stream.rs)` → demux (**`ff.rs`, the TV's own libavformat
over a custom AVIO**) → AU queues with backpressure (`aq.rs`) → the pump `Feed()`s the Starfish
pipeline. Two worker threads (demux, media/load) sit beside the main loop, which owns all
ACB/Starfish control calls. Also linked and
used: **libcurl** (`net.rs`) does the plex.tv account/login TLS+DNS that the raw-socket
`stream.rs` can't.

## Key files

- `Makefile` — build/deploy/run/ipk; toolchain, stub SONAME rules, TV ssh creds.
- `src/main.c` — the **boot shim** (crash tracer, event-log/stderr setup, process bring-up); calls
  the Rust `plex_run()`. `src/starfish.c` — the StarfishMediaAPIs C++/ACB seam. `src/svg.c` —
  nanosvg rasterizer. These three are the *entire* C side.
- `rust-modules/src/` — the app core (Rust): `app.rs` (event loop/input), `system.rs` (wayland),
  `player/` (buffer-feed engine + worker threads — **`rust-modules/src/player/CLAUDE.md` is the
  playback deep-dive; read it before touching playback**), `ff.rs` (THE demuxer — the TV's own
  libavformat), `stream.rs`/`aq.rs` (HTTP socket → AU pipeline), `net.rs` (libcurl/TLS), and the
  Plex data layer.
- `rust-modules/src/ui/` — **the UI, as a shared design system**: `theme.rs` tokens, the retui core
  (`mod.rs` `Painter`/`View`), reusable components (`widgets.rs`/`table.rs`/`label.rs`/`icons.rs`),
  and the screens (`home.rs`/`detail.rs`/`player_hud.rs`/…). **`rust-modules/src/ui/CLAUDE.md` is the
  contribution guide — read it before touching UI: use tokens + components, never inline colors,
  never raw font sizes (ALL text in the UI takes its size from the `theme::size` token scale — add
  a documented rung when a new role needs one), never hand-place text.** Full design/status:
  `docs/ui-system-migration.md`.
  (`rust-modules/src/stream.rs` — blocking HTTP/1.1 GET over a raw TCP socket: numeric IP,
  Content-Length/close delimited, **no chunked decoding**, no DNS. `aq.rs` — one-producer/
  one-consumer AU FIFO with byte-cap backpressure. Both are Rust ports of the deleted C headers;
  the hand-rolled `mkv.rs` demuxer they fed is retired — `ff.rs` is the only demux path.)
- `stub/*.c` + `stub/*.so` — link-time symbol stubs carrying the TV's real SONAMEs — now just
  FFmpeg + curl (see above).
- `pkg/` — deployable payload: `appinfo.json` (native app manifest), `plxnative` binary, icons,
  `appfont*.ttf`, and the prebuilt `.ipk`.
- `ipkroot/` — ipk staging (`ctl/control`, `data/`, `debian-binary`); assembled by `make ipk`.
- `tools/capture-screen.sh` — pull the TV screen (incl. video plane) to a local image.
- `tools/netcond.py` — **network-conditioning TCP proxy** (host-side), for the failures a healthy LAN
  cannot produce. Sits between the TV and the PMS (`--listen 32499 --target 127.0.0.1:32400`; the PMS
  runs on the dev Mac) and makes the server misbehave on demand via `/tmp/netcond.mode`:
  `pass` / `stall` (accept, hold open, answer nothing — the case that turns a join into a parked
  frame loop) / `blackhole` / `reject` / `delay:<ms>`. Any mode scopes to matching requests —
  `stall@/:/timeline` freezes the progress reporter while video keeps streaming, which is what makes
  a clean experiment possible. Modes apply to connections ALREADY OPEN, so a POST can be frozen
  mid-flight. Point the app at it by editing `PMS_PORT` in the gitignored `src/config.local.h` and
  `make deploy` (host/port are compiled into `main.c`). **Pick a port Plex is not already on** — it
  binds `127.0.0.1:32401` itself, and the more specific bind wins, so the proxy is silently bypassed.
  Measured with it 2026-07-29: teardown's join of the `/:/timeline` reporter parked the main loop
  **6974 ms**; after moving that join onto the scrobble worker, BACK→teardown is **0.5 s**. NB a
  request occasionally fails through the proxy that succeeds direct (seen once on `POST /playQueues`)
  — confirm any new failure against a direct run before believing it.
- `tools/sockprobe.c` — standalone ARM diagnostic (`make sockprobe`, scp, run, delete) for socket
  semantics **the host suite cannot answer**: `cargo test` runs on Darwin, the app on Linux, and
  they disagree. Measured 2026-07-28: on this kernel `shutdown(2)` **does** abort a `connect(2)`
  in progress (rv=0, handshake dies at once) — which is why `stream.rs::http_open` publishes its fd
  at `socket()`. On Darwin the same call instead makes `connect_timeout` report *success* on a
  socket that never connected. Reach for this before asserting any syscall behaviour from memory.
- `tools/threadprobe.c` — standalone ARM diagnostic (`make threadprobe`, scp, run as root, delete):
  spawns under the app's uid until `pthread_create` refuses. Measured 2026-07-28 — **2 MB stacks
  die at 2043 threads on `RLIMIT_AS` (the full AArch32 4 GB), 256 KB stacks at 3745 on
  `RLIMIT_NPROC` (3746)**, both EAGAIN, against the app's 31 threads at playback peak. Which limit
  binds depends on the stack size; that is why `task::spawn_small` uses 256 KB.
- `docs/pms-api.md` — **verified** PMS REST reference (sections, hubs, metadata, image transcode,
  direct-play URLs, timeline). The authoritative spec for the data layer.
- `docs/buffer-feed-plan.md` — historical design note for the buffer-feed pivot (partly outdated).

## Non-obvious conventions & gotchas (all verified in code)

- **LG's SDL fork has a shifted `SDL_KeyboardEvent`.** `e.key.keysym` is unreliable; the handler
  (`app.rs`, via `rd_u32`) reads raw bytes off the event: `+16` = state (u32), `+20` = webOS keycode
  (u32), `+24` = sym (u32). State low byte = pressed(1)/released(0); bit `0x100` = auto-repeat.
  Magic-Remote buttons are matched by these `wcode`s (e.g. PAUSE=72, PLAY=450, BACK=461/482, stop=413,
  D-pad L/R alt 412/417; the wcodes live in `ui/consts.rs`). Preserve this raw-offset reading if you
  touch input.
- **Starfish/ACB ABI + bind-order rules** (the C++-from-C mangled-symbol seam, `Load` with
  `uid=NULL`, the exact ACB bind sequence, sourceInfo-verbatim, never feed audio to ACB, the
  3-arg taskId ABI) — moved to **`rust-modules/src/player/CLAUDE.md`**; read it before touching
  playback or `src/starfish.c`.
- **Wayland transparency** (`system.rs`)**:** the UI surface is forced to a 32-bit RGBA config and
  made non-opaque by driving the wayland proxy directly (`wl_proxy_marshal(surface, 4, NULL)` =
  set_opaque_region NULL), re-asserted each frame while playing, so the video plane shows through.
  The TV's SDL is 2.0.4 (no transparency hint). (`sys_grab_wayland` also over-allocates the
  `SDL_SysWMinfo` buffer — the fork writes a larger struct than the headers declare.)
- **Deploy uses a tmp+mv dance** (`plxnative.new` → `mv`) so scp succeeds while the old binary is
  still executing (avoids `ETXTBSY`). The TV drops to standby after a few idle minutes, so a deploy
  can die mid-scp — when scripting around `make deploy`, md5-compare local vs on-TV binary after
  (and wake the TV with WoL first).
- **Text/icon crispness contract:** all 1:1-texel content (glyph strings, icon masks) snaps its
  composited origin to whole pixels via `gfx::snap` (a fractional origin + GL_LINEAR smears strokes),
  and fonts open with FreeType **light** hinting (`text.rs::font_at` — the default NORMAL hinting
  lets Arial's bytecode round horizontal bars up a pixel, inverting stem/bar weights). Never snap
  scaled content (posters). Full rationale: the "Rasterization contract" note above `theme.rs`'s
  size ladder; after a font swap re-verify with `tools/font-hint-audit.py` (host-side, freetype-py).
- **SAM keeps stale "running" state after a hard kill**, so a launch is a silent no-op relaunch
  unless you close-first — `make run`/`kill` do the `closeByAppId` first (and `luna-send -i` must
  stay subscribed for the launch to take).
- **App-switch lifecycle (was a black-screen bug), handled in `app.rs`:** the TV sends SDL app
  events — `0x103`/`0x104` (will/did enter **background**) and `0x105`/`0x106` (will/did enter
  **foreground**). On background during playback the loop **suspends the buffer-feed** (preserving the
  session) and drops to Home; on foreground `0x106` it **reloads** and resumes at the saved position
  with a single `Load`. In-app Home/Settings are *overlays* and do **not** fire these — only a real OS
  app-switch does. Preserve the suspend/reload pairing if you touch playback or routing.
- **Crash forensics:** the C tracer (`main.c`) logs the faulting PC + `/proc/self/maps` line, then
  **re-raises to `SIG_DFL`** so the OS/crashd still captures a real backtrace. Two logs: `plxnative-events.log`
  is truncated each launch; **`/tmp/plxnative-crash.log` is append-only and survives the relaunch** — read it
  after a crash+restart. Note pmlog's wall clock is ~3h skewed on this TV, so correlate by **monotonic
  `SDL_GetTicks`** timestamps (and the SAM `exit_status`), not pmlog time.
- Video track is always full-panel `1920x1080`; the UI is authored at a fixed `1920x1080`
  (`SCR_W`/`SCR_H`), no DPI scaling (panel is 1:1 at 1080p).

## Testing / verification (on-device, no host runtime)

There is no host-side test suite — the code only runs on the TV. Verify by observing behavior.
**Wake the TV first** (`wake-tv` skill) — asleep, every assertion fails as "no line found", which
reads exactly like a total regression. The **`tv-session` skill** is the bring-up/observe/drive
loop; **`crash-triage`** handles a death; **`bind-tv-lib-abi`** covers new FFI into the TV's own
libraries. The full on-device suite is `./tests/run.py` (18 cases; `--fps` for the perf gates).

- **Event log:** the app writes `/tmp/plxnative-events.log` on the TV (LS2/ACB/Starfish replies, feed
  stats, seek/bind steps, key raw bytes, crash tracer). `make run` fetches it automatically; it's
  the primary debugging surface. stderr goes to `/tmp/plxnative-stderr.log`.
- **A case's `run_secs` is a CAP, not a runtime.** `tests/run.py` launches via `make run-stream`
  (tail -F over ssh) and re-grades the log as each line arrives, ending the case the moment every
  assertion passes — so a *passing* case costs what it needs. A failing one burns the full
  `run_secs` unless its verdict is already settled (`failed_for_good`: a `Playing error`, or a
  rapid-seek burst that escalated to `reload_at: fresh Load` — lines that never un-appear), since
  every other failure means "not appeared YET", which more time could still fix.
  Sound because assertions are monotone once satisfied, with two ABSENCE-check
  exceptions that can only flip the other way — `no_error` and `op_seek_rapid`'s `reload_at: fresh
  Load`; adding a third means re-reading `stream_case`'s soundness note. `--no-early` restores the
  old fixed window when you want the longer look at a late error. The cap is measured from the
  app's FIRST LOG LINE, not from ssh start, so it keeps meaning app runtime the way
  `make run RUN_SECS=` did. Two more consequences when editing the harness: raising a manifest `run_secs` no longer
  slows the suite down, and **never pre-create the event log on the TV** — the app runs jailed
  under its own uid, so a root-owned file left in place is one it cannot write (log stays 0 bytes,
  every assertion reads as a total regression). `make run` keeps the old sleep-then-`cat` shape
  because the FPS scenes need a fixed sampling window; both share `BOOT_SH`.
- **A case's start position is server state, so the harness resets it every time.** The resume
  point (`viewOffset`) lives on the PMS, outlives the run, and the app's timeline reporter posts
  progress every 10s while playing — so before this was fixed, a case inherited wherever the
  previous case *or the previous run* stopped (`rk=4` is shared by five cases, `rk=1804` by three),
  and `resume_ns` resumes anything past 10s. "Play from the start" was silently a resume test. Now
  `run_case` **always** clears first, then seeds `setup.viewOffset_ms` if the case declares one.
  **The reset must be `/:/unscrobble`** — a `PUT /:/progress?time=0` returns 200 and changes
  nothing (verified live; `time=1` too), which is exactly what makes this look already handled.
  Don't make the reset conditional again to save the pre-seed close: the wandering seek-tier
  failures were this, not the player.
- **The once/sec `FPS=` heartbeat carries `pos=<s>` while frames are presenting** — the same
  `SHARED.playpos_ns` the 10s `/:/timeline` reporter posts, sampled at 1 Hz. The harness grades
  playback progress from it (`progress_secs`), because observing a 15s climb through 10s samples
  costs ~30s of playback. It is gated on `player::is_playing()`, not `is_started()`: a direct-play
  resume does not seed `playpos_ns` (only the transcode branch does), so the pre-roll would log a
  0 and a 0→600 step reads as 600s of "climb" in one second — a false PASS.
- **`tests/run.py` always cleans the TV on exit** — pass, fail, Ctrl-C, `kill`, or crash: it closes
  the app, clears every `/tmp/plxnative-*` trigger **including the injected PMS token**, and reaps
  stray ssh clients. Only the three append-only `*.log` files survive. Nothing did this before
  2026-07-28 except the normal path, so an interrupted run left the app playing (scrobbling a
  resume point the next run then inherited) and a live per-server token in world-readable `/tmp`.
  The teardown is armed at the moment the harness commits to driving the TV, so `--list` and a
  no-match `--filter` still exit without closing an app you are watching.
- **`ps | grep plxnative` finds NOTHING on this TV even while the app is running** — busybox `ps`
  here shows neither the path nor the argv. Use **`pidof plxnative`** (or `fuser <the binary>`) for
  liveness. A liveness check built on `ps` reads exactly like "the app is closed", which will
  cheerfully confirm whatever you were hoping to prove.
- **Screen capture:** `tools/capture-screen.sh [out.png] [DISPLAY|VIDEO|GRAPHIC]` grabs the panel
  output. `DISPLAY` = video plane + UI composited (use this); `VIDEO`-only failing with "no
  signal state" is itself a diagnostic that nothing is decoded on the video plane.
- **Perf gates:** `./tests/run.py --fps` runs the UI-tier FPS regression scenes (floors per scene in
  `tests/manifest.json`; `--fps-player` adds the player tier), asserting the app's once/sec `FPS=`
  heartbeat. For by-hand judder hunts: `/tmp/plxnative-framedrop` logs any frame over 22ms (or over
  N ms — the file's content) with a pump/draw/swap/upload breakdown and adds `worstframe` to the
  heartbeat; `/tmp/plxnative-homeosc` sweeps the grid focus top↔bottom perpetually to reproduce
  scroll judder headlessly.
- **Dev trigger files (read once at boot, on the TV).** There are ~40; this lists the ones worth
  knowing by name. **The catalog is the source, not this list** — get the real one with
  `grep -rhoE '/tmp/plxnative-[a-z0-9]+' rust-modules/src src | sort -u`. Two behaviours bite:
  `make run` clears ONLY the event log (unlike `tests/run.py`, which glob-clears triggers), so a
  by-hand run inherits whatever the last session armed; and any non-DIAG trigger left behind also
  suppresses the who's-watching picker, silently changing which screen you boot to. The
  **`tv-session` skill** drives all of this (clear → arm → launch → assert) and owns the
  screen-to-trigger recipes. Named highlights: `/tmp/plxnative-url` (override the streamed part
  URL), `/tmp/sample.h264` (feed a local raw Annex-B sample instead of streaming),
  `/tmp/plxnative-autoplay` (auto-press OK for headless capture), `/tmp/plxnative-autoseek` (empty =
  one seek to 140s; else a seek script: optional `gap=<ms>` + comma steps, absolute `120` or
  tap-relative `+10`/`-10` — rapid-burst seek testing), `/tmp/plxnative-ptype` (ACB playerType
  bisect knob), and the Library browse set: `/tmp/plxnative-library[=N]` (boot straight into the
  browse grid on section N), `/tmp/plxnative-libosc` (perpetual grid focus sweep), and
  `/tmp/plxnative-libswitch` (cycle every switch: tabs, sort menu, unwatched, filter→genre).
  Remote-driving: `/tmp/plxnative-remote` is **not** a trigger — the app mkfifos and drains it
  every frame on every boot (so it never affects the picker; its DIAG entry is a permanent
  requirement, not an exception). Write key tokens like `down`/`ok`, or pointer clicks `ck:X,Y`
  in authored 1920x1080 coords, and they replay through the real key/pointer handlers;
  `tools/stream-screen.py` is the host driver — its page maps browser clicks on the streamed
  picture to `ck:` tokens (hover is deliberately NOT forwarded — it used to park app focus on a
  tab pill so the next ENTER opened the library). The one real trigger here is
  `/tmp/plxnative-capture[=port]` (the in-app live UI capture stream:
  the app's own GLES frames over TCP :8910 — **UI plane only**, the video overlay is invisible to
  it, so the service capture stays the only way to see real playback. Two hello-selected wire
  modes, **MPEG1-in-TS** (default) and **JPEG/PXFR** (fallback); `stream-screen.py --source
  app|auto` consumes either and its page switches itself. Both encoders and the measured numbers
  are documented where they live — `capture.rs`'s module doc (slots, wire formats, fd ownership)
  and `ff.rs`'s `venc` section (the device-verified FFmpeg ABI offsets + the RGBA→NV12-NEON
  colorspace path). `make deploy` also ships the NDK's NEON libjpeg-turbo next to the binary
  best-effort, which JPEG mode dlopen's).
  **Any `/tmp/plxnative-*` trigger (except the logs/`plxnative-profile`/`plxnative-anim`/
  `plxnative-remote`/`plxnative-capture`) marks the boot as
  automated and suppresses the boot who's-watching picker**, and `/tmp/plxnative-token` beats the
  stored session entirely — so headless runs always land on a deterministic Home.
  `/tmp/plxnative-pickuser=<index>` forces the picker anyway and auto-picks that roster tile.
- **The binary carries NO credentials** (no compiled PMS token, no demo URL). PMS access comes
  from the signed-in session (QR login) or, for automated runs only, `/tmp/plxnative-token` — which
  `tests/run.py` always injects (it reads the owner token from the gitignored
  `src/config.local.h` on the HOST; that macro is never compiled in). An interactive boot with
  no session lands on the QR sign-in screen.
- Normal interactive flow: who's-watching picker (multi-user) → Home; D-pad/pointer to focus a
  card → **OK** opens the detail page → Play starts playback; OK toggles play/pause, LEFT/RIGHT
  scrub-seek, **BACK/Stop** returns.
