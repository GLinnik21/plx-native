#!/usr/bin/env python3
#
# stream-screen.py — live screen stream from the LG webOS TV to this Mac,
#                    plus a remote-control panel (keyboard + on-screen buttons).
#
# TWO frame sources (--source, default auto):
#
#   app     — the app's OWN capture stream (crate::capture, enabled by touching
#             /tmp/plxnative-capture on the TV before launch): the app GPU-downscales
#             its GLES frames and pushes JPEGs over TCP :8910 (NEON libjpeg-turbo
#             on-device: ~30fps at 960x540).
#             UI PLANE ONLY — the hardware video overlay is invisible to it, so
#             use it for UI/navigation work, not for watching playback.
#   service — the one-shot AV-framework capture service looped
#             (luna://com.webos.service.tv.capture/executeOneShot — the only path
#             that sees the video plane; there is NO continuous-capture API on this
#             webOS 4.5 build). Hard floor measured on 49SM9000PLA:
#                 1920x1080 JPEG ~0.5 s/frame (~1.9 fps)
#                 960x540   JPEG ~0.33 s/frame (~2.9 fps)
#   auto    — app when TCP :8910 answers, else service; switches back to app
#             automatically when the port comes alive (checks every 5s).
#
# Wire modes (--codec, default mpeg): with a current app binary the app source is
# MPEG1 video in MPEG-TS (the TV's own libavcodec mpeg1video encoder, ~29fps at
# 960x540 in ~0.3-2.5Mbit) relayed to the page over a WebSocket (/ws) and decoded
# by the vendored jsmpeg (served at /jsmpeg.js) into a <canvas>. The JPEG path
# remains as the fallback: legacy app binaries, and PLAYBACK viewing — the GL
# capture can't see the video plane, so when the TS byte-flow stalls the service
# capture feeds JPEG frames and the page flips to its pipelined long-poll pull
# (/frame.jpg?after=<seq>; pull pacing keeps a slow uplink showing fewer frames,
# never older ones). Plain /frame.jpg (no query) = the latest frame, for scripting.
# The page switches automatically via /version ("<ver> <mode>").
#
# Usage:
#   ./stream-screen.py [--method DISPLAY|VIDEO|GRAPHIC] [--res 1920x1080|960x540|...]
#                      [--port 8909] [--fps N] [--open]
#     --method : plane(s) to capture. DISPLAY (default) = video + UI composited,
#                exactly as capture-screen.sh. VIDEO = overlay only, GRAPHIC = UI only.
#     --res    : capture resolution WxH (default 960x540; mpeg mode is always 960x540).
#     --port   : local HTTP port (default 8909).
#     --host   : bind address. 127.0.0.1 (default) = this machine only. Use
#                0.0.0.0 to let OTHER hosts on your network view it at
#                http://<this-machine-ip>:PORT (UNAUTHENTICATED — trusted LAN only).
#                To watch from off-network, prefer an SSH tunnel over --host 0.0.0.0:
#                on the remote host run  ssh -L 8909:127.0.0.1:8909 you@this-mac
#                then open http://127.0.0.1:8909/ there (keeps the default bind).
#     --fps    : cap the loop to at most N fps (default: unthrottled = as fast as
#                the service returns, ~2-3 fps). Lower it to reduce TV/CPU load.
#     --open   : open the stream URL in the default browser on start (macOS `open`).
#     --no-control : stream only; don't open the control channel to the TV.
#
# Remote control: the served page captures your keyboard (arrows, Enter=OK,
# Backspace/Esc=Back, PgUp/PgDn=CH, P=Play/Pause, S=Stop) and shows clickable
# buttons; each POSTs to /key, which a held SSH connection writes into the app's
# on-device FIFO /tmp/plxnative-remote — the app (crate::remote) drains it each
# frame and injects the key. Requires the plxnative app to be running (it creates
# the FIFO at boot). External input injection can't reach the app: the wayland
# compositor only reads a fixed evdev device set, and LG's keymanager luna API
# injects into the web-app layer, not our SDL/wayland path — hence the in-app FIFO.
#
# Environment overrides (same as capture-screen.sh):
#     TV_HOST (default: the gitignored .tv-host)  TV_USER (root)  TV_PASS (alpine)
#
# Auth: prefers an installed SSH key; falls back to `sshpass -p $TV_PASS` if present.
# Stop with Ctrl-C. Requires: python3 (stdlib only), ssh; sshpass only if no key.
#
import argparse, base64, functools, hashlib, os, re, shutil, signal, socket, subprocess, sys, threading, time, webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

def _default_tv_host():
    """The TV's address, from the gitignored .tv-host beside the Makefile — the same file
    `make TV=` falls back to, so the repository carries no home-network address of its own."""
    try:
        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), os.pardir, ".tv-host")) as f:
            return f.read().strip()
    except OSError:
        return ""


TV_HOST = os.environ.get("TV_HOST") or _default_tv_host()
TV_USER = os.environ.get("TV_USER", "root")
TV_PASS = os.environ.get("TV_PASS", "alpine")

SSH_OPTS = [
    "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
    "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=10",
    "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=3",
]

# ---- shared latest-frame slot ------------------------------------------------
class FrameHub:
    def __init__(self):
        self._lock = threading.Lock()
        self._cond = threading.Condition(self._lock)
        self._frame = None      # bytes of the most recent JPEG
        # Seed seq from the wall clock (ms), NOT 0: open tabs carry their last-seen seq
        # across a server restart, and a reset-to-0 counter would leave their long-polls
        # waiting minutes for "newer than N" frames that don't exist yet. The ms clock
        # outruns any frame rate, so a restart always jumps seq FORWARD past every
        # client's high-water mark and stale claims resolve immediately.
        self._seq = int(time.time() * 1000)
        self.stopped = False

    def publish(self, jpeg: bytes):
        with self._cond:
            self._frame = jpeg
            self._seq += 1
            self._cond.notify_all()

    def wait_after(self, last_seq, timeout=5.0):
        """Block until a frame with seq > last_seq exists. Returns (frame, seq) or
        (None, last_seq) on timeout/stop. Ordered (>, not !=): pipelined pollers pass
        FUTURE seqs to claim distinct upcoming frames, and each claim must wait for
        exactly that frame to exist rather than bounce back the current one."""
        deadline = time.time() + timeout
        with self._cond:
            while not self.stopped and self._seq <= last_seq:
                rem = deadline - time.time()
                if rem <= 0:
                    break
                self._cond.wait(rem)
            if self.stopped or self._seq <= last_seq or self._frame is None:
                return None, last_seq
            return self._frame, self._seq

    def snapshot(self):
        with self._lock:
            return self._frame, self._seq

    def stop(self):
        with self._cond:
            self.stopped = True
            self._cond.notify_all()


