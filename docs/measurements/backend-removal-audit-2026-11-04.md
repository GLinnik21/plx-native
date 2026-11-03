# Backend removal audit — 2026-11-04

This is a read-only source audit of commit `308b0f92a` on the URI-first integration
branch. Neither backend was deleted, built, packaged or run for this audit. The
disposable-checkout tests below remain **unpassed**. In particular, an optional
normal dependency graph or a feature-off build is not physical deletion proof.

## Boundary to preserve

`plx_play_uri` and `plx_play_feed` have no source imports of `plx_media`, Plex, or
one another. Shared route selection, caption retention, reporting and recovery
remain in `plx_media`; neutral interfaces remain in `plx_playback_contract`.
The FFmpeg ABI/loader, capture encoder and independent subtitle and audio jobs
remain in `plx_media_codecs`. Deleting Feed must not delete that crate, its bundled
FFmpeg staging/ABI check, libass, or subtitle fonts. Deleting URI does not remove
the platform's shared display/video-plane code.

The application and media crates contain source references to the two drivers
behind their respective `play-uri` and `play-feed` feature gates. Keep the
feature **names** as empty declarations for the deleted driver in a disposable
checkout. This lets unchanged, dormant source pass Cargo's expected-`cfg`
checking while removing the path dependency and every feature edge that could
enable it. An empty feature is not a stand-in driver. The surviving driver must
remain a real dependency. Host tests can select neither; native builds and
packages must still refuse neither.

## Disposable checkout edits

Create a separate checkout at the commit being tested. For each row below, make
the edits in that checkout, delete only the named driver directory, regenerate
`rust-modules/Cargo.lock` with pinned Cargo, then make a local disposable commit
and confirm a clean worktree before running the gates. Restore from Git or use
a fresh checkout for the other row; do not push either deletion commit. Do not
change any remaining production Rust source to make a deletion pass.
The lockfile refresh command is
`(cd rust-modules && CARGO_INCREMENTAL=0 cargo +1.99.1 metadata --format-version 1 > /dev/null)`;
run it before that disposable commit.

| Registration | Delete `rust-modules/play-feed/` | Delete `rust-modules/play-uri/` |
| --- | --- | --- |
| `rust-modules/Cargo.toml` | Remove `"play-feed"` workspace member and default feature; retain `play-feed = []`. There is no direct app `plx_play_feed` dependency. | Remove `"play-uri"` workspace member, default feature and optional `plx_play_uri` path dependency; retain `play-uri = []`. |
| App feature composition | Remove `plx_media/play-feed` and `plx_platform/feed-sink` edges from the now-empty `play-feed` feature. | Remove `dep:plx_play_uri`, `plx_media/play-uri` and `plx_platform/uri-transport` edges from the now-empty `play-uri` feature. |
| `rust-modules/media/Cargo.toml` | Change `default` to `[]`; remove optional **normal** and unconditional **dev** `plx_play_feed` path dependencies. Retain `play-feed = []`, with no `dep:` or platform edge. Remove `plx_play_feed?/devtriggers`, `?/hostsim` and `?/test-support` forwarding. | Remove optional **normal** and unconditional **dev** `plx_play_uri` path dependencies. Retain `play-uri = []`, with no `dep:` edge. Remove `plx_play_uri?/devtriggers` forwarding. Feed remains media's default. |
| `ci/module-layers.ini` | Remove `[play_feed]` and `play_feed` from `[media]` and `[app]` `uses`. | Remove `[play_uri]` and `play_uri` from `[media]` and `[app]` `uses`. |
| `Makefile` | Select `uri` as default/only native backend configuration; remove `both` and `feed` feature mappings and the deleted source directory from `RUST_INPUTS`. Retain `none` for the host/no-package check. | Select `feed` as default/only native backend configuration; remove `both` and `uri` feature mappings and the deleted source directory from `RUST_INPUTS`. Retain `none` for the host/no-package check. |
| Package inventories | Remove `plx_play_feed` from explicit Makefile/CI/tool package lists. Remove Feed-only entries in `ci/list-caps.ini`. | Remove `plx_play_uri` from explicit Makefile/CI/tool package lists. Remove URI-only entries in `ci/list-caps.ini` and the stale URI probe entry in `ci/machines.ini`. |

