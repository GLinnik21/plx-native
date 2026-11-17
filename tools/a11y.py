#!/usr/bin/env python3
"""Drive and read the app BY ID: the harness side of the `focus:` / `press:` / `state:` FIFO tokens.

The app (a `devtriggers` build, so not a `RELEASE=1` one) takes three accessibility-id tokens (the
a11y-ids plan, D1-D8):

    focus:<id>             move focus to the element the id names
    press:<id>             focus it, then press OK (an ordinary OK Down and Up)
    press:<id>@<subject>   ... but only if the element's subject (a card's ratingKey) is <subject>;
                           a mismatch refuses and presses nothing
    state:<nonce>          replace `plxnative-a11y.json` in the runtime directory with a snapshot of the
                           page that owns input: its route word, who owns input, the focused full id,
                           every focus group and every element with its id, role, rest rectangle,
                           visibility, focus, loaded flag, label and subject

An id is `<ns>.<path>` (`nav.watchlist`, `home.hero.play`, `home.shelf.2.card.5`, `watchlist.grid.card.3`):
a namespace `[a-z][a-z0-9_-]*`, then segments that are each a name `[a-z][a-z0-9_]*` or a canonical decimal
index. A refused id leaves one event-log line, `a11y: refused <act> <id> <reason> owner=<ns>`; nothing moves.

The state file is replaced atomically, so a reader never sees half of one; but it may still be the PREVIOUS
request's answer, which is why every request carries a fresh nonce and a reader waits until the file names it.

    from a11y import focus, press, expect_focus, state, send, wait_for
    focus(fifo, "nav.watchlist")                       # no key walk: the id names the target
    expect_focus(runtime, "nav.watchlist")             # `state`, then compare the focused id (FocusMismatch)
    press(fifo, "home.hero.play")
    snap = state(runtime)                              # send `state:<random nonce>`, wait for it
    ids = [e["id"] for e in snap["elements"]]
    wait_for(lambda: any(e["id"] == "nav.watchlist" for e in state(runtime)["elements"]), timeout=10)

Prefer these to key walks for REACHING a target. A walk stays right for a test whose subject is travel (an
fps walk, a focus fingerprint) and for keys whose subject is the key itself (a hold, a timing, back).

A snapshot holds titles and rating keys. It is for the harness that asked: do not paste it into an
issue, a log or a recording.

Command line (a manual smoke: boot the simulator on the mock, then):

    python3 tools/a11y.py state  <runtime-dir> [--timeout SECONDS]
    python3 tools/a11y.py send   <runtime-dir> <token>
    python3 tools/a11y.py focus  <runtime-dir> <id>
    python3 tools/a11y.py press  <runtime-dir> <id> [--subject RATINGKEY]
    python3 tools/a11y.py expect <runtime-dir> <id> [--timeout SECONDS]

`<runtime-dir>` is the app's runtime directory (`PLXNATIVE_RUNTIME_DIR` under the simulator); the
token goes into its `plxnative-remote` FIFO. On the television use `tools/tv-session.sh focus|press|state`.
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


# The id grammar the app enforces (`plx_ui::a11y::A11yPath::parse_id`): `<ns>.<path>`, the namespace
# `[a-z][a-z0-9_-]*`, at least one segment after it, each a name `[a-z][a-z0-9_]*` or a canonical
# decimal index (`0`, or digits with no leading zero: one element has one spelling).
ID_RE = re.compile(r"[a-z][a-z0-9_-]*(\.([a-z][a-z0-9_]*|0|[1-9][0-9]*))+")
# A press subject: printable ASCII with no space and no `@` (`by_id_subject_ok`).
SUBJECT_RE = re.compile(r"[\x21-\x3f\x41-\x7e]+")
REFUSED_PREFIX = "a11y: refused "


def valid_id(text):
    """Is `text` a full accessibility id in the app's grammar?"""
    return isinstance(text, str) and ID_RE.fullmatch(text) is not None


def valid_subject(text):
    """May `text` ride after the `@` of `press:<id>@<subject>`?"""
    return isinstance(text, str) and SUBJECT_RE.fullmatch(text) is not None


def focus_token(id_):
    """`focus:<id>`. Raises ValueError for an id the app would refuse as malformed."""
    if not valid_id(id_):
        raise ValueError(f"not an accessibility id (<ns>.<segment>..., e.g. nav.watchlist): {id_!r}")
    return f"focus:{id_}"


def press_token(id_, subject=None):
    """`press:<id>`, or `press:<id>@<subject>` when the press must name the card it means."""
    if not valid_id(id_):
        raise ValueError(f"not an accessibility id (<ns>.<segment>..., e.g. nav.watchlist): {id_!r}")
    if subject is None:
        return f"press:{id_}"
    if not valid_subject(subject):
        raise ValueError(f"a press subject is printable ASCII with no space and no '@': {subject!r}")
    return f"press:{id_}@{subject}"


