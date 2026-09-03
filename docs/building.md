# Building PlxNative

The build, the two installs, the device loop and the test tiers — everything a contributor needs
that a television owner does not. The
[README](../README.md) is the owner-facing page.

`AGENTS.md` and [`agent-reference.md`](agent-reference.md) are the deep reference behind this file —
architecture, portability, and the non-obvious things that took a while to work out. Each major
subsystem has a guide of its own next to the code.

## Requirements

Cross-compiled to 32-bit ARM. Host must be macOS (x86_64 or arm64) or **arm64** Linux — there is no
x86_64 build of the webOS NDK, so an x86_64 Linux host cannot build this.

- The **webOS NDK**, fetched by `make setup-env` (a few hundred MB, once).
- A **Rust nightly** toolchain with `rust-src` (for `-Z build-std`) and `clippy` (the lint gate).
- **CMake** — the normal build cross-compiles Sentry Native, and `ci/build-sentry-native.sh` stops
  with an explicit error without it.
- `sshpass`, for the deploy/run targets that talk to a television.

```sh
make setup-env
rustup toolchain install nightly --component rust-src --component clippy
brew install cmake sshpass        # or your distribution's equivalent
make                              # builds pkg/plxnative — a developer build
make ipk                          # pkg/com.beb.plxnative.debug_<version>_arm.ipk
```

## Disk, and the build cache

Build trees are per-checkout and large. `make disk` reports what this machine is holding, and
`tools/build-gc.sh --incremental|--lanes|--all` reclaims it — it deletes nothing `make` cannot
rebuild. Run `tools/build-gc.sh --orphans` after tearing down a set of worktrees: lane target
directories live outside the repo and outlive the worktree that made them.

The bundled FFmpeg is not rebuilt per checkout. Its source and object tree is machine-wide under
`$PLX_BUILD_CACHE` (default `~/.cache/plxnative`), keyed by configure flags and toolchain, so a
fresh clone gets its own prefix in seconds rather than minutes.

## Two flavours, and the default is the developer one

That is why the filename above says `.debug`. The app can be installed twice on one television:
`com.beb.plxnative`, the id in every release and what users install, and `com.beb.plxnative.debug`
beside it, with its own launcher tile, sign-in and log. `FLAVOR` chooses which one every TV-facing
target talks to, and the checked-in default is `debug`.

The asymmetry is deliberate. `stable` is the install somebody may be watching a film on, and no
command typed from muscle memory should be able to overwrite it. So the shippable artifact has to be
asked for by name:

```sh
make FLAVOR=stable RELEASE=1 ipk   # pkg/com.beb.plxnative_<version>_arm.ipk — what a release publishes
```

`make FLAVOR=stable ipk` on its own is refused: the stable id is release-only. Add
`FLAVOR=stable RELEASE=1` to **every** command that builds or packages something you intend to ship
— `RELEASE=1` drops the developer feature set (the on-screen frame counter and the whole `/tmp`
trigger surface the harness drives the app through) and is not sticky between invocations.
[`two-installs.md`](two-installs.md) is the whole story: what the two share, what they don't, and
the name traps.

## The desktop simulator

`make sim` builds the same application core against desktop SDL2 and GL; `make sim-run` opens the
window, and `make sim-shot` boots it headlessly and writes a PNG. It renders the real interface
against a real Plex Media Server, and since 2026-08-28 it also streams and demuxes, so the whole
pipeline between the socket and the decoder runs on the host.

Several simulators run side by side, which the television cannot — prefer it for ordinary UI and
data-layer work. It **cannot** answer frame rate (different GPU), text rasterization, or anything
about LG's decoder and video plane. Those need the set.

## Developing against a real TV

This loop assumes a **rooted** television reachable over ssh, because it deploys by copying the
binary straight into the installed app directory. That directory has to exist first, so install
once — it builds, installs and deploys in one go:

```sh
make FLAVOR=debug TV=<ip> install   # ONCE per TV
```

After that (every target here defaults to the `debug` flavour):

```sh
make TV=<ip> deploy   # scp the binary + assets
make TV=<ip> run      # launch, hold it up, then print the on-device event log
make TV=<ip> test     # deploy + run
```

Skip the install and `deploy` stops with *"the debug flavour is not installed"* rather than
half-working. Drop the address into a gitignored `.tv-host` (one line, an IP or hostname) and you
can leave `TV=<ip>` off every command. Ask `make -s print-appid print-appdir print-rundir
FLAVOR=<f>` when you need to know where a given flavour's binary, logs and dev triggers live.

## Tests

**`make check`** — the host gate: the lint pass plus the Rust unit suite run twice (default features
and `hostsim`), alongside the package, C, harness and tooling self-tests. Seconds once warm, no
television. Run it before you push, and run it **on nightly** — `make check` uses `cargo +nightly`
while a bare `cargo test` picks up your default toolchain, and the two have disagreed.

**`./tests/run.py`** — the synthetic device tier. Generated clips served off the host and played
through a dev trigger; it needs a television address and `make fixtures-pipeline`, and nothing else.
No Plex Media Server, no token, no manifest. This is the tier that separates "the player is broken" from "the library layer
is broken", and it is the only one a stranger can run.

```sh
make fixtures-pipeline   # generate the clips once
./tests/run.py           # the synthetic player suite
```

**`./tests/run.py --server`** — the library-backed suite: `/decision`, direct play vs transcode,
track menus, markers, resume, and progress reporting. It needs two gitignored files —
`tests/manifest.local.json`, mapping named media shapes to items in *your* library (copy the
`.example` beside it and drop the ones you don't have), and `src/config.local.h` with your Plex
token, which the harness reads on the host and injects. The token is never compiled into the binary.
A media shape your library lacks is skipped, not failed, so read the skip count beside the pass
count.

**`./tests/run.py --fps`** — the frame-rate regression scenes, which imply `--server`. Per-screen
floors live in `tests/manifest.json`. This tier is opt-in and runs on a real television; CI cannot
grade frame rate, so a green CI run says nothing about pacing.

**Two checks CI runs that `make check` does not**, and which are easy to break locally: the shipping
feature set (`cargo +nightly check --manifest-path rust-modules/Cargo.toml --lib
--no-default-features`) after any `rust-modules/src/**.rs` edit, and a firmware-compatibility review
of anything touching FFI, linkage, `dynlib!`, Starfish, ACB, curl or the bundled FFmpeg —
`tools/fwcompat.py` grades the binary against 14 real firmware images in under a second.

## What proves what

The device is the real test. The host suite and the simulator between them cover a great deal, but
nothing off the television decodes a frame or talks to LG's media stack, so a green host run proves
less than it looks like it does. Anything touching playback, the video plane, text rasterization or
frame rate has to be checked on a real set — say in the pull request what you verified and how.

There is exactly one development television and no operating-system mutex on it. Two jobs driving it
at once do not fail cleanly; they produce plausible wrong data. `tools/tv-lock.sh` holds a lease, and
the make targets and harness take it for you.

## What I most need help with

**A rooted webOS 5+ set.** I can't develop for one blind — no emulator substitutes for the hardware
([why](distribution.md#34a-no-emulator-substitutes-for-the-hardware-researched-2026-07-28)) — and
what's missing is someone who can run and debug on the set, not a report of what's installed on it.
A different 4.x panel, a remote or relayed server, or media this has never met are all useful too.
