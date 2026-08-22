#!/usr/bin/env python3
"""PreToolUse guard: no command may drive the television without holding its lock.

WHY A HOOK AND NOT JUST A CONVENTION. Every TV-facing tool in this repo now takes the lock
(`make deploy`, `tests/run.py`, `tools/tv-session.sh`, `tools/capture-screen.sh`), so the only way
left to collide is to go around them — a raw `ssh root@<tv> luna-send …`, an `scp` into the app
directory, a `sshpass` one-liner out of a memory or a doc. Those are exactly what an agent reaches
for when a tool refuses, and the damage from two lanes on one set is not a clean failure: it is
plausible WRONG data (an fps number measured while somebody else's binary was landing, a capture
of a screen the other job navigated away from). Nothing downstream can tell that from a real
regression, so it has to be stopped at the point of the command.

WHAT IT COSTS. Nothing on an unrelated command: the classifier is pure string work over the
command line, and the lease check is a single small file read, with no ssh and no network. The
television is never contacted from here.

WHAT IT DELIBERATELY DOES NOT BLOCK.
  * read-only diagnostics that cannot disturb a running session: `tools/crash-report.sh`,
    `tv-session.sh log|status`, `make -s print-*`;
  * the lock tool itself and `wake-tv.sh` — waking a set is harmless and is how you get to a
    television you are about to lock;
  * every host-only path, which is most of this repo: `make check`, `make sim`, cargo, the
    simulator. When this hook refuses something, the simulator (`ui-sim` skill) is usually the
    right next move rather than waiting.

THE ESCAPE HATCH is a prefix on the command itself — `PLX_TV_LOCK_BYPASS=1 ssh root@…`. It is for
a human who knows the set is theirs (and for breaking a genuinely wedged lock); an agent reaching
for it is an agent working around a lock rather than waiting for one.

FAIL-OPEN ON ITS OWN BUGS, fail-closed on the answer: a crash in this file must not wedge every
Bash call in the session, but "no lease" is a refusal.  Contract: exit 0 allows, exit 2 blocks and
feeds stderr back to the model.
"""
import json
import os
import re
import subprocess
import sys
import time

STATE_DIR = os.environ.get("PLX_TV_LOCK_STATE") or os.path.expanduser("~/.plxnative/tv-lock")

# Anything that reaches the television. Matched against the COMMAND WORD of each segment (so a
# `git commit -m "make deploy now locks the TV"` is a git command, not a deploy).
TV_MAKE_GOALS = {"deploy", "run", "run-stream", "kill", "test", "install", "uninstall"}
TV_SCRIPTS = {"tv-session.sh", "capture-screen.sh", "stream-screen.py", "remote-dpad.py", "run.py"}
ALWAYS_OK = {"tv-lock.sh", "wake-tv.sh", "crash-report.sh"}
# `luna-send` only exists on the set, so seeing it here means an ssh payload.
RAW_SSH = re.compile(r"\b(sshpass|ssh|scp|rsync)\b")
ROOT_AT = re.compile(r"root@")


def segments(cmd):
    """Split a shell command into segments on ; && || | & and newlines — QUOTE-AWARE.

    A plain regex split gets this wrong in a way that reads as the guard being broken:
    `pgrep -fl "tests/run.py|capture-screen"` splits inside the quoted PATTERN and hands the tail
    to the classifier as if it were a command, so the pre-flight that ASKS whether anybody is on
    the television is itself refused. (Both of those fired in the first two minutes this hook was
    live, on this file's own author.) A false positive here is worse than a miss: it blocks work
    that never touches the set, and it teaches the reader to reach for the bypass.
    """
    out, cur, quote, i = [], [], None, 0
    while i < len(cmd):
        c = cmd[i]
        if quote:
            cur.append(c)
            if c == quote and cmd[i - 1:i] != "\\":
                quote = None
            i += 1
            continue
        if c in "'\"":
            quote = c
            cur.append(c)
            i += 1
            continue
        if c == "\\" and i + 1 < len(cmd):
            cur.append(c)
            cur.append(cmd[i + 1])
            i += 2
            continue
        if c in ";\n|&":
            # `&&` and `||` end a segment, as do a bare `|`, `;`, `&` and a newline. Consume the
            # pair so the second character cannot start an empty one.
            i += 2 if cmd[i:i + 2] in ("&&", "||") else 1
            out.append("".join(cur))
            cur = []
            continue
        cur.append(c)
        i += 1
    out.append("".join(cur))
    # Command substitution runs whatever is inside it, and `"$(ssh root@…)"` is inside double
    # quotes as far as the scanner above is concerned — so its contents are pulled out and graded
    # as segments of their own, wherever they appeared.
    #
    # BACKTICKS ARE DELIBERATELY NOT DONE HERE, though they are the same shell feature: in this
    # repo a backtick is overwhelmingly markdown, not substitution — every doc, skill and memory
    # this agent writes is full of `ssh root@…` in prose, and treating those as commands blocks
    # the writing of the very documentation that explains the lock. (It did, on its second minute.)
    # The `$( )` form carries no such ambiguity.
    for inner in re.findall(r"\$\(([^()]*)\)", cmd):
        if inner.strip():
            out.extend(segments(inner))
    return [seg for seg in out if seg.strip()]


HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")


