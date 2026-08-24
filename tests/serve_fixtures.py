#!/usr/bin/env python3
"""
A Range-capable static file server, for the player-PIPELINE test tier.

Why this file exists at all, when Python ships `http.server`: **`SimpleHTTPRequestHandler` does not
implement `Range`, and the failure is silent and total.** The app's demuxer seeks by closing the
socket and re-opening with `Range: bytes=<target>-` (`ff.rs::seek_cb`), and `stream.rs` accepts any
2xx — so a server that ignores the header answers `200` with the file *from byte zero*, the AVIO
layer believes it is positioned at `target`, and every subsequent byte is offset garbage. There is
no error anywhere: the demuxer reports a corrupt bitstream, or hangs looking for a start code, and
the case reads as a player bug. That is the single most important thing this file does, and it is
why `Range` is not optional here (§4 of the spec below).

THE SPEC — what the app actually requires of a server (all read out of the code, not assumed):

  1. Request line is `GET <abs-path> HTTP/1.1` with `Host:`, `User-Agent: plxnative/0.1`,
     `Accept: */*` and `Connection: close` (`stream.rs::http_open`). One request per connection;
     there is no keep-alive to support and no pipelining.
  2. Only the status code, `Content-Length:` and `Transfer-Encoding: chunked` are parsed
     (`stream.rs`, the block after the header read). Everything else we send is ignored, so extra
     headers are free but never load-bearing.
  3. Any 2xx is success (`status < 200 || status >= 300` is the rejection). `206` is therefore
     accepted, and is what a Range response must be.
  4. `Range: bytes=<n>-` MUST be honoured with `206` + a body starting at byte n. Open-ended only:
     the demuxer never sends an end offset. See the note above for what ignoring it does.
  5. `Content-Length` MUST be the length of THIS response's body (`size - n` on a 206), not the
     file size. `stream.rs` counts `consumed` per connection and stops at `content_length`; the
     total file size is captured once, at the FIRST open, into `AvioState.size` (`ff.rs:1845`) and
     is what `AVSEEK_SIZE` answers. Sending the full size on a 206 makes the reader wait for bytes
     that never come — a 15 s `SO_RCVTIMEO` stall per seek.
  6. First byte promptly: `SO_RCVTIMEO` is 15 s and the header read is a single blocking `recv`.
  7. `HEAD` is answered because it is free and makes the server debuggable by hand; the app never
     sends one.

Run it directly to serve a directory, or import `serve()` from the harness — `tests/run.py` starts
one per pipeline-tier run and stops it in teardown.

    ./tests/serve_fixtures.py --root tests/fixtures/out --port 8020

macOS trap, and it costs an afternoon every time: the **application firewall silently drops the
TV's connections to an ad-hoc python listener** — no refusal, no log line, the TV's open just reads
empty. The same shape bit `tools/netcond.py` (verified 2026-08-11). The GUI prompt "allow incoming
connections?" must be accepted once per python binary, so start this ONCE with a human at the
keyboard before going headless, and read "the server logged nothing" as the firewall rather than as
a quiet television. `--selftest` fetches from itself over the LAN address and says which it is.
"""
import argparse
import os
import re
import socket
import socketserver
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler

# `bytes=<n>-` and `bytes=<n>-<m>`. The app only ever sends the open-ended form, but a closed range
# is what a browser sends when someone points one at this server to check a fixture by hand, and
# answering it wrongly there would look like a server bug while debugging a real one.
RE_RANGE = re.compile(r"^bytes=(\d+)-(\d*)$")
RE_ABR_PLAYLIST = re.compile(r"^/__abr/(320|720|2000|4000|8000|20000)/(master|media)\.m3u8$")
RE_ABR_SEGMENT = re.compile(r"^/__abr/(320|720|2000|4000|8000|20000)/segment\.ts$")
ABR_FIXTURE = {
    "320": "pipe_abr_240p.ts", "720": "pipe_abr_480p.ts",
    "2000": "pipe_abr_720p.ts", "4000": "pipe_abr_720p.ts",
    "8000": "pipe_abr_1080p.ts", "20000": "pipe_abr_1080p.ts",
}
ABR_RASTER = {
    "320": "426x240", "720": "854x480", "2000": "1280x720",
    "4000": "1280x720", "8000": "1920x1080", "20000": "1920x1080",
}


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "plxfixtures/1.0"
    # The default logs to stderr with a timestamp; ours go through the server's sink so a harness
    # run can attribute every open and every seek to a case.
    def log_message(self, fmt, *args):  # noqa: A003  (BaseHTTPRequestHandler's name)
        self.server.note(f"{self.address_string()} {fmt % args}")

    def _resolve(self):
        """Map the request target to a real file under the root, or None.

        Path traversal is refused by resolving and re-checking containment rather than by filtering
        `..` out of the string — the filter form is the one that keeps being wrong.
        """
        target = self.path.split("?", 1)[0].split("#", 1)[0]
        seg = RE_ABR_SEGMENT.match(target)
        rel = ABR_FIXTURE[seg.group(1)] if seg else target.lstrip("/")
        full = os.path.realpath(os.path.join(self.server.root, rel))
        if full != self.server.root and not full.startswith(self.server.root + os.sep):
            return None
        return full if os.path.isfile(full) else None

    def _fail(self, code, why):
        self.server.note(f"-> {code} {why} ({self.path})")
        body = f"{code} {why}\n".encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_HEAD(self):
        self._serve(body=False)

    def do_GET(self):
        self._serve(body=True)

    def _abr_playlist(self):
        target = self.path.split("?", 1)[0].split("#", 1)[0]
        match = RE_ABR_PLAYLIST.match(target)
        if not match:
            return None
        kbps, kind = match.groups()
        if kind == "master":
            text = ("#EXTM3U\n#EXT-X-VERSION:3\n"
                    f"#EXT-X-STREAM-INF:BANDWIDTH={int(kbps) * 1000},RESOLUTION={ABR_RASTER[kbps]}\n"
                    "media.m3u8\n")
        else:
            rows = ["#EXTM3U", "#EXT-X-VERSION:3", "#EXT-X-TARGETDURATION:2",
                    "#EXT-X-MEDIA-SEQUENCE:0"]
            for sequence in range(90):
                # Each backing file is an independent MPEG-TS program with an IDR and in-band
                # SPS/PPS at its head, matching the measured PMS segment contract.
                rows.extend(("#EXTINF:2.0,", f"segment.ts?sequence={sequence}"))
            rows.append("#EXT-X-ENDLIST")
            text = "\n".join(rows) + "\n"
        return text.encode("utf-8")

    def _serve(self, body):
        playlist = self._abr_playlist()
        if playlist is not None:
            self.send_response(200)
            self.send_header("Content-Type", "application/vnd.apple.mpegurl")
            self.send_header("Content-Length", str(len(playlist)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            if body:
                self.server.count(False)
                self.server.write_body(self.wfile, playlist)
            return
        path = self._resolve()
        if path is None:
            return self._fail(404, "Not Found")
        size = os.path.getsize(path)
        start, end = 0, size - 1
        partial = False
        raw = self.headers.get("Range")
        if raw:
            m = RE_RANGE.match(raw.strip())
            if not m:
                # 416 rather than a silent 200: an unsatisfiable range that answers 200 is exactly
                # the corruption this whole file exists to prevent, and a loud refusal is the only
                # honest answer when we cannot tell what was asked for.
                return self._fail(416, f"Requested Range Not Satisfiable ({raw!r})")
            start = int(m.group(1))
            if m.group(2):
                end = min(int(m.group(2)), size - 1)
            if start >= size:
                return self._fail(416, f"Requested Range Not Satisfiable (start {start} >= {size})")
            partial = True
        length = end - start + 1
        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", "video/x-matroska")
        # §5: the length of THIS body. On a 206 that is size-start, never size.
        self.send_header("Content-Length", str(length))
        self.send_header("Accept-Ranges", "bytes")
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        if not body:
            return
        self.server.count(partial)
        try:
            with open(path, "rb") as f:
                f.seek(start)
                left = length
                while left > 0:
                    chunk = f.read(min(self.server.chunk_size(), left))
                    if not chunk:
                        break
                    self.server.write_body(self.wfile, chunk)
                    left -= len(chunk)
        except (BrokenPipeError, ConnectionResetError):
            # The demuxer closes mid-body on every seek and on teardown. That is the protocol here,
            # not an error, and printing a traceback for it would bury the real ones.
            self.server.note(f"client closed mid-body ({os.path.basename(path)} @{start})")


class FixtureServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, root, port, sink=None, bind="0.0.0.0"):
        self.root = os.path.realpath(root)
        self.sink = sink
        self.lock = threading.Lock()
        # Two counters, not a log: every request is already narrated through `note()`, and
        # `stats()` is the only reader — it wants totals. A list of per-request tuples grew for
        # the life of the run and read as though something consulted it.
        self.n_opens = self.n_ranged = 0
        self.rate_profile = []
        self.rate_started = None
        super().__init__((bind, port), FixtureHandler)

    def set_network_profile(self, profile):
        """Install one case-local wall-clock rate schedule: [{until_s, kbps}, ...].

        The clock begins on the first response body, not when the harness starts the app, so SSH,
        boot and Load latency cannot consume the fast leg before the television opens the file.
        Empty restores the ordinary unshaped fixture server.
        """
        cleaned = []
        for leg in profile or []:
            until_s, kbps = float(leg["until_s"]), int(leg["kbps"])
            if until_s <= 0 or kbps <= 0 or (cleaned and until_s <= cleaned[-1][0]):
                raise ValueError(f"invalid network-profile leg: {leg!r}")
            cleaned.append((until_s, kbps))
        with self.lock:
            self.rate_profile = cleaned
            self.rate_started = None

    def _rate_kbps(self):
        with self.lock:
            if not self.rate_profile:
                return None
            now = time.monotonic()
            if self.rate_started is None:
                self.rate_started = now
            elapsed = now - self.rate_started
            return next((kbps for until_s, kbps in self.rate_profile if elapsed < until_s),
                        self.rate_profile[-1][1])

    def chunk_size(self):
        return 64 * 1024 if self._rate_kbps() is not None else 262144

    def write_body(self, stream, data):
        stream.write(data)
        stream.flush()
        kbps = self._rate_kbps()
        if kbps is not None:
            time.sleep(len(data) * 8 / (kbps * 1000.0))

    def note(self, msg):
        if self.sink:
            self.sink(msg)

    def count(self, partial):
        with self.lock:
            self.n_opens += 1
            self.n_ranged += bool(partial)

    def stats(self):
        """(total opens, range opens) — the harness asserts on these: a seek case that never
        produced a range open never reached the demuxer's seek path at all, which is a different
        failure from a seek that landed in the wrong place."""
        with self.lock:
            return self.n_opens, self.n_ranged


def default_root():
    """Where `make fixtures-pipeline` writes the pack, and where the harness looks for it.

    One definition, imported by `tests/run.py` — the expression lived in both files, and the
    failure mode of letting them drift is the quiet one: the harness serves one directory while
    the standalone server defaults to another. NOT inside the repo, and the generator enforces
    that from its own side by refusing an `--out` under the repo root: this repository is public
    and `.gitignore` as the only defence against committing media has been got wrong here before.
    """
    return os.path.join(
        os.environ.get("FIXTURES_OUT") or os.path.expanduser("~/plxnative-fixtures"), "pipeline")


def lan_ip():
    """This machine's LAN address as the TV will reach it.

    A UDP `connect` to an off-link address picks the interface the routing table would use without
    sending anything. `gethostbyname(gethostname())` is the obvious alternative and is wrong on
    macOS — it answers 127.0.0.1 on a machine with a perfectly good LAN address, and the app cannot
    resolve names anyway (`stream.rs` takes a dotted quad only), so a loopback answer here produces
    a URL the television will never connect to.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 53))
        return s.getsockname()[0]
    finally:
        s.close()


def serve(root, port=0, sink=None):
    """Start a server on its own thread. Returns (server, url_base). Port 0 picks a free one."""
    srv = FixtureServer(root, port, sink=sink)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://{lan_ip()}:{srv.server_address[1]}"


def _selftest(root, port):
    """Prove the three things that are silently broken otherwise: the listener is reachable on the
    LAN address (not just loopback — the firewall trap), Range yields 206 with the right
    Content-Length, and the ranged body really starts at the requested offset."""
    import http.client
    names = sorted(f for f in os.listdir(root) if not f.startswith("."))
    if not names:
        print(f"selftest: nothing in {root} to serve", file=sys.stderr)
        return 1
    srv, base = serve(root, port)
    host = base.split("//", 1)[1]
    name = names[0]
    ok = True
    try:
        c = http.client.HTTPConnection(host, timeout=5)
        c.request("GET", f"/{name}", headers={"Connection": "close"})
        r = c.getresponse()
        whole = r.read()
        size = len(whole)
        print(f"  full   GET /{name} -> {r.status} {size} bytes")
        ok &= r.status == 200
        off = size // 2
        c = http.client.HTTPConnection(host, timeout=5)
        c.request("GET", f"/{name}", headers={"Range": f"bytes={off}-", "Connection": "close"})
        r = c.getresponse()
        part = r.read()
        clen = int(r.getheader("Content-Length", -1))
        print(f"  ranged GET /{name} bytes={off}- -> {r.status} "
              f"Content-Length={clen} got={len(part)} Content-Range={r.getheader('Content-Range')}")
        ok &= r.status == 206
        ok &= clen == size - off == len(part)
        ok &= part == whole[off:]
        if not ok:
            print("  FAIL: the ranged body is not the tail of the whole body", file=sys.stderr)
    finally:
        srv.shutdown()
    print(f"selftest: {'OK' if ok else 'FAILED'} on {base} (reachable from the TV at this address)")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    root = default_root()
    ap.add_argument("--root", default=root,
                    help=f"directory to serve (default $FIXTURES_OUT/pipeline, i.e. {root})")
    ap.add_argument("--port", type=int, default=8020, help="TCP port (0 = pick a free one)")
    ap.add_argument("--selftest", action="store_true",
                    help="prove Range works and the LAN address is reachable, then exit")
    a = ap.parse_args()
    if not os.path.isdir(a.root):
        print(f"no such directory: {a.root}", file=sys.stderr)
        return 2
    if a.selftest:
        return _selftest(a.root, a.port)
    srv, base = serve(a.root, a.port, sink=lambda m: print(m, flush=True))
    print(f"serving {a.root}")
    print(f"  {base}/<file>   (this is the address to put in plxnative-url)")
    print("  ^C to stop")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        srv.shutdown()
        print("\nstopped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
