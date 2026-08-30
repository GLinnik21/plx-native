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
  systems instead of adding screen-local visual primitives.
- Repository skills live in `.agents/skills/`. Use the matching skill whenever its description
  fits the task; the TV, simulator, FFI, release, and verification workflows have non-obvious
  constraints that generic commands miss.

## Working rules

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

- `make check` runs the fast host unit suite and lint gate. Use it for ordinary Rust changes.
- `make` performs the ARM cross-build. Do not assume a host-only green result proves the target
  still builds.
- After editing `rust-modules/src/**/*.rs`, also check the shipping feature set with
  `cargo +nightly check --manifest-path rust-modules/Cargo.toml --lib --no-default-features` when
  the Claude-only release hook did not run.
- Use the `which-tier` skill to choose between host checks, `ui-sim`, and real-device verification.
  Pixel output, LG text rasterization, video-plane composition, performance, and native playback
  generally need the TV before being called verified.
- FFI, linkage, `dynlib!`, Starfish, ACB, curl, or bundled-FFmpeg changes require the
  `fw_compat_reviewer` custom agent (or the equivalent manual review) before push.
- After behavior changes, use the `doc_claim_auditor` custom agent (or perform the same audit) to
  find prose that the change made false. It reports contradictions, not missing documentation.

## Television and release safety

- There is one physical television. Before any command that deploys, runs, drives, captures, or
  tests it, use the `tv-lock` skill and acquire the repository TV lock. Prefer `ui-sim` when the
  device is unnecessary or busy. Read-only status/log collection is the documented exception.
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
