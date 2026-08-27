#!/usr/bin/env python3
"""netcond — a network-conditioning TCP proxy, for the failures you cannot otherwise reach.

Most of what is left to harden in the async model is FAULT-CONDITIONAL: the main thread parks in
`teardown` only when a PMS round trip stalls, and `stream.rs`'s deadlines (2 s connect, 15 s
`SO_RCVTIMEO`, 10 s `SO_SNDTIMEO`) only bite against a server that stops answering. On a healthy
LAN none of it happens — the measured teardown-join baseline is `demux 0ms media 0ms timeline 0ms`
(see `task::join`) — so every one of those findings was reasoned about rather than measured, which
is exactly the habit that has been wrong three times on this target already.

This sits between the TV and the PMS and makes the server misbehave on demand.

    ┌────────┐        ┌──────────────┐        ┌─────────┐
    │  TV    │──────▶ │ netcond.py   │──────▶ │  PMS    │
    │  app   │  :32401│ (this Mac)   │  :32400│         │
    └────────┘        └──────────────┘        └─────────┘

The PMS runs on this same Mac, so the proxy only needs a second port; point the app at it by
editing `PMS_PORT` in the gitignored `src/config.local.h`, then `make deploy` (the host/port are
compiled into `main.c`'s `plex_run` call — there is no runtime override).

## Modes

Live-switchable: the mode is re-read from the control file on every accept AND by the relay loops,
so it changes the behaviour of connections ALREADY OPEN. That is the whole point — the interesting
bug is a POST that was in flight when the server went quiet.

    pass                normal forwarding
    stall               relay nothing, in either direction, but hold the sockets OPEN. This is the
                        "accepted but silent" server: the app's recv blocks to SO_RCVTIMEO. THE
                        important one — it is what turns a join into a 15 s parked frame loop.
    blackhole           accept, never connect upstream, never answer. Same shape as `stall` for a
                        NEW connection; distinct because it never touches the real PMS.
    reject              close immediately (RST) — the fast-failure path
    delay:<ms>          forward, but add <ms> of latency to every chunk in both directions
    rate:<kbps>         forward, but SHAPE the link to <kbps> kilobits per second (see below)

Any mode may be scoped to matching requests only:

    stall@/:/timeline   stall ONLY connections whose first bytes contain "/:/timeline";
                        everything else passes normally.

That scoping is what makes a clean experiment possible: freeze the progress reporter while the
video stream keeps flowing, so the freeze you then measure at teardown is unambiguously the
reporter's POST and not a starved demuxer.

**Scoping was broken until 2026-08-23 and only the ACCEPT half worked.** `serve_conn` consulted
`applies()` (scope-aware) to decide reject/blackhole and to log, and then handed `relay` a bare
`Mode.split`, which throws the scope away — so `stall@/:/timeline` really stalled the relay of
every open connection, media stream included, and the experiment the scope exists to make clean
was not clean. `relay` now takes the connection's own first bytes and asks `applies()` per chunk,
which is what this docstring has claimed all along.

## `rate:<kbps>` — the throttled link

The one mode that is not a fault but a SPEED, and it exists for LG App Self Checklist #43 CASE1,
whose legs are 512 Kbps → 1 Mbps → 7 Mbps → 17.5 Mbps ("buffering should not occur constantly"),
and for #14's "abnormal end" under a degrading link. Without it those items are anecdote: nothing
else in this repo can make the server slow rather than dead.

    echo rate:512   > /tmp/netcond.mode      # 512 Kbps  — CASE1 leg 1
    echo rate:1000  > /tmp/netcond.mode      # 1 Mbps    — leg 2
    echo rate:7000  > /tmp/netcond.mode      # 7 Mbps    — leg 3
    echo rate:17500 > /tmp/netcond.mode      # 17.5 Mbps — leg 4
    echo pass       > /tmp/netcond.mode      # off

Four properties, each deliberate:

  * **kbps is KILOBITS, decimal** — 1 kbps = 1000 bits/s — because that is the unit the checklist
    item states its legs in. `rate:512` is 64,000 bytes/s, not 65,536.
  * **One bucket for the whole proxy, not one per connection.** `rate:` names a LINK, and a link
    is shared: the media GET and a `/:/timeline` POST racing each other is exactly the contention
    a real slow network produces, and per-connection buckets would hand each of them the full
    rate. Scope it (`rate:512@/library/parts`) when you want only the media stream throttled and
    the control calls left at full speed.
  * **It applies to connections ALREADY OPEN**, like every other mode here — the bucket's rate is
    re-read per chunk — so a transfer can be throttled, released and re-throttled MID-FLIGHT
    without touching the app. That is what makes one scripted run cover all four CASE1 legs
    instead of four launches.
  * **The bucket starts EMPTY.** A full one would hand the first quarter-second of every transfer
    a free burst at infinite speed, which is the shape that makes a throttle look like it worked
    when it did not.

## Use

    tools/netcond.py --listen 32401 --target 127.0.0.1:32400 \
        --allow-client <TV_IP>                                     # starts in `pass`
    echo 'stall@/:/timeline' > /tmp/netcond.mode                    # freeze the reporter
    echo 'rate:512'          > /tmp/netcond.mode                    # throttle the link
    echo pass                > /tmp/netcond.mode                    # let it go again

It logs one line per connection with the matched request line, so you can see what the app is
actually asking for, and a summary on exit. `--mode` sets the starting mode; `--control` moves the
control file. `--selftest` proves the shaper against a loopback transfer and exits, which is also
how to answer the macOS firewall question below without a television in the room.

**macOS trap, and it costs an afternoon every time: the application firewall silently drops
inbound connections to an ad-hoc python listener** — no refusal, no log line, the TV's open just
reads empty (verified 2026-08-11: netcond up, mode armed, zero requests logged). The GUI "allow
incoming connections?" prompt must be accepted ONCE PER PYTHON BINARY, so start netcond with a
human at the keyboard before going headless, and read "netcond logs nothing" as this rather than
as a quiet television. `tests/serve_fixtures.py` carries the same warning for the same reason.

Nothing here is deployed to the TV and nothing is secret: it forwards bytes it does not inspect
beyond the first line, and it never logs the query string — a PMS URL carries `X-Plex-Token`.
"""
import argparse
import ipaddress
import os
import re
import select
import shutil
import socket
import sys
import tempfile
import threading
import time

