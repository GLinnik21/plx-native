# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A **native (C) proof-of-concept Plex client for LG webOS 4.5 TVs**, cross-compiled from
macOS and sideloaded onto a rooted 32-bit ARM TV. It renders an Apple-TV-style gallery/shelf
UI with SDL2 + OpenGL ES 2 and plays video from a Plex Media Server (PMS) entirely in-app.
The whole app is essentially one file: `src/main.c` (~2000 lines) plus three header-only
modules that form the streaming pipeline (`stream.h`, `mkv.h`, `aq.h`).

Target device (per `Makefile`/memory): LG 49SM9000PLA, webOS 4.5, rooted, `root@192.168.0.114`
(ssh password `alpine`, already committed in the Makefile). App id `com.glin.plexpoc`.

## Build / deploy / run

The `Makefile` is the entire dev loop. Requires `zig` and `sshpass` (both via Homebrew here).

- `make` — build `pkg/plexpoc` (the ARM binary). Also builds the stub `.so` files if stale.
- `make deploy` — scp the binary + `appinfo.json` (+ fonts if missing) to the TV app dir.
- `make run` — close any running instance, wipe `/tmp/poc-events.log`, launch, keep alive
  `RUN_SECS` (default 18s), then `cat` the on-device event log back to your terminal.
- `make test` — `deploy` then `run` (the normal iteration command).
- `make kill` — close the app on the TV.
- `make ipk` — repackage the installable `pkg/com.glin.plexpoc_0.1.0_arm.ipk`.
- Override the TV IP with `make TV=1.2.3.4 …`; the run duration with `make run RUN_SECS=30`.

