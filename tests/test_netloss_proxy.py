"""`tools/netloss-proxy.py`: the fault-injecting TCP proxy that makes a real-looking network loss.
Host only, real loopback sockets on port 0, a few seconds. The semantics that matter are the ones a
TV client sees: blackhole = silence with the connection still open (no FIN, no RST), reset = a RST,
refuse = ECONNREFUSED, a restore that delivers what was held, and throttle = ONE link rate shared by
every connection, timed at the receiver (the loopback buffers swallow megabytes, so a send's return
time says nothing)."""
import importlib.util
import json
import pathlib
import socket
import sys
import threading
import unittest
import urllib.error
import urllib.request
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tests"))
_spec = importlib.util.spec_from_file_location("netloss_proxy", ROOT / "tools" / "netloss-proxy.py")
np = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(np)

import mock_pms  # noqa: E402


class Echo:
    """An upstream that echoes every byte, and counts its accepts."""

    def __init__(self):
        self.srv = socket.socket()
        self.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(16)
        self.port = self.srv.getsockname()[1]
        self.accepts = 0
        threading.Thread(target=self._loop, daemon=True).start()

    def _loop(self):
        while True:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            self.accepts += 1
            threading.Thread(target=self._echo, args=(c,), daemon=True).start()

    @staticmethod
    def _echo(c):
        try:
            while True:
                d = c.recv(4096)
                if not d:
                    break
                c.sendall(d)
        except OSError:
            pass
        finally:
            c.close()

    def close(self):
        self.srv.close()


class ProxyCase(unittest.TestCase):
    def setUp(self):
        self.lines = []
        self.echo = Echo()
        self.proxy = np.NetlossProxy(("127.0.0.1", self.echo.port), log=self.lines.append).start()
        self.addCleanup(self.proxy.stop)
        self.addCleanup(self.echo.close)
        self.socks = []

    def connect(self, timeout=2.0):
        s = socket.create_connection(self.proxy.listen_addr, timeout=timeout)
        self.socks.append(s)
        self.addCleanup(s.close)
        return s

    def roundtrip(self, s, data=b"ping"):
        s.sendall(data)
        got = b""
        while len(got) < len(data):
            chunk = s.recv(4096)
            if not chunk:
                break
            got += chunk
        return got

    def ctl(self, mode=None, **body):
        url = "http://%s:%d/_proxy/mode" % self.proxy.control_addr
        if mode is None:
            return json.load(urllib.request.urlopen(url, timeout=2))
        req = urllib.request.Request(url, json.dumps({"mode": mode, **body}).encode(), method="POST")
        return json.load(urllib.request.urlopen(req, timeout=2))


class Pass(ProxyCase):
    def test_forwards_both_ways_and_logs_the_connection(self):
        s = self.connect()
        self.assertEqual(self.roundtrip(s, b"hello"), b"hello")
        s.close()
        deadline = time.time() + 2
        while time.time() < deadline and not any("closed" in l for l in self.lines):
            time.sleep(0.02)
        text = "\n".join(self.lines)
        self.assertIn("conn #1 open from 127.0.0.1:", text)
        self.assertIn("conn #1 closed", text)
        self.assertIn("up=5B down=5B", text)
        self.assertRegex(self.lines[-1], r"^netloss \d\d:\d\d:\d\d\.\d{3} ")


class Blackhole(ProxyCase):
    def test_existing_connection_goes_silent_without_fin_or_rst_then_delivers_on_pass(self):
        s = self.connect()
        self.assertEqual(self.roundtrip(s), b"ping")
        self.ctl("blackhole")
        s.sendall(b"lost?")
        s.settimeout(0.6)
        with self.assertRaises(socket.timeout):  # silence: not EOF (FIN) and not ECONNRESET
            s.recv(10)
        self.ctl("pass")
        s.settimeout(2)
        self.assertEqual(s.recv(10), b"lost?")  # held, then delivered, as TCP retransmission would

    def test_new_connection_is_accepted_but_never_answered_nor_forwarded(self):
        self.ctl("blackhole")
        s = self.connect()  # the kernel completes the handshake
        s.sendall(b"hello?")
        s.settimeout(0.6)
        with self.assertRaises(socket.timeout):
            s.recv(10)
        self.assertEqual(self.echo.accepts, 0, "a blackholed connection must not reach the upstream")
        self.ctl("pass")
        s.settimeout(2)
        self.assertEqual(s.recv(10), b"hello?")

    def test_upstream_data_is_held_too(self):
        s = self.connect()
        self.roundtrip(s)
        self.ctl("blackhole")
        time.sleep(0.4)
        s.sendall(b"x")  # echoed by the upstream, but the downstream leg is dead
        s.settimeout(0.5)
        with self.assertRaises(socket.timeout):
            s.recv(1)


