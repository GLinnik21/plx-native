@AGENTS.md

# Claude Code configuration

Project-specific Claude subagents are available under `.claude/agents/`. Shared skills are exposed
through `.claude/skills`, which is a relative symlink to the canonical `.agents/skills/` tree.
Claude-only hooks and the Rust analyzer plugin are configured in `.claude/settings.json`.

## Worktree gotchas (Claude sessions)

- The Bash tool's "Shell cwd was reset to …" can leave you in the MAIN checkout (`/Users/…/plx-native`), not your worktree: a bare `git checkout -B x` there moves the owner's checkout off `main`. Start branch-switching commands with `cd <your worktree> &&` (or `git -C`), and confirm with `git branch --show-current`.
- A branch an `isolation: "worktree"` subagent created stays checked out in its worktree, so you cannot `git checkout` it elsewhere: `git checkout -B tmp origin/<branch>`, then `git push origin HEAD:<branch>`.
- Finished lanes: `tools/build-gc.sh --worktrees -n` previews, but it lists squash-merged lanes as "unmerged". A clean lane (`git status --short` empty) can be removed with plain `git worktree remove <path>` (no `--force`), which keeps its branch. The auto-mode classifier may refuse a batch removal until the owner says yes.
