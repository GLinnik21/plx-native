# Module layers: getting rust-modules out of one big cycle

Status: target graph declared and gated 2026-10-02, and all fourteen migration steps (L1 to L14)
are done: 0 of the 231 baseline entries remain, so no layer names a layer it may not use. That is
not yet "extractable": 134 `cfg(test)` items were still named from another layer's tests after
L14, which a split hides from them, and the gate does not check impl coherence. Both are below
("Then the split"), and so is the split itself. Step L15, declared after them, fenced the webOS code
off behind a port, so that another TV OS can be a second port. L15 is **gate-complete**: its 44
entries are gone, and the gate fails on any reference from outside the port to a member of it. It
is not done in the sense of its own goal. L15b, the OS-neutral port, is open (below). Neither holds
up the split. **The split has started: `base` is its own crate, `plx_base`** (`rust-modules/base/`,
"Split 1: base" below); the other thirteen layers are still modules of `plxnative-modules`.

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
dev textinput`, platform's `i18n storage webos`, gfx's `gfx gpu_timer ui::overdraw`, plex's `http
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
The six files of the `machine` layer, and `ui/overdraw.rs` in `gfx`, may no longer name the rest
of `ui/`, and the machine files may also not name `gfx`, `text` or `i18n`, which the `ui/` row of
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
`secure::{STORE_FOR_TEST, TestStore}`) that other layers' tests still name, so `--report` now lists
136 (15 in `platform`, the rest as above); run it for the current count. The
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
  `plex::account`'s tests), `ui::machine::{BareArg, BareMeasure}` (from `auth::owner`),
  `gfx::backdrop::commit` (from `ui/frame/backdrop_tests.rs`), and `plex::session`'s `TempSession`,
  `with_io_for_test`, `invalidate_for_test` and `reads_for_test` (from
  `app/plex_session_app_tests.rs`) among them. A `#[cfg(test)]` trait impl is a hazard `--report`
  cannot see, because trait dispatch names nothing: `ui::machine`'s `impl Measure for
  fontcov::advances::ShippedMeasure`, which `auth::owner`'s and the `ui`/`appkit`/`screens` fit
  tests measure through, needs the same feature;
- checks impl coherence by hand. An impl written in a third layer, of one layer's trait for another
  layer's type, is legal in one crate and E0117 (the orphan rule) after the split. `app/recorder.rs`
  had `impl pms::initial::Sink for ui::machine::Canon`; the impl now lives beside the trait in
  `pms/initial.rs`, since `data` may name `machine`. Before extracting a layer, look for
  `impl … for …` in the layers above whose trait and self type both live in other crates;
- watches `#[macro_export]`. `dynlib!` and the `focusable_via_*!` macros keep working through
  `$crate`, but a macro body that names another layer's path needs that layer as a dependency of
  the macro's crate.

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
  in `plx_base` (`ShippedMeasure`'s impl is `ui::machine`'s trait on a `plx_base` type, which is
  legal in the application crate and will be legal in the `machine` crate).
- **The tooling that knew the tree's shape**: `ci/module_graph.py` reads every sibling
  `rust-modules/<layer>/` package named `plx_*` as part of the same module tree (so the layer gate
  and `check-module-cycle` keep one graph and `base uses nothing` is still enforced; the cycle
  baseline shrank by `diag`, which was in the big cycle only through `diag::heartbeat`), the
  `cargo test/check` recipes pass `-p plxnative-modules -p plx_base` (a bare `cargo test --lib`
  would run the application's tests only), `ci/check-deps.sh` and `ci/check-build-budgets.py`
  scan `base/src` as well, `tools/cargo-seed.py` keys on the layer's manifest, and
  `ci/test_no_host_staticlib.py` holds the layer to `rlib`. The remaining layers each need the
  same list walked again; `SRC_BASE` in `ci/check-deps.sh` is where a second crate is added.

Measured effect (`make build-bench`, same machine, 3 interleaved runs, median; the baseline runs
logged a host load above the core count, the later ones did not, so read single seconds as noise):
an edit that only touches the application crate went from 34.9 s (the old one-crate "leaf" edit)
to 31.9 s with `plx_base` fresh, an edit inside `plx_base` costs 33.2 s (it rebuilds the layer and
then the application behind it), the hub edit 35.5 s to 34.2 s, and the unit suite 59.8 s to 61.8 s
with the same 5603 tests. That is the expected size: `base` is 6.7k of 450k lines, so the split
buys about the 2-3 s that crate cost per edit. The leverage is in the layers above it, which are
the other crates' worth of lines an edit stops recompiling.

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
- The `cfg(test)` item report sees a name written as a path (`crate::net::clear()`,
  `crate::pms::HubsSnapshot::empty_for_test()`, a `use` of the item), a glob of the item's module
  followed by the bare name, and a `use` of the module followed by `module::name`. It does not see
  a method call, trait dispatch through a `cfg(test)` impl, an associated item called through a
  `use`d type name, or a module imported under another name.
- Impl coherence is not checked at all (see "Then the split").
