# plexpoc — native webOS build (cross-compiled from macOS with zig cc)
#
# The TV (LG 49SM9000PLA, webOS 4.5, 32-bit ARM userspace) links against its
# own libraries at runtime; we link against hand-written stub .so files that
# carry the TV's real SONAMEs (see stub/*.c).
#
# make          — build pkg/plexpoc
# make deploy   — scp binary + appinfo to the TV (rooted, root@TV)
# make run      — launch on TV, keep alive $(RUN_SECS)s, fetch event log
# make test     — build + deploy + run
# make kill     — close the app on the TV
# make ipk      — repackage pkg/com.glin.plexpoc_0.1.0_arm.ipk

TV       ?= 192.168.0.114
SSH       = sshpass -p alpine ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@$(TV)
SCP       = sshpass -p alpine scp -O -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
APPDIR    = /media/developer/apps/usr/palm/applications/com.glin.plexpoc
RUN_SECS ?= 18

ZIG       = zig cc -target arm-linux-gnueabi.2.24 -mcpu=cortex_a53
CFLAGS    = -O2 -Iinclude -Isrc -D_GNU_SOURCE    # -Isrc: module cross-headers; -D_GNU_SOURCE: strcasestr
LIBS      = -lSDL2 -lSDL2_ttf -lGLESv2 -lluna-service2 -lglib-2.0 -lAcbAPI \
            -lwayland-client -lplayerAPIs
STUBFLAGS = -shared -nostdlib -fno-unwind-tables -fno-asynchronous-unwind-tables

STUBS = stub/libSDL2.so stub/libSDL2_ttf.so stub/libGLESv2.so \
        stub/libwayland-client.so stub/libluna-service2.so stub/libglib-2.0.so \
        stub/libAcbAPI.so stub/libplayerAPIs.so

# Hybrid C+Rust build (gradual migration). Modules ported to Rust are compiled
# into a staticlib and linked in; their src/*.c is excluded from the C build.
# Ported so far: img, stream, aq, mkv, pms — impls in rust-modules/.
# (src/gpdebug.c is a debug-only guard-page allocator — never in the normal build.)
RUST_TARGET = arm-unknown-linux-gnueabi.2.24
RUST_LIB    = rust-modules/target/arm-unknown-linux-gnueabi/release/libplexpoc_modules.a

