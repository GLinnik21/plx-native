#!/usr/bin/env python3
"""Cases for the disk-low hook. `python3 .claude/hooks/disk-low-notify-test.py`.

Host-only and hermetic: no real `tools/build-gc.sh` is ever started (the launcher is a recording fake, or a
stub script named by `PLX_DISK_NOTIFY_LAUNCH`), free space is a fake function (or, through a real process,
an absurd threshold over the real `statvfs`), and every write goes to a directory this file makes (the
steps of ci/check-python-steps.txt run side by side and may not write to a fixed path). Wall-clock time is
a parameter (`now`), so no case sleeps; the one process case that waits polls for the detached stub for a
bounded time and asserts nothing about how long the hook took.

The payload shapes are the ones the hook documents: PostToolUse `tool_response` is `{stdout, stderr, ...}`
(measured, see tv-lock-notify-test.py); PostToolUseFailure has `error`, `is_interrupt`, `duration_ms`,
`tool_use_id` and no `tool_response` (code.claude.com/docs/en/hooks, "PostToolUseFailure input").
"""
import ast
import importlib.util
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
HOOK = os.path.join(HERE, "disk-low-notify.py")
spec = importlib.util.spec_from_file_location("notify", HOOK)
notify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notify)

GIB = 1 << 30
MIB = 1 << 20
NOW = 1_800_000_000.0                  # a fixed "now" for the throttle cases
FULL = "write error: No space left on device"
NO_ENV = {}                            # thresholds from the defaults: comfort 20 GiB, reserve 3 GiB


def post(stdout="", stderr="", command="make check", **extra):
    """A PostToolUse payload for Bash."""
    p = {"session_id": "s1", "cwd": "/repo", "hook_event_name": "PostToolUse", "tool_name": "Bash",
         "tool_input": {"command": command},
         "tool_response": {"stdout": stdout, "stderr": stderr, "interrupted": False, "isImage": False,
                           "noOutputExpected": False}}
    p.update(extra)
    return p


def failure(error="Exit code 1", command="make check", **extra):
    """A PostToolUseFailure payload for Bash: `error`, no `tool_response`."""
    p = {"session_id": "s1", "cwd": "/repo", "hook_event_name": "PostToolUseFailure", "tool_name": "Bash",
         "tool_input": {"command": command}, "tool_use_id": "toolu_01", "error": error, "is_interrupt": False,
         "duration_ms": 4187}
    p.update(extra)
    return p


