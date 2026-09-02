# Detailed agent reference

This is the long-form architecture, build, portability, and verification reference shared by all
coding agents. The concise, always-loaded project contract is `AGENTS.md`; Claude imports both.

## What this is

A **real, native Plex client for LG webOS 4.5 TVs** — built toward production quality, not a
throwaway. **Build proper, reusable, well-factored components and finish them** — a shortcut is
never justified by "it's only a demo." See `rust-modules/src/ui/CLAUDE.md` for how the UI is
expected to be built. It's cross-compiled from macOS and sideloaded onto a rooted 32-bit ARM TV,
renders an Apple-TV-style gallery/shelf UI with SDL2 + OpenGL ES 2, and plays video from a Plex
Media Server (PMS) entirely in-app.

**Almost everything is Rust** in `rust-modules/src/` (UI, event loop, input, player orchestration,
the streaming/demux pipeline, and the Plex data layer), compiled to a static lib and linked in
(see the Makefile). Only two things stay C: `src/main.c` — a small **boot shim** (the event-log
handle, stderr capture, process bring-up) that then calls the Rust `plex_run()` — and
`src/starfish.c`, the **StarfishMediaAPIs C++/ACB seam**. Two more `.c` files sit beside them:
`src/svg.c` (the nanosvg rasterizer) and, since 2026-08-29, `src/crashtrace.c` — the async-signal-
safe **crash tracer**, lifted out of `main.c` into its own translation unit for one reason, that
`ci/crashtrace-test.c` links it ALONE and can therefore fault a process on purpose and check how it
died. Doing that immediately found a seven-week-old bug no log could have shown (below).

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
setup-env`), a **Rust nightly toolchain + `rust-src`** (for `-Z build-std`), CMake (Homebrew, for
the pinned Sentry Native cross-build), and `sshpass` (Homebrew, for deploy/run). See the
**`setup-environment` skill** (`.agents/skills/`) for the full one-time setup + troubleshooting.

- `make setup-env` — download + extract + `relocate-sdk.sh` the webOS NDK into `$(WEBOS_SDK)`
  (default `~/webos-ndk/…`). One-time; re-run `relocate-sdk.sh` if you move the SDK.
- `make` — build `pkg/plxnative` (the ARM binary), and, first, the FFmpeg it ships
  (`ci/build-ffmpeg.sh`; ~2 minutes cold, nothing after) plus the checksum-pinned Sentry Native
  static libraries and out-of-process handler (`ci/build-sentry-native.sh`; CMake, patched for the
  webOS glibc-2.12/ARM32 ABI). Also compiles `ci/ffabi-assert.c` against
  `vendor/ffmpeg-prefix/include` — **the headers the shipped libraries were built from**, installed
  by the same invocation that produced them — which is what proves `ff.rs`'s ABI table. **One**
  header tree per target, **one** FFmpeg. This line long said BOTH vendored trees (n3.3 and n4.0)
  and *two* tables, which was true while the app read the television's FFmpeg and had to select a
  table per firmware; bundling collapsed that into a single equality (`ffabi-assert.c` opens by
  asserting `LIBAVFORMAT_VERSION_MAJOR == 63`), and the vendored trees are gone.
  **Since 2026-08-28 there ARE two tables again, and the axis is POINTER WIDTH rather than
  version.** `make sim` builds the same FFmpeg 9.0 from the same component list for this Mac
  (`HOST=1 ci/build-ffmpeg.sh` → `vendor/ffmpeg-prefix-host`, staged into `pkg/` as
  `libavformat-plx.63.dylib`) so the simulator can demux at all, and `ffabi-assert.c` `#if`s on
  `__SIZEOF_POINTER__`, each half compiled against its own build's headers. That is not the old
  runtime major-selected table returning: this picks between two ABIs of ONE version at COMPILE
  time, on evidence the compiler holds — and deriving the second half is what found
  `AVSubtitleRect` modelled with `flags` in the wrong place.