def parse_token(token):
    """The inverse of the two builders: `(act, id, subject)` for a `focus:` or `press:` token (`subject` is
    None without one). Raises ValueError for anything else, a malformed id, or a `focus:` with a subject."""
    for act, build in (("focus", focus_token), ("press", press_token)):
        if token.startswith(act + ":"):
            id_, at, subject = token[len(act) + 1:].partition("@")
            if at and act == "focus":
                raise ValueError(f"a focus token takes no subject: {token!r}")
            subject = subject if at else None
            if act == "focus":
                build(id_)
            else:
                build(id_, subject)
            return act, id_, subject
    raise ValueError(f"not a focus: or press: token: {token!r}")


def refusals(log_text):
    """The `a11y: refused <act> <id> <reason> owner=<ns>` lines of an event log, in order. They carry
    words and ids only (never a label or a subject), so they are safe to print."""
    out = []
    for line in log_text.splitlines():
        at = line.find(REFUSED_PREFIX)
        if at >= 0:
            out.append(line[at:].strip())
    return out


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


def focus(fifo, id_):
    """Move focus to the element `id_` names: `focus:<id>` into the FIFO. Nothing is waited for; a refused
    id logs `a11y: refused focus ...` and moves nothing (`expect_focus` is how a caller knows it landed)."""
    send(fifo, focus_token(id_))


def press(fifo, id_, subject=None):
    """Focus `id_` and press OK on it, once. With `subject` (a card's ratingKey) the app presses only if the
    element's subject is that; a mismatch presses nothing and logs `a11y: refused press ... subject_mismatch`."""
    send(fifo, press_token(id_, subject))


class FocusMismatch(AssertionError):
    """The app's focused id is not the one a caller expected. `want` and `got` are ids (`got` None when
    nothing is focused) and `snapshot` is the last answer, whose `route` and `owner` words the message carries."""

    def __init__(self, want, got, snapshot):
        self.want, self.got, self.snapshot = want, got, snapshot
        route, owner = snapshot.get("route"), snapshot.get("owner")
        super().__init__(f"expected focus on {want}, the app has {got!r} (route {route!r}, input owned by {owner!r})")


def expect_focus(runtime, want, timeout=DEFAULT_TIMEOUT_S, fifo=None, interval=POLL_S, retry_s=0.25,
                 clock=time.monotonic, sleep=time.sleep):
    """Return the snapshot once the app's focused id is `want`, asking again (a fresh `state` each time)
    until `timeout` seconds pass: focus lands a frame after its token, and a page's first tile takes it only
    when its rows land. Raises FocusMismatch (naming both ids) when time is up with the app answering
    something else, and TimeoutError when the app never answered at all (not running, a RELEASE=1 build)."""
    if not valid_id(want):
        raise ValueError(f"not an accessibility id (<ns>.<segment>..., e.g. nav.watchlist): {want!r}")
    deadline = clock() + timeout
    last = None
    while True:
        remaining = max(deadline - clock(), 0.0)
        try:
            last = state(runtime, timeout=max(min(DEFAULT_TIMEOUT_S, remaining), interval), fifo=fifo,
                         interval=interval, clock=clock, sleep=sleep)
        except TimeoutError:
            if last is None:
                raise
            raise FocusMismatch(want, last.get("focused"), last) from None
        if last.get("focused") == want:
            return last
        if clock() >= deadline:
            raise FocusMismatch(want, last.get("focused"), last)
        sleep(min(retry_s, max(deadline - clock(), 0.0)))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p_state = sub.add_parser("state", help="request a snapshot and print it")
    p_state.add_argument("runtime")
    p_state.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S)
    p_send = sub.add_parser("send", help="write one token to the FIFO")
    p_send.add_argument("runtime")
    p_send.add_argument("token")
    p_focus = sub.add_parser("focus", help="focus the element an id names")
    p_focus.add_argument("runtime")
    p_focus.add_argument("id")
    p_press = sub.add_parser("press", help="focus an element and press OK on it")
    p_press.add_argument("runtime")
    p_press.add_argument("id")
    p_press.add_argument("--subject", help="press only if the element's subject (a ratingKey) is this")
    p_expect = sub.add_parser("expect", help="wait until the focused id is the one given (exit 1 if not)")
    p_expect.add_argument("runtime")
    p_expect.add_argument("id")
    p_expect.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S)
    args = parser.parse_args(argv)
    fifo = pathlib.Path(args.runtime, FIFO_NAME)
    if args.command == "state":
        json.dump(state(args.runtime, args.timeout), sys.stdout, indent=1)
        print()
    elif args.command == "focus":
        focus(fifo, args.id)
    elif args.command == "press":
        press(fifo, args.id, args.subject)
    elif args.command == "expect":
        try:
            expect_focus(args.runtime, args.id, args.timeout)
        except FocusMismatch as e:
            print(e, file=sys.stderr)
            return 1
    else:
        send(fifo, args.token)
    return 0


if __name__ == "__main__":
    sys.exit(main())
