# Fake `keymanager3` — simulator matrix, 2026-09-10

Issue #76: `com.webos.service.keymanager3` seals and round-trips a session envelope IN-PROCESS but
the envelope never reopens on the NEXT launch. A fake, in-process double of the service
(`rust-modules/src/keymanager.rs`'s `fake` module, selected by `/tmp/plxnative-keymanager=<mode>`
— see `rust-modules/src/dev.rs`'s `keymanager_fake_mode` and `docs/agent-reference.md`'s dev-trigger
catalog) reproduces that failure — and its fix — end to end without a real keymanager3 anywhere:
the dev television predates the service and the simulator has no LS2 bus at all.

This is a `make sim` (macOS desktop simulator) run, not a device run — see `.claude/skills/ui-sim/
SKILL.md` for what does and does not transfer. It answers the storage-persistence question (which
is process/file state, not GPU or decoder behaviour), which is exactly what the simulator can see.

## Setup

Built once:

```sh
SIM_TDIR=/Users/gleblinnik/plx-fleet/issue75/target-sim make sim
```

For each mode, a fresh instance root was seeded with a session that is signed in (a real
`address`/`port`/`token` pointing at the dev PMS, `account_token` left empty since this is a
hand-seeded session rather than a QR sign-in — `Session::can_go_local()` only needs
`server.address`/`port` and a non-empty `pms_token()`, not `account_token`) and armed with
`plxnative-noidle` (required for `sim-shot` to ever present a frame on a screen that settles —
see the skill's "traps") and `plxnative-keymanager=<mode>`:

```sh
# per mode, once:
rm -rf /tmp/sim-km-<mode>
mkdir -p /tmp/sim-km-<mode>
python3 - <<'PY'   # writes /tmp/sim-km-<mode>/auth.json: client_id + server{name,machine_id,
                    # address=192.168.0.3,port=32400,token=<PMS_TOKEN>,origin=http://192.168.0.3:32400}
PY
touch /tmp/sim-km-<mode>/plxnative-noidle
printf '%s' '<mode>' > /tmp/sim-km-<mode>/plxnative-keymanager
```

Each "launch" is a **separate process**, same instance root (`SIM_DIR`), run three times in a row:

```sh
SIM_TDIR=/Users/gleblinnik/plx-fleet/issue75/target-sim SIM_DIR=/tmp/sim-km-<mode> SIM_FRAME=200 \
  make sim-shot
```

After each launch, the event log (`$SIM_DIR/plxnative-events.log`) and the runtime dir's file set
were inspected: `auth.json` (plaintext session vs. a `{"format":"plxnative-secure-session",...}`
envelope), `secure-storage.refused` / `secure-storage.proven` (the cross-launch markers),
`secure-probe.json` (the planted probe, removed once answered), and `plxnative-keymanager-key`
(the `healthy` mode's persisted fake key). `boot: stored session — local server (offline-capable)`
in the log is "landed on signed-in Home without a QR prompt".

## Results

| mode | launch | `keymanager:`/`session protection:`/`session:` lines | `auth.json` | markers / probe | signed in? |
|---|---|---|---|---|---|
| `perprocess` | 1 | `keymanager: FAKE service armed mode=perprocess`; `session protection: keymanager3`; `session: secure storage has not yet been proven on this install; keeping the 0600 file and probing` | plaintext | `secure-probe.json` planted | yes (Home) |
| `perprocess` | 2 | `keymanager: finish(decrypt) refused errorCode=-20030`; `session: secure storage is marked refused on this install; keeping the 0600 file` | plaintext | probe consumed → `secure-storage.refused` written | yes (Home, no re-sign-in) |
| `perprocess` | 3 | `session: secure storage is marked refused on this install; keeping the 0600 file` (no further keymanager calls — the marker short-circuits `seal_permitted`) | plaintext | `secure-storage.refused` persists | yes (Home, no re-sign-in) |
| `healthy` | 1 | `session protection: keymanager3`; `session: secure storage has not yet been proven...` | plaintext | `secure-probe.json` planted; `plxnative-keymanager-key` written (64 hex chars) | yes |
| `healthy` | 2 | `session protection: keymanager3` (a fresh save's own promotion round trip) — no `session: not yet proven` line this time | **becomes a secure envelope** (`891` bytes vs `518` plaintext) | probe reopened → `secure-storage.proven` written; probe removed | yes (Home, no re-sign-in) |
| `healthy` | 3 | no `keymanager:`/`session protection:` line at all (nothing needed sealing or opening beyond the ordinary read) | secure envelope (unchanged) | `secure-storage.proven` persists | yes (Home, no re-sign-in) |
| `stall` | 1–3 (identical each launch) | `session protection: no usable key manager; using the 0600 file fallback`; `session: secure storage has not yet been proven...` | plaintext | **no probe ever planted** — `seal()` fails at `generateKey` (the fake's `call()` returns `Err` immediately for every method), so `plant_probe`'s own `keymanager::seal` call returns `None` before it can write anything | yes, every launch |
| `refuse=-20099` | 1–3 (identical) | `keymanager: begin(encrypt) refused errorCode=-20099 (fake refusal)`; `session protection: no usable key manager; using the 0600 file fallback` | plaintext | no probe planted (same reason as `stall`: `seal()` fails before `plant_probe` gets an envelope to persist) | yes, every launch |
| `nocode` | 1–3 (identical) | `session protection: no usable key manager; using the 0600 file fallback` (no `keymanager: begin… refused` line — `log_refusal` only fires when `errorCode` is present, and `nocode`'s reply carries none) | plaintext | no probe planted | yes, every launch |
| `badoutput` | 1–3 (identical) | `session protection: keymanager3 sealed but could not open its own envelope — using the 0600 file fallback` | plaintext | no probe planted (`seal`'s own in-process `round_trips` check fails at `finish(decrypt)`'s non-base64 `output`, so `seal()` returns `None` before `plant_probe` runs) | yes, every launch |
| `absent` | 1–3 (identical) | `keymanager: keymanager3 is not on this firmware; using the 0600 file`; `session protection: no usable key manager; using the 0600 file fallback` | plaintext | no probe planted | yes, every launch |

## Reading the matrix against the spec

- **`perprocess` reproduces the reporter's failure exactly**, and the fix (the persisted refused
  marker) contains it after one extra launch rather than looping forever: launch 1 seals and
  round-trips in-process (its own promotion check uses the SAME process's key) and plants a probe;
  launch 2 is a fresh process with a fresh random key, so the probe — sealed by launch 1's key —
  fails `finish(decrypt)` with `errorCode -20030`, the real service's own tag-mismatch code, and the
  refused marker is written; launch 3 (and every launch after) short-circuits on the marker with no
  further keymanager3 traffic at all. **Nobody is ever asked to sign in again** — the session stays
  on the 0600 plaintext file across the whole sequence, which is the whole point of the fallback.
- **`healthy` shows the fix's other half**: a backend whose key genuinely survives a process
  boundary gets promoted (`secure-storage.proven`) on launch 2 and its session file becomes a real
  sealed envelope that same launch (a save landed after the promotion, with `has_proven_marker()`
  now true); launch 3 opens that envelope with no incident. Signed in throughout.
- **`stall`, `refuse=<code>`, `nocode`, `badoutput`, `absent` all fail closed identically across
  three launches**, which is itself the finding worth stating: none of them ever plants a probe at
  all, because in every one of these modes `keymanager::seal()` fails (at `generateKey` for `stall`/
  `refuse`/`nocode`/`absent`, at its own in-process round-trip check for `badoutput`) BEFORE
  `plant_probe` has a sealed envelope to persist — `plant_probe` calls `keymanager::seal` itself and
  bails out on `None`. So the cross-launch probe/marker machinery this issue is about is reachable
  only through `perprocess`/`healthy` (or any other mode whose `seal()` can succeed at least once);
  the other five modes exercise the **immediate**, single-launch fallback path (`seal_permitted`'s
  "no usable backend" branch) rather than the cross-launch one, every launch, identically. That is
  consistent with the code — `plant_probe`'s doc says as much — but it means `PROBE_MAX_ATTEMPTS`'s
  retry counter (for a probe that exists but gets `NoReply`/`Unreachable` on reopen) is **not**
  exercised by `stall` in this matrix, because `stall` never gets far enough to write a probe in the
  first place. Exercising that specific retry-counter path would need a probe planted under a
  working mode and THEN switched to `stall` for the reopen — a two-phase scenario this matrix did
  not run, since the task's per-mode launches keep the same mode for all three.

## No finding to report

No mode, across any of the 21 launches run (7 modes × 3), ever dropped the boot to the QR sign-in
screen or the who's-watching picker — every launch's log shows `boot: stored session — local
server (offline-capable)`. The reporter's failure (an endless silent-refusal loop that keeps
re-attempting a doomed seal) does not reproduce with the landed fix: `perprocess` settles after
exactly one refused launch and stays settled.
