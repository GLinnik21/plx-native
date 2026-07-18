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

**Video playback** uses LG's in-process **StarfishMediaAPIs** (`libplayerAPIs.so`) in
`BUFFERSTREAM` **buffer-feed** mode, with the decoded sink bound to the hardware video plane
via **`libAcbAPI` (ACB = App Common Binding)**. The pipeline runs *in our process* so ACB can
bind the app-owned sink — the earlier URI/out-of-process path (`com.webos.media/load`,
`start_playback()`) could not, and is kept only as dead-ish reference. `docs/buffer-feed-plan.md`
records why the pivot happened (it predates the working MKV path — treat it as history, not spec).

**Media pipeline (in-app, all Rust — the `player/` engine + `stream.rs`/`ff.rs`/`aq.rs`):**
`PMS HTTP GET (raw TCP socket, stream.rs)` → `demux` → `access-unit queue with backpressure
(aq.rs)` → the pump `Feed()`s each AU to the Starfish pipeline. The demuxer emits Annex-B video
AUs (param sets prepended at each keyframe) and raw AC3/EAC3/AAC audio frames. **The default
demuxer is the TV's FFmpeg** (`ff.rs`, libavformat, via a custom AVIO over `stream.rs`); the
hand-rolled Matroska demuxer (`mkv.rs`) is the **live fallback**, selected with the
`/tmp/plxnative-demux=mkv` dev trigger, until it's retired (see `docs/ffmpeg-demuxer-plan.md`).
Also linked and used: **libcurl** (`net.rs`) does the plex.tv account/login TLS+DNS that the
raw-socket `stream.rs` can't.

**Threads (spawned by `player/engine.rs`; the pump is `player/pump.rs`, shared state is
`player/shared.rs` — the old C `g_*` volatiles):**
- **Main loop** — input, springs, draw, and the pump (drives ACB bind → Play → feed, and handles
  seeks). All ACB/Starfish control calls happen here.
- **Demux thread** (`threads::stream_thread`) — opens the part URL, runs the demuxer (`ff.rs` by
  default, `mkv.rs` fallback), pushes AUs to the queue. Loops for seeks: the pump sets the seek
  target and closes the socket to interrupt the blocking read; the thread re-opens and resumes
  (mkv fallback: byte `Range:` + resync to the next Cluster).
- **Cue-preflight thread** (`threads::cues_thread`) — a second HTTP connection parses just the MKV
  header to find the Cues element, fetches it by Range, and builds a time→byte index for accurate
  seeks (falls back to a CBR byte estimate until ready).
- **Media/load thread** (`threads::load_thread`) — constructs the Starfish object and calls `Load()`
  (via the `starfish.c` seam), which owns its own GMainContext/loop; callbacks arrive on the
  library's thread.

## Key files

- `Makefile` — build/deploy/run/ipk; toolchain, stub SONAME rules, TV ssh creds.
- `src/main.c` — the **boot shim** (crash tracer, event-log/stderr setup, process bring-up); calls
  the Rust `plex_run()`. `src/starfish.c` — the StarfishMediaAPIs C++/ACB seam. `src/svg.c` —
  nanosvg rasterizer. These three are the *entire* C side.
- `rust-modules/src/` — the app core (Rust): `app.rs` (event loop/input), `system.rs` (wayland),
  `player/` (buffer-feed engine + worker threads), `ff.rs` (the default libavformat demuxer),
  `stream.rs`/`mkv.rs`/`aq.rs` (HTTP socket → fallback MKV demux → AU pipeline, ports of the old
  C headers), `net.rs` (libcurl/TLS), and the Plex data layer.
- `rust-modules/src/ui/` — **the UI, as a shared design system**: `theme.rs` tokens, the retui core
  (`mod.rs` `Painter`/`View`), reusable components (`widgets.rs`/`table.rs`/`label.rs`/`icons.rs`),
  and the screens (`home.rs`/`detail.rs`/`player_hud.rs`/…). **`rust-modules/src/ui/CLAUDE.md` is the
  contribution guide — read it before touching UI: use tokens + components, never inline colors,
  never raw font sizes (ALL text in the UI takes its size from the `theme::size` token scale — add
  a documented rung when a new role needs one), never hand-place text.** Full design/status:
  `docs/ui-system-migration.md`.
  (`rust-modules/src/stream.rs` — blocking HTTP/1.1 GET over a raw TCP socket: numeric IP,
  Content-Length/close delimited, **no chunked decoding**, no DNS. `mkv.rs` — streaming Matroska
  demuxer → H264 Annex-B AUs + raw audio, parsing SeekHead/Cues for the seek index; H264
  `V_MPEG4/ISO/AVC`, unlaced blocks. `aq.rs` — one-producer/one-consumer AU FIFO with byte-cap
  backpressure. These three are Rust ports of the deleted C `stream.h`/`mkv.h`/`aq.h`.)
