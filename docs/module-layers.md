# Module layers: getting rust-modules out of one big cycle

Status: target graph declared and gated 2026-10-02, with step L1 (the event log) landed in the
same change: 159 of the 231 baseline entries remain.

The gate is `ci/check-module-layers.py`, its config is `ci/module-layers.ini`, and the remaining
work is listed in `ci/allow/layers.txt`. Run `ci/check-module-layers.py --report` for current
numbers. The figures below are the baseline, before L1.

## Why this exists

rustc compiles and caches per **crate**. `plxnative-modules` is one 441k-line crate. Any edit
recompiles all of it, and with `CARGO_INCREMENTAL=0` (every linked worktree and every gate) it
recompiles all of it from scratch. A Cargo workspace of smaller crates would recompile only the
edited crate and the crates above it. Cargo rejects a dependency cycle between crates, so the
split needs the modules grouped into an **acyclic** graph. Diamonds are fine.

A cycle between modules inside one crate costs nothing at compile time. It matters because it is
what blocks the split.

## What the graph is today

`ci/module_graph.py` reads every place a module **names** another one out of the Rust tokens:
`crate::`/`super::`/`self::`/`$crate::` paths (including the ones in `#[serde(with = "…")]`
strings), `use` trees, bare top-level paths in `lib.rs`, and `#[macro_export]` and `#[macro_use]`
macros. These are exactly the references that would need a `[dependencies]` entry after a split. Method calls and trait dispatch name nothing and add no edge, which matches
how cross-crate dependencies work.

