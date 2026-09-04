---
name: tv-session
description: >
  Boot the app on the LG webOS dev TV into a chosen screen, inspect it, drive it live, collect
  screenshots or logs, and hand the TV back. Use for on-device UI or playback verification,
  reproducing a bug on the set, navigating a live screen, capturing the video plane, or giving
  the user a live view. Also covers off-network phone/Safari access through the authenticated
  `up --remote` HTTPS tunnel, the `/tmp/plxnative-*` boot triggers, token and profile-picker boot
  gates, remote key/click injection, and capture-source choice. Use this instead of generic
  run/verify commands when behavior must be proven on the cross-compiled ARM television.
---

# Working on the TV

> **FIRST: take the television's lock.** One set, no OS-level mutex — two jobs on it produce
> plausible WRONG data rather than a clean failure. `tools/tv-lock.sh acquire --why "…"` before the
> session and `release` after (and `tools/tv-lock.sh status` to see who has it). Every driving
> subcommand below refuses without it; `log` and `status` are read-only and only name the holder.
> The **`tv-lock` skill** is the workflow, and the reason to take one lease for the whole session
> rather than letting each command take its own: the gap between two of your own commands is where
> another lane lands.

> **Before booking the TV: can the simulator answer this?** `make sim` runs the same app core on
> macOS — real UI, real PMS data, boot triggers, the same FIFO tokens, self-screenshotting, and
> several instances at once. Layout, focus, navigation and the whole data layer are answerable
> there without touching the television, which is a single shared resource. See the **`ui-sim`**
> skill. Come back here for frame rates, text rasterization, video, the video plane, or a final
> device check — none of which the simulator can settle.

There is no host runtime: the only way to know whether something works is to run it on
the television and observe. That ritual has a lot of steps and each fails in its own
silent way, so it lives in one asserting driver.

All paths are relative to the repo root. The TV address comes from the `Makefile`
(override with `TV=…`); no addresses or credentials live in this skill. The PMS token is
read from the gitignored `src/config.local.h` at runtime and never printed.

## The driver

```bash
tools/tv-session.sh up [--flavor <f>] [--screen <name>] [--server <slot>] [--stream[=PORT]] [--remote[=PORT]] [--no-token]
tools/tv-session.sh status [--flavor <f>]   # re-assert without disturbing the session
tools/tv-session.sh key down down ok        # key tokens through the real handlers
tools/tv-session.sh click 960 540           # authored 1920x1080 coords
tools/tv-session.sh shot [out.png]          # panel capture (video plane included)
tools/tv-session.sh log [--flavor <f>] [regex]   # the on-device event log
tools/tv-session.sh down [--flavor <f>]     # hand the TV back
```

`down` hands back the APP (interactive boot, triggers cleared); `tools/tv-lock.sh release` hands
back the TELEVISION. Do both, in that order — a released lock with an automated app still on
screen is the next lane's confusing morning.

`--flavor <f>` selects **which install** the command talks to, and every subcommand accepts it —
each of them names an app id, an app directory or a runtime root, and none of those is a single
value any more. Omitted, you get the Makefile's default, which is the **developer** install. Read
*Which install* below before your first `up` on a television that has never had one; the flavour
`up` brought a session up with is what `status` reports, so a session driven with the wrong one
fails at the FIFO rather than doing something to the wrong app.

`up` asserts every step instead of assuming it: TV reachable (waking it if not) → the
requested flavour is installed (a deploy cannot create one; see below) → deployed binary
md5-matches your build (deploying and **re-verifying** if not, because a standby can
truncate an scp mid-flight) → triggers cleared → requested triggers armed →
token injected → close-first relaunch → process alive → the route heartbeat says you are
on the screen you asked for → the remote FIFO exists. A live view is one flag away.

