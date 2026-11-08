# Backend removal drill, 2026-11-06

This is the record of a delete-and-restore drill on the URI-first integration branch. It replaces
the read-only source audit of `308b0f92a` (2026-11-04), whose deletion tests were still unpassed
when it was written. The drill ran them.

The commits this record names (`869ff76e2`, `308b0f92a`, `e391d89bf`) are on the `integ/uri-first` branch, which `main` took as a
series of squash merges, so they are not ancestors of `main`; the boundary counts below are as measured at those commits (at the
landing tree, for example, `feature = "play-uri"` appears on 30 lines under `rust-modules/src`, one more than at `e391d89bf`).

Base: `869ff76e2` ("W3f: items only play-feed names leave the playback contract"). Two detached,
disposable worktrees were cut from it, one local commit each, never pushed and never merged:

| Name | Deleted | Survivor | Local commit |
| --- | --- | --- | --- |
| `nofeed` | `rust-modules/play-feed/` | URI (`plx_play_uri`) | `128bc3282` |
| `nouri` | `rust-modules/play-uri/` | Feed (`plx_play_feed`) | `d17fe5b9d` (amended once, see check-deps below) |

The two commits are unreferenced objects that `git gc` will reclaim; the edit lists below are
their record. Toolchain: the pinned one (`make -s print-rust-toolchain`), every `make` line
prefixed `PATH="$HOME/.cargo/bin:$PATH"`. No private file was copied into either worktree and no
build asked for `src/config.local.h`. Shipped builds used `FLAVOR=debug`, the tracked default.
Both worktrees were clean on their commits, were removed with a plain `git worktree remove`, and
the integration checkout was left clean at the base.

**Result: both survivors pass every check in the sequence below.** What the drill did not
cover, and what it found beside the checks, is under "What failed or was left over".

## The contract, and the one allowance

The contract for a deletion: remove one crate directory and edit only registrations (manifests,
the lockfile, the layer table, the Makefile, CI and tool lists, the ledgers and registries that
name the crate). No other production Rust source changes, and the shared crates (`media`,
`playback-contract`, `media-codecs`, `ui`, `screens`, `appkit`, `platform`) and the survivor stay
byte-identical.

The allowance (R-W5): deleting `play-feed` may also remove the `plx_play_feed`
`[dev-dependencies]` lines of `media` and `screens`, with their feature fan-outs, and nothing
else under those crates. A dev-dependency on a deleted path crate stops Cargo resolving the
workspace at all. The Feed integration tests that dependency served were therefore not run in the
`nofeed` checkout. Deleting `play-uri` needed no such allowance: no crate but the application
names it.

## Boundary to preserve

Re-verified with `git grep` at `e391d89bf`, which descends from the drill base.

- `plx_play_uri` and `plx_play_feed` have no source imports of `plx_media`, the Plex client or
  one another: no `plx_media::`, `plx_plex::`, `plx_play_uri::` or `plx_play_feed::` path, and
  no `use` of those crates, in `rust-modules/play-uri/` or `rust-modules/play-feed/`. The layer
  table agrees: `play_uri` uses `base platform playback_contract`; `play_feed` uses
  `base platform net playback_contract media_codecs`.
- Route selection (`media/src/route/`), the failed-open ladder (`media/src/player/recovery.rs`)
  and attempt and backend reporting (`media/src/player/attempt_metrics.rs`,
  `backend_metrics.rs`) are in `plx_media`; the neutral interfaces are in
  `plx_playback_contract`. The FFmpeg ABI and loader (`media-codecs/src/ffi.rs`), the capture
  encoder (`venc.rs`), and the subtitle and audio jobs (`subtitle.rs`, `subtitle_extract.rs`,
  `pcm.rs`, `speed_*.rs`) are in `plx_media_codecs`. Deleting Feed deletes none of them, nor the
  bundled FFmpeg staging and ABI check, libass or the subtitle fonts.
