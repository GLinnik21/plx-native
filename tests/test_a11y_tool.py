"""tools/a11y.py: the nonce matching of the `state:<nonce>` read-back, the id grammar, the `focus:` /
`press:` tokens and `expect_focus`, with no app, no television and no real clock. A fake clock and an
injected `sleep` stand in for time; the "app" is a function that reads the token the tool wrote to a real
FIFO and answers into a temp directory the way the app does (replace the state file whole).

It also holds the harness half of the first by-id migration: tools/watchlist_captures.py's step
classification and focus check, and the committed tests/scenarios/watchlist_captures.json held to the
id grammar (no simulator is needed to know a token is malformed, or that a walk crept back in)."""
import importlib.util
import json
import os
import pathlib
import re
import shutil
import sys
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("a11y_tool", ROOT / "tools" / "a11y.py")
a11y = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(a11y)

# The capture driver does `import a11y` from its own directory; give it THIS module so the exceptions it
# catches are the ones the tests raise.
sys.modules["a11y"] = a11y
_cspec = importlib.util.spec_from_file_location("watchlist_captures_tool", ROOT / "tools" / "watchlist_captures.py")
captures = importlib.util.module_from_spec(_cspec)
_cspec.loader.exec_module(captures)

HAS_FIFO = hasattr(os, "mkfifo")


class Clock:
    """A clock that only moves when something sleeps on it."""

    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def answer_file(runtime, doc):
    """Replace the state file whole, as the app's atomic writer does."""
    tmp = pathlib.Path(runtime, a11y.STATE_NAME + ".tmp")
    tmp.write_text(json.dumps(doc))
    os.replace(tmp, pathlib.Path(runtime, a11y.STATE_NAME))


class Nonces(unittest.TestCase):
    def test_a_fresh_nonce_is_in_the_apps_grammar_and_does_not_repeat(self):
        seen = {a11y.new_nonce() for _ in range(50)}
        self.assertEqual(len(seen), 50)
        for nonce in seen:
            self.assertRegex(nonce, r"^[A-Za-z0-9_-]{1,32}$")

    def test_only_the_file_naming_the_nonce_answers_it(self):
        self.assertTrue(a11y.answers({"nonce": "abc", "elements": []}, "abc"))
        self.assertFalse(a11y.answers({"nonce": "abd"}, "abc"), "another request's file")
        self.assertFalse(a11y.answers({"nonce": "ABC"}, "abc"), "no case folding")
        self.assertFalse(a11y.answers({"elements": []}, "abc"), "no nonce at all")
        self.assertFalse(a11y.answers({"nonce": 1}, "1"), "a number is not the string that was sent")
        self.assertFalse(a11y.answers(None, "abc"))
        self.assertFalse(a11y.answers(["abc"], "abc"))

    def test_an_explicit_nonce_outside_the_grammar_is_refused_before_anything_is_sent(self):
        for bad in ["", "a b", "a.b", "x" * 33, "é"]:
            with self.assertRaises(ValueError, msg=repr(bad)):
                a11y.state("/nonexistent", nonce=bad)


class ReadState(unittest.TestCase):
    def setUp(self):
        self.runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, self.runtime, ignore_errors=True)

    def test_a_missing_or_unreadable_file_is_none(self):
        self.assertIsNone(a11y.read_state(self.runtime))
        pathlib.Path(self.runtime, a11y.STATE_NAME).write_text('{"nonce": "a", "elem')
        self.assertIsNone(a11y.read_state(self.runtime), "half a document")
        pathlib.Path(self.runtime, a11y.STATE_NAME).write_text("[1, 2]")
        self.assertIsNone(a11y.read_state(self.runtime), "JSON, but not a snapshot")

    def test_a_snapshot_reads_back_whole(self):
        doc = {"schema": 1, "nonce": "n1", "focused": "home.shelf.0.card.1", "elements": [{"id": "nav.watchlist"}]}
        answer_file(self.runtime, doc)
        self.assertEqual(a11y.read_state(self.runtime), doc)


class WaitFor(unittest.TestCase):
    def test_it_returns_the_first_truthy_answer(self):
        clock = Clock()
        answers = iter([None, 0, "", {"found": True}])
        got = a11y.wait_for(lambda: next(answers), 10, interval=0.1, clock=clock, sleep=clock.sleep)
        self.assertEqual(got, {"found": True})
        self.assertAlmostEqual(clock.now, 0.3)

    def test_it_raises_when_the_time_is_up_and_never_waits_past_it(self):
        clock = Clock()
        tries = []
        with self.assertRaises(TimeoutError):
            a11y.wait_for(lambda: tries.append(1) and None, 1.0, interval=0.4, clock=clock, sleep=clock.sleep)
        self.assertAlmostEqual(clock.now, 1.0, msg="the last sleep is cut to the deadline")
        self.assertEqual(len(tries), 4, "0.0, 0.4, 0.8 and the deadline itself")

    def test_a_zero_timeout_still_asks_once(self):
        self.assertEqual(a11y.wait_for(lambda: "now", 0, clock=Clock(), sleep=lambda s: None), "now")


#: One token as the writer sees it, spelled with each separator the app cuts a FIFO line at:
#: `rust-modules/src/remote.rs` finds the end of the run at ASCII whitespace and then `split_whitespace`s it, so
#: VT, NEL, NBSP, the U+2000 block, the line and paragraph separators and the ideographic space split a word too.
SMUGGLED = ["back press:home.continue.card.0", "back\tpress:x", "back\rpress:x", "back\x0bpress:x", "back\x0cpress:x",
            "back\npress:x", "back\u0085press:x", "back press:x", "back press:x", "back press:x",
            "back press:x", "back press:x", "back press:x", "back press:x", "back press:x",
            "back　press:x"]


class Send(unittest.TestCase):
    def test_a_token_with_whitespace_or_nothing_is_refused(self):
        for bad in ["", "two words", "tab\tbed", "line\nbreak", " lead"] + SMUGGLED:
            with self.assertRaises(ValueError, msg=repr(bad)):
                a11y.send("/nonexistent", bad)

    @unittest.skipUnless(HAS_FIFO, "needs os.mkfifo")
    def test_a_refused_token_writes_nothing_and_a_non_space_word_is_one_token(self):
        """U+2019, U+2014 and U+2026 share lead bytes with the Unicode spaces; Cyrillic and accented text are words."""
        runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, runtime, ignore_errors=True)
        fifo = pathlib.Path(runtime, a11y.FIFO_NAME)
        os.mkfifo(fifo)
        reader = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, reader)
        for bad in SMUGGLED:
            with self.assertRaises(ValueError, msg=repr(bad)):
                a11y.send(fifo, bad)
        try:
            nothing = os.read(reader, 4096)   # b"" (no writer left) or, on some platforms, would-block
        except BlockingIOError:
            nothing = b""
        self.assertEqual(nothing, b"")
        words = ["txt:привет", "txt:café", "txt:it’s—a…b", "ck:10,20"]
        for word in words:
            a11y.send(fifo, word)
        self.assertEqual(os.read(reader, 4096).decode().split("\n")[:-1], words)

    @unittest.skipUnless(HAS_FIFO, "needs os.mkfifo")
    def test_one_token_is_one_newline_terminated_write_to_the_fifo(self):
        runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, runtime, ignore_errors=True)
        fifo = pathlib.Path(runtime, a11y.FIFO_NAME)
        os.mkfifo(fifo)
        reader = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, reader)
        a11y.send(fifo, "state:abc")
        a11y.send(fifo, "down")
        self.assertEqual(os.read(reader, 4096), b"state:abc\ndown\n")