SRCS = $(filter-out src/gpdebug.c src/img.c src/stream.c src/aq.c src/mkv.c src/pms.c,$(wildcard src/*.c))
OBJS = $(SRCS:.c=.o)

all: pkg/plexpoc

# per-file compile; each object depends on ALL headers so a header edit rebuilds all
src/%.o: src/%.c $(wildcard src/*.h)
	$(ZIG) $(CFLAGS) -c $< -o $@

# Rust staticlib (cargo-zigbuild → same armv7 soft-float glibc-2.24 target).
# CRITICAL codegen flags for this TV (32-bit ARMv8/A53):
#  - target-cpu=cortex-a53: default arm-*-gnueabi (ARMv6) codegen emits the legacy
#    CP15 memory barrier (mcr p15,...,c7,c10,5), UNDEFINED on the A53 (ARMv8) →
#    SIGILL. A53 emits the dedicated `dmb` (like the C build's -mcpu=cortex_a53).
#  - target-feature=-neon: NEON isn't needed (VFP still on for floats), and it
#    dodges crates (simd-adler32, ...) whose NEON path uses unstable intrinsics.
#  - -Z build-std: rebuilds std itself with these flags (precompiled std shipped
#    the CP15 barriers), so needs the nightly toolchain + rust-src.
RUSTFLAGS_TV = -C target-cpu=cortex-a53 -C target-feature=-neon
$(RUST_LIB): $(wildcard rust-modules/src/*.rs) rust-modules/Cargo.toml
	cd rust-modules && PATH="$$HOME/.cargo/bin:$$PATH" RUSTFLAGS="$(RUSTFLAGS_TV)" \
	  cargo +nightly zigbuild -Z build-std=std,panic_unwind --release --target $(RUST_TARGET)

# link C objects + the Rust staticlib (+ Rust std's deps: dl/pthread/m + the
# ARM-EHABI unwinder its precompiled std references; -lunwind is zig's, static)
pkg/plexpoc: $(OBJS) $(RUST_LIB) $(STUBS)
	$(ZIG) $(OBJS) $(RUST_LIB) -Lstub $(LIBS) -ldl -lpthread -lm -lunwind -o $@

# stub .so files embed the TV's real SONAMEs (must match DT_NEEDED exactly)
stub/libSDL2.so: stub/sdl_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libSDL2-2.0.so.0 -o $@ $<
stub/libSDL2_ttf.so: stub/sdl_ttf_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libSDL2_ttf-2.0.so.0 -o $@ $<
stub/libGLESv2.so: stub/gl_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libGLESv2.so.2 -o $@ $<
stub/libwayland-client.so: stub/wl_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libwayland-client.so.0 -o $@ $<
stub/libluna-service2.so: stub/ls2_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libluna-service2.so.3 -o $@ $<
stub/libglib-2.0.so: stub/glib_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libglib-2.0.so.0 -o $@ $<
stub/libAcbAPI.so: stub/acb_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libAcbAPI.so.1 -o $@ $<
stub/libplayerAPIs.so: stub/starfish_stub.c
	$(ZIG) $(STUBFLAGS) -Wl,-soname,libplayerAPIs.so.1 -o $@ $<

# tmp+mv so deploy works while the old binary is still executing (ETXTBSY)
deploy: pkg/plexpoc
	$(SCP) pkg/plexpoc root@$(TV):$(APPDIR)/plexpoc.new
	$(SCP) pkg/appinfo.json root@$(TV):$(APPDIR)/
	$(SSH) 'test -f $(APPDIR)/appfont.ttf' || $(SCP) pkg/appfont.ttf root@$(TV):$(APPDIR)/appfont.ttf
	$(SSH) 'test -f $(APPDIR)/appfont-bold.ttf' || $(SCP) pkg/appfont-bold.ttf root@$(TV):$(APPDIR)/appfont-bold.ttf
	$(SSH) 'mv $(APPDIR)/plexpoc.new $(APPDIR)/plexpoc && chmod +x $(APPDIR)/plexpoc'

# NB (this webOS build): luna-send must stay subscribed (-i) for the launch to
# take; SAM keeps stale "running" state after a hard kill, so close via SAM
# first or the next launch is a silent no-op relaunch.
run:
	$(SSH) '(luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"com.glin.plexpoc\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plexpoc 2>/dev/null; rm -f /tmp/poc-events.log; \
	  luna-send -i "luna://com.webos.applicationManager/launch" "{\"id\":\"com.glin.plexpoc\"}" >/dev/null 2>&1 & LP=$$!; \
	  sleep $(RUN_SECS); kill $$LP 2>/dev/null; sleep 1; \
	  cat /tmp/poc-events.log'

kill:
	$(SSH) '(luna-send -i "luna://com.webos.applicationManager/closeByAppId" "{\"id\":\"com.glin.plexpoc\"}" >/dev/null 2>&1 & P=$$!; sleep 2; kill $$P 2>/dev/null); \
	  fuser -k $(APPDIR)/plexpoc 2>/dev/null; echo closed'

clean:
	rm -f src/*.o pkg/plexpoc

test: deploy run

# ipk assembly: deb-style ar archive; zig ar emits GNU format (macOS ar is BSD)
ipk: pkg/plexpoc
	rm -rf ipkroot/data/usr && mkdir -p ipkroot/data/usr/palm/applications/com.glin.plexpoc
	cp pkg/plexpoc pkg/appinfo.json pkg/icon.png pkg/largeIcon.png \
	  ipkroot/data/usr/palm/applications/com.glin.plexpoc/
	cd ipkroot && tar czf control.tar.gz -C ctl control && \
	  tar czf data.tar.gz -C data usr && \
	  printf '2.0\n' > debian-binary
	rm -f pkg/com.glin.plexpoc_0.1.0_arm.ipk
	cd ipkroot && zig ar rc ../pkg/com.glin.plexpoc_0.1.0_arm.ipk \
	  debian-binary control.tar.gz data.tar.gz
	shasum -a 256 pkg/com.glin.plexpoc_0.1.0_arm.ipk | tee pkg/ipk.sha256

.PHONY: all deploy run kill test ipk clean