- `plx_platform` keeps the shared display and video-plane code (`platform/src/tv/plane.rs`,
  `sink.rs`) and `src/video_plane.c`. Its `feed-sink` and `uri-transport` features stay declared
  after a deletion, but no application feature enables the deleted backend's any more (the shared
  port's accepted exception).
- Only the application crate still names the backend features in `cfg` predicates:
  `feature = "play-feed"` on 124 lines in 18 files of `rust-modules/src`, `feature = "play-uri"`
  on 29 lines in 6. No other crate does. (The 2026-11-04 audit said the media crate did too; W3
  moved those out.)
- Apart from the two driver manifests, three manifests name a driver: the application manifest
  (the workspace member list, the `play-uri` and `play-feed` features, the two optional
  dependencies and the two `test-support` dev-dependencies), `media/Cargo.toml` (`plx_play_feed`
  only: the `devtriggers` and `hostsim` fan-outs and one dev-dependency) and `screens/Cargo.toml`
  (a `plx_play_feed` dev-dependency). `plx_play_uri` is named by no crate but the application.

## Results

| Check | `nofeed` (URI survives) | `nouri` (Feed survives) |
| --- | --- | --- |
| zero-diff: `git diff --stat 869ff76e2 -- rust-modules/{media,playback-contract,media-codecs,ui,screens,appkit,platform,<survivor>}` | only the R-W5 lines: `rust-modules/media/Cargo.toml \| 5 ++---`, `rust-modules/screens/Cargo.toml \| 1 -`, `2 files changed, 2 insertions(+), 4 deletions(-)` | empty |
| `cargo tree -e normal` lacks the deleted crate | 0 `plx_play_feed` lines (default features, `--features play-uri`, `-p plx_media`); 0 in `Cargo.lock` | 0 `plx_play_uri` lines, same three forms; 0 in `Cargo.lock` |
| `make PLAYBACK_BACKENDS=<s> check-nodef` | `Finished dev profile [unoptimized + debuginfo] target(s) in 18.03s`, exit 0 | `Finished dev profile ... in 17.36s`, exit 0 |
| `cargo test --lib --no-default-features --features play-<s>,devtriggers` (in `rust-modules`, `CARGO_INCREMENTAL=0`) | `test result: ok. 1218 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 41.07s` | `test result: ok. 1196 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 41.67s` |
| simulator targets | built without simulated playback (registration edit); five type-checks pass | five type-checks pass, feature sets unchanged |
| `make RELEASE=1 PLAYBACK_BACKENDS=<s> ipk` | exit 0, `all packaging assertions passed`, `PASS actual IPK: 7 ELF files, all link inputs and transformed SHA-256 verified`; 20,174,614 bytes | exit 0, same two lines; 20,541,048 bytes |
| package inspection, `make -s print-playback-config` | passes (see below) | passes (unchanged Feed configuration) |
| `make RELEASE=1 PLAYBACK_BACKENDS=none pkg/plxnative` refuses | `A television package requires a URI or feed playback backend.`, make exit 2 | same |
| non-test code naming the deleted backend, over the shared crates and the survivor | 0 hits in 568 `.rs` files | 0 hits in 592 `.rs` files |
| `python3 ci/check-module-layers.py` | `green - 17 layers, 1716 modules` | `green - 17 layers, 1767 modules` |
| `bash ci/check-deps.sh` | `check-deps: all gates green` | `check-deps: all gates green` (after the `MACHINES` fix below) |

`make check` ignores `PLAYBACK_BACKENDS`, so the test run is the explicit `cargo test` line above.

The non-test-code row is not a literal `grep -rn`: that also matches comments and test code
(664 raw lines in `rust-modules/media` naming `play-feed` or `plx_play_feed` in `nofeed`, 7 in
`screens`, 14 naming `play-uri` or `plx_play_uri` in `nouri`). Non-test code was scanned with a
scratch script that blanks comments, skips wholly-test files (`ci/rust_test_modules.py`) and drops
`cfg(test)`-gated items. Its positive control finds `PhysicalSet` in non-test media code and drops
`new_probed`. The script is not in the repository.