CHUNK = 65536
_stats = {
    "opened": 0,
    "denied": 0,
    "stalled": 0,
    "rejected": 0,
    "blackholed": 0,
    "rated": 0,
    "passed": 0,
}
_lock = threading.Lock()


def _stdout(msg):
    print(msg, flush=True)


#: Where the connection log goes. One function rather than bare `print`s so an importer can take it
#: somewhere else — `make check` drives this proxy over loopback, and a per-connection narration
#: interleaved into a test runner's dots is noise nobody reads. Rebind `netcond.SINK`, not `say`:
#: the indirection is what lets it be swapped after the relay threads are already running.
SINK = _stdout


def say(msg):
    SINK(msg)


def _redact(s: str) -> str:
    """A PMS request line carries X-Plex-Token in the query string. Never print it."""
    return re.sub(r"([?&](?:X-Plex-Token|token)=)[^&\s]*", r"\1<redacted>", s)


class Mode:
    """The control file, re-read on demand. `action` plus an optional path scope."""

    def __init__(self, path, initial):
        self.path = path
        self.raw = initial
        self._mtime = 0

    def read(self) -> str:
        try:
            st = os.stat(self.path)
            if st.st_mtime != self._mtime:
                self._mtime = st.st_mtime
                with open(self.path) as f:
                    new = f.read().strip()
                if new and new != self.raw:
                    say(f"[netcond] mode -> {new}")
                    self.raw = new
        except FileNotFoundError:
            pass
        return self.raw

    @staticmethod
    def split(raw):
        """'stall@/:/timeline' -> ('stall', '/:/timeline'); 'delay:250' -> ('delay:250', None)"""
        act, _, scope = raw.partition("@")
        return act.strip(), (scope.strip() or None)


def applies(raw, first_bytes):
    """Is `raw`'s action in force for a connection whose request starts with `first_bytes`?"""
    act, scope = Mode.split(raw)
    if scope and scope.encode() not in first_bytes:
        return "pass"
    return act


_warned = set()


def arg_of(act, prefix, default=None):
    """The number after `<prefix>:` in an action, or `default` when it is not that action.

    Tolerant on purpose. The control file is edited by hand mid-experiment — that IS the workflow —
    so `delya:200` or `rate:` is a likely keystroke, and `int()` raising inside a relay thread kills
    a live connection with a traceback that reads like a proxy bug. A bad value warns ONCE per
    distinct string (the relay re-reads the mode many times a second; an unthrottled warning would
    bury the connection log it sits in) and behaves as `pass`.
    """
    if not act.startswith(prefix + ":"):
        return default
    raw = act.split(":", 1)[1].strip()
    try:
        return float(raw)
    except ValueError:
        if act not in _warned:
            _warned.add(act)
            say(f"[netcond] ignoring malformed mode {act!r} (want {prefix}:<number>)")
        return default


