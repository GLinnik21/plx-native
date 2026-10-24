# Project instructions

## Project

PlxNative is a production-quality native Plex client for rooted LG webOS 4.5 TVs. Most of the
application is Rust under `rust-modules/` (the application in `src/`, the layers in their own crates); `src/main.c` is the boot/crash shim,
`src/starfish.c` is the StarfishMediaAPIs/ACB seam, and `src/svg.c` rasterizes SVGs. Keep changes
properly factored and finished; "only a demo" is never a reason to leave a shortcut behind.

The target is a cross-compiled 32-bit ARM application with a hardware video plane. Host tests and
the macOS simulator are valuable, but they cannot prove every device behavior.

## Load the right context

- `docs/agent-reference.md` is the detailed architecture, build, portability, and verification
  reference. Read the relevant section before changing a subsystem; do not load the whole file
  when a narrower section is enough.
- Before playback work, read `rust-modules/media/src/player/CLAUDE.md`.
- Before Plex data-layer work, read `rust-modules/plex/src/plex/CLAUDE.md` and `docs/pms-api.md`.
- Before UI work, read `rust-modules/ui/src/CLAUDE.md` and use the shared theme, layout, and widget
  systems instead of adding screen-local visual primitives. Before touching a screen, also read
  `rust-modules/screens/src/CLAUDE.md`; the restructure's design record is
  `docs/ui-restructure-spec-v4.md`, which `rust-modules/ui/src/lib.rs` cites by section number.
- Repository skills live in `.agents/skills/`. Use the matching skill whenever its description
  fits the task; the TV, simulator, FFI, release, and verification workflows have non-obvious
  constraints that generic commands miss.

## Working rules

- **Never write an ad-hoc mechanism when a shared one exists.** Before building any behaviour, find
  the mechanism the codebase already has for it and use it: focus travel and key routing, motion
  and springs, glass and materials, scrolling and chrome, captions, menus and popovers, data
  fetching. If the shared one almost fits, extend it in place for every caller. If it truly cannot
  do the job, stop and say why before writing a new one, and the new one lands in the shared layer,
  never in a screen. PR #843's library redesign routed the Up key by hand, so Up from the
  Recommended page's breadcrumb could not reach the top navigation, and its new header pills had
  no transitions, while the shared `FocusEngine` and the nav pill's animated `TabStrip` already
  existed. `rust-modules/ui/src/CLAUDE.md` (rule 4) names the ready-made ones for UI work.
- **Trunk-based development, and `main` takes SQUASH merges only.** Work happens on short-lived
  branches or worktrees cut from `main`. Commit as often as is useful on a branch — there is no
  limit on how many commits a branch carries and no need to tidy them. When the work is verified it
  lands on `main` as ONE squash commit whose message is the change's own account: the PR's squash
  merge on GitHub, never a squash made locally and pushed, never a fast-forward of a working
  branch's history and never a merge commit. To bring a moving `main` in, merge it into the branch. A fleet of lanes
  integrates into its integration branch however it likes; what reaches `main` is the squash of the
  whole. The reason is the history itself: `main` is read by `git log`, by the release audit and by
  `git bisect`, and a trunk of "fix typo" / "wip" / merge commits pollutes all three. Push `main`
  only when the user asked for it.
- **A diff stat is not a measure of change.** `dec32f2e` ran rustfmt over all of `ui/`, so
  `detail.rs` read +2416 lines for +113 characters of content. `git diff -w` does not rescue it —
  that ignores whitespace *within* a line, not one line split into three. Size a change by content
  (`git show <rev>:<path> | tr -d '[:space:]' | wc -c`, before and after); a near-zero delta means
  reflow. Then diff the symbol sets to find what actually moved.
- **New module references only point down, and only the webOS port names webOS.**
  `ci/module-layers.ini` declares the crates `rust-modules/src` is being split into, and
  `ci/check-module-layers.py` (in `make check`) fails on a reference that names a layer its own
  layer may not use, on a module placed in no layer, and on a reference from outside
  `[port webos]` to `webos`, `keymanager`, `system`, `player::ffi` or `port`. Fix the reference
  (`docs/module-layers.md` says where code belongs and how to cut an upward name) instead of
  adding a (file, member) pair to `ci/allow/layers.txt`, which is empty and only shrinks.
