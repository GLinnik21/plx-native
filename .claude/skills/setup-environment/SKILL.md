---
name: setup-environment
description: >
  Set up (or repair) the cross-compile build environment for this native webOS TV
  app on a macOS host — the webOS NDK toolchain, the Rust nightly + rust-src needed
  for the static lib, and the sysroot the Makefile links against. Use this whenever
  the build environment isn't ready or is broken: a fresh clone, a new machine, a
  teammate onboarding, or errors like "arm-webos-linux-gnueabi-gcc: command not
  found", "cannot find -lSDL2 / -lplayerAPIs", "No such file or directory
  ...sysroot...", `cargo +nightly` / `-Z build-std` failures, or a binary that
  SIGILLs on the TV. Also use it to understand *why* the build is structured the
  way it is (why some libs are real and two are still stubs). Reach for this before
  hand-debugging toolchain/link errors — most of them are a missing or mis-located
  NDK.
---

# Set up the webOS build environment

This app is cross-compiled from macOS to a 32-bit ARM webOS TV. "Setting up the
environment" means getting three things in place so `make` works:

1. **The webOS NDK** — a buildroot cross-toolchain (GCC 12, glibc 2.12, armv7-a
   soft-float) plus a **sysroot** containing the TV's own SONAME'd libraries. The
   Makefile compiles and links against this.
2. **Rust nightly + `rust-src`** — the UI/logic is a Rust static lib built with
   `-Z build-std`, which recompiles `std` with our codegen flags (needed to avoid
   a SIGILL on the TV — see "Why build-std" below). That requires the nightly
   toolchain and the `rust-src` component.
3. **Host CLI tools** — `curl`, `tar`, and `sshpass` (deploy/run over ssh).

The end state you're verifying: `make` produces `pkg/plexpoc`, and it runs on the
TV with no missing-symbol or illegal-instruction errors.

## Fast path

From the repo root:

```bash
make setup-env        # download + extract + relocate the NDK into ~/webos-ndk
```

Then make sure the Rust side is ready (safe to re-run — no-ops if already done):

```bash
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
brew install sshpass      # deploy/run only; skip if you won't touch the TV
```

Now build:

```bash
make                  # -> pkg/plexpoc
```

That's the whole setup. The sections below explain what each piece is, how to
verify it, and how to fix it when it goes wrong — read them when the fast path
doesn't Just Work.

## What `make setup-env` does (and how to do it by hand)

The `setup-env` target downloads the webosbrew **native-toolchain** buildroot SDK
for your Mac's arch (`darwin-arm64` on Apple Silicon, `darwin-x86_64` on Intel),
extracts it to `$(WEBOS_SDK)`, and runs its `relocate-sdk.sh`. The relocate step
is **required**: the SDK bakes absolute paths at build time, and `relocate-sdk.sh`
rewrites them to wherever you extracted it. If you ever move the SDK directory,
re-run `relocate-sdk.sh` or nothing will compile.

