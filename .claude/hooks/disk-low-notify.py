#!/usr/bin/env python3
"""PostToolUse / PostToolUseFailure note: tells a running agent the disk is low, and that the reclaim is already running.

WHY A HOOK. An agent that only hears "disk low" improvises: `rm -rf` on a build tree another lane is
using, `git clean`, a reset. The tooling already owns the cleanup (`tools/build-gc.sh --auto`, which
skips a tree with a live builder, an open file or a touch inside the idle window; `tools/build-gc.sh --evict`
and docs/disk-budget.md say so), but nothing told a LIVE agent. `make`'s
disk preflight (`tools/disk-budget.py preflight`) evicts silently before a build, and
`.claude/hooks/build-gc-auto.sh` starts the reclaim only when a session ENDS. This hook runs after every
Bash call; below the comfort line it detaches `tools/build-gc.sh --auto` (from the MAIN checkout, the way
build-gc-auto.sh does) and adds context saying how much is free, that the reclaim was started and where it
logs, and that disk relief in this repo goes through `build-gc.sh` and `make disk`.

THE TEXT IS FACTS, NOT COMMANDS. code.claude.com/docs/en/hooks says `additionalContext` should be factual
statements ("This repo uses `bun test`"): text framed as an out-of-band system command can trip Claude's
prompt-injection defenses, and Claude then shows it to the user instead of using it. Each string is also
capped at 10,000 characters (this one stays under 2,000). The same docs say injected text is saved in the
transcript and REPLAYED on `--resume` / `--continue`, never re-run, so the note carries its own clock time
and says it is a snapshot. A top-level `systemMessage` (one line) is the docs' universal field for text shown
to the person; its delivery for these two events was not observed in a live session.

LEVELS. Thresholds are `tools/disk-budget.py`'s own: comfort `PLX_GC_MIN_FREE_GIB` (default 20 GiB) and
reserve `PLX_MIN_FREE_MB` (default 3072 MiB), same defaults, same digits-only parsing (the test pins the
defaults to that file's constants). Free space is `f_bavail * f_frsize` on the volume holding
`$CLAUDE_PROJECT_DIR` (else the payload's `cwd`), as `disk_budget.free_at` computes it.
  low       free < comfort. The reclaim is started and the note says so.
  critical  free < reserve, OR a PostToolUseFailure `error` holds "No space left on device", "ENOSPC" or
            "disk quota exceeded" while free < comfort (or unreadable). The note adds that builds, `make
            check` and new worktrees can fail partway, that the reclaim also runs in the foreground, and
            what the foreground reclaim checks that `rm -rf` and `git clean` do not. The reclaim is started.
  elsewhere a failure holds one of those phrases while THIS volume is at or above the comfort line: another
            volume may be full. The note is given, the reclaim is NOT started (it frees space here, not
            there), and the `systemMessage` says so without the word critical.
The phrases are matched in `error` only, never in `tool_input` and never in a successful PostToolUse's
`tool_response`: a `grep ENOSPC src/` or a `cat` of this very file exits 0 and stays silent.

THROTTLE. One note per level per session (a subagent has its own key, its context is separate), again only
if free space fell by 5 GiB since that level's last note or 30 minutes passed. A critical note fires its
first time even after a low one, and counts as the low one too. State is a small JSON file per session in
`${TMPDIR:-/tmp}/plxnative-disk-notify/` (`$PLX_DISK_NOTIFY_STATE` names another directory), written to a
temp file and renamed over, so a concurrent call never reads a truncated one. The reclaim is started only
when a low or critical note is, and at most once per 10 minutes machine-wide (a stamp file in the same
directory); `build-gc.sh --auto` also takes its own single-instance mkdir lock, so a second copy that gets
through exits quietly. On a volume at 0 bytes the temp file may not be creatable: the note is then
given on every call and the reclaim started on every call (the script's lock absorbs it). That is the right
side to fail on.

WHAT IT COSTS. It runs after EVERY Bash call. The common case is one `statvfs` plus one substring scan of
the raw stdin, no JSON parse, no subprocess, no import of tools/disk-budget.py. `git` runs only when a
reclaim is about to start (to find the main checkout).

PAYLOADS. PostToolUse for Bash: `tool_response` is `{stdout, stderr, interrupted, isImage,
noOutputExpected}` with no exit code (measured on Claude Code 2.1.310, see tv-lock-notify.py). It does not
fire when the command exits non-zero. That is a PostToolUseFailure: `error` ("Exit code 1\\n..."),
`is_interrupt`, `duration_ms`, `tool_use_id`, no `tool_response` (docs, "PostToolUseFailure input"), and an
ENOSPC usually exits non-zero, so settings.json registers both events. `hookEventName` in the output must
name the event it answers; it is taken from `hook_event_name`, or inferred when that key is absent. A
subagent's payload carries `agent_id` (measured).

WHAT IT DOES NOT SEE. A command that hangs or runs for an hour is seen when it returns. A volume other than
the checkout's (the cold store, a tmp dir on another disk) is seen only when a command's text says so.
Only Bash calls are watched: an Edit or Write that fails with ENOSPC is not. A command that exits 0 with
the phrase in its output (`make 2>&1 | tail`, which hides make's status) is not read for it.
`PLX_DISK_NOTIFY=off` disables the hook. `PLX_DISK_NOTIFY_LAUNCH` names an executable run instead of
`sh tools/build-gc.sh --auto` (tests only).

FAIL-OPEN, always: any problem (bad stdin, an unwritable state dir, a missing script, a hung git) exits 0
with no output, so a bug here can never wedge a Bash call. Contract: exit 0, and one JSON object on stdout
adds context.
"""
import os
import sys
import time

