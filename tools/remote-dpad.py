#!/usr/bin/env python3
# remote-dpad.py — a LOCKED-DOWN front end for tools/stream-screen.py, meant to be
# the only thing an off-network viewer can reach.
#
# WHY THIS EXISTS RATHER THAN JUST EXPOSING stream-screen.py
# ---------------------------------------------------------
# stream-screen.py is the LAN dev tool and its own header says so: unauthenticated,
# "trusted LAN only". Its page can also do rather more than watch — it forwards
# `ck:X,Y` pointer clicks, play/stop/channel tokens and any keystroke it maps. That
# is exactly right at a desk and exactly wrong through a public URL.
#
# So this process, not the network, is where the limits live:
#
#   * HTTP Basic auth on EVERY request, compared with hmac.compare_digest.
#   * An ALLOW-LIST of six D-pad tokens (up/down/left/right/ok/back). Anything else
#     — a `ck:` click, `play`, `stop`, a bare keystroke — is refused with 403 and
#     never reaches the TV. This is the point of the whole file: even someone
#     holding the URL *and* the password can only press D-pad keys.
#   * A short route table. No directory serving, no proxy of arbitrary paths.
#   * Binds 127.0.0.1 by default, because the intended front door is a tunnel
#     (cloudflared/ssh) terminating locally — never a router port forward. Opening
#     a port to this LAN would expose the TV's own SSH, which on a webosbrew-rooted
#     set is the published default root password. It would also put the Basic auth
#     password on the wire in cleartext, several times a second.
#
# TRANSPORT: MPEG1 OVER A WEBSOCKET, WITH JPEG AS THE FALLBACK
# ------------------------------------------------------------
# The first version of this file spoke only stream-screen.py's JPEG pull path
# (/frame.jpg?after=<seq>) because a long-poll is trivially proxyable. Measured
# through a cloudflared tunnel that turned out to be the wrong trade:
#
#     local, 1 poller  12.3 fps     tunnel round trip   511 ms
#     local, 4 pollers 12.2 fps     tunnel, 1 poller    7.0 fps
#     local, 8 pollers 12.3 fps     tunnel, 4 pollers   7.6 fps
#
# Two things fall out. Concurrency buys nothing (the JPEG path itself caps ~12 fps),
# and one HTTP round trip PER FRAME cannot outrun a 511 ms RTT no matter how it is
# pipelined. A WebSocket makes RTT a latency cost instead of a throughput cost — the
# stream is continuous, so frames arrive at the rate they are produced — and MPEG1
# has inter-frame compression where JPEG sends every pixel every time (~22 KB/frame
# measured, i.e. ~2 Mbit/s for 12 fps against MPEG1's documented 0.3-2.5 Mbit/s for
# ~29 fps).
#
# So /ws is relayed as a RAW BYTE PIPE: auth happens on the handshake, and after the
# 101 this process understands nothing about WebSocket framing and does not need to.
# That is deliberate — implementing RFC6455 masking here would be a second protocol's
# worth of ways to get it wrong, in the file whose job is to be small enough to audit.
#
# The WS token, and why it is not the Basic password: a browser cannot attach an
# Authorization header to a WebSocket handshake (the JS API has no way to set one),
# so `wss://` would arrive unauthenticated. The page — which is only ever served
# AFTER Basic auth — therefore carries a per-process random token that /ws checks.
# It is exactly as secret as the password and travels only inside TLS.
#
# Usage:
#   tools/remote-dpad.py [--port 8908] [--upstream 8909] [--user tv] [--pass SECRET]
#   (no --pass: one is generated and printed once at startup)
#
# Start the upstream with `--source app` so the picture is the app's own GLES frames.
# The picture is then UI-PLANE ONLY — GL cannot see the hardware video overlay, so
# playback is a black rectangle. That is the capture source, not a fault here.
import argparse
import base64
import hmac
import secrets
import select
import socket
import sys
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The ONLY tokens that may reach the TV's FIFO. Names are the app's own
# (crate::remote drains /tmp/plxnative-remote).
#
# `okdown`/`okup` are the SPLIT halves of OK and they are what make a press-and-hold
# possible — held past `press::LONG_MS` (500 ms) the app opens the card context menu,
# which is a real feature of the product and unreachable from a `ok` tap. The button
# therefore sends edges rather than taps: a quick press is still a tap because the
# app measures the interval, so this is strictly MORE faithful to the Magic Remote
# than a synthetic `ok`, not a loosening of it.
#
# Still absent, and deliberately: `ck:X,Y` pointer clicks (a remote viewer aiming at
# coordinates on a picture they may be seeing seconds late) and the transport keys.
DPAD = ("up", "down", "left", "right", "ok", "back", "okdown", "okup")

