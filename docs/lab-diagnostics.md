# Lab Diagnostics — getting the app's log off a television we cannot ssh into

**Status: BUILT and DEVICE-VERIFIED on the dev set, 2026-08-26** (against `main` @5a8ef2ef). The whole chain —
a **physical BLUE press** on the Magic Remote → snapshot → scrub → gzip → pinned TLS POST from the
television's own libcurl → receiver → `plxnative-lab logs` — has run on the LG 49SM9000PLA. §11 is
what that did and did not prove; the **public** leg through the router is the one part that does
not work today, and §12 says why.

LG Cloud Test Lab rents us physical sets on webOS/SoC combinations nobody here owns. It gives a
picture and a virtual remote, and **no console, no ssh, no stdout, no way to download a file**. We
can reproduce a bug on a k8hpp webOS 10 set and watch it happen, and the agent fixing it cannot see
one line of `plxnative-events.log`.

This is the bridge: a lab-only build presses its own log out through the internet to a receiver on
the developer's Mac, triggered by a button on the rented remote. It is **ephemeral development
infrastructure** — not telemetry, not a crash service, and it does not exist in a build anybody
installs.

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
* **The colour buttons are UNBOUND AND UNMEASURED.** `docs/remote-keys.md` §2 is the full map and
  BLUE is not in it; §6 lists the colour buttons among the *unsupported* keys the app deliberately
  ignores. Nobody has ever captured what wcode BLUE sends on this fork, and it is not derivable
  from a desk — see §7 below.

---

## 2. Topology

```
Cloud Test Lab TV  ──HTTPS POST──▶  lab.plxnative.com:39443   (DNS-only A → static IPv4)
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

## 3. Wire protocol — one endpoint, one method

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

Response `200 {"ok":true,"seq":N}` / `401` / `413` / `429`. Nothing else is routed; every other
path and method is a bare 404 with no body.

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

Playback mode (Direct Play / Direct Stream / Transcode) is **not** in `Diag` today; it is decided in
`route.rs`. MVP takes it from the log lines (`load:` and route's own lines are already in the ring);
promoting it to a `Diag` field is a follow-up, not a blocker.

---

## 4. App side — new module `rust-modules/src/lab/`, one feature `lab-diagnostics`

`Cargo.toml`: a third feature, **off by default**, alongside `devtools`/`devtriggers`. Off means
the module is not compiled: no ring, no key arm, no config read, no socket. `RELEASE=1` already
drops the other two; this one is never in the default set at all, so a release build cannot get it
by forgetting a flag.

| file | what it owns |
| --- | --- |
| `lab/mod.rs` | the feature's doc (why it exists, what may never enter it), `enabled()`, `boot()` |
| `lab/config.rs` | reads `lab.json` **from the app directory** (`paths::in_app_dir("lab.json")`) once at boot: `{endpoint, session, secret, pin, trigger_wcodes[]}`. Absent or malformed → the feature is inert and says so in one log line. |
| `lab/ring.rs` | the bounded buffer: `VecDeque<(u32 t_ms, String)>`, capped by **both** 4000 records and 768 KB of text, evicting oldest, counting evictions. One `Mutex`. |
| `lab/snapshot.rs` | envelope construction from `Diag` + `webos` + `devcaps` + `paths` + uptime, and the **second redaction pass** (§6). Pure and host-testable. |
| `lab/upload.rs` | gzip (optional, §5) then one `net::post_pinned`, on a `task::spawn_small` worker. Single-flight: a second BLUE while one is in flight is refused, not queued. |

**The tap** is two lines in `lib.rs::log`, after `redact_tokens`:

```rust
let line = redact_tokens(m);
#[cfg(feature = "lab-diagnostics")]
crate::lab::ring::record(&line);
```

so the ring is a strict subset of what the event log already contains — there is no second logging
system and no call site to update.

**Sizing.** 4000 records / 768 KB is minutes, not seconds: a settled screen writes a line a second
(the heartbeat), and the noisiest thing this app does — a playback join — writes on the order of
tens of lines a second for a few seconds. It is a fixed allocation ceiling on a device whose app
budget is declared in `appinfo.json`, which is why both caps exist rather than a record count alone.

**The trigger** (`app.rs`, one arm in the global key ladder, `#[cfg]`-gated):