@unittest.skipUnless(HAS_FIFO, "needs os.mkfifo")
class State(unittest.TestCase):
    def setUp(self):
        self.runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, self.runtime, ignore_errors=True)
        self.fifo = pathlib.Path(self.runtime, a11y.FIFO_NAME)
        os.mkfifo(self.fifo)
        self.reader = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, self.reader)
        self.clock = Clock()

    def requested(self):
        """The nonces the tool has sent so far, read off the FIFO like the app reads them."""
        try:
            data = os.read(self.reader, 4096).decode()
        except BlockingIOError:
            return []
        return [t[len("state:"):] for t in data.split() if t.startswith("state:")]

    def test_a_stale_file_is_ignored_until_the_one_naming_this_nonce_arrives(self):
        answer_file(self.runtime, {"nonce": "an-older-request", "focused": "stale"})
        polls = []

        def app_sleep(seconds):
            """The simulated app: it answers on the third idle poll, and only the request it was sent."""
            self.clock.sleep(seconds)
            polls.append(seconds)
            if len(polls) == 3:
                (nonce,) = self.requested()
                answer_file(self.runtime, {"nonce": nonce, "focused": "home.shelf.0.card.1", "elements": []})

        got = a11y.state(self.runtime, timeout=5, clock=self.clock, sleep=app_sleep)
        self.assertEqual(got["focused"], "home.shelf.0.card.1")
        self.assertEqual(len(polls), 3, "stale answers were waited out, not returned")
        self.assertRegex(got["nonce"], r"^[0-9a-f]{16}$")

    def test_an_answer_already_there_for_this_nonce_returns_without_waiting(self):
        answer_file(self.runtime, {"nonce": "fixed-1", "focused": None})
        got = a11y.state(self.runtime, nonce="fixed-1", clock=self.clock, sleep=self.clock.sleep)
        self.assertEqual(got["nonce"], "fixed-1")
        self.assertEqual(self.clock.now, 0.0)
        self.assertEqual(self.requested(), ["fixed-1"], "the request still went to the FIFO")

    def test_two_requests_in_a_row_each_wait_for_their_own_answer(self):
        def app_sleep(seconds):
            self.clock.sleep(seconds)
            for nonce in self.requested():
                answer_file(self.runtime, {"nonce": nonce, "answered": nonce})

        first = a11y.state(self.runtime, clock=self.clock, sleep=app_sleep)
        second = a11y.state(self.runtime, clock=self.clock, sleep=app_sleep)
        self.assertNotEqual(first["nonce"], second["nonce"])
        self.assertEqual(second["answered"], second["nonce"])

    def test_no_answer_times_out_naming_the_nonce_and_the_directory(self):
        answer_file(self.runtime, {"nonce": "stale-forever"})
        with self.assertRaises(TimeoutError) as caught:
            a11y.state(self.runtime, timeout=0.5, nonce="wanted-1", clock=self.clock, sleep=self.clock.sleep)
        message = str(caught.exception)
        self.assertIn("state:wanted-1", message)
        self.assertIn(self.runtime, message)
        self.assertGreaterEqual(self.clock.now, 0.5)
        self.assertEqual(self.requested(), ["wanted-1"])

    def test_the_request_is_a_state_token_in_the_apps_grammar(self):
        with self.assertRaises(TimeoutError):
            a11y.state(self.runtime, timeout=0.1, clock=self.clock, sleep=self.clock.sleep)
        (nonce,) = self.requested()
        self.assertTrue(re.fullmatch(a11y.NONCE_RE, nonce), nonce)


GOOD_IDS = [
    "nav.watchlist", "nav.profile", "home.hero.play", "home.shelf.2.card.5", "home.continue.card.0",
    "watchlist.grid.card.0", "watchlist.header.sort", "watchlist_menu.type_all", "library.rail.12",
    "itemmenu.version.0", "detail.hero.mark_watched", "under-posters.row.3", "player.control.subtitles",
]
BAD_IDS = [
    "", "home", "nav.", ".nav", "nav..x", "Nav.watchlist", "nav.Watchlist", "nav.watch-list", "nav.05",
    "nav.-1", "nav.a b", "nav.a;b", "nav.x'y", "nav.$HOME", "nav.a[0]", "nav.0x1", "under-posters.row-3",
    "nav.watchlist\nnav.home", "nav.watchlist ", " nav.watchlist", "-nav.x", "1nav.x", "nav.x@12", None, 7,
]


class IdGrammar(unittest.TestCase):
    """The app's grammar (`plx_ui::a11y::A11yPath::parse_id`), mirrored; tools/tv-session.sh mirrors it too
    (its `cmd_selftest` holds the same lists)."""

    def test_well_formed_ids_pass(self):
        for id_ in GOOD_IDS:
            self.assertTrue(a11y.valid_id(id_), id_)

    def test_everything_else_is_refused_before_it_is_sent(self):
        for id_ in BAD_IDS:
            self.assertFalse(a11y.valid_id(id_), repr(id_))

    def test_an_index_is_canonical_and_a_hyphen_is_only_for_the_namespace(self):
        self.assertTrue(a11y.valid_id("nav.0"))
        self.assertTrue(a11y.valid_id("nav.10"))
        self.assertFalse(a11y.valid_id("nav.00"))
        self.assertFalse(a11y.valid_id("nav.01"))
        self.assertTrue(a11y.valid_id("a-b.c_d"))
        self.assertFalse(a11y.valid_id("a-b.c-d"))

    def test_a_press_subject_is_printable_ascii_with_no_space_and_no_at(self):
        for ok in ["1234", "5d7768ab", "a_b-c", "1.2", "x:y"]:
            self.assertTrue(a11y.valid_subject(ok), ok)
        for bad in ["", "1 2", "12@3", "tab\t", "é", "new\nline", None]:
            self.assertFalse(a11y.valid_subject(bad), repr(bad))


class Tokens(unittest.TestCase):
    def test_the_builders_and_the_parser_are_inverses(self):
        self.assertEqual(a11y.focus_token("nav.watchlist"), "focus:nav.watchlist")
        self.assertEqual(a11y.press_token("detail.hero.play"), "press:detail.hero.play")
        self.assertEqual(a11y.press_token("home.shelf.2.card.5", "1234"), "press:home.shelf.2.card.5@1234")
        for token, parsed in [
            ("focus:nav.watchlist", ("focus", "nav.watchlist", None)),
            ("press:detail.hero.play", ("press", "detail.hero.play", None)),
            ("press:watchlist.grid.card.3@9001", ("press", "watchlist.grid.card.3", "9001")),
        ]:
            self.assertEqual(a11y.parse_token(token), parsed)

    def test_a_malformed_id_or_subject_never_becomes_a_token(self):
        for id_ in BAD_IDS:
            with self.assertRaises(ValueError, msg=repr(id_)):
                a11y.focus_token(id_)
            with self.assertRaises(ValueError, msg=repr(id_)):
                a11y.press_token(id_)
        for subject in ["", "1 2", "12@3", "é"]:
            with self.assertRaises(ValueError, msg=repr(subject)):
                a11y.press_token("watchlist.grid.card.3", subject)

    def test_focus_takes_no_subject_and_other_tokens_are_not_by_id(self):
        for token in ["focus:nav.watchlist@12", "focus:", "press:", "press:nav", "ok", "state:abc", "ck:1,2",
                      "FOCUS:nav.x", "press:nav.x@", "press:nav.x@a@b"]:
            with self.assertRaises(ValueError, msg=token):
                a11y.parse_token(token)

    def test_refusal_lines_are_picked_out_of_an_event_log(self):
        log = "\n".join([
            "12:00:00 boot: fine",
            "12:00:01 a11y: refused press watchlist.grid.card.3 subject_mismatch owner=watchlist",
            "12:00:02 route=watchlist",
            "a11y: refused focus nav.nope unknown owner=home  ",
        ])
        self.assertEqual(a11y.refusals(log), [
            "a11y: refused press watchlist.grid.card.3 subject_mismatch owner=watchlist",
            "a11y: refused focus nav.nope unknown owner=home",
        ])
        self.assertEqual(a11y.refusals("nothing here\n"), [])


