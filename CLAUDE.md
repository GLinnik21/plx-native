@AGENTS.md

# Claude Code configuration

Project-specific Claude subagents are available under `.claude/agents/`. Shared skills are exposed
through `.claude/skills`, which is a relative symlink to the canonical `.agents/skills/` tree.
Claude-only hooks and the Rust analyzer plugin are configured in `.claude/settings.json`.

## Worktree gotchas (Claude sessions)

- The Bash tool's "Shell cwd was reset to …" can leave you in the MAIN checkout (`/Users/…/plx-native`), not your worktree: a bare `git checkout -B x` there moves the owner's checkout off `main`. Start branch-switching commands with `cd <your worktree> &&` (or `git -C`), and confirm with `git branch --show-current`.
- A branch an `isolation: "worktree"` subagent created stays checked out in its worktree, so you cannot `git checkout` it elsewhere: `git checkout -B tmp origin/<branch>`, then `git push origin HEAD:<branch>`.
- Finished lanes: `tools/build-gc.sh --worktrees -n` previews. It judges a lane against `origin/main`, so a squash-merged lane reads removable once `git fetch` has brought the squash in; it still reads "unmerged" while `origin/main` predates the squash, or after `main` has changed that lane's lines again. A clean lane (`git status --short` empty) can be removed with plain `git worktree remove <path>` (no `--force`), which keeps its branch but takes the lane's IGNORED files too (`git status --short --ignored` lists them; `--worktrees -n` lists the ones that are not build output, e.g. `src/config.local.h`, and `--auto` keeps such a lane). The auto-mode classifier may refuse a batch removal until the owner says yes.
- The tv-lock guard hook reads the lane only from a `PLX_TV_LOCK_LANE=<worktree>` prefix at the very START of the Bash command: no `sleep;`, `cd &&` or second statement before it. Run `tv-lock.sh acquire` as its own call, then one device command per call.
