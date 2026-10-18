"""`tools/netloss-proxy.py`: the fault-injecting TCP proxy that makes a real-looking network loss.
Host only, real loopback sockets on port 0, a few seconds. The semantics that matter are the ones a
TV client sees: blackhole = silence with the connection still open (no FIN, no RST), reset = a RST,
refuse = ECONNREFUSED, and a restore that delivers what was held."""
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

    def ctl(self, mode=None):
        url = "http://%s:%d/_proxy/mode" % self.proxy.control_addr
        if mode is None:
            return json.load(urllib.request.urlopen(url, timeout=2))
        req = urllib.request.Request(url, json.dumps({"mode": mode}).encode(), method="POST")
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


class MockAdvertisesTheProxy(unittest.TestCase):
    def test_advertise_port_names_the_proxy_in_the_resources_row(self):
        srv, pms = mock_pms.serve(0, seed=7, advertise_ip="127.0.0.1", advertise_port=45678)
        self.addCleanup(lambda s=srv: (s.shutdown(), s.server_close()))
        self.assertEqual(pms.account_resource["connections"][0]["port"], 45678)
        self.assertEqual(pms.account_resource["connections"][0]["uri"], "http://127.0.0.1:45678")
        srv2, pms2 = mock_pms.serve(0, seed=7, advertise_ip="127.0.0.1")
        self.addCleanup(lambda s=srv2: (s.shutdown(), s.server_close()))
        self.assertEqual(pms2.account_resource["connections"][0]["port"], srv2.server_address[1])


if __name__ == "__main__":
    unittest.main()
