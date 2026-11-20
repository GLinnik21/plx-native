# Platform ports: running PlxNative on more than webOS

Status: approved 2026-11-20 and recorded here (workstream W0). This is a plan, not a description of the tree: a
workstream is done when its PR has landed. The owner's decisions are at the end of the page and the body follows
them; no owner question is open. The waves, each lane's files, its bounded tests and its television and
build-bench flags are in "Workstreams". The layer-graph side of the plan is `docs/module-layers.md`, steps L15c to
L15h. Unprefixed ids are this page's own: F facts, T Tizen research, E emulator evidence, H hello-world questions and
W workstreams.

The sections "Verified facts" (F1-F52), "Tizen research" (T1-T8) and "Emulator run, 2026-11-20" (E1-E13) quote the
tree at `e9924f0f9`, before the bottom-layer rename (Wave R), so they keep the old names and the old line numbers;
"Names and paths" gives the map. Every other section uses the final names (Wave R's own text names the old ones it
replaces), and "today", "now" and "as today" anywhere on the page mean that tree. Hello-world refs
(`tizen/hello/...`) pin to `origin/samsung/hello-tpk` at `72c5cd82a`, except the H5 result (E10), which pins to
`64c609a46`, the tester build of E11, `1afe1978b`, and the probe build of E13, `e8da14ce6`.

## History

The plan was written by a planner and revised over six rounds with a critic and after two independent reviews of its
drafts; the critic approved it on 2026-11-20. Finding ids in the body: review B1-B3, M4-M11, C1-C10 (the two reviews);
critic M1-M12, F1-F12, G1-G8, N1-N5 (its passes, in order); "critic round N, point M" (earlier rounds). Every finding
was accepted unless an owner decision of 2026-11-20 overrode it, and the body says where it landed; the changelog and
review texts are not kept, and no section depends on a revision number.

## Names and paths

Wave R renames the bottom layer first, so the design below uses the final names: crate `plx_os` (directory
`rust-modules/os`, layer `[os]`, inner module `tv` unchanged), fence sections `[platform <name>]`
(was `[port <name>]`), platform crates `plx_platform_webos`, `plx_platform_desktop`, `plx_platform_tizen`.
Verified facts F1-F52, T1-T8 and E1-E13 quote the tree at `e9924f0f9`, before the rename, where the old names
stand, so every grep in them reproduces: read `plx_platform` as `plx_os`, `rust-modules/platform/` as
`rust-modules/os/`, `[platform]` as `[os]` and `[port X]` as `[platform X]`; the bare path `platform/src/...`
(relative to `rust-modules/`, as `ci/list-caps.ini` and the certificates spell it) is `os/src/...`, and the shell
variable `SRC_PLATFORM` is `SRC_OS`. Every `path:line` citation on this page is a line of the `e9924f0f9` tree, in
the target-shape and workstream sections as well as the fact sections: the rename can reorder `use` lines and later
changes move the rest, so a lane re-verifies a citation by grep on the symbol, and every brief carries this map.
Layer rules quoted from `ci/module-layers.ini` are quoted at `e9924f0f9` (`[play_uri] uses = base platform
playback_contract`, `:148`): after Wave R the same line reads `uses = base os playback_contract`.

## Problem

PlxNative is one Rust application (17 layer crates, the 13 named in `docs/module-layers.md:12-17` plus playback_contract, media_codecs, play_uri and play_feed, under the top crate `plxnative-modules`) that
reaches the device through `plx_os::tv`, a function-pointer table filled once at boot by the
webOS port (`port::plex_run`). Today there is exactly one implementation of that table. The desktop
simulator and `PlxNative.app` install the SAME webOS table and take the `hostsim` arms inside the
webOS code. The interfaces are also still webOS-shaped: a 29-verb Starfish sink, ACB plane verbs,
an LG key-manager enum in the on-disk envelope, SDL-event bytes in the loop.

The owner wants the app runnable and pluggable on several platforms:
- webOS (shipping, must keep shipping at every step);
- Samsung Tizen through a C# .NET `.tpk` host that P/Invokes our native library;
- the macOS and Windows/Linux simulators and the macOS app as real platforms of their own;
- later possibly iOS/tvOS and Android (no Flutter path: owner decision).

This plan gives:
- the target crate and layer shape and the platform interfaces: window/surface, events/input,
  lifecycle, key store, identity/caps, video sink and playback backends, entry point;
- what each platform supplies;
- a PLATFORM build axis next to FLAVOR, with packaging and CI;
- what moves where, and an ordered multi-PR migration from the `[platform webos]` fence (`[port webos]` before
  Wave R) and the open L15b step of `docs/module-layers.md`.

Every step keeps three rules: layers point only down, machines stay Sans-IO, and the ledgers only
shrink.

## Verified facts

Citations in this section quote the tree at e9924f0f9 with its old names (see "Names and paths").

### The fence and the gate