**Cross-compile toolchain:** `zig cc -target arm-linux-gnueabi.2.24 -mcpu=cortex_a53`, headers
from `include/`. `zig ar` is used for the ipk (GNU ar format; macOS BSD `ar` won't work).

## The stub `.so` linking trick (crucial and unusual)

The app links against **hand-written stub shared objects in `stub/`** (`libSDL2.so`,
`libGLESv2.so`, `libplayerAPIs.so`, `libAcbAPI.so`, `libluna-service2.so`, `libglib-2.0.so`,
`libwayland-client.so`, `libSDL2_ttf.so`). Each stub is a `.c` file of empty symbol bodies,
compiled `-shared -nostdlib` with `-Wl,-soname,<the TV's real SONAME>` (e.g. the SDL2 stub
carries `libSDL2-2.0.so.0`). At link time these satisfy the symbols; **at runtime the TV's
own real libraries (matching those SONAMEs via `DT_NEEDED`) are loaded instead.** The real
libs exist *only on the TV* — never on the build host. So:

- Adding a call to a new library function means **adding its symbol to the matching `stub/*.c`**
  or the link fails. For plain C symbols an empty `void foo(void){}` body suffices (never run
  on host); only the *name* must match.
- `stub/starfish_stub.c` carries the **mangled C++ symbols** of LG's `StarfishMediaAPIs` class.
  `main.c` calls those C++ methods from C via `extern … __asm__("<mangled name>")` declarations
  (see the `SMP_*` decls near the top of `main.c`).
- `sysroot/usr/lib/` holds a few *real* TV `.so` files pulled off the device for reference/
  inspection; it is **not** used by the build (the Makefile links `-Lstub` only).

## Runtime architecture (big picture)

Two planes are composited by the TV: the app's **GLES/graphics plane** (UI, drawn by us) sits
over the hardware **VIDEO overlay plane** (decoded frames). The UI plane is made non-opaque so
video shows through.

**UI (`main` loop in `main.c`):** SDL2 window + GLES2 context. All UI is drawn with two tiny
shaders — an SDF rounded-rect/triangle shader (cards, focus glow, HUD widgets, seven-segment
FPS) and a text shader that samples SDL2_ttf-rendered glyph textures (cached by string+size).
Critically-damped springs animate focus scale and shelf scroll. Fonts are `appfont.ttf` /
`appfont-bold.ttf` deployed next to the binary.

**Video playback** uses LG's in-process **StarfishMediaAPIs** (`libplayerAPIs.so`) in
`BUFFERSTREAM` **buffer-feed** mode, with the decoded sink bound to the hardware video plane
via **`libAcbAPI` (ACB = App Common Binding)**. The pipeline runs *in our process* so ACB can
bind the app-owned sink — the earlier URI/out-of-process path (`com.webos.media/load`,
`start_playback()`) could not, and is kept only as dead-ish reference. `docs/buffer-feed-plan.md`
records why the pivot happened (it predates the working MKV path — treat it as history, not spec).

**Media pipeline (in-app, no ffmpeg/libcurl):**
`PMS HTTP GET (raw TCP socket, stream.h)` → `Matroska/MKV demux (mkv.h)` → `access-unit queue
with backpressure (aq.h)` → `bf_feed_stream()` `Feed()`s each AU to the Starfish pipeline.
The demuxer emits H264 Annex-B video AUs (SPS/PPS prepended at each IDR) and raw AC3/EAC3/AAC
audio frames.

**Threads (see `start_bufferfeed` / `stop_bufferfeed`):**
- **Main SDL loop** — input, springs, draw, and `bufferfeed_pump()` (drives ACB bind → Play →
  feed, and handles seeks). All ACB/Starfish control calls happen here.
- **Demux thread** (`stream_thread`) — opens the part URL, runs `mkv_run`, pushes AUs to `g_aq`.
  Loops for seeks: the pump sets `g_seek_byte` and closes the socket to interrupt the blocking
  read; the thread re-opens with a byte `Range:` and resyncs to the next Cluster (`mkv_seek_run`).
- **Cue-preflight thread** (`cues_thread`) — a second HTTP connection parses just the MKV header
  to find the Cues element, fetches it by Range, and builds a time→byte index (`g_cues`) for
  accurate seeks (falls back to a CBR byte estimate until ready).
- **Media/load thread** (`load_thread`) — constructs the Starfish object and calls `Load()`,
  which owns its own GMainContext/loop; callbacks (`starfish_cb`) arrive on the library's thread.

## Key files

- `Makefile` — build/deploy/run/ipk; toolchain, stub SONAME rules, TV ssh creds.
- `src/main.c` — the whole app: GLES2 UI, HUD, input, ACB + Starfish glue, pipeline orchestration.
- `src/stream.h` — blocking HTTP/1.1 GET over a raw TCP socket (numeric IP, Content-Length/close
  delimited; **no chunked decoding**, no DNS).
- `src/mkv.h` — streaming Matroska demuxer → H264 Annex-B AUs + raw audio frames; also parses
  SeekHead/Cues for the seek index. Scope: H264 `V_MPEG4/ISO/AVC`, unlaced blocks.
- `src/aq.h` — one-producer/one-consumer access-unit FIFO with byte-cap backpressure.
- `stub/*.c` + `stub/*.so` — link-time symbol stubs carrying the TV's real SONAMEs (see above).
- `pkg/` — deployable payload: `appinfo.json` (native app manifest), `plexpoc` binary, icons,
  `appfont*.ttf`, and the prebuilt `.ipk`.
- `ipkroot/` — ipk staging (`ctl/control`, `data/`, `debian-binary`); assembled by `make ipk`.
- `tools/capture-screen.sh` — pull the TV screen (incl. video plane) to a local image.
- `docs/pms-api.md` — **verified** PMS REST reference (sections, hubs, metadata, image transcode,
  direct-play URLs, timeline). The authoritative spec for the data layer.
- `docs/buffer-feed-plan.md` — historical design note for the buffer-feed pivot (partly outdated).

## Non-obvious conventions & gotchas (all verified in code)

- **LG's SDL fork has a shifted `SDL_KeyboardEvent`.** `e.key.keysym` is unreliable; the handler
  reads raw bytes off the event: `+16` = state (u32), `+20` = webOS keycode (u32), `+24` = sym
  (u32). State low byte = pressed(1)/released(0); bit `0x100` = auto-repeat. Magic-Remote buttons
  are matched by these `wcode`s (e.g. PAUSE=72, PLAY=450, BACK=461, stop=413, D-pad L/R alt
  412/417). Preserve this raw-offset reading if you touch input.
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
- **Wayland transparency:** the UI surface is forced to a 32-bit RGBA config and made non-opaque
  by driving the wayland proxy directly (`wl_proxy_marshal(surface, 4, NULL)` = set_opaque_region
  NULL), re-asserted each frame while playing, so the video plane shows through. The TV's SDL is
  2.0.4 (no transparency hint).
- **MKV seeking** re-opens the HTTP stream at a byte offset from the Cue index and resyncs to the
  next Cluster (clusters start on a keyframe). The fed PTS timeline is rebased on the first
  post-seek keyframe so the pipeline never sees a jump (`g_pts_shift`, `g_rebase_pending`).
- **Deploy uses a tmp+mv dance** (`plexpoc.new` → `mv`) so scp succeeds while the old binary is
  still executing (avoids `ETXTBSY`).
- **SAM keeps stale "running" state after a hard kill**, so a launch is a silent no-op relaunch
  unless you close-first — `make run`/`kill` do the `closeByAppId` first (and `luna-send -i` must
  stay subscribed for the launch to take).
- Video track is always full-panel `1920x1080`; the UI is authored at a fixed `1920x1080`
  (`SCR_W`/`SCR_H`), no DPI scaling (panel is 1:1 at 1080p).

## Testing / verification (on-device, no host runtime)

There is no host-side test suite — the code only runs on the TV. Verify by observing behavior:

- **Event log:** the app writes `/tmp/poc-events.log` on the TV (LS2/ACB/Starfish replies, feed
  stats, seek/bind steps, key raw bytes, crash tracer). `make run` fetches it automatically; it's
  the primary debugging surface. stderr goes to `/tmp/poc-stderr.log`.
- **Screen capture:** `tools/capture-screen.sh [out.png] [DISPLAY|VIDEO|GRAPHIC]` grabs the panel
  output. `DISPLAY` = video plane + UI composited (use this); `VIDEO`-only failing with "no
  signal state" is itself a diagnostic that nothing is decoded on the video plane.
- **Dev trigger files (read once at boot, on the TV):** `/tmp/poc-url` (override the streamed part
  URL), `/tmp/sample.h264` (feed a local raw Annex-B sample instead of streaming),
  `/tmp/poc-autoplay` (auto-press OK for headless capture), `/tmp/poc-autoseek` (one auto-seek),
  and `/tmp/poc-mode` / `/tmp/poc-variant` / `/tmp/poc-ptype` (playback-path bisect knobs).
- Normal interactive flow: D-pad/pointer to focus a shelf card → **OK** starts the demo movie →
  OK toggles play/pause, LEFT/RIGHT scrub-seek, **BACK/Stop** returns to the shelf.