* BLUE, matched against `trigger_wcodes` **from the config** rather than a constant — because we do
  not yet know the code (§7) and a lab session must be able to try a list without a rebuild.
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
happen on the worker. Nothing in the render, pump or feed path waits on any of it.

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
existing `request` passes `None` (no behaviour change, one line), and a `#[cfg(feature =
"lab-diagnostics")] post_pinned(...)` is the second caller. Handed to the **`fw-compat-reviewer`**
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

Two further facts only hardware settles: whether webOS's key access policy delivers colour keys to
a native app at all (BACK needs `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK`, set in `main.c`), and whether
the Cloud Test Lab virtual remote even has the buttons.

**The design absorbs every outcome**, which is why none of this blocked the build:

* the trigger is a **list in `lab.json`** (`trigger_wcodes` / `trigger_syms`), so trying another
  code is a repack, not a rebuild — and `plxnative-lab start --trigger 406,504,403` writes it;
* the **account-menu and player-overflow rows** reach the same upload with the D-pad alone, which
  is the path that always works;
* the app logs every press's raw 48 bytes unconditionally, and those lines are in the ring — so
  **the first successful upload by any route carries the answer**, and the feature bootstraps its
  own key binding. The recipe is `docs/remote-keys.md` §7, run through this bridge instead of over
  ssh.

The default in `plxnative-lab start` is `406`, which is a guess (it is the CEA-2014 / webOS
web-runtime keycode for BLUE, and 406 is producible on this fork from evdev 491) and is documented
as one.

## 8. Receiver — `tools/plxnative-lab`, python3 stdlib only

Matches the repo's existing tool idiom (`tools/netcond.py`, `tools/stream-screen.py`): one file, no
dependencies, `--selftest`.

```
plxnative-lab start [--port 39443] [--no-upnp] [--json]
plxnative-lab status [--json]
plxnative-lab logs [--follow] [--since 5m] [--seq N] [--raw]
plxnative-lab stop
```

`start` generates the session (id, secret, cert, pin), binds `0.0.0.0:<ephemeral>`, creates the UPnP
mapping, writes `~/.plxnative-lab/session.json` + `receiver.pid`, forks to the background and prints
one JSON object containing the credentials **and the exact `make` line to build the matching ipk**.
It refuses to start if a session is already live (one session, by design).

`status --json` is the agent's poll:

```json
{"receiver":"listening","endpoint":"lab.plxnative.com:39443","upnp":"mapped",
 "external_ip_matches_dns":true,"tv":"recent upload","uploads":3,
 "last_upload_age_s":14,"webos":"10.3.1","board":"k8hpp","model":"OLED55C1",
 "app_version":"0.4.1","session":"a1b2c3d4"}
```

`logs` prints the newest snapshot's records as JSONL to stdout (gunzipped, `--raw` for the file as
received); `--follow` blocks and emits each new upload as it lands; `--since 5m` filters by record
timestamp. Uploads are stored verbatim under `~/.plxnative-lab/uploads/NNNN-<unix>.jsonl.gz` — the
agent can also just read those.

**Hardening** (the exposed surface is the whole internet for the life of the session):
one route (`POST /v1/diag`), bearer required before the body is read, `Content-Length` required and
capped at 4 MiB with the read bounded independently, 20 s socket timeout, ≥2 s between accepted
uploads and 60 per session, connection cap, no filesystem serving, no subprocess, no remote control
of any kind, and the whole server is ~200 lines it is practical to read end to end. Mapping deleted
on every exit path; `stop` verifies deletion with `GetSpecificPortMappingEntry`.

**UPnP IGD** is SSDP `M-SEARCH` → device description → `WANIPConnection`/`WANPPPConnection` control
URL → `AddPortMapping` (1 h lease, renewed while running) / `DeletePortMapping`, plus
`GetExternalIPAddress` compared against the `lab.plxnative.com` A record — that comparison is the
only cheap check that the path actually exists before a tester spends a lab hour on it.

---

## 9. Build and deploy loop

```
plxnative-lab start                       # prints session + the make line below
make LAB=1 FLAVOR=debug ipk               # bakes pkg/lab.json into the package
   → upload pkg/com.beb.plxnative.debug_0.4.1_arm.ipk to Cloud Test Lab, install, reproduce, press BLUE
plxnative-lab logs --follow               # the agent watches
plxnative-lab stop                        # mapping removed
```

