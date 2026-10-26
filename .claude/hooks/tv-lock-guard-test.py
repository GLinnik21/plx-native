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
    (BLOCK, "tools/tv-session.sh screen off"),
    # `sound` is a device command exactly like `screen` -- it reaches the television over ssh for
    # off/on AND for status (getVolume is still a round trip to the set), so all three subcommands
    # block. The classifier keys on the tv-session.sh SUBCOMMAND word ("sound"), not on the literal
    # word "status" (which only the top-level `tv-session.sh status` allows through read-only) --
    # `tv-session.sh sound status` is a different, nested word and must still block.
    (BLOCK, "tools/tv-session.sh sound off"),
    (BLOCK, "tools/tv-session.sh sound on"),
    (BLOCK, "tools/tv-session.sh sound status"),
    (BLOCK, "tools/capture-screen.sh out.png DISPLAY"),
    (BLOCK, "ssh root@192.0.2.10 'cat /tmp/plxnative-events.log'"),
    (BLOCK, "sshpass -p alpine scp pkg/plxnative root@192.0.2.10:/tmp/"),
    # tools/tv-ssh is the shared key-first ssh/scp wrapper every TV caller goes through; it spells
    # the television as the literal word `tv` (no `root@<ip>`), so the raw-ssh pattern cannot see it.
    (BLOCK, "tools/tv-ssh ssh tv 'cat /tmp/plxnative-events.log'"),
    (BLOCK, "tools/tv-ssh scp pkg/plxnative tv:/tmp/"),
    (BLOCK, "./tools/tv-ssh ssh tv true"),
    (BLOCK, "tools/tv-sched-trace.sh --secs 20 --out /tmp/t.gz"),
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
    (ALLOW, 'git commit -m "tools/tv-ssh ssh tv: key first, sshpass only when the key is refused"'),
    (ALLOW, "grep -rn tv-ssh docs/ tests/README.md"),
    (ALLOW, "PLX_TV_LOCK_BYPASS=1 ssh root@1.2.3.4 uptime"),          # the documented hatch
    # "sound" alone, off the tv-session.sh command word, must not trip the classifier -- it keys
    # on the SUBCOMMAND of tv-session.sh specifically, not on the word appearing anywhere on the
    # line (prose, a commit message, an unrelated script).
    (ALLOW, 'git commit -m "tools: add tv-session.sh sound off|on|status"'),
    (ALLOW, 'grep -rn "sound off" .agents/skills/tv-session/SKILL.md'),
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
    """A temp STATE_DIR holding one `.lease` file per entry of `live_lanes`: a bare lane (a live
    lease with no SESSIONS field, as an owner's terminal or an older `tv-lock.sh` writes it), or
    `(lane, sessions)` with the space-separated SESSIONS line, or `(lane, sessions, "expired")`.
    """
    d = tempfile.mkdtemp(prefix="tv-lock-guard-test-")
    for i, entry in enumerate(live_lanes):
        if isinstance(entry, str):
            entry = (entry, None)
        lane, sessions, *rest = entry
        state = rest[0] if rest else "live"
        expires = _time.time() + (3600 if state == "live" else -3600)
        with open(os.path.join(d, f"lane{i}.lease"), "w") as fh:
            fh.write(
                f"TOKEN='tok{i}'\n"
                f"EXPIRES={expires}\n"
                f"ACQUIRED={_time.time()}\n"
                f"VERIFIED={_time.time()}\n"
                f"TV='192.0.2.10'\n"
                f"LANE='{lane}'\n"
                + (f"SESSIONS='{sessions}'\n" if sessions is not None else "")
            )
    return d


def blocked_full(cmd, cwd, env=None, live_lanes=(), session_id=None):
    """The hook's whole decision, `guard.decide()` (what `main()` calls), against a disposable
    STATE_DIR rather than the real mirror.
    """
    if "PLX_TV_LOCK_BYPASS=1" in cmd:
        return False
    d = _fake_state_dir(live_lanes)
    old_state_dir = guard.STATE_DIR
    try:
        guard.STATE_DIR = d
        return bool(guard.decide(cmd, env or {}, cwd, session_id))
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


