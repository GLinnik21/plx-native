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
- `make check` — the **host** unit suite (`cargo test --lib`, ~0.3s, no TV), preceded by `make
  lint`. Not a prerequisite of `all` — the cross-build must never depend on a host toolchain run.
  See the testing section for what it does and does not cover.
- `make lint` — three **named** clippy lints (`ifs_same_cond`, `same_functions_in_if_condition`,
  `if_same_then_else`) over the whole crate, `-A clippy::all` first so nothing else can *fail* the
  gate (rustc's own warnings still print). It exists for one bug class the unit suite cannot reach:
  a **shadowed branch**. A duplicated `else if` with an empty body once hid the arm that opens the
  player's track menu — rustc does not warn on a repeated condition, and the dispatch is inside the
  SDL event loop where no host test can see it. Needs the **clippy component on nightly** (rustup's
  default profile ships it; a `--profile minimal` nightly does not).
- `make test` — `deploy` then `run` (the normal iteration command).
- `make kill` — close the app on the TV.
- `make ipk` — repackage the installable `pkg/com.beb.plxnative_<version>_arm.ipk`. The version
  comes from `pkg/appinfo.json` (the single source; `ci/check-package.py` asserts the control
  file agrees), and the archive is **reproducible** — `ci/mkipk.py` normalises tar identity and
  the gzip header and writes the `ar` container itself. Two builds of one commit produce the same
  sha256, which is what makes the manifest hash the TV verifies at install meaningful.
  **Two things here are counter-intuitive and were both shipping broken until 2026-08-02** (found
  the first time anyone actually installed an ipk — the dev loop is `make deploy`, which scp's into
  an already-registered app dir and so never exercises the package). **(1)** webOS needs *two*
  descriptors: `usr/palm/applications/<id>/appinfo.json` AND
  `usr/palm/packages/<id>/packageinfo.json`. Without the second, `appinstalld` registers nothing.
  **(2) GNU `ar` produces an ipk the TV rejects** — it suffixes short member names with `/`, and
  `appinstalld` looks them up verbatim, failing the whole package with `error_code -5, "Failed to
  extract package"`. So `mkipk.py` writes bare `debian-binary` / `control.tar.gz` / `data.tar.gz`
  headers by hand. Neither bug is visible from the other's side: `webosbrew-ipk-verify` reads a
  GNU-named archive fine, and the TV never gets far enough to miss a descriptor. `check-package.py`
  now asserts both. Full account: `docs/distribution.md` §9.
- **`RELEASE=1`** drops the `devtools` cargo feature — today the on-screen counter. It must be on
  EVERY invocation that produces or ships the binary (`make RELEASE=1 deploy`, **not**
  `make RELEASE=1 && make deploy`, which rebuilds as dev and ships that). `deploy`/`ipk` echo
  which configuration they shipped. Switching configuration DELETES `pkg/plxnative` at Makefile
  parse time — deliberately: make 3.81 on macOS compares mtimes at one-second granularity and
  decides staleness from a stat taken before prerequisites run, so no stamp-mtime scheme works.
  Each feature set also gets its own `--target-dir`, because cargo does not hash its output and
  would otherwise report the dev build fresh while the release `.a` sat at that path.
- Override the TV IP with `make TV=1.2.3.4 …`; the run duration with `make run RUN_SECS=30`.

**Cross-compile toolchain:** the webosbrew **native-toolchain** buildroot NDK —
`arm-webos-linux-gnueabi-gcc` (GCC 12, **glibc 2.12, armv7-a soft-float**; default `cortex-a9`
codegen, so we do *not* pin `-mcpu`). It ships a **sysroot** with the TV's own SONAME'd libs,
which the Makefile links against. Rust is a static lib built with plain `cargo +nightly build -Z
build-std --target arm-unknown-linux-gnueabi` (a staticlib needs no linker, so no external
cross-linker — but `-Z build-std` + `-C target-cpu=cortex-a9` is load-bearing: the default
ARMv6 codegen emits the CP15 barrier that SIGILLs on the A53; see the Makefile comment). Headers
come from `include/` (the TV's SDL2 2.0.4-fork headers, kept ahead of the sysroot's newer copies
so we compile against the ABI the TV runs). The ipk needs **no `ar` at all** — `ci/mkipk.py` writes
the archive itself; this line used to say the opposite ("uses the NDK's `ar` (GNU format; macOS BSD
`ar` won't work)") and had it exactly backwards, see the ipk bullet below. The old `zig cc` path is
gone.

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
- **The app's own files are located at RUNTIME (`paths.rs`), never by literal.** webOS picks one
  of two install prefixes — `/media/developer/apps/…` (Developer Mode) or `/media/cryptofs/apps/…`
  (Homebrew Channel) — and the two jail profiles disagree about which directories are WRITABLE:
  `jail_native_devmode.conf` mounts `/media/developer` rw and `/media/internal` ro, and
  `jail_native.conf` does the opposite with no `/media/developer` at all. Resolve via
  `read_link("/proc/self/exe")`, **not** `$HOME` (LG's conf sets HOME twice and which wins
  differs by profile). The session file is a probed SEARCH ORDER for the same reason. Both
  failures were silent before: fonts fell through to DroidSans while `init_text` still logged
  `ok=1`, and the session write dropped ENOENT into a best-effort save. Both now log loudly.
- Video track is always full-panel `1920x1080`; the UI is authored at a fixed `1920x1080`
  (`SCR_W`/`SCR_H`), no DPI scaling (panel is 1:1 at 1080p).

## Testing / verification (two tiers: a fast host unit suite, then the device)

There **is** a host unit suite, and it is not the real gate — both halves matter, and conflating
them is how this section used to be wrong in three files at once.

**Tier 1 — `make check` (host, sub-second).** `cd rust-modules && cargo test --lib` runs the whole
host suite in **~0.3s** on the dev Mac, no TV involved (59 tests as of 2026-07-29 — re-derive with
`cargo test --lib -- --list | grep -c ': test'` rather than trusting this or the per-module counts
below; the first version of this section was stale within one commit because two agents added tests
to the same batch that documented it). `make check` is that command **plus `make lint`** (three
named clippy lints, see the build section — the shadowed-branch gate, ~1s warm); it is deliberately
**not** a prerequisite of `all`, so an ordinary `make` still cross-compiles without ever invoking a
host toolchain run. Run it before `make test` — it is free by comparison, and it is the only signal
you get without waking a television. What it covers today, by module:

  - `stream.rs` (10) — **socket semantics against real loopback sockets, plus response-header
    parsing**: `connect(2)` giving up on its deadline (RFC 5737 TEST-NET black hole), a refused
    connect failing fast rather than reporting success, every failed open retiring its fd (asserted
    by counting `/dev/fd`), `shutdown(2)` waking a reader already blocked in `recv`, the fd being
    claimable exactly once — and, since headers are parsed as bytes rather than strict UTF-8, that
    one non-UTF-8 byte cannot turn a good 200 into a transport failure, that a status line
    straddling the status offset is rejected rather than fatal, and that header names are found
    whatever their casing.
  - `remote.rs` (3) — **the remote-FIFO token framing**: complete tokens fire while a partial
    trailing one is held over to the next drain, and a multi-byte whitespace separates tokens
    without panicking the tokenizer (the separator search is ASCII-only because the split that
    follows is by BYTE index). The FIFO is world-writable in `/tmp` and drained every frame on every
    boot, so a panic here unwinds out of the SDL loop and kills the app.
  - `ff.rs` (11) — the **pure** demuxer logic: the `nal_end` 32-bit bounds guard, AVCC→Annex-B
    conversion (keyframe detection, parameter-set prepending, truncation instead of panic), and the
    AVIO abort guards (a seek after teardown must not open a second connection — graded on an
    accept COUNT from a counting listener, not a return value).
  - `route.rs` (8) — direct-play vs transcode **selection policy**: track fallbacks, English over
    the file's default, the flagged default, part-id parsing, mkv-only direct play.
  - `ui/home.rs` (5) + `ui/card_row.rs` (4) — **focus/geometry/spring math**: focus packing round
    trips, row stepping staying inside the shelf array, the pointer hit column matching the drawn
    card at every snap phase, and the shelf heading's clearance behaviour frame by frame.
  - `metadata.rs` (2) + `browse.rs` (2) — **async landing/mailbox invariants**: a detail or season
    response only installs while it is still the one being awaited (a failed `/children` must not
    land as an empty season), and `reset` clearing the single-flight flags and retry backoff.
  - `task.rs` (3) — **threading invariants**: a refused spawn is a return value not a panic, and the
    `MainThread` token cannot cross a spawn (an absent `!Send` impl is invisible to ordinary code,
    so it is detected via inherent-vs-trait const resolution, with a `Send` control case).

  Three structural limits, all deliberate and all worth knowing before you trust a green run.
  **(1) It cannot link the TV's libraries.** `ff.rs`'s four `#[link]` directives are
  `cfg_attr(not(test))`-gated precisely so the crate's pure logic stays host-testable; the dev Mac
  has no libavformat, so a test that actually *calls* FFmpeg or GL fails to link — that is the
  intended boundary, not a bug. **(2) It runs on Darwin; the app runs on Linux, and they disagree**
  — see `tools/sockprobe.c` above, where `shutdown`-during-`connect` behaves oppositely on the two
  kernels. A socket assertion that passes here is evidence about macOS, not about the TV.
  **(3) The app's async seams are process-wide**, so some tests are serialized rather than parallel:
  `metadata.rs`'s two take `lib.rs`'s crate-wide `testlock::serial()` (the detail and season
  mailboxes contend across modules — a per-module mutex cannot see that, because the season
  generation also moves under `pump_detail`), and `ui/home.rs`'s five take that module's own `FOCUS`
  mutex for `static mut fr`/`fc`. Those locks are load-bearing, not incidental — hold one for the
  whole test in anything new that touches a crate global, and reach for `testlock` (not a fresh
  local mutex) whenever the global is shared across modules.

**Tier 2 — the device, which is still the real gate.** There is no host *runtime*: nothing above
draws a pixel, decodes a frame, or talks to Starfish/ACB, so playback and UI correctness are only
observable as behavior on the TV. **Wake the TV first** (`wake-tv` skill) — asleep, every assertion
fails as "no line found", which reads exactly like a total regression. The **`tv-session` skill** is
the bring-up/observe/drive loop; **`crash-triage`** handles a death; **`bind-tv-lib-abi`** covers new
FFI into the TV's own libraries. The full on-device suite is `./tests/run.py` (18 cases; `--fps` for
the perf gates), and `make test` = `deploy` + `run`.

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
- **A settled non-player screen STOPS PRESENTING** (`ui::idle`, the whole-frame present gate). The
  loop keeps running at full rate — input, pumps and every `*_update` are untouched, so key latency
  and timers are unchanged — but `glViewport`…`SDL_GL_SwapWindow` is skipped while nothing is
  moving, and a 2s keepalive bounds staleness. This is NOT the dirty-RECTANGLE tracking
  `ui/mod.rs` rejects: when a frame does run it is the same immediate-mode full redraw it always
  was. Motion is detected exactly (both `gfx::spring*` integrators report), and discrete changes
  call `ui::idle::invalidate()` — **a new async landing that repaints must add a call there**, or
  it arrives invisibly until the next keypress. **So must anything that animates from a CLOCK
  rather than a spring** — a millisecond ramp, a phase, a countdown — since `note_spring` cannot see
  it: `Xfade::tick` (every route dip) and `Spinner::draw` (every loading read-out) both shipped
  FROZEN before they were made to report, and no fps scene caught it because those grade `loop=`.
  The rest test is visibility — magnitude-relative, capped under a quarter pixel, velocity judged
  as `vel*dt` (the travel this frame) — not a bare epsilon.
  Measured 2026-07-31 on a still Home grid: **39.6%
  → 1.67% of one core** (ours 15.4→1.05, `surface-manager` 24.2→0.62). Consequences for anyone
  reading a log: the heartbeat carries **two different rates and they are easy to swap** —
  **`fps=<n>`** is frames actually swapped that second (the real frame rate, what this gate moves)
  while **`loop=<n>` counts LOOP iterations** — so `loop=62 fps=0` is a healthy settled screen,
  `loop=0` is an app in trouble, and `fps=0` on its own is not a fault at all; the on-screen
  counter draws `loop=` and FREEZES when idle (it is drawn, so it can
  only update on a present) and that is expected, not a hang; and **an fps floor taken on a static
  screen now grades nothing**, which is why `fps:home-grid` arms `plxnative-homeosc` and the still
  case is gated by `fps:home-idle`'s `fps_ceiling` instead. `/tmp/plxnative-noidle` turns the
  gate off (DIAG-exempt, so an A/B does not also change which screen you boot to). The **player
  route is deliberately excluded** — `system.rs` documents the video plane as *slaved* to our
  surface, and playback already draws 0 draw calls with the HUD hidden.
- **The heartbeat fields were RENAMED 2026-08-01 and the old name was REUSED**, so a log or doc
  predating that reads as the opposite of what it says. Old `FPS=` is today's **`loop=`** (loop
  iterations); old `pres=` is today's **`fps=`** (frames presented). An old `FPS=60` says nothing
  about frames at all. The manifest gates moved with them: `floor`→`loop_floor`,
  `present_floor`→`fps_floor`, `present_ceiling`→`fps_ceiling`. Both harness regexes were made to
  match the NEW names only, so an old log fails loudly as "no samples" rather than silently grading
  a loop rate as a frame rate. Analysis docs under `docs/` still quote the old names against the
  line numbers of their day and carry a mapping banner instead of being rewritten.