- **Logic never performs IO.** A machine (a screen, a `data/` store, the session owner, the player's
  foreground lifecycle, a route planner) starts no worker, reads no process-global registry and has
  no `cfg(test)` fork: its work leaves as a typed effect the app's runners execute, and every result
  returns as one addressed event. `ci/check-deps.sh` freezes today's sites in the ledgers under
  `ci/allow/{workers,globals,forks,fxlanes}/`, which only shrink: a new site fails `make check`, and
  a migration lands by deleting lines (`docs/sans-io.md`).
- **Re-basing a stacked PR after the one below it squashes:** on the upper branch, `git merge
  <lower's final head>`, `git merge <squash>^`, `git merge -s ours <squash>`, then
  `git merge origin/main`. `-s ours` on the squash alone drops whatever `main` gained before it.
- `gh pr merge --auto --squash --match-head-commit <sha>` can fail just after a push with "expected
  head oid does not match"; wait ~15 s and retry. It is a race, not a real mismatch.
- Preserve unrelated user changes and generated artifacts. Never clean or reset a dirty tree to
  make a task easier.
- This repository is public. Never publish values from gitignored private files such as `.tv-host`,
  `.tv-mac`, `src/config.local.h`, `tests/manifest.local.json`, `pkg/auth.json`, `pkg/lab.json`, or
  the other paths enumerated by `.claude/hooks/outbound-guard.py` — including the per-user copies of
  the TV address and MAC, `~/.config/plxnative/tv-host` and `tv-mac` (the files a worktree reads;
  see `docs/agent-reference.md`). Use the documented placeholders.
- webOS native behavior is under-documented. When an answer depends on platform behavior, verify it
  from primary documentation, vendored source, firmware inventories, or the device binaries; do not
  infer behavior from a symbol name or from a webOS web-app example.
- Fixing a reported bug starts with a regression test or reproducible artifact that fails against
  the broken behavior. Observe the failure, implement the fix, then observe it pass.
- Do not use `make -p` or `make -pn`; recursive variables such as `TV` are misleading there. Use
  the `make -s print-*` query targets documented in `docs/agent-reference.md`.
