#!/usr/bin/env python3
"""Cases for the TV-lock heads-up hook. `python3 .claude/hooks/tv-lock-notify-test.py`.

Host-only: no television, no lock state, no `tools/tv-lock.sh` run. It imports the hook and feeds it
PostToolUse payloads, then runs it as the harness does (JSON on stdin, JSON or nothing on stdout) for
the contract cases. The line itself, and the claim that `acquire` prints it once and a renew never
does, are `tools/tv-lock.sh selftest`'s job; this file owns the other half: given a Bash result, does
the agent get told to push it.

The payload shape was MEASURED (a headless run on Claude Code 2.1.310): for `Bash`, `tool_response` is
`{stdout, stderr, interrupted, isImage, noOutputExpected}` (no exit code in it), the payload's keys are
`session_id, cwd, transcript_path, tool_name, tool_input, tool_response`, a subagent's also carries
`agent_id` and `agent_type`, and PostToolUse does not fire at all when the command exits non-zero (only
PostToolUseFailure does: `error="Exit code 1"`, no `tool_response`). `bash()` and `fields()` below build
exactly that and every positive case uses it. The hook still finds the text in any other shape (a
string, `output`/`content`, a list of text blocks) as a fallback, so `flatten()` has its own cases.
"""
import importlib.util
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
HOOK = os.path.join(HERE, "tv-lock-notify.py")
spec = importlib.util.spec_from_file_location("notify", HOOK)
notify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notify)

LINE = "HEADS-UP: fps suite, lease 45 min"
# What `tv-lock.sh acquire` prints for a NEW lease (colour codes and all), and for the other outcomes.
ACQUIRE = ("  \x1b[32m✓\x1b[0m TV lock held by this lane for 45m 00s — fps suite\n"
           "  · release it: tools/tv-lock.sh release\n" + LINE + "\n")
BREAK_LINE = "HEADS-UP: break: stale, lease 45 min"
BREAK = "  \x1b[32m✓\x1b[0m lock broken and taken by this lane\n" + BREAK_LINE + "\n"
RENEW = "  \x1b[32m✓\x1b[0m lock renewed (already yours) — 45m 00s from now\n"
REFUSED = ("  \x1b[31m✗\x1b[0m the TV is held by another lane:\n"
           "someone@host held 3m 10s, 41m 50s left\n    why:  other job\n")
UNRELATED = "Compiling plx_ui v0.1.0\n    Finished `test` profile in 12.4s\ntest result: ok. 3 passed\n"
# The pre-flight's verdict (tools/tv-lock.sh preflight_report): a plain `IN USE:` line at column 0, printed
# BEFORE the heads-up on a new acquire, or on its own from `status`. IDLE and UNKNOWN are not a stop.
IN_USE = "IN USE: youtube.leanback.v4: stop and tell the owner"
ACQUIRE_IN_USE = ("  \x1b[32m✓\x1b[0m TV lock held by this lane for 45m 00s — fps suite\n"
                  "  · release it: tools/tv-lock.sh release\n"
                  "  · sound: muted=false volume=30\n" + IN_USE + "\n" + LINE + "\n")
STATUS_IN_USE = "== TV lock: the TV\n  \x1b[32m✓\x1b[0m FREE — nobody holds the television\n" + IN_USE + "\n"
# Our own app in front with the sound on (tools/tv-lock.sh idle_verdict): the app id and the reason share
# the one line, `IN USE: <id>, sound is on: stop and tell the owner`.
IN_USE_SOUND = "IN USE: com.example.plx, sound is on: stop and tell the owner"
STATUS_IN_USE_SOUND = ("== TV lock: the TV\n  \x1b[32m✓\x1b[0m FREE — nobody holds the television\n"
                       + IN_USE_SOUND + "\n")
ACQUIRE_IN_USE_SOUND = ACQUIRE.replace("  · release it", "  · sound: muted=false volume=30\n" + IN_USE_SOUND + "\n  · release it")
ACQUIRE_IDLE = ACQUIRE.replace("  · release it", "  · IDLE (LG Home launcher)\n  · release it")
ACQUIRE_UNKNOWN = ACQUIRE.replace(
    "  · release it", "  \x1b[33m!\x1b[0m UNKNOWN (the foreground app could not be read) — treat it as possibly in use\n  · release it")