- `stub/*.c` + `stub/*.so` — link-time symbol stubs carrying the TV's real SONAMEs — now just
  FFmpeg + curl (see above).
- `pkg/` — deployable payload: `appinfo.json` (native app manifest), `plxnative` binary, icons,
  `appfont*.ttf`, and the prebuilt `.ipk`.
- `ipkroot/` — ipk staging (`ctl/control`, `data/`, `debian-binary`); assembled by `make ipk`.
- `tools/capture-screen.sh` — pull the TV screen (incl. video plane) to a local image.
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
- **C-from-C++ Starfish calls** go through `extern … __asm__("<mangled>")`. The object is an
  over-sized static buffer (`g_smp[65536]`) constructed in place by calling the ctor symbol —
  **never** hand it to C++ `new`/`delete` (real object size is unknown). Methods returning a
  `std::string` use a hidden sret first-arg; read the `char*` at offset 0 (SSO) for short replies
  like `"Ok"`/`"BufferFull"`.
- **Starfish `Load` must be constructed with `uid = NULL`** (`SMP_ctor(g_smp, NULL)`), and in
  buffer-feed mode the app must **not** `LSRegister` its own `com.webos.media` client — either
  collides with the pipeline's uMS connection (CONN_FIND_ERR). See the comment in `load_thread`.
- **ACB bind order matters** (mirrors Kodi/ss4s): `setSinkType(MAIN)` → `setMediaId` →
  `setState(LOADED)` → *wait for decoded frames* → `setMediaVideoData(<sourceInfo envelope
  VERBATIM>)` → `setDisplayWindow` → `setState(PLAYING)`. The payload passed to
  `setMediaVideoData` is the **whole `sourceInfo` envelope** captured verbatim from the pipeline's
  callback (`sourceInfoRaw`), not a reconstructed one. Audio is owned by the pipeline — **never**
  feed audio to ACB (causes SOUND_ERROR_019). `AcbAPI_setMediaVideoData`/`setState`/
  `setDisplayWindow` take a `long *taskId` out-param as their last arg — the 3-arg ABI is required
  (2-arg calls corrupt memory / segfault).
- **Wayland transparency** (`system.rs`)**:** the UI surface is forced to a 32-bit RGBA config and
  made non-opaque by driving the wayland proxy directly (`wl_proxy_marshal(surface, 4, NULL)` =
  set_opaque_region NULL), re-asserted each frame while playing, so the video plane shows through.
  The TV's SDL is 2.0.4 (no transparency hint). (`sys_grab_wayland` also over-allocates the
  `SDL_SysWMinfo` buffer — the fork writes a larger struct than the headers declare.)
- **MKV seeking** re-opens the HTTP stream at a byte offset from the Cue index and resyncs to the
  next Cluster (clusters start on a keyframe). The fed PTS timeline is rebased on the first
  post-seek keyframe so the pipeline never sees a jump (`g_pts_shift`, `g_rebase_pending`).
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

There is no host-side test suite — the code only runs on the TV. Verify by observing behavior:

- **Event log:** the app writes `/tmp/plxnative-events.log` on the TV (LS2/ACB/Starfish replies, feed
  stats, seek/bind steps, key raw bytes, crash tracer). `make run` fetches it automatically; it's
  the primary debugging surface. stderr goes to `/tmp/plxnative-stderr.log`.
- **Screen capture:** `tools/capture-screen.sh [out.png] [DISPLAY|VIDEO|GRAPHIC]` grabs the panel
  output. `DISPLAY` = video plane + UI composited (use this); `VIDEO`-only failing with "no
  signal state" is itself a diagnostic that nothing is decoded on the video plane.
- **Perf gates:** `./tests/run.py --fps` runs the UI-tier FPS regression scenes (floors per scene in
  `tests/manifest.json`; `--fps-player` adds the player tier), asserting the app's once/sec `FPS=`
  heartbeat. For by-hand judder hunts: `/tmp/plxnative-framedrop` logs any frame over 22ms (or over
  N ms — the file's content) with a pump/draw/swap/upload breakdown and adds `worstframe` to the
  heartbeat; `/tmp/plxnative-homeosc` sweeps the grid focus top↔bottom perpetually to reproduce
  scroll judder headlessly.
- **Dev trigger files (read once at boot, on the TV):** `/tmp/plxnative-url` (override the streamed part
  URL), `/tmp/sample.h264` (feed a local raw Annex-B sample instead of streaming),
  `/tmp/plxnative-autoplay` (auto-press OK for headless capture), `/tmp/plxnative-autoseek` (one auto-seek),
  `/tmp/plxnative-demux=mkv` (fall back to the mkv.rs demuxer), and `/tmp/plxnative-ptype` (ACB
  playerType bisect knob).
  **Any `/tmp/plxnative-*` trigger (except the logs/`plxnative-profile`/`plxnative-anim`) marks the boot as
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