### Package inspection

URI only (`make -s RELEASE=1 PLAYBACK_BACKENDS=uri print-playback-config`):
`backends=uri`, `features=--no-default-features --features play-uri`, `target=target-release-uri`,
`native=src/crashtrace.c src/main.c src/sentry_context.c src/svg.c src/video_plane.c src/compat/getauxval.c`
(no `src/starfish.c`), `libraries=-lSDL2 -lSDL2_ttf -lGLESv2 -lluna-service2 -lglib-2.0 -lwayland-client`
(no `-lplayerAPIs -lpf-1.0`), `interposer=` empty. `PLAYBACK_BACKENDS=uri bash ci/check-elf.sh
pkg/plxnative` exits 0 (`no Feed hook or relocation; Load-with-context has no loader dependency`,
`13 entries for uri, unchanged`); `DT_NEEDED` has neither `libplayerAPIs` nor `libpf`, and
`strings` finds no `StarfishMediaAPIs` or `callbackFunctionHook`. The package's data tar still holds
`libavutil-plx.so.61`, `libavcodec-plx.so.63`, `libavformat-plx.so.63` and `libass-plx.so.0`.

Feed only keeps `src/starfish.c`, `-lplayerAPIs -lpf-1.0` and
`interposer=-Wl,--export-dynamic-symbol=_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc`;
`ci/check-elf.sh` exits 0 (`exact hook exported GLOBAL/DEFAULT`, `15 entries for feed, unchanged`).

### Simulator targets

These are `cargo check` type-checks of the simulator feature sets, not the full `make macapp`,
`screenshots-sim` or `site-video-sim` links (those need the staged host FFmpeg and libass). The
make recipes' own command lines were the edited ones.

| Target | `nofeed` | `nouri` |
| --- | --- | --- |
| CI step, `--lib --no-default-features` | `--features hostsim` (25.28s) | `--features hostsim,play-feed` (18.05s) |
| `screenshots-sim` | `hostsim,devtriggers --bin plxnative-sim` | `hostsim,play-feed,devtriggers --bin plxnative-sim` |
| `site-video-sim` | `--release ... hostsim,devtriggers,placeholder-sentinel` | `--release ... hostsim,play-feed,devtriggers,placeholder-sentinel` |
| `sim-macos` | `--features hostsim --bin plxnative-sim` (default features, no edit) | same |
| `macapp` | `--release --no-default-features --features hostsim --bin plxnative-sim` | `... hostsim,play-feed --bin plxnative-sim` |

Without Feed the simulator installs `NoSink`, so Play reaches the failure read-out
(`ci/test_sim_backend.py` states this).

## `nofeed`: URI survives, `play-feed` deleted

Commit `128bc3282`, 52 files: 38 tracked files of `rust-modules/play-feed/` deleted (`git rm -r`)
and these 14 edited or deleted. "old L" is the line in the base file.

- `ci/allow/forks/play-feed.txt`: deleted (the whole ledger file, 3 sites).
- `.github/workflows/ci.yml`: old L266 `--features hostsim,play-feed` becomes `--features hostsim`;
  old L297 drops `plx_play_feed -p` from the package list.
