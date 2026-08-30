# plxnative — native webOS build (cross-compiled from macOS with the webOS NDK)
#
# Toolchain: the webosbrew "native-toolchain" buildroot SDK (GCC 12, glibc 2.12,
# armv7-a soft-float). Install it once with `make setup-env` (or see the
# setup-environment skill); override the location with WEBOS_SDK=… if you put it
# elsewhere. The SDK ships a real sysroot with the TV's own SONAME'd libraries
# (SDL2/GLESv2/wayland/glib/luna-service2 + LG's libplayerAPIs/libpf), so we link
# against the real thing.
#
# THE STUB .so TRICK IS GONE. It existed so FFmpeg and libcurl — absent from the
# sysroot, or present under the wrong SONAME — could be named in DT_NEEDED and
# resolved to the TV's real libraries at runtime. But a DT_NEEDED entry is a hard
# requirement for one exact SONAME, and those SONAMEs move between webOS releases
# (FFmpeg 55->57->58->59->60, curl .so.5->.so.4, libAcbAPI deleted at 5.0), so the
# trick pinned the binary to webOS 4.x: on anything newer the loader killed it at
# exec(), before main, before the event log existed. Those libraries are now
# dlopen'd by SONAME candidate list instead — rust-modules/src/dynlib.rs, and the
# video-plane comment at the top of src/starfish.c. `tools/fwcompat.py` grades the
# result against 14 real firmware inventories without leaving the desk.
#
# make          — build pkg/plxnative
# make setup-env— download+extract+relocate the NDK into $(WEBOS_SDK)
# make deploy   — scp binary + appinfo to the TV (rooted, root@TV)
# make run      — launch on TV, keep alive $(RUN_SECS)s, fetch event log
# make test     — build + deploy + run
# make kill     — close the app on the TV
# make ipk      — repackage pkg/<app id>_<version>_arm.ipk (version from appinfo.json)
# make install  — install THIS flavour on the TV from its own .ipk, then deploy into it
# make uninstall— remove this flavour from the TV (refuses the stable id)
#
# FLAVOR selects WHICH INSTALL every TV-facing target talks to: `debug` (the default —
# com.beb.plxnative.debug, its own tile, its own sign-in, its own /tmp root) or `stable`
# (com.beb.plxnative, the app users install). See the FLAVOR block below for why the default is the
# developer one. A flavour must be `make FLAVOR=… install`ed once before `deploy` can reach it.
#
# RELEASE=1 drops BOTH default cargo features: `devtools` (the on-screen counter) and
# `devtriggers` (the /tmp trigger surface, the remote FIFO, the capture listener — see
# rust-modules/src/dev.rs). It must be on EVERY invocation that produces or ships the
# binary — `make RELEASE=1 deploy`, not
# `make RELEASE=1 && make deploy`, which rebuilds as dev and deploys that. Both targets echo
# which configuration they are shipping.

# Which television. `make TV=1.2.3.4 …` overrides for one invocation; otherwise it comes from the
# gitignored `.tv-host` (one line, an IP or hostname), so this repository carries nobody's home
# network. `tools/` reads the same file via $TV_HOST. Absent, the targets that need a TV say so.
# The second `cat` is for a LINKED WORKTREE, and it is not a convenience: `.tv-host` is gitignored,
# so a worktree cut from this repo has none — and worktrees are exactly where the parallel agents
# live. Without it, the checkouts most likely to collide over the one television are also the only
# ones that cannot ask `tools/tv-lock.sh` who is holding it, which is how a lane ends up dialling
# `root@` out of somebody's memory instead. `--git-common-dir` is the MAIN checkout's `.git` from
# anywhere in the worktree family (and plain `.git` in the main one, where the first cat already won).
TV       ?= $(strip $(shell cat .tv-host 2>/dev/null || cat "$$(git rev-parse --git-common-dir 2>/dev/null)/../.tv-host" 2>/dev/null))
# Expanded only inside a recipe, so `make`, `make check` and `make ipk` never need a TV at all —
# but anything that talks to one fails with this sentence instead of dialling `root@`.
# `alpine` is NOT a secret: it is webosbrew's published dev-mode root password, the same on every
# rooted webOS TV. It stays in the clear because removing it would break the loop for everyone and
# protect nothing. The ADDRESS is the part that identified one household, and that is now local.
TV_OR_DIE = $(if $(TV),$(TV),$(error no TV configured — put its IP in .tv-host, or pass TV=<ip>))
SSH       = sshpass -p alpine ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@$(TV_OR_DIE)
SCP       = sshpass -p alpine scp -O -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
RUN_SECS ?= 18

# --- THE TELEVISION IS A MUTEX, and this is what enforces it -------------------------------
#
# One dev set, one app instance, no OS-level lock: two jobs on it do not fail cleanly, they
# produce plausible WRONG data (a bogus timeline_climb, an fps number measured while somebody
# else's binary was being deployed underneath). Every recipe below that TOUCHES the television
# takes `tv-lock-require` as its first prerequisite, so the refusal happens before the first scp
# rather than halfway through one.
#
# It is cheap: `require` re-checks a lease this lane verified in the last minute from a local file,
# with no ssh at all, which is what makes it affordable on `run-stream` — a recipe tests/run.py
# invokes once per case. And it is not a gate you have to remember: with nobody holding the set it
# takes a short implicit lease rather than failing, so a lone `make deploy` still works and still
# cannot collide with a fleet job. A SESSION should take a real one (`tools/tv-lock.sh acquire`),
# because the gap between two of your own commands is where another lane lands.
#
# `$(TV)` rather than `$(TV_OR_DIE)`: with no television configured this must stay silent and let
# the recipe's own ssh produce the familiar error, not replace it with a complaint about a lock.
TVLOCK = tools/tv-lock.sh
tv-lock-require:
	@$(if $(TV),TV=$(TV),) $(TVLOCK) require --quiet --why "make $(or $(MAKECMDGOALS),deploy) [$(FLAVOR)]"

# --- WHICH INSTALL: the FLAVOR axis --------------------------------------------------------
#
# Two builds live on one television: `stable` is the app users get (`com.beb.plxnative`, the id in
# every release, manifest and channel listing), and `debug` is the day-to-day developer build
# beside it (`com.beb.plxnative.debug`) with its own launcher tile, its own sign-in and its own
# runtime files. webOS keys everything — the install directory, SAM's launch/closeByAppId, the LS2
# role file — on that id, so two ids are two apps that cannot touch each other.
#
# THE DEFAULT IS `debug`, IN THIS TRACKED FILE, and that is a deliberate asymmetry rather than a
# preference. Every command in this repo's muscle memory, every skill recipe and every harness
# invocation is spelled `make deploy` / `make run` / `./tests/run.py` with no flavour, and each one
# used to overwrite the only install there was. The two mistakes are not comparable: deploying to
# `debug` when you meant `stable` costs you retyping one command, while deploying to `stable` when
# you meant `debug` destroys a working install — possibly mid-film, with no undo, on the app the
# household actually watches with. So the safe one is what you get for free and the other has to be
# asked for by name.
#
# Tracked rather than a gitignored `.app-flavor` dotfile (which is how `.tv-host` works, and was the
# obvious thing to copy): a fresh clone, and especially a fresh worktree in an agent fleet, has no
# dotfile — so the dangerous default would be inherited invisibly by exactly the checkouts nobody
# is watching.
#
# The whitelist is not decoration. `make FLAVOR=stabel deploy` would otherwise mint a third
# registered app called `com.beb.plxnative.stabel` on the television (LG's id charset accepts it,
# so nothing downstream objects) and the symptom is a mystery tile on a TV rather than a message on
# a terminal. `$(error)` at parse time costs one line.
FLAVORS      = stable debug
FLAVOR      ?= debug
$(if $(filter $(FLAVOR),$(FLAVORS)),,$(error unknown FLAVOR "$(FLAVOR)" — one of: $(FLAVORS)))

# The id users get. Also `paths::STABLE_APP_ID` in the Rust half and `STABLE_ID` in ci/flavor.py —
# three copies of one string, each in a language that cannot see the others, and ci/flavor.py's
# selftest is what keeps them in step.
APPID_STABLE = com.beb.plxnative
APPID        = $(if $(filter stable,$(FLAVOR)),$(APPID_STABLE),$(APPID_STABLE).$(FLAVOR))
APPDIR       = /media/developer/apps/usr/palm/applications/$(APPID)

# Where this install's runtime files live — the event log, the crash log, the `plxnative-*` dev
# triggers and the remote FIFO. The app resolves this itself (`paths::resolve_runtime_dir`); this
# is the same rule spelled for the shell, and `make print-rundir` is how every tool asks for it
# instead of restating it a fourth time.
#
# The stable install keeps `/tmp` byte for byte, so every existing recipe, doc line and harness
# glob stays true for the app users get. A flavoured install gets `/tmp/<app id>` — a DOT in the
# name, never a hyphen, because `dev::any_trigger_present` scans the root for entries beginning
# `plxnative-` and a sibling directory matching that prefix would silently suppress the OTHER
# install's who's-watching picker.
RUNDIR       = $(if $(filter stable,$(FLAVOR)),/tmp,/tmp/$(APPID))
EVENTLOG     = $(RUNDIR)/plxnative-events.log

# Machine-readable answers, so no tool has to restate any of the above. `make -s print-appdir
# FLAVOR=debug` and every tool in tools/ and tests/ asks this way — which also means the flavour a
# tool RESOLVED and the flavour it deploys, kills and launches with are one value, and cannot
# disagree (closing install A while launching install B reproduces SAM's stale-running no-op and
# then grades the wrong app's log).
#
# NOT `make -p`/`make -pn`, which is what these replace: that prints a RECURSIVE variable's
# UNEXPANDED DEFINITION, so `TV` comes out as the literal `$(strip $(shell cat .tv-host …))` on any
# checkout that uses `.tv-host` — the trap tools/tv-session.sh already documents. Real recipes
# echoing real values cannot do that. See PURE_QUERY below for why they are also side-effect free.
# The capture listener's TCP port for THIS install. Two installs cannot both bind one port, and
# the failure is silent on both sides — the second bind fails with one line in a log nobody is
# tailing, and the operator then watches one install's picture while every key they type goes into
# the other install's FIFO. `capture::default_port` is the same rule in Rust; ci/flavor.py's
# selftest compares them, and is what will object when a third flavour needs a real decision.
APPPORT      = $(if $(filter stable,$(FLAVOR)),8910,8911)

# The default goal, stated rather than inherited. Make takes the FIRST target in the file, and the
# seven query targets below are the first ones now — so a bare `make` printed the flavour and
# exited 0 without building anything. That is the worst shape this failure could take: `make &&
# make deploy` succeeds end to end and ships whatever binary was already sitting in pkg/, which is
# exactly the stale-binary trap the header-dependency rule further down exists to prevent. Pinning
# it here also survives the next block that gets added above `all:`.
.DEFAULT_GOAL := all

QUERY_GOALS = print-flavor print-appid print-appdir print-rundir print-eventlog print-appport print-tv \
              print-simbin
print-flavor:   ; @echo '$(FLAVOR)'
print-appid:    ; @echo '$(APPID)'
print-appdir:   ; @echo '$(APPDIR)'
print-rundir:   ; @echo '$(RUNDIR)'
print-eventlog: ; @echo '$(EVENTLOG)'
print-appport:  ; @echo '$(APPPORT)'
print-tv:       ; @echo '$(TV)'
# Where `make sim` put the simulator. It is `$(SIM_TDIR)`-derived and `SIM_TDIR` is deliberately
# overridable — agents running several simulators at once keep separate target dirs, and the
# `macapp` build has its own — so a tool that restates the path silently runs another lane's
# binary. Same argument as `print-appdir`: ask, never restate.
print-simbin:   ; @echo '$(SIM_BIN)'

# --- webOS NDK toolchain -----------------------------------------------------
WEBOS_SDK   ?= $(HOME)/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot
# Which nightly to use. Defaults to whatever rustup calls `nightly` (the dev-machine behaviour
# this has always had). CI pins an exact date — `make RUST_NIGHTLY=nightly-2026-07-02 …` — because
# `-Z build-std` recompiles std from source and is the most drift-sensitive thing this build does.
# NB a rust-toolchain.toml would NOT work here: a `cargo +toolchain` on the command line outranks
# the file, so the pin has to come through this variable.
RUST_NIGHTLY ?= nightly

# sha256 helper: coreutils `sha256sum` on Linux, perl `shasum -a 256` on macOS. Both print the
# same "<hex>  <path>" format, so `-c -` works against either.
SHA256SUM := $(shell command -v sha256sum >/dev/null 2>&1 && echo sha256sum || echo 'shasum -a 256')