class Bulk:
    """An upstream that sources and sinks bulk bytes. `GET n\\n` is answered with n bytes; `PUT n\\n`
    followed by n bytes is answered with `ok\\n` once all n have arrived."""

    def __init__(self):
        self.srv = socket.socket()
        self.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(16)
        self.port = self.srv.getsockname()[1]
        threading.Thread(target=self._loop, daemon=True).start()

    def _loop(self):
        while True:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(c,), daemon=True).start()

    @staticmethod
    def _serve(c):
        buf = b""
        try:
            while True:
                while b"\n" not in buf:
                    d = c.recv(65536)
                    if not d:
                        return
                    buf += d
                line, _, buf = buf.partition(b"\n")
                verb, n = line.split()
                n = int(n)
                if verb == b"GET":
                    c.sendall(b"x" * n)
                else:
                    got, buf = len(buf), b""
                    while got < n:
                        d = c.recv(1 << 20)
                        if not d:
                            return
                        got += len(d)
                    c.sendall(b"ok\n")
        except OSError:
            pass
        finally:
            c.close()

    def close(self):
        self.srv.close()


class FakeClock:
    """A clock the bucket's sleeps advance, so its arithmetic is tested without real time."""

    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, dt):
        self.now += dt


class ThrottleBucket(unittest.TestCase):
    RATE = 2_500_000  # 20000 kbps in bytes per second

    def bucket(self, rate=RATE):
        clock = FakeClock()
        b = np.Bucket(clock=clock, sleep=clock.sleep)
        b.configure(rate)
        return b, clock

    def test_a_fresh_link_starts_with_one_chunk_then_grants_ten_ms_quanta(self):
        b, clock = self.bucket()
        self.assertEqual(b.take(np.CHUNK, lambda: False), np.CHUNK)  # the burst
        self.assertEqual(clock.now, 0.0)
        n = b.take(np.CHUNK, lambda: False)
        self.assertLessEqual(n, self.RATE * 0.0125)  # about one 10 ms tick, never a 1-byte recv
        self.assertGreaterEqual(n, self.RATE * 0.0099)
        self.assertGreaterEqual(clock.now, 0.0099)

    def test_grants_never_exceed_the_link_rate_plus_one_burst(self):
        b, clock = self.bucket()
        granted = 0
        while clock.now < 2.0:
            granted += b.take(np.CHUNK, lambda: False)
        self.assertLessEqual(granted, np.CHUNK + self.RATE * clock.now)
        self.assertGreaterEqual(granted, np.CHUNK + self.RATE * clock.now - self.RATE * 0.0125)

    def test_an_idle_link_banks_no_more_than_the_burst_and_a_refund_returns_tokens(self):
        b, clock = self.bucket()
        clock.now += 60
        self.assertEqual(b.take(10 * np.CHUNK, lambda: False), np.CHUNK)
        t = clock.now
        b.give(1000)
        self.assertEqual(b.take(1000, lambda: False), 1000)
        self.assertEqual(clock.now, t, "refunded tokens are granted without waiting")

    def test_a_wait_bails_out_when_told_and_a_cleared_bucket_grants_nothing(self):
        b, clock = self.bucket()
        b.take(np.CHUNK, lambda: False)
        self.assertEqual(b.take(np.CHUNK, lambda: True), 0)
        b.clear()
        self.assertEqual(b.take(np.CHUNK, lambda: False), 0)

    def test_a_rate_that_needs_more_than_one_chunk_per_tick_still_keeps_up(self):
        b, clock = self.bucket(12_500_000)  # 100 Mbps: one CHUNK is 5 ms, so the burst grows to two ticks
        granted = 0
        while clock.now < 1.0:
            granted += b.take(np.CHUNK, lambda: False)
        self.assertGreaterEqual(granted, 12_500_000 * 0.97)


