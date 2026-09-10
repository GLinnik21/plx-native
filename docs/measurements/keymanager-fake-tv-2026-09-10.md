# Fake `keymanager3` — device matrix, 2026-09-10

Issue #76: `com.webos.service.keymanager3` seals and round-trips a session envelope IN-PROCESS but
the envelope never reopens on the NEXT launch. This is the **device** companion to
`docs/measurements/keymanager-fake-sim-matrix-2026-09-10.md` (a `make sim` run) — the same fake,
in-process double of the service (`rust-modules/src/keymanager.rs`'s `fake` module, selected by
`/tmp/com.beb.plxnative.debug/plxnative-keymanager=<mode>`), run on the real jail/flash of the dev
television (webOS 4.10.2, `m16p3`/`M19_DVB`), which predates the real `keymanager3` service and so
has no real backend to confuse the fake with. **Debug flavour only** (`com.beb.plxnative.debug`);
the stable install was never touched.

Three modes were run, three launches each, against a **pre-existing real stored sign-in** already
present on the debug install (no seeding needed — `/media/developer/com.beb.plxnative.debug-auth.json`
already held a signed-in session before this task started).

## Build / deploy

```sh
CARGO_TARGET_DIR=/Users/gleblinnik/plx-fleet/issue75/target make            # cross-build, dev flavour (default features, devtriggers on)
tools/tv-lock.sh acquire --ttl 15 --why "deploy dev build to debug install (issue #76 device matrix)"
CARGO_TARGET_DIR=/Users/gleblinnik/plx-fleet/issue75/target make deploy     # FLAVOR=debug is the Makefile default; RELEASE=1 never passed
```

`verify-deploy` reported **15 files match**. The deployed binary's own event log confirms the
install:

```
install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug features=dev APPID_env=com.beb.plxnative.debug
```

`features=dev` on every launch below — `devtriggers` is compiled in, `RELEASE=1` was never passed.

## Per-launch procedure

```sh
tools/tv-session.sh screen off        # panel OFF for every run
# arm the mode (once per mode, kept armed across that mode's 3 launches — a trigger is read once at boot):
sshpass -p alpine ssh root@192.168.0.114 \
  "printf '%s' '<mode>' > /tmp/com.beb.plxnative.debug/plxnative-keymanager"
CARGO_TARGET_DIR=/Users/gleblinnik/plx-fleet/issue75/target FLAVOR=debug RUN_SECS=15 make run
# (make run closes any running instance, wipes only the event log — not triggers or session
#  files — launches, waits RUN_SECS, cats the event log back)
sshpass -p alpine ssh root@192.168.0.114 \
  "ls -la /media/developer/com.beb.plxnative.debug-auth.json \
          /media/developer/com.beb.plxnative.debug-secure-probe.json \
          /media/developer/com.beb.plxnative.debug-secure-storage.*"
```

**One deliberate deviation from a literal reading of "do not clear the session files":** the
per-launch instruction not to clear session files is about not signing the account out between
*launches of the same mode* (exactly what the perprocess/healthy sequences below rely on). The
cross-launch **markers** (`…-secure-storage.refused` / `…-secure-storage.proven` /
`…-secure-probe.json`) are a different thing — they are this dev-scenario's own persisted state,
not part of the real session, and `seal_permitted`'s refused-marker gate is a **permanent**,
install-wide latch (`rust-modules/src/plex/session.rs:668`): once written by one mode, it silently
disables every later mode's keymanager traffic for the rest of the sequence. Between *modes* (not
between launches within a mode) these three files were removed so each mode's 3-launch sequence
started from the same clean slate the simulator matrix used — otherwise `healthy` and `stall` would
each have reported nothing but "marked refused, no further keymanager calls" inherited from
`perprocess`. `auth.json` itself (the real session) was never touched by this reset.

## Results