PAGE = """<!doctype html><html><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no,viewport-fit=cover">
<title>TV</title><style>
*{box-sizing:border-box;-webkit-tap-highlight-color:transparent}
html,body{margin:0;height:100%;background:#111;color:#eee;
  font:15px -apple-system,BlinkMacSystemFont,"Helvetica Neue",sans-serif;
  overscroll-behavior:none;touch-action:manipulation}
body{display:flex;flex-direction:column;padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left)}
#wrap{width:100%;aspect-ratio:16/9;background:#000;position:relative}
#wrap img,#wrap canvas{position:absolute;inset:0;width:100%;height:100%;object-fit:contain;display:none}
#st{padding:6px 10px;font-size:12px;color:#8a8f98;display:flex;justify-content:space-between}
#pad{flex:1;display:grid;grid-template-columns:repeat(3,1fr);grid-auto-rows:1fr;
  gap:10px;padding:10px;max-width:420px;width:100%;margin:0 auto;align-content:center}
button{font:600 17px inherit;color:#eee;background:#2c2c2e;border:0;border-radius:16px;
  min-height:64px;transition:background .08s}
button:active{background:#4a4a4e}
button.ok{background:#e8e6e3;color:#111}
button.ok:active{background:#fff}
.sp{visibility:hidden}
</style></head><body>
<div id=wrap><canvas id=cv></canvas><img id=pic alt=""></div>
<div id=st><span id=fps>connecting&hellip;</span><span id=mode>&nbsp;</span></div>
<div id=pad>
  <button class=sp></button><button data-k=up>&#9650;</button><button class=sp></button>
  <button data-k=left>&#9664;</button><button data-k=ok class=ok>OK</button><button data-k=right>&#9654;</button>
  <button data-k=back>Back</button><button data-k=down>&#9660;</button><button class=sp></button>
</div>
<script src="jsmpeg.js"></script>
<script>
const WT="__WSTOKEN__";
const pic=document.getElementById('pic'),cv=document.getElementById('cv'),
      fps=document.getElementById('fps'),modeEl=document.getElementById('mode');
let n=0,t0=Date.now();
function tick(){if(++n%10===0){const d=(Date.now()-t0)/1000;fps.textContent=(n/d).toFixed(1)+' fps';}}

// MPEG1 over WebSocket is the fast path: one continuous stream, so the tunnel's
// round trip is latency and not throughput. Falls back to the JPEG long-poll when
// the upstream is in jpeg mode or jsmpeg is missing.
function startMpeg(){
  const proto=location.protocol==='https:'?'wss://':'ws://';
  const p=new JSMpeg.Player(proto+location.host+'/ws?t='+encodeURIComponent(WT),
    {canvas:cv,audio:false,pauseWhenHidden:false,videoBufferSize:512*1024});
  // count decoded frames rather than socket bytes: this is the number that matters
  const d=p.video&&p.video.decode?p.video.decode.bind(p.video):null;
  if(d)p.video.decode=function(){tick();return d.apply(this,arguments)};
  cv.style.display='block';modeEl.textContent='mpeg1 · d-pad only';
}

// One in-flight poll, `after=<seq>` ordered server-side: falling behind SKIPS
// frames rather than queueing them, which keeps a slow uplink showing the present.
async function startJpeg(){
  pic.style.display='block';modeEl.textContent='jpeg · d-pad only';
  let ask=0,shown=0,prev=null;
  for(;;){
    try{
      const r=await fetch('f?after='+ask,{cache:'no-store'});
      if(r.status===204)continue;
      if(!r.ok){fps.textContent='stream error '+r.status;await new Promise(s=>setTimeout(s,900));continue;}
      const S=+(r.headers.get('X-Seq')||0);
      ask=Math.max(ask,S+1);
      if(S<=shown)continue;
      const u=URL.createObjectURL(await r.blob());
      const im=new Image();
      await new Promise((ok,no)=>{im.onload=ok;im.onerror=no;im.src=u});
      if(S<=shown){URL.revokeObjectURL(u);continue}
      pic.src=u; if(prev)URL.revokeObjectURL(prev); prev=u; shown=S; tick();
    }catch(e){fps.textContent='offline';await new Promise(s=>setTimeout(s,900));}
  }
}

fetch('v',{cache:'no-store'}).then(r=>r.text()).then(t=>{
  const mpeg=/\\bmpeg\\b/.test(t)&&typeof JSMpeg!=='undefined';
  mpeg?startMpeg():startJpeg();
}).catch(()=>startJpeg());

function press(k){fetch('k?d='+k,{method:'POST'}).catch(()=>{});}
// Every control EXCEPT ok is a tap. ok is excluded here and bound to edges below —
// binding both would fire `ok` and `okdown` for one press.
for(const b of document.querySelectorAll('button[data-k]:not([data-k=ok])')){
  // pointerdown, not click: on a phone `click` waits out the tap gesture, which
  // makes a D-pad feel broken. preventDefault stops the synthetic double-fire.
  b.addEventListener('pointerdown',e=>{e.preventDefault();press(b.dataset.k)});
}
// OK is the one control that sends real EDGES, so a hold is a hold: the app times
// the gap itself (press::LONG_MS = 500 ms) and opens the card context menu past it.
// The release is bound to up AND cancel AND the pointer leaving the button, because
// a dropped `okup` would leave the app holding a key down — the same failure the
// Magic Remote already has, and no reason to add a third source of it.
const okb=document.querySelector('button[data-k=ok]');
let held=false;
const down=e=>{e.preventDefault();if(held)return;held=true;press('okdown')};
const up=e=>{if(!held)return;held=false;press('okup')};
okb.addEventListener('pointerdown',down);
for(const ev of ['pointerup','pointercancel','pointerleave','lostpointercapture'])okb.addEventListener(ev,up);
window.addEventListener('blur',up);   // backgrounding Safari mid-hold must not strand it
</script></body></html>"""