class ThrottleCase(ProxyCase):
    def setUp(self):
        super().setUp()
        self.bulk = Bulk()
        self.addCleanup(self.bulk.close)
        self.proxy.upstream = ("127.0.0.1", self.bulk.port)

    def fetch(self, s, n):
        """GET n bytes on `s`; the seconds until the last one arrived."""
        t0 = time.monotonic()
        s.sendall(b"GET %d\n" % n)
        got = 0
        while got < n:
            d = s.recv(1 << 20)
            if not d:
                self.fail("EOF after %d of %d bytes" % (got, n))
            got += len(d)
        return time.monotonic() - t0

    def assertRate(self, elapsed, nominal, tol=0.15):
        self.assertGreaterEqual(elapsed, nominal * (1 - tol), "faster than the link: %.2f s" % elapsed)
        self.assertLessEqual(elapsed, nominal * (1 + tol), "slower than the link: %.2f s" % elapsed)


class Throttle(ThrottleCase):
    def test_down_runs_at_the_link_rate_and_pass_restores_full_rate_on_the_same_connection(self):
        self.ctl("throttle", kbps=20000)
        s = self.connect(timeout=15)
        self.assertRate(self.fetch(s, 8_000_000), 3.2)  # 8 MB at 20 Mbps
        self.ctl("pass")
        fast = self.fetch(s, 8_000_000)  # the live connection, no reconnect
        self.assertLess(fast, 1.0, "pass must lift the limit: %.2f s" % fast)

    def test_the_limit_is_one_link_shared_by_every_connection(self):
        self.ctl("throttle", kbps=20000)
        socks = [self.connect(timeout=15) for _ in range(2)]
        t0 = time.monotonic()
        threads = [threading.Thread(target=self.fetch, args=(s, 4_000_000)) for s in socks]
        for t in threads:
            t.start()
        for t in threads:
            t.join(30)
        # 2 x 4 MB = 8 MB over ONE 20 Mbps link: 3.2 s (a per-connection limit would take 1.6 s).
        self.assertRate(time.monotonic() - t0, 3.2)

    def test_up_is_limited_too(self):
        self.ctl("throttle", kbps=20000)
        s = self.connect(timeout=15)
        t0 = time.monotonic()
        s.sendall(b"PUT 4000000\n" + b"y" * 4_000_000)
        self.assertEqual(s.recv(8), b"ok\n")
        self.assertRate(time.monotonic() - t0, 1.6)

    def test_a_new_rate_takes_effect_on_a_live_connection(self):
        s = self.connect(timeout=15)
        self.ctl("throttle", kbps=8000)
        self.assertRate(self.fetch(s, 1_000_000), 1.0)
        self.ctl("throttle", kbps=40000)
        fast = self.fetch(s, 1_000_000)  # 0.2 s at the new rate, a fifth of the old one
        self.assertTrue(0.1 < fast < 0.6, "%.2f s" % fast)

    def test_mode_endpoint_validates_kbps_and_reports_it(self):
        for body in ({}, {"kbps": 0}, {"kbps": -5}, {"kbps": "fast"}, {"kbps": True}, {"kbps": None},
                     {"kbps": float("nan")}, {"kbps": float("inf")}):
            with self.assertRaises(urllib.error.HTTPError, msg=repr(body)) as cm:
                self.ctl("throttle", **body)
            self.assertEqual(cm.exception.code, 400, body)
            self.assertEqual(self.proxy.mode, "pass")
        snap = self.ctl("throttle", kbps=27000)
        self.assertEqual((snap["mode"], snap["kbps"]), ("throttle", 27000))
        self.assertEqual(self.ctl()["kbps"], 27000)
        self.assertIsNone(self.ctl("pass")["kbps"])
        self.assertIsNone(self.ctl("blackhole")["kbps"])

    def test_blackhole_holds_a_throttled_stream_and_pass_resumes_it(self):
        self.ctl("throttle", kbps=8000)
        s = self.connect(timeout=15)
        s.sendall(b"GET 400000\n")
        got = len(s.recv(1 << 20))
        self.ctl("blackhole")
        s.settimeout(0.6)
        try:
            while True:  # what was already in flight, until the silence
                d = s.recv(1 << 20)
                self.assertTrue(d, "a blackhole must not FIN")
                got += len(d)
        except socket.timeout:
            pass
        self.assertLess(got, 400_000, "the stream was held, not finished")
        self.ctl("pass")
        s.settimeout(10)
        while got < 400_000:
            got += len(s.recv(1 << 20))
        self.assertEqual(got, 400_000)

    def test_the_log_names_the_rate(self):
        self.ctl("throttle", kbps=27000)
        self.ctl("pass")
        self.assertTrue(any("mode pass -> throttle:27000 " in l for l in self.lines), self.lines)
        self.assertTrue(any("mode throttle:27000 -> pass " in l for l in self.lines), self.lines)


