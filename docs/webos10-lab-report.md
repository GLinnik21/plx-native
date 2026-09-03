# webOS 10.3.1 in the LG Cloud Test Lab — 2026-08-27

**One hour on a rented 2024 LG set, from another country, over the public internet. The app signed
in, browsed two servers, and direct-played a 25 GB 4K HEVC Dolby Vision Profile 8 + Atmos remux.
Every server transcode was refused outright by the pipeline, and the reason was one line of ours.**

This is the first time this project has had a *log* off hardware it does not own. Not the first
time the app has run on webOS 10 — `docs/webos5-port.md`'s status banner records mariotaku running
0.2.0 on "webOS 6 and 10" during the Homebrew Channel review, with the verdict *"It didn't crash
and I was able to see the media library… But I didn't get media to load."* What was missing then was
the event log. This session is that log, and it supersedes the "playback does not work on webOS 10"
half of that banner.

## 0. Provenance, and how to re-read the evidence

Ten diagnostic snapshots reached `tools/plxnative-lab` through the bridge of
`docs/lab-diagnostics.md`. Each is one `{"kind":"envelope",…}` line plus the ring's tail, so the
ring's records are re-sent on every upload: **6907 record lines, 3083 distinct records**, across
**three app runs**. `seq` restarts at 1 per relaunch, so the runs are ordered by upload number
(0001–0010), not by `seq`:

| run | uploads | app uptime covered | build | boot state |
| --- | --- | --- | --- | --- |
| 1 | 0001–0007 | 0 → 570 733 ms | **unpatched** | no session → QR sign-in |
| 2 | 0008 | 0 → 367 769 ms | **unpatched** | stored session |
| 3 | 0009–0010 | 0 → 182 864 ms | **patched** (§3.4) | stored session |

Every record carries only a monotonic `t_ms`; there is no wall clock in the ring. Two lines do carry
epoch seconds — `coldstart: place on file kind=item at=…`, written by the previous run and read by
the next — and they anchor the session: run 1's place was stamped **2026-08-27 03:10:57 MSK**,
run 2's **03:20:15 MSK**. Working backwards through each run's own uptime, the three uploaded runs
span roughly **03:02:45 → 03:24 MSK**. The rest of the booked hour left no records.

Every quoted line below is verbatim from the ring, trimmed only at the right. `<addr>`, `<host>`
and `<name>` are the scrubber's own placeholders (§4.2 is about the places it failed). Library
titles are the maintainer's and a friend's and are never named here.

The app under test: `install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug
features=dev APPID_env=com.beb.plxnative.debug`, version 0.4.1, features
`["lab-diagnostics","devtools","devtriggers"]`, installed at the Developer Mode prefix
`/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug`. Note `APPID_env=` is
populated — SAM **does** export `APPID` to a native app on this firmware, which is one free answer
the boot line was added to get.

---

## 1. The device, and the correction it forces

```
webos: webOS TV release=10.3.1 codename=ponytail-papikonda api= major=10
webos: model=k24 board=K24_DVB hw=BOARD_DV_1ST
devcaps: hevc=true 4096x2176 vp9=true audio=aac,ac3,eac3 (device table)
surface: window=1920x1080 drawable=1920x1080 panel=3840x2160 logical=1920x1080 scale=1.000
wm sdl=2.0.14 subsys=6 wl_surface=0x… wl_display=0x… alpha=8
```

The lab console listed the booking as platform **"webOS24"**, model k24, board K24_DVB, hw
BOARD_DV_1ST, UK. The records corroborate the model/board/hw triple independently, and `k24` /
`K24_DVB` are themselves a 2024 chassis name.

**So `release=10.x` is a 2024 set, and `docs/distribution.md` line 263 said otherwise.** It read
webosbrew's `webos-bridge-64to32` note — *"tested on webOS 10 and 11"* — and glossed it as
"= webOS 25 and 26". That was an inference from a release-year mapping nobody had checked against a
television. Device evidence beats inference: webOS 10 is webOS TV 24. A one-line correction has been
applied to that file citing this session. It matters because §3.4's effort estimate, the firmware
map in `docs/webos5-port.md` §2, and any future reading of a webosbrew compatibility note all hang
off which physical generation a release number names.

Two smaller corroborations of the existing firmware map, worth having because they were derived
from symbol inventories rather than from a set: **SDL is 2.0.14**, exactly as the map predicts for
10.2.0, and **ACB is gone** — `vp_mode: exported window (webOS 5+)`, `acb_ok: false` in all ten
envelopes.

One thing the records **contradict** in the booking: the lab listed the set as 1920x1080.
`SDL_webOSGetPanelResolution` reports **3840x2160**, with a 1920x1080 UI surface and drawable —
byte for byte the dev set's shape, and exactly the reason CLAUDE.md forbids using the panel number
as a layout input. Whichever of the two is "the set's resolution", the app read three numbers and
used the right one.

---

## 2. Everything that worked, and all of it for the first time

### 2.1 EGL and GLES on Valhall — and the feared regression did not happen

```
GL: Mali-G57 / OpenGL ES 3.2 v1.r51p0-00eac0.e072184f997ee3dc64473a43c8a331d4
egl vendor: ARM
egl version: 1.5 Valhall-"r51p0-00eac0"
FB bits: alpha=8 red=8 depth=0 stencil=0 (config alpha=8 depth=0 stencil=0)
egl surface: 1920x1080 (ok=1/1) swap_behavior=0x3095 BUFFER_DESTROYED (ok=1 err=0x3000)
```

