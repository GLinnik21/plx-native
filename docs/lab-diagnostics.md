# Cloud Lab Bridge — logs out and bounded app commands in, without SSH

**Diagnostics status: BUILT and DEVICE-VERIFIED on the dev set, 2026-08-26** (against `main`
@5a8ef2ef). The whole chain —
a **physical BLUE press** on the Magic Remote → snapshot → scrub → gzip → pinned TLS POST from the
television's own libcurl → receiver → `plxnative-lab logs` — has run on the LG 49SM9000PLA. §11 is
what that did and did not prove; §12 records the separately verified public leg through the router.

**Lab Control status: BUILT and HOST-VERIFIED, not yet device-verified.** The same lab package can
hold an outbound pinned HTTPS poll, receive ordered app-input commands from `plxnative-lab send`,
dispatch them on the SDL main thread and acknowledge delivery. It adds no dependency. The remaining
proof is an `.ipk` installed on a Cloud Test Lab set and one command/ack round trip (§11).

LG Cloud Test Lab rents us physical sets on webOS/SoC combinations nobody here owns. It gives a
picture and a virtual remote, and **no console, no ssh, no stdout, no way to download a file**. We
can reproduce a bug on a k8hpp webOS 10 set and watch it happen, and the agent fixing it cannot see
one line of `plxnative-events.log`.

This is the bridge: a lab-only build presses its own log out through the internet to a receiver on
the developer's Mac and optionally holds an outbound command poll. Commands come back only as the
response to that TV-initiated request, so Cloud Test Lab needs no inbound socket. It is **ephemeral
development infrastructure** — not telemetry, not a crash service, and it does not exist in a
normal build.

---

## 1. What the codebase already gives us (all verified in code, not assumed)

Five pieces of this feature already exist, and the design is mostly wiring them together.

* **One log sink.** Every diagnostic line in the app goes through `crate::log(&str)` in `lib.rs`,
  which is also where `redact_tokens` already strips `X-Plex-Token=…`. A ring buffer tapped there
  sees *everything* the app knows and inherits the redaction that is already the shipped policy.
* **One structured state snapshot, already audited for secrets.** `player::Diag` (`player/mod.rs`,
  ~50 fields) is the exact list this ask enumerates — playback stage, load payload video/audio
  codec, video w/h, position/duration, fed bytes, feed state, `http_status`, `net_rx`, callback
  errors, the whole ABR block, the video-plane mode and `windowId`. Its every field is a number,
  bool or enum **by rule**: `ui/stats.rs`'s module doc is a written no-URL / no-credential /
  no-identity contract for exactly this data, because it is already photographed and posted into
  public issue threads. A snapshot built from `Diag` is redacted by construction.
