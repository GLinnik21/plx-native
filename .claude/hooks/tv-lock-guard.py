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

LANE IDENTITY has to agree with `tools/tv-lock.sh`'s, and `payload["cwd"]` cannot supply it by
itself: the harness reports the SESSION's own cwd for a plain subagent's Bash call, whatever
worktree that agent was told to work in (only an `isolation: worktree` subagent reports its own
worktree). So each television-driving segment of a command gets the lane the tool it runs will
compute (`LANE=${PLX_TV_LOCK_LANE:-$REPO}`, REPO being the parent of the script's directory), and
`decide()` below lists the order it is read in: a `PLX_TV_LOCK_LANE=`
prefix on the command, that variable in the hook's environment, the segment's own absolute script
path or `make -C <dir>`, an absolute `cd <dir>`, and last the cwd's checkout. Every segment's lane
needs a live lease. A lane read off the command (the script path or the cd) that is not the cwd's own
also needs the payload's `session_id` among the lease's SESSIONS, which only `tools/tv-lock.sh`
writes, so a session holding nothing cannot use a lane on the strength of somebody else's lease.

WHAT IT DELIBERATELY DOES NOT BLOCK.
  * read-only diagnostics that cannot disturb a running session: `tools/crash-report.sh`,
    `tv-session.sh log|status|fg`, `make -s print-*`;
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
import posixpath
import re
import shlex
import subprocess
import sys
import time

STATE_DIR = os.environ.get("PLX_TV_LOCK_STATE") or os.path.expanduser("~/.plxnative/tv-lock")

# Anything that reaches the television. Matched against the COMMAND WORD of each segment (so a
# `git commit -m "make deploy now locks the TV"` is a git command, not a deploy).
TV_MAKE_GOALS = {"deploy", "verify-deploy", "run", "run-stream", "kill", "test", "install", "uninstall"}
TV_SCRIPTS = {"tv-session.sh", "capture-screen.sh", "stream-screen.py", "remote-dpad.py", "run.py",
              "tv-ssh", "tv-sched-trace.sh"}
ALWAYS_OK = {"tv-lock.sh", "wake-tv.sh", "crash-report.sh"}
# `luna-send` only exists on the set, so seeing it here means an ssh payload.
RAW_SSH = re.compile(r"\b(sshpass|ssh|scp|rsync)\b")
ROOT_AT = re.compile(r"root@")


def split_segments(cmd):
    """The segments of `segments()`, each with the separator that came BEFORE it: `""` for the
    first, else one of `;` `\\n` `&&` `||` `|` `&`, and `"$("` for a command substitution's contents
    (which lane derivation must not treat as sitting at any one place in the chain). A blank
    segment between two separators hands the earlier `&&`/`||`/`|` on, so `a &&\\n b` is still `&&`.

    The splitting is QUOTE-AWARE. A plain regex split gets this wrong in a way that reads as the
    guard being broken: `pgrep -fl "tests/run.py|capture-screen"` splits inside the quoted PATTERN
    and hands the tail to the classifier as if it were a command, so the pre-flight that ASKS
    whether anybody is on the television is itself refused. (Both of those fired in the first two
    minutes this hook was live, on this file's own author.) A false positive here is worse than a
    miss: it blocks work that never touches the set, and it teaches the reader to reach for the
    bypass.
    """
    out, cur, quote, i, pending = [], [], None, 0, ""

    def flush(sep):
        nonlocal cur, pending
        text = "".join(cur)
        cur = []
        if text.strip():
            out.append((text, pending))
            pending = sep
        elif pending in ("", ";", "\n"):
            pending = sep

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
            sep = cmd[i:i + 2] if cmd[i:i + 2] in ("&&", "||") else c
            i += len(sep)
            flush(sep)
            continue
        cur.append(c)
        i += 1
    flush("")
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
            out.extend((text, "$(") for text, _ in split_segments(inner))
    return out


def segments(cmd):
    """Split a shell command into segments on ; && || | & and newlines — QUOTE-AWARE
    (`split_segments()` says why that matters) — with the contents of `$( )` appended."""
    return [text for text, _ in split_segments(cmd)]


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
    return classify_lane(seg)[0]


def classify_lane(seg):
    """`(why, own)`: the reason this segment drives the TV (None if it does not) and, for one that
    names its own checkout, `own` = `(lane, how)`: the lane `tools/tv-lock.sh require` will compute,
    step 3 of `decide()`'s order. That is the parent of the directory of the TV script the SAME
    token classification matched (never any other `/…` string in the segment: `ssh root@tv 'cat
    /B/tools/x'` is not a use of /B), or the one absolute directory of a `make -C`. Both are string
    work; the filesystem is not consulted.
    """
    w = words(seg)
    if not w:
        return None, None
    cw = command_word(w)
    base = os.path.basename(cw)
    if base in ALWAYS_OK:
        return None, None
    # `python3 tools/foo.py` / `bash tools/foo.sh`: the interpreter is not the command.
    if base in ("python3", "python", "bash", "sh", "zsh") and len(w) > 1:
        for tok in w[1:]:
            b = os.path.basename(tok)
            if b in ALWAYS_OK:
                return None, None
            if b in TV_SCRIPTS:
                base, cw = b, tok
                break

    if base in TV_SCRIPTS:
        # Three words of tv-session are read-only and stay allowed, all judged the same way: on the
        # first non-flag word AFTER the script (never the script path itself, so `bash
        # tools/tv-session.sh fg` reads like `tools/tv-session.sh fg`). What follows the word is
        # ignored, so `2>&1`, a redirect or a pipe change nothing; a chain was already split into
        # segments by now. Everything else drives.
        #
        # `fg` (the foreground-app read) is exempt on purpose, and the same file blocks `sound
        # status`: a pre-flight that asks "is anybody watching?" must not need a lease, and `fg` is
        # the one such read that never wakes the set or changes anything (tv-session.sh
        # `cmd_lifecycle` reads only `$1` and returns before it uses any other word, so extra words
        # cannot change what it does), whereas `sound status` is still a getVolume round trip.
        if base == "tv-session.sh":
            subs = [t for t in w[w.index(cw) + 1:] if not t.startswith("-")]
            if subs and subs[0] in ("log", "status", "fg"):
                return None, None
        # `tests/run.py --list` never commits to driving the television: `main()` returns 0 from
        # every `args.list` branch (the pipeline-tier listing and the server-tier one right after
        # it) before the TV lock is acquired, before any trigger is armed and before `make deploy`/
        # `run-stream` runs — see the `--list`-vs-drive split in `tests/run.py`'s `main()`. argparse
        # makes `--list` an `action="store_true"`, so it takes effect wherever it sits on the
        # command line, and so does this allowance: `--list --server`, `--only x --list`, `--list
        # --fps` are all list-only. Match the flag as its own TOKEN, never a substring — `--list-
        # foo` is a different, unrecognised flag and must still block.
        if base == "run.py" and "--list" in w[1:]:
            return None, None
        return f"{base} drives the television", _script_lane(cw)

    if base == "make":
        goals = [t for t in w[1:] if not t.startswith("-") and "=" not in t]
        hit = TV_MAKE_GOALS.intersection(goals)
        if hit:
            return ("make " + " ".join(sorted(hit)) + " talks to the television",
                    _make_lane(w[1:]))
        return None, None

    if RAW_SSH.search(cw) and (ROOT_AT.search(seg) or "luna-send" in seg):
        return "a raw ssh/scp to the television, around every tool that takes the lock", None

    if "luna-send" in seg and ROOT_AT.search(seg):
        return "a luna-send on the television", None
    return None, None


# What no literal absolute path contains: the shell has to expand it first (`$W`, a glob, a brace,
# a backslash escape), so the hook does not guess.
_UNKNOWABLE = re.compile(r"[$`*?\[\]{}\\\"']")


def _abs_path(tok):
    """`tok` as a normalised absolute path (lexically: `..` and `//` folded, no filesystem), or None
    when it is not ONE literal absolute path."""
    tok = _strip_quotes(tok)
    if not tok.startswith("/") or _UNKNOWABLE.search(tok):
        return None
    return posixpath.normpath("/" + tok.lstrip("/"))


def _script_lane(tok):
    """The checkout an absolute TV-script path belongs to: the parent of its directory, which is
    `REPO` in `tv-lock.sh` and `tv-session.sh`. Only a script in `tools/` or `tests/` — where every
    entry of TV_SCRIPTS lives — says that; any other directory is not one of ours, so it names no
    lane and the later steps decide, as they did before this one existed."""
    path = _abs_path(tok)
    if path is None:
        return None
    here = posixpath.dirname(path)
    if posixpath.basename(here) not in ("tools", "tests"):
        return None
    return posixpath.dirname(here), "the script's own path"


def _make_lane(args):
    """The absolute directory of the ONE `-C <dir>` / `-C<dir>` / `--directory[=]<dir>` on a `make`
    command line, else None (none, several, or one `make` has to expand or join to the cwd)."""
    dirs, i = [], 0
    while i < len(args):
        a = args[i]
        if a in ("-C", "--directory"):
            dirs.append(args[i + 1] if i + 1 < len(args) else "")
            i += 1
        elif a.startswith("--directory="):
            dirs.append(a[len("--directory="):])
        elif a.startswith("-C") and not a.startswith("-C="):
            dirs.append(a[2:])
        i += 1
    path = _abs_path(dirs[0]) if len(dirs) == 1 else None
    return (path, "make -C") if path else None


def repo_root(cwd):
    try:
        out = subprocess.run(["git", "-C", cwd, "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, timeout=5)
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except Exception:
        pass
    return os.path.abspath(cwd)


_ASSIGN_RE = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$")
_LEADING_CD_RE = re.compile(r'^cd\s+((?:"[^"]*")|(?:\'[^\']*\')|\S+)\s*&&\s*', re.S)


def _strip_quotes(s):
    if len(s) >= 2 and s[0] == s[-1] and s[0] in "'\"":
        return s[1:-1]
    return s


def _leading_env_assignments(text):
    """The `NAME=value` tokens sitting at the very start of `text`, optionally after a leading
    `env`. Stops at the first token that is not itself an assignment — that token is the real
    command, and everything after it is none of this function's business.
    """
    assigns = []
    n = len(text)

    def next_token(p):
        while p < n and text[p] in " \t":
            p += 1
        if p >= n:
            return None, p
        if text[p] in "'\"":
            q = text[p]
            j = p + 1
            while j < n and text[j] != q:
                j += 1
            return text[p:j + 1], min(j + 1, n)
        j = p
        while j < n and text[j] not in " \t":
            j += 1
        return text[p:j], j

    pos = 0
    tok, pos = next_token(pos)
    if tok == "env":
        tok, pos = next_token(pos)
    while tok is not None:
        m = _ASSIGN_RE.match(tok)
        if not m:
            break
        assigns.append((m.group(1), _strip_quotes(m.group(2))))
        tok, pos = next_token(pos)
    return assigns


def explicit_lane(command, env):
    """`(lane, how)` when the command or the hook's environment NAMES the lane, else None: steps 1
    and 2 of `decide()`'s order, the two a person or wrapper chooses. `cwd` cannot play that role by
    itself: the harness reports the SESSION's own checkout as `payload["cwd"]` for a plain
    subagent's Bash call regardless of which worktree that agent is actually acting in (only an
    `isolation: worktree` subagent reports its own), so a hook that only ever reads `cwd` refuses
    a subagent's own, correctly-taken lease (wrong lane) — and exporting `PLX_TV_LOCK_LANE` once
    for the whole session collapses every agent onto ONE lane, which is the failure the lane
    exists to prevent (`docs/agent-reference.md`, the 2026-09-03 collision).

      1. an explicit `PLX_TV_LOCK_LANE=<path>` assignment PREFIXING the command text — directly,
         after a leading `env `, or after a leading `cd <dir> &&`. It names the lane of the WHOLE
         command, as it always has. Of the two it is the only one that can vary per Bash call,
         which is what a subagent naming its OWN worktree needs.
      2. `PLX_TV_LOCK_LANE` in the hook's own environment (a human or wrapper that exported it for
         the whole process rather than per command).

    A `#`-comment or a heredoc BODY never counts — only the start of the command text, after
    heredoc bodies are stripped, is ever inspected.
    """
    text = strip_heredocs(command).lstrip()
    m = _LEADING_CD_RE.match(text)
    if m:
        text = text[m.end():].lstrip()
    for name, value in _leading_env_assignments(text):
        if name == "PLX_TV_LOCK_LANE" and value:
            return value, "the PLX_TV_LOCK_LANE prefix"
    if env.get("PLX_TV_LOCK_LANE"):
        return env["PLX_TV_LOCK_LANE"], "PLX_TV_LOCK_LANE in the hook's environment"
    return None


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


_CD_LIKE = re.compile(r"(?<![\w./-])(?:cd|pushd|popd)(?![\w./-])")
_CD_STAYS = ("", ";", "\n", "&&")      # what may sit on either side of a cd the shell certainly kept
_CD_HOW = "an absolute cd"
# What may follow `cd <abs> ||` for the cd to count as kept: after it the shell is in the directory
# or the command has ended. (A bash that is not inside a function only complains about a `return`
# and carries on, but only when the cd itself failed, which a directory holding a live lease does
# not.)
_LEAVES = re.compile(r"^\s*(?:exit|return)(?:\s+\d+)?\s*$")


def _paren_depth(text):
    """The unquoted `(` minus `)` in one segment's text, quote- and escape-aware like
    `split_segments()`: how many subshells (`(`, `$(`) the segment opens, negative when it closes."""
    depth, quote, i = 0, None, 0
    while i < len(text):
        c = text[i]
        if quote:
            if c == quote and text[i - 1:i] != "\\":
                quote = None
        elif c in "'\"":
            quote = c
        elif c == "\\":
            i += 1
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        i += 1
    return depth


def _cd_target(seg, before, after):
    """The absolute directory of a plain `cd <abs>` segment, else None. `before`/`after` are the
    separators around it: a cd in a pipeline, a background job, an `||` branch (or any other
    context this function is not sure the shell kept) is not trusted. `cd X || exit` is looked
    past by the caller, which hands over the separator that follows the `exit`."""
    if before not in _CD_STAYS or after not in _CD_STAYS:
        return None
    try:
        toks = shlex.split(seg)
    except ValueError:
        return None
    if len(toks) != 2 or toks[0] != "cd":
        return None
    return _abs_path(toks[1])


def decide(cmd, env, cwd, session_id=None):
    """The hook's whole decision for one Bash command: every refusal, or [] to let it run.

    Each television-driving segment has its OWN lane, the invoked tool's `LANE=${PLX_TV_LOCK_LANE:-
    $REPO}` (`tools/tv-lock.sh require`; REPO is the parent of the script's directory), and needs a
    live lease on it. The first of these that applies names it:

      1. a `PLX_TV_LOCK_LANE=` prefix on the command;
      2. `PLX_TV_LOCK_LANE` in the hook's environment (`explicit_lane()`);
      3. the segment's own TV script by absolute path, as command word or interpreter argument, or
         an absolute `make -C <dir>` (`classify_lane()`);
      4. an absolute `cd <dir>`, kept until the next cd, naming the git checkout <dir> is in (as
         the tools' REPO is; a directory in no checkout names itself): `cd "$W"`, a relative cd,
         `cd ~`, `cd -` and anything this function is not sure the shell kept fall through to the
         cwd. Only `&&`, `;`, newlines and a defensive `|| exit [n]` / `|| return [n]` carry it, and
         a cd that sits behind an `&&` lasts to the end of its own `&&` chain. Not kept: a cd in a
         pipeline, an `||` branch, or inside an unclosed `(` or `$(` (a subshell's, however far
         past its `)` the command goes); one in a list that ends in `&` (a background job) outside
         that list; and `cur` is not carried into an `||` branch, which ran because the left side
         failed. A command substitution never picks one up;
      5. the git checkout of `payload["cwd"]`.

    Steps 3 and 4 read the lane OFF THE COMMAND, which would let a session holding nothing run
    `/B/tools/...` against B's live lease, so when such a lane is not the cwd's own, the lease must
    also list the payload's `session_id` in its SESSIONS (written only by `tools/tv-lock.sh`).
    The cwd's own checkout, by path or not, and steps 1, 2 and 5 behave exactly as they always did.

    One entry per segment that fails, as `(segment, why, lane, how, kind)`: the text, why it counts
    as driving the set, the lane that was checked, where that came from, and `"nolease"` (nothing
    live for the lane) or `"session"` (live, but not taken by this session).
    """
    explicit = explicit_lane(cmd, env)

    def lane_of(own, cd):               # (lane or None for the cwd, where it came from)
        return explicit or own or ((cd, _CD_HOW) if cd else (None, "the cwd"))

    checks = []                         # (segment, why, lane, how)
    parts = split_segments(strip_heredocs(cmd))

    def sep_at(k):                      # the separator before parts[k], "" past the end
        return parts[k][1] if k < len(parts) and parts[k][1] != "$(" else ""

    persistent = cur = listed = None    # the lane an absolute cd set: for the rest, for this chain,
    depth = 0                           # and as this list began; the `(`s still open
    for n, (seg, before) in enumerate(parts):
        why, own = classify_lane(seg)
        if before == "$(":              # graded where it is written, never at the final cd
            if why:
                checks.append((seg.strip()[:120], why, *lane_of(own, None)))
            continue
        if before in ("", ";", "\n", "&"):
            if before == "&":
                persistent = listed     # the list before it was a background job: a subshell's cd
            cur = listed = persistent   # a new chain: only a cd that certainly ran carries over
        elif before == "||" and not _LEAVES.match(seg):
            cur = None                  # it runs because the left side failed, which may be the cd
        if why:
            checks.append((seg.strip()[:120], why, *lane_of(own, cur)))
        target = None
        if depth == 0:                  # inside an unclosed `(` or `$(` it is a subshell's cd
            after = sep_at(n + 1)
            if after == "||" and _LEAVES.match(parts[n + 1][0]):
                after = sep_at(n + 2)   # `cd X || exit`: in X, or the command has ended
            target = _cd_target(seg, before, after)
        depth = max(0, depth + _paren_depth(seg))
        if target:
            if before == "&&":
                cur, persistent = target, None
            else:
                cur = persistent = target
        elif _CD_LIKE.search(seg):
            cur = persistent = None
    if not checks:
        return []

    cwd_lane = None
    problems = []
    checkouts = {}                      # an absolute cd's directory -> the checkout it is in
    for seg, why, lane, how in checks:
        derived = lane is not None and explicit is None
        if lane is None:
            if cwd_lane is None:
                cwd_lane = repo_root(cwd)
            lane = cwd_lane
        elif derived and cwd_lane is None:
            cwd_lane = repo_root(cwd)
        if derived and how == _CD_HOW:  # only a segment that uses the cd pays for the lookup
            if lane not in checkouts:
                checkouts[lane] = repo_root(lane)
            lane = checkouts[lane]
        lease = lease_for(lane)
        if not lease:
            problems.append((seg, why, lane, how, "nolease"))
        elif derived and os.path.realpath(lane) != os.path.realpath(cwd_lane):
            sessions = lease.get("SESSIONS", "").split()
            if not (isinstance(session_id, str) and session_id in sessions):
                problems.append((seg, why, lane, how, "session"))
    return problems


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

    problems = decide(cmd, os.environ, payload.get("cwd") or os.getcwd(),
                      payload.get("session_id"))
    if not problems:
        return 0

    seg, why, lane, how, kind = problems[0]
    if kind == "session":
        head = ("BLOCKED: that lane's lease was not taken by this session.\n"
                f"  the command: {seg}\n"
                f"  why blocked: {why}\n"
                f"  the lane:    {lane} (from {how})\n"
                "\n"
                "The lane is a different checkout from this session's own, and its lease is live but\n"
                "does not list this session, so the command could be driving somebody else's job on the\n"
                "set. A lane belongs to whoever holds its lease. Take it again from this session, so it is\n"
                "recorded, only if this checkout's job is yours (`tools/tv-lock.sh acquire` or `renew` in\n"
                "that lane); otherwise it is not yours to use.\n"
                "\n")
    else:
        head = ("BLOCKED: this lane does not hold the television's lock.\n"
                f"  the command: {seg}\n"
                f"  why blocked: {why}\n"
                f"  the lane:    {lane} (from {how})\n"
                "\n")
    sys.stderr.write(
        head +
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
        "runs N simulators at once). See .agents/skills/tv-lock/SKILL.md.\n")
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:          # a bug in the guard must not wedge every Bash call
        sys.stderr.write(f"tv-lock-guard: internal error, allowing ({e})\n")
        sys.exit(0)