`docs/distribution.md` §3.4 names, as one of the two real webOS-24 breakage reports it could find,
*"a jail-permission regression (`/dev/dma_buf_unified` became inaccessible to libmali, EGL init
failed)"*. **It did not occur.** EGL 1.5 came up, the surface was created at the first attempt
(`ok=1/1`), the 32-bit RGBA config was granted (`alpha=8` in both the framebuffer and the config,
which is the precondition for the video plane showing through), and the app presented frames
continuously for three runs.

The UI plane is a Mali-G57 (Valhall, DDK r51p0) against the dev set's Mali-T820 MP2 (Midgard,
r12p0) — three DDK generations and a different architecture — and nothing in the renderer needed to
know.

### 2.2 The video plane: `VP_EXPORTED`, sixteen times, no leak

Sixteen Load payloads across the session, each carrying a freshly acquired exported window id:

```
vplane: exported windowId=_Window_Id_12 spliced into the Load payload
vplane: exported window placed src=3840x2160 rv=1
```

**Twenty-two `placed` calls, all `rv=1`.** Placement happens twice per playback in the common case
— once at Load with whatever dimensions were known then, and again the moment `ff` parses the
container and the true raster is known:

```
148408  vplane: exported window placed src=3840x2160 rv=1        ← at Load
163843  vplane: exported window placed src=1280x640 rv=1         ← when the HLS raster landed
168729  vplane: exported window placed src=720x360 rv=1          ← and again on an ABR step
```

The ids observed are `_Window_Id_1` … `_Window_Id_12`, but **not as one monotone sweep**, and the
detail is worth recording: run 1 used 3–6, run 2 restarted at 1 and ran to 10, run 3 continued at
11–12. So the counter is **not per-process** — it survived the relaunch between runs 2 and 3 — and
it reset once, between runs 1 and 2, for a reason the records do not carry. What matters for the
port is what did *not* happen: no window was ever refused, none failed to re-acquire after a
teardown, and every one of sixteen Loads got a distinct id.

`docs/webos5-port.md` §4 item 1 was settled for webOS 6.5.2 by issue #22. It is now settled for
10.3.1 as well, on the same three log lines that report says to look for.

### 2.3 The bundled FFmpeg, on a firmware whose own FFmpeg we have no table for

```
ff: bound avutil -> libavutil-plx.so.61
ff: bound avcodec -> libavcodec-plx.so.63
ff: bound avformat -> libavformat-plx.so.63
ff: bound swscale -> libswscale-plx.so.10
ff: avformat=63.1.100 avcodec=63.1.100 avutil=61.1.100
```

This is the whole argument for bundling, on the firmware that used to be the argument against
shipping at all. `docs/webos5-port.md` §4 item 6 reads *"webOS 10.2.0 and 11.2.0 (libavformat 59/60).
The binary loads there, and `ff::boot` refuses to demux because there is no table."* That refusal is
gone; the app opened its own libraries out of its own app directory, by absolute path, and got the
exact 63/63/61 triple `boot()` demands.

The `-plx` build suffix is doing real work here and the log is where you can see it: the names in
those four lines cannot resolve to the television's copies whatever version they are. **What the
records do not say is what the set's own FFmpeg is** — the app never opens it, and the nearest
firmware inventory we hold (10.2.0) says avformat 59, i.e. FFmpeg 5.x, not 6. Nothing in this
session tests that; the point is that it no longer matters.

### 2.4 libcurl, TLS, and the bridge carrying its own weight

```
net: bound libcurl -> libcurl.so.4 (libcurl/7.82.0 OpenSSL/3.0.13 zlib/1.2.11 c-ares/1.19.0
     nghttp2/1.47.0; AsynchDNS=yes); threaded-tls=true legacy-locks=NotNeeded
curlio: bound libcurl.so.4 curl_multi_* (7 symbols)
```

The SONAME candidate list picked `.so.4` — the flip `dynlib.rs` exists for. Both tables bound: the
`net.rs` one for plex.tv and PMS control, and `curlio.rs`'s separate seven-symbol multi table,
frozen to the oldest supported set, which resolved here on a stack four major versions newer.

**And `CURLOPT_PINNEDPUBLICKEY` worked against OpenSSL 3.0.13.** `docs/lab-diagnostics.md` §11 lists
*"whether that set's libcurl and CA-less pinning behave like the dev set's"* as unverified; the only
proof it had was one upload through the dev set's libcurl **7.53.1 / OpenSSL 1.0.2**. Ten uploads
landed here, all `status=200`:

```
lab: uploaded seq=1  9810B ->  3346B (gzip) status=200
lab: uploaded seq=6 105819B -> 17936B (gzip) status=200
```

Round trip snapshot → confirmation was 1.1–1.6 s throughout, and gzip returned 2.9x on the smallest
document and 5.9x on the largest. One structural note for anyone reading these files: **the upload
confirmation for snapshot N only appears inside snapshot N+1**, because the line is written after
the document is serialised. The last upload of each run is therefore unconfirmed in-band, and
confirmed only by having arrived.

### 2.5 Sign-in, two servers, profiles, hubs

Full cold path, from nothing:

```
auth: authorized — discovering server
auth: resources n=3 servers=2
auth: reached "<name>" <addr> (ours) via https://<host>
auth: reached "<name>" <addr> (shared by <name>) via https://<host>
auth: 2 server(s) reached, primary '<name>'
auth: home users n=3
auth: showing who's-watching
auth: switch '<name>' -> ok (per-user server token)
hubs: landed — 54 items, 6 shelves
```

