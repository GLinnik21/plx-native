# Module layers: getting rust-modules out of one big cycle

Status: target graph declared and gated 2026-10-02, and all fourteen migration steps (L1 to L14)
are done: 0 of the 231 baseline entries remain, so no layer names a layer it may not use. That is
not yet "extractable": 134 `cfg(test)` items were still named from another layer's tests after
L14, which a split hides from them, and the gate does not check impl coherence. Both are below
("Then the split"), and so is the split itself. Step L15, declared after them, fenced the webOS code
off behind a port, so that another TV OS can be a second port. L15 is **gate-complete**: its 44
entries are gone, and the gate fails on any reference from outside the port to a member of it. It
is not done in the sense of its own goal. L15b, the OS-neutral port, is open (below). Neither holds
up the split. **The split has started: `base`, `machine`, `platform`, `gfx` and `net` are their own
crates, `plx_base`, `plx_machine`, `plx_platform`, `plx_gfx` and `plx_net`** (`rust-modules/base/`,
`machine/`, `platform/`, `gfx/` and `net/`; "Split 1: base", "Split 2: machine", "Split 3:
platform", "Split 4 (gfx)" and "Split 5 (net)" below); the other nine layers are still modules of
`plxnative-modules`.

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
`ui/CLAUDE.md` and `screens/CLAUDE.md`). `ui` names no application type, `screens` never names
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
modules (44 at baseline), and `ci/module-cycle-baseline.json` has the current set. It sees `ui` and
`diag` as one node each, so the machine-layer and
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
(`gfx`, `plex`, `ui`, `screens`) are still inside the application crate.

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
