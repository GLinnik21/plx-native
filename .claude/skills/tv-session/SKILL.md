---
name: tv-session
description: >
  Boot the app on the LG webOS dev TV into a chosen screen, look at it, drive it live,
  and hand the TV back. Use whenever a change must be seen or verified on the device:
  "show me the home screen", "screenshot the library grid", "does this UI change look
  right", "reproduce it on device", "click through the detail page", "give me a live view
  of the TV", "run it and show me the log", "I want the TV back". Covers the
  /tmp/plxnative-* boot triggers (which screen each reaches, and why a stale one silently
  changes what you are looking at), token/picker boot gating, the remote FIFO for live key
  and click injection, and which capture source can actually see the video plane. Use this
  instead of generic run/verify patterns — this is a cross-compiled ARM TV binary with no
  host runtime, so behaviour is the only test.
---

# Working on the TV

There is no host runtime: the only way to know whether something works is to run it on
the television and observe. That ritual has a lot of steps and each fails in its own
silent way, so it lives in one asserting driver.

All paths are relative to the repo root. The TV address comes from the `Makefile`
(override with `TV=…`); no addresses or credentials live in this skill. The PMS token is
read from the gitignored `src/config.local.h` at runtime and never printed.

## The driver

```bash
tools/tv-session.sh up [--screen <name>] [--stream[=PORT]] [--no-token]
tools/tv-session.sh status              # re-assert without disturbing the session
tools/tv-session.sh key down down ok    # key tokens through the real handlers
tools/tv-session.sh click 960 540       # authored 1920x1080 coords
tools/tv-session.sh shot [out.png]      # panel capture (video plane included)
tools/tv-session.sh log [regex]         # the on-device event log
tools/tv-session.sh down                # hand the TV back
```

`up` asserts every step instead of assuming it: TV reachable (waking it if not) →
deployed binary md5-matches your build (deploying and **re-verifying** if not, because a
standby can truncate an scp mid-flight) → triggers cleared → requested triggers armed →
token injected → close-first relaunch → process alive → the route heartbeat says you are
on the screen you asked for → the remote FIFO exists. A live view is one flag away.

`--screen` accepts: `home` (default), `profiles`, `login`, `account`, `library[=N]`,
`detail=<ratingKey>`, `person=<MOVIE ratingKey>`, `player=<ratingKey>`.

`person` is the odd one: the actor page has **no boot trigger of its own** — it is *reached*
from a detail page's cast row, so the rk you pass is the **movie's**, and `up` arms the three
triggers that walk there (`detail=<rk>` + `detailsec=1` to drop focus onto Cast & Crew + the
`detailok` press). Pick a movie whose FIRST cast member has titles in more than one library if
you want both the Movies and Shows shelves populated.

## Triggers are boot state; the FIFO is live state

Every `/tmp/plxnative-*` trigger is read **once at boot**, so it must be in place before
the launch. Anything you want to do to a *running* app goes through the remote FIFO
(`tv-session.sh key` / `click`).

**Two traps that cost real time:**

1. **A stale trigger silently changes what you are looking at.** `make run` clears only
   the event log — unlike `tests/run.py`, it does **not** clear triggers — so a by-hand
   run inherits whatever the last session armed. `tv-session.sh up` glob-clears first.
2. **Any non-DIAG trigger also suppresses the who's-watching picker.** The app treats the
   presence of any `/tmp/plxnative-*` file outside a 7-entry exemption list as "this is an
   automated boot". The exempt ones are the three `*.log` files plus `plxnative-profile`,
   `plxnative-anim`, `plxnative-remote` and `plxnative-capture` — which is why arming a
   live view does not change the boot you are observing.

**The catalog is the source, not a doc.** There are ~40 triggers today and `CLAUDE.md`
highlights only the common ones. Get the real list with:

```bash
grep -rhoE '/tmp/plxnative-[a-z0-9]+' rust-modules/src src | sort -u
```

Boot gate order, when you care which identity you land as: `plxnative-login` forces the QR
screen → `plxnative-token` beats any stored session → a stored session (with the picker
for a multi-user account) → otherwise QR sign-in. Nothing is compiled into the binary.

## Observing: pick the right capture source

| Source | Sees | Rate |
|---|---|---|
| In-app stream (`--stream`, port 8910 → browser) | **UI plane only** | see below |
| `tools/tv-session.sh shot` / `capture-screen.sh` (luna service) | UI **+ the hardware video plane** | ~2–3fps |

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
- **Standby wipes `/tmp`.** Triggers, the token, the FIFO and any helper script you left
  there are gone after a sleep. Re-run `up`.
- **SAM keeps stale "running" state**, so a launch without a close-first is a silent no-op
  relaunch, and `luna-send` must stay subscribed (`-i`) for the launch to take — which
  means the SSH session has to stay OPEN while it does. Backgrounding the launch and
  letting ssh return kills the subscriber: the old instance keeps running, so every check
  downstream passes while you are testing yesterday's build. The driver holds the session
  and then asserts the **pid changed**.
- **Do not run the harness and a live session at once.** `tests/run.py` glob-clears
  triggers and kills the app per case; a concurrent session produces bogus failures that
  look like real regressions. Run `tv-session.sh down` (or just stop the streamer) first.
- **`BACK` at the Home root exits the app** — an easy way to end a session by accident.
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
| `no route= heartbeat` after launch | The app died on boot or is still starting. Run `tools/crash-report.sh` (crash-triage skill). |
| Landed on the wrong screen | A stale trigger. Re-run `up` (it clears), or check `status`, which lists what is armed. |
| Boot shows the QR sign-in screen | No token — `src/config.local.h` is missing/unreadable, or you passed `--no-token`. |
| Picker appeared during an automated run | You armed only DIAG-exempt triggers. Add any other trigger, or use `--screen profiles` deliberately. |
| `deploy did not land (md5 still differs)` | The TV likely slept mid-scp. Re-run `up`. |
| Stream shows a black rectangle during playback | Expected — the in-app stream cannot see the video plane. Use `shot`. |
| Keys do nothing | The FIFO only exists while the app runs; check `status`. A key at the wrong route may also be a no-op by design. |