# ---- TS broadcast hub (mpeg mode): raw MPEG-TS chunks -> WebSocket clients ----
# The app's mpeg slot emits raw unframed TS (self-syncing 188B packets; jsmpeg's
# demuxer accepts arbitrary chunk boundaries). Each chunk is WS-framed ONCE here
# and fanned out to per-client bounded queues. Slow-client policy: on queue
# overflow the client is marked dead and its socket shutdown() (which also
# unblocks a writer stuck in sendall) — NEVER drop mid-stream bytes, corrupt TS
# is worse than a reconnect (the page auto-rejoins at the next GOP, <=1s).
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
WS_CLIENT_CAP = 2 * 1024 * 1024  # bytes queued per client before we cut them loose


def ws_frame(payload: bytes, opcode=0x2) -> bytes:
    """One server->client frame: FIN|opcode, unmasked, minimal length encoding."""
    n = len(payload)
    if n < 126:
        hdr = bytes([0x80 | opcode, n])
    elif n < 65536:
        hdr = bytes([0x80 | opcode, 126]) + n.to_bytes(2, "big")
    else:
        hdr = bytes([0x80 | opcode, 127]) + n.to_bytes(8, "big")
    return hdr + payload


class TsHub:
    def __init__(self):
        self.lock = threading.Lock()
        self.clients = []
        self.last_bytes = 0.0  # monotonic time TS last flowed (drives the page's mode)

    def feed(self, chunk: bytes):
        frame = ws_frame(chunk)  # framed once, shared bytes object for every client
        self.last_bytes = time.monotonic()
        with self.lock:
            for c in self.clients:
                if c["dead"]:
                    continue
                if c["bytes"] + len(frame) > WS_CLIENT_CAP:
                    self._kill_locked(c)
                    continue
                c["q"].append(frame)
                c["bytes"] += len(frame)
                c["cond"].notify_all()

    def add(self, sock):
        c = {"q": [], "bytes": 0, "cond": threading.Condition(self.lock), "dead": False, "sock": sock}
        with self.lock:
            self.clients.append(c)
        return c

    def enqueue(self, c, frame: bytes):
        with self.lock:
            if not c["dead"]:
                c["q"].append(frame)
                c["bytes"] += len(frame)
                c["cond"].notify_all()

    def _kill_locked(self, c):
        c["dead"] = True
        try:
            c["sock"].shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        c["cond"].notify_all()

    def kill(self, c):
        with self.lock:
            self._kill_locked(c)

    def remove(self, c):
        with self.lock:
            self._kill_locked(c)
            if c in self.clients:
                self.clients.remove(c)


def ws_reader(rfile, hub: TsHub, c):
    """Per-client reader: parse masked client frames via rfile (NOT sock.recv — the
    handler's BufferedReader may hold pre-read bytes). Ping -> queue a Pong for the
    WRITER to send (single-writer invariant); Close/EOF -> mark dead. This thread
    never writes the socket."""
    try:
        while not c["dead"]:
            h = rfile.read(2)
            if not h or len(h) < 2:
                break
            opcode = h[0] & 0x0F
            masked = h[1] & 0x80
            ln = h[1] & 0x7F
            if ln == 126:
                ln = int.from_bytes(rfile.read(2), "big")
            elif ln == 127:
                ln = int.from_bytes(rfile.read(8), "big")
            mask = rfile.read(4) if masked else b"\x00" * 4
            payload = rfile.read(ln) if ln else b""
            if masked and payload:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if opcode == 0x9:  # ping
                hub.enqueue(c, ws_frame(payload, opcode=0xA))
            elif opcode == 0x8:  # close
                hub.enqueue(c, ws_frame(payload[:2], opcode=0x8))
                break
    except (OSError, ValueError):
        pass
    finally:
        hub.kill(c)


# ---- on-device capture loop (busybox sh, POSIX) ------------------------------
def remote_loop(method: str, w: int, h: int, min_interval_ms: int) -> str:
    # Emits, per frame, the literal marker line "JFRM", a decimal byte-length line,
    # then exactly that many raw JPEG bytes. The pipe carries ONLY that framing.
    #
    # Two gotchas baked in here:
    #  * luna-send silently no-ops unless it has a controlling TTY (the documented
    #    ssh -tt requirement). But -tt would CRLF-mangle the binary JPEG stream. So
    #    instead we run plain (binary-clean) SSH and give luna-send its TTY *on the
    #    device* by wrapping the one-shot in `script -qc <cmd> /dev/null` — its stdout
    #    is discarded, so only our framing reaches the pipe.
    #  * busybox `date` has no %N, so `now()` reads centisecond uptime for the throttle.
    #
    # The one-shot is written to /tmp/capture/shot.sh (a heredoc, so no quote-nesting
    # war with the JSON) and re-run each frame. If a frame write fails (SSH client
    # gone → SIGPIPE) the loop dies, so it doesn't orphan on the TV.
    tmpl = r'''
mkdir -p /tmp/capture
cat > /tmp/capture/shot.sh <<'EOF'
#!/bin/sh
luna-send -n 1 "luna://com.webos.service.tv.capture/executeOneShot" "{\"path\":\"/tmp/capture/stream.jpg\",\"method\":\"__METHOD__\",\"width\":__W__,\"height\":__H__,\"format\":\"JPEG\"}"
EOF
chmod +x /tmp/capture/shot.sh
F=/tmp/capture/stream.jpg
MININT=__MININT__
now() { awk '{print int($1*1000)}' /proc/uptime; }
trap 'exit 0' TERM INT HUP
while :; do
  t0=$(now)
  rm -f "$F"
  script -qc /tmp/capture/shot.sh /dev/null >/dev/null 2>&1
  sz=$(stat -c %s "$F" 2>/dev/null || echo 0)
  if [ "$sz" -gt 0 ]; then
    printf 'JFRM\n%s\n' "$sz"
    cat "$F" || exit 0
  fi
  if [ "$MININT" -gt 0 ]; then
    dt=$(( $(now) - t0 ))
    [ "$dt" -lt "$MININT" ] && usleep $(( (MININT - dt) * 1000 )) 2>/dev/null
  fi
done
'''
    return (tmpl.replace("__METHOD__", method)
                .replace("__W__", str(w)).replace("__H__", str(h))
                .replace("__MININT__", str(min_interval_ms)).strip())