- `Makefile`:
  - L405 `-lwayland-client $(if $(PLAYBACK_HAS_FEED),-lplayerAPIs -lpf-1.0,)` becomes
    `-lwayland-client`.
  - L565 `PLAYBACK_BACKENDS ?= both` becomes `uri`; L569-570 the accepted set
    `both uri feed none` becomes `uri none`, with the error text to match.
  - L572, L574, L577 delete `PLAYBACK_FEATURES_both`, `PLAYBACK_FEATURES_feed` and
    `PLAYBACK_HAS_FEED`.
  - L874 `SRCS` drops the `$(if $(PLAYBACK_HAS_FEED),,src/starfish.c)` condition and always
    filters `src/starfish.c` out; L1072 `SMP_INTERPOSER_LDFLAG` becomes empty.
  - L1001 drops `rust-modules/play-feed` from the Rust inputs; L1653, L1672, L1701, L1735,
    L1759 drop `plx_play_feed` from the package lists.
  - L2464 and L2497 drop `play-feed` from the `screenshots-sim` and `site-video-sim` feature
    sets.
- `ci/check-elf.sh`: L18 default mode `both` becomes `uri`; L20-21 accepts exactly `uri`.
- `ci/mkmacapp.py`: L135 `hostsim,play-feed` becomes `hostsim`.
- `ci/module-layers.ini`: L146-149 deletes `[play_feed]`; L157 (`[media]`), L177 and L182
  (`[app]`) drop `play_feed` from `uses`.
- `ci/rust_toolchain.py`: L38 `PACKAGED_BACKENDS = ("both", "uri", "feed")` becomes `("uri",)`.
- `rust-modules/Cargo.lock`: the `plx_play_feed` package (L880-894) and its three dependency
  mentions (L833, L941, L1008), regenerated with the pinned cargo.
- `rust-modules/Cargo.toml`: L23 drops the `"play-feed"` member; L77 drops it from `default`;
  L81 `play-feed = ["dep:plx_play_feed", "plx_platform/feed-sink"]` becomes `play-feed = []`
  (the hollow feature, see below); L109 drops `plx_play_feed?/devtriggers`; L178 drops
  `plx_play_feed?/hostsim`; L262-263 and L346-347 drop the optional dependency and the
  `test-support` dev-dependency with their comments.
- `rust-modules/media/Cargo.toml` (R-W5): L31 drops `plx_play_feed/devtriggers` from
  `devtriggers`; L36 drops `plx_play_feed/hostsim` from `hostsim`; L69 drops the
  `[dev-dependencies]` `plx_play_feed`.
- `rust-modules/screens/Cargo.toml` (R-W5): L70 drops the `[dev-dependencies]` `plx_play_feed`.
- `tools/build-bench.py`: L62 and L164 drop `plx_play_feed` from the package lists.
- `ci/list-caps.ini`: 13 `[play-feed/src/...]` sections: `ff.rs` (`demux`, `drain_wire`,
  `dts_core_packet`, `hls_fetch_text`, `strip_hdr10plus_sei`, `read_leb128`), `ff_pcm.rs`
  `MAX_CONSECUTIVE_DECODE_ERRORS`, `feed_state.rs` (`FEED_PACE_CAP_MS`, `PRIME_VIDEO_MAX_NS`),
  `native_executor.rs` `MAX_REBASE_DROPS`, `sf_event.rs` `sf_on_event_inner`, `side_reader.rs`
  `MAX_REOPENS`, `ff_fixture_bytes.rs` `rpu_fixture_nal`. (The 2026-11-04 audit predicted a
  different list, including `MAX_FEED_AHEAD_NS`, which was not there.)
- `ci/check-deps.sh`: delete `SRC_PLAY_FEED=` (L149) and its
  `python3 ci/rust_test_modules.py "$SRC_PLAY_FEED"` call (L264); delete the three-line sink case
  for `"$SRC_PLAY_FEED"/*` and its comment (L962-966); drop `"$SRC_PLAY_FEED"` from the 13 scan
  lists (L610, 621, 646, 804, 892, 947, 970, 1053, 1196, 1226, 1255, 1426, 1525).

## `nouri`: Feed survives, `play-uri` deleted

Commit `d17fe5b9d`, 26 files: 15 tracked files of `rust-modules/play-uri/` deleted and these 11
edited.