TOOLPREFIX   = $(WEBOS_SDK)/bin/arm-webos-linux-gnueabi-
CC           = $(TOOLPREFIX)gcc
AR           = $(TOOLPREFIX)ar
SYSROOT      = $(WEBOS_SDK)/arm-webos-linux-gnueabi/sysroot
# NDK gcc default target is cortex-a9 / armv7-a / soft-float — portable across
# webOS 3–6 and safe on the A53 (ARMv7 barriers are `dmb`, not the ARMv6 CP15
# `mcr p15` that SIGILLs on ARMv8). So we do NOT pin -mcpu here.
# -Wall -Wextra -Werror: the C side is five files (boot shim, crash tracer, narrow Sentry value
# wrapper, Starfish/ACB seam and nanosvg rasterizer) and it built without this gate until
# 2026-08-15 — so the crash tracer and the 15-symbol C++ seam, the two places where a sloppy cast is
# least survivable, were compiled at gcc's bare default. Turning it on cost nothing: the only hit
# in the tree is inside vendored nanosvg, suppressed at its include in src/svg.c.
#
# -Werror is safe to pin HERE in a way it would not be on a moving toolchain: the NDK is a fixed
# GCC 12 installed by `make setup-env`, so no compiler upgrade can invent a new diagnostic under
# us. The Rust half of the same rule lives in rust-modules/Cargo.toml's `[lints]`. Both gates have
# been verified to actually FAIL on a planted warning — the lesson of ci/check-package.py's
# release witness, which never once fired in any configuration.
#
# Escape hatch while mid-edit: `make WERROR= …` (and see Cargo.toml for the Rust equivalent).
WERROR      ?= -Werror
# The out-of-process crash walker follows ARM's APCS frame chain. Keeping r11 frame pointers in
# every C frame is therefore part of the crash-reporting ABI, not debug-only codegen; without it a
# valid envelope contains only the faulting PC. Rust's matching flag is in RUST_ENV/config.toml.
CFLAGS       = --sysroot=$(SYSROOT) -O2 -fno-omit-frame-pointer -Wall -Wextra $(WERROR) -Iinclude -Isrc -Ivendor/nanosvg -D_GNU_SOURCE
# DEBUG=1 keeps DWARF in the binary so a crash PC symbolizes to file:line instead of just
# a function name (tools/crash-report.sh / the crash-triage skill). Same codegen, bigger
# binary — deploy it only while chasing a crash.
ifeq ($(DEBUG),1)
# -DPLX_DEBUG lets the C shim keep core dumps enabled for a post-mortem (src/crashtrace.c's
# setrlimit(RLIMIT_CORE, 0) — a shipping build must not write 200 MB into the TV's app
# partition). This is the only thing DEBUG=1 changes about behaviour rather than debuginfo.
CFLAGS      += -g -DPLX_DEBUG
RUST_DEBUGINFO = -C debuginfo=2
endif

# SYMBOLS=1 — the same DWARF, WITHOUT DEBUG=1's behaviour change, for producing a separated
# `pkg/plxnative.debug` that a symbol server can match to a stripped shipped binary by build id.
# `make symbols` below is the whole recipe.
#
# **NOT the default, and the numbers are why** (measured 2026-08-29 on the dev Mac, cold):
#
#   full cross build with debuginfo=2   30 s wall / 145 s CPU   (the same build is seconds warm)
#   its target dir                      356 MB   vs a normal one's share of a tree already at 26 GB
#   the Rust staticlib                  148 MB   vs 19.6 MB
#   the linked binary                    86.7 MB vs 10.0 MB
#   -> pkg/plxnative.debug               79.8 MB
#   -> stripped, i.e. what ships          6.93 MB vs 6.99 MB from a non-debuginfo build
#
# So it costs nothing that SHIPS — the stripped artifact is if anything a hair smaller — and the
# time is bearable. What it costs is disk, ~356 MB per feature-keyed target dir, on a machine whose
# `rust-modules/target*` already runs to tens of gigabytes across the configurations this repo
# keys, and where a worktree fleet multiplies that again. That is the only reason this is opt-in.
ifeq ($(SYMBOLS),1)
CFLAGS      += -g
RUST_DEBUGINFO = -C debuginfo=2
endif

# Rust codegen flags. rust-modules/.cargo/config.toml carries the SIGILL-critical pair for cargo
# invoked ANOTHER way (an IDE, rust-analyzer, a hand-typed `cargo build --target …`); the Makefile
# has to repeat them, because the RUSTFLAGS *environment variable* REPLACES that list wholesale
# rather than appending to it. Anything added here must keep the full list intact — a partial
# RUSTFLAGS silently drops target-cpu, and the ARMv6 CP15 barrier SIGILLs on the A53.
#
# --remap-path-prefix makes the binary independent of WHO built it. `-Z build-std` compiles std
# from rust-src under $RUSTUP_HOME and every dependency from $CARGO_HOME, and rustc writes each
# absolute path into .rodata as a panic location: 252 of them on this machine before this landed
# (113 rustup + 139 registry), which is what ci/check-elf.sh's build-host section gates on.
# It is NOT only a privacy fix. Those paths are why two developers at the same commit and the
# same toolchain got DIFFERENT sha256s — so the reproducibility the ipk claims, and with it the
# manifest hash a user's TV verifies at install, was untrue until the builder's $HOME stopped
# being an input.
#
# ORDER IS LOAD-BEARING: rustc applies the LAST matching --remap-path-prefix, not the longest or
# the first (measured — with $(HOME) listed last, $CARGO_HOME/registry came out as
# /build/.cargo/registry rather than /cargo). So the broad $(HOME) catch-all goes FIRST and the
# specific roots after it, which is also what makes CI work: a runner may put CARGO_HOME or
# RUSTUP_HOME outside the home directory entirely (/usr/share/rust/.rustup), where the catch-all
# cannot reach them and only the explicit remap does.
CARGO_HOME  ?= $(HOME)/.cargo
RUSTUP_HOME ?= $(HOME)/.rustup
RUST_REMAP   = --remap-path-prefix=$(HOME)=/build \
               --remap-path-prefix=$(CARGO_HOME)=/cargo \
               --remap-path-prefix=$(RUSTUP_HOME)=/rustup
# CAPLINTS=1 relaxes rust-modules/Cargo.toml's `[lints] warnings = "deny"` for ONE invocation:
# --cap-lints puts a ceiling on every lint level, so a deny comes back out as a warning that still
# prints. It goes through this variable rather than the caller exporting RUSTFLAGS, because that
# environment variable REPLACES the list below wholesale — dropping target-cpu, and with it the
# SIGILL guard. Pair with WERROR= to relax the C half: `make CAPLINTS=1 WERROR=`.
ifeq ($(CAPLINTS),1)
RUST_CAPLINTS = --cap-lints=warn
endif
RUST_ENV = RUSTFLAGS="-C target-cpu=cortex-a9 -C target-feature=-neon -C force-frame-pointers=yes $(RUST_DEBUGINFO) $(RUST_CAPLINTS) $(RUST_REMAP)"
# -Iinclude keeps the TV's SDL2/GLES2 headers (its SDL is a 2.0.4 fork) ahead of
# the NDK's newer sysroot copies, so we compile against the ABI the TV runs.

# Real sysroot libraries. Every one of these has the SAME SONAME on every webOS release from
# 2.2.3 to 11.2.0 (`tools/fwcompat.py --inventory` will show you), which is what makes linking
# them normally — with real link-time symbol checking — the right call.
#   libpf-1.0 carries mediapipeline::CustomPipeline (the webOS<11 seek path).
LIBS_REAL = -lSDL2 -lSDL2_ttf -lGLESv2 -lluna-service2 -lglib-2.0 \
            -lwayland-client -lplayerAPIs -lpf-1.0
# NOT LISTED, DELIBERATELY — but for TWO different reasons, and this comment used to give only
# the first, for all three.
#
#   libcurl and libAcbAPI: their SONAMEs MOVE between releases (curl .so.5 -> .so.4, ACB deleted
#   outright at webOS 5.0). A DT_NEEDED entry is a hard requirement for one exact name, cannot say
#   "either of these", and a name the device lacks kills the process at exec() — before main,
#   before the event log exists. So they are dlopen'd by SONAME CANDIDATE LIST:
#   rust-modules/src/dynlib.rs, and the video-plane comment at the top of src/starfish.c.
#
#   FFmpeg is not a version question at all any more: we SHIP our own, pinned (the bundled-FFmpeg
#   section below builds libav*-plx.so.63/63/61 and stages them into pkg/). It is unlinked because
#   those files land BESIDE the binary, which is on no library search path, and they carry no rpath
#   — so a DT_NEEDED entry would be resolved at exec() against the system path, where it either
#   finds nothing (dead before main) or finds the TELEVISION's copy, which is the wrong one:
#   webOS 11.2.0 ships FFmpeg 6 itself. ff.rs::load_libraries opens the three by ABSOLUTE PATH out
#   of paths::app_dir(), one pinned SONAME each, in dependency order under RTLD_GLOBAL — not a
#   candidate list, and nothing is being tolerated.
#
# The old wording ("FFmpeg 55->57->58->59->60") described the app that read the TV's FFmpeg, and it
# survived the bundling by 100 lines. Left alone it points the next reader at version tolerance we
# no longer need and at a library we no longer use — which is how "just use the TV's libavformat
# for https" keeps coming back, when the copy we ship is configured --disable-network and cannot
# open a URL at all. `docs/agent-reference.md`'s "Linking" section carries the same account at length.

# Rust-first build. The app is Rust (rust-modules/, compiled to a staticlib and
# linked in); C is only main.c (boot shim) + starfish.c (the StarfishMediaAPIs
# C++/ACB seam) + svg.c (nanosvg rasterizer).
# (src/gpdebug.c is a debug-only guard-page allocator — never in the normal build.)
# RELEASE=1 drops BOTH default cargo features — `devtools` (the on-screen seven-segment counter,
# app.rs) and `devtriggers` (the whole /tmp surface, src/dev.rs). Neither may ship to users.
# See rust-modules/Cargo.toml's [features].
#
# The stamp exists because of this project's classic stale-artifact trap: cargo fingerprints the
# feature set and would rebuild, but MAKE would never invoke it — toggling RELEASE=1 touches no
# file, so the recipe looks up to date and the PREVIOUS feature set's staticlib gets linked with
# no comment. Depending on a stamp whose CONTENT is the flag makes the switch a real prerequisite.
# RELEASE=1 drops BOTH default cargo features — `devtools` (the on-screen seven-segment counter,
# app.rs) and `devtriggers` (the whole /tmp surface, src/dev.rs). Neither may ship to users.
# See rust-modules/Cargo.toml's [features].
#
# Each feature set gets its OWN target dir, and that is load-bearing rather than tidy. Cargo names
# the staticlib by crate, so both builds would otherwise write the SAME libplxnative_modules.a —
# and cargo fingerprints the build without hashing its output, so after a RELEASE=1 build it
# reports the dev build "Finished in 0.04s" and leaves the release .a sitting there. `make` then
# links it with no comment: a release binary shipped as if it were the tested dev one. Measured,
# not theorised. Separate dirs also let make track each artifact honestly.
#
# **LAB=1** adds one more feature — `lab-diagnostics`, the diagnostics + control Cloud Test Lab bridge
# (`docs/lab-diagnostics.md`). It is a THIRD configuration, with its own target dir for exactly
# the reason above, and it composes with RELEASE: `make LAB=1 RELEASE=1 …` is a release-featured
# binary that can still upload a snapshot, which is the configuration a submission candidate is
# actually tested in. The feature is not in the default set at all, so no ordinary build and no
# forgotten flag can produce it.
RUST_FEATFLAGS = $(if $(RELEASE),--no-default-features,)$(if $(LAB), --features lab-diagnostics,)
RUST_TDIR      = target$(if $(RELEASE),-release,)$(if $(LAB),-lab,)$(if $(SYMBOLS),-sym,)
# OVERRIDING RUST_FEATFLAGS BY HAND? PASS RUST_TDIR TOO. This dir is keyed on RELEASE, not on the
# flag set, so `make RUST_FEATFLAGS=...` alone lands in the SAME target/ as an ordinary build:
# cargo's staticlib looks up to date, the stamp below deletes pkg/plxnative, and make relinks the
# PREVIOUS feature set without a word. Exactly the stale-artifact trap the stamp exists to prevent,
# one step removed — and it burned two builds while shooting the README screenshots (which want
# devtriggers ON and devtools OFF, a combination neither RELEASE nor the default gives). Correct:
#   make RUST_FEATFLAGS="--no-default-features --features devtriggers" RUST_TDIR=target-shots deploy
# ...and the LINK needs its own witness, because pkg/plxnative is a path BOTH configurations
# write. Per-dir targets keep cargo honest, but after a RELEASE=1 build the dev .a is older
# than the release binary sitting at pkg/plxnative, so make would call the link up to date and
# leave the release binary in place under a plain `make`. The stamp's CONTENT is the flag set,
# so switching configuration is a real prerequisite change.
RUST_STAMP     = pkg/.build-config
# The bundled FFmpeg is configured differently by RELEASE=1 too (no swscale, no mpeg1/mpegts), so
# it belongs in the SAME stamp. It was missed at first, and the failure was exactly the one this
# mechanism exists to prevent, one layer down: `ci/build-ffmpeg.sh` is reached through a single
# header sentinel, so make never re-ran it once the header existed, its own .plx-flags guard was
# unreachable, and `make RELEASE=1` shipped the libraries built for a DEV configuration — while
# the comment two blocks below claimed otherwise. Building release-first then dev is worse: the
# swscale staging rule globs a prefix that has none and the build dies.
# SYMBOLS belongs in the stamp for the same reason RELEASE and the FFmpeg configuration do, and the
# failure it prevents is the sharpest one here: a debuginfo build and a plain one produce DIFFERENT
# BUILD IDs (measured — 3ede46f8… vs cc4a5c7b… for the same sources), and the build id is the only
# thing that matches a separated `.debug` back to the binary a crash came from. Without this,
# `make RELEASE=1 ipk` followed by `make RELEASE=1 SYMBOLS=1 symbols` silently relinks a different
# binary and hands you a `.debug` that will never match anything a user's television reports — the
# same shape as `make RELEASE=1 && make deploy`, which is the trap this whole mechanism exists for.
# The telemetry endpoints, read out of the gitignored pkg/telemetry.local.json and handed to the
# compiler as option_env! values. Absent file, absent key, EMPTY value -> the constant is None and
# `telemetry::sender::configured()` is false at COMPILE time, so a fork, a CI runner and anyone
# building from source get a binary with no endpoint in it at all. That is the safe direction and
# it is a property of the artifact rather than of a runtime flag.
#
# **FOUR values, in two PAIRS, and the pair decides the environment.** This is not symmetry for its
# own sake: the build's `environment` field is derived from WHICH pair it was given
# (`telemetry::sender::ENVIRONMENT`), so a binary cannot label itself `production` while sending to
# the dev project. The first draft derived it from the feature set instead, and
# `make RELEASE=1 FLAVOR=debug deploy` -- an ordinary command -- would have done exactly that:
# `RELEASE=1` drops `devtriggers`, so it read as production, while the key still came from the local
# file and the data still went to dev. Destination and label diverging silently is the worst
# outcome for a field whose only job is to say which side data is on.
#
# The DEV pair is what a developer's machine holds; the PRODUCTION pair exists only as a GitHub
# repository variable and is injected by the release workflow. So a local build physically cannot
# reach production -- not because a flag was set, but because the value is not on the machine.
#
# All four are WRITE-ONLY ingest credentials and publishable by design -- any client that sends
# anything must carry one, and `strings` finds them in any shipped binary. The Sentry AUTH TOKEN is
# a different thing entirely (it can read and delete the project's data), is not read here, and is
# never compiled in; it lives only as a GitHub secret and is used only by `sentry-cli` in CI.
# `python3 -c` rather than a grep because the file is JSON and a value can contain a `:` or a `/`;
# `|| true` because the whole point is that a checkout without the file still builds.
TELEMETRY_JSON = pkg/telemetry.local.json
telemetry_val = $(shell python3 -c "import json,sys;print(json.load(open('$(TELEMETRY_JSON)')).get('$(1)',''))" 2>/dev/null || true)
# The dev pair: read from the working copy, written there by `make telemetry-local`.
PLX_SENTRY_DSN_DEV  ?= $(call telemetry_val,sentry_dsn_dev)
PLX_POSTHOG_KEY_DEV ?= $(call telemetry_val,posthog_key_dev)
# The production pair: deliberately NOT read from the file. Only the environment can supply these,
# which in practice means the release workflow. Reading them from the working copy is precisely the
# bug this whole arrangement exists to make impossible.
PLX_SENTRY_DSN  ?=
PLX_POSTHOG_KEY ?=

