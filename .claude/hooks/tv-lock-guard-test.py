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


def main():
    fails = 0
    for want, cmd in CASES:
        got = blocked(cmd)
        if got != want:
            fails += 1
            print(f"  FAIL  expected {'BLOCK' if want else 'ALLOW'}, got "
                  f"{'BLOCK' if got else 'ALLOW'}: {cmd}")
    print(f"tv-lock-guard: {len(CASES) - fails}/{len(CASES)} cases correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