class RateBucket:
    """ONE token bucket for the whole proxy — `rate:` names a link speed, and a link is shared.

    Not a rate stored at construction: the rate arrives on every `take`, out of the control file,
    because the whole idiom here is that a mode changes under connections that are already open.
    A rate change re-sizes the bucket's capacity from the next call onward and never refunds
    tokens already spent, which is what a real link does when its speed drops.

    Starts EMPTY. A full bucket would let the first `BURST_S` of every transfer through at
    unbounded speed, and at the short transfers this is used on that is most of the transfer —
    a throttle that measures as unthrottled and reads as "the proxy is not in the path".
    """

    #: Capacity, in seconds of the CURRENT rate. Large enough that TCP keeps moving (a bucket
    #: smaller than an MSS makes every send a dribble and the measured rate collapses below the
    #: requested one), small enough to actually shape rather than to pass bursts through.
    BURST_S = 0.25
    #: Never below roughly one Ethernet MTU, or `rate:64` grants fractions of a packet.
    MIN_CAP = 1500.0
    #: The longest a starved caller blocks before returning 0. It is what keeps the mode live:
    #: the relay loops back to `mode.read()`, so `echo pass > …` releases a parked transfer within
    #: one slice instead of at the end of the file.
    SLICE_S = 0.05

    def __init__(self):
        self.lock = threading.Lock()
        self.tokens = 0.0
        self.stamp = time.monotonic()

    def reset(self):
        """Drain the link back to empty. For a TEST that measures a second leg — carrying a
        quarter-second of credit out of the previous transfer into the next one is exactly the
        free burst `BURST_S`'s note is about, and it lands entirely inside a short measurement."""
        with self.lock:
            self.tokens = 0.0
            self.stamp = time.monotonic()

    def take(self, kbps, want):
        """Bytes the caller may move right now, 0 when the bucket is empty (having slept a slice).

        `kbps` is decimal KILOBITS per second — the unit LG's checklist states its legs in — so the
        conversion is `* 1000 / 8`, not `* 1024 / 8`.
        """
        rate = max(float(kbps), 1.0) * 1000.0 / 8.0
        cap = max(rate * self.BURST_S, self.MIN_CAP)
        with self.lock:
            now = time.monotonic()
            self.tokens = min(cap, self.tokens + (now - self.stamp) * rate)
            self.stamp = now
            if self.tokens >= 1.0:
                n = int(min(want, self.tokens))
                self.tokens -= n
                return n
            need = (1.0 - self.tokens) / rate
        time.sleep(min(need, self.SLICE_S))
        return 0

    def refund(self, n):
        """Hand back tokens taken for bytes that never arrived.

        `recv(budget)` may return fewer bytes than budgeted (or none, on its timeout). Without
        this the shaper charges for bytes it did not move, so a chatty-but-idle connection would
        drag the measured rate below the requested one for no reason anyone could see.
        """
        if n <= 0:
            return
        with self.lock:
            self.tokens += n


BUCKET = RateBucket()


