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
# -Iinclude keeps the TV's SDL2/GLES2 headers (its SDL is a 2.0.4 fork) ahead of
# the NDK's newer sysroot copies, so we compile against the ABI the TV runs.

# Real sysroot libraries (SONAMEs already match the TV's DT_NEEDED):
#   libpf-1.0 carries mediapipeline::CustomPipeline (the webOS<11 seek path).
LIBS_REAL = -lSDL2 -lSDL2_ttf -lGLESv2 -lluna-service2 -lglib-2.0 -lAcbAPI \
            -lwayland-client -lplayerAPIs -lpf-1.0
# Stub-only libraries (not in the sysroot): FFmpeg + curl.so.5.
LIBS_STUB = -lavformat -lavcodec -lavutil -lcurl

STUBFLAGS = -fPIC -shared -nostdlib -fno-unwind-tables -fno-asynchronous-unwind-tables
STUBS = stub/libavformat.so stub/libavcodec.so stub/libavutil.so stub/libcurl.so

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
src/%.o: src/%.c $(wildcard src/*.h)
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
RUSTFLAGS_TV = -C target-cpu=cortex-a9 -C target-feature=-neon
$(RUST_LIB): $(wildcard rust-modules/src/*.rs rust-modules/src/ui/*.rs rust-modules/src/player/*.rs rust-modules/src/plex/*.rs) rust-modules/Cargo.toml
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" RUSTFLAGS="$(RUSTFLAGS_TV)" \
	  cargo +nightly build -Z build-std=std,panic_unwind --release --target $(RUST_TARGET)

# link C objects + the Rust staticlib. gcc pulls in libgcc_s (the ARM-EHABI
# unwinder Rust's panic_unwind std references) + libc/pthread/dl/m/rt itself.
# -Lstub first so the curl.so.5 stub wins over the sysroot's curl.so.4.
pkg/plxnative: $(OBJS) $(RUST_LIB) $(STUBS)
	$(CC) $(CFLAGS) $(OBJS) $(RUST_LIB) -Lstub $(LIBS_REAL) $(LIBS_STUB) -ldl -lpthread -lm -o $@

# stub .so files embed the TV's real SONAMEs (must match DT_NEEDED exactly).
# FFmpeg (demux + bitstream filters) + libcurl (HTTPS/DNS/TLS for plex.tv login).
stub/libavformat.so: stub/avformat_stub.c
	$(CC) $(STUBFLAGS) -Wl,-soname,libavformat.so.57 -o $@ $<
stub/libavcodec.so: stub/avcodec_stub.c
	$(CC) $(STUBFLAGS) -Wl,-soname,libavcodec.so.57 -o $@ $<
stub/libavutil.so: stub/avutil_stub.c
	$(CC) $(STUBFLAGS) -Wl,-soname,libavutil.so.55 -o $@ $<
stub/libcurl.so: stub/curl_stub.c
	$(CC) $(STUBFLAGS) -Wl,-soname,libcurl.so.5 -o $@ $<

# --- NDK bootstrap -----------------------------------------------------------
# Download + extract + relocate the webosbrew native-toolchain into $(WEBOS_SDK).
NDK_REL ?= webos-d7ed7ee.6
NDK_HOST := $(shell uname -m | sed 's/arm64/arm64/;s/x86_64/x86_64/')
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
deploy: pkg/plxnative
	$(SCP) pkg/plxnative root@$(TV):$(APPDIR)/plxnative.new
	$(SCP) pkg/appinfo.json root@$(TV):$(APPDIR)/
	$(SSH) 'test -f $(APPDIR)/appfont.ttf' || $(SCP) pkg/appfont.ttf root@$(TV):$(APPDIR)/appfont.ttf
	$(SSH) 'test -f $(APPDIR)/appfont-bold.ttf' || $(SCP) pkg/appfont-bold.ttf root@$(TV):$(APPDIR)/appfont-bold.ttf
	$(SSH) 'mv $(APPDIR)/plxnative.new $(APPDIR)/plxnative && chmod +x $(APPDIR)/plxnative'

# NB (this webOS build): luna-send must stay subscribed (-i) for the launch to
# take; SAM keeps stale "running" state after a hard kill, so close via SAM
# first or the next launch is a silent no-op relaunch.
run:
	$(SSH) '(luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"com.beb.plxnative\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plxnative 2>/dev/null; rm -f /tmp/plxnative-events.log; \
	  luna-send -i "luna://com.webos.applicationManager/launch" "{\"id\":\"com.beb.plxnative\"}" >/dev/null 2>&1 & LP=$$!; \
	  sleep $(RUN_SECS); kill $$LP 2>/dev/null; sleep 1; \
	  cat /tmp/plxnative-events.log'

kill:
	$(SSH) '(luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"com.beb.plxnative\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plxnative 2>/dev/null; echo closed'

clean:
	rm -f src/*.o pkg/plxnative

test: deploy run

# ipk assembly: deb-style ar archive; the NDK ar emits GNU format (macOS ar is BSD)
ipk: pkg/plxnative
	rm -rf ipkroot/data/usr && mkdir -p ipkroot/data/usr/palm/applications/com.beb.plxnative
	cp pkg/plxnative pkg/appinfo.json pkg/icon.png pkg/largeIcon.png \
	  ipkroot/data/usr/palm/applications/com.beb.plxnative/
	cd ipkroot && tar czf control.tar.gz -C ctl control && \
	  tar czf data.tar.gz -C data usr && \
	  printf '2.0\n' > debian-binary
	rm -f pkg/com.beb.plxnative_0.1.0_arm.ipk
	cd ipkroot && $(AR) rc ../pkg/com.beb.plxnative_0.1.0_arm.ipk \
	  debian-binary control.tar.gz data.tar.gz
	shasum -a 256 pkg/com.beb.plxnative_0.1.0_arm.ipk | tee pkg/ipk.sha256

.PHONY: all setup-env deploy run kill test ipk clean
