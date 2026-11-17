"""tools/a11y.py: the nonce matching of the `state:<nonce>` read-back, with no app, no television and no
real clock. A fake clock and an injected `sleep` stand in for time; the "app" is a function that reads
the token the tool wrote to a real FIFO and answers into a temp directory the way the app does
(replace the state file whole)."""
import importlib.util
import json
import os
import pathlib
import re
import shutil
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("a11y_tool", ROOT / "tools" / "a11y.py")
a11y = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(a11y)

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


class Send(unittest.TestCase):
    def test_a_token_with_whitespace_or_nothing_is_refused(self):
        for bad in ["", "two words", "tab\tbed", "line\nbreak", " lead"]:
            with self.assertRaises(ValueError, msg=repr(bad)):
                a11y.send("/nonexistent", bad)

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


if __name__ == "__main__":
    unittest.main()