At baseline, of the 64 top-level modules (counting the crate root's own items as `crate`), **50
formed one strongly connected component** from production references alone. Only `aq`, `b64`,
`cbuf`, `checkpoint`, `fontcov`, `hwcnt`, `sha256`, `spki`, `svg` and the test-only modules sat
outside it. `ci/check-module-layers.py --cycles` prints the current components, with the config's
members (`ui::machine`, `diag::zlib`, …) as separate nodes.

That component looks like one tangle but comes from a short list of misplaced items:

| hub | why it ties everything together |
|---|---|
| `crate::log` (lib.rs) | 42 modules called it, and it called `lab::record`. `lab` names `route`, `player` and `ui`, so every caller was "above" the whole app. Fixed by L1: it is `crate::eventlog::log` in `base` now. |
| `dev` | Low-level trigger reads (`dev::read`, `flag`, `latched_flag!`) live in the same module as the scenario driver, which names `app`, `screens` and `ui`. |
| `ui::machine`, `ui::idle`, `ui::present`, `ui::landgate`, `ui::landing` | The state-machine runtime and the frame wake. `ui::machine` alone is named about 960 times from outside `ui` (screens, app, auth, stores, metadata), and `plex`, `webos`, `browse` and `lab` wake the frame through `ui::idle`. None of it is UI. |
| player widgets in `ui/` | `player_hud`, `track_menu`, `more_menu`, `info_panel`, `up_next`, `chapters_panel`, `timing_capsule` name `route`, `player`, `metadata` and `plex` about 500 times. |
| `gfx` ↔ `ui` | `gfx.rs` and `text.rs` name `ui::Rect`, `ui::Zoom`, `ui::theme`, `ui::frame::backdrop`, `ui::profile`. |
| `app::bootstrap::stores` | The record/replay tape that `metadata`, `person` and `collection` call through `app`. |
| `player::report` | The telemetry wire classes are defined in the player, so telemetry names media. |
| `net`/`stream` → `plex` | The transport reads `ResolvePin`, `url_host`, `user_agent` and `Origin` from the Plex layer above it. |

## The target graph

Thirteen layers, each a future crate. Each row lists what the layer may name (its `uses` in the
config). The config lists every layer explicitly, because Cargo dependencies are not transitive.
There are two stacks, graphics (`gfx` → `ui`) and data (`net` → `plex` → `telemetry` → `data`/
`session` → `media`). They meet in `screens`; `media` also draws through `gfx`.

```
app        everything below
screens    ui  media  session  data  telemetry  plex  net  gfx   + platform machine base
media          data  telemetry  plex  net  gfx                    + platform machine base
session        telemetry  plex  net                               + platform machine base
data           telemetry  plex  net                               + platform machine base
ui             gfx                                                + platform machine base
telemetry      plex  net                                          + platform machine base
plex           net                                                + platform machine base
net                                                               + platform base
gfx                                                               + platform machine base
platform                                                                     machine base
machine                                                                              base
base
```

| layer | members | prod lines | an edit there recompiles |
|---|---|---:|---:|
| base | `eventlog paths task cbuf sha256 b64 spki dynlib checkpoint storage_worker fontcov surface diag::{zlib,spans,heartbeat} testlock testnet` | 8k | everything |
| machine | `ui::{machine,present,idle,landgate,landing,motion}` | 4k | 98% |
| platform | `webos storage keymanager devcaps imgcache i18n` | 10k | 96% |
| gfx | `gfx egl text img svg gpu_timer hwcnt ui::overdraw` | 13k | 69% |
| net | `net stream` | 5k | 70% |
| plex | `plex http` | 26k | 69% |
| telemetry | `telemetry diag` (the event schema) | 15k | 61% |
| ui | `ui` (the library) | 66k | 49% |
| data | `stores browse metadata person collection search viewstate pms` | 27k | 53% |
| session | `auth` | 11k | 32% |
| media | `ff aq abr hls curlio player route` | 55k | 45% |
| screens | `screens` | 54k | 29% |
| app | `crate app dev lab capture remote focusprobe shot coldstart textinput system` | 45k | 13% |

"Prod lines" counts files that are not wholly `cfg(test)`. The last column is the share of all
production lines in that layer plus every layer above it. Line counts stand in for build time
here; they are not measured build times. Today every row would read 100%. Of the last 33 commits
that touched `rust-modules/src`, 26 touched `screens/` or `ui/`, so a screens-only edit dropping
from 100% to about 29% is where most of the payoff is. The `ui` row shrinks further once L10
moves the player widgets out.

Two choices that were not obvious:

- **media sits above data**, not below it. Route selection and the player name the data layer's
  types (`metadata::Stream`, `Dovi`, the metadata store) 84 times in production code and 191
  times in tests. The data layer names `media` 11 times. With this order the baseline is 231
  entries and 783 production references; the reverse order gives 254 and 856.
- **machine sits below platform.** The runtime names nothing above `base`. `webos` already wakes
  the frame through `ui::idle::invalidate`, and `auth`, `stores` and `plex` are written against
  `ui::machine`.

This agrees with the hand-written rules already gated by `ci/check-deps.sh` (the tables in
`ui/CLAUDE.md` and `screens/CLAUDE.md`). `ui` names no application type, `screens` never names
`app`, and `stores` names `ui::machine`, which is now its own layer. It is stricter in one place:
the six files of the `machine` layer, and `ui/overdraw.rs` in `gfx`, may no longer name the rest
of `ui/`. The machine files may also not name `gfx`, `text` or `i18n`, which the `ui/` row of that
table allows.

## The gate

`make check-python` runs `ci/check-module-layers.py` (about 3 s, no cargo) after its own suite,
`ci/test_module_graph.py`. It fails when:

- a reference, production **or** `cfg(test)`, names a layer its own layer does not `use`, and
  `ci/allow/layers.txt` has no entry for that (file, member) pair;
- an allowlist entry has gone stale. `--prune` drops fixed entries, and `tests/test_harness.py`
  pins the count;
- a module belongs to no layer. A new top-level module has to be placed in the config;
- the config itself is wrong: a cycle among `uses`, an unknown layer, a missing or duplicate
  member.

`ci/check-module-cycle.py`, which landed separately (#387), is the coarse companion. It holds the
SET of top-level modules on the big cycle and fails when a module joins it, so it catches a cycle
forming between modules this config puts in one layer. This gate is the fine one: it checks each
reference against the target graph. They agree on direction. When a step shrinks the cycle, run
`ci/check-module-cycle.py --update-baseline` in the same change.

Test code is gated too. After the split a crate's `#[cfg(test)]` code sees only that crate and its
dependencies. A test that assembles `Bridge`, `AppHost` or a screen from a low layer is an
integration test and belongs to the layer that owns all of its parts (step L13).

**When the gate fails**, fix it in this order:

1. Name the lower thing instead. A type the low layer needs usually belongs in the low layer.
2. Pass the value in. A low layer that needs a high layer's answer should take it as a parameter
   or a field, as `ui/` already does with `DrawFrame`.
3. Install a hook. Behaviour the low layer must trigger, but the high layer owns, goes through a
   function pointer or trait object the high layer registers at boot.
4. Move the module. If the code really belongs higher, move it there, as the player widgets do in
   L10.
5. Change the graph. A new `uses` edge or a re-layered module is a design change: edit
   `ci/module-layers.ini` and this document in the same diff and say why.

Adding a line to `ci/allow/layers.txt` is not on the list. The one exception is a file that
already has entries and is **renamed or split**: entries are keyed by path, so the gate reports
the old key as stale and the new path as unlisted. Move the entry to the new path in the same
diff, and do not run `--prune` first (it would delete the old key and leave the new one failing).
A split that keeps the upward name on both sides needs one line per new file, and raises the pin
in `tests/test_harness.py` by the same number.

## The migration

Each step deletes its entries from `ci/allow/layers.txt` (every entry names its step), and the
gate proves the step is done. The numbers are entries / references at baseline. Steps L1 to L3
are mechanical. The steps are independent and can land in any order, since each one only removes
edges.

| step | entries / refs | what moves |
|---|---:|---|
| **L1** log core to base — **done** | 72 / 325 | `log`, `redact_tokens`, `events_log`, `open_private_log_append`, `write_log_line` and their tests moved from `lib.rs` to `eventlog.rs` in `base`. `log` calls `eventlog::ring::record` directly (`lab::record` was a one-line wrapper of it and is gone), and all 473 references in 95 files name `crate::eventlog::log`. The log's own guards moved under it too (`diag::scrub` and `diag::ring`, which named nothing but `redact_tokens`, are `eventlog::{scrub, ring}`), and `paths::app_dir` no longer logs: the boot preamble writes the same `appdir:` line from `paths::app_dir_line()`. So `eventlog` names nothing but `paths`, `paths` names nothing, and both sit outside every cycle. |
| **L2** dev trigger primitives to base | 28 / 68 | `dev::{read, flag, latched_flag!, read_sample, guard_log_only, controlled_trigger, listed, no_wan, holdload_delay_ms}` move to base. Typed triggers (`playurl`/`PlayUrl`/`PlayDovi`, `abr_pin`, `playback_quality_override`) get parsed next to their only consumer. The `dev::scenarios` hooks called from `auth`, `telemetry` and `screens` (`signin_trouble_*`, `readout_case`, `consent_state_override`, `failure_fixture`, `harness_driven`) become seams the app installs. |
| **L3** lab config to platform | 5 / 7 | `lab::{config, is_trigger_key, menu_row_enabled}` move down; `ui/lab_toast.rs` moves to `screens`. |
| **L4** machine runtime leaves ui | 4 / 6 | `ui/{machine,present,idle,landgate,landing,motion}.rs` become their own layer. Cut the six upward names: the `page_frozen` flag moves from `gfx` into `idle`, `Measure`'s default `fit_line` (→ `text::fit_line_by`) moves to an impl in `text`, `ScreenArg`/`ScreenEvent` leave `machine.rs`, `card_motion_metrics::presented` becomes a callback, and `idle.rs`'s test stops constructing `ui::Spring`. |
| **L5** gfx stops naming ui | 2 / 61 | `Rect` and `Zoom` move into `gfx`, along with the `theme` card/clear constants `gfx`/`text` draw with. The `frame::backdrop` clip/capture state and `profile::phase` become gfx-owned state that `ui::frame` drives. |
| **L6** transport takes Plex values | 5 / 13 | `net` and `stream_redirect` receive host, user agent, resolve pin and credential policy as values. `ResolvePin`, `url_host` and the scheme type move into `net`. |
| **L7** platform owns its types | 2 / 5 | `DP_AUDIO_CODECS` moves into `devcaps`. The Dolby Vision capability types `webos/caps.rs` tests against move out of `metadata`. |
| **L8** plex stops naming upward | 7 / 10 | `pms::{urlenc_str, backoff_secs}` and `stores::EndpointRefresh(Set)` move into `plex`. `route::auto_quality_ready` and `telemetry::cleanup_after_account_clear` become values or hooks the upper layers install. The probe's link classification moves to `plex::probe`. |
| **L9** telemetry owns its wire schema | 5 / 69 | `player::report`'s `*Class`/`Trace*` enums and the `FailureKind` mapping move into `telemetry`; the player converts. `telemetry::incident` stops naming `ui::icons::Icon`, and the screen maps an incident kind to an icon. |
| **L10** player and Plex-aware widgets to screens | 38 / 511 | `ui/{player_hud,track_menu,more_menu,info_panel,up_next,chapters_panel,timing_capsule,source_list}.rs` move under `screens/player/` and `screens/`. `card_row`, `widgets`, `hero_logo`, `collection_tile` and `fmt` take plain values (an id, a kind, a rating source) instead of `plex`/`pms`/`metadata` types. |
| **L11** data owns its seams | 13 / 74 | `app::bootstrap::stores` (the record/replay tape; everything it names, `plex`, `metadata`, `person`, `collection`, `stores` and `ui::landgate`, is at or below `data`) moves into `stores`. `metadata` takes the playhead and track names as inputs. `screens::registry::ContentArg` and the shared constants (`MAX_ROW_ITEMS`, `still_key`, `item_count`) move down. |
| **L12** media owns its lifecycle seams | 7 / 28 | `app::lifecycle::{ForegroundLifecycle, transport_target, set_transport_paused, …}` move into `player`, which they describe. `app::diagnostics::enabled` and ff's `capture::send_all` tap become inputs/hooks. `route` stops formatting with `ui::fmt`. |
| **L13** tests move up to the layer that owns their parts | 41 / 96 | Test code that names a layer its own does not use (in `auth`, `plex`, `telemetry`, `data`, `ui`, `i18n`, `task`, `player`, `route` and `fontcov`) moves to the lowest layer that owns all of its parts. That is `app/` or `screens/` when it builds `Bridge`, `AppHost`, `SessionAdapter` or a screen, and lower otherwise. |
| **L14** session-layer presentation to screens | 2 / 4 | `auth::signed_in_reason` lays out the sign-in read-out's first line against `ui::widgets::StatusOverlay`'s column with `text::elide_middle_by`. It moves to `screens/login.rs`, and `auth` hands over the account name. |

### Then the split

`--report` ends with an "extractable as a crate" list. A layer is ready when neither it nor
anything it uses has entries left. Extract bottom-up: `base`, then `machine`, `platform`, and so
on. Each extraction:

- creates `rust-modules/<layer>/` as a workspace member (the storage helper in `storage/` is the
  existing example), moves the files, and turns `crate::x::` into `plx_<layer>::x::` in the layers
  above. `pub(crate)` items named from another layer become `pub`;
- keeps `plxnative-modules` as the top crate and the one `staticlib` the Makefile links. The layer
  crates are `rlib`s it depends on. `ci/test_no_host_staticlib.py` and the `$(RUST_LIB)` rule
  stay valid;
- forwards the features. `devtools`, `devtriggers`, `threadcheck`, `lab-diagnostics` and `hostsim`
  become features of each layer that has a `cfg` on them, enabled from the top crate;
- gives test helpers a feature. `testlock` and `testnet` are `cfg(test)` today, and `cfg(test)` of
  a dependency is never set when a dependent's tests build. So `base` exports them under a
  `test-support` feature that the layers above enable in `[dev-dependencies]`;
- watches `#[macro_export]`. `dynlib!` and the `focusable_via_*!` macros keep working through
  `$crate`, but a macro body that names another layer's path needs that layer as a dependency of
  the macro's crate.

## Limits of the analysis

- `cfg` predicates other than `test` count as possibly on, so the graph is the union of every
  feature configuration.
- Files included from `OUT_DIR` are not read: the generated `i18n::msg` catalog and
  `storage::state`'s install identities. Today they name nothing outside their own parent module.
- A `macro_rules!` that is neither `#[macro_export]` nor inside a `#[macro_use]` module is
  visible only to its own module and to children declared after it. The analyzer does not follow
  it; that only matters if `lib.rs` defines one, and it does not.
- The source side is per module, not per item. When an item has to move, the whole file's
  references count against the file's current layer until it does.
