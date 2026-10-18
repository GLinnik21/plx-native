#!/usr/bin/env python3
"""A fault-injecting TCP proxy that makes a REAL-LOOKING network loss between the TV and `tests/mock_pms.py`.

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

MODES (`POST /_proxy/mode {"mode": ...}`, `GET /_proxy/mode` reads it back with the counters):

    pass       forward both directions.
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
runs); the log shows each flip.
"""
import argparse
import http.server
import json
import socket
import struct
import sys
import threading
import time

MODES = ("pass", "blackhole", "reset", "refuse")
CHUNK = 65536
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
    def set_mode(self, mode):
        if mode not in MODES:
            raise ValueError(f"mode must be one of {', '.join(MODES)}")
        with self.cond:
            old, self.mode = self.mode, mode
            self.cond.notify_all()
        self.log(f"mode {old} -> {mode} (connections={len(self.conns)})")
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
            mode = self.mode
        return {"mode": mode, "connections": len(self.conns), **self.stats,
                "listen": f"{self.listen_addr[0]}:{self.listen_addr[1]}"}

    def _gate(self, conn):
        """Block while the link is dead. False when the connection or the proxy is finished."""
        with self.cond:
            while self.mode == "blackhole" and not conn.dead and not self._stop.is_set():
                self.cond.wait(POLL)
        return not conn.dead and not self._stop.is_set()

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
        try:
            while True:
                if not self._gate(conn):
                    break
                try:
                    data = src.recv(CHUNK)
                except socket.timeout:
                    continue
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
                    mode = json.loads(self.rfile.read(n) or b"{}").get("mode")
                    proxy.set_mode(mode)
                except (ValueError, AttributeError, TypeError) as e:
                    return self._send(400, {"error": str(e)})
                self._send(200, proxy.snapshot())

        return Handler


def parse_schedule(text):
    out = []
    for item in filter(None, (p.strip() for p in text.split(","))):
        mode, sep, secs = item.partition("@")
        if not sep or mode not in MODES:
            raise ValueError(f"--schedule item {item!r}: want MODE@SECONDS with MODE one of {', '.join(MODES)}")
        out.append((float(secs), mode))
    return sorted(out)


def run_schedule(proxy, schedule):
    t0 = time.monotonic()
    for at, mode in schedule:
        delay = at - (time.monotonic() - t0)
        if delay > 0 and proxy._stop.wait(delay):
            return
        proxy.set_mode(mode)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--upstream", required=True, metavar="HOST:PORT", help="the mock to forward to")
    ap.add_argument("--listen", default="0.0.0.0:0", metavar="HOST:PORT",
                    help="where the TV connects (default 0.0.0.0, a kernel-picked port); the mock's "
                         "--advertise-port must name this port")
    ap.add_argument("--control-port", type=int, default=0,
                    help="loopback port of POST /_proxy/mode (default: kernel-picked, printed)")
    ap.add_argument("--mode", choices=MODES, default="pass", help="initial mode")
    ap.add_argument("--schedule", default="", metavar="MODE@SECONDS,...",
                    help="flip modes on a timer from start, e.g. blackhole@10,pass@70")
    a = ap.parse_args(argv)
    try:
        schedule = parse_schedule(a.schedule)
        proxy = NetlossProxy(parse_hostport(a.upstream, "127.0.0.1"),
                             parse_hostport(a.listen, "0.0.0.0"), ("127.0.0.1", a.control_port))
    except ValueError as e:
        ap.error(str(e))
    proxy.start()
    if a.mode != "pass":
        proxy.set_mode(a.mode)
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