def bash(response, command="tools/tv-lock.sh acquire --why 'fps suite'"):
    return {"session_id": "00000000-0000-0000-0000-000000000000", "cwd": "/repo",
            "transcript_path": "/tmp/transcript.jsonl", "tool_name": "Bash",
            "tool_input": {"command": command}, "tool_response": response}


def fields(stdout="", stderr=""):
    return {"stdout": stdout, "stderr": stderr, "interrupted": False, "isImage": False,
            "noOutputExpected": False}


# --- flatten(): every string in a tool_response, one per line, whatever its shape ------------------
# (value, lines that must come out). Non-text leaves contribute nothing.
FLATTEN_CASES = [
    ("plain text", ["plain text"]),
    (fields("out line", "err line"), ["out line", "err line"]),
    ({"output": "from output"}, ["from output"]),
    ({"content": "from content"}, ["from content"]),
    ({"content": [{"type": "text", "text": "block one"}, {"type": "text", "text": "block two"}]},
     ["block one", "block two"]),
    ([{"type": "text", "text": "listed"}, "bare string"], ["listed", "bare string"]),
    ({"result": {"deeper": ["a", {"b": "c"}]}}, ["a", "c"]),
    (None, []),
    (7, []),
    (True, []),
    ({}, []),
    ([], []),
]

# --- context_for(): payload -> the text the agent is shown, or None --------------------------------
# (the line the agent must be told to push, or None for silence, label, payload)
CASES = [
    # --- told: a NEW lease, in every shape the line can arrive in --------------------------------
    (LINE, "acquire, stdout field", bash(fields(ACQUIRE))),
    (LINE, "acquire, stderr field", bash(fields("", ACQUIRE))),
    (LINE, "a subagent's payload (agent_id, agent_type)",
     dict(bash(fields(ACQUIRE)), agent_id="a1b2c3", agent_type="general-purpose")),
    (BREAK_LINE, "break", bash(fields(BREAK), "tools/tv-lock.sh break --yes")),
    (LINE, "tool_response is one string", bash(ACQUIRE)),
    (LINE, "output field", bash({"output": ACQUIRE})),
    (LINE, "content field", bash({"content": ACQUIRE})),
    (LINE, "list of text blocks", bash([{"type": "text", "text": ACQUIRE}])),
    (LINE, "content blocks", bash({"content": [{"type": "text", "text": ACQUIRE}]})),
    (LINE, "the line is the whole output", bash(fields(LINE))),
    (LINE, "CRLF line ending", bash(fields(LINE + "\r\n"))),
    (LINE, "second field starts with it (stdout has no final newline)", bash(fields("done", LINE))),
    (LINE, "an IDLE verdict before it is not a stop: the heads-up only", bash(fields(ACQUIRE_IDLE))),
    (LINE, "an UNKNOWN verdict before it is not a stop either", bash(fields(ACQUIRE_UNKNOWN))),
    (LINE, "inside a longer run (with -- cmd)", bash(fields(ACQUIRE + "building...\nok\n"),
                                                     "tools/tv-lock.sh with --why x -- make deploy")),

    # --- silent: everything else ------------------------------------------------------------------
    (None, "a renew", bash(fields(RENEW))),
    (None, "a status that found the set idle", bash(fields("== TV lock: the TV\n  · IDLE (LG Home launcher)\n"))),
    (None, "a refused acquire (the failure output)", bash(fields("", REFUSED))),
    (None, "unrelated output", bash(fields(UNRELATED), "make check")),
    (None, "empty output", bash(fields())),
    (None, "empty string response", bash("")),
    (None, "no response at all", {"tool_name": "Bash", "tool_input": {"command": "x"}}),
    (None, "response is null", bash(None)),
    # The marker has to START a line. `cat tools/tv-lock.sh` and `grep -n HEADS-UP` print it mid-line.
    (None, "quoted source line", bash(fields("  printf 'HEADS-UP: %s, lease %s min\\n' \"$x\" \"$y\"\n"))),
    (None, "grep -n output", bash(fields("358:HEADS-UP: x, lease 1 min\n"))),
    (None, "indented", bash(fields("   HEADS-UP: x, lease 1 min\n"))),
    (None, "lower case", bash(fields("heads-up: x, lease 1 min\n"))),
    (None, "no colon", bash(fields("HEADS-UP x\n"))),
    # Only the RESULT counts: a command that merely mentions the marker (a commit message, this file).
    (None, "marker only in the command", bash(fields(UNRELATED),
                                               "git commit -m 'prints\nHEADS-UP: x, lease 1 min'")),
    # A command that exits non-zero never reaches PostToolUse (measured): it is a PostToolUseFailure with
    # `error` and no `tool_response`, which the settings entry does not register for. Were one delivered
    # anyway it is not ours to nudge. Pinned so a widening is a decision, not an accident.
    (None, "failure-shaped payload", {"hook_event_name": "PostToolUseFailure", "tool_name": "Bash",
                                       "tool_input": {"command": "x"},
                                       "error": "Exit code 1\n" + LINE}),
    (None, "payload is not an object", ["HEADS-UP: x"]),
    (None, "payload is a string", LINE),
]