* **Device identity.** `webos::info()` (release, codename, api, name, from `/var/run/nyx/os_info.json`),
  `devcaps::caps()` (the SoC's own codec table), `paths::app_id()`/`flavour()`,
  `env!("CARGO_PKG_VERSION")`.
* **A TLS client with runtime-bound libcurl.** `net::request` — verification on, `NOSIGNAL`, curl
  bound by SONAME candidate list so it works from webOS 4.4 to 11.2. Adding one pinning option is a
  parameter, not a new transport.
* **A thread primitive that cannot kill the app.** `task::spawn_small` (256 KB stacks; a refused
  spawn is a return value, measured against `RLIMIT_NPROC` in `tools/threadprobe.c`).

And two constraints that shape everything below:

* **No `/tmp` trigger can be armed on a lab set.** The whole `dev.rs` surface assumes ssh. On Cloud
  Test Lab the *only* channel into the device is **the .ipk we upload**. So per-session
  configuration has to ride in the package.
* **The colour buttons are UNBOUND, and until this feature nobody had measured them.**
  `docs/remote-keys.md` §2 is the full map and no colour key was in it; §6 lists them among the
  *unsupported* keys the app deliberately ignores. They are measured now — BLUE is `wcode` **489**
  (486 RED / 487 GREEN / 488 YELLOW, `sym` 0, dev set 2026-08-26) — and the trigger stayed
  configuration anyway, for the reason §7 gives: that is ONE remote on ONE firmware.

---

## 2. Topology

```
Cloud Test Lab TV  ──HTTPS POST / long poll──▶  lab.plxnative.com:39443
                   ◀── bounded command response ──┘          (same outbound connection)
                                            │
                                        Keenetic  ──UPnP IGD AddPortMapping (temporary)
                                            │
                                    Mac:<ephemeral>  ──▶  plxnative-lab  ──▶  ~/.plxnative-lab/…
                                                                                    │
                                                                          the coding agent reads
```

Fixed external port, dynamic local port, mapping created at `start` and deleted at `stop` (and on
SIGINT/SIGTERM/atexit). The app therefore needs no discovery: the endpoint is a constant of the
lab build.

---

## 3. Wire protocol — two public TV routes, both outbound POSTs

The bearer secret and `X-Plx-Session` id authenticate both routes. The per-session SPKI pin
authenticates the receiver to the television. Redirects are disabled.

### Diagnostic upload

```
POST /v1/diag HTTP/1.1
Host: lab.plxnative.com:39443
Authorization: Bearer <session secret, 32 random bytes, base64url>
X-Plx-Session: <session id, 8 hex>
Content-Type: application/x-ndjson
Content-Encoding: gzip | identity
Content-Length: <= 4 MiB, enforced both ends

<envelope line>
<record line>
…
```

Response `200 {"ok":true,"seq":N}` / `401` / `413` / `429`. Outside the declared diagnostic and
control POST routes, every path and method is a bare 404.

**Body = JSONL.** Line 1 is the envelope, lines 2..n are ring records:

```json
{"kind":"envelope","seq":3,"session":"a1b2c3d4","sent_at_ms":812433,
 "app":{"version":"0.4.1","id":"com.beb.plxnative.debug","flavour":"debug",
        "features":["lab-diagnostics","devtools"],"uptime_ms":812433},
 "device":{"webos_release":"10.3.1","codename":"…","api":"…","model":"OLED55C1",
           "board":"k8hpp","panel":"3840x2160","drawable":"1920x1080"},
 "caps":{"video":["h264","hevc","av1"],"audio":["ac3","eac3","aac"]},
 "player":{"vp_mode":"exported window (webOS 5+)","stage":6,"load_v":"H265","load_a":"AC3 PLUS",
           "video_w":3840,"video_h":2160,"pos_ns":…,"dur_ns":…,"feed_state":"BufferFull (sink is full)",
           "fed_v":…,"fed_a":…,"frames":…,"cb_err":0,"http_status":206,"net_rx":…,
           "abr":{"mode":…,"kbps":…,"buffer_ms":…,"action":…,"why":…}},
 "route":"player","dropped":0,"records":642}
{"t_ms":94112,"m":"load: v=H265 a=AC3 PLUS fps=23.976 dv=8.1 atmos=0"}
{"t_ms":94180,"m":"feed v#1 reply=Ok"}
```

`t_ms` is monotonic since process start (same clock as the heartbeat, so lines correlate with
`loop=`/`fps=`/`pos=`). `dropped` is how many records the ring evicted since the last upload — the
one number that tells the agent the window was too small.

### Lab Control long poll

```
POST /v1/control/poll HTTP/1.1
Authorization: Bearer <same session secret>
X-Plx-Session: <session id>
Content-Type: application/json

{"ack":{"id":41,"ok":true,"detail":"dispatched"}}
```

The receiver holds an idle poll for 15 seconds. It answers immediately when a command exists:

```json
{"ok":true,"command":{"id":42,"token":"down"}}
```

Only one command is in flight. Until id 42 is acknowledged, another poll receives id 42 again;
after the ack, the next queued id is returned in that same response. This is ordered and at-least-
once. Within one app process an acknowledgement is not sent until the SDL main thread accepts the
token, so a lost ack response does not double-press. A process crash between dispatch and ack may
replay that one command after relaunch; no durable writable state exists on the rented set.

The host enqueues through `/v1/control/enqueue` and reads `/v1/control/status`, but both routes
require a loopback peer address in addition to the bearer. They cannot be reached through the
router mapping even by somebody who extracted the session secret from the `.ipk`.

Playback mode (Direct Play / Direct Stream / Transcode) is **not** in `Diag` today; it is decided in
`route.rs`. MVP takes it from the log lines (`load:` and route's own lines are already in the ring);
promoting it to a `Diag` field is a follow-up, not a blocker.

---

## 4. App side — new module `rust-modules/src/lab/`, one feature `lab-diagnostics`

`Cargo.toml`: a third feature, **off by default**, alongside `devtools`/`devtriggers`. Off means
the module is not compiled: no ring, no key arm, no config read, no poll worker. `RELEASE=1` already
drops the other two; this one is never in the default set at all, so a release build cannot get it
by forgetting a flag.

| file | what it owns |
| --- | --- |
| `lab/mod.rs` | the feature boundary, boot/config gating and the main-thread control mailbox seam |
| `lab/config.rs` | reads `lab.json` **from the app directory** (`paths::in_app_dir("lab.json")`) once at boot: `{endpoint, session, secret, pin, control, trigger_wcodes[]}`. Absent or malformed → the feature is inert and says so in one log line. Missing `control` is false, so an older package remains upload-only. |
| `lab/control.rs` | one persistent pinned HTTPS long-poll worker, ordered command parsing, the main-thread mailbox, dispatch acknowledgement and bounded reconnect backoff |
| `diag/ring.rs` | the bounded buffer: `VecDeque<(u32 t_ms, String)>`, capped by **both** 4000 records and 768 KB of text, evicting oldest, counting evictions. One `Mutex`. |
| `lab/snapshot.rs` | envelope construction from `Diag` + `webos` + `devcaps` + `paths` + uptime. Pure and host-testable. |
| `diag/scrub.rs` | the **redaction pass** (§6), moved out of `lab/snapshot.rs` on 2026-08-29 when `crate::log` became a second caller. Ungated, so its assertions finally run in the default `make check` — under `lab/` they were skipped by every build that did not set the feature. Two exits: `scrub` may refuse a line, `scrub_local` may only rewrite one. |
| `diag/zlib.rs` | the one-symbol `compress2` table and the gzip envelope (§5) |
| `lab/upload.rs` | gzip (optional, §5) then one `net::post_pinned`, on a `task::spawn_small` worker. Single-flight: a second BLUE while one is in flight is refused, not queued. |

**The tap** is two lines in `lib.rs::log`, after `redact_tokens`:

```rust
let line = diag::scrub::scrub_local(m);   // was: redact_tokens(m)
lab::record(&line);                        // a no-op without the feature
```

so the ring is a strict subset of what the event log already contains — there is no second logging
system and no call site to update.

**Since 2026-08-29 the tap sits below the FULL local scrub, not just the token backstop.** The line
the ring sees has already had credentials, hosts, bare addresses, viewing identity and household
names rewritten, so the upload's own `scrub` pass is now genuinely defence in depth rather than the
first line of defence. `scrub_local` never DROPS a record — the drop belongs to the network exit —
so the ring's contents still correspond one-to-one with the file on disk.

**Sizing.** 4000 records / 768 KB is minutes, not seconds: a settled screen writes a line a second
(the heartbeat), and the noisiest thing this app does — a playback join — writes on the order of
tens of lines a second for a few seconds. It is a fixed allocation ceiling on a device whose app
budget is declared in `appinfo.json`, which is why both caps exist rather than a record count alone.

**The trigger** (`app.rs`, one arm in the global key ladder, `#[cfg]`-gated):

* BLUE (`wcode` 489, measured — §7), matched against `trigger_wcodes` **from the config** rather
  than a constant, so a lab session can try another code with a repack instead of a rebuild. That
  mattered more when the code was unknown; it still matters, because the measurement is one remote
  on one firmware.
* **A fallback that needs only the D-pad**, since a Cloud Test Lab virtual remote may not offer
  colour keys at all: a new row in `ui::account_menu` (`Action::SendDiagnostics`, lab builds only) —
  reachable by ordinary navigation on Home, and a `TableView` row is the idiom that module already
  is. The player-side twin is a row in `ui::more_menu`, beside Stats for nerds.
* `ui::consts::is_bound` must answer `true` for the lab key in a lab build, or pressing it also
  wakes the player HUD and aborts an armed click (`docs/remote-keys.md` §6 is that whole story).

**The overlay**: `Uploading diagnostics…` → `Diagnostics uploaded (142 KB)` / `Upload failed: <reason>`,
auto-dismissing after ~4 s, drawn from `theme` tokens through the existing `Label`/card widgets, and
calling `ui::idle::invalidate()` on every state change — it animates from a clock, not a spring, so
the present gate cannot see it otherwise (root `CLAUDE.md`, the `Xfade`/`Spinner` precedent).

**Threading.** `Diag` is main-thread-only by contract, so the snapshot is built on the SDL thread
(one ring clone, sub-millisecond) and *moved* into the worker. Gzip and the blocking curl call
happen on the worker. Lab Control has one 256 KB persistent worker. It blocks only in libcurl,
`wait:<ms>` or its mailbox condvar; the SDL loop drains commands before `SDL_PollEvent`, dispatches
through the same function as the remote FIFO and posts the completion. Nothing in render, pump or
feed waits on network I/O. On a firmware where `net.rs` could not install the legacy OpenSSL lock
callbacks, control disables itself rather than holding the fallback HTTPS mutex for a 15-second
poll and starving sign-in or diagnostics.

---

## 5. Transport security: pin, don't trust the world

The Mac has no CA-issued certificate and getting one for `lab.plxnative.com` means ACME plumbing,
renewals and a private key on a laptop. The cheaper and *stronger* answer for a two-party private
channel:

* `plxnative-lab start` generates a **fresh self-signed cert per session** (`openssl`, already on
  macOS) and computes its **SPKI SHA-256 pin**.
* The lab build sets `CURLOPT_PINNEDPUBLICKEY = "sha256//<base64>"` with `SSL_VERIFYPEER=0` /
  `VERIFYHOST=0`. libcurl checks the pin **independently of** VERIFYPEER (documented), and
  `CURLOPT_PINNEDPUBLICKEY` is 7.39+, against the dev set's 7.53.1 — so this reaches every firmware
  the app already claims.
* No private key ships in PlxNative; the app carries a 32-byte hash. Only that one endpoint's key
  is accepted, which is a narrower trust root than the public CA set.

`net.rs` change: the body of `request` gains an internal `pin: Option<&CStr>` parameter; the
existing `request` passes `None` (no behaviour change, one line), and the `#[cfg(feature =
"lab-diagnostics")] post_pinned(...)` wrapper serves both upload and control. Handed to the **`fw-compat-reviewer`**
before push, per the root `CLAUDE.md`, because it touches the curl seam.

**Compression** is `compress2` from libz, bound through `dynlib!` as its own one-symbol table (libz
is present wherever libcurl and OpenSSL are, and on macOS). All-or-nothing loading means a set
without it simply sends `Content-Encoding: identity` — the fallback is a header, not a failure.

**Authentication** is a bearer secret over that pinned channel, compared with a constant-time
compare on the receiver. No HMAC: it would need SHA-256 in the app (a second crypto dependency) and
buys nothing once the channel is confidential and endpoint-authenticated.

---

## 6. Redaction: three layers, none of which trusts the others

1. **At logging time** — `redact_tokens` in `lib.rs`, already shipped, already unit-tested.
2. **By construction** — the envelope is built from `Diag`, `webos::Info` and `devcaps::Caps`, all
   of which are numbers/enums/short platform strings. No field of the envelope is a URL, a path, a
   title, an account id, a server name or a `machineIdentifier`. This is `ui/stats.rs`'s rule,
   applied to a second consumer, and it is enforced the same way: the envelope is built in one
   file, so adding a field is a deliberate edit to the file that carries the rule.
3. **At snapshot time** — `snapshot::scrub` runs over every ring record before it is serialised:
   the token backstop again, plus `Authorization:` / `Cookie:` / `Set-Cookie:` / `X-Plex-Token:`
   *header-shaped* lines, `?…token=`/`password=`/`access_token=` query parameters, and any
   `plex.direct` hostname (which encodes a household's LAN address). Pure function, host-tested
   with the real shapes as fixtures — the same test style `redact_tests` already uses.

If a record cannot be scrubbed confidently it is dropped and counted, not sent. `dropped` in the
envelope covers both eviction and refusal, split into two counters.

---

## 7. The BLUE button: what is now measured, and what is still open

**Measured offline, 2026-08-26**, out of LG's own evdev→scancode table at file offset `0x92840` of
the harvested `libSDL2-2.0.so.0.4.1` (the authority `docs/remote-keys.md` §1 describes; 624 `u32`
entries, index = evdev code, value = the scancode delivered in `wcode`):

| evdev | key | fork's scancode |
| --- | --- | --- |
| 398 | `KEY_RED` | **0 — not producible** |
| 399 | `KEY_GREEN` | **504** |
| 400 | `KEY_YELLOW` | **0 — not producible** |
| 401 | `KEY_BLUE` | **0 — not producible** |

So the colour keys are **not a uniform family on this firmware**, and three of the four standard
evdev codes are dead ends. That does not mean the buttons are undeliverable: LG's own private evdev
range (402–615, which this table maps densely onto scancodes 300–506) is where every other remote
button on this set already comes from — BACK is evdev 303, PLAY is 207, the channel rocker is
402/403. Whatever the physical BLUE button emits is somewhere in that space, and **no table can say
which**: the table says what a code would translate to, never which code a button sends.

**And the hardware settled it, the same day.** Pressing all four on the dev set with the log open
gave `wcode` **486 RED / 487 GREEN / 488 YELLOW / 489 BLUE**, `sym` 0 — LG's private range as
predicted, from evdev 289–292, and matched in `wcode` like every other remote button above 300.
Note what that means for the table above: `KEY_GREEN`→504 was the ONE colour code that looked
answerable offline, and it is a code this remote never sends. The offline answer was not merely
incomplete, it was wrong. No key access policy was needed either: unlike BACK, which requires
`SDL_WEBOS_ACCESS_POLICY_KEYS_BACK` in `main.c`, the colour keys arrive unasked.

**And a lab set closed the last of it, 2026-08-27.** The question that stood here — *whether the
Cloud Test Lab virtual remote offers colour buttons at all* — is answered: it sent `wcode` **489**,
`sym` **0**, byte for byte what the dev set's Magic Remote sends, and it fired the bridge **nine
times** across three app runs on a webOS 10.3.1 set (`lab: snapshot seq=… reason=key`). The tenth
upload of that session came from the menu row (`reason=menu`), so both routes below are now
device-proven on rented hardware rather than only on the dev set. **486, 487 and 488 were never
pressed there**, so nothing says whether that remote sends RED, GREEN or YELLOW — which is why the
trigger stays a list. Full account: `docs/webos10-lab-report.md` §5. That session also delivered
`wcode` 484 (`PointerHidden`, already swallowed — `docs/remote-keys.md` §2) and `wcode` **485**,
which appears nowhere in this tree and is unbound.

**The design absorbs every outcome**, which is why none of this blocked the build:

* the trigger is a **list in `lab.json`** (`trigger_wcodes` / `trigger_syms`), so trying another
  code is a repack, not a rebuild — `plxnative-lab start --trigger 489,488` writes it. It stays a
  list now that the codes are known, because 486–489 is one remote on one firmware and a rented
  set may spell them differently or not send them at all;
* the **account-menu and player-overflow rows** reach the same upload with the D-pad alone, which
  is the path that always works;
* the app logs every press's raw 48 bytes unconditionally, and those lines are in the ring — so
  **the first successful upload by any route carries the answer**, and the feature bootstraps its
  own key binding. The recipe is `docs/remote-keys.md` §7, run through this bridge instead of over
  ssh.

The default in `plxnative-lab start` is **489**, the measured BLUE. `406` survives only as a unit
test's fixture in `lab/config.rs` — it was the original guess (the CEA-2014 / webOS web-runtime
keycode for BLUE) and it was wrong, which is the whole lesson of this section.

## 8. Receiver — `tools/plxnative-lab`, python3 stdlib only

Matches the repo's existing tool idiom (`tools/netcond.py`, `tools/stream-screen.py`): one file, no
dependencies, `--selftest`.

```
plxnative-lab start [--port 39443] [--no-upnp] [--json]
plxnative-lab status [--json]
plxnative-lab send down down ok wait:1000 diag [--timeout 30] [--no-wait]
plxnative-lab clear
plxnative-lab logs [--follow] [--since 5m] [--seq N]
plxnative-lab stop
```

`start` generates the session (id, secret, cert, pin), binds `0.0.0.0:<ephemeral>`, creates the UPnP
mapping, writes `~/.plxnative-lab/session.json` + `receiver.pid`, forks to the background and prints
one JSON object containing the credentials **and the exact `make` line to build the matching ipk**.
It refuses to start if a session is already live (one session, by design).

`status --json` is the agent's poll:

```json
{"receiver":"listening","endpoint":"lab.plxnative.com:39443","upnp":"mapped",
 "dns_matches":true,"tv":"recent upload","uploads":3,"tv_control":"connected",
 "last_poll_age_s":2,"queued":0,"inflight":null,
 "last_upload_age_s":14,"webos":"10.3.1","board":"k8hpp","model":"OLED55C1",
 "app_version":"0.4.1","session":"a1b2c3d4"}
```

`send` queues an ordered batch and waits up to 30 seconds for main-thread delivery by default.
Named keys, split `okdown`/`okup`, `wait:<ms>`, `diag`, raw `k:<sym>,<wcode>`, authored-coordinate
`ck:<x>,<y>`, test patterns and `txt:` input use the same grammar as the development FIFO. `--no-wait`
only confirms queueing; the default acknowledgement is the useful automation contract.
`clear` cancels the receiver's queued and in-flight state, so input queued while a TV is disconnected
is not redelivered on a later relaunch. It cannot recall a response the app has already received.

`logs` prints the newest snapshot's records as JSONL to stdout (gunzipped); `--follow` blocks and
emits each new upload as it lands; `--since 5m` filters by record
timestamp. Uploads are stored verbatim under `~/.plxnative-lab/uploads/NNNN-<unix>.jsonl.gz` — the
agent can also just read those.

**Hardening** (the exposed surface is the whole internet for the life of the session): the public
surface is only authenticated `POST /v1/diag` and `POST /v1/control/poll`; auth and session checks
happen before body reads. Enqueue/status additionally require a loopback source address. Bodies,
uploads, command batch/queue/session totals, token bytes, wait durations, result detail, socket time
and request concurrency are all capped. Commands are parsed into app-input tokens only: no shell,
filesystem, subprocess, arbitrary URL or webOS service call. The mapping is deleted on every exit
path; `stop` verifies deletion with `GetSpecificPortMappingEntry`.

**UPnP IGD** is SSDP `M-SEARCH` → device description → `WANIPConnection`/`WANPPPConnection` control
URL → `AddPortMapping` (1 h lease, renewed while running) / `DeletePortMapping`, plus
`GetExternalIPAddress` compared against the `lab.plxnative.com` A record — that comparison is the
only cheap check that the path actually exists before a tester spends a lab hour on it.

---

## 9. Build and deploy loop

```
plxnative-lab start                       # prints session + the make line below
make LAB=1 FLAVOR=debug ipk               # bakes pkg/lab.json into the package
   → upload pkg/com.beb.plxnative.debug_0.4.1_arm.ipk to Cloud Test Lab and install it
plxnative-lab status                       # wait for tv_control: connected
plxnative-lab send down down ok wait:1000 diag
plxnative-lab logs --follow               # the agent watches
plxnative-lab stop                        # mapping removed
```

`LAB=1` sets `RUST_FEATFLAGS += --features lab-diagnostics` and its own `RUST_TDIR=target-lab` —
the Makefile already documents that exact escape hatch for a non-standard feature set, and cargo
does not hash its output, so a shared target dir would hand back a non-lab `.a` and report it fresh.
`pkg/lab.json` reaches the payload through the **Makefile** — `LAB_FILES` is empty unless `LAB` is
set, `APP_FILES` includes it, and the `ipk` recipe's `cp $(APP_FILES) $(STAGE)/` does the rest;
`ci/mkipk.py` names it nowhere and needs no change. Its EXISTENCE is enforced earlier, by
`lab-guard`. `ci/check-package.py` gains
the inverse assertion — **a non-lab package containing `lab.json`, or a lab build on the stable app
id, is a packaging error**, which is where this class of mistake gets caught rather than in review.
`pkg/lab.json` is gitignored and added to `PRIVATE_FILES` in `.claude/hooks/outbound-guard.py`
(it holds a live secret and the developer's endpoint).

---

## 10. What was built

**In:** the feature flag, the ring + tap, the snapshot envelope from `Diag`/`webos`/`devcaps`, the
scrub pass, pinned gzip upload on a worker, the account-menu row **and** the configurable colour-key
arm, the toast, the pinned HTTPS long-poll worker + main-thread mailbox, bounded ordered command
queue/redelivery/ack, loopback-only enqueue/status/clear, `plxnative-lab
start/status/send/clear/logs/stop` with
UPnP, and `LAB=1` packaging + the two package assertions.

**Still deliberately out:** a shell or general webOS-control surface, remotely creating the
SSH-era `/tmp` boot triggers, automatic restart/relaunch, screenshot/video upload, blindly running
the existing SSH-based harness unchanged, auto-upload on crash, several concurrent sessions,
durable retry across an app restart, persisting the ring, and a web UI. Scripted navigation and log
capture work now; porting a boot-trigger-heavy test requires an explicit app-level setup command,
not pretending the old filesystem contract exists.

---

## 11. What is verified, and what is not

**Verified, all on the host, no television:**

* `make check` is green in both configurations. **Take the counts yourself** — this repository has
  rotted four written test counts already and the fifth is not going to be this one:
  `cd rust-modules && cargo +nightly test --lib -- --list | grep -c ': test'`, with and without
  `--features lab-diagnostics`. What the feature's own tests cover: the ring's two caps and its
  `dropped` delta, the `lab.json` parse and each refusal it names, the five scrub rewrites and the
  outright refusal (including the bare address and the household name the device test found), the
  document's JSONL shape with a record carrying a newline and a quote, the gzip member's CRC and
  trailers against the standard check value, the toast's single-flight flag and its expiry, and
  that the toast sits inside the safe area and clear of the stats panel.
* All four feature configurations type-check clean under `warnings = "deny"`: default,
  `--no-default-features`, `+lab-diagnostics`, and `--no-default-features +lab-diagnostics`.
* `tools/plxnative-lab selftest` — a real TLS listener with a freshly generated certificate, a real
  gzip upload accepted and stored, upload refusals, command enqueue, ordered delivery, redelivery
  before ack, advancement after ack, status, grammar refusal and stale-session refusal. Wired into
  `make check`.
* Lab Control's Rust tests cover response parsing, bounded waits and the worker↔main-thread mailbox.
  Both default and `+lab-diagnostics` configurations compile under `warnings = "deny"`; the control
  module and its persistent worker do not exist in the former.
* **The whole chain, end to end, on loopback**: `plxnative-lab start --hostname 127.0.0.1
  --no-upnp` → `make LAB=1 sim` → `k:0,406` down the remote FIFO → the app logged
  `lab: snapshot seq=1 reason=key route=login` and `lab: uploaded seq=1 4689B -> 2053B (gzip)
  status=200`, and `plxnative-lab logs` printed the envelope and 44 records. The toast was
  screenshotted reading *Diagnostics uploaded / 2 KB sent*.
* The **network path exists**: the Keenetic answers SSDP (`http://<router>:1900/ctl/IPConn`),
  reports its external address, and `lab.plxnative.com` resolves to exactly that address. Checked
  by `plxnative-lab selftest`'s closing note and by `status`'s `dns_matches`.

**Verified ON THE DEV TELEVISION** (LG 49SM9000PLA, webOS 4.10.2), 2026-08-26, under the
`tv-lock`, `make LAB=1 FLAVOR=debug deploy`:

* **A physical BLUE press on the Magic Remote produced an upload.** `lab: snapshot seq=1
  reason=key route=profiles` then `lab: uploaded seq=1 10718B -> 3510B (gzip) status=200`. A RED
  press right after it did nothing, which is the selectivity half.
* **`CURLOPT_PINNEDPUBLICKEY` works against the television's own libcurl 7.53.1/OpenSSL 1.0.2** —
  that upload is the proof, and it is the one thing no host run could show.
* **The ARM cross-build**: `make LAB=1 FLAVOR=debug` builds clean through the NDK, and
  `tools/fwcompat.py` is unchanged at OK 4.4.2 → 11.2.0 (see the LAB-ELF note below).
* **The envelope's device block is real**: `status` read webOS 4.10.2 and the board and model
  strings off the set, so `webos::device()`'s `device_info.json` parse works on hardware.
* **The toast renders on the panel**, photographed over the who's-watching screen.
* **The public leg**, via a phone off the LAN entirely — §12, which is the whole account.
* And the **`fw-compat-reviewer`** pass `net.rs`'s new option warrants has been done: it found the
  fail-open pin (`CURLOPT_PINNEDPUBLICKEY`'s return code was discarded, so a libcurl that refused
  the option would have sent the upload with neither pinning nor CA verification) and the
  per-upload `dlopen` of libz. Both fixed; the review is otherwise PASS.

**Still NOT verified:**

* **Lab Control on hardware.** The receiver protocol and Rust mailbox are host-tested, but no TV
  has yet held `/v1/control/poll`, dispatched a returned key through LG's SDL event queue and sent
  its acknowledgement. In particular, the old dev set must prove that a concurrent 15-second easy
  request behaves alongside sign-in/upload even though `net.rs` installed the required OpenSSL
  locks; the code disables control if that concurrency prerequisite is absent.

* **The `.ipk` path.** `ci/check-package.py`'s two new assertions have never executed, because no
  lab package has been built — every device run so far went through `make deploy`. That is the gap
  Cloud Test Lab actually walks through, since it installs a package rather than scp-ing a binary.
* **A Cloud Test Lab set itself**: whether its virtual remote offers a colour button at all, and
  whether that set's libcurl and CA-less pinning behave like the dev set's. The account-menu row
  exists precisely so the answer to the first does not matter. Lab Control would remove the need
  to use that virtual remote once its first command round trip is proven.

**The LAB ELF is graded by hand, and by nobody else.** `.github/workflows/ci.yml` builds and grades
the DEFAULT configuration only, so `make LAB=1`'s binary — the one that actually flies to Cloud
Test Lab, and the one a submission candidate is tested in when LAB is composed with RELEASE — never
reaches the firmware load matrix. Nothing in this feature *should* move `DT_NEEDED` (it adds no
`#[link]`, no `extern "C"` block, no crate, and touches neither `LIBS_REAL` nor the link line), but
a transitive entry is exactly the thing only a built ELF can show. Graded by hand on 2026-08-26
against `make LAB=1 FLAVOR=debug`: **15 DT_NEEDED, OK on 4.4.2 through 11.2.0**, identical to the
default build. Re-run it after any change here, or give the matrix a LAB leg.

---

## 12. The public leg, and the one prober that cannot see it

**It works.** Measured 2026-08-26, in this order, so none of it is inference:

1. `plxnative-lab start` discovered the Keenetic's IGD (`http://<router>:1900/ctl/IPConn`,
   `WANIPConnection:1`) and **`AddPortMapping` succeeded** — not the tool trusting its own return
   value: a raw `GetSpecificPortMappingEntry` read back
   `NewInternalPort <the ephemeral local port> / NewInternalClient 203.0.113.7 / NewEnabled 1 /
   NewLeaseDuration 3600 / NewPortMappingDescription plxnative-lab`.
2. The router's external address equals what `lab.plxnative.com` resolves to (`status`'s
   `dns_matches: true`).
3. **A phone on LTE — off the LAN entirely — reached the receiver**, got the expected certificate
   warning (the self-signed cert of §5, which is the thing the app pins) and then the receiver's
   bare 404, `{"ok": false, "error": "not found"}`. The receiver logged it from a **carrier IP**.
   That is the whole path: public DNS → the router → the UPnP mapping → the listener.
4. `stop` removed the mapping and `GetSpecificPortMappingEntry` then reported it gone.

**One prober is refused and it is worth knowing about rather than chasing.** An automated external
fetcher (Anthropic's, used from this session) gets `ECONNREFUSED` — a reset, not a drop — from the
same address at the same moment the phone succeeds. So something between that fetcher and the
router refuses inbound from *that* source: an ISP-side filter or a reputation/geography rule, not a
property of the mapping. **The lesson for anyone debugging this later is the one that cost a round
here: a single external prober's refusal does not mean the port is closed.** Get a second vantage
point, and read the receiver log — which is why it now prints the peer address on every request.
A private (RFC1918) address in that column means a LAN client hairpinned, which proves nothing
about the internet either way.

**Two hairpin facts, so they are not re-derived:** the dev Mac cannot reach its own mapping through
the WAN address (a host hairpinning to a mapping that points back at itself; ordinary, and not a
fault), and the television could not either — which is why the on-device verification in §11 was
run against the Mac's LAN address. Neither says anything about a client that is genuinely outside.
