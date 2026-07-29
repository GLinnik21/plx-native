# plxnative — native webOS build (cross-compiled from macOS with the webOS NDK)
#
# Toolchain: the webosbrew "native-toolchain" buildroot SDK (GCC 12, glibc 2.12,
# armv7-a soft-float). Install it once with `make setup-env` (or see the
# setup-environment skill); override the location with WEBOS_SDK=… if you put it
# elsewhere. The SDK ships a real sysroot with the TV's own SONAME'd libraries
# (SDL2/GLESv2/wayland/glib/luna-service2 + LG's libAcbAPI/libplayerAPIs/libpf),
# so we link against the real thing — no more hand-written stubs for those.
#
# Only two library families are NOT in the sysroot and still use the link-time
# stub trick (empty symbol bodies carrying the TV's exact SONAME, resolved to the
# TV's real libs at runtime): FFmpeg (libav*.so.57/.55) and libcurl.so.5 (the
# sysroot ships curl .so.4, but the TV wants .so.5). See stub/*.c.
#
# make          — build pkg/plxnative
# make setup-env— download+extract+relocate the NDK into $(WEBOS_SDK)
# make deploy   — scp binary + appinfo to the TV (rooted, root@TV)
# make run      — launch on TV, keep alive $(RUN_SECS)s, fetch event log
# make test     — build + deploy + run
# make kill     — close the app on the TV
# make ipk      — repackage pkg/com.beb.plxnative_0.1.0_arm.ipk

TV       ?= 192.168.0.114
SSH       = sshpass -p alpine ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@$(TV)
SCP       = sshpass -p alpine scp -O -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
APPDIR    = /media/developer/apps/usr/palm/applications/com.beb.plxnative
RUN_SECS ?= 18

# --- webOS NDK toolchain -----------------------------------------------------
WEBOS_SDK   ?= $(HOME)/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot
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
CFLAGS      += -g
# The RUSTFLAGS env var REPLACES rust-modules/.cargo/config.toml's list rather than appending,
# so this must carry the SIGILL-critical flags too, not just -C debuginfo=2.
RUST_ENV = RUSTFLAGS="-C target-cpu=cortex-a9 -C target-feature=-neon -C debuginfo=2"
endif
# -Iinclude keeps the TV's SDL2/GLES2 headers (its SDL is a 2.0.4 fork) ahead of
# the NDK's newer sysroot copies, so we compile against the ABI the TV runs.

# Real sysroot libraries (SONAMEs already match the TV's DT_NEEDED):
#   libpf-1.0 carries mediapipeline::CustomPipeline (the webOS<11 seek path).
LIBS_REAL = -lSDL2 -lSDL2_ttf -lGLESv2 -lluna-service2 -lglib-2.0 -lAcbAPI \
            -lwayland-client -lplayerAPIs -lpf-1.0
# Stub-only libraries (not in the sysroot): FFmpeg + curl.so.5.
LIBS_STUB = -lavformat -lavcodec -lavutil -lswscale -lcurl

STUBFLAGS = -fPIC -shared -nostdlib -fno-unwind-tables -fno-asynchronous-unwind-tables
STUBS = stub/libavformat.so stub/libavcodec.so stub/libavutil.so stub/libswscale.so stub/libcurl.so

# Rust-first build. The app is Rust (rust-modules/, compiled to a staticlib and
# linked in); C is only main.c (boot shim) + starfish.c (the StarfishMediaAPIs
# C++/ACB seam) + svg.c (nanosvg rasterizer).
# (src/gpdebug.c is a debug-only guard-page allocator — never in the normal build.)
RUST_TARGET = arm-unknown-linux-gnueabi
RUST_LIB    = rust-modules/target/$(RUST_TARGET)/release/libplxnative_modules.a

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
RUST_INPUTS := $(shell find rust-modules/src assets -type f 2>/dev/null)
$(RUST_LIB): $(RUST_INPUTS) rust-modules/Cargo.toml rust-modules/Cargo.lock rust-modules/.cargo/config.toml Makefile
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" $(RUST_ENV) \
	  cargo +nightly build --release --target $(RUST_TARGET)