def relay(src, dst, mode, head, stop):
    """Pump one direction, consulting the mode each chunk so a live connection can be frozen.

    `head` is this connection's own first bytes, so a SCOPED mode is honoured per connection rather
    than applied to every open relay — see the scoping note in the module docstring for what that
    was doing before.

    The readable poll is `select`, NOT `src.settimeout(0.5)`, and that is load-bearing rather than
    stylistic. Both directions run over the SAME PAIR of sockets, so one thread's `src` is the
    other's `dst` — a timeout set for this loop's poll interval is also the timeout on the other
    thread's `sendall`. Python's `sendall` on a timed-out socket raises `socket.timeout` having
    written an unspecified number of bytes, the `except OSError` below swallows it, and the relay
    exits mid-body: a SILENTLY TRUNCATED stream. It is reachable in exactly the situation this
    proxy exists to measure — the app's `aq` backpressure parks the demux, the TV stops reading,
    the window closes, and a 64 KB write blocks past 0.5 s. Selecting leaves both sockets blocking,
    so a write waits as long as the peer needs and the teardown's `shutdown` is what unblocks it.
    """
    try:
        while not stop.is_set():
            act = applies(mode.read(), head)
            if act == "stall":
                time.sleep(0.2)  # hold the socket open, forward nothing
                continue
            ms = arg_of(act, "delay")
            if ms is not None:
                time.sleep(max(ms, 0.0) / 1000.0)  # `delay:-1` is a typo, not a time machine
            kbps = arg_of(act, "rate")
            budget = CHUNK
            if kbps is not None:
                budget = BUCKET.take(kbps, CHUNK)
                if budget <= 0:
                    continue  # bucket empty — loop back and re-read the mode
            b, dead, ready = b"", False, ()
            try:
                ready, _, _ = select.select([src], [], [], 0.5)
                if ready:
                    b = src.recv(budget)
            except (OSError, ValueError):
                # ValueError as well as OSError: `serve_conn` closes both sockets after joining
                # this thread with a 2 s timeout, so a relay still parked in `select` when that
                # expires meets a fd of -1, which `select` reports as ValueError rather than
                # OSError. Same event, same handling — a traceback out of a daemon thread at
                # teardown is noise in the one log this tool exists to produce.
                dead = True
            finally:
                # ONE refund for every way of not moving the budgeted bytes — the poll expiring,
                # a short read, EOF, a dead socket. Written as a `finally` because the three
                # explicit copies it replaced had already lost one: a connection that died
                # mid-chunk left its whole budget charged to the shared bucket, i.e. a transient
                # under-rate on any link that churns connections.
                if kbps is not None:
                    BUCKET.refund(budget - len(b))
            if dead:
                break
            if not b:
                if ready:
                    break  # readable and empty is EOF; not readable is just an idle poll
                continue
            dst.sendall(b)
    except (OSError, ValueError):
        pass
    finally:
        stop.set()
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def serve_conn(cli, target, mode):
    up = None
    try:
        # Peek the request line so a scoped mode can decide. The app sends the full header in one
        # write, so a single recv is enough in practice; a short read just means no scope match.
        cli.settimeout(5.0)
        try:
            head = cli.recv(CHUNK)
        except (socket.timeout, OSError):
            return
        if not head:
            return
        act = applies(mode.read(), head)
        line = _redact(head.split(b"\r\n", 1)[0].decode("latin1", "replace"))[:120]

        if act == "reject":
            with _lock:
                _stats["rejected"] += 1
            say(f"[netcond] REJECT  {line}")
            return
        if act == "blackhole":
            with _lock:
                _stats["blackholed"] += 1
            say(f"[netcond] BLACKHOLE {line}  (holding open, never answering)")
            while mode.read() and applies(mode.read(), head) == "blackhole":
                time.sleep(0.25)
            return

        if act == "stall":
            bucket = "stalled"
        elif arg_of(act, "rate") is not None:
            bucket = "rated"
        else:
            bucket = "passed"
        with _lock:
            _stats[bucket] += 1
        say(f"[netcond] {act.upper():9s} {line}")

        up = socket.create_connection(target, timeout=8)
        up.sendall(head)  # the bytes already taken off the client
        # Both sockets back to BLOCKING before the relays start. `cli` still carries the 5 s
        # header-peek timeout and `up` the 8 s connect one, and a socket timeout applies to
        # `sendall` as well as to `recv` — which, once the relays are running, means a write to
        # a slow peer raising `socket.timeout` after an unspecified number of bytes have already
        # gone out. See `relay`'s note: the poll interval lives in `select`, never on a socket.
        cli.settimeout(None)
        up.settimeout(None)
        stop = threading.Event()
        # Both directions get this connection's own `head`, so a scoped mode decides per
        # connection. The upstream leg is a thread; the downstream one runs here.
        t = threading.Thread(target=relay, args=(up, cli, mode, head, stop), daemon=True)
        t.start()
        relay(cli, up, mode, head, stop)
        t.join(timeout=2)
    except OSError as e:
        say(f"[netcond] conn error: {e}")
    finally:
        for s in (cli, up):
            if s:
                try:
                    s.close()
                except OSError:
                    pass


