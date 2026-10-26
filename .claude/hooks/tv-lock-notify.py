#!/usr/bin/env python3
"""PostToolUse nudge: a Bash call that took the television's lease tells the agent to push it.

WHY A HOOK. When a lane takes (or breaks) the one dev set, the owner should hear about it before the
set starts moving under them, and a shell cannot call PushNotification: only the agent can. So
`tools/tv-lock.sh acquire` and `break` print one line, `HEADS-UP: <why>, lease <ttl> min`, on a new
lease, including your own expired one (never on a live renew), and this hook turns that line in a Bash
result into an instruction, as `additionalContext`, to send it NOW. It is a nudge, not a gate: nothing
here blocks, and nothing checks that the push was sent (whether to enforce it is the owner's call). A
subagent's messages go to its caller, not the owner, and some subagent toolsets have no PushNotification
at all, so a payload with an `agent_id` gets its own text: put the line first in the final report so the
caller sends the push.

The second line it keys on is the pre-flight's verdict. `tools/tv-lock.sh` (an acquire, and `status`)
looks at who is on the set, and when someone else is, prints `IN USE: <app>: stop and tell the owner` at
the start of a line. That is a stop, not a nudge to push: the agent is told not to drive the television
and to tell the owner (a subagent: first in its report). `IDLE` and `UNKNOWN` verdicts print no such line,
so they get nothing. `acquire` and `status` still exit 0 on IN USE (the acquire keeps its lease), so the
text says what to do with that lease: a main session keeps it only while it waits for the owner's answer
and releases it if it will not drive the set now; a subagent cannot wait, so it releases it before its
turn ends. `with` is different: it refuses to start its command (exit 3) and has already released the
lease, and a non-zero exit is not nudged at all (see below), so its own stderr line is the instruction
there. When a result holds both lines the stop wins and the heads-up push is not asked for.

WHAT IT COSTS. It runs after EVERY Bash call, so the common case must be free: the raw stdin is
searched for the marker before anything is parsed, and no marker means exit at once with no output.

WHAT IT READS. Only `tool_response`, never `tool_input`: a command that merely mentions the marker (a
commit message, this file) is not a lease. Measured on Claude Code 2.1.310, a Bash `tool_response` is
`{stdout, stderr, interrupted, isImage, noOutputExpected}` with no exit code in it, so the line can be in
either stream. `flatten()` also takes the text from any other shape (a string, `output`/`content`, a list
of text blocks) in case that moves. The marker has to START a line, which keeps `cat tools/tv-lock.sh` and
`grep -n` from matching.

WHAT IT DOES NOT SEE. PostToolUse does not fire when the command exits non-zero (measured): that is a
PostToolUseFailure (`error="Exit code 1"`, no `tool_response`), and this hook is not registered for it.
Right for a refused acquire, which prints no HEADS-UP; it also means `tv-lock.sh with -- <command that
fails>` is not nudged, nor is a `with` that refused an IN USE set (exit 3). And a `with -- <long command>` is nudged only when the command ends, because the
hook runs after the Bash call returns, not when the line is printed.

FAIL-OPEN, always: any problem (bad stdin, a shape nobody expected) exits 0 with no output, so a bug
here can never wedge a Bash call. Contract: exit 0 with one JSON object on stdout adds context.
"""
import json
import sys

MARKER = "HEADS-UP:"
IN_USE_MARKER = "IN USE:"

INSTRUCTION = (
    "tools/tv-lock.sh just took (or broke) the television's lease. Its heads-up line:\n"
    "\n"
    "{line}\n"
    "\n"
    "Send a PushNotification NOW with that line as the message, before your next step, so the owner\n"
    "knows the TV is about to be in use. A shell cannot send it; only you can. If your toolset has no\n"
    "PushNotification, say the line to the owner in your next message instead."
)

