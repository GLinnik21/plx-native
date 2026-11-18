#!/usr/bin/env python3
"""Drive and read the app BY ID: the harness side of the `focus:` / `press:` / `state:` FIFO tokens.

The app (a `devtriggers` build, so not a `RELEASE=1` one) takes three accessibility-id tokens (the
a11y-ids plan, D1-D8):

    focus:<id>             move focus to the element the id names
    press:<id>             focus it, then press OK (an ordinary OK Down and Up)
    press:<id>@<subject>   ... but only if the element's subject (the ratingKey of the title the press
                           acts on) is <subject>; a mismatch refuses and presses nothing
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
    press(fifo, "nav.watchlist")                       # an element with no subject: nothing to name
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
    python3 tools/a11y.py press  <runtime-dir> <id> [--subject RATINGKEY]    (a title's id REQUIRES --subject)
    python3 tools/a11y.py expect <runtime-dir> <id> [--timeout SECONDS]

`<runtime-dir>` is the app's runtime directory (`PLXNATIVE_RUNTIME_DIR` under the simulator); the
token goes into its `plxnative-remote` FIFO. Exit 0 is done; 1 is a focus that is not the expected one,
an app that never answered or a FIFO that is not there; 2 is an id, subject or token the grammar refuses
(nothing was sent). Each failure is one line on stderr, never a traceback.

A title is pressed by NAME. A `press` (or a `send press:<id>`) of an element that ACTS ON a title it names
(the ids are `SUBJECT_BEARING` below: the rule is stated there) needs `--subject <ratingKey>`, the ratingKey of
the title the press acts on, and exits 2 without it, because the simulator boots against the owner's real server
unless it is aimed at a mock, and OK on a title can play it. Only the COMMAND LINE refuses: the library
functions `press` / `press_token` stay unguarded for a script that drives the mock, so a script that imports
them against a real server has no check. `send` writes ONE word: a token with whitespace in it
(`"back press:card"`) is refused, because the app would cut it into two tokens and the second would never meet
the press guard.

On the television use `tools/tv-session.sh focus|press|state`.
"""
import argparse
import fnmatch
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


