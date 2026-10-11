"""The link conditioner (`tests/link_conditioner.py`) and its use in `tests/mock_pms.py`:
the rate a body is written at, determinism under one seed, per-class profiles, run-time change and
the paging contract the conditioned test depends on. Host only, no app, no TV; the real-socket
cases take a second or two."""
import json
import sys
import pathlib
import shutil
import unittest
import urllib.error
import urllib.request
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import link_conditioner as lc  # noqa: E402
from mock_pms import serve  # noqa: E402


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, s):
        self.now += s


class Rate(unittest.TestCase):
    def test_a_body_takes_exactly_its_size_over_the_rate(self):
        for kbit, nbytes in ((780, 30_000), (240, 15_000), (8000, 120_000), (100, 1)):
            clock, out = FakeClock(), bytearray()
            took = lc.write_throttled(out.extend, b"x" * nbytes, kbit, clock=clock, sleep=clock.sleep)
            self.assertEqual(len(out), nbytes)
            self.assertAlmostEqual(took, lc.transfer_seconds(nbytes, kbit), delta=0.025)

    def test_a_listing_and_a_poster_take_different_times_at_one_rate(self):
        listing, poster = 30_000, 14_000
        self.assertGreater(lc.transfer_seconds(listing, 780), 2 * lc.transfer_seconds(poster, 780))

    def test_unlimited_is_one_write_and_no_sleep(self):
        clock, writes = FakeClock(), []
        lc.write_throttled(writes.append, b"abc", 0, clock=clock, sleep=clock.sleep)
        self.assertEqual((writes, clock.now), ([b"abc"], 0.0))

    def test_a_slow_writer_does_not_stretch_the_transfer(self):
        clock, out = FakeClock(), bytearray()

        def slow(chunk):  # each write itself costs time; the deadline pacing absorbs it
            out.extend(chunk)
            clock.now += 0.005
        took = lc.write_throttled(slow, b"x" * 10_000, 800, clock=clock, sleep=clock.sleep)
        self.assertAlmostEqual(took, lc.transfer_seconds(10_000, 800), delta=0.03)


class Classes(unittest.TestCase):
    def test_requests_fall_into_the_class_the_app_cares_about(self):
        for path, want in (("/photo/:/transcode", "image"), ("/library/metadata/5/thumb/1", "image"),
                           ("/library/sections/1/all", "listing"), ("/hubs", "listing"),
                           ("/library/search", "listing"), ("/_mock/link", None),
                           ("/library/parts/9/file.mkv", None)):
            self.assertEqual(lc.request_class(path), want, path)

    def test_a_class_override_leaves_the_other_class_alone(self):
        c = lc.Conditioner()
        c.set("image", lc.make_profile("edge"))
        self.assertEqual(c.plan("/hubs").delay_s, 0.0)
        self.assertGreaterEqual(c.plan("/photo/:/transcode", "url=a").delay_s, 0.4)
        c.set("listing", lc.make_profile("none", latency_ms=300))
        self.assertAlmostEqual(c.plan("/hubs").delay_s, 0.3)
        self.assertIsNone(c.plan("/_mock/requests"))

    def test_specs(self):
        self.assertEqual(lc.parse_spec("3g"), ("all", lc.PROFILES["3g"]))
        klass, p = lc.parse_spec("image=edge:error=0.1,kind=503")
        self.assertEqual((klass, p.kbit, p.error_rate, p.error_kind), ("image", 240, 0.1, "503"))
        self.assertEqual(lc.parse_spec(":kbit=500")[1].kbit, 500)
        for bad in ("nope", "disk=3g", "3g:kbit", "3g:speed=1", "3g:error=2", "3g:kbit=-1"):
            with self.assertRaises(ValueError, msg=bad):
                lc.parse_spec(bad)

    def test_every_named_profile_is_valid(self):
        for name in ("3g", "edge", "dsl", "lossy", "very-bad", "remote-wan"):
            self.assertIn(name, lc.PROFILES)
            lc.PROFILES[name].validate()