- Edition 2021 keeps a `while let`/`match` scrutinee's temporaries alive for the whole body:
  `while let Some(x) = m.lock().next() { io(x) }` holds the lock across `io`. Bind first
  (`let next = m.lock().next();`) whenever the body blocks (#908).

## Build and verification

- **A gate result is evidence about the exact tree it ran on, and that is not automatically the
  tree you hand over.** Gates run on the COMMITTED tree: commit, confirm `git status --short` is
  empty, run the gates, confirm it is empty again (restoring `make check`'s own self-test debris),
  then push. A lane once reported `make check` 3686/0, a clean `--no-default-features` and a real
  ARM link for fixes that were sitting on disk and never committed; CI failed to compile it in 89
  seconds. Report the `test result:` line itself rather than a summary — `| tail -n 25` eats it
  when several stages run.
- `make check` runs the fast host unit suite and lint gate. Use it for ordinary Rust changes.
  `make check` is bounded machine-wide to TWO concurrent runs (two `flock` slots in
  `tools/check-lock.py`, shared by every worktree): a third invocation waits and prints EVERY
  holder's pid/worktree/start time every 60 s rather than compiling alongside them, because
  concurrent cold builds thrash one Mac far worse than queuing (measured 2026-09-28: a lone run
  ~10 min, seven at once made one take 60 min). It was one at a time until 2026-10-06, when two at
  once was measured on an Apple M4 (10 cores, 16 GB): a lone warm run ~185-205 s, a pair finishes
  in 0.67-0.76 of the back-to-back time (warm, cold, cold without the seed cache), 5-6 GB peak, no
  swap growth, each run of a pair 1.35-1.55x slower than alone. Three at once was not measured and
  is expected to swap on 16 GB, so two is the ceiling. The default is two only on a host with at
  least 8 cores and 14 GiB of RAM (else one); `PLX_CHECK_SLOTS=1` restores one at a time and
  `PLX_CHECK_LOCK=off` bypasses the lock. Whatever the slot count, a second run from the SAME checkout
  waits for the first (two share one cargo target directory and scratch files). `make build-bench*`
  takes ALL the slots (`--exclusive`)
  and no new check starts once it has asked. Never launch `make check` in the foreground with a
  short tool timeout — a queued run can wait a long time before it even starts building.
- Two `make check` traps: `ci/check-deps.sh`'s libm lint reads any `.log(` call as `f64::ln`, so
  don't name a method `log`; every new `MAX_*` / `*_MAX_*` constant needs a classification in
  `ci/list-caps.ini`.
- While iterating on one crate, `make test-crate C=plx_ui [T=filter] [DEPS=1]` builds and runs only
  that crate's lib tests in `make test-fast`'s incremental tree. It is a loop, not a gate.
- `make test-crate` does NOT enable `hostsim`: tests under `#[cfg(all(test, feature = "hostsim"))]`
  (the application crate's `app/run.rs` `lifecycle_regression_tests`, the player-return rig;
  `plx_media`'s `player/engine.rs`, `pump.rs`, `route/decision.rs`) run as `make test-hostsim
  [C=<crate>] [T=filter]`. `C` defaults to the application crate; `C=plx_media` (or `C=media`)
  runs that crate's `hostsim` tests, the same ones `make check`'s hostsim pass runs for it. It runs
  `cargo test --lib -p <crate> --features hostsim` on the pinned
  toolchain with a throwaway `PLXNATIVE_RUNTIME_DIR` (removed afterwards), incrementally in its own
  gitignored `rust-modules/target-hostsim`: inside the checkout, so `make disk` and
  `tools/build-gc.sh` see it and it goes with the worktree, like `target-fast`. A `T=` that matches
  no test FAILS (libtest reports `0 passed` and exits 0 for a mistyped name), which includes a
  correctly spelled test that lives in another crate than `C`. Do not hand-roll a
  cargo run whose `CARGO_TARGET_DIR` points into `/tmp`: nothing tracks those trees, and 28 of them
  (45.6 GiB) had piled up there when this target replaced the recipe. The "matches nothing, exits 0" trap
  still applies to `make test-crate T=` and `make test-fast T=`. `git add` explicit paths: an
  untracked `rust-modules/target-*` that is not listed in `.gitignore` is not ignored, and
  `git add -A rust-modules` commits it.
- `make sim-macos` / `make` fail with `make: cargo: No such file` from a non-login shell: prefix
  `PATH="$HOME/.cargo/bin:$PATH"`. Test builds deny warnings, so a new `App` field must be read
  (`dead-code`) and initialised at BOTH construction sites (`app/boot.rs` and the `app/run.rs` tests).
- A PR that touches `Makefile`, `Cargo.toml`, `Cargo.lock`, `build.rs`, `.cargo/` config,
  `.github/workflows/` or the module layout pastes a before/after `make build-bench` table in its
  body (`docs/agent-reference.md`, build section); the tool takes the `make check` lock exclusively
  (every slot), so it waits for running checks and nothing else builds while it measures.
  Dependabot PRs (`.github/dependabot.yml`) are exempt: a bot cannot run it, so whoever merges a grouped
  bump measures first when it changes the toolchain or the dependency set the build compiles.
- `make` performs the ARM cross-build. Do not assume a host-only green result proves the target
  still builds.
- A plain local `make` / `make deploy` compiles the ARM library with the fast `tvdev` cargo profile
  (no LTO); CI, `RELEASE=1`, `SYMBOLS=1` and `FLAVOR=stable|nightly` keep the fat-LTO `release`
  profile. That binary is larger and slower in per-frame code, so a size-budget check needs
  `ARM_PROFILE=release`. Timing runs enforce it themselves: `tests/run.py --fps*` builds with it
  under `--build` and, like `tools/profile-graphics` and `tools/tv-sched-trace.sh`, refuses a deployed
  `tvdev` binary (`make deploy` records the profile on the set); build with `make ARM_PROFILE=release
  deploy` before any other FPS or smoothness measurement (`docs/agent-reference.md`, "ARM_PROFILE").
- After editing `rust-modules/src/**/*.rs` or a layer crate's `rust-modules/*/src/**/*.rs`, also check the shipping feature set with
  `make check-nodef` (`cargo check --lib --no-default-features`, non-incremental, in the checkout's
  own `target/`: it shares the third-party units `make lint` built and adds no tree) when the
  Claude-only release hook did not run. Keep the `CARGO_INCREMENTAL=0` prefix on every other direct
  `cargo` call: they bypass `make`,
  so the Makefile's linked-worktree setting cannot reach them, and a one-shot gate has no use for
  a multi-gigabyte incremental cache. `tools/build-gc.sh` also installs
  `.claude/worktrees/.cargo/config.toml` (`plx-build-gc-policy`) with `incremental = false`,
  `[profile.dev] debug = "line-tables-only"` and `debug = false` for third-party packages as a
  backstop; the env var outranks that file and states the intent where a reader can see it.
- **`make disk` before and after a fleet.** Build trees are per-checkout; the cargo
  **incremental cache**, not FFmpeg, is most of the bulk.
  `tools/build-gc.sh --incremental|--lanes|--stale|--all` deletes only rebuildable output. After tearing
  a fleet down, run `--worktrees` (removes finished lanes — clean, unlocked, already on `origin/main` —
  which `git branch --merged` cannot see once they are squash-merged; the removal takes the lane's
  IGNORED files too, so `-n` lists the ones that are not build output, e.g. `src/config.local.h`) and then `--orphans` (lane
  target dirs live outside the repo under `$PLX_FLEET_DIR` and outlive their worktree).
  `docs/agent-reference.md` keeps the measurements behind this. None of this has to be run by
  hand any more: `tools/build-gc.sh --auto` reclaims on its own, gated by free space, from a
  `SessionEnd` hook and the optional hourly `make disk-watch` launchd agent. Past `--orphans` and
  an idle-guarded `--worktrees` (which also keeps any lane holding an ignored file that is not build
  output, and logs which) it follows the PLANNER's order (`tools/disk-budget.py`,
  `docs/disk-budget.md`): `build-gc.sh --evict PLAN` is the one narrow deleting mode, and it
  re-checks every path of a plan against the live machine immediately before it renames it away
  (a live or idle-window-young tree is skipped, a path that is not a derived build tree is
  refused). It never evicts the cargo seed, and unlike the other modes it does not stop for a
  held FFmpeg cache lock. Preview a plan with `python3 tools/disk-budget.py reclaim --to GIB
  --emit > plan; tools/build-gc.sh --evict plan -n`; never run `--evict` for real from a test
  or an experiment outside a fixture. The budget is also checked at every build entry: each
  cargo-running `make` target (`make -s print-disk-entries` lists them) takes `disk-preflight` as an
  order-only prerequisite, which is `python3 tools/disk-budget.py preflight`. At or above the
  comfort line (`PLX_GC_MIN_FREE_GIB`, 20) it is one `statvfs` and a Python start (tens of
  milliseconds); below it, it evicts through the planner and `--evict`, which spare the tree being
  built and every live tree (the current checkout's incremental dirs in its other trees go only when
  nothing cheaper covers the build), and it refuses only when the whole ladder cannot cover the build
  plus the `PLX_MIN_FREE_MB` reserve. That eviction is real deletion time before cargo starts, so a
  `make` below the comfort line that sits in the preflight is working, not hung. A new
  cargo-running target joins `DISK_ENTRIES` in the Makefile (`ci/test_disk_preflight.py` fails
  until it does), and a `$(MAKE)` line inside an entry passes `PLX_DISK_BUDGET_DONE=1`. `make disk`
  stays the report to read before intervening yourself, or when the volume needs relief faster than
  the automatic trigger provides it.
- Use the `which-tier` skill to choose between host checks, `ui-sim`, and real-device verification.
  Pixel output, LG text rasterization, video-plane composition, performance, and native playback
  generally need the TV before being called verified.
- **A frame drop is diagnosed with the tools that point at it, not guessed at.** Any late frame, fps
  dip or "I see it stutter" on the TV goes straight to the `profile-tv` skill: Mali HWCNT, the
  every-frame legs and `tools/tv-sched-trace.sh`, attributing the late frames' time before any cause
  is named. A count of late frames, an A/B between two builds, or a reading of the heartbeat line
  is not a diagnosis, and "probably the upload" is not a finding. State a cause only with the
  counter or trace evidence behind it; what was not measured is reported as not determined.
- A main-thread stall (one long `navcommit=`/`results=` leg on a `FRAMEDROP` line, `--arm
  framedrop=N`) is attributed by wrapping that leg's calls in temporary
  `plx_base::diag::spans::span("label", || …)` on a scratch branch: the labels print in that
  frame's `spans=` field, so one deploy names the call.
- FFI, linkage, `dynlib!`, Starfish, ACB, curl, or bundled-FFmpeg changes require the
  `fw-compat-reviewer` custom agent (`fw_compat_reviewer` in Codex), or the equivalent manual
  review, before push.
- After behavior changes, use the `doc-claim-auditor` custom agent (`doc_claim_auditor` in
  Codex), or perform the same audit, to find prose that the change made false. It reports
  contradictions, not missing documentation.

## Television and release safety

- There is one physical television. Before any command that deploys, runs, drives, captures, or
  tests it, use the `tv-lock` skill and acquire the repository TV lock. Prefer `ui-sim` when the
  device is unnecessary or busy. Read-only status/log collection is the documented exception.
- **Never write to a real person's Plex account.** Boot with `--guest` and verify the identity
  line in the log before sending any key; never press `ok` on a card whose `ratingKey` you have not
  read from the log; reopen a screen by relaunching with `--screen` rather than walking Home. A
  Continue Watching tile resumes playback on OK whenever Settings > Playback > Continue Watching
  is "Play" (the default, "Open details", opens the page) — and the setting is saved on the set
  PER PROFILE (a profile that never chose reads the install's older value), so never assume which
  one a given install, or a given profile on it, has. In the default the card's hold menu opens ON a
  Play/Resume row, so hold-then-OK plays too. That press is how two separate sessions started
  real playback on the owner's account. Seeding, scrobbling, unscrobbling, marking watched and
  "Remove from Deck" are all writes to a household's actual viewing record — the managed and Guest profiles
  are real users too. When screenshot or demo work needs particular content, mock the server.
  So is the plex.tv Watchlist: a debug build refuses every add/remove unless the mock's discover
  stand-in is armed or `plxnative-watchlist-live` exists in the install's runtime root. **Never
  create that file unless the owner asks for it in that same session** — with it, a debug build
  writes to the owner's real Watchlist. `.claude/hooks/watchlist-live-guard.py` refuses a command
  that would (`docs/watchlist.md`, "The live switch").
- Hold the TV lock around the device commands themselves and release as soon as the device work
  pauses. Do not hold a lease while building, reading screenshots or writing Markdown; other lanes
  queue behind it.
- A dev-token boot (`--guest --mock`) registers its server undescribed, so `owned` is false and
  owned-only features (the PMS notification feed) stay dark. Test them with `tools/tv-session.sh up
  --mock-plextv=PORT --screen login`, which signs in through the mock's plex.tv; never vouch the
  rule in code.
- `tools/mock-guest.py` can refuse a freshly started LAN mock ("configured PMS must answer as
  tests/mock_pms.py") for a few minutes while loopback already answers; retry rather than debugging
  the app.
- The tracked default is `FLAVOR=debug`. Any stable install, package, deploy, or release action
  must name `FLAVOR=stable` explicitly and follow the `cut-release` skill. Do not bypass the stable
  guard or the TV lock unless the user explicitly directs the exceptional action.
- `RELEASE=1` must be present on every make invocation that builds or ships the release artifact;
  it is not persistent between commands.

## Harness-specific files

- `.agents/skills/` and `.agents/agents/` are canonical shared content.
- `.claude/skills` and `.claude/agents/*.md` are compatibility symlinks. Claude-only hooks and
  settings remain under `.claude/`.
- `.codex/agents/*.toml` are thin Codex wrappers around the canonical prompts. They inherit the
  parent model and reasoning effort unless a future task deliberately changes that policy.