# The id shapes whose element carries a SUBJECT, the ratingKey of the title the press acts on (a catalog page's
# hex id). THE RULE: an element is on this list when its press ACTS ON a title it names: it plays, restarts,
# finishes or retries it, marks it watched or unwatched, bookmarks it, opens or removes it from a menu that acts
# on it, or opens that title's own page or menu where the title can change under the press (a card, a credit and
# the Home carousel's Info only open a page, and are on it). Navigation is not the test: only a control whose
# target the page fixes (Go to Show, Also available, the Play Version opener, More, a panel opener, a seek) is
# not, and stays pressable bare. The real-account rule is never to press OK on a title whose ratingKey you have
# not read, so the COMMAND LINE and `tools/tv-session.sh press` make the press name it (`--subject` / `--rk`) and
# the app refuses a mismatch. This tuple is THE list (the other spellings of it,
# `a11y_subject_bearing` in tools/tv-session.sh, the docs and the refusal message, describe it by that rule): it
# is read off the code, not guessed. Each production `A11y::with_subject` call SITE is an entry
# (`git grep -n with_subject rust-modules`), a test here holds the number of calls per file equal to the table
# below, and a census test reads every file that constructs a title-acting message (Play, PlayVersion, Resume, a
# queue step, a watched or watchlist write) and holds the set of those files equal to the table's.
#
#   *.card.*             ui/src/cards/mod.rs `card_a11y`: <section>.card.<n>, every poster, still and headshot
#   detail.episodes.*    screens/src/detail/mod.rs: `episodes.still.<i>` (OK plays) and `episodes.text.<i>`
#   detail.season.*      the same file: `season.<i>`, a season tab
#   filmography.row.*    screens/src/filmography.rs: `row.<i>`, a credit (its subject is the local copy's key)
#   catalog.primary      screens/src/catalog.rs: the Add to / On Watchlist pill (its subject is the hex id)
#   home.hero.play       screens/src/home/mod.rs: `hero.play|info|watchlist`, the hero item's ratingKey (three
#   home.hero.info       explicit ids, not a glob: `home.hero.action`, the failure read-out's retry, is not a title's
#   home.hero.watchlist  control and stays pressable)
#   detail.hero.play            screens/src/detail/mod.rs `hero_control_subject`: the Detail hero's six controls that
#   detail.hero.restart         PLAY or WRITE, each named by the ratingKey it acts on and not by the route's: Play and
#   detail.hero.trailer         Restart the episode (a show) or the picked version's movie, Mark watched / unwatched
#   detail.hero.mark_watched    the ON-DECK episode of a started show (`hero::hero_episode`, so a chain `press
#   detail.hero.mark_unwatched  detail.episodes.text.0 --rk E`, `press detail.hero.go_to_show`, `press
#   detail.hero.watchlist       detail.hero.mark_watched` cannot mark an episode nobody named), Watchlist and Trailer
#                               the page's own title. Explicit ids, not a glob: `go_to_show`, `alt`, `version` and
#                               `more` only navigate or open a menu and stay pressable bare
#   versions.row.*       screens/src/versions_panel.rs: the Play Version panel's `row.<n>` (OK plays the hero's title
#                        in that version): the Play pill's own key (`registry::versions_subject`)
#   itemmenu.*           screens/src/item_menu.rs: `<row>` and `version.<n>`, the menu's item's ratingKey
#   player.control.previous      screens/src/player/mod.rs `control_subject`, the player's control row: Previous and
#   player.control.next          Next the queue neighbour they would start, Up Next's `next_episode` the episode
#   player.control.next_episode  `up_next` holds, From beginning and the Skip pill (a final credits segment finishes
#   player.control.from_beginning  the item) the item playing. Explicit words, not a glob: `play_pause`, the seeks,
#   player.control.skip          `subtitles`, `audio`, `more`, `watch` and `watch_credits` start no title and stay
#                                pressable bare (tests/player_shots.sh opens its panels with them)
#   player.control.[0-9]*  the row's items an id spells by POSITION (a package placing two controls with one word):
#                          the position hides the word, so the whole spelling is guarded and each carries its
#                          command's key, else the item playing
#   player.section.*     the same file: the open Info card's buttons (From beginning, Go to details), positional too
#   player.failure.try_again          the failure read-out's two actions that resolve the failed play again: the key
#   player.failure.play_automatically of the REQUEST they repeat (`route::retry_rk`; the item playing, `cur_rk`, is
#                                     only installed when a plan lands, so a play that failed earlier differs).
#                                     `change_quality`, `repair` and `back` start no title
#   more.fold.previous      screens/src/player/overlay.rs: the More menu's folded rows that start a title (the
#   more.fold.next          package placed the command in no reachable state), `HudCommand::plays_rk`; the other
#   more.fold.from_beginning  folds start none
#   more.rung.*          the same file: the Quality page's rungs. A pick re-resolves the title: after a failed play it
#                        is the failure read-out's Retry at that rung (the request's key, as above), on a live one
#                        it asks the route again for the item playing (`quality_pick_subject`)
#   menu.audio.*         the same file, the Tracks menu: a track pick selects the part's streams on the PMS (`PUT
#   menu.sub.*           /library/parts/{id}`) and a subtitle-search result installs its subtitle on the item (`PUT
#   menu.off             /library/metadata/{rk}/subtitles`), each naming the item playing
#   menu.forced          (`TrackRow::writes_title`, an exhaustive match: a new row cannot compile without the decision)
#   menu.hit.*
#
# fnmatch globs, which are spelled exactly like the `case` patterns of `a11y_subject_bearing` in
# tools/tv-session.sh (tests/test_a11y_tool.py reads both and holds them equal). Not subject-bearing, so never
# in the list: `detail.hero.go_to_show|alt|version|more` (they navigate or open a menu), `detail.about.card`,
# `under-posters.row.3` and every other `*.row.*` outside Filmography and the Play Version panel. NOT GUARDED, on
# purpose: the rest of the Tracks menu (`menu.boost|loudness` reconfigure the live stream, `menu.timing|style|
# field.*|<field>.<n>|search_language|language.*|lang.*|other|search|retry` open a page or pick a local setting),
# the player's panel openers and seeks, `player.chapter.<i>` and the scrubber (they only seek) and `more.quality|
# speed|stats|rate.*` (a drill-in, a local switch, a speed). The discriminator for the reconfigures (`more.rate.*`,
# `menu.boost|loudness`): on a URI attempt they re-resolve the playing request too (`route::request_viewer_reconfigure`),
# but that needs a proven playhead and pause state, so it refuses without a live attempt: they can never START a
# title from a failed or idle session, and only a quality rung can (it retries the failed request).
#
# The rows of a Catalog item menu have NO ratingKey to name (a Watchlist title no server holds: its identity is a
# guid), so the guarded doors cannot press them at all (the library `press`, unguarded, still can): the safe side,
# because they act on a catalog title. `home.hero.action` is not in the list: it has no title (the failure
# read-out's retry, shown while there is no hero) and a bare press of it goes out.
SUBJECT_BEARING = ("*.card.*", "detail.episodes.*", "detail.season.*", "filmography.row.*", "catalog.primary",
                   "home.hero.play", "home.hero.info", "home.hero.watchlist",
                   "detail.hero.play", "detail.hero.restart", "detail.hero.trailer", "detail.hero.mark_watched",
                   "detail.hero.mark_unwatched", "detail.hero.watchlist", "itemmenu.*", "versions.row.*",
                   "player.control.previous", "player.control.next", "player.control.next_episode",
                   "player.control.from_beginning", "player.control.skip", "player.control.[0-9]*",
                   "player.section.*", "player.failure.try_again", "player.failure.play_automatically",
                   "more.fold.previous", "more.fold.next", "more.fold.from_beginning", "more.rung.*",
                   "menu.audio.*", "menu.sub.*", "menu.off", "menu.forced", "menu.hit.*")