# A subagent (the payload carries `agent_id`, measured) is how a fleet lane runs, and its messages go to
# its caller, not the owner: so "say it in your next message" would never reach anyone who can push.
SUBAGENT_INSTRUCTION = (
    "tools/tv-lock.sh just took (or broke) the television's lease. Its heads-up line:\n"
    "\n"
    "{line}\n"
    "\n"
    "You are a subagent, so your messages reach your caller, not the owner. Put that line FIRST in your\n"
    "final report (or in a message to your caller right away) so the caller sends the push before the\n"
    "owner is surprised by the TV moving. If you have a PushNotification tool, send it yourself NOW with\n"
    "that line as the message, before your next step."
)

# The pre-flight found somebody on the set (tools/tv-lock.sh preflight_report). Not a nudge to push: the
# agent must stop, so neither text mentions the heads-up. `acquire` and `status` exit 0 on IN USE and the
# acquire keeps its lease, so each text says what becomes of that lease: the main session may wait for the
# owner (and `with` and tests/run.py then need PLX_TV_IN_USE_OK=1 after a yes), a subagent cannot, so it hands the lease back.
IN_USE_INSTRUCTION = (
    "tools/tv-lock.sh's pre-flight found the television IN USE. Its line:\n"
    "\n"
    "{line}\n"
    "\n"
    "STOP. Do not deploy, run, drive or test on the television. Tell the owner what was seen: send a\n"
    "PushNotification with that line as the message (if your toolset has none, say the line in your\n"
    "next message). Keep the lease only while you wait for their answer; if you will not drive the\n"
    "television now, release it (tools/tv-lock.sh release). Proceed only after the owner says so\n"
    "(tools/tv-lock.sh with and tests/run.py need PLX_TV_IN_USE_OK=1 then)."
)

SUBAGENT_IN_USE_INSTRUCTION = (
    "tools/tv-lock.sh's pre-flight found the television IN USE. Its line:\n"
    "\n"
    "{line}\n"
    "\n"
    "STOP. Do not deploy, run, drive or test on the television. You are a subagent, so your messages\n"
    "reach your caller, not the owner, and you cannot wait for the owner: release the lease\n"
    "(tools/tv-lock.sh release) before you end your turn, and put that line FIRST in your final report\n"
    "so your caller can tell the owner."
)


def flatten(value):
    """Every string inside a tool_response, newline-joined, whatever shape it has.

    Strings are taken as they are; dicts and lists contribute their values (keys are labels, not
    output); numbers, booleans and None contribute nothing.
    """
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        value = value.values()
    elif not isinstance(value, (list, tuple)):
        return ""
    return "\n".join(flatten(v) for v in value)


def marked_line(text, marker):
    """The first line of `text` that starts with `marker`, or None."""
    for line in text.split("\n"):
        if line.startswith(marker):
            return line.rstrip()
    return None


def context_for(payload):
    """The instruction to show the agent for this PostToolUse payload, or None."""
    if not isinstance(payload, dict):
        return None
    text = flatten(payload.get("tool_response"))
    subagent = bool(payload.get("agent_id"))
    line = marked_line(text, IN_USE_MARKER)       # a stop outranks the heads-up when a result has both
    if line:
        return (SUBAGENT_IN_USE_INSTRUCTION if subagent else IN_USE_INSTRUCTION).format(line=line)
    line = marked_line(text, MARKER)
    if not line:
        return None
    return (SUBAGENT_INSTRUCTION if subagent else INSTRUCTION).format(line=line)


def main():
    raw = sys.stdin.buffer.read().decode("utf-8", "replace")
    if MARKER not in raw and IN_USE_MARKER not in raw:   # the overwhelmingly common case: nothing to do
        return 0
    context = context_for(json.loads(raw))
    if context:
        sys.stdout.write(json.dumps({"hookSpecificOutput": {
            "hookEventName": "PostToolUse", "additionalContext": context}}) + "\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:                    # a bug here must not wedge every Bash call, nor speak
        sys.exit(0)