- F1. `[port webos]` has members `webos keymanager system port` (`ci/module-layers.ini:191-202`, last
  line `members = webos keymanager system port`). The gate checks ports generically:
  - a member listed in two ports is an error (`ci/check-module-layers.py:81-82`);
  - a reference passes only when source and target are in the SAME port (`:139-140`, `if source is
    not None and source[1] == target[1]: continue`).

  So a second or third `[port X]` section needs no gate code. Anything two ports share must be a
  LAYER, not a port member. That holds for another section under today's keyword. Renaming the
  keyword to `platform` (the owner's naming) is gate code: the parser keys on
  `startswith('port ')` (`ci/check-module-layers.py:68,74`), and a layer `[platform]` already
  exists (`ci/module-layers.ini:53`) (critic M9; F36). Wave R renames that layer (to `os`) and then the keyword, first and alone.
- F2. `port` and `system` are members of `[app]` (the `[app]` members line ends `system release_line
  port`). `webos` and `keymanager` are members of `[platform]`. Port members stay placed in their
  layers (`ci/check-module-layers.py:14-15`). A port is "Not a layer" (ini comment under `[port
  webos]`).
- F3. The design record:
  - The port crate waits: "the `[port webos]` members stay where they are ... **until a second TV OS
    exists.**" (`docs/module-layers.md:21-27`, the quoted words at `:24`).
  - "L15b is done when the sink is OS-neutral, the simulator is its own port, and these facts are the
    port's. The port can then leave its layers for a crate of its own on top" (`:434-436`). So the
    record orders crate extraction AFTER all of L15b.
  - The three L15b items are: item 1 (OS-neutral sink, needs the television) at `:407-412`, item 2
    (simulator as its own port, plus neutral `tv::secure::Backend` and `tv::sandbox`) at `:413-418`,
    and item 3 (webOS facts the gate cannot see) at `:419-433`.
- F4. `ci/allow/layers.txt` is empty and only shrinks (`docs/module-layers.md:30-34`). The ledger
  ratchet compares every `ci/allow/` counter with the BASE branch. A family directory
  (`workers/ globals/ forks/ fxlanes/`) is one total, "so an entry may move between the area files of
  a family" (`ci/check-ledger-ratchet.py` docstring). No `ci/allow` file names `webos.rs`,
  `keymanager`, `port.rs` or `system.rs` as an entry. `grep -n 'webos\|keymanager\|system\.rs\|port\.rs'
  ci/allow/...` hits only explanatory text: `wall.txt:7` names `app/window_activity.rs` "out of
  system.rs".

### The port table

- F5. `plx_platform::tv::Port` (`rust-modules/platform/src/tv.rs:21-55`) has 27 fields in one flat
  table:
  - 11 identity, caps, sandbox, store, locale, sound, home and toast entries;
  - 7 window, surface, opaque-region and bus entries (`bind_window` .. `pump_bus`, `:33-40`);
  - 6 `frame_probe_*` entries;
  - `sink`, `display_sink` and `uri_media`.

  `ABSENT` holds the no-port defaults (`:62-90`). `install` uses a `OnceLock` (`:92-95`), and only
  `tv`'s own modules read the table (`:100`).
- F6. The webOS table is `rust-modules/src/port.rs:25-53`. `plex_run` installs it, then calls
  `install_activator(webos::activate_storage_helper)` under `cfg(target_os = "linux")`, then
  `app::run_application` (`:57-63`). Under `hostsim` the same table points its sink at `HOST_SINK`
  (`:11-12`).
- F7. `src/bin/sim.rs` enters through `use plxnative_modules::port::plex_run;`.
- F8. The interfaces in `rust-modules/platform/src/tv/` total 3127 lines (`wc -l`):
  - `plane.rs` (1598) is "The ACB video-plane owner" (`:1`). It is ACB semantics, and it is named from
    BELOW `app`: `grep -rln 'tv::plane\|PlaneOwner'` hits these files:
    - `play-uri/src/{backend,picture_gate,uri_lifecycle,acb_drive,uriprobe_drive}.rs`;
    - `play-feed/src/{control,ffi_host,feed_pump,native_executor}.rs`;
    - `media/src/player/{mod,pump,engine,uriprobe_drive,test_physical}.rs`.
  - `sink.rs` (484): `DisplaySink` at `:85` (window create/place/destroy plus the `plane_*` ACB verbs)
    and `VideoSink: DisplaySink` at `:155`.
  - `uri.rs` (156): `UriMedia` at `:71`. `tv::uri`/`UriMedia` is named in `play-uri` (7 files),
    `platform` (4) and `media` (1).
  - The app crate also names the plane owner: `src/app/adapters/backends.rs:25-31` (`use
    plx_platform::tv::plane::{self, PlaneFacts, PlaneOwner};` and three more), plus
    `src/app/run_uri_boot_tests.rs` and `src/dev/uriprobe_tests.rs` (critic round 1, point 1).

### `hostsim` and the television cfgs

- F9. There are 465 `feature = "hostsim"` lines. Per crate (`grep -rn 'feature = "hostsim"' --include='*.rs' <dir> | wc -l`):
  - src 232, media 71, platform 58, gfx 30, telemetry 28, plex 21, base 16, play-feed 9;
  - 0 in ui, appkit, screens, data, session, net, machine, play-uri and playback-contract.
- F10. Of those, 102 are inside the fence members (`grep -c` per file):
  - `system.rs` 53, `webos.rs` 22, `webos/caps.rs` 8, `port.rs` 7;
  - `webos/toast.rs` 5, `webos/jail_repair.rs` 5, `keymanager.rs` 2, `webos/video_sink.rs` 0.

  The other 363 `feature = "hostsim"` lines are outside, so L15b item 2 alone does not retire `hostsim`. Of
  those 363, 360 are cfg lines and three are doc comments (`base/src/paths.rs:1347`,
  `src/app/session_picker_regression_tests.rs:326`, `src/app/surface_navigation_tests.rs:4`; `git grep -n
  'feature = "hostsim"' -- '*.rs' | grep -E ':[0-9]+:\s*//'` finds them).
- F11. `hostsim` is forwarded to ten crates (`rust-modules/Cargo.toml:179`). Its comment relies on
  the app crate being "a leaf with no dependents, so feature unification can never enable it behind
  someone's back". `make check` DOES resolve the whole workspace at once:
  - `lint` runs `cargo +RUST_TOOLCHAIN clippy --workspace --all-targets` (`Makefile:2094`);
  - `Makefile:1682` (`check --lib --tests -p plxnative-modules -p plx_base ...`) and `:1776` (`test
    --lib -p ...`) resolve many packages in one invocation.

  So a workspace member that depends on the app crate WITH `hostsim` would switch `hostsim` on for the
  whole lint pass, and the television arms would stop being linted. W7 states the mechanism that
  prevents it.
- F12. "Television" is also spelled `target_arch = "arm"`, in 20 files under `rust-modules`
  (`grep -rln 'target_arch = "arm"' --include='*.rs' rust-modules | grep -v /target | wc -l`). They
  include `platform/src/{webos.rs, webos/caps.rs, storage/mod.rs, storage_service/*}`,
  `plex/src/plex/session*.rs`, `telemetry/*`, `src/lib.rs`, `src/app/boot.rs`, four `build.rs`,
  `storage/src/main.rs` and `build_support/host_link.rs`.

  `target_os = "linux"` is 151 lines in 27 files (`grep -rn 'target_os = "linux"' --include='*.rs'
  rust-modules | grep -v /target`). Per directory:
  - platform 45, telemetry 41, plex 21, src 20, base 11, storage 8, play-feed 5;
  - the largest files are telemetry/persistence.rs 23, plex/session/persistence.rs 15,
    storage_service/runtime.rs 12, src/app/run.rs 10, and 9 each in base/task.rs, storage/mod.rs,
    storage/client.rs and telemetry/native.rs (critic round 1, point 3; per-directory sums checked).

  A Tizen ARM build is `linux` + `arm` + not `hostsim`, so today it would compile every webOS arm.
- F13. `target_os = "android"` appears 8 times: 5 in `base/src/task.rs` and 3 in
  `telemetry/src/telemetry/crashreport.rs`.

### SDL, input and the loop

- F14. The app crate calls SDL directly:
  - the extern block is in `src/app/mod.rs:154-175`;
  - `SDL_Init` is at `app/boot.rs:1547`, `SDL_CreateWindow` at `:1648` and `SDL_GL_CreateContext` at
    `:1652`;
  - the swap is at `app/run.rs:974` and the poll at `app/run.rs:1066-1069`.

  Source-text pins search for those calls: tests at `app/boot.rs:705-785` look for `"SDL_Init("` and
  `"SDL_CreateWindow("`, and `app/run.rs:11467` looks for `"SDL_GL_SwapWindow(app.win);"`. W6 moved the extern block,
  the window, the swap and the poll into `plx_shell_sdl` (`sys.rs`, `window.rs`, `event.rs`); the application crate declares
  SDL functions in two files only now, `system.rs` (the `SDL_SysWMinfo` handshake and the GL attribute read) and
  `app/clock.rs` (`SDL_GetTicks`), and its swap pin reads `app.surface.swap();`.
- F15. SDL is also below `app`:
  - `base/src/surface.rs:38-39`;
  - `base/src/diag/heartbeat.rs:30-31`;
  - `gfx/src/gpu_timer.rs:89`;
  - `gfx/src/text.rs:220`, with `TTF_` in `gfx/src/text.rs` and `base/src/fontcov.rs`.

  Labels rasterize through SDL2_ttf (the `src/ass_text.c` header reads "The application draws its
  labels with the television's SDL2_ttf 2.0.x"). The link line is `LIBS_REAL = -lSDL2 -lSDL2_ttf
  -lGLESv2 -lluna-service2 -lglib-2.0 -lwayland-client` (`Makefile:404-405`).
- F16. Events are decoded from raw SDL bytes in two layouts. "LG's fork writes `state` (u32) at +16,
  the webOS keycode at +20 and the SDL sym at +24. Stock SDL2 ... every field the app reads is at a
  different offset" (`app/events.rs:24-34`), and `decode_key` switches on `cfg!(feature = "hostsim")`
  (`:84-86`). Lifecycle arrives as SDL app events: `0x103|0x104` (background), `0x106` (restore) and
  `SDL_QUIT` (`app/run.rs:1194-1196, 1239-1242`). The recorder encodes them by SDL code:
  `app.rec.input(super::recorder::enc_lifecycle(et))` (`app/run.rs:1236-1237`).
- F17. There are two key vocabularies, and the raw codes are part of the contract:
  - `plx_machine::machine::Key { Up, Down, Left, Right, Ok, Back, Other }`
    (`machine/src/machine.rs:867-875`) and `InputKind<K>` with `Key { key, sym, wcode, edge, at_edge
    }`, `Pointer`, `Click`, `Drag` and `Wheel` (`:878-905`). The app builds these at
    `src/app/bridge.rs:5471-5561`.
  - `plx_ui::consts::Key` (`ui/src/consts.rs:269`) comes from `classify(sym, wcode)` (`:322`), with
    `SDLK_*` (e.g. `:137`) and LG `WCODE_*` (`:184-190`).

  `consts.rs` is NOT movable as a whole. It names `plx_machine::machine::Key` (`:380`
  `classify_input`), `plx_platform::labcfg::is_trigger_key` (`:484`), and `crate::card_row`/`crate::Rect`
  (`:52-113`).

  "`Other` does NOT mean unbound" (`consts.rs:262-267`): the CH rocker, `SDLK_PAGE*`,
  `SDLK_BACKSPACE`/`SDLK_CLEAR` and the PIN digits are read from the raw sym/wcode. These files name
  `WCODE_`/`classify(`:
  - `screens/src/{login,hud_settings,detail/trailer,player/*}.rs`;
  - `src/app/{events,bridge,run,mod,adapters/huds,adapters/poster,adapters/session}.rs`;
  - `src/app/adapters/poster/{refresh,baked_tests}.rs`.

  Target shape 2 and F33 (review M6) change what crosses the platform edge.
- F18. `MainThread` is "Proof that the holder runs on the SDL main thread" (`base/src/task.rs:105`).
  It is minted in production at `src/app/mod.rs:635` (`let main_thread = unsafe {
  plx_base::task::MainThread::assume() };`), inside the app's boot path, not in `port.rs`. Its
  mechanism is a `!Send + !Sync` ZST (`task.rs:107-109`). So the token tracks the thread that runs
  the boot and loop, whichever OS thread that is.

### C sources

- F19. `src/*.c` has 12 files (`ls src/*.c`, header lines read):
  - `main.c` (195), the webOS executable shim. It runs the Sentry spool re-entry (`:162`), the
    event, crash and stderr descriptors with `dup2` (`:163-187`), `plx_crash_install` (`:188`),
    `plx_launch_args(argv[1])` (`:191`) and `setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", ...)`
    (`:193`), then `plex_run`.
  - `access.c` and `exit.c`: "the executable's own definition of libc `access`" and "`exit`".
  - `crashtrace.c`: the signal tracer.
  - `sentry_context.c`.
  - `starfish.c` (602): the Feed seam.
  - `video_plane.c` (418): ACB on 2.2.3-4.10, the SDL exported window on 5.3.1-11.2 (`:14-37`).
  - `svg.c`: nanosvg.
  - `ass.c`, `ass_fonts.c` and `ass_text.c`: compiled into libass-plx.
  - `gpdebug.c`: debug only.

### Playback

- F20. A backend-neutral contract exists in `plx_playback_contract`:
  - `DriverSet` (`playback-contract/src/lib.rs:276-283`): `capabilities`, `dispatch`, `poll`,
    `discard_staged` and `exit_disposition`;
  - `UriDriver: DriverSet` (`uri.rs:60-81`): `stage_source`, `cancel_staged`, `plane_owned`,
    `maps_track` and `ready_to_start`.

  `UriNotReady` has the single variant `AcbHandle` (`uri.rs:84-88`). The contract is NOT clean of
  webOS names: F31 lists the types and result variants that carry ACB (critic M2). The composition is `compose_backends` (`app/adapters/backends.rs:141-164`),
  with `acb_init` at `:64`.
- F21. `play-uri` is the webOS URI driver. Its files include `acb_bind.rs`, `acb_drive.rs` and
  `acb_envelope.rs`, and its layer uses `base platform playback_contract` (`ci/module-layers.ini`
  `[play_uri]`). Production routes choose Feed: the qualifier "returns Unknown ... so ordinary routes
  still choose Feed" (`media/src/player/CLAUDE.md:25-33`). The opt-in reaches the planner as a
  snapshot in `ResolveEnv::uri_engine` (same file, `:50-53`). `ResolveEnv` is in
  `media/src/route/decision.rs` (`grep -rln`).
- F22. The Feed bind stages are in `pump`'s `Stage` machine, and no host test covers them
  (`docs/module-layers.md:375-385`).

### Build and packaging

- F23. The Makefile is webOS-only:
  - `WEBOS_SDK` (`Makefile:246`), `TOOLPREFIX` (`:261`), `CC = ci/arm-cc.py` (`:262`) and `SYSROOT`
    (`:265`);
  - `RUST_TARGET = armv7-unknown-linux-gnueabi` (`:881`), and `RUST_LIB` names
    `libplxnative_modules.a` (`:882`).

  `FLAVOR ?= debug` (`:128`) is validated by `ci/flavor.py --list` (`:126`) and is install identity
  (APPID `:135`, RUNDIR `:155`). `PLAYBACK_BACKENDS ?= both` maps to `play-uri`/`play-feed`
  (`:565-577`). Other targets: `ipk:` at `:2219`, `sim-macos:` at `:2521`, `sim-linux:` at `:2573`,
  `sim-wsl: sim-linux` at `:2595` and `macapp:` at `:2668`. There is no PLATFORM variable: `grep -n
  '^PLATFORM' Makefile` printed nothing. The query targets are explicit `print-flavor:`,
  `print-appid:`, ... (`Makefile:189-207`), and there is no `print-%` pattern rule (`grep -n
  '^print-[A-Za-z%_-]*:'`).
- F24. The app crate is `crate-type = ["rlib"]` (`rust-modules/Cargo.toml:61`), and the ARM archive
  comes from `cargo rustc --crate-type staticlib`. `ci/test_no_host_staticlib.py` fails a non-rlib
  crate type on the app crate (its docstring).

### Path-pinned checks, run-time names, identity

- F25. These name the moving files (`grep -rln` over `ci tools tests .github`):
  - the `sink` rule in `ci/check-deps.sh:17,1119-1131`;
  - `ci/test_sim_backend.py`, `ci/mkmacapp.py:8,129`, `ci/test_test_crate.py:214` and
    `.github/workflows/ci.yml:379`;
  - `ci/allow/wall.txt` and `ci/allow/forks/play-feed.txt`;
  - `tests/test_harness.py` and `tests/fixtures/persistence/generated/generators.patch`.

  `ci/proofs/platform-generated-enums.json` with `ci/readonly_generated.py` pins 8 sources by sha256
  (`SOURCES`, `ci/readonly_generated.py:17-19`):
  - `platform/Cargo.toml`, `platform/src/lib.rs`, `platform/src/i18n/mod.rs`, `platform/build.rs`;
  - `platform/build_support/catalog.rs`, `build_support/install_identities.rs`;
  - `machine/src/lib.rs`, `machine/src/machine.rs`.

  Its docstring reads "certificate changes require source review". It pins `CERTIFICATE_SHA256` and
  per-file hash lists in code, and no regenerating tool exists. So an edit to any of the 8 is a
  REVIEWED certificate change in the same PR. `platform/src/lib.rs:26` is `pub mod keymanager;` and
  `:33` is `pub mod webos;`. A new `tv::*` file is declared in `tv.rs` and does not touch the pin.

  `ci/check-staticlib-artifact.py` has `CRATE = "plxnative_modules"` and requires exactly one `.a` for
  that crate at the path make links (critic round 1, point 8). `ci/build-budgets.json` budgets the
  stripped ARM binary. `Makefile` itself is a prerequisite of `RUST_LIB` (`Makefile:1017`), and many
  `ci/test_*.py` read Makefile text (critic: 27 files; not recounted).
- F26. `plex::identity::PLATFORM` is `"webOS"` (`plex/src/plex/identity.rs:86`), sent as
  `X-Plex-Platform` (`plex/src/plex/client.rs:307`, `account.rs:443`).
- F27. The run-time library name lists:
  - `EGL_SONAMES: &[&str] = &["libEGLfk.so.2", "libEGL.so.1", "libEGL.so"]` (`gfx/src/egl.rs:110`);
  - libcurl's `["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"]`
    (`net/src/net/connect_only.rs:14`).
- F28. Branch `samsung/hello-tpk` now carries the hello-world. `git log --oneline
  origin/main..origin/samsung/hello-tpk` lists `72c5cd82a` ("tizen/hello: sign with a Samsung
  certificate profile, stream the log over UDP"), `ab0ba40ca` ("tizen/hello: log without eprint, ...")
  and `f51d0db87` ("Samsung Tizen TV step 1: hello-world .tpk (throwaway test build)"). It is
  `tizen/hello/`: a standalone Rust cdylib (critic: `crate-type = ["cdylib"]`, only `libc`, its own
  `[workspace]`) plus a C# `CoreUIApplication` host. Its README (`git show
  samsung/hello-tpk:tizen/hello/README.md`, `:8`, `:19-21`) says:
  - "It answers four questions": host starts, `.so` loads, window draws, keys arrive;
  - "`OnCreate` calls the library once on the main thread; the library creates an Ecore_Wl2 window
    there (Ecore's main loop runs on that thread), registers key handlers there, and a separate Rust
    thread owns the EGL context and draws."

  That is window Option B with GL on a non-main thread. It links no SDL2/SDL2_ttf and plays no
  media. Since `72c5cd82a` it signs only with a Samsung certificate profile (E2) and streams every log line
  over UDP (E5); the emulator run (E1-E9) used that build.

### Evidence from the first reviews: F29-F39 (each citation checked in the tree)

- F29. The process end (review B1).
  - `run_application` is `-> !` (`rust-modules/src/app/mod.rs:642`). An early-boot refusal calls
    `exit_now(code)` (`:645`). A played session ends in `run_and_shutdown -> !` (`:561`): `run::shutdown`
    (`:572`), the telemetry guard drop, then `exit_now(code)` (`:576`).
  - `exit_now` is `fflush(NULL)` then `_exit` (`base/src/process.rs:30`). Its module doc (`:1-10`) says
    why not `exit`: OpenSSL's `atexit` cleanup frees locks under the detached curl workers
    (notification feed, metadata, watchlist, auth probes), a SIGSEGV on a clean exit (#1115), and the
    firmware libraries' finalizers run under a worker. `src/exit.c` interposes `exit` only in the TV
    executable (F19), so a host-loaded `.so` has no such guard. `src/bin/sim.rs:149-152` already says
    `plex_run` ends the process itself.
  - The hello-world leaves teardown to process exit when its render thread is still busy after 2 s
    (`origin/samsung/hello-tpk:tizen/hello/plx/src/lib.rs:79-80, 342-365` at `72c5cd82a`: `terminate`
    starts at `:342` and the timeout message is at `:359`; `:339-358` and `:339` were the earlier commit's
    lines).
- F30. Session persistence (review B2).
  - `save_legacy_fallback_locked` (`plex/src/plex/session.rs:4826`) is reached when the canonical
    write fails (`:4777`). It tries `tv::secure::seal` (`:4837`). With no protected record and a seal
    that answers `None`, it serializes the session and writes it to each candidate path in turn
    (`:4872-4882`): a plaintext credential file. So `secure::NONE` (seal answers `None`) does not give
    an ephemeral sign-in. An earlier draft of W3 and W9 said it did; that was wrong.
  - The session file has four writers behind one door, and `peek()` serves a cache filled by reads
    and durable writes (`plex/src/plex/CLAUDE.md`). No memory-only mode exists:
    `session/memory_tests.rs` is the preference "memory door", unrelated.
- F31. URI admission and the shared contract (review B3, critic M1, M2, review M8).
  - `Binding { Exported{canvas}, Acb, Unbound }` is in the shared contract (`playback-contract/src/uri.rs:13`).
    `ProductEvidence::unqualified_on` compares the display mode with the LG constant
    `tv::sink::VP_EXPORTED` (`media/src/route/uri_preflight.rs:45`). `display_path_offerable`
    (`:76-82`) is false for `Unbound`, true for `Acb`, and true for `Exported` only with
    `display_exported` and a positive canvas. `permits` (`:84-91`) needs that plus callable and
    certified pause and seek and the requirement flags. `certified` is all false until the viewer's
    opt-in (`opted_in`, `:55-69`). A Tizen URI driver today would have to fake `VP_EXPORTED`, a
    canvas and an opt-in. The composition builds that evidence at `app/adapters/backends.rs:386`.
  - `BackendSet` is already "the set a composition's installed drivers form"
    (`playback-contract/src/lib.rs:34-48`). `compose_backends` ends with
    `session.install_backends(adapter.composed_backends())` (`backends.rs:163`). The session starts
    at `BackendSet::FEED` (`media/src/route/decision.rs:387,1686`). So `default_backend` repeats a
    fact the set already holds. The recipe table already carries `URI_PMS_*` conversions
    (`media/src/route/backend_policy.rs:130-155`).
  - ACB is in contract RESULT types too: `Accepted { resume_acb }` (`playback-contract/src/feed.rs:385`),
    `Played { resume_acb }` (`:416`), `AcbPlaneUnload` (`:1115`), `UriNotReady::AcbHandle`
    (`uri.rs:84-88`). The URI-side one goes in W3b (F45: three files name it, nothing pins its text),
    before Tizen's driver. The Feed-side ones are debt that exits in W11a, because Tizen has no Feed
    before W12.
  - `tv::sink` (`sink.rs:85-197`) and `tv::uri` (`uri.rs:1,71-76`) are Starfish, ACB and
    `com.webos.media` verbs, read by shared `media` through globals (`media/src/player/mod.rs:97,1676`).
    A fake URI driver exists: `playback-contract/src/test_support.rs:143` (`FakeUriDriver`).
- F32. Boot order and the thread token (review M4, M5).
  - `acb_init(&mt)` runs at `app/boot.rs:2043` inside `if !controlled`, before
    `plx_media_codecs::ffi::boot()`. `PlayerAdapter::new(mt)` consumes the token (`:2316`) and
    `compose_backends` runs later (`:2323`, every build). So `acb_init` is an early phase and
    composition a late one. The review cited `:2042` (the `if`) and `:2311` (a comment).
  - `MainThread` is the struct at `base/src/task.rs:129`. The "this is the SDL main thread" safety
    text is on `assume` (`:138`), which `app/mod.rs:635` calls on whichever thread runs the boot.
  - The hello-world creates the window and registers key handlers on the host main thread inside
    `plx_tizen_create` (`lib.rs:126`; window `:229`, handlers `:235`) and starts a separate render
    thread (`:251`). The emulator run exercised it (E6, E8).
- F33. Keys as the code has them (review M6, critic M7).
  - `plx_machine::machine::Key` has seven variants (F17). `InputKind::Key` carries `key`, `sym`,
    `wcode`, `edge`, `at_edge` (`machine/src/machine.rs:878-891`), and `InputKind::Wheel { dy }` exists
    (`:903`). The tape canon writes `sym` and `wcode` (`:816-819`). `machine.rs` is hash-pinned (F25),
    so changing the machine's key type is a certificate review plus a tape-format change.
  - The wheel delta is read at a different offset per host: an `i32` at +20 on the TV, a float at +32
    under `hostsim` (`app/run.rs:1855-1877`), so the pointer event needs a wheel member.
  - `classify` takes Pause and Play from `wcode` alone but their alternates and Stop from either field,
    and `SDLK_SELECT` is really END (`ui/src/consts.rs:137-145, 322-375`). `classify_input` trusts the
    machine's canonical key first (`:380`). `classify` alone is `Other` for paging (`page_dir`, `:433`),
    digits (`screens/src/profiles.rs:340`) and the edit keys, so a decode guard needs all four readers.
  - Readers of the raw pair: `WCODE_` or `classify(` appears in 7 files under `screens/src`
    (`grep -rlE 'WCODE_|classify\(' screens/src`); a looser pattern (`sym`/`wcode` in any spelling)
    gives 49. The critic's "16 screens files" did not reproduce; an earlier F17 list had 16 names across
    `screens` and `src/app`.
  - `remote_token_key` (`app/events.rs:211-246`) names 19 tokens plus `k:<sym>,<wcode>`.
- F34. Who calls the shell and the frame probes (critic M4).
  - Every `tv::window::` caller is in the app crate: `src/app/boot.rs:1460-1729`, `bridge.rs:35,3374-3377`
    and `run.rs:173-11199` (the critic listed boot and run, not `bridge.rs`; hits past `run.rs:8200` are in or
    beside test modules and were not classified one by one). The dispatcher reaches them through a rig
    trait that `app/run.rs` implements (`ui/src/dispatch.rs:728-730`). `ci/check-deps.sh:1097` (the `frame`
    gate) pins the call text `tv::window::(pump_bus|opaque_route|clear_opaque_region)(` to `run.rs`.
- F35. The hostsim gate's package list (review M11).
  - `check-cargo-unit-hostsim` (`Makefile:1764`) names eleven packages inline (`:1776`: `plxnative-modules
    plx_base plx_machine plx_platform plx_gfx plx_plex plx_telemetry plx_data plx_session plx_media
    plx_play_feed`), the same eleven as `HOSTSIM_SUITE` (`:1713`). `HOSTSIM_PKGS` (`:1859`) is only
    the developer's `make test-hostsim` list. An earlier draft named the second as the gate's.
  - The app's lifecycle suite is `#[cfg(all(test, feature = "hostsim", feature = "play-feed"))] mod
    lifecycle_regression_tests` (`app/run.rs:3914-3915`).
- F36. Naming (critic M9) and the size of Wave R. `plx_platform` is the bottom-layer crate
  (`rust-modules/Cargo.toml:231`, `ci/module-layers.ini:53,64`). Counted: 3280 lines (3438
  occurrences) in 296 `.rs` files, 12 `Cargo.toml` files plus `Cargo.lock`, the nested `CLAUDE.md` files
  (`ui/src`, `media/src/player`), and 26 files under `ci/ tools/ tests/ .github/ Makefile docs/`
  (`grep -rIlw plx_platform`). Two readonly certificates name the package or its paths and pin every member
  manifest and `Cargo.lock` (F47: both pin every member manifest and `Cargo.lock`, not only the crate's
  `Cargo.toml` and `lib.rs`; `ci/readonly_generated.py:60` is the package field, that certificate's own hash is at
  `:16`). The layer holds more than `tv` (`webos storage keymanager devcaps imgcache i18n labcfg tv playback_trace`,
  `ci/module-layers.ini:53-64`). 25 tracked files name the path `rust-modules/platform/` (`git grep -Il`; F48 adds the
  bare and assembled spellings), among them `.agents/agents/fw-compat-reviewer.md`, `tools/plxnative-lab`,
  `ci/check-build-budgets.py`, `.claude/hooks/release-config-check-test.py` and `docs/`. Source-text pins and CI
  tests embed the name: `src/app/boot.rs:640,668` (`"plx_platform::storage::diagnostics::start("`),
  `src/app/run.rs:11184,11199` (`plx_platform::tv::window::opaque_route(...)`), `ci/test_ci_split.py:58-67`
  (a regex over Makefile `-p plx_platform`), `tests/test_harness.py:9916`, `ci/test_test_crate.py`,
  `ci/test_no_host_staticlib.py`, `ci/test_build_bench.py` and `tools/build-bench.py`. The gate keys on the
  keyword (`ci/check-module-layers.py:68,74`). These are the size of Wave R.
- F37. What the hello-world build can answer (review M9, M10); the emulator's results are E1-E13.
  - It builds one library per `--arch`: `arm` for retail 2021+ sets, `x86_64` ("the default emulator
    target") and `i686` (the older emulator) (`tizen/hello/README.md:41-43`). The emulator cannot test
    `Tizen.Multimedia` (`:120`), and the run did not probe it (E9). The host's TFM is `tizen80`
    (`PlxHello.csproj:14`), and its comment says coverage of later firmware by `tizen80` alone is
    UNVERIFIED (`:7-8`); E4 settles that for the Tizen 10 emulator only.
  - So an emulator run loads the x86 library. It answered H3 and H4 (E6, E8) and answers neither the
    retail process architecture (H1) nor whether the NDK-linked ARM library loads under a Public
    certificate (H2). The Samsung pages the review cites were not read here.
- F38. Fourth-platform leftovers (critic M12). `tv::device::Info` has webOS fields (`codename`, `api`,
  `name`, `major`; `platform/src/tv/device.rs:15-25`). `sandbox::Verdict` is the `/dev/rtkmem` probe
  (`sandbox.rs:1-14`).
- F39. What a launch with no credential on disk already reads (Decision 7; checked in the tree).
  - The two halves of a session already separate: `split_canonical` (`plex/src/plex/session.rs:692`) cuts a
    `Session` into a public payload (preferences, `client_id`, pins, recents: `split_public`, `:713`) and a protected
    auth string; `public_session` (`:790`) is the credential-free snapshot. The legacy fallback does NOT use that
    cut: it writes the whole `Session` in one file (`fallback_bytes(s)`, `:4812`; `save_legacy_fallback_locked`,
    `:4826`).
  - What sign-out keeps is defined twice. `Mutation::ClearTenure` (`platform/src/storage/state.rs:709`) keeps
    `public.preferences` minus the seven `ACCOUNT_BOUND_PREFERENCES` (`:184-192`) and drops pins, recents and
    `client_id`. The file authorities keep only `install_preferences::Retained`, `language` and `ui_resolution`
    (`session/install_preferences.rs:10`), "credential-free ... cannot authorize login" (`:1-3`); `clear_for_erase`
    saves it before a file backend's credential file goes (`session.rs:5344`) and after a DB8 sign-out whose helper
    clear failed (`:5481`); "Delete all local data" erases it (`:5473`). With no session file the legacy read ends
    at `install_preferences::load()`: a `Retained` file reads as `Cleared { kept }`, none as `Missing` (`:4222`).
  - Per-profile settings live in `profile_prefs`, account-bound ("a household member's habits, so it goes with the
    credentials", `state.rs:175`), so they are forgotten with the credentials.
  - `client_id` is in the public half, not a credential, and `plex/CLAUDE.md:398-399` says "a new id raises a
    new-device alert". First boot persists it through a token-less session save (`load`'s doc, `session.rs:4326`;
    `load_login_client_id`, `:4336-4349`), reusing one process-local id when storage is unavailable.
    `install_preferences::Saved` is `deny_unknown_fields`, version 1 (`:28-38`): it cannot carry the id without a
    new version.
  - The authority was chosen by `target_arch = "arm"`, not by a platform value (`session/persistence.rs:1400-1425`,
    `commit_data` `:288-308`); a Tizen ARM build would select the DB8 arm with no helper behind it (F12). The
    `MemoryOnly` gate sits above that selection. W8a re-keyed plex's 21 sites to `tv::authority::authority()`:
    `persistence.rs`'s dispatchers take the authority and the helper's transport, so a host test drives the helper
    arm with a fake one. Telemetry's consent store was re-keyed first (#1490), and that PR installs the account-clear
    sweep on every platform, so consent on a Tizen build takes the files arm, not DB8.

### Evidence from the second reviews: F40-F46 (each citation checked in the tree)

- F40. Exits a hosted process can reach (review C1; critic F8).
  - `gfx_compile` (`gfx/src/gfx.rs:1231-1233`) calls `exit_now(1)` on a compile rejection, and `link_program`
    (`:1281-1283`) goes through it for both shaders, so every REQUIRED program exits on a compile failure; the
    base program also exits on a link failure (`:1444-1452`, the call at `:1451`). Boot runs `init_gl`,
    `init_text`, `init_image` and `init_blur` back to back (`app/boot.rs:1730-1733`), but `blur_lazy_init`
    (`gfx.rs:6217`; called from the frame at `:4054`, `:6212`, `:6400`, `:7845`) links mid-loop through the
    same function: one exit is frame-time, not boot-time. Optional programs already use
    `link_optional_program` (`:1285`) and degrade.
  - Every `exit_now(` site: `gfx.rs:1232,1451`, `app/mod.rs:576,645`, `bin/sim.rs` (six) and
    `dev/framedump.rs:691` (`devtriggers`, off in a RELEASE build, `Makefile:581`). `std::process::abort`
    (`base/src/task/blocking.rs:116`) is `threadcheck`-only. The source guard `app/boot.rs:545` forbids
    `process::exit(` and `libc::exit(` only, so it permits `exit_now`; `const _` at `:540-541` pins
    `run_application` and `run_and_shutdown` to `-> !`.
- F41. What returning from the loop does today (review C2; critic F1).
  - `run_and_shutdown` (`app/mod.rs:561-576`) relies on `exit_now` diverging: "nothing drops the `App`" (`:574`).
    `run::shutdown` (`app/run.rs:3723-3752`) returns nothing, and `quits_sdl` (`:3712`) acts on a
    `NativeExit::WorkerInFirmware` that nothing else records.
  - A returned `App` would run `SessionAdapter::drop` (`app/adapters/session.rs:332`), GL object drops
    (`QrCode::drop` deletes a texture, `ui/src/qr.rs:147`) and field drops under live detached workers
    (#1115). `Engine`'s explicit `Drop` is test-only (`play-feed/src/feed_state.rs:242-246`) and feeds the
    counter `ENGINES_DROPPED` (`:235`).
- F42. The watchdog has no disarm. `set_foreground` (`base/src/task/watchdog.rs:222`) only mutes freeze
  reports (`:585-588,778`). Its fatal arm (`:812-820`, `threadcheck` only) and the SIGUSR2 probe (`:686-702`)
  `pthread_kill` a thread id captured from the loop owner "for the whole process"; after a hosted loop thread
  returns, that id is dead.
- F43. Composition publishes in two places (review C3; critic F7). `compose_backends`
  (`app/adapters/backends.rs:141-164`) takes `&mut PlaybackSession` and ends with `install_backends` (`:163`);
  `install_uri_product_with_ports` calls `session.install_uri_preflight_port(port)` (`:421`) only after the
  physical install succeeded (`:418`). The planner reads that port (`media/src/route/plan.rs:2048`, `NoPort`
  when absent); the session starts at `FEED` (F31).
- F44. The session authority (review C4; critic F3).
  - `update_guarded_with_outcome` (`plex/src/plex/session.rs:4555`) rereads `read_live_locked()` on every edit
    and can replace the cache or refuse it; `cache_revoked` and `clear_for_erase` (`:5340`) are separate
    doors; `peek()` caches what reads and durable writes installed. The auth owner releases a held handoff and
    sets `proves_saved_login` only on a `Durable` completion (`session/src/auth/owner.rs:3478,3507`).
  - `CompletionOutcome` (`plex/src/plex/session/async_persistence.rs:112`) and `LiveWrite` are matched in
    `session.rs`, `session/{persistence,async_persistence,prefs/profile_prefs}.rs`, `session/src/auth/owner.rs`,
    `telemetry/src/telemetry/incident.rs:592-600`, `src/app/adapters/session.rs` and three test files.
  - `install_preferences::candidates` (`:40`) and `clear_for_erase` (`session.rs:5341`) keyed on the ARM cfg, so
    Tizen ARM read `native = true` (W8a re-keyed them to `tv::authority::authority()`); `load` accepts
    `version == 1` only (`:59`) and `Saved` is `deny_unknown_fields`. The ARM canonical write was cfg'd out of
    host tests (`session/persistence.rs:1400-1425`); it is now the `Helper` arm of `persistence::*_via`, which every
    host runs over a fake transport.
  - `ci/check-deps.sh`: `plex/` is in `FORK_SCOPE`, `session.rs` in `MACHINES` (`:1406-1408`): a new
    `cfg(test)` branch there is a `forks` line, a new static a `statics` line. `platform/` is not in `FORK_SCOPE`,
    and only `platform/src/tv/plane.rs` is in `MACHINES` (`:1406-1408`; the scan `find` at `:1439` lists it too). The
    `globals` pattern (`:1429`) does not name `secure::mode`: UNVERIFIED whether it should.
- F45. Facts that fix file ownership (the lanes' Files lines are these).
  - `src/app/run_uri_boot_tests.rs` is `include!`d at `src/app/run.rs:6953`: a test added there needs no line in
    `run.rs`, and runs under `make test-hostsim`. `src/dev/uriprobe.rs:117` `#[path]`-includes `uriprobe_tests.rs`.
  - `ScreenInputs` (`screens/src/inputs.rs:29`) is filled in `src/app/bridge.rs` (`:1700`).
  - `device::Info.codename/api/name` are read in `platform/src/{webos,devcaps}.rs`,
    `telemetry/src/telemetry/native.rs:896`, `src/app/diagnostics.rs:465,596`, `src/lab/snapshot.rs:279-281`;
    `.major` in `app/boot.rs:906`, `adapters/backends.rs:378`, `media/src/player/pump.rs`,
    `play-feed/src/{feed_pump,seek,sf_event}.rs`, `play-uri/src/backend.rs`. The sandbox verdict is read in
    `media/src/{route/decision,player/{mod,adapter,machine,route_start}}.rs`, `appkit/src/player_hud/state.rs`,
    `screens/src/player/mod.rs`, `src/app/playback.rs` and telemetry.
  - The contract `Binding` is named in `playback-contract/src/uri.rs`, `play-uri/src/{backend,acb_bind}.rs`,
    `media/src/route/{uri_preflight,plan,decision_version_tests (26),decision_resolve_route_tests}.rs`,
    `src/app/{run_uri_boot_tests (18),adapters/backends,diagnostics}.rs`, `src/dev/{uri_product,uri_exported}.rs`;
    `media/src/player/preview.rs` matches a different `Binding` (`Phase::Binding`).
  - `AcbHandle` is named in `playback-contract/src/uri.rs` (type, `log_name`), `play-uri/src/backend.rs:1130`
    and two adapter tests (`media/src/player/adapter.rs:224,273`); nothing under `tests/`, `tools/` or `ci/`.
- F46. `ci/mkmacapp.py:32`, `ci/build-libass.py:13` and `ci/test-compat.py:6` import the stdlib `platform` and
  call `platform.system()` / `.machine()`: a `ci/platform.py` would shadow it for every script run from `ci/`.

### Evidence from the critic's third pass: F47-F52 (each citation checked in the tree at `e9924f0f9`)

- F47. The two reviewed certificates (G1). Both pin every member manifest and `Cargo.lock`, so the rename,
  which edits 12 manifests and the lock, invalidates both.
  - `ci/readonly_generated.py:16`: `CERTIFICATE_SHA256` over `ci/proofs/platform-generated-enums.json`. `SOURCES`
    (`:17-19`) is eight files: five under the moved directory (`platform/Cargo.toml`, `platform/src/lib.rs`,
    `platform/src/i18n/mod.rs`, `platform/build.rs`, `platform/build_support/catalog.rs`; `catalog.rs` is the
    only file in that `build_support/`), one OUTSIDE it (`build_support/install_identities.rs`, that is
    `rust-modules/build_support/`; its line 3 names `plx_platform` and `platform/src/storage/state.rs`, so the
    script rewrites it and its hash changes; `platform/build.rs:19` includes it by `#[path = "../build_support/...
    "]`, which survives the move) and two in `machine`. `produce_certificate` writes `package: "plx_platform"` and
    `module: platform/src/i18n/mod.rs` (`:60-61`). The `.json` holds seven bare `platform/` strings.
  - `ci/readonly_dependencies.py:21`: `CERTIFICATE_SHA256` over `ci/proofs/serde-json-immutable.json` (`:20`),
    checked at `:388-389` and `:458-471`. It holds `workspace.manifests` (19 hashes: the root and 18 members,
    keyed by path, among them `platform/Cargo.toml`), `workspace.lock_sha256`, and `workspace.owners` (13
    manifests, among them `platform/Cargo.toml` with `lib: platform/src/lib.rs` and its `lib_sha256`). It is
    regenerated by `python3 ci/readonly_dependencies.py --produce <verified serde_json crate archive> --root
    rust-modules --output ci/proofs/serde-json-immutable.json` (`:488-497`), which prints the SHA256 to pin
    after review.
  - `ci/test_readonly_generated.py` builds a scratch tree in the bare spelling: 13 `platform/...` lines (`:31,
    102,121,127,132-139,141,169`) and a `plx_platform` source string (`:179-180`). `ci/test_check_machine_shape.py:153`
    lists `platform/build.rs`, `platform/build_support/catalog.rs` and `build_support/install_identities.rs` too.
- F48. The old name, by spelling, counted with `git grep` at e9924f0f9 (G2). The F36 count covers only the first.
  - A. `plx_platform` not followed by `_webos`, `_desktop` or `_tizen`: 338 tracked files, 3461 lines (296 `.rs` files,
    3280 lines; 12 manifests and `Cargo.lock`; two nested `CLAUDE.md`; 27 files under `ci/ tools/ tests/
    .github/ Makefile docs/ .agents/`). It includes the `-p plx_platform` package lists in the `Makefile` (`:1682,
    1701,1713,1742,1776,1800`) and cargo features such as `plx_platform/uri-transport` (`rust-modules/Cargo.toml:81`).
  - B1. The path `rust-modules/platform/`: 25 tracked files. `Makefile:1016` (`RUST_INPUTS`) and `:1055`
    (`STORAGE_INPUTS`, with `2>/dev/null`) name `rust-modules/platform` and `rust-modules/platform/src/
    storage_service` inside `find` lists: a missing directory is dropped from the list, silently (the ARM library
    and the storage helper would then not rebuild when `os/` changes).
  - B2. The bare path `platform/...`, relative to `rust-modules/`: 23 tracked files outside the directory.
    `ci/list-caps.ini` (23 section keys such as `[platform/src/devcaps.rs::PCM_MAX_CHANNELS]`; a key that matches no
    constant is reported stale by `ci/check-list-caps.py:49`, so this one is loud), `ci/test_readonly_generated.py` (13),
    `ci/readonly_generated.py` (7) and both proofs, `ci/test_storage_package_isolated.py` (2),
    `ci/test_check_machine_shape.py:153`, `ci/test_build_bench.py:61`, `ci/check-deps.sh:1402`,
    `rust-modules/build.rs:4`, `rust-modules/storage/{build.rs:1,Cargo.toml:16}`,
    `rust-modules/build_support/install_identities.rs:3`, `Makefile:1052`, and prose in `docs/`.
  - B3. Manifest and workspace paths: `members = [..., "platform", ...]` (`rust-modules/Cargo.toml:14`) and 21
    `path = "platform"` / `"../platform"` lines in the 12 manifests.
  - B4. A path assembled from components, invisible to a path grep: `tools/cargo-seed.py:190`
    (`os.path.join(root, "rust-modules", "platform", "Cargo.toml")`; `read_bytes` returns `b""` for a missing file,
    `:122-127`, so the cargo seed key silently stops covering the bottom layer's manifest) and
    `ci/test_no_host_staticlib.py:45` (`WORKSPACE_FILES`, a quoted `"platform"`: its symlink dangles, so cargo
    fails loudly).
  - C. The layer token: `[platform]` (`ci/module-layers.ini:53`), the token in 13 `uses =` lines (`:73,84,92,102,
    112,123,134,148,152,162,172,182,187`), comments (`:54,56,72,80,120,199`), `docs/module-layers.md`, and
    `rust-modules/Cargo.toml:228`. The fence keyword `[port X]` is a second rename in the same files (`ci/check-
    module-layers.py:14-15,68-84`, `docs/module-layers.md`: 9 lines). The new keyword `[platform X]` shares its
    word with the old layer `[platform]`: `--reverse` must turn `[platform webos]` into `[port webos]` BEFORE it
    turns `[os]` into `[platform]`, and it must never rewrite a bare `os/` or `os` anywhere but the sites the
    forward pass wrote (a bare `os/` is also `std::os/`, `libc` paths and prose).
  - D. Names derived from it: `SRC_PLATFORM` (`ci/check-deps.sh` 18 lines, `docs/module-layers.md` 3, `tests/
    test_harness.py` 1) and the build-bench scenario `platform` (`tools/build-bench.py:132,149,171,180,201`,
    `ci/test_build_bench.py:96,212,229,244-245`).
  - Exempt, dated records that keep the old name: `docs/release-audits/*`, `docs/measurements/*`, dated
    `docs/*-20YY-MM-DD*.md`, and this plan (the whole page, `tools/rename-os-layer.py`'s `EXEMPT`).
- F49. `ProductQualifier` and the layer rule (G3).
  - It is a unit struct in `plx_media` (`media/src/route/uri_preflight.rs:768`) implementing media's
    `OriginalQualifier`; the composition builds it at `src/app/adapters/backends.rs:387`, after the
    `dev::uri_exported::compose` override of the binding (`:384-385`) and before the `dev::uri_product::compose`
    swap (`:393`); that dev route already swaps an `Arc<dyn OriginalQualifier>` in the app crate
    (`src/dev/uri_product.rs:230`). `OriginalRequest.binding` (`uri_preflight.rs:278`) is read only by this
    qualifier (`:775`) and by tests.
  - `[play_uri] uses = base platform playback_contract` (`ci/module-layers.ini:148`) cannot name `plx_media`, so a
    qualifier implemented inside `plx_play_uri` is a layer violation, and moving
    `OriginalQualifier`, `OriginalRequest` and the verdict types to `playback_contract` would put route policy
    into the contract. `[app]` (`:187`) uses media and play_uri.
  - Named in 10 tracked files: the three media files, `backends.rs`, `run_uri_boot_tests.rs`, `dev/uri_exported.rs`,
    `dev/uri_product.rs`, `media/src/player/CLAUDE.md`, and two that name it only in prose:
    `plex/src/plex/session/playback_options.rs:39` (a doc comment naming `route::uri_preflight::ProductQualifier`)
    and `docs/module-layers.md:180`.
  - `backends.rs`'s tests are `#[cfg(test)]` (`:455`) in the app crate and `default` includes `play-uri`
    (`rust-modules/Cargo.toml:78`): they run under `make test-hostsim`, not under `make test-crate C=plx_media`.
- F50. W3a's test seam, the globals ledger and the doc (G6).
  - `secure.rs` already has a per-THREAD test seam, `STORE_FOR_TEST` (`:23-31`). A mode override cannot be
    thread-local: session writes run on `plx_base::storage_worker` (`async_persistence.rs:3,968-969`). The session
    persistence tests already hold `testlock::serial()` (`session_persistence_tests.rs:11,24,40,...`), and
    `testlock::assert_held` (`base/src/testlock.rs:152`) is what "any new global store" owes (its module doc).
  - `globals_pat` (`ci/check-deps.sh:1429`) is a fixed list of spellings and `secure::` is not in it: `plx_platform::
    tv::secure::mode()`, `::open(` and `::set_mode_for_test(` do not match it (`grep -E`). `session.rs` already
    makes four unledgered `secure::` calls (`:4197,4837,4921,5454`); `ci/allow/globals/plex.txt` has 12 lines
    (`peek`, `peek_settled`, `current_snapshot`); the rule scans MACHINES files, `session.rs` as a file and not its
    `session/*.rs` children. So a mode read adds no ledger line, and extending the pattern would turn four
    existing calls into new sites (counts only fall). F44's UNVERIFIED is settled: no line, no pattern change.
  - `plex/src/plex/CLAUDE.md:501,646` call `peek()` "a write-through cache over the persisted session". Under
    `MemoryOnly` it is the authority; the PR updates that prose.
- F51. The shutdown plumbing (G4, G5).
  - `run_application` (`app/mod.rs:642-647`) has a 10-line budget: `ci/check-deps.sh:1176-1178` (`fnlen`). The leak and
    the `Exit` mapping therefore live in `app/exit.rs`, and the gate is not edited.
  - `quits_sdl` (`app/run.rs:3712-3718`) matches the two variants of `NativeExit` (`media/src/player/route_start.rs:
    1082`): `Clean => true` and `WorkerInFirmware => false`; `shutdown` (`:3723-3752`) logs "SDL_Quit skipped" on
    the second. `run.rs:3921` asserts the second.
  - `App` is built at two sites (`app/boot.rs:2474`, the `app/run.rs:3973` test rig); `AGENTS.md` requires a new
    field to be read and initialised at both. `rust-modules/src` is outside `FORK_SCOPE` (`check-deps.sh:1409`), so a
    `cfg(test)` field adds no `forks` line; a field, unlike a static, adds no `statics` line.
  - `gfx/src/gfx/straighten.rs:1272` finds `pub fn init_gl()` by text and `gfx/src/gfx.rs:10616` finds `pub fn
    init_image(`: both survive a return-type change.
- F52. W7a's test has no runner (G8).
  - `rust-modules/tests/` does not exist. Every unit target runs `cargo test --lib` (`Makefile:1742,1776,1800,1869`);
    `--tests` appears only in `cargo check` (`:1682`). The root crate is `crate-type = ["rlib"]` (`rust-modules/
    Cargo.toml:61`), so an integration test can link it.
  - `$(UNIT_RUN)` (`Makefile:1700`, `tools/cargo-test-parallel.py`) is documented for `cargo test --lib` and wants one
    executable per `-p` package; UNVERIFIED whether it accepts `--test host_interface` (one package, one executable
    would hold). The precedent for a non-`--lib` run inside a gate is `cargo test -p plxnative-storage --bin
    plxnative-storage` (`Makefile:1744`).
  - `ci/test_ci_split.py:58-67` pins each cargo gate's recipe and which of the three `check-cargo-*` targets holds
    it. `DISK_ENTRIES` (`Makefile:1548`) and `ci/test_disk_preflight.py` require every cargo-running target to join.

### Tizen research

From the Tizen research report (not in the repository), by section. None of it is checked on a retail set; where
the emulator runs (E1-E13) measured it, the item says so.
- T1. A retail TV refuses a native `capp`. The host is a `type="dotnet"` C# `.tpk` P/Invoking our
  `.so` (section 1).
- T2. The floor is Tizen 6.0 (2021). The 4.0-5.5 sets block file-backed exec of the app's `.so`, and
  Rust std's TLS defeats the in-memory loaders (sections 1 and 4, item 2).
- T3. The ABI is armv7 softfp NEON. Whether a model year is aarch64 is undetermined (2c); open as H1,
  the emulator is x86_64 and says nothing about retail sets (E1, E9).
- T4. glibc is 2.30 on Tizen 6.0-9.0 and 2.40 on Tizen 10 (ABI break) (2c). E3 measured 2.40 on the
  Tizen 10 x86_64 image; the 2.30 of the 6.0-9.0 sets is not measured. UNVERIFIED (H2): that an `.so`
  linked against our NDK's glibc 2.12 sysroot loads on Tizen; the x86_64 library (at most
  `GLIBC_2.34`) loading on the 2.40 image is no evidence for ARM. libstdc++/libgcc and the absence of
  SDL or libcurl on a retail set are not checked (the emulator image's libcurl and other libraries are E12).
- T5. Window Option A is host-owned GLView/GLWindow with the EGL context handed over, and SDL2 static
  with the dummy video driver. Option B is a native Ecore_Wl2 window with EGL. esplusplayer needs
  Option B's `Ecore_Wl2_Window*`. SDL2 on a retail Tizen TV is not evidenced (2d, 1); the emulator ran static SDL2 + SDL2_ttf with `SDL_Init(TIMER|EVENTS)`
  and no video driver (E10), so the dummy-video-driver variant of Option A was not exercised. Option B drawing
  from a Rust thread works on the emulator (H3, H4; E6, E8) and is unmeasured on a retail set.
- T6. URI playback via `Tizen.Multimedia.Player` (C#) is proven:
  - only the User-Agent and Cookie headers pass;
  - no DTS or TrueHD;
  - esplusplayer feed is firmware-dependent (2e).
- T7. Use one `.tpk` per API generation, bundle no TizenFX DLLs, force `LC_NUMERIC=C`, export every
  `DllImport` symbol, and keep certs and DUIDs out of the repo (2b, 2d, 2f). Measured: a `tizen80`
  `.tpk` runs on Tizen 10 (E4), so one `.tpk` is viable to start; sets of Tizen 6.0-9.0 and `tizen90`
  are untested.
- T8. There is no `sdb shell` or `dlog` on a retail TV, so logs must be in-app (2a). Measured on the
  TV 10.0 emulator too: UDP is the only channel out, and a file in the app data dir cannot be read (E5).

### The hello-world questions (H1-H8)

The Tizen hello-world (F28) exists to answer these, and the text cites them by number:
- H1: whether a retail set's host process is armv7 or aarch64 (T3).
- H2: whether the NDK-linked ARM `.so` loads under a Samsung Public certificate on a retail set (T2, T4).
- H3: whether Option B, a native Ecore_Wl2 window, draws from a Rust thread (T5).
- H4: whether Ecore main-thread handlers and a Rust loop thread run without deadlock (T5, F32).
- H5: whether static SDL2 and SDL2_ttf draw text and a GLES2 texture through our path (F15).
- H6: whether `Tizen.Multimedia.Player` shows a mock title beneath our window and survives a TV-menu round trip
  (T6, W10).
- H7: whether the application's own renderer, `plx_gfx`, compiles its shader programs and draws on the set's GL
  (README "H7" on `samsung/hello-tpk` at `e8da14ce6`; F40 says why a rejected required shader ends the process).
- H8: whether the application's own libcurl binding (`plx_net` on the `dynlib!` loader) finds a libcurl on the set
  and reaches a server on the tester's PC (README "H8" on `samsung/hello-tpk` at `e8da14ce6`; F27).

Status: H3, H4 and H5 are answered yes, on the emulator only (E6, E8, E10); H1 and H2 are open and need a retail
set (E9); H6 is not probed (E9, E11). H7 and H8 were probed by the build of E13: H8 yes, and H7 yes once
`vs_ambient.vert`'s one multi-declarator is split, with one optional program still failing; both on the emulator
only.

### Emulator run, 2026-11-20

Reported by the operator of the emulator run (E1-E9, built from `origin/samsung/hello-tpk` at `72c5cd82a`);
E10-E13 are later runs on the same emulator. Measured on the emulator only; no retail set has reported a result.

- E1. Setup. TV Extension 10.0 emulator, image 10.0.6 (`tv-samsung-10.0-x86_64`, BUILD_ID
  `EMULATOR64-202601071715`), under WSL2/KVM; the library is x86_64, built on Ubuntu 22.04. `sdb
  capability`: `cpu_arch x86_64`; `intershell`, `rootperm` and `appcmd` disabled. WSLg's Mesa D3D12
  segfaults the emulator in `yagl_host_eglSwapBuffers`, so it ran with `LIBGL_ALWAYS_SOFTWARE=1
  GALLIUM_DRIVER=llvmpipe`. Certificate Manager under WSLg needs `GDK_BACKEND=x11`.
- E2. Signing. Tizen.NET.Sdk 1.1.9's bundled certificates are expired (author 2019-01-01, distributor
  2022-10-27). A renewed "Tizen Studio Public Signer" distributor is still refused (`Check certificate
  error`, 118,-12): the image's `fingerprint_list.xml` trusts only Samsung VD roots for the public and
  partner domains. Install worked with a Samsung certificate profile from Certificate Manager (TV,
  Public): author Samsung VD Author CA to Tizen Developers Root; distributor VD DEVELOPER Public CA to
  VD DEVELOPER Public Root, bound to the emulator's DUID, valid to 2027-11-19. So the emulator needs the
  same Samsung Public profile as a retail TV. Samsung's FAQ says a Tizen profile suffices for the
  emulator; that is false for this image. `build.sh`'s `PLX_TIZEN_*` path signed with it, and
  Certificate Manager's `.p12` (`pbeWithSHA1And3DES`, SHA-1 MAC) was accepted.
- E3. The image. glibc 2.40, newest symbol node `GLIBC_2.39`. .NET runtimes 2.0 to 8.0.11 and the full
  TizenFX TV set. `/usr/lib64` has `libecore_wl2.so.1` (1.25.1), `libEGL.so.1`, `libGLESv2.so.2`,
  `libwayland-client.so.0`, `libtbm.so.1`, `libcapi-appfw-application.so.0` and `libdlog.so.0`. The
  hello-world's x86_64 `.so` needs at most `GLIBC_2.34`. That is one x86_64 data point: the ARM
  library's limit is unmeasured (H2).
- E4. Run. Install took 1277 ms; launch with `sdb shell 0 execute plxnhello0`; executable
  `/usr/bin/dotnet-launcher`. The `tizen80` host runs on Tizen 10: .NET 8.0.11, `ProcessArchitecture`
  X64, `IntPtr.Size` 8. It was launched through sdb, not from the TV menu.
- E5. Logs. `res/log-host.txt` was found, and UDP from the emulator to `10.0.2.2` delivered 206 lines with
  0 sequence gaps. It is the only log channel on this image: `sdb shell <cmd>` prints nothing (only
  `sdb shell 0 <cmd>` answers), `sdb dlog` returns 0 lines and `sdb pull` of the app data dir is
  refused. A retail set has no sdb shell or dlog (T8, research; not measured here).
- E6. Window and GL (H3). `ecore_wl2` display connect, `window_new` 1920x1080, `egl_window_create`,
  TOPLEVEL, alpha false; `ecore_event_handler_add` resolved from `libecore_wl2.so.1`. EGL 1.4, vendor
  Samsung; ES2 context, RGBA8888; `eglMakeCurrent` on the Rust render thread; `eglSwapInterval(0)`
  then 1; renderer "YaGL GLESv2"; first frame presented, `glGetError` 0. The emulator draws through
  YaGL over llvmpipe, so this is no GPU, compositor or frame-rate result.
- E7. Keys. Injected with `ecp-cli` as evdev events: Right keycode 114, Left 113, Up 111, Down 116 and
  Return 36, reported as Ecore key names. Every press changed the colour. Key to frame took 8-30 ms (key
  on the host thread, colour on the render thread): an emulator figure, not a retail one. Back, media
  keys, digits and the channel rocker were not exercised by this run (the colour button came later, E11), and a
  retail remote's codes are not measured.
- E8. Lifecycle (H4). OnResume after create; focus IN at +1.9 s. A kill gave OnPause (the render thread
  idles), then OnTerminate: handlers deleted, the render loop stopped after 2478 frames, EGL destroyed
  and terminated, the `ecore_wl2` window and display torn down, "terminate: done". No deadlock between
  the Ecore main-thread handlers and the Rust render thread. `terminate: done` is logged only after the
  render thread joined (`lib.rs:365`), so the 2 s fallback of F29 was not taken.
- E9. Not answered. H1 (retail armv7 or aarch64) and H2 (the NDK-linked ARM `.so` under a Public
  certificate on a retail set): the emulator is x86_64 and runs a different build. H6 (the player under our
  window) was not probed, and H5 (SDL2 + SDL2_ttf) was not probed by THIS run (it came later: E10). The ~33 fps is llvmpipe through
  YaGL, not an app measurement. Launch from the TV menu and its pause behaviour (README, report item 5)
  are untested, because the run launched through sdb.

- E10. H5, SDL2 + SDL2_ttf inside the host (2026-11-20, the same emulator; `samsung/hello-tpk` at `64c609a46`).
  PASSED, emulator only. What the run measured:
  - static SDL2 2.32.10 and SDL2_ttf 2.24.0 (FreeType 2.13.2, no HarfBuzz) linked into `libplx.so`, loaded by the
    .NET host;
  - `SDL_Init(SDL_INIT_TIMER | SDL_INIT_EVENTS)` with `SDL_NO_SIGNAL_HANDLERS=1` succeeded; the SDL timer thread
    ran. The video subsystem was not initialised;
  - TTF rendered Latin, the check mark and Cyrillic with Inter; a GLES2 texture was drawn on the Rust render thread
    (YaGL);
  - keys worked and teardown (`TTF_Quit`, `SDL_Quit`) was clean;
  - the x86_64 `.so` is 2.5 MB, needs at most `GLIBC_2.34`, and its SDL symbols are local;
  - the screen output could not be screenshotted: QEMU's `screendump` cannot capture YaGL output.
  This answers H5 on the emulator. It is no retail-set result (H1, H2: the ABI and the loader), no frame-rate
  claim and no armv7 size or symbol figure. The tester build whose screen shows diagnostics and a key-code panel
  is E11 on the emulator; its run on a retail set, which would answer H1 and H2 and return a retail key table
  (E7's codes are the emulator's), had not been reported on 2026-11-20.

- E11. Tester build `1afe1978b`, on-screen panel photographed (2026-11-20, relayed from the emulator operator).
  Emulator only; it is not H6 (no player in this build).
  - All seven steps green on the panel: the `.so` loaded (x86_64); six libraries `dlopen`ed; Ecore_Wl2 1920x1080
    with four of four handlers (key down and up, focus in and out); EGL 1.4 Samsung, ES2; GL renderer YaGL GLESv2
    "OpenGL ES 2.0"; SDL2 2.32.10 and SDL2_ttf 2.24.0; the UTF-8 sample string (Latin, a check mark, Cyrillic).
  - Keys: the panel listed 31 distinct keys. The one new beyond the earlier emulator key table (not reproduced here)
    is the remote's colour-key button, `XF86Color` = 385. `Shift_L` = 50 came after Home and is most likely the host keyboard (the screenshot
    shortcut), not a remote key. The other codes were not relayed, so none is recorded here.
  - Home round trip: pause at +181.7 s together with focus OUT; resume at +191.6 s after 9.9 s paused; focus IN
    at +191.7 s. The panel redrew after the resume, so the window and the EGL surface survive a Home pause and
    resume on the emulator. No player was running, so this is not H6.
  - Device line: Tizen 10.0 (native API 3.0), model Emulator, build `tizen_20260107.16983968`, .NET 8.0.11 X64,
    `IntPtr` 8 bytes, glibc 2.40, executable `/usr/bin/dotnet-launcher`. The panel's CPU line was truncated
    ("aarch64 (unav...") and is not read as a result; the process architecture is the device line's X64.
    The build commit `d8c4b2766` shortens that line's "(unavailable)" text to "n/a".

- E12. Image library check for W9 (2026-11-20, read-only inspection of the base emulator image
  `emulimg-10.0.x86_64`). Emulator image only; a retail set's libraries are not measured.
  - libcurl: `/usr/lib64/libcurl.so.4` (`.4.8.0`, SONAME `libcurl.so.4`, libcurl/8.11.0, TLS OpenSSL 1.1), with
    its `NEEDED` libraries (`libcares.so.2`, `libnghttp2.so.14`, `libssl.so.1.1`, `libcrypto.so.1.1`,
    `libz.so.1`) all present. There is no unversioned `libcurl.so`; our list's first name, `libcurl.so.4` (F27),
    matches. The default CAINFO is `/etc/ssl/ca-bundle.pem`, which resolves to
    `/opt/var/lib/ca-certificates/ca-bundle.pem` (202 certificates, base image), and the CAPATH `/etc/ssl/certs`
    resolves to `/opt/etc/ssl/certs`.
  - Also present, all in `/usr/lib64`: `libssl.so.1.1` and `.so.3`, `libcrypto.so.1.1` and `.so.3`, `libz.so.1`,
    `libfreetype.so.6`, `libpng16.so.16`, `libjpeg.so.8`, `libbrotli*.so.1` and `libnss3.so`. There is no gnutls.
  - The mock server: `tests/mock_pms.py` runs under the host's python3 (3.10) and defaults to `--host 127.0.0.1
    --port 32499`. The emulator's `10.0.2.2` is the host's loopback (QEMU user networking), as for the UDP log (E5).
  - W9 design note. The mock plex.tv override (`plex_tv()` in `plex/src/plex/account.rs`) accepts only
    `127.0.0.1` and `localhost`, on purpose: the trigger carries the token. Reaching it from the guest would need a
    reverse forward into the emulator (`sdb reverse` mode: unchecked), so the emulator MVP (Decision 8) boots on the
    dev-token path (a `--guest --mock` style boot, the PMS advertised as `10.0.2.2`), and the sign-in against the
    mock plex.tv is a follow-up check. The loopback rule is not widened.

- E13. The real renderer and libcurl (H7, H8; 2026-11-20, the TV 10.0 x86_64 emulator, a probe build of
  `samsung/hello-tpk` at `e8da14ce6`). Emulator only; no retail set has run it.
  - H8: yes. libcurl, loaded through the app's own `dynlib` mechanism (`libcurl.so.4`, E12), reached
    `tests/mock_pms.py` on the host as `10.0.2.2` and got HTTP 200.
  - H7, shaders. YaGL ("YaGL GLESv2", OpenGL ES 2.0, GLSL ES 1.4, over the host's Mesa llvmpipe) rejected
    `VS_AMBIENT_DITHERED` with `0:37(9): error: syntax error, unexpected NEW_IDENTIFIER`, at
    `varying highp vec2 v_dither_nc;`. That is the error Mesa gives for raw desktop GLSL 1.10, where `highp` is an
    ordinary identifier, so the source looks passed through untranslated (an inference from the message, not a
    measurement of YaGL).
    - The only multi-declarator in `shaders/`, `uniform vec4 u_atl, u_atr, u_abr, u_abl;` in `vs_ambient.vert`,
      split into four declarations, makes it compile. Before and after: the original fails and the split compiles,
      and a cut-down variant does the same. The tree these facts pin still has the statement; the split was made in
      the probe build.
    - A `#version 100` preamble also made it compile, but the link then failed ("all shaders must use same shading
      language version"), so that route was dropped.
    - With the split, all 14 required programs compile and link. `plx_gfx`'s `init_gl`, `init_text`, `init_image`
      and `init_blur` all returned, and it drew a rect and text. `glGetError` was 0 at init, frame and swap, and it
      drew 3420 frames at about 33 fps (llvmpipe through YaGL: no frame-rate claim, E6).
  - One optional program still fails the same way. The straighten program (`fs_straighten.frag`, its copy variant,
    because the emulator has neither framebuffer-fetch extension) fails with `0:37(1): syntax error, unexpected
    NEW_IDENTIFIER` at `precision mediump float;`. It degrades ("frames over the video plane stay premultiplied")
    and does not exit. The suspect, untested, is its `#extension ... : require` lines inside inactive `#if`
    branches. It matters only over the video plane, so it is for W10, not the W9 MVP.
  - Font. `plx_gfx` opens `<exe dir>/appfont.ttf`, but the executable is `/usr/bin/dotnet-launcher` (E4), so the open
    fails. The probe retried from the package's `res/` and it opened. The resource directory has to become a
    platform value (W8b, W9).
  - Linking. `plx_gfx` imports `gl*` at link time, and the hosted `.so` is loaded `BIND_NOW`. The probe used a
    `dlsym` shim; W9 either links `libGLESv2.so.2` or keeps a shim.

## Target shape

### What stays shared (every platform)

- All layer crates: base, machine, os (the bottom layer, Wave R), gfx, net, plex, telemetry, data, session,
  media, appkit, screens and ui. Also playback-contract, media-codecs, the neutral part of play-feed (demux,
  HLS, ABR) and the app crate `plxnative-modules` (loop, adapters, dev surfaces).
- Every machine, the route planner, the stores and the screens. A platform adds no machine. It supplies
  runners, function-pointer hooks and published boot values, exactly as F5's table does now.
- FFmpeg, libass, the `dynlib!` mechanism, the GLES2 renderer and SDL2_ttf text (F15). SDL2 stays a shared
  dependency for timing, surfaces and TTF; on Tizen that is verified on the emulator only (E10, H5: static SDL2 +
  SDL2_ttf, no video subsystem) and still unverified on a retail set (H1, H2). The SDL VIDEO driver is
  the platform's choice. Removing SDL from gfx and base is out of scope.

### The exported value and the two tables

Names follow the owner: the crates are `plx_platform_webos`, `plx_platform_desktop` and `plx_platform_tizen`
(Decision 1, B); the bottom layer is `plx_os` after Wave R, so no layer shares a crate's name. Each exports one
`pub static PLATFORM: host::Platform` (Target shape 8), and exactly one is the root of a build. The value is
split by who reads it (critic M4):

1. `tv::Services` (`plx_os::tv`, installed once by `run_application`) holds what LOWER layers read through the
   private `tv::port()` (F5):
   - `identity { probe_device, start_capability_probe, repair_native_video: Option<fn() -> Result<(), Failure>>,
     device_files: Files, system_locale, sound_output }` (`repair_native_video` is optional: `None` is a platform
     with nothing to repair, and the repair then answers `Failure::Unsupported`; `device_files` is the reader
     `devcaps` parses through, `Files::NONE` for a platform with none);
   - `secure { seal, open, remove, mode }` (Target shape 4; until W2 `mode` is W3a's own once-set value);
   - `system { go_home, poll_home, deliver_toast }`;
   - `storage { authority }` (W8a): `Authority::Helper` (webOS: the LS2/DB8 storage helper is the canonical store of
     the session, the install's preferences and the consent record; the app's own files there are the legacy
     migration inputs, the marked fallback of a helper outage and the install-preferences file a sign-out writes
     when the helper's clear is not durable) or `Authority::Files` (desktop, Tizen, a host
     test: the app's own record files are the store), read through
     `tv::authority::authority()`. `secure::mode()` decides the session record ahead of it: under `MemoryOnly`
     (where Tizen starts, W9) the session lives in memory and no credential store is read or written, whatever the
     authority says; only the install's own preference file persists. It replaces the `target_os = "linux"` + `target_arch = "arm"` fence the persistence
     layers chose their store by; the helper's wake-up (`storage::client::install_activator`) joins it after W7b;
   - `legacy_webos_media { sink, display_sink, uri_media }`: the Starfish, ACB and `com.webos.media` verbs (F31),
     named for what they are (critic M3). Every hook is required (Decision 2, C): a platform states each field,
     a named `NONE` being a statement and a `Default`-filled gap not allowed. The one exemption is this group:
     until W11a a non-webOS platform leaves it at `media::NONE` (`NoSink`/`NoUriMedia`), as `ABSENT` does today,
     and W11a injects the LG transports into the LG drivers, so the group and the exemption disappear.
2. `app::Platform` (the app crate) holds what only the loop reads; the shell and the frame probes have one
   consumer, the loop (F34), so they do not belong in the bottom layer's process global:
   ```
   pub struct Platform {
       pub services: &'static tv::Services,
       pub shell: &'static dyn Shell,        // a Sync FACTORY of the loop's surface (Target shape 8)
       pub diag: &'static dyn FrameProbe,    // the six frame probes; a named NONE is fine
       pub media: MediaBoot,                 // Target shape 6a
   }
   pub fn run_application(p: &'static Platform) -> Exit;   // returns; Target shape 7
   ```
   `Shell` and `FrameProbe` are traits in the app crate, beside their consumer. A `&'static dyn` can read its
   own statics (the Tizen host table); a plain `fn` field could not (critic M6). A new `App` field is
   initialised at both construction sites (AGENTS.md), and `Platform::HOST_TEST` replaces the "no port
   installed, every hook is a no-op" default for the loop's host tests.

There is no `default_backend` (review M8, critic M1): the backend set is derived from the installed drivers (F31,
6a). There is no runtime capability registry (owner decision).

### 1. Shell

`Shell` (the factory) opens the loop's `Surface`: `create_surface`, `make_current`, `swap`, `drawable_size`,
`gl_proc_address`; `release`/`reacquire` around background; `video_hole(Option<Rect>)`, which replaces
`arm_opaque_region`, `opaque_route` and `clear_opaque_region`; `pump()`, which replaces `pump_bus`. The trait doc
states which thread runs each call (Target shape 7). `Shell`, `Surface` and `HostEvent` stay app-private until
H1 and H2 return (critic M5; H3, H4 and H5 returned on the emulator only, E6, E8, E10; Decision 3, A): code moves
now, but no type a Samsung shell must match leaves the app crate; Target shape 8 is the one narrow, public
construction interface. The dispatcher's rig hooks (`ui/src/dispatch.rs:728-730`) keep delegating through
`run::rig_*`.

### 2. Input and lifecycle: canonical actions (review M6, C8; critic F11)

This replaces two earlier designs, "`HostEvent::Key` carries SDL keycodes plus LG scancodes" and critic M7's "map
Ecore names to `remote_token_key` pairs".
- `HostEvent` (app-private, like `Shell`) has:
  - `Key { action: Option<Action>, edge: Edge, raw: Option<RawKey> }`, `Edge` being `Down | Repeat | Up` as in
    `InputKind::Key` (F33). `action` is `Option` because unbound keys flow today as raw pairs (`k:0,269` HOME,
    `app/events.rs:202-212`), the configurable lab trigger consumes pairs no `Action` names, and the recorder
    retains them: a raw-only event keeps its original pair and edge, and the loop treats `action: None` as the
    raw path it is today;
  - `Text(String)` and `Edit`; `Pointer { x, y, buttons, shown }` and `Wheel { dy }` (carried explicitly, F33);
    - As landed (W5c, #1492): `Pointer { x: f32, y: f32, edge: PointerEdge }`, `PointerEdge` being `Move | Down |
      Up` (the SDL event type alone, for any button), and `Wheel { dy: i32 }`. There is no `buttons`, because the
      loop never read which button was pressed: whether one is held is its own state (`Pointer::button_down`). There
      is no `shown`, because that is the pointer machine's own `cur_hidden`, not event data (LG's hide arrives as a
      key, `Action::PointerHidden`, and a show is a motion). See `app/events.rs`.
  - `Lifecycle(WillBackground | DidBackground | WillForeground | DidForeground)` and `Quit`;
  - `LocaleChanged`, `LowMemory` and `Launch(String)`.
- `Action` names what callers consume, not a physical key: `Up Down Left Right Ok Back Play Pause PlayPause Stop
  Rewind FastForward Exit PageUp PageDown Backspace ClearAll Digit(0-9) PointerHidden`. The final list comes
  from W5b's inventory.
- It lives in `plx_ui::consts` (not hash-pinned), beside `classify` and `is_bound`. Two total functions:
  `raw_pair(Action) -> (sym, wcode)`, the only place a pair is built, and `action_of(sym, wcode) ->
  Option<Action>`, the only decode, built from `classify`, `page_dir`, the edit-key readers and the digit rule.
  `classify` alone returns `Other` for paging, editing and digits (F33), so it cannot be the guard.
- Digits. `Action::Digit` reads `sym` only, as `is_bound` does (`ui/src/consts.rs:466-483`: desktop punctuation
  shares the wcode range 48-57, so a wcode-only digit is deliberately not bound). The PIN keypad reads 48-57 from
  either field (`screens/src/profiles.rs:340`, `digit_of`). That reader moves down into `consts` unchanged as its
  own `pin_digit(sym, wcode)`; a wcode-only press stays a raw event whose PIN effect is exactly today's.
- The SDL decoders (LG layout, stock layout) fill `raw` and, where defined, `action`. A platform that is not SDL
  sets `action` only, and the app derives the pair with `raw_pair`. So `InputKind::Key`, the machines, the tape
  codec and the 7 raw-reading screens are unchanged, and tapes stay byte-identical (F16).
- A platform's key table maps its native names to `Action` and nothing else; no platform builds a pair. The
  round trip `action_of(raw_pair(a)) == Some(a)` stays as a unit test but is NOT the guard: it never calls the
  native-name decoder and cannot see a swapped table. The guard (W5b, extended by W9 for Ecore) is native name
  -> the platform's decoder -> the real consumer's effect: transport (Play is not Pause, Rewind is
  `Left { alt: true }`, never plain `Left`), paging (PageUp is not PageDown), Search editing and the PIN keypad
  (each digit its own). Further cases: an unknown pair (no action, raw kept, ignored as today), a lab trigger key,
  a wcode-only punctuation press on the PIN screen and in `is_bound`, and a replay that is byte-identical, `Edge`
  included. The review's example, a Tizen table that swaps Play and Pause and still passes `is_bound`, fails it.
- Measured so far (E7): Right is keycode 114, Left 113, Up 111, Down 116 and Return 36, as Ecore key names. They
  seed the table's first rows; `Return` (36) is the candidate for Ok, not for Back. E11 adds the remote's colour
  button, `XF86Color` = 385 (its panel listed 31 distinct keys; this is the one new beyond the earlier emulator key
  table, which is not reproduced here). Everything else waits for a key dump from a retail set.
- Raw SDL stays where it is: `decode_key`/`rd_u32`, the `TEXTINPUT` offset, both wheel offsets and the
  sdl2-compat injected-key carrier (`app/events.rs:40-70`) move into the SDL decoders unchanged.
- Exit step (critic M7): the machine's own key type carries the full `Action`: a certificate review of
  `machine.rs` and a tape-format version (F33). It is W5d, due before a FOURTH platform (Decision 6, A).

### 3. Home, toast, launch: `tv::home`, `tv::toast` and `tv::launch` are already neutral and unchanged.

### 4. Secure store and the persistence mode (review B2, C4; critic M8, F2, F3; Decision 7, B)

- The envelope's `Backend` stops being a closed enum in shared code (`secure.rs:8-11`): an opaque string tag,
  serde-transparent, with constants for `"keymanager3"` and `"palm_keymanager"`, so every sealed session on a
  TV still opens. W3d's literal fixture guards the wire form. A platform adds its own tag in its own crate
  (`"host_file"`; a Tizen tag, UNVERIFIED which Tizen API a dotnet app may call). A tag no installed platform
  knows is refused on open, never reinterpreted.
- Mode. `secure::mode() -> StoreMode { Durable, MemoryOnly }` is a once-set value (`secure::publish_mode`) in
  `os/src/tv/secure.rs`, default `Durable`. It is NOT a `Port` field in W3a (critic F2): W2 folds it into
  `Services.secure` and the accessor's signature does not change. `secure::NONE` alone is NOT memory-only (F30).
- The test override (G6; F50). `tv::install` is once-only, so a test cannot go through it, and the once-set value
  cannot be reset. The override is therefore a SEPARATE process-global in `secure.rs` (an atomic), consulted
  before the once-set value under `cfg(any(test, feature = "test-support"))`, like `STORE_FOR_TEST` but NOT
  thread-local: session writes run on `plx_base::storage_worker`, a thread the setting test does not own.
  `secure::set_mode_for_test(StoreMode) -> ModeReset` calls `plx_base::testlock::assert_held(..)` first and its
  guard restores `Durable` on drop, so a failing test cannot leave `MemoryOnly` behind for the next one. Every test
  that sets it begins with `let _serial = plx_base::testlock::serial();` (the session persistence tests already do),
  and no test sets it without that. `os/` is not in `FORK_SCOPE`, and only `os/src/tv/plane.rs` of it is in
  `MACHINES`, so the override in `tv/secure.rs` adds no ledger line.
- Globals (G6; F44, F50). Reading `secure::mode()` from the session code adds NO line to `ci/allow/globals/`: the
  gate's pattern is a fixed list of spellings that does not name `secure::`, and `session.rs` already makes four
  unledgered `secure::` calls. The pattern is NOT extended (it would turn those four into new sites, and counts
  only fall). W3a shows `ci/check-deps.sh --only workers,globals,forks,fxlanes` and `ci/check-ledger-ratchet.py`
  with no ledger delta.
- Authority. Under `MemoryOnly` the session boundary has ONE process-local authority for reads, guarded edits
  (`update_guarded*`), revocation (`cache_revoked`) and clear (`clear_for_erase`): the `peek()` cache cell becomes
  the source of truth, `read_live_locked()` answers from it, and no durable credential read or write is
  reachable (the four writers, the canonical commit and `save_legacy_fallback_locked`'s plaintext arm). The gate
  sits above the storage-authority selection (`tv::authority::authority()`, which replaced the `target_arch = "arm"`
  fence in W8a; F39, F44). It adds no `cfg(test)` branch to `session.rs`
  (F44: a `forks` line) and extends the existing cache rather than adding a second static.
- Outcome. A successful memory-only write reports a new ephemeral outcome (`LiveWrite` and `CompletionOutcome`
  gain `Ephemeral`; not `Durable`, not `Failed`). The auth owner treats it as signed in for this run: it
  releases the held fresh-login handoff and proceeds to Home, `proves_saved_login` stays false, no "saved"
  claim is shown, and one event-log line says memory only. Every consumer in F44 handles the variant.
- Decision 7, B (owner, 2026-11-20): what persists is `client_id`, language and interface resolution, nothing
  else. The carrier is the `install_preferences` file at version 2, which adds `client_id`. Version 2 is written
  only under `MemoryOnly` (the id is serialized only when set), `load` accepts versions 1 and 2, and webOS and
  desktop keep writing version 1, so their files are byte-identical (v1 is `deny_unknown_fields`). No session
  writer runs in `MemoryOnly`; the v2 file is written when the id is minted (`load_login_client_id`'s
  `save_locked`) and when language or resolution changes. `candidates()` and `clear_for_erase` follow
  `StoreMode`, not the ARM cfg (F44). Sign-out drops the in-memory session and keeps the v2 file; "Delete all
  local data" removes it too. Every other preference, the per-profile settings among them, is forgotten each
  launch, and the Tizen PR says so.
- Tests (W3a). Mock sign-in through to the auth owner's Home state under `MemoryOnly` with a writable data
  directory: no credential file, and a scan of the WHOLE data directory finds no byte of the mock token. A
  profile/settings edit after sign-in neither refuses nor loses the session. Sign-out, then erase, leaves nothing.
  A fresh process on the same directory with planted legacy credentials ignores them, asks for sign-in again and
  has the same `client_id`, language and resolution, with per-profile settings forgotten. A failure-injection
  test forces the canonical write to fail and drives the legacy fallback: under `Durable` it writes (the control),
  under `MemoryOnly` it never runs. webOS/desktop `Retained` fixtures are byte-identical; existing LG envelope
  fixtures are graded separately.

### 5. Device, caps, sandbox

- Identity publishes `platform` and `platform_version`, and `plex::identity::PLATFORM` (F26) is the default
  until the platform publishes. `device::Info`'s webOS-only fields (`codename`, `api`, `name`) move behind a
  webOS-owned detail (F38); `release`, `build` and `major` stay as the neutral OS-version facts.
- The devcaps file reader moves to the platform, and the parsed model stays.
- The sandbox becomes `native_video: Available | Blocked(reason) | NotApplicable` with an optional repair hook.

### 6. Media, as four things

- a. Composition and the two boot phases (review M5, C3; critic F7). `Platform.media` is `MediaBoot { early_init:
  fn(&MainThread), compose: fn(&mut PlayerAdapter, controlled: bool) -> Composition }`, with
  `Composition { resolver: Option<ResolvePort> }` (W6b landed this shape; the `&PlaybackSession` parameter this
  paragraph first named is dropped, see the deviation below):
  - `early_init` runs at today's `acb_init` site (`app/boot.rs:2043`, inside `if !controlled`, before
    `ffi::boot()`, F32). webOS fills it with `acb_init`, the others with a no-op.
  - `compose` runs where `compose_backends` runs (`:2323`). webOS fills it with today's function
    (`backends.rs:64,141-164`) minus both publications; the desktop installs HostSink Feed; Tizen installs
    nothing (W9), then its URI driver (W10). It returns the resolver of a URI product only when the physical
    install succeeded (today's `install_uri_product_with_ports`, `:418-421`); `None` on a refused install, a
    controlled boot or no URI driver. It publishes nothing. Deviation, W6b: it takes no `&PlaybackSession`. The
    session was read by nothing in the composition (the only session use was the resolver publication that moved
    out), so the parameter would have been dead; W10 adds a read-only session parameter if a Tizen driver needs
    one.
  - After `compose` returns, the APP publishes both facts in one function, once, always: the resolver
    (`install_uri_preflight_port`) if present, then `install_backends(adapter.composed_backends())`,
    `BackendSet::NONE` included (`backends.rs:163` and `:421` moved app-side, F43; landed in W6b as
    `app::platform::publish_composition`, which also logs the one `compose: backends=<set> resolver=<yes|no>`
    line). The session starts at `FEED`
    (F31), so a zero-driver platform that forgot the call would offer Feed, and a set that advertised URI without
    its resolver would reach the planner as `NoPort`. No platform keeps adapter and session in step.
  - The Cargo feature `play-uri` (`rust-modules/Cargo.toml:81`) keeps meaning "the webOS ACB URI driver" and is
    webOS-only. `PLAYBACK_BACKENDS` maps per platform (Tizen: its own `uri` feature; `play-uri` never). The
    `feature = "play-uri"` sites in `rust-modules/src` (37 when this was written, 51 at W6b: #1478 added 14 to
    `backends.rs`) are sorted in W6b: the 10 in `playback.rs` and `run.rs` read the installed drivers at run
    time (the attempt's backend, the adapter's queues, the resolver's offer) and lost their gate; every site that
    remains names ACB or the URI product and keeps it (`git grep -n 'feature = "play-uri"' -- rust-modules/src`
    lists them: `backends.rs`, `dev.rs`, `lib.rs`'s ARM guard and two test files).
    Composition is a SEAM until W11a, not a move: `port.rs` fills `Platform.media` with
    `backends::acb_init` and `backends::compose_native`, and the bodies stay in `app/adapters/backends.rs`. They
    cannot go to `port` yet, because `dev/uri_product.rs`, `dev/uri_exported.rs`, `run_uri_boot_tests.rs` and
    `backends.rs`'s own tests name `UriProduct`, `UriDriverSettings` and `install_uri_product_with_ports`, and the
    layer fence forbids anything outside the webOS platform naming `port`. W11a moves the file whole, with its
    callers.
- b. Evidence is the driver's, in three separate decisions (review B3, C5; critic M1, F12).
  - Display readiness: `UriDriver::display() -> DisplayEvidence { readiness: Ready | NotReady(DisplayUnavailable),
    preview_rectangle, tag }`, beside the capabilities and certification its offers carry. `Binding` leaves the
    shared contract; `Acb`, `Exported{canvas}` and the `VP_EXPORTED` comparison become the LG driver's own detail
    inside `plx_play_uri` (`Binding` is defined there; the app names it as `plx_play_uri::backend::Binding`,
    `backends.rs:45`).
  - Source qualification: whether THIS source may play stays the composition's qualifier, and the qualifier stays
    in the app crate (G3; F49). LG's rule that declines a multi-audio original on an exported display
    (`ProductQualifier`, `uri_preflight.rs:771-775`) and the viewer opt-in (`uri_opt_in`, `:771`, independent of the
    capability bits) move from `plx_media` to `src/app/adapters/backends.rs`, where the qualifier is built
    (`:387`). They do not go "into `plx_play_uri`'s qualifier": `[play_uri] uses = base os playback_contract`
    (`ci/module-layers.ini:148`, where it read `base platform playback_contract` at `e9924f0f9`) forbids
    `plx_play_uri` to name `plx_media`'s `OriginalQualifier`, and moving that trait, `OriginalRequest` and the verdict types to
    `playback_contract` would put route policy into the contract, so neither move is made. `ProductQualifier`
    keeps its name and implements media's `OriginalQualifier` in the app crate (`[app]` may use `media` and
    `play_uri`, `:187`). It holds the `Binding` it was built with, read from the same value the driver is
    configured with, so `OriginalRequest.binding` is deleted. It is built AFTER the `dev::uri_exported::compose`
    override of the binding (as `:384-387` already orders it) and BEFORE the `dev::uri_product::compose` swap.
    The shared route reads only immutable evidence, which the resolve worker receives: `plx_media` holds
    `DisplayEvidence` and an opaque display tag (`&'static str` the driver names, printed in the boot log and the
    diagnostics, branched on by no shared code), and never a `Binding`.
  - Start readiness: `ready_to_start` stays an on-thread check (the ACB handle is made lazily on the loop thread,
    `play-uri/src/backend.rs:1124`; `media/src/player/adapter.rs:1175` falls back to Feed when it fails).
  - A driver that is a platform's only path declares its own certification (Tizen: its capability set); the
    webOS driver keeps `certified` empty until the viewer's opt-in, as today.
  - `UriNotReady` gains `DisplayUnavailable`, and `AcbHandle` is replaced in W3b by a driver-owned reason
    (`UriNotReady::Driver(&'static str)`, the LG text unchanged) because three files name it and nothing pins its
    text (F45); `log_name()` still prints the same line.
  - The ACB plane owner (`tv/plane.rs`), `tv::uri` and the `plane_*` half of `DisplaySink` are webOS DRIVER code
    named by layers below `app` (F8), so they go to a new layer crate `plx_acb` (`[acb]`, `uses = base machine
    os`) in W11a, once neither `media` nor the app crate names them. The app's own names go to the webOS platform
    in W6b. Until W11a the contract is NOT called neutral (critic M3).
- c. The Feed sink (L15b item 1): codec config plus timestamped access units, with seek, flush, pause and events;
  the Starfish half of play-feed moves behind it in W11b. It does not gate Tizen URI playback: a Tizen URI driver
  implements `UriDriver` and never touches `VideoSink`.
- d. Routing stays as it is. `BackendKind::{Uri, Feed}` means locator versus packets and `Capabilities` states
  needs (critic M10). The set comes from the installed drivers. W6c exercises zero drivers, URI only with the
  experimental setting off, a source the preferred driver declines (review M8), and a TWO-driver set: today's
  `backend_policy::Offer::rank` (`media/src/route/backend_policy.rs:48`) hard-codes a URI tie-break and a
  `FEED_ORIGINAL` tier, which a URI-only fixture cannot see; W3b pins that order first (C5).

### 7. Entry, shutdown and the host doorway

- Entry: webOS is `src/main.c` calling `plex_run`; desktop is the `plxnative-sim` bin; Tizen is a shared library
  exporting the three calls below.
- Return (review B1, C2; critic F1, F8). `run_application` returns `Exit { code, release }` instead of `-> !`
  (`release` is `Release::Complete | Retained`). An early-boot refusal returns its code, and a played session
  returns after `run::shutdown` and the telemetry guard drop (F29). The executable entries own the process end:
  `plex_run`, for webOS and the desktop, calls `exit_now(code)` right after, in today's order, so webOS
  behaviour is unchanged. A hosted library never calls `exit_now` or `exit`.
- Exit inventory (C1; F40). No exit a hosted process can reach remains. The graphics boot (`init_gl`,
  `init_text`, `init_image`, `init_blur`) returns a typed `GfxFault` for a compile or link failure of a required
  program and `enter_application` returns it as an `Exit`. `blur_lazy_init` links mid-loop (F40); its programs
  take the optional path (a compile rejection then degrades as its link failure already does), and a required
  program ever linked lazily ends the loop through a loop-level fatal with code 1. The source guard
  (`app/boot.rs:545`) drops the `-> !` pins and becomes a scan: no production `exit_now(` outside `src/port.rs`,
  `src/bin/sim.rs`, `src/dev/framedump.rs` (`devtriggers`) and `base/src/process.rs`. Tests inject a compile and a
  link failure: the fake host stays alive and receives the failure code.
- App stays leaked (critic F1; G5; F41, F51). The `App` is `ManuallyDrop`/`forget`-ed after the loop on BOTH entry
  kinds, so returning drops nothing: `SessionAdapter::drop`, GL objects and the feed `Engine` never run under live
  workers. The sentinel is a probe FIELD on `App` itself, not a counter on the dropped types: a
  `#[cfg(test)] drop_probe: DropProbe` (an `Arc<AtomicUsize>` whose `Drop` increments), so dropping the `App`
  drops the probe whatever its other fields are. It is a field, not a static (no `statics` line), and
  `rust-modules/src` is outside `FORK_SCOPE`. Like every new `App` field it is read (dead-code is denied in test
  builds) and initialised at BOTH construction sites (`app/boot.rs:2474` and the `app/run.rs:3973` test rig). The
  host test hands such an `App` to the returning exit path and asserts the count is 0, and a control drops
  an `App` the ordinary way and asserts 1, so the probe is shown to detect. The leak and the `Exit` mapping live in
  `app/exit.rs`: `run_application` keeps its 10-line `fnlen` budget (`ci/check-deps.sh:1176-1178`), which W3c does
  not edit.
- Release status (C2; G4; F51). `run::shutdown` returns a `Release`, and `Release::Complete` is defined from the
  arm of `quits_sdl` (`app/run.rs:3712-3718`) that ends with `SDL_Quit()`: `NativeExit::Clean`. `NativeExit::
  WorkerInFirmware`, the arm that logs "SDL_Quit skipped" today, is `Retained`. ONE function computes both from the
  one `native_exit` (`quits_sdl(e)` is `release_of(e) == Complete`, or the reverse), by an exhaustive `match` with
  no `_` arm, so a third `NativeExit` fails to compile in both and the hosted report cannot say `Complete` where
  the executable skipped `SDL_Quit`. The existing assertion (`run.rs:3921`) is extended to pin both variants of
  both answers. `Complete` additionally needs the loop to have released its GL context (`Shell::release`);
  `Retained` otherwise. Teardown order is unchanged. The executable ignores it; the hosted entry maps `Retained`
  to `clean = 0`.
- Drain and gating (C2; critic F8). Every host-bound callback (window, player request, log sink, `on_stopped`)
  enters through one `HostGate { stopping: AtomicBool, inflight: AtomicUsize }` and is refused once `stopping`
  is set; a late player completion is dropped. `clean = 1` is delivered only after the loop returned
  `Complete` AND `inflight == 0`: a flag alone picks the completion sender but cannot drain a callback already
  running. The watchdog is disarmed before `on_stopped` and across `OnPause` (`watchdog::disarm`/`arm`, new in
  W3c, F42; `arm` re-captures the loop thread id).
- The hosted contract is private to the Tizen platform (review M4, critic M6):
  - `plx_host_start(const PlxHostApi *, const char *launch_json)`, `plx_host_post(const PlxHostEvent *)` and
    `plx_host_stop()`. `PlxHostApi` starts with `size` and `abi_version`; the library reads a later field only
    when `size` covers it and refuses a table smaller than version 1.
  - Threads. `plx_host_start` runs on the host's main thread inside `OnCreate`: it creates the Ecore_Wl2 window and
    registers the key handlers there (the hello-world's model, F28; E6, E8), copies the table and the launch
    string, spawns the loop thread and returns. `plx_host_post` may be called from any thread: a bounded queue
    push and a wake, never a block. `plx_host_stop` runs on the host main thread, sets `stopping` and returns.
  - The loop thread runs `run_application` and mints `MainThread` (F32). There the token means "the loop thread":
    it authorises the player adapter and EGL, NOT Ecore window calls, and `MainThread::assume`'s safety text
    (`base/src/task.rs:138`, "the SDL main thread") is updated in W9 before it is minted there. Work that must run
    on the host thread leaves as a queued request through the callback table and comes back as an addressed
    completion event; the loop never waits on the host thread.
  - Ownership: a string passed into the library is copied before the call returns; a string passed to the host
    is valid for that call only; the host keeps its managed delegates alive until completion.
  - Completion. `on_stopped(code, clean)` is delivered exactly once, guarded by an atomic flag. On a clean stop the
    loop thread calls it with `clean = 1` once the drain above holds. If the loop has not finished within a
    deadline (a stalled swap), a watchdog thread started by `plx_host_stop` calls it with `clean = 0` and the
    loop's own report is suppressed. After the call no callback is made again; with `clean = 1` the host may
    release callbacks and surfaces, with `clean = 0` it releases nothing and treats process exit as the teardown,
    as the hello-world does (`lib.rs:342-365`; the emulator run took the clean path, E8).
  - The library is never `dlclose`d, and detached workers keep running until the process ends. The host must not
    call `exit()`: without `src/exit.c` the #1115 hazard returns. UNVERIFIED: how Tizen ends a .NET app's
    process, and whether our signal tracer can be installed in the host process.
  - Host-driven loops (iOS, Android; critic M12) are not built; the contract is in thread roles so a host tick can
    be added later, and `start` reserves a `flags` word.
  - Tests: a fake host table in host tests is the second implementation (W3c builds the gate and the fake; W9
    wires the C ABI to it): boot refusal; quit during playback; stop with a player request outstanding and a late
    completion; a returning loop with a retained worker (`clean = 0`); a callback in flight at stop (`clean = 1`
    only after it returns); a swap that stalls and wakes after the timeout; pause then stop; the drop probe (G5).
- `access.c` and `exit.c` do not apply in a host-loaded `.so` (F19).
- Diagnostics (requirement; E5, T8). A Tizen build streams its event log from inside the app: one UDP datagram per
  line to an address the developer configures at build or launch, as the hello-world does (`--log-host`,
  `res/log-host.txt`): a sequence number per sender so a lost datagram shows as a gap, non-blocking sends,
  failures ignored, off unless configured, an IPv4 literal; the host's own lines (a failed `.so` load) leave the
  same way. A file in the app data dir is not a channel: it cannot be read off the device (E5). W9 decides where
  the sink plugs into the app's event log (`plx_base`); the acceptance is a run whose log reaches a receiver
  with no gap.

### 8. The cross-crate construction interface (review C9; W7a)

- The problem. W7 makes the platform crates siblings of the app crate, but `mod app;` is private
  (`src/lib.rs:17`), so a sibling cannot name `app::Platform` or implement a crate-private `Shell`; and a
  `static PLATFORM` must be `Sync`, while window state is thread-affine (`MainThread` is `!Send + !Sync`,
  `base/src/task.rs:129`).
- The interface. One narrow public module, `plxnative_modules::host`, of re-exports only: `Platform`, `Services`,
  `Shell`, `Surface`, `FrameProbe`, `MediaBoot`, `Composition`, `Exit`, `Release` and an `EventSink` trait. The
  split is by thread: registration data (the `static PLATFORM`: function pointers and `&'static dyn` of `Sync`
  types, `Shell` included as a factory) is `Sync` and shared; thread-affine state (window, GL context, the
  Ecore handles) lives only in a `Surface` that `Shell::open(&self, &MainThread)` returns on the loop thread and
  that the app owns inside `App`. The platform reports input through `EventSink` (`key(Option<Action>,
  Option<RawKey>, Edge)`, `text`, `pointer`, `wheel`, `lifecycle`, `quit`, ...); the APP converts to its private
  `HostEvent`, so the owner's decision that the event API stays private holds. `Action`, `RawKey` and `Edge`
  are already public lower-layer types.
- Gate. Before extraction, a desktop-shaped root is compiled against `host` alone: an integration test
  (`rust-modules/tests/host_interface.rs`, a separate crate that can name only `pub` items) builds a
  `static PLATFORM` with a fake `Shell` and boots it under `hostsim`. W7b extracts only after it passes.
- Runner (G8; F52). No target runs it today (every one is `cargo test --lib`; `rust-modules/tests/` does not
  exist), so a test file alone would be a gate nobody runs. W7a adds (1) `make test-host-interface`: `cargo
  test -p plxnative-modules --features hostsim --test host_interface`, incrementally in `target-hostsim` like
  `make test-hostsim`, through `$(UNIT_RUN)` if it accepts `--test` (UNVERIFIED: F52) and as a plain `cargo test`
  otherwise; it is a cargo-running target, so it joins `DISK_ENTRIES` (with its `DISK_TREE` line) or
  `ci/test_disk_preflight.py` fails; and (2) the same invocation as a recipe line of `check-cargo-unit-hostsim`, the
  `host-unit-hostsim` CI job's target, so `make check` and CI run it, pinned in `ci/test_ci_split.py`'s `GATES`.


### What each platform supplies

| interface | webOS | desktop | Tizen |
|---|---|---|---|
| entry | `main.c` exe, `plex_run` | `plxnative-sim` bin, `PlxNative.app` | `.so` + C# host, `plx_host_*` |
| shell (a `Surface` the loop owns, Target shape 8) | `plx_shell_sdl` + LG-fork bits (SysWMinfo, `wl_surface` opaque region, LS2 pump) | `plx_shell_sdl`, stock SDL | Ecore_Wl2 window on the host thread, EGL on the loop thread (F28; H3 and H4 yes on the emulator, E6, E8); Option A only if a retail run fails |
| input | LG layout `decode_key`, Magic Remote | stock layout, FIFO injection | Ecore key names to `Action` (Target shape 2; five codes measured, E7) |
| lifecycle | SDL app events | SDL app events | C# OnPause/OnResume/OnTerminate through `plx_host_post`; stop request and completion |
| key store | keymanager3 / palm, `Durable` | `HostFile` (later Keychain) | `MemoryOnly` first (client id, language, resolution kept; Decision 7, B), then the Tizen store |
| identity, caps | `webos::probe`, codec table, jail verdict | host values, `NotApplicable` | `Tizen` + version, player codec set, `NotApplicable` |
| backends | Feed (Starfish) + URI (`play-uri`, ACB) | Feed over `HostSink` | URI over `Tizen.Multimedia.Player` (W10), Feed later (W12) |
| display evidence | the LG driver's (ACB, exported window) | n/a | the Tizen driver's own |
| storage helper | LS2/DB8 service (`rust-modules/storage`) | none | none |
| diagnostics | event, crash and stderr descriptors (`main.c`, F19) | unchanged | in-app UDP log sink, required (Target shape 7, E5) |
| packaging | `.ipk` (`ci/mkipk.py`) | `.app` (`ci/mkmacapp.py`), bare bin | one `.tpk` first per configuration (x86_64 for the emulator MVP, armv7 for retail; Decision 8), signed with a Samsung Public profile (PLATFORM axis, E2) |

### Crates and layers at the end

- `[shell_sdl]`: layer crate `plx_shell_sdl` (`uses = base machine os`): SDL window, GL context, event drain and
  stock key layout. A layer because webOS and the desktop share it (F1); not the shell Tizen will use (critic M11).
- `[acb]`: layer crate `plx_acb` (6b), used by `play_uri`, the Starfish half of `play_feed` and the webOS
  platform. W11a.
- `[platform webos]`: crate `plx_platform_webos` (`rust-modules/platform-webos`): `webos`, `keymanager`, the LG
  half of `system`, `webos::video_sink`, the storage activator, the value and `plex_run`; it produces the
  staticlib `main.c` links.
- `[platform desktop]`: crate `plx_platform_desktop`: the desktop value, the 5 host arms of the fence that only the
  simulator uses (W4: two stubs in `system.rs`, the two `SINK` arms and the table's test in `port.rs`; the other 97
  lines are television gates, the simulator's stand-ins kept for tests and `test-support`, or TV-side lines and
  stay) and the sim bin. `HostSink` stays in `play_feed`.
- `[platform tizen]`: crate `plx_platform_tizen` plus top-level `tizen/` (C# host, manifest, sysroot scripts).
- The bottom layer is `[os]`, crate `plx_os` in `rust-modules/os`: Wave R (Decision 1, B; owner 2026-11-20: first
  and alone) renames it with its directory, its 12 manifests and its references (F36), keeping the inner `tv`
  module so a call site changes by its crate segment only. W7 later drops `webos` and `keymanager` from it.
- `plxnative-modules` (`[app]`) loses `port` and `system`, names no platform and gains the public `host` module
  (Target shape 8).
- Gate change. Wave R switches the fence keyword (`[port X]` to `[platform X]`: gate code, since the parser keys
  on `startswith('port ')`, F1, F36) together with the layer name. W7: a platform section whose members form one
  crate above `app` may carry `uses` (every layer). Platforms may not name one another (F1), and nothing names
  a platform.

### The PLATFORM axis

- `PLATFORM ?= webos`, validated by a new `ci/platform_axis.py --list` as FLAVOR is (F23). The selector is NOT
  `ci/platform.py`: scripts run from `ci/` already import the stdlib `platform` (F46), and `ci/test_platform_axis.py`
  also runs an existing consumer (`ci/mkmacapp.py`'s import, `ci/test-compat.py --help`) from that directory so a
  shadow fails a test, not only a `make -n` diff. Values: `webos`, `desktop`, `tizen`. FLAVOR stays install identity.
- `PLATFORM` does not choose the compatibility axis (review M10): Rust target, sysroot, managed TFM, firmware floor
  and signing path. Each platform records its tested configurations, one per Rust target it is tested on. Tizen has
  two (Decision 8). x86_64 is a first-class configuration of `mk/platform-tizen.mk`: the TV 10.0 emulator, `tizen80`,
  the Samsung Public certificate profile the emulator needs too (E2), and the configuration W9's MVP is accepted on.
  armv7 softfp (T3) with the NDK sysroot, a named firmware range and the same kind of profile is the retail
  configuration; it is recorded but untested until a retail set answers H1 and H2. Their variables are printed by the
  CI job and quoted in the PR. What has run so far is x86_64 on the Tizen 10 emulator (E1-E13): it exercised
  `tizen80`, the signing path and the host contract but neither the ARM library nor the NDK sysroot (H1, H2), and its
  results read "emulator only". A further configuration (aarch64, `tizen90`, glibc 2.40) is added only when a
  measured incompatibility on a named device requires it (E4: `tizen80` runs on Tizen 10). The packaged `.so` is
  inspected (`readelf -d`, its undefined symbols against the sysroot's glibc symbol versions), and that exact `.tpk` is the one loaded on the device. One `.tpk` starts; T7's "one `.tpk` per API
  generation" is the later case.
- W1 adds the variable, its validation and the `print-platform`/`print-rust-target` query targets (explicit
  entries beside `Makefile:189-207`). Public targets (`sim-*`, `macapp`, `ipk`) keep their names. Per-platform
  `mk/` files are split out only when `mk/platform-tizen.mk` exists (critic M11).
- Cargo: the root of each build is exactly one platform crate, and platform crates never depend on one another.
  `make check` lints `--workspace` (F11), so a platform crate's edge to the app crate carries NO platform feature.
  `plx_platform_desktop` has its own `hostsim` feature, off by default, forwarding `plxnative-modules/hostsim`;
  its lib body is `#![cfg(feature = "hostsim")]`, so the workspace lint compiles it empty. `plx_platform_tizen`
  does the same with a `tizen` feature.
- Stages that need a second invocation exist already: the hostsim type-check (`.github/workflows/ci.yml:379`),
  `make test-hostsim`, the `sim-*`/`macapp` builds, `check-cargo-unit-hostsim` and `check-nodef`
  (`Makefile:1928`), which after W7 also checks `-p plx_platform_webos`. A Tizen clippy pass runs in the Tizen CI job.
- The hostsim gate (review M11, F35). `check-cargo-unit-hostsim` keeps its eleven packages, including
  `-p plxnative-modules --features hostsim`, and adds `plx_platform_desktop`; `HOSTSIM_PKGS` is the developer's
  loop. Before and after every PR that moves tests (W4, W7), a script lists the discovered test names
  (`cargo test --lib -- --list`, run with the gate's package AND feature arguments) and the diff must be moves
  only, the app's lifecycle suite (`run.rs:3915`) and the Feed suites included. A separate check asserts that each
  artifact root has exactly one `plx_platform_*` in its normal dependency tree.
- Tizen link shape: `cargo rustc --crate-type staticlib` of `plx_platform_tizen` (F24), then `cc -shared
  -Wl,--whole-archive` over it plus `svg.o`, `sentry_context.o`, `crashtrace.o` and libass-plx. No `main.c`,
  `access.c`, `exit.c`, `starfish.c` or `video_plane.c`; no cdylib crate type, so `ci/test_no_host_staticlib.py`
  is untouched.
- CI: `cross-build` (`ci.yml:531`) stays required and webOS-only. A new compile-only `tizen` job builds the `.so`
  and checks its exports against the C# `DllImport` list (T7); it stops at the `.so` (`build.sh --so-only` needs
  no certificate): the `.tpk` needs a device-bound profile that stays out of the repo (T7, E2). The simulators
  job runs `PLATFORM=desktop`.

## Workstreams (ordered; every PR ships webOS)

Groups: (N) needs nothing from Tizen and no television; (H) gated on a named hello-world result; (T) needs the
webOS television for acceptance. Lanes in a wave are file-disjoint by the Files lines below; `->` is a stack
inside one lane (the next PR starts after the previous merged).

| Wave | Lanes | Bounded test (`make test-crate C=`, or `test-hostsim`) | TV | build-bench |
|---|---|---|---|---|
| R | the rename, alone | its own review gate: `--check`, the `--reverse` equivalence, two rehearsals (below) | boot log diff | yes |
| 0 | W0 | none (doc-claim-auditor) | no | no |
| 1 | W3a; W3b; W3c | W3a `plx_plex`, `plx_session`, `plx_media`; W3b `plx_media`, `plx_play_uri`, `plx_playback_contract`, hostsim; W3c hostsim, `plx_gfx`, `plx_base` | W3b, W3c | no |
| 2 | W1; W2 | W1 python tests; W2 `plx_os`, hostsim | W2 | W1 |
| 3 | W3d -> W4; W5a -> W5b -> W5c | W3d `plx_os`, `plx_plex`; W4 hostsim; W5 hostsim, `plx_ui` | W5b | W4 |
| 4 | W6b, then W6 and W6c | W6b hostsim, `plx_media`; W6 hostsim; W6c `plx_media`, `plx_screens`, hostsim | W6b, W6 | W6 |
| 5 | W7a -> W7b | hostsim, `make test-host-interface` (the integration test; G8) | W7b | W7a, W7b |

Order (Decision 4, A, with the owner's rename decision of 2026-11-20): R; W0; the three contracts W3a-c (who
may persist credentials, what evidence admits a URI player, who ends the process); W1 and W2; W3d then W4
beside W5; W6b; W6 and W6c; W7 (Decision 3, A: H3 and H4 are in on the emulator only, H1 and H2 gate a retail
set, not W7; Decision 8 takes W9's MVP to the emulator); W8; then W9-W10. W11a/b and W13 are independent. Why the
cut (review C6; critic F2, F5, F6; F45):
- R first and alone: a lane open across it re-runs the script, and W0's document then carries the final names.
- W1 leaves wave 1: Decision 4 puts it after W3a-c and nothing in them needs the Makefile.
- W2 before W3d: both own `tv/*.rs`. W3d before W4: both edit `os/src/webos.rs`, `webos/caps.rs` and
  `webos/jail_repair.rs` (W3d writes `Info` and reads the sandbox there; W4 moves the hostsim arms out of them),
  so they stack in one lane. W3d keeps `app/{boot,run,bridge,mod}.rs` out of its Files, so the W5 chain,
  which owns those, runs beside it (the `devcaps` calls keep their signature).
- W6b after W4 (`src/port.rs`, `platform_desktop*`, the `sink` rule) and W5 (`run.rs`, the event dispatcher).
  W6c consumes W6b's installed set and resolver, so it is not beside W6b but beside W6: its tests extend
  `run_uri_boot_tests.rs` (`include!`d, F45) and the `backends.rs` tests, W6 owns `boot.rs`, `run.rs`, `mod.rs`.

Gates (review C7; critic F9; owner, 2026-11-20):
- A lane runs one bounded `make test-crate C=<crate> T=<filter>` per crate its tests live in, or `make
  test-hostsim [C=] [T=]` for `hostsim` tests; no `make check` and no cross-build in a lane.
- Each PR's own CI gates its own head (`make check`, `make check-nodef`, the ARM cross-build). A tree merged from
  several lanes is never the evidence for one of them.
- One review reads 2-3 PRs together (each `base...head`) and is recorded on each head
  (`tools/pr-land.py reviewed`); a push after it needs a new review. fw-compat-reviewer where a lane lists it;
  doc-claim-auditor after behaviour change.
- TV checks are batched: one `--guest --mock` lease, taken with `tv-lock` and released at once, on the commit the
  result supports (a wave's last-merged PR, for its file-disjoint lanes).
- A `make build-bench` table on the PR that changes `Makefile`, `Cargo.toml`, workflows or the module layout.
- `ci/check-module-layers.py` with `ci/allow/layers.txt` empty and `ci/check-ledger-ratchet.py` on every Rust PR (a
  ledger line is never the fix); a reviewed `ci/readonly_generated.py` certificate change for any PR that edits
  one of its pinned sources (F25).
- TWO reviewed certificates pin every member manifest and `Cargo.lock` (F47): any PR that changes a `Cargo.toml`
  or the lock (Wave R, W6's workspace `Cargo.toml`, W7b, W9) re-pins BOTH
  (`ci/readonly_generated.py:16` and `ci/readonly_dependencies.py:21`) in the same PR, each a reviewed change.

### Wave R (N, also T). Rename the bottom layer, first and alone (Decision 1, B; owner, 2026-11-20)
- Status: landed on 2026-11-20 as #1474 (`e2088058d`); `origin/main` has the names this page's non-fact sections use.
- Goal: `plx_platform` becomes `plx_os`, one PR, no behaviour change, before W0 and with no other PR merging
  meanwhile (critic F4: the "quiet window" the old W6d relied on does not exist).
  - Package, directory (`git mv rust-modules/platform rust-modules/os`), 12 manifests, `Cargo.lock`, the 3280 lines
    in 296 `.rs` files, the nested `CLAUDE.md` files and the 27 files under `ci/ tools/ tests/ .github/ Makefile
    docs/ .agents/` (F36, F48 A: 3461 lines in 338 files counting every kind).
  - `[platform]` becomes `[os]` in `ci/module-layers.ini` (the section, the token in 13 `uses =` lines, the
    comments) and in `docs/module-layers.md`; the fence keyword `[port X]` becomes `[platform X]` (gate code,
    `ci/check-module-layers.py:14-15,68-84`); the inner `tv::` module stays. `SRC_PLATFORM` becomes `SRC_OS`.
  - Pins that embed the name are changed in the same PR, each found by grep (F36, F48): `app/boot.rs:640,668`,
    `app/run.rs:11184,11199`, `ci/test_ci_split.py:58-67`, `tests/test_harness.py:9916`,
    `ci/test_{test_crate,no_host_staticlib,build_bench,storage_package_isolated,check_machine_shape}.py`,
    `tools/build-bench.py` (package, leaf file and the scenario id `platform`), `ci/check-deps.sh`, the `Makefile`
    gate package lists and its two `find` lists (`:1016,1055`), `ci/list-caps.ini`'s 23 keys,
    `tools/cargo-seed.py:190`, `rust-modules/{build.rs,storage/build.rs,storage/Cargo.toml}`, `.github/workflows/
    ci.yml`, `.agents/agents/fw-compat-reviewer.md`.
  - Certificates (G1; F47): TWO reviewed re-pins, not one. `ci/readonly_generated.py` (`:16` the constant, `:17-19`
    `SOURCES`, `:60-61` package and module) with `ci/proofs/platform-generated-enums.json` (the file keeps its
    name), and `ci/readonly_dependencies.py:21` with `ci/proofs/serde-json-immutable.json` (its manifest keys, the
    `platform/Cargo.toml` owner entry and `lib_sha256`, `lock_sha256`). The pinned sources are five files under the
    directory (`Cargo.toml`, `src/lib.rs`, `src/i18n/mod.rs`, `build.rs`, `build_support/catalog.rs`) AND
    `rust-modules/build_support/install_identities.rs`, which is outside it and which the directory move does not
    carry. `ci/test_readonly_generated.py` is edited with them (13 bare paths). Order, because each step feeds
    the next: script; `Cargo.lock` regenerated by cargo (`--locked` fails until it is); any reflow; both
    certificates regenerated; both constants re-pinned.
  - It ships `tools/rename-os-layer.py`: idempotent; `--check` (below); `--reverse`, which rewrites a tree back to
    the old names, and `--reverse-patch FILE`, which rewrites a backport's diff to the old names so that a
    `backport` cherry-pick of a later fix onto a release line, which still has the old names, applies
    (`backport.yml` opens nothing when a pick conflicts, and every pick touching a renamed path does); and
    `--equivalence BASE` (below). The script does not run rustfmt, and `--reverse` rewrites only the sites the
    forward pass wrote (F48 C).
- Proof (G2). A missed reference does NOT always fail the build: `tools/cargo-seed.py:190` reads a missing manifest
  as empty and `Makefile:1016,1055` drop a missing directory from their `find` lists (F48 B1, B4), and both stay green.
  Two checks are needed for that reason.
  - `--check` lists the old-name sites left, in every spelling of F48, not one: (1) `plx_platform` not followed by
    `_webos`, `_desktop` or `_tizen`; (2) the path `rust-modules/platform` NOT followed by `-webos`, `-desktop` or
    `-tizen` (after W7b those directories exist and would match, N3), the bare `platform/...` paths relative to
    `rust-modules/` with or without `../` prefixes (the `#[path = "../../platform/src/..."]` lines of
    `storage/src/main.rs` and `manifest.join("../platform/src")` in `base/src/eventlog/scrub.rs:1081`, N2), the
    manifest forms (`path = "platform"`, `"../platform"`, the `members` entry) and every quoted `"platform"` or
    `'platform'` (the components assembled in `cargo-seed.py`, `ci/test_no_host_staticlib.py` and the build-bench
    scenario, and the sites a hand list missed, N1). The script classifies EACH such hit over the tracked files as
    renamed or exempt (`sys.platform`, a JSON key) and FAILS on an unclassified one; it does not rely on a list of
    files. The silent one is `rust-modules/media/src/player/report.rs:3135` (the layer array of the `fetch_update`
    scan: `walk` returns on a missing directory and `files > 500` still holds, so the scan drops the bottom layer and
    every gate, `--equivalence` included, stays green); `.claude/hooks/release-config-check.py:276` is loud only
    through its test (`:69`); (3) the layer token: `[platform]` and `platform` in a `uses =` line of `ci/module-layers.ini`,
    and `[port X]`. Exempt, by a list in the script: `docs/release-audits/*`, `docs/measurements/*`, dated
    `docs/*-20YY-MM-DD*.md` and this plan (the whole page: the Wave R section itself quotes the old names). It must
    print zero outside the list. The reviewer ALSO
    runs plain `git grep` counts for each class of F48 before and after, in the PR body, so a bug shared by the
    script's rewrite and its check cannot hide.
  - Equivalence: `--equivalence BASE` applies the forward pass to a scratch copy of BASE and `--reverse` to a scratch
    copy of the head, and compares each with the other side. The only differences allowed are an enumerated list,
    printed with the result: (a) the two certificates, each a `.json` and one constant line; (b)
    `rust-modules/Cargo.lock` (the package renames and re-sorts); (c) a `.rs` hunk that is a pure reflow or `use`
    reorder (the same multiset of tokens), which the shorter name causes (`plx_os` sorts differently and lets a
    wrapped line fit); (d) the tool itself, its test and its two lines (a comment and the step) in
    `ci/check-python-steps.txt`. Everything else must be byte-identical; a file in none of the four classes fails the
    gate. The reviewer reads the list, not the 338-file stat (AGENTS: a diff stat is
    not a measure of change).
  - Two rehearsals, before the PR lands, results in the PR body. (1) One open PR head (the one touching the most
    renamed paths; `gh pr list`): run the open-PR recipe on it (before Wave R lands, `origin/main` in the recipe is
    the Wave R branch) and build its tests. The recipe's ORDER is decided by the rehearsal and recorded: the first
    recipe was `git merge origin/main`, then the script, then commit; the alternative
    is the script first, commit, then the merge, where both sides made the same edit to every renamed line. The
    rehearsal runs both on the same head and writes down each one's conflict count and which won; the briefs carry
    the winner. (2) `release/v0.10` (it exists on `origin`): no real post-rename fix exists before Wave R
    lands, and the reverse of the PR's own diff reproduces the base by construction (vacuous, N4), so make a
    synthetic content edit on the Wave R branch (one changed line in a file the rename rewrites, one in a file it
    leaves alone), rewrite its diff with `--reverse-patch`, `git apply --check` it on a scratch checkout of the line,
    and run `--check` on the result (the old names must be back, the new ones gone).
- Files: everything above; `docs/module-layers.md` here only for the names (W0 follows). Also `rust-modules/
  build_support/install_identities.rs` (outside the directory, F47) and the bare-path files of F48 B2 (a
  `rust-modules/platform/` grep does not find them), and the sites of N1 and N2: `media/src/player/report.rs`,
  `.claude/hooks/release-config-check.py` with its test, `storage/src/main.rs` and `base/src/eventlog/scrub.rs`.
- Gate (its own review): a `make build-bench` before/after table (the PR touches `Cargo.toml` and the
  layout); fw-compat-reviewer (the move touches `os/src/webos/ls2/uri.rs`, a file on its list); both certificate
  re-pins; the old name counted by the F36 and F48 greps before and 0 after (outside the exempt list); the
  `--equivalence` result; `make check`, `make check-nodef` and the ARM cross-build on the PR's own CI; the
  discovered-test diff (moves only); `ci/check-module-layers.py` with `layers.txt` still empty; a `--guest --mock`
  TV boot with the log diff; the `make` input lists (`RUST_INPUTS`, `STORAGE_INPUTS`) printed before and after, equal
  but for the directory name, and the cargo seed key (`tools/cargo-seed.py`) computed over both manifests.
- Risk: the silent sites above, which the second check exists for; a lane open across the rename conflicts on
  every touched line (the script is the mitigation); `--reverse` over-reaching (the bare `os/`).

### W0 (N). Record the plan
- Goal: `docs/platform-ports.md` (this page: the plan with final names; its fact sections quote the old names on
  purpose) and an amendment of `docs/module-layers.md`: steps L15c-L15h, introduced there, mapped to the
  workstreams, and its order (F3) amended from "extraction after all of L15b" to "extraction (W7) after item 2 (W4);
  item 3 (W8) and item 1 (W11) afterwards". The desktop platform satisfies "a second implementation", and an order of
  "after items 2 and 3" would contradict W8, which follows W7 (review C6). The page records H3 and H4 as answered on
  the emulator only, H5 likewise, and H1 and H2 as open: they gate a retail set, not the emulator MVP of W9
  (Decision 8). It also records the owner decisions at the end of the page. W0 does not wait for a retail set.
- Files: `docs/platform-ports.md`, `docs/module-layers.md`. Verify: doc-claim-auditor reports no contradiction. No
  test, no TV, no build-bench.

### W1 (N). The PLATFORM axis, webOS and desktop only
- Goal: `PLATFORM ?= webos`, `ci/platform_axis.py`, the compatibility-axis variables, the two query targets.
  `sim-*` and `macapp` imply `PLATFORM=desktop`. No `mk/` split yet. No behaviour change.
- Files: `Makefile`, `ci/platform_axis.py`, `ci/test_platform_axis.py`, and `DISK_ENTRIES` if a cargo-running
  target is added (`ci/test_disk_preflight.py`).
- Test: `python3 ci/test_platform_axis.py` (includes the stdlib-shadow case, F46) and `ci/test_disk_preflight.py`.
- Verify: `make -s print-flavor print-appid` unchanged plus `print-platform`; the ARM link command diffed before
  and after from `make -n` (a byte-identical binary is NOT claimed); a build-bench table. TV: no.
- Risk: a variable changes through include order; the `make -n` diff catches it.

### W2 (N, also T). Split `tv::Port`; shell and diag move to `app::Platform`
- Goal (critic M4): F5's 27 fields regrouped with the same function pointers.
  - `tv::Services { identity, secure, system, legacy_webos_media }` as in Target shape 1; W3a's once-set
    `StoreMode` is folded into `secure` with the accessor unchanged (critic F2).
  - The seven window and bus entries and the six frame probes leave `plx_os` for the app crate as `Shell` and
    `FrameProbe`; `tv/window.rs` is deleted; `app/{boot,run,bridge}.rs` call `Platform`'s members. The webOS
    implementations stay in `crate::system` (already `[app]`, F2) until W6.
  - `ABSENT` becomes `Services::HOST_TEST` and `Platform::HOST_TEST`.
- Files: `os/src/tv.rs`, `os/src/tv/*.rs`, `src/port.rs`, `src/app/{boot,run,bridge,mod}.rs`, `ci/check-deps.sh` (the
  `frame` gate at `:1097` is rewritten to pin the `Shell` call text to `run.rs`). `os/src/lib.rs` is untouched.
- Test: `make test-crate C=plx_os T=tv`, `make test-hostsim`; a host test pins every `HOST_TEST` answer; the
  `frame` gate's self-test still rejects a new call site. TV: a `--guest --mock` boot, log diff. Risk: low.

### W3 (N, W3b and W3c also T). The three contracts, then neutral types

W3a. Persistence mode (review B2, C4; critic F2, F3; F30, F39, F44; Target shape 4; Decision 7, B).
- Goal: the once-set `StoreMode` (default `Durable`, no `Port` field) with a SEPARATE test override (a
  process-global guarded by `testlock::serial()` and reset on drop, G6), the memory-only authority, the `Ephemeral`
  outcome and the `install_preferences` v2 carrier, all as Target shape 4.
- Files: `os/src/tv/secure.rs`; `plex/src/plex/session.rs`; `plex/src/plex/session/{persistence,async_persistence,
  install_preferences}.rs` and `session/prefs/profile_prefs.rs`; `plex/src/plex/session/migration_tests.rs` (`:779`,
  only if `Retained` gains a field); `plex/src/plex/session_persistence_tests.rs`;
  `session/src/auth/owner.rs` and `owner/attempt_characterization_tests.rs`; `telemetry/src/telemetry/incident.rs`;
  `src/app/adapters/session.rs`; `media/src/route/decision_prefs_characterisation_tests.rs`; and
  `plex/src/plex/CLAUDE.md` (`:501,646` say `peek()` is a write-through cache; F50). Not `tv.rs`, not
  `src/port.rs`, not `src/app/{mod,run,boot}.rs`. Read `plex/src/plex/CLAUDE.md` first.
- Test: `make test-crate C=plx_plex T=session`, `C=plx_session T=auth` and `C=plx_media T=prefs_characterisation` (that
  file is `#[cfg(test)]` under `route/decision.rs:14253`, not `hostsim`; the cases of Target shape 4, on
  `tests/mock_pms.py` and the mock plex.tv, never a real account); every existing session test unchanged;
  a test that sets the mode holds `testlock::serial()` and the setter's guard resets it (G6); `ci/check-deps.sh
  --only workers,globals,forks,fxlanes` and `ci/check-ledger-ratchet.py` show no ledger delta: `secure::` is not
  in the `globals` pattern and the pattern is not extended (F44, F50). TV: no.
- Risk: `session.rs` is the largest file and a gate at one writer leaves another writing plaintext: the data-dir
  scan, not a list of writers, is the check.

W3b. URI evidence (review B3, C5; critic M1, F10, F12; F31, F45; Target shape 6b).
- Goal: `DisplayEvidence` (with the opaque display tag), `UriDriver::display`, `UriNotReady::{DisplayUnavailable,
  Driver}` (replacing `AcbHandle`); `Binding` and the `VP_EXPORTED` comparison move into the LG driver in
  `plx_play_uri`; LG's multi-audio rule and the opt-in move with `ProductQualifier` from `plx_media` to
  `src/app/adapters/backends.rs`, NOT into `plx_play_uri` (its layer may not name media) and with no trait or
  request type moved to `playback_contract` (G3; F49); `OriginalRequest.binding` is deleted; `ProductEvidence` is
  built from driver evidence; `ready_to_start` stays on-thread. webOS behaviour is identical.
- Files: `playback-contract/src/{uri,test_support}.rs`; `media/src/route/{uri_preflight,plan,backend_policy,
  decision_version_tests,decision_resolve_route_tests}.rs`; `media/src/player/adapter.rs` (two tests) and
  `media/src/player/CLAUDE.md:30-56`; `play-uri/src/*`; `src/app/adapters/backends.rs`, `src/app/{run_uri_boot_tests,
  diagnostics}.rs`; `src/dev/{uri_product,uri_exported,uriprobe_tests}.rs`; and two that named the old home of the
  qualifier (G7; F49): `plex/src/plex/session/playback_options.rs:39` (a doc comment) and `docs/module-layers.md:180`.
  Not `run.rs` (it only `include!`s the test file).
- Test: `make test-crate C=plx_media T=route::`, `C=plx_play_uri`, `C=plx_playback_contract`; `make test-hostsim
  T=backends` and `T=uri_boot`. The qualifier's own tests leave `plx_media` with it (`uri_preflight.rs:838-869,
  1490-1512`) and run in the app crate, where a `plx_media` run does not execute them: the hostsim runs are their
  bounded test (`--require-match`: a `T=` that matches nothing fails). The route tests that stay in media run over a
  fake qualifier. The expectations are unchanged, plus: a test that the qualifier is built from the binding AFTER the
  `dev::uri_exported` override (`devtriggers`); `ci/check-module-layers.py` with `layers.txt` empty (no
  `plx_play_uri` item names `plx_media`); the same multi-audio source on both LG paths (ACB accepts, exported refuses); a failed lazy
  ACB handle falls back to Feed; a certified non-LG driver with the opt-in off is admitted and starts; a
  TWO-driver (Feed plus URI) selection that pins today's `Offer::rank` order (C5). fw-compat-reviewer
  (`play-uri/src` is on its list). TV: a `--guest --mock` boot and play, the URI opt-in declines or plays as before.

W3c. Shutdown ownership (review B1, C1, C2; critic F1, F8; F29, F40-F42; Target shape 7).
- Goal: `run_application` returns `Exit { code, release }`; the exit inventory, the leaked `App` with its drop
  probe field (G5), the release status defined from `quits_sdl`'s arm (G4), the `HostGate` and the watchdog disarm
  of Target shape 7; `plex_run` and the sim call `exit_now` in today's order. The C ABI is W9's; W3c ships the
  gate and a fake host. `run_application` stays within its 10-line `fnlen` budget (`ci/check-deps.sh:1176-1178`,
  F51): the leak and the `Exit` mapping are in `app/exit.rs`, and the gate is not edited.
- Files: `src/app/{mod,run,boot}.rs` and a new `src/app/exit.rs`; `src/port.rs`; `src/bin/sim.rs`;
  `gfx/src/{gfx,text}.rs`; `base/src/task/watchdog.rs`. Not `adapters/*` (the drop probe is a `cfg(test)` field of
  `App`, read in the test and set at the two construction sites, `app/boot.rs` and the `run.rs` test rig), not
  `backends.rs`. `gfx/src/gfx/straighten.rs:1272` pins `pub fn init_gl()` by text and survives the new return type
  (F51); it is read, not edited.
- Test: `make test-hostsim T=lifecycle` (boot refusal returns its code; quit; the cases of Target shape 7 through
  the fake host; the drop probe and its control; `release_of` and `quits_sdl` agreeing on both `NativeExit`
  variants; the rewritten source guard), `make test-crate C=plx_gfx T=fault` (injected
  compile and link failures return a `GfxFault`), `C=plx_base T=watchdog`. TV: a `--guest --mock` boot and quit
  with the same last log lines and exit status. Risk: teardown order, which stays as today.

W3d. Neutral types (L15b item 2 prerequisites; part of item 3).
- Goal: the opaque `Backend` tag (critic M8); `sandbox` becomes `native_video`; identity publishes
  `platform`/`platform_version` and `device::Info` loses `codename`, `api` and `name` to a webOS-owned detail (F38,
  F45); the devcaps reader moves behind the platform.
- Files: `os/src/tv/{secure,sandbox,device}.rs`, `os/src/{webos,devcaps}.rs`, `os/src/webos/{caps,jail_repair}.rs`,
  `os/src/keymanager.rs` (the tag constants), `plex/src/plex/identity.rs`, `plex/src/plex/session_persistence_tests.rs`
  (`Backend::Keymanager3` at `:769,1724,1730`; W3a's file too, but W3a is merged before wave 3), the sandbox and `Info` readers of F45
  except `app/{boot,run,bridge,mod}.rs`, `src/app/{diagnostics,playback}.rs`, `src/lab/snapshot.rs`, and the L15
  table in `docs/module-layers.md`. `devcaps::probe()` and `caps()` keep path and signature (the file read moves
  behind `probe`), so their callers `app/mod.rs:537` and five in `run.rs` stay W5's, untouched (grep).
- Test: `make test-crate C=plx_os`, `C=plx_plex T=identity`. A host test serializes and deserializes a `Sealed`
  for each existing tag and compares the JSON with a literal fixture written from today's code; the X-Plex headers
  test (`plex/src/plex/client.rs:1099`) is unchanged. UNVERIFIED whether `tests/fixtures/persistence` already
  pins envelopes; this test is added either way. Risk: a changed tag loses every TV's saved session; the fixture
  is the guard. TV: no.

### W4 (N). The simulator as its own platform (L15b item 2)
- Goal: a second fence section, `[platform desktop]` (no gate code: Wave R already switched the keyword, F1), with
  member `platform_desktop` (a module of the app crate until W7) and its own `Platform` value; the simulator's host
  arms (5 of the fence's 102 `hostsim` lines, sorted by the rule below) move out of `system` and `port` into it; the
  other `hostsim` terms of `webos`, `keymanager`, `system` and `port` go (the simulator's stand-ins in `webos` and
  `keymanager` stay, compiled for tests and `test-support`: see the rule below); `sim.rs` names it (F7).
- Rule for the three configurations. The 102 fenced `hostsim` lines (F10) are cfg lines of both polarities, not 102
  host arms. Each was read and classified by what it gates (the same counts at `e9924f0f9` and on `main` after Wave R).
  - 77 carry a negated `hostsim` (`git grep -h 'feature = "hostsim"' -- <F10's files> | grep -c -e 'not(feature =
    "hostsim")' -e '!cfg!(feature = "hostsim")'`): they gate the TELEVISION arm, so they lose only the `hostsim` term
    and stay in their files (`src/system.rs` included). 17 of them are exactly `all(not(feature = "hostsim"),
    not(any(test, feature = "test-support")))` (`webos/toast.rs:26`) and keep their `not(any(test, ...))` term:
    dropping it would turn a production gate into a test-only one.
  - The other 25 name `hostsim` only positively. 5 move to `platform_desktop`: the host stubs `opaque_region_init`
    and `opaque_route` (`system.rs:423,469`), `port.rs`'s two simulator `SINK` arms (`:11,14`), and the simulator
    table's test (`port.rs:79`, `all(test, hostsim, play-feed)`: it names `app::adapters::hostsim::HOST_SINK`, which
    exists only under `all(hostsim, play-feed)`, so dropping the term would break the plain test build).
  - The other 20 stay:
    - 10 lose only the `hostsim` term. Nine are the simulator's stand-ins for the television's services,
      `any(feature = "hostsim", test, feature = "test-support")` (`keymanager.rs:198`, `webos.rs` six times,
      `webos/jail_repair.rs:44`, `webos/toast.rs:55`): what the simulator runs today (`launch_home`,
      `system_locale`, `sound_output`, `bind_window`, the keymanager client, `deliver` and `call_hbc` among
      them), so more than test seams. They stay in `webos` and `keymanager`, compiled for tests and
      `test-support` as `cfg(any(test, feature = "test-support"))`. The tenth, `port.rs:22`'s `any(hostsim,
      test)`, becomes `cfg(test)`.
    - 6 are the frame-probe no-op stubs (`system.rs:761-772`, `any(feature = "hostsim", not(feature =
      "devtriggers"))`). Every TV build without `devtriggers` compiles them, a RELEASE build included, and
      `app/run.rs` reaches them through the port, so they are re-gated as `cfg(not(feature = "devtriggers"))`.
    - 3 are `cfg_attr(feature = "hostsim", allow(dead_code))` on TV items (`webos/toast.rs:33,40`, `system.rs:22`)
      and stay with their item, and 1 is the `let _ = winp;` statement inside a TV function (`system.rs:173`).
  - Under `cfg(test)` without `hostsim` the webOS table stays as today (`SINK = NoSink`, `DISPLAY = SINK`).
- Order: after W2 and W3d (stacked in its lane). Files: `src/port.rs`, `src/platform_desktop*`, `src/lib.rs`,
  `src/system.rs`, `os/src/webos*.rs`, `os/src/keymanager.rs`, `src/bin/sim.rs`, `ci/module-layers.ini`,
  `ci/check-deps.sh` (the `sink` rule's paths, F25), `ci/mkmacapp.py`, `ci/test_sim_backend.py`,
  `ci/test_test_crate.py`, `.github/workflows/ci.yml:379`.
- Test: `make test-hostsim`; the fenced `hostsim` count (F10's grep) falls from 102 to 0 (77 television gates lose a
  term, 10 stand-ins kept for tests do too, 6 stubs are re-gated on `devtriggers`, 4 lines stay with their TV items,
  5 host arms move; the five numbers, each with the grep that gives it, in the PR); the discovered-test diff;
  `make sim-macos` and `make macapp` booting to Home under ui-sim; replay fixtures unchanged; a build-bench table;
  fw-compat-reviewer (`system.rs`'s Wayland and SDL externs are split). TV: none beyond the wave's. Risk: the 363
  non-fence `feature = "hostsim"` lines (360 cfg lines) remain (W8); F11's mechanism lands in W7.

### W5 (N, W5b also T). The event contract (one lane: W5a -> W5b -> W5c)
- Goal: `HostEvent` with `Option<Action>` (Target shape 2), app-private, consumed by the loop. `drain_sdl`
  (`app/run.rs:1066-1069`) becomes a shell drain; `decode_key`/`rd_u32`, the wheel and `TEXTINPUT` decodes move
  into the SDL decoders unchanged.
- PRs: W5a lifecycle and quit (the hosted stop request arrives as `Quit`). W5b keys and text: step 1 is the
  inventory (review M6): a PR table with one row per consumer of a raw pair (the 7 `screens` files naming
  `WCODE_`/`classify(`, Search editing, Library `page_dir`, the PIN keypad, Player transport, the HUD,
  `app/{events,bridge,run}`) saying what each does on each `Action`; then `Action`, `raw_pair`, `action_of`, the
  `pin_digit` move and the tests of Target shape 2 (C8). W5c the pointer and `Wheel { dy }` and the Magic Remote
  hide (`app/boot.rs:794-836`, UNVERIFIED location). W5d, the exit step (Decision 6, A), is not scheduled until a
  fourth platform is planned.
- Files: `src/app/{events,run,input,mod,lifecycle,recorder,bridge}.rs`, `src/app/boot.rs` (W5c only),
  `src/textinput.rs`, `ui/src/consts.rs`, `screens/src/profiles.rs` (W5b only).
- Test: `make test-hostsim` with the FIFO, `make test-crate C=plx_ui T=consts`; replay tapes replay identically,
  `enc_lifecycle` is byte-equal, `tests/keytable.json` unchanged. TV: W5b only, one `--guest --mock` lease (the LG
  layout is device-only, F16). Risk: the LG `TEXTINPUT` offset (`app/run.rs:1208-1212`) and the sdl2-compat carrier
  must move unchanged.

### W6 (N, accepted T). The shell and `plx_shell_sdl`
- Goal: `rust-modules/shell-sdl` (`[shell_sdl]`) takes window creation (`app/boot.rs:1547-1652`), the swap
  (`app/run.rs:974`), the drain and the stock decode, behind `Shell`/`Surface`. The LG half of `system.rs`
  (SysWMinfo over-allocation, `wl_proxy_marshal` opaque region, the `g_main_context` pump; `system.rs:1-6,34-38`)
  stays in the webOS platform. No Samsung shell is designed here.
- Order: after W6b. Files: `src/app/{boot,run,mod}.rs` (not `bridge.rs`), `src/system.rs`, `rust-modules/shell-sdl/`,
  the workspace `Cargo.toml`, `ci/module-layers.ini`. The source-text pins at `app/boot.rs:705-785` and
  `app/run.rs:11467` (F14) move with the code they search.
- Test: `make test-hostsim`; a build-bench table; `check-nodef`, the ARM cross-build and fw-compat-reviewer; a TV
  lease: boot log diff and the video plane still showing through (opaque region); frame claims only via
  `profile-tv`. Risk: the SysWMinfo overrun (`system.rs:4-5`); that code moves verbatim.
- As landed: `Shell` gained `init` and `open`, and `open` returns the application-private `Surface` (`window`, `swap`,
  `set_swap_interval`, `poll` and an unsafe `quit` that `run::shutdown` calls). The bring-up order is kept: `SDL_Init` and the
  GL request come before the stored session is read, the window after it. The placement is a value, `OpenAsk { title,
  placement, high_dpi }`, so the panel's size with its smaller fallback and the desktop fit (`Placement::Desktop`, `hostsim`
  builds only) are the crate's; the webOS shell hands `init` two hooks, the cursor-sleep hint before `SDL_Init` and the
  relaunch tap after it (`system::webos3_cursor_sleep_hint` and `system::tap_relaunch_payloads`, moved without their `hostsim`
  gates), and the desktop shell none. Text-input state stays in `textinput.rs`; the crate owns the byte layouts and the SDL
  calls. The pins moved with their code: the init order into the crate's `init`, the window order into its
  `open_order_tests`, and the session-before-window order stays in `app/boot.rs`.

### W6b (N, accepted T). Platform-supplied composition, two phases
- Goal: `app::Platform.media` (Target shape 6a). `acb_init` moves behind `early_init` at its present call site
  (`app/boot.rs:2043`); `compose_backends` minus both publications behind `compose`, which returns the installed
  resolver; the app publishes resolver then set in one function. The 37 `play-uri` sites are sorted as 6a says.
  The desktop composes HostSink Feed.
- Order: after W4, W5 and W3b. Files: `src/app/adapters/backends.rs`, `src/app/{playback,run,boot}.rs`, `src/dev.rs`,
  `src/lib.rs`, `src/app/run_uri_boot_tests.rs`, `src/dev/uriprobe_tests.rs`, `src/port.rs`, `src/platform_desktop*`
  and the `sink` rule in `ci/check-deps.sh` (its allowed paths name `app/adapters/backends.rs`, F25).
- Test: `make test-hostsim T=uri_boot` and `make test-crate C=plx_media`, with REAL composition (review C3): success,
  a refused URI install, a controlled boot and zero drivers, each asserting the session's set and resolver
  agree; the PR's CI runs `PLAYBACK_BACKENDS=uri|feed|none`; fw-compat-reviewer (the ACB init path moves). TV: a
  `--guest --mock` lease: Feed plays a mock title, the URI opt-in declines or plays as before, and the boot log
  shows `acb_init` at the same point.
- Risk: `acb_init`'s order relative to `ffi::boot()` and the first frame; moving it into `compose` would change
  its timing (review M5).
- As landed: the seam, not the move (Target shape 6a, "Composition is a SEAM until W11a"). `port.rs` fills
  `Platform.media` with `backends::acb_init` and `backends::compose_native`; the bodies stay in
  `app/adapters/backends.rs` until W11a, because dev and the tests name `UriProduct` and its siblings and the fence
  forbids them naming `port`. `early_init` is called at the old `acb_init` line and `compose` at the old
  `compose_backends` line, so neither phase moved. `dev.rs`, `lib.rs` and the `sink`
  rule are unchanged (`backends.rs` stays an allowed path). The desktop composes the Feed over the host sink and no
  URI driver; its `early_init` is a no-op.

### W6c (N). A URI-only and a two-driver composition play an ordinary title (host tests, before W10)
- Goal (review B3, M8, C5; critic M1): through the real planner, with `FakeUriDriver` (`test_support.rs:143`) carrying
  no LG binding and no Feed driver, an ordinary title resolves to a URI offer and starts. Also: zero drivers
  (typed refusal, Play disabled with a reason through the new `ScreenInputs.backends`); URI only with the
  experimental setting off, the driver certifying itself; a source the driver declines (a `URI_PMS_*` conversion
  or a refusal); and the two-driver case of Target shape 6d.
- Order: after W6b, beside W6. Files: new `media/src/route/*_tests.rs`, `screens/src/inputs.rs` and the Play
  control that reads `backends` (UNVERIFIED which `screens/` file; no other wave-4 lane edits `screens/`), `src/app/bridge.rs` (`:1700`, where `ScreenInputs` is
  filled), tests in `src/app/adapters/backends.rs` and `src/app/run_uri_boot_tests.rs`. No `run.rs` line.
- Test: each owning crate: `make test-crate C=plx_media`, `C=plx_screens`, and `make test-hostsim T=uri_boot`
  (a `plx_media` run alone does not execute the app tests). Risk: if this step needs planner changes they are the
  hidden W10 work critic M1 named, and they land here.

### W7 (N, accepted T). Extract the platform crates (W7a -> W7b)
- W7a: the interface of Target shape 8: `pub mod host` in `src/lib.rs`, `host_interface.rs`, a desktop-shaped root
  compiled against it. W7b: `[platform webos]` becomes `rust-modules/platform-webos` and `[platform desktop]`
  becomes `rust-modules/platform-desktop`. `RUST_LIB` (`Makefile:882`) archives `plx_platform_webos`; the gate
  learns "platform as crate above app". `tv/plane.rs` and `tv::uri` stay in the bottom layer until W11a.
- Order: after W0, W4, W6, W6b, W6c and Wave R. W0's reading (the desktop counts as the second implementation)
  reverses `docs/module-layers.md:22-24`; the owner's yes is Decision 3, A. The owner's condition was H1-H4 from
  the emulator or a set: H3 and H4 returned on the emulator (E6, E8) and are recorded as answered on the
  emulator only; H1 and H2 are open (E9), recorded as open, and gate a retail set, not W7 (nor the emulator MVP of W9,
  Decision 8). `HostEvent` and `Shell` stay app-private.
- The Cargo mechanism of the PLATFORM axis lands here: the ARM recipe passes `RUST_FEATFLAGS`
  (`Makefile:1023-1024`), so each platform crate depends on the app with `default-features = false` and forwards
  every app feature the Makefile passes (`rust-modules/Cargo.toml:78-82`), same defaults.
- Files: `src/lib.rs`, `rust-modules/tests/host_interface.rs`, `rust-modules/Cargo.toml`, the `Makefile` `RUST_LIB`
  and `cargo rustc` lines and (W7a, G8; F52) the new `test-host-interface` target with its `DISK_ENTRIES` entry and a
  line in `check-cargo-unit-hostsim`, `ci/test_ci_split.py` (its `GATES`), `ci/check-staticlib-artifact.py`
  (`CRATE`, F25), `ci/check-module-layers.py`,
  `ci/module-layers.ini`, `docs/module-layers.md`, `ci/build-budgets.json` (re-measure; no raise), and the bottom
  layer's `src/lib.rs` (drops `pub mod keymanager;` and `pub mod webos;`) with its `Cargo.toml`: both pinned, a
  reviewed certificate change (F25).
- Test: `make test-hostsim` and `make test-host-interface` (the integration test, run by no target today; `ci/
  test_disk_preflight.py` and `ci/test_ci_split.py` pass with it); W7a changes the `Makefile`, so its PR carries a
  build-bench table as well as W7b's; `ARM_PROFILE=release` size within budget;
  every undefined symbol of the C objects (`main.c`, `crashtrace.c`, `sentry_context.c`, `starfish.c`,
  `video_plane.c`, ...) resolves in the new archive, by a scripted `nm -u` against `nm --defined-only`; a
  `--guest --mock` TV boot with log diff; fw-compat-reviewer; `--report` shows the platforms and 0 entries; the
  gate package list and the test-name diff (review M11). Risk: a symbol dropped from the archive fails the exe link;
  CI's cross-build catches it.

### W8 (N). The webOS facts and the remaining `hostsim` (L15b item 3, F9)
- W8a, required before W9: review the 20 `target_arch = "arm"` files and the 151 `target_os = "linux"`
  lines in 27 files (F12). An "is webOS" site becomes a platform value or runner; an "is 32-bit ARM" or
  "is Linux" site stays. One PR per directory: os (45), telemetry (41), plex (21), src (20), base
  (11), storage (8, the LS2 helper), play-feed (5). Each reports its count before and after.
- W8b, one row per fact: the library name lists (F27) become per-platform lists supplied through the
  platform (`dynlib!` stays shared); `paths`'s fallback install prefix becomes a platform value; the
  storage helper is packaged only by `PLATFORM=webos`; `main.c` and `starfish.c` are webOS-only.
- W8c, the 363 non-fence `feature = "hostsim"` lines (360 cfg lines and three doc comments), sorted per crate: a
  host difference becomes a platform value or runner; simulator tooling goes under a renamed `sim` feature; a test
  seam goes to `test-support`.
- Verify: the F9/F12 grep count before and after; `ci/check-ledger-ratchet.py` at or below every family
  total (a host difference that needs IO becomes a runner, never a `forks/` line); fw-compat-reviewer on
  W8b; a build-bench table for W8b. Risk: size. W8c blocks nothing Tizen needs; only W8a does.

### W9 (H). The Tizen skeleton: the UI on a dev-token boot, no playback
- Prerequisites: the hello-world's four answers (F28), as the emulator runs left them (E1-E13; what the
  emulator can and cannot answer: F37, Decision 3); H5 (static SDL2 + SDL2_ttf draws text and a GLES2 texture
  through our path, F15): ANSWERED, yes, emulator only (E10); a Samsung Public certificate profile naming each
  device's DUID, kept out of the repo (E2, T7); W8a; W3a-c.
  - H3: Option B draws from the Rust thread. ANSWERED, yes, emulator only (E6); Option A is the
    fallback if a retail run fails. H4: Ecore main-thread handlers plus a Rust loop thread do not
    deadlock. ANSWERED, yes, emulator only (E8).
  - H1: armv7 or aarch64 (T3). H2: whether the NDK-linked `.so` loads under a Public cert (T2, T4). OPEN.
    They gate a retail set, not the emulator MVP (Decision 8) and not W7 (Decision 3, A): both need a retail set
    (E9). The tester build with an on-screen diagnostics and key-code panel has run on the emulator (E11); its
    run on a retail set would answer H1 and H2 and return a retail key table, which replaces E7's and E11's
    emulator codes as the table W5b and W9 build from. The tester request on #1460 covering H1, H2 and H6 is
    posted only with the owner's approval (Decision 5).
- Goal: taken to the TV 10.0 x86_64 emulator as the MVP (Decision 8): the interface on the dev-token path (a
  `--guest --mock` style boot, the PMS advertised as `10.0.2.2`) against the mock server, no playback, labelled
  emulator only. The sign-in against the mock plex.tv is a follow-up check, and the `plex_tv()` loopback rule is not
  widened (E12, "W9 design note"). `[platform tizen]`, `rust-modules/platform-tizen` and `tizen/host/`
  (T7); the hosted contract of Target shape 7 with its fake host; the Ecore key table to `Action` (Target shape 2;
  E7's five codes first, E11's colour button); in-app UDP logging (Target shape 7, E5); `StoreMode::MemoryOnly` (no
  credential is persisted; per Decision 7, B only `client_id`, language and interface resolution are, in the
  `install_preferences` v2 file, and every other setting is forgotten each launch: stated in the PR); identity
  `Tizen`, `native_video: NotApplicable`, `PLAYBACK_BACKENDS=none`, `BackendSet::NONE` with Play disabled and a
  reason; `mk/platform-tizen.mk` with its two configurations, x86_64 first class and armv7 recorded (the PLATFORM
  axis); the CI compile-only job.
- Verify: a build-bench table; the export check against `DllImport`; the webOS ARM link command unchanged
  (`make -n` diff); fw-compat-reviewer; the packaged `.so`'s dependencies inspected, that exact `.tpk` loaded;
  `ci/check-deps.sh` confirms the new thread and the host-table static trip no ledger (UNVERIFIED: the ledgers scan
  machine crates; this crate is runner code).
  - MVP acceptance (Decision 8): an emulator run (x86_64, the TV 10.0 image) against `tests/mock_pms.py`, never a
    real account: the interface draws, the Ecore keys drive it, there is no playback, and the log reaches a UDP
    receiver with no sequence gap (E5). It is labelled emulator only and gives no frame-rate or latency claim (E6).
    The emulator boots on the dev-token path against the mock PMS advertised as `10.0.2.2`; the sign-in against the
    mock plex.tv is a follow-up check, because that override stays loopback-only and is not widened (E12).
  - Retail acceptance, not part of the MVP: a device run on the owner's Samsung set, never a real account, in the
    shape of the MVP: the dev-token boot against `tests/mock_pms.py` running on a PC the set can reach. The sign-in
    against the mock plex.tv is a follow-up here too: the `plex_tv()` override accepts only loopback and is not
    widened (E12), and a retail set has no `sdb shell` (T8), so `sdb reverse` (the only route E12 names; emulator,
    unchecked) is not assumed to reach it. It answers H1 and H2 (E9).
  - The LG TV lock covers neither.
- Risk: store distribution may need Partner status for a native `.so` (report 2b). Sideloading does not. The MVP
  proves the emulator only: a retail set may still fail H1 or H2.

### W10 (H). The Tizen URI playback driver
- Prerequisites: W9, W6c, and H6: `Tizen.Multimedia.Player` shows a mock title beneath our window with
  the alpha model that works (report 2e, open conflict). It also survives a TV-menu round trip (review
  M9). The hello-world plays no media (F28, F37), so H6 needs a step-3 probe and a retail set: the
  emulator cannot test `Tizen.Multimedia` (F37) and the runs did not probe it (E9, E11). The Home round trip of E11
  covers the window and the EGL surface on the emulator only, with no player, so it is not H6. The tester request
  for H6 (with H1 and H2) is posted only with the owner's approval (Decision 5).
- Goal: a `UriDriver` in `plx_platform_tizen` over `PlxHostApi` player calls (T6): capabilities (no
  DTS/TrueHD, token in the query string, no variable speed), `display()` evidence including readiness, and
  the shell's `video_hole`. No LG binding is faked and no default is published (Target shape 6b).
- Verify: `DriverSet` contract tests against the fake host table, and a late completion after stop; a
  device run on mock media. Risk: UNVERIFIED how PMS's decision answers `X-Plex-Platform: Tizen`: measured
  on the mock, then on a household-owned test server only with the owner's consent.

### W11 (T). The OS-neutral Feed sink (L15b item 1) and `plx_acb`, in two halves (critic M3, review M7)
- W11a: `tv/plane.rs`, `tv::uri` and the `plane_*` verbs leave `os` for `plx_acb`; the Feed-side
  results (`resume_acb`, `AcbPlaneUnload`, F31) become neutral; the LG transports are injected into the LG drivers instead of read through globals
  (`legacy_webos_media` disappears, and with it Decision 2's exemption: every hook is then plainly
  required). The exit condition is that neither `media` nor the app crate names
  them (F8; W6b moves the app's names). Until it lands, no document calls the contract neutral. It also
  edits the pinned `os/Cargo.toml` if the `uri-transport`/`feed-sink` features move: a certificate
  review. `ci/check-deps.sh` carries the `os/src/tv/plane.rs` entry of `MACHINES` and its path in the scan `find`
  with the file to `plx_acb`.
- W11b: the ACB bind sequence, `sf_on_event`/`acb_on_event` and `sink_counter_kind` move behind a neutral
  Feed sink (`docs/module-layers.md:407-412`).
- Order: after W7; independent of W9-W10. Verify: the TV log graded before and after (F22),
  fw-compat-reviewer, `profile-tv` for frame claims, the release-profile size budget, a build-bench
  table. Risk: F22; the riskiest webOS change. It gates neither W9 nor W10.

### W12 (H, after W10 and W11). Tizen feed over esplusplayer (optional)
- A Feed sink over `dlopen("libesplusplayer.so")` (T6), after Option B is chosen. Verify: struct layouts
  against the set's binary (report 2e), a device run on mock media. Risk: firmware-dependent ABI and
  certificate policy; decided after W10 ships.

### W13 (N). The macOS app and the Windows simulator as product platforms
- A Keychain key store, and a URI driver over AVPlayer or a Feed sink over VideoToolbox ("A Mac playback
  backend would be a new media engine", `docs/macos-app.md`); Windows stays `sim-linux` under WSLg
  (`Makefile:2571-2595`). In `plx_platform_desktop`, after W7, each its own plan; this plan guarantees the seams.

## Where the Tizen hello-world on `samsung/hello-tpk` fits

- It does not land on main (owner decision). It is committed on the branch (F28) and ran on the TV 10.0 emulator
  on 2026-11-20 (E1-E13): H3, H4, H5, H7 and H8 yes, emulator only (H7 after one shader source change, with one
  optional program still failing, E13); H1 and H2 need a retail set (F37). Its Option B (EGL on a
  Rust thread) is what W9's shell follows unless a retail run fails; its `plx_tizen_*` exports grow into Target
  shape 7. H5 (SDL2 + SDL2_ttf) passed on the emulator at `64c609a46` (E10, Decision 5, emulator only); the
  tester build with an on-screen diagnostics and key-code panel (`1afe1978b`) has also run on the emulator (E11) and,
  as of 2026-11-20, is the next probe for a retail set (H1, H2, a retail key table). H6 needs a retail set and a
  step-3 probe: the tester request on #1460 is posted only with the owner's approval. The branch links nothing from `rust-modules`, so
  it runs in parallel with every wave; its C# host and build commands seed `tizen/host/` and `mk/platform-tizen.mk`
  in W9, whose MVP is accepted on the emulator (Decision 8).

## Later platforms (seam check only)

- iOS/tvOS and Android own the run loop: `plx_shell_sdl` plus a platform crate; Keychain and an AVPlayer URI driver
  on iOS; the 8 android cfgs (F13) and a MediaCodec Feed sink after W11b on Android. They use the hosted contract's
  tick word and thread roles (critic M12); their keys arrive as `Action`, and W5d is due before them.

## Out of scope

- Removing SDL2/SDL2_ttf from `gfx` and `base` (F15). Samsung store submission, Partner membership and certificates
  beyond the developer's own Public profile (E2). HDR and Dolby Vision off webOS (report 2e). Tizen 4.0-5.5 (T2).
  Changing FLAVOR or release lines. Flutter, anywhere (owner decision).

## Decisions for the owner

None open. Decisions 1-8 were answered on 2026-11-20 (Decision 7 after the second reviews, Decision 8 after the
emulator runs E11 and E12); they are recorded below and the body follows them.

## Conceded points

- The earlier rounds' 20 accepted critic points stand, except: round 1 point 5 and round 2 point 3 (keys in the
  sym/wcode namespace) became Target shape 2; `PortApp` (round 1 point 1) is `app::Platform`; round 2 point 1's package-list
  sentence is corrected by F35.

## Owner decisions after approval

- 2026-11-19, W7: wait for the hello-world run. W7 starts only after H1-H4 come back from the emulator or a
  Samsung set. Read with Decision 3 (below): H3 and H4 came back on the emulator, which cannot answer H1 and H2.
- 2026-11-19, amendments proposed to the owner (naming
  `plx_platform_*` / `tv::Services` / `app::Platform`; every hook required with per-group `NONE`; one exported
  `PLATFORM`; screen inputs for backends and sign-in kept; C entry for the Tizen host only): accepted 2026-11-20
  by Decisions 1 and 2. Changed by review: `HostEvent` carries canonical actions, not LG key codes (review M6);
  `default_backend` is dropped (review M8, critic M1); shell and diag sit on `app::Platform`, not `tv::Services`
  (critic M4); the doorway gains the stop/completion and thread contract (review B1, M4).
- 2026-11-19, standing: naming is "platform", not "port"; no runtime capability registry (one platform per
  build); no Flutter anywhere; the plan goes to `docs/` after test results (this page, W0).
- 2026-11-20, emulator run: the hello-world ran on the Samsung TV 10.0 emulator and returned H3 and H4 (yes,
  emulator only; E6, E8). H1 and H2 stay open and need a retail set (E9). The owner said to use this evidence.
- 2026-11-20, Decision 1 (naming): B. Keep `plx_platform_webos`, `_desktop` and `_tizen`; rename the bottom layer
  first. Superseded in timing below: the rename is no longer W6d.
- 2026-11-20, Decision 2 (hooks): C. Every hook is required, except the `legacy_webos_media` group until W11a;
  W11a injects the LG transports into the LG drivers.
- 2026-11-20, Decision 3 (W7 gate): A. W7 may start. H3 and H4 recorded as answered on the emulator only; H1 and
  H2 stay open and gate W9, not W7. `Shell` and `HostEvent` stay app-private. Read with Decision 8: the emulator MVP
  of W9 does not wait for H1 and H2 either.
- 2026-11-20, Decision 4 (order): A. The three contracts (W3a-c) first, then the rest, the Samsung-facing types
  app-private. Cut into the waves of "Workstreams".
- 2026-11-20, Decision 5 (probes): go. H5 was built on `samsung/hello-tpk` for the emulator and passed there (E10,
  `64c609a46`); the tester request on #1460 for H1, H2 and H6 is posted only with the owner's
  approval; the tester build with an on-screen diagnostics and key-code panel ran on the emulator (E11) and is the
  next probe for a retail set.
- 2026-11-20, critic G1-G8 (no owner decision): the critic's findings G1-G8 on the third draft accepted as brief
  amendments; the qualifier stays in the app crate (G3). The owner's decisions above are unchanged.
- 2026-11-20, Decision 6 (keys): A. `Action` at the edge with `raw_pair` translation; the machine, tapes and
  screens are unchanged; W5d before a fourth platform.
- 2026-11-20, after the second reviews (review: no-go on Wave 1; critic: BLOCKED): (a) the bottom-layer rename runs FIRST and
  ALONE, before W0: `plx_platform` becomes `plx_os` (crate, directory, layer `[os]`; fence keyword `[platform X]`),
  with its own review gate (build-bench, fw-compat-reviewer, certificate re-pin, the CI tests and source-text pins
  that embed the name) and a shipped rename script so open PRs can apply it; later briefs use a path map; W6d is
  removed; the platform crates keep `plx_platform_webos`, `_desktop`, `_tizen`. (b) Gating: lanes run one bounded
  `make test-crate C=<crate> T=<filter>` (or `make test-hostsim` where needed), each PR's own CI gates its own head,
  one review reads 2-3 PRs together and is recorded per PR head (`tools/pr-land.py reviewed`), TV checks are
  batched on the commit the result supports, build-bench rides the PR that changes build or layout. (c) Decision 7:
  B. Persist only `client_id`, language and interface resolution, carried by the `install_preferences` file at
  version 2 (adds `client_id`), written only under `MemoryOnly`; no session writer runs in `MemoryOnly`; webOS and
  desktop files stay byte-identical (v1 is `deny_unknown_fields`).
- 2026-11-20, Decision 8 (MVP target): W9 is taken to the TV 10.0 x86_64 emulator as the MVP (the interface on the
  dev-token path against the mock PMS advertised as `10.0.2.2`, no playback, labelled emulator only; the sign-in
  against the mock plex.tv is a follow-up check and the `plex_tv()` loopback rule is not widened, E12). H1 and H2
  still gate a retail set. x86_64 is a
  first-class configuration of `mk/platform-tizen.mk`.

## Open disagreements

None between planner and critic. No owner question is open.