class Determinism(unittest.TestCase):
    def draws(self, seed, order):
        c = lc.Conditioner(seed=seed)
        c.set("all", lc.make_profile("lossy", error_rate=0.3))
        return {(path, nth): (round(p.delay_s, 6), p.fail)
                for path in order for nth in (0, 1)
                for p in [c.plan(path, "X-Plex-Container-Start=0")]}

    def test_one_seed_gives_one_answer_whatever_the_arrival_order(self):
        paths = [f"/library/metadata/{n}" for n in range(40)]
        self.assertEqual(self.draws(7, paths), self.draws(7, list(reversed(paths))))

    def test_another_seed_gives_another_answer(self):
        paths = [f"/library/metadata/{n}" for n in range(40)]
        self.assertNotEqual(self.draws(7, paths), self.draws(8, paths))

    def test_jitter_is_bounded_and_failures_follow_the_rate(self):
        c = lc.Conditioner(seed=3)
        c.set("all", lc.LinkProfile(latency_ms=100, jitter_ms=50, error_rate=0.25))
        plans = [c.plan(f"/hubs/{n}") for n in range(2000)]
        self.assertTrue(all(0.1 <= p.delay_s <= 0.15 + 1e-9 for p in plans))
        self.assertGreater(len({round(p.delay_s, 4) for p in plans}), 100)
        failed = sum(p.fail is not None for p in plans) / len(plans)
        self.assertAlmostEqual(failed, 0.25, delta=0.04)

    def test_a_retry_of_the_same_request_draws_afresh(self):
        c = lc.Conditioner(seed=1)
        c.set("all", lc.LinkProfile(error_rate=0.5))
        seen = {c.plan("/hubs", "a=1").fail for _ in range(40)}
        self.assertEqual(seen, {None, "reset"})  # a retry is not doomed to fail forever