class UpstreamRetry(ProxyCase):
    def test_a_connect_refused_at_the_burst_is_retried_until_the_upstream_answers(self):
        free = socket.socket()
        free.bind(("127.0.0.1", 0))
        port = free.getsockname()[1]
        free.close()
        self.proxy.upstream = ("127.0.0.1", port)
        s = self.connect(timeout=5)
        s.sendall(b"late")
        time.sleep(0.3)  # the first attempts are refused: nothing listens yet
        late = Echo.__new__(Echo)
        late.srv = socket.socket()
        late.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        late.srv.bind(("127.0.0.1", port))
        late.srv.listen(4)
        late.accepts = 0
        threading.Thread(target=late._loop, daemon=True).start()
        self.addCleanup(late.close)
        self.assertEqual(s.recv(10), b"late")


class Reset(ProxyCase):
    def test_existing_connections_get_a_rst_and_new_ones_are_reset_on_accept(self):
        s = self.connect()
        self.assertEqual(self.roundtrip(s), b"ping")
        self.ctl("reset")
        s.settimeout(2)
        with self.assertRaises(ConnectionResetError):
            s.recv(10)
        # The proxy RSTs on accept, which is after the kernel completed the handshake. On loopback
        # the RST can land before connect() has returned, so connect() itself may raise the reset:
        # that is the same outcome as the first read or write failing, so the connect is inside.
        with self.assertRaises((ConnectionResetError, BrokenPipeError)):
            n = self.connect()
            n.settimeout(2)
            n.sendall(b"a")
            time.sleep(0.2)
            n.sendall(b"b")
            n.recv(10)
        self.assertGreaterEqual(self.proxy.stats["reset"], 2)
        self.ctl("pass")
        self.assertEqual(self.roundtrip(self.connect()), b"ping")


class Refuse(ProxyCase):
    def test_listener_closed_then_reopened_on_the_same_port(self):
        port = self.proxy.listen_addr[1]
        keep = self.connect()
        self.assertEqual(self.roundtrip(keep), b"ping")
        self.ctl("refuse")
        time.sleep(0.4)
        with self.assertRaises(ConnectionRefusedError):
            socket.create_connection(("127.0.0.1", port), timeout=1)
        self.assertEqual(self.roundtrip(keep), b"ping")  # existing connections keep forwarding
        self.ctl("pass")
        self.assertEqual(self.proxy.listen_addr[1], port)
        self.assertEqual(self.roundtrip(self.connect()), b"ping")