# tools/disk-budget.py: DEFAULT_COMFORT_GIB, DEFAULT_RESERVE_MIB. disk-low-notify-test.py pins them equal.
DEFAULT_COMFORT_GIB = 20
DEFAULT_RESERVE_MIB = 3072
GIB = 1 << 30
MIB = 1 << 20

RENOTIFY_DROP = 5 * GIB                  # a level speaks again when free space fell this far since its last note
RENOTIFY_SECONDS = 30 * 60               # ... or when this long has passed
LAUNCH_DEDUPE_SECONDS = 10 * 60          # the reclaim is started at most this often, machine-wide
STATE_MAX_AGE_SECONDS = 3 * 24 * 3600    # a session's state file older than this is removed
STAMP = "gc-launch.stamp"
SUBAGENT_LINE = ("This session is a subagent, so its report goes to the caller and not to the owner; this note "
                 "belongs at the start of that report so the caller can tell the owner.")
FULL_DISK_PHRASES = ("no space left on device", "enospc", "disk quota exceeded")
EVENTS = ("PostToolUse", "PostToolUseFailure")


def int_env(env, name, default):
    """tools/disk-budget.py's `int_env`: digits only, anything else is the default."""
    value = env.get(name, "")
    return int(value) if value.isdigit() and value.isascii() else default


def thresholds(env):
    """(comfort, reserve) in bytes."""
    return (int_env(env, "PLX_GC_MIN_FREE_GIB", DEFAULT_COMFORT_GIB) * GIB,
            int_env(env, "PLX_MIN_FREE_MB", DEFAULT_RESERVE_MIB) * MIB)


def free_bytes(path):
    """Bytes available to this user on the volume holding `path` (its closest existing ancestor), or None."""
    try:
        path = os.path.abspath(path)
        while not os.path.exists(path):
            parent = os.path.dirname(path)
            if parent == path:
                break
            path = parent
        st = os.statvfs(path)
        return st.f_bavail * st.f_frsize
    except (OSError, ValueError, TypeError):
        return None


def mentions_full_disk(text):
    low = text.lower()
    return any(phrase in low for phrase in FULL_DISK_PHRASES)