The registry of `rust-modules/src/plex/CLAUDE.md` held two servers with separate tokens — the
owner's (PMS 1.43.3) and a shared one (PMS 1.43.4) — and the who's-watching picker, the managed
profile switch and the per-user server token all worked. A managed profile's PIN keypad was
exercised twice (§4.2 is about what that put in the log).

`login: server installed — asking which sources feed Home` → `onboard: Home selection recorded —
3 of 3 libraries on`: the multi-source onboarding ran too.

### 2.6 Playback: 4K HEVC Dolby Vision Profile 8 + Atmos, direct play, over the public internet

The centrepiece of what *works*. A 4K DV title on the shared server, reached from another country
by `https://<host>`, `clen=25036888793` — **25.0 GB** — direct-played with no transcode:

```
route: dolby vision P8 (bl_compat=1 el=0) — declaring DolbyHdrInfo (trackType=single profileId=8); direct play
dv: sourceInfo contents.DolbyHdrInfo profileId=8 trackType=single encryptionType=clear (codec H265)
atmos: sourceInfo contents.immersive=ATMOS
esInfo: videoFps 24/1 + adaptiveResolution (src 24.000)
load: v=H265 a="AC3 PLUS" fps=24.000 dv=present:1 P8/1 el:0 atmos:1
ff: v=#0 codec=hevc codec_id=172 3840x2160 trc=2 pri=2 spc=2 dovi=P8 level=6 bl_compat=1 rpu=1 el=0 bl=1
```

Both declarations were **accepted, and echoed back by the television's own pipeline** — which is
the half that cannot be inferred:

```
smp_cb type=8 num=0 str=audio/x-eac3 … eac3 0 (null) 0 ATMOS
smp_cb type=4 num=0 str={… "width":3840,"height":2160,… "hdrType":"DolbyVision"}
```

Also on the wire and worth knowing: the file carries HDR10+ SEI alongside the DV RPU, and `ff`
stripped over 1500 of them in one sitting (`ff: stripped HDR10+ SEI #1500 (55 bytes)`).

A second item — 1920x960 HEVC / E-AC3 Atmos, 2.68 GB, on the owner's server — direct-played too,
with 42 text subtitle tracks parsed and client-rendered cues appearing on schedule.

**Seek.** Twelve `ff: seek` calls across the session, **every one `rv=0`**, each followed by a PTS
rebase and a clean prime:

```
ff: seek 502s rv=0
rebase: first post-seek keyframe pts=501667000000 -> pts_shift=-501667000000
primed: v=749ms a=477ms -> Play
```

Six of the twelve were user-driven mid-film reloads (five `reload_at: fresh Load at …`, one of them
from an explicit `scrub: tap commit 502s`); the rest were resume seeks at Load. Prime times were
tight and consistent: video 708–833 ms on direct play, audio 477–827 ms.

**App background / foreground mid-playback**, the old black-screen bug, on a firmware six releases
past where it was fixed:

```
314206  LIFECYCLE: background (playing=1)
314206  ff: media transport failed during av_read_frame r=-5
314650  stop_bufferfeed: torn down
329162  LIFECYCLE: foreground (wasPlaying=1)
329196  vplane: exported windowId=_Window_Id_9 spliced into the Load payload
335290  ff: seek 498s rv=0
337711  primed: v=749ms a=802ms -> Play
```

Suspend, teardown, a *fresh* exported window on the way back, a seek to the saved position, and
Play — 3.5 s from foreground to picture, and the position it resumed at (498 s) is within 5 s of
where it stopped (503 s).

---

## 3. The bug: a 4K sink ceiling declared for every codec

**On webOS 10.3.1 the app cannot play a server transcode at all.** Every one is H.264, and every
H.264 Load was refused by the pipeline before a byte was fed.

### 3.1 The line

`rust-modules/src/player/engine.rs:601`, in the streamed-A/V branch of `apply_plan`:

```rust
// Sink envelope = the panel max (4K) regardless of codec; the pipeline reads the
// true dims from the bitstream (SPS), so this is just a ceiling and is correct for a
// 4K stream (HEVC transcode / HEVC direct-play) AND harmless for a 1080p H264 file.
let (mw, mh) = (3840, 2160);
```

`build_av_payload` then substitutes those into `adaptiveStreaming` and forces `maxFrameRate` to 60,
so **every** streamed Load — H.264 and H.265 alike — declares a `3840 × 2160 @ 60` sink.

That comment is a hypothesis about how the pipeline treats the field, and on webOS 4.10 it holds.
On 10.3.1 it does not: the pipeline **allocates against the declared ceiling**, and no AVC decoder
on this SoC does 4K60, so the allocation is refused and the Load dies.

### 3.2 The A/B

Three facts make this airtight, and all three are in one session on one set with one server.

**The `load:` line is byte-identical on both sides**, so nothing about the item, the route, the
codec strings, the frame rate or the Dolby declaration differs:

```
load: v=H264 a="AAC" fps=0.000 dv=present:0 P0/0 el:0 atmos:0
```

**Unpatched — refused, twice, in two separate app runs.** Run 1, `t=235929`; run 2, `t=121636`:

