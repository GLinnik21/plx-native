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

Target device: LG 49SM9000PLA, webOS 4.5, rooted, reached as `root` over ssh. **Its address is NOT
in the repo** — it comes from the gitignored **`.tv-host`** (one line, an IP or hostname), which the
Makefile's `TV` and `tools/`' `TV_HOST` both fall back to; `make TV=1.2.3.4 …` overrides for one
invocation, and a target that needs a TV with neither set fails saying so. The ssh password
`alpine` IS still in the Makefile and that is deliberate — it is webosbrew's *published* dev-mode
root password, identical on every rooted webOS TV, so it identifies nobody and removing it would
break the loop for everyone. App id `com.beb.plxnative` — and since 2026-08-21 a second
install, `com.beb.plxnative.debug`, can sit beside it on the same set (`FLAVOR`, below;
`docs/two-installs.md`).

## Build / deploy / run

The `Makefile` is the entire dev loop. Requires the **webOS NDK** (install with `make
setup-env`), a **Rust nightly toolchain + `rust-src`** (for `-Z build-std`), and `sshpass`
(Homebrew, for deploy/run). See the **`setup-environment` skill** (`.claude/skills/`) for the
full one-time setup + troubleshooting.

- `make setup-env` — download + extract + `relocate-sdk.sh` the webOS NDK into `$(WEBOS_SDK)`
  (default `~/webos-ndk/…`). One-time; re-run `relocate-sdk.sh` if you move the SDK.
- `make` — build `pkg/plxnative` (the ARM binary), and, first, the FFmpeg it ships
  (`ci/build-ffmpeg.sh`; ~2 minutes cold, nothing after). Also compiles `ci/ffabi-assert.c` against
  `vendor/ffmpeg-prefix/include` — **the headers the shipped libraries were built from**, installed
  by the same invocation that produced them — which is what proves `ff.rs`'s ABI table. **One**
  header tree, **one** table. This line long said BOTH vendored trees (n3.3 and n4.0) and *two*
  tables, which was true while the app read the television's FFmpeg and had to select a table per
  firmware; bundling collapsed that into a single equality (`ffabi-assert.c` opens by asserting
  `LIBAVFORMAT_VERSION_MAJOR == 63`), and the vendored trees are gone — so anyone who went looking
  for them found `vendor/` holding nanosvg and nothing else.
- `make deploy` — scp the binary + this flavour's `appinfo.json` + the fonts (UNCONDITIONALLY —
  the old `test -f || scp` guard meant a changed font could never reach the TV) into that
  install's app dir. Refuses if the flavour has never been installed, naming
  `make FLAVOR=… install`, and refuses a dev build on the stable id (see `release-guard`).
- `make run` — close any running instance, wipe this install's event log (`make -s print-eventlog`),
  launch, keep alive `RUN_SECS` (default 18s), then `cat` the on-device event log back to your
  terminal.
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
- `make ipk` — repackage the installable `pkg/<app id>_<version>_arm.ipk`. The version
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
- `make macapp` / `make macapp-zip` — **`pkg/PlxNative.app`: the app as a self-contained macOS
  application**, for sending to somebody who has none of this installed. Same `hostsim` core the
  simulator runs, built `--release --no-default-features` (so no dev counter, no `/tmp` trigger
  surface, no FIFO, no capture listener), with every non-system dylib copied in and rewritten to
  `@rpath`, ad-hoc codesigned. It signs in with the real QR flow and browses a real server; **it
  cannot play video** — the Starfish/ACB seam does not exist off-device, so Play lands on the app's
  failure read-out, exactly as in the simulator. Apple Silicon only, LAN only. `ci/mkmacapp.py` is
  the recipe (its module doc names the three ways a Mac bundle silently ships broken);
  `docs/macos-app.md` is the design note, and `docs/macos-app-readme.md` is what ships beside the
  zip for the recipient.
- **`FLAVOR`** selects **WHICH INSTALL** every TV-facing target talks to. Two builds live on one
  television: `stable` (`com.beb.plxnative` — the app users install, the id in every release,
  manifest and channel listing) and `debug` (`com.beb.plxnative.debug` — the day-to-day developer
  build beside it, with its own launcher tile, its own sign-in and its own runtime root).
  **`FLAVOR ?= debug` in the tracked Makefile, and `stable` has to be TYPED.** That asymmetry is
  the safety argument, not a preference: every command in this repo's muscle memory is spelled
  `make deploy` / `make run` / `./tests/run.py` with no flavour, and each one used to overwrite the
  only install there was — retyping one command is not comparable to destroying the install the
  household watches with, possibly mid-film, with no undo. Tracked rather than a gitignored dotfile
  because a fresh clone or worktree has none, so the dangerous default would be inherited invisibly
  by exactly the checkouts nobody is watching. An unknown value is a parse-time `$(error)` rather
  than a third registered app on the television. A flavour must be installed ONCE before `deploy`
  can reach it — `make FLAVOR=debug install` builds its .ipk, `dev/install`s it and then deploys
  into it (appinstalld replaces `applications/<id>/` WHOLESALE, so stopping at the install leaves
  the packaged binary behind); `make FLAVOR=debug uninstall` removes one and refuses the stable id.
  `deploy`/`ipk` on the stable id refuse a dev build unless `ALLOW_DEV_ON_STABLE=1`.
  **FLAVOR is NOT a codegen input**, which is what makes it cheap: the app reads its id from the
  INSTALL DIRECTORY at runtime (`paths::app_id`, via `/proc/self/exe`), so flipping it costs
  nothing — no rebuild, no second `--target-dir`, no FFmpeg rebuild, one `pkg/plxnative`. Ask the
  seven query targets for any of it (they compose — several goals on one command line print several
  lines): `make -s print-flavor print-appid print-appdir print-rundir print-eventlog print-appport
  print-tv FLAVOR=<f>`. `print-appport` is the newest and the least obvious: the capture listener's
  TCP port MOVES with the flavour (8910 stable, 8911 flavoured), because two installs cannot both
  bind one and both halves of that failure are silent — see the capture trigger below.
  **Never `make -p`/`make -pn`**, which prints a recursive variable's UNEXPANDED
  definition, so `TV` comes back as the literal
  `$(strip $(shell cat .tv-host …))` and every ssh built from it fails against a live television.
  Full account: **`docs/two-installs.md`**.
- **`RELEASE=1`** drops **both** default cargo features: `devtools` (the on-screen counter — the
  feature is contracted to be draw-only) and `devtriggers` (the whole `/tmp` surface, the remote
  FIFO and the capture listener — see `rust-modules/src/dev.rs`). It must be on
  EVERY invocation that produces or ships the binary (`make RELEASE=1 deploy`, **not**
  `make RELEASE=1 && make deploy`, which rebuilds as dev and ships that). `deploy`/`ipk` echo
  which configuration they shipped. Switching configuration DELETES `pkg/plxnative` at Makefile
  parse time — deliberately: make 3.81 on macOS compares mtimes at one-second granularity and
  decides staleness from a stat taken before prerequisites run, so no stamp-mtime scheme works.
  Each feature set also gets its own `--target-dir`, because cargo does not hash its output and
  would otherwise report the dev build fresh while the release `.a` sat at that path.
  **`make check` cannot see a break in this configuration and neither can the PR gate** — both
  build the default feature set, and `--no-default-features` is first compiled during a release
  cut. So a **`PostToolUse` hook** (`.claude/hooks/release-config-check.py`) type-checks it after
  every edit to a `rust-modules/src/**.rs`; it costs well under a second warm, because cargo keys
  fingerprints by feature set and the two configurations coexist in one `target/`. The hazard it
  guards is hand-written `#[cfg(feature = "devtriggers")]` PAIRS, where a spliced-in function
  swallows a neighbour's attribute — `dev::latched_flag!` exists to avoid most of them.
- Override the TV IP with `make TV=1.2.3.4 …`; the run duration with `make run RUN_SECS=30`.