- `.github/workflows/ci.yml`: old L297 drops `plx_play_uri -p`.
- `Makefile`: L565 default `both` becomes `feed`; L569-570 accepted set becomes `feed none`;
  L572-573 delete `PLAYBACK_FEATURES_both` and `PLAYBACK_FEATURES_uri`; L577
  `PLAYBACK_HAS_FEED = $(filter feed,$(PLAYBACK_BACKENDS))`; L1001 drops `rust-modules/play-uri`
  from the Rust inputs; L1653, L1672, L1701, L1735, L1759 drop `plx_play_uri` from the package
  lists.
- `ci/check-elf.sh`: L18 default `both` becomes `feed`; L20-21 accepts exactly `feed`.
- `ci/module-layers.ini`: L142-145 deletes `[play_uri]`; L182 (`[app]`) drops `play_uri`.
- `ci/rust_toolchain.py`: L38 becomes `PACKAGED_BACKENDS = ("feed",)`.
- `rust-modules/Cargo.lock`: the `plx_play_uri` package (L895-906) and one dependency mention
  (L1009).
- `rust-modules/Cargo.toml`: L22 drops the `"play-uri"` member; L77 drops it from `default`;
  L80 `play-uri = ["dep:plx_play_uri", "plx_platform/uri-transport"]` becomes `play-uri = []`;
  L108 drops `plx_play_uri?/devtriggers`; L261 and L344-345 drop the optional dependency and the
  `test-support` dev-dependency with their comments.
- `tools/build-bench.py`: L62 and L164 drop `plx_play_uri`.
- `ci/list-caps.ini`: 3 sections, `uri_protocol.rs` `MAX_ABR_BPS`, `diagnostic_text`,
  `scrub_text`.
- `ci/machines.ini`: 2 sections, `play-uri/src/uriprobe.rs::UriProbe` and
  `play-uri/src/picture_gate.rs::PictureGate`.
- `ci/check-deps.sh`: delete `SRC_PLAY_URI=` (L148) and its `rust_test_modules.py` call (L263);
  drop `"$SRC_PLAY_URI"` from the scan lists (L610, 621, 646, 804, 892, 947, 970, 1053, 1196,
  1255, 1426, 1525) and `|"$SRC_PLAY_URI"/*` from the sink exemption case (L967); drop the five
  `"$SRC_PLAY_URI/<file>.rs"` entries (`uriprobe.rs`, `uri_protocol.rs`, `uri_lifecycle.rs`,
  `acb_bind.rs`, `picture_gate.rs`) from `MACHINES=(...)` (L1223-1225).

No `media` or `screens` edit was needed, and the simulator feature sets keep `play-feed`
unchanged.

## What failed or was left over

1. **Deleting the feature name from the application manifest fails.** `rust-modules/src` names
   the backend features in `cfg` predicates (counts above). With `play-feed` removed from
   `[features]`, `make PLAYBACK_BACKENDS=uri check-nodef` fails with 93
   `error: unexpected cfg condition value: play-feed` (`warnings = "deny"`), and with `play-uri`
   removed it fails with 24. Both drills therefore kept a HOLLOW feature in the application
   manifest (`play-feed = []`, `play-uri = []`). That passes every check, but it leaves the
   deleted backend's name as a live feature whose `--features play-feed` would enable `cfg`
   blocks that name the missing crate. Removing the sites instead is an edit of those 124 and 29 `cfg` lines, in 19 distinct files of
   `rust-modules/src` (`diagnostics.rs`, `run.rs`, `lifecycle.rs`, `content.rs`, `backends.rs`
   and others). The contract permits it as a composition edit, and the
   drill did not make it, so a real deletion owes it.