def flatten(value):
    """Every string inside a tool_response, newline-joined, whatever shape it has (as tv-lock-notify.py does)."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        value = value.values()
    elif not isinstance(value, (list, tuple)):
        return ""
    return "\n".join(flatten(v) for v in value)


def event_name(payload):
    name = payload.get("hook_event_name")
    if name in EVENTS:
        return name
    return "PostToolUseFailure" if "tool_response" not in payload and "error" in payload else "PostToolUse"


def project_root(payload, env):
    for candidate in (env.get("CLAUDE_PROJECT_DIR"), payload.get("cwd")):
        if isinstance(candidate, str) and candidate and os.path.isdir(candidate):
            return candidate
    return os.getcwd()


# --- the reclaim ----------------------------------------------------------------------------------------

def log_path(env, platform=None, home=None):
    """Where build-gc-auto.sh (and `tools/build-gc.sh`'s own gc_log_path) put the reclaim's log."""
    if env.get("PLX_GC_LOG"):
        return env["PLX_GC_LOG"]
    platform = sys.platform if platform is None else platform
    home = home or env.get("HOME") or os.path.expanduser("~")
    if platform == "darwin":
        return os.path.join(home, "Library", "Logs", "plxnative-build-gc.log")
    if env.get("XDG_STATE_HOME"):
        return os.path.join(env["XDG_STATE_HOME"], "plxnative", "build-gc.log")
    return os.path.join(home, ".cache", "plxnative", "build-gc.log")


def main_checkout(root):
    """The MAIN checkout, from `git rev-parse --git-common-dir`, as build-gc-auto.sh finds it; `root` if git cannot say.

    A lane under .claude/worktrees/ can be the one about to be torn down, the worst place to run a job that
    has to outlive the session.
    """
    import subprocess
    try:
        r = subprocess.run(["git", "-C", root, "rev-parse", "--path-format=absolute", "--git-common-dir"],
                           capture_output=True, text=True, timeout=5)
        common = r.stdout.strip()
        if r.returncode == 0 and common:
            return os.path.dirname(common)
    except Exception:
        pass
    return root


def gc_command(env, root):
    """(argv, cwd) of the reclaim, or None when the script is not there."""
    stub = env.get("PLX_DISK_NOTIFY_LAUNCH")
    if stub:
        return [stub], root
    main = main_checkout(root)
    script = os.path.join(main, "tools", "build-gc.sh")
    return (["sh", script, "--auto"], main) if os.path.isfile(script) else None


def launch_gc(argv, cwd, log):
    """Start `argv` detached and return at once: its own session, no inherited fds, output to the reclaim's log.

    The same detach build-gc-auto.sh makes with `( nohup sh script --auto >>log 2>&1 </dev/null & )`: the
    child holds none of this process's pipes, so the harness is not left waiting on one.
    """
    import subprocess
    os.makedirs(os.path.dirname(log), exist_ok=True)
    with open(log, "ab") as out:
        subprocess.Popen(argv, cwd=cwd, stdin=subprocess.DEVNULL, stdout=out, stderr=out,
                         close_fds=True, start_new_session=True)


def ensure_gc(env, root, now, directory, launcher):
    """Start the reclaim unless one was started in the last 10 minutes. (status, when) with status one of
    started | recent | unavailable | failed; `when` is the start time for the first two."""
    stamp = os.path.join(directory, STAMP)
    try:
        stamped = os.path.getmtime(stamp)
        if 0 <= now - stamped < LAUNCH_DEDUPE_SECONDS:
            return "recent", stamped
    except OSError:
        pass
    command = gc_command(env, root)
    if command is None:
        return "unavailable", None
    argv, cwd = command
    try:
        os.makedirs(directory, exist_ok=True)
        write_atomic(stamp, "%d\n" % now, now)
    except OSError:
        pass                              # cannot dedupe here; build-gc.sh's own lock still holds
    try:
        launcher(argv, cwd, log_path(env))
    except Exception:
        try:
            os.unlink(stamp)              # a failed start must not hold off the next attempt
        except OSError:
            pass
        return "failed", None
    return "started", now


# --- the throttle ---------------------------------------------------------------------------------------

def state_dir(env):
    return env.get("PLX_DISK_NOTIFY_STATE") or os.path.join(env.get("TMPDIR") or "/tmp", "plxnative-disk-notify")


def state_key(payload):
    parts = [str(payload.get("session_id") or "nosession")]
    if payload.get("agent_id"):
        parts.append(str(payload["agent_id"]))
    return "".join(c if c.isalnum() or c in "._-" else "_" for c in "-".join(parts))[:120]


def load_state(path):
    import json
    try:
        with open(path) as f:
            state = json.load(f)
        return state if isinstance(state, dict) else {}
    except (OSError, ValueError):
        return {}


def write_atomic(path, text, now):
    """Write `text` to a temp file beside `path`, then `os.replace` it over: a concurrent reader (another Bash
    call's hook) sees the old file or the new one, never a truncated one. File times follow the hook's clock."""
    tmp = "%s.%d.tmp" % (path, os.getpid())
    try:
        with open(tmp, "w") as f:
            f.write(text)
        os.utime(tmp, (now, now))
        os.replace(tmp, path)
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def save_state(directory, path, state, now):
    """Best effort: an unwritable state only means the next call speaks again."""
    import json
    try:
        os.makedirs(directory, exist_ok=True)
        write_atomic(path, json.dumps(state), now)
        for name in os.listdir(directory):
            old = os.path.join(directory, name)
            if name.endswith((".json", ".tmp")) and old != path and now - os.path.getmtime(old) > STATE_MAX_AGE_SECONDS:
                os.unlink(old)
    except OSError:
        pass


def should_speak(previous, free, now):
    """May this level speak again? `previous` is its last note, {t, free}, or None."""
    if not isinstance(previous, dict):
        return True
    try:
        since = now - float(previous["t"])
        before = previous.get("free")
    except (KeyError, TypeError, ValueError):
        return True
    if not 0 <= since < RENOTIFY_SECONDS:     # a clock that went backwards is stale too
        return True
    return free is not None and isinstance(before, (int, float)) and before - free >= RENOTIFY_DROP


# --- the text -------------------------------------------------------------------------------------------

def gib_text(n):
    return "%.1f GiB" % (n / GIB)


def compose(level, free, comfort, reserve, root, now, gc, log, subagent, enospc):
    """(additionalContext, systemMessage). Statements of fact, a few lines, stamped with the clock time."""
    def hhmm(t):
        return time.strftime("%H:%M", time.localtime(t))

    stamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(now))     # replayed days later: the date matters

    volume = "the volume holding %s" % root
    free_text = gib_text(free) + " free" if free is not None else "free space unknown"
    reading = ("Free space is %s on %s" % (gib_text(free), volume) if free is not None
               else "The free space of %s could not be read" % volume)
    status, when = gc
    lines = ["Disk check at %s (a snapshot taken then; the text is replayed, not re-run, when a session resumes)."
             % stamp]
    if level == "elsewhere":
        lines.append("A command's output in this session reports a full filesystem. %s, above the comfort line, so "
                     "the full filesystem may be another volume." % reading)
        lines.append("tools/build-gc.sh --auto was not started: it frees space on this volume, which is above the "
                     "comfort line. `make disk` shows this volume's build trees. If another volume is full, what to "
                     "free there is for the owner to decide.")
        if subagent:
            lines.append(SUBAGENT_LINE)
        return "\n".join(lines), ("disk-low-notify: a command reported a full filesystem; this volume has %s free."
                                  % gib_text(free))
    if level == "low":
        lines.append("%s, below the %s comfort line (PLX_GC_MIN_FREE_GIB); builds also need a %s reserve "
                     "(PLX_MIN_FREE_MB) on top of their own size." % (reading, gib_text(comfort), gib_text(reserve)))
    elif enospc:
        lines.append("A command's output in this session reports a full filesystem. %s. Builds, make check and new "
                     "worktrees can fail partway with ENOSPC while it stays full." % reading)
    else:
        lines.append("%s, below the %s reserve (PLX_MIN_FREE_MB) that a build needs on top of its own size. Builds, "
                     "make check and new worktrees can fail partway with ENOSPC at this level."
                     % (reading, gib_text(reserve)))
    if status in ("started", "recent"):
        started = "tools/build-gc.sh --auto was started in the background at %s" % hhmm(when)
        lines.append("%s; it logs to %s." % (started, log))
    elif status == "unavailable":
        started = "tools/build-gc.sh was not found, so the background reclaim could not be started"
        lines.append("%s from %s." % (started, root))
    else:
        started = "starting tools/build-gc.sh --auto in the background failed"
        lines.append(started[0].upper() + started[1:] + ".")
    if level == "low":
        lines.append("In this repo, disk relief goes through tools/build-gc.sh and `make disk` (which shows where "
                     "the space is); build trees deleted by hand are not tracked by those tools and may be another "
                     "lane's live tree. If free space stays low, that is for the owner to hear about.")
        summary = "disk-low-notify: %s, below the %d GiB comfort line; %s." % (free_text, comfort // GIB, started)
    else:
        lines.append("In this repo the reclaim is `tools/build-gc.sh --auto` (it also runs in the foreground) and "
                     "`make disk` shows where the space is. The reclaim skips a tree with a live builder, an open file or "
                     "recent writes; `rm -rf` of build trees and `git clean` or reset do not check. If the volume is still "
                     "below the reserve after the reclaim, the next step is the owner's.")
        why = "a command reported a full filesystem" if enospc else "below the %s reserve" % gib_text(reserve)
        summary = "disk-low-notify: critical, %s, %s; %s." % (free_text, why, started)
    if subagent:
        lines.append(SUBAGENT_LINE)
    return "\n".join(lines), summary


def respond(payload, env, now, free_fn=free_bytes, launcher=launch_gc):
    """The hook's output object for this payload, or None for silence."""
    if not isinstance(payload, dict):
        return None
    comfort, reserve = thresholds(env)
    enospc = event_name(payload) == "PostToolUseFailure" and mentions_full_disk(flatten(payload.get("error")))
    root = project_root(payload, env)
    free = free_fn(root)
    if (enospc and (free is None or free < comfort)) or (free is not None and free < reserve):
        level = "critical"
    elif free is not None and free < comfort:
        level = "low"
    elif enospc:
        level = "elsewhere"                  # this volume is fine, so the full one is another
    else:
        return None
    directory = state_dir(env)
    path = os.path.join(directory, state_key(payload) + ".json")
    state = load_state(path)
    if not should_speak(state.get(level), free, now):
        return None
    gc = (None, None) if level == "elsewhere" else ensure_gc(env, root, now, directory, launcher)
    note = {"t": now, "free": free}
    state[level] = note
    if level == "critical":
        state["low"] = note                  # the critical note says everything the low one would
    save_state(directory, path, state, now)
    context, summary = compose(level, free, comfort, reserve, root, now, gc, log_path(env),
                               bool(payload.get("agent_id")), enospc)
    return {"systemMessage": summary,
            "hookSpecificOutput": {"hookEventName": event_name(payload), "additionalContext": context}}


def main():
    env = os.environ
    raw = sys.stdin.buffer.read().decode("utf-8", "replace")
    if env.get("PLX_DISK_NOTIFY", "").strip().lower() == "off":
        return 0
    comfort, reserve = thresholds(env)
    root = env.get("CLAUDE_PROJECT_DIR")
    if root:                                 # the common case: room to spare and no full-disk text anywhere
        free = free_bytes(root)
        if free is not None and free >= max(comfort, reserve) and not mentions_full_disk(raw):
            return 0
    import json
    out = respond(json.loads(raw), env, time.time())
    if out:
        sys.stdout.write(json.dumps(out) + "\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:                        # a bug here must not wedge every Bash call, nor speak
        sys.exit(0)