`--screen` accepts: `home` (default), `profiles`, `login`, `account`, `library[=N]`,
`detail=<ratingKey>`, `person=<MOVIE ratingKey>`, `player=<ratingKey>`, `itemmenu`.
(That is the script's whole `case`; `tools/tv-session.sh --help` is the other copy.)

For a multi-server boot, `--server <slot>` supplies the server half of a `detail=` or `player=`
item identity. Use it instead of navigating through Search/Sources with key presses: for example,
`--screen player=5469 --server 1` arms `plxnative-play` and `plxnative-server` together. The app
refuses an invalid or unregistered explicit slot rather than falling back to the current server,
because the same numeric rating key normally names a different item on each PMS. The option also
selects the signed-in stored-session boot automatically: that is the credential source which
restores the persisted multi-server roster. After launch the command requires both the requested
route and the exact `ratingKey + server slot` success marker; a missing slot exits nonzero instead
of printing a misleading `session up`.

`itemmenu` is the press-and-hold card context menu, and it is the only boot path to it. The
interactive gesture is a real ≥500 ms hold (`press::LONG_MS`), which no boot trigger can express,
so `plxnative-itemmenu` snaps into the grid and holds the focused card for you. Driving it by hand
instead means the FIFO's split halves — `okdown`, sleep past 500 ms, `okup` — never the `ok` tap.

`person` is the odd one: the actor page has **no boot trigger of its own** — it is *reached*
from a detail page's cast row, so the rk you pass is the **movie's**, and `up` arms the three
triggers that walk there (`detail=<rk>` + `detailsec=1` to drop focus onto Cast & Crew + the
`detailok` press). Pick a movie whose FIRST cast member has titles in more than one library if
you want both the Movies and Shows shelves populated.

## Which install — and the one step `up` cannot do for you

Two builds live on this television. **stable** is `com.beb.plxnative`, the app the household
watches with; **debug** is `com.beb.plxnative.debug`, the developer build beside it, with its own
launcher tile (amber DEV bar), its own sign-in and its own runtime files. webOS keys the install
directory, SAM's `launch`/`closeByAppId` and the LS2 role file on that id, so the two cannot touch
each other. **`debug` is the default**, deliberately: deploying to `debug` when you meant `stable`
costs you retyping one command, while the reverse destroys a working install — possibly mid-film —
on the app somebody actually watches with. So `stable` has to be typed.

**A flavour must be INSTALLED once before `deploy`, and therefore before `up`, can reach it:**

```bash
make FLAVOR=debug install       # builds its .ipk, installs it through appinstalld, deploys into it
```

`scp` cannot create an app. The app directory, SAM's registration, and the per-id LS2 role file
that lets the app talk to `com.webos.media.*` are all written by the installer, and none of them
exist until the package has been through it once. The target then deploys, deliberately —
appinstalld replaces `applications/<id>/` **wholesale**, so stopping at the install would leave you
looking at the packaged binary rather than the one you just built, which is this project's
signature "plausible wrong data" failure. `make deploy` `test -d`s the app directory and fails
naming that command instead of `mkdir -p`-ing a directory SAM knows nothing about;
`make FLAVOR=debug uninstall` removes one (it refuses the stable id).

Ask the Makefile rather than restating any of this — these are real recipes, free and
side-effect-free:

```bash
make -s print-flavor   FLAVOR=debug     # debug
make -s print-appid    FLAVOR=debug     # com.beb.plxnative.debug
make -s print-appdir   FLAVOR=debug     # /media/developer/apps/usr/palm/applications/<id>
make -s print-rundir   FLAVOR=debug     # /tmp/com.beb.plxnative.debug   (stable: /tmp)
make -s print-eventlog FLAVOR=debug
make -s print-tv                        # the TV address, expanded
```

**Never reach for `make -p`/`make -pn` to read one of these.** It prints a recursive variable's
UNEXPANDED DEFINITION, so `TV` comes back as the literal `$(strip $(shell cat .tv-host …))` — every
ssh built from it then fails, and the tool reports an unreachable television that is in fact awake
and idle in front of you.

### The runtime root moved, and only for a flavoured install

The stable install keeps `/tmp` byte for byte, so every recipe and every `/tmp/plxnative-…` line
below stays literally true for the app users get. A flavoured install puts its triggers, its
`plxnative-remote` FIFO and its three `*.log` files in `/tmp/<app id>` instead. **Every name is
unchanged — only the directory moved**, so read each `/tmp/plxnative-…` path here as
`$(make -s print-rundir FLAVOR=…)/plxnative-…`.

**Two payloads do not carry the prefix, and that rewrite rule silently misses them:**
`sample.h264` and `sample.h265`, the raw Annex-B samples the player feeds instead of streaming.
They moved into the runtime root with everything else — `$(make -s print-rundir FLAVOR=…)/sample.h264`,
not a shared `/tmp/sample.h264` — and they are read through `dev::read_sample`, which resolves via
`paths::in_runtime_dir` like every other dev read. A second consequence follows from the same
missing prefix and is easy to want the other way round: `dev::any_trigger_present` matches on
`plxnative-`, so **a sample does NOT mark the boot as automated** and does not suppress the
who's-watching picker. Arm one on a multi-user account and you land on the picker, not on Home.

The separator is a DOT, and that is structural rather than cosmetic. The **stable** install's
runtime root *is* `/tmp`, and it treats every entry there whose name begins `plxnative-` as an
armed trigger. So the flavour suffix has to stay outside that prefix namespace: a root named
`/tmp/plxnative-debug` would sit in `/tmp` reading, to the *other* install, as a permanently armed
trigger — silently suppressing the released app's who's-watching picker, with no line in any log.
`com.beb.plxnative.debug` contains no `plxnative-`, so it cannot. The rule is the **prefix**, not
avoiding a clash with some file that happens to exist: there is no `plxnative-debug` trigger, and
the dot is what makes it not matter if one is ever added. (`docs/two-installs.md` §4.1 has the
second, independent guard.)

The root is created **`mkdir` then an explicit `chmod 1777`**, because a umask masks mkdir's mode
and two uids write into it with no way to order them: `up` and the harness arm triggers there over
ssh **as root** before the app has ever booted, while the app runs jailed under its own uid and
creates its logs there. Owner-only locks one of them out, and a root-owned event log the jailed app
cannot write stays 0 bytes — which every tool in this repo reports as "no line found", i.e. exactly
like a total regression.

### md5 proves the BYTES, not which install they are in

`up` still md5-compares your `pkg/plxnative` against the copy in the app directory, and that check
is worth having — but with two installs it answers a narrower question than it looks like it does.
`pkg/plxnative` is a path that **every** flavour and **both** configurations write, so a match
proves only "these are the bytes on my disk right now". It says nothing about which app produced
the log you are about to read. `pidof plxnative` cannot close the gap either: both binaries are
named `plxnative`, so on this busybox set it returns two pids in an order nothing promises. For
liveness use `fuser $(make -s print-appdir FLAVOR=…)/plxnative`, which is inode-scoped and can only
match one install. And anchor any path match on a delimiter — `com.beb.plxnative` is a **prefix**
of `com.beb.plxnative.debug`, so match `/<id>/`, never the bare id.

**The strong witness is the first line of the event log**, written before anything can fail:

```
install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug features=dev APPID_env=…
appdir: /media/… (from current_exe)
```

It is the only thing in the system that names which install wrote a log (`features=` is `dev` or
`release`). Read it whenever a result is surprising, and always before reporting a regression from
a log you did not watch being produced.

## Triggers are boot state; the FIFO is live state

Every `plxnative-*` trigger in the install's runtime root is read **once at boot**, so it
must be in place before the launch. Anything you want to do to a *running* app goes through
the remote FIFO (`tv-session.sh key` / `click`).

**Three exceptions, all deliberate and all read LIVE**: `plxnative-failtest` (so a read-out variant
can be swapped mid-playback), `plxnative-testpat` (the same for the synthetic ground), and
`plxnative-gohome` (which leg of the root press to force — armed AFTER the screen you want has
settled, because arming it before the launch also makes the boot count as automated and moves which
screen you land on).

**Two traps that cost real time:**

1. **A stale trigger silently changes what you are looking at.** `make run` clears only
   the event log — unlike `tests/run.py`, it does **not** clear triggers — so a by-hand
   run inherits whatever the last session armed. `tv-session.sh up` glob-clears first,
   in the runtime root of the flavour it was given.
2. **Any non-DIAG trigger also suppresses the who's-watching picker.** The app treats the
   presence of any `plxnative-*` file in its runtime root, outside an exemption list, as
   "this is an automated boot" — which is why arming a live view does not change the boot
   you are observing. **The exempt list is `dev::DIAG` in `rust-modules/src/dev.rs`, and
   only that**; it has grown past the three `*.log` files and the four names this skill
   used to spell out, and a count written here rots without anything failing.

**The catalog is the source, not a doc.** There are ~40 triggers worth knowing and
`docs/agent-reference.md` highlights only the common ones. **Run the two-part catalog command from
`docs/agent-reference.md`** (the "The catalog is the source, not this list" bullet) rather than any copy —
it is deliberately not transcribed here, because it has already been transcribed once too often.
What it gives you is the set of **names**, which is the same for both installs; the `/tmp`
literals inside it are just where the stable install's root is.

Two halves are needed and a single grep is the trap: a path literal now only ever appears in a
COMMENT, and four triggers (`grid`, `h265`, `playidx`, `ptype`) are named nowhere but their
`dev::flag`/`dev::read` call, so grepping paths alone silently under-reports the catalog.

Boot gate order, when you care which identity you land as: `plxnative-login` forces the QR
screen → `plxnative-token` beats any stored session → a stored session (with the picker
for a multi-user account) → otherwise QR sign-in. Nothing is compiled into the binary.

## Observing: pick the right capture source

| Source | Sees | Rate |
|---|---|---|
| In-app stream (`--stream`, browser on `:8909`) | **UI plane only** | see below |
| `tools/tv-session.sh shot` / `capture-screen.sh` (luna service) | UI **+ the hardware video plane** | ~2–3fps |

**Two ports, and they are not the same one.** `:8909` is the local page `stream-screen.py`
serves you; the app's own capture listener on the TV is a second port that the page consumes.
That listener is **8910 for the stable install and 8911 for a flavoured one** — two installs must
not fight over one socket — and `make -s print-appport FLAVOR=…` is that rule for the shell
(`capture::default_port` is the same rule in Rust; `ci/flavor.py --selftest` compares them).

The split only bites when you arm the trigger **by hand**: an empty `plxnative-capture` takes the
default for that install, so a debug install lands on 8911 and the page needs
`--app-port 8911` (or `TV_APP_PORT=8911`) to find it. `tv-session.sh --stream` never has to be told:
it writes the resolved number into the trigger content (`plxnative-capture=$APPPORT`, where
`APPPORT` came from `make -s print-appport FLAVOR=$FLAVOR`) and hands the SAME variable to
`stream-screen.py --app-port`, so the arm and the viewer cannot address different ports. The
session is therefore on 8910 for `--flavor stable` and 8911 at the default — the port follows the
flavour rather than being pinned to either.

**Stream resolution is a speed lever, not cosmetics.** MPEG1 has no intra prediction, so
encode cost tracks screen *detail* as much as size. Measured on the same home screen (a
full-bleed photo backdrop plus poster shelves):

| Size | Encode | Result |
|---|---|---|
| `960x540` | 53–114 ms/frame | ~9–19fps — looks stuck while it is in fact live |
| `480x270` | ~13 ms/frame | ~24–30fps, soft but easily readable |

A flat screen (the who's-watching picker) costs ~22 ms even at 960x540 — so judge by what
is on screen, not by the number alone. `STREAM_RES=480x270 tools/tv-session.sh up --stream`
for smooth; the default 960x540 for sharp stills.

**This is the trap that produces wrong conclusions:** GL cannot see the hardware video
overlay, so verifying *playback* with the in-app stream shows a plausible black rectangle
and no error. Anything involving decoded video must be checked with the service capture
(`shot`), or with `VIDEO`-only capture, whose `CAPTURE_ERROR_09 "no signal state"` doubles
as a decode health check.

Often the **event log is the real evidence** and the picture is a nicety — `tv-session.sh
log <regex>` is usually faster than looking.

## Watching from off-network (a phone, another house)

```bash
tools/tv-session.sh up --screen home --remote     # prints an https:// URL + a generated password
tools/tv-session.sh status                        # re-prints them; says the URL is LIVE
tools/tv-session.sh down                          # revokes the URL, discards the password
```

`--remote` implies `--stream`, then puts an authenticated **D-pad-only** page
(`tools/remote-dpad.py`) in front of it and publishes that through a cloudflared tunnel.
Needs `brew install cloudflared`; it is entirely opt-in, so an ordinary `up`/`--stream`
session is unchanged and costs nothing.

**Why a tunnel and never a router port forward.** The obvious ask is "forward a port"
(a torrent client does it via UPnP), and it is the wrong tool here twice over. The page
uses HTTP Basic auth, so a plain forward puts the password on the wire **in cleartext,
several times a second**, while the tunnel is TLS end-to-end. And a forward aimed at the
TV itself would expose `root`/`alpine` — the published webosbrew default, sitting in this
repo's own Makefile — which is compromised by automated scanners in minutes. The tunnel
also needs no router change at all and works behind CGNAT.

**What a remote viewer can and cannot do.** The limit lives in the proxy, not the network,
so it holds even for someone who has both the URL and the password: six D-pad tokens
(`up/down/left/right/back` plus `okdown`/`okup`, which is what makes a press-and-**hold**
context menu reachable). `ck:X,Y` pointer clicks and the transport keys are refused with
403 — aiming at coordinates on a picture you may be seeing seconds late is not worth it.
`up` asserts the 401 before opening the tunnel.

**FPS: the transport is the lever, then the TV's CPU.** Measured this way, end to end:

| transport | fps | bitrate |
|---|---|---|
| JPEG pull (`/frame.jpg?after=`) over the tunnel | 7.0–7.6 | ~2 Mbit/s |
| **MPEG1 over the WebSocket, 480x270** | **23.6** | 0.6 Mbit/s |
| MPEG1 over the WebSocket, 960x540 (default) | 17.4 | 2.0 Mbit/s |

A JPEG pull costs one HTTPS round trip **per frame**, and the tunnel's RTT was 511 ms —
so it cannot be fixed by pipelining (1, 4 and 8 concurrent pollers all measured ~12.2 fps
locally; the JPEG path itself caps there). A WebSocket makes RTT a latency cost instead of
a throughput cost, and MPEG1 has inter-frame compression where JPEG resends every pixel.

Past that it is the TV's ARM CPU, not the network: readback → colour-convert → **software**
encode, and encode time scales with pixel count (`venc:` in the event log reports it —
53.5 ms/frame at 960x540). **1080p is therefore not viable**: ~4× that, ≈4.7 fps, i.e.
worse than the problem you started with. `STREAM_RES=480x270` is the fps setting; the
960x540 default is the readable one, which is usually what a UI review wants.

**The trap that costs an hour.** The app serves one capture client per connection and does
**not** hang up on a dead peer. Restart the streamer without closing the old one and a stale
client is left on the app: the encoder keeps running and the event log keeps printing
`venc: N frm ...` while the new streamer reads **zero bytes**. Everything looks healthy and
nothing arrives. `up` now stops all local viewers *before* the relaunch for exactly this
reason — if you are driving `stream-screen.py` by hand, do the same, and treat
"`/version` says `jpeg` when you asked for `mpeg`" as the symptom (that field only flips to
`mpeg` once TS has actually flowed).

**Handback etiquette applies double.** A published URL outlives the terminal, so `down`
prints that it revoked it, and `status` prints that it is live. Do not leave one up.

## Handback etiquette

This is the user's actual television. When you are done, `tv-session.sh down` strips the
token and every automation trigger and relaunches a genuine interactive boot (picker or QR,
as a real user gets). Leaving an injected-token session up means the next person to turn on
the TV is signed in as whatever the automation chose.

## Gotchas

- **A sleeping TV mimics total failure.** Every log assertion fails as "no line found" — an
  FPS suite reported `0 samples` on all five scenes for exactly this reason, which reads
  like a catastrophic regression. The driver wakes it first; if you are running something
  else by hand, wake it first (`wake-tv` skill).
- **A standby cycle is ASSUMED to clear `/tmp`, and nobody has checked** — see the `wake-tv`
  skill, which carries the open question and the one-session probe that would settle it.
  Either way the safe move after a sleep is the same: re-run `up`, which recreates the
  runtime root 1777 and re-arms everything before it launches. Do not build a measurement
  on triggers armed before a sleep. (The app directory is on flash and does survive; an
  install is one-time.)
- **SAM keeps stale "running" state**, so a launch without a close-first is a silent no-op
  relaunch, and `luna-send` must stay subscribed (`-i`) for the launch to take — which
  means the SSH session has to stay OPEN while it does. Backgrounding the launch and
  letting ssh return kills the subscriber: the old instance keeps running, so every check
  downstream passes while you are testing yesterday's build. The driver holds the session
  and then asserts the **pid changed**.
- **Do not run the harness and a live session at once — two installs are not two
  televisions.** `tests/run.py` glob-clears triggers and kills the app per case; a
  concurrent session produces bogus failures that look like real regressions. The flavour
  split separates the app directories and the runtime roots, and nothing else: one panel,
  one video plane, one capture service, one set of luna calls. Run `tv-session.sh down`
  (or just stop the streamer) first.
- **`BACK` at a ROOT hands the screen to the LG launcher** (`webos::go_home`) — Home's root, the
  who's-watching picker and the QR sign-in all do this since 2026-09-03, and none of them ends the
  session. The consequence for a driver: a stray `back` at a root puts the **LG launcher RIBBON
  over the still-running, still-drawing app** — on this webOS 4 set the launcher is an overlay, so
  the app keeps its pid, keeps presenting, and receives NO `LIFECYCLE: background` (device-measured
  2026-09-04; there is no `0x105`/`0x106` pair to wait for here). The next `shot` captures the
  ribbon over your screen, and keys go to the launcher until it is dismissed. To carry on without a
  relaunch, ask SAM to foreground the app again — a `launch` of its own id, the same call
  `tools/tv-session.sh up` makes — which removes the ribbon and logs nothing new; `up` itself is
  NOT that, it closes first and waits for a CHANGED pid. A webOS 5+ set (full-screen launcher)
  should background the app instead, and is unmeasured.
  `tv-session.sh down` still closes through SAM and is unaffected; the remote's EXIT key is now the
  only key that ends the process.
- **Clicks need the jitter**, which the app's injection path already applies: a pointer
  click after D-pad use is swallowed unless enough motion accumulates first. Hover is
  deliberately *not* forwarded (it used to park focus on a tab pill so the next ENTER
  opened the library).
- **Dead ends, so nobody re-tries them:** external input injection cannot reach this app
  (the compositor opens a fixed evdev set at boot; LG's keymanager only reaches the web-app
  layer) — the in-app FIFO is the only path. There is no continuous-capture API on this
  build, only the one-shot service. `luna-send` silently no-ops without a controlling TTY,
  so on-device calls are wrapped in `script -qc` (never `ssh -tt` for a binary stream — it
  mangles the bytes).

## Troubleshooting

| Symptom | Fix |
|---|---|
| `no route= heartbeat` after launch | Check the install-scoped PID with `fuser`. No PID: `crash-triage`; live PID: `profile-tv` / `tools/plxnative-sample snapshot`. |
| Landed on the wrong screen | A stale trigger. Re-run `up` (it clears), or check `status`, which lists what is armed. |
| Boot shows the QR sign-in screen | No token — `src/config.local.h` is missing/unreadable, or you passed `--no-token`. |
| Picker appeared during an automated run | You armed only DIAG-exempt triggers. Add any other trigger, or use `--screen profiles` deliberately. |
| `<appdir> does not exist … the <f> flavour is not installed` | The one thing a deploy cannot do. `make FLAVOR=<f> install` once, then re-run `up`. |
| `deploy did not land (md5 still differs)` | The TV likely slept mid-scp. Re-run `up`. **Check the install first** — if the flavour was never installed, `up` says so in the line above and no amount of re-running fixes it. |
| The log names an app id you did not ask for | You are reading the other install's log. The `install:` boot line is the witness; `make -s print-eventlog FLAVOR=<f>` is the path you meant. |
| Stream shows a black rectangle during playback | Expected — the in-app stream cannot see the video plane. Use `shot`. |
| Keys do nothing | The FIFO only exists while the app runs; check `status`. A key at the wrong route may also be a no-op by design. |
