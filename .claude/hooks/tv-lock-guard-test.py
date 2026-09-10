#!/usr/bin/env python3
"""Cases for the TV-lock guard's classifier. `python3 .claude/hooks/tv-lock-guard-test.py`.

Host-only, no television and no lock state involved: it imports the guard and asks it to classify
command lines. The pairs below are the ones that matter, and half of them are FALSE POSITIVES —
the guard's real failure mode is not missing an `ssh root@`, it is refusing `pgrep -fl
"…|make deploy"`, which is the pre-flight that asks whether anybody is on the set. Both of the
quoting cases here fired for real in the first two minutes the hook was live.

Addresses below are RFC 5737 documentation ranges, never this household's: the TV's real address
lives in the gitignored `.tv-host` and nowhere in the tree (docs/distribution.md §"private data").
The guard keys on `root@`, not on any particular host, so a placeholder tests it exactly.
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("guard", os.path.join(HERE, "tv-lock-guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

BLOCK, ALLOW = True, False
CASES = [
    # --- must be blocked: everything that reaches the television --------------
    (BLOCK, "make deploy"),
    (BLOCK, "make FLAVOR=debug RELEASE=1 deploy"),
    (BLOCK, "make test"),
    (BLOCK, "make -C /repo run RUN_SECS=30"),
    (BLOCK, "./tests/run.py --fps"),
    (BLOCK, "python3 tests/run.py --filter seek"),
    # `--list-foo` is a different (unrecognised) flag, not `--list` — a substring match here would
    # wrongly allow it.
    (BLOCK, "tests/run.py --list-foo"),
    (BLOCK, "tools/tv-session.sh up --screen home"),
    (BLOCK, "tools/tv-session.sh key down ok"),
    (BLOCK, "tools/capture-screen.sh out.png DISPLAY"),
    (BLOCK, "ssh root@192.0.2.10 'cat /tmp/plxnative-events.log'"),
    (BLOCK, "sshpass -p alpine scp pkg/plxnative root@192.0.2.10:/tmp/"),
    (BLOCK, "echo hi && make deploy"),
    (BLOCK, "./tests/run.py --fps | tee out.log"),
    (BLOCK, "for f in stable debug; do ssh root@1.2.3.4 fuser x; done"),
    (BLOCK, 'echo "$(ssh root@1.2.3.4 uptime)"'),          # substitution inside double quotes
    (BLOCK, "luna-send -i luna://x/y '{}' # via ssh root@1.2.3.4"),

    # --- must be allowed: host-only work, read-only diagnostics, quoted text --
    (ALLOW, "make check"),
    (ALLOW, "make lint"),
    (ALLOW, "make sim && make sim-shot"),
    (ALLOW, "make -s print-appdir FLAVOR=stable"),
    (ALLOW, "cargo +nightly test --lib"),
    (ALLOW, "tools/tv-lock.sh acquire --why 'verify hud'"),
    (ALLOW, "tools/tv-lock.sh status"),
    (ALLOW, "tools/tv-session.sh log 'route='"),
    (ALLOW, "tools/tv-session.sh status"),
    # `tests/run.py --list` never commits to driving the television — `main()` returns 0 for every
    # `args.list` branch before the TV lock is ever acquired (see the pipeline-tier listing branch
    # and the server-tier one right after it). argparse's `--list` is `action="store_true"`, so it
    # takes effect wherever it sits on the line — leading, trailing, or beside `--server`/`--fps`/
    # `--only` — and so does this allowance.
    (ALLOW, "./tests/run.py --list"),
    (ALLOW, "tests/run.py --list --server"),
    (ALLOW, "python3 tests/run.py --list --fps"),
    (ALLOW, "tests/run.py --list --only cold-open"),
    (ALLOW, "tests/run.py --only x --list"),
    (ALLOW, "tools/crash-report.sh --flavor debug"),
    (ALLOW, ".agents/skills/wake-tv/wake-tv.sh"),
    (ALLOW, 'pgrep -fl "tests/run.py|capture-screen|make deploy"'),   # the pre-flight itself
    (ALLOW, 'ps aux | grep -c "[s]sh .*192.0.2.10"'),
    (ALLOW, 'git commit -m "make deploy now takes the TV lock; tests/run.py releases it"'),
    (ALLOW, 'grep -rn "ssh root@" docs/'),
    (ALLOW, "ssh someserver.example.com uptime"),
    (ALLOW, "PLX_TV_LOCK_BYPASS=1 ssh root@1.2.3.4 uptime"),          # the documented hatch
]


HEREDOC_DOC = """cat > .agents/skills/tv-lock/SKILL.md <<'EOF'
Take the lock before `make deploy`, and never a raw `ssh root@1.2.3.4`.
  tools/tv-session.sh up --screen home
