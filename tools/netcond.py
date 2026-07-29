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

Any mode may be scoped to matching requests only:

    stall@/:/timeline   stall ONLY connections whose first bytes contain "/:/timeline";
                        everything else passes normally.

That scoping is what makes a clean experiment possible: freeze the progress reporter while the
video stream keeps flowing, so the freeze you then measure at teardown is unambiguously the
reporter's POST and not a starved demuxer.

## Use

    tools/netcond.py --listen 32401 --target 127.0.0.1:32400        # starts in `pass`
    echo 'stall@/:/timeline' > /tmp/netcond.mode                    # freeze the reporter
    echo pass                > /tmp/netcond.mode                    # let it go again

It logs one line per connection with the matched request line, so you can see what the app is
actually asking for, and a summary on exit. `--mode` sets the starting mode; `--control` moves the
control file.

Nothing here is deployed to the TV and nothing is secret: it forwards bytes it does not inspect
beyond the first line, and it never logs the query string — a PMS URL carries `X-Plex-Token`.
"""
import argparse
import os
import re
import socket
import sys
import threading
import time

CHUNK = 65536
_stats = {"opened": 0, "stalled": 0, "rejected": 0, "blackholed": 0, "passed": 0}
_lock = threading.Lock()


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
                new = open(self.path).read().strip()
                if new and new != self.raw:
                    print(f"[netcond] mode -> {new}", flush=True)
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


def relay(src, dst, mode, direction, stop):
    """Pump one direction, consulting the mode each chunk so a live connection can be frozen."""
    try:
        while not stop.is_set():
            act, _ = Mode.split(mode.read())
            if act == "stall":
                time.sleep(0.2)  # hold the socket open, forward nothing
                continue
            if act.startswith("delay:"):
                time.sleep(int(act.split(":", 1)[1]) / 1000.0)
            src.settimeout(0.5)
            try:
                b = src.recv(CHUNK)
            except socket.timeout:
                continue
            except OSError:
                break
            if not b:
                break
            dst.sendall(b)
    except OSError:
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
            print(f"[netcond] REJECT  {line}", flush=True)
            return
        if act == "blackhole":
            with _lock:
                _stats["blackholed"] += 1
            print(f"[netcond] BLACKHOLE {line}  (holding open, never answering)", flush=True)
            while mode.read() and applies(mode.read(), head) == "blackhole":
                time.sleep(0.25)
            return

        with _lock:
            _stats["stalled" if act == "stall" else "passed"] += 1
        print(f"[netcond] {act.upper():9s} {line}", flush=True)

        up = socket.create_connection(target, timeout=8)
        up.sendall(head)  # the bytes already taken off the client
        stop = threading.Event()
        t = threading.Thread(target=relay, args=(up, cli, mode, "up->cli", stop), daemon=True)
        t.start()
        relay(cli, up, mode, "cli->up", stop)
        t.join(timeout=2)
    except OSError as e:
        print(f"[netcond] conn error: {e}", flush=True)
    finally:
        for s in (cli, up):
            if s:
                try:
                    s.close()
                except OSError:
                    pass


def main():
    ap = argparse.ArgumentParser(description="network-conditioning TCP proxy")
    ap.add_argument("--listen", type=int, default=32401)
    ap.add_argument("--target", default="127.0.0.1:32400")
    ap.add_argument("--mode", default="pass")
    ap.add_argument("--control", default="/tmp/netcond.mode")
    a = ap.parse_args()

    host, _, port = a.target.partition(":")
    target = (host, int(port))
    mode = Mode(a.control, a.mode)
    with open(a.control, "w") as f:
        f.write(a.mode)

    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", a.listen))
    srv.listen(64)
    print(f"[netcond] :{a.listen} -> {target}   mode={a.mode}   control={a.control}", flush=True)
    try:
        while True:
            cli, _ = srv.accept()
            with _lock:
                _stats["opened"] += 1
            threading.Thread(target=serve_conn, args=(cli, target, mode), daemon=True).start()
    except KeyboardInterrupt:
        pass
    finally:
        print(f"[netcond] {_stats}", flush=True)
        srv.close()


if __name__ == "__main__":
    sys.exit(main())