def main():
    fails = 0
    total = 0

    def check(ok, label, detail=""):
        nonlocal fails, total
        total += 1
        if not ok:
            fails += 1
            print(f"  FAIL  {label}{': ' + detail if detail else ''}")

    tmp = os.path.realpath(tempfile.mkdtemp(prefix="disk-low-notify-tests-"))
    ceiling = os.environ.get("GIT_CEILING_DIRECTORIES")
    os.environ["GIT_CEILING_DIRECTORIES"] = tmp      # a rig directory is not inside whatever repo holds $TMPDIR
    counter = [0]

    class Rig:
        """One private state directory, a fake free-space reading and a recording launcher."""

        def __init__(self, free=100 * GIB, env=None, launcher_error=None, script=True):
            counter[0] += 1
            self.dir = os.path.join(tmp, "rig%d" % counter[0])
            self.root = os.path.join(self.dir, "checkout")
            os.makedirs(os.path.join(self.root, "tools"))
            if script:
                open(os.path.join(self.root, "tools", "build-gc.sh"), "w").write("#!/bin/sh\n")
            self.free = free
            self.launches = []
            self.error = launcher_error
            self.env = dict({"PLX_DISK_NOTIFY_STATE": os.path.join(self.dir, "state"),
                             "CLAUDE_PROJECT_DIR": self.root, "PLX_GC_LOG": os.path.join(self.dir, "gc.log")},
                            **(env or {}))

        def launcher(self, argv, cwd, log):
            if self.error:
                raise self.error
            self.launches.append((argv, cwd, log))

        def run(self, payload, now=NOW, free=None):
            if free is not None:
                self.free = free
            return notify.respond(payload, self.env, now, free_fn=lambda _root: self.free, launcher=self.launcher)

    def context(out):
        return out["hookSpecificOutput"]["additionalContext"] if out else None

    try:
        # --- thresholds: the SAME env vars and defaults as tools/disk-budget.py ----------------------
        tree = ast.parse(open(os.path.join(ROOT, "tools", "disk-budget.py")).read())
        consts = {t.id: ast.literal_eval(n.value) for n in tree.body if isinstance(n, ast.Assign)
                  for t in n.targets if isinstance(t, ast.Name) and t.id.startswith("DEFAULT_")}
        check(consts.get("DEFAULT_COMFORT_GIB") == notify.DEFAULT_COMFORT_GIB,
              "default comfort line equals tools/disk-budget.py's DEFAULT_COMFORT_GIB",
              f"{consts.get('DEFAULT_COMFORT_GIB')!r} vs {notify.DEFAULT_COMFORT_GIB!r}")
        check(consts.get("DEFAULT_RESERVE_MIB") == notify.DEFAULT_RESERVE_MIB,
              "default reserve equals tools/disk-budget.py's DEFAULT_RESERVE_MIB",
              f"{consts.get('DEFAULT_RESERVE_MIB')!r} vs {notify.DEFAULT_RESERVE_MIB!r}")
        budget_text = open(os.path.join(ROOT, "tools", "disk-budget.py")).read()
        for name in ("PLX_GC_MIN_FREE_GIB", "PLX_MIN_FREE_MB"):
            check(f'"{name}"' in budget_text and name in open(HOOK).read(),
                  f"{name} is read by both tools/disk-budget.py and the hook")
        check(notify.thresholds(NO_ENV) == (20 * GIB, 3072 * MIB), "thresholds: defaults",
              repr(notify.thresholds(NO_ENV)))
        check(notify.thresholds({"PLX_GC_MIN_FREE_GIB": "50", "PLX_MIN_FREE_MB": "512"}) == (50 * GIB, 512 * MIB),
              "thresholds: both env vars override")
        check(notify.thresholds({"PLX_GC_MIN_FREE_GIB": "0", "PLX_MIN_FREE_MB": "0"}) == (0, 0),
              "thresholds: 0 is a value (PLX_MIN_FREE_MB=0 is disk-budget's way to skip)")
        for bad in ("", "abc", "-5", "1.5", "²"):
            check(notify.thresholds({"PLX_GC_MIN_FREE_GIB": bad, "PLX_MIN_FREE_MB": bad}) == (20 * GIB, 3072 * MIB),
                  f"thresholds: {bad!r} falls back to the defaults, as disk-budget's int_env does")

        # --- the log path is the one build-gc-auto.sh / build-gc.sh use ------------------------------
        check(notify.log_path({"PLX_GC_LOG": "/x/log"}, "darwin", "/h") == "/x/log", "log path: PLX_GC_LOG wins")
        check(notify.log_path({}, "darwin", "/h") == "/h/Library/Logs/plxnative-build-gc.log", "log path: macOS")
        check(notify.log_path({"XDG_STATE_HOME": "/s"}, "linux", "/h") == "/s/plxnative/build-gc.log",
              "log path: XDG_STATE_HOME")
        check(notify.log_path({}, "linux", "/h") == "/h/.cache/plxnative/build-gc.log", "log path: ~/.cache")
        gc_script = open(os.path.join(ROOT, "tools", "build-gc.sh")).read()
        check("Library/Logs/plxnative-build-gc.log" in gc_script and "plxnative/build-gc.log" in gc_script
              and ".cache/plxnative/build-gc.log" in gc_script,
              "log path: tools/build-gc.sh still names the three paths this hook mirrors")

        # --- silence: room to spare, or a mention that is not a result ---------------------------------
        rig = Rig(free=25 * GIB)
        check(rig.run(post()) is None and not rig.launches, "above the comfort line: silent, nothing launched")
        check(rig.run(post(), free=20 * GIB) is None, "exactly at the comfort line is not below it")
        rig = Rig(free=100 * GIB)
        check(rig.run(post(command="echo 'No space left on device' ENOSPC")) is None,
              "ENOSPC only in the command (tool_input) is silent")
        check(rig.run(failure(command="grep -rn ENOSPC src/")) is None, "failure payload: ENOSPC only in the command")
        check(rig.run(post("ordinary output")) is None and rig.run(failure("Exit code 2\nerror: boom")) is None,
              "unrelated output and an unrelated failure are silent")
        check(rig.run("not a dict") is None and rig.run(None) is None and rig.run([1]) is None,
              "a payload that is not an object is silent")
        check(Rig(free=None).run(post()) is None, "free space unreadable and no full-disk text: silent")
        check(not rig.launches, "silent cases launched nothing")

        # --- levels --------------------------------------------------------------------------------------
        rig = Rig(free=int(4.1 * GIB))
        low = rig.run(post())
        ctx = context(low)
        check(isinstance(low, dict) and sorted(low) == ["hookSpecificOutput", "systemMessage"]
              and sorted(low["hookSpecificOutput"]) == ["additionalContext", "hookEventName"],
              "low: output is systemMessage + hookSpecificOutput{hookEventName, additionalContext}", repr(low))
        check(low and low["hookSpecificOutput"]["hookEventName"] == "PostToolUse", "low: hookEventName PostToolUse")
        check(ctx and "Free space is 4.1 GiB" in ctx and "20.0 GiB comfort line" in ctx and "3.0 GiB reserve" in ctx
              and rig.root in ctx, "low: free space, both lines and the volume are in the text", repr(ctx))
        check(ctx and "tools/build-gc.sh --auto was started in the background" in ctx
              and rig.env["PLX_GC_LOG"] in ctx and "make disk" in ctx and "owner" in ctx,
              "low: says the reclaim was started, where it logs, `make disk`, and the owner", repr(ctx))
        check(ctx and re.search(r"Disk check at \d{4}-\d\d-\d\d \d\d:\d\d", ctx) and "snapshot" in ctx and "replayed" in ctx,
              "low: stamped with the date and clock time and marked as a snapshot", repr(ctx))
        check(ctx and "critical" not in ctx.lower(), "low: not worded as critical")
        check(rig.launches == [(["sh", os.path.join(rig.root, "tools", "build-gc.sh"), "--auto"], rig.root,
                                rig.env["PLX_GC_LOG"])],
              "low: one launch, `sh <main>/tools/build-gc.sh --auto` from the main checkout, to the reclaim's log",
              repr(rig.launches))
        summary = low["systemMessage"] if low else ""
        check("\n" not in summary and "4.1 GiB" in summary and "comfort" in summary and "build-gc.sh --auto" in summary,
              "low: systemMessage is one line with the free space and the reclaim", repr(summary))
        check(len(ctx) < 2000 and len(ctx.split("\n")) <= 6, "low: a few lines, far under the 10,000 character cap",
              str(len(ctx)))

        rig = Rig(free=int(1.8 * GIB))
        crit = rig.run(failure("Exit code 1\n" + FULL))
        ctx = context(crit)
        check(crit and crit["hookSpecificOutput"]["hookEventName"] == "PostToolUseFailure",
              "critical: a PostToolUseFailure payload is answered as PostToolUseFailure")
        check(ctx and "Free space is 1.8 GiB" in ctx and "reserve" in ctx and "full filesystem" in ctx
              and "build-gc.sh --auto" in ctx and "foreground" in ctx and "make disk" in ctx
              and "rm -rf" in ctx and "git clean" in ctx and "owner" in ctx and "worktrees" in ctx,
              "critical: free space, ENOSPC, foreground reclaim, make disk, what rm -rf skips, the owner", repr(ctx))
        check(len(rig.launches) == 1, "critical: the background reclaim is started too")
        check("critical" in crit["systemMessage"] and "\n" not in crit["systemMessage"],
              "critical: systemMessage is one line", repr(crit["systemMessage"]))

        rig = Rig(free=int(2.5 * GIB))
        reserve_only = rig.run(post())
        check(reserve_only and "below the 3.0 GiB reserve" in context(reserve_only)
              and "full filesystem" not in context(reserve_only), "critical by free space alone (below the reserve)")
        check(Rig(free=int(3 * GIB)).run(post()) is not None
              and "critical" not in Rig(free=int(3 * GIB)).run(post())["systemMessage"],
              "exactly at the reserve is low, not critical")

        # --- the full-disk text counts ONLY in a PostToolUseFailure's `error` -----------------------------------
        failing = [
            ("failure error", failure("Exit code 1\n" + FULL)),
            ("ENOSPC", failure("Exit code 1\nENOSPC: no space left on device, write")),
            ("Disk quota exceeded", failure("Exit code 1\ncp: Disk quota exceeded")),
            ("Rust's wording", failure("Exit code 101\nfailed to write: No space left on device (os error 28)")),
            ("enospc, lower case", failure("os error: enospc")),
        ]
        succeeding = [
            ("PostToolUse stdout", post(FULL)),
            ("PostToolUse stderr", post("", "cc: " + FULL)),
            ("a grep of the source", post("src/io.rs:12: // ENOSPC", command="grep -rn ENOSPC src/")),
            ("a cat of this hook", post("full-disk phrases: ... enospc ...", command="cat .claude/hooks/disk-low-notify.py")),
            ("a response that is one string", dict(post(), tool_response="x: " + FULL)),
            ("a content-block response", dict(post(), tool_response={"content": [{"type": "text", "text": FULL}]})),
            ("a PostToolUse that carries an error key", dict(post(), error=FULL)),
        ]
        for label, payload in succeeding:
            rig = Rig(free=100 * GIB)
            check(rig.run(payload) is None and not rig.launches, f"exit 0 with the phrase, space free: silent: {label}")
            rig = Rig(free=15 * GIB)
            out = rig.run(payload)
            check(out is not None and "critical" not in out["systemMessage"] and "full filesystem" not in context(out)
                  and len(rig.launches) == 1, f"exit 0 with the phrase, below comfort: only the low note: {label}", repr(out))
        for label, payload in failing:
            rig = Rig(free=100 * GIB)
            out = rig.run(payload)
            check(out is not None and "critical" not in out["systemMessage"].lower()
                  and "critical" not in context(out).lower()
                  and out["systemMessage"] == "disk-low-notify: a command reported a full filesystem; this volume has "
                  "100.0 GiB free." and not rig.launches
                  and not os.path.exists(os.path.join(rig.env["PLX_DISK_NOTIFY_STATE"], notify.STAMP)),
                  f"failure with the phrase, this volume fine: a note, no critical, NO reclaim: {label}", repr(out))
            rig = Rig(free=15 * GIB)
            out = rig.run(payload)
            check(out is not None and "critical" in out["systemMessage"] and len(rig.launches) == 1,
                  f"failure with the phrase, below comfort: critical, reclaim started: {label}", repr(out))
        rig = Rig(free=100 * GIB)
        ctx = context(rig.run(failure(FULL)))
        check(ctx and "Free space is 100.0 GiB" in ctx and "another volume" in ctx and "was not started" in ctx
              and "build-gc.sh --auto was started" not in ctx and "fail partway" not in ctx and "owner" in ctx,
              "failure with the phrase, this volume fine: the note says another volume may be full and why no reclaim",
              repr(ctx))
        check(rig.run(failure(FULL), now=NOW + 60) is None, "elsewhere: once per session")
        out = rig.run(post(), now=NOW + 120, free=4 * GIB)
        check(out is not None and "critical" not in out["systemMessage"] and len(rig.launches) == 1,
              "elsewhere: a later real low reading is not covered by it, and starts the reclaim")
        check(Rig(free=20 * GIB).run(failure(FULL)) is not None
              and not Rig(free=20 * GIB).launches, "elsewhere: exactly at the comfort line is not below it")
        out = Rig(free=100 * GIB).run(dict(failure(FULL), agent_id="a1", agent_type="general-purpose"))
        check(out is not None and "This session is a subagent" in context(out), "elsewhere: the subagent variant")
        rig = Rig(free=None)
        ctx = context(rig.run(failure(FULL)))
        check(ctx and "could not be read" in ctx, "full-disk text with an unreadable volume: still told", repr(ctx))

        # --- the text is facts, not commands -------------------------------------------------------------
        imperative = ("STOP", "NEVER", "MUST", "Never ", "Do not", "You must", "Always ")
        for label, out in (("low", Rig(free=4 * GIB).run(post())),
                           ("critical", Rig(free=1 * GIB).run(failure(FULL))),
                           ("elsewhere", Rig(free=100 * GIB).run(failure(FULL))),
                           ("subagent", Rig(free=1 * GIB).run(dict(post(), agent_id="a1", agent_type="general-purpose"))),
                           ("failed launch", Rig(free=1 * GIB, launcher_error=OSError("no")).run(post())),
                           ("no script", Rig(free=1 * GIB, script=False).run(post()))):
            body = context(out) + "\n" + out["systemMessage"]
            check(not [w for w in imperative if w in body], f"{label}: no imperative system-command phrasing",
                  repr([w for w in imperative if w in body]))
            check(len(context(out)) < 2000, f"{label}: well under the 10,000 character cap")

        # --- who reads it ----------------------------------------------------------------------------------
        main_ctx = context(Rig(free=4 * GIB).run(post()))
        sub_ctx = context(Rig(free=4 * GIB).run(dict(post(), agent_id="a1b2c3", agent_type="general-purpose")))
        check("subagent" not in main_ctx and "caller" not in main_ctx, "main session: no subagent wording")
        check("This session is a subagent" in sub_ctx and "caller" in sub_ctx and "start of that report" in sub_ctx
              and sub_ctx.startswith(main_ctx.split("\n")[0]), "subagent: told the note belongs at the start of the "
              "report to the caller", repr(sub_ctx))
        for empty in ("", None, 0):
            check("subagent" not in context(Rig(free=4 * GIB).run(dict(post(), agent_id=empty))),
                  f"an empty agent_id ({empty!r}) is the main session")

        # --- the event name ---------------------------------------------------------------------------------
        for label, payload, want in [
            ("PostToolUse", post(), "PostToolUse"),
            ("PostToolUseFailure", failure(), "PostToolUseFailure"),
            ("no hook_event_name, tool_response present", {k: v for k, v in post().items() if k != "hook_event_name"},
             "PostToolUse"),
            ("no hook_event_name, error only", {k: v for k, v in failure().items() if k != "hook_event_name"},
             "PostToolUseFailure"),
            ("an unknown hook_event_name falls back to the shape", dict(failure(), hook_event_name="Other"),
             "PostToolUseFailure"),
        ]:
            out = Rig(free=4 * GIB).run(payload)
            check(out and out["hookSpecificOutput"]["hookEventName"] == want, f"hookEventName: {label}", repr(out))

        # --- the throttle ------------------------------------------------------------------------------------
        rig = Rig(free=15 * GIB)
        check(rig.run(post()) is not None, "throttle: first low note speaks")
        check(rig.run(post(), now=NOW + 60) is None, "throttle: the same level is silent a minute later")
        check(rig.run(post(), now=NOW + 29 * 60 + 59) is None, "throttle: still silent at 29m59s")
        check(rig.run(post(), now=NOW + 30 * 60) is not None, "throttle: speaks again at 30 min")
        rig = Rig(free=15 * GIB)
        rig.run(post())
        check(rig.run(post(), now=NOW + 60, free=int(10.1 * GIB)) is None, "throttle: a 4.9 GiB drop is not enough")
        check(rig.run(post(), now=NOW + 120, free=10 * GIB) is not None, "throttle: a 5 GiB drop speaks again")
        check(rig.run(post(), now=NOW + 180, free=int(5.5 * GIB)) is None,
              "throttle: the drop is measured from the LAST note, not the first")
        check(rig.run(post(), now=NOW + 240, free=5 * GIB) is not None, "throttle: another 5 GiB speaks again")
        rig = Rig(free=15 * GIB)
        rig.run(post())
        check(rig.run(post(), now=NOW - 3600) is not None, "throttle: a clock that went backwards is stale")
        rig = Rig(free=15 * GIB)
        rig.run(post())
        check(rig.run(post(), now=NOW + 60, free=19 * GIB) is None, "throttle: free space rising is silent")

        rig = Rig(free=15 * GIB)
        rig.run(post())
        crit = rig.run(failure(FULL), now=NOW + 5, free=14 * GIB)
        check(crit is not None and "critical" in crit["systemMessage"],
              "throttle: critical speaks its first time even after a low note")
        check(rig.run(failure(FULL), now=NOW + 10) is None, "throttle: the second critical is silent")
        check(rig.run(post(), now=NOW + 20, free=15 * GIB) is None,
              "throttle: a low reading after a critical note is covered by it")
        check(rig.run(failure(FULL), now=NOW + 31 * 60) is not None, "throttle: critical speaks again after 30 min")

        rig = Rig(free=4 * GIB)
        rig.run(post(session_id="s1"))
        check(rig.run(post(session_id="s2"), now=NOW + 1) is not None, "throttle: another session has its own state")
        check(rig.run(post(session_id="s1", agent_id="sub1"), now=NOW + 2) is not None,
              "throttle: a subagent has its own key, its context is separate")
        check(rig.run(post(session_id="s1", agent_id="sub1"), now=NOW + 3) is None, "throttle: ... once per level too")
        check(rig.run(post(session_id="../../etc/x y"), now=NOW + 4) is not None
              and not os.path.exists(os.path.join(rig.dir, "etc")), "throttle: a hostile session_id stays in the dir")
        check(rig.run({k: v for k, v in post().items() if k != "session_id"}, now=NOW + 5) is not None,
              "throttle: a payload without session_id still works")

        # --- the reclaim is started once per 10 minutes, machine-wide ---------------------------------------
        rig = Rig(free=4 * GIB)
        rig.run(post(session_id="a"))
        out = rig.run(post(session_id="b"), now=NOW + 599)
        check(len(rig.launches) == 1, "launch: a second session within 10 minutes does not start another")
        check(out and "was started in the background at" in context(out) and "build-gc.sh --auto" in out["systemMessage"],
              "launch: ... and its note still says the reclaim was started", repr(out))
        rig.run(post(session_id="c"), now=NOW + 600)
        check(len(rig.launches) == 2, "launch: after 10 minutes a note starts it again")
        rig = Rig(free=4 * GIB)
        rig.run(post())
        rig.run(post(), now=NOW + 60, free=int(1.1 * GIB))
        check(len(rig.launches) == 1 and rig.launches[0][1] == rig.root,
              "launch: a note inside the window is not a launch; the cwd is the checkout")

        # --- the launch failing, or impossible ----------------------------------------------------------------
        rig = Rig(free=4 * GIB, launcher_error=OSError("no fork"))
        out = rig.run(post())
        check(out is not None and "failed" in context(out), "launcher raising: the note says the start failed", repr(out))
        check(not os.path.exists(os.path.join(rig.env["PLX_DISK_NOTIFY_STATE"], notify.STAMP)),
              "launcher raising: the stamp is removed so the next note tries again")
        rig = Rig(free=4 * GIB, script=False, env={"PLX_DISK_NOTIFY_LAUNCH": ""})
        check(rig.root in context(rig.run(post())) and not rig.launches and "not found" in context(rig.run(post(), now=NOW + 1800)),
              "no tools/build-gc.sh: the note says it could not start the reclaim, nothing is launched")

        # --- fail-open state ------------------------------------------------------------------------------------
        rig = Rig(free=4 * GIB)
        os.makedirs(rig.env["PLX_DISK_NOTIFY_STATE"])
        open(os.path.join(rig.env["PLX_DISK_NOTIFY_STATE"], "s1.json"), "w").write("{not json")
        check(rig.run(post()) is not None, "garbage state file: treated as empty, the note is given")
        open(os.path.join(rig.env["PLX_DISK_NOTIFY_STATE"], "s1.json"), "w").write("[1, 2]")
        check(rig.run(post(), now=NOW + 5000) is not None, "state file holding a list: treated as empty")
        blocker = os.path.join(tmp, "state-is-a-file")
        open(blocker, "w").write("x")
        rig = Rig(free=4 * GIB, env={"PLX_DISK_NOTIFY_STATE": os.path.join(blocker, "sub")})
        check(rig.run(post()) is not None and rig.run(post(), now=NOW + 1) is not None,
              "unwritable state dir: no exception; the note is given every time (the safe side)")
        # state and stamp are written to a temp file and renamed over, so a concurrent reader never sees a truncation
        rig = Rig(free=4 * GIB)
        rig.run(post())
        directory = rig.env["PLX_DISK_NOTIFY_STATE"]
        state_file = os.path.join(directory, "s1.json")
        stamp_file = os.path.join(directory, notify.STAMP)
        before = (os.stat(state_file).st_ino, os.stat(stamp_file).st_ino)
        rig.run(post(), now=NOW + 31 * 60)
        check(len(rig.launches) == 2 and (os.stat(state_file).st_ino, os.stat(stamp_file).st_ino)[0] != before[0]
              and os.stat(stamp_file).st_ino != before[1],
              "state and stamp: replaced by a new file (a new inode), not truncated in place")
        check([n for n in os.listdir(directory) if n.endswith(".tmp")] == [], "state and stamp: no temp file left behind")
        target = os.path.join(tmp, "target-is-a-directory")
        os.makedirs(target)
        try:
            notify.write_atomic(target, "x", NOW)
            raised = False
        except OSError:
            raised = True
        check(raised and [n for n in os.listdir(tmp) if n.endswith(".tmp")] == [],
              "write_atomic: a failing rename raises and removes its temp file")
        rig = Rig(free=4 * GIB)
        old =os.path.join(rig.env["PLX_DISK_NOTIFY_STATE"], "old-session.json")
        os.makedirs(rig.env["PLX_DISK_NOTIFY_STATE"])
        open(old, "w").write("{}")
        os.utime(old, (NOW - 4 * 86400, NOW - 4 * 86400))
        rig.run(post())
        check(not os.path.exists(old), "a session's state file older than 3 days is removed on the next write")

        # --- free_bytes / main_checkout, for real -----------------------------------------------------------------
        real = notify.free_bytes(tmp)
        check(isinstance(real, int) and real > 0, "free_bytes: a real directory", repr(real))
        # Free space can change between real reads while other CI steps write to this volume.
        snapshot = os.statvfs(tmp)
        with patch.object(notify.os, "statvfs", return_value=snapshot) as statvfs:
            missing_free = notify.free_bytes(os.path.join(tmp, "not", "made", "yet"))
            check(missing_free == snapshot.f_bavail * snapshot.f_frsize
                  and statvfs.call_args_list == [((tmp,), {})],
                  "free_bytes: a path that does not exist reads its closest existing ancestor's volume")
        check(notify.free_bytes(None) is None, "free_bytes: None is None, not an exception")

        git = shutil.which("git")
        if git:
            main = os.path.join(tmp, "repo")
            lane = os.path.join(tmp, "lane")
            os.makedirs(main)
            env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
            for args in (["init", "-q"], ["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q",
                                          "--allow-empty", "-m", "x"], ["worktree", "add", "-q", lane, "-b", "lane"]):
                subprocess.run([git, "-C", main] + args, check=True, env=env, capture_output=True)
            check(os.path.realpath(notify.main_checkout(lane)) == os.path.realpath(main),
                  "main_checkout: a linked worktree resolves to the MAIN checkout", notify.main_checkout(lane))
            check(os.path.realpath(notify.main_checkout(main)) == os.path.realpath(main),
                  "main_checkout: the main checkout is itself")
        plain = os.path.join(tmp, "plain")
        os.makedirs(plain)
        check(notify.main_checkout(plain) == plain, "main_checkout: outside a repository it is the directory itself",
              notify.main_checkout(plain))

        # --- the settings entry and the wiring ----------------------------------------------------------------------
        settings = json.load(open(os.path.join(ROOT, ".claude", "settings.json")))
        for event in ("PostToolUse", "PostToolUseFailure"):
            hits = [h for group in settings["hooks"].get(event, []) if group.get("matcher") == "Bash"
                    for h in group["hooks"] if h["command"].endswith("/disk-low-notify.py")]
            check(len(hits) == 1 and hits[0]["timeout"] == 15 and hits[0]["type"] == "command",
                  f"settings.json: disk-low-notify.py registered for {event} (matcher Bash, timeout 15)", repr(hits))
        check(os.access(HOOK, os.X_OK), "the hook file is executable (settings.json runs it directly)")

        # --- through a real process, as the harness runs it ----------------------------------------------------------
        proc_dir = os.path.join(tmp, "proc")
        os.makedirs(proc_dir)
        stub = os.path.join(proc_dir, "stub.sh")
        pidfile = os.path.join(proc_dir, "stub.pid")
        open(stub, "w").write("#!/bin/sh\necho $$ > '%s'\necho stub-says-hi\nexec sleep 30\n" % pidfile)
        os.chmod(stub, 0o755)
        quick = os.path.join(proc_dir, "quick.sh")
        open(quick, "w").write("#!/bin/sh\nexit 0\n")
        os.chmod(quick, 0o755)
        log = os.path.join(proc_dir, "gc.log")

        def run_hook(stdin, **env):
            base = {k: v for k, v in os.environ.items() if not k.startswith(("PLX_", "CLAUDE_"))}
            base.update({"CLAUDE_PROJECT_DIR": proc_dir, "PLX_DISK_NOTIFY_STATE": os.path.join(proc_dir, "state"),
                         "PLX_DISK_NOTIFY_LAUNCH": stub, "PLX_GC_LOG": log})
            base.update(env)
            data = stdin if isinstance(stdin, bytes) else stdin.encode("utf-8")
            r = subprocess.run([sys.executable, HOOK], input=data, capture_output=True, timeout=60, env=base)
            return r.returncode, r.stdout.decode("utf-8", "replace"), r.stderr.decode("utf-8", "replace")

        silent = {"PLX_GC_MIN_FREE_GIB": "0", "PLX_MIN_FREE_MB": "0"}           # nothing is ever below 0
        absurd = {"PLX_GC_MIN_FREE_GIB": "100000000", "PLX_MIN_FREE_MB": "0"}  # everything is below 100 PiB
        pid = None
        try:
            rc, out, err = run_hook(json.dumps(failure("Exit code 1\n" + FULL)), **absurd)
            body = json.loads(out) if out.strip() else None
            check(rc == 0 and err == "" and isinstance(body, dict) and sorted(body) == ["hookSpecificOutput", "systemMessage"]
                  and body["hookSpecificOutput"]["hookEventName"] == "PostToolUseFailure"
                  and "full filesystem" in body["hookSpecificOutput"]["additionalContext"] and out.count("\n") == 1
                  and "critical" in body["systemMessage"],
                  "process: a PostToolUseFailure with 'No space left on device', volume below comfort: one JSON line, critical",
                  f"rc={rc} out={out!r} err={err!r}")
            for _ in range(200):                      # the detached stub, polled for up to ~10 s
                if os.path.exists(pidfile) and open(pidfile).read().strip() and os.path.exists(log) \
                        and "stub-says-hi" in open(log).read():
                    break
                time.sleep(0.05)
            pid = int(open(pidfile).read().strip()) if os.path.exists(pidfile) else None
            alive = False
            if pid:
                try:
                    os.kill(pid, 0)
                    alive = True
                except OSError:
                    pass
            # run() above returned while the stub (sleep 30) was still alive: the child holds none of the hook's pipes
            check(alive, "process: the reclaim is detached (alive after the hook returned, own output in the log)")
            check("stub-says-hi" in open(log).read(), "process: the reclaim's output went to the reclaim's log")
            check(pid is not None and os.getsid(pid) != os.getsid(0), "process: the reclaim has its own session")

            rc, out, err = run_hook(json.dumps(post(command="x")), PLX_DISK_NOTIFY_STATE=os.path.join(proc_dir, "s2"),
                                    PLX_DISK_NOTIFY_LAUNCH=quick, **absurd)
            body = json.loads(out) if out.strip() else None
            check(rc == 0 and err == "" and isinstance(body, dict)
                  and body["hookSpecificOutput"]["hookEventName"] == "PostToolUse"
                  and "comfort line" in body["hookSpecificOutput"]["additionalContext"],
                  "process: a PostToolUse below an absurd comfort line is a PostToolUse note", f"rc={rc} out={out!r}")

            elsewhere_state = os.path.join(proc_dir, "state-elsewhere")
            rc, out, err = run_hook(json.dumps(failure(FULL)), PLX_DISK_NOTIFY_STATE=elsewhere_state,
                                    PLX_DISK_NOTIFY_LAUNCH=quick, **silent)
            body = json.loads(out) if out.strip() else None
            check(rc == 0 and err == "" and isinstance(body, dict) and out.count("\n") == 1
                  and body["hookSpecificOutput"]["hookEventName"] == "PostToolUseFailure"
                  and body["systemMessage"].startswith("disk-low-notify: a command reported a full filesystem; this volume has ")
                  and "critical" not in out.lower()
                  and not os.path.exists(os.path.join(elsewhere_state, notify.STAMP)),
                  "process: a failure with the phrase on a volume above comfort: a note, no critical, no reclaim started",
                  f"rc={rc} out={out!r} err={err!r}")
            for label, stdin, env in [
                ("room to spare", json.dumps(post("fine")), silent),
                ("exit 0 with the phrase in the result (a grep, a cat)", json.dumps(post(FULL)), silent),
                ("the command mentions ENOSPC, the result does not", json.dumps(post(command="echo ENOSPC")), silent),
                ("PLX_DISK_NOTIFY=off", json.dumps(failure(FULL)), dict(absurd, PLX_DISK_NOTIFY="off")),
                ("PLX_DISK_NOTIFY=OFF", json.dumps(failure(FULL)), dict(absurd, PLX_DISK_NOTIFY="OFF")),
                ("truncated JSON", '{"error": "' + FULL, absurd),
                ("not JSON", "oops " + FULL, absurd),
                ("empty stdin", "", absurd),
                ("JSON null", "null", absurd),
                ("a JSON list", json.dumps([FULL]), absurd),
                ("invalid UTF-8", b"\xff\xfe" + FULL.encode() + b"\xff", absurd),
                # a parser that copes with the depth must not turn it into a note; one that does not must not trace back
                ("deeply nested response", '{"tool_response": ' + "[" * 5000 + '"' + FULL + '"' + "]" * 5000 + "}", silent),
                ("deeply nested error", '{"hook_event_name": "PostToolUseFailure", "error": ' + "[" * 5000 + '"' + FULL
                 + '"' + "]" * 5000 + "}", absurd),
            ]:
                state = os.path.join(proc_dir, "state-%d" % counter[0])
                counter[0] += 1
                rc, out, err = run_hook(stdin, PLX_DISK_NOTIFY_STATE=state, **env)
                check(rc == 0 and out == "" and err == "", f"process: {label} -> silent, exit 0",
                      f"rc={rc} out={out!r} err={err!r}")
            rc, out, err = run_hook(json.dumps(failure(FULL, cwd=proc_dir)), CLAUDE_PROJECT_DIR="",
                                    PLX_DISK_NOTIFY_STATE=os.path.join(proc_dir, "s3"), PLX_DISK_NOTIFY_LAUNCH=quick,
                                    **absurd)
            check(rc == 0 and err == "" and '"additionalContext"' in out and proc_dir in out,
                  "process: without CLAUDE_PROJECT_DIR the payload's cwd names the volume (and a note is still given)",
                  f"rc={rc} out={out!r}")
        finally:
            if pid:
                try:
                    os.kill(pid, signal.SIGTERM)
                except OSError:
                    pass
    finally:
        if ceiling is None:
            os.environ.pop("GIT_CEILING_DIRECTORIES", None)
        else:
            os.environ["GIT_CEILING_DIRECTORIES"] = ceiling
        shutil.rmtree(tmp, ignore_errors=True)

    print(f"disk-low-notify: {total - fails}/{total} cases correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