def without_test_code(text):
    """`text` of a Rust file with every `#[cfg(test)]` item (a `mod tests { .. }`, a `mod tests;`, a fixture fn) and every
    `//` line removed: what the production build compiles, near enough to ask which messages it constructs."""
    out, i = [], 0
    attr = re.compile(r"#\[cfg\((?:test|all\(test|any\(test)[^\n]*\]\s*")
    while True:
        found = attr.search(text, i)
        if not found:
            out.append(text[i:])
            break
        out.append(text[i:found.start()])
        j = found.end()
        while text.startswith("#[", j):  # further attributes of the same item
            j = text.index("]", j) + 1
            while j < len(text) and text[j] in " \n":
                j += 1
        depth = 0
        while j < len(text):  # the item: up to its `;`, or its balanced braces
            c = text[j]
            j += 1
            if c == ";" and depth == 0:
                break
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    break
        i = j
    return "\n".join(line for line in "".join(out).splitlines() if not line.lstrip().startswith("//"))


class SubjectBearing(unittest.TestCase):
    """An element whose press ACTS ON a title it names (it plays or writes it, or opens that title's own page or menu
    where the title can change under the press) is pressed by NAMING that title, the ratingKey of the title the
    press acts on: the real-account rule is never to press OK on a title whose ratingKey you have not read, and OK on
    a Detail episode's still PLAYS it at once. ONE list of those id shapes lives in `a11y.SUBJECT_BEARING`;
    tools/tv-session.sh's `a11y_subject_bearing` is the same list."""

    SUBJECT_IDS = ["home.shelf.2.card.5", "home.continue.card.0", "watchlist.grid.card.7", "library.grid.card.3",
                   "collection.card.1", "detail.related.card.0", "detail.episodes.still.3", "detail.episodes.text.3",
                   "detail.season.2", "filmography.row.4", "catalog.primary", "home.hero.play", "home.hero.info",
                   "home.hero.watchlist", "detail.hero.play", "detail.hero.restart", "detail.hero.trailer",
                   "detail.hero.mark_watched", "detail.hero.mark_unwatched", "detail.hero.watchlist",
                   "itemmenu.play", "itemmenu.watchlist_remove", "itemmenu.version.0", "versions.row.0", "versions.row.3",
                   "player.control.previous", "player.control.next", "player.control.next_episode",
                   "player.control.from_beginning", "player.control.skip", "player.control.5", "player.control.12",
                   "player.section.0", "player.section.1", "player.failure.try_again",
                   "player.failure.play_automatically", "more.fold.previous", "more.fold.next",
                   "more.fold.from_beginning", "more.rung.auto", "more.rung.p720", "menu.audio.0", "menu.sub.1", "menu.off",
                   "menu.forced", "menu.hit.2"]
    # `detail.hero.go_to_show|alt|version|more` only navigate or open a menu or a panel: no title is acted on; the
    # player's panel openers, seeks, pause and the Up Next / Skip cancels start none (tests/player_shots.sh presses
    # `player.control.subtitles|audio|more` and `player.tab.*` bare)
    PLAIN_IDS = ["nav.watchlist", "detail.hero.go_to_show", "detail.hero.alt", "detail.hero.version",
                 "detail.hero.more", "detail.about.card",
                 "detail.about.languages", "detail.collection.heading", "under-posters.row.3", "filmography.tab.2",
                 "watchlist.header.sort", "player.control.play", "catalog.back",
                 "player.control.subtitles", "player.control.audio", "player.control.more", "player.control.play_pause",
                 "player.control.seek_back", "player.control.seek_forward", "player.control.watch",
                 "player.control.watch_credits", "player.control.chapters", "player.control.info",
                 "player.control.go_to_detail", "player.tab.info", "player.tab.chapters", "player.chapter.0",
                 "player.scrub", "player.failure.change_quality", "player.failure.repair", "player.failure.back",
                 "player.repair.cancel", "more.quality", "more.speed", "more.stats",
                 "more.rate.1500", "more.fold.go_to_detail", "more.fold.play_pause", "menu.boost", "menu.loudness",
                 "menu.timing", "menu.style", "menu.search", "menu.field.size", "menu.retry", "menu.lang.0",
                 "home.hero.action"]  # the failure read-out's retry: no title, so pressed bare

    def test_the_subject_bearing_shapes_are_matched_and_nothing_else(self):
        for id_ in self.SUBJECT_IDS:
            self.assertTrue(a11y.subject_bearing(id_), id_)
        for id_ in self.PLAIN_IDS:
            self.assertFalse(a11y.subject_bearing(id_), id_)

    def test_the_guarded_builder_wants_a_subject_for_each_and_the_library_builder_does_not(self):
        for id_ in self.SUBJECT_IDS:
            with self.assertRaises(ValueError, msg=id_) as e:
                a11y.guarded_press_token(id_)
            self.assertIn("--subject", str(e.exception))
            self.assertEqual(a11y.guarded_press_token(id_, "77"), f"press:{id_}@77")
            self.assertEqual(a11y.press_token(id_), f"press:{id_}")  # the library stays unguarded
        for id_ in self.PLAIN_IDS:
            self.assertEqual(a11y.guarded_press_token(id_), f"press:{id_}")

    def test_the_shell_guard_is_the_same_list(self):
        """tools/tv-session.sh cannot import this module, so its `case` line is read back and compared."""
        text = (ROOT / "tools" / "tv-session.sh").read_text()
        found = re.search(r"^a11y_subject_bearing\(\) \{.*?^\s*((?:[^\s|)]+\|)*[^\s|)]+)\) return 0 ;;", text,
                          re.S | re.M)
        self.assertIsNotNone(found, "tv-session.sh no longer has a11y_subject_bearing's case line")
        self.assertEqual(tuple(found.group(1).split("|")), a11y.SUBJECT_BEARING)

    def test_every_production_with_subject_call_is_a_site_the_list_was_derived_from(self):
        """`SUBJECT_BEARING` was read off the code (`git grep with_subject rust-modules`), one entry per production
        CALL site. A new call site, even in a file that already has one, is a new id shape: the COUNT per file
        fails here until the list above, tv-session.sh's copy and this table say what it presses (a table of
        files alone would pass a second call added beside the first)."""
        calls = {}
        for root, dirs, files in os.walk(ROOT / "rust-modules"):
            dirs[:] = [d for d in dirs if not d.startswith("target")]
            for name in files:
                path = pathlib.Path(root, name)
                if path.suffix == ".rs":
                    n = path.read_text(errors="replace").count(".with_subject(")
                    if n:
                        calls[path.relative_to(ROOT / "rust-modules").as_posix()] = n
        # file -> (calls in it, what it presses). The detail call is ONE closure, applied to four id shapes.
        production = {
            "ui/src/cards/mod.rs": (1, "card_a11y: <section>.card.<n>"),
            "screens/src/detail/mod.rs": (1, "detail.season.<i>, detail.episodes.still|text.<i>, the six detail.hero.* "
                                              "controls that play or write (one closure)"),
            "screens/src/filmography.rs": (1, "filmography.row.<i>"),
            "screens/src/catalog.rs": (1, "catalog.primary"),
            "screens/src/home/mod.rs": (1, "home.hero.play|info|watchlist (not the failure read-out's action)"),
            "screens/src/item_menu.rs": (1, "itemmenu.<row>, itemmenu.version.<n> (not a Catalog menu's rows)"),
            "screens/src/versions_panel.rs": (1, "versions.row.<n>: the Play Version panel's rows (OK plays)"),
            "screens/src/player/mod.rs": (3, "player.control.<word|i> (control_subject), player.section.<i>, "
                                             "player.failure.try_again|play_automatically"),
            "screens/src/player/overlay.rs": (2, "more.fold.previous|next|from_beginning (the More menu's folded rows), "
                                                 "more.rung.<name> (the quality rungs re-resolve the title); "
                                                 "menu.audio|sub|off|forced|hit (the Tracks menu's writes)"),
        }
        not_a_production_site = {
            "ui/src/a11y.rs": (1, "the builder itself and its tests"),
            "ui/src/dispatch/snapshot.rs": (1, "its own #[cfg(test)] fixture"),
            "ui/src/fixture.rs": (1, "FixtureHost, test-only"),
        }
        expected = {**production, **not_a_production_site}
        self.assertEqual(calls, {path: n for path, (n, _) in expected.items()})
        # every production site is spelled in the list: one glob of SUBJECT_BEARING per id shape it presses
        # the Home hero is three explicit ids, not a glob: `home.hero.action` (the failure read-out's retry) acts on
        # no title; the Detail hero is six explicit ids: `go_to_show`, `alt`, `version` and `more` only navigate
        for glob in ("*.card.*", "detail.episodes.*", "detail.season.*", "filmography.row.*", "catalog.primary",
                     "home.hero.play", "home.hero.info", "home.hero.watchlist",
                     "detail.hero.play", "detail.hero.restart", "detail.hero.trailer", "detail.hero.mark_watched",
                     "detail.hero.mark_unwatched", "detail.hero.watchlist", "itemmenu.*", "versions.row.*",
                     "player.control.previous", "player.control.next", "player.control.next_episode",
                     "player.control.from_beginning", "player.control.skip", "player.control.[0-9]*",
                     "player.section.*", "player.failure.try_again", "player.failure.play_automatically",
                     "more.fold.previous", "more.fold.next", "more.fold.from_beginning", "more.rung.*", "menu.audio.*", "menu.sub.*", "menu.off",
                     "menu.forced", "menu.hit.*"):
            self.assertIn(glob, a11y.SUBJECT_BEARING)
        self.assertEqual(len(a11y.SUBJECT_BEARING), 34)
        self.assertNotIn("menu.*", a11y.SUBJECT_BEARING)
        self.assertNotIn("home.hero.*", a11y.SUBJECT_BEARING)
        self.assertNotIn("detail.hero.*", a11y.SUBJECT_BEARING)
        # the player's controls are spelled by word, never `player.control.*`: its panel openers stay pressable bare
        self.assertNotIn("player.control.*", a11y.SUBJECT_BEARING)
        self.assertNotIn("more.fold.*", a11y.SUBJECT_BEARING)

    # The messages a screen constructs when a press PLAYS or WRITES a title: a Play request, a queue step or restart
    # (`PlayerReq::Command` carries Previous, Next and From beginning), the Play Version commit, the watched,
    # deck and watchlist writes, the catalog pill's write, the item menu's own actions that Detail and Home push
    # through `AppFx::ItemMenu` (`ItemMenuAction::Play`, `PlayFromStart`, `PlayTrailer`, `PlayVersion`,
    # `MarkWatched`, `MarkUnwatched`, `RemoveFromDeck`; a file that imports the type as `ItemMenuAction as
    # Action`, as Detail's hero and Home's do, is read under that alias too: `acting_hits`), and the Tracks menu's
    # two writes to the server: a track pick (`PlayerReq::CommitTrack`, which selects the part's streams) and a
    # subtitle-search result (`SubSearchCmd::Download`, which installs the subtitle on the item). It does NOT see
    # the presses that only OPEN a title's page or menu (a card, a credit, the Home carousel's Info): those are held
    # by the `with_subject` call count per file, and by `card_a11y`'s `*.card.*`.
    ITEM_MENU_ACTIONS = r"(?:Play|PlayFromStart|PlayTrailer|PlayVersion|MarkWatched|MarkUnwatched|RemoveFromDeck)"
    ITEM_MENU_ALIAS = re.compile(r"ItemMenuAction as (\w+)")
    TITLE_ACTING = re.compile(
        r"\b(ContentReq::Play\b|HomeReq::Play\b|LibraryReq::Play\b|AppMsg::PlayVersion|"
        r"PlayerReq::(?:Command|StandIn|RetryPlayback|PlayAutomatically|More|CommitTrack)\b|SubSearchCmd::Download\b|"
        r"Write::(?:Watched|Unwatched|RemoveFromDeck)\b|"
        r"ItemMenuAction::" + ITEM_MENU_ACTIONS + r"\b|"
        r"WatchlistAdd\b|WatchlistRemove\b|AppFx::Catalog\b|PlayIntent::\w+)")

    @classmethod
    def acting_hits(cls, source):
        """The title-acting messages the PRODUCTION code of one file constructs."""
        text = without_test_code(source)
        hits = cls.TITLE_ACTING.findall(text)
        for alias in cls.ITEM_MENU_ALIAS.findall(text):
            hits += re.findall(rf"\b{alias}::{cls.ITEM_MENU_ACTIONS}\b", text)
        return hits
    PLAYER_WORDS = ("player.control.previous", "player.control.next", "player.control.next_episode",
                    "player.control.from_beginning", "player.control.skip", "player.control.[0-9]*",
                    "player.section.*", "player.failure.try_again", "player.failure.play_automatically")
    # file -> the SUBJECT_BEARING entries guarding the presses it constructs. A file that constructs one of the
    # messages and is not here fails this test until someone has decided what its elements are: guard them (a subject
    # and an entry) or say why they are not pressable by id.
    TITLE_ACTING_FILES = {
        "screens/src/catalog.rs": ("catalog.primary",),
        "screens/src/detail/extras.rs": ("*.card.*",),   # an extra is a card: its subject is the clip's key
        "screens/src/detail/hero.rs": ("detail.hero.watchlist",),
        "screens/src/detail/mod.rs": ("detail.hero.play", "detail.hero.restart", "detail.hero.trailer",
                                      "detail.hero.mark_watched", "detail.hero.mark_unwatched", "detail.episodes.*",
                                      "versions.row.*"),   # receives AppMsg::PlayVersion; the panel names the title
        "screens/src/home/mod.rs": ("home.hero.play", "home.hero.watchlist", "*.card.*"),
        "screens/src/item_menu.rs": ("itemmenu.*",),
        "screens/src/item_menu/watchlist.rs": ("itemmenu.*",),   # a Catalog menu's rows have no key: unpressable
        "screens/src/library/recommended.rs": ("*.card.*",),
        "screens/src/player/mod.rs": PLAYER_WORDS,
        "screens/src/player/overlay.rs": ("more.fold.previous", "more.fold.next", "more.fold.from_beginning",
                                          "more.rung.*", "menu.audio.*", "menu.sub.*", "menu.off", "menu.forced",
                                          "menu.hit.*"),
        "screens/src/versions_panel.rs": ("versions.row.*",),
        "appkit/src/track_menu.rs": ("menu.hit.*",),   # a search result's OK asks the download; the overlay names the title
    }

    def test_every_file_that_constructs_a_title_acting_message_is_covered_by_the_list(self):
        """The systematic half of the guard: SUBJECT_BEARING is read off `with_subject` calls (what carries a
        subject), and this reads the other side (what ACTS). Every non-test file of the screens, appkit and ui
        crates that constructs a title-acting message is named in `TITLE_ACTING_FILES`, with the list entries that
        guard its presses, and no file there has stopped constructing one. An unguarded new Play, queue step or write
        is therefore a failure here, not a review finding."""
        found = {}
        for crate in ("screens", "appkit", "ui"):
            base = ROOT / "rust-modules" / crate / "src"
            for root, _dirs, files in os.walk(base):
                for name in files:
                    if not name.endswith(".rs") or name == "tests.rs" or name.endswith("_tests.rs"):
                        continue
                    path = pathlib.Path(root, name)
                    hits = set(self.acting_hits(path.read_text(errors="replace")))
                    if hits:
                        found[path.relative_to(ROOT / "rust-modules").as_posix()] = sorted(hits)
        self.assertEqual(set(found), set(self.TITLE_ACTING_FILES), found)
        for file, globs in self.TITLE_ACTING_FILES.items():
            for glob in globs:
                self.assertIn(glob, a11y.SUBJECT_BEARING, f"{file}: its presses are guarded by {glob}")

    def test_the_census_helper_reads_production_code_only(self):
        text = ("fn a() { ContentReq::Play }\n// PlayerReq::Command\n#[cfg(test)]\nmod tests { fn t() { PlayerReq::Command } }\n"
                "#[cfg(test)]\nmod more_tests;\nfn b() { Write::Watched }\n")
        self.assertEqual(sorted(self.acting_hits(text)), ["ContentReq::Play", "Write::Watched"])

    def test_the_census_reads_the_item_menu_actions_by_either_spelling(self):
        spelled = "fn a() { Some(ItemMenuAction::PlayTrailer { rk }) }\nfn b() { Write::RemoveFromDeck }\n"
        self.assertEqual(sorted(self.acting_hits(spelled)), ["ItemMenuAction::PlayTrailer", "Write::RemoveFromDeck"])
        aliased = ("fn a() {\n    use crate::registry::ItemMenuAction as Action;\n    Action::MarkWatched(rk);\n"
                   "    Action::WatchlistRemove(guid);\n}\n")
        self.assertEqual(sorted(self.acting_hits(aliased)), ["Action::MarkWatched", "WatchlistRemove"])
        # another enum called `Action` in a file that never imports the item menu's is none of the census's business
        self.assertEqual(self.acting_hits("fn a() { Action::Play(i); episodes::Action::Play(i) }\n"), [])

    def test_the_detail_closure_is_applied_to_exactly_the_shapes_the_list_names(self):
        """The one `.with_subject(` call in screens/src/detail/mod.rs sits inside a closure applied at each id shape
        it names (the hero's controls, a season tab, an episode), so the CALL count above cannot see a fourth
        application: this holds those, and a new one fails here until the list and the table say what it presses."""
        text = (ROOT / "rust-modules" / "screens" / "src" / "detail" / "mod.rs").read_text()
        self.assertEqual(text.count("with_subject(a, "), 3, "hero, season, episode (still and text)")

    def test_the_detail_hero_ids_are_the_controls_whose_subject_the_screen_tests_assert(self):
        """`hero_control_subject` names a ratingKey for each control that plays or writes and none for the rest, and
        the screens test `the_hero_controls_that_play_or_write_have_a_subject_and_the_rest_have_none` holds that table.
        This reads that table and `HeroCtl::a11y_name` back, so the `detail.hero.*` ids of SUBJECT_BEARING are
        exactly the controls with a subject: a control added to one side and not the other fails here."""
        hero = (ROOT / "rust-modules" / "screens" / "src" / "detail" / "hero.rs").read_text()
        names = dict(re.findall(r'HeroCtl::(\w+) => "(\w+)",', hero.split("pub const fn a11y_name", 1)[1].split("}", 1)[0]))
        self.assertEqual(len(names), 10, names)
        tests = (ROOT / "rust-modules" / "screens" / "src" / "detail" / "tests.rs").read_text()
        table = tests.split("fn the_hero_controls_that_play_or_write_have_a_subject_and_the_rest_have_none", 1)[1].split("\n}\n", 1)[0]
        acts = dict(re.findall(r"\(hero::HeroCtl::(\w+), (true|false)\)", table))
        self.assertEqual(set(acts), set(names), "the screens test places every control")
        with_subject = {f"detail.hero.{names[ctl]}" for ctl, flag in acts.items() if flag == "true"}
        listed = {shape for shape in a11y.SUBJECT_BEARING if shape.startswith("detail.hero.")}
        self.assertEqual(with_subject, listed)