@functools.lru_cache(maxsize=1)
def _ssh_prefix():
    """Prefer key auth; fall back to sshpass if we have no key and it's installed.
    Probed ONCE — the answer can't change during a run, and KeySink reconnects call
    build_ssh_cmd from the HTTP thread serving /key, where a 10s probe would stall
    a key press (which is exactly when the TV just came back from standby)."""
    keycheck = subprocess.run(
        ["ssh", *SSH_OPTS, "-o", "BatchMode=yes", f"{TV_USER}@{TV_HOST}", "true"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if keycheck.returncode == 0:
        return []
    if shutil.which("sshpass"):
        return ["sshpass", "-p", TV_PASS]
    sys.exit(f"ERROR: cannot auth to {TV_USER}@{TV_HOST}. Install an SSH key or sshpass.")


def build_ssh_cmd(remote_script: str):
    return [*_ssh_prefix(), "ssh", *SSH_OPTS, f"{TV_USER}@{TV_HOST}", remote_script]


def read_exact(stream, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            return b""
        buf.extend(chunk)
    return bytes(buf)


def read_line(stream) -> bytes:
    # Byte-wise line read on the raw pipe (can't wrap in a buffered reader without
    # stealing bytes from the binary frame body that follows the length line).
    out = bytearray()
    while True:
        c = stream.read(1)
        if not c:
            return b""
        if c == b"\n":
            return bytes(out)
        out.extend(c)


def capture_thread(cmd, hub: FrameHub, stats: dict):
    # One run of the luna-service capture loop. Returns (instead of stopping the hub)
    # when the SSH dies — the source supervisor owns restart/failover policy.
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            bufsize=0, preexec_fn=os.setsid)
    stats["proc"] = proc
    out = proc.stdout
    try:
        while not hub.stopped:
            line = read_line(out)
            if not line:
                break
            if line != b"JFRM":         # resync: ignore any non-marker noise
                continue
            szline = read_line(out)
            if not szline:
                break
            try:
                n = int(szline)
            except ValueError:
                continue
            if n <= 0 or n > 32 * 1024 * 1024:
                continue
            jpeg = read_exact(out, n)
            if len(jpeg) != n:
                break
            hub.publish(jpeg)
            stats["frames"] += 1
    finally:
        stats["proc"] = None
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except Exception:
            pass


# ---- app source: the in-app capture stream (fast, UI plane only) --------------
# JPEG wire mode (the fallback; see the file header for the mpeg default). Framing,
# LE: "PXFR" | len:u32 | seq:u32 | ticks_ms:u32 | <len JPEG bytes>; client hello
# "PXRQ" w:u16 h:u16 (0,0 = TV default 480x270).
def sock_read_exact(sock, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return b""
        buf.extend(chunk)
    return bytes(buf)


def app_reader(hub: FrameHub, stats: dict, port: int, res, svc_cmd=None):
    """One connection to the app's capture port; returns on any error/EOF.

    Playback fallback: the app stream carries UI frames only — during video playback
    no fresh frames flow, and the TV resends the last frame every 5s with an
    UNCHANGED seq as a keepalive. When seq stalls >7s we start the service capture
    (`svc_cmd` — the only source that sees the hardware video plane, ~3fps) into the
    same hub, and kill it the moment a fresh seq arrives (back to the UI at ~20fps)."""
    STALE_S = 7.0
    try:
        sock = socket.create_connection((TV_HOST, port), timeout=3)
    except OSError:
        return False
    svc = ServiceFallback(svc_cmd, hub, stats, "UI stream")

    last_seq = None
    last_fresh = time.monotonic()
    try:
        sock.settimeout(10)  # keepalive cadence is 5s, so a healthy link never trips this
        w, h = res if res else (0, 0)
        sock.sendall(b"PXRQ" + w.to_bytes(2, "little") + h.to_bytes(2, "little"))
        while not hub.stopped:
            hdr = sock_read_exact(sock, 16)
            if len(hdr) != 16:
                return True
            if hdr[:4] != b"PXFR":
                # desync: scan forward for the magic, then re-read the rest of a header
                win = bytearray(hdr)
                while not hub.stopped:
                    i = win.find(b"PXFR")
                    if i >= 0:
                        rest = sock_read_exact(sock, 16 - (len(win) - i))
                        if not rest:
                            return True
                        hdr = bytes(win[i:]) + rest
                        break
                    win = win[-3:]
                    more = sock.recv(4096)
                    if not more:
                        return True
                    win.extend(more)
            n = int.from_bytes(hdr[4:8], "little")
            if not (0 < n <= 8 * 1024 * 1024):
                continue  # insane length: drop and rescan
            seq = int.from_bytes(hdr[8:12], "little")
            jpeg = sock_read_exact(sock, n)
            if len(jpeg) != n:
                return True
            now = time.monotonic()
            if seq != last_seq:
                last_seq = seq
                last_fresh = now
                svc.stop()  # fresh UI frame: the fast stream is authoritative again
                hub.publish(jpeg)
                stats["frames"] += 1
            elif now - last_fresh > STALE_S:
                svc.start()  # keepalive resends only: playback is up — show the video plane
    except OSError:
        return True
    finally:
        svc.stop()
        try:
            sock.close()
        except OSError:
            pass
    return True


def app_reader_mpeg(tshub: TsHub, hub: FrameHub, stats: dict, port: int, kbps: int, res=None, svc_cmd=None):
    """One connection to the app's capture port in MPEG1/TS mode (PXR2 hello kind=1).
    Raw TS bytes fan out to the WebSocket clients via TsHub. Returns 'legacy' when
    the app predates PXR2 (it answers with PXFR jpeg frames — caller falls back),
    False on connect failure, True on disconnect/EOF.

    Playback fallback mirrors app_reader: the UI stops swapping during video
    playback, the TS byte-flow stalls (>7s), and we run the service capture into
    the JPEG FrameHub — the page notices mode=jpeg (via /version) and switches to
    the pull loop until TS flows again."""
    STALE_S = 7.0
    try:
        sock = socket.create_connection((TV_HOST, port), timeout=3)
    except OSError:
        return False
    svc = ServiceFallback(svc_cmd, hub, stats, "TS stream")

    try:
        sock.settimeout(5)
        rate = max(1, min(255, kbps // 100))
        # Resolution is a real lever here, not cosmetics: MPEG1 encode cost scales with
        # macroblock count and this SoC needs ~22ms/frame at 480x270 but 50-110ms at
        # 960x540 on a detailed screen (photo backdrop) — i.e. 30fps vs ~10fps.
        w, h = res if res else (960, 540)
        sock.sendall(b"PXR2" + int(w).to_bytes(2, "little") + int(h).to_bytes(2, "little")
                     + bytes([1, rate, 0, 0]))
        first = sock_read_exact(sock, 4)
        if first == b"PXFR":
            return "legacy"  # old app binary: hello fell back to jpeg defaults
        if not first:
            return True
        tshub.feed(first)
        last_fresh = time.monotonic()
        sock.settimeout(2)
        while not hub.stopped:
            try:
                chunk = sock.recv(65536)
            except socket.timeout:
                if time.monotonic() - last_fresh > STALE_S:
                    svc.start()
                continue
            if not chunk:
                return True
            tshub.feed(chunk)
            stats["frames"] += 1
            last_fresh = time.monotonic()
            svc.stop()
    except OSError:
        return True
    finally:
        svc.stop()
        try:
            sock.close()
        except OSError:
            pass
    return True


def kill_proc(proc):
    """SIGTERM a capture subprocess's whole group (it's spawned with start_new_session)."""
    if not proc:
        return
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except Exception:
        pass


class ServiceFallback:
    """The ~3fps luna capture service, started on demand as the app stream's stand-in.

    The app stream sees the UI plane only, so during video playback it goes quiet and
    this is the ONLY source that shows the video plane. One object owns the thread +
    process lifecycle for all three callers (both app readers and the supervisor)."""

    def __init__(self, svc_cmd, hub, stats, label="UI stream"):
        self.cmd, self.hub, self.stats, self.label = svc_cmd, hub, stats, label
        self.thread = None

    def start(self):
        if self.cmd is None or (self.thread and self.thread.is_alive()):
            return
        if self.label:
            print(f"  source: {self.label} idle (video playing?) — showing the service view (~3fps)")
        self.thread = threading.Thread(target=capture_thread, args=(self.cmd, self.hub, self.stats), daemon=True)
        self.thread.start()

    def stop(self):
        if self.thread and self.thread.is_alive():
            if self.label:
                print(f"  source: {self.label} is live again — back to the app stream")
            kill_proc(self.stats.get("proc"))
            self.thread.join(timeout=3)
        self.thread = None

    def alive(self):
        return bool(self.thread and self.thread.is_alive())


def probe_app_port(port: int, timeout=1.5) -> bool:
    try:
        s = socket.create_connection((TV_HOST, port), timeout=timeout)
        s.close()
        return True
    except OSError:
        return False


def source_supervisor(hub: FrameHub, stats: dict, args, w, h, min_interval_ms, res, tshub=None):
    """Owns which source feeds the hub. app = reconnect loop on the TCP stream;
    service = restart loop on the SSH/luna capture; auto = prefer app, fall back to
    the service, and switch back the moment the app port answers again.
    With --codec mpeg (default) the app connection asks for MPEG1/TS first and
    permanently falls back to the jpeg stream when the app binary predates PXR2."""
    svc_cmd = build_ssh_cmd(remote_loop(args.method, w, h, min_interval_ms))
    legacy_app = False  # set once the app answers a PXR2 hello with PXFR frames

    svc = ServiceFallback(svc_cmd, hub, stats, label=None)  # label=None: supervisor prints its own lines

    mode = args.source
    while not hub.stopped:
        if mode in ("auto", "app") and probe_app_port(args.app_port):
            use_mpeg = args.codec == "mpeg" and tshub is not None and not legacy_app
            print(f"  source: app stream (tcp {TV_HOST}:{args.app_port}, UI plane only, "
                  f"{'mpeg1/ts' if use_mpeg else 'jpeg'})")
            # auto mode arms the playback fallback (service view while the UI stream idles)
            if use_mpeg:
                r = app_reader_mpeg(tshub, hub, stats, args.app_port, args.kbps, res,
                                    svc_cmd=svc_cmd if mode == "auto" else None)
                if r == "legacy":
                    legacy_app = True
                    print("  source: app predates the mpeg stream — using the jpeg stream")
                    continue
            else:
                app_reader(hub, stats, args.app_port, res,
                           svc_cmd=svc_cmd if mode == "auto" else None)  # returns on disconnect
            if hub.stopped:
                return
            # grace window: app relaunches (make test) come back within seconds
            for _ in range(10):
                if hub.stopped:
                    return
                if probe_app_port(args.app_port, timeout=1.0):
                    break
                time.sleep(1)
            continue  # retry app (or fall through the probe into service next loop)
        if mode == "app":
            time.sleep(1)
            continue
        # ---- service path (auto fallback, or forced) ----
        print("  source: capture service (~3fps, full DISPLAY compositing)"
              if mode == "auto" else "  source: capture service")
        svc.start()
        while not hub.stopped and svc.alive():
            if mode == "auto" and probe_app_port(args.app_port, timeout=1.0):
                print("  source: app port is back — switching to the app stream")
                svc.stop()
                break
            time.sleep(5 if mode == "auto" else 1)
        if not svc.alive() and not hub.stopped:
            time.sleep(3)  # service capture died (standby?) — restart it


# ---- remote control: write key tokens into the app's on-device FIFO ----------
# The app (crate::remote) drains /tmp/plxnative-remote each frame and pushes each
# token as a synthetic SDL key. We hold ONE persistent SSH connection running a
# writer loop and feed it tokens on stdin — no per-key SSH handshake. The loop only
# writes when the FIFO exists (app running) and time-boxes each write so a stale
# FIFO with no reader can't wedge it.
VALID_KEYS = {
    "up", "down", "left", "right", "ok", "enter", "select", "back", "esc",
    "pageup", "chup", "pagedown", "chdown", "play", "pause", "stop",
    # the two halves of OK. Every other token is a TAP (the app pushes both key edges back to
    # back), so a press-and-HOLD — which is what opens the item context menu, at press::LONG_MS
    # = 500ms — is only expressible as `okdown`, a wait, then `okup`.
    "okdown", "okup",
}
CLICK_RE = re.compile(r"^ck:\d{1,4},\d{1,4}$")  # pointer click at authored 1920x1080 coords


# Served-pull stats (the page is the only long-poll consumer, so this reads as "the
# viewer's real receive rate"): printed every 15s while frames are being served.
PULL = {"frames": 0, "bytes": 0, "nofresh": 0}
PULL_LOCK = threading.Lock()

def pull_reporter():
    while True:
        time.sleep(15)
        with PULL_LOCK:
            f, b, n = PULL["frames"], PULL["bytes"], PULL["nofresh"]
            PULL["frames"] = PULL["bytes"] = PULL["nofresh"] = 0
        if f or n:
            kb = b // f // 1024 if f else 0
            print(f"  pull: {f} frm/15s ({f/15:.1f} fps) {kb}KB avg · idle-204s {n}")


def valid_token(tok: str) -> bool:
    return tok in VALID_KEYS or bool(CLICK_RE.match(tok))
PAGE_VER = "v15"
REMOTE_FIFO = "/tmp/plxnative-remote"
_WRITER_LOOP = f'''
while IFS= read -r line; do
  [ -p {REMOTE_FIFO} ] || continue
  ( printf '%s\\n' "$line" > {REMOTE_FIFO} ) & W=$!
  ( sleep 1; kill $W 2>/dev/null ) 2>/dev/null &
done
'''

class KeySink:
    def __init__(self):
        self._lock = threading.Lock()
        self.proc = None
        self.ok = False

    def start(self):
        cmd = build_ssh_cmd(_WRITER_LOOP.strip())
        try:
            self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE,
                                         stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL,
                                         preexec_fn=os.setsid)
            self.ok = True
        except Exception as e:
            print(f"  control: failed to start ({e}); stream-only")
            self.ok = False
        return self.ok

    def send(self, token: str) -> bool:
        if not valid_token(token):
            return False
        # auto-reconnect: TV standby kills the held SSH writer; without this every
        # key after a standby is silently dead until the streamer restarts.
        if not self.ok or self.proc is None or self.proc.poll() is not None:
            now = time.monotonic()
            if now - getattr(self, "_last_retry", 0) < 3:
                return False
            self._last_retry = now
            self.stop()
            print("  control: reconnecting the key channel...")
            if not self.start():
                return False
        if self.proc is None or self.proc.stdin is None:
            return False
        with self._lock:
            try:
                self.proc.stdin.write((token + "\n").encode())
                self.proc.stdin.flush()
                return True
            except (BrokenPipeError, OSError):
                self.ok = False
                return False

    def stop(self):
        if self.proc:
            kill_proc(self.proc)


# ---- local HTTP: MJPEG stream + control + single-frame + WS endpoints --------
def make_handler(hub: FrameHub, sink, tshub: TsHub = None, jsmpeg_js: bytes = b""):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):  # quiet
            pass

        def _mode(self) -> str:
            # what the PAGE should display right now: mpeg while TS flows (UI up +
            # mpeg-capable app), jpeg otherwise (legacy app, or playback -> the
            # service capture feeds the JPEG hub and TS goes byte-silent).
            if tshub is not None and time.monotonic() - tshub.last_bytes < 6.0:
                return "mpeg"
            return "jpeg"

        def _ws(self):
            # WebSocket relay of the raw TS stream (RFC6455, stdlib-only). The 101
            # is raw-written: send_response would emit HTTP/1.0 + Server/Date headers.
            key = self.headers.get("Sec-WebSocket-Key")
            upgrade = (self.headers.get("Upgrade") or "").lower()
            if tshub is None or not key or "websocket" not in upgrade:
                self.send_error(400 if tshub is not None else 503)
                return
            accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode("ascii")).digest())
            self.close_connection = True  # never try to parse a 2nd request on this socket
            try:
                self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            except OSError:
                pass
            try:
                self.wfile.write(b"HTTP/1.1 101 Switching Protocols\r\n"
                                 b"Upgrade: websocket\r\nConnection: Upgrade\r\n"
                                 b"Sec-WebSocket-Accept: " + accept + b"\r\n\r\n")
            except OSError:
                return
            c = tshub.add(self.connection)
            threading.Thread(target=ws_reader, args=(self.rfile, tshub, c), daemon=True).start()
            # this request thread is the SINGLE writer for the socket (reader only queues)
            try:
                while True:
                    with tshub.lock:
                        while not c["q"] and not c["dead"]:
                            c["cond"].wait(timeout=30)
                        if c["dead"] and not c["q"]:
                            break
                        frame = c["q"].pop(0)
                        c["bytes"] -= len(frame)
                    self.connection.sendall(frame)
            except OSError:
                pass
            finally:
                tshub.remove(c)

        def _jsmpeg(self):
            if not jsmpeg_js:
                self.send_error(404, "jsmpeg.min.js not found next to stream-screen.py")
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/javascript")
            self.send_header("Cache-Control", "max-age=86400")
            self.send_header("Content-Length", str(len(jsmpeg_js)))
            self.end_headers()
            self.wfile.write(jsmpeg_js)

        def _index(self):
            control = bool(sink and sink.ok)
            panel = """
<div id=remote title="click a button, or use your keyboard">
  <div class=row><button data-k=back>⏴ Back</button><button data-k=up>▲</button><button data-k=play>⏯</button></div>
  <div class=row><button data-k=left>◀</button><button data-k=ok class=ok>OK</button><button data-k=right>▶</button></div>
  <div class=row><button data-k=stop>⏹</button><button data-k=down>▼</button><button data-k=pageup>CH▲</button></div>
</div>""" if control else ""
            kbdhelp = (f"{PAGE_VER} · Click the picture to tap that spot on the TV · Keyboard: arrows · Enter=OK · Backspace/Esc=Back · P=Play/Pause · PgUp/PgDn=CH · S=Stop"
                       if control else "Control disabled (started with --no-control, or the app/FIFO isn't up).")
            js = """
<script>
const MAP={ArrowUp:'up',ArrowDown:'down',ArrowLeft:'left',ArrowRight:'right',
  Enter:'ok',Backspace:'back',Escape:'back',PageUp:'pageup',PageDown:'pagedown',
  ' ':'play',p:'play',P:'play',s:'stop',S:'stop'};
const VER='{PAGE_VER}';
// phone home on load (dbg- tokens are rejected but logged server-side) so the server
// log always shows WHICH page version a client is actually running
fetch('/key?k=dbg-load-'+VER,{method:'POST'}).catch(()=>{});
function key(k){fetch('/key?k='+encodeURIComponent(k),{method:'POST'})
  .then(r=>{const f=document.getElementById('flash');f.textContent=k;f.style.opacity=1;
    setTimeout(()=>f.style.opacity=0,250);}).catch(()=>{});}
if(document.getElementById('remote')){
  // e.repeat guard: OS key auto-repeat would fire a burst of discrete presses
  // (a held OK once auto-started a movie). Hold = one press.
  addEventListener('keydown',e=>{if(e.repeat)return;const k=MAP[e.key];if(k){e.preventDefault();key(k);}});
  document.querySelectorAll('#remote button').forEach(b=>b.onclick=()=>key(b.dataset.k));
  // click-through: a click on the streamed picture becomes a Magic-Remote pointer
  // click at the same spot on the TV (the app hit-tests it — cards, buttons, tabs).
  // mousedown, NOT click: a click with any drag motion starts a native image-drag
  // and the click event never fires (macOS/trackpad especially). mousedown always
  // fires first, and preventDefault also stops the drag ghost.
  // Bound to the CONTAINER: the picture is the <img> in jpeg mode and the <canvas>
  // in mpeg mode; the container's rect tracks whichever is visible.
  const scr=document.getElementById('screen');
  const simg=document.querySelector('#screen img');
  simg.draggable=false;
  simg.style.webkitUserDrag='none';
  scr.style.cursor='crosshair';
  scr.style.userSelect='none';
  const mapXY=e=>{const r=scr.getBoundingClientRect();
    return [Math.round((e.clientX-r.left)/r.width*1920),
            Math.round((e.clientY-r.top)/r.height*1080)];};
  // LOCAL crosshair only — hover must NOT move the TV focus: forwarding hover
  // (the v8 experiment) meant every mouse pass over the picture's top band parked
  // the app focus on a tab pill, and the next ENTER opened the library — the
  // exact "throws me to TV Shows" trap. Keyboard focus now moves only on keys;
  // clicks hit-test by coordinates and need no hover.
  const cross=document.createElement('div');
  cross.style.cssText='position:absolute;width:10px;height:10px;border-radius:50%;'+
    'border:2px solid #fb5;pointer-events:none;transform:translate(-50%,-50%);display:none';
  document.getElementById('screen').appendChild(cross);
  scr.addEventListener('mousemove',e=>{
    const r=scr.getBoundingClientRect();
    cross.style.display='block';
    cross.style.left=(e.clientX-r.left)+'px';
    cross.style.top=(e.clientY-r.top)+'px';
  });
  scr.addEventListener('mouseleave',()=>{cross.style.display='none';});
  scr.addEventListener('mousedown',e=>{
    if(e.button!==0)return;
    e.preventDefault();
    const [x,y]=mapXY(e);
    key('ck:'+x+','+y);
    // click-landing dot: brief marker exactly where the click mapped
    const d=document.createElement('div');
    d.style.cssText='position:absolute;width:14px;height:14px;border-radius:50%;'+
      'border:2px solid #7fd;background:#7fd5;pointer-events:none;transform:translate(-50%,-50%);'+
      'left:'+(e.clientX-scr.getBoundingClientRect().left)+'px;top:'+(e.clientY-scr.getBoundingClientRect().top)+'px';
    document.getElementById('screen').appendChild(d);
    setTimeout(()=>d.remove(),600);
  });
}
</script>""" if control else ""
            js = js.replace("{PAGE_VER}", PAGE_VER)
            # Always-on script (control or not): mode switching + both frame paths.
            # PRIMARY (mode=mpeg): jsmpeg decodes the MPEG1/TS WebSocket stream into the
            # canvas — real video codec, ~30fps at a fraction of the JPEG bandwidth.
            # FALLBACK (mode=jpeg): the pipelined long-poll JPEG pull into the <img>
            # (legacy app, or during playback when the service capture is the source).
            # The server's /version reply ("<ver> <mode>") drives the switch.
            pump_js = """
<script src=/jsmpeg.js></script>
<script>
(()=>{
  const PVER='{PAGE_VER}';  // scoped: the control script declares its own global VER
  const img=document.querySelector('#screen img');
  const canvas=document.querySelector('#screen canvas');
  const stat=document.getElementById('stat');
  const sleep=ms=>new Promise(z=>setTimeout(z,ms));
  let mode='jpeg',player=null,wsBytes=0;
  function startWS(){
    if(player||typeof JSMpeg==='undefined')return;
    try{
      player=new JSMpeg.Player('ws://'+location.host+'/ws',{canvas:canvas,audio:false,pauseWhenHidden:false});
    }catch(e){player=null;}
  }
  function stopWS(){
    if(!player)return;
    try{player.destroy();}catch(e){} // WASM-compile race: socket may not exist yet
    player=null;
  }
  function setMode(m){
    if(m===mode)return;
    mode=m;
    if(m==='mpeg'){startWS();}else{stopWS();}
    img.style.display=(m==='mpeg')?'none':'block';
    canvas.style.display=(m==='mpeg')?'block':'none';
  }
  setInterval(()=>{fetch('/version').then(r=>r.text()).then(t=>{
    const p=t.trim().split(/\\s+/);
    if(p[0]&&p[0]!==PVER)location.reload();
    setMode(p[1]==='mpeg'?'mpeg':'jpeg');
  }).catch(()=>{});},2000);
  // mpeg-mode stat: count WS payload bytes (re-hook after every jsmpeg reconnect)
  setInterval(()=>{
    if(player&&player.source&&player.source.socket&&!player.source.socket._hooked){
      const s=player.source.socket;s._hooked=true;
      s.addEventListener('message',e=>{wsBytes+=(e.data&&e.data.byteLength)||0;});
    }
  },1000);
  setInterval(()=>{
    if(mode==='mpeg'){stat.textContent=(wsBytes*8/2e6).toFixed(1)+' Mbps · mpeg1 960x540';wsBytes=0;}
  },2000);
  // PIPELINED pull (jpeg mode only): DEPTH pollers each claim a distinct upcoming
  // frame (after=N = "first frame with seq>N", ordered server-side), so a high-RTT
  // link gets DEPTH frames per round trip while every response is still the newest
  // frame the server had — falling behind skips frames, never queues them. On 204
  // (UI idle or server restart renumber) claims resync after 3 straight timeouts.
  const DEPTH=4;
  let shown=0,ask=0,prev=null,n=0,bytes=0,t0=performance.now(),idle=0;
  async function one(){
    if(mode!=='jpeg'){await sleep(400);return;}
    const a=ask;ask=a+1;
    const r=await fetch('/frame.jpg?after='+a,{cache:'no-store'});
    if(r.status===204){
      if(++idle>=3){shown=0;ask=0;}else{ask=Math.min(ask,shown);}
      return;
    }
    if(!r.ok){await sleep(700);return;}
    idle=0;
    const S=+(r.headers.get('X-Seq')||0);
    ask=Math.max(ask,S+1);
    const b=await r.blob();
    if(S<=shown)return;                       // stale/duplicate — a newer frame already shown
    const u=URL.createObjectURL(b);
    // decode on a private off-screen Image: concurrent pollers sharing img.onload
    // would overwrite each other's handler and strand a worker mid-await
    const t=new Image();
    await new Promise((res,rej)=>{t.onload=res;t.onerror=rej;t.src=u;});
    if(S<=shown){URL.revokeObjectURL(u);return;} // lost the race while decoding
    img.src=u;                                // decoded already — swap is instant
    if(prev)URL.revokeObjectURL(prev);        // revoke AFTER the swap: early revoke = broken img
    prev=u;shown=S;
    n++;bytes+=b.size;
    const dt=performance.now()-t0;
    if(dt>2000&&mode==='jpeg'){
      stat.textContent=(n*1000/dt).toFixed(1)+' fps · '+Math.round(bytes/n/1024)+' KB/frm';
      n=0;bytes=0;t0=performance.now();
    }
  }
  for(let i=0;i<DEPTH;i++)(async()=>{await sleep(i*40);for(;;){try{await one();}catch(e){await sleep(700);}}})();
})();
</script>""".replace("{PAGE_VER}", PAGE_VER)
            html = f"""<!doctype html><meta charset=utf-8>
<title>TV — {TV_HOST}</title>
<style>
 html,body{{margin:0;background:#0b0b0d;height:100%;font:14px system-ui,sans-serif;color:#ccc}}
 body{{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px}}
 #screen{{position:relative;line-height:0}}
 #screen img,#screen canvas{{display:block;width:min(96vw,calc(78vh*1.7778));height:auto;image-rendering:auto}}
 #flash{{position:absolute;top:10px;left:10px;background:#000a;padding:4px 10px;border-radius:6px;
   font:600 13px system-ui;opacity:0;transition:opacity .15s;color:#7fd}}
 #remote{{display:flex;flex-direction:column;gap:8px;align-items:center}}
 .row{{display:flex;gap:8px}}
 #remote button{{width:76px;height:44px;border:1px solid #333;border-radius:10px;background:#1a1a1e;
   color:#ddd;font-size:16px;cursor:pointer}}
 #remote button:active{{background:#2b2b34}}
 #remote button.ok{{background:#2d4bff22;border-color:#2d4bff88}}
 .hint{{color:#666;font-size:12px}}
</style>
<div id=screen><img alt="waiting for first frame…"><canvas style="display:none"></canvas><div id=flash></div></div>
{panel}
<div class=hint>{kbdhelp} · <span id=stat></span></div>
{pump_js}
{js}""".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store, must-revalidate")  # stale page = dead controls
            self.send_header("Content-Length", str(len(html)))
            self.end_headers()
            self.wfile.write(html)

        def _key(self):
            from urllib.parse import urlparse, parse_qs
            q = parse_qs(urlparse(self.path).query)
            tok = (q.get("k") or [""])[0].lower()
            ok = bool(sink) and sink.send(tok)
            print(f"  [{time.strftime('%H:%M:%S')}] key: {tok!r} -> {'sent' if ok else 'REJECTED'}")
            self.send_response(200 if ok else 400)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"ok" if ok else b"no")

        def _frame(self):
            # Plain GET = the latest frame now. `?after=<seq>` = long-poll: block until a
            # frame NEWER than seq exists (204 after 10s of nothing — player route sends no
            # UI frames). This is what the page uses: pull pacing keeps a slow link at ~1
            # frame of latency, where the push MJPEG stream buffers seconds of stale frames
            # into the SSH hops' flow-control windows (and delays /key POSTs behind them).
            from urllib.parse import urlparse, parse_qs
            q = parse_qs(urlparse(self.path).query)
            after = q.get("after")
            if after is not None:
                try:
                    last = int(after[0])
                except ValueError:
                    last = 0
                f, seq = hub.wait_after(last, timeout=10.0)
                if f is None:
                    with PULL_LOCK:
                        PULL["nofresh"] += 1
                    self.send_response(204)
                    self.send_header("X-Seq", str(last))
                    self.send_header("Cache-Control", "no-store")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                with PULL_LOCK:
                    PULL["frames"] += 1
                    PULL["bytes"] += len(f)
            else:
                f, seq = hub.snapshot()
                if not f:
                    self.send_error(503, "no frame yet")
                    return
            self.send_response(200)
            self.send_header("Content-Type", "image/jpeg")
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Seq", str(seq))
            self.send_header("Content-Length", str(len(f)))
            self.end_headers()
            self.wfile.write(f)

        def do_GET(self):
            if self.path.startswith("/version"):
                # "<ver> <mode>": the page reloads on a ver change and switches its
                # display (jsmpeg canvas vs JPEG pull img) on the mode token.
                body = f"{PAGE_VER} {self._mode()}".encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
                self.send_header("Cache-Control", "no-store")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif self.path.startswith("/ws"):
                self._ws()
            elif self.path.startswith("/jsmpeg.js"):
                self._jsmpeg()
            elif self.path.startswith("/frame.jpg"):
                self._frame()
            else:
                self._index()

        def do_POST(self):
            if self.path.startswith("/key"):
                self._key()
            else:
                self.send_error(404)

    return H


