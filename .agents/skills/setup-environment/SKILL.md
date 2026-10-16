---
name: setup-environment
description: >
  Set up (or repair) the cross-compile build environment for this native webOS TV
  app on a macOS host — the webOS NDK toolchain, the pinned stable Rust toolchain
  (with its armv7 rust-std) needed for the static lib, and the sysroot the Makefile links against. Use this whenever
  the build environment isn't ready or is broken: a fresh clone, a new machine, a
  teammate onboarding, or errors like "arm-webos-linux-gnueabi-gcc: command not
  found", "cannot find -lSDL2 / -lplayerAPIs", "No such file or directory
  ...sysroot...", `cargo +<toolchain>` "target may not be installed" failures, or a binary that
  SIGILLs on the TV, or "cmake is required" while building Sentry Native. Also use it to understand *why* the build is structured the
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
2. **The pinned stable Rust toolchain** — the UI/logic is a Rust static lib built for
   `armv7-unknown-linux-gnueabi` with that release's PREBUILT std (the ARMv7 std has no
   SIGILL barrier — see "Why armv7" below). The release is the `channel` in
   `rust-modules/rust-toolchain.toml`; it needs the armv7 rust-std, clippy and rustfmt, and no
   nightly, no `rust-src`, no `-Z build-std`. Dependabot bumps that one line weekly (the
   `rust-toolchain` entry in `.github/dependabot.yml`); nothing else in the repo names the version.
3. **Host CLI tools** — `curl`, `tar`, CMake (the pinned Sentry Native build), and `sshpass`
   (deploy/run over ssh, only when the TV refuses your key).

The end state you're verifying: `make` produces `pkg/plxnative`, and it runs on the
TV with no missing-symbol or illegal-instruction errors.

## Fast path

From the repo root:

```bash
make setup-env        # download + extract + relocate the NDK into ~/webos-ndk
```

`make setup-env` also runs `make setup-rust`, which installs the pinned toolchain with clippy,
rustfmt and the armv7 rust-std (safe to re-run). Then the host tools:

```bash
make setup-rust           # only the Rust half; `make -s print-rust-toolchain` prints the pinned release
brew install cmake        # builds the pinned, statically linked Sentry Native capture backend
brew install sshpass      # deploy/run fallback when the TV refuses your ssh key; skip if you won't touch the TV
```

Now build:

```bash
make                  # -> pkg/plxnative
```

### With a TV but no Plex account or token

The build needs no private data, and neither does driving the TV: the app can run against the
synthetic `tests/mock_pms.py`, which accepts any non-empty token.

```bash
mkdir -p ~/.config/plxnative     # ONCE per machine: every checkout and worktree reads these (tools/tv-config.sh)
echo '<tv-host>' > ~/.config/plxnative/tv-host   # the TV's address, one line
echo '<tv-mac>'  > ~/.config/plxnative/tv-mac    # optional: only Wake-on-LAN needs it (cached from ARP once the TV is up)
printf '#define PMS_HOST  "<this-mac-lan-ip>"\n#define PMS_PORT  32499\n' > src/config.local.h   # gitignored; no PMS_TOKEN
make                             # PMS_HOST/PMS_PORT are compiled in; nothing else is read from config.local.h
python3 tests/mock_pms.py --host <this-mac-lan-ip> --port 32499   # same address as PMS_HOST; the default binds loopback only
tools/tv-session.sh up --guest --mock --screen home       # debug flavor only; deploys and md5-verifies the binary, boots the synthetic identity
```

The TV address and MAC are per-USER, not per-checkout: `${PLX_TV_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/plxnative}`,
so a fresh worktree needs nothing seeded for them and no setup hook (a copied file would go stale). A
`.tv-host` / `.tv-mac` in a checkout (or in the main checkout) still works and takes precedence, as a
per-checkout override. `src/config.local.h` is the one gitignored file that is still per-checkout, so a
fresh worktree has none. `up` deploys
the built binary itself; `make FLAVOR=debug install` is needed only on a TV that has never had
the debug build. `--mock` makes `--guest` use `tools/mock-guest.py`, which refuses any server that
is not the synthetic mock, instead of resolving a managed user through plex.tv. Take the TV lock
first (`tv-lock` skill).