@unittest.skipUnless(HAS_FIFO, "needs os.mkfifo")
class Fifo(unittest.TestCase):
    def setUp(self):
        self.runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, self.runtime, ignore_errors=True)
        self.fifo = pathlib.Path(self.runtime, a11y.FIFO_NAME)
        os.mkfifo(self.fifo)
        self.reader = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, self.reader)

    def written(self):
        try:
            return os.read(self.reader, 4096).decode().split()
        except BlockingIOError:
            return []

    def test_focus_and_press_write_their_tokens(self):
        a11y.focus(self.fifo, "nav.watchlist")
        a11y.press(self.fifo, "detail.hero.play")
        a11y.press(self.fifo, "home.shelf.0.card.1", "4711")
        self.assertEqual(self.written(), ["focus:nav.watchlist", "press:detail.hero.play", "press:home.shelf.0.card.1@4711"])

    def test_a_malformed_id_writes_nothing(self):
        with self.assertRaises(ValueError):
            a11y.focus(self.fifo, "Nav.Watchlist")
        with self.assertRaises(ValueError):
            a11y.press(self.fifo, "nav.x", "1 2")
        self.assertEqual(self.written(), [])


@unittest.skipUnless(HAS_FIFO, "needs os.mkfifo")
class CommandLine(unittest.TestCase):
    """`python3 tools/a11y.py <verb>`: a bad argument or a missing app is one line on stderr and an exit
    code, never a traceback (found by running it against the simulator: `focus Bad.Id` printed one)."""

    def setUp(self):
        self.runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, self.runtime, ignore_errors=True)
        self.fifo = pathlib.Path(self.runtime, a11y.FIFO_NAME)
        os.mkfifo(self.fifo)
        self.reader = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, self.reader)

    def run_main(self, *argv):
        import contextlib
        import io
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = a11y.main(list(argv))
        return code, err.getvalue()

    def written(self):
        try:
            return os.read(self.reader, 4096).decode().split()
        except BlockingIOError:
            return []

    def test_a_malformed_id_or_subject_is_one_line_and_exit_2_and_sends_nothing(self):
        for argv in (("focus", self.runtime, "Bad.Id"), ("press", self.runtime, "nav.x", "--subject", "a b"),
                     ("expect", self.runtime, "nav")):
            code, err = self.run_main(*argv)
            self.assertEqual(code, 2, argv)
            self.assertTrue(err.startswith("a11y: ") and err.count("\n") == 1, f"{argv}: {err!r}")
        self.assertEqual(self.written(), [])

    def test_a_subject_bearing_press_without_a_subject_is_refused_and_sends_nothing(self):
        for id_ in ("home.shelf.2.card.5", "detail.episodes.still.3", "detail.episodes.text.3", "detail.season.1",
                    "filmography.row.0", "catalog.primary", "home.hero.play", "home.hero.info", "home.hero.watchlist",
                    "itemmenu.play", "itemmenu.watchlist_remove", "itemmenu.version.0", "versions.row.1",
                    "player.control.next", "player.control.next_episode", "player.control.5", "player.section.0",
                    "player.failure.try_again", "more.fold.from_beginning", "more.rung.p720"):
            code, err = self.run_main("press", self.runtime, id_)
            self.assertEqual(code, 2, id_)
            self.assertTrue(err.startswith("a11y: ") and err.count("\n") == 1 and "--subject" in err, f"{id_}: {err!r}")
            code, err = self.run_main("send", self.runtime, f"press:{id_}")  # the raw writer is no way round it
            self.assertEqual(code, 2, id_)
            self.assertTrue("--subject" in err or "subject" in err, err)
        self.assertEqual(self.written(), [])

    def test_send_is_one_word_so_no_press_rides_in_behind_another_token(self):
        """`send "back press:<card>"` would write ONE line the app cuts into two tokens, the second a title's press
        with no subject named: it is refused (exit 2, one line, nothing written) for every separator the app uses."""
        for token in SMUGGLED + ["", " back", "back "]:
            code, err = self.run_main("send", self.runtime, token)
            self.assertEqual(code, 2, repr(token))
            self.assertTrue(err.startswith("a11y: ") and err.count("\n") == 1 and "ONE token" in err, f"{token!r}: {err!r}")
        self.assertEqual(self.written(), [])

    def test_a_subject_bearing_press_that_names_its_title_is_sent(self):
        self.assertEqual(self.run_main("press", self.runtime, "detail.episodes.still.3", "--subject", "77"), (0, ""))
        self.assertEqual(self.run_main("press", self.runtime, "home.hero.play", "--subject", "200622"), (0, ""))
        self.assertEqual(self.run_main("send", self.runtime, "press:itemmenu.watchlist_remove@2001"), (0, ""))
        self.assertEqual(self.run_main("send", self.runtime, "press:detail.episodes.text.0@78"), (0, ""))
        self.assertEqual(self.run_main("press", self.runtime, "detail.hero.mark_watched", "--subject", "200622"), (0, ""))
        self.assertEqual(self.run_main("press", self.runtime, "detail.hero.go_to_show"), (0, ""))   # only navigates
        self.assertEqual(self.run_main("press", self.runtime, "home.hero.action"), (0, ""))   # the retry names no title
        self.assertEqual(self.run_main("press", self.runtime, "versions.row.1", "--subject", "1001"), (0, ""))
        self.assertEqual(self.run_main("press", self.runtime, "player.control.next", "--subject", "200623"), (0, ""))
        self.assertEqual(self.run_main("press", self.runtime, "player.control.subtitles"), (0, ""))   # opens a panel
        self.assertEqual(self.run_main("send", self.runtime, "ok"), (0, ""))
        self.assertEqual(self.written(), ["press:detail.episodes.still.3@77", "press:home.hero.play@200622",
                                          "press:itemmenu.watchlist_remove@2001", "press:detail.episodes.text.0@78",
                                          "press:detail.hero.mark_watched@200622", "press:detail.hero.go_to_show",
                                          "press:home.hero.action", "press:versions.row.1@1001",
                                          "press:player.control.next@200623", "press:player.control.subtitles", "ok"])

    def test_the_library_press_stays_unguarded_for_a_card_with_no_subject(self):
        """The mock scenario presses `watchlist.grid.card.7`, a coming-soon card whose subject is null."""
        fifo = pathlib.Path(self.runtime, a11y.FIFO_NAME)
        a11y.press(fifo, "watchlist.grid.card.7")
        self.assertEqual(self.written(), ["press:watchlist.grid.card.7"])

    def test_the_verbs_write_their_tokens_and_exit_0(self):
        self.assertEqual(self.run_main("focus", self.runtime, "nav.watchlist"), (0, ""))
        self.assertEqual(self.run_main("press", self.runtime, "watchlist.grid.card.2", "--subject", "77"), (0, ""))
        self.assertEqual(self.run_main("send", self.runtime, "back"), (0, ""))
        self.assertEqual(self.written(), ["focus:nav.watchlist", "press:watchlist.grid.card.2@77", "back"])

    def test_no_fifo_or_no_answer_is_one_line_and_exit_1(self):
        gone = pathlib.Path(self.runtime, "nowhere")
        code, err = self.run_main("focus", str(gone), "nav.watchlist")
        self.assertEqual(code, 1)
        self.assertTrue(err.startswith("a11y: ") and err.count("\n") == 1, err)
        with mock.patch.object(a11y, "expect_focus", side_effect=TimeoutError("no snapshot named state:x")):
            code, err = self.run_main("expect", self.runtime, "nav.watchlist")
        self.assertEqual((code, err), (1, "a11y: no snapshot named state:x\n"))

    def test_a_focus_mismatch_names_both_ids_and_exits_1(self):
        err = a11y.FocusMismatch("nav.watchlist", "nav.home", {"route": "home", "owner": "page"})
        with mock.patch.object(a11y, "expect_focus", side_effect=err):
            code, text = self.run_main("expect", self.runtime, "nav.watchlist")
        self.assertEqual(code, 1)
        self.assertIn("nav.watchlist", text)
        self.assertIn("nav.home", text)