def strip_heredocs(cmd):
    """Remove heredoc BODIES, keeping the line that introduces them.

    A heredoc body is data — a file being written, a Python program, a commit message — and this
    agent writes them constantly. Classifying their contents means `cat > SKILL.md <<'EOF'`
    containing the words `make deploy` is refused as a deploy, which is both wrong and maddening.
    The introducing line survives, so `ssh root@tv <<EOF` (a real way this project's TV work has
    been driven by hand) is still caught on the `ssh` itself.
    """
    lines = cmd.split("\n")
    out, i = [], 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        delims = [m.group(2) for m in HEREDOC.finditer(line)]
        i += 1
        for d in delims:
            while i < len(lines) and lines[i].strip() != d:
                i += 1
            i += 1                      # consume the terminator itself
    return "\n".join(out)


def words(seg):
    return seg.strip().split()


def command_word(w):
    """First word that is not an env assignment (FOO=bar) or a `sudo`/`time`-style prefix."""
    for tok in w:
        if "=" in tok.split("/")[0] and not tok.startswith("/") and re.match(r"^\w+=", tok):
            continue
        # Wrappers, and the shell keywords a split leaves at the head of a segment: `for f in …;
        # do ssh root@… ; done` hands the classifier a segment whose first word is `do`, and a
        # loop is exactly the shape a per-flavour device check is written in.
        if tok in ("sudo", "time", "env", "nohup", "exec", "command",
                   "do", "then", "else", "{", "(", "!", "&&", "||"):
            continue
        return tok
    return ""


def classify(seg):
    """Return a reason string if this segment drives the TV, else None."""
    w = words(seg)
    if not w:
        return None
    cw = command_word(w)
    base = os.path.basename(cw)
    if base in ALWAYS_OK:
        return None
    # `python3 tools/foo.py` / `bash tools/foo.sh`: the interpreter is not the command.
    if base in ("python3", "python", "bash", "sh", "zsh") and len(w) > 1:
        for tok in w[1:]:
            b = os.path.basename(tok)
            if b in ALWAYS_OK:
                return None
            if b in TV_SCRIPTS:
                base, cw = b, tok
                break

    if base in TV_SCRIPTS:
        # Two subcommands of tv-session are read-only and stay allowed; everything else drives.
        if base == "tv-session.sh":
            subs = [t for t in w[1:] if not t.startswith("-")]
            if subs and subs[0] in ("log", "status"):
                return None
        return f"{base} drives the television"

    if base == "make":
        goals = [t for t in w[1:] if not t.startswith("-") and "=" not in t]
        hit = TV_MAKE_GOALS.intersection(goals)
        if hit:
            return "make " + " ".join(sorted(hit)) + " talks to the television"
        return None

    if RAW_SSH.search(cw) and (ROOT_AT.search(seg) or "luna-send" in seg):
        return "a raw ssh/scp to the television, around every tool that takes the lock"

    if "luna-send" in seg and ROOT_AT.search(seg):
        return "a luna-send on the television"
    return None


def repo_root(cwd):
    try:
        out = subprocess.run(["git", "-C", cwd, "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, timeout=5)
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except Exception:
        pass
    return os.path.abspath(cwd)


def lease_for(lane):
    """A live lease for THIS lane (this checkout), or None.

    Reads only the local mirror `tools/tv-lock.sh` writes when it takes a lock. That is the cheap
    half of a two-sided lock — the television holds the authoritative copy, and the tools reconcile
    the two on every use. Here we only need to know whether this lane ever took one and whether it
    has run out, which the mirror answers without a round trip.
    """
    if not os.path.isdir(STATE_DIR):
        return None
    now = time.time()
    for name in os.listdir(STATE_DIR):
        if not name.endswith(".lease"):
            continue
        try:
            body = open(os.path.join(STATE_DIR, name)).read()
        except OSError:
            continue
        f = dict(re.findall(r"^(\w+)='?([^'\n]*)'?$", body, re.M))
        if os.path.realpath(f.get("LANE", "")) != os.path.realpath(lane):
            continue
        try:
            if float(f.get("EXPIRES", 0)) > now:
                return f
        except ValueError:
            continue
    return None


def main():
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw)
    except Exception:
        return 0
    if payload.get("tool_name") != "Bash":
        return 0
    cmd = (payload.get("tool_input") or {}).get("command", "")
    if not cmd.strip():
        return 0
    if re.search(r"\bPLX_TV_LOCK_BYPASS=1\b", cmd):
        return 0

    reasons = []
    for seg in segments(strip_heredocs(cmd)):
        why = classify(seg)
        if why:
            reasons.append((seg.strip()[:120], why))
    if not reasons:
        return 0

    lane = repo_root(payload.get("cwd") or os.getcwd())
    if lease_for(lane):
        return 0

    seg, why = reasons[0]
    sys.stderr.write(
        "BLOCKED: this lane does not hold the television's lock.\n"
        f"  the command: {seg}\n"
        f"  why blocked: {why}\n"
        "\n"
        "There is ONE dev set and no OS-level mutex. Two jobs on it do not fail cleanly — they\n"
        "produce plausible WRONG data (an fps number measured while another lane's deploy landed,\n"
        "a capture of a screen the other job navigated away from), which reads exactly like a real\n"
        "regression. So take the lock, or use the simulator:\n"
        "\n"
        "  tools/tv-lock.sh status                      # who holds it, and is anyone on it unlocked\n"
        "  tools/tv-lock.sh acquire --why '<what for>'  # take it (add --wait 540 to queue)\n"
        "  …device work…\n"
        "  tools/tv-lock.sh release                     # hand it back\n"
        "\n"
        "Host-only work needs no lock and is not blocked: make check, make sim (the ui-sim skill\n"
        "runs N simulators at once). See .claude/skills/tv-lock/SKILL.md.\n")
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:          # a bug in the guard must not wedge every Bash call
        sys.stderr.write(f"tv-lock-guard: internal error, allowing ({e})\n")
        sys.exit(0)