def subject_bearing(id_):
    """Does the element `id_` names carry a title's subject (see SUBJECT_BEARING)?"""
    return any(fnmatch.fnmatchcase(id_, shape) for shape in SUBJECT_BEARING)


def focus_token(id_):
    """`focus:<id>`. Raises ValueError for an id the app would refuse as malformed."""
    if not valid_id(id_):
        raise ValueError(f"not an accessibility id (<ns>.<segment>..., e.g. nav.watchlist): {id_!r}")
    return f"focus:{id_}"


def press_token(id_, subject=None):
    """`press:<id>`, or `press:<id>@<subject>` when the press must name the title it acts on (`SUBJECT_BEARING`)."""
    if not valid_id(id_):
        raise ValueError(f"not an accessibility id (<ns>.<segment>..., e.g. nav.watchlist): {id_!r}")
    if subject is None:
        return f"press:{id_}"
    if not valid_subject(subject):
        raise ValueError(f"a press subject is printable ASCII with no space and no '@': {subject!r}")
    return f"press:{id_}@{subject}"


def guarded_press_token(id_, subject=None):
    """`press_token`, for a caller that is a person's or an agent's command: a subject-bearing id (see
    SUBJECT_BEARING) must name the title it means, or this raises ValueError and nothing is sent.
    `press_token` itself stays unguarded, because a library caller may mean an element whose subject is
    null (the mock scenario's coming-soon card `watchlist.grid.card.7`)."""
    if subject is None and valid_id(id_) and subject_bearing(id_):
        raise ValueError(f"press {id_}: this element is a title's, so the press must name the title with "
                         f"--subject <ratingKey> (read it from `state` first); nothing was sent")
    return press_token(id_, subject)


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
    must not contain whitespace and must end with one. A word with whitespace in it is TWO tokens to the app
    (`rust-modules/src/remote.rs` cuts at ASCII whitespace and then at any Unicode whitespace), and the
    second would never meet a guard written for the first, so it is refused here: `str.isspace` is the
    superset of both. A single `write` of fewer than PIPE_BUF bytes is atomic, so two writers cannot
    interleave inside a token. The FIFO is opened read-write: a write-only open blocks when the app is gone,
    and this one never does.
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
    """Focus `id_` and press OK on it, once. With `subject` (the ratingKey of the title the press acts on;
    `SUBJECT_BEARING` lists the ids that need one) the app presses only if the element's subject is that; a
    mismatch presses nothing and logs `a11y: refused press ... subject_mismatch`. UNGUARDED: nothing here
    refuses an unnamed press of a title-acting id, only the command line does (`guarded_press_token`)."""
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
    try:
        if args.command == "state":
            json.dump(state(args.runtime, args.timeout), sys.stdout, indent=1)
            print()
        elif args.command == "focus":
            focus(fifo, args.id)
        elif args.command == "press":
            send(fifo, guarded_press_token(args.id, args.subject))
        elif args.command == "expect":
            expect_focus(args.runtime, args.id, args.timeout)
        else:
            # ONE word: the app cuts a line at whitespace, so `back press:<card>` is two tokens and the
            # second would skip the press guard below. Checked here, before the guard, so the refusal says why.
            if not args.token or any(c.isspace() for c in args.token):
                raise ValueError(f"send writes ONE token, with no whitespace in it (the app would read {args.token!r} "
                                 f"as several, past the press guard); nothing was sent")
            act, id_, subject = parse_token(args.token) if args.token.startswith("press:") else (None, None, None)
            if act:  # the raw writer is no way round the guard: a `press:` token is held to it too
                guarded_press_token(id_, subject)
            send(fifo, args.token)
    except FocusMismatch as e:
        print(e, file=sys.stderr)
        return 1
    except ValueError as e:  # a malformed id, subject or token: nothing was sent
        print(f"a11y: {e}", file=sys.stderr)
        return 2
    except (TimeoutError, OSError) as e:  # the app never answered, or there is no FIFO to write to
        print(f"a11y: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
