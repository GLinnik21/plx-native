#!/usr/bin/env python3
"""A fault-injecting TCP proxy that makes a REAL-LOOKING network loss, or a too-slow link, between the TV and `tests/mock_pms.py`.

Why it exists. Silencing or restarting the mock is not a loss of the link: a stopped process closes
its sockets (FIN/RST) or answers instantly, and a SIGSTOPped one still completes TCP handshakes in
the kernel. The set's firewall (`tv-session.sh wan off`) edits the television's network
configuration and a host packet filter needs root. This proxy needs neither: it sits on the host,
listens on the LAN, forwards to the mock, and the mock advertises the PROXY's port
(`mock_pms.py --advertise-port`), so every PMS connection the app opens (the SSE feed and the plain
requests) crosses it. Plex.tv sign-in still goes straight to the mock through the reverse tunnel.

    python3 tests/mock_pms.py --host 0.0.0.0 --port 32499 --advertise-ip <HOST-LAN-IP> \\
        --advertise-port 32500 --home-users 2 --authorize-after 2
    python3 tools/netloss-proxy.py --upstream 127.0.0.1:32499 --listen 0.0.0.0:32500 --control-port 32498
    tools/tv-session.sh up --mock-plextv=32499 --screen login
    curl -s -X POST -d '{"mode":"blackhole"}' http://127.0.0.1:32498/_proxy/mode      # the loss
    curl -s -X POST -d '{"mode":"pass"}'      http://127.0.0.1:32498/_proxy/mode      # restore

A too-slow link instead of a dead one: add `--mode throttle:27000` to the proxy command (a 27 Mbps link
from the start), or flip to it at any time with
    curl -s -X POST -d '{"mode":"throttle","kbps":27000}' http://127.0.0.1:32498/_proxy/mode
The TV reaches the mock through the proxy at `<HOST-LAN-IP>:32500` (the `--listen` and `--advertise-port`
port, not the mock's own 32499), so its PMS traffic, a stream included, is limited to that rate.

MODES (`POST /_proxy/mode {"mode": ...}`, `GET /_proxy/mode` reads it back with `kbps` and the counters):

    pass       forward both directions.
    throttle   forward both directions through a LINK RATE: `{"mode":"throttle","kbps":N}` (N kilobits
               per second, 1 kbps = 1000 bit/s; the schedule and `--mode` spell it `throttle:N`). Each
               direction has its own token bucket of N kbps shared by EVERY connection, so the SSE feed
               and a bulk download compete for the one link as they do on a real one, rather than each
               getting N. A pump reads only as many bytes as its bucket grants (and only once its socket is
               readable, so an idle connection holds no tokens), which keeps TCP back-pressure real: the
               proxy never reads ahead of the link into an unbounded buffer, and the peers' own windows
               fill exactly as they would behind a slow link. Tokens accrue from the monotonic clock and
               are granted in 10 ms quanta; the burst is one CHUNK (64 KiB), or two 10 ms ticks of the
               link where that is more (above ~52 Mbps a CHUNK is under 10 ms). A new rate applies to the
               live connections at once; `pass` lifts the limit from them, and `blackhole`, `reset` and
               `refuse` take over from it as from `pass`. What is measured is the rate the proxy
               forwards, not the latency of a real link.
    blackhole  the closest userland analogue to a dead link: existing connections stay open, but the
               proxy stops reading and forwarding in BOTH directions (so the kernel buffers fill and
               TCP back-pressures the peers, as on a link that drops everything), and a new
               connection is accepted by the kernel but never read, never forwarded and never
               answered. No FIN, no RST. What was in flight is delivered on `pass`, as TCP
               retransmission would deliver it once a link comes back.
    reset      RST every existing connection (SO_LINGER 0) now, and RST each new connection the
               moment it is accepted (the client's connect usually succeeds and its first read or
               write fails with a reset; on loopback the RST can land first and fail the connect
               itself). The listener stays bound, so `pass` resumes with no re-bind.
    refuse     the listener is closed, so a new connect is refused (ECONNREFUSED); existing
               connections are left alone and keep forwarding.

LOGGING. Every mode change and every connection (open, close with the byte counts and the reason) is
logged to stdout with a wall-clock timestamp, flushed per line. The control endpoint binds loopback
only. `--schedule MODE@SECONDS,...` flips modes on a timer from the start of the run (for one-lease
runs), e.g. `throttle:27000@10,blackhole@60,pass@90`; the log shows each flip.
"""
import argparse
import http.server
import json
import math
import select
import socket
import struct
import sys
import threading
import time