The default mock answers 3 Home hubs; `mock_pms.py --home-hubs N` serves N, counting Continue
Watching (so N-1 shelves plus Continue Watching), for work on Home with many rows. To measure
Home, add `--arm framedrop=25` to `up`: the event log then gets `FRAMEDROP` detail lines for
frames over 25 ms.

Without a TV, `make check` and `make sim` need no account either. `./tests/run.py --fps --mock`
runs the fps scenes that declare a `mock` block (`grep '"mock"' tests/manifest.json` lists them) against the mock
with no `PMS_TOKEN`, no `tests/manifest.local.json` and no plex.tv call: it starts and stops
`tests/mock_pms.py` itself on `PMS_HOST`/`PMS_PORT` with each scene's arguments (stop a mock you
started by hand first), boots the same synthetic guest as `up --guest --mock`, and keeps the panel
and sound off. `./tests/run.py --server` (and plain `--fps`) still call `read_token()` and read
`tests/manifest.local.json`, so they cannot target the mock. `tests/run.py` does not deploy the binary, so deploy the build you mean to measure first (`make deploy` under the TV lock, or `tools/tv-session.sh up`, which deploys when stale; for FPS or frame pacing build with `ARM_PROFILE=release`, e.g. `make ARM_PROFILE=release deploy`, because a bare `make deploy` ships the no-LTO `tvdev` build, which `--fps*` and `tools/profile-graphics` refuse to grade; see `docs/agent-reference.md` "ARM_PROFILE") — a stress run once measured an older binary this way.

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

Manual equivalent. The release, URLs and sha256 of each host build are pinned in
`ci/dependencies.json` (entry `webos-ndk`); `ci/deps.py` downloads and verifies:

```bash
mkdir -p ~/webos-ndk && cd ~/webos-ndk
tar xjf "$(python3 <repo>/ci/deps.py fetch webos-ndk --asset darwin-arm64 --into .)"
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

Sentry Native is a different case: `ci/build-sentry-native.sh` downloads a checksum-pinned source
archive, applies the tracked webOS/glibc-2.12/ARM32 patch, and cross-builds `libsentry.a`,
`libunwind.a` and the separate `sentry-crash` daemon with CMake. The libraries are statically
linked and the daemon is packaged beside the app; this adds no Sentry shared-library SONAME to the
television. Its HTTP transport is compiled out.

If you add a dependency on a *new* library that the TV has and the **sysroot also
has**, prefer linking it real (add `-l<name>` to `LIBS_REAL`) over writing a stub —
that's the whole point of the NDK. Only reach for a stub when the sysroot lacks it
or ships a different SONAME than the TV.

### Why `armv7-unknown-linux-gnueabi` (don't go back to `arm-unknown-linux-gnueabi`)

Rust's `arm-unknown-linux-gnueabi` target is ARMv6, and its PREBUILT `std` carries the legacy
CP15 memory barrier (`mcr p15, ..., c7, c10, 5`), which is **UNDEFINED on the TV's
Cortex-A53 (ARMv8) → SIGILL** the moment std touches an atomic. `-C target-cpu=cortex-a9`
fixes only OUR crates, so the build used to rebuild std from source (`-Z build-std`, hence nightly
+ `rust-src`). `armv7-unknown-linux-gnueabi` ships a prebuilt ARMv7 std that emits `dmb`, so stable
rustc is enough. That target defaults to `+soft-float` (every float op a libcall), so the codegen
flags carry `-C target-feature=-neon,-soft-float` to give our crates VFP back: `-soft-float` is an
UNSTABLE target feature on stable rustc, and `ci/check-vfp.py` (in `make check` and CI) fails if a
rustc release stops honouring it. `-neon` dodges crates whose NEON paths use unstable intrinsics.
The flags live in `RUST_CODEGEN` in the Makefile and, as the fallback for a bare cargo, in
`rust-modules/.cargo/config.toml`; keep the two in step and leave them.

## Verify the setup

```bash
# 1. Toolchain present and is the right target
"$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot/bin/arm-webos-linux-gnueabi-gcc" -dumpmachine
#   -> arm-webos-linux-gnueabi

# 2. Full build
make            # -> pkg/plxnative, no errors

