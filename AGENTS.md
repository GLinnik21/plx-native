# Project instructions

## Project

PlxNative is a production-quality native Plex client for rooted LG webOS 4.5 TVs. Most of the
application is Rust under `rust-modules/src/`; `src/main.c` is the boot/crash shim,
`src/starfish.c` is the StarfishMediaAPIs/ACB seam, and `src/svg.c` rasterizes SVGs. Keep changes
properly factored and finished; "only a demo" is never a reason to leave a shortcut behind.

The target is a cross-compiled 32-bit ARM application with a hardware video plane. Host tests and
the macOS simulator are valuable, but they cannot prove every device behavior.

## Load the right context

- `docs/agent-reference.md` is the detailed architecture, build, portability, and verification
  reference. Read the relevant section before changing a subsystem; do not load the whole file
  when a narrower section is enough.
- Before playback work, read `rust-modules/src/player/CLAUDE.md`.
- Before Plex data-layer work, read `rust-modules/src/plex/CLAUDE.md` and `docs/pms-api.md`.
- Before UI work, read `rust-modules/src/ui/CLAUDE.md` and use the shared theme, layout, and widget
  systems instead of adding screen-local visual primitives. Before touching a screen, also read
  `rust-modules/src/screens/CLAUDE.md`; the restructure's design record is
  `docs/ui-restructure-spec-v4.md`, which `ui/mod.rs` cites by section number.
- Repository skills live in `.agents/skills/`. Use the matching skill whenever its description
  fits the task; the TV, simulator, FFI, release, and verification workflows have non-obvious
  constraints that generic commands miss.

## Working rules

- **Trunk-based development, and `main` takes SQUASH merges only.** Work happens on short-lived
  branches or worktrees cut from `main`; when a piece of work is verified it lands on `main` as ONE
  commit (`git merge --squash <branch>` on `main`, then a single commit whose message is the
  change's own account), never as a fast-forward of a working branch's history and never as a
  merge commit. A fleet of lanes integrates into its integration branch however it likes; what
  reaches `main` is the squash of the whole. The reason is the history itself: `main` is read by
  `git log`, by the release audit and by `git bisect`, and a trunk of "fix typo" / "wip" / merge
  commits pollutes all three. Push `main` only when the user asked for it.
- **A squash onto a MOVING `main` silently reverts it.** `git reset --soft origin/main` keeps the
  index, so any commit that landed on `origin/main` after your picks shows up inside your squash as
  a line-for-line reversal — and the reversal compiles, so every gate stays green. PR #156 undid
  #153 exactly this way. Before pushing any squash, run `git diff --stat HEAD^ HEAD` and confirm
  every path is one the branch meant to touch; anything else is `git checkout origin/main --
  <path>`, amend, re-gate. Prefer squashing onto the same base the picks were made on and rebasing.
  `git fetch -q` can fail silently, so check `git log HEAD..origin/main` after fetching.
- **A diff stat is not a measure of change.** `dec32f2e` ran rustfmt over all of `ui/`, so
  `detail.rs` read +2416 lines for +113 characters of content. `git diff -w` does not rescue it —
  that ignores whitespace *within* a line, not one line split into three. Size a change by content
  (`git show <rev>:<path> | tr -d '[:space:]' | wc -c`, before and after); a near-zero delta means
  reflow. Then diff the symbol sets to find what actually moved.
- Preserve unrelated user changes and generated artifacts. Never clean or reset a dirty tree to
  make a task easier.
- This repository is public. Never publish values from gitignored private files such as `.tv-host`,
  `.tv-mac`, `src/config.local.h`, `tests/manifest.local.json`, `pkg/auth.json`, `pkg/lab.json`, or
  the other paths enumerated by `.claude/hooks/outbound-guard.py`. Use the documented placeholders.
- webOS native behavior is under-documented. When an answer depends on platform behavior, verify it
  from primary documentation, vendored source, firmware inventories, or the device binaries; do not
  infer behavior from a symbol name or from a webOS web-app example.
- Fixing a reported bug starts with a regression test or reproducible artifact that fails against
  the broken behavior. Observe the failure, implement the fix, then observe it pass.
- Do not use `make -p` or `make -pn`; recursive variables such as `TV` are misleading there. Use
  the `make -s print-*` query targets documented in `docs/agent-reference.md`.

## Build and verification

- **A gate result is evidence about the exact tree it ran on, and that is not automatically the
  tree you hand over.** Gates run on the COMMITTED tree: commit, confirm `git status --short` is
  empty, run the gates, confirm it is empty again (restoring `make check`'s own self-test debris),
  then push. A lane once reported `make check` 3686/0, a clean `--no-default-features` and a real
  ARM link for fixes that were sitting on disk and never committed; CI failed to compile it in 89
  seconds. Report the `test result:` line itself rather than a summary — `| tail -n 25` eats it
  when several stages run.
- `make check` runs the fast host unit suite and lint gate. Use it for ordinary Rust changes.
- `make` performs the ARM cross-build. Do not assume a host-only green result proves the target
  still builds.
- After editing `rust-modules/src/**/*.rs`, also check the shipping feature set with
  `CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path rust-modules/Cargo.toml --lib
  --no-default-features` when the Claude-only release hook did not run. Keep the
  `CARGO_INCREMENTAL=0` prefix on this and on every other direct `cargo` call: they bypass `make`,
  so the Makefile's linked-worktree setting cannot reach them, and a one-shot gate has no use for
  a multi-gigabyte incremental cache. `tools/build-gc.sh` also installs
  `.claude/worktrees/.cargo/config.toml` (`plx-build-gc-policy`) with `incremental = false`,
  `[profile.dev] debug = "line-tables-only"` and `debug = false` for third-party packages as a
  backstop; the env var outranks that file and states the intent where a reader can see it.
- **`make disk` before and after a fleet.** Build trees are per-checkout and nothing collects
  them for you; the cargo **incremental cache**, not FFmpeg, is most of the bulk.
  `tools/build-gc.sh --incremental|--lanes|--all` deletes only rebuildable output. After tearing
  a fleet down, run `--worktrees` (removes finished lanes — clean, unlocked, already on `main` —
  which `git branch --merged` cannot see once they are squash-merged) and then `--orphans` (lane
  target dirs live outside the repo under `$PLX_FLEET_DIR` and outlive their worktree).
  `docs/agent-reference.md` keeps the measurements behind this.
- Use the `which-tier` skill to choose between host checks, `ui-sim`, and real-device verification.
  Pixel output, LG text rasterization, video-plane composition, performance, and native playback
  generally need the TV before being called verified.
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
  Continue Watching tile RESUMES PLAYBACK on OK, which is how two separate sessions started real
  playback on the owner's account. Seeding, scrobbling, unscrobbling, marking watched and "Remove
  from Deck" are all writes to a household's actual viewing record — the managed and Guest profiles
  are real users too. When screenshot or demo work needs particular content, mock the server.
- Hold the TV lock around the device commands themselves and release as soon as the device work
  pauses. Do not hold a lease while building, reading screenshots or writing Markdown; other lanes
  queue behind it.
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
