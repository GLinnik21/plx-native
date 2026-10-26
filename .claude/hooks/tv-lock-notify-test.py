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
    (LINE, "inside a longer run (with -- cmd)", bash(fields(ACQUIRE + "building...\nok\n"),
                                                     "tools/tv-lock.sh with --why x -- make deploy")),

    # --- silent: everything else ------------------------------------------------------------------
    (None, "a renew", bash(fields(RENEW))),
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

    # --- who is told what: a main session may message the owner, a subagent only its caller -------
    # (fleet lanes are subagents, and a subagent's messages never reach the owner, so its wording
    # sends the line up through the caller instead of "in your next message").
    main_text = notify.context_for(bash(fields(ACQUIRE)))
    sub_text = notify.context_for(dict(bash(fields(ACQUIRE)), agent_id="a1b2c3", agent_type="general-purpose"))
    flat = lambda text: " ".join(text.split()) if isinstance(text, str) else ""   # wrapping is not wording
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

    for label, stdin in [
        ("a renew", json.dumps(bash(fields(RENEW)))),
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
