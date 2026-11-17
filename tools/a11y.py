#!/usr/bin/env python3
"""Read what is on screen BY ID: the harness side of the app's `state:<nonce>` token.

The app (a `devtriggers` build, so not a `RELEASE=1` one) answers `state:<nonce>` by replacing
`plxnative-a11y.json` in its runtime directory with a snapshot of the page that owns input: its
route word, who owns input, the focused full id, every focus group and every element with its id,
role, rest rectangle, visibility, focus, loaded flag, label and subject (the a11y-ids plan, D7/D8).
The file is replaced atomically, so a reader never sees half of one; but it may still be the PREVIOUS
request's answer, which is why every request carries a fresh nonce and a reader waits until the file
names it.

    from a11y import state, send, wait_for
    snap = state(runtime)                              # send `state:<random nonce>`, wait for it
    ids = [e["id"] for e in snap["elements"]]
    wait_for(lambda: any(e["id"] == "nav.watchlist" for e in state(runtime)["elements"]), timeout=10)

A snapshot holds titles and rating keys. It is for the harness that asked: do not paste it into an
issue, a log or a recording.

Command line (a manual smoke: boot the simulator on the mock, then):

    python3 tools/a11y.py state <runtime-dir> [--timeout SECONDS]
    python3 tools/a11y.py send  <runtime-dir> <token>

`<runtime-dir>` is the app's runtime directory (`PLXNATIVE_RUNTIME_DIR` under the simulator); the
token goes into its `plxnative-remote` FIFO.
"""
import argparse
import json
import os
import pathlib
import re
import secrets
import sys
import time

FIFO_NAME = "plxnative-remote"
STATE_NAME = "plxnative-a11y.json"
# The grammar the app enforces: [A-Za-z0-9_-]{1,32}.
NONCE_RE = re.compile(r"[A-Za-z0-9_-]{1,32}")
DEFAULT_TIMEOUT_S = 5.0
POLL_S = 0.05


def new_nonce():
    """A fresh nonce in the app's grammar (16 hex digits)."""
    return secrets.token_hex(8)


def send(fifo, token):
    """Write ONE token to the remote FIFO, newline-terminated, in a single write.

    The FIFO is whitespace-delimited and a trailing partial token waits for its terminator, so a token
    must not contain whitespace and must end with one. A single `write` of fewer than PIPE_BUF bytes is
    atomic, so two writers cannot interleave inside a token. The FIFO is opened read-write: a
    write-only open blocks when the app is gone, and this one never does.
    """
    if not token or any(c.isspace() for c in token):
        raise ValueError(f"a FIFO token is one non-empty word with no whitespace: {token!r}")
    data = (token + "\n").encode()
    fd = os.open(fifo, os.O_RDWR)
    try:
        written = os.write(fd, data)
    finally:
        os.close(fd)
    if written != len(data):
        raise OSError(f"short write to {fifo}: {written} of {len(data)} bytes")


def read_state(runtime):
    """The state file's document, or None when there is none (yet) or it is not JSON."""
    try:
        text = pathlib.Path(runtime, STATE_NAME).read_text(encoding="utf-8")
        doc = json.loads(text)
    except (OSError, ValueError):
        return None
    return doc if isinstance(doc, dict) else None


def answers(doc, nonce):
    """Is `doc` the answer to the request that carried `nonce`? Anything else (another request's
    file, a stale one, none) is not."""
    return isinstance(doc, dict) and doc.get("nonce") == nonce


def wait_for(pred, timeout, interval=POLL_S, clock=time.monotonic, sleep=time.sleep):
    """Poll `pred()` until it answers something truthy, and return that. Raise TimeoutError when
    `timeout` seconds pass first (the predicate is always tried once, and once more at the deadline).
    `clock` and `sleep` are injectable so the wait is testable without waiting."""
    deadline = clock() + timeout
    while True:
        value = pred()
        if value:
            return value
        now = clock()
        if now >= deadline:
            raise TimeoutError(f"nothing satisfied the predicate within {timeout} s")
        sleep(min(interval, deadline - now))


def state(runtime, timeout=DEFAULT_TIMEOUT_S, fifo=None, nonce=None, interval=POLL_S,
          clock=time.monotonic, sleep=time.sleep):
    """Ask the app what is on screen and return its answer (the snapshot document).

    Sends `state:<nonce>` (a fresh random one unless `nonce` is given) to `fifo` (default: the
    runtime directory's `plxnative-remote`) and waits for the state file to name that nonce. A file
    left by an earlier request is ignored. Raises TimeoutError when the app does not answer in
    `timeout` seconds: it is not running, it was built without devtriggers, or its loop is stalled.
    """
    nonce = new_nonce() if nonce is None else nonce
    if not NONCE_RE.fullmatch(nonce):
        raise ValueError(f"not a nonce the app accepts ([A-Za-z0-9_-]{{1,32}}): {nonce!r}")
    send(fifo or pathlib.Path(runtime, FIFO_NAME), f"state:{nonce}")

    def answer():
        doc = read_state(runtime)
        return doc if answers(doc, nonce) else None

    try:
        return wait_for(answer, timeout, interval, clock, sleep)
    except TimeoutError:
        raise TimeoutError(f"no snapshot named state:{nonce} appeared in {runtime} within {timeout} s") from None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p_state = sub.add_parser("state", help="request a snapshot and print it")
    p_state.add_argument("runtime")
    p_state.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S)
    p_send = sub.add_parser("send", help="write one token to the FIFO")
    p_send.add_argument("runtime")
    p_send.add_argument("token")
    args = parser.parse_args(argv)
    if args.command == "state":
        json.dump(state(args.runtime, args.timeout), sys.stdout, indent=1)
        print()
    else:
        send(pathlib.Path(args.runtime, FIFO_NAME), args.token)
    return 0


if __name__ == "__main__":
    sys.exit(main())
