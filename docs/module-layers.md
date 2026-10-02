# Module layers: getting rust-modules out of one big cycle

Status: target graph declared and gated 2026-10-02, and all fourteen migration steps (L1 to L14)
are done: 0 of the 231 baseline entries remain, and `--report` lists every layer as extractable.
What is left is the split itself ("Then the split", below).

The gate is `ci/check-module-layers.py` and its config is `ci/module-layers.ini`.
`ci/allow/layers.txt` held the migration list; it stays, empty, so a new upward reference still
fails. Run `ci/check-module-layers.py --report` for current numbers. The graph findings and the
migration table's figures are the baseline, before L1; the target-graph table is measured after
L14.

## Why this exists

rustc compiles and caches per **crate**. `plxnative-modules` is one 441k-line crate. Any edit
recompiles all of it, and with `CARGO_INCREMENTAL=0` (every linked worktree and every gate) it
recompiles all of it from scratch. A Cargo workspace of smaller crates would recompile only the
edited crate and the crates above it. Cargo rejects a dependency cycle between crates, so the
split needs the modules grouped into an **acyclic** graph. Diamonds are fine.

A cycle between modules inside one crate costs nothing at compile time. It matters because it is
what blocks the split.

## What the graph was

`ci/module_graph.py` reads every place a module **names** another one out of the Rust tokens:
`crate::`/`super::`/`self::`/`$crate::` paths (including the ones in `#[serde(with = "…")]`
strings), `use` trees, bare top-level paths in `lib.rs`, and `#[macro_export]` and `#[macro_use]`
macros. These are exactly the references that would need a `[dependencies]` entry after a split. Method calls and trait dispatch name nothing and add no edge, which matches
how cross-crate dependencies work.