```
237035  smp_cb type=13 num=1 str=
237035  smp_cb type=14 num=0 str=1
237036  smp_cb type=5 num=0 str=0 video/x-h264 (null) (null) 3840 2160 (null) 60.000000 0 0 0
237036  smp_cb type=15 num=0 str=1
237036  smp_cb type=8 num=0 str=audio/mpeg (null) (null) 0.000000 0 (null) 0 0 0 0 aac 0 (null) 0
237100  smp_cb type=18 num=601 str=Resource Allocation Error
237100  SMP: Load returned ok=1
```

Note `smp_cb type=5` — the pipeline echoing back the sink envelope it was handed. `3840 2160
60.000000` is our `adaptiveStreaming` block, verbatim. Note also what is **absent**: no `type=17`
`resourceList`. No VDEC and no ADEC were ever allocated.

**Control, same set, same session, same code.** Thirteen H.265 Loads declared the *same*
`3840 2160 60.000000` and every one succeeded — so the ceiling is not refused per se, only for
AVC:

```
smp_cb type=5 num=0 str=0 video/x-h265 (null) (null) 3840 2160 (null) 60.000000 0 0 0
smp_cb type=17 num=0 str={"context":"…","resourceList":[{"type":"VDEC","portNumber":0},{"type":"ADEC","portNumber":1}]}
SMP loadCompleted
```

Two of those H.265 items reached the envelope's frame counter at **197 frames** (the 1920x960 item,
upload 0005) and **324 frames** (the 4K DV item, upload 0007).

**Patched — accepted, 117 ms later.** Run 3, `t=148291`, same `load:` line:

```
148291  load: v=H264 a="AAC" fps=0.000 dv=present:0 P0/0 el:0 atmos:0
148292  SMP: calling Load (uid=NULL)
148322  smp_cb type=5 num=0 str=0 video/x-h264 (null) (null) 1920 1080 (null) 60.000000 0 0 0
148395  smp_cb type=17 num=0 str={"context":"…","resourceList":[{"type":"VDEC","portNumber":2},{"type":"ADEC","portNumber":1}]}
148400  SMP: Load returned ok=1
148408  SMP loadCompleted
```

…and then it actually played, through the HLS pipeline and the adaptive controller, stepping down
twice on a link that could not hold the top rung:

```
163828  hls: segment=979 bytes=605736 raster=1280x640 … first_au_ms=2000 total_ms=2010
168725  abr: committed Down to 2000kbps 1280x720
168732  primed: v=1999ms a=2069ms -> Play
169331  timeline playing t=1959s/3705s
176090  abr: committed Down to 720kbps 854x480
```

**And the patch does not disturb HEVC.** The same run 3 direct-played the 4K DV P8 + Atmos title
before it (`_Window_Id_11`, `hdrType":"DolbyVision"`, `timeline playing t=804s/7928s` after 13
minutes of continuous play).

The three `smp_cb type=5` shapes in the whole session, in one census:

| echoed sink | count | outcome |
| --- | --- | --- |
| `video/x-h264 … 3840 2160 … 60.000000` | 2 | **`type=18 num=601 Resource Allocation Error`** |
| `video/x-h264 … 1920 1080 … 60.000000` | 1 | `SMP loadCompleted` |
| `video/x-h265 … 3840 2160 … 60.000000` | 13 | `SMP loadCompleted` |

### 3.3 Why four years of dev-set testing never saw it

The dev set is webOS 4.10 and its pipeline evidently treats `adaptiveStreaming` as advisory — the
comment at engine.rs:601 was written from that behaviour and is *true there*. It is exactly the
class of assumption `docs/webos5-port.md` §4 exists to enumerate, and it was not on the list,
because nothing in a symbol inventory can express "how does this firmware interpret a JSON field's
value". `tools/fwcompat.py` grades whether the binary starts; this is a payload semantics change,
invisible to it by construction.

The blast radius is total on this firmware and zero on the dev set: **every** PMS transcode target
that is not HEVC is H.264, so on webOS 10.3.1 the app direct-plays whatever it can and shows a
black screen for everything else — which is most libraries, most of the time.

### 3.4 `1920x1080` is a demonstration, not the fix

The patched build carried:

```rust
let (mw, mh) = if vc == "H265" { (3840, 2160) } else { (1920, 1080) };
```

**That is wrong as a final value** and it was written to prove the mechanism, not to ship. It
under-declares a genuine 4K H.264 file — which this session **did not test**, because the library
in reach had none and a transcode never produces one.

**The patch has been reverted in this worktree at the maintainer's request.** It is not the ABR bug
and it is not covered by the ABR work: both `origin/abr-canonical-plant` and
`origin/abr-i0-instrumentation` still carry `let (mw, mh) = (3840, 2160);` at `engine.rs:601`.

The obvious principled fix is to declare a **per-codec** ceiling from the television's own capability
table, which `rust-modules/src/devcaps.rs` already parses and then throws away: `parse()` builds
`h264_wh` and `hevc_wh` separately (`devcaps.rs:170`, and the two `match` arms at 177 and 181), and
then folds them into one combined bound at `devcaps.rs:197` —

```rust
// The bound BOTH consumers apply to every codec at once — the per-axis MIN across the two
// decoders' merged rows, so neither decoder is over-claimed (see the field doc).
let hevc_max = (min_nz(h264_wh.0, hevc_wh.0), min_nz(h264_wh.1, hevc_wh.1));
```

**But keeping the per-codec width and height alone would not have fixed this session's failure, and
the records say so.** This set's own table yields `devcaps: hevc=true 4096x2176 …` — and since that
number is the *minimum* across both decoders' rows, the table claims **the H.264 decoder also does
at least 4096x2176**. Width and height were never the binding constraint. The discriminator is
almost certainly the frame rate, and `devcaps.rs` ignores it *by design*:

```rust
/// The table's shape, structurally: unknown fields (maxFrameRate, maxBitRate, channels, the
/// license blurb) are ignored by serde …
```

So the real fix is a per-codec `(maxWidth, maxHeight, maxFrameRate)` triple read out of
`/etc/umediaserver/device_codec_capability_config.json` and declared per Load — which also closes
the gap CLAUDE.md already names, that the profile sent to PMS bounds no frame rate at all. That is
bench work with the pipeline test tier behind it (`pipe_h264_*` covers all four rasters SD→UHD, and
`pipe_h264_1080p5994` is the one fixture that exercises a non-integer rate), not something to land
off a lab session.

### 3.5 What the app did *not* do about the refusal — a second defect in the same place

`type=18` is latched for diagnostics and nothing else. The seq=3 envelope, taken 49 s after the
refusal, is a textbook symptom signature:

```json
"stage":0, "load_completed":false, "load_failed":false, "cb_count":6, "cb_err":18, "cb_err_at":6,
"feed_state":"— nothing fed yet", "fed_v":0, "fed_a":0,
"aq_video":1144106, "aq_audio":985039, "frames":0, "seen_frame":false,
"http_status":200, "net_rx":2626924
```

`load_failed` is **false** — because `Load()` returned `ok=1` and the refusal arrived
asynchronously by callback. So the player state machine believes it is mid-load, the demuxer keeps
downloading (2.6 MB in by then), the AU queues fill to their caps and stall on backpressure, the
adaptive controller keeps stepping down against an estimator that will never see a frame, and
**nothing is on screen and no failure read-out is reached**. In run 1 that state persisted for
about **70 s** of HLS segment fetching and ABR stepping (down to `320kbps 426x240`) before the user
intervened; in run 2, about 12 s.

`player::error_shape` and the full-screen failure read-out — the one screen designed to survive a
phone photograph in an issue thread — never ran, on the failure they most exist for. Whatever the
ceiling fix turns out to be, `type=18` needs to become a *verdict*.

---

## 4. Two more defects, found and not fixed

### 4.1 Remote cold start burns 13–21 s probing LAN addresses that cannot work

This is precisely the reviewer-with-no-PMS-on-their-LAN scenario `curlio.rs` was written for, and
the *transport* half is solved while the *discovery* half is not. Every server is probed at its
LAN candidates first, serially, at roughly 4.5–5 s per candidate, before falling back to the
public `https://<host>` URI that works:

Run 1, from `auth: authorized` to both servers reachable — **15 977 ms**:

```
151247  auth: authorized — discovering server
152625  plex: server slot 0 registered at <addr>
157208  auth: '<name>' probe timed out at <addr>
158223  plex: server slot 0 re-pointed to https://<host>
162319  stream: GET /identity DNS FAILED host=…
167223  auth: '<name>' probe timed out at <addr>
167224  auth: 2 server(s) reached, primary '<name>'
```

Run 2 was worse — server 0 had **two** dead candidates (an IPv4 and an IPv6), so it paid the
timeout twice, and both servers were only on `https` at **21 361 ms**, with Home landing at
27 376 ms. Run 3 was luckier at **13 416 ms** and Home at 16 338 ms.

So: **13–21 s of dead air on every cold start from off-LAN**, scaling with the number of dead
candidates the server advertises, and entirely spent on connections that cannot succeed from
another country. The failure modes visible in the log are `net: curl rc=7 — transport error` (fast),
`rc=28 — timed out` (the ~5 s one) and `rc=6 — could not resolve host`. The obvious shapes of fix —
probing candidates concurrently, remembering which one won, deprioritising RFC1918 literals when
the last success was a relay — are all cheap; none is done.

### 4.2 Redaction: two leaks and a household secret

`docs/lab-diagnostics.md` §6 promises three layers. The device found three holes, in increasing
order of severity, and **none of the offending values appears anywhere in this document** — printing
what a redactor missed is the same leak by a shorter route, which is why
`.claude/hooks/outbound-guard.py` never prints what it matched either.

**(a) A bare hostname, three times.** `rust-modules/src/stream.rs:673` logs a resolver failure as
`stream: GET /identity DNS FAILED host=<the hostname verbatim>`. It appeared once per run — three
of the ten uploads carry a household's PMS hostname in the clear. The scrubber cannot see it:
`scrub_authority` only rewrites what follows `://`, `scrub_addresses` only matches numeric
addresses, and the hostname is a custom domain rather than a `plex.direct` name, so the one
hostname special-case does not apply either. **Strictly, §6 does not promise to catch this** — its
layers are tokens, credential headers, credential query params, `plex.direct`, `scheme://host`,
bare addresses, and known household identities. A bare hostname in a `host=` field is outside every
one of them. That makes it a design gap rather than a broken guarantee, and the call site's own
comment shows how it got there:

```rust
// The host is on the line because "which name failed to resolve" is the only
// question this failure raises, and it is not a secret the way a query string is
```

True of an event log read over ssh on the household's own LAN. False the moment the same ring is
gzipped and posted across the public internet, which is what the bridge does.

**(b) An IPv6 literal, once, past a scrubber that documents itself as catching it.** One
`auth: '<name>' probe timed out at …` line carries a full global-scope IPv6 address with its port —
the profile name beside it correctly replaced by `<name>`, so the identity layer ran and the address
layer did not. `scrub_addresses`'s doc comment reads:

```rust
/// **A BARE ADDRESS, outside any URL** — `203.0.113.7:32400`, `10.0.0.2`, an IPv6 literal.
```

The implementation accepts only dotted-quad IPv4: it scans digits and dots, requires exactly three
dots and four octets ≤ 255, and never looks at hex groups or colons. **This one is a broken promise,
not a gap**, and it is a household's LAN — well, WAN — topology in an upload. It is also directly
testable on the host, which is where the fix belongs.

**(c) A managed profile's PIN, in the clear, twice.** The one the maintainer did not raise, and the
most sensitive of the three. The app logs every key event's raw 48 bytes unconditionally — which is
deliberate and valuable, and is how §5 below answers the colour-button question for free. But the
PIN keypad is a key sequence like any other. In run 1 the ring holds four digit presses at
`t=177411…178947`, immediately before `auth: switch '<name>' -> ok (per-user server token)`; run 3
holds the **same four digits in the same order** at `t=9320…11121`, before the same line. Anyone
holding those two uploads holds the profile's PIN.

The fix is not to stop logging keys. It is for `snapshot::scrub` to blank the `raw=` payload of a
key record while the app is on the PIN route — or, more robustly, to blank the sym/wcode fields of
any key event whose sym is an ASCII digit, since a remote's digit keys carry no diagnostic value
that the press count does not. Either is a pure-function change with a host test, which is how the
other five rewrites in that file are already built.

### 4.3 "Original recovery" failed both times it was asked, and neither failure surfaced

Twice in the session the user picked Original from the quality menu while a transcode was running —
`route.rs:1396`'s explicitly user-initiated path, not an autonomous controller decision. Both
restores failed, differently, and both ended the playback with no error on screen.

**Run 1 — the server refused the native part.** The adaptive controller had already probed the
Original eleven times, each answered `curlio: status=503`:

```
abr: checking actual Original in parallel with HLS
curlio: status=503
abr: Original probe #11 measured=0kbps 0KiB/0ms complete=0 left=1819s verdict=Some(Insufficient)
```

The user picked Original anyway; the reload was accepted by the pipeline and then:

```
305424  quality: Original picked — restoring the native source
305490  load: v=H265 a="AC3 PLUS" fps=23.976 dv=present:0 P0/0 el:0 atmos:1
305777  SMP loadCompleted
305778  SMP loadCompleted (priming before Play)
306562  curlio: status=503
306565  ff: https open FAILED: Status(503)
306565  ff: demux produced no access units — treating as a failure
306565  ff: demux ended
```

Then **32 seconds of nothing** — primed, fed zero bytes, no read-out — until the user pressed BACK
at `t=338435` and the app tore down. The seq=4 envelope caught the state exactly:
`"stage":3,"load_completed":true,"feed_state":"queue empty (no data)","http_status":503,"net_rx":0`.

Worth flagging as a hypothesis rather than a finding: the same URL direct-played fine (HTTP 200)
from a fresh entry minutes later in runs 2 and 3, so the 503 looks specific to fetching the
Original **while a transcode session for the same item is live** on that server — a server-side
behaviour, not ours.

**Run 2 — a range request into the container's tail was aborted.** Same pick, a clean
`ff: open https status=200 clen=2675640695`, container parsed, and then a seek to byte
**2 674 506 870** — within 1.1 MB of the end of the file, i.e. the tail index rather than the
1947 s content position — came back `Aborted`:

```
135877  curlio: seek to 2674506870 failed: Aborted
135877  ff: seek 1947s rv=0
135879  ff: demux produced no access units — treating as a failure
136817  timeline stopped t=1947s/3704s ok=1
```

Note `ff: seek … rv=0` reporting success on a seek whose transport had already aborted — the
disagreement between those two adjacent lines is itself worth a look.

The server-side half of run 1 is not ours to fix. **The client half is: two different failure modes,
two dead playbacks, zero failure read-outs.** Same root as §3.5.

---

## 5. The BLUE button — `docs/lab-diagnostics.md` §7's open question is closed

§7 ends: *"One question is still open and only a lab set can close it: **whether the Cloud Test Lab
virtual remote offers colour buttons at all.**"*

**It does.** The lab's virtual remote sent `wcode` **489**, `sym` **0** — byte for byte what the dev
set's Magic Remote sends for BLUE, measured 2026-08-26 — and it fired the bridge nine times:

```
lab: armed session=… endpoint=… triggers=[486, 487, 488, 489] ring=4000rec/768KiB
lab: snapshot seq=1 reason=key route=login
```

Nine `489` presses across three runs, nine `reason=key` snapshots. The tenth upload came from the
other route, `reason=menu` — so **both** of §7's paths are now device-proven on a lab set: the
colour key and the D-pad-reachable menu row.

What is **not** closed: 486, 487 and 488 were never pressed, so nothing here says whether this
remote sends RED, GREEN or YELLOW. The trigger stays a list.

**Two corrections to the wcodes the session turned up.** The remote also delivered `wcode` **484**
(73 presses) and **485** (53 presses), and these are *not* unmapped mystery codes:

* **484 is already documented and already handled.** `docs/remote-keys.md` §2 "Swallowed" lists it
  as `PointerHidden` — *"the Magic Remote reporting its pointer auto-hid… evdev 614, an LG-private
  code and not a key"* — consumed by an arm with an empty body. So it matches something in our
  ladder, and the fact that it arrives at all on a virtual remote is mildly interesting in itself.
