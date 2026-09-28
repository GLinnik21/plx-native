#!/bin/sh
# SessionEnd hook: kick off `tools/build-gc.sh --auto` in the background and return immediately.
#
# WHY A HOOK, AND WHY IT CANNOT JUST RUN THE RECLAIM. `tools/build-gc.sh --auto` reclaims
# derived build trees when free space on this volume drops under `PLX_GC_MIN_FREE_GIB` — see that
# script's own header for the incident that motivated it (a volume at 0 bytes free, every writing
# tool dead). Nothing ran it automatically before this hook; a human had to remember `make disk`.
# SessionEnd is the natural trigger (a session ending is exactly when a lane's build tree stops
# being needed for a while), but per code.claude.com/docs/en/hooks, SessionEnd CANNOT BLOCK, shares
# a 1.5s budget across every SessionEnd hook (raised to this hook's own `timeout`, capped at 60s),
# and ignores `async`. `--auto` itself can run for tens of seconds — a `--worktrees` pass alone
# walks `du` over every lane on the machine — so the only way to fire it from here is to DETACH a
# background process and return before the harness's clock has any reason to notice this hook at
# all. `nohup … &` inside its own subshell does that; `</dev/null` and redirecting both streams
# keep the child from holding this hook's own stdio open, which is what would otherwise leave the
# harness waiting on a pipe nobody downstream is going to close.
#
# WHICH CHECKOUT RUNS IT. Not necessarily this one: a SessionEnd firing for a lane under
# `.claude/worktrees/` can fire because that very lane is about to be torn down — the worst
# possible process to hand a background job that is meant to keep running after this session
# exits. `git rev-parse --git-common-dir` names the MAIN
# checkout regardless of which worktree the hook was invoked from (`tools/build-gc.sh` resolves
# `$MAIN` the same way, for the same reason), so this hook always launches the MAIN checkout's
# copy of the script. `$CLAUDE_PROJECT_DIR` is the fallback when git cannot answer at all — no
# repository, a corrupt `.git`, or `git` missing from PATH — rather than doing nothing, since a
# session ending with the volume already full is precisely the case this exists to catch.
#
# CONTRACT: exit 0 always. A hook that fails here costs one missed background sweep, not a wedged
# session — see the script's own fail-open account below.

# Discard stdin without leaving a payload the harness thinks nobody is reading; nothing in the
# SessionEnd JSON changes what this hook does.
cat >/dev/null 2>&1 || true

cwd=${CLAUDE_PROJECT_DIR:-$PWD}
common_dir=$(cd "$cwd" 2>/dev/null && git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || common_dir=""
if [ -n "$common_dir" ]; then
  main=$(dirname "$common_dir")
else
  main=${CLAUDE_PROJECT_DIR:-$cwd}
fi

script="$main/tools/build-gc.sh"
[ -f "$script" ] || exit 0

# Where the launched job's own stdout/stderr goes — belt-and-suspenders beside `--auto`'s own
# internal one-line-per-stage log (`gc_log` inside build-gc.sh), for the rare crash that happens
# before that function is ever reached. Mirrors that function's own path resolution (macOS vs.
# XDG vs. plain ~/.cache) rather than importing it, since this hook must not depend on sourcing
# the script it is about to detach.
if [ -n "${PLX_GC_LOG-}" ]; then
  log=$PLX_GC_LOG
elif [ "$(uname -s 2>/dev/null)" = "Darwin" ]; then
  log="$HOME/Library/Logs/plxnative-build-gc.log"
elif [ -n "${XDG_STATE_HOME-}" ]; then
  log="$XDG_STATE_HOME/plxnative/build-gc.log"
else
  log="$HOME/.cache/plxnative/build-gc.log"
fi
mkdir -p "$(dirname "$log")" 2>/dev/null || true

# THE DETACH. A bare `&` alone is not enough — this shell's own exit can still race the harness
# closing stdio out from under a child that inherited it, and some shells wait on background jobs
# started in the CURRENT shell before an implicit `exit` at end of script. Wrapping the whole
# pipeline in its own `( … ) &` subshell and closing every fd the child could block on (`nohup`
# plus explicit redirects, not just relying on nohup's own stdout handling) is what actually lets
# this script's `exit 0` return without waiting on anything.
( nohup sh "$script" --auto >>"$log" 2>&1 </dev/null & ) >/dev/null 2>&1

exit 0
