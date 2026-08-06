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
# make ipk      — repackage pkg/com.beb.plxnative_<version>_arm.ipk (version from appinfo.json)
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
TV       ?= $(strip $(shell cat .tv-host 2>/dev/null))
# Expanded only inside a recipe, so `make`, `make check` and `make ipk` never need a TV at all —
# but anything that talks to one fails with this sentence instead of dialling `root@`.
# `alpine` is NOT a secret: it is webosbrew's published dev-mode root password, the same on every
# rooted webOS TV. It stays in the clear because removing it would break the loop for everyone and
# protect nothing. The ADDRESS is the part that identified one household, and that is now local.
TV_OR_DIE = $(if $(TV),$(TV),$(error no TV configured — put its IP in .tv-host, or pass TV=<ip>))
SSH       = sshpass -p alpine ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@$(TV_OR_DIE)
SCP       = sshpass -p alpine scp -O -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
APPDIR    = /media/developer/apps/usr/palm/applications/com.beb.plxnative
RUN_SECS ?= 18

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
CFLAGS       = --sysroot=$(SYSROOT) -O2 -Iinclude -Isrc -Ivendor/nanosvg -D_GNU_SOURCE
# DEBUG=1 keeps DWARF in the binary so a crash PC symbolizes to file:line instead of just
# a function name (tools/crash-report.sh / the crash-triage skill). Same codegen, bigger
# binary — deploy it only while chasing a crash.
ifeq ($(DEBUG),1)
# -DPLX_DEBUG lets the C shim keep core dumps enabled for a post-mortem (src/main.c's
# setrlimit(RLIMIT_CORE, 0) — a shipping build must not write 200 MB into the TV's app
# partition). This is the only thing DEBUG=1 changes about behaviour rather than debuginfo.
CFLAGS      += -g -DPLX_DEBUG
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
RUST_ENV = RUSTFLAGS="-C target-cpu=cortex-a9 -C target-feature=-neon $(RUST_DEBUGINFO) $(RUST_REMAP)"
# -Iinclude keeps the TV's SDL2/GLES2 headers (its SDL is a 2.0.4 fork) ahead of
# the NDK's newer sysroot copies, so we compile against the ABI the TV runs.

# Real sysroot libraries. Every one of these has the SAME SONAME on every webOS release from
# 2.2.3 to 11.2.0 (`tools/fwcompat.py --inventory` will show you), which is what makes linking
# them normally — with real link-time symbol checking — the right call.
#   libpf-1.0 carries mediapipeline::CustomPipeline (the webOS<11 seek path).
LIBS_REAL = -lSDL2 -lSDL2_ttf -lGLESv2 -lluna-service2 -lglib-2.0 \
            -lwayland-client -lplayerAPIs -lpf-1.0
# NOT LISTED, DELIBERATELY: FFmpeg, libcurl and libAcbAPI. Their SONAMEs move between releases
# (FFmpeg 55->57->58->59->60, curl .so.5->.so.4, ACB deleted outright at webOS 5.0), and a
# DT_NEEDED entry for a name the device lacks kills the process at exec() — before main, before
# the event log exists. They are dlopen'd by SONAME candidate list instead: see
# rust-modules/src/dynlib.rs, and the video-plane comment at the top of src/starfish.c.

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
RUST_FEATFLAGS = $(if $(RELEASE),--no-default-features,)
RUST_TDIR      = target$(if $(RELEASE),-release,)
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
RUST_CFG       = features:$(RUST_FEATFLAGS)
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
ifneq ($(RUST_CFG),$(shell cat $(RUST_STAMP) 2>/dev/null))
  $(shell mkdir -p pkg && printf '%s' '$(RUST_CFG)' > $(RUST_STAMP) && rm -f pkg/plxnative)
endif

RUST_TARGET = arm-unknown-linux-gnueabi
RUST_LIB    = rust-modules/$(RUST_TDIR)/$(RUST_TARGET)/release/libplxnative_modules.a