* **485 appears nowhere in the tree** — not in `ui/consts.rs`, not in `remote-keys.md`. It is
  genuinely unbound. Given it interleaves with 484 throughout, `PointerShown` is the obvious guess
  and there is no evidence for it here.

One thing the records cannot settle: 484 and 485 arrive irregularly across all three runs, including
at `t=1089` before the app had reached a screen. The ring says a key event happened; it cannot say
whether a human pressed it or the remote emitted it unbidden.

**§7 has been updated in this change**, to one paragraph: the question is closed for BLUE on this
lab set, 486–488 remain untested, and the menu route was exercised too. Nothing more — the
measurement is still one virtual remote on one firmware.

---

## 6. Two things the records answer that nobody asked

### 6.1 The partial-update / damage direction is *closed* on the dev set and *open* here

`docs/egl-partial-update-and-damage.md` closes the whole direction on the dev television's own
strings. Set the two boot logs side by side:

| | dev set (Mali-T820, webOS 4.10.0) | lab set (Mali-G57, webOS 10.3.1) |
| --- | --- | --- |
| EGL | 1.4 Midgard `r12p0` | **1.5 Valhall `r51p0`** |
| `EGL_KHR_partial_update` | absent | **advertised** |
| `EGL_KHR_swap_buffers_with_damage` | absent | **advertised** |
| `EGL_EXT_swap_buffers_with_damage` | absent | **advertised** |
| config `surface_type` | `0x0007` | `0x0405` |
| `SWAP_BEHAVIOR_PRESERVED_BIT` | **0** (and `eglSurfaceAttrib` → `EGL_BAD_MATCH`) | **1** |
| `buffer_age` after 120 presents | 2 | **3** |

That document's verdict — *"On the extension string alone the whole direction is closed"* — is
correct **for the dev set and only for it**. On webOS 10 hardware the extension is real, the
preserved-swap bit is on the config, and a damage implementation would have to union **three**
frames rather than two. Nothing here says the optimisation is worth doing; it says the reason it was
rejected does not generalise, and that file's scope line should say so.

Also advertised here and absent from the dev set's string: `EGL_startfish_surface_LG` (sic),
`EGL_EXT_image_dma_buf_import`, `EGL_ANDROID_native_fence_sync`, `EGL_KHR_no_config_context`. The GL
extension string is ~100 entries including the full ES 3.2 pack, ASTC, `GL_EXT_shader_framebuffer_fetch`,
`GL_KHR_debug`, `GL_EXT_buffer_storage` and `GL_EXT_disjoint_timer_query` — so the frame profiler
would work here too.

### 6.2 The heartbeat reports two frame rates, and one of them is not 60

Across 628 player-route heartbeats the `fps=` field clusters in two bands, not one:

| run | `fps` ≥ 90 | `fps` 55–65 | other |
| --- | --- | --- | --- |
| 1 | 69 | 199 | 5 |
| 2 | 136 | 58 | 12 |
| 3 | 127 | 18 | 4 |

`loop=` tracks `fps=` in the high band (`loop=96 route=player overlay=none pos=505s vtick=5
vgap=201ms fps=97`), so the loop itself is running at ~96 Hz and presenting every iteration — the
compositor is not holding it to 60 the way the dev set's frame callback does. The high band lines up
with the 4K DV item being on the video plane and its pre-roll; the ~60 band with the UI and with the
HLS/1080p legs. A brief high-band burst also appears on the **detail** route in run 3 at
`t=17377–19387` with nothing on the video plane at all, so "DV forces 96 Hz" does not fully explain
it either.

**A 96 Hz mode for 24p Dolby Vision is a plausible reading and this is not evidence for it.** What
is measured is that the UI presents at ~96 fps for long stretches on this set, which means:

* "steady 60 fps" is right for the UI and **wrong as a summary of the session** — playback rarely
  sat *below* 60 (21 of the 628 player heartbeats fall outside both bands, most of them at a Load
  boundary) but frequently ran well above it;
* every `fps_floor` / `fps_ceiling` gate in `tests/manifest.json` is calibrated against a 60 Hz
  present cadence, and an `fps_ceiling` in particular would need re-reading on a set that idles
  differently;
* `vgap=201ms` is present and flat throughout, exactly as the `[[silent-instrument-trap]]` note
  says it will be. It is a 5 Hz position callback and says nothing about frames.

The present gate itself works: settled UI screens report `fps=0`, `fps=1`, `fps=2` (63 samples at 0
and 70 at 1 across home and detail) while `loop=` stays in the sixties.

---

## 7. The `ff: media transport failed during av_read_frame r=-5` count, both ways

Seven occurrences, and the honest answer is that **all seven have a trigger** — but only five have
the trigger the shorthand looks for.

| trigger, in the same millisecond | count |
| --- | --- |
| `reload_at: fresh Load at …` (a seek or a quality reload) | 4 |
| `scrub: tap commit …` + `reload_at:` | 1 |
| `LIFECYCLE: background (playing=1)` | 1 |
| **a BACK keypress** (`wcode` 482), then `ff: demux ended` 5 ms later | 2 |

Counting only `reload_at` and `LIFECYCLE`, two occurrences look unexplained. They are not: run 2
`t=345896` and run 3 `t=134370` are each preceded, in the same millisecond, by
`[t] key type=0x300 raw=…` decoding to `state=1 wcode=482 sym=0` — the remote's BACK — and followed
by the normal teardown chain. So **zero unexplained occurrences**, and `r=-5` in this session is
entirely teardown aborting an in-flight read, which is expected noise. Reported both ways because
the distinction is the kind that gets asserted away and then relied on.