2. **Eight Python CI pins pin the backend lists.** `ci/test_arm_profile.py`,
   `test_managed_targets.py`, `test_ci_split.py`, `test_test_fast.py`, `test_build_bench.py`,
   `test_no_host_staticlib.py`, `test_check_elf.py` and `test_sim_backend.py` all pass at the base
   and fail after either deletion, because they pin the literal `-p ... plx_play_*` lists, the
   `play-uri,play-feed` strings, `LAYER_CRATES` and the `both|feed|uri` modes (`test_sim_backend.py`
   only after `nofeed`: it pins that every no-default-features simulator names `play-feed`).
   Failure counts per survivor, `nofeed` / `nouri`: `arm_profile` 3F+1E / 5F, `managed_targets` 2F / 2F,
   `ci_split` 1F / 1F, `test_fast` 2F / 2F, `build_bench` 6F / 6F, `no_host_staticlib` 1F / 1F,
   `check_elf` 46F / 51F, `sim_backend` 5F / passes. `test_gen_notices.py` and
   `test_module_graph.py` pass in both. The drill's edit lists omit these pins: a real deletion
   edits them as part of the CI lists. `ci/check-package.py --selftest` also holds `play-uri,play-feed`
   literals, which the packaging run does not execute.
3. **`ci/check-deps.sh` failed `SRC_PLAY_URI: unbound variable` in `nouri`** at the
   `MACHINES=(...)` list (L1223), which names five `play-uri` files. The first edit missed them; the
   amended commit fixes it. The ipk was built before the amend, and `check-deps.sh` is not an ipk
   input.
4. **Prose left stale.** The comments in `media/Cargo.toml` and `screens/Cargo.toml` that explain
   the removed dev-dependency (the allowance covers the dependency lines only), comments in the
   Makefile, `ci.yml` and `check-deps.sh`, `//!` docs naming `plx_play_feed` or `plx_play_uri` in
   appkit, media, media-codecs, platform and playback-contract, `docs/*`, and the headroom text of
   `ci/build-budgets.json`. Finding them is the documentation audit's work.
5. `plx_platform`'s `feed-sink` (`nofeed`) and `uri-transport` (`nouri`) features stay declared
   and no application feature enables them any more.
6. `make RELEASE=1 PLAYBACK_BACKENDS=none pkg/plxnative` changes the `RUST_CFG` stamp, so it
   deleted that tree's `pkg/plxnative` build output. The ipk and its checks had already run.
7. **Not run:** the `media` and `screens` Feed integration tests in `nofeed` (the allowance), `make
   check` in either tree, the full simulator links, and anything on the television. The Python
   pins were not run against the base inside the drill; they pass at `869ff76e2` as verified
   separately.

## Commands

Run once per survivor in its checkout, `SURVIVOR=uri` after deleting Feed and `feed` after deleting
URI, with `TC=$(make -s print-rust-toolchain)`:

```sh
cd rust-modules
CARGO_INCREMENTAL=0 cargo +"$TC" tree -e normal --offline                                         # and with --no-default-features --features play-$SURVIVOR, and -p plx_media
CARGO_INCREMENTAL=0 cargo +"$TC" test --lib --no-default-features --features play-$SURVIVOR,devtriggers
cd ..
make "PLAYBACK_BACKENDS=$SURVIVOR" check-nodef
make -s RELEASE=1 "PLAYBACK_BACKENDS=$SURVIVOR" print-playback-config
make RELEASE=1 "PLAYBACK_BACKENDS=$SURVIVOR" ipk
PLAYBACK_BACKENDS=$SURVIVOR bash ci/check-elf.sh pkg/plxnative
make RELEASE=1 PLAYBACK_BACKENDS=none pkg/plxnative        # must refuse
python3 ci/check-module-layers.py && bash ci/check-deps.sh
```

The normal graph has to be read on its own: tests unify a driver's features across the workspace,
so a green workspace test does not prove the production graph excludes the removed crate. Merely
disabling a feature also leaves a local path dependency that Cargo still resolves for test targets,
which is why the dev-dependency lines go with the crate.