def start_proxy(listen, target, mode, bind="0.0.0.0", allow_clients=None):
    """Bind, listen, and accept on a daemon thread. Returns (listener, port).

    Factored out of `main` so `--selftest` and `tests/test_harness.py` drive the REAL accept path
    rather than a second, simpler copy of it. Shaping only means anything measured through the same
    `serve_conn`/`relay` the television goes through — a test against a private mini-proxy would
    grade a function nothing in the field calls.

    A listener reachable beyond loopback MUST name the clients allowed to use it. PMS requests
    carry an authentication token in the URL, so an open forwarding proxy on the LAN is not a
    harmless test fixture: another client could ask it for private server responses. The check is
    made before the request line is read and before an upstream connection is opened.
    """
    allowed = frozenset(str(ipaddress.ip_address(client)) for client in (allow_clients or ()))
    if not ipaddress.ip_address(bind).is_loopback and not allowed:
        raise ValueError("a non-loopback netcond listener requires at least one allowed client")

    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((bind, listen))
    srv.listen(64)

    def _accept():
        try:
            while True:
                cli, peer = srv.accept()
                if allowed and peer[0] not in allowed:
                    with _lock:
                        _stats["denied"] += 1
                    say("[netcond] DENY client not on allowlist")
                    cli.close()
                    continue
                with _lock:
                    _stats["opened"] += 1
                threading.Thread(target=serve_conn, args=(cli, target, mode), daemon=True).start()
        except OSError:
            pass  # the listener was closed — the normal way this thread ends

    threading.Thread(target=_accept, daemon=True).start()
    return srv, srv.getsockname()[1]