@unittest.skipUnless(HAS_FIFO, "needs os.mkfifo")
class ExpectFocus(unittest.TestCase):
    """`expect_focus` asks `state` again until the focused id is the one wanted; the fake app answers each
    request (by its nonce) with the next focus of a script."""

    def setUp(self):
        self.runtime = tempfile.mkdtemp(prefix="a11y-tool-")
        self.addCleanup(shutil.rmtree, self.runtime, ignore_errors=True)
        self.fifo = pathlib.Path(self.runtime, a11y.FIFO_NAME)
        os.mkfifo(self.fifo)
        self.reader = os.open(self.fifo, os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, self.reader)
        self.clock = Clock()

    def app(self, focuses, route="watchlist", owner="page"):
        """An idle hook that answers every state request with the next entry of `focuses` (the last repeats)."""
        script = list(focuses)
        asked = []

        def sleep(seconds):
            self.clock.sleep(seconds)
            try:
                data = os.read(self.reader, 4096).decode()
            except BlockingIOError:
                return
            for token in data.split():
                if token.startswith("state:"):
                    focused = script.pop(0) if len(script) > 1 else script[0]
                    asked.append(focused)
                    answer_file(self.runtime, {"nonce": token[len("state:"):], "route": route, "owner": owner,
                                               "focused": focused, "elements": []})
        return sleep, asked

    def test_it_returns_the_snapshot_when_the_focus_is_already_there(self):
        sleep, asked = self.app(["nav.watchlist"])
        got = a11y.expect_focus(self.runtime, "nav.watchlist", clock=self.clock, sleep=sleep)
        self.assertEqual(got["focused"], "nav.watchlist")
        self.assertEqual(asked, ["nav.watchlist"], "one request")

    def test_it_asks_again_until_the_focus_lands(self):
        sleep, asked = self.app([None, "home.hero.play", "nav.watchlist"])
        got = a11y.expect_focus(self.runtime, "nav.watchlist", timeout=10, clock=self.clock, sleep=sleep)
        self.assertEqual(got["focused"], "nav.watchlist")
        self.assertEqual(asked, [None, "home.hero.play", "nav.watchlist"], "a fresh state each time, until it matched")

    def test_a_focus_that_never_lands_is_a_mismatch_naming_both_ids_and_the_route(self):
        sleep, _ = self.app(["home.shelf.0.card.0"], route="home", owner="page")
        with self.assertRaises(a11y.FocusMismatch) as caught:
            a11y.expect_focus(self.runtime, "nav.watchlist", timeout=1, clock=self.clock, sleep=sleep)
        e = caught.exception
        self.assertIsInstance(e, AssertionError)
        self.assertEqual((e.want, e.got), ("nav.watchlist", "home.shelf.0.card.0"))
        message = str(e)
        for part in ["nav.watchlist", "home.shelf.0.card.0", "'home'", "'page'"]:
            self.assertIn(part, message)
        self.assertGreaterEqual(self.clock.now, 1.0, "it waited out its time")

    def test_an_app_that_never_answers_is_a_timeout_not_a_mismatch(self):
        with self.assertRaises(TimeoutError) as caught:
            a11y.expect_focus(self.runtime, "nav.watchlist", timeout=0.5, clock=self.clock, sleep=self.clock.sleep)
        self.assertNotIsInstance(caught.exception, a11y.FocusMismatch)

    def test_nothing_focused_reads_as_a_mismatch_with_none(self):
        sleep, _ = self.app([None])
        with self.assertRaises(a11y.FocusMismatch) as caught:
            a11y.expect_focus(self.runtime, "nav.watchlist", timeout=0.3, clock=self.clock, sleep=sleep)
        self.assertIsNone(caught.exception.got)

    def test_an_app_that_goes_quiet_after_answering_is_a_mismatch_with_what_it_said(self):
        answers = {"n": 0}

        def sleep(seconds):
            self.clock.sleep(seconds)
            data = b""
            try:
                data = os.read(self.reader, 4096)
            except BlockingIOError:
                pass
            if answers["n"] == 0 and data:
                answers["n"] += 1
                (token,) = data.decode().split()
                answer_file(self.runtime, {"nonce": token[len("state:"):], "focused": "home.hero.play"})

        with self.assertRaises(a11y.FocusMismatch) as caught:
            a11y.expect_focus(self.runtime, "nav.watchlist", timeout=8, clock=self.clock, sleep=sleep)
        self.assertEqual(caught.exception.got, "home.hero.play")

    def test_a_malformed_id_is_refused_before_anything_is_sent(self):
        with self.assertRaises(ValueError):
            a11y.expect_focus(self.runtime, "Nav.Watchlist", clock=self.clock, sleep=self.clock.sleep)
        try:
            data = os.read(self.reader, 4096)
        except BlockingIOError:
            data = b""
        self.assertEqual(data, b"", "no state request went out")