# --- lane derivation: the lane the INVOKED TOOL computes ----------------------------------------
#
# `tools/tv-lock.sh require` names its lane `${PLX_TV_LOCK_LANE:-$REPO}`, REPO being the parent of
# the script's own directory. So a command that names a TV script by absolute path, or `make -C`
# an absolute directory, or `cd`s to one, has a lane of its own whatever `cwd` says (and the
# harness reports the SESSION's checkout there for a subagent). Per TV segment the first of these
# applies: the prefix, the hook's env, the script/`make -C` path, an absolute `cd`, then cwd.
#
# Reading the lane off the command would let session A, holding nothing, run `/B/tools/...`
# against B's live lease, so the lease must also list the payload's session id in SESSIONS --
# but only when that lane is not the cwd's own, which behaves exactly as it always did.
SID, OTHER = "sess-1", "sess-2"
SSH = "ssh root@192.0.2.10 true"
B_TOOL = f"{LANE_B}/tools/tv-session.sh up"

# (expect_blocked, cmd, cwd, env, live_lanes, payload session_id)
DERIVE_CASES = [
    # B by absolute path, B's lease lists this session: allowed from a cwd that holds nothing.
    (False, B_TOOL, SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # The same lease also listing others, in any position.
    (False, B_TOOL, SESSION_CWD, {}, ((LANE_B, f"{OTHER} {SID}"),), SID),
    # B's lease is another session's: refused.
    (True, B_TOOL, SESSION_CWD, {}, ((LANE_B, OTHER),), SID),
    # B's lease carries no SESSIONS at all (the owner's terminal, an older tv-lock.sh): refused.
    (True, B_TOOL, SESSION_CWD, {}, (LANE_B,), SID),
    # The payload has no session id: refused, never an exception.
    (True, B_TOOL, SESSION_CWD, {}, ((LANE_B, SID),), None),
    # Only A leased (A is the cwd): B by path is refused.
    (True, B_TOOL, LANE_A, {}, ((LANE_A, SID),), SID),
    # The cwd's own checkout by absolute path: as before, no SESSIONS needed, after /clear too.
    (False, f"{LANE_A}/tools/tv-session.sh up", LANE_A, {}, (LANE_A,), SID),
    (False, f"{LANE_A}/tools/tv-session.sh up", LANE_A, {}, (LANE_A,), None),
    (False, f"{LANE_A}/tools/tv-session.sh up", LANE_A, {}, ((LANE_A, OTHER),), SID),
    # B's lease ran out, or another session re-acquired it (a new TOKEN carries its own SESSIONS).
    (True, B_TOOL, SESSION_CWD, {}, ((LANE_B, SID, "expired"),), SID),
    (True, B_TOOL, SESSION_CWD, {}, ((LANE_B, OTHER),), SID),
    # An interpreter's argument, a quoted path, and every script the classifier knows.
    (False, f"bash {LANE_B}/tools/tv-session.sh up", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"bash {LANE_B}/tools/tv-session.sh up", SESSION_CWD, {}, ((LANE_B, OTHER),), SID),
    (False, f'"{LANE_B}/tools/tv-session.sh" up', SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"python3 {LANE_B}/tests/run.py --fps", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"{LANE_B}/tools/capture-screen.sh out.png", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"{LANE_B}/tools/capture-screen.sh out.png", SESSION_CWD, {}, ((LANE_B, OTHER),), SID),
    # `..` is folded lexically, no filesystem involved.
    (False, f"{LANE_A}/../lane-b/tools/tv-session.sh up", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # `make -C <abs>`, in all its spellings; a relative -C is not a lane (cwd decides).
    (False, f"make -C {LANE_B} deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"make -C {LANE_B} deploy", SESSION_CWD, {}, ((LANE_B, OTHER),), SID),
    (False, f"make -C{LANE_B} deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"make --directory={LANE_B} deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"make --directory {LANE_B} deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f'make -C "{LANE_B}" deploy', SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, "make -C lane-b deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, "make -C lane-b deploy", LANE_A, {}, (LANE_A,), SID),
    # `make -C $VAR` is unknowable: the cwd decides, as it always did.
    (True, 'make -C "$W" deploy', SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # A path that is not a TV script's own is not a lane: only the matched token counts.
    (True, f"ssh root@192.0.2.10 'cat {LANE_B}/tools/tv-session.sh'", SESSION_CWD, {},
     ((LANE_B, SID),), SID),
    (False, f"ssh root@192.0.2.10 'cat {LANE_B}/tools/tv-session.sh'", LANE_A, {}, (LANE_A,), SID),
    # A script outside tools/ and tests/ is not one of ours: the cwd decides.
    (True, f"python3 {LANE_B}/scripts/run.py", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"python3 {LANE_B}/scripts/run.py", LANE_A, {}, (LANE_A,), SID),
    # The lock tool itself stays outside the guard whatever its path.
    (False, f"{LANE_B}/tools/tv-lock.sh acquire --why x", SESSION_CWD, {}, (), SID),
    # `cd <abs>` persists across `&&`, `;` and newlines until the next cd.
    (False, f"cd {LANE_B} && make deploy && tools/tv-session.sh up",
     SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"cd {LANE_B} && make deploy && tools/tv-session.sh up",
     SESSION_CWD, {}, ((LANE_B, OTHER),), SID),
    (False, f"cd {LANE_B}; make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"cd {LANE_B}\nmake deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f'cd "{LANE_B}" && {SSH}', SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"cd {LANE_B} && true; make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # The next cd ends it: `"$W"`, a relative cd and `~` fall back to the cwd, as does `cd -`.
    (True, f'cd {LANE_B} && cd "$W" && make deploy', SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f'cd {LANE_B} && cd "$W" && make deploy', LANE_A, {}, (LANE_A,), SID),
    (False, f"cd {LANE_B} && cd .. && make deploy", LANE_A, {}, (LANE_A,), SID),
    (False, f"cd {LANE_B} && cd ~ && make deploy", LANE_A, {}, (LANE_A,), SID),
    (False, f"cd {LANE_B} && cd - && make deploy", LANE_A, {}, (LANE_A,), SID),
    (True, f"cd {LANE_B} && cd sub && make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # `cd "$W"` alone is the cwd's lane.
    (False, 'cd "$W" && make deploy', LANE_A, {}, (LANE_A,), SID),
    (True, 'cd "$W" && make deploy', SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # An absolute cd to the cwd's own checkout needs no SESSIONS.
    (False, f"cd {LANE_A} && make deploy", LANE_A, {}, (LANE_A,), None),
    # A cd the shell may not keep (subshell, pipeline, background, an `||` branch, a cd that only
    # runs if something else succeeded and is followed by `;`) is never trusted: cwd decides.
    (True, f"(cd {LANE_B} && tools/tv-session.sh up)", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"cd {LANE_B} | cat; make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"cd {LANE_B} & make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"cd {LANE_B} || exit 1; make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"false && cd {LANE_B}; make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"if cd {LANE_B}; then make deploy; fi", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"builtin cd {LANE_B} && make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"pushd {LANE_B} && make deploy", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # A command substitution is graded where it is written, not at the final cd state: it gets the
    # prefix, the env, its own path, or the cwd -- never a cd.
    (True, f"cd {LANE_B} && x=$({SSH})", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (False, f"cd {LANE_B} && x=$({B_TOOL})", SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # Step 3 beats step 4: the script's own checkout, not the directory it was started from.
    (False, f"cd {LANE_A} && {LANE_B}/tools/tv-session.sh up",
     SESSION_CWD, {}, ((LANE_B, SID),), SID),
    (True, f"cd {LANE_B} && {LANE_A}/tools/tv-session.sh up",
     SESSION_CWD, {}, ((LANE_B, SID),), SID),
    # Every segment is graded on its own lane.
    (True, f"make deploy && make -C {LANE_B} run", LANE_A, {}, ((LANE_A, SID),), SID),
    (True, f"make deploy && make -C {LANE_B} run", LANE_A, {}, ((LANE_B, SID),), SID),
    (False, f"make deploy && make -C {LANE_B} run", LANE_A, {}, (LANE_A, (LANE_B, SID)), SID),
    (True, f"make deploy && make -C {LANE_B} run", LANE_A, {}, (LANE_A, (LANE_B, OTHER)), SID),
    (True, f"cd {LANE_B} && make deploy && {LANE_A}/tools/tv-session.sh up", SESSION_CWD, {},
     ((LANE_B, SID),), SID),
    # The prefix still comes first, over every derived lane, and needs no SESSIONS.
    (True, f"PLX_TV_LOCK_LANE={LANE_B} {LANE_A}/tools/tv-session.sh up", LANE_A, {},
     (LANE_A,), SID),
    (False, f"PLX_TV_LOCK_LANE={LANE_B} {LANE_A}/tools/tv-session.sh up", LANE_A, {},
     (LANE_B,), None),
    (False, f"PLX_TV_LOCK_LANE={LANE_B} make -C {LANE_A} deploy", SESSION_CWD, {},
     (LANE_B,), None),
    # (it names the lane of the WHOLE command, a second segment's own path included, as ever)
    (False, f"PLX_TV_LOCK_LANE={LANE_B} make deploy; make -C {LANE_A} run", SESSION_CWD, {},
     (LANE_B,), SID),
    # So does the hook's environment, over the script path and the cd.
    (False, B_TOOL, SESSION_CWD, {"PLX_TV_LOCK_LANE": LANE_A}, (LANE_A,), None),
    (True, B_TOOL, SESSION_CWD, {"PLX_TV_LOCK_LANE": LANE_A}, ((LANE_B, SID),), SID),
    (False, f"cd {LANE_B} && make deploy", SESSION_CWD, {"PLX_TV_LOCK_LANE": LANE_A},
     (LANE_A,), None),
]


def main_exit(payload, state_dir):
    """Run the guard as the harness does: JSON on stdin, the exit code back."""
    import json
    import subprocess
    env = dict(os.environ, PLX_TV_LOCK_STATE=state_dir)
    env.pop("PLX_TV_LOCK_LANE", None)
    raw = payload if isinstance(payload, str) else json.dumps(payload)
    out = subprocess.run([sys.executable, os.path.join(HERE, "tv-lock-guard.py")], input=raw,
                         capture_output=True, text=True, env=env)
    return out.returncode, out.stderr


def main_cases():
    """`main()` itself: the payload's session_id and cwd reach the decision, a refusal says which
    lane it checked, and an unreadable payload fails OPEN, as the module docstring promises."""
    fails = 0
    d = _fake_state_dir(((LANE_B, SID),))
    try:
        def payload(cmd, **extra):
            return dict({"tool_name": "Bash", "tool_input": {"command": cmd},
                         "cwd": SESSION_CWD}, **extra)
        cases = [
            ("session listed", payload(B_TOOL, session_id=SID), 0, ()),
            ("session not listed", payload(B_TOOL, session_id=OTHER), 2,
             ("not taken by this session", LANE_B, "the script's own path")),
            ("no session_id", payload(B_TOOL), 2, ("not taken by this session", LANE_B)),
            ("subagent carries its parent's id", payload(B_TOOL, session_id=SID, agent_id="a1",
                                                         agent_type="general-purpose"), 0, ()),
            ("no lease at all", payload("make deploy", session_id=SID), 2,
             ("does not hold the television's lock", SESSION_CWD, "the cwd")),
            ("a cd lane says so", payload(f"cd {LANE_A} && make deploy", session_id=SID), 2,
             (LANE_A, "an absolute cd")),
            ("not Bash", {"tool_name": "Read", "tool_input": {}}, 0, ()),
            ("malformed stdin fails open", "{not json", 0, ()),
            # A bug in the guard must not wedge every Bash call: an exception inside decide() (a cwd
            # that is not a string cannot be resolved to a checkout) allows, and says so.
            ("an internal error fails open", payload("make deploy", cwd=123), 0,
             ("internal error, allowing",)),
        ]
        for name, pl, want_rc, want_in_err in cases:
            rc, err = main_exit(pl, d)
            ok = rc == want_rc and all(w in err for w in want_in_err)
            if not ok:
                fails += 1
                print(f"  FAIL  main(): {name}: exit {rc} (want {want_rc}), stderr: {err[:200]!r}")
    finally:
        shutil.rmtree(d, ignore_errors=True)
    return fails, len(cases)


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
    for want, cmd, cwd, env, live_lanes, sid in DERIVE_CASES:
        got = blocked_full(cmd, cwd, env, live_lanes, sid)
        if got != want:
            fails += 1
            print(f"  FAIL  expected {'BLOCK' if want else 'ALLOW'}, got "
                  f"{'BLOCK' if got else 'ALLOW'}: {cmd!r} cwd={cwd!r} env={env!r} "
                  f"live={live_lanes!r} session={sid!r}")
    main_fails, main_total = main_cases()
    fails += main_fails
    total = len(CASES) + len(LANE_CASES) + len(DERIVE_CASES) + main_total
    print(f"tv-lock-guard: {total - fails}/{total} cases correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