- `make deploy` — ships the binary and the native crash handler through a `.new` + `mv` dance (a
  running process holds their inodes), the bundled FFmpeg libraries with a retirement loop for any
  previous major, and — since 2026-09-02 — **everything else in ONE scp from `DEPLOY_FILES`**,
  which is `APP_FILES` (the exact list `ipk` stages into the `.ipk`) minus those three carve-outs:
  this flavour's `appinfo.json`, both icon sizes, `pkg/splash.png`, the three font files
  (UNCONDITIONALLY, including the 21 MB CJK face — the old `test -f || scp` guard on the fonts
  meant a changed one could never reach the TV, and a separate md5 guard on the CJK face alone had
  the same failure mode by omission: it kept `pkg/splash.png`, both icons, `pkg/OFL.txt` and
  `THIRD-PARTY-NOTICES.md` off the deploy path for as long as this recipe spelled its file list out
  by hand instead of sharing `ipk`'s), `pkg/OFL.txt` and `THIRD-PARTY-NOTICES.md`. `deploy`'s last
  step is now `verify-deploy`: it md5sums every shipped file **on the television** (`ci/verify-
  deploy.py` against the local copy) and fails loudly on any mismatch or absence, which is the
  check that would have caught the splash regression instead of leaving a stale launch image on a
  debug install for weeks. **Whether webOS itself also caches `splashBackground` somewhere outside
  the app directory is not settled** — webosbrew's own `appinfo.json` guide documents SAM caching
  the *appinfo.json* JSON at boot for BUILT-IN apps only ("you will likely have to restart sam"),
  which is a different claim (metadata, not the referenced PNG's bytes) about a different install
  class (built-in, not a sideloaded native app whose directory `deploy` overwrites in place); no
  vendor or community source found describes a separate cache of the image itself. Community tier
  only, and unverified either way — settling it needs a real device (deploy a changed splash,
  relaunch without reinstalling, and see which image shows). Refuses if the flavour has never been installed, naming
  `make FLAVOR=… install`, and refuses a dev build on the stable id (see `release-guard`).
- `make run` — close any running instance, wipe this install's event log (`make -s print-eventlog`),
  launch, keep alive `RUN_SECS` (default 18s), then `cat` the on-device event log back to your
  terminal.
- `make check` — the **host** unit suite, no TV, preceded by `make lint`. Not a prerequisite of
  `all` — the cross-build must never depend on a host toolchain run. It runs `cargo test --lib`
  **twice: once on the default feature set and once with `--features hostsim`**, which is not a
  duplicate run. The host feed seam (`player/ffi_host.rs`) exists ONLY in the hostsim
  configuration, so every test that drives an access unit through `sf_feed` is compiled out of the
  default pass and cannot fail it — which is how the prime-livelock regression
  (`player::engine::prime_livelock_tests`) sat outside the gate entirely while 1398 default-feature
  tests passed. Cargo keys fingerprints by feature set, so the two coexist in one `target/` and the
  second pass costs seconds warm. See the testing section for what it does and does not cover.
- `make lint` — three **named** clippy lints (`ifs_same_cond`, `same_functions_in_if_condition`,
  `if_same_then_else`) over the whole crate, `-A clippy::all` first so nothing else can *fail* the
  gate (rustc's own warnings still print). It exists for one bug class the unit suite cannot reach:
  a **shadowed branch**. A duplicated `else if` with an empty body once hid the arm that opens the
  player's track menu — rustc does not warn on a repeated condition, and the dispatch is inside the
  SDL event loop where no host test can see it. Needs the **clippy component on nightly** (rustup's
  default profile ships it; a `--profile minimal` nightly does not).
- `make test` — `deploy` then `run` (the normal iteration command).
- `make kill` — close the app on the TV.
- **`make SYMBOLS=1 symbols`** — build with DWARF and split it into **`pkg/plxnative.debug`**, the
  file that turns an address in a stranger's crash report into a source line. The binary users get
  is stripped, so the only thing that can pair the two is the **GNU build id** — an allocated note
  that `-Wl,--build-id=sha1` puts on every link (unconditionally; it costs 20 bytes and `strip`
  preserves it). Verified end to end 2026-08-29: the full binary, the `.debug` and the stripped one
  all carry the same id, and `addr2line -e pkg/plxnative.debug` resolves an address the stripped
  binary answers `?? ??:0` for. **It is opt-in for one reason and it is not build time** — a
  debuginfo cross build is 30 s cold and the artifact that SHIPS is unchanged (6.93 MB stripped, a
  hair *smaller* than without) — but its target dir is 356 MB, and this repo already keys a
  separate `rust-modules/target*` per configuration and multiplies that again per worktree.
  `SYMBOLS` is in the `RUST_CFG` stamp beside `RELEASE`, and it has to be: a debuginfo build and a
  plain one produce **different build ids from identical sources**, so without the stamp
  `make RELEASE=1 ipk` followed by `make RELEASE=1 SYMBOLS=1 symbols` would hand you a `.debug`
  matching nothing that was ever shipped. Cut both in one invocation:
  `make RELEASE=1 SYMBOLS=1 ipk symbols`. `make symbols` without the flag REFUSES rather than
  writing the empty shell `objcopy --only-keep-debug` produces from a binary with no DWARF.
  **`SYMBOLS=1` is sticky the way `RELEASE=1` is: pass it to EVERY invocation in the session.** It
  is in the stamp, so a bare `make run` after `make SYMBOLS=1 deploy` deletes `pkg/plxnative` at
  parse time and leaves you unable to symbolize the crash you just captured — the stamp working as
  designed, but it costs a rebuild to notice. **Device-verified end to end 2026-08-29**: a
  deliberate SIGSEGV on the set resolved through `pkg/plxnative.debug` to
  `dev::crash_on_purpose at rust-modules/src/dev.rs:218` — file and line, from a binary the
  television runs, matched by build id alone.
  `make SYMBOLS=1 sentry-symbols` additionally runs Sentry CLI's local DIF check and uploads that
  exact binary/debug pair with source context; it requires `SENTRY_AUTH_TOKEN`. The release
  workflow treats a missing token, unusable DIF or failed upload as a release failure, because an
  accepted native event with no matching DIF is not a working crash report.
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
  FIFO and the capture listener — see `rust-modules/src/dev.rs`). **It also decides WHICH VERSION
  THE BINARY SAYS IT IS**: the Makefile exports `PLX_RELEASE`, and `rust-modules/build.rs` publishes
  `PLX_VERSION` as the `Cargo.toml` version exactly for a release build and as the **next MINOR plus
  `-dev`** for every other one — `0.5.0` published, `0.6.0-dev` in the tree. The minor rather than the
  patch because development is TRUNK-BASED here: features land on main, so the next release cut from
  it is a minor (or a major, which no build script can predict); a patch is cut from an existing
  minor's own line, where trunk's number is not the question. It also makes the semver ordering
  mean something — `0.6.0-dev` precedes `0.6.0`. That is the string every
  surface reports (X-Plex-Version, the Sentry release, PostHog's `app_version`, the lab snapshot, the
  photographed diagnostics panel); before it, a release commit left the whole tree claiming to BE the
  release it had just cut, and nothing downstream could separate a working tree from the shipped
  artifact. The suffix never reaches `pkg/appinfo.json` or the control file — LG takes three integers
  and nothing else — so a developer flavour's package is labelled `0.5.0` while its binary says
  `0.6.0-dev`, deliberately; `ci/check-package.py` grades both directions on the packaged bytes. It
  must be on
  EVERY invocation that produces or ships the binary (`make RELEASE=1 deploy`, **not**
  `make RELEASE=1 && make deploy`, which rebuilds as dev and ships that). `deploy`/`ipk` echo
  which configuration they shipped. Switching configuration DELETES `pkg/plxnative` at Makefile
  parse time — deliberately: make 3.81 on macOS compares mtimes at one-second granularity and
  decides staleness from a stat taken before prerequisites run, so no stamp-mtime scheme works.
  Each feature set also gets its own `--target-dir`, because cargo does not hash its output and
  would otherwise report the dev build fresh while the release `.a` sat at that path.
  **`make check` cannot see a break in this configuration** — it builds the default feature set —
  **but the PR gate now can**: `.github/workflows/ci.yml` type-checks `--no-default-features` and
  both `hostsim` configurations on every push. Until that landed, `--no-default-features` was first
  compiled during a release cut, i.e. after the change had merged. The
  **`PostToolUse` hook** (`.claude/hooks/release-config-check.py`) stays and is now the fast path
  rather than the only one — it catches the break before the push, and CI does not run in somebody
  else's checkout. It type-checks after
  every edit to a `rust-modules/src/**.rs`; it costs well under a second warm, because cargo keys
  fingerprints by feature set and the two configurations coexist in one `target/`. The hazard it
  guards is hand-written `#[cfg(feature = "devtriggers")]` PAIRS, where a spliced-in function
  swallows a neighbour's attribute — `dev::latched_flag!` exists to avoid most of them.
- **`LAB=1`** adds a THIRD cargo feature, `lab-diagnostics` — the **Cloud Lab bridge** that gets
  logs off and app-level commands onto a television in **LG Cloud Test Lab**, where there is no
  ssh, no console, no stdout and no way to download a file, so the entire `/tmp` trigger surface
  and every recipe in this file is unreachable. In a lab build `crate::log` also feeds a bounded in-memory ring (4000
  records / 768 KiB), a configured remote key or a **Send diagnostics** row in the account /
  player-overflow menu snapshots it together with `player::Diag`, `webos` and `devcaps`, scrubs it
  again, gzips it and POSTs it over **pinned** TLS to `tools/plxnative-lab` on the dev Mac. An
  opt-in outbound HTTPS long poll carries the same bounded synthetic-input token grammar back to
  the app's SDL main thread; `tools/plxnative-lab send down ok wait:1000 diag` queues and waits for
  delivery acknowledgements. It adds no dependency and cannot run a shell or control another
  webOS process. Unlike
  `devtools`/`devtriggers` it is **not in the default set at all**, so it cannot ship by forgetting
  a flag — and `make LAB=1` refuses the stable id, and refuses to build without the session file
  `pkg/lab.json` (gitignored, a live secret, in `outbound-guard.py`'s `PRIVATE_FILES`;
  `ci/check-package.py` refuses any non-LAB package that carries it). The receiver logs the PEER
  ADDRESS of every request, which is the only thing that tells "the television reached me from the
  internet" apart from "something on the LAN hairpinned". It composes with `RELEASE=1`
  and with `make sim`, each getting its own `--target-dir` for the reason every other feature set
  does. Full account: **`docs/lab-diagnostics.md`**. The trigger is a code LIST in that file rather
  than a constant, and the reason is worth carrying: the colour buttons are **`wcode` 486 RED /
  487 GREEN / 488 YELLOW / 489 BLUE** (device-measured 2026-08-26), while the STANDARD evdev
  colour codes are a dead end on this firmware — `KEY_RED`/`KEY_YELLOW`/`KEY_BLUE` translate to
  nothing at all and `KEY_GREEN` to a 504 this remote never sends. The one answer that looked
  derivable offline was derivable WRONG; `docs/remote-keys.md` §9 is the record.
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
cannot open a URL — http or https, on any firmware. Every byte reaches the demuxer through **the
custom AVIO**, whichever transport happens to be under it (`stream.rs` for http, `curlio.rs` for
https), and that is not an accident of the current pipeline but a build-time fact you cannot route
around. "Does the TV's FFmpeg have https?" is therefore not
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
set, `.agents/skills/decompile-tv-lib/` harvests and decompiles them; for OTHER releases we have no
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
  the answer decides a device session, settle it with `.agents/skills/decompile-tv-lib/`.

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
is all Rust: `PMS HTTP GET` → demux (**`ff.rs`, over a custom AVIO on the
FFmpeg the app BUNDLES** — not the television's; see the linking section, and note ours is built
`--disable-network`, so the AVIO is the *only* way bytes reach it) → AU queues with backpressure
(`aq.rs`) → the pump `Feed()`s the Starfish
pipeline. Two worker threads (demux, media/load) sit beside the main loop, which owns all
ACB/Starfish control calls. **That GET has TWO transports and the part URL's SCHEME picks one**,
once, in `ff::demux`: `http` reads through `stream.rs`'s raw socket, `https` through
**`curlio.rs`** — libcurl's *multi* interface behind a `read`/`seek`/`size`/`status`/`abort` pull
source, so `ff.rs` never learns curl-multi mechanics. It exists because LG's reviewers have no PMS
on their LAN and stream from the public internet, which `stream.rs` cannot reach: it speaks
cleartext. So **libcurl is used by two modules for two jobs** — `net.rs` for plex.tv plus HTTPS PMS
control calls, `curlio.rs` for the media bytes — and each binds its own `dynlib!` table,
which the linking section explains is load-bearing rather than tidy.

## Key files

- `Makefile` — build/deploy/run/ipk; toolchain, the bundled-FFmpeg build + staging + its ABI gate
  (one header tree, not the old dual one), TV ssh creds.
- `src/main.c` — the **boot shim** (event-log/stderr setup, process bring-up); calls the Rust
  `plex_run()`. `src/crashtrace.c` (+ `crashtrace.h`) — the **fatal-signal tracer**, its own TU so
  the signal path can be tested; `src/crashfmt.h` is its pure half. **Both halves are host-tested in
  `make check`** — `ci/crashfmt-test.c` grades the parsing, `ci/crashtrace-test.c` crashes seven
  processes on purpose and checks the record AND the exit status. `src/starfish.c` — the
  StarfishMediaAPIs C++/ACB seam. `src/svg.c` — nanosvg rasterizer. `src/sentry_context.c` — the
  narrow C wrapper that keeps Sentry's opaque by-value object ABI out of Rust. These five are the
  entire normal C side (`gpdebug.c` is an opt-in allocator instrument). Reach for
  `/tmp/plxnative-crashtest=<segv|abrt|bus|ill|trap>` to fault the app deliberately ON the
  television — `segv` is a real null write, the rest are `raise`.
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
  (`rust-modules/src/stream.rs` — blocking HTTP/1.1 over a raw TCP socket: hostname or IPv4/IPv6
  address, and a
  body delimited by `Content-Length`, by close, **or by `Transfer-Encoding: chunked`, which it does
  decode** (`HttpStream`'s `chunked`/`chunk_left`, the header match in `http_open`, and
  `hs_next_chunk`). This line claimed "no chunked decoding" long after that stopped being true,
  which makes `stream.rs` read as less capable than it is and sends work to `net.rs` that it would
  have handled — its remaining transport disqualifier is **TLS**: it resolves through
  `getaddrinfo`, walks either address family, and speaks cleartext. `aq.rs` — one-producer/
  one-consumer AU FIFO with byte-cap backpressure. Both are Rust ports of the deleted C headers;
  the hand-rolled `mkv.rs` demuxer they fed is retired — `ff.rs` is the only demux path.)
- `rust-modules/src/diag/` — **the redaction pass and the diagnostic plumbing every off-device
  report shares**. `scrub.rs` is the one that matters and it is **UNGATED**: `crate::log` runs
  `scrub_local` on every line in every build, so credentials, hosts, bare addresses, Plex GUIDs,
  search queries and this household's names are rewritten **before the write**, not on the way out.
  **Two exits, differing in exactly one respect** — `scrub` (network) may DROP a line it cannot
  make safe; `scrub_local` (disk) may only rewrite one, because a line silently vanishing from the
  primary debugging surface is worse than a leaky one. `ring.rs`/`zlib.rs` stay feature-gated to
  their consumer. Lifted out of `lab/` on 2026-08-29 — which also fixed the fact that the 31
  assertions guarding this function **never ran in `make check`**, `lab/` being wholly behind a
  feature the default gate does not build.
  **A title cannot be scrubbed** (nothing distinguishes it from `task: spawn 'labup' REFUSED`), so
  the mechanism for viewing content is that call sites do not write it, pinned by a test that greps
  the tree — see `no_log_call_site_interpolates_viewing_content`. Adding a `log(&format!(…))` that
  interpolates an item title, a search query or subtitle text will fail `make check`.
  Identities come from `plex::session::publish_identities`, PUSHED on load/save; the scrubber must
  never call `session::peek()` from the log path — it takes the session lock and reads files, which
  deadlocked the whole `auth` test block and put five `read`s on every log line.
- `rust-modules/src/dynlib.rs` — the runtime library binder (`dlopen`, by SONAME candidate list or
  by absolute path). **Four** callers in a lab build and three in every other, each for its own
  reason: `net.rs` binds **curl** by candidate list because its SONAME moves between releases;
  `ff.rs` binds the **bundled FFmpeg** by absolute path because ours ships beside the binary, on
  no library search path — not because any
  version varies; and `curlio.rs` binds **`curl_multi_*` in a SECOND table of its own**, from the
  same candidate list, because `load_into` is all-or-nothing and a set missing one multi symbol
  must still be able to SIGN IN. That table is frozen to the oldest supported set:
  `curl_multi_poll`/`curl_multi_wakeup` resolve on the dev Mac, are absent on the dev television,
  and first appear at webOS 7.4.0 — so binding them would have emptied this table on four of the
  nine gated releases. The fourth is `diag/zlib.rs`, which binds **one** symbol — `compress2` — in
  a table of its own so that a television without libz degrades to an uncompressed upload rather
  than emptying anybody else's table; it exists only in a `lab-diagnostics` build, which is not the
  default set, so an ordinary binary really does have three. (**ACB** is the same idea but not this
  module: `src/starfish.c` is C and does its own `dlopen`.) Replaced `stub/`, which is deleted.
  `tools/fwcompat.py` grades the result;
  `docs/webos5-port.md` is the full account.
- `docs/release-notes/` + `docs/release-audits/` — **the two halves of a release, split
  2026-08-29**: the note is the body CI publishes, written for a television owner and deliberately
  short; the audit is the evidence (package facts, hashes, `DT_NEEDED`, payload inventory,
  provenance, firmware matrix, LGPL position), whose measurable half `ci/gen-release-audit.py`
  READS OUT OF THE .ipk during the release run rather than anyone typing it. Each directory carries
  its own standard as a README, and `ci/check-package.py` gates both documents — including, for
  notes, that prose is NOT hard-wrapped and every link is absolute, because a release body is
  rendered at a width nobody controls and resolves no repo-relative path. The `cut-release` skill
  is the procedure.
- `docs/install-and-verify.md` — the invariant half a release note used to repeat every time:
  which asset is which, both install routes and why the Homebrew Channel wins, how to check the
  sha256 per platform, and what the app writes, reads and reaches on your television.
- `pkg/` — deployable payload: `appinfo.json` (native app manifest), `plxnative` binary, icons,
  `appfont*.ttf`, and the prebuilt `.ipk`.
- `ipkroot/` — ipk staging (`ctl/control`, `data/`, `debian-binary`); assembled by `make ipk`.
- `tools/capture-screen.sh` — pull the TV screen (incl. video plane) to a local image.
- `tools/plxnative-lab` — the **Cloud Lab diagnostics/control receiver** (host-side, python3 stdlib only):
  `start` mints a session (id, bearer secret, self-signed certificate, SPKI pin), writes
  `pkg/lab.json` for `make LAB=1`, listens on TLS and opens a TEMPORARY **UPnP IGD** mapping on the
  router for a fixed external port; `status --json` is what an agent polls (receiver, mapping,
  last-upload age, control-poll age, the webOS/board/model/version of the set that uploaded);
  `send <tokens...>` queues bounded app-input commands and waits for their acknowledgements;
  `clear` cancels stale queued/in-flight input before a disconnected TV comes back;
  `logs [--follow] [--since 5m]` prints snapshots as JSONL; `stop` removes the mapping and VERIFIES
  it is gone. The public routes are `POST /v1/diag` and `POST /v1/control/poll`; enqueue/status are
  loopback-only even to a caller holding the session secret. Auth precedes body reads, every body
  and queue is capped, and there is no filesystem serving or subprocess. `tools/plxnative-lab
  selftest` proves upload, ordered redelivery/ack and refusal paths on loopback with no television,
  and runs inside `make check`.
- `tools/netcond.py` — **network-conditioning TCP proxy** (host-side), for the failures a healthy LAN
  cannot produce. Sits between the TV and the PMS (`--listen 32499 --target 127.0.0.1:32400
  --allow-client <TV_IP>`; the PMS runs on the dev Mac) and makes the server misbehave on demand
  via `/tmp/netcond.mode`. A non-loopback listener refuses to start without an allowlisted client:
  PMS request URLs carry credentials, so a LAN-wide forwarding proxy may never be open by default.
  Modes:
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
  **`tests/run.py` owns one for the whole SERVER tier** since 2026-08-27, so a case can declare a
  `link_profile` — legs of `{"at_s": …, "mode": …}` anchored at the app's first log line — and be
  graded over a link the harness controls rather than over whatever the LAN was doing. It is
  fail-closed and has to be: the app's primary server is `plex_run(PMS_HOST, PMS_PORT)` plus the
  injected `plxnative-token` (`plxnative-servers` is strictly ADDITIVE and cannot move the
  primary), so the link is conditioned only if the DEPLOYED BINARY was built with `PMS_PORT`
  pointing at the proxy — which the harness can read out of `src/config.local.h` but never
  arrange. When it cannot bind, or when the binary talks to the server directly, every case
  naming a `link_profile` SKIPS with that reason in the summary, because a shaped assertion on an
  unshaped link is a false pass. A `link_profile` also disables early exit: the interesting leg is
  never the first.
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
- `docs/lab-diagnostics.md` — the **Cloud Lab bridge, end to end**: diagnostics, authenticated
  long-poll commands, topology through the router, wire formats, redaction, pinning, receiver
  hardening, the `LAB=1` loop, the measured colour-key table, and — §11 — exactly what has been
  proven on the host and what still needs hardware.
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
  session) and drops to Home. On foreground `0x106` it tracks one exact Load attempt at a time,
  follows reducer-approved superseding or rollback attempts, retries an exact failure without
  repeating route preparation, and applies the saved clock only after `Started`. In-app
  Home/Settings are *overlays* and do **not** fire these — only a real OS app-switch does. Preserve
  the suspend/reload pairing if you touch playback or routing.
- **Crash forensics has two layers.** With error-report consent and a compiled Sentry endpoint,
  Sentry Native's patched ARM32 backend replaces the signal disposition and wakes the shipped
  `sentry-crash` daemon. The dying process stays stopped while the daemon copies its `ucontext`,
  enumerates the other Linux LWPs, `PTRACE_ATTACH`es them, snapshots PC/SP/FP with
  `PTRACE_GETREGS`, and keeps them stopped while it walks every APCS frame chain. That lifetime is
  the Linux counterpart of KSCrash's suspend → context → unwind → resume sequence; enumerating
  `/proc/<pid>/task` alone only produces names and zeroed contexts. The crashed thread keeps up to
  128 frames and each other thread up to 32 to reduce pressure on the 256 KiB durable-record
  ceiling; the importer still rejects an oversized envelope rather than claiming a bound for an
  arbitrary 256-LWP process. The JSON therefore carries ARM registers and real multi-frame stacks
  for all successfully captured threads, plus modules and both Linux-kernel and webOS firmware
  context.
  The SDK has **no HTTP transport and writes no minidump**: it launches the
  same `plxnative` binary in spool-only mode, which moves the bounded envelope into the install's
  runtime root. A healthy launch rejects user/request scope, strips path prefixes and queues the
  event through the existing consent-aware sender. `-fno-omit-frame-pointer` / Rust
  `force-frame-pointers=yes` are therefore crash-reporting ABI, not optional debug flags.

  The patched ARM handler waits up to 30 seconds for that walk. The upstream 10-second budget was
  too short for the first cold crash on this Cortex-A9: the parent died while the daemon was still
  opening `/proc/<pid>/maps`, producing an accepted envelope with no modules and therefore no
  symbolication. Warm crashes usually finish within the original budget; the longer ceiling is a
  failure bound, not an added delay after a completed report.

  The always-armed fallback is still the C tracer (`main.c`). It writes a startup `img:` marker
  (build id, load address, mapped size), then on a fault logs PC/LR, **the ARM registers around
  them** and the `/proc/self/maps` line(s) containing either. Native always chains this saved
  handler after the daemon's attempt (and immediately when the daemon is unavailable), so the
  bounded local record survives even a daemon that reaches `DONE` after failing to write its
  envelope. On the next healthy boot the native envelope is imported first; a matching local
  record is consumed by build id + signal, so one process death still becomes one Sentry event.
  Closing or withdrawing consent restores the tracer as the primary handler. It **re-raises to
  `SIG_DFL`** so
  SAM sees a real signal death (`exit_status: 11` for a SIGSEGV — device-verified 2026-08-29).
  **This fallback does NOT also get a system crashd backtrace, and this line used to say it did**:
  `core_pattern` on this firmware is the bare string `core`, so the report chain starts from
  a core FILE, and `setrlimit(RLIMIT_CORE, 0)` means none is written. Two deliberate SIGSEGVs
  produced the signal status and no `/var/log/reports/librdx/` entry. Suppressing cores stays right
  — 615.6 MB shared partition, 125.9 MB free, ~200 MB core — so the two are simply exclusive, and
  the fallback evidence is its own fault event plus SAM's status. Call that local C record a
  **fault event, not a backtrace** — `backtrace()` is not async-signal-safe and ARM unwinding in the
  crashing process commonly stops at `gsignal()`, so two frames plus registers plus the faulting
  module is the honest ceiling.
  **Two things about it were broken until 2026-08-29 and neither was visible in a log** — and the
  first of them had been **written down and left undone for five weeks**, as finding **C3** of
  `docs/architecture-review-2026-07-26.md`, which named the same six unsafe calls and prescribed the
  same fix ("use `write(2)` into a pre-opened fd with a preformatted buffer"). It was filed
  *medium / Wave 1*. Worth knowing when reading either: only the SECOND was a discovery.
  (1) The handler was not async-signal-safe: it called `fprintf`/`fopen`/`fgets`/`sscanf`/`fclose`,
  none of which is on POSIX's list, so a fault inside the allocator or while another thread held a
  stdio lock could have deadlocked or refaulted and lost the report — in the crash class most worth
  having one for. It is now `open`/`read`/`write` and hand-rolled formatting, with both descriptors
  opened before `sigaction` arms it. (2) **THE RE-RAISE DID NOT RE-RAISE**, from the day it was
  added (2026-07-09) until then, so every sentence anywhere in this repo claiming crashd captured a
  backtrace for this app was FALSE for seven weeks. `sigaction` without `SA_NODEFER` masks the
  signal for the duration of its own handler, so `raise(sig)` only marked it pending, returned 0,
  and fell through to the `_exit(128 + sig)` beneath it — commented "only reached if raise() somehow
  returns", and reached every single time. The result was a CLEAN EXIT: no core, no
  `/var/log/reports/librdx/` report, and `WIFEXITED` rather than `WIFSIGNALED` for SAM. The commit
  that introduced it was fixing this same failure in an earlier form (a bare `_exit(3)`) and swapped
  one clean exit for a more plausible-looking one. The fix is a `sigprocmask(SIG_UNBLOCK)` before
  the `raise`. **Consequence for anyone reading an OLD device log: a SAM `exit_status` of 35584
  (`139 << 8`) is a SIGSEGV, not an exit** — and there will be no crashd report beside it to find. The PURE half
  (record formatting, and deciding what a maps line means) lives in **`src/crashfmt.h`** so that
  `make check` can compile and RUN it on the Mac — `ci/crashfmt-test.c`, which is where the parsing
  bugs of this tracer have historically been, and which on being written by watching it fail
  disproved a justification `main.c` had carried since the tracer existed. Two logs, both in the
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
exists because nothing compiles this Markdown, which is why the paragraph below has to open by telling
you its own numbers are wrong.

> ### FIXING A REPORTED BUG STARTS WITH A TEST THAT REPRODUCES WHAT WAS REPORTED, AND YOU MUST WATCH IT FAIL.
>
> **The first artifact is not the fix. It is a failing test that reproduces the SYMPTOM AS
> DESCRIBED** — the maintainer's own words, their scenario, their sequence — and you have to
> *see it red* against the broken build before you touch anything. A test written after the fix
> and green on the first run is evidence about nothing: it proves the code does what it now does.
>
> **This is not a style preference; it has already failed here in exactly the way it always
> fails.** On 2026-08-29 a client-side bug killed a playback after a seek. The fix was landed
> first and the regression case written after it, and the case was green on the television — so
> it looked done. Replayed against the log of the BROKEN build, that case scored **five green
> assertions out of five on a run that died** with `HLS segment was not produced in time`. Two
> separate reasons, both of which generalise: the global `timeline_climb` counted the seek
> DISCONTINUITY as 313 s of progress, and `no_playing_error` greps the *Starfish* error surface,
> which a death on the acquisition side never reaches. It took `no_demux_failure` plus a
> post-target progress floor (`min_climb_after_s`) to make the case discriminate — and only the
> replay against the broken log could have shown that, because on the fixed build every version
> of the case looks identical.
>
> So, in order: **reproduce → watch it fail → fix → watch it pass → keep the failing artifact.**
> Two rules fall out of it.
>
> * **A test you cannot run against the broken code is not yet evidence.** Sometimes the fix
>   changes the signature the test calls (it did here: `prime` lost its `generation` parameter, so
>   the unit test cannot compile against the old code). Then simulate the defect narrowly, watch
>   the test go red, and **say in the commit that the red was simulated rather than historical** —
>   it is a weaker claim and must not be reported as the stronger one.
> * **Keep the broken run's log.** It is the only thing that can answer "would this test have
>   caught it", and that question cannot be asked of a fixed build at all. Replaying a case's real
>   assertions over a saved failing log costs seconds (`run.evaluate(case, lines)` off a saved
>   `plxnative-events.log`) and is the cheapest audit in this repo.
>
> The host simulator makes the whole loop cheap and takes no television: `tools/abr-scenario.sh`
> builds and runs one scenario end to end, so an A/B of two builds over one scenario is minutes,
> not a device session.

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

**Tier 1 — `make check` (host).** `cd rust-modules && cargo test --lib` runs the whole
host suite on the dev Mac, no TV involved — and `make check` runs it a SECOND time under
`--features hostsim`, because the host feed seam only exists there and the tests that need it are
compiled out of the first pass (see the build section). **Treat every test COUNT in this section as
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
navigation, every screen, and the whole Plex data layer.
**And since 2026-08-28 it STREAMS — real HTTP, real demux, real HLS, the real adaptive
controller.** `make sim` builds a HOST copy of the same bundled FFmpeg 9.0 from the same
`ci/build-ffmpeg.sh` component list (`HOST=1`, into `vendor/ffmpeg-prefix-host`, staged into
`pkg/` as `libavformat-plx.63.dylib` beside the ARM `.so.63`), and `ff.rs` carries a second ABI
table selected on `target_pointer_width` with `ci/ffabi-assert.c` holding both. Arm
`plxnative-clocksink` (`player/ffi_host.rs` — AUs accepted and discarded, a presentation clock
clamped to the last fed PTS, position reported at the television's measured 5 Hz) and the whole
pipeline between the socket and the decoder runs on the Mac: both AVIO transports, `ff.rs`'s
demux, the AU queues and their byte-cap backpressure, the feed-ahead throttle, rung transactions,
seek. Measured the day it landed: 94 `abr:` lines and a rung commit in one 30 s host run against
`tests/serve_fixtures.py`. Until then this half was device-only and `make sim` said so
(`ff: FFmpeg unavailable — the app runs, playback will refuse`), which is why the ABR work was
pinned to the one-television mutex.
It still CANNOT answer frame rate (different
GPU — every simulator heartbeat carries **`sim=1`** so a pasted log cannot be mistaken for a
device measurement), text rasterization, or anything about **LG's decoder** — resource-allocation
refusals, the ACB video-plane bind, the Load payload's Dolby declaration, `SOUND_ERROR_019`, frame
pacing, which codecs the panel takes. Nothing decodes: the clock sink throws every AU away, and a
Mac decodes things that television will not and the reverse. Without the trigger, Play still lands
on the real failure read-out, which is what the UI work wants to see.
Bugs it has already found in DEVICE code: the glyph upload
ignored `SDL_Surface::pitch` (`text.rs`), `dev`/`remote`/`log` all hardcoded `/tmp`, and — from
deriving the ABI table at a second pointer width — `AVSubtitleRect` was modelled with `flags`
last where the header puts it before `type`, so `type_` read `flags` and on 64-bit `flags` landed
one word past the end of the struct.
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
(count it with `--list --server`, never from here; `--fps` for the perf gates), and `make test` = `deploy` + `run`.

**Tier 2 is TWO SUITES on one television, and since 2026-08-22 the DEFAULT IS THE SYNTHETIC ONE.**
A bare **`./tests/run.py`** now runs the SYNTHETIC tier (generated clips, no Plex — take the count
yourself with `./tests/run.py --list`, which is the only census that cannot rot; the number written
here went stale inside the branch that wrote it, in one commit); the
library-backed cases everything above describes are **`./tests/run.py --server`**, and `--fps` /
`--fps-player` imply `--server` because those scenes navigate a real signed-in Home. `--pipeline`
still parses (it names the default); pairing it with `--server`/`--fps` is refused rather than
silently resolved. The inversion is about what the obvious command should mean: the default has to
be the thing that runs for everybody, needs no credentials, touches nobody's watch history, and
answers "is the PLAYER broken" — charging a PMS, a token and a filled-in overlay for typing
`./tests/run.py` meant most people could not type it at all. **Never ship on the default alone**
(what it cannot see is three paragraphs down). The server tier is still the right shape for what it
grades — SELECTION: `/decision`, direct-play vs transcode, track menus from PMS metadata, markers,
resume, the `/:/timeline` reporter — which is also why it needs somebody's library.
**The synthetic tier is the PLAYER PIPELINE, with no Plex anywhere.** A generated clip (`make fixtures-pipeline`, ~0.9 GB flat in
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
  **Beside it since 2026-08-27 is `play=<pm>`: media time advanced per WALL millisecond, in per
  mille — 1000 is the film running at speed and 670 is it crawling — and it is the ONLY field on
  that line that can see a slow film.** `fps=` counts our GL swaps and sits at 60 through a stream
  the television is decoding at two-thirds speed; every buffer number the adaptive controller reads
  is a RESERVE, and a reserve is media time measured against the same playhead, so when the
  playhead slows the reserve stops draining and `slope`, `min_buf_ms` and `draining()` all read
  healthy at exactly the moment the picture is worst. `max_stall_s` is blind from the other side —
  it grades the clock STOPPING, and this is the clock advancing too slowly. The harness derives the
  same quantity from `pos=` alone (`playback_rate`, reported on every case's `timeline_climb`
  evidence, asserted only where a case declares `min_play_rate_pm`), so an old log is still
  readable. There is deliberately no magnitude gate on `play=`: a seek reads as a huge or negative
  value and a catch-up leg as something above 1000, and both are real observations.
  `docs/measurements/local-original-blind.md` is what it found first.
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
  given oscillators and real `fps_floor`s, which is the fix that note asks for. The other three
  `loop_floor`-only scenes are the player-tier overlays (`info-panel`, `track-menu`, `chapters-panel` — take the list from `./tests/run.py --list --server`, not from here) and need no note, because
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
  instruments and their structural blind spots are `docs/backdrop-blur-profiling.md`. **A third profiler mode, `/tmp/plxnative-cpuprof` (2026-09-02), times every `ui::profile::phase` on the RENDER THREAD** — inclusive wall time, every phase at once, no `glFinish`, a `~src` suffix for the blur source pass's copy of a phase — and it is the one that can read a frame the frame-drop detector reports as `draw=24ms swap=0.3ms`: on this driver the wait for the GPU lands in the frame's FIRST framebuffer-0 command, i.e. inside `hm.clear`, so a fat `draw=` is not CPU work until this mode says which phase holds it. That is how the Home hero regression was read (`docs/backdrop-blur-profiling.md`, the 2026-09-02 section): 26 ms in `hm.clear`, 2 ms in everything Home actually computes. For by-hand judder hunts: `/tmp/plxnative-framedrop` logs any frame over 22ms (or over
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
  minutes of episode first),
  `/tmp/plxnative-failtest[=verdict|audio|novideo|stream|connection|tv|none]` (force one
  variant of the full-screen **failure read-out** — the one screen that cannot be reached on
  purpose, since it needs a server that refuses, and the one most meant to be LOOKED at: it is
  shaped to survive a phone photograph in an issue thread. Live-read, so arming it mid-playback
  swaps the frame at once; `stream`, `connection`, and `tv` exercise the runtime media-source,
  interrupted-transfer, and native-pipeline reasons; pair `audio` with
  `/tmp/plxnative-nopass` for the PLEX PASS capsule line. It feeds the real
  `player::error_shape`, and forces the STATE only at
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
- **`/tmp/plxnative-consent` is the same escape hatch for the CONSENT question**, and it exists
  because that screen is suppressed BY the presence of any trigger — so without an override it is
  the one screen in the app that cannot be reached headlessly at all: arming anything to reach it
  is what hides it. It forces the question regardless of a stored decision, so it is also how the
  screen is re-examined after answering once.
- **The binary carries no credential that grants access to anything of YOURS** — no compiled PMS
  token, no demo URL, and never the Sentry auth token, which can read and delete the project and
  lives only as a GitHub secret. A RELEASE build does carry two **write-only ingest credentials**
  (`PLX_SENTRY_DSN`, `PLX_POSTHOG_KEY`, compiled in via `option_env!`): they permit sending to a
  project and reading nothing from it, `strings` finds them, `ci/gen-release-audit.py` prints them
  into the audit on purpose, and `release.yml` REFUSES to publish without them. The distinction is
  the point, and this line read "NO credentials" flatly — which stops a reader before they reach
  it. A build with no credential compiled in cannot report at all, which is the guarantee that
  replaced the cargo feature that used to claim it. PMS access comes
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
  no keyboard. The field, shelves, test seams and every trap in that path are documented in
  **`docs/search.md`**. Search is **server-only** by decision — Plex
  Discover / Watchlist catalog results are out of scope.