# --- context_for(), IN USE: the pre-flight found somebody on the set -------------------------------
# (the line the agent must be shown on a line of its own, label, payload). The instruction says STOP and
# tell the owner; it must not ask for the heads-up push, which would announce a session that should not start.
IN_USE_CASES = [
    (IN_USE, "status, stdout field", bash(fields(STATUS_IN_USE), "tools/tv-lock.sh status")),
    (IN_USE, "status, stderr field", bash(fields("", STATUS_IN_USE), "tools/tv-lock.sh status")),
    (IN_USE, "acquire that found the set in use (the heads-up follows it)", bash(fields(ACQUIRE_IN_USE))),
    (IN_USE, "a subagent's payload",
     dict(bash(fields(ACQUIRE_IN_USE)), agent_id="a1b2c3", agent_type="general-purpose")),
    (IN_USE, "tool_response is one string", bash(ACQUIRE_IN_USE)),
    (IN_USE, "content blocks", bash({"content": [{"type": "text", "text": ACQUIRE_IN_USE}]})),
    (IN_USE, "the line is the whole output", bash(fields(IN_USE))),
    (IN_USE, "CRLF line ending", bash(fields(IN_USE + "\r\n"))),
    (IN_USE, "second field starts with it", bash(fields("done", IN_USE))),
    (IN_USE_SOUND, "sound-on shape (<id>, sound is on), status", bash(fields(STATUS_IN_USE_SOUND), "tools/tv-lock.sh status")),
    (IN_USE_SOUND, "sound-on shape, acquire", bash(fields(ACQUIRE_IN_USE_SOUND))),
    (IN_USE_SOUND, "sound-on shape, a subagent",
     dict(bash(fields(STATUS_IN_USE_SOUND)), agent_id="a1b2c3", agent_type="general-purpose")),
]
IN_USE_SILENT = [
    ("a printed source line", bash(fields("  printf 'IN USE: %s: stop and tell the owner\\n' \"$1\"\n"))),
    ("grep -n output", bash(fields("483:IN USE: x: stop and tell the owner\n"))),
    ("indented", bash(fields("   IN USE: x: stop and tell the owner\n"))),
    ("mid-line", bash(fields("the TV is IN USE: x\n"))),
    ("lower case", bash(fields("in use: x: stop and tell the owner\n"))),
    ("no colon (the verdict form)", bash(fields("IN USE (x)\n"))),
    ("marker only in the command", bash(fields(UNRELATED), "git commit -m 'prints\nIN USE: x: stop'")),
    ("failure-shaped payload", {"hook_event_name": "PostToolUseFailure", "tool_name": "Bash",
                                "tool_input": {"command": "x"}, "error": "Exit code 1\n" + IN_USE}),
    ("payload is not an object", ["IN USE: x"]),
    ("payload is a string", IN_USE),
]


def run_hook(stdin):
    """The hook as the harness runs it: (exit code, stdout, stderr)."""
    data = stdin if isinstance(stdin, bytes) else stdin.encode("utf-8")
    r = subprocess.run([sys.executable, HOOK], input=data, capture_output=True, timeout=30)
    return r.returncode, r.stdout.decode("utf-8", "replace"), r.stderr.decode("utf-8", "replace")