class CaptureDriver(unittest.TestCase):
    """tools/watchlist_captures.py: how a `keys` entry is classified and when `expect_focus` is checked."""

    def test_every_keys_entry_has_one_kind(self):
        for token, kind in [
            ("hold", "hold"), ("up", "key"), ("ok", "key"), ("back", "key"), ("ok@0.12", "key"),
            ("ck:854,91", "key"), ("mock:{\"pms\":\"fail\"}", "mock"), ("lib:{\"op\":\"add\"}", "lib"),
            ("sleep:3", "sleep"), ("focus:nav.watchlist", "byid"), ("press:detail.hero.watchlist@1001", "byid"),
            ("press:watchlist.grid.card.3@9001", "byid"),
        ]:
            self.assertEqual(captures.step_kind(token), kind, token)

    def test_the_at_of_a_press_subject_is_not_a_timing(self):
        """`press:<id>@<rk>` must never reach the `key@wait` branch, which would send the bare press and sleep <rk> s."""
        token = "press:watchlist.grid.card.3@9001"
        self.assertNotEqual(captures.step_kind(token), "key")
        self.assertEqual(captures.expect_position(["focus:nav.x", token]), 2, "not a timed key: checked after the walk")

    def test_the_focus_is_checked_after_the_walk_or_before_a_final_timed_key(self):
        self.assertEqual(captures.expect_position([]), 0)
        self.assertEqual(captures.expect_position(["focus:nav.watchlist"]), 1)
        self.assertEqual(captures.expect_position(["focus:nav.watchlist", "hold"]), 2)
        self.assertEqual(captures.expect_position(["focus:detail.hero.watchlist", "ok@0.12"]), 1,
                         "the shot must follow the timed key within its wait: check before it")
        self.assertEqual(captures.expect_position(["ok@0.12", "focus:nav.watchlist"]), 2, "only the LAST entry decides")

    def test_a_malformed_by_id_step_fails_before_anything_boots(self):
        captures.check_steps("a.png", ["focus:nav.watchlist", "press:home.shelf.0.card.1@12", "hold", "back"])
        for token in ["focus:Nav.Watchlist", "press:nav", "focus:nav.x@12", "press:nav.x@a b", "focus:"]:
            with self.assertRaises(captures.Failure, msg=token):
                captures.check_steps("a.png", [token])

    def test_no_expect_focus_checks_nothing(self):
        with mock.patch.object(a11y, "expect_focus", side_effect=AssertionError("must not ask")):
            captures.check_focus(pathlib.Path("/nonexistent"), {"file": "a.png"})

    def test_a_mismatch_fails_the_capture_with_both_ids_and_the_apps_refusals(self):
        runtime = pathlib.Path(tempfile.mkdtemp(prefix="a11y-tool-"))
        self.addCleanup(shutil.rmtree, runtime, ignore_errors=True)
        (runtime / "plxnative-events.log").write_text(
            "boot\na11y: refused focus watchlist.grid.card.99 unknown owner=watchlist\n")
        err = a11y.FocusMismatch("watchlist.grid.card.99", "watchlist.grid.card.0", {"route": "watchlist", "owner": "page"})
        with mock.patch.object(a11y, "expect_focus", side_effect=err):
            with self.assertRaises(captures.Failure) as caught:
                captures.check_focus(runtime, {"file": "a.png", "expect_focus": "watchlist.grid.card.99"})
        message = str(caught.exception)
        for part in ["a.png", "watchlist.grid.card.99", "watchlist.grid.card.0", "a11y: refused focus watchlist.grid.card.99 unknown"]:
            self.assertIn(part, message)

    def test_an_app_that_never_answers_fails_the_capture_naming_the_cause(self):
        with mock.patch.object(a11y, "expect_focus", side_effect=TimeoutError("no snapshot named state:x")):
            with self.assertRaises(captures.Failure) as caught:
                captures.check_focus(pathlib.Path("/nonexistent"), {"file": "a.png", "expect_focus": "nav.x"})
        self.assertIn("RELEASE=1", str(caught.exception))

    def test_a_capture_that_fails_before_its_shot_reports_the_failure_and_keeps_its_logs(self):
        """A `w6/x.png` capture that fails before its shot (here: the app never opens the FIFO) leaves its
        logs where the shot would have gone. The directory does not exist yet (the shot makes it), so the
        driver makes it in its `finally`: without it the copy raised FileNotFoundError, which hid the
        Failure and ended the whole run instead of printing `FAIL` and going on to the next capture."""
        out = pathlib.Path(tempfile.mkdtemp(prefix="a11y-tool-"))
        self.addCleanup(shutil.rmtree, out, ignore_errors=True)
        spec = {"file": "w6/x.png", "mock": "default", "triggers": {}, "keys": []}
        with mock.patch.object(captures, "sign_in"), mock.patch.object(captures, "post"), \
                mock.patch.object(captures, "require_stand_in"), \
                mock.patch.object(captures.subprocess, "Popen", return_value=mock.Mock()), \
                mock.patch.object(captures.time, "sleep"):
            with self.assertRaises(captures.Failure) as caught:
                captures.capture(pathlib.Path("plxnative-sim"), spec, {"boot_wait_s": 0, "key_wait_s": 0}, out)
        self.assertIn("plxnative-remote", str(caught.exception))
        self.assertTrue((out / "w6" / "x.out").is_file(), "the console log is kept beside the missing shot")

    def test_the_wait_is_passed_to_the_read_back(self):
        with mock.patch.object(a11y, "expect_focus", return_value={}) as asked:
            captures.check_focus(pathlib.Path("/nonexistent"), {"file": "a.png", "expect_focus": "nav.watchlist"})
        asked.assert_called_once_with(pathlib.Path("/nonexistent"), "nav.watchlist", timeout=captures.FOCUS_WAIT_S)