- **The once/sec `loop=` heartbeat carries `pos=<s>` while frames are presenting** — the same
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
- **Perf gates:** `./tests/run.py --fps` runs the UI-tier FPS regression scenes (gates per scene in
  `tests/manifest.json`; `--fps-player` adds the player tier), asserting the app's once/sec
  heartbeat. **Three assertions, and picking the wrong one is how a frozen animation ships:**
  `loop_floor` grades `loop=`, which counts LOOP iterations — it proves the app is alive, and cannot
  see a stopped animation at all; `fps_floor` grades `fps=` and is what proves an animation still
  RUNS (`login-spinner`, the two `*-nav` scenes); `fps_ceiling` grades `fps=` from the other
  side and proves a still screen stops (`home-idle`). A scene with no motion and only a `loop_floor`
  gates nothing — three carry an `_idle_gate_note` saying exactly that. Every run also reports
  **`drift`** (last-third minus first-third mean): sorting used to destroy sample ORDER, so a
  monotone 60→53 decay and a flat 53 were byte-identical output. It is reported, never asserted —
  18–36 s is far too short to gate a thermal ramp on, and **the "the panel thermally throttles"
  line in `tests/README.md` is an unmeasured hypothesis**, not a finding. For by-hand judder hunts: `/tmp/plxnative-framedrop` logs any frame over 22ms (or over
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
  bisect knob), `/tmp/plxnative-marker[=intro|credits]` (once playing, seek to 5s before that
  server marker — the only practical way to reach the Skip Intro / Skip Credits pill, and, via a
  `final` credits marker, the whole finish → Up Next → auto-advance chain, without playing 50
  minutes of episode first), and the Library browse set: `/tmp/plxnative-library[=N]` (boot straight into the
  browse grid on section N), `/tmp/plxnative-libosc` (perpetual grid focus sweep), and
  `/tmp/plxnative-libswitch` (cycle every switch: tabs, sort menu, unwatched, filter→genre), plus
  `/tmp/plxnative-navosc[=<ratingKey>]` (bounce the ROUTE every 1400 ms through the real press path —
  the only scenes that change route, and so the only ones that sample the whole-screen page
  cross-fade `ui::nav` draws. EMPTY = Home↔the first library section, the two pages that SHARE the
  top tab bar (`fps:home-library-nav`); a ratingKey = Home↔that item's DETAIL page instead, which
  has no shared chrome, a hero backdrop and ambient ground on the far side, and a real teardown at
  the fade floor (`fps:home-detail-nav`). Both boot to Home), and
  `/tmp/plxnative-itemmenu` (snap into the grid, then open the **press-and-hold card context menu**
  on the focused card — `route=itemmenu`; the interactive path is a real ≥500 ms hold, which no boot
  trigger can express). Note `/tmp/plxnative-press` is its TAP twin: it now schedules its own release
  ~150 ms in, because a down with no up is past `press::LONG_MS` and is a HOLD, not a tap.
  Remote-driving: `/tmp/plxnative-remote` is **not** a trigger — the app mkfifos and drains it
  every frame on every boot (so it never affects the picker; its DIAG entry is a permanent
  requirement, not an exception). Write key tokens like `down`/`ok`, or pointer clicks `ck:X,Y`
  in authored 1920x1080 coords, and they replay through the real key/pointer handlers. `ok` is a
  TAP (both edges at once); **`okdown` / `okup` are the split halves**, which is the only way to
  drive a press-and-**hold** — `okdown`, sleep past `press::LONG_MS` (500 ms), `okup` opens the
  item context menu;
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
  `plxnative-remote`/`plxnative-capture`/`plxnative-noidle`) marks the boot as
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