SRCS = $(filter-out src/gpdebug.c,$(wildcard src/*.c))   # = main.c + starfish.c + svg.c
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
	  cargo +$(RUST_NIGHTLY) build --release --target $(RUST_TARGET) \
	    --target-dir $(RUST_TDIR) $(RUST_FEATFLAGS)

# link C objects + the Rust staticlib. gcc pulls in libgcc_s (the ARM-EHABI
# unwinder Rust's panic_unwind std references) + libc/pthread/dl/m/rt itself.
pkg/plxnative: $(OBJS) $(RUST_LIB) $(FFMPEG_STAGED) Makefile
	$(CC) $(CFLAGS) $(OBJS) $(RUST_LIB) $(LIBS_REAL) -ldl -lpthread -lm -o $@

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
APP_FILES = pkg/plxnative pkg/appinfo.json pkg/icon.png pkg/largeIcon.png pkg/splash.png \
            pkg/appfont.ttf pkg/appfont-bold.ttf pkg/OFL.txt THIRD-PARTY-NOTICES.md \
            $(FFMPEG_STAGED)
# TRADEMARKS.md ships too: it carries the brand reservation and the Plex/LG non-affiliation
# statement, which used to be appended to LICENSE. It was moved out because GitHub's `licensee`
# matches LICENSE against known texts by SIMILARITY, and the appended thirty lines pushed the file
# under the threshold — so the repository advertised "Other" rather than MIT, misrepresenting the
# terms in the one place most people look. Splitting the file must not un-ship the reservation.
LICENSE_FILES = LICENSE TRADEMARKS.md $(wildcard licenses/*.txt)

deploy: pkg/plxnative $(FFMPEG_STAGED)
	@echo "deploying $(if $(RELEASE),RELEASE,dev) build ($(RUST_CFG))"
	# The bundled FFmpeg, under its SONAME — ff.rs dlopens these by absolute path from the app
	# directory. Unconditional, like the fonts: a CHANGED library must be able to reach the TV.
	for f in $(FFMPEG_SONAMES); do $(SCP) pkg/$$f root@$(TV):$(APPDIR)/$$f; done
	$(SCP) pkg/plxnative root@$(TV):$(APPDIR)/plxnative.new
	$(SCP) pkg/appinfo.json root@$(TV):$(APPDIR)/
	# Copy the fonts unconditionally: the old `test -f || scp` guard meant a CHANGED font could
	# never reach the TV, so a font swap looked like it had no effect. They are ~300 KB.
	$(SCP) pkg/appfont.ttf root@$(TV):$(APPDIR)/appfont.ttf
	$(SCP) pkg/appfont-bold.ttf root@$(TV):$(APPDIR)/appfont-bold.ttf
	@if [ -n "$(TURBOJPEG_SO)" ]; then \
	  $(SSH) 'test -f $(APPDIR)/libturbojpeg.so.0' || $(SCP) $(TURBOJPEG_SO) root@$(TV):$(APPDIR)/libturbojpeg.so.0; \
	else echo "note: no libturbojpeg in the sysroot — capture JPEG mode will use the slow encoder"; fi
	$(SSH) 'mv $(APPDIR)/plxnative.new $(APPDIR)/plxnative && chmod +x $(APPDIR)/plxnative'

# NB (this webOS build): luna-send must stay subscribed (-i) for the launch to
# take; SAM keeps stale "running" state after a hard kill, so close via SAM
# first or the next launch is a silent no-op relaunch.
#
# BOOT_SH is the close+launch dance itself, shared verbatim by `run` and `run-stream`
# so there is exactly ONE copy of the SAM incantation. It leaves the app running and
# the subscription pid in $$LP; how to observe the app is the caller's business.
#
# Only `rm -f` the log — never pre-create it. The app runs jailed under its own uid
# (not root), so a root-owned 644 file left in place is one the app cannot write: the
# log stays 0 bytes and every assertion reads as a total regression. `tail -F` retries
# until the app creates the file itself, which is exactly what -F is for.
BOOT_SH = (luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"com.beb.plxnative\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plxnative 2>/dev/null; rm -f /tmp/plxnative-events.log; \
	  luna-send -i "luna://com.webos.applicationManager/launch" "{\"id\":\"com.beb.plxnative\"}" >/dev/null 2>&1 & LP=$$!;

run:
	$(SSH) '$(BOOT_SH) \
	  sleep $(RUN_SECS); kill $$LP 2>/dev/null; sleep 1; \
	  cat /tmp/plxnative-events.log'

# Same launch, but stream the event log as it is written instead of sleeping a fixed
# RUN_SECS and catting at the end. tests/run.py grades the stream line-by-line and closes
# the connection the moment a case has passed, which is what stops a 20s case from costing
# 60s. Hanging up kills the tail; the trap takes the luna-send subscription with it so 18
# cases don't leave 18 of them behind. There is no time limit here on purpose — the caller
# owns the deadline (run.py caps each case at its manifest run_secs).
run-stream:
	$(SSH) '$(BOOT_SH) \
	  trap "kill $$LP 2>/dev/null" EXIT INT TERM HUP; \
	  tail -F -n +1 /tmp/plxnative-events.log'

kill:
	$(SSH) '(luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"com.beb.plxnative\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plxnative 2>/dev/null; echo closed'

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
check: lint
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" cargo +$(RUST_NIGHTLY) test --lib

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
IPK         := pkg/com.beb.plxnative_$(IPK_VERSION)_arm.ipk

# The .ipk is REPRODUCIBLE: same commit + same toolchain -> same sha256. That matters because the
# manifest carries that hash and every user's TV verifies it at install time (there is no code
# signing anywhere in the webosbrew chain — sha256 over HTTPS is the entire integrity story), so a
# non-reproducible archive makes "rebuilt" and "tampered with" indistinguishable.
#   - ci/mkipk.py normalises uid/gid/uname/gname/mtime/mode/order and the gzip header. `tar czf`
#     was embedding `gleblinnik/staff` in every shipped archive.
#   - `ar` gets D (deterministic): binutils' default embeds the builder's uid and a real mtime.
ipk: pkg/plxnative
	@echo "packaging $(if $(RELEASE),RELEASE,dev) build ($(RUST_CFG))"
	rm -rf ipkroot/data/usr && mkdir -p ipkroot/data/usr/palm/applications/com.beb.plxnative/licenses
	cp $(APP_FILES) ipkroot/data/usr/palm/applications/com.beb.plxnative/
	cp $(LICENSE_FILES) ipkroot/data/usr/palm/applications/com.beb.plxnative/licenses/
	@# Strip the STAGED copy only, never pkg/plxnative. ~2.4 MB of .symtab+.strtab (30% of the
	@# download, on a device whose app partition is 615 MB total and shared with every other app).
	@# It must not be pkg/plxnative because tools/crash-report.sh symbolizes a crash PC against
	@# that local binary AND md5-compares it to the on-TV copy to prove they are the same build —
	@# stripping in place would break the identity check and lose function names from every
	@# release crash report. Deploy ships the unstripped one by design; only the ipk is stripped.
	$(TOOLPREFIX)strip --strip-unneeded ipkroot/data/usr/palm/applications/com.beb.plxnative/plxnative
	rm -f pkg/com.beb.plxnative_*_arm.ipk
	python3 ci/mkipk.py
	$(SHA256SUM) $(IPK) | tee pkg/ipk.sha256

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

.PHONY: all setup-env deploy run run-stream kill check lint test ipk clean threadprobe sockprobe
