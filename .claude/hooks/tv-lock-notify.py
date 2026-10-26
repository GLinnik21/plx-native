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
fails>` is not nudged. And a `with -- <long command>` is nudged only when the command ends, because the
hook runs after the Bash call returns, not when the line is printed.

FAIL-OPEN, always: any problem (bad stdin, a shape nobody expected) exits 0 with no output, so a bug
here can never wedge a Bash call. Contract: exit 0 with one JSON object on stdout adds context.
"""
import json
import sys

MARKER = "HEADS-UP:"

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


def heads_up_line(text):
    """The first line of `text` that starts with the marker, or None."""
    for line in text.split("\n"):
        if line.startswith(MARKER):
            return line.rstrip()
    return None


def context_for(payload):
    """The instruction to show the agent for this PostToolUse payload, or None."""
    if not isinstance(payload, dict):
        return None
    line = heads_up_line(flatten(payload.get("tool_response")))
    if not line:
        return None
    return (SUBAGENT_INSTRUCTION if payload.get("agent_id") else INSTRUCTION).format(line=line)


def main():
    raw = sys.stdin.buffer.read().decode("utf-8", "replace")
    if MARKER not in raw:                 # the overwhelmingly common case: nothing to do
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