# 3. Inspect the binary (readelf is in the NDK bin/ dir)
RE="$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot/bin/arm-webos-linux-gnueabi-readelf"
"$RE" -A pkg/plxnative | grep Tag_CPU_arch       # -> v7  (NOT v6 — v6 SIGILLs)
"$RE" --version-info pkg/plxnative | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1   # -> GLIBC_2.12
"$RE" -d pkg/plxnative | grep NEEDED             # SONAMEs should match the TV's libs
```

The decisive check is on the TV itself: `make test` (deploy + run) and read the
event log it prints. A healthy boot shows the GL context, `ff: avformat=...`, `acb
create=1`, and rising FPS with no `SIGILL` / `undefined symbol`. See the main
`docs/agent-reference.md` "Testing / verification" section for the full event-log surface.

**On a television that has never had this checkout's developer build, that first `make
test` stops before it deploys**, saying the app directory does not exist. That is
correct and it is not a toolchain fault: `make deploy` scp's into an app directory that
is already registered, while creating an app is appinstalld's job — SAM has to learn the
id and the per-id LS2 role file has to be written. Three builds live on one TV now
(`com.beb.plxnative`, what users install, `com.beb.plxnative.debug`, the Makefile's
**default**, and `com.beb.plxnative.nightly`, the nightly build), so install this one once
and everything after it is a plain `make test`:

```bash
make FLAVOR=debug install    # builds its .ipk, installs it, then deploys into it
```

The event log then lands under that install's own runtime root, not `/tmp` — ask for
the path with `make -s print-eventlog FLAVOR=debug` rather than typing one. The
`tv-session` skill owns the rest of the on-device loop.

## Troubleshooting

| Symptom | Cause → fix |
|---|---|
| `arm-webos-linux-gnueabi-gcc: command not found` / `No such file ...sysroot...` | NDK not installed or moved. Run `make setup-env`; if you moved it, re-run `relocate-sdk.sh`. |
| Paths inside the SDK point at `/Users/runner/...` | You skipped/failed `relocate-sdk.sh`. Re-run it from the SDK root. |
| `cannot find -lSDL2 / -lplayerAPIs / -lpf-1.0` | Wrong/incomplete sysroot (partial download, or `WEBOS_SDK` points somewhere stale). Re-extract; verify `find $SYSROOT -name 'libplayerAPIs.so*'`. |
| E0463 "can't find crate for core", or "toolchain is not installed" | The pinned toolchain or its armv7 target is missing: `make setup-rust` (`rustup toolchain install <pin> --profile minimal --target armv7-unknown-linux-gnueabi`). No nightly or `rust-src` is involved. |
| `make check` dies at `make lint` with `no such command: clippy` | The toolchain was installed without clippy: `make setup-rust` (or `rustup component add clippy --toolchain $(make -s print-rust-toolchain)`). `make check` runs `lint` first; the cross-build itself never needs clippy. |
| `build-sentry-native: cmake is required` | Install the host build generator with `brew install cmake`. It runs the webOS cross-compiler; it does not compile target code with the Mac toolchain. |
| Binary is `Tag_CPU_arch: v6`, or SIGILLs on the TV at first atomic | `RUST_CODEGEN` got dropped, or the build targeted `arm-unknown-linux-gnueabi` (ARMv6 std). Ensure `--target armv7-unknown-linux-gnueabi` and `-C target-cpu=cortex-a9` are intact (`make -s print-rust-codegen`); `rm` the stale `libplxnative_modules.a` and rebuild. |
| `relocation R_ARM_MOVW_ABS_NC ... recompile with -fPIC` when building a stub | A stub needs PIC. Stubs already use `-fPIC` in `STUBFLAGS`; if you added a bespoke stub rule, add `-fPIC`. |
| Deploy/run steps fail with `the TV refused this machine's ssh key and sshpass is not installed` | Authorize your key on the TV, or `brew install sshpass`. `the TV is unreachable` means it is off or asleep (wake-tv skill; `make TV=<ip> ...`). |

## Portability note

The whole reason for this setup (vs. an ad-hoc `zig cc` + hand-stubbed libs) is
that the NDK targets **glibc 2.12 / armv7-a soft-float**, the webOS baseline — so
one binary is far more portable across webOS versions and TV models than a build
pinned to one firmware's glibc and one SoC. Keep new code within that baseline:
don't reintroduce a newer-glibc or SoC-specific toolchain pin without a reason.
Runtime-level couplings to this specific TV (LG's SDL 2.0.4 fork key offsets, the
reverse-engineered Starfish/ACB object offsets) still exist in the Rust/C code and
are *not* solved by the NDK — they're a separate, larger effort.
