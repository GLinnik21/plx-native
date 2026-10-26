#!/usr/bin/env python3
"""Cases for the TV-lock heads-up hook. `python3 .claude/hooks/tv-lock-notify-test.py`.

Host-only: no television, no lock state, no `tools/tv-lock.sh` run. It imports the hook and feeds it
PostToolUse payloads, then runs it as the harness does (JSON on stdin, JSON or nothing on stdout) for
the contract cases. The line itself, and the claim that `acquire` prints it once and a renew never
does, are `tools/tv-lock.sh selftest`'s job; this file owns the other half: given a Bash result, does
the agent get told to push it.

The payload shape is documented only for `Write`; for `Bash` the docs say `tool_response` is an object
with `stdout`, `stderr`, `interrupted` and `isImage`. The hook is written to find the text in any
shape (a string, those fields, `output`/`content`, a list of text blocks), so `flatten()` has its own
cases here, and so does every shape the line can arrive in.
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
    return {"hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": command}, "tool_response": response}


def fields(stdout="", stderr=""):
    return {"stdout": stdout, "stderr": stderr, "interrupted": False, "isImage": False}


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
    (None, "no response at all", {"hook_event_name": "PostToolUse", "tool_name": "Bash"}),
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
    # A failed call arrives as PostToolUseFailure -- `error`, no `tool_response` -- and is not ours to
    # nudge (and the settings entry is PostToolUse only). Pinned so a widening is a decision.
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