Both media driver dependencies have unconditional `[dev-dependencies]` entries
at this commit. Merely disabling a feature leaves a local path dependency that
Cargo still has to resolve for test targets. Conversely, tests can unify a
driver's features across the workspace, so a green workspace test alone does
not prove the normal production dependency graph excludes the removed crate.
Check the normal graph separately.

The explicit package inventories are the Makefile's lint/unit package lists;
`.github/workflows/ci.yml`; `tools/build-bench.py` and its test;
`ci/test_no_host_staticlib.py`'s crate and workspace-file lists; and the
expected lists in `ci/test_arm_profile.py`, `ci/test_ci_split.py` and
`ci/test_test_fast.py`. Remove the deleted source path from `ci/check-deps.sh`'s
scan lists and `rust_test_modules.py` calls. The caps registry rejects entries
whose source site is gone; the machine-shape registry rejects a missing URI
probe file. These are build/test registrations associated with the removed
directory, not changes to the surviving production implementation.

For Feed, the stale caps sections are `play-feed/src/ff.rs::{demux,drain_wire,
dts_core_packet,hls_fetch_text,strip_hdr10plus_sei,read_leb128}`,
`play-feed/src/ff_pcm.rs::MAX_CONSECUTIVE_DECODE_ERRORS`,
`play-feed/src/feed_state.rs::{FEED_PACE_CAP_MS,MAX_FEED_AHEAD_NS,
PRIME_VIDEO_MAX_NS}` and `play-feed/src/ff_fixture_bytes.rs::rpu_fixture_nal`.
For URI they are `play-uri/src/uri_protocol.rs::{diagnostic_text,scrub_text}`.

The existing native selection already omits `src/starfish.c`,
`-lplayerAPIs`, `-lpf-1.0` and the Starfish interposer export when
`PLAYBACK_HAS_FEED` is empty. URI has no distinct C object or package file to
remove. `src/video_plane.c` and the display sink remain shared. Keep the
Makefile's `FFMPEG_STAGED`, `FFABI_STAMP`, `LIBASS_STAGED`, `APP_FILES`, deploy,
verification and IPK staging entries: FFmpeg is loaded dynamically through the
independent codec crate, so absence of an ELF `DT_NEEDED` entry would not prove
its package can be dropped.

## Commands for each edited disposable checkout

Run once with `SURVIVOR=uri` after deleting Feed, and once with
`SURVIVOR=feed` after deleting URI. The commands assume the checkout has the
registration edits above and the pinned Rust toolchain (`1.99.1`) installed.
They are a proposed verification sequence, **not results from this audit**.

```sh
SURVIVOR=uri  # use feed in the other disposable checkout
cd rust-modules
CARGO_INCREMENTAL=0 cargo +1.99.1 tree -p plxnative-modules -e normal --no-default-features --features "play-$SURVIVOR" --locked
CARGO_INCREMENTAL=0 cargo +1.99.1 check -p plxnative-modules --lib --no-default-features --features "play-$SURVIVOR" --locked
cd ..
make -s "PLAYBACK_BACKENDS=$SURVIVOR" print-playback-config
make "PLAYBACK_BACKENDS=$SURVIVOR" check
make "PLAYBACK_BACKENDS=$SURVIVOR" check-nodef
make PLAYBACK_BACKENDS=none check-nodef
make RELEASE=1 "PLAYBACK_BACKENDS=$SURVIVOR" pkg/plxnative
make RELEASE=1 "PLAYBACK_BACKENDS=$SURVIVOR" ipk
```

Inspect the `cargo tree -e normal` output for absence of the deleted package;
the surviving package must be present. `make check` then exercises host tests,
including their dev-dependency feature unification. For the native build,
inspect `print-playback-config` and the linked object/library list: URI-only
must omit the Feed C object, native Feed libraries and interposer. Inspect the
generated package's staged files and FFmpeg loader/table evidence separately;
the shared codec libraries should still be present. Also confirm
`make RELEASE=1 PLAYBACK_BACKENDS=none pkg/plxnative` refuses before compiling.
No command in this audit was executed against either deletion checkout.

## Audit result

The registrations above identify the intended removable boundary at
`308b0f92a`. No driver-to-common or driver-to-driver upward source import was
found. The actual deletion builds, package inspection and source-preservation
condition are still open; a compilation error after either directory is removed
would be a failed removal gate, not evidence to weaken the condition.