**Cross-compile toolchain:** the webosbrew **native-toolchain** buildroot NDK —
`arm-webos-linux-gnueabi-gcc` (GCC 12, **glibc 2.12, armv7-a soft-float**; default `cortex-a9`
codegen, so we do *not* pin `-mcpu`). It ships a **sysroot** with the TV's own SONAME'd libs,
which the Makefile links against. Rust is a static lib built with plain `cargo +nightly build -Z
build-std --target arm-unknown-linux-gnueabi` (a staticlib needs no linker, so no external
cross-linker — but `-Z build-std` + `-C target-cpu=cortex-a9` is load-bearing: the default
ARMv6 codegen emits the CP15 barrier that SIGILLs on the A53; see the Makefile comment). Headers
come from `include/` (the TV's SDL2 2.0.x-fork headers, kept ahead of the sysroot's newer copies
so we compile against the ABI the TV runs). The ipk needs **no `ar` at all** — `ci/mkipk.py` writes
the archive itself; this line used to say the opposite ("uses the NDK's `ar` (GNU format; macOS BSD
`ar` won't work)") and had it exactly backwards, see the ipk bullet below. The old `zig cc` path is
gone.

## Linking: real libraries, and the ones resolved at RUNTIME

Most of the app links against the **real sysroot libraries** (SDL2, SDL2_ttf, GLESv2,
wayland-client, glib-2.0, luna-service2, and LG's proprietary `libplayerAPIs` / `libpf-1.0` — all
bundled in the NDK), so the Starfish C++ calls get real link-time symbol checking. Every one of
those has the **same SONAME on every webOS release from 2.2.3 to 11.2.0**, which is what makes
linking them normally the right call — check any of it with `tools/fwcompat.py --inventory`.

**Two families are deliberately NOT linked because their SONAME MOVES**: `libcurl`
(`.so.5`→`.so.4`) and `libAcbAPI` (*deleted outright* at webOS 5.0). A `DT_NEEDED` entry is a hard
requirement for one exact name, which cannot express "either of these", and a name the device lacks
kills the process at `exec()` — before `main`, before the event log exists. So they are
**`dlopen`'d by SONAME candidate list**:

- **`rust-modules/src/dynlib.rs`** is the one door. `dynlib!` takes a block shaped exactly like the
  `extern "C"` block it replaces and emits same-named wrappers, so call sites don't change.
  Loading is **all-or-nothing** — every symbol resolves or the table stays empty and the missing
  names go to the event log. Read that module's doc before adding a library to it; the rule is
  *only* when the version actually varies, because moving a library there trades link-time symbol
  checking for version tolerance.
- **`src/starfish.c`** resolves ACB and the webOS 5+ SDL exported-window API the same way, and
  picks between them (`vp_mode()`). The two are complementary across all 14 firmwares.
- Adding a new FFmpeg/curl call means **adding it to the `dynlib!` block**. There is no link error
  to catch you any more — the failure is a logged `Incomplete` at boot and a refusal to demux.
- **A VARIADIC C function keeps its `...`, in the position `curl.h` puts it** —
  `fn curl_easy_setopt_ptr = "curl_easy_setopt"(h: *mut CURL, opt: c_int, ..., v: *const c_void)`.
  Naming the trailing argument's concrete type is right and is how one C symbol is bound as three
  wrappers; moving it *before* the ellipsis is a different CALLING CONVENTION, because **Apple's
  ARM64 ABI passes variadic arguments on the stack** while named ones go in registers. ARM32 and
  x86-64 pass both ways identically, so this compiles, passes `make check`, runs on the television
  — and SIGSEGVs inside libcurl's `strlen` on a Mac, at the first plex.tv call. It was the shape
  the macro emitted until 2026-08-16; `docs/macos-app.md` §2 is the account.

**FFmpeg is a third unlinked family, and it is no longer a version question at all: the app BUNDLES
its own and PINS it.** This doc used to file FFmpeg beside curl and ACB as "SONAME moves,
55→57→58→59→60", which is the wrong mental model to carry into any FFmpeg change today.
`ci/build-ffmpeg.sh` cross-compiles FFmpeg **9.0** with the NDK — shared, LGPL-clean (no
`--enable-gpl`), under a `-plx` build suffix: `libavutil-plx.so.61`, `libavcodec-plx.so.63`,
`libavformat-plx.so.63`, plus `libswscale-plx.so.10` in dev builds only. `make deploy`/`make ipk`
ship those `.so` files **beside the binary**, and `ff.rs::load_libraries` opens them by **absolute
path out of `paths::app_dir()`**, in dependency order under `RTLD_GLOBAL` (they carry no rpath —
FFmpeg's configure evals its flags and `$ORIGIN` does not survive), after which `boot()` refuses to
demux unless the majors are exactly **63/63/61**. Both the suffix and the absolute path are
load-bearing: webOS 11.2.0 ships FFmpeg 6 itself, so a bare SONAME could open the *television's*
copy, and "which libavformat did we actually get" is precisely the question that cannot be answered
over ssh. The reason to bundle was never the SONAME drift, which was survivable — it is that
demuxers, parsers and bitstream filters live in a **registry, as data**, so no symbol table and no
firmware inventory can answer "does this set's libavcodec have `h264_mp4toannexb`". Bundling makes
both halves compile-time facts, and it is also webosbrew's published guidance.

**One consequence settles a question that keeps getting re-opened: our FFmpeg has NO network.** It
is configured `--disable-network` with `--enable-protocol=file` as the *only* protocol, so it
cannot open a URL — http or https, on any firmware. Every byte reaches the demuxer through
`stream.rs` and the custom AVIO, and that is not an accident of the current pipeline but a
build-time fact you cannot route around. "Does the TV's FFmpeg have https?" is therefore not
something to go and probe on a device; it is decided, and the answer is that no FFmpeg in this app
has any transport of its own.

**This replaced the old stub-`.so` trick, and `stub/` is deleted.** Empty `.so` files carrying the
TV's SONAMEs got the link to succeed, but in doing so pinned the binary to webOS 4.x: on anything
newer the loader killed the process at `exec()`, before `main`, before the event log existed. That
was the entire portability problem. Full account: **`docs/webos5-port.md`**.

- The C++ `StarfishMediaAPIs` methods are still called from C via `extern … __asm__("<mangled
  name>")` declarations (in `src/starfish.c`), resolved against the real `libplayerAPIs`. All 15
  are present unchanged from webOS 4.4.2 through 11.2.0.
- The gitignored `sysroot/usr/lib/` (a few real TV `.so` files pulled off the device) is only for
  reference/inspection; the build uses the **NDK's** sysroot, not that directory.

## Portability: grading the binary against 14 real firmwares, offline

**`tools/fwcompat.py`** resolves the built ELF's `DT_NEEDED` and undefined symbols against
webosbrew's firmware **inventories** — for 14 real LG images, every library, its `DT_NEEDED`, and
its full exported-symbol list, keyed by webOS release. It runs on the dev Mac, offline, in under a
second, and it reproduces `webosbrew-ipk-verify`'s verdict exactly. Run it after any change to
linkage or FFI.

```sh
tools/fwcompat.py                       # the matrix: OK/FAIL per release
tools/fwcompat.py --release 5.3.1       # one release, listing what is missing
tools/fwcompat.py --inventory libAcbAPI libavformat libcurl
tools/fwcompat.py --lib libSDL2-2.0.so.0 --grep webOS
```

**It grades whether the app STARTS, and nothing else.** A firmware can export every ACB entry point
and still refuse to put a picture on the video plane. Today: OK on releases 4.4.2 through 11.2.0;
playback is device-verified on 4.10.0 (the dev set) and 6.5.2 (the webosbrew reviewer's set,
issue #22 — the `VP_EXPORTED` path works), and `docs/webos5-port.md` §4 is the list of what
webOS 5+ still needs a human with a television to settle.

**The inventories are SYMBOL LISTS — `name`, `package`, `needed`, `symbols`, and nothing else.**
So `fwcompat.py` answers "does this release export that function" and cannot answer anything about
**strings, struct layouts or code**. A JSON payload key like
`option.externalStreamingInfo.contents.DolbyHdrInfo` lives in `.rodata`, so it is invisible here —
proving one of those across releases needs the actual `.so` files, not this database. For our own
set, `.claude/skills/decompile-tv-lib/` harvests and decompiles them; for OTHER releases we have no
binaries at all today, which is exactly the gap to state out loud rather than infer past.

**Before pushing a change that touches FFI, linkage or `dynlib!`, hand it to the
`fw-compat-reviewer` subagent.** It runs the matrix above and reads the new declarations against
the rules the matrix cannot express — variadic placement, the all-or-nothing loading contract, and
whether a new `DT_NEEDED` names a library whose SONAME actually holds still. CI gates the same
check, so the value is catching it here, before the runner and before a television refuses to
`exec()` the process.

## Look it up: this platform is under-documented, so search before assuming

Almost nothing about webOS native app development is in anyone's training data, and much of what
*is* there describes the web-app stack, which is a different world from ours. **Search the internet
proactively** — do not reason from a symbol name, a header found once, or another client's source
and call it settled.

- **<https://www.webosbrew.org/develop/>** is the reference for homebrew/native development on
  these sets: the NDK we build with, the app/package model, jail profiles and install prefixes,
  and the Homebrew Channel. `https://www.webosbrew.org/webos-userland/` additionally publishes
  Doxygen for LG's own headers (`StarfishMediaAPIs.h` among them) — a header, not documentation,
  but often the only public statement of a signature.
- Also worth searching, in roughly this order of authority: **LG's own published docs**;
  **source of other clients that drive the same API** (Kodi's webOS port is the closest analogue —
  it drives StarfishMediaAPIs in the same `BUFFERSTREAM` mode; jellyfin-webos and Plex's own webOS
  app are NOT analogues, they hand the TV a URL, which is a different pipeline and their results
  do not transfer); **vendor specs** for formats (Dolby, ITU, RFC); the **openlgtv / webosbrew**
  community; and the FFmpeg source we vendor.
- **Grade what you find.** Vendor doc, a spec, or source you can read beats a forum post, which
  beats a recollection. Say which tier a claim came from. And prefer the `find-docs` skill / `ctx7`
  for library and SDK documentation over a raw web search.
- **Nothing external is authority over THIS firmware.** Another client's source proves what some
  webOS does; only the binaries on the shelf prove what ours does. When the two disagree, or when
  the answer decides a device session, settle it with `.claude/skills/decompile-tv-lib/`.

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
is all Rust: `PMS HTTP GET (raw TCP, stream.rs)` → demux (**`ff.rs`, over a custom AVIO on the
FFmpeg the app BUNDLES** — not the television's; see the linking section, and note ours is built
`--disable-network`, so the AVIO is the *only* way bytes reach it) → AU queues with backpressure
(`aq.rs`) → the pump `Feed()`s the Starfish
pipeline. Two worker threads (demux, media/load) sit beside the main loop, which owns all
ACB/Starfish control calls. Also linked and
used: **libcurl** (`net.rs`) does the plex.tv account/login TLS+DNS that the raw-socket
`stream.rs` can't.

## Key files

- `Makefile` — build/deploy/run/ipk; toolchain, the bundled-FFmpeg build + staging + its ABI gate
  (one header tree, not the old dual one), TV ssh creds.
- `src/main.c` — the **boot shim** (crash tracer, event-log/stderr setup, process bring-up); calls
  the Rust `plex_run()`. `src/starfish.c` — the StarfishMediaAPIs C++/ACB seam. `src/svg.c` —
  nanosvg rasterizer. These three are the *entire* C side.
- `rust-modules/src/` — the app core (Rust): `app.rs` (event loop/input), `system.rs` (wayland),
  `player/` (buffer-feed engine + worker threads — **`rust-modules/src/player/CLAUDE.md` is the
  playback deep-dive; read it before touching playback**), `ff.rs` (THE demuxer — the **bundled,
  pinned** libavformat shipped beside the binary, *not* the TV's), `stream.rs`/`aq.rs` (HTTP socket
  → AU pipeline), `net.rs` (libcurl/TLS), and the Plex data layer (`plex/` — **its own
  `rust-modules/src/plex/CLAUDE.md`**, which the rest of this file never pointed at: read it before
  adding a PMS query, and before assuming there is one server. There is a REGISTRY behind
  `client()` now — the app can hold a friend's shared server beside your own, each with its own
  token, `ratingKey` space and watch state. `docs/shared-servers.md` is the design note).
- `rust-modules/src/ui/` — **the UI, as a shared design system**: `theme.rs` tokens, the retui core
  (`mod.rs` `Painter`/`View`), reusable components (`widgets.rs`/`table.rs`/`label.rs`/`icons.rs`),
  and the screens (`home.rs`/`detail.rs`/`player_hud.rs`/…). **`rust-modules/src/ui/CLAUDE.md` is the
  contribution guide — read it before touching UI: use tokens + components, never inline colors,
  never raw font sizes (ALL text in the UI takes its size from the `theme::size` token scale — add
  a documented rung when a new role needs one), never hand-place text.** Full design/status:
  `docs/ui-system-migration.md`.
  (`rust-modules/src/stream.rs` — blocking HTTP/1.1 GET over a raw TCP socket: numeric IP, and a
  body delimited by `Content-Length`, by close, **or by `Transfer-Encoding: chunked`, which it does
  decode** (`HttpStream`'s `chunked`/`chunk_left`, the header match in `http_open`, and
  `hs_next_chunk`). This line claimed "no chunked decoding" long after that stopped being true,
  which makes `stream.rs` read as less capable than it is and sends work to `net.rs` that it would
  have handled — its only real disqualifiers are **DNS and TLS**: it takes a numeric address and
  speaks cleartext, nothing more. `aq.rs` — one-producer/
  one-consumer AU FIFO with byte-cap backpressure. Both are Rust ports of the deleted C headers;
  the hand-rolled `mkv.rs` demuxer they fed is retired — `ff.rs` is the only demux path.)
- `rust-modules/src/dynlib.rs` — the runtime library binder (`dlopen`, by SONAME candidate list or
  by absolute path). Two callers, for two different reasons: `net.rs` binds **curl** by candidate
  list because its SONAME moves between releases, and `ff.rs` binds the **bundled FFmpeg** by
  absolute path because ours ships beside the binary, on no library search path — not because any
  version varies. (**ACB** is the same idea but not this module: `src/starfish.c` is C and does its
  own `dlopen`.) Replaced `stub/`, which is deleted. `tools/fwcompat.py` grades the result;
  `docs/webos5-port.md` is the full account.
- `pkg/` — deployable payload: `appinfo.json` (native app manifest), `plxnative` binary, icons,
  `appfont*.ttf`, and the prebuilt `.ipk`.
- `ipkroot/` — ipk staging (`ctl/control`, `data/`, `debian-binary`); assembled by `make ipk`.
- `tools/capture-screen.sh` — pull the TV screen (incl. video plane) to a local image.
- `tools/netcond.py` — **network-conditioning TCP proxy** (host-side), for the failures a healthy LAN
  cannot produce. Sits between the TV and the PMS (`--listen 32499 --target 127.0.0.1:32400`; the PMS
  runs on the dev Mac) and makes the server misbehave on demand via `/tmp/netcond.mode`:
  `pass` / `stall` (accept, hold open, answer nothing — the case that turns a join into a parked
  frame loop) / `blackhole` / `reject` / `delay:<ms>` / **`rate:<kbps>`**. Any mode scopes to
  matching requests — `stall@/:/timeline` freezes the progress reporter while video keeps
  streaming, which is what makes a clean experiment possible. **That sentence was FALSE for as
  long as it existed and became true on 2026-08-23**: `serve_conn` consulted the scope-aware
  `applies()` to decide reject/blackhole, then handed `relay` a bare `Mode.split`, which throws
  the scope away — so a scoped mode really applied to every open connection, media stream
  included, which is the exact opposite of what a scope is for. Both halves are scope-aware now
  and `tests/test_harness.py` pins it. Modes apply to connections ALREADY
  OPEN, so a POST can be frozen mid-flight. **`rate:` is the newest and it is a SPEED rather than a
  fault**: one token bucket for the whole proxy (a link is shared), decimal kilobits, live under an
  open transfer, so the four legs of LG checklist item #43 CASE1 — 512 Kbps → 1 Mbps → 7 Mbps →
  17.5 Mbps — are one scripted run instead of four launches, and #14's degrading link is
  measurable rather than anecdotal. `tools/netcond.py --selftest`
  proves the shaper against a loopback transfer with no television in the room. Point the app at it by editing `PMS_PORT` in the gitignored `src/config.local.h` and
  `make deploy` (host/port are compiled into `main.c`). **Pick a port Plex is not already on** — it
  binds `127.0.0.1:32401` itself, and the more specific bind wins, so the proxy is silently bypassed.
  **And the macOS application firewall silently drops the TV's connections to the ad-hoc python
  listener** (verified 2026-08-11: netcond up, mode armed, zero requests logged, the TV's probe gets
  an empty read) — the "allow incoming connections?" GUI prompt must be clicked once per python
  path, so start netcond BEFORE going headless, and treat "netcond logs nothing" as this, not as a
  quiet TV.
  Measured with it 2026-07-29: teardown's join of the `/:/timeline` reporter parked the main loop
  **6974 ms**; after moving that join onto the scrobble worker, BACK→teardown is **0.5 s**.
  **That run was taken while the scope bug above was live, so the proxy was stalling EVERY
  connection and not only the reporter's — and the number and the conclusion both survive it,
  for a reason worth writing down rather than re-deriving.** The finding was recorded as
  `THREADJOIN timeline 6974ms` (`task::join` emits one NAMED line per join), so the attribution
  came from the instrument and not from inferring a total; and `timeline` is the only join that
  COULD have parked whatever else was stalled, because `engine::teardown`'s step 1 deliberately
  wakes the other two before joining them — `http_shutdown` on the demux socket, `aq_abort` on
  both AU lanes — while the reporter's POST had no such wake, `stream`'s one-shot wrappers boxing
  their socket privately. The before and after were taken the same way, so the 14x is
  apples-to-apples. What CANNOT be claimed from that session is the thing the scope sentence
  promises — that video kept streaming while the reporter was frozen. It is a property of the tool
  today and was not a property of that run; re-running it scoped (and seeing `demux`/`media` at 0
  beside it) is a device job nobody has done. NB a
  request occasionally fails through the proxy that succeeds direct (seen once on `POST /playQueues`)
  — confirm any new failure against a direct run before believing it.
- `tools/sockprobe.c` — standalone ARM diagnostic (`make sockprobe`, scp, run, delete) for socket
  semantics **the host suite cannot answer**: `cargo test` runs on Darwin, the app on Linux, and
  they disagree. Measured 2026-07-28: on this kernel `shutdown(2)` **does** abort a `connect(2)`
  in progress (rv=0, handshake dies at once) — which is why `stream.rs::http_open` publishes its fd
  at `socket()`. On Darwin the same call instead makes `connect_timeout` report *success* on a
  socket that never connected. Reach for this before asserting any syscall behaviour from memory.
- `tools/logmprobe.c` — standalone ARM diagnostic (`make logmprobe`, scp, run, delete) for LG's
  **KADP log masks, on a RUNNING app**. It exists because of one specific trap that cost a day:
  `KADP_LOGM_WriteLog` gates **BITWISE**, not by threshold — `(1 << level) & rec[0x20] & ~rec[0x24]`
  — and `kad-hdr` ships with `enable=0x0000000b`, i.e. levels 0/1/3 with **bit 2 clear**. So
  `DOVI_MDAsync_WriteOTTMetaData`'s only unconditional line is invisible, a perfectly healthy
  metadata writer logs NOTHING, and "it never appears in the log" is not evidence it never ran. The
  mask table is mmap'd `MAP_SHARED` from `/dev/lg/logm`, so it is shared by every process and can be
  flipped from a SECOND ssh session mid-playback — no rebuild, no relaunch, no perturbation of the
  session being measured. Read-only unless given `set`/`clear`. This is what ended the Profile 5
  investigation (2026-08-21) in one run, and the general rule it teaches is in
  `[[silent-instrument-trap]]`: **prove the instrument can see the thing before reading its
  silence.** Two more instruments here are silent by construction — the heartbeat's `vtick=`/`vgap=`
  count a **5 Hz** position callback, not presented frames, and read a flat `vgap=201ms` straight
  through a visible stutter; and LG's `GST_DEBUG` was long avoided as perturbing, which is true of
  `dualsequencer:9` and **false of `:6`** (same scene, 123 misses uninstrumented vs 122 traced) —
  `:6` is the only per-frame cadence instrument this project has.
- `tools/threadprobe.c` — standalone ARM diagnostic (`make threadprobe`, scp, run as root, delete):
  spawns under the app's uid until `pthread_create` refuses. Measured 2026-07-28 — **2 MB stacks
  die at 2043 threads on `RLIMIT_AS` (the full AArch32 4 GB), 256 KB stacks at 3745 on
  `RLIMIT_NPROC` (3746)**, both EAGAIN, against the app's 31 threads at playback peak. Which limit
  binds depends on the stack size; that is why `task::spawn_small` uses 256 KB.
- `docs/dolby-vision.md` — **Dolby Vision + Dolby Atmos, end to end**: what ships per profile, the
  two Load-payload nodes and the binaries they were recovered from, the ACB Atmos forward and the
  `SOUND_ERROR_019` rule it retired, the Profile 5 one-tick stutter with the six hypotheses that
  were wrong first, the three instruments that were silent by construction, and what the Dolby
  specifications do and do not require (with page citations). Read it before touching
  `Dovi::presentation`, `with_dolby_hdr_info`, `with_immersive`, `acb_send_atmos` or `pts_nudge_ns`.
- `docs/two-installs.md` — **why two builds live on one television and what they do and do not
  share**: the `FLAVOR` axis and its seven query targets, the identity model (the app id is the
  install DIRECTORY's name, read from `/proc/self/exe`, so nothing about it reaches codegen), the
  shared-resource inventory (separate: runtime root and everything in it, session file, plex.tv
  device name, launcher tile, the Load payload's `option.appId` and the ACB id — still shared: the
  jail template, the ONE video plane, `/media/developer`, `splash.png`, the `requiredMemory`
  budget), the two name traps, and the ordered list of what only a television can settle. Read it
  before adding anything per-install, and before assuming a log came from the install you meant.
- `docs/pms-api.md` — **verified** PMS REST reference (sections, hubs, metadata, image transcode,
  direct-play URLs, timeline). The authoritative spec for the data layer.
- `docs/buffer-feed-plan.md` — historical design note for the buffer-feed pivot (partly outdated).

## Non-obvious conventions & gotchas (all verified in code)

- **This repository is PUBLIC, and some of the data in this working copy is not the maintainer's
  to publish.** Several paths are gitignored for that reason — `.tv-host`, `.tv-mac`,
  `src/config.local.h` (a live Plex token), `tests/manifest.local.json` and `pkg/auth.json` among
  them; the LIST is `PRIVATE_FILES` in `.claude/hooks/outbound-guard.py`, not this sentence, which
  is why no count is given here. The rule for anything leaving the machine is **placeholders,
  always** — a PR body, an issue comment, a release note, a commit message — and
  `docs/shared-servers.md` carries the stand-in table this repo actually uses. **It has already
  failed once as a convention, in these words.** A batch of subagents told to write device
  recipes "executable without me" each did the obviously helpful thing and pasted a friend's real
  server address, port, `machineIdentifier` and handle into **four PR bodies (#28-31) on
  2026-08-14**. All four were redacted; GitHub keeps PR-body edit history, so those values are
  permanently public, and they were the FRIEND's rather than this project's to give. Since then a
  **`PreToolUse` hook** (`.claude/hooks/outbound-guard.py`) refuses any publishing command whose
  text carries one of those values — including a heredoc body and a `--body-file`, which are how a
  PR body of any length is actually passed. It never prints the value it matched, since that is
  the same leak by a shorter route. The escape hatch is a prefix on the command, and an agent
  reaching for it is doing the thing the hook exists to prevent.
- **LG's SDL fork has a shifted `SDL_KeyboardEvent`.** `e.key.keysym` is unreliable; the handler
  (`app.rs`, via `rd_u32`) reads raw bytes off the event: `+16` = state (u32), `+20` = webOS keycode
  (u32), `+24` = sym (u32). State low byte = pressed(1)/released(0); bit `0x100` = auto-repeat.
  Magic-Remote buttons are matched by these `wcode`s (e.g. PAUSE=72, PLAY=450, BACK=461/482, stop=413,
  D-pad L/R alt 412/417; the wcodes live in `ui/consts.rs`, as `WCODE_*` — true of the L/R pair only
  since 2026-08-15, when it was named; until then it was five bare literals in `app.rs` and this
  sentence was false. One literal is left, and deliberately: BACK's secondary 461 is written out
  inside `is_back`, in `consts.rs` itself, beside the constant and the comment explaining it).
  Preserve this raw-offset reading if you touch input.
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
  **re-raises to `SIG_DFL`** so the OS/crashd still captures a real backtrace. Two logs, both in the
  install's runtime root (`/tmp`, or `/tmp/<app id>` for a flavoured install): `plxnative-events.log`
  is truncated each launch; **`plxnative-crash.log` is append-only and survives the relaunch** — read it
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
- **Three resolutions, and they are not the same number.** The UI is authored at a fixed logical
  `1920x1080` (`SCR_W`/`SCR_H`) and the video track is full-panel `1920x1080`. The **drawable** —
  what GL renders into — is read back at boot by `rust-modules/src/surface.rs` and is what
  `glViewport` uses; it has been 1920x1080 on every device so far, so `surface::scale()` is 1.0 and
  nothing scales. The **panel** is a third number entirely: `SDL_webOSGetPanelResolution` reports
  `3840x2160` on the dev TV, whose UI surface is 1080p. It is a diagnostic — **never a layout
  input**, since sizing the UI from it renders a 4K interface into a 1080p buffer. The boot log
  prints all three on one `surface:` line. Do not render at panel resolution even if a set offers
  it: 4x the fill rate through shaders that needed work to reach 60 fps at 1080p, versus a free
  hardware upscale.

## Testing / verification (two tiers: a fast host unit suite, then the device)

**Two skills sit on top of this section; reach for them before reading it end to end.**
**`which-tier`** decides which tier a given change actually needs and — the half that gets skipped
— which tiers are structurally blind to it, routing by what the change touched. This section
documents what each tier IS; that skill decides which to run. **`doc-claim-auditor`** (a subagent)
answers the other post-change question: did this change make any claim in the prose FALSE. That one
exists because nothing compiles CLAUDE.md, which is why the paragraph below has to open by telling
you its own numbers are wrong.

> ### THERE IS ONE TELEVISION AND IT IS A MUTEX. TAKE THE LOCK.
>
> There is exactly one dev set, one app instance on it, and webOS enforces nothing: two
> `tests/run.py` runs, or a run plus a `make deploy`, or a capture session plus either, **kill each
> other's app**. The damage is not a clean failure — it is *plausible wrong data*: bogus
> `timeline_climb` failures, an fps number measured while somebody else's binary was being deployed
> underneath, a capture of a screen the other job navigated away from. You cannot tell those from a
> real regression by looking at them.
>
> **Since 2026-08-22 there IS a lock, and it is enforced.** `tools/tv-lock.sh` holds a lease in a
> directory ON THE TELEVISION (`/tmp/plx-tv.lock`, so it spans worktrees and machines, and outside
> the `plxnative-*` prefix so it neither trips `dev::any_trigger_present` nor gets swept by a
> teardown). **The `tv-lock` skill is the workflow** — acquiring and queueing, the two things the
> lock CANNOT see (a human watching television, and a job started from a checkout without these
> tools) together with the `fuser`-per-install and ssh-count pre-flight that is the only thing that
> catches them, and when a lease is safe to break.
>
> **You cannot skip it.** `tv-session.sh` (`up`/`key`/`click`/`shot`/`down`), `make deploy`/`run`/
> `run-stream`/`kill`/`install`/`uninstall`, `tests/run.py` and `tools/capture-screen.sh` all take
> it, and a **`PreToolUse` hook** (`.claude/hooks/tv-lock-guard.py`) refuses any Bash command that
> reaches the set without a lease — including a raw `ssh root@…`, an `scp` into the app directory
> and a `sshpass` one-liner. Read-only diagnostics are deliberately not blocked:
> `tv-session.sh log|status`, `tools/crash-report.sh`, `make -s print-*`. With nobody holding the
> set a single command takes a short implicit lease rather than failing; **a SESSION should take a
> real one**, because the gap between two of your own commands is exactly where another lane lands.
>
> **Running SEVERAL agents at once is a PLANNING problem, not a locking one, and it has its own
> skill: `fleet-plan`.** The lock schedules; it does not plan — two lanes that both want the set
> still run in series, and that queue is invisible in the plan you wrote. The one line to carry
> without opening it: the television is the scheduling constraint, **a lane is a CHECKOUT** (so a
> second worktree on the same Mac is a second lane, however the prompt describes it), give device
> access to at most one and send every other lane to `make sim`. Telling two prompts "you own the
> television exclusively" is *not* a mutex — each is true when written and false the moment the
> second one starts, which is the 2026-08-21 collision that was caught by luck rather than by
> anything failing loudly. The skill carries the rest: the shared stash stack that hands one lane
> another lane's work, what a second build tree costs on disk, cutting a worktree from the right
> base, the gitignored files a lane has to be seeded with, the worker-prompt block, and the
> collision recovery — stop **one** job, re-run it from scratch, and treat anything measured during
> the overlap as contaminated whether or not it looks fine.

There **is** a host unit suite, and it is not the real gate — both halves matter, and conflating
them is how this section used to be wrong in three files at once.

**Tier 1 — `make check` (host, sub-second).** `cd rust-modules && cargo test --lib` runs the whole
host suite in **~0.3s** on the dev Mac, no TV involved. **Treat every test COUNT in this section as
already wrong, including the one in this sentence.** 386 measured 2026-08-13; 284 on 2026-08-02; a
documented 59 before that, which was five times stale before anyone noticed — and the first version
of this paragraph was stale within one *commit*, because two agents were adding tests to the same
batch that documented it. Three numbers have now rotted here, so do not add a fourth: the only
count worth having is the one you take yourself, with
`cd rust-modules && cargo +nightly test --lib -- --list | grep -c ': test'`. **The per-module counts
below have the same disease and are worse**, because a stale one reads as precise rather than round
— several were written when the module was a third its present size (`route.rs` and `ui/home.rs`
are both well past the numbers they carry). Read those bullets as **what each module covers**,
which is stable and is why they are here, and never as a census. **Run it with the same toolchain
the Makefile does.** A bare
`cargo test` uses the default toolchain; `make check` uses `cargo +$(RUST_NIGHTLY)`, and the two
have disagreed — `task.rs`'s refused-spawn test passed 284/284 on stable while panicking inside
`std` on nightly, which reads as flakiness and is not. Nightly is the gate, because `-Z build-std`
means nightly is what ships. `make check` is that command **plus `make lint`** (three
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
  **(1) It cannot run the native libraries, and it no longer fails by FAILING TO LINK.** `ff.rs`
  used to carry four `cfg_attr(not(test))`-gated `#[link]` directives, so a host test that called
  FFmpeg died at link time; those directives are gone (everything goes through `dynlib!` now), the
  crate links unconditionally, and such a test instead takes `dlopen`'s `None` branch on Darwin.
  Same boundary, quieter failure — a test that "passes" having never entered FFmpeg or GL is the
  shape to watch for. **(2) It runs on Darwin; the app runs on Linux, and they disagree**
  — see `tools/sockprobe.c` above, where `shutdown`-during-`connect` behaves oppositely on the two
  kernels. A socket assertion that passes here is evidence about macOS, not about the TV.
  **(3) The app's async seams are process-wide**, so some tests are serialized rather than parallel:
  `metadata.rs`'s two take `lib.rs`'s crate-wide `testlock::serial()` (the detail and season
  mailboxes contend across modules — a per-module mutex cannot see that, because the season
  generation also moves under `pump_detail`), and `ui/home.rs`'s five take that module's own `FOCUS`
  mutex for `static mut fr`/`fc`. Those locks are load-bearing, not incidental — hold one for the
  whole test in anything new that touches a crate global, and reach for `testlock` (not a fresh
  local mutex) whenever the global is shared across modules.

**Tier 1.5 — the desktop simulator (`make sim`), which DOES draw pixels on the host.** This tier
did not exist before 2026-08-14, and the line below used to read "there is no host *runtime*" flatly
— that is now wrong for the UI half and right for everything else. `plxnative-sim` is the same app
core built with `--features hostsim` and linked against desktop SDL2 + desktop GL 4.1 core: it
renders the real interface against a real PMS, boots to a screen with the same `plxnative-*`
triggers, is driven by the same remote-FIFO tokens, and screenshots itself. **The
`ui-sim` skill is the loop.** It exists because the TV is a mutex — one set, one app instance, two
harness jobs kill each other — while N simulators run side by side, each pointed at its own
instance root (`PLXNATIVE_RUNTIME_DIR`, which is where the triggers, FIFO and event log now come
from; unset it and everything resolves to `/tmp` exactly as before). It answers layout, focus,
navigation, every screen, and the whole Plex data layer. It CANNOT answer frame rate (different
GPU — every simulator heartbeat carries **`sim=1`** so a pasted log cannot be mistaken for a
device measurement), text rasterization, or anything about video (the 29-symbol Starfish/ACB seam
is absent; `player::ffi`'s host arm reports the seam's own "no video path" failure, so Play lands
on the real failure read-out). Two bugs it has already found in DEVICE code: the glyph upload
ignored `SDL_Surface::pitch` (`text.rs`), and `dev`/`remote`/`log` all hardcoded `/tmp`.
**Two host-only traps that read as your change being broken.** (1) **`make sim-shot` HANGS on a
settled screen** — `SIM_FRAME` is a count of *presented* frames (`shot.rs`, and `app.rs` says the
same at the `shot` token: "presented frames only accrue when something repaints"), and `ui::idle`
gates presents, so a screen that settles before frame N never reaches N. Arm
`plxnative-noidle` in the instance root first; three agents lost time to this in one day. (2) macOS
`libSDL2` is **sdl2-compat forwarding into SDL3**, so pushing a synthetic **`SDL_TEXTINPUT`** through
`SDL_PushEvent` SIGSEGVs *inside SDL* — SDL3's text event carries a `char *text` where SDL2 carries
an inline `char[32]`, and the shim dereferences it. No Rust panic, no log line, the process is just
gone. The remote FIFO's key and `ck:` tokens are safe because every field they set is a scalar; see
`docs/search.md` §3.

**Tier 2 — the device, which is still the real gate.** Nothing on the host decodes a frame or talks
to Starfish/ACB, so playback correctness — and every pixel-level and perf question — is only
observable as behavior on the TV. **Wake the TV first** (`wake-tv` skill) — asleep, every assertion
fails as "no line found", which reads exactly like a total regression. The **`tv-session` skill** is
the bring-up/observe/drive loop; **`crash-triage`** handles a death; **`bind-tv-lib-abi`** covers new
FFI into the TV's own libraries. **`./tests/run.py` needs a gitignored `tests/manifest.local.json`**
— `manifest.json` holds only the installation-INDEPENDENT case definitions, and each case names the
SHAPE of item it needs (`item: "movie_h264_ac3_1080p"`); the overlay maps that to a ratingKey on
this server and supplies the PMS host, TV address and test user. Copy
`tests/manifest.local.json.example` and fill it in; the runner refuses to start without the FILE, and
without `pms.host` / `tv` / `test_user.id`. **An `item` key it cannot resolve is a SKIP, not a
death** (since 2026-08-22) — absent or left as the example's `<ratingKey>`, it skips the cases and
fps scenes naming it, prints the reason in the summary and in `--list`, and runs the rest. The
matrix is a SUPERSET of what any one library holds (it names 4K DoVi P8, TrueHD, PGS, AV1-with-no-
DP-audio), so before that change the suite ran for exactly one library in the world; one ordinary
h264/ac3 movie now gets a stranger five playback cases and 13 of 16 fps scenes. Consequence when
reading a result: **the pass count is meaningless without the skip count beside it** — `16 passed`
can mean sixteen of the shapes that installation happens to own. Resolution happens once at load and
writes `rk` back for the resolvable ones, so everything downstream still reads `case["rk"]`; a
skipped case has NO `rk` at all and is partitioned out in `main()` before anything subscripts it.
`tests/test_harness.py` (in `make check`) pins that partition, because the maintainer's own overlay
resolves every key and so never enters the path. The full on-device suite is `./tests/run.py`
(21 cases; `--fps` for the perf gates), and `make test` = `deploy` + `run`.

**Tier 2 is TWO SUITES on one television, and since 2026-08-22 the DEFAULT IS THE SYNTHETIC ONE.**
A bare **`./tests/run.py`** now runs the SYNTHETIC tier (generated clips, no Plex — take the count
yourself with `./tests/run.py --list`, which is the only census that cannot rot; the number written
here went stale inside the branch that wrote it, in one commit); the
21 library-backed cases everything above describes are **`./tests/run.py --server`**, and `--fps` /
`--fps-player` imply `--server` because those scenes navigate a real signed-in Home. `--pipeline`
still parses (it names the default); pairing it with `--server`/`--fps` is refused rather than
silently resolved. The inversion is about what the obvious command should mean: the default has to
be the thing that runs for everybody, needs no credentials, touches nobody's watch history, and
answers "is the PLAYER broken" — charging a PMS, a token and a filled-in overlay for typing
`./tests/run.py` meant most people could not type it at all. **Never ship on the default alone**
(what it cannot see is three paragraphs down). The server tier is still the right shape for what it
grades — SELECTION: `/decision`, direct-play vs transcode, track menus from PMS metadata, markers,
resume, the `/:/timeline` reporter — which is also why it needs somebody's library.
**The synthetic tier is the PLAYER PIPELINE, with no Plex anywhere.** A generated clip (`make fixtures-pipeline`, ~0.7 GB flat in
`$FIXTURES_OUT/pipeline`) is served off the dev Mac by `tests/serve_fixtures.py` and played through
**`/tmp/plxnative-playurl`**, one JSON object carrying the URL *and the Load payload declaration*.
It needs a TV address and nothing else — no token, no ratingKey, no `manifest.local.json`, no
sharing — so it is the only tier a stranger can run, and it is what separates "the player is
broken" from "the library layer is broken" when a server case fails. **What it covers, precisely:**
the player direct-plays exactly `{h264,hevc}` × `{aac,ac3,eac3}` in mkv/mp4/m4v (`route.rs`'s codec
gate + `plex::DP_AUDIO_CODECS`) — 2 of the 19 video and 3 of the 19 audio codecs the television's
own table (`/etc/umediaserver/device_codec_capability_config.json`, which `devcaps.rs` reads)
claims to decode, everything else being a server transcode BY DESIGN since the Load payload has
only `H264`/`H265` and `AC3`/`AC3 PLUS`/`AAC`. All six of those payload combinations are covered
here, plus DV 8.1, both containers, in-place seek in each, and the FRAME-RATE axis added the same
day (`pipe_h264_1080p5994` is the only fixture that reaches `fps_rational`'s 1001-denominator
branch — device-verified `esInfo: videoFps 60000/1001` — and `pipe_hevc_4k_60fps` is 4K60 HEVC;
every other fixture in both packs is 24p), and — since 2026-08-23 — the **RESOLUTION x CODEC
matrix** LG checklist #50/#51 is graded as: SD 720x480 / HD 1280x720 / FHD 1920x1080 / UHD
3840x2160 x {h264, hevc}, eight cells, one audio codec per column so a row-to-row difference is the
raster alone, each grading `expect.video_size` EXACTLY out of the `ff:` line rather than a width
floor — which is what stopped the item being answerable only as "pieces are covered", and which
closed the `4k-h264` library gap on this tier (`8-bit-hevc` was already closed by
`pipe_hevc_aac_mp4` and the gap list had not caught up; a generated clip closes neither gap's real
half, which is a PMS DECISION on such an item). The same day added the one clip in
either pack that is MEANT to run out (`pipe_finish_eos` and `pipe_replay_after_eos`, 20 s), which
is #46 END TO END — the second of those restarts the finished stream through
`/tmp/plxnative-replay[=N]`, a bounded counter re-arming `app.rs`'s one-shot autoplay latch, and
grades the re-entry COUNT, a second `load:` line, a second fetch off the fixture server and a
media position that falls and then climbs. Still
uncovered: HLG, HDR10+, DV P5/P7, Atmos, the
4096-wide edge and any refusal above it, a USER-driven replay (a Play control on a detail page is
server-tier by construction), and the transcode INPUT space
(three server cases on one AV1 item stand in for 17 codecs). One of those is an app gap, not a test
gap — `devcaps` ignores the table's `maxFrameRate`, so the profile sent to PMS bounds no frame
rate at all. Three things about it
are worth knowing before reading a result. **(1)** The declaration is the interesting half and the
main false-PASS risk: `engine`'s `_ =>` arm maps an unrecognised audio codec to `"AC3"` and a
non-`hevc` video codec to the H264 payload, so a trigger that was never read produces exactly the
right payload for the AC-3 baseline case — which is why the matrix carries cases expecting
`"AC3 PLUS"` and `"AAC"`, and why the engine now logs one `load: v=… a=… fps=… dv=… atmos=…` line
per streamed playback (the only place an event log says what the app told the television the stream
WAS, as opposed to what the demuxer found in it). **(2)** `python3 -m http.server` is DISQUALIFIED
and the failure is silent: the AVIO seeks by reopening with `Range: bytes=N-`, `stream.rs` accepts
any 2xx, and a server that ignores the header answers 200 from byte zero while the demuxer believes
it is at N. `serve_fixtures.py` answers 206 or 416, never 200-with-a-Range, and the suite asserts
the ranged-open COUNT off the server itself — the one assertion no log line can give. **(3)** It
cannot prove that the declaration it feeds is the one a real item would produce: it writes those
five `route` fields itself, so `metadata → plan → apply_plan` is bypassed and a regression there
passes it green. It also reaches no resume, marker, Up Next, timeline, track-SELECTION or transcode
path. Never run only this one before a release. `tests/README.md` has the tier table.

- **Event log:** the app writes `plxnative-events.log` in its runtime root on the TV (LS2/ACB/
  Starfish replies, feed stats, seek/bind steps, key raw bytes, crash tracer) — `/tmp` for the
  stable install, `/tmp/<app id>` for a flavoured one; `make -s print-eventlog FLAVOR=<f>` resolves
  it. `make run` fetches it automatically; it's the primary debugging surface. stderr goes to
  `plxnative-stderr.log` beside it. **Its FIRST line names the install** —
  `install: id=… flavour=… runtime=… features=dev|release APPID_env=…` (with `appdir:` on the
  next line, from `app_dir()`'s own provenance-carrying log), written before
  anything can fail. It is the only witness that says which of two binaries *both named
  `plxnative`* produced a log (`pidof` cannot tell them apart, and `pkg/plxnative` is a path every
  configuration writes, so an md5 proves only "some flavour of some configuration"), so read it
  before grading anything. `APPID_env=` is evidence rather than configuration: nothing off a desk
  says whether SAM exports `APPID` to a native app on this firmware, and this answers it for free
  on every run.
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
  the app, clears every `plxnative-*` trigger in that install's runtime root **including the
  injected PMS token**, and reaps stray ssh clients. Only the three append-only `*.log` files
  survive. Nothing did this before
  2026-07-28 except the normal path, so an interrupted run left the app playing (scrobbling a
  resume point the next run then inherited) and a live per-server token in world-readable `/tmp`.
  The teardown is armed at the moment the harness commits to driving the TV, so `--list` and a
  no-match `--filter` still exit without closing an app you are watching.
- **`ps | grep plxnative` finds NOTHING on this TV even while the app is running** — busybox `ps`
  here shows neither the path nor the argv. Use **`fuser $(make -s print-appdir)/plxnative`** for
  liveness: it is INODE-scoped, so it answers about exactly ONE install — which is the right
  question *here*, where you are asking whether the install you are driving is up, and so the bare
  form (no `FLAVOR`, i.e. the flavour everything else in your session is using) is the correct
  spelling. It is the mutex pre-flight above that has to run this once per flavour, because that
  one asks the opposite question — is anybody *else* on the set. `pidof plxnative` is NAME-scoped,
  and since the flavour split it matches BOTH installs: two binaries, one name. It returns two pids
  in an order busybox does not promise, so it cannot say which install it found. When you need the
  pid itself, resolve `readlink /proc/<pid>/exe` per pid. A liveness check built on `ps` reads
  exactly like "the app is closed", which will cheerfully confirm whatever you were hoping to
  prove.
- **Screen capture:** `tools/capture-screen.sh [out.png] [DISPLAY|VIDEO|GRAPHIC]` grabs the panel
  output. `DISPLAY` = video plane + UI composited (use this); `VIDEO`-only failing with "no
  signal state" is itself a diagnostic that nothing is decoded on the video plane.
- **Perf gates:** `./tests/run.py --fps` runs the UI-tier FPS regression scenes (gates per scene in
  `tests/manifest.json`; `--fps-player` adds the player tier), asserting the app's once/sec
  heartbeat. **Three assertions, and picking the wrong one is how a frozen animation ships:**
  `loop_floor` grades `loop=`, which counts LOOP iterations — it proves the app is alive, and cannot
  see a stopped animation at all; `fps_floor` grades `fps=` and is what proves an animation still
  RUNS (`login-spinner`, the two `*-nav` scenes, `search-type`); `fps_ceiling` grades `fps=` from the
  other side and proves a still screen stops (`home-idle`, `search-idle`). The Search pair is the
  clearest illustration that these are two halves of ONE question — same screen, same trigger, the
  oscillator added or taken away. A scene with no motion and only a `loop_floor`
  gates nothing — **`home-hero` carries an `_idle_gate_note` saying exactly that, and it is the only
  one left**; this line said "three" long after the other two (`home-grid`, `library-scroll`) were
  given oscillators and real `fps_floor`s, which is the fix that note asks for. The two other
  `loop_floor`-only scenes are player-tier (`info-panel`, `track-menu`) and need no note, because
  the present gate excludes the player route. Every run also reports
  **`drift`** (last-third minus first-third mean): sorting used to destroy sample ORDER, so a
  monotone 60→53 decay and a flat 53 were byte-identical output. It is reported, never asserted —
  18–36 s is far too short to gate a thermal ramp on, and **the "the panel thermally throttles"
  story is MEASURED AND REFUTED** (2026-08-19): a control leg holds 60/60/60 across six runs on a
  set up 2 h 15 m under continuous load, and what actually produces a 50 fps reading is **arming a
  profiler** — `frame.ui` brackets every frame with two `glFinish`es and drops a 60 fps leg to 45.
  **Never quote `fps=` from a run with `/tmp/plxnative-profile` or `/tmp/plxnative-hwcnt` armed**;
  take pacing in a separate unarmed run. What this hardware WILL give you, priced in frames and
  milliseconds for design rather than in cycles, is **`docs/glass-hardware-budget.md`**; the
  instruments and their structural blind spots are `docs/backdrop-blur-profiling.md`. For by-hand judder hunts: `/tmp/plxnative-framedrop` logs any frame over 22ms (or over
  N ms — the file's content) with a pump/draw/swap/upload breakdown and adds `worstframe` to the
  heartbeat; `/tmp/plxnative-homeosc` sweeps the grid focus top↔bottom perpetually to reproduce
  scroll judder headlessly.
- **Dev trigger files (read once at boot, in the install's RUNTIME ROOT).** There are ~40; this
  lists the ones worth knowing by name. **The ROOT moved for flavoured installs and ONLY for
  them:** the stable install keeps `/tmp` byte for byte, so every `/tmp/plxnative-*` path written
  out below stays literally true for the app users get, while a flavoured install puts the SAME
  names under `/tmp/<app id>` (`/tmp/com.beb.plxnative.debug/plxnative-library`). Nothing was
  renamed — not the ~40 triggers, not the `plxnative-remote` FIFO, not the three logs, not
  `dev::DIAG`; only the directory they sit in. `make -s print-rundir FLAVOR=<f>` is how a tool asks
  rather than restating the rule, and the root is created **1777, mkdir THEN an explicit chmod**
  (umask masks mkdir's mode) because root arms triggers there over ssh before the jailed app has
  ever booted, and an owner-only mode locks one of the two out — a 0-byte event log, which every
  tool here reports as "no line found", i.e. exactly like a total regression. Why any of it:
  **`docs/two-installs.md`**.
  **The catalog is the source, not this list** — get the real one with
  `{ grep -rhoE '/tmp/plxnative-[a-z0-9]+' rust-modules/src src | sed 's|.*/||'; grep -rhoE 'dev::(flag|read)\("[a-z0-9]+"' rust-modules/src src | sed 's/.*("/plxnative-/;s/"$//'; } | sort -u`.
  **Both halves are needed**: a path literal only ever appears in a COMMENT now, and four triggers
  (`grid`, `h265`, `playidx`, `ptype`) are named nowhere but their `dev::flag`/`dev::read` call, so
  the path grep alone silently under-reports. This line carried that grep alone and called it
  complete.
  **Every read goes through `rust-modules/src/dev.rs`, gated on the `devtriggers` cargo feature —
  read that module's doc before adding a trigger, and never open a `/tmp` path directly.** Default
  builds are unchanged; `RELEASE=1` drops the feature, and then `dev::flag` is `false` and
  `dev::read` is `None` at COMPILE time, so a public binary opens nothing under `/tmp` but its own
  logs (`capture::init` is compiled out, so there is no listener on ANY port — a compile-time fact;
  device-verified on the stable install: no FIFO and nothing on `:8910`. This line used to assert
  the device measurement alone, which could only ever have probed the one port it knew about). The same feature gates `Remote::open` and
  `capture::init` — those are structural surfaces with no path literal, which is also why
  `dev::any_trigger_present` (the whole-`/tmp` scan behind the picker suppression) lives there
  rather than being greppable. The harness is unaffected: `tests/run.py` builds with plain `make`.
  Two behaviours bite:
  `make run` clears ONLY the event log (unlike `tests/run.py`, which glob-clears triggers), so a
  by-hand run inherits whatever the last session armed; and any non-DIAG trigger left behind also
  suppresses the who's-watching picker, silently changing which screen you boot to. The
  **`tv-session` skill** drives all of this (clear → arm → launch → assert) and owns the
  screen-to-trigger recipes. Named highlights: `/tmp/plxnative-url` (override the streamed part
  URL) and **`/tmp/plxnative-playurl`** (the same, plus the LOAD DECLARATION — one JSON object,
  `{"url":…,"vcodec":…,"acodec":…,"fps":…,"dovi":{…},"atmos":…}`, which is what the pipeline test
  tier drives and the only way to declare HEVC / `"AC3 PLUS"` / Dolby for a stream no PMS chose;
  it also ENTERS the player on its own from a boot with no session, since there is no home grid to
  press OK on), **`/tmp/plxnative-replay[=N]`** (once a `playurl` stream reaches EOS,
  start it AGAIN, N times — LG checklist #46's replay half; a COUNTER rather than a lifted latch
  because `auto_tried` also guards the autoplay+playidx arm, which fetches a catalog item, so an
  unconditionally re-armable latch would loop a real playback forever. Absent = 0 = the one-shot
  behaviour every other boot has),
  **`sample.h264` / `sample.h265`** (feed the player a local raw Annex-B sample instead of
  streaming — the two names that predate the `plxnative-` prefix, and since the flavour split the
  last two runtime surfaces to stop being pinned to a shared `/tmp`: they resolve through the
  install's own root like everything else, `$(make -s print-rundir)/sample.h264`),
  `/tmp/plxnative-autoplay` (auto-press OK for headless capture), `/tmp/plxnative-autoseek` (empty =
  one seek to 140s; else a seek script: optional `gap=<ms>` + comma steps, absolute `120` or
  tap-relative `+10`/`-10` — rapid-burst seek testing), `/tmp/plxnative-ptype` (ACB playerType
  bisect knob), `/tmp/plxnative-marker[=intro|credits]` (once playing, seek to 5s before that
  server marker — the only practical way to reach the Skip Intro / Skip Credits pill, and, via a
  `final` credits marker, the whole finish → Up Next → auto-advance chain, without playing 50
  minutes of episode first), `/tmp/plxnative-failtest[=verdict|audio|novideo|none]` (force one
  variant of the full-screen **failure read-out** — the one screen that cannot be reached on
  purpose, since it needs a server that refuses, and the one most meant to be LOOKED at: it is
  shaped to survive a phone photograph in an issue thread. Live-read, so arming it mid-playback
  swaps the frame at once; pair `audio` with `/tmp/plxnative-nopass` for the PLEX PASS capsule
  line. It feeds the real `player::error_shape`, and forces the STATE only at
  `player_hud::busy` — never at `player::state()`, which the pump acts on),
  `/tmp/plxnative-testpat=<spec>` — **replace the page's picture with a SYNTHETIC ground**
  (`flat:<L*>`, `ramp`, `edge`, `checker:<px>`, `lines:<px>`, `hbars:<px>`, `hue[:L*]`, `rainbow[:L*]`,
  `solid:<deg>[:L*]`), drawn as page content so it is exactly what the tab track samples and what
  the backdrop blur sources. The remote token **`pat:<spec>`** changes it live, which is what makes a
  graded ladder one scripted run instead of a dozen launches that each land on a different hero;
  `tools/glass-patterns.py` drives that ladder and assembles the contact sheets. It exists because
  judging a glass material against whatever poster the hero happened to be showing is not
  repeatable — the hero advances on its own clock and two simulators launched together drift apart
  within seconds — and two comparisons were silently mis-paired that way before it did.
  And the Library browse set: `/tmp/plxnative-library[=N]` (boot straight into the
  browse grid on section N), `/tmp/plxnative-libosc` (perpetual grid focus sweep), and
  `/tmp/plxnative-libswitch` (cycle every switch: tabs, sort menu, unwatched, filter→genre), and the
  Search pair: `/tmp/plxnative-search[=<query>]` (boot straight into Search with the field already
  holding `<query>` — the seed is not a convenience, since neither the harness nor `sim-shot` can
  type and the TV's own keyboard is raised by a user, so without it every headless look at this
  screen is the empty state) and `/tmp/plxnative-searchosc` (sweep the result shelves' focus down↔up
  perpetually, 350 ms per step reversing every 3 s — the same cadence as `libosc`/`homeosc`). The
  oscillator does NOT reach the screen on its own: pair it with `plxnative-search`, and with a query
  the library actually matches, or `fps:search-type` has no shelves to sweep. Design, and the
  on-screen-keyboard research behind the field (three traps, two dead ends): **`docs/search.md`**.
  Plus
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
  the app's own GLES frames over TCP — **:8910 for the stable install, :8911 for a flavoured one**
  when the trigger names no port (`capture::default_port`; `make -s print-appport` is the same rule
  for the shell, and is what `tools/tv-session.sh` hands `stream-screen.py --app-port`). Two
  installs cannot both bind one port and neither side says so: the second `bind` writes one line
  into a log nobody is tailing, and the operator then watches ONE install's picture while every key
  they type goes into the OTHER's FIFO. **UI plane only**, the video overlay is invisible to it, so
  the service capture stays the only way to see real playback. Two hello-selected wire
  modes, **MPEG1-in-TS** (default) and **JPEG/PXFR** (fallback); `stream-screen.py --source
  app|auto` consumes either and its page switches itself. Both encoders and the measured numbers
  are documented where they live — `capture.rs`'s module doc (slots, wire formats, fd ownership)
  and `ff.rs`'s `venc` section (the device-verified FFmpeg ABI offsets + the RGBA→NV12-NEON
  colorspace path). `make deploy` also ships the NDK's NEON libjpeg-turbo next to the binary
  best-effort, which JPEG mode dlopen's).
  **Any `plxnative-*` file in the install's runtime root marks the boot as automated and suppresses
  the boot who's-watching picker** unless it is EXEMPT — and the exemption list is **`dev::DIAG` in
  `rust-modules/src/dev.rs`, and only that**. This line used to transcribe it as the logs plus five
  names, and the array had already grown well past that — the GPU-time log, the hardware-counter
  pair, the GStreamer pair and the focus probe are exempt as well. A transcribed list, or a count,
  rots here without anything failing, because nothing compiles this file. Read the array; its doc
  comment carries the reasoning per entry, and it is the thing to extend when a new diagnostic must
  not move the boot screen out from under the very session it was armed to watch.
  `/tmp/plxnative-token` beats the stored session entirely — so headless runs always land on a
  deterministic Home.
  `/tmp/plxnative-pickuser=<index>` forces the picker anyway and auto-picks that roster tile.
- **The binary carries NO credentials** (no compiled PMS token, no demo URL). PMS access comes
  from the signed-in session (QR login) or, for automated runs only, `/tmp/plxnative-token` — which
  `tests/run.py` always injects (it reads the owner token from the gitignored
  `src/config.local.h` on the HOST; that macro is never compiled in). An interactive boot with
  no session lands on the QR sign-in screen.
- Normal interactive flow: who's-watching picker (multi-user) → Home; D-pad/pointer to focus a
  card → **OK** opens the detail page → Play starts playback; OK toggles play/pause, LEFT/RIGHT
  scrub-seek, **BACK/Stop** returns. The strip's **last pill is Search** (a mark, not a word) — a
  peer of Home and the Library, not a page stacked over them, so BACK from it returns to Home. BACK
  at **Home's own root** is the end of that chain and raises the app's ONE decision alert
  (`ui/exit_alert.rs`) — *Cancel* focused, *Exit* in the destructive control face — rather than
  quitting on the press, which is what it did until 2026-08-21. Nothing automated depended on the
  old behaviour (`make kill`, `tests/run.py` and `tools/tv-session.sh` all close through SAM's
  `closeByAppId`), and `/tmp/plxnative-noexitconfirm` restores it for a script that wants it. Text
  entry is the **television's own keyboard**, raised by plain `SDL_StartTextInput` — the backend is
  in LG's Wayland driver, not the webOS extension API, which is why `SDL_webOS.h` looks like it has
  no keyboard. The field, the shelves and every trap in that seam are **`docs/search.md`**, whose
  status note says which halves are in the tree yet. Search is **server-only** by decision — Plex
  Discover / Watchlist catalog results are out of scope.