EOF"""

HEREDOC_PY = """python3 - <<'PY'
s = "refuses raw `ssh root@…`; every TV-facing tool requires it"
open('MEMORY.md', 'w').write(s)
PY"""

HEREDOC_SSH = """ssh root@1.2.3.4 <<'EOF'
luna-send -i luna://com.webos.applicationManager/launch '{}'
EOF"""

CASES += [
    # Heredoc BODIES are data — writing documentation about the lock must not trip the lock.
    # Both of these fired for real while this mechanism was being built.
    (ALLOW, HEREDOC_DOC),
    (ALLOW, HEREDOC_PY),
    # …but the line that OPENS the heredoc is still a command, and this is how hand-driven TV
    # work has actually been written in this project.
    (BLOCK, HEREDOC_SSH),
]


def blocked(cmd):
    if "PLX_TV_LOCK_BYPASS=1" in cmd:
        return False
    return any(guard.classify(seg) for seg in guard.segments(guard.strip_heredocs(cmd)))


# --- lane identity: the hook and tools/tv-lock.sh must agree on ONE lane -------------------------
#
# A subagent's own worktree is not `payload["cwd"]` (the harness always reports the SESSION
# checkout there), so it names its lane by prefixing `PLX_TV_LOCK_LANE=<its worktree>` on the
# command itself. These cases run the SAME decision `main()` makes — classify, then resolve a
# lane with `lane_from_command()`, then check `lease_for()` — against a throwaway mirror directory,
# never the real `~/.plxnative/tv-lock` a live `tools/tv-lock.sh` writes to.
import shutil
import tempfile
import time as _time


def _fake_state_dir(live_lanes):
    """A temp STATE_DIR holding one live `.lease` file per lane in `live_lanes`."""
    d = tempfile.mkdtemp(prefix="tv-lock-guard-test-")
    for i, lane in enumerate(live_lanes):
        with open(os.path.join(d, f"lane{i}.lease"), "w") as fh:
            fh.write(
                f"TOKEN='tok{i}'\n"
                f"EXPIRES={_time.time() + 3600}\n"
                f"ACQUIRED={_time.time()}\n"
                f"VERIFIED={_time.time()}\n"
                f"TV='192.0.2.10'\n"
                f"LANE='{lane}'\n"
            )
    return d


def blocked_full(cmd, cwd, env=None, live_lanes=()):
    """The hook's whole decision — classify, resolve the lane, check for a mirror lease — the way
    `main()` does it, against a disposable STATE_DIR rather than the real mirror.
    """
    if "PLX_TV_LOCK_BYPASS=1" in cmd:
        return False
    if not any(guard.classify(seg) for seg in guard.segments(guard.strip_heredocs(cmd))):
        return False
    lane = guard.lane_from_command(cmd, env or {}, cwd)
    d = _fake_state_dir(live_lanes)
    old_state_dir = guard.STATE_DIR
    try:
        guard.STATE_DIR = d
        return not bool(guard.lease_for(lane))
    finally:
        guard.STATE_DIR = old_state_dir
        shutil.rmtree(d, ignore_errors=True)


LANE_A = "/repo/worktrees/lane-a"
LANE_B = "/repo/worktrees/lane-b"
SESSION_CWD = "/repo/session-checkout"

# (expect_blocked, cmd, cwd, env, live_lanes)
LANE_CASES = [
    # A prefixed lane whose own mirror lease is live is allowed, even though the reported cwd is
    # the session checkout (never lane-a) and holds nothing itself.
    (False,
     f"PLX_TV_LOCK_LANE={LANE_A} make deploy",
     SESSION_CWD, {}, (LANE_A,)),
    # The identical command naming a DIFFERENT lane, which holds no lease, is blocked — the prefix
    # is honoured, not the cwd, and not lane-a's lease either.
    (True,
     f"PLX_TV_LOCK_LANE={LANE_B} make deploy",
     SESSION_CWD, {}, (LANE_A,)),
    # A prefixed lane must not be satisfied by a lease belonging to the CWD's own lane: cwd is
    # lane-a (which has a live lease), but the command explicitly names lane-b (which has none).
    (True,
     f"PLX_TV_LOCK_LANE={LANE_B} make deploy",
     LANE_A, {}, (LANE_A,)),
    # A `# PLX_TV_LOCK_LANE=` inside a COMMENT does not count as the prefix: the would-be lane
    # (lane-a) holds the only live lease, but cwd resolves to lane-b, which holds none — so this
    # must still block. If the comment were mistakenly read as the prefix, it would wrongly allow.
    (True,
     f"# PLX_TV_LOCK_LANE={LANE_A}\nmake deploy",
     LANE_B, {}, (LANE_A,)),
    # The same escape, but as a heredoc BODY rather than a comment — also must not count.
    (True,
     f"cat > note.txt <<'EOF'\nPLX_TV_LOCK_LANE={LANE_A}\nEOF\nmake deploy",
     LANE_B, {}, (LANE_A,)),
    # `env PLX_TV_LOCK_LANE=<path>` spelling is honoured the same as a bare assignment.
    (False,
     f"env PLX_TV_LOCK_LANE={LANE_A} make deploy",
     SESSION_CWD, {}, (LANE_A,)),
    # A leading `cd <dir> &&` before the assignment is still a prefix.
    (False,
     f"cd /repo/worktrees/lane-a && PLX_TV_LOCK_LANE={LANE_A} make deploy",
     SESSION_CWD, {}, (LANE_A,)),
    # No prefix at all: falls back to the hook's own environment variable.
    (False,
     "make deploy",
     SESSION_CWD, {"PLX_TV_LOCK_LANE": LANE_A}, (LANE_A,)),
    # No prefix, no env var: falls back to cwd, exactly as before this change.
    (False,
     "make deploy",
     LANE_A, {}, (LANE_A,)),
]


def main():
    fails = 0
    for want, cmd in CASES:
        got = blocked(cmd)
        if got != want:
            fails += 1
            print(f"  FAIL  expected {'BLOCK' if want else 'ALLOW'}, got "
                  f"{'BLOCK' if got else 'ALLOW'}: {cmd}")
    for want, cmd, cwd, env, live_lanes in LANE_CASES:
        got = blocked_full(cmd, cwd, env, live_lanes)
        if got != want:
            fails += 1
            print(f"  FAIL  expected {'BLOCK' if want else 'ALLOW'}, got "
                  f"{'BLOCK' if got else 'ALLOW'}: {cmd!r} cwd={cwd!r} env={env!r}")
    total = len(CASES) + len(LANE_CASES)
    print(f"tv-lock-guard: {total - fails}/{total} cases correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