MODES = ("pass", "blackhole", "reset", "refuse", "throttle")
CHUNK = 65536
TICK = 0.01  # throttle: the quantum a grant waits for, and the unit of the burst floor
UPSTREAM_TRIES = 6
POLL = 0.25  # how often a blocked pump looks at the mode / its connection's fate


def stamp():
    t = time.time()
    return time.strftime("%H:%M:%S", time.localtime(t)) + f".{int(t * 1000) % 1000:03d}"


def parse_hostport(text, default_host):
    host, sep, port = text.rpartition(":")
    if not sep:
        host, port = default_host, text
    return host or default_host, int(port)


def rst_close(sock):
    """Close with SO_LINGER 0: the peer sees a RST, not a FIN."""
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    except OSError:
        pass
    try:
        sock.close()
    except OSError:
        pass


def parse_kbps(value):
    """A throttle rate in kilobits per second: a finite number above 0 (not a bool, not a string)."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError("throttle needs a numeric kbps")
    value = float(value)
    if not math.isfinite(value) or value <= 0:
        raise ValueError("throttle kbps must be a finite number above 0")
    return int(value) if value == int(value) else value


def parse_mode_spec(text):
    """`MODE` or `throttle:KBPS` (the `--mode` and `--schedule` spelling) to `(mode, kbps or None)`."""
    name, sep, rate = text.partition(":")
    if name not in MODES:
        raise ValueError(f"mode must be one of {', '.join(MODES)}")
    if name != "throttle":
        if sep:
            raise ValueError(f"{name} takes no rate")
        return name, None
    if not sep:
        raise ValueError("throttle needs a rate: throttle:KBPS")
    try:
        return name, parse_kbps(float(rate))
    except ValueError as e:
        raise ValueError(f"throttle rate {rate!r}: {e}") from None


class Bucket:
    """One direction of the throttled link: a token bucket (tokens are bytes) shared by the pumps of
    every connection that goes that way, so the connections split one link rather than each having it.

    Tokens accrue from the monotonic clock whenever the bucket is touched, so a sleep that overshoots
    catches up instead of losing time (no ticker thread, and the 10 ms granularity of `sleep` does not
    bend the rate). The burst is one CHUNK, or two TICKs of the link where that is more: at 100 Mbps a
    CHUNK is 5 ms, which a 10 ms cadence could not otherwise sustain. `take` grants nothing until one
    TICK of tokens (at most a CHUNK) exists, so a recv is never a handful of bytes, and never grants
    more than asked; the caller recvs exactly that many bytes, which is what keeps TCP back-pressure
    real (the proxy never reads ahead of the link). A fresh bucket starts full: a link that has just
    been limited is idle. `clock` and `sleep` are injectable so the arithmetic is unit-tested without
    real time."""

    def __init__(self, clock=time.monotonic, sleep=time.sleep):
        self._clock, self._sleep = clock, sleep
        self._lock = threading.Lock()
        self._rate = None  # bytes per second; None while no throttle is configured
        self._cap = CHUNK
        self._tokens = 0.0
        self._at = 0.0

    def configure(self, rate):
        """(Re)limit the link to `rate` bytes per second; tokens already banked are kept."""
        with self._lock:
            now = self._clock()
            fresh = self._rate is None
            if not fresh:
                self._refill(now)
            self._rate = float(rate)
            self._cap = max(CHUNK, math.ceil(2 * TICK * self._rate))
            self._tokens = self._cap if fresh else min(self._tokens, self._cap)
            self._at = now

    def clear(self):
        with self._lock:
            self._rate = None

    def _refill(self, now):
        self._tokens = min(self._cap, self._tokens + (now - self._at) * self._rate)
        self._at = now

    def take(self, want, abort):
        """Block until a quantum of tokens exists, then take and return `n` of them, 1 <= n <= want.
        Returns 0 as soon as `abort()` is true or the bucket is cleared (the caller re-reads the mode)."""
        while True:
            if abort():
                return 0
            with self._lock:
                if self._rate is None:
                    return 0
                self._refill(self._clock())
                need = min(want, max(1, int(self._rate * TICK)))
                if self._tokens >= need:
                    n = min(want, int(self._tokens))
                    self._tokens -= n
                    return n
                wait = (need - self._tokens) / self._rate
            self._sleep(min(max(wait, 0.001), POLL))

    def give(self, n):
        """Return the unused part of a grant (a short recv, an EOF, a timeout)."""
        if n > 0:
            with self._lock:
                if self._rate is not None:
                    self._tokens = min(self._cap, self._tokens + n)


class Conn:
    def __init__(self, cid, client, peer):
        self.cid, self.client, self.peer = cid, client, peer
        self.upstream = None
        self.up = 0      # client -> upstream bytes forwarded
        self.down = 0    # upstream -> client bytes forwarded
        self.dead = False
        self.reason = None
        self.lock = threading.Lock()
        self.pumps_left = 2


class NetlossProxy:
    def __init__(self, upstream, listen=("127.0.0.1", 0), control=("127.0.0.1", 0), log=None):
        self.upstream = upstream
        self.listen_addr = listen
        self.control_addr = control
        self._log = log or (lambda line: print(line, flush=True))
        self.mode = "pass"
        self.kbps = None  # the throttle's rate while mode == "throttle", else None
        self._buckets = {"up": Bucket(), "down": Bucket()}  # one link per direction, shared by all connections
        self.cond = threading.Condition()
        self.conns = {}
        self.next_id = 1
        self.stats = {"accepted": 0, "reset": 0, "closed": 0}
        self._listener = None
        self._ctl = None
        self._stop = threading.Event()
        self._threads = []

    # ---- logging ----
    def log(self, msg):
        self._log(f"netloss {stamp()} {msg}")

    # ---- lifecycle ----
    def start(self):
        self._open_listener()
        self._ctl = http.server.ThreadingHTTPServer(self.control_addr, self._handler())
        self._ctl.daemon_threads = True
        self.control_addr = self._ctl.server_address[:2]
        threading.Thread(target=self._ctl.serve_forever, name="netloss-ctl", daemon=True).start()
        self.log(f"listening on {self.listen_addr[0]}:{self.listen_addr[1]} -> "
                 f"{self.upstream[0]}:{self.upstream[1]}; control http://{self.control_addr[0]}:"
                 f"{self.control_addr[1]}/_proxy/mode; mode={self.mode}")
        return self

    def _open_listener(self):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(self.listen_addr)
        s.listen(128)
        self.listen_addr = s.getsockname()[:2]
        self._listener = s
        t = threading.Thread(target=self._accept_loop, args=(s,), name="netloss-accept", daemon=True)
        t.start()

    def _close_listener(self):
        s, self._listener = self._listener, None
        if s is not None:
            try:
                s.close()  # the accept loop's 0.25 s poll notices and ends
            except OSError:
                pass

    def stop(self):
        self._stop.set()
        with self.cond:
            self.cond.notify_all()
        self._close_listener()
        if self._ctl is not None:
            self._ctl.shutdown()
            self._ctl.server_close()
        for c in list(self.conns.values()):
            self._kill(c, "proxy stopped", rst=False)

    # ---- mode ----
    def _label(self):
        return f"throttle:{self.kbps}" if self.mode == "throttle" else self.mode

    def set_mode(self, mode, kbps=None):
        """Switch the mode. `kbps` is the throttle's rate and is required by it, ignored by the rest."""
        if mode not in MODES:
            raise ValueError(f"mode must be one of {', '.join(MODES)}")
        kbps = parse_kbps(kbps) if mode == "throttle" else None
        with self.cond:
            old = self._label()
            self.mode, self.kbps = mode, kbps
            for bucket in self._buckets.values():
                if kbps is None:
                    bucket.clear()
                else:
                    bucket.configure(kbps * 125.0)  # 1 kbps = 1000 bit/s = 125 B/s
            self.cond.notify_all()
            new = self._label()
        self.log(f"mode {old} -> {new} (connections={len(self.conns)})")
        if mode == "refuse":
            self._close_listener()
        elif self._listener is None:
            self._open_listener()
        if mode == "reset":
            for c in list(self.conns.values()):
                self._kill(c, "reset by mode", rst=True)
        return old

    def snapshot(self):
        with self.cond:
            mode, kbps = self.mode, self.kbps
        return {"mode": mode, "kbps": kbps, "connections": len(self.conns), **self.stats,
                "listen": f"{self.listen_addr[0]}:{self.listen_addr[1]}"}

    def _gate(self, conn):
        """Block while the link is dead. False when the connection or the proxy is finished."""
        with self.cond:
            while self.mode == "blackhole" and not conn.dead and not self._stop.is_set():
                self.cond.wait(POLL)
        return not conn.dead and not self._stop.is_set()

    def _left_throttle(self, conn):
        """A token wait is over: the connection or the proxy ended, or the mode is no longer throttle."""
        return conn.dead or self._stop.is_set() or self.mode != "throttle"

    # ---- accepting ----
    def _accept_loop(self, lsock):
        lsock.settimeout(POLL)
        while not self._stop.is_set() and self._listener is lsock:
            try:
                client, peer = lsock.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with self.cond:
                cid, self.next_id = self.next_id, self.next_id + 1
                mode = self.mode
            self.stats["accepted"] += 1
            conn = Conn(cid, client, f"{peer[0]}:{peer[1]}")
            self.log(f"conn #{cid} open from {conn.peer} (mode={mode})")
            if mode == "reset":
                conn.dead, conn.reason = True, "reset on accept"
                self.stats["reset"] += 1
                rst_close(client)
                self.log(f"conn #{cid} closed: reset on accept")
                continue
            self.conns[cid] = conn
            threading.Thread(target=self._serve, args=(conn,), name=f"netloss-c{cid}", daemon=True).start()

    def _serve(self, conn):
        # A connection accepted during a blackhole waits here, unread and unanswered, until the link
        # comes back (or its client gives up and the pump finds the EOF).
        if not self._gate(conn):
            return self._finish(conn)
        up, err = None, None
        for attempt in range(UPSTREAM_TRIES):
            # A restore releases every held connection at once, and the mock's small accept queue
            # can answer the burst with a reset: that is the mock's, not the link's, so retry.
            try:
                up = socket.create_connection(self.upstream, timeout=10)
                break
            except OSError as e:
                err = e
                if conn.dead or self._stop.wait(0.15 * (attempt + 1)):
                    break
        if up is None:
            return self._kill(conn, f"upstream connect failed: {err}", rst=True)
        up.settimeout(POLL)
        conn.client.settimeout(POLL)
        conn.upstream = up
        threading.Thread(target=self._pump, args=(conn, conn.client, up, "up"), daemon=True).start()
        self._pump(conn, up, conn.client, "down")

    def _pump(self, conn, src, dst, way):
        bucket = self._buckets[way]
        try:
            while True:
                if not self._gate(conn):
                    break
                grant = 0
                if self.mode == "throttle":
                    # Wait for data BEFORE taking tokens: a grant held across an idle recv would drain
                    # the link for every other connection (the feed sits silent for 25 s at a time).
                    try:
                        if not select.select([src], [], [], POLL)[0]:
                            continue
                    except ValueError:  # the socket was closed under us: select refuses fd -1
                        raise OSError("connection ended") from None
                    grant = bucket.take(CHUNK, lambda: self._left_throttle(conn))
                    if not grant:  # the mode changed or the connection ended while waiting: look again
                        continue
                try:
                    data = src.recv(grant or CHUNK)
                except socket.timeout:
                    bucket.give(grant)
                    continue
                except OSError:
                    bucket.give(grant)
                    raise
                bucket.give(grant - len(data))  # the unused part of the grant; a no-op when not throttled
                if not data:
                    try:
                        dst.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    break
                if not self._gate(conn):  # the link died while this chunk was in hand: hold it
                    break
                view = memoryview(data)
                while view:
                    try:
                        view = view[dst.send(view):]
                    except socket.timeout:  # a full send buffer is back-pressure, not an error
                        if conn.dead or self._stop.is_set():
                            raise OSError("connection ended")
                with conn.lock:
                    if way == "up":
                        conn.up += len(data)
                    else:
                        conn.down += len(data)
        except OSError:
            if not conn.dead:
                conn.reason = conn.reason or "socket error"
        with conn.lock:
            conn.pumps_left -= 1
            last = conn.pumps_left == 0
        if last or conn.dead:
            self._finish(conn)

    # ---- ending ----
    def _kill(self, conn, reason, rst):
        with conn.lock:
            if conn.dead:
                return
            conn.dead, conn.reason = True, reason
        if rst:
            self.stats["reset"] += 1
            rst_close(conn.client)
            if conn.upstream is not None:
                rst_close(conn.upstream)
        self._finish(conn)

    def _finish(self, conn):
        with conn.lock:
            first = self.conns.pop(conn.cid, None) is not None
        if not first:
            return
        for s in (conn.client, conn.upstream):
            if s is not None:
                try:
                    s.close()
                except OSError:
                    pass
        self.stats["closed"] += 1
        self.log(f"conn #{conn.cid} closed: {conn.reason or 'ended'} (up={conn.up}B down={conn.down}B)")

    # ---- control endpoint ----
    def _handler(self):
        proxy = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def _send(self, code, obj):
                body = json.dumps(obj).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                if self.path.split("?")[0] in ("/_proxy/mode", "/_proxy/stats"):
                    return self._send(200, proxy.snapshot())
                self._send(404, {"error": "not found"})

            def do_POST(self):
                if self.path.split("?")[0] != "/_proxy/mode":
                    return self._send(404, {"error": "not found"})
                try:
                    n = int(self.headers.get("Content-Length") or 0)
                    body = json.loads(self.rfile.read(n) or b"{}")
                    proxy.set_mode(body.get("mode"), body.get("kbps"))
                except (ValueError, AttributeError, TypeError) as e:
                    return self._send(400, {"error": str(e)})
                self._send(200, proxy.snapshot())

        return Handler