class Control(ProxyCase):
    def test_mode_endpoint_reads_writes_and_rejects(self):
        self.assertEqual(self.ctl()["mode"], "pass")
        self.assertEqual(self.ctl("blackhole")["mode"], "blackhole")
        self.assertEqual(self.ctl()["mode"], "blackhole")
        with self.assertRaises(urllib.error.HTTPError) as cm:
            self.ctl("sideways")
        self.assertEqual(cm.exception.code, 400)
        self.assertEqual(self.proxy.mode, "blackhole")
        self.assertTrue(any("mode pass -> blackhole" in l for l in self.lines))

    def test_schedule_parse_and_run(self):
        self.assertEqual(np.parse_schedule("pass@70, blackhole@10"), [(10.0, "blackhole"), (70.0, "pass")])
        for bad in ("blackhole", "nope@3", "pass@x"):
            with self.assertRaises(ValueError):
                np.parse_schedule(bad)
        np.run_schedule(self.proxy, [(0.0, "blackhole"), (0.1, "pass")])
        self.assertEqual(self.proxy.mode, "pass")
        self.assertEqual([l.split("mode ")[1].split(" (")[0] for l in self.lines if " mode " in l],
                         ["pass -> blackhole", "blackhole -> pass"])

    def test_throttle_in_a_schedule_and_as_a_mode_spec(self):
        self.assertEqual(np.parse_schedule("pass@60, throttle:27000@5"),
                         [(5.0, "throttle:27000"), (60.0, "pass")])
        self.assertEqual(np.parse_mode_spec("throttle:27000"), ("throttle", 27000))
        self.assertEqual(np.parse_mode_spec("blackhole"), ("blackhole", None))
        for bad in ("throttle", "throttle:", "throttle:0", "throttle:-1", "throttle:x", "throttle:nan",
                    "throttle:inf", "pass:3", "nope:3"):
            with self.assertRaises(ValueError, msg=bad):
                np.parse_mode_spec(bad)
            with self.assertRaises(ValueError, msg=bad):
                np.parse_schedule(bad + "@3")
        np.run_schedule(self.proxy, [(0.0, "throttle:5000"), (0.05, "pass")])
        self.assertEqual(self.proxy.mode, "pass")
        self.assertEqual([l.split("mode ")[1].split(" (")[0] for l in self.lines if " mode " in l],
                         ["pass -> throttle:5000", "throttle:5000 -> pass"])


class MockAdvertisesTheProxy(unittest.TestCase):
    def test_advertise_port_names_the_proxy_in_the_resources_row(self):
        srv, pms = mock_pms.serve(0, seed=7, advertise_ip="127.0.0.1", advertise_port=45678)
        self.addCleanup(lambda s=srv: (s.shutdown(), s.server_close()))
        self.assertEqual(pms.account_resource["connections"][0]["port"], 45678)
        self.assertEqual(pms.account_resource["connections"][0]["uri"], "http://127.0.0.1:45678")
        srv2, pms2 = mock_pms.serve(0, seed=7, advertise_ip="127.0.0.1")
        self.addCleanup(lambda s=srv2: (s.shutdown(), s.server_close()))
        self.assertEqual(pms2.account_resource["connections"][0]["port"], srv2.server_address[1])