def main():
    ap = argparse.ArgumentParser(description="Live MJPEG stream of the webOS TV screen.")
    ap.add_argument("--method", default="DISPLAY", choices=["DISPLAY", "VIDEO", "GRAPHIC"],
                    help="capture-service plane (service source only; the app source is "
                         "always UI-only). VIDEO forces --source service.")
    ap.add_argument("--res", default=None,
                    help="capture WxH — 480x270 or 960x540 (quantized to the nearer). "
                         "This is a SPEED lever for mpeg mode: 480x270 encodes in ~22ms "
                         "(~30fps) where a detailed screen at 960x540 costs 50-110ms "
                         "(~10-19fps) on this SoC. Default 960x540.")
    ap.add_argument("--source", default="auto", choices=["auto", "app", "service"],
                    help="frame source: 'app' = the in-app stream (fast, UI plane only; "
                         "needs /tmp/plxnative-capture on the TV), 'service' = the luna "
                         "capture service (~3fps, sees the video plane), 'auto' (default) "
                         "= app when its port answers, else service, switching back "
                         "automatically.")
    ap.add_argument("--codec", default="mpeg", choices=["mpeg", "jpeg"],
                    help="app-source wire codec: mpeg = MPEG1/TS -> jsmpeg canvas (default), "
                         "jpeg = legacy PXFR JPEG pull")
    ap.add_argument("--kbps", type=int, default=2500,
                    help="mpeg video bitrate in kbps (default 2500)")
    ap.add_argument("--app-port", type=int, default=int(os.environ.get("TV_APP_PORT", "8910")),
                    help="the app capture stream's TCP port (default 8910 / $TV_APP_PORT)")
    ap.add_argument("--port", type=int, default=8909)
    ap.add_argument("--host", default="127.0.0.1",
                    help="bind address. 127.0.0.1 (default) = this machine only; "
                         "0.0.0.0 = reachable from other hosts on the network "
                         "(UNAUTHENTICATED — only on a network you trust).")
    ap.add_argument("--fps", type=float, default=0.0, help="cap loop to N fps (0 = unthrottled)")
    ap.add_argument("--open", action="store_true", help="open the URL in a browser")
    ap.add_argument("--control", dest="control", action="store_true", default=True,
                    help="serve a remote-control panel (keyboard + buttons) — default on")
    ap.add_argument("--no-control", dest="control", action="store_false",
                    help="stream only; do not open the control channel to the TV")
    args = ap.parse_args()

    res = None  # None = each source's default (service 960x540, app 480x270)
    if args.res:
        try:
            res = tuple(int(x) for x in args.res.lower().split("x"))
        except Exception:
            sys.exit(f"ERROR: bad --res {args.res!r}; expected WxH like 960x540")
    w, h = res if res else (960, 540)  # service-path capture size
    min_interval_ms = int(1000 / args.fps) if args.fps and args.fps > 0 else 0
    if args.method == "VIDEO":
        if args.source == "app":
            sys.exit("ERROR: --source app streams the UI plane only; --method VIDEO needs --source service")
        args.source = "service"  # the app source can't see the video plane

    hub = FrameHub()
    tshub = TsHub() if args.codec == "mpeg" else None
    jsmpeg_js = b""
    if tshub is not None:
        try:
            with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "jsmpeg.min.js"), "rb") as f:
                jsmpeg_js = f.read()
        except OSError:
            print("WARN: jsmpeg.min.js not found next to stream-screen.py — mpeg mode disabled")
            tshub = None
            args.codec = "jpeg"
    stats = {"frames": 0, "proc": None}
    sup = threading.Thread(target=source_supervisor,
                           args=(hub, stats, args, w, h, min_interval_ms, res, tshub), daemon=True)
    sup.start()
    threading.Thread(target=pull_reporter, daemon=True).start()

    sink = None
    if args.control:
        sink = KeySink()
        sink.start()

    httpd = ThreadingHTTPServer((args.host, args.port),
                                make_handler(hub, sink, tshub, jsmpeg_js))
    # For the printed URL: on a wildcard/LAN bind, resolve this machine's primary
    # LAN IP so the line is copy-pasteable from another host; else use the bind addr.
    disp_host = args.host
    if args.host in ("0.0.0.0", "::", ""):
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.connect(("192.0.2.1", 80))   # TEST-NET-1; any routable addr, no packets sent
            disp_host = s.getsockname()[0]
            s.close()
        except Exception:
            disp_host = "127.0.0.1"
    url = f"http://{disp_host}:{args.port}/"
    print(f"Streaming TV {TV_HOST} [source={args.source}"
          f"{'' if not min_interval_ms else f', <= {args.fps:g} fps'}]  ->  {url}")
    print("  live view : " + url)
    print("  latest jpg: " + url + "frame.jpg")
    if sink and sink.ok:
        print("  remote    : keyboard + on-screen buttons in the page (arrows/Enter/Backspace/…)")
    elif args.control:
        print("  remote    : control channel unavailable (couldn't reach the TV)")
    if args.host in ("0.0.0.0", "::", ""):
        print("  (bound to all interfaces — anyone on this network can view AND control it, no auth)")
    print("  Ctrl-C to stop.")
    if args.open:
        webbrowser.open(f"http://127.0.0.1:{args.port}/")

    def shutdown(*_):
        hub.stop()
        if sink:
            sink.stop()
        proc = stats.get("proc")
        if proc:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except Exception:
                pass
        threading.Thread(target=httpd.shutdown, daemon=True).start()

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    t0 = time.monotonic()
    try:
        httpd.serve_forever(poll_interval=0.5)
    finally:
        dt = time.monotonic() - t0
        n = stats["frames"]
        print(f"\nStopped. {n} frames in {dt:.0f}s ({n/dt:.1f} fps avg)." if dt > 0 else "\nStopped.")


if __name__ == "__main__":
    main()