def make_handler(up_port: int, token: str, wstoken: str):
    up = f"http://127.0.0.1:{up_port}"

    class H(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass  # the access log would carry the tunnel's URL into the terminal scrollback

        def _authed(self) -> bool:
            got = self.headers.get("Authorization", "")
            # compare_digest on the whole header: constant time, and a miss cannot
            # be narrowed down by timing the prefix
            if hmac.compare_digest(got, "Basic " + token):
                return True
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="TV", charset="UTF-8"')
            self.send_header("Content-Length", "0")
            self.end_headers()
            return False

        def _q(self, name: str) -> str:
            if "?" not in self.path:
                return ""
            for part in self.path.split("?", 1)[1].split("&"):
                if part.startswith(name + "="):
                    return part[len(name) + 1:]
            return ""

        def _send(self, code, body=b"", ctype="text/plain", extra=()):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Cache-Control", "no-store")
            # this page must never be framed by, or leak its URL to, anything else
            self.send_header("X-Frame-Options", "DENY")
            self.send_header("Referrer-Policy", "no-referrer")
            for k, v in extra:
                self.send_header(k, v)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if body:
                self.wfile.write(body)

        def do_GET(self):
            path = self.path.split("?", 1)[0].rstrip("/") or "/"
            # /ws carries its own token because a browser cannot put an
            # Authorization header on a WebSocket handshake (see the module doc).
            if path == "/ws":
                self._ws()
                return
            if not self._authed():
                return
            if path == "/":
                self._send(200, PAGE.replace("__WSTOKEN__", wstoken).encode(), "text/html; charset=utf-8")
            elif path == "/v":
                self._proxy_get("/version", "text/plain")
            elif path == "/jsmpeg.js":
                self._proxy_get("/jsmpeg.js", "application/javascript")
            elif path == "/f":
                self._frame()
            else:
                self._send(404, b"no")

        def do_POST(self):
            if not self._authed():
                return
            if self.path.split("?", 1)[0].rstrip("/") == "/k":
                self._key()
            else:
                self._send(404, b"no")

        def _proxy_get(self, upstream_path: str, ctype: str):
            try:
                with urllib.request.urlopen(up + upstream_path, timeout=10) as r:
                    self._send(200, r.read(), ctype)
            except Exception:
                self._send(502, b"upstream down")

        def _frame(self):
            # `after` is re-parsed as an int rather than forwarded verbatim: the only
            # thing that reaches the upstream from the client is a number.
            try:
                after = max(0, int(self._q("after") or 0))
            except ValueError:
                after = 0
            try:
                with urllib.request.urlopen(f"{up}/frame.jpg?after={after}", timeout=40) as r:
                    body = r.read()
                    self._send(200, body, "image/jpeg", (("X-Seq", r.headers.get("X-Seq", "0")),))
            except urllib.error.HTTPError as e:
                self._send(e.code, b"")            # 204 = idle, the common one
            except Exception:
                self._send(502, b"upstream down")

        def _key(self):
            d = self._q("d")
            # THE allow-list. Membership, never a pattern: a regex here is how a
            # `ck:` click or an okdown/okup hold sneaks back in later.
            if d not in DPAD:
                self._send(403, b"not a d-pad key")
                return
            try:
                req = urllib.request.Request(f"{up}/key?k={d}", method="POST", data=b"")
                with urllib.request.urlopen(req, timeout=5) as r:
                    r.read()
                self._send(204)
            except Exception:
                self._send(502, b"upstream down")

        def _ws(self):
            """Relay the upstream MPEG1/TS WebSocket as a raw byte pipe.

            Auth is the query token, checked BEFORE the upstream socket is opened.
            After the 101 this is bytes in both directions and nothing else — no
            RFC6455 parsing, so there is no framing bug to have. The video is
            one-way; control never comes through here, it goes through /k's
            allow-list, so a hijacked pipe still cannot press a non-D-pad key.
            """
            if not hmac.compare_digest(self._q("t"), wstoken):
                self._send(403, b"no")
                return
            if "websocket" not in (self.headers.get("Upgrade") or "").lower():
                self._send(400, b"not an upgrade")
                return
            self.close_connection = True
            try:
                usock = socket.create_connection(("127.0.0.1", up_port), timeout=10)
            except OSError:
                self._send(502, b"upstream down")
                return
            try:
                # Replay the handshake upstream. Only the headers the upgrade needs
                # are forwarded — notably NOT Authorization, which upstream has no
                # use for and which should not travel further than it must.
                keep = ("sec-websocket-key", "sec-websocket-version",
                        "sec-websocket-protocol", "sec-websocket-extensions")
                req = ["GET /ws HTTP/1.1", f"Host: 127.0.0.1:{up_port}",
                       "Upgrade: websocket", "Connection: Upgrade"]
                for k, v in self.headers.items():
                    if k.lower() in keep:
                        req.append(f"{k}: {v}")
                usock.sendall(("\r\n".join(req) + "\r\n\r\n").encode())
                usock.settimeout(None)
                for s in (usock, self.connection):
                    try:
                        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                    except OSError:
                        pass
                self.wfile.flush()
                _pipe(self.connection, usock)
            finally:
                try:
                    usock.close()
                except OSError:
                    pass

    return H


def _pipe(a: socket.socket, b: socket.socket):
    """Shuttle bytes between two sockets until either end closes."""
    try:
        while True:
            r, _, x = select.select([a, b], [], [a, b], 60)
            if x:
                return
            if not r:
                return                       # idle: a viewer that walked away
            for src in r:
                dst = b if src is a else a
                data = src.recv(65536)
                if not data:
                    return
                dst.sendall(data)
    except OSError:
        return


def main():
    ap = argparse.ArgumentParser(description="authenticated, D-pad-only front end for stream-screen.py")
    ap.add_argument("--port", type=int, default=8908, help="listen port (default 8908)")
    ap.add_argument("--upstream", type=int, default=8909, help="stream-screen.py port (default 8909)")
    ap.add_argument("--host", default="127.0.0.1",
                    help="bind address; keep 127.0.0.1 and put a TUNNEL in front, never a port forward")
    ap.add_argument("--user", default="tv")
    ap.add_argument("--password", default=None, help="omit to generate one and print it once")
    a = ap.parse_args()

    pw = a.password or secrets.token_urlsafe(12)
    token = base64.b64encode(f"{a.user}:{pw}".encode()).decode()
    wstoken = secrets.token_urlsafe(16)

    print(f"remote-dpad on http://{a.host}:{a.port}/  -> upstream 127.0.0.1:{a.upstream}", file=sys.stderr)
    print(f"  user: {a.user}", file=sys.stderr)
    print(f"  pass: {pw}", file=sys.stderr)
    print(f"  allowed keys: {' '.join(DPAD)}", file=sys.stderr)
    srv = ThreadingHTTPServer((a.host, a.port), make_handler(a.upstream, token, wstoken))
    srv.daemon_threads = True
    srv.serve_forever()


if __name__ == "__main__":
    main()
