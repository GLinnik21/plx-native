# Module layers: getting rust-modules out of one big cycle

Status: target graph declared and gated 2026-10-02, and all fourteen migration steps (L1 to L14)
are done: 0 of the 231 baseline entries remain, so no layer names a layer it may not use. That is
not yet "extractable": 134 `cfg(test)` items were still named from another layer's tests after
L14, which a split hides from them, and the gate does not check impl coherence. Both are below
("Then the split"), and so is the split itself. Step L15, declared after them, fenced the webOS code
off behind a port, so that another TV OS can be a second port. L15 is **gate-complete**: its 44
entries are gone, and the gate fails on any reference from outside the port to a member of it. It
is not done in the sense of its own goal. L15b, the OS-neutral port, is open (below). Neither holds
up the split. **Ten of the fourteen layers, `base`, `machine`, `platform`, `gfx`, `net`, `ui`,
`plex`, `telemetry`, `data` and `session`, are their own crates, `plx_base`, `plx_machine`,
`plx_platform`, `plx_gfx`, `plx_net`, `plx_ui`, `plx_plex`, `plx_telemetry`, `plx_data` and
`plx_session`** (`rust-modules/base/`, `machine/`, `platform/`, `gfx/`, `net/`, `ui/`, `plex/`,
`telemetry/`, `data/` and `session/`; "Split 1: base" to "Split 10 (session)" below, and "Splits 9
and 10 together" for how the last two were combined); the other four (`media`, `appkit`, `screens`,
`app`) are still modules of `plxnative-modules`.

The gate is `ci/check-module-layers.py` and its config is `ci/module-layers.ini`.
`ci/allow/layers.txt` holds the migration list. L1 to L14 emptied it, L15 declared 44 entries of
its own and then removed them, so it is empty again and only shrinks. Run
`ci/check-module-layers.py --report` for current numbers. The graph findings and the migration
table's figures are the baseline, before L1; the target-graph table is measured after L14, so it
does not count `tv` or `port`.

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
macros. These are exactly the references that would need a `[dependencies]` entry after a split.
Method calls and trait dispatch name nothing and add no edge, which matches how cross-crate
dependencies work.

At baseline, of the 64 top-level modules (counting the crate root's own items as `crate`), **50
formed one strongly connected component** from production references alone. Only `aq`, `b64`,
`cbuf`, `checkpoint`, `fontcov`, `hwcnt`, `sha256`, `spki`, `svg` and the test-only modules sat
outside it. `ci/check-module-layers.py --cycles` prints the current components, with the config's
members (`ui::machine`, `diag::zlib`, …) as separate nodes. After L14 every production component
sat inside one layer: media's `abr curlio ff hls player route`, data's eight modules, app's `app
dev textinput`, platform's `i18n storage webos`, gfx's `gfx gpu_timer overdraw`, plex's `http
plex`, telemetry's `diag telemetry` and machine's `ui::machine ui::present`. L15 took the platform
one apart (`webos` is behind the port, and `tv` names neither `i18n` nor `storage`: the release
line is `i18n::webos_release_line`, and the port installs the storage helper's activator through
`storage::client::install_activator`), so seven components remain, the others unchanged. A cycle
inside a layer stays inside one crate, so none of them blocks the split.

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
| platform | `webos storage keymanager devcaps imgcache i18n labcfg tv` | 11k | 96% |
| gfx | `gfx egl text img svg gpu_timer hwcnt overdraw` | 15k | 68% |
| net | `net stream` | 6k | 74% |
| plex | `plex http` | 26k | 72% |
| telemetry | `telemetry diag` (the event schema) | 16k | 64% |
| ui | `ui` (the library) | 51k | 47% |
| data | `stores browse metadata person collection search viewstate pms` | 27k | 56% |
| session | `auth` | 11k | 35% |
| media | `ff aq abr hls curlio player route` | 56k | 49% |
| appkit | `appkit` (the player panels and the Sources row several screens draw) | 13k | 32% |
| screens | `screens` | 54k | 28% |
| app | `crate app dev lab capture remote focusprobe shot coldstart textinput system release_line port` | 42k | 12% |

"Prod lines" counts files that are not wholly `cfg(test)`, measured after L14. The last column is
the share of all production lines in that layer plus every layer above it. Line counts stand in
for build time here; they are not measured build times. Today every row would read 100%, because
the split has not happened. Of the last 33 commits that touched `rust-modules/src` at baseline, 26
touched `screens/` or `ui/`, so a screens-only edit dropping from 100% to about 28% is where most
of the payoff is. L10 took the player widgets out of `ui` (66k lines at baseline, 51k now).

Four choices that were not obvious:

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
- **The webOS port is a fence, not a layer.** `[port webos]` lists `webos`, `keymanager`,
  `system`, `player::ffi` and `port`, which stay in `platform`, `app` and `media` above, and
  nothing outside the port may name them. A layer on top would say the same thing, but then the
  modules in `platform`, `plex` and `telemetry` that named webOS before L15 would have held up
  those layers' extraction until it was finished, video sink and all. As a fence it held the line
  without blocking the split. The gate no longer sees a reference into the port from outside, so
  it can become a crate on top. L15b is the open step that makes it a port another OS could fill.

This agrees with the hand-written rules already gated by `ci/check-deps.sh` (the tables in
`rust-modules/ui/src/CLAUDE.md` and `screens/CLAUDE.md`). `ui` names no application type, `screens` never names
`app`, and `stores` names `ui::machine`, which is now its own layer. It is stricter in two places.
The six files of the `machine` layer, and `overdraw.rs` in `gfx` (it was `ui/overdraw.rs`), may no
longer name the rest of `ui/`, and the machine files may also not name `gfx`, `text` or `i18n`, which the `ui/` row of
that table allows. And `appkit` never naming `screens` or `app` is this gate's rule alone: the
tables say so, but `check-deps.sh`'s `layer` gate scans only `screens/`.

## The gate

`make check-python` runs `ci/check-module-layers.py` (about 3 s, no cargo) after its own suite,
`ci/test_module_graph.py`. It fails when:

- a reference, production **or** `cfg(test)`, names a layer its own layer does not `use`, and
  `ci/allow/layers.txt` has no entry for that (file, member) pair;
- a reference from outside a port names one of the port's members, with no entry either, and the
  list is empty. A port (`[port webos]`, step L15) is not a layer, and a port reference never held
  up the split;
- an allowlist entry has gone stale. `--prune` drops fixed entries, and `tests/test_harness.py`
  pins the count;
- a module belongs to no layer. A new top-level module has to be placed in the config;
- the config itself is wrong: a cycle among `uses`, an unknown layer, a missing or duplicate
  member.

It also counts, without failing, the `cfg(test)` references that name a test-only module or a
`#[cfg(test)]` item of **another** layer (`net::with_h2_reset_failure` from `plex::account`'s
tests, say). Those names are legal today and invisible after the split, which is why `--report`
lists every one; "Then the split" says what each needs.