# In the stamp, and it has to be: switching a build from unconfigured to configured changes what
# the binary CAN DO and nothing about the sources, so without this the configuration would be
# baked in from whichever build happened to run first -- the same class of trap as
# `make RELEASE=1 && make deploy`. The values are hashed rather than written, so the stamp file
# (which is not gitignored) never contains a credential.
TELEMETRY_CFG  = $(shell printf '%s|%s|%s|%s' '$(PLX_SENTRY_DSN)' '$(PLX_POSTHOG_KEY)' '$(PLX_SENTRY_DSN_DEV)' '$(PLX_POSTHOG_KEY_DEV)' | shasum | cut -c1-12)
RUST_CFG       = features:$(RUST_FEATFLAGS)$(if $(SYMBOLS),+symbols,)+tel:$(TELEMETRY_CFG)
# Handled by $(shell) during PARSING, and by DELETING the output rather than by timestamps.
# Both choices are load-bearing, and both were arrived at by measuring the failures:
#   * A rule cannot do it. macOS ships GNU make 3.81, which decides whether a target is up to date
#     from a stat taken BEFORE its prerequisites' recipes run, so a stamp written by a recipe is
#     read with its OLD mtime and the relink is skipped.
#   * A newer stamp is not enough either. make 3.81 compares mtimes at ONE-SECOND granularity, so
#     a stamp written 0.5s after the binary compares EQUAL and the link is still skipped —
#     measured: stamp 1785559259.894 vs binary 1785559259.408, no relink.
# Removing pkg/plxnative cannot be defeated by either, because "the target does not exist" is not
# a timestamp comparison. The failure this prevents is silent: you get the OTHER configuration's
# binary, with the dev counter burned into a release build or vice versa.
# (Consequence: a `make -n` that CHANGES the configuration really does delete the binary. The next
# real build restores it; a dry run that does not switch configuration touches nothing.)
#
# ...and it is skipped for a PURE QUERY. `make -s print-appdir` and friends exist so tools stop
# restating this file's values, but the block above runs at PARSE time, on every invocation,
# whatever the goal — so asking a question would delete `pkg/plxnative`, the FFmpeg header sentinel
# and the staged libraries whenever the asking invocation's configuration differed from the stamp.
# That is not hypothetical: `tools/crash-report.sh` shells out to make for the TV address, and it
# is the tool you run immediately after a crash, i.e. right after the RELEASE=1 build you were
# testing. A ~2-minute FFmpeg rebuild as the side effect of asking a question is the wrong shape,
# and worse, it would silently discard the very binary the crash is being symbolized against.
#
# The test is that EVERY goal builds nothing. A mixed `make print-appid deploy` still stamps,
# because it really is going to build something.
#
# `release-guard` is in the list beside the queries for the same reason it is not one of them: it
# is a check-only phony that produces no artifact, and it is exactly the target somebody runs to
# SEE the refusal message the cut-release skill quotes. Without it, asking that question on a
# different configuration deleted `pkg/plxnative`, the FFmpeg header sentinel and the staged
# libraries — measured, by a reviewer, mid-review.
SIDE_EFFECT_FREE = $(QUERY_GOALS) release-guard lab-guard
PURE_QUERY := $(if $(MAKECMDGOALS),$(if $(filter-out $(SIDE_EFFECT_FREE),$(MAKECMDGOALS)),,yes),)
ifneq ($(PURE_QUERY),yes)
ifneq ($(RUST_CFG),$(shell cat $(RUST_STAMP) 2>/dev/null))
  $(shell mkdir -p pkg && printf '%s' '$(RUST_CFG)' > $(RUST_STAMP) && rm -f pkg/plxnative \
          vendor/ffmpeg-prefix/include/libavformat/avformat.h pkg/lib*-plx.so.* pkg/.ffabi-ok)
endif
endif

RUST_TARGET = arm-unknown-linux-gnueabi
RUST_LIB    = rust-modules/$(RUST_TDIR)/$(RUST_TARGET)/release/libplxnative_modules.a