class RateLog(ThrottleCase):
    """`--rate-log`: bytes DELIVERED per interval, the proof of which side limited a run."""

    def setUp(self):
        unittest.TestCase.setUp(self)
        self.lines = []
        self.bulk = Bulk()
        self.addCleanup(self.bulk.close)
        self.proxy = np.NetlossProxy(("127.0.0.1", self.bulk.port), log=self.lines.append,
                                     rate_log=0.25).start()
        self.addCleanup(self.proxy.stop)
        self.socks = []

    def rates(self):
        import re
        out = []
        for line in list(self.lines):
            m = re.search(r" rate down=(\d+)B up=(\d+)B over=(\d+)ms mode=(\S+) conns=(\d+)(?: top=#(\d+):(\d+)B)?", line)
            if m:
                out.append({"down": int(m[1]), "up": int(m[2]), "over": int(m[3]), "mode": m[4],
                            "top": (int(m[6]), int(m[7])) if m[6] else None})
        return out

    def settle(self):
        deadline = time.time() + 3
        while time.time() < deadline and not (self.rates() and self.rates()[-1]["down"] == 0):
            time.sleep(0.05)

    def test_a_pass_download_is_counted_in_full_and_names_its_connection(self):
        s = self.connect(timeout=15)
        self.fetch(s, 6_000_000)
        self.settle()
        rates = self.rates()
        self.assertEqual(sum(r["down"] for r in rates), 6_000_000)
        self.assertGreater(sum(r["up"] for r in rates), 0)  # the GET line itself
        busy = [r for r in rates if r["down"]]
        self.assertTrue(all(r["top"] == (1, r["down"]) for r in busy), busy)
        self.assertTrue(all(150 <= r["over"] <= 600 for r in rates), rates)
        self.assertEqual({r["mode"] for r in rates}, {"pass"})

    def test_a_throttled_download_is_logged_at_the_link_rate(self):
        self.ctl("throttle", kbps=20000)  # 2.5 MB/s
        s = self.connect(timeout=15)
        self.fetch(s, 6_000_000)
        self.settle()
        rates = [r for r in self.rates() if r["down"]]
        self.assertGreaterEqual(len(rates), 4, rates)
        for r in rates[1:-1]:  # the first and last intervals are partial
            want = 2_500_000 * r["over"] / 1000
            self.assertAlmostEqual(r["down"] / want, 1.0, delta=0.25, msg=str(r))
        self.assertEqual({r["mode"] for r in rates}, {"throttle:20000"})

    def test_a_silent_link_is_logged_once_not_every_interval(self):
        s = self.connect(timeout=15)
        self.fetch(s, 100_000)
        self.settle()
        n = len(self.rates())
        self.assertEqual(self.rates()[-1]["down"], 0)
        time.sleep(0.8)  # three more intervals with nothing moving
        self.assertEqual(len(self.rates()), n)

    def test_off_by_default_and_validated(self):
        echo = Echo()
        self.addCleanup(echo.close)
        lines = []
        quiet = np.NetlossProxy(("127.0.0.1", echo.port), log=lines.append).start()
        self.addCleanup(quiet.stop)
        s = socket.create_connection(quiet.listen_addr, timeout=2)
        self.addCleanup(s.close)
        s.sendall(b"hello")
        self.assertEqual(s.recv(10), b"hello")
        time.sleep(0.4)
        self.assertFalse([l for l in lines if " rate " in l])
        with self.assertRaises(SystemExit):
            np.main(["--upstream", "127.0.0.1:1", "--rate-log", "-1"])


class ServeMeter(unittest.TestCase):
    """`mock_pms.py --serve-rate-log`: media bytes served per interval and where the time went."""

    def test_sample_sums_the_interval_and_reports_silence_once(self):
        m = mock_pms.ServeMeter(1.0, log=lambda line: None)
        m.add(1000, 0.010, 0.020, 1)
        m.add(500, 0.005, 0.020, 2)
        line = m.sample(1000)
        self.assertRegex(line, r"^mock_pms: served \d\d:\d\d:\d\d\.\d{3} bytes=1500 read_ms=15 write_ms=40 "
                               r"over=1000ms streams=2$")
        self.assertIn("bytes=0 ", m.sample(1000))  # the first silent interval after traffic
        self.assertIsNone(m.sample(1000))          # then nothing until bytes flow again
        m.add(1, 0.0, 0.0, 1)
        self.assertIn("bytes=1 ", m.sample(1000))

    def test_a_ranged_body_is_metered_and_off_by_default(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            clip = pathlib.Path(tmp) / "fixture.mkv"
            clip.write_bytes(bytes(range(256)) * 8192)  # 2 MiB
            plain, _ = mock_pms.serve(0, movies=0, transcode_fixture=clip)
            self.addCleanup(lambda s=plain: (s.shutdown(), s.server_close()))
            self.assertIsNone(plain.serve_meter)
            srv, _ = mock_pms.serve(0, movies=0, transcode_fixture=clip, serve_rate_log=0.1)
            self.addCleanup(lambda s=srv: (s.shutdown(), s.server_close(), s.serve_meter.stop()))
            lines = []
            srv.serve_meter._log = lines.append
            url = "http://127.0.0.1:%d/video/:/transcode/universal/start.mkv?path=x" % srv.server_address[1]
            self.assertEqual(urllib.request.urlopen(url, timeout=5).read(), clip.read_bytes())
            deadline = time.time() + 3
            while time.time() < deadline and not any("bytes=0 " in l for l in lines):
                time.sleep(0.05)
            import re
            got = [int(re.search(r"bytes=(\d+)", l)[1]) for l in lines]
            self.assertEqual(sum(got), 2 * 1024 * 1024, lines)
            self.assertTrue(all("streams=1" in l for l in lines if "bytes=0 " not in l), lines)


if __name__ == "__main__":
    unittest.main()