| mode | launch | `keymanager:`/`session protection:`/`session:` lines | `auth.json` | markers / probe | boot route |
|---|---|---|---|---|---|
| `perprocess` | 1 | `keymanager: FAKE service armed mode=perprocess`; `session protection: keymanager3`; `session: secure storage has not yet been proven on this install; keeping the 0600 file and probing` | plaintext, 3149 B | `secure-probe.json` planted (178 B) | `boot: stored session — local server (offline-capable)` |
| `perprocess` | 2 | `keymanager: finish(decrypt) refused errorCode=-20030`; `session: secure storage is marked refused on this install; keeping the 0600 file` | plaintext, 3149 B | probe consumed → `secure-storage.refused` written (103 B) | `boot: stored session — local server (offline-capable)` |
| `perprocess` | 3 | `session: secure storage is marked refused on this install; keeping the 0600 file` (no further keymanager traffic — the marker short-circuits `seal_permitted`) | plaintext, 3149 B | `secure-storage.refused` persists | `boot: stored session — local server (offline-capable)` |
| `healthy` | 1 | `keymanager: FAKE service armed mode=healthy`; `session protection: keymanager3`; `session: secure storage has not yet been proven on this install; keeping the 0600 file and probing` | plaintext, 3149 B | `secure-probe.json` planted (178 B); `plxnative-keymanager-key` written (64 hex chars) in the runtime dir | `boot: stored session — local server (offline-capable)` |
| `healthy` | 2 | `keymanager: FAKE service armed mode=healthy`; `session protection: keymanager3` (a fresh save's own promotion round trip) — no "not yet proven" line this time | **becomes a real sealed envelope** — `{"format":"plxnative-secure-session","version":1,"sealed":{...}}`, 4399 B (was 3149 B plaintext) | probe reopened → `secure-storage.proven` written (65 B); probe removed | `boot: stored session — local server (offline-capable)` |
| `healthy` | 3 | `keymanager: FAKE service armed mode=healthy` only — no `session protection:`/`session:` line (nothing needed sealing or opening beyond the ordinary read) | secure envelope unchanged, 4399 B | `secure-storage.proven` persists | `boot: stored session — local server (offline-capable)` |
| `stall` | 1 | `keymanager: FAKE service armed mode=stall`; **`session: secure file is present but its device key is unavailable`** | secure envelope **unchanged**, 4399 B (never destroyed, never rewritten) | `secure-storage.refused` (re-)written (97 B) | **`boot: no session — starting QR sign-in`** |
| `stall` | 2 | `keymanager: FAKE service armed mode=stall`; `session: secure file is present but its device key is unavailable` | secure envelope unchanged, 4399 B | `secure-storage.refused` persists | `boot: no session — starting QR sign-in` |
| `stall` | 3 | `keymanager: FAKE service armed mode=stall`; `session: secure file is present but its device key is unavailable` | secure envelope unchanged, 4399 B | `secure-storage.refused` persists | `boot: no session — starting QR sign-in` |

## Reading the matrix

- **`perprocess` reproduces the reporter's failure and shows the fix containing it**, matching the
  simulator run exactly: launch 1 seals in-process and plants a probe; launch 2's fresh process has
  a fresh random key, so the probe (sealed by launch 1's key) fails `finish(decrypt)` with the real
  service's own tag-mismatch code (`-20030`) and the refused marker is written; launch 3 short-
  circuits on the marker with no further keymanager3 traffic. The account stays signed in
  (plaintext 0600 file) throughout — nobody is ever asked to sign in again.
- **`healthy` shows the fix's other half**, also matching the simulator run: a backend whose key
  survives a process boundary gets promoted on launch 2 (`secure-storage.proven`) and the session
  file becomes a real sealed envelope that same launch; launch 3 opens it with no incident. Signed
  in throughout.
- **`stall` is where the device run finds something the simulator matrix could not, because of the
  order these three modes were run in on one shared install.** The simulator matrix seeded each
  mode into its own fresh instance root, so its `stall` result (session stays on the 0600 plaintext
  file, signed in every launch, because `keymanager::seal()` fails at `generateKey` before any
  probe or envelope exists) tests a **plaintext** session meeting a stalled backend. This device run
  reused one install's real session across all three modes, and `healthy` (run second) had already
  turned that session into a **sealed** envelope. `stall` (run third) then met a *sealed* file with
  a backend that never answers `open` at all — and the app could not tell "stalled" apart from
  "genuinely gone": `session: secure file is present but its device key is unavailable`, followed by
  `boot: no session — starting QR sign-in`, on all three launches. **The sealed file itself is never
  destroyed or overwritten** — `auth.json` stayed byte-identical (4399 B) across all three `stall`
  launches, consistent with `session.rs`'s "preserving the existing secure file; refusing a
  plaintext downgrade" rule — but the account is, from the user's perspective, locked out: every
  boot lands on the QR sign-in screen instead of Home, with no automatic recovery visible in three
  launches. This is a genuine, device-observed consequence of issue #76's shape (a session sealed by
  a backend that later becomes transiently unavailable) that the simulator's fresh-per-mode
  methodology does not exercise, and is worth a maintainer decision on its own: whether a *stalled*
  (as opposed to a *proven-refused*) backend should eventually fall back to demanding a fresh
  sign-in the way this does, or whether it should retry treating the file as recoverable once the
  backend answers again.
- **Recovery is possible but needs the SAME fake backend, not the real one.** Re-arming
  `plxnative-keymanager=healthy` (the mode that originally sealed the file, with its persisted key
  file `plxnative-keymanager-key` still in the runtime dir) let one more launch open the envelope
  and land back on `boot: stored session`, proving the file itself was never corrupted — only
  unreadable while `stall` (or the absence of any working backend) is what answers. **This device
  has no real `keymanager3` at all (webOS 4.10.2 predates it)**, so once the fake trigger and its
  key file are cleared for cleanup (see below), the install is left holding a **sealed envelope
  that nothing on this firmware can open** — the next ordinary, trigger-free boot of the debug
  install will show `session: secure file is present but its device key is unavailable` /
  `boot: no session — starting QR sign-in` too, and will need a fresh QR sign-in to clear. This is
  a direct, unavoidable consequence of exercising `healthy` mode (which requires a persisted fake
  key to ever re-open its own envelope) followed by removing that fake key as instructed — not a
  bug this task found, but a state change worth flagging explicitly rather than leaving implicit.

## Cleanup

```sh
CARGO_TARGET_DIR=/Users/gleblinnik/plx-fleet/issue75/target FLAVOR=debug make kill
sshpass -p alpine ssh root@192.168.0.114 \
  "rm -f /tmp/com.beb.plxnative.debug/plxnative-keymanager /tmp/com.beb.plxnative.debug/plxnative-keymanager-key"
```

Final on-device state: `…-auth.json` is the sealed envelope from `healthy` mode (4399 B, unchanged
since `healthy` launch 2), `…-secure-storage.proven` persists (65 B) from the post-`stall` recovery
launch. No `plxnative-keymanager*` file remains in the runtime dir. `tools/tv-lock.sh release`
issued after the last device command.