def main():
    fails = 0
    total = 0

    def check(ok, label, detail=""):
        nonlocal fails, total
        total += 1
        if not ok:
            fails += 1
            print(f"  FAIL  {label}{': ' + detail if detail else ''}")

    for value, want in FLATTEN_CASES:
        got = notify.flatten(value)
        check(isinstance(got, str) and all(w in got.split("\n") for w in want) and (want or got == ""),
              f"flatten {value!r}", f"want lines {want!r}, got {got!r}")

    for line, label, payload in CASES:
        got = notify.context_for(payload)
        if line is None:
            check(got is None, label, f"expected no context, got {got!r}")
        else:
            check(isinstance(got, str) and line in got.split("\n") and "PushNotification" in got,
                  label, f"expected an instruction carrying {line!r} on a line of its own, got {got!r}")

    flat = lambda text: " ".join(text.split()) if isinstance(text, str) else ""   # wrapping is not wording
    for line, label, payload in IN_USE_CASES:
        got = notify.context_for(payload)
        gflat = " ".join(got.split()) if isinstance(got, str) else ""
        check(isinstance(got, str) and line in got.split("\n") and "STOP" in got
              and "Do not deploy, run, drive or test on the television" in gflat
              and "tell the owner" in gflat and "heads-up" not in got.lower(),
              f"IN USE: {label}", f"expected a stop instruction carrying {line!r} on a line of its own, got {got!r}")
    for label, payload in IN_USE_SILENT:
        got = notify.context_for(payload)
        check(got is None, f"IN USE: {label}", f"expected no context, got {got!r}")
    # Both markers in one result (a new acquire that found the set in use): the stop wins, the heads-up
    # push is NOT asked for, and the instruction is still the one for who is reading it.
    both = notify.context_for(bash(fields(ACQUIRE_IN_USE)))
    check(isinstance(both, str) and "PushNotification NOW" not in both and LINE not in both.split("\n"),
          "IN USE: with a heads-up line too, only the stop is shown", repr(both))
    in_use_main = notify.context_for(bash(fields(STATUS_IN_USE)))
    in_use_sub = notify.context_for(dict(bash(fields(STATUS_IN_USE)), agent_id="a1b2c3", agent_type="general-purpose"))
    main_iu, sub_iu = flat(in_use_main), flat(in_use_sub)
    check("owner" in main_iu and "PushNotification" in main_iu and "subagent" not in main_iu and "caller" not in main_iu,
          "IN USE: main session: told to tell the owner", repr(in_use_main))
    # The main session keeps the lease only while it waits for the owner's answer, releases it when it will
    # not drive the set now, and proceeds only on their say-so (`with` and tests/run.py then need the override).
    check("Keep the lease only while you wait for their answer" in main_iu
          and "tools/tv-lock.sh release" in main_iu and "if you will not drive the television now" in main_iu
          and "Proceed only after the owner says so" in main_iu and "PLX_TV_IN_USE_OK=1" in main_iu
          and "tools/tv-lock.sh with and tests/run.py need PLX_TV_IN_USE_OK=1" in main_iu
          and "wait for the owner's answer" not in main_iu,
          "IN USE: main session: keep the lease while waiting, release it otherwise, override only after a yes",
          repr(in_use_main))
    check("You are a subagent" in sub_iu and "FIRST in your final report" in sub_iu and "your caller" in sub_iu
          and "STOP" in sub_iu,
          "IN USE: subagent: stop, and the line FIRST in the final report for the caller", repr(in_use_sub))
    # A subagent cannot wait for the owner, so it must not keep the lease: release it before the turn ends.
    check("tools/tv-lock.sh release" in sub_iu and "before you end your turn" in sub_iu
          and "cannot wait for the owner" in sub_iu and "Keep the lease" not in sub_iu
          and "PLX_TV_IN_USE_OK" not in sub_iu,
          "IN USE: subagent: release the lease before the turn ends, no override, no waiting", repr(in_use_sub))
    check(in_use_main != in_use_sub, "IN USE: the two audiences get different text")
    for text, who in ((in_use_main, "main"), (in_use_sub, "subagent")):
        check(isinstance(text, str) and IN_USE in text.split("\n"),
              f"IN USE: {who}: the marker line is quoted verbatim on a line of its own", repr(text))

    # --- who is told what: a main session may message the owner, a subagent only its caller -------
    # (fleet lanes are subagents, and a subagent's messages never reach the owner, so its wording
    # sends the line up through the caller instead of "in your next message").
    main_text = notify.context_for(bash(fields(ACQUIRE)))
    sub_text = notify.context_for(dict(bash(fields(ACQUIRE)), agent_id="a1b2c3", agent_type="general-purpose"))
    main_flat, sub_flat = flat(main_text), flat(sub_text)
    check("say the line to the owner in your next message" in main_flat
          and "subagent" not in main_flat and "caller" not in main_flat,
          "main session: the owner-facing wording, unchanged", repr(main_text))
    check("You are a subagent" in sub_flat and "FIRST in your final report" in sub_flat
          and "so the caller sends the push" in sub_flat and "If you have a PushNotification tool" in sub_flat
          and "to the owner in your next message" not in sub_flat,
          "subagent: told to hand the line to its caller, FIRST in the final report", repr(sub_text))
    check(isinstance(sub_text, str) and LINE in sub_text.split("\n"),
          "subagent: the line is still on a line of its own", repr(sub_text))
    for empty in ("", None, 0):
        text = notify.context_for(dict(bash(fields(ACQUIRE)), agent_id=empty))
        check(text == main_text, f"an empty agent_id ({empty!r}) is the main session", repr(text))

    # --- the contract, through a real process ---------------------------------------------------
    rc, out, err = run_hook(json.dumps(bash(fields(ACQUIRE))))
    check(rc == 0 and err == "", "process: acquire exits 0 quietly", f"rc={rc} err={err!r}")
    try:
        body = json.loads(out)
    except ValueError:
        body = None
    check(isinstance(body, dict) and list(body) == ["hookSpecificOutput"]
          and isinstance(body["hookSpecificOutput"], dict)
          and sorted(body["hookSpecificOutput"]) == ["additionalContext", "hookEventName"]
          and body["hookSpecificOutput"]["hookEventName"] == "PostToolUse"
          and LINE in body["hookSpecificOutput"]["additionalContext"]
          and "PushNotification" in body["hookSpecificOutput"]["additionalContext"],
          "process: stdout is exactly one PostToolUse hookSpecificOutput with additionalContext", out)
    check(out.count("\n") <= 1, "process: stdout is one JSON object on one line", out)

    rc, out, err = run_hook(json.dumps(bash(fields(STATUS_IN_USE), "tools/tv-lock.sh status")))
    try:
        body = json.loads(out)
    except ValueError:
        body = None
    check(rc == 0 and err == "" and isinstance(body, dict) and list(body) == ["hookSpecificOutput"]
          and body["hookSpecificOutput"]["hookEventName"] == "PostToolUse"
          and IN_USE in body["hookSpecificOutput"]["additionalContext"].split("\n")
          and out.count("\n") <= 1,
          "process: IN USE: is one PostToolUse additionalContext on one line", f"rc={rc} out={out!r} err={err!r}")

    for label, stdin in [
        ("a renew", json.dumps(bash(fields(RENEW)))),
        ("a failure-shaped payload holding IN USE:", json.dumps({"error": "Exit code 1\n" + IN_USE})),
        ("truncated JSON that holds IN USE:", '{"tool_response": {"stdout": "' + IN_USE),
        ("a refused acquire", json.dumps(bash(fields("", REFUSED)))),
        ("unrelated output", json.dumps(bash(fields(UNRELATED), "make check"))),
        ("a failure-shaped payload", json.dumps({"error": "Exit code 1\n" + LINE})),
        # Malformed stdin: with the marker in it (so the fast exit does not save us), without it, and
        # the shapes JSON allows that are not an object. Never a traceback, never a block.
        ("truncated JSON that holds the marker", '{"tool_response": {"stdout": "' + LINE),
        ("not JSON, holds the marker", "oops\n" + LINE + "\n"),
        ("not JSON, no marker", "oops"),
        ("empty stdin", ""),
        ("whitespace only", " \n"),
        ("a JSON list that holds the marker", json.dumps([LINE])),
        ("a bare JSON string that holds the marker", json.dumps(LINE)),
        ("JSON null", "null"),
        ("invalid UTF-8 around the marker", b"\xff\xfe" + LINE.encode() + b"\xff"),
        ("deeply nested response", '{"tool_response": ' + "[" * 5000 + '"' + LINE + '"' + "]" * 5000 + "}"),
    ]:
        rc, out, err = run_hook(stdin)
        check(rc == 0 and out == "" and err == "", f"process: {label} -> silent, exit 0",
              f"rc={rc} out={out!r} err={err!r}")

    print(f"tv-lock-notify: {total - fails}/{total} cases correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