class CommittedScenario(unittest.TestCase):
    """The committed tests/scenarios/watchlist_captures.json, held to the grammar without a simulator."""

    # Captures with no `expect_focus`: a boot trigger decides what is focused (an account or item menu opens on
    # a row that depends on the session and the card), or a pointer click does (`ck:`, the one capture that
    # navigates by pointer: `w6/detail-offline.png`).
    NO_EXPECT = {"account-menu-new.png", "item-menu-new.png", "w6/detail-offline.png"}
    POINTER_WALKS = {"w6/detail-offline.png"}
    DIRECTIONS = {"up", "down", "left", "right"}

    @classmethod
    def setUpClass(cls):
        cls.doc = json.loads((ROOT / "tests" / "scenarios" / "watchlist_captures.json").read_text())
        cls.captures = {c["file"]: c for c in cls.doc["captures"]}

    def test_every_by_id_token_and_every_expect_focus_is_in_the_grammar(self):
        by_id = 0
        for file, spec in self.captures.items():
            captures.check_steps(file, spec["keys"])
            by_id += sum(captures.step_kind(k) == "byid" for k in spec["keys"])
            if "expect_focus" in spec:
                self.assertTrue(a11y.valid_id(spec["expect_focus"]), f"{file}: {spec['expect_focus']!r}")
        self.assertGreater(by_id, 10, "the migration is in the file")

    def test_no_capture_walks_with_direction_keys_to_reach_its_target(self):
        for file, spec in self.captures.items():
            walked = [k for k in spec["keys"] if k.partition("@")[0] in self.DIRECTIONS]
            self.assertEqual(walked, [], f"{file} walks with {walked}: name the target by id (focus:/press:)")

    def test_the_only_capture_that_navigates_by_pointer_is_the_named_one(self):
        clicking = {file for file, spec in self.captures.items() if any(k.startswith("ck:") for k in spec["keys"])}
        self.assertEqual(clicking, self.POINTER_WALKS)

    def test_the_texts_that_say_no_direction_key_stands_between_boot_and_target_say_the_exception(self):
        """tools/watchlist_captures.py's docstring, the scenario's comment and tests/README.md all state it."""
        for name, text in (("the scenario comment", self.doc["comment"]),
                           ("tools/watchlist_captures.py", (ROOT / "tools" / "watchlist_captures.py").read_text())):
            self.assertIn("w6/detail-offline.png", text, name)
            self.assertIn("ck:", text, name)
            self.assertNotIn("whose target no id is read for", text, name)
        readme = (ROOT / "tests" / "README.md").read_text()
        self.assertIn("w6/detail-offline.png", readme)

    def test_a_subject_bearing_press_in_the_scenario_is_the_named_coming_soon_card(self):
        """The driver writes a token raw (the library path, unguarded), so a title's press with no `@<subject>`
        is a decision: the one here is `watchlist.grid.card.7`, a coming-soon card whose subject is null. Every
        other title's press names it (`@<ratingKey>`), so a hero that moved or a menu opened on another card is
        refused by the app and fails the capture."""
        presses = [k for spec in self.captures.values() for k in spec["keys"]
                   if k.startswith("press:") and a11y.subject_bearing(k[len("press:"):].partition("@")[0])]
        self.assertEqual({k for k in presses if "@" not in k}, {"press:watchlist.grid.card.7"})
        self.assertEqual({k for k in presses if "@" in k},
                         {"press:itemmenu.watchlist_remove@2001", "press:home.hero.watchlist@200622",
                          "press:detail.hero.watchlist@1001"})

    def test_a_capture_without_expect_focus_is_a_named_exception(self):
        missing = {file for file, spec in self.captures.items() if "expect_focus" not in spec}
        self.assertEqual(missing, self.NO_EXPECT)

    def test_the_first_capture_is_the_nav_watchlist_tab_by_id(self):
        spec = self.captures["home-bookmark-tab-focused.png"]
        self.assertEqual(spec["keys"], ["focus:nav.watchlist"])
        self.assertEqual(spec["expect_focus"], "nav.watchlist")

    def test_the_keys_that_stay_are_the_ones_whose_subject_is_the_key(self):
        """A hold, a timing and back stay keys; the mid-flip captures focus by id and then send the timed OK."""
        for file in ["w6/detail-added-midflip.png", "home-hero/added-midflip.png"]:
            keys = self.captures[file]["keys"]
            self.assertEqual(len(keys), 2, file)
            self.assertEqual((captures.step_kind(keys[0]), keys[1]), ("byid", "ok@0.12"), file)
            self.assertEqual(captures.expect_position(keys), 1, file)
        for file in ["hold-menu.png", "w6/hold-menu.png", "after-removal.png", "w6/watchlist-hold.png",
                     "w6/watchlist-hold-unplayable.png"]:
            self.assertIn("hold", self.captures[file]["keys"], file)
        self.assertEqual(self.captures["filtered.png"]["keys"][-1], "back")

    def test_the_comment_describes_the_forms_the_driver_takes(self):
        comment = self.doc["comment"]
        for part in ["focus:<id>", "press:<id>", "expect_focus", "`hold`", "ok@0.12", "`back`", "state:"]:
            self.assertIn(part, comment)
        self.assertNotIn("sends each key as a FIFO token", comment, "the old recipe sentence is gone")

    def test_a_hold_that_opens_a_menu_expects_the_menus_namespace(self):
        for file in ["hold-menu.png", "w6/hold-menu.png", "w6/watchlist-hold.png", "w6/watchlist-hold-unplayable.png"]:
            self.assertTrue(self.captures[file]["expect_focus"].startswith("itemmenu."), file)
        self.assertTrue(self.captures["sort-filter-open.png"]["expect_focus"].startswith("watchlist_menu."))


if __name__ == "__main__":
    unittest.main()