Manual equivalent (useful if the download URL changed — check the
[latest release](https://github.com/webosbrew/native-toolchain/releases)):

```bash
mkdir -p ~/webos-ndk && cd ~/webos-ndk
curl -fL -o sdk.tar.bz2 \
  https://github.com/webosbrew/native-toolchain/releases/download/webos-d7ed7ee.6/arm-webos-linux-gnueabi_sdk-buildroot_darwin-arm64.tar.bz2
tar xjf sdk.tar.bz2
cd arm-webos-linux-gnueabi_sdk-buildroot && ./relocate-sdk.sh
```

Override the install location for the whole build with `make WEBOS_SDK=/path/... `.

## How the build is wired (the important mental model)

The Makefile links **against real sysroot libraries** for almost everything,
because their SONAMEs already match what the TV loads at runtime:

- `libSDL2 / libSDL2_ttf / libGLESv2 / libwayland-client / libglib-2.0 /
  libluna-service2` — standard libs, present in the sysroot.
- `libAcbAPI / libplayerAPIs / libpf-1.0` — **LG-proprietary**, but the webosbrew
  NDK bundles them, so we link the real thing and get link-time symbol checking on
  the Starfish/ACB C++ calls (no more hand-maintained mangled-symbol stubs).

Only **two families are still link-time stubs** (`stub/*.c` → `stub/*.so`), because
the sysroot can't satisfy them:

- **FFmpeg** (`libavformat/avcodec/avutil.so.57/.55`) — not in the sysroot at all.
- **libcurl** — the sysroot ships `libcurl.so.4`, but the **TV wants `.so.5`**.

A stub is a `.c` file of empty symbol bodies compiled `-shared -nostdlib -fPIC`
with `-Wl,-soname,<the TV's exact SONAME>`. It satisfies the linker on the host;
at runtime the TV's real library (matching that SONAME via `DT_NEEDED`) is loaded
instead. So **adding a call to a new FFmpeg/curl function means adding its name to
the matching `stub/*_stub.c`** or the link fails — only the name must match, an
empty `void foo(void){}` body is fine (it never runs on the host).

If you add a dependency on a *new* library that the TV has and the **sysroot also
has**, prefer linking it real (add `-l<name>` to `LIBS_REAL`) over writing a stub —
that's the whole point of the NDK. Only reach for a stub when the sysroot lacks it
or ships a different SONAME than the TV.

### Why `build-std` (don't remove it)

Rust's default `arm-unknown-linux-gnueabi` codegen is ARMv6 and emits the legacy
CP15 memory barrier (`mcr p15, ..., c7, c10, 5`), which is **UNDEFINED on the TV's
Cortex-A53 (ARMv8) → SIGILL** the moment std touches an atomic. `-C
target-cpu=cortex-a9` fixes our crates, but the *precompiled* `std` still carries
the bad barrier — so we rebuild std from source (`-Z build-std`, hence nightly +
`rust-src`) with the same flags. `-C target-feature=-neon` dodges crates whose NEON
paths use unstable intrinsics. These live in `RUSTFLAGS_TV` in the Makefile; leave
them.

## Verify the setup

```bash
# 1. Toolchain present and is the right target
"$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot/bin/arm-webos-linux-gnueabi-gcc" -dumpmachine
#   -> arm-webos-linux-gnueabi

# 2. Full build
make            # -> pkg/plexpoc, no errors

# 3. Inspect the binary (readelf is in the NDK bin/ dir)
RE="$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot/bin/arm-webos-linux-gnueabi-readelf"
"$RE" -A pkg/plexpoc | grep Tag_CPU_arch       # -> v7  (NOT v6 — v6 SIGILLs)
"$RE" --version-info pkg/plexpoc | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1   # -> GLIBC_2.12
"$RE" -d pkg/plexpoc | grep NEEDED             # SONAMEs should match the TV's libs
```

The decisive check is on the TV itself: `make test` (deploy + run) and read the
event log it prints. A healthy boot shows the GL context, `ff: avformat=...`, `acb
create=1`, and rising FPS with no `SIGILL` / `undefined symbol`. See the main
`CLAUDE.md` "Testing / verification" section for the full event-log surface.

## Troubleshooting

| Symptom | Cause → fix |
|---|---|
| `arm-webos-linux-gnueabi-gcc: command not found` / `No such file ...sysroot...` | NDK not installed or moved. Run `make setup-env`; if you moved it, re-run `relocate-sdk.sh`. |
| Paths inside the SDK point at `/Users/runner/...` | You skipped/failed `relocate-sdk.sh`. Re-run it from the SDK root. |
| `cannot find -lSDL2 / -lplayerAPIs / -lpf-1.0` | Wrong/incomplete sysroot (partial download, or `WEBOS_SDK` points somewhere stale). Re-extract; verify `find $SYSROOT -name 'libplayerAPIs.so*'`. |
| `error: "-Z build-std" ... rust-src` or `cargo +nightly` fails | Missing nightly or rust-src: `rustup toolchain install nightly && rustup component add rust-src --toolchain nightly`. |
| Binary is `Tag_CPU_arch: v6`, or SIGILLs on the TV at first atomic | `RUSTFLAGS_TV` got dropped, or std wasn't rebuilt. Ensure `-C target-cpu=cortex-a9` and `-Z build-std` are intact; `rm` the stale `libplexpoc_modules.a` and rebuild. |
| `relocation R_ARM_MOVW_ABS_NC ... recompile with -fPIC` when building a stub | A stub needs PIC. Stubs already use `-fPIC` in `STUBFLAGS`; if you added a bespoke stub rule, add `-fPIC`. |
| Deploy/run steps fail with `sshpass: command not found` | `brew install sshpass`. The TV must be on and reachable (`make TV=<ip> ...`). |

## Portability note

The whole reason for this setup (vs. an ad-hoc `zig cc` + hand-stubbed libs) is
that the NDK targets **glibc 2.12 / armv7-a soft-float**, the webOS baseline — so
one binary is far more portable across webOS versions and TV models than a build
pinned to one firmware's glibc and one SoC. Keep new code within that baseline:
don't reintroduce a newer-glibc or SoC-specific toolchain pin without a reason.
Runtime-level couplings to this specific TV (LG's SDL 2.0.4 fork key offsets, the
reverse-engineered Starfish/ACB object offsets) still exist in the Rust/C code and
are *not* solved by the NDK — they're a separate, larger effort.