def parse_schedule(text):
    """`MODE@SECONDS,...` (MODE as `parse_mode_spec` reads it) to `[(seconds, spec)]`, earliest first."""
    out = []
    for item in filter(None, (p.strip() for p in text.split(","))):
        spec, sep, secs = item.partition("@")
        try:
            if not sep:
                raise ValueError("no @SECONDS")
            parse_mode_spec(spec)
            out.append((float(secs), spec))
        except ValueError as e:
            raise ValueError(f"--schedule item {item!r}: want MODE@SECONDS, MODE one of "
                             f"{', '.join(MODES)} (throttle as throttle:KBPS): {e}") from None
    return sorted(out)


def run_schedule(proxy, schedule):
    t0 = time.monotonic()
    for at, spec in schedule:
        delay = at - (time.monotonic() - t0)
        if delay > 0 and proxy._stop.wait(delay):
            return
        proxy.set_mode(*parse_mode_spec(spec))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--upstream", required=True, metavar="HOST:PORT", help="the mock to forward to")
    ap.add_argument("--listen", default="0.0.0.0:0", metavar="HOST:PORT",
                    help="where the TV connects (default 0.0.0.0, a kernel-picked port); the mock's "
                         "--advertise-port must name this port")
    ap.add_argument("--control-port", type=int, default=0,
                    help="loopback port of POST /_proxy/mode (default: kernel-picked, printed)")
    ap.add_argument("--mode", default="pass", metavar="MODE|throttle:KBPS",
                    help=f"initial mode, one of {', '.join(MODES)}; throttle takes its rate in kilobits "
                         "per second, shared by every connection, e.g. throttle:27000")
    ap.add_argument("--schedule", default="", metavar="MODE@SECONDS,...",
                    help="flip modes on a timer from start, e.g. blackhole@10,pass@70 or "
                         "throttle:27000@10,pass@70")
    a = ap.parse_args(argv)
    try:
        initial = parse_mode_spec(a.mode)
        schedule = parse_schedule(a.schedule)
        proxy = NetlossProxy(parse_hostport(a.upstream, "127.0.0.1"),
                             parse_hostport(a.listen, "0.0.0.0"), ("127.0.0.1", a.control_port))
    except ValueError as e:
        ap.error(str(e))
    proxy.start()
    if initial[0] != "pass":
        proxy.set_mode(*initial)
    if schedule:
        threading.Thread(target=run_schedule, args=(proxy, schedule), daemon=True).start()
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        proxy.stop()


if __name__ == "__main__":
    sys.exit(main())