# link C objects + the Rust staticlib. gcc pulls in libgcc_s (the ARM-EHABI
# unwinder Rust's panic_unwind std references) + libc/pthread/dl/m/rt itself.
# -Lstub first so the curl.so.5 stub wins over the sysroot's curl.so.4.
pkg/plxnative: $(OBJS) $(RUST_LIB) $(STUBS) Makefile
	$(CC) $(CFLAGS) $(OBJS) $(RUST_LIB) -Lstub $(LIBS_REAL) $(LIBS_STUB) -ldl -lpthread -lm -o $@

# stub .so files embed the TV's real SONAMEs (must match DT_NEEDED exactly).
# FFmpeg (demux + bitstream filters) + libcurl (HTTPS/DNS/TLS for plex.tv login).
stub/libavformat.so: stub/avformat_stub.c Makefile
	$(CC) $(STUBFLAGS) -Wl,-soname,libavformat.so.57 -o $@ $<
stub/libavcodec.so: stub/avcodec_stub.c Makefile
	$(CC) $(STUBFLAGS) -Wl,-soname,libavcodec.so.57 -o $@ $<
stub/libavutil.so: stub/avutil_stub.c Makefile
	$(CC) $(STUBFLAGS) -Wl,-soname,libavutil.so.55 -o $@ $<
stub/libswscale.so: stub/swscale_stub.c Makefile
	$(CC) $(STUBFLAGS) -Wl,-soname,libswscale.so.4 -o $@ $<
stub/libcurl.so: stub/curl_stub.c Makefile
	$(CC) $(STUBFLAGS) -Wl,-soname,libcurl.so.5 -o $@ $<

# --- NDK bootstrap -----------------------------------------------------------
# Download + extract + relocate the webosbrew native-toolchain into $(WEBOS_SDK).
NDK_REL ?= webos-d7ed7ee.6
NDK_HOST := $(shell uname -m)
NDK_TARBALL = arm-webos-linux-gnueabi_sdk-buildroot_darwin-$(NDK_HOST).tar.bz2
NDK_URL = https://github.com/webosbrew/native-toolchain/releases/download/$(NDK_REL)/$(NDK_TARBALL)
setup-env:
	@test -x $(CC) && { echo "NDK already present at $(WEBOS_SDK)"; exit 0; } || true
	mkdir -p $(dir $(WEBOS_SDK))
	curl -fL -o $(dir $(WEBOS_SDK))/ndk.tar.bz2 "$(NDK_URL)"
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
APP_FILES = pkg/plxnative pkg/appinfo.json pkg/icon.png pkg/largeIcon.png \
            pkg/appfont.ttf pkg/appfont-bold.ttf

deploy: pkg/plxnative
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

# `make check` — the HOST unit suite (~0.3s), the only correctness signal available
# without a television. Deliberately NOT a prerequisite of `all`: the normal build is a cross
# compile for the TV and must not be made to depend on a host toolchain run succeeding (a host
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
check:
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" cargo test --lib

# ipk assembly: deb-style ar archive; the NDK ar emits GNU format (macOS ar is BSD)
ipk: pkg/plxnative
	rm -rf ipkroot/data/usr && mkdir -p ipkroot/data/usr/palm/applications/com.beb.plxnative
	cp $(APP_FILES) ipkroot/data/usr/palm/applications/com.beb.plxnative/
	cd ipkroot && tar czf control.tar.gz -C ctl control && \
	  tar czf data.tar.gz -C data usr && \
	  printf '2.0\n' > debian-binary
	rm -f pkg/com.beb.plxnative_0.1.0_arm.ipk
	cd ipkroot && $(AR) rc ../pkg/com.beb.plxnative_0.1.0_arm.ipk \
	  debian-binary control.tar.gz data.tar.gz
	shasum -a 256 pkg/com.beb.plxnative_0.1.0_arm.ipk | tee pkg/ipk.sha256

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

.PHONY: all setup-env deploy run run-stream kill check test ipk clean threadprobe sockprobe