class OnTheMock(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.srv, cls.pms = serve(0, seed=2, movies=60, link=["listing=none:latency=120"])
        cls.base = f"http://127.0.0.1:{cls.srv.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()
        cls.srv.server_close()

    def timed(self, path):
        t = time.monotonic()
        with urllib.request.urlopen(self.base + path, timeout=10) as r:
            r.read()
        return time.monotonic() - t

    def post(self, obj):
        req = urllib.request.Request(self.base + "/_mock/link", data=json.dumps(obj).encode(), method="POST")
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.loads(r.read())

    def test_fast_then_slow_then_fast_within_one_server(self):
        self.assertGreaterEqual(self.timed("/library/sections/1/all"), 0.11)
        self.post({"spec": "all=none"})
        self.assertLess(self.timed("/library/sections/1/all"), 0.1)
        self.post({"spec": "listing=none:latency=120"})
        self.assertGreaterEqual(self.timed("/library/sections/1/all"), 0.11)

    def test_the_wire_log_says_how_long_a_page_was_in_flight(self):
        self.post({"spec": "listing=none:latency=100"})
        urllib.request.urlopen(urllib.request.Request(self.base + "/_mock/wire", method="DELETE"), timeout=5)
        self.timed("/library/sections/1/all?X-Plex-Container-Start=24&X-Plex-Container-Size=24")
        rows = json.loads(urllib.request.urlopen(self.base + "/_mock/wire", timeout=5).read())["rows"]
        row = next(r for r in rows if r["path"] == "/library/sections/1/all")
        self.assertEqual((row["start"], row["size"], row["class"]), ("24", "24", "listing"))
        self.assertGreaterEqual(row["t_done"] - row["t_recv"], 0.09)

    def test_a_bad_profile_is_a_400_and_changes_nothing(self):
        before = self.post({"class": "image", "profile": "none"})
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.post({"spec": "warp=9"})
        self.assertEqual(e.exception.code, 400)
        self.assertEqual(self.post({"class": "image", "profile": "none"}), before)


class Faults(unittest.TestCase):
    PAGE = "/library/sections/1/all"

    def fails(self, c, path, start, n=1):
        return [bool(c.plan(path, f"X-Plex-Container-Start={start}").fail) for _ in range(n)]

    def test_each_page_fails_its_first_attempts_then_passes(self):
        c = lc.Conditioner()
        c.set_faults([lc.make_fault("^/library/sections/1/all$", attempts=2, kind="503")])
        self.assertEqual(self.fails(c, self.PAGE, 24, 4), [True, True, False, False])
        self.assertEqual(self.fails(c, self.PAGE, 48, 3), [True, True, False])  # another page: its own count
        self.assertEqual(c.plan(self.PAGE, "X-Plex-Container-Start=72").fail, "503")

    def test_the_head_page_and_other_endpoints_are_left_alone(self):
        c = lc.Conditioner()
        c.set_faults([lc.make_fault("^/library/sections/1/all$")])
        self.assertEqual(self.fails(c, self.PAGE, 0, 2), [False, False])
        self.assertEqual(c.plan(self.PAGE, "").fail, None)  # no start at all is the head
        self.assertEqual(self.fails(c, "/library/search", 24), [False])
        self.assertEqual(c.plan("/_mock/wire", "X-Plex-Container-Start=24"), None)

    def test_the_start_may_come_as_a_header_value(self):
        c = lc.Conditioner()
        c.set_faults([lc.make_fault("all$", kind="reset")])
        self.assertEqual(c.plan(self.PAGE, "type=1", start="24").fail, "reset")
        self.assertEqual(c.plan(self.PAGE, "type=1", start="24").fail, None)

    def test_rearming_forgets_the_counts_and_clearing_disarms(self):
        c = lc.Conditioner()
        fault = lc.make_fault("all$")
        c.set_faults([fault])
        self.assertEqual(self.fails(c, self.PAGE, 24, 2), [True, False])
        c.set_faults([fault])
        self.assertEqual(self.fails(c, self.PAGE, 24, 2), [True, False])
        c.set_faults([])
        self.assertEqual(self.fails(c, self.PAGE, 48, 1), [False])

    def test_a_fault_fails_even_on_an_unconditioned_link_and_beats_the_rate(self):
        c = lc.Conditioner(profiles={"all": lc.make_profile("none", error=0, kind="reset")})
        c.set_faults([lc.make_fault("all$", kind="503")])
        plan = c.plan(self.PAGE, "X-Plex-Container-Start=24")
        self.assertEqual((plan.fail, plan.delay_s), ("503", 0.0))

    def test_specs_parse_and_bad_ones_are_refused(self):
        f = lc.parse_fault("^/a$@attempts=3,kind=503,min_start=0")
        self.assertEqual((f.path, f.attempts, f.kind, f.min_start), ("^/a$", 3, "503", 0))
        self.assertEqual(lc.parse_fault("x").attempts, 1)
        for bad in ("x@attempts=0", "x@kind=slow", "x@nope=1", "(", "x@attempts"):
            with self.assertRaises(ValueError, msg=bad):
                lc.parse_fault(bad)


class FaultsOnTheWire(unittest.TestCase):
    def setUp(self):
        self.srv, self.pms = serve(0, seed=3, movies=60, link_faults=["^/library/sections/1/all$@kind=503"])
        self.base = f"http://127.0.0.1:{self.srv.server_address[1]}"

    def tearDown(self):
        self.srv.shutdown()
        self.srv.server_close()

    def get(self, query, headers=None):
        req = urllib.request.Request(self.base + "/library/sections/1/all?" + query, headers=headers or {})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status
        except urllib.error.HTTPError as e:
            return e.code

    def post(self, obj):
        req = urllib.request.Request(self.base + "/_mock/link", data=json.dumps(obj).encode(), method="POST")
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.loads(r.read())

    def test_a_page_fails_once_by_query_and_by_header_then_lands(self):
        q = "type=1&X-Plex-Container-Start=24&X-Plex-Container-Size=24"
        self.assertEqual([self.get(q), self.get(q)], [503, 200])
        h = {"X-Plex-Container-Start": "48", "X-Plex-Container-Size": "24"}
        self.assertEqual([self.get("type=1", h), self.get("type=1", h)], [503, 200])
        self.assertEqual(self.get("type=1&X-Plex-Container-Start=0&X-Plex-Container-Size=24"), 200)

    def test_post_rearms_and_the_state_is_readable(self):
        q = "type=1&X-Plex-Container-Start=24&X-Plex-Container-Size=24"
        self.assertEqual([self.get(q), self.get(q)], [503, 200])
        got = self.post({"faults": [{"path": "all$", "attempts": 2, "kind": "503"}]})
        self.assertEqual(got["faults"][0]["attempts"], 2)
        self.assertEqual([self.get(q) for _ in range(3)], [503, 503, 200])
        self.assertEqual(self.post({"faults": []})["faults"], [])
        self.assertEqual(self.get(q), 200)

    def test_a_bad_fault_is_a_400(self):
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.post({"faults": [{"path": "x", "kind": "slow"}]})
        self.assertEqual(e.exception.code, 400)


class MockSelftest(unittest.TestCase):
    @unittest.skipUnless(
        shutil.which("ffmpeg"),
        "needs ffmpeg: mock_pms.selftest() muxes real media; the host CI runner has none",
    )
    def test_the_mock_selftest_passes(self):
        import contextlib
        import io
        import mock_pms
        with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            mock_pms.selftest()


if __name__ == "__main__":
    unittest.main()