At baseline, of the 64 top-level modules (counting the crate root's own items as `crate`), **50
formed one strongly connected component** from production references alone. Only `aq`, `b64`,
`cbuf`, `checkpoint`, `fontcov`, `hwcnt`, `sha256`, `spki`, `svg` and the test-only modules sat
outside it. `ci/check-module-layers.py --cycles` prints the current components, with the config's
members (`ui::machine`, `diag::zlib`, …) as separate nodes. After L14 every production component
sits inside one layer: media's `abr curlio ff hls player route`, data's eight modules, app's `app
dev textinput`, platform's `i18n storage webos`, gfx's `gfx gpu_timer ui::overdraw`, plex's `http
plex`, telemetry's `diag telemetry` and machine's `ui::machine ui::present`. A cycle inside a layer
stays inside one crate, so none of them blocks the split.

That component looked like one tangle but came from a short list of misplaced items, each now cut:

| hub | why it tied everything together |
|---|---|
| `crate::log` (lib.rs) | 42 modules called it, and it called `lab::record`. `lab` names `route`, `player` and `ui`, so every caller was "above" the whole app. Fixed by L1: it is `crate::eventlog::log` in `base` now. |
| `dev` | Low-level trigger reads (`dev::read`, `flag`, `latched_flag!`) lived in the same module as the scenario driver, which names `app`, `screens` and `ui`. Fixed by L2: they are `devtrig` in `base`. |
| `ui::machine`, `ui::idle`, `ui::present`, `ui::landgate`, `ui::landing` | The state-machine runtime and the frame wake. `ui::machine` alone was named about 960 times from outside `ui` (screens, app, auth, stores, metadata), and `plex`, `webos`, `browse` and `lab` woke the frame through `ui::idle`. None of it is UI. It is the `machine` layer, and L4 cut its last upward names. |
| player widgets in `ui/` | `player_hud`, `track_menu`, `more_menu`, `info_panel`, `up_next`, `chapters_panel`, `timing_capsule` named `route`, `player`, `metadata` and `plex` about 500 times. L10 moved them to `appkit/`. |
| `gfx` ↔ `ui` | `gfx.rs` and `text.rs` named `ui::Rect`, `ui::Zoom`, `ui::theme`, `ui::frame::backdrop`, `ui::profile`. L5 moved what they read into `gfx`. |
| `app::bootstrap::stores` | The record/replay tape that `metadata`, `person` and `collection` called through `app`. L11 moved it to `stores::tape`. |
| `player::report` | The telemetry wire classes were defined in the player, so telemetry named media. L9 moved them to `telemetry::classes`. |
| `net`/`stream` → `plex` | The transport read `ResolvePin`, `url_host`, `user_agent` and `Origin` from the Plex layer above it. L6 moved the URL types to `net::origin` and hands the rest in as values. |

## The target graph

Fourteen layers, each a future crate. Each row lists what the layer may name (its `uses` in the
config). The config lists every layer explicitly, because Cargo dependencies are not transitive.
There are two stacks, graphics (`gfx` → `ui`) and data (`net` → `plex` → `telemetry` → `data`/
`session` → `media`). They meet in `appkit`, the application widgets several screens share, and
in `screens` above it; `media` also draws through `gfx`.

```
app        everything below
screens    appkit  ui  media  session  data  telemetry  plex  net  gfx    + platform machine base
appkit         ui  media  session  data  telemetry  plex  net  gfx        + platform machine base
media          data  telemetry  plex  net  gfx                            + platform machine base
session        telemetry  plex  net                                       + platform machine base
data           telemetry  plex  net                                       + platform machine base
ui             gfx                                                        + platform machine base
telemetry      plex  net                                                  + platform machine base
plex           net                                                        + platform machine base
net                                                                       + platform base
gfx                                                                       + platform machine base
platform                                                                             machine base
machine                                                                                      base
base
```

| layer | members | prod lines | an edit there recompiles |
|---|---|---:|---:|
| base | `eventlog paths task cbuf sha256 b64 spki dynlib checkpoint storage_worker fontcov surface tile devtrig diag::{zlib,spans,heartbeat} testlock testnet` | 9k | everything |
| machine | `ui::{machine,present,idle,landgate,landing,motion}` | 4k | 97% |
| platform | `webos storage keymanager devcaps imgcache i18n labcfg` | 11k | 96% |
| gfx | `gfx egl text img svg gpu_timer hwcnt ui::overdraw` | 15k | 68% |
| net | `net stream` | 6k | 74% |
| plex | `plex http` | 26k | 72% |
| telemetry | `telemetry diag` (the event schema) | 16k | 64% |
| ui | `ui` (the library) | 51k | 47% |
| data | `stores browse metadata person collection search viewstate pms` | 27k | 56% |
| session | `auth` | 11k | 35% |
| media | `ff aq abr hls curlio player route` | 56k | 49% |
| appkit | `appkit` (the player panels and the Sources row several screens draw) | 13k | 32% |
| screens | `screens` | 54k | 28% |
| app | `crate app dev lab capture remote focusprobe shot coldstart textinput system release_line` | 42k | 12% |

"Prod lines" counts files that are not wholly `cfg(test)`, measured after L14. The last column is
the share of all production lines in that layer plus every layer above it. Line counts stand in
for build time here; they are not measured build times. Today every row would read 100%, because
the split has not happened. Of the last 33 commits that touched `rust-modules/src` at baseline, 26
touched `screens/` or `ui/`, so a screens-only edit dropping from 100% to about 28% is where most
of the payoff is. L10 took the player widgets out of `ui` (66k lines at baseline, 51k now).

Three choices that were not obvious:

- **media sits above data**, not below it. Route selection and the player name the data layer's
  types (`metadata::Stream`, `Dovi`, the metadata store) 84 times in production code and 191
  times in tests. The data layer names `media` 11 times. With this order the baseline is 231
  entries and 783 production references; the reverse order gives 254 and 856.
- **machine sits below platform.** The runtime names nothing above `base`. `webos` already wakes
  the frame through `ui::idle::invalidate`, and `auth`, `stores` and `plex` are written against
  `ui::machine`.
- **appkit sits between media and screens.** It was added by L10, which planned to move the player
  and Plex-aware widgets under `screens/` and could not: `player_hud` is drawn by the player and
  detail screens, `track_menu` by the player and preferences, `source_list` by onboarding and the
  library, and `ci/check-deps.sh`'s `sibling` gate forbids one screen family naming another. They
  name `route`, `player`, `metadata`, `plex` and `stores`, so they cannot stay in `ui` either.
  `appkit` may name everything below `screens`; `screens` and `app` may name it.

This agrees with the hand-written rules already gated by `ci/check-deps.sh` (the tables in
`ui/CLAUDE.md` and `screens/CLAUDE.md`). `ui` names no application type, `screens` never names
`app`, and `stores` names `ui::machine`, which is now its own layer. It is stricter in two places.
The six files of the `machine` layer, and `ui/overdraw.rs` in `gfx`, may no longer name the rest
of `ui/`, and the machine files may also not name `gfx`, `text` or `i18n`, which the `ui/` row of
that table allows. And `appkit` never naming `screens` or `app` is this gate's rule alone: the
tables say so, but `check-deps.sh`'s `layer` gate scans only `screens/`.

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
`ci/check-module-cycle.py --update-baseline` in the same change. After L14 its baseline holds 13
modules (44 at baseline): it sees `ui` and `diag` as one node each, so the machine-layer and
gfx-layer parts of `ui` and the base-layer parts of `diag` still close a cycle there with the
layers that may name them. This gate, which sees the members, finds no upward reference.

Test code is gated too. After the split a crate's `#[cfg(test)]` code sees only that crate and its
dependencies. A test that assembles `Bridge`, `AppHost` or a screen from a low layer is an
integration test and belongs to the layer that owns all of its parts (step L13).

**When the gate fails**, fix it in this order:

1. Name the lower thing instead. A type the low layer needs usually belongs in the low layer.
2. Pass the value in. A low layer that needs a high layer's answer should take it as a parameter
   or a field, as `ui/` already does with `DrawFrame`.
3. Install a hook. Behaviour the low layer must trigger, but the high layer owns, goes through a
   function pointer or trait object the high layer registers at boot.
4. Move the module. If the code really belongs higher, move it there, as L10 did with the player
   widgets.
5. Change the graph. A new `uses` edge or a re-layered module is a design change: edit
   `ci/module-layers.ini` and this document in the same diff and say why.

Adding a line to `ci/allow/layers.txt` is not on the list. The one exception is a file that
already has entries and is **renamed or split**: entries are keyed by path, so the gate reports
the old key as stale and the new path as unlisted. Move the entry to the new path in the same
diff, and do not run `--prune` first (it would delete the old key and leave the new one failing).
A split that keeps the upward name on both sides needs one line per new file, and raises the pin
in `tests/test_harness.py` by the same number. Since L14 the list is empty, so no file has entries
to carry: every upward reference is a fix.

## The migration

Each step deleted its entries from `ci/allow/layers.txt` (every entry named its step), and the
gate proves the step is done. The numbers are entries / references at baseline. All fourteen steps
are done; L10 and L13 each landed in two parts (a and b). The steps were independent, since each
one only removed edges, and where a step landed differently from its plan the row says what
actually moved.

| step | entries / refs | what moved |
|---|---:|---|
| **L1** log core to base — **done** | 72 / 325 | `log`, `redact_tokens`, `events_log`, `open_private_log_append`, `write_log_line` and their tests moved from `lib.rs` to `eventlog.rs` in `base`. `log` calls `eventlog::ring::record` directly (`lab::record` was a one-line wrapper of it and is gone), and all 473 references in 95 files name `crate::eventlog::log`. The log's own guards moved under it too (`diag::scrub` and `diag::ring`, which named nothing but `redact_tokens`, are `eventlog::{scrub, ring}`), and `paths::app_dir` no longer logs: the boot preamble writes the same `appdir:` line from `paths::app_dir_line()`. So `eventlog` names nothing but `paths`, `paths` names nothing, and both sit outside every cycle. |
| **L2** dev trigger primitives to base — **done** | 28 / 68 | The primitives (`read`, `flag`, `latched_flag!`, `read_sample`, `controlled_trigger`, `listed`, `guard_log_only`, `no_wan`, `holdload_delay_ms`) are the base module `devtrig`, which every caller names; the typed triggers moved beside their one consumer (`abr_pin` to `abr::ladder`, `PlayUrl`/`PlayDovi` to `player::playurl`), and the scenario reads lower layers made moved down rather than becoming hooks (`auth::scripted`, `telemetry::consent::state_override`, `screens::login`'s `harness_driven`, `player::failure_fixture`). |
| **L3** lab config to platform — **done** | 5 / 7 | `lab::{config, is_trigger_key, menu_row_enabled}` are the platform module `labcfg`, and `ui/lab_toast.rs` went to `lab/toast.rs`, since only `lab` draws it. |
| **L4** machine runtime leaves ui — **done** | 4 / 6 | The six upward names are cut: `page_frozen` lives in `ui::idle` (`gfx` re-exports it), `ScreenArg`/`ScreenEvent` and `fit_line_by`/`elide_by` moved into `ui::machine` (`ui::screen` and `text` re-export them), the loop calls `card_motion_metrics::presented` itself, and idle's plane-bit test moved to `app::run`. |
| **L5** gfx stops naming ui — **done** | 2 / 61 | `Rect`, `Crop` and `Zoom` are `gfx/geom.rs`, the colour and size tokens `gfx` and `text` draw with are `gfx/tokens.rs`, and the backdrop walk and the profilers moved whole to `gfx/backdrop.rs` and `gfx/profile.rs`; `ui` re-exports all of them at the old paths. |
| **L6** transport takes Plex values — **done** | 5 / 13 | `Origin`, `Scheme`, `url_host`, `ResolvePin` and `dial_port` are `net/origin.rs` (`plex::origin` re-exports them and keeps `CredentialPolicy`), the user agent is installed once at boot through `net::set_user_agent`, and `stream::redirect::Request` takes the credential-transport check as a function pointer. |
| **L7** platform owns its types — **done** | 2 / 5 | `DP_AUDIO_CODECS` lives in `devcaps` (`plex` re-exports it), and the Dolby Vision half of `webos/caps.rs`'s frame-safety test moved to `metadata`'s tests. |
| **L8** plex stops naming upward — **done** | 7 / 10 | `urlenc_str` is `plex::client`'s, `backoff_secs` and `EndpointRefresh(Set)` are `plex::retry` (`pms` and `stores` re-export them), `LinkClass`/`classify` are `plex::probe`'s, `route::auto_quality_ready` and `telemetry::cleanup_after_account_clear` are hooks `app::boot::install_plex_seams` installs, and `plex::session`'s whole-app tests moved to `app/plex_session_app_tests.rs`. |
| **L9** telemetry owns its wire schema — **done** | 5 / 69 | The `*Class`/`Trace*` vocabulary and a new `FailureClass` are `telemetry::classes` (`player::report` re-exports them and converts through `FailureKind::class()`), clearing the error trace is a hook the player installs, an incident hands over a telemetry-owned `ReadoutGlyph` that `screens::login` maps to an icon, and the consent adapter's live half is `telemetry::transition`. |
| **L10** player and Plex-aware widgets out of ui — **done** | 38 / 511 | Part a moved `ui/{player_hud,track_menu,more_menu,info_panel,up_next,chapters_panel,timing_capsule,source_list}.rs` and `screens/player/skip_pill.rs` to the new `appkit` layer rather than `screens/` (the third choice above); part b gave `widgets`, `card_row`, `hero_logo`, `collection_tile` and `fmt` plain values (`ui::tile::TileFacts`, a raw `u16` server id, `fmt::RatingScale`), with `screens::registry::tile_facts::of` the one `PmsMovie` converter. |
| **L11** data owns its seams — **done** | 13 / 74 | `app::bootstrap::stores` is `stores::tape`, `metadata` takes a `Playhead` value and owns `track_names`, `ContentArg` is `stores::content_arg`, `person` and `search` cap shelves at `pms::MAX_SHELF_ITEMS`, and the `Tile` trait is the base module `tile`. |
| **L12** media owns its lifecycle seams — **done** | 7 / 28 | The foreground-resume reducer and the transport-pause contract are `player::lifecycle` (`app::lifecycle` re-exports them), the stats switch is `player::DIAG_READOUT_ON`, `Venc::open` takes the capture socket writer as an argument, and `route` takes the HUD context line as a parameter, with the up-next still prefetch a hook the app installs. |
| **L13** tests move up to the layer that owns their parts — **done** | 41 / 96 | Part a moved the auth, plex, i18n, task and fontcov tests that named upper layers to `app/` (`session_*_tests.rs`), `screens/login_text_fit_tests.rs`, `plex`, `auth::owner` and `storage::client`, and moved `fontcov`'s `Measure` impl beside the trait, with `ui::machine`'s new `BareArg`/`BareMeasure` fixtures for the rest; part b moved the data, media and ui ones to `app/` (`dispatch_return_tests.rs`, `overscan_audit_tests.rs`) and `screens/` (`plaintext_question`, `library/labels_tests.rs`, `search/tests.rs`, `player`), and rewrote two against their own layer. |
| **L14** session-layer presentation to screens — **done** | 2 / 4 | `auth::signed_in_reason` is a private fn of `screens::login`, its only caller, with its two tests; `auth` already handed over the plain account name. |

### Then the split

`--report` ends with an "extractable as a crate" list. A layer is ready when neither it nor
anything it uses has entries left; since L14 every layer is. Extract bottom-up: `base`, then `machine`, `platform`, and so
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