---

## 8. What is NOT proven

**One set.** One SoC (Mali-G57), one firmware (10.3.1, `ponytail-papikonda`), one chassis (k24 /
K24_DVB). Nothing here transfers to 10.2.0, to 11.x, or to another 2024 panel without repeating it.
`tools/fwcompat.py` still says the binary *starts* on 4.4.2 → 11.2.0; this session says one of those
fourteen also *plays*.

**Nobody saw the picture.** Every claim about playback here is from the app's log and from the
television's own `sourceInfo` echo. `hdrType":"DolbyVision"` is the *pipeline reporting what it
configured*, not a photograph of a panel in Dolby Vision mode. No capture, no screenshot, no human
eye is in this evidence. The same goes for audio: `contents.immersive=ATMOS` was accepted and echoed;
whether anything came out of the speakers is unknown.

**One library, and a narrow one.** Two items carried essentially all of the playback: a 4K HEVC DV
P8 + Atmos remux and a 1920x960 HEVC/E-AC3 title. Not tested at all: any 4K H.264 file (which is
what §3.4 turns on), HDR10 or HLG, DV Profile 5 or 7, TrueHD, image subtitles, AV1, live TV, or any
item over 8 GB other than the one 25 GB remux.

**One link, and it was the bottleneck.** The Original probe measured **5402 kbps** (run 2) and
**5508 kbps** (run 3) against a **5773 kbps** source, so Auto chose a transcode by a ~6 % margin
every time — which is why transcode-vs-direct-play got exercised at all. The controller's own
in-run estimator then reported `abr_net_kbps` of **1445** and **1965**, and the HLS legs settled at
`320kbps 426x240` and `720kbps 854x480`. On a fatter link the H.264 ceiling bug might never have
been hit in this session, and on a thinner one the DV direct play would not have run. **Both halves
of the session's most interesting result are artefacts of one particular link speed.**

**One patch, one leg.** The patched build ran for 183 seconds and played two items. It is not a
regression run: `./tests/run.py` was never executed against it, on either tier.

**Not answered by anything here:**

* whether declaring `maxFrameRate` below 60 would have sufficed on its own, with the 4K raster kept
  — the single experiment that would most cheaply pin the mechanism, and it was not run;
* whether a 4K H.264 file plays at all on this set, at any declared ceiling;
* what the set's own FFmpeg is (§2.3);
* whether RED / GREEN / YELLOW exist on the lab's virtual remote (§5);
* whether `type=18` is the only refusal shape, or one of several this firmware can return;
* whether the `_Window_Id_` counter's reset between runs 1 and 2 means anything.

**One thing the records suggest is settled and cannot themselves prove.**
`docs/lab-diagnostics.md` §11 lists *"The `.ipk` path"* as never exercised, since every device run
so far went through `make deploy`. A Cloud Test Lab set has no ssh and takes a package, and this
build reported `appdir: /media/developer/apps/usr/palm/applications/com.beb.plxnative.debug (from
current_exe)` — the Developer Mode prefix, which is where `appinstalld` lands a dev-mode `.ipk` as
well as where `scp` would put one. The install method is not in the ring. If the binary got there
as `make LAB=1 FLAVOR=debug ipk`, that gap is closed and §11 should say so; the maintainer knows
which it was.

---

## 9. Actions this session generates

1. **DONE, 2026-09-03 — see `docs/webos10-resource-allocation.md`'s status banner and
   `engine::sink_envelope`.** ~~Fix the sink ceiling per codec~~ (§3.1–§3.4). Read `(maxWidth, maxHeight, maxFrameRate)` per
   codec in `devcaps.rs` and declare them per Load. Bench work, pipeline tier behind it. Blocks any
   claim that the app works on webOS 10.
2. **DONE, 2026-09-03 — `player::sf_on_event_inner` publishes `load_failed` on a `type=18` seen
   before any picture.** ~~Make `type=18` a verdict~~ (§3.5). Today it is latched into a diagnostic counter and nothing
   else, so a refused Load presents as a silent black screen with a healthy-looking state machine.
3. **Fix the two redaction holes and blank PIN keystrokes** (§4.2). All three are pure functions
   with host tests; `scrub_addresses` in particular documents an IPv6 case it does not implement.
4. **Cut the off-LAN cold start** (§4.1). 13–21 s of serial probing of unreachable candidates, on
   exactly the path a reviewer takes.
5. **Correct `docs/distribution.md` line 263** — done, in this change. Only **webOS 10 = webOS TV
   24** is device evidence; that 11 is then 25 follows from the same mapping and remains inference,
   and the edit says so.
6. **Update `docs/lab-diagnostics.md`** — §7 done, in this change: BLUE closed on a lab set,
   486–488 untested. **§11's "Still NOT verified" list also needs a pass**: its second bullet
   ("whether that set's libcurl and CA-less pinning behave like the dev set's") is answered by ten
   `status=200` uploads through libcurl 7.82.0 / OpenSSL 3.0.13, and its first bullet (the `.ipk`
   path) may be answered too — see the last note in §8.
7. **Re-scope `docs/egl-partial-update-and-damage.md`** (§6.1) to say its verdict is a dev-set
   verdict.
8. **Supersede `docs/webos5-port.md`'s status banner** for webOS 10: it runs *and* it plays, with
   the one bug in §3 standing between it and a normal library.