def start_origin(nbytes, bind="127.0.0.1"):
    """A stand-in PMS: answers every connection with `nbytes` of body, then closes. (listener, port)

    Deliberately not HTTP. Nothing in this proxy parses a response — it forwards bytes — so the
    only property a shaping test needs from the far end is "produces data as fast as it is asked
    to", and a real server would put its own scheduling between the shaper and the measurement.
    """
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((bind, 0))
    srv.listen(8)
    body = bytes(range(256)) * (nbytes // 256 + 1)
    body = body[:nbytes]

    def _accept():
        try:
            while True:
                c, _ = srv.accept()
                threading.Thread(target=_answer, args=(c,), daemon=True).start()
        except OSError:
            pass

    def _answer(c):
        try:
            c.settimeout(5.0)
            try:
                c.recv(CHUNK)  # the request, which nothing here reads
            except (socket.timeout, OSError):
                pass
            c.sendall(body)
        except OSError:
            pass
        finally:
            try:
                c.close()
            except OSError:
                pass

    threading.Thread(target=_accept, daemon=True).start()
    return srv, srv.getsockname()[1]


def pull(port, nbytes, host="127.0.0.1", timeout=60):
    """Fetch `nbytes` through the proxy at `port`. Returns (elapsed_s, bytes_received)."""
    c = socket.create_connection((host, port), timeout=timeout)
    try:
        c.settimeout(timeout)
        c.sendall(b"GET /library/parts/1/file.mkv HTTP/1.1\r\nHost: netcond\r\n\r\n")
        t0 = time.monotonic()
        got = 0
        while got < nbytes:
            b = c.recv(CHUNK)
            if not b:
                break
            got += len(b)
        return time.monotonic() - t0, got
    finally:
        try:
            c.close()
        except OSError:
            pass


def _selftest(kbps, kbytes):
    """Prove the shaper against a loopback transfer, and that the mode is live.

    Three legs, and the third is the one that matters for #43 CASE1: the same OPEN connection is
    throttled, then released, without the client reconnecting — because that is how one scripted
    run covers four bitrate legs instead of four app launches.

    It writes its OWN control file in a temp directory and ignores `--control` deliberately. The
    default control path is `/tmp/netcond.mode`, which is the file a live session is conditioning
    a television through: a selftest run beside one would rewrite it three times and leave it at
    `pass`, silently releasing the mode under a measurement in progress. There is nothing to gain
    from sharing it — the selftest builds its own origin and its own listener.
    """
    nbytes = kbytes * 1024
    origin, oport = start_origin(nbytes)
    tmp = tempfile.mkdtemp(prefix="netcond-selftest-")
    control = os.path.join(tmp, "netcond.mode")
    mode = Mode(control, "pass")
    with open(control, "w") as f:
        f.write("pass")
    _srv, pport = start_proxy(0, ("127.0.0.1", oport), mode, bind="127.0.0.1")
    ok = True
    try:
        free_s, free_n = pull(pport, nbytes)
        print(f"  pass       {free_n} bytes in {free_s:.3f}s "
              f"({free_n * 8 / max(free_s, 1e-6) / 1000:.0f} kbps)")
        ok &= free_n == nbytes

        BUCKET.reset()  # a fresh link for the throttled leg
        with open(control, "w") as f:
            f.write(f"rate:{kbps:g}")
        slow_s, slow_n = pull(pport, nbytes)
        got_kbps = slow_n * 8 / max(slow_s, 1e-6) / 1000
        floor_s = nbytes * 8 / (kbps * 1000)
        print(f"  rate:{kbps:<6g} {slow_n} bytes in {slow_s:.3f}s ({got_kbps:.0f} kbps; "
              f"the floor for {kbps:g} kbps is {floor_s:.3f}s)")
        ok &= slow_n == nbytes
        # A shaper is graded from ABOVE: it must not exceed what was asked for. A lower bound would
        # be grading the host's scheduler, which is the flaky direction.
        ok &= got_kbps <= kbps * 1.35
        if got_kbps > kbps * 1.35:
            print(f"  FAIL: measured {got_kbps:.0f} kbps against a requested {kbps:g}",
                  file=sys.stderr)

        # ...and live: throttle, then release mid-transfer from another thread.
        BUCKET.reset()
        with open(control, "w") as f:
            f.write(f"rate:{kbps:g}")

        def _release():
            time.sleep(0.2)
            with open(control, "w") as f:
                f.write("pass")

        threading.Thread(target=_release, daemon=True).start()
        live_s, live_n = pull(pport, nbytes)
        print(f"  released   {live_n} bytes in {live_s:.3f}s (throttled, then freed mid-flight)")
        ok &= live_n == nbytes
        ok &= live_s < slow_s  # the release has to be visible, or the mode is not live
        if live_s >= slow_s:
            print("  FAIL: releasing the mode mid-transfer changed nothing", file=sys.stderr)
    finally:
        origin.close()
        _srv.close()
        shutil.rmtree(tmp, ignore_errors=True)
    print(f"selftest: {'OK' if ok else 'FAILED'}")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description="network-conditioning TCP proxy")
    ap.add_argument("--listen", type=int, default=32401)
    # **`--bind 127.0.0.1` is the SIMULATOR's shape and it needs no allowlist**, because both ends
    # are this machine. Since 2026-08-28 `make sim` streams and demuxes, so conditioning a host
    # playback is an ordinary thing to want — and the alternative spelling
    # (`--allow-client 127.0.0.1` against the 0.0.0.0 default) puts a forwarding proxy for a
    # token-carrying PMS on the LAN for an experiment that never leaves loopback. The television
    # still needs the default bind and a real allowlist; this is the other case, named.
    ap.add_argument("--bind", default="0.0.0.0",
                    help="listener address (default 0.0.0.0; use 127.0.0.1 for a simulator run, "
                         "which then needs no --allow-client)")
    ap.add_argument("--target", default="127.0.0.1:32400")
    ap.add_argument("--mode", default="pass")
    ap.add_argument("--control", default="/tmp/netcond.mode")
    ap.add_argument(
        "--allow-client",
        action="append",
        default=[],
        type=ipaddress.ip_address,
        help="client IP allowed to use a non-loopback listener (repeatable; required on the LAN)",
    )
    ap.add_argument("--selftest", action="store_true",
                    help="prove rate: shapes a loopback transfer and is live-switchable, then exit "
                         "(uses its own temp control file, never --control: see _selftest)")
    ap.add_argument("--selftest-kbps", type=float, default=2000.0,
                    help="the rate --selftest shapes to (default 2000)")
    ap.add_argument("--selftest-kbytes", type=int, default=128,
                    help="how much --selftest pushes through, in KiB (default 128)")
    a = ap.parse_args()

    if a.selftest:
        return _selftest(a.selftest_kbps, a.selftest_kbytes)

    host, _, port = a.target.partition(":")
    target = (host, int(port))
    mode = Mode(a.control, a.mode)
    with open(a.control, "w") as f:
        f.write(a.mode)

    srv, _port = start_proxy(a.listen, target, mode, bind=a.bind, allow_clients=a.allow_client)
    print(
        f"[netcond] {a.bind}:{a.listen} -> {target}   mode={a.mode}   control={a.control} "
        f"allow_clients={len(a.allow_client)}",
        flush=True,
    )
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
    finally:
        print(f"[netcond] {_stats}", flush=True)
        srv.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