# Every ordinary C translation unit ships; gpdebug remains an opt-in allocator guard.
SRCS = $(filter-out src/gpdebug.c,$(wildcard src/*.c))
OBJS = $(SRCS:.c=.o)

all: pkg/plxnative

# per-file compile; each object depends on ALL headers so a header edit rebuilds all
src/%.o: src/%.c $(wildcard src/*.h) Makefile
	$(CC) $(CFLAGS) -c $< -o $@

# Rust staticlib (built-in arm-unknown-linux-gnueabi target = soft-float ABI,
# matching the NDK's softfp calling convention). A staticlib needs no linker, so
# this needs only the nightly compiler + rust-src — no external cross-linker.
# CRITICAL codegen flags for this TV (32-bit ARMv7+ / A53):
#  - target-cpu=cortex-a9: the default arm-*-gnueabi (ARMv6) codegen emits the
#    legacy CP15 memory barrier (mcr p15,...,c7,c10,5), UNDEFINED on the A53
#    (ARMv8) → SIGILL. cortex-a9 (ARMv7-A) emits the dedicated `dmb`, matching
#    the C side's default and staying portable to older ARMv7 webOS devices.
#  - target-feature=-neon: NEON isn't needed (VFP still on for floats), and it
#    dodges crates (simd-adler32, ...) whose NEON path uses unstable intrinsics.
#  - -Z build-std: rebuilds std itself with these flags (precompiled std shipped
#    the CP15 barriers), so needs the nightly toolchain + rust-src.
# Codegen flags now live in rust-modules/.cargo/config.toml so they bind to the TARGET and
# every cargo invocation gets them, not just this recipe. See that file before changing them.
#
# The prerequisite list is a `find`, not a hand-kept wildcard: the crate embeds shaders
# (gfx.rs include_str! of src/shaders/*.vert|*.frag) and icons (ui/icons.rs include_str! of
# assets/icons/*.svg) at COMPILE time. Those were in no dependency list, so editing a shader
# or an icon produced no rebuild and the TV silently kept running the old one — the worst
# failure mode on a project whose only verification is observing the device.
# ---- The bundled FFmpeg ------------------------------------------------------
# The app ships its own FFmpeg rather than reading the television's. Why, at length, in
# ci/build-ffmpeg.sh and docs/webos5-port.md; the short version is that the TV's version moves with
# the firmware (55 -> 57 -> 58 -> 59 -> 60 across webOS 2 to 11), and while the struct OFFSETS
# could be re-derived from upstream headers at the matching version, the component list could not
# be checked at all — demuxers and bitstream filters live in a registry, as data, invisible to
# every symbol table. Bundling makes both compile-time facts.
#
# Built once into vendor/ffmpeg-prefix (gitignored, derived); ~2 minutes cold, nothing after.
# RELEASE=1 drops swscale and the mpeg1/mpegts pair, which only the dev capture stream uses.
FFMPEG_PREFIX = vendor/ffmpeg-prefix
FFMPEG_INC    = $(FFMPEG_PREFIX)/include
# Staged into pkg/ under their SONAMEs, which is the name ff.rs dlopens by absolute path.
FFMPEG_SONAMES = libavutil-plx.so.61 libavcodec-plx.so.63 libavformat-plx.so.63 \
                 $(if $(RELEASE),,libswscale-plx.so.10)
FFMPEG_STAGED = $(addprefix pkg/,$(FFMPEG_SONAMES))

# Sentry Native supplies only the async-signal-safe capture and out-of-process ARM stack walk. Its
# HTTP transport is compiled out: the resulting envelope is handed back to this executable in
# spool-only mode and sent later by telemetry::sender, behind the app's own consent and retry rules.
# The pinned source, webOS/glibc-2.12/ARM32 patch and build recipe live in ci/build-sentry-native.sh.
SENTRY_NATIVE_PREFIX = vendor/sentry-native-prefix
SENTRY_NATIVE_LIB     = $(SENTRY_NATIVE_PREFIX)/lib/libsentry.a
SENTRY_UNWIND_LIB     = $(SENTRY_NATIVE_PREFIX)/lib/libunwind.a
SENTRY_HANDLER        = $(SENTRY_NATIVE_PREFIX)/bin/sentry-crash
SENTRY_NATIVE_STAMP   = $(SENTRY_NATIVE_PREFIX)/.built
SENTRY_NATIVE_INPUTS  = ci/build-sentry-native.sh vendor/sentry-native/webos-arm32.patch

# `sentry_context.c` is the only application TU that includes the SDK header. Its Rust caller sees
# plain C strings rather than sentry_value_t's opaque by-value union, whose AAPCS ABI must stay on
# the C side. The header is generated by the pinned SDK build, hence the explicit prerequisite.
src/sentry_context.o: CFLAGS += -I$(SENTRY_NATIVE_PREFIX)/include
src/sentry_context.o: $(SENTRY_NATIVE_STAMP)

$(SENTRY_NATIVE_STAMP): $(SENTRY_NATIVE_INPUTS)
	WEBOS_SDK=$(WEBOS_SDK) ./ci/build-sentry-native.sh
	@touch $@

sentry-native: $(SENTRY_NATIVE_STAMP)

$(FFMPEG_INC)/libavformat/avformat.h:
	RELEASE=$(RELEASE) ./ci/build-ffmpeg.sh

# The real files carry a full version (libavutil-plx.so.58.29.100); ship them under the SONAME.
$(FFMPEG_STAGED): pkg/%: $(FFMPEG_INC)/libavformat/avformat.h
	@mkdir -p pkg
	cp $$(ls $(FFMPEG_PREFIX)/lib/$*.* | head -1) $@

# The FFmpeg ABI gate. ff.rs reads FFmpeg structs at hardcoded offsets; ci/ffabi-assert.c
# re-derives every one with offsetof against THE HEADERS THE SHIPPED LIBRARIES WERE BUILT FROM, so
# a slip is a compile error naming the field instead of a wild pointer on a television. Compiled,
# never linked — it contains only _Static_asserts — and a prerequisite of the staticlib, so no
# binary is produced if a constant is wrong.
FFABI_STAMP = pkg/.ffabi-ok
$(FFABI_STAMP): ci/ffabi-assert.c $(FFMPEG_INC)/libavformat/avformat.h Makefile
	@mkdir -p pkg
	$(CC) $(CFLAGS) -I $(FFMPEG_INC) -std=c11 -c ci/ffabi-assert.c -o /dev/null
	@touch $@

RUST_INPUTS := $(shell find rust-modules/src assets -type f 2>/dev/null)
$(RUST_LIB): $(RUST_INPUTS) rust-modules/Cargo.toml rust-modules/Cargo.lock rust-modules/.cargo/config.toml Makefile $(FFABI_STAMP)
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" $(RUST_ENV) \
	  PLX_SENTRY_DSN='$(PLX_SENTRY_DSN)' PLX_POSTHOG_KEY='$(PLX_POSTHOG_KEY)' \
	  PLX_SENTRY_DSN_DEV='$(PLX_SENTRY_DSN_DEV)' PLX_POSTHOG_KEY_DEV='$(PLX_POSTHOG_KEY_DEV)' \
	  cargo +$(RUST_NIGHTLY) build --release --target $(RUST_TARGET) \
	    --target-dir $(RUST_TDIR) $(RUST_FEATFLAGS)

# link C objects + the Rust staticlib. gcc pulls in libgcc_s (the ARM-EHABI
# unwinder Rust's panic_unwind std references) + libc/pthread/dl/m/rt itself.
# `--build-id` is UNCONDITIONAL and costs 20 bytes in an allocated note section. It is the only
# stable identifier that survives `strip` and matches a separated `.debug` file back to the binary
# it came from — which is what every symbol server, Sentry's included, keys a debug image on, and
# what makes "is this .debug the one for that crash?" answerable at all. Until 2026-08-29 the ELF
# carried only `.note.ABI-tag` and there was no such identifier; `tools/crash-report.sh` compares
# whole-file md5s instead, which works only while the local binary is byte-identical to the shipped
# one and says nothing once the shipped one has been stripped.
#
# sha1 explicitly rather than the linker's default: the default is a configure-time choice of the
# binutils build, so pinning it here is what keeps two different NDK installs producing the same
# id for the same input — which the reproducible-package guarantee (`ci/mkipk.py`, and the sha256
# every user's television verifies) depends on.
pkg/plxnative: $(OBJS) $(RUST_LIB) $(FFMPEG_STAGED) $(SENTRY_NATIVE_STAMP) Makefile
	$(CC) $(CFLAGS) -Wl,--build-id=sha1 $(OBJS) $(RUST_LIB) $(SENTRY_NATIVE_LIB) \
	  $(SENTRY_UNWIND_LIB) $(LIBS_REAL) -ldl -lrt -lpthread -lm -o $@

# --- NDK bootstrap -----------------------------------------------------------
# Download + extract + relocate the webosbrew native-toolchain into $(WEBOS_SDK).
NDK_REL ?= webos-d7ed7ee.6
# Host-aware asset selection. webosbrew/native-toolchain publishes exactly THREE host builds for
# this release — darwin-arm64, darwin-x86_64, linux-aarch64 — and NO linux-x86_64. That is why CI
# runs on `ubuntu-24.04-arm`: an x86_64 Linux runner cannot obtain this toolchain at all, and the
# old hardcoded `darwin-$(uname -m)` name RESOLVED there (downloading a Mach-O toolchain that
# passed the `test -x` guard below and died later with "Exec format error").
NDK_OS   := $(shell uname -s | tr A-Z a-z)
NDK_HOST := $(shell uname -m)
ifeq ($(NDK_OS),darwin)
  NDK_PLAT = darwin-$(if $(filter arm64,$(NDK_HOST)),arm64,x86_64)
else ifeq ($(NDK_HOST),aarch64)
  NDK_PLAT = linux-aarch64
else
  NDK_PLAT = UNSUPPORTED
endif
NDK_TARBALL = arm-webos-linux-gnueabi_sdk-buildroot_$(NDK_PLAT).tar.bz2
NDK_URL = https://github.com/webosbrew/native-toolchain/releases/download/$(NDK_REL)/$(NDK_TARBALL)
# sha256 of the linux-aarch64 tarball (the one CI fetches), verified 2026-08-01. Empty for the
# darwin assets — `make setup-env` on a dev Mac keeps its existing no-checksum behaviour; CI is
# where an unverified 156 MB download actually matters.
NDK_SHA256_linux-aarch64 = 45a2d12ff557457d92cde4fddaa77a6f1090fca03adc43bb74397e5e0c379501
NDK_SHA256 = $(NDK_SHA256_$(NDK_PLAT))
# Fetch this checkout's DEV telemetry credentials from GitHub into the gitignored local file.
#
# **GitHub is the single source of truth for every credential**, including the dev ones; the working
# copy is a cache and never an origin. That is what makes a fresh clone or a second worktree one
# command instead of a hunt — `[[worktree-fleet-hazards]]` records forgotten gitignored files as a
# recurring cost here, and this is one of them.
#
# It fetches ONLY the `_DEV` pair, and being the sole writer of this file is what keeps the dev /
# production separation true. The production pair is never written to disk on a developer's machine.
#
# The AUTH TOKEN is deliberately absent and cannot be fetched: `gh` reads repository VARIABLES but
# GitHub secrets are write-only through the API, and the only consumer — `sentry-cli` — runs in CI.
# So the one genuinely dangerous credential never lands on this machine at all.
#
# No `gh`, no network, or never having run this: the build simply carries no endpoint and sends
# nothing, which is the safe direction and already the behaviour.
telemetry-local:
	@command -v gh >/dev/null || { echo "telemetry-local: needs the gh CLI (brew install gh)"; exit 1; }
	@gh auth status >/dev/null 2>&1 || { echo "telemetry-local: gh is not authenticated (gh auth login)"; exit 1; }
	@set -e; \
	  dsn=$$(gh variable get PLX_SENTRY_DSN_DEV 2>/dev/null || true); \
	  key=$$(gh variable get PLX_POSTHOG_KEY_DEV 2>/dev/null || true); \
	  if [ -z "$$dsn$$key" ]; then \
	    echo "telemetry-local: neither PLX_SENTRY_DSN_DEV nor PLX_POSTHOG_KEY_DEV is set on the repo"; \
	    exit 1; \
	  fi; \
	  python3 -c 'import json,sys; json.dump({"_comment":["Written by `make telemetry-local`. GITIGNORED. DEV credentials only — the production pair lives solely in GitHub repository variables and is injected by the release workflow.","No auth token here: gh cannot read secrets, and sentry-cli runs in CI."],"sentry_dsn_dev":sys.argv[1],"posthog_key_dev":sys.argv[2],"sentry_org":"gleb-linnik","sentry_project":"plx-native","posthog_host":"https://eu.i.posthog.com"}, open("$(TELEMETRY_JSON)","w"), indent=2)' "$$dsn" "$$key"; \
	  chmod 0600 $(TELEMETRY_JSON); \
	  echo "telemetry-local: wrote $(TELEMETRY_JSON) (dev credentials; environment=development)"

setup-env:
	@test "$(NDK_PLAT)" != UNSUPPORTED || { \
	  echo "no webOS NDK published for $(NDK_OS)-$(NDK_HOST) at $(NDK_REL)."; \
	  echo "Available: darwin-arm64, darwin-x86_64, linux-aarch64."; exit 1; }
	@test -x $(CC) && { echo "NDK already present at $(WEBOS_SDK)"; exit 0; } || true
	mkdir -p $(dir $(WEBOS_SDK))
	curl -fL --retry 3 --retry-all-errors -o $(dir $(WEBOS_SDK))/ndk.tar.bz2 "$(NDK_URL)"
	@test -z "$(NDK_SHA256)" || { \
	  echo "$(NDK_SHA256)  $(dir $(WEBOS_SDK))/ndk.tar.bz2" | $(SHA256SUM) -c -; }
	tar xjf $(dir $(WEBOS_SDK))/ndk.tar.bz2 -C $(dir $(WEBOS_SDK))
	cd $(WEBOS_SDK) && ./relocate-sdk.sh
	rm -f $(dir $(WEBOS_SDK))/ndk.tar.bz2
	@echo "NDK ready: $$($(CC) --version | head -1)"

# tmp+mv so deploy works while the old binary is still executing (ETXTBSY)
# The NDK sysroot's NEON libjpeg-turbo rides along for the dev capture stream's JPEG
# mode (capture.rs dlopen's it next to the binary). BEST-EFFORT on purpose: the app
# runs fine without it, so a sysroot that ships a different patch version must never
# break `make deploy` — the wildcard just finds nothing and the copy is skipped.
TURBOJPEG_SO := $(firstword $(wildcard $(SYSROOT)/usr/lib/libturbojpeg.so.0.*))

# The app payload, in ONE place. `ipk` and `deploy` used to carry different file sets, and the
# ipk — the only artifact a non-developer ever receives — shipped WITHOUT the fonts, so a clean
# install silently rendered the whole theme::size ladder in the system DroidSans, invalidating
# the light-hinting/pixel-snapping contract that tools/font-hint-audit.py exists to guard.
# Everything the .ipk installs. THIRD-PARTY-NOTICES.md and licenses/ are payload, not repo
# decoration: LGPL-2.1 §6 requires the notice and the licence text to travel WITH the binary, so
# a copy in the GitHub repo does not discharge it for someone who received only the package.
# NB adding libturbojpeg.so.0 here would create IJG + BSD-3-Clause + Zlib obligations that the
# current notices do NOT cover — it is deliberately dev-deploy-only (see capture.rs).
# The bundled FFmpeg is PAYLOAD, not an optional extra: without it the app starts, browses and
# refuses to play. It is also the reason THIRD-PARTY-NOTICES.md and licenses/ must travel with the
# package — LGPL-2.1 §6 wants the notice and the licence text alongside the binary, and shipping
# FFmpeg ourselves makes that our obligation rather than the television's.
#
# THE THREE FLAVOUR-DEPENDENT ENTRIES ARE SOURCE PATHS ONLY. Everything here is staged with a plain
# `cp … $(STAGE)/`, which preserves BASENAMES — and that is exactly what is wanted, because
# `appinfo.json`'s own `icon`/`largeIcon` fields name `icon.png`/`largeIcon.png` and
# ci/check-package.py grades the payload by basename. So the flavour lives in the directory a file
# is read FROM and never in the name it is packaged UNDER. (A `pkg/appinfo.debug.json` would ship
# under that name and fail an otherwise correct package.)
APPINFO   = $(if $(filter stable,$(FLAVOR)),pkg/appinfo.json,pkg/.flavor/$(FLAVOR)/appinfo.json)
ICONS     = $(if $(filter stable,$(FLAVOR)),pkg/icon.png pkg/largeIcon.png,pkg/dev/icon.png pkg/dev/largeIcon.png)
# `pkg/lab.json` is in this list ONLY under LAB=1, and it is the whole handoff between the two
# halves of the Cloud Lab bridge: `tools/plxnative-lab start` writes it (endpoint, session,
# secret, certificate pin), and the app reads it out of its own install directory at boot, because
# a Cloud Test Lab set has no ssh and the package is the only channel into it. GITIGNORED, 0600,
# and in `.claude/hooks/outbound-guard.py`'s PRIVATE_FILES — it carries a live credential.
LAB_FILES = $(if $(LAB),pkg/lab.json,)
APP_FILES = pkg/plxnative $(SENTRY_HANDLER) $(APPINFO) $(ICONS) pkg/splash.png \
            pkg/appfont.ttf pkg/appfont-bold.ttf pkg/appfont-cjk.ttf pkg/OFL.txt \
            THIRD-PARTY-NOTICES.md \
            $(LAB_FILES) \
            $(FFMPEG_STAGED)
# appfont-cjk.ttf is the fallback face (Noto Sans CJK KR, tools/cut-noto-cjk.py) and it is the
# single largest thing in the package — 21 MB raw, ~11 MB of the .ipk. It is PAYLOAD, not an
# optional extra: without it a Korean, Japanese or Chinese library renders as tofu end to end, and
# the television's own DroidSansFallback is present on the sets we have measured but is not
# something a submission can be graded against. `rust-modules/src/fontcov.rs`'s host gate asserts
# what it must cover; `text.rs`'s module doc is the chain.

# TRADEMARKS.md ships too: it carries the brand reservation and the Plex/LG non-affiliation
# statement, which used to be appended to LICENSE. It was moved out because GitHub's `licensee`
# matches LICENSE against known texts by SIMILARITY, and the appended thirty lines pushed the file
# under the threshold — so the repository advertised "Other" rather than MIT, misrepresenting the
# terms in the one place most people look. Splitting the file must not un-ship the reservation.
LICENSE_FILES = LICENSE TRADEMARKS.md $(wildcard licenses/*.txt)

# The derived descriptor for a flavoured install, written by the SAME transform that packages it
# (ci/flavor.py, through ci/mkipk.py) so `make deploy`'s scp'd appinfo and the .ipk's staged one
# cannot drift — one code path, asked twice. Gitignored: it derives from pkg/appinfo.json, which
# stays the single source of the version and of every field that must NOT differ between flavours
# (only `id` and `title` may, and ci/flavor.py's selftest asserts exactly that set).
pkg/.flavor/$(FLAVOR)/appinfo.json: pkg/appinfo.json ci/flavor.py ci/mkipk.py
	@mkdir -p $(dir $@)
	python3 ci/mkipk.py --emit-appinfo $(FLAVOR) $@

# WHICH INSTALL, and WHICH CONFIGURATION — both, every time, because both have been shipped wrong.
#
# `test -d`, not `mkdir -p`: a hand-made app directory gets no SAM registration and no
# `/var/palm/ls2-dev/roles/pub/<id>.json`, so the app would launch and then be denied the LS2 calls
# the ACB bind needs — a stuck pipeline rather than an error. A flavour has to be INSTALLED once
# from its own package (`make FLAVOR=$(FLAVOR) install`), and failing here names that, instead of
# failing three steps later as something else.
# `tv-lock-require` is LAST in that list, not first, and that is the whole point of where it sits:
# prerequisites run left to right, and a cold `make deploy` spends ~2 minutes building FFmpeg
# before it touches the television. Taking the lock first would hold the set through a build that
# needs no television — and, on the short implicit lease, could even let it expire before the scp.
deploy: pkg/plxnative $(FFMPEG_STAGED) $(SENTRY_NATIVE_STAMP) $(APPINFO) release-guard tv-lock-require
	@echo "deploying $(if $(RELEASE),RELEASE,dev) build ($(RUST_CFG)) to $(APPID) [$(FLAVOR)]"
	@$(SSH) 'test -d $(APPDIR)' || { \
	  echo "$(APPDIR) does not exist on $(TV) — the $(FLAVOR) flavour is not installed."; \
	  echo "install it once:  make FLAVOR=$(FLAVOR)$(if $(RELEASE), RELEASE=1,) install"; exit 1; }
	# The descriptor and the directory it lands in must name the same app: `paths::app_id` reads
	# the DIRECTORY, so a mismatch means the running binary and its own appinfo disagree about
	# which install this is — and every id-keyed thing (SAM's launch, closeByAppId, the session
	# file, the Load payload) then splits between the two answers. Packaging asserts this three
	# times over; deploy is the path used a hundred times a day and had no equivalent, so it gets
	# one for a `python3 -c`.
	@python3 -c "import json,sys; got=json.load(open('$(APPINFO)'))['id']; \
	  sys.exit(0) if got=='$(APPID)' else sys.exit('$(APPINFO) declares id=%s but deploys into $(APPID)' % got)"
	# The bundled FFmpeg, under its SONAMEs — ff.rs dlopens these by absolute path from the app
	# directory. ONE scp for all of them: each connection is a full SSH handshake to a television
	# that is not fast, and this is ~2.1 MB on every deploy. Unconditional, like the fonts —
	# a CHANGED library must be able to reach the TV.
	$(SCP) $(FFMPEG_STAGED) root@$(TV):$(APPDIR)/
	# The native crash daemon is a separate process so it can read the dying process's stack while
	# the signal handler keeps that process parked. It has no network transport of its own. Stage
	# then rename: an already-running app keeps the old daemon executable open, and scp directly to
	# that inode fails with ETXTBSY (observed on the first handler upgrade). Renaming is atomic and
	# leaves the old process on its old inode while the next launch gets this one.
	$(SCP) $(SENTRY_HANDLER) root@$(TV):$(APPDIR)/sentry-crash.new
	$(SSH) 'chmod 755 $(APPDIR)/sentry-crash.new && mv $(APPDIR)/sentry-crash.new $(APPDIR)/sentry-crash'
	# ...then retire any FFmpeg from a PREVIOUS version. `scp` only adds, so bumping the bundled
	# release left the old majors sitting in the app directory forever — observed on the dev TV,
	# which was carrying libavcodec-plx.so.60 and .so.58 from an earlier experiment alongside the
	# live .63/.61. Harmless (ff.rs opens by exact name) but it accumulates, and it ships nothing:
	# the .ipk only ever contains the current set. Removal comes AFTER the copy so there is never
	# a moment with no FFmpeg on the device.
	$(SSH) 'cd $(APPDIR) && for f in lib*-plx.so.*; do case " $(FFMPEG_SONAMES) " in *" $$f "*) ;; *) rm -f "$$f";; esac; done'

	$(SCP) pkg/plxnative root@$(TV):$(APPDIR)/plxnative.new
	@# The lab session file, under LAB=1 only. Shipped by deploy as well as by the .ipk so the
	@# whole path — trigger, snapshot, pinned upload — can be rehearsed on the dev television
	@# before an hour of Cloud Test Lab is spent on it. A non-LAB deploy REMOVES any file a
	@# previous lab deploy left behind: a stale session's secret sitting in an app directory is
	@# the one piece of this feature that outlives the build it belongs to.
	@# ...and CHMOD it. `pkg/lab.json` is 0600 on the host because it holds a session secret, scp
	@# preserves that mode, and the app runs JAILED UNDER ITS OWN UID while the file arrives owned
	@# by root — so the binary cannot read its own configuration and boots `lab: INERT`, which is
	@# indistinguishable from a build that was never given a session. Measured on the dev set,
	@# first device run. 0644 is not a downgrade that matters here: the secret is already sitting
	@# in that app directory, and the jail — not the mode — is what stands between it and anything
	@# else on the television. (`ci/mkipk.py` normalises payload modes, so the .ipk path never had
	@# this problem; only deploy did.)
	@if [ -n "$(LAB)" ]; then $(SCP) pkg/lab.json root@$(TV):$(APPDIR)/lab.json && \
	   $(SSH) 'chmod 644 $(APPDIR)/lab.json'; \
	 else $(SSH) 'rm -f $(APPDIR)/lab.json'; fi
	$(SCP) $(APPINFO) root@$(TV):$(APPDIR)/appinfo.json
	# Copy the fonts unconditionally: the old `test -f || scp` guard meant a CHANGED font could
	# never reach the TV, so a font swap looked like it had no effect. They are ~300 KB.
	$(SCP) pkg/appfont.ttf root@$(TV):$(APPDIR)/appfont.ttf
	$(SCP) pkg/appfont-bold.ttf root@$(TV):$(APPDIR)/appfont-bold.ttf
	# ...and the CJK fallback face, which is 21 MB — the one exception to "unconditional", because
	# it is a minute of scp on every deploy and it changes only when tools/cut-noto-cjk.py is
	# re-run. Without it text.rs's chain has no link 2 and CJK falls through to the system face.
	# The .ipk path (APP_FILES) has no guard at all and is what a real install uses.
	#
	# The guard is an MD5, deliberately not a size compare, and this is not hypothetical caution:
	# the very first re-cut of this font (adding `recalcTimestamp=False` for reproducibility)
	# produced a file of IDENTICAL SIZE and different bytes, because only `head.modified` moved. A
	# size guard would have kept the old font on the television and said nothing — which is exactly
	# the failure the `test -f || scp` guard above was removed for. One ssh round trip either way.
	@l=`md5 -q pkg/appfont-cjk.ttf 2>/dev/null || md5sum pkg/appfont-cjk.ttf | cut -d' ' -f1`; \
	 t=`$(SSH) 'md5sum $(APPDIR)/appfont-cjk.ttf 2>/dev/null | cut -d" " -f1'`; \
	 if [ "$$l" != "$$t" ]; then \
	   echo "  scp pkg/appfont-cjk.ttf (21 MB — absent or re-cut)"; \
	   $(SCP) pkg/appfont-cjk.ttf root@$(TV):$(APPDIR)/appfont-cjk.ttf; \
	 fi
	@if [ -n "$(TURBOJPEG_SO)" ]; then \
	  $(SSH) 'test -f $(APPDIR)/libturbojpeg.so.0' || $(SCP) $(TURBOJPEG_SO) root@$(TV):$(APPDIR)/libturbojpeg.so.0; \
	else echo "note: no libturbojpeg in the sysroot — capture JPEG mode will use the slow encoder"; fi
	@# `scp` preserves the host mode. This checkout runs under umask 077, so `chmod +x` left a
	@# root-owned 0700 binary: SAM could launch it before entering the app jail, but Sentry's
	@# same-uid external reporter could not `execv` it to spool a crash envelope. The .ipk builder
	@# already normalises this member to 0755; make the fast deploy path identical.
	$(SSH) 'mv $(APPDIR)/plxnative.new $(APPDIR)/plxnative && chmod 755 $(APPDIR)/plxnative'

# NB (this webOS build): luna-send must stay subscribed (-i) for the launch to
# take; SAM keeps stale "running" state after a hard kill, so close via SAM
# first or the next launch is a silent no-op relaunch.
#
# BOOT_SH is the close+launch dance itself, shared verbatim by `run` and `run-stream`
# so there is exactly ONE copy of the SAM incantation. It leaves the app running and
# the subscription pid in $$LP; how to observe the app is the caller's business.
#
# The two profiler JSONLs are cleared here for the same reason and under the same rule as the
# event log: they are OUTPUTS the app creates, and a root-owned leftover is one the jailed app
# cannot open — which disables the profiler with a single `Permission denied` line that is easy to
# scroll past. Clearing an output is not disarming a trigger: `make run` deliberately preserves
# `$(RUNDIR)/plxnative-*` scene triggers, including `plxnative-profile` and `plxnative-hwcnt`, so a
# profiling run is armed once and repeated.
#
# Only `rm -f` the log — never pre-create it. The app runs jailed under its own uid
# (not root), so a root-owned 644 file left in place is one the app cannot write: the
# log stays 0 bytes and every assertion reads as a total regression. `tail -F` retries
# until the app creates the file itself, which is exactly what -F is for.
#
# `fuser -k $(APPDIR)/plxnative` is INODE-scoped where `closeByAppId` is ID-scoped, and with two
# installs that difference is the point: both binaries are named `plxnative`, so a name-based kill
# (`pidof`, `killall`) would take down the OTHER install too. Keep it addressed by path.
#
# `mkdir -p` + `chmod 1777` on the runtime root before anything writes into it: two uids write here
# and neither can be made to go second — this shell is root, the app is jailed under its own uid.
# Whoever creates the directory sets its mode, and an owner-only mode locks the other out. A
# root-owned root the app cannot write means a 0-byte event log, which every tool in this repo
# reports as "no line found", i.e. exactly like a total regression. A no-op for the stable flavour,
# whose root is `/tmp` itself (already 1777).
CLOSE_SH = (luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"$(APPID)\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plxnative 2>/dev/null;
BOOT_SH = $(CLOSE_SH) \
	  mkdir -p $(RUNDIR) && chmod 1777 $(RUNDIR); \
	  rm -f $(EVENTLOG) $(RUNDIR)/plxnative-gputime.jsonl $(RUNDIR)/plxnative-hwcnt.jsonl; \
	  luna-send -i "luna://com.webos.applicationManager/launch" "{\"id\":\"$(APPID)\"}" >/dev/null 2>&1 & LP=$$!;

run: tv-lock-require
	@echo "running $(APPID) [$(FLAVOR)] — log $(EVENTLOG)"
	$(SSH) '$(BOOT_SH) \
	  sleep $(RUN_SECS); kill $$LP 2>/dev/null; sleep 1; \
	  cat $(EVENTLOG)'

# Same launch, but stream the event log as it is written instead of sleeping a fixed
# RUN_SECS and catting at the end. tests/run.py grades the stream line-by-line and closes
# the connection the moment a case has passed, which is what stops a 20s case from costing
# 60s. Hanging up kills the tail; the trap takes the luna-send subscription with it so 18
# cases don't leave 18 of them behind. There is no time limit here on purpose — the caller
# owns the deadline (run.py caps each case at its manifest run_secs).
run-stream: tv-lock-require
	$(SSH) '$(BOOT_SH) \
	  trap "kill $$LP 2>/dev/null" EXIT INT TERM HUP; \
	  tail -F -n +1 $(EVENTLOG)'

kill: tv-lock-require
	$(SSH) '$(CLOSE_SH) echo closed $(APPID)'

clean:
	rm -f src/*.o pkg/plxnative

test: deploy run

# `make check` — the HOST unit suite (~0.3s) plus `lint` below, the only correctness signal
# available without a television. Deliberately NOT a prerequisite of `all`: the normal build is a
# cross compile for the TV and must not be made to depend on a host toolchain run succeeding (a host
# cargo failure has nothing to do with whether the ARM staticlib is buildable, and `make deploy`
# on a machine mid-toolchain-churn should not be blocked by it). Run it before `make test`.
#
# No --target and no RUST_ENV here on purpose. Those flags exist to stop the ARM build SIGILL-ing
# on this TV's A53 (see the codegen comment above); pointed at the aarch64 host they are wrong at
# best. A bare `cargo test` builds for the host triple, which is exactly what we want — and it is
# also why this suite cannot cover anything that links the TV's own libraries: ff.rs gates its four
# `#[link]` directives out of cfg(test) so the crate's pure logic stays host-testable, and a test
# that actually calls into FFmpeg or GL fails to link by design. --lib keeps it to the crate's own
# `#[cfg(test)] mod tests` blocks (there are no integration tests in tests/ — that directory is the
# on-device Python harness, a different thing entirely).
# NB the suite APPENDS to `/tmp/plxnative-events.log` on the dev Mac, and deliberately cannot be
# pointed elsewhere. Since 2026-08-15 enough of the tree logs its failures (stream, posters, img,
# account, remote) that host tests exercising those paths write real lines. `PLXNATIVE_RUNTIME_DIR`
# does NOT help: `paths::ENV_STEERABLE` is `cfg!(feature = "hostsim")`, off here, and widening it to
# `cfg(test)` would silently retire `paths.rs`'s own test that a television build ignores that
# variable — the suite is the only place that guarantee is ever checked. Host-side cosmetics are not
# worth a device guarantee. The television is unaffected either way (`make run` and `tests/run.py`
# both clear the log on the TV), so this is only about not confusing yourself locally.
# Built into the shell's temp dir rather than the tree: it is a host binary in a repository whose
# every other artifact is ARM, and one that landed in `src/` or `pkg/` would be a genuinely
# confusing thing to find. `$(TMPDIR)` is set on macOS and empty on a Linux runner, hence the
# fallback.
CRASHFMT_TEST_BIN := $(or $(TMPDIR),/tmp/)plx-crashfmt-test
CRASHTRACE_TEST_BIN := $(or $(TMPDIR),/tmp/)plx-crashtrace-test

check: lint
	@# The telemetry credentials are passed here too, and that is not decoration: `ENVIRONMENT` and
	@# the two compile-time refusals are derived from them, so a host suite run WITHOUT them grades a
	@# configuration nobody builds. With them, `the_environment_matches_the_credential_pair_that_was_
	@# supplied` checks this checkout's actual configuration rather than the empty one.
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" \
	  PLX_SENTRY_DSN='$(PLX_SENTRY_DSN)' PLX_POSTHOG_KEY='$(PLX_POSTHOG_KEY)' \
	  PLX_SENTRY_DSN_DEV='$(PLX_SENTRY_DSN_DEV)' PLX_POSTHOG_KEY_DEV='$(PLX_POSTHOG_KEY_DEV)' \
	  cargo +$(RUST_NIGHTLY) test --lib
	@# The flavour transform, host-side and free. Its central assertion — that the STABLE transform
	@# is the identity — is the mechanical guarantee that having a second app id cannot perturb the
	@# released .ipk, whose sha256 every user's television verifies at install. That property is
	@# worth checking on every host run rather than only in the release job, which is the one place
	@# it would be too late to learn otherwise. It also cross-checks the three copies of the app id
	@# (here, ci/flavor.py, rust-modules/src/paths.rs), which no compiler can.
	python3 ci/flavor.py --selftest
	@# The crash tracer's PURE half (src/crashfmt.h), compiled and RUN with the host compiler.
	@# The tracer runs in signal context on ARM and can only be graded on a television — but the
	@# part of it that has ever been wrong is the parsing, and a `bin:` line naming the wrong
	@# mapping is silent: tools/crash-report.sh subtracts that base and answers with a confident
	@# wrong function. Splitting the pure half out is what makes that decidable here, and writing
	@# the test by watching it fail is what disproved the justification main.c had carried since
	@# the tracer was written. Milliseconds, no NDK, no device.
	cc -O1 -Wall -Wextra -Werror -Isrc -o $(CRASHFMT_TEST_BIN) ci/crashfmt-test.c && $(CRASHFMT_TEST_BIN)
	@# …and then ACTUALLY CRASH a process, five times, through the real handler (src/crashtrace.c,
	@# linked alone — no SDL, no Rust). It asks the one question no log can answer, which is HOW the
	@# process died: a handler that quietly exits looks identical in the crash log and silently
	@# costs SAM its WIFSIGNALED status. (Not a crashd backtrace — this firmware writes no core, so
	@# there is never one to lose; an empty /var/log/reports/librdx/ is expected.) Not hypothetical
	@# — it is what this app did for seven weeks, because the signal is MASKED inside its own
	@# handler, so `raise()` returned and the `_exit(128+sig)` beneath it ran every time.
	cc -O1 -Wall -Wextra -Werror -Isrc -o $(CRASHTRACE_TEST_BIN) ci/crashtrace-test.c src/crashtrace.c && $(CRASHTRACE_TEST_BIN)
	@# The harness's own host unit tests (tests/test_harness.py, stdlib unittest, ~0.5s — most of
	@# it is five `run.py --list` subprocesses, not test logic; measure before budgeting). run.py
	@# decides WHAT gets driven on the one television and had no test of any kind until 2026-08-22.
	@# What it pins is the code path a full manifest.local.json never enters: an `item` key this
	@# installation cannot resolve SKIPS the cases that need it instead of killing the run. A
	@# regression there is invisible here and shows up as a stranger concluding the suite is broken.
	python3 tests/test_harness.py
	@# The three PreToolUse/PostToolUse hooks' own suites (~0.6s together). They were not in this
	@# target until 2026-08-26, which meant the guard that decides whether a private value may
	@# leave this machine was covered by a test nobody ran on a normal check -- the same shape as
	@# the bug that produced the leak it exists to prevent, where the check computed its answer and
	@# printed it instead of gating on it. Cheap, and the one place a weakened rule gets caught.
	python3 .claude/hooks/outbound-guard-test.py
	python3 .claude/hooks/release-config-check-test.py
	python3 .claude/hooks/tv-lock-guard-test.py
	@# The PMS probe's own offline suite (~3ms). It talks to the maintainer's real server with a
	@# real token, so its redaction and its session CLEANUP are the two things that must not
	@# regress -- and both are only covered here. It sat outside this target until 2026-08-27,
	@# which is the same shape as the hook gap above: a suite that exists, passes, and is never
	@# run. `pms-rung-sweep.py` drives that probe once per rung, so its pairing rules ride along.
	python3 tools/test_pms_hls_probe.py
	python3 tools/test_pms_rung_sweep.py
	@# The Cloud Lab diagnostics/control receiver, against a real TLS listener on loopback with a freshly
	@# generated certificate: an accepted upload, a wrong secret, an oversized body, a foreign
	@# path, a bare GET and the rate limit — the six refusals that are the whole of its exposure to
	@# the public internet. ~5 s, most of it the two deliberate rate-limit waits, and it needs no
	@# router: the UPnP half reports what is on this LAN rather than asserting anything.
	@# It runs here because the receiver has no other gate — nothing in cargo can see a python file.
	python3 tools/plxnative-lab selftest
	python3 tools/test_abr_transfer_bound.py
	python3 tools/test_abr_calibrate_plant.py
	python3 tools/test_abr_window_grade.py
	python3 tools/test_scrub_logs.py

# `make lint` — the three clippy lints that catch a SHADOWED branch, the one bug class the unit
# suite structurally cannot reach. `app.rs` shipped a duplicated `else if` whose empty body hid the
# real arm, so OK on the Subtitles/Audio discs did nothing at all — no menu, no log line — and rustc
# does not warn on a repeated condition. That dispatch lives inside the SDL event loop, so there is
# no host test for it; the lint is the whole gate. Explicitly NAMED lints, not a group: `-A
# clippy::all` first because this crate is not clippy-clean and making it so is not this gate's job,
# and naming them means a nightly bump cannot silently widen what `make check` fails on. All three
# are clean as of 2026-07-29 (so is `clippy::correctness` as a group, except ff.rs:1330's deliberate
# `loop { … break; }`). `--all-targets` so the `#[cfg(test)]` blocks are linted too, not just the
# lib. ~12s cold, <1s warm — clippy needs the nightly clippy component, which rustup's DEFAULT
# profile ships (a `--profile minimal` nightly does not).
#
# `if_same_then_else` is the one with a legitimate false positive here: two deliberately-identical
# arms, e.g. `if back { exit_player() } else if stop { exit_player() }` — which app.rs's key chain
# is full of. The escape hatch is this repo's own habit: clippy suppresses it when each arm carries
# its own comment. Comment the arms, do not reach for an `#[allow]`.
lint:
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" cargo +$(RUST_NIGHTLY) clippy --all-targets -- \
	  -A clippy::all \
	  -D clippy::ifs_same_cond -D clippy::same_functions_in_if_condition -D clippy::if_same_then_else

# ipk assembly: deb-style ar archive; the NDK ar emits GNU format (macOS ar is BSD)
# pkg/appinfo.json is the ONE place the version is written; the ipk filename and everything else
# derive from it, and ci/check-package.py asserts ipkroot/ctl/control still agrees. The registry
# reads both out of the archive (webosbrew repogen/ipk_file.py), so a mismatch is a rejected
# submission rather than a warning.
IPK_VERSION := $(shell python3 -c "import json;print(json.load(open('pkg/appinfo.json'))['version'])")
IPK         := pkg/$(APPID)_$(IPK_VERSION)_arm.ipk
# Where the payload is assembled. The DIRECTORY NAME is part of the package's identity — it is
# what `paths::app_id` reads at runtime — so ci/mkipk.py and ci/check-package.py both assert it
# equals the staged `appinfo.json`'s `id`.
STAGE       := ipkroot/data/usr/palm/applications/$(APPID)

# The .ipk is REPRODUCIBLE: same commit + same toolchain -> same sha256. That matters because the
# manifest carries that hash and every user's TV verifies it at install time (there is no code
# signing anywhere in the webosbrew chain — sha256 over HTTPS is the entire integrity story), so a
# non-reproducible archive makes "rebuilt" and "tampered with" indistinguishable.
#   - ci/mkipk.py normalises uid/gid/uname/gname/mtime/mode/order and the gzip header. `tar czf`
#     was embedding `gleblinnik/staff` in every shipped archive.
#   - `ar` gets D (deterministic): binutils' default embeds the builder's uid and a real mtime.
# `make SYMBOLS=1 symbols` — separate the debug info into `pkg/plxnative.debug`.
#
# What this is FOR: a crash reported from a stranger's television carries an address, and the
# binary they are running is stripped. Matching the two needs a debug file identified by the same
# BUILD ID, which `-Wl,--build-id=sha1` on the link puts in an allocated note that survives
# `strip`. Verified end to end 2026-08-29 — full, `.debug` and stripped all carry
# `cc4a5c7b3923da5e872ee3c8f5054a3b23f07568`, and `addr2line -e pkg/plxnative.debug` resolves an
# address the stripped binary answers `?? ??:0` for.
#
# **It refuses without SYMBOLS=1 rather than producing an empty shell.** `objcopy --only-keep-debug`
# on a binary with no `.debug_*` sections succeeds and writes a file; that file matches nothing and
# fails only much later, at the symbol server, on somebody else's crash. Fail here instead.
symbols: pkg/plxnative
ifneq ($(SYMBOLS),1)
	@echo "make symbols needs SYMBOLS=1 — without it this binary carries no DWARF and" >&2
	@echo "objcopy would write an empty .debug that silently matches nothing." >&2
	@echo "  correct: make RELEASE=1 SYMBOLS=1 ipk symbols" >&2
	@false
else
	$(TOOLPREFIX)objcopy --only-keep-debug pkg/plxnative pkg/plxnative.debug
	@# The build ids MUST agree, and asserting it here is the point: everything downstream —
	@# the DIF upload, the symbol server's lookup, `addr2line` against the right file — keys on
	@# this one value, and a mismatch is invisible until a real crash fails to symbolize.
	@bin=$$($(TOOLPREFIX)readelf -n pkg/plxnative | sed -n 's/.*Build ID: //p'); \
	 dbg=$$($(TOOLPREFIX)readelf -n pkg/plxnative.debug 2>/dev/null | sed -n 's/.*Build ID: //p'); \
	 if [ -z "$$bin" ]; then echo "pkg/plxnative has NO build id — is -Wl,--build-id still on the link?" >&2; exit 1; fi; \
	 if [ "$$bin" != "$$dbg" ]; then echo "build id mismatch: binary $$bin, debug $$dbg" >&2; exit 1; fi; \
	 echo "symbols: pkg/plxnative.debug  build-id $$bin  ($$(du -h pkg/plxnative.debug | cut -f1))"
endif

# Validate and upload the exact debug file that matches `pkg/plxnative`. This is a separate target
# so a development build used for an on-device crash test gets the same fail-closed pairing as CI.
# `SENTRY_AUTH_TOKEN` is read only from the process environment and is never echoed or written.
SENTRY_ORG     ?= gleb-linnik
SENTRY_PROJECT ?= plx-native
SENTRY_CLI     ?= npx --yes @sentry/cli@latest
sentry-symbols: symbols
	@test -n "$${SENTRY_AUTH_TOKEN:-}" || { \
	  echo "sentry-symbols: SENTRY_AUTH_TOKEN is required" >&2; exit 1; }
	@SENTRY_ORG='$(SENTRY_ORG)' SENTRY_PROJECT='$(SENTRY_PROJECT)' \
	  $(SENTRY_CLI) debug-files check pkg/plxnative.debug
	@SENTRY_ORG='$(SENTRY_ORG)' SENTRY_PROJECT='$(SENTRY_PROJECT)' \
	  $(SENTRY_CLI) debug-files upload --include-sources pkg/plxnative.debug pkg/plxnative

ipk: pkg/plxnative $(APPINFO) release-guard
	@echo "packaging $(if $(RELEASE),RELEASE,dev) build ($(RUST_CFG)) as $(APPID) [$(FLAVOR)]"
	rm -rf ipkroot/data/usr && mkdir -p $(STAGE)/licenses
	cp $(APP_FILES) $(STAGE)/
	cp $(LICENSE_FILES) $(STAGE)/licenses/
	@# Strip the STAGED copy only, never pkg/plxnative. ~2.4 MB of .symtab+.strtab (30% of the
	@# download, on a device whose app partition is 615 MB total and shared with every other app).
	@# It must not be pkg/plxnative because tools/crash-report.sh symbolizes a crash PC against
	@# that local binary AND md5-compares it to the on-TV copy to prove they are the same build —
	@# stripping in place would break the identity check and lose function names from every
	@# release crash report. Deploy ships the unstripped one by design; only the ipk is stripped.
	$(TOOLPREFIX)strip --strip-unneeded $(STAGE)/plxnative
	@# Only THIS flavour's artifact — packaging one must never delete the other's.
	rm -f pkg/$(APPID)_*_arm.ipk
	FLAVOR=$(FLAVOR) python3 ci/mkipk.py
	@# Emitted from INSIDE pkg/ so the line carries the bare filename. With the `pkg/` prefix
	@# in it, `shasum -a 256 -c ipk.sha256` fails for everyone who downloads the two release
	@# assets side by side — which is every user, and is what shipped through v0.2.1.
	@# STABLE ONLY: `ipk.sha256` is a published RELEASE ASSET NAME, quoted verbatim in every
	@# release note's verification command. A flavoured package writing it would replace the
	@# checksum that describes the artifact users downloaded with one for a build they cannot get.
	@if [ "$(FLAVOR)" = stable ]; then cd pkg && $(SHA256SUM) $(notdir $(IPK)) | tee ipk.sha256; \
	 else $(SHA256SUM) $(IPK); fi
	@# ...and the packaging gates run HERE, not only in CI. Every assertion in check-package.py was
	@# written because something shipped broken, and until this line the machine that built the
	@# package was the one machine that never ran them — the first sight of a failure was a push.
	python3 ci/check-package.py

# THE STABLE INSTALL IS ALWAYS A RELEASE BUILD, and that is a gate rather than a habit.
#
# `com.beb.plxnative` is the id users get. A dev-featured binary under it carries the whole
# `/tmp` trigger surface, the world-writable `plxnative-remote` FIFO and the `:8910` capture
# listener — the exact surface the `cut-release` skill's §2 exists to keep out of a shipped
# artifact, seen from the other side. Before the flavour split this could only happen by
# publishing by hand, which is how v0.2.1's defects got out; now it is one forgotten `RELEASE=1`
# on a machine that also has a television, so it gets a mechanism.
#
# The escape hatch is named rather than absent, because there is one legitimate use — reproducing a
# user's report against the shipped id with instrumentation on — and a gate with no hatch gets
# deleted rather than respected.
# A LAB BUILD IS NEVER THE STABLE ID, and it never ships without its session file.
#
# Both halves are the same argument as `release-guard`'s, one feature along. `com.beb.plxnative` is
# the id users install; a lab-featured binary under it carries an upload endpoint and a bearer
# secret, which is the one thing in this repository that must never reach a stranger's television.
# And a LAB build with no `pkg/lab.json` is inert — it boots, logs `lab: INERT`, and answers the
# button with nothing, which in a rented Cloud Test Lab hour is indistinguishable from the colour
# key not being delivered at all. Failing here names the command that fixes it.
release-guard: lab-guard
lab-guard:
	@if [ -n "$(LAB)" ] && [ "$(FLAVOR)" = stable ] && [ -z "$(ALLOW_DEV_ON_STABLE)" ]; then \
	  echo "refusing to put a LAB build on $(APPID_STABLE) — that id is what users install."; \
	  echo "  developer install:  make LAB=1 $(firstword $(MAKECMDGOALS))          (FLAVOR=$(FLAVOR) is not the default)"; \
	  exit 1; fi
	@if [ -n "$(LAB)" ] && [ ! -f pkg/lab.json ]; then \
	  echo "LAB=1 but pkg/lab.json is missing — the build would ship with no session."; \
	  echo "  start a receiver first:  tools/plxnative-lab start"; \
	  echo "  (it writes pkg/lab.json for you; docs/lab-diagnostics.md)"; \
	  exit 1; fi

release-guard:
	@if [ "$(FLAVOR)" = stable ] && [ -z "$(RELEASE)" ] && [ -z "$(ALLOW_DEV_ON_STABLE)" ]; then \
	  echo "refusing to put a DEV build on $(APPID_STABLE) — that id is what users install."; \
	  echo "  release build:      make FLAVOR=stable RELEASE=1 $(firstword $(MAKECMDGOALS))"; \
	  echo "  developer install:  make $(firstword $(MAKECMDGOALS))          (FLAVOR=$(FLAVOR) is not the default)"; \
	  echo "  really meant it:    make FLAVOR=stable ALLOW_DEV_ON_STABLE=1 $(firstword $(MAKECMDGOALS))"; \
	  exit 1; fi

# --- installing a flavour on the television ---------------------------------------------------
#
# `make deploy` scp's into an app directory that ALREADY EXISTS and is already registered. Creating
# a second app is a different operation: SAM has to learn the id, and the LS2 role file that lets
# the app talk to `com.webos.media.*` is written by the installer. So a flavour is installed once,
# from its own .ipk, exactly the way a user installs the real one — which is also the only path
# that exercises the package (`make deploy` never consults packageinfo.json, which is how a missing
# one hid for months; see ci/mkipk.py).
#
# AND THEN IT DEPLOYS, deliberately. appinstalld replaces `applications/<id>/` WHOLESALE — the same
# fact that keeps the session file outside it (paths.rs) — so an install wipes whatever was in
# there and leaves the PACKAGED binary behind. Ending here would leave you looking at a build you
# did not make, which is the "plausible wrong data" failure this repo cares most about.
install: ipk tv-lock-require
	@echo "installing $(IPK) as $(APPID) on $(TV)"
	$(SCP) $(IPK) root@$(TV):/tmp/
	@# `script -qc` because luna-send needs a tty (see the webos-screen-capture note); `-i` keeps
	@# the subscription open long enough for appinstalld to report, which is the only place a
	@# failure is ever named.
	$(SSH) 'script -qc "luna-send -i -a com.webos.appInstallService \
	  luna://com.webos.appInstallService/dev/install \
	  \"{\\\"id\\\":\\\"$(APPID)\\\",\\\"ipkUrl\\\":\\\"/tmp/$(notdir $(IPK))\\\",\\\"subscribe\\\":true}\"" /dev/null' | head -20
	$(SSH) 'rm -f /tmp/$(notdir $(IPK))'
	@$(MAKE) --no-print-directory deploy FLAVOR=$(FLAVOR) RELEASE=$(RELEASE) TV=$(TV)
	@echo "installed and deployed $(APPID)"

# Remove a flavour from the television. Refuses the stable id: uninstalling the app the household
# watches with is not something a make target should make easy, and appinstalld gives no undo.
uninstall: tv-lock-require
	@test "$(FLAVOR)" != stable || { echo "refusing to uninstall $(APPID_STABLE) — do that from the TV's own app list"; exit 1; }
	$(SSH) 'script -qc "luna-send -i -a com.webos.appInstallService \
	  luna://com.webos.appInstallService/dev/remove \
	  \"{\\\"id\\\":\\\"$(APPID)\\\",\\\"subscribe\\\":true}\"" /dev/null' | head -20
	$(SSH) 'rm -rf $(RUNDIR)' || true
	@echo "removed $(APPID)"

# tools/threadprobe.c — measures where pthread_create actually gives up on the TV (the question
# behind rust-modules/src/task.rs). Standalone diagnostic: not linked into the app, not deployed
# by `make deploy`. Build it, scp it, run it as root, delete it.
threadprobe: tools/threadprobe.c
	$(CC) $(CFLAGS) -o pkg/threadprobe tools/threadprobe.c -lpthread

# tools/sockprobe.c — socket semantics the host suite can't answer (cargo test runs on macOS, the
# app runs on Linux, and they disagree about shutdown-during-connect). Same deal: build, scp, run,
# delete.
sockprobe: tools/sockprobe.c
	$(CC) $(CFLAGS) -o pkg/sockprobe tools/sockprobe.c -lpthread

# tools/logmprobe.c — read/flip LG's KADP log MASK on a RUNNING app. The masks are BITWISE and
# level 2 is off on this set, so the absence of a level-2 line proves nothing; this is what makes
# the Dolby Vision metadata path observable. Read-only by default. See the file's header.
logmprobe: tools/logmprobe.c
	$(CC) $(CFLAGS) -o pkg/logmprobe tools/logmprobe.c

# tools/mali-hwcnt-probe.c — userspace-only validation of the exact Midgard r12p0 vinstr ABI used
# by the in-app profiler.  It is standalone, never linked into or deployed with the application.
mali-hwcnt-probe: tools/mali-hwcnt-probe.c
	$(CC) $(CFLAGS) -o pkg/mali-hwcnt-probe tools/mali-hwcnt-probe.c

# ---------------------------------------------------------------------------------------------
# The desktop UI simulator — the same app core against a desktop SDL2 + desktop GL, no television.
#
# It exists because the TV serializes the whole dev loop: one set, one app instance, and two
# `tests/run.py` jobs kill each other's app. Several simulators run at once, each with its own
# instance root, so UI and data-layer work can proceed in parallel. It has NO video (the 29-symbol
# Starfish/ACB seam does not exist off-device) and its frame rates describe this Mac, not the panel.
#
# `SIM_PMS` defaults to the host compiled into src/config.local.h so the common case needs no
# argument. SIM_DIR is the instance root: give each concurrent simulator a different one.
# Read one #define out of the gitignored header. `[^"]*`, NOT `.*`: the greedy form captures
# everything up to the LAST quote on the line, so a trailing comment containing a quote silently
# puts garbage in the value. tools/tv-session.sh and tests/run.py both already use this form.
cfg_macro = $(shell sed -n 's/^\#define[ \t]*$(1)[ \t]*"\([^"]*\)".*/\1/p' src/config.local.h 2>/dev/null)
SIM_PMS  ?= $(call cfg_macro,PMS_HOST)
SIM_PORT ?= $(shell sed -n 's/^\#define[ \t]*PMS_PORT[ \t]*\([0-9]*\).*/\1/p' src/config.local.h 2>/dev/null)
SIM_DIR  ?= /tmp/plxnative-sim
# Its OWN target dir, per this file's rule for feature-set splits: `make check` builds default
# features on nightly, `make sim` builds `hostsim` on the default toolchain. Sharing one dir makes
# each invocation rebuild the crate the other way round.
#
# `?=` so it can also come from the environment: a checkout on a network or external volume
# (/Volumes/…, SMB, some external SSDs) cannot be a cargo target dir at all — those filesystems do
# not implement flock, and cargo fails with "could not create session directory lock file
# (os error 45)" before compiling anything. Point this at a local path and the checkout can stay
# where it is:  export SIM_TDIR=$HOME/plxnative-sim-target
SIM_TDIR  ?= rust-modules/target-sim
SIM_BIN   = $(SIM_TDIR)$(if $(LAB),-lab,)/debug/plxnative-sim
# Which presented frame `sim-shot` grabs. 200 is comfortably past first paint and the poster
# fetches on a warm cache; raise it if a shot catches a screen mid-load.
SIM_FRAME ?= 200
SIM_SHOT  ?= $(SIM_DIR)/shot.png
# Shared by every sim recipe so the wiring and the error sentence have exactly one copy — the same
# reason BOOT_SH exists for `run`/`run-stream`.
# Window size for the simulator, in DRAWABLE pixels. Empty = fit the display (see
# `desktop_window_size`), which on a 1x screen is half the authored canvas and therefore half the
# resolution of every screenshot. Set both to look at the UI the size it is drawn:
#   make sim-shot SIM_W=1920 SIM_H=1080
SIM_W ?=
SIM_H ?=
SIM_WIN = $(if $(and $(SIM_W),$(SIM_H)),PLXNATIVE_WIN=$(SIM_W)x$(SIM_H),)
SIM_ENV = PLXNATIVE_RUNTIME_DIR=$(SIM_DIR) PLXNATIVE_APP_DIR=$(CURDIR)/pkg $(SIM_WIN)
SIM_PRE = mkdir -p $(SIM_DIR); test -n "$(SIM_PMS)" || \
          { echo "no PMS host — set SIM_PMS=<ip> or add PMS_HOST to src/config.local.h"; exit 1; }

# **The simulator needs its own FFmpeg, and that is what makes it able to STREAM.** `ff.rs` opens
# the bundled libraries by absolute path out of the app directory, where they are 32-bit ARM ELF —
# so until 2026-08-28 the entire streaming half of the app (both AVIO transports, the HLS demux,
# the AU queues and therefore the whole adaptive controller) was device-only, and `make sim`
# logged `ff: FFmpeg unavailable`. `HOST=1 ci/build-ffmpeg.sh` builds the SAME FFmpeg 9.0 from the
# SAME component list for this Mac; `ci/stage-host-ffmpeg.sh` puts it in pkg/ with loader-relative
# names. `APP_FILES` is an explicit list, so none of it can reach an .ipk or a television.
FFMPEG_HOST_PREFIX = vendor/ffmpeg-prefix-host
FFMPEG_HOST_INC    = $(FFMPEG_HOST_PREFIX)/include
FFMPEG_HOST_NAMES  = libavutil-plx.61 libavcodec-plx.63 libavformat-plx.63 libswscale-plx.10
FFMPEG_HOST_STAGED = $(addprefix pkg/,$(addsuffix .dylib,$(FFMPEG_HOST_NAMES)))

$(FFMPEG_HOST_INC)/libavformat/avformat.h:
	HOST=1 ./ci/build-ffmpeg.sh

$(FFMPEG_HOST_STAGED): pkg/%.dylib: $(FFMPEG_HOST_INC)/libavformat/avformat.h ci/stage-host-ffmpeg.sh
	@mkdir -p pkg
	./ci/stage-host-ffmpeg.sh $*

# The same ABI gate the cross build runs, at the other pointer width. ci/ffabi-assert.c `#if`s on
# `__SIZEOF_POINTER__` and carries both tables, so this compile is what holds ff.rs's 64-bit
# constants in place — and it is the reading that found `AVSubtitleRect`'s field order.
pkg/.ffabi-host-ok: ci/ffabi-assert.c $(FFMPEG_HOST_INC)/libavformat/avformat.h Makefile
	@mkdir -p pkg
	cc -I $(FFMPEG_HOST_INC) -std=c11 -c ci/ffabi-assert.c -o /dev/null
	@touch $@

# LAB=1 adds the Lab Diagnostics feature here too, and that is not a curiosity: it is the only way
# to exercise the WHOLE upload path — trigger, snapshot, scrub, gzip, pinned TLS POST, receiver —
# without a television and without opening a port on the router (`tools/plxnative-lab start
# --hostname 127.0.0.1 --no-upnp`). The simulator reads `lab.json` out of its app dir, which is
# `pkg/`, which is where `plxnative-lab start` writes it. It has its own target dir for the same
# reason every other feature set does.
#
# The host FFmpeg is a prerequisite of BOTH configurations: a lab simulator that cannot demux
# would exercise the upload path over a playback that never started.
sim: $(FFMPEG_HOST_STAGED) pkg/.ffabi-host-ok
	PLX_SENTRY_DSN='$(PLX_SENTRY_DSN)' PLX_POSTHOG_KEY='$(PLX_POSTHOG_KEY)' \
	  PLX_SENTRY_DSN_DEV='$(PLX_SENTRY_DSN_DEV)' PLX_POSTHOG_KEY_DEV='$(PLX_POSTHOG_KEY_DEV)' \
	  cargo build --manifest-path rust-modules/Cargo.toml --target-dir $(SIM_TDIR)$(if $(LAB),-lab,) --features hostsim$(if $(LAB), --features lab-diagnostics,) --bin plxnative-sim

# Interactive: opens a window. Ctrl-C to quit.
sim-run: sim
	@$(SIM_PRE)
	$(SIM_ENV) $(SIM_BIN) $(SIM_PMS) $(SIM_PORT)

# Headless: boot, settle, write ONE png, exit. This is the agent-facing entry point.
sim-shot: sim
	@$(SIM_PRE)
	$(SIM_ENV) PLXNATIVE_SHOT=$(SIM_SHOT) PLXNATIVE_SHOT_FRAME=$(SIM_FRAME) PLXNATIVE_SHOT_EXIT=1 \
	  $(SIM_BIN) $(SIM_PMS) $(SIM_PORT)
	@echo "wrote $(SIM_SHOT)"

# Copy the owner token out of the gitignored header into this instance's root, so the simulator
# boots straight to a signed-in Home. Same mechanism `tests/run.py` uses on the TV; the value is
# never echoed. It is a SHORTCUT, not the only way in: plex.tv QR sign-in works on the desktop as
# of 2026-08-16 (net.rs's candidate list gained macOS's libcurl, and `dynlib!` learned to bind a
# variadic C function correctly) — this line used to say it could not.
sim-token:
	@mkdir -p $(SIM_DIR)
	@printf '%s' '$(call cfg_macro,PMS_TOKEN)' > $(SIM_DIR)/plxnative-token
	@test -s $(SIM_DIR)/plxnative-token || { echo "no PMS_TOKEN in src/config.local.h"; rm -f $(SIM_DIR)/plxnative-token; exit 1; }
	@echo "token staged in $(SIM_DIR)"

# `make sim-play` — a REAL playback session on this Mac, with no television and no Plex.
#
# Arms the CLOCK SINK (`rust-modules/src/player/ffi_host.rs`): access units are accepted, discarded,
# and a presentation clock advances at real time, reporting position at the television's own
# measured 5 Hz. Nothing decodes. What that exercises is everything between the demuxer and the
# decoder — the AU queues and their byte-cap backpressure, the feed-ahead throttle, the engine's
# Load/Play sequence, the exported-window video path, the HUD and the position clock — none of
# which had any host coverage at all before.
#
# **The source is a raw Annex-B sample, deliberately.** `sample.h264` bypasses `ff.rs` entirely,
# which matters because the bundled FFmpeg is an ARM build and `ff.rs`'s struct offsets are
# hard-coded for 32-bit ARM EABI (`ci/ffabi-assert.c` asserts `AVDictionaryEntry` is two 32-bit
# pointers). Demuxing on a 64-bit host needs a second offset table and a host FFmpeg 9.0; until
# then the STREAMING half of the pipeline stays device-only and this target covers the rest.
#
# Needs no PMS, so it does not go through SIM_PRE. `SIM_SECS` bounds it.
SIM_SAMPLE ?=
SIM_SECS   ?= 20
sim-play: sim
	@test -n "$(SIM_SAMPLE)" || { echo "SIM_SAMPLE=<file.h264> is required — an Annex-B elementary stream WITH access-unit delimiters, e.g."; \
	  echo "  ffmpeg -i clip.ts -c:v copy -an -bsf:v h264_metadata=aud=insert -f h264 /tmp/sample.h264"; exit 1; }
	@mkdir -p $(SIM_DIR)
	@rm -f $(SIM_DIR)/plxnative-playurl $(SIM_DIR)/plxnative-events.log
	@cp $(SIM_SAMPLE) $(SIM_DIR)/sample.h264
	@touch $(SIM_DIR)/plxnative-clocksink $(SIM_DIR)/plxnative-autoplay $(SIM_DIR)/plxnative-stats
	$(SIM_ENV) PLXNATIVE_SHOT=$(SIM_SHOT) PLXNATIVE_SHOT_FRAME=$$(( $(SIM_SECS) * 60 )) \
	  PLXNATIVE_SHOT_EXIT=1 $(SIM_BIN) 127.0.0.1 32400 || true
	@echo "--- $(SIM_DIR)/plxnative-events.log ---"
	@grep -E 'clocksink|bf_split|SMP |vplane|route=player' $(SIM_DIR)/plxnative-events.log | head -20

sim-clean:
	rm -rf $(SIM_DIR)

# ---------------------------------------------------------------------------------------------
# PlxNative.app — the same host build, packaged as a SELF-CONTAINED macOS application you can send
# to somebody who has none of this installed. `ci/mkmacapp.py` is the whole recipe and its module
# doc is the account of the three ways it silently ships broken; `docs/macos-app.md` is the design
# note (what it can and cannot do — chiefly: no video off-device).
#
# Its own target dir, per this file's rule for feature-set splits: this one is `--release
# --no-default-features --features hostsim`, which is a fourth configuration, and sharing a dir
# with `sim` would make each invocation rebuild the crate the other way round.
MACAPP_TDIR ?= rust-modules/target-macapp

macapp:
	MACAPP_TDIR=$(MACAPP_TDIR) ci/mkmacapp.py

# The artifact to actually send: a ditto archive (which preserves the signature) beside the bundle.
macapp-zip:
	MACAPP_TDIR=$(MACAPP_TDIR) ci/mkmacapp.py --zip

# ---------------------------------------------------------------------------------------------
# `make fixtures` — SYNTHESIZE THE TEST MEDIA. Host-side, and there is NO TELEVISION anywhere in
# it: it is ffmpeg + lavfi on this Mac, writing into $(FIXTURES_OUT) (default ~/plxnative-fixtures,
# OUTSIDE the repo — tests/fixtures/make_fixtures.py refuses an --out inside it, because media must
# never be committed).
#
# What it is for: `./tests/run.py --server` grades the player against nine symbolic item SHAPES, and
# tests/manifest.local.json maps each to a ratingKey on whatever PMS you own. That mapping is the
# entire barrier to entry — the shapes include a TrueHD default with an AC-3 sibling, a Dolby
# Vision 8.1 base layer, a PGS bitmap subtitle track and an eight-track audio file with English DTS
# at ordinal 6, and two of those have no freely-licensed example anywhere. This builds all of them
# from nothing, lays them out in two Plex-scannable trees, and writes a fixtures.json saying what
# it PROVED about each file. tests/fixtures/README.md is the walkthrough, including the shapes it
# cannot solve (the three marker_* cases) and why.
#
# Deliberately NOT a prerequisite of `all` or `check`, and never run by them: this writes ~2.5 GB
# and takes ~20 minutes, which is not something a build should do because somebody typed `make`.
# `fixtures-quick` builds the same shapes at ~20 s each in about a minute — structurally correct,
# and NOT suite-valid, since every seek/resume/marker depth the suite asserts is deeper than that.
FIXTURES_OUT ?= $(HOME)/plxnative-fixtures

# $(FIXTURES_OUT) is QUOTED: it defaults under $(HOME), and a home directory with a space in it
# would otherwise split into two arguments and fail as an unknown option.
fixtures:
	python3 tests/fixtures/make_fixtures.py --out "$(FIXTURES_OUT)" $(FIXTURES_ARGS)

fixtures-quick:
	python3 tests/fixtures/make_fixtures.py --quick --out "$(FIXTURES_OUT)" $(FIXTURES_ARGS)

# ...and the PIPELINE tier's pack, which is a different thing for a different suite. `fixtures`
# above builds media for the INTEGRATION suite: full-length, laid out in two Plex-scannable trees,
# because those cases go through a PMS and every duration in them is a Plex constant (the ~90%
# watched threshold that drops a seeded resume point, the marker windows, the Up Next tail).
# `./tests/run.py` (the DEFAULT tier since 2026-08-22) needs none of that — it serves these files
# off this machine over HTTP
# and plays them through plxnative-playurl with no Plex anywhere — so the pack is short clips in a
# FLAT directory, ~0.7 GB and a few minutes instead of ~3 GB and twenty. Same root, its own
# subdirectory, which is also where the harness looks by default.
#
# It grew from ~0.4 GB on 2026-08-23, when the RESOLUTION x CODEC matrix landed (LG App Self
# Checklist #50/#51): six new clips at SD/HD/FHD/UHD, of which the 4K H.264 one is 93 MB on its
# own, plus a deliberately SHORT 20 s clip that exists to run out (#46). Resolution is a property
# of the media, so a matrix cell cannot be a second case on an existing file — that is the whole
# reason this is disk rather than manifest. `--only <shape>` rebuilds one.
fixtures-pipeline:
	python3 tests/fixtures/make_fixtures.py --tier pipeline --out "$(FIXTURES_OUT)/pipeline" $(FIXTURES_ARGS)

# Retrieve whichever profiler JSONL the last run produced. Both are fetched best-effort because a
# leg arms exactly one of the two modes, never both.
# ...and out of THIS INSTALL's runtime root, which is where the app writes them
# (`paths::in_runtime_dir`) and where BOOT_SH clears them. It was two `/tmp` literals: at the
# default flavour the profiler wrote into `/tmp/<app id>/` while this reached for `/tmp/`, and
# because both lines are `-` prefixed the miss was swallowed — so a profiling run that produced
# exactly the data asked for reported "no profiler output on the TV".
fetch-profile:
	-$(SCP) root@$(TV):$(RUNDIR)/plxnative-gputime.jsonl pkg/plxnative-gputime.jsonl
	-$(SCP) root@$(TV):$(RUNDIR)/plxnative-hwcnt.jsonl pkg/plxnative-hwcnt.jsonl
	@ls -l pkg/plxnative-*.jsonl 2>/dev/null || echo "no profiler output in $(RUNDIR) on the TV ($(APPID))"

.PHONY: symbols sentry-symbols sentry-native all setup-env telemetry-local deploy run run-stream kill check lint test ipk clean tv-lock-require threadprobe sockprobe logmprobe mali-hwcnt-probe sim sim-run sim-shot sim-token sim-clean macapp macapp-zip fixtures fixtures-quick fixtures-pipeline fetch-profile \
        release-guard lab-guard install uninstall $(QUERY_GOALS)
