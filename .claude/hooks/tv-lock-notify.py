#!/usr/bin/env python3
"""PostToolUse nudge: a Bash call that took the television's lease tells the agent to push it.

WHY A HOOK. When a lane takes (or breaks) the one dev set, the owner should hear about it before the
set starts moving under them, and a shell cannot call PushNotification: only the agent can. So
`tools/tv-lock.sh acquire` and `break` print one line, `HEADS-UP: <why>, lease <ttl> min`, on a NEW
lease (never on a renew), and this hook turns that line in a Bash result into an instruction, as
`additionalContext`, to send it NOW. It is a nudge, not a gate: nothing here blocks, and nothing checks
that the push was sent (whether to enforce it is the owner's call; some subagent toolsets have no
PushNotification at all, so the text says what to do then).

WHAT IT COSTS. It runs after EVERY Bash call, so the common case must be free: the raw stdin is
searched for the marker before anything is parsed, and no marker means exit at once with no output.

WHAT IT READS. Only `tool_response`, never `tool_input`: a command that merely mentions the marker (a
commit message, this file) is not a lease. The documented `Bash` response is an object with `stdout`,
`stderr`, `interrupted` and `isImage`, but the payload shape has moved before, so `flatten()` takes the
text from any shape (a string, those fields, `output`/`content`, a list of text blocks). The marker has
to START a line, which keeps `cat tools/tv-lock.sh` and `grep -n` from matching.

WHAT IT DOES NOT SEE. A call that exits non-zero reaches PostToolUseFailure (an `error` string, no
`tool_response`), which this hook is not registered for. That is right for a refused acquire (it prints
no HEADS-UP), and it also means `tv-lock.sh with -- <command that fails>` is not nudged. Registering the
hook there and reading `error` is the whole fix, if the owner wants it.

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
    return INSTRUCTION.format(line=line) if line else None


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