`LAB=1` sets `RUST_FEATFLAGS += --features lab-diagnostics` and its own `RUST_TDIR=target-lab` —
the Makefile already documents that exact escape hatch for a non-standard feature set, and cargo
does not hash its output, so a shared target dir would hand back a non-lab `.a` and report it fresh.
`ci/mkipk.py` stages `pkg/lab.json` **only when it exists and `LAB=1`**; `ci/check-package.py` gains
the inverse assertion — **a non-lab package containing `lab.json`, or a lab build on the stable app
id, is a packaging error**, which is where this class of mistake gets caught rather than in review.
`pkg/lab.json` is gitignored and added to `PRIVATE_FILES` in `.claude/hooks/outbound-guard.py`
(it holds a live secret and the developer's endpoint).

---

## 10. What was built

**In:** the feature flag, the ring + tap, the snapshot envelope from `Diag`/`webos`/`devcaps`, the
scrub pass, pinned gzip upload on a worker, the account-menu row **and** the configurable colour-key
arm, the toast, `plxnative-lab start/status/logs/stop` with UPnP, `LAB=1` packaging + the two
package assertions.

**Deliberately out of the first iteration:** any Mac→TV direction, auto-upload on crash or on the
player failure read-out, multiple concurrent sessions, retry/queueing across an app restart,
persisting the ring to disk, a web UI, and promoting playback mode into `Diag`.

**Deliberately out of this iteration:** any Mac→TV direction, auto-upload on crash or on the player
failure read-out, several concurrent sessions, retry or queueing across an app restart, persisting
the ring to disk, a web UI, and promoting Direct Play / Direct Stream / Transcode into `Diag` (the
`load:` and route lines carry it in the ring today).

---

## 11. What is verified, and what is not

**Verified, all on the host, no television:**

* `make check` is green in both configurations — 1265 tests without the feature, 1290 with it. The
  new ones cover the ring's two caps and its `dropped` delta, the `lab.json` parse and each refusal
  it names, the four scrub rewrites and the outright refusal, the document's JSONL shape (including
  a record carrying a newline and a quote), the gzip member's CRC and trailers against the standard
  check value, the toast's single-flight flag and its expiry, and that the toast sits inside the
  safe area and clear of the stats panel.
* All four feature configurations type-check clean under `warnings = "deny"`: default,
  `--no-default-features`, `+lab-diagnostics`, and `--no-default-features +lab-diagnostics`.
* `tools/plxnative-lab selftest` — a real TLS listener with a freshly generated certificate, a real
  gzip upload accepted and stored, and the six refusals (wrong secret, oversized declared body,
  empty body, foreign path, bare GET, rate limit). Wired into `make check`.
* **The whole chain, end to end, on loopback**: `plxnative-lab start --hostname 127.0.0.1
  --no-upnp` → `make LAB=1 sim` → `k:0,406` down the remote FIFO → the app logged
  `lab: snapshot seq=1 reason=key route=login` and `lab: uploaded seq=1 4689B -> 2053B (gzip)
  status=200`, and `plxnative-lab logs` printed the envelope and 44 records. The toast was
  screenshotted reading *Diagnostics uploaded / 2 KB sent*.
* The **network path exists**: the Keenetic answers SSDP (`http://<router>:1900/ctl/IPConn`),
  reports its external address, and `lab.plxnative.com` resolves to exactly that address. Checked
  by `plxnative-lab selftest`'s closing note and by `status`'s `dns_matches`.

**NOT verified, and each needs hardware:**

* A real UPnP `AddPortMapping` on the Keenetic, and an inbound connection through it. Discovery and
  the external address are proven; the mapping itself has never been created.
* `CURLOPT_PINNEDPUBLICKEY` against the **television's** libcurl 7.53.1/OpenSSL 1.0.2. It is
  documented from 7.39 and works on the Mac's curl; the pin arm has never run on ARM.
* The colour button — every word of §7.
* Anything about the ARM cross-build of this feature: `make LAB=1` has not been run against the
  NDK, and the `fw-compat-reviewer` pass that `net.rs`'s new option warrants has not been done.
* The `.ipk` path: `ci/check-package.py`'s two new assertions have not been executed, because no
  lab package has been built.

The next step is the dev television — `make LAB=1 FLAVOR=debug deploy` under the `tv-lock`, with a
receiver on the LAN — before an hour of Cloud Test Lab is spent on any of it.

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