`ci/check-module-cycle.py`, which landed separately (#387), is the coarse companion. It holds the
SET of top-level modules on the big cycle and fails when a module joins it, so it catches a cycle
forming between modules this config puts in one layer. This gate is the fine one: it checks each
reference against the target graph. They agree on direction. When a step shrinks the cycle, run
`ci/check-module-cycle.py --update-baseline` in the same change. After L14 its baseline held 13
modules (44 at baseline), and `ci/module-cycle-baseline.json` has the current set. It sees `diag` as one node, so the base-layer parts of `diag` still close a cycle there with the
layers that may name them (`ui` was the other such node until it became `plx_ui` and left that graph). This gate, which sees the members, finds no upward reference.

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
   `ci/module-layers.ini` and this document in the same diff and say why. If the change
   re-layers a module, or declares a port, that other code already names, record those
   references as a new step's entries in the same diff, raise the pin, and add the step to the
   migration table. L15 was added this way.

Adding a line to `ci/allow/layers.txt` is not a fix. Lines are added only by a design change under
item 5, and moved when a file that already has entries is **renamed or split**: entries are keyed by
path, so the gate reports the old key as stale and the new path as unlisted. Move the entry to the
new path in the same diff, and do not run `--prune` first (it would delete the old key and leave the
new one failing). A split that keeps the upward name on both sides needs one line per new file, and
raises the pin in `tests/test_harness.py` by the same number. Since L15 the list is empty (the pin
is 0), so a new upward reference is always a fix, and there are no entries to carry.

## The migration

Each step deletes its entries from `ci/allow/layers.txt` (every entry names its step), and the
gate proves the step is done. The numbers are entries / references at baseline, except L15's, which
are from when it was declared, after L14. L1 to L14 are done; L10 and L13 each landed in two parts
(a and b). Those steps were independent, since each one only removed edges, and where a step
landed differently from its plan the row says what actually moved. L15 is gate-complete, which
proves the references are gone and nothing more; L15b is open.

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
| **L9** telemetry owns its wire schema — **done** | 5 / 69 | The `*Class`/`Trace*` vocabulary and a new `FailureClass` are `telemetry::classes` (`player::report` re-exports them and converts through `FailureKind::class()`), clearing the error trace is a hook (`player::report::install_trace_eraser`, run at boot by `app::enter_application`, not by the first attempt: a failed preview traces without one), an incident hands over a telemetry-owned `ReadoutGlyph` that `screens::login` maps to an icon, and the consent adapter's live half is `telemetry::transition`. |
| **L10** player and Plex-aware widgets out of ui — **done** | 38 / 511 | Part a moved `ui/{player_hud,track_menu,more_menu,info_panel,up_next,chapters_panel,timing_capsule,source_list}.rs` and `screens/player/skip_pill.rs` to the new `appkit` layer rather than `screens/` (the third choice above); part b gave `widgets`, `card_row`, `hero_logo`, `collection_tile` and `fmt` plain values (`ui::tile::TileFacts`, a raw `u16` server id, `fmt::RatingScale`), with `screens::registry::tile_facts::of` the one `PmsMovie` converter. |
| **L11** data owns its seams — **done** | 13 / 74 | `app::bootstrap::stores` is `stores::tape`, `metadata` takes a `Playhead` value and owns `track_names`, `ContentArg` is `stores::content_arg`, `person` and `search` cap shelves at `pms::MAX_SHELF_ITEMS`, and the `Tile` trait is the base module `tile`. |
| **L12** media owns its lifecycle seams — **done** | 7 / 28 | The foreground-resume reducer and the transport-pause contract are `player::lifecycle` (`app::lifecycle` re-exports them), the stats switch is `player::DIAG_READOUT_ON`, `Venc::open` takes the capture socket writer as an argument, and `route` takes the HUD context line as a parameter, with the up-next still prefetch a hook the app installs. |
| **L13** tests move up to the layer that owns their parts — **done** | 41 / 96 | Part a moved the auth, plex, i18n, task and fontcov tests that named upper layers to `app/` (`session_*_tests.rs`), `screens/login_text_fit_tests.rs`, `plex`, `auth::owner` and `storage::client`, and moved `fontcov`'s `Measure` impl beside the trait, with `ui::machine`'s new `BareArg`/`BareMeasure` fixtures for the rest; part b moved the data, media and ui ones to `app/` (`dispatch_return_tests.rs`, `overscan_audit_tests.rs`) and `screens/` (`plaintext_question`, `library/labels_tests.rs`, `search/tests.rs`, `player`), and rewrote two against their own layer. |
| **L14** session-layer presentation to screens — **done** | 2 / 4 | `auth::signed_in_reason` is a private fn of `screens::login`, its only caller, with its two tests; `auth` already handed over the plain account name. |
| **L15** the webOS port — **gate-complete** | 44 / 205 | Everything outside `[port webos]` reaches the television through the `tv` interfaces in `platform` that the port fills at boot: `tv::{device, sandbox, secure, home, toast, window}`, `devcaps::dv`, and `tv::sink::VideoSink`, a Starfish-shaped verb trait that `player::ffi::StarfishSink` and `player::ffi_host::HostSink` implement. `plex_run` is `port::plex_run`. The allowlist is empty. Not "done": the sink is not OS-neutral and the simulator is not its own port (L15b). |
| **L15b** the OS-neutral port — **open** | — | An OS-neutral video sink with the ACB bind sequence behind it, the simulator as its own port, and the webOS facts the gate cannot see. See below. |

### L15: the webOS port

L1 to L14 gave the crate a direction, but webOS was still spread through it. The modules that exist
only because the target is webOS (`webos`, `keymanager`, `system` and `player::ffi`) were named
from 29 production files in 8 layers, from `platform` up to `app`, so supporting another
television OS would have meant edits in all of them. `[port webos]` fences them off: the gate fails
on a new reference from outside, and the ones that were there when the port was declared were
L15's 44 entries, 205 references of which 95 were in production code. The port's members stay in
their layers, so none of this held up the split.

L15 moved every one of those references behind an interface in `platform`'s new `tv` module that
the port fills at boot. `tv::Port` is a table of function pointers, installed once as the first
statement of `port::plex_run`, before anything that reads it. With no port installed (host unit
tests, where nothing calls `plex_run`) `tv::ABSENT` answers what the host arms answered before; a
shipping build that reaches it logs `tv: port not installed - using the no-port defaults` once.
Only `tv`'s own modules read the table. The one thing that leaves it is the sink, through
`tv::sink::installed()`, and `ci/check-deps.sh` (rule `sink`) fails on that name or `VideoSink`
outside `player/`, `tv/`, `tv.rs` and `port.rs`.
Lazily computed facts (device identity, the sandbox verdict, the Dolby Vision capability) are
published values rather than hooks: the webOS code writes them into `tv::device`, `tv::sandbox` and
`devcaps::dv` at the same boot moment as before, so readers keep their `OnceLock` semantics.

| interface | where it went | named from |
|---|---|---|
| device identity | `tv::device::{info, device, Info, Hardware}`, published by `webos::probe` | `telemetry`, `diag::schema`, `plex::identity`, `plex::session`'s tests, `screens::login`, `lab::snapshot`, `app`, `player` |
| playback capability | `devcaps::dv::{capability, probe, DvCapability}`, published by `webos::caps`; `tv::start_capability_probe` starts the probe | `metadata`, `route`, `player::engine`, `app` |
| native-video availability and repair | `tv::sandbox::{blocks_native_video, context, repair, Verdict, State, Failure, FORCE_BLOCKED}`; the verdict is published by `webos::probe` and the repair is a port hook | `player`, `route`, `appkit::player_hud`, `screens::player`, `app::playback` |
| video sink | `tv::sink::VideoSink`, implemented by `player::ffi::StarfishSink` and `player::ffi_host::HostSink`, installed in `Port.sink` and read through `tv::sink::installed` | `player::{engine, pump, threads}`, `player::claim_hold`'s tests |
| secure store | `tv::secure::{seal, open, remove, Sealed, Backend}` | `plex::session` |
| storage backend | `storage::client::install_activator`, which the port calls with `webos::activate_storage_helper` | `port::plex_run` |
| locale | `tv::system_locale` and `tv::LocaleReply`; `i18n` still owns the parse and the log lines | `i18n` |
| window, surface, video plane, bus pump and frame probe | `tv::window` | `app::boot`, `app::run`, `app/mod.rs` |
| home key | `tv::home::{go_home, poll, take_root_press, release_root_press}` | `app::input`, `app::run`, `app::adapters::session`, `app::lifecycle`'s tests |
| system toast | `tv::toast::{toast, send, Identity, Outcome, Sent}` | `app::clock_notice`, `dev::scenarios::toast_probe` |

`tv` may name only `base`, `machine` and `platform`. `port.rs` holds `plex_run` and the `PORT` table
that points each hook at the `webos`, `keymanager` and `system` function behind it. The C shim and
the simulator both enter through `port::plex_run`; `app::run_application` is the two-line body it
hands over to. `lib.rs` declares the port but re-exports nothing from it, because a re-export would
be a reference from the crate root into the port, which the gate refuses.

The video sink landed as a verb-level cut. `tv::sink::VideoSink` has one method per verb of the
Starfish seam, 29 of them, with the C return values and the `&MainThread` token on every method but
`load`, `window_mode` and `window_id`. It is **Starfish-shaped**: a load payload, `feed`, and the
ACB bind verbs that `pump` walks. Another TV OS cannot implement it as it stands. `player::ffi`
keeps its `extern "C"` block unchanged and implements the trait as `StarfishSink`; `engine`, `pump`
and `threads` call it through `player::sink()`. `player/ffi_host.rs` is no longer swapped in under
`player::ffi`. It is `player::ffi_host`, the simulator's own `VideoSink` (`HostSink`), outside the
fence. Call order, arguments and log text did not change. The deeper cut is L15b.

The verb cut was taken first because the bind sequence cannot be moved safely from here. It lives in
`pump`'s `Stage` machine, is gated on state the firmware callback writes (`SHARED`) and interleaves
with seek re-anchoring, the auto-rebuffer pause and the claim hold. `ffi_host` deliberately never
models ACB (it reports `VP_EXPORTED`, which skips the bind stages), so no host test covers the bind
order or its interleaving, and its log lines are graded on the television. A verb-for-verb move
can be graded by comparing the television's log before and after; a redesign that changes where
those stages run, and when, is its own step's scope and risk.

The simulator runs the same table. Under `hostsim` `port.rs` installs a `PORT` whose fields point
at the same `webos`, `keymanager` and `system` functions, whose `hostsim` arms answer on the host,
and whose sink is `HostSink`. So the simulator behaves as it did, but it is not yet a second
implementation of the interfaces.

L15 is gate-complete: no entry carries its tag, `plex_run`, which the C shim calls, lives in the
port, and the gate fails on a new reference into it. That is all the gate can see. L15 also set out
to put an OS-neutral sink with the bind sequence behind it and the simulator's stand-ins in their
own port. Neither landed, so L15 is not called done. They are L15b.

### L15b: the OS-neutral port

Open. L15 made every reference to the webOS code visible and one-directional. It did not make the
interfaces something another OS could implement, because the verbs and several types in them are
still webOS's. Three things are left:

1. **An OS-neutral video sink.** A sink another OS can implement takes codec configuration and
   timestamped access units, and it seeks, flushes, pauses and reports events. The ACB bind
   sequence that `pump`'s `Stage` machine walks, the callback decode in
   `sf_on_event`/`acb_on_event` and `sink_counter_kind(ty, major)` in `player/mod.rs` move behind
   it, into the port. This changes where the stages and the callback decode live and when they
   run, so it needs the television: see the reasons above.
2. **The simulator as its own port.** `ffi_host` and the `hostsim` arms of `webos`, `system` and
   `keymanager` become a second `Port` table, a second implementation of the same interfaces,
   instead of the webOS table running its host arms. That keeps both honest. The webOS-shaped types
   in `tv` also need neutral shapes then: `tv::secure::Backend` names the two webOS key managers
   and is part of the on-disk envelope, and `tv::sandbox`'s verdict and repair describe webOS's
   own device jail.
3. **The webOS facts the gate cannot see.** The gate sees names, not literals, and not modules
   that stay where they are. The port takes these too:

   - `devcaps` reads webOS's own table, `/etc/umediaserver/device_codec_capability_config.json`.
     The parsed model stays and the reader moves.
   - The libraries the app loads at run time are the television's: `libcurl` (`net`, `curlio`) and
     `libEGLfk` (`egl`). The bundled FFmpeg in `ff` is loaded by absolute path and is not one of
     them.
   - `plex::identity`'s platform constant and client headers.
   - The Magic Remote's key codes and pointer in `app::{input, boot, events}`.
   - `paths`'s fallback install prefix.
   - Outside `rust-modules/src`: `src/starfish.c`, `src/main.c`'s boot shim, the storage helper
     crate (`rust-modules/storage`, an LS2 service over DB8), and the Makefile's NDK cross-build
     and `.ipk` packaging.

L15b is done when the sink is OS-neutral, the simulator is its own port, and these facts are the
port's. The port can then leave its layers for a crate of its own on top: it names `app` to start
it, and nothing names it. `plxnative-modules` stays the staticlib the Makefile links, now holding
the port. Another TV OS is another port in that position. The simulator binary, `src/bin/sim.rs`,
is already a separate crate there, and enters through `port::plex_run`.

### Then the split

`--report` ends with an "extractable as a crate" list. A layer is ready when neither it nor anything
it uses has entries left, **and** no other layer's tests name a `cfg(test)` item of it or of a
layer below it. Since L14 the first half holds for every layer and the second for none: `--report`
listed 134 such items after L14 (8 in `base`, 11 `machine`, 13 `platform`, 8 `gfx`, 10 `net`, 21
`plex`, 8 `telemetry`, 12 `ui`, 25 `data`, 4 `session`, 6 `media`, 4 `appkit`, 4 `screens`). L15
replaced `webos::FORCE_JAIL_BLOCKED`, `webos::home_requests` and `keymanager::RPC_FOR_TEST` with
`cfg(test)` seams of `tv` (`sandbox::FORCE_BLOCKED`, `home::home_requests`,
`secure::{STORE_FOR_TEST, TestStore}`) that other layers' tests still name, so `--report` listed
136 (15 in `platform`, the rest as above) before Split 3 took the 15 with it; run it for the current
count. The
gate cannot see either remaining hazard on its own, because it checks names, not `cfg(test)`-ness
of the item named or impl coherence. Extract bottom-up: `base`, then `machine`, `platform`, and so
on. Each extraction:

- creates `rust-modules/<layer>/` as a workspace member (the storage helper in `storage/` is the
  existing example), moves the files, and turns `crate::x::` into `plx_<layer>::x::` in the layers
  above. `pub(crate)` items named from another layer become `pub`;
- keeps `plxnative-modules` as the top crate and the one `staticlib` the Makefile links. The layer
  crates are `rlib`s it depends on. `ci/test_no_host_staticlib.py` and the `$(RUST_LIB)` rule
  stay valid;
- forwards the features. `devtools`, `devtriggers`, `threadcheck`, `lab-diagnostics` and `hostsim`
  become features of each layer that has a `cfg` on them, enabled from the top crate;
- gives test helpers a feature. `cfg(test)` of a dependency is never set when a dependent's tests
  build, so every `--report` item of the layer being extracted moves behind a `test-support` feature
  (`#[cfg(any(test, feature = "test-support"))]`) that the layers above enable in
  `[dev-dependencies]`, or the test that names it moves down. They are not only `testlock` and
  `testnet`: `storage_worker::drain_for_test` (67 references), `net::with_h2_reset_failure` (from
  `plex::account`'s tests), `machine::{BareArg, BareMeasure}` (from `auth::owner`; `ui::machine`
  before Split 2),
  `gfx::backdrop::commit` (from `ui/frame/backdrop_tests.rs`), and `plex::session`'s `TempSession`,
  `with_io_for_test`, `invalidate_for_test` and `reads_for_test` (from
  `app/plex_session_app_tests.rs`) among them. A `#[cfg(test)]` trait impl is a hazard `--report`
  cannot see, because trait dispatch names nothing: `machine`'s `impl Measure for
  fontcov::advances::ShippedMeasure`, which `auth::owner`'s and the `ui`/`appkit`/`screens` fit
  tests measure through, needs the same feature;
- checks impl coherence by hand. An impl written in a third layer, of one layer's trait for another
  layer's type, is legal in one crate and E0117 (the orphan rule) after the split. `app/recorder.rs`
  had `impl pms::initial::Sink for machine::Canon`; the impl now lives beside the trait in
  `pms/initial.rs`, since `data` may name `machine`. Before extracting a layer, look for
  `impl … for …` in the layers above whose trait and self type both live in other crates;
- watches `#[macro_export]`. `dynlib!` and the `focusable_via_*!` macros keep working through
  `$crate`, but a macro body that names another layer's path needs that layer as a dependency of
  the macro's crate;
- sweeps the workflows. CI-only steps are not in `make check`: list every `run:` step of
  `.github/workflows/*.yml` and `.github/actions/*/action.yml` and every script they invoke, and
  look in each for a path under `rust-modules/src/` that the moved files left, a `cargo` command
  without the full `-p` list, and a `--src` list missing the new crate. Splits 4 and 5 shipped red
  on two such steps: `tools/font-hint-audit.py` still opened `rust-modules/src/gfx/tokens.rs`
  (cross-build job), and the build-budget graph step counted `plx_net`'s `test-support` optional
  dependencies because `cargo metadata` unifies features across dependency kinds (host-lint job;
  the tool now reads `cargo tree`). Then
  `grep -rn "rust-modules/src/" tools ci tests Makefile .github` for each moved module name.

### Split 1: base

`base` was extracted first: `rust-modules/base/` is the workspace member `plx_base` (an `rlib`, in
the workspace beside the storage helper), `plxnative-modules` depends on it by path and is still
the one crate the Makefile links as a `staticlib`. Its members are the `[base]` list in
`ci/module-layers.ini`; `diag::{heartbeat, spans, zlib}` left `diag` and are `plx_base::diag::*`
(`base/src/diag.rs` holds only those three, and the application's own `diag` is a different module
of the same name). Every `crate::<member>::` in the application became `plx_base::<member>::`,
written by a script, not by hand; the only hand edits were `lib.rs`'s two simulator accessors.
What the extraction taught, beyond what the recipe above predicted:

- **`--report`'s 8 items were not the whole `cfg(test)` surface.** Behaviour hangs on `cfg(test)`
  in `base` too: the watchdog disarms itself (`task::watchdog`), `assert_may_block` panics instead of
  aborting (`task::blocking`), `persistent_state_root` resolves to a per-process scratch directory
  (`paths`), `diag::heartbeat` stubs the two SDL clock calls so a test binary links no SDL, and
  `devtrig` arms its readers. A dependent's tests build `plx_base` without `cfg(test)`, so all of
  those would have silently run in their shipping form. Each is now `cfg(any(test, feature =
  "test-support"))` (and `cfg(not(...))` for the shipping arm), so a dependent that enables the
  feature gets the behaviour its tests had before. The gate cannot see this class: it reads
  `cfg(test)` on *items named from another layer*, not `cfg!(test)` inside a body.
- **`pub(crate)` became `pub` across the whole layer**, except inside `macro_rules!` bodies, where
  `dynlib!` expands `pub(crate) mod`/`fn` into the *calling* crate and must keep doing so.
  `devtrig::latched_flag!` could not stay a `pub(crate) use` re-export of a private macro; it is a
  `#[macro_export]`ed `__latched_flag` re-exported as `devtrig::latched_flag`. `dynlib!`'s
  `$crate` paths already pointed at the defining crate, so no macro body changed.
- **Tests that read the source tree or the repository** moved with their crate and needed a root:
  `eventlog::scrub`'s "no log call interpolates viewing content" scan walks `base/src` *and*
  `../src` (it would otherwise have stopped reading the application, silently, and still passed:
  every later layer must be added to its root list); `paths` and `fontcov` climb one more `..`.
- **No orphan-rule hazard appeared**: no `impl` in the application has both its trait and its type
  in `plx_base` (`ShippedMeasure`'s impl is `machine`'s trait on a `plx_base` type, which is
  legal in the application crate and is legal in the `plx_machine` crate; Split 2 below).
- **The tooling that knew the tree's shape**: `ci/module_graph.py` reads every sibling
  `rust-modules/<layer>/` package named `plx_*` as part of the same module tree (so the layer gate
  and `check-module-cycle` keep one graph and `base uses nothing` is still enforced; the cycle
  baseline shrank by `diag`, which was in the big cycle only through `diag::heartbeat`), the
  `cargo test/check` recipes pass `-p plxnative-modules -p plx_base` (a bare `cargo test --lib`
  would run the application's tests only), `ci/check-deps.sh` and `ci/check-build-budgets.py`
  scan `base/src` as well, `tools/cargo-seed.py` keys on the layer's manifest, and
  `ci/test_no_host_staticlib.py` holds the layer to `rlib`. The remaining layers each need the
  same list walked again; `SRC_BASE` in `ci/check-deps.sh` is where a second crate is added, and
  Split 2 below says what that list turned out to miss.

Measured effect (`make build-bench`, same machine, 3 interleaved runs, median; the baseline runs
logged a host load above the core count, the later ones did not, so read single seconds as noise):
an edit that only touches the application crate went from 34.9 s (the old one-crate "leaf" edit)
to 31.9 s with `plx_base` fresh, an edit inside `plx_base` costs 33.2 s (it rebuilds the layer and
then the application behind it), the hub edit 35.5 s to 34.2 s, and the unit suite 59.8 s to 61.8 s
with the same 5603 tests. That is the expected size: `base` is 6.7k of 450k lines, so the split
buys about the 2-3 s that crate cost per edit. The leverage is in the layers above it, which are
the other crates' worth of lines an edit stops recompiling.

### Split 2: machine

`machine` was extracted second: `rust-modules/machine/` is the workspace member `plx_machine` (an
`rlib`, `uses = base`), holding what was `ui::machine`, `ui::present`, `ui::idle`, `ui::landgate`,
`ui::landing` and `ui::motion`. They are top-level modules of that crate, so a path
`crate::ui::machine::Host` is `plx_machine::machine::Host`; `ci/module-layers.ini` lists them as
`machine present idle landgate landing motion`, and `ci/module_graph.py` read the new crate with no
change (it picks up every sibling `plx_*` package). The module-cycle baseline did not move: none of
the six was on the cycle. There is no re-export in `ui`: the 234 files that named the modules were
rewritten by a script (`crate::ui::<m>` to `plx_machine::<m>`, `use` groups split so the machine
items get their own `use plx_machine::{..}`), and the two things it could not resolve were
`super::super::machine`/`::idle` in a nested test module (`ui/dispatch.rs`, `ui/runtime_warning.rs`)
and the `$crate::ui::machine` inside `focusable_via_*!`, which became `plx_machine::…` (a macro body
that names another crate's path expands in the caller, which depends on `plx_machine` anyway).
`pub(crate)` and `pub(super)` became `pub` across the moved files; `machine` has one
`macro_rules!`, `newtype!`, used only inside `machine.rs`. Features: `devtriggers` and `hostsim`
are forwarded (`motion`'s held phase clock, `idle`'s simulator settle clock) and `test-support`
is new. What it taught beyond the recipe:

- **`--report` listed 11 items, and the first thing it missed was behaviour.** `Present::new()`
  hands every gate a *private* wake door under `cfg(test)`, so that parallel tests cannot wake each
  other's dispatcher, and `idle::invalidate` bumps a per-thread `LOCAL_DAMAGE` counter that
  `take_local_damage` reads. A dependent's tests build `plx_machine` without `cfg(test)`, so every
  gate in the `ui`, `screens` and `app` tests would have shared the one global door and every
  quiet-frame assertion would have read a counter nothing incremented: no compile error, just
  tests that pass or fail at random. Both are `cfg(any(test, feature = "test-support"))` now, as
  are the 11 named items (`landgate`'s fixture gate and its free-function wrappers, `Armed`,
  `idle::{take_local_damage, reset_for_test}`, `BareArg`, `BareMeasure`). The private
  `landgate::phase` wrapper and `Present::global` stay `cfg(test)`: only this crate's tests call
  them, and under `test-support` alone they would be dead code.
- **The `Measure` impl for `fontcov::advances::ShippedMeasure` lives in `plx_machine`.** The trait
  is the machine crate's and the type is `plx_base`'s, so the impl may sit in either crate; it
  cannot sit in a third (E0117), which is why it could not stay in `ui` for the layers above.
  The machine crate is the lowest one that names both. It is `test-support` only, and
  `plx_machine/test-support` enables `plx_base/test-support`, because `fontcov::advances` is
  itself behind that feature. No other orphan-rule hazard appeared (the compiler is the checker:
  an `impl` of a `plx_machine` trait for a `plx_base` type in the application would be the next
  one, and there is none).
- **Moving a trait to another crate changes dead-code analysis.** `app/recorder.rs`'s
  `RecordedInit` is constructed only by the `cfg(test)` header builder; with `LogicalState` local,
  rustc counted its `impl LogicalState` as a use, and with the trait in another crate it does not
  (`never constructed`, an error under `warnings = "deny"`). It is `cfg(test)` now, which is what
  its only constructor already was. Expect the same in the next layer for any type that exists
  only for a moved trait.
- **Gates scoped to `ui/` stopped seeing the moved files.** `ci/check-deps.sh` rules that read
  `$SRC/ui` (wall clock, mutators, `session::load`, `storage` from `ui`, the focus ladder) and the
  ones that named `ui/motion.rs`, `ui/present.rs` as files (the libm and `dt` exemptions, the
  one-door gate for `present`) are pointed at `SRC_MACHINE` too, and `wholly_test_files` classifies
  the crate's `landing/stream_tests.rs` as test code. Without that the move would have removed six
  files from six gates and every gate would have stayed green.
- **Tooling that knows the tree's shape**: the `-p` lists (`-p plxnative-modules -p plx_base -p
  plx_machine`) in the Makefile, the workflow, `tools/build-bench.py` and the tests that pin them;
  `--src rust-modules/machine/src` for the line budget; `RUST_INPUTS`; `tools/cargo-seed.py` keys on
  the new manifest; the eventlog scrub test's root list gained `../machine/src`;
  `ci/test_no_host_staticlib.py` holds the crate to `rlib`; the release-configuration hook treats
  an edit in `machine/src` as a shipping-feature risk; and `make build-bench` has a `machine`
  scenario ("Edit leaf (plx_machine landgate.rs)").

Measured effect (`make build-bench`, same machine, 3 interleaved runs, median; the
host was loaded unevenly, a few runs of both sets took twice as long as their siblings, so the
medians below are noisy and the minimum is the better guide). Before, on `b1bfa1f2`: an edit in
`plx_base` 37.4 s (min 35.4), an edit of the application crate 38.1 s (min 36.9), the hub edit
45.8 s (min 33.6), the unit suite 60.8 s. After: an edit in `plx_base` 37.9 s (min 31.9), an edit
in `plx_machine` 53.3 s (min 34.6, it rebuilds the machine crate and the application behind it),
an edit of the application crate 47.2 s (min 31.2, `plx_base` and `plx_machine` fresh), the hub
edit 39.0 s (min 33.4), and the unit suite 60.9 s with the same 5603 tests (157 + 66 + 5380).
That is the expected size, and it is small: `machine` is 4.1k of 450k lines, so the split buys the
couple of seconds that crate cost per edit to everything above it. The gain is in the minima (the
application edit is 5.7 s faster), not in the noisy medians; the crates that carry the line count
(`gfx`, `plex`, `ui`, `screens`) were still inside the application crate at that point.

### Split 3: platform

`platform` was extracted third: `rust-modules/platform/` is the workspace member `plx_platform` (an
`rlib`, `uses = base machine`), holding `webos storage keymanager devcaps imgcache i18n labcfg tv`
and the `storage_service/` files that `storage` and the storage helper both include by `#[path]`
(they are not a module of their own). `webos` and `keymanager` are still `[port webos]` members, and
the port fence reads `plx_platform::webos::` exactly as it read `crate::webos::`: a probe naming
either from `coldstart.rs` still fails the gate for both. The 1828 `crate::<member>::` references in
150 application files were rewritten by a script to `plx_platform::<member>::`; `pub(crate)` became
`pub` across the moved files and in what the two generators emit (`Flavor`, the `i18n::msg`
accessors). Features: `devtriggers`, `hostsim` and `lab-diagnostics` are forwarded, `test-support`
is new and enables the two lower layers'. The 218 tests of the layer run in their own binary: the
suite is 5603 tests before and after (157 + 66 + 218 + 5162). What it taught beyond the recipe:

- **The generated code is the layer's, and so is the build script that generates it.**
  `platform/build.rs` runs the catalog generator (`platform/build_support/catalog.rs`, reading
  `locales/`) and the install-identity generator (`rust-modules/build_support/install_identities.rs`,
  which stays where it is because the storage helper's build script calls it too). It reads no
  `PLX_*` variable and emits no `rustc-link-*`, so it is never dirty on a second run:
  `ci/test_build_not_always_dirty.py` now fails on `plx_platform` as well as on the application
  crate. The application's `build.rs` kept the version, the build SHA, the host link configuration
  and the nanosvg object, lost its `serde`/`serde_json` build-dependencies, and the application
  manifest lost the `icu_*` crates, which only `i18n` used.
- **A `cargo:rustc-env` reaches one crate.** `storage::diagnostics` read `env!("PLX_VERSION")`,
  which the application's build script publishes; the platform crate cannot see it, and
  re-deriving the version rule in a second build script would have been a third copy of it (it is
  already in `build.rs` and `ci/version_rule.py`). The application hands it in instead:
  `storage::diagnostics::start(env!("PLX_VERSION"))`. The platform layer does not know what
  version the application is.
- **A test that reads another layer's source moves up.** `storage::diagnostics`'s boot-order test
  `include_str!`ed `app/mod.rs`; it now sits beside its sibling in `app/boot.rs`'s
  `seam_order_tests`, which already read the same file.
- **`cfg(not(test))` arms switch, and `test-support` must switch them the same way.** Beyond the 15
  items `--report` listed, a dependent's tests would have built `webos`, `keymanager` and `tv` with
  their real LS2 arms (`extern "C"` blocks a host test binary cannot link), `i18n::current()` with
  its "initialized before any screen" panic instead of the English default, and `storage` without
  the commit-failure hook. Each is `cfg(any(test, feature = "test-support"))` now, and
  `cfg(not(any(...)))` for the shipping arm. Items that only the layer's own tests use
  (`webos::storage_activation_reply`, `webos::caps::{ProbeFailure, parse_dv_reply}`) stay
  `cfg(test)`: under `test-support` alone they would be dead code.
- **`test-support` may add or switch behaviour; it may not remove an item the application names.**
  `cargo check --lib --tests` (the lab line of `make check`) builds the application's non-test
  library with the dev-dependency features unified in, so `i18n::initialize` and
  `tv::system_locale`, which were `cfg(not(test))` and which the application's own
  `cfg(not(test))` boot code calls, stopped existing there. They are unconditional now (a `pub`
  function is never dead code). Run `cargo check --lib --tests` after gating anything that way.
- **A latent race the split made deterministic.** `storage::diagnostics`'s umask test sets the
  process umask to 0o777 while it holds the global test lock; `imgcache`'s fixture wrote and read
  back real files without it. In one binary of 5000 tests the overlap was rare; in the layer's own
  binary of 218 it failed every run. `imgcache`'s `TestDir` takes the lock now. A layer that is
  split off may expose a race that the big suite averaged away: run the new crate's tests a few
  times alone before trusting them.
- **No orphan-rule hazard, and nothing was dead because of a trait.** `platform` defines traits
  (`tv::sink::VideoSink`) and the application implements them for its own types, which is legal; the
  compiler is the checker, as in Split 2. `tv::sandbox::Failure::Timeout` had a
  `cfg_attr(..., expect(dead_code))` that became an unfulfilled expectation once the enum was `pub`;
  it is gone.
- **FFI moved byte for byte.** `webos.rs`'s `extern "C"` blocks and `storage_service/{auxv,bus}.rs`
  (the only files with C declarations in the layer; there is no `#[link]` and no `dynlib!` in it)
  differ from their old text in `pub(crate)` alone, and the new build script links nothing, so
  `LIBS_REAL` and `ci/expected-dt-needed.txt` are untouched. The host never compiles the ARM arms:
  `cargo check --target arm-unknown-linux-gnueabi --lib -p plxnative-modules` (`.cargo/config.toml`'s
  `build-std`, no NDK, no link step) does, and passes with and without default features; use it for
  any split that moves FFI when no NDK is at hand.
- **Gates scoped by the path of a moved file stopped seeing it, and some by its spelling.**
  `ci/check-deps.sh` reads `SRC_PLATFORM` where it read `SRC_MACHINE`; the `frame` and `uistorage`
  rules name a moved module as `crate::tv::window::` and `crate::storage::`, which would have kept
  passing while matching nothing, so they accept `plx_platform::` too; the `sink` gate exempts
  `$SRC_PLATFORM/tv*`; the `fpflags` rule and the harness's private tree copy include the new
  `Cargo.toml` and `build.rs`. `ci/check-localization.py` named `webos.rs`, `tv/device.rs` and
  `devcaps/dv.rs` under `rust-modules/src` and skips a missing path without a word: it reads
  `platform/src` for them and for the constants table now. Grep the gate scripts for
  `crate::<member>` as well as for directory names.
- **Tooling that knows the tree's shape**: the `-p` lists (`-p plxnative-modules -p plx_base -p
  plx_machine -p plx_platform`) in the Makefile, the workflow, `tools/build-bench.py` and the tests
  that pin them; `--src rust-modules/platform/src` for the line budget; `RUST_INPUTS` and
  `STORAGE_INPUTS` (the helper's `#[path]` files are under `platform/`); `tools/cargo-seed.py` keys
  on the new manifest; the eventlog scrub test's root list gained `../platform/src`;
  `ci/test_no_host_staticlib.py` holds the crate to `rlib`; the release-configuration hook treats an
  edit in `platform/src` as a shipping-feature risk; and `make build-bench` has a `platform`
  scenario ("Edit leaf (plx_platform devcaps.rs)").

Measured effect (`make build-bench`, same machine, 3 interleaved runs; the host was quiet for
both sets, and the figures are non-incremental). Before, on `b131c181`: an edit in `plx_base` 31.3 s
(min 31.2), in `plx_machine` 31.4 s (min 31.3), of the application crate 32.0 s (min 31.0), the hub
edit 32.3 s (min 31.4), the unit suite 60.5 s. After: an edit in `plx_base` 30.6 s (min 30.5), in
`plx_machine` 30.9 s (min 30.5), in `plx_platform` 31.2 s (min 30.1, it rebuilds the platform crate
and the application behind it), of the application crate 30.8 s (min 28.4, the three layer crates
fresh), the hub edit 30.8 s (min 28.2), and the unit suite 61.1 s with the same 5603 tests. The gain
is the 1 to 3 s that `platform` (11k of 450k lines) cost every application edit, and no more: the
application crate is still about 30 s of every row. The leverage is in `gfx`, `net` and `plex`
and above, which carry the line count.

### Split 4 (gfx)

`gfx` was extracted fourth: `rust-modules/gfx/` is the workspace member `plx_gfx` (an `rlib`,
`uses = base machine platform`; the manifest depends on `plx_base` and `plx_machine` only), holding `gfx` (with `backdrop`, `geom`, `profile`, `tokens`), `egl`, `text`,
`img`, `svg`, `gpu_timer`, `hwcnt` and `overdraw` (which was `ui::overdraw`), and the `shaders/`
directory the renderer embeds. The module paths are `plx_gfx::<member>::` (a path
`plx_gfx::gfx::draw_rect` names module `gfx`), so `ci/module-layers.ini` lists `overdraw` where it
listed `ui::overdraw`. A script wrote the rewrite (`crate::<member>::` to `plx_gfx::<member>::` in
the application, `crate::ui::overdraw` to `plx_gfx::overdraw`, `pub(crate)` to `pub` in the moved
files except the `glsl!` re-export, which names a non-exported `macro_rules!` and must stay
`pub(crate)`); the application's `lib.rs` and `ui/mod.rs` lost the six `mod` lines and the
`pub mod overdraw;`. Features: `devtriggers` and `hostsim` are forwarded to the lower layers, `devtools`
is a feature of this crate alone (the seven-segment counter in `gfx`, enabled by the application's
own `devtools`), and `test-support` is new and enables the two lower layers'. The 94 tests of the
layer run in their own binary. What it taught beyond the recipe:

- **A layer that holds `extern "C"` blocks has a test binary that has to link them.** Splits 1 to 3
  moved no code whose own tests reach SDL, GL or nanosvg. `plx_gfx`'s `--test` binary does (the
  drawing tests make `gfx` and `text` live), and with the link lines left in the application's
  `build.rs` it fails on undefined symbols (observed: it did, with the call to the shared emitter
  removed). A `cargo:rustc-link-lib` line reaches the package that prints it and the packages that
  depend on it; `cargo:rustc-link-arg`, which carries the nanosvg object, reaches the printing
  package's own targets only. So the host link configuration moved into
  `rust-modules/build_support/host_link.rs`, which both build scripts include by `#[path]`
  (`build.rs` and `gfx/build.rs`), the way the install-identity generator is shared. The brief's
  "the new crate's build script links nothing" holds where it matters: on the television
  (`target_arch = "arm"`) `host_link::emit` returns before printing a line, the final link is the
  Makefile's, and `LIBS_REAL` and `ci/expected-dt-needed.txt` are untouched. On the host it prints
  the same lines the application's script always printed, from one file, so they cannot disagree.
- **`--report` listed 8 items, and the first thing it missed was behaviour.** `text::queue_prewarm`
  records every run into `CAPTURED_FOR_TEST` under `cfg(test)`, `text::rasterise_warm` records
  residency in a ledger instead of calling `text_tex` (no GL context on a host), and
  `gfx::delete_tex` skips `glDeleteTextures` under `cfg(not(test))` because the host driver's
  dispatch table is a null vtable and the call is an immediate SIGSEGV (`screens::login`'s
  `unmount_frees_the_qr_texture` is the test that proves it). A dependent's tests build `plx_gfx`
  without `cfg(test)`, so all three would have run in their shipping form: the last one as a crash
  of the whole `plxnative-modules` test binary. A fourth was nested, `cfg(all(debug_assertions,
  not(test)))` on `video_plane_refuses`' panic, and a grep for `cfg(not(test))` did not find it: the
  suite did (two `ui` tests that drive the refusal on purpose died on it), so grep for `test` inside
  any `cfg(...)`, not for the spelled forms. All are `cfg(any(test, feature = "test-support"))`
  now, the shipping arms `cfg(not(any(...)))`, as are the 8 named items and `text`'s
  `reset_font_warm_for_test` and a test-only `width` helper. `cargo check --lib --tests` passes on the first
  try because every switched item is `pub` or already `allow(dead_code)`.
- **An embedded asset moves with the file that names it, and nothing else follows it.** `gfx.rs`
  and `text.rs` `include_str!` `shaders/*`, resolved relative to the including file, so the
  directory moved beside them (`gfx/src/shaders/`); the Makefile's `RUST_INPUTS` `find` gained
  `rust-modules/gfx`, which is also what rebuilds the ARM archive when a shader changes (a stale
  shader on the television is the failure mode that comment exists for). `hwcnt`'s test reads
  `tools/analyze-hwcnt.py` from `CARGO_MANIFEST_DIR`, one `..` deeper now, and `ui/fixture.rs`'s
  "the four video-plane doors" test reads `gfx/src/gfx.rs` by the same manifest-relative path.
- **A gate that spells a moved item's path goes silent, not red.** `ci/check-deps.sh`'s
  `textmeasure` rule greps `crate::text::(text_width|elide|cap_h)(` and exempts `$SRC/text.rs`; after
  the rewrite no file contains that spelling, so the zero-tolerance gate would have matched
  nothing and stayed green. It accepts `(crate|plx_gfx)::text::` now and exempts
  `$SRC_GFX/text.rs`. The libm allowlist (`ci/allow/libm.txt`) names `gfx.rs` by path and failed
  loudly, which is the better way to be wrong. `ci/check-localization.py` read `ui/overdraw.rs`
  as part of `ui/` and now reads it as `gfx/src/overdraw.rs`.
- **`overdraw` left `ui`, and nothing in `ui` named it.** `gfx` and `text` use the ledger (`gate`,
  `set_clip`, `note_px`) and the application drives it (`frame_end`, `set_ledger`, `set_mask`);
  it was the one thing `gfx.rs`'s header said the renderer named of `ui`, so it moves down with
  the renderer. The rules scoped to `$SRC/ui` stopped reading the file; it holds no clock, store
  or session call, and the whole-tree rules read `$SRC_GFX`.
- **The crate does not depend on `plx_platform`.** The layer's `uses` ceiling includes `platform`
  and nothing in the moved files names it, so the manifest depends on `plx_base` and `plx_machine`
  only. An edit in `plx_platform` therefore does not rebuild `plx_gfx`, which a declared-but-unused
  dependency would have caused.
- **No orphan-rule hazard and no dead code appeared.** No `impl` in the application has both its
  trait and its type in `plx_gfx`; the compiler is the checker.
- **Tooling that knows the tree's shape**: the `-p` lists (`... -p plx_platform -p plx_gfx -p plx_net`, with Split 5) in the
  Makefile, the workflow, `tools/build-bench.py` and the tests that pin them;
  `--src rust-modules/gfx/src` for the line budget; `RUST_INPUTS`; `tools/cargo-seed.py` keys on the
  new manifest; the eventlog scrub test's root list gained `../gfx/src` (a log call in `gfx` would
  otherwise be unread); `ci/test_no_host_staticlib.py` holds the crate to `rlib`;
  `ci/test_build_not_always_dirty.py` fails on `plx_gfx` as well (and its throwaway-repository half
  copies `build.rs` alone, so it has to copy `build_support/host_link.rs` too, or the script it
  grades does not compile) (its build script prints
  `rerun-if-changed` lines and reads no environment variable of ours); the
  release-configuration hook treats an edit in `gfx/src` as a shipping-feature risk; the harness's
  private tree copy and the `fpflags` rule include the new `Cargo.toml` and `build.rs`; and
  `make build-bench` has a `gfx` scenario ("Edit leaf (plx_gfx overdraw.rs)"). The `--no-default-features`
  and `cargo check --target arm-unknown-linux-gnueabi --lib` gates pass.

### Split 5 (net)

`net` was extracted fifth: `rust-modules/net/` is the workspace member `plx_net` (an `rlib`), holding
`net` (`net.rs`, `net/origin.rs`, the HTTP/2 reset fixture script `net/h2_reset_fixture.py`) and
`stream` (`stream.rs`, `stream_redirect.rs` and the six `stream_*_tests.rs` files plus
`stream_test_support.rs`, all declared by `#[path]` from `stream.rs`). It depends on `plx_base` and
nothing else: `[net] uses = base platform` still allows `platform`, but no line of the layer names
it, so the crate does not depend on it. It has no build script and links nothing (libcurl is
`dlopen`ed through `plx_base::dynlib!`; the final link line stays the application crate's and the
Makefile's `LIBS_REAL`; `ci/expected-dt-needed.txt` did not change). The references were rewritten
by script (`crate::net` and `crate::stream` to `plx_net::net` and `plx_net::stream`, 44 files) and
`pub(crate)` became `pub` across the moved files. Features: `devtriggers` and `lab-diagnostics` are
forwarded, `test-support` is new and enables `plx_base`'s. The 128 tests of the layer run in their
own binary: the suite is 5603 before and after (with Split 4 landed in the same change: 157 + 66 +
218 + 94 + 128 + 4940). What it taught beyond
the recipe and Splits 1 to 3:

- **A `dynlib!` table is `pub(crate)` in the crate that expands it, and a layer's callers are in
  another crate.** `curlio` (in `media`) drives libcurl's easy API through `net`'s own table:
  `curl_easy_init`, `curl_easy_cleanup`, `curl_easy_setopt_{ptr,long}`, `curl_slist_{append,free_all}`
  and the `curl` module's `curl_easy_init` cell. They cannot be re-exported (E0364: a `pub(crate)`
  item cannot be `pub use`d), and wrapping them would add a second layer of calls to the variadic
  ones. `dynlib!` gained a `pub` form instead, `dynlib! { pub curl: [...] { ... } }`, implemented as
  an internal `@emit [$vis:vis]` rule that the old form forwards to with `pub(crate)`. Every other
  table (`ff.rs`, `curlio.rs`, `player/ass.rs`, `diag::zlib`) expands token for token as before.
  The visibility has to be a single `$vis:vis` fragment: a `$($vis:tt)+` repetition cannot be
  used inside the per-function repetition ("meta-variable `vis` repeats 1 time, but `fname`
  repeats 12 times"). Check for this whenever a layer owns a `dynlib!` table the layers above call.
- **`--report`'s 10 items were again not the whole surface.** It does not see a `pub use` of a
  `cfg(test)` module's items (`net::{mint_cert, spawn_dual_protocol, spawn_observed, TestCert,
  TestCaGuard, curl_ready, ...}`, which `curlio`'s tests, `auth_discovery_tests` and the
  `tls-selftest` dev trigger's tests use), an associated function called through a type
  (`ResolvePin::for_test`, from `http`'s and `curlio`'s tests), or behaviour: `request_result`
  honours `test_ca_bundle` only `cfg(test)`, which is what lets a loopback HTTPS server be
  trusted, so the bypass is `cfg(any(test, feature = "test-support"))` along with the module. The
  compiler found the first two (`cargo check --lib --tests`); only reading the code finds the
  third. The seams that only this crate's tests use (`keypin::reset_facts_for_test`, the `*_tests`
  modules) stay `cfg(test)`.
- **A fixture can live inside a test module.** `with_test_response` and `with_h2_reset_failure`
  were thin wrappers over functions inside `request_tests`, a `cfg(test)` module that also holds
  `#[test]`s, and `test-support` does not build tests. The two helpers moved to a module of their
  own, `wire_fixtures`, gated like the wrappers; `request_tests` imports them back.
- **Fixtures that need crates make those crates dependencies.** `loopback_pms` mints certificates
  (`rcgen`) and runs a TLS server (`rustls`), and the H2 fixture reads its startup line as JSON
  (`serde_json`). They left the application's `[dev-dependencies]`; in `plx_net` they are
  `optional` dependencies behind `test-support` (`dep:`) *and* ordinary dev-dependencies, because
  the crate's own tests build with `cfg(test)` and without the feature. No shipped build sees them.
- **A gate that recognised one spelling of "this is test code".** The `threads` rule in
  `ci/check-deps.sh` skips a `thread::spawn` inside a `#[cfg(test)] mod` block by matching exactly
  that attribute on the line before `mod`. `loopback_pms` is `cfg(any(test, feature =
  "test-support"))` now (its helpers are used from other crates), and its mock servers spawn
  threads. The rule accepts the second spelling. `ci/rust_test_modules.py` does not treat that
  attribute as test-only (an `any(...)` containing an unknown feature may be on), which is right:
  the whole-file exemption applies to the `stream_*_tests.rs` files, which stay bare `cfg(test)`.
- **A `cargo:rustc-env` was not needed, and a path was.** Nothing in the layer reads a `PLX_*`
  variable. `net.rs` spawns the H2 fixture from `concat!(env!("CARGO_MANIFEST_DIR"),
  "/src/net/h2_reset_fixture.py")`; `CARGO_MANIFEST_DIR` is the manifest of the crate being
  compiled, so it is `rust-modules/net` now and the script moved with the module (one `src/net/`
  directory below it). Any `include_str!`/`env!("CARGO_MANIFEST_DIR")` in a moved file needs the
  same check.
- **Scrub, budgets and the private tree copy.** The eventlog scrub test's root list gained
  `../net/src` (these are the files that handle URLs and tokens, so the privacy scan matters most
  here), `tests/test_harness.py`'s `TREE_INPUTS` gained the crate's source and manifest, and
  `ci/check-deps.sh` reads `SRC_NET` wherever it reads `SRC_PLATFORM` except the video-sink rule.
  The module-cycle baseline moved from 43 modules outside the cycle to 26 with Splits 4 and 5
  together, and the cycle itself is the same 8 modules (no moved module was on it; the smaller
  `gfx`/`text`/`ui` component the baseline also listed is gone with `gfx`);
  `ci/check-module-cycle.py --update-baseline` records it.
- **Tooling that knows the tree's shape**: the `-p` lists (`-p plxnative-modules -p plx_base -p
  plx_machine -p plx_platform -p plx_gfx -p plx_net`) in the Makefile, the workflow, `tools/build-bench.py` and
  the tests that pin them; `--src rust-modules/net/src` for the line budget; `RUST_INPUTS`;
  `tools/cargo-seed.py` keys on the new manifest; `ci/test_no_host_staticlib.py` holds the crate to
  `rlib`; the release-configuration hook treats an edit in `net/src` as a shipping-feature risk; the
  `fw-compat-reviewer` prompt names the moved file; and `make build-bench` has a `net` scenario
  ("Edit leaf (plx_net stream_redirect.rs)").
- **FFI moved byte for byte.** `net.rs` has no `#[link]` and no plain `extern "C"` block; its
  libcurl table is one `dynlib!` invocation, whose only change is the `pub` in front of `curl`, and
  its two `extern "C"` callbacks (`write_cb`, `legacy_crypto_lock`)
  differ from their old text in `pub(crate)` alone. `stream.rs` calls `libc` only.

### Splits 4 and 5 together: what the combination needed, and the measurement

The two splits were made in parallel from the same commit and landed as one change. Combining them:

- **The files both touched are lists, and the union is mechanical.** 22 files conflicted (the `-p`
  lists, `SRC_*` roots, scrub roots, workspace members, feature forwards, bench scenarios, the
  pinned tests, the docs that quote them) and none in the moved code: a token-level three-way merge
  that lets both sides insert at the same point resolved all but one hunk (the application's
  `[dev-dependencies]`, where one side added a line and the other removed two). Both rewrite
  scripts were re-run on the result and on a `main` that had moved meanwhile, and changed nothing.
- **Both gates still fire.** Proven by a temporary violating edit, not by reading: a
  `plx_gfx::text::text_width(` call in `coldstart.rs` fails `textmeasure`; a `thread::spawn`, an
  `SDL_GetTicks(` and a `/tmp/plxnative-` literal in `net/src/stream_redirect.rs` fail `threads`,
  `ticks` and `tmppath`; `SDL_GetTicks(` and `Effect::` in `gfx/src/overdraw.rs` fail `ticks` and
  `effect`; and a `log(&format!(.., d.title))` in either crate fails the eventlog scrub scan.
- **The `pub` `dynlib!` table stands.** The alternative, a narrower wrapper API exported from
  `plx_net`, is not a small cut: `curlio` is a second libcurl client that drives the easy handle
  directly (two dozen `curl_easy_setopt_{ptr,long}` calls with its own option set, slists and
  callbacks), so the wrapper would be either a pass-through with the same surface or a redesign of
  the media transport. A second `dynlib!` table in the application would bind the same library
  twice. What is `pub` is visible to this workspace's crates only; nothing is exported from the
  staticlib. The non-`pub` form expands to the same tokens as before (the `@emit` arm's body
  differs from the old single arm only in `$vis` for `pub(crate)`), the variadic arm of
  `dynlib_wrapper!` is unchanged, and an existing table cannot select the `pub` arm.
- **`uses` is a ceiling.** `ci/module-layers.ini` keeps `gfx uses base machine platform` and `net
  uses base platform`; neither crate names `platform`, so neither manifest depends on
  `plx_platform`, and an edit there rebuilds neither (the bench rows below show it).

Measured effect (`make build-bench`, same machine, 3 interleaved runs, no load warning in either
set; non-incremental). Before, on `464ceb33` (the commit before `main`'s current tip, which
differs from it by a Home change only): an edit in `plx_base` 30.7 s (min 30.4), in `plx_machine`
30.4 s (min 30.1), in `plx_platform` 30.3 s (min 30.1), of the application crate 28.4 s (min 28.1),
the hub edit 29.2 s (min 28.1), the unit suite 62.3 s (min 61.7) with 5603 tests. After: an edit in
`plx_base` 29.3 s (min 29.3), in `plx_machine` 29.2 s (min 29.2), in `plx_platform` 28.9 s (min
28.6), in `plx_gfx` 27.7 s (min 27.6), in `plx_net` 27.3 s (min 27.1), of the application crate
26.7 s (min 26.6), the hub edit 26.8 s (min 26.6), and the unit suite 67.7 s (min 67.5) with 5606
tests (the base moved: 3 new Home tests). Every edit is 1.4 to 2.4 s faster, which is what 25k of
450k lines leaving the application crate buys; the application crate is still about 27 s of every
row. The unit suite is 5 s slower: six test binaries are linked and started where four were, and
the two new ones link SDL/GL (`plx_gfx`) and build the TLS fixtures (`plx_net`).

### Split 6 (ui)

`ui` was extracted sixth: `rust-modules/ui/` is the workspace member `plx_ui` (an `rlib`,
`uses = base machine platform gfx`; the manifest depends on all four), holding everything that was
`rust-modules/src/ui/`: 59k lines, about sixty modules, the widgets, containers, frame, focus engine,
recorder and the fixtures they are tested against. Nothing under `ui/` belongs to a layer above it
(`appkit` and `screens` are top-level modules), so nothing stayed behind and the application has no
`mod ui;` any more. `ui/mod.rs` is `rust-modules/ui/src/lib.rs`, and `crate::ui::theme` is
`plx_ui::theme` (153 application files, by script). The crate owns the QR encoder, so
`qrcodegen` left the application's manifest; its features are `devtriggers` and `threadcheck`
(forwarded) and `test-support` (new, enables the four lower layers').
What it taught beyond the recipe and Splits 1 to 5:

- **A layer that is one module needs a mount, not a root.** `ci/module_graph.py` read every layer
  crate's `lib.rs` as "the crate root again", which is right for `base` or `machine` (many
  top-level modules) and wrong for `ui`: its `lib.rs` holds items (`Painter`, `draw_census`,
  `Zoom`, ...) that belong to the `ui` layer, and listing 63 top-level members instead would have
  moved those items into `crate`, the application's layer. The manifest now says
  `[package.metadata.plx] mount = "ui"`; the analyzer resolves `crate::`/`$crate::` inside the crate
  and `plx_ui::` outside it under module `ui`, so `[ui] members = ui` is unchanged and `--report`
  lists `ui::table::assert_no_fit_failures`, not `table::...`. `ci/test_module_graph.py` pins it.
  A later layer whose crate is one module (`session`'s `auth`, if it splits alone) uses the same key.
- **`--report`'s 12 items were again a fraction of the `cfg(test)` surface.** The compiler found the
  other named items (about forty, all `pub fn` helpers of `table`, `panel_motion`, `rec`, `widgets`,
  ...) as soon as the application's tests built: `cargo check --lib --tests` failed with 353 errors
  before the gating and passes after. What it cannot find is behaviour. Six places in `ui` switch on
  `cfg(test)`/`cfg(not(test))` and a dependent's tests would have run the shipping arm: the scissor
  call of `screen::gl_scissor`, `underlay::upload`, the text-measure closure of
  `widgets::value_lines`, the field kick in `containers::modal`, the `FrameCache` that
  `popover::host` is built against, and the recording hooks of `draw_census` in `lib.rs`. Each is `cfg(any(test, feature =
  "test-support"))` now, the shipping arms `cfg(not(any(...)))`. The last one lived inside
  `popover_host_tests.rs`, a test file; its no-GL stand-in moved to `popover_host_mock.rs` so the
  dependents get it without the tests. `fixture.rs` and `testapp.rs` are whole-file `#![cfg(test)]`
  and are `test-support` too (the application's screen tests are built on `FixtureHost`).
- **A layer's test binary has to link what it reaches.** As in `gfx`, the drawing tests make GL, SDL
  and nanosvg symbols live, and a `cargo:rustc-link-arg` reaches only the package that prints it,
  so `plx_ui` has its own `build.rs` that includes the shared `build_support/host_link.rs`. Without
  it `cargo test -p plx_ui` fails to link on `_svg_rasterize_rgba`. It links nothing on the television.
- **Six tests read sources through `CARGO_MANIFEST_DIR`, and the manifest directory moved a level
  down.** Five failed loudly on the first run (`fixture`'s video-plane and loop-source pins,
  `press`'s ownership test, `containers::tests::no_surface_states_its_own_dim_weight`,
  `widgets_glass_budget_tests`); each reads `../` now, because they inspect the application's
  `appkit/` and `screens/` too. The sixth would have passed silently:
  `player::report`'s "no `fetch_update`" walk only has to find 50 files, and the 63 files of `ui`
  left its root without any failure. It walks `ui/src` as well now; the eventlog scrub test gained
  `../ui/src` for the same reason. Grep for `CARGO_MANIFEST_DIR` and `read_dir` in every moved file.
  A seventh is a string rather than a path: `press`'s "App owns Input" assertion greps
  `app/mod.rs` for the spelling `input: crate::input::Input`, which the script rewrote in the
  assertion (to `crate::input`, wrong) and in the application (to `plx_ui::input`, right).
- **`$crate` and the two exported macros.** `focusable_via_composed!` and `focusable_via_view!`
  are `#[macro_export]`, so they live at the root of `plx_ui` and the six invocations in the
  application spell `plx_ui::focusable_via_view!`; a `crate::focusable_via_view!` left in the application is
  "cannot find macro in the crate root" plus six unsatisfied `Focusable` bounds. Their bodies name
  `$crate::screen::...` and `plx_machine::machine::...`: the second works because every crate that
  invokes them depends on `plx_machine`, which `ci/module-layers.ini` guarantees for `appkit` and
  `screens`. `include_str!("../../../assets/icons/...")` needed no change: `rust-modules/src/ui/`
  and `rust-modules/ui/src/` are the same depth below the repository root.
- **`pub(crate)` became `pub` across the crate, including `pub mod` for every module** (a private
  module of a `pub` item is still dead code to rustc once the item has no outside caller), and
  `cargo check -p plx_ui --features test-support` is clean, so no `pub` item hides dead code under
  the feature. No `#[expect(dead_code)]` became unfulfilled and no orphan-rule hazard appeared:
  the traits in `plx_ui` (`Focusable`, `Part`, `Screen`, `Adapters`) are implemented in the
  application for application types, which is legal.
- **Intra-doc links to layers above lose their brackets.** The `[`crate::appkit::...`]` and
  `[`crate::screens::...`]` links in the moved comments name modules `plx_ui` cannot see; the script
  strips the brackets (the gfx split did the same) so the names stay readable.
- **Gates scoped by the path or the spelling of the moved files.** `ci/check-deps.sh` has
  `SRC_UI`: the `wall`, `mutators`, `sessionwrite`, `uistorage`, `ladder` and `textmeasure`-seam
  rules named `$SRC/ui`, and the whole-tree rules (`libm`, `ticks`, `effect`, `legacy`, `frame`,
  `threads`, `tmppath`, `dt`, `sink`, `route`) scanned `$SRC` and now also `$SRC_UI`. Two of them
  would have gone silent rather than red: `nav` grepped for `crate::ui::nav::` in `screens/` (the
  spelling is `plx_ui::nav::` now) and `mutators` masks a screen's own `crate::ui::x::y(` call
  before matching. Both accept either spelling. Proven by planting a violation in a copy of the
  file and reading the gate go red, then restoring it byte for byte: `wall` (an `Instant::now()` in
  `ui/src/dwell.rs`), `uistorage` (`plx_platform::storage::` in `fmt.rs`), `nav`
  (`plx_ui::nav::` in `screens/about_panel.rs`), `textmeasure` (`plx_gfx::text::text_width(` in
  `fmt.rs`), `sessionwrite` and `ticks`. `ci/check-statics.sh` named `$SRC/ui` for its gated paths
  and fails loudly on a missing one (it did not need the fix to be noticed, which is the better way
  to be wrong); `ci/check-localization.py` read `screens`, `ui` and `appkit` under `rust-modules/src`
  and would have scanned 63 files fewer without a word, so it reads `rust-modules/ui/src` as a
  layer source (and the constants table); `ci/allow/{libm,statics}.txt` name the new paths.
- **Tooling that knows the tree's shape**: the `-p` lists (`... -p plx_gfx -p plx_net -p plx_ui`)
  in the Makefile, the workflow, `tools/build-bench.py` and the tests that pin them;
  `--src rust-modules/ui/src` for the line budget (and the budget's description);
  `RUST_INPUTS`; `tools/cargo-seed.py` keys on the new manifest; `ci/test_no_host_staticlib.py` holds
  the crate to `rlib`; `ci/test_build_not_always_dirty.py` fails on `plx_ui` as well (its build
  script prints `rerun-if-changed` only); the release-configuration hook treats an edit in `ui/src`
  as a shipping-feature risk; the harness's private tree copy and the `fpflags` rule include the
  crate's `Cargo.toml` and `build.rs`; `tests/test_harness.py`'s `rec.rs` schema read follows the
  file; `ui/src/CLAUDE.md` moved with the code and `AGENTS.md`/`docs/agent-reference.md` point at
  it. `make build-bench` has a `ui` scenario ("Edit leaf (plx_ui dwell.rs)") and its `hub`
  scenario, which appended a line to `ui/mod.rs`, appends to `plx_ui`'s `lib.rs` now, so it
  measures the same dependency (everything in the application names the crate root).
- **The module-cycle baseline is re-recorded with the wave.** The cycle is the same 8 modules; `ui`
  left the application's graph (with `plex` and `http` of Split 7: 23 modules outside the cycle,
  where the baseline said 26).

### Split 7 (plex)

`plex` was extracted seventh (Split 6 is `ui`, above; the two were made in parallel): `rust-modules/plex/` is the
workspace member `plx_plex` (an `rlib`, `uses = base machine platform net`), holding `plex` (the
typed Plex API, the session store, `grant`, `probe`, `identity`, with its `CLAUDE.md`) and `http`
(the one request door). The members kept their names, so a path is `plx_plex::plex::session::load`
and `plx_plex::http::...`, and every `crate::plex::` *inside* the moved files stayed valid; only the
3 897 references in 213 application files changed, by `split-plex-rewrite.py` (`crate::plex` and
`crate::http` to `plx_plex::plex` and `plx_plex::http`, `pub(crate)` to `pub`). The crate
depends on `plx_base`, `plx_machine`, `plx_platform` and `plx_net`, owns no build script and links
nothing. Features: `devtriggers` and `hostsim` are forwarded, `test-support` is new and enables the
four lower layers'. Its 458 tests run in their own binary. What it taught beyond the recipe:

- **`--report`'s 21 items were the smaller half of the test seam.** Most of `plex::session`'s
  behaviour hangs on `cfg(test)`: the redirected store file (`TEST_FILE`, `auth_paths`,
  `fallback_file`), the per-thread cache read and clock hooks, the `cfg(not(test))` shipping arms
  beside them (`read_live_locked`, `Instant::now`, the migration candidates) and the test-mode
  `testlock` assertions in `grant` and `servers`. A dependent's tests build `plx_plex` without
  `cfg(test)`, so every one of them would have run in its shipping form: a test through
  `TempSession` would have written the real credential store. The rule applied is mechanical, not
  per item: **every `test` inside any `cfg(..)` or `cfg!(..)` of the layer's non-test files became
  `any(test, feature = "test-support")`** (94 lines by `split-plex-gate.py`, which skips only
  `mod tests` / `mod *_tests` / `mod test_support` and the wholly-test files), so a dependent's tests
  get exactly the behaviour the layer had under `cfg(test)`. Two forms hid from the first pass:
  `cfg!(all(.., not(test)))` (a *macro*, in `session.rs` and `install_preferences.rs`) and seven
  `#[cfg(all(` attributes in `session/persistence.rs` whose `not(test)` sits alone on its own line,
  which a per-line rewrite cannot see. After any such rewrite, grep for a bare `not(test)`.
- **This is the security-relevant seam, so the shipping build was checked for it.** With the
  feature off, `cargo check -p plx_plex --lib` and the application's `--lib` (default features and
  `--no-default-features`, host and `arm-unknown-linux-gnueabi`) compile and the `*_for_test` seams
  do not exist: the symbols are absent from the rlib (`TempSession`, `redirect_for_test`,
  `CACHE_READ_FOR_TEST` found by `strings` only in the `test-support` build). The feature is enabled
  from `[dev-dependencies]` only, which cargo does not apply to a normal build.
- **An orphan-rule hazard did appear: an inherent `impl` in a higher layer.** `route/decision.rs`
  had `impl Quality { ceiling, label, from_index, index }` where `Quality` is `plex`'s
  `PlaybackQuality`, legal in one crate and E0116 in two. `ceiling` and `label` (which build a
  `plex::Ceiling` and a localized row) moved onto `PlaybackQuality` in `plex::session`, beside its
  other methods; `from_index` and `index` read the route-owned `QUALITY_LADDER`, so they became
  private free functions of `decision.rs` (`quality_from_index`, `quality_index`). Look for `impl X`
  without a trait in the layers above, not only for trait impls.
- **A `cargo:rustc-env` reaches one crate, and the version is the thing that needed it.**
  `identity.rs` reported `env!("PLX_VERSION")` as `X-Plex-Version` and in the `User-Agent`.
  Re-deriving the rule in a second build script would be a third copy of it (`build.rs`,
  `ci/version_rule.py`). The application hands it in as Split 3 did for `storage::diagnostics`:
  `plx_plex::plex::identity::set_version(env!("PLX_VERSION"))`, the first line of
  `enter_application`, and `identity::VERSION` is `identity::version()`. Unset, a `test-support`
  build answers `0.0.0-test` and a shipping build panics in every profile (the television's build is
  `--release`, where a `debug_assert!` is compiled out), so a missed wiring cannot report a
  placeholder to Plex; `app::boot::seam_order_tests` holds the hand-in as the first statement. `ci/check-package.py` still reads the three
  copies of the number (it never read the Rust constant); its comments now say the crate reports
  the number it is handed. The one test that graded the derivation
  (`version_is_the_package_or_the_next_minor_dev`, with its `RELEASE_LINE` reader) moved to
  `release_line::tests`, because `CARGO_PKG_VERSION` and `PLX_RELEASE` are the application crate's.
- **A hidden dependency on a dependency's default features.** `plex::session`'s key-mode tests call
  `plx_net::net::keypin::is_latched`, which `plx_net` gates `any(test, feature = "devtriggers")`.
  The application's tests always had `devtriggers` (a default feature); `plx_plex` has none, so
  its own tests did not compile. The two keypin readers are `test-support` too now. Expect this
  whenever a layer's tests reach a lower layer's dev-trigger readers.
- **Fixtures by relative path.** `session/migration_tests.rs` `include_str!`s
  `tests/fixtures/persistence/*` four directories up; the crate is one directory deeper (five).
  `replay_manifest_session_is_byte_stable` reads `tests/fixtures/replay` through
  `CARGO_MANIFEST_DIR/..`, now `../..`.
- **Gates.** `ci/check-deps.sh` reads `SRC_PLEX` wherever it reads `SRC_NET` (the whole-tree rules:
  libm, ticks, effect, legacy, dt, frame, spawn, tmppath) and `wholly_test_files` classifies the
  crate's test files. Proven by temporary violating edits rather than by reading: an
  `SDL_GetTicks(` in `plex/src/http.rs` fails `ticks`, and a `log(&format!(.., d.title))` in
  `plex/src/plex/retry.rs` fails the eventlog scrub scan (its root list gained `../plex/src`; these
  are the files that handle tokens and URLs). The `-p` lists gained `-p plx_plex` (Makefile,
  workflow, `tools/build-bench.py` and the tests that pin them, the docs that quote them);
  the budget step gained `--src rust-modules/plex/src`; `RUST_INPUTS`, the harness's `TREE_INPUTS`
  and the `fpflags` file list, `tools/cargo-seed.py`, `ci/test_no_host_staticlib.py`, the
  release-configuration hook and `make build-bench` (scenario "Edit leaf (plx_plex retry.rs)")
  know the crate. `module-cycle` is the same 8-module cycle; with Split 6 the baseline records 23 modules outside it,
  and the `http`/`plex` pair left its list of smaller cycles (both are inside `plx_plex`).
- **FFI and the final link are untouched.** The layer has no `extern "C"`, `#[link]` or `dynlib!`;
  `plx_plex` compiles for the ARM target (`cargo check --target arm-unknown-linux-gnueabi --lib`
  with and without default features).

### Splits 6 and 7 together: what the combination needed, and the measurement

The two splits were made in parallel from the same commit and landed as one change, on a `main`
that moved twice meanwhile (four changes, three of them to Home). Combining them:

- **The files both touched are lists again.** 42 files conflicted: the same 22 lists as in Splits 4
  and 5 plus 20 source files in which one lane rewrote `crate::ui::` and the other `crate::plex::`
  on the same line (and three doc comments in the moved `plex` files that named `crate::ui`). The
  token-level three-way merge resolved all but two prose hunks; it left two list joins that were
  not valid TOML (`"plx_ui/devtriggers",\n, "plx_plex/devtriggers"]`), so read every merged
  manifest. Both rewrite scripts were re-run on the result and again after each move of `main`: they
  rewrote the `crate::ui`/`crate::plex` paths `main` had added to `screens/home/mod.rs` since, and
  where `main` conflicted with the rewritten file its version was taken and rewritten again.
- **The gates still fire in the combination**, proven by temporary violating edits: `plx_ui`
  naming `plx_plex`, `plx_plex` naming `plx_ui`, and `plx_platform::webos`/`::keymanager` named
  from `ui/src`, `plex/src` and `coldstart.rs` each fail `ci/check-module-layers.py` (the mounted
  crate is reported as `[ui]`); `nav`, `textmeasure` (in `ui/src` and in the application), `wall`,
  `uistorage`, `ticks`, `threads`, `tmppath` and `effect` fail `ci/check-deps.sh` from the new
  crates; an English literal in `ui/src/dwell.rs` fails `ci/check-localization.py`; a
  `log(&format!(.., d.title))` in `ui/src`, `plex/src` or `screens/home` fails the eventlog scrub
  scan. A reference from `plx_ui` to the application is not the layer gate's to catch: the crate
  has no such dependency, so it does not compile.
- **One gate had gone quiet, and it was neither lane's own.** `ci/check-localization.py` reads a
  fixed list of product files, and `route/decision.rs` was on it for the quality row's text. The
  plex split moved that text (`PlaybackQuality::label`) into `plex/session.rs`, which the gate did
  not read, and its constants table read `platform`, `gfx` and `ui` only. It reads
  `plex/src/plex/session.rs` and every layer crate's constants now. When an inherent method moves
  down a layer, look for the gates that listed its old file.
- **The security-relevant seams were checked in the artifact, not in the manifest.** In a host
  release build of the application with `--no-default-features`, and in the ARM shipping archive
  itself (`PLX_RELEASE=1 cargo rustc --release --target arm-unknown-linux-gnueabi --lib
  --crate-type staticlib --no-default-features`), `nm` and `strings` find none of `TempSession`,
  `redirect_for_test`, `with_io_for_test`, `invalidate_for_test`, `reads_for_test`,
  `store_for_test`, `loopback_pms`, `test_ca_bundle`, `is_latched` or `keypin::holds`; with default
  features only the two keypin readers exist (they are the `tls-selftest` dev trigger's, as before
  the split); a `--features test-support` build of `plx_plex` has all of them, which is what shows
  the search can find them. `cargo tree -e normal -f '{p} [{f}]'` lists no `test-support` on any
  crate for either feature set (`-e normal,dev` lists it 28 times): with `resolver = "2"` a
  `[dev-dependencies]` feature reaches test and `--tests` builds only.
- **An unset version is a panic, not a placeholder.** Split 7 left `identity::version()` answering
  `0.0.0-unset` behind a `debug_assert!`, and the television's build is `--release`. Nothing can
  reach it today (`set_version` is the first statement of `enter_application`, which has one
  caller), and a source-order test now holds that; if it is ever reached, the process stops instead
  of labelling itself with a version no release had.
- **The simulator draws the same screens.** `plxnative-sim` against `tests/mock_pms.py` (the seed
  library, the placeholder token, plex.tv replaced by the mock) settles on Home and on a library
  page, and the captures match the same captures from `main` apart from the frame-pacing tick.

Measured effect (`make build-bench`, same machine, 3 interleaved runs each, non-incremental; other
lanes were building, both sets carry load warnings, so read the minima). Before, on `cab5834f`: an
edit in `plx_base` 35.3 s (min 35.1), in `plx_machine` 34.6 s (min 34.6), in `plx_platform` 34.6 s
(min 34.0), in `plx_gfx` 33.0 s (min 32.8), in `plx_net` 33.0 s (min 32.0), of the application
crate 32.4 s (min 31.5), the hub edit (`ui/mod.rs`) 32.3 s (min 31.8), the unit suite 76.0 s (min
75.2) with 5613 tests. After (measured before `main`'s second move, which adds Home tests): an edit in `plx_base` 30.9 s (min 27.8), in `plx_machine` 27.9 s
(min 27.2), in `plx_platform` 28.1 s (min 26.7), in `plx_gfx` 25.3 s (min 24.2), in `plx_net`
26.3 s (min 24.2), in `plx_ui` 24.4 s (min 23.7), in `plx_plex` 24.7 s (min 24.1), of the
application crate 21.8 s (min 21.6), the hub edit (`plx_ui`'s `lib.rs`) 23.8 s (min 23.6), and the
unit suite 72.0 s (min 71.7) with 5614 tests (the one added is the hand-in order test). An
application edit is about 10 s faster and an edit in a layer 6 to 8 s: 92k of 450k lines (`ui` 59k, `plex` 33k) no longer
recompile behind it. The unit suite did not grow this time, with eight test binaries where there
were six: the row is the run of a built suite, cargo runs the binaries one after another, and the
time is the tests themselves (per binary, one run: the application 59.9 s before and 43.8 s after,
`plx_plex` 9.3 s, `plx_gfx` 6.1 s, `plx_ui` 5.3 s, the other four under 3 s together), not linking
or starting them.

### Split 8 (telemetry)

`telemetry` was extracted eighth, alone (no parallel lane): `rust-modules/telemetry/` is the
workspace member `plx_telemetry` (an `rlib`, `uses = base machine platform net plex`; the manifest
depends on `plx_base`, `plx_platform`, `plx_net` and `plx_plex`, because no line of the crate names
`machine`), holding `telemetry` (consent, the Sentry and PostHog wire formats, the spool and the
worker) and `diag` (the typed usage-event schema; `diag::{heartbeat, spans, zlib}` are not here, they
are `plx_base::diag::*`). The members kept their names, so a path is
`plx_telemetry::telemetry::consent::current` and `plx_telemetry::diag::schema::EVENT_SPECS`, every
`crate::telemetry::` *inside* the moved files stayed valid, and only the references in 33
application files changed, by `split-telemetry-rewrite.py` (`crate::telemetry` and `crate::diag` to
`plx_telemetry::telemetry` and `plx_telemetry::diag`, `pub(crate)` to `pub`). Nothing stayed behind:
`diag` and `telemetry` named each other and nothing above them, so the pair that was one module cycle
is inside one crate and left the cycle baseline. Features: `devtriggers` and `hostsim` are
forwarded, `test-support` is new and enables the four lower layers'. Its 235 tests run in their own
binary: the suite is 5620 tests before and after the split (5630 on the tip it landed on, the ten more being `main`'s Home tests). This is the privacy-critical layer, so what it
taught is mostly about what would have gone quiet:

- **A `cargo:rustc-env` reaches one crate, and the version needed it six times; the application
  hands it in, as it does to `plx_plex`.** Six files read `env!("PLX_VERSION")` (13 sites: the
  Sentry `release` and SDK version of the crash, panic, incident and playback envelope builders, the
  auth header, the consent preview, the native SDK's release, `diag::schema`'s `app_version`
  dimension and one test assertion). They read `telemetry::release()` and `telemetry::app_version()`
  now, which answer what `telemetry::set_release` was handed: `enter_application`'s second
  statement, right behind `plex::identity::set_version`, passes `concat!("plxnative@",
  env!("PLX_VERSION"))`. Unset, a shipping build panics (it never reports a placeholder), a
  `test-support` build answers `plxnative@0.0.0-test`, and `app::boot::seam_order_tests` pins both
  hand-ins' order. **Why a second hand-in and not `plx_plex::plex::identity::version()`, which the
  first version of this change read: the release gate.** `ci/check-package.py` greps the packaged
  binary for the contiguous `plxnative@<version>` that `concat!` composes at compile time, in the
  crate that can see `PLX_VERSION`. Composed at run time in `plx_telemetry` (`format!("plxnative@{}",
  ..)`) the prefix and the number sit in different places in `.rodata`, and the gate, which no host
  check runs, would have failed on the first real package. Both hand-ins are the same `env!` on
  adjacent lines, so they cannot disagree; one version source for both would need the store to move
  into `plx_base` and the release string to be composed there, which cannot see the variable
  either. The two entry points `src/main.c` calls before `enter_application`
  (`plx_sentry_spool_external`, `plx_crash_write_image_marker`) read no version, which the fw-compat
  review confirmed by reading their call graph. `env!("PLX_VERSION")` cannot come back into the
  crate: it is a compile error where the build script does not publish it.
- **The credentials are not a `rustc-env` and needed nothing.** `telemetry::sender` reads
  `option_env!("PLX_SENTRY_DSN")`, `PLX_POSTHOG_KEY` and the two `_DEV` names from the PROCESS
  environment of the compile (the Makefile's `TELEMETRY_ENV` words, the release workflow's `env:`),
  which cargo hands to every crate it compiles, so they reach `plx_telemetry` unchanged, no build
  script is involved and nothing secret is in a tracked file. Proven with dummy values, not the real
  ones: a `--no-default-features` release build with a dummy DSN and key carries both in the
  `plx_telemetry` rlib (`strings`), the same build without them carries neither.
- **`cfg!(feature = "devtriggers")` is a compile-time guard here, and the forward is what keeps it
  alive.** `sender` refuses to compile a build that holds the production credentials and still has
  the dev-trigger surface (`const _: () = { if HAS_PROD && cfg!(feature = "devtriggers") { panic!(..) } }`).
  In a crate that does not receive the feature the condition is a constant `false`, the guard would
  stay green and a production build with dev triggers would ship. `plx_telemetry/devtriggers` is
  forwarded from the application's `devtriggers`. Proven both ways: dummy production credentials
  with the default features fail with E0080 from `plx_telemetry` (also with `-p plx_telemetry
  --features devtriggers`), the same credentials with `--no-default-features` compile, and the
  development credentials compile with either.
- **`--report`'s 8 items were again a fraction of the seam, and the shipping arms switch too.**
  Behaviour hangs on `cfg(not(test))` in the layer: the consent file's real candidates against the
  redirected one (`candidates`, `load_from`), the dev `state_override` read of `capture_initial`,
  the one-off transport's stub, `persistence`'s redirected root and thread probe, the spool's test
  path, and the ARM arms that select the storage helper over the file store. A dependent's tests
  build `plx_telemetry` without `cfg(test)`, so each would have run its shipping form, and the
  nearest consequence is a test writing the real consent file. The rule applied is the one of Split
  7, mechanical: every `test` inside any `cfg(..)` of the layer's non-test files became `any(test,
  feature = "test-support")` (73 lines by `split-telemetry-gate.py`, which skips `mod tests` and
  `*_tests`), two forms by hand (`cfg(any(all(.., not(test)), test))` and the same spread over
  several lines). Then the exceptions: what only this crate's own tests use stays `cfg(test)`,
  because under `test-support` alone it is dead code (`persistence`'s helper-store arms and their
  `cfg(any(<ARM>, test))` gates, `spool::on_append_for_test`, `diag`'s `record_stamp_used`), and the
  `cfg_attr(test, allow(dead_code))` on `consent::state_override` is gone, since a `pub` function
  is never dead. `cargo check -p plx_telemetry --features test-support` is clean with no new
  `allow`.
- **The shipping build was checked for the seams, in the artifact.** `cargo tree -e normal -f '{p}
  [{f}]'` for `plxnative-modules` lists no `test-support` on any crate, with the default features
  and with `--no-default-features` (`-e normal,dev` lists it 34 times). In the `plx_telemetry` rlib
  of a `--no-default-features` release build, `strings` finds none of `redirect_for_test`,
  `redirect_root_for_test`, `set_test_path`, `last_call_thread`, `test_events`, `deferred_len`,
  `claims_neutral_format`, `privacy_table`, `privacy_context_table`, `TEST_FILE`, `AFTER_APPEND`,
  `LAST_CALL_THREAD`, `after_append_for_test`, `note_call_thread`, `record_with_legacy` or
  `last_outcome`; the `--features test-support` control build of the same crate finds every one
  (constants such as `EVENT_SPECS` compile away in both and prove nothing), and `nm` defines 7
  symbols matching the seam names there against 0. The ARM shipping archive built the way the
  release is (`PLX_RELEASE=1 cargo rustc --release --target arm-unknown-linux-gnueabi --lib
  --crate-type staticlib --no-default-features`) has none of them either.
- **FFI moved without a change.** `telemetry/native.rs`'s `mod sdk` (the ARM-only `extern "C"` block
  of 17 Sentry Native declarations, no `#[link]`) and the two `#[no_mangle]` entry points that
  `src/main.c` calls are the old text, apart from `pub(crate)` becoming `pub` and, in two of them,
  the version read above. The new build script does not exist; the final link is the Makefile's
  (`libsentry.a` after `$(RUST_LIB)`), and `ci/expected-dt-needed.txt` and `LIBS_REAL` are
  untouched. A `#[no_mangle]` item of an rlib dependency reaches the application's staticlib, as
  `plx_base`'s `plx_runtime_path` already does, and that was observed, not assumed: the ARM
  shipping archive from this tree and the one from `main` export the same `plx_*` symbols
  (`plx_sentry_spool_external`, `plx_crash_write_image_marker`, `plx_runtime_path`) and leave the same
  395 non-Rust symbols undefined for the final link. The `fw-compat-reviewer` verdict is "safe on
  source review, ELF ungraded": no DT_NEEDED input or import changed, and the firmware matrix that
  grades the built binary is CI's. `cargo check --target arm-unknown-linux-gnueabi --lib` passes with
  and without default features.
- **Relative paths one directory deeper, one of them silent.** The `include_str!`s of `PRIVACY.md`
  and `tests/fixtures/persistence/*` gained a `../`. Two tests build a path from
  `CARGO_MANIFEST_DIR.parent()`: `diag::schema`'s "the privacy document carries the generated table"
  would have panicked on the missing file (loud), but `sentry`'s "the configured DSN parses and is
  EU" returns early when `pkg/telemetry.local.json` is unreadable, so it would have gone on passing
  while reading a file that is not there. Both go two levels up now.
- **Gates that read the old location.** `ci/check-deps.sh` has `SRC_TELEMETRY` wherever it reads
  `SRC_PLEX` (the whole-tree rules libm, ticks, effect, legacy, dt, frame, spawn, tmppath and the
  wholly-test-file resolver); `ci/check-localization.py` reads the crate for prose constants; the
  eventlog scrub test's root list gained `../telemetry/src` (these files handle consent, ids and
  report bodies, which is what the scan is for); `tests/test_harness.py`'s `TREE_INPUTS` and
  `fpflags` list, `tools/cargo-seed.py`, `ci/test_no_host_staticlib.py` (both its `LAYER_CRATES` and
  the copied `WORKSPACE_FILES`, which `make check` found the hard way), the release-configuration
  hook and the build-budget `--src` list know the crate, and the module-cycle baseline lost the
  `diag`/`telemetry` pair. Proven by temporary violating edits, reverted byte for byte: an
  `SDL_GetTicks(` in `telemetry/src/telemetry/window.rs` fails `ticks` in `ci/check-deps.sh`, and a
  `log(&format!(.., d.title))` there fails `no_log_call_site_interpolates_viewing_content` in
  `plx_base` with the new file's path in the message. `ci/check-module-layers.py` reports the layer as
  ready and the `--report` count of `cfg(test)` items named across layers went from 52 to 44.
- **No orphan-rule hazard, no inherent `impl` in a higher layer, and no dead code from `pub`.** The
  compiler was the checker: `cargo check --lib --tests` of all nine crates passed on the first try
  after the move, and the `test-support` build needed only the three reverts above. Nothing named
  `crate::telemetry` or `crate::diag` except the application's own references, which the script
  rewrote.
- **Tooling that knows the tree's shape**: the `-p` lists (`... -p plx_plex -p plx_telemetry`) in the
  Makefile, the workflow, `tools/build-bench.py` and the tests that pin them, and in the docs that
  quote them; `--src rust-modules/telemetry/src` for the line budget (450,644 lines counted, limit
  485,000); `RUST_INPUTS`; `ci/test_no_host_staticlib.py` holds the crate to `rlib`; and `make
  build-bench` has a `telemetry` scenario ("Edit leaf (plx_telemetry window.rs)").

Measured effect (`make build-bench`, same machine, 3 interleaved runs each, non-incremental, medians
with the minimum after the slash). The "before" set, on `a23ddee1`, ran while this lane was also
running cargo and other lanes were building (the log warns of a load of 16 to 18 on 10 cores in
three runs), so its medians for the first rows (`plx_base` 57.5 s, `plx_platform` 40.7 s) are
noise and the minima are the guide; the "after" set, on the same base plus this change, ran with no
load warning. Before: an edit in `plx_base` 57.5 s / 29.4 s, in `plx_machine` 30.9 / 29.3, in
`plx_platform` 40.7 / 28.8, in `plx_gfx` 29.0 / 26.6, in `plx_net` 26.6 / 25.7, in `plx_ui` 25.6 /
25.0, in `plx_plex` 26.2 / 26.1, of the application crate 23.7 / 23.0, the hub edit 25.7 / 25.6, the
unit suite 81.0 / 80.8 with 5620 tests. After: `plx_base` 26.9 / 25.6, `plx_machine` 26.3 / 25.1,
`plx_platform` 25.9 / 24.7, `plx_gfx` 22.9 / 22.4, `plx_net` 23.7 / 22.9, `plx_ui` 22.4 / 21.6,
`plx_plex` 22.8 / 22.5, `plx_telemetry` 20.7 / 20.5 (new row: it rebuilds the telemetry crate and the
application behind it), the application crate 19.8 / 19.7, the hub edit 21.6 / 21.5, the unit suite
72.7 / 70.9 with the same 5620 tests (157 + 66 + 218 + 94 + 128 + 752 + 458 + 235 + 3512). The
minima are the honest comparison: every edit is 2.5 to 3.5 s faster, which is what 15.8k of the
application's 450k lines (`telemetry` 14.1k, `diag` 1.7k) no longer
recompiling behind it buys, and the no-op build is 0.1 s in both. The unit suite gains one more test
binary and is no slower.

### Split 9 (data)

`data` was extracted ninth, in parallel with the `session` lane: `rust-modules/data/` is the
workspace member `plx_data` (an `rlib`, `uses = base machine platform net plex telemetry`; the
manifest depends on `plx_base`, `plx_machine`, `plx_platform`, `plx_plex` and `plx_telemetry`, and
on `plx_net` as a dev-dependency only, because one fixture test calls it), holding `stores` (one
command vocabulary and one machine per data store), `pms` (the Home catalog), `browse`, `metadata`,
`person`, `collection`, `search` and `viewstate`. The members kept their names, so a path is
`plx_data::pms::seed_for_test` and every `crate::<member>::` *inside* the moved files stayed valid;
only the references in 135 application files changed, by `split-data-rewrite.py` (`crate::<member>`
to `plx_data::<member>`, `pub(crate)` to `pub`, the eight `mod` lines out of `lib.rs`). The root-level
files the members include by `#[path]` (`pms_*_tests.rs`, `pms_test_support.rs`, `metadata_*`,
`search_*`: 17 files) moved with their module into `data/src/`, because none of them names a higher
layer. Features: `devtriggers` and `hostsim` are forwarded, `test-support` is new and enables the
five lower layers'. Its 422 tests (425 with `devtriggers`) run in their own binary. It was the
layer with the most `--report` items (26); the lane's report listed 18 across all layers after it (14 with Split 10 combined) and `data` is
ready. What it taught beyond the recipe:

- **Gate by rule, not by item.** Every `test` inside any `cfg(..)` of the layer's non-test code
  became `any(test, feature = "test-support")` (243 lines, by `split-data-gate.py`, which skips the
  attribute and body of `mod tests` / `mod *_tests` / `mod test_support` and the wholly-test files).
  No `cfg(not(test))` arm hid from it except the shipping arms it switches: `metadata`'s detail
  fetch (a test holds the fetch instead of spawning a thread), its two dev stand-ins, `person`'s two
  plex.tv fetches (a test that reaches them fails to link) and `pms::run`'s `EditItem` arm. Then
  the exceptions the compiler found under `--features test-support`: what only the layer's own
  tests use stays `cfg(test)` (13 private items and two imports, because under `test-support`
  alone they are dead code; `split-data-revert.py` flips them).
- **A fixture inside a `cfg(test)` module cannot be named by a seam.** `seed_two_source_table_for_owner_test`
  called `test_support::a_source`, a function of the `cfg(test)`-only fixtures module. The
  `BrowseSource` literal moved beside the seams that seed it (`browse::a_source`, one function).
- **A test that reads another layer's source moves up.** `stores/metadata.rs`'s
  `pump_wiring_tests` pinned the text of the *application's* `app/run.rs`; it is now the last module
  of `app/run.rs`. `stores/mod.rs`'s "data layers do not execute endpoint recovery" test reads
  `pms.rs`, `browse/mod.rs` and `viewstate.rs` from `CARGO_MANIFEST_DIR/src`, which are the crate's
  own, so it stays; its needle lost the `crate::` (the session layer is not nameable from data at
  all, and the test now catches either spelling). `metadata/record.rs`'s replay fixture
  `include_str!` gained a `../`.
- **The tests need the dev-trigger credential policy, so `cargo test -p plx_data` alone has to
  ask for it.** The browse and search fixtures register an `http://10.0.0.1` server and select it,
  which `plex::origin` refuses unless `devtriggers` is on (39 tests failed alone, all green in the
  unified build). The lane enabled `plx_plex/devtriggers` from `[dev-dependencies]`; the
  combination replaced that with the workspace rule ("Splits 9 and 10 together": the crate
  dev-depends on itself with `devtriggers`), so the crate alone runs the same 425 tests the suite
  does. Dev-dependency features reach test builds only.
- **Upward names were prose only.** Nothing in the layer named `ui`, `gfx`, `route`, `player`,
  `screens`, `app`, `dev` or `auth` in code; seven intra-doc links did, and lost their brackets. No
  orphan-rule hazard and no inherent `impl` in a higher layer appeared: `pms::initial::Sink` (the
  trait whose `Canon` impl was moved down in Split 2) and the `Tile` impl for `PmsMovie` (the trait
  is `plx_base`'s) are both inside the crate. `serde` needs the `rc` feature in this manifest too
  (Home cards are `Arc<PmsMovie>`).
- **Gates that read the old location.** `ci/check-deps.sh` reads `SRC_DATA` wherever it reads
  `SRC_TELEMETRY`; the six `*-owner` rules scan `$SRC_DATA` for their declaration files, spell the
  call pattern `(crate|plx_data)::` (the application now writes the second), and `OWNER_LINES` reads
  `$SRC_DATA`; the `mutators-visibility` table reads the crate; the `wall` rule's `stores` root
  moved. `ci/check-statics.sh` gates `data/src/{person.rs, metadata*, pms.rs, stores}` and
  `search/` (and the two `recents.rs` entries of `ci/allow/statics.txt` follow, or the stale-entry
  check fails). `ci/check-localization.py` listed `metadata.rs` and `person.rs` under the
  application and skipped a missing path without a word (it fails on one now, below). `tests/test_harness.py`'s
  planted-violation helpers resolve a store file in the private copy's `data/src` first. Also the
  eventlog scrub test's root list (`../data/src`), `TREE_INPUTS` and the `fpflags` list,
  `tools/cargo-seed.py`, `ci/test_no_host_staticlib.py`, the release-configuration hook, the
  build-budget `--src` list, the `-p` lists, `RUST_INPUTS`, and `make build-bench` (scenario "Edit
  leaf (plx_data tape.rs)"; added, not run). The module-cycle baseline had to change: the eight
  data modules left the 8-module cycle, and the cycle of 6 (`abr curlio ff hls player route`) is the
  remaining one.
- **Proven by temporary violating edits, reverted byte for byte:** a `pub fn pump() {}` prepended to
  `data/src/viewstate.rs` fails `viewstate-owner` in `ci/check-deps.sh`; an `SDL_GetTicks(` in
  `data/src/pms.rs` fails `ticks`; an `Instant::now()` in `data/src/stores/hubs.rs` fails `wall`; a
  `static mut` in `data/src/stores/mod.rs` fails `ci/check-statics.sh`; and a
  `log(&format!(.., d.title))` in `data/src/pms.rs` fails
  `no_log_call_site_interpolates_viewing_content` in `plx_base` with the new file's path in the
  message (the scrub test's root list gained `../data/src`; these files carry titles, ratingKeys and
  server identities). `ci/check-module-layers.py` reports the layer ready and `--report`'s count of
  `cfg(test)` items named across layers went from 44 to 18 (14 with Split 10).
- **The shipping build was checked for the seams, in the artifact.** `cargo tree -e normal -f '{p} [{f}]'`
  for `plxnative-modules` lists no `test-support` on any crate, with the default features and with
  `--no-default-features` (`-e normal,dev` listed it 41 times in the lane, 49 in the combination). In the `plx_data` rlib of a
  `--no-default-features` release build, `strings` finds none of `begin_detail_for_test`,
  `land_detail_for_test`, `detail_generation_for_test`, `set_current_for_test`, `seed_for_test`,
  `seed_grid_for_test`, `seed_named_hubs_for_test`, `with_refused_fetches_for_test`,
  `with_refused_discovery_for_test`, `fixture_with_sources`, `queue_test_landing`,
  `reverse_test_hubs`, `remove_test_item`, `land_for_test`, `publish_shelves_for_test`,
  `hold_inflight_for_test`, `REFUSE_FETCH_FOR_TEST`, `REFUSE_DISCOVERY_FOR_TEST`,
  `seed_items_for_owner_test`, `held_detail` or `seed_two_source_table_for_owner_test`; the
  `--features test-support` control build of the same crate finds every one (1 to 8 times each),
  and `nm` matches the seam names in 4 symbols there against 0. The ARM shipping archive built the
  way the release is (`PLX_RELEASE=1 cargo rustc --release --target arm-unknown-linux-gnueabi --lib
  --crate-type staticlib --no-default-features`) has none of them either.
- **FFI and the final link are untouched.** The layer has no `extern "C"`, `#[link]`, `dynlib!`,
  build script or `#[no_mangle]`; `cargo check --target arm-unknown-linux-gnueabi --lib` passes with
  and without default features.

### Split 10 (session)

`session` was extracted tenth, in parallel with `data` (Split 9, its own lane): `rust-modules/session/`
is the workspace member `plx_session` (an `rlib`, `uses = base machine platform net plex
telemetry`; the manifest names exactly those six, because no line of the crate names anything
else), holding `auth` and nothing else: the sign-in controller (PIN and QR, discovery of the
account's servers, who's-watching and the profile PIN, the install of the chosen profile's
credentials), `auth::owner` (the owner machine that holds the whole flow and the incident hand-off
to telemetry) and `auth::observation`. The crate keeps `auth` as its one top-level module, so
`crate::auth::` inside the moved files stayed valid and only the 41 application files that named
it changed, by `split-session-rewrite.py` (`crate::auth` to `plx_session::auth`, `pub(crate)` and
`pub(super)` to `pub`). Nine paths moved: `auth.rs`, `auth/`, `auth_test_support.rs` and the six
`auth_*_tests.rs` files that `auth.rs` mounts by `#[path]`. This is the layer that owns sign-in,
the profile PIN and the token hand-off, so what it taught is mostly about the test arms that must
not ship:

- **Every `cfg(test)` arm of the crate was read, not only the four `--report` named.** `--report`
  listed `SessionIdentity::of`, `synthetic_incident`, `settled_probe_for_test` and
  `test_support`; they are `cfg(any(test, feature = "test-support"))` now. What it could not see
  was the rest of the seam, and two of those were security-shaped:
  `ProfileWorkIo::admit` was `cfg(not(test))` *required* and `cfg(test)` *defaulted to
  `EndpointAdmission::Usable`* (a default that admits every endpoint), and
  `scripted::signinfail_spec` returned `None` under `if cfg!(test)`. A dependent's tests build
  `plx_session` without `cfg(test)`, so the first would have stopped the application's
  `ProfileWorkIo` test impls from compiling and the second would have let a trigger file in `/tmp`
  reach the application's tests. Both are `any(test, feature = "test-support")` now, and the
  shipping arm is the one with no default: a shipping build must have every `ProfileWorkIo` say
  explicitly what it admits. There is no `cfg!(test)`, `cfg(not(test))` or `cfg(all(.., not(test)))`
  left in the crate that is not one of these two sites (the rest are private helpers that only
  the crate's own tests call, which stay `cfg(test)`: under `test-support` alone they would be dead
  code).
- **Four scenario functions were exported through a `cfg(test)` module, which no report can see.**
  `owner.rs` ended with `#[cfg(test)] pub(crate) use tests::{late_roster_of_the_seated_profile,
  admin_boot_refresh_of_the_seated_profile, refresh_under_another_accounts_token,
  activation_under_another_accounts_token}`, the session halves of the roster-art scenarios that
  `app/session_roster_art_tests.rs` drives and grades against the poster. They and the helper chain
  they build on (`OwnerHost`, `step`, `roster_refresh_fixture`, `picker_switch_seated`,
  `late_profile_roster`, `member_account_on_admin_seat`) moved out of `mod tests` into
  `auth/owner/scenarios.rs`, `cfg(any(test, feature = "test-support"))`, and the tests import the
  three helpers they still use. `scenarios.rs` is a cut and paste (293 lines under a new header) with the indentation
  removed and `pub` added, so `mod tests` reads the same helpers it always read.
- **Private types in a public test interface.** `test_support::test_policy()` returns
  `ProbeDeadlines`, a private struct; once `test_support` is `pub` that is E0446 under
  `test-support`. `ProbeDeadlines` is `pub` (two `Duration`s, no behaviour) rather than the helper
  being `cfg(test)`d, since `auth_registry_tests` and the discovery tests share it. The
  `cfg_attr(not(test), expect(dead_code))` on `SessionMachine::take_logical_dirty` is gone for the
  reason Split 8 gave: the method is `pub`, so it is never dead and the expectation was unfulfilled.
- **A `cfg(not(feature = "devtriggers"))` test needs the feature forwarded.** Two cases in
  `auth_registry_tests.rs` (`shipping_cold_boot_degrades_gracefully_with_only_a_plaintext_stored_source`
  and `shipping_recovery_repoints_plaintext_metadata_to_https_and_refreshes_normally`) select
  themselves with `cfg(not(feature = "devtriggers"))`. In a crate that does not receive the feature
  the condition is always true, so the two would have run in the default build, where the
  development credential policy is on, and failed. `plx_session/devtriggers` is forwarded from the
  application (and forwards to the six layers below, `plx_telemetry`'s included, so its
  production-credentials guard keeps seeing the feature); `hostsim` is forwarded the same way.
  `cargo test -p plx_session` *alone* built the layers below with the shipping credential policy,
  and `a_changed_refresh_republishes_reached_unauthorized_and_offline_after_registry_replacement`
  failed (it registers plaintext test servers). That configuration did not exist before the split
  either: the application's own tests do not compile with `--no-default-features`. The combination
  gave the crate the workspace rule ("Splits 9 and 10 together"), so alone it builds what the suite
  builds.
- **No version, no build variable, no new hand-in.** The crate has no build script and no `env!`,
  `option_env!` or `include_str!` of the repository. What it reports to plex.tv is `plx_plex`'s
  `identity` (handed the version once, at startup), and the consent hand-off goes through
  `plx_telemetry`'s own API. The tests that read the crate's own source
  (`env!("CARGO_MANIFEST_DIR")` + `src/auth.rs`, and `include_str!("auth.rs")` /
  `include_str!("owner.rs")`) found the same relative layout under `session/`, so they read what
  they read before; they were checked by running, not by assumption.
- **The seams were checked in the artifact.** `cargo tree -e normal -f '{p} [{f}]'` for
  `plxnative-modules` lists no `test-support` on any crate with the default features and with
  `--no-default-features` (`-e normal,dev` listed it 42 times in the lane). In the `plx_session` rlib of a
  `--release --no-default-features` build, `strings` and `nm -C` find none of
  `settled_probe_for_test`, `synthetic_incident`, `late_roster_of_the_seated_profile`,
  `admin_boot_refresh_of_the_seated_profile`, `refresh_under_another_accounts_token`,
  `activation_under_another_accounts_token`, `picker_switch_seated`, `roster_refresh_fixture`,
  `member_account_on_admin_seat`, `late_profile_roster`, `OwnerHost`, `test_support`,
  `cached_session`, `a_two_server_account`, `race_plan`, `threaded_spawn`, `extract_fn_body`,
  `calls_a_function_prefixed` or a `SessionIdentity::of`; the `--features test-support` control
  build finds every one (`status_dial` and `test_policy` are inlined in both and prove nothing).
  The two behaviours that are not symbols (the defaulted `admit` and the silenced `signinfail`
  trigger) are held by the compile: the shipping build compiles only because every `ProfileWorkIo`
  implementation names its own `admit`.
- **Gates that read the old location.** `ci/check-localization.py` listed `auth/owner.rs` among
  the product files it scans, skipped a missing path without a word, and would have stopped reading
  the session owner's read-outs; it reads `session/src/auth/owner.rs` now and follows the crate's
  constants. `ci/check-deps.sh` has `SRC_SESSION` wherever it reads `SRC_TELEMETRY` (the whole-tree
  rules libm, ticks, effect, legacy, dt, frame, spawn, tmppath and the wholly-test-file resolver);
  the eventlog scrub test's root list gained `../session/src` (these files log profile switches and
  PIN outcomes, which is what the scan is for; its `tile.title` exemption is scoped by
  `Path::ends_with("auth.rs")`, which `session/src/auth.rs` still satisfies);
  `tests/test_harness.py`'s `TREE_INPUTS` and `fpflags` list, `tools/cargo-seed.py`,
  `ci/test_no_host_staticlib.py` (`LAYER_CRATES` and the copied `WORKSPACE_FILES`), the
  release-configuration hook and the build-budget `--src` list know the crate. Proven by temporary
  violating edits, reverted byte for byte: a `log(&format!(.., ep_title))` in `session/src/auth/observation.rs` fails
  `no_log_call_site_interpolates_viewing_content` in `plx_base` with the new file's path in the
  message; an `SDL_GetTicks(` there fails `ticks` and a `/tmp/plxnative-` literal in
  `session/src/auth/scripted.rs` fails `tmppath` in `ci/check-deps.sh`; and `use plx_ui::..` in the
  crate fails `ci/check-module-layers.py` (`[session]` may not use `[ui]`). `--report` lists the
  layer as ready and the count of `cfg(test)` items named across layers was 40 after this lane's split
  (44 before it, the four being this layer's; 14 with Split 9 combined).
- **A file mounted under `any(test, feature = "test-support")` is not "wholly test" to the
  `threads` gate.** `ci/rust_test_modules.py` reads a mount as test-only when its `cfg` is false
  with `test` false, and `any(test, feature = "test-support")` is not. `auth_test_support.rs` was a
  `cfg(test)` mount until it had to be `test-support`, and its one `std::thread::spawn` (the probe
  spawner the discovery tests pass in) became a production exception of the gate (red in `make
  check`, declared count 0). It sits in a private inline `mod spawner` under the same `cfg` and is
  re-exported, which is the form the gate reads, as `plx_net`'s `loopback_pms` does; the tool was
  left alone, since reading `feature = "test-support"` as false would also change what the gate
  skips in every other crate.
- **No orphan-rule hazard and no inherent `impl` in a higher layer.** `ProfileWorkIo` and
  `SessionHost` are implemented in the application's tests for the application's own types, which
  is legal; the compiler was the checker (`cargo check --lib --tests` of all ten crates, the
  hostsim set and the lab set passed after the move).
- **Tooling that knows the tree's shape**: the `-p` lists (`... -p plx_telemetry -p plx_data -p plx_session`)
  in the Makefile, the workflow, `tools/build-bench.py` and the tests that pin them, and in the
  docs that quote them; `--src rust-modules/session/src` for the line budget; `RUST_INPUTS`;
  `ci/test_no_host_staticlib.py` holds the crate to `rlib`; and `make build-bench` has a `session`
  scenario ("Edit leaf (plx_session scripted.rs)"; added, not run).

### Splits 9 and 10 together: what the combination needed, and the measurement

The two splits were made in parallel from the same commit (`11584830`, which had not moved when
they were combined) and landed as one change. Combining them:

- **The files both touched are lists again.** 30 files conflicted (the `-p` lists, `SRC_*` roots,
  scrub roots, workspace members, feature forwards, bench scenarios, the pinned tests, the docs that
  quote them, and seven application files in which one lane rewrote `crate::auth` and the other a
  `crate::<data member>` on the same line). The token-level three-way merge resolved all but four
  hunks, three of them prose ("X and data" against "X and session") and one a string needle in
  `stores/mod.rs` that both lanes had edited (the data lane's shorter `auth::request_endpoint_refresh(`
  matches either spelling and was kept). Every merged manifest was read and parses; both rewrite
  scripts were re-run last and changed nothing; `Cargo.lock` was regenerated by cargo and `--locked`
  passes. Neither crate names the other: `data` may not use `session`, and nothing in `auth` names a
  data module.
- **The module-cycle baseline is re-recorded once, on the combined tree.** The cycle did not grow:
  `main` had the 8-module data cycle as its largest and the 6-module media cycle (`abr curlio ff hls
  player route`) among the smaller ones; the eight data modules left the graph, the media cycle is
  the same six, and 14 modules are outside it (21 on `main`, 15 in the data lane alone).
- **One rule for a crate whose own tests need the development feature set: it dev-depends on
  itself.** The lanes had answered the same question two ways. `plx_data` enabled
  `plx_plex/devtriggers` from its dev-dependencies, which made `cargo test -p plx_data` green but in
  a configuration nothing else builds (the plex layer with the dev policy, the data crate without
  its own feature: 422 tests, three missing). `plx_session` could not do that at all: its two
  `cfg(not(feature = "devtriggers"))` shipping-policy cases would have been built against a plex
  layer that has the development policy, so it left `cargo test -p plx_session` failing one test.
  Both manifests now say `plx_<layer> = { path = ".", features = ["devtriggers"] }` under
  `[dev-dependencies]`. Alone, each crate builds itself and every layer below as the Makefile's
  suite does and runs the same tests (425 and 258, twice each, green); `make test-fast T=…` is
  unchanged. A dev-dependency feature reaches test builds only: `cargo tree -e
  features,normal,build --no-default-features` for `plxnative-modules` differs from `main`'s by the
  two new crates and nothing else, with no `devtriggers`, `test-support` or `hostsim` in it. A later
  layer whose tests need the dev set uses the same line; the lower
  crates did not need it, because their tests pass without the feature.
- **The seams were checked in the combined artifact.** `cargo tree -e normal -f '{p} [{f}]'` lists
  no `test-support` with the default features or with `--no-default-features` (`-e normal,dev`
  lists it 49 times). In host release builds of the application (both feature sets) and in the ARM
  shipping archive (`PLX_RELEASE=1 cargo rustc --release --target arm-unknown-linux-gnueabi --lib
  --crate-type staticlib --no-default-features`), `strings` and `nm` find none of `plx_data`'s 22
  seam names (the 21 of Split 9 and `a_source`) and none of `plx_session`'s 20; the
  `--features test-support` control builds of the two crates find 22 of 22 and 19 of 20
  (`test_policy` is inlined). A control has to be built per crate: `-p plx_data -p plx_session
  --features plx_data/test-support,...` from the workspace root built both without the feature and
  found nothing, which reads exactly like a clean shipping build. `browse::a_source`, which the data
  lane moved out of a `cfg(test)`-only module, is `cfg(any(test, feature = "test-support"))` and is
  not in a shipping build. `ProfileWorkIo::admit` and `signinfail_spec` differ from `main` only in
  `test` becoming `any(test, feature = "test-support")`: with the feature off (every normal build)
  the trait method has no default, exactly as on `main`, and the trigger is read under
  `devtriggers` only.
- **The gates still fire in the combination**, proven by temporary violating edits, reverted byte
  for byte: `plx_data` naming `plx_session`, `plx_session` naming `appkit`, and
  `plx_platform::webos`/`::keymanager` named from either crate fail `ci/check-module-layers.py`;
  `plx_data::viewstate::reset(` in `coldstart.rs` fails `viewstate-owner` and
  `plx_data::person::pump(` in a screen fails `person-owner` (the application's spelling of the
  owner rules); `ticks` fires in both crates, `tmppath` in `session/src`, `threads` in `data/src`;
  a `static mut` in `data/src/person.rs` and `data/src/stores`, and a `Mutex` static in
  `data/src/search`, fail `ci/check-statics.sh`; an English literal in `session/src/auth/owner.rs`,
  `data/src/metadata.rs` and `data/src/person.rs` fails `ci/check-localization.py`; and a
  `log(&format!(.., d.title))` in `session/src/auth/owner.rs`, `data/src/person.rs` and
  `data/src/stores/metadata.rs` fails the eventlog scrub scan with the new path in the message.
- **`ci/check-localization.py` fails on a missing path now.** Splits 3, 4, 6, 7, 9 and 10 each moved a file
  or directory it reads, and it skipped what was missing without a word, so each lane had to notice. A listed product file or a
  scanned directory that does not exist is a failure with the path in it; the unit test runs the
  gate on an empty tree and on the real one.
- **The simulator draws the same screens.** `plxnative-sim` against `tests/mock_pms.py` (the seed
  library, the placeholder token, plex.tv replaced by the mock) settles on Home, a library page and
  a detail page; the detail capture is byte-identical to `main`'s and the other two differ in the
  frame-pacing tick only. Sign-in goes through `plx_session` and all three read through `plx_data`.

Measured effect (`make build-bench`, same machine, 3 interleaved runs each, non-incremental, no load
warning in either set; median / minimum). Before, on `11584830`: an edit in `plx_base` 28.9 s /
28.7 s, in `plx_machine` 28.6 / 28.1, in `plx_platform` 27.8 / 27.5, in `plx_gfx` 24.9 / 24.5, in
`plx_net` 25.6 / 25.1, in `plx_ui` 24.2 / 23.6, in `plx_plex` 25.2 / 24.7, in `plx_telemetry` 23.1 /
22.8, of the application crate 22.2 / 22.0, the hub edit 24.1 / 24.1, the unit suite 80.7 / 80.4
with 5630 tests in nine binaries. After: `plx_base` 23.5 / 23.3, `plx_machine` 22.8 / 22.7,
`plx_platform` 22.4 / 22.4, `plx_gfx` 17.0 / 17.0, `plx_net` 20.4 / 20.3, `plx_ui` 16.4 / 16.3,
`plx_plex` 20.0 / 20.0, `plx_telemetry` 18.0 / 17.9, `plx_data` 16.9 / 16.8 and `plx_session` 16.1 /
16.0 (new rows: each rebuilds its crate and the application behind it), the application crate 14.7 /
14.5, the hub edit 16.5 / 16.4, and the unit suite 73.6 / 73.3 with the same 5630 tests in eleven
binaries (157 + 66 + 218 + 94 + 128 + 756 + 458 + 236 + 425 + 258 + 2841, seven of them ignored;
`plx_base` is 156 on CI's Linux, where `paths`' macOS bundle test does not exist). An application
edit is 7.5 s faster and an edit in a layer 5 to 8 s: 54k lines (`data` and `session`) no
longer recompile behind it, and an edit in `gfx` or `ui`, which neither new crate depends on,
rebuilds the application only. The no-op build is 0.1 s in both.

## Limits of the analysis

- `cfg` predicates other than `test` count as possibly on, so the graph is the union of every
  feature configuration.
- Files included from `OUT_DIR` are not read: the generated `i18n::msg` catalog and
  `storage::state`'s install identities (both generated by `plx_platform`'s build script since
  Split 3). Today they name nothing outside their own parent module.
- A `macro_rules!` that is neither `#[macro_export]` nor inside a `#[macro_use]` module is
  visible only to its own module and to children declared after it. The analyzer does not follow
  it; that only matters if `lib.rs` defines one, and it does not.
- The source side is per module, not per item. When an item has to move, the whole file's
  references count against the file's current layer until it does.
- The `cfg(test)` item report sees a name written as a path (`crate::net::clear()`,
  `crate::pms::HubsSnapshot::empty_for_test()`, a `use` of the item), a glob of the item's module
  followed by the bare name, and a `use` of the module followed by `module::name`. It does not see
  a method call, trait dispatch through a `cfg(test)` impl, an associated item called through a
  `use`d type name, or a module imported under another name.
- Impl coherence is not checked at all (see "Then the split").
