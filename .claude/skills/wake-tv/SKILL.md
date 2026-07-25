---
name: wake-tv
description: >
  Wake the LG webOS dev TV from standby via Wake-on-LAN and wait for
  SSH — run this whenever the TV is unreachable, asleep, timing out, or "connection
  refused / operation timed out" before a deploy, test run, capture, or stream session.
  Also: put the TV into standby, or check whether it is up.
---

# wake-tv — wake the dev TV before touching it

The LG 49SM9000PLA (rooted, webOS 4.5) **drops to standby after a few idle minutes**.
Every automation session against it — `make deploy` / `make test`, `tests/run.py`,
`tools/capture-screen.sh`, `tools/stream-screen.py` — dies with SSH timeouts when it's
asleep. Wake it first; don't wait for it to wake itself (it won't).

All paths below are relative to the repo root.

## Run (the only path)

```bash
.claude/skills/wake-tv/wake-tv.sh            # wake + wait for ssh; no-op if already up
.claude/skills/wake-tv/wake-tv.sh status     # one probe: UP/DOWN (exit 0 = up)
.claude/skills/wake-tv/wake-tv.sh standby    # clean standby (for testing the cycle)
```

Verified live (three real cycles): wake from **overnight** standby took **18 s**;
wakes from a **fresh** standby took **6 s** and **1 s**. Budget up to ~2–3 min for a
long-cold TV (observed once in earlier sessions) — the driver resends the magic packet
every ~20 s and polls SSH every 3 s, default timeout 180 s (`WAKE_TIMEOUT=300` to
extend). Exit 0 = SSH answers; exit 1 = gave up.

Config resolves at runtime — no network details are stored in the skill. The host
comes from `$TV_HOST`/`$TV`, else the `Makefile`'s `TV` value (the single source of
truth). The MAC needed for the magic packet comes from `$TV_MAC`, else the gitignored
`.tv-mac` cache, else it is read from the ARP table while the TV is reachable and
cached for next time — so the first `wake-tv.sh status` against a live TV is enough to
arm future wakes. `TV_USER` (root) and `WAKE_TIMEOUT` (180) are the other overrides.

No prerequisites beyond macOS built-ins (`python3` broadcasts the WoL packet — there is
no `wakeonlan` binary on a stock Mac) and working SSH auth to the TV.

## Gotchas — what standby BREAKS (why you run this first)

- **A deploy can die mid-scp** when the TV sleeps under it. After any interrupted
  `make deploy`, md5-compare before trusting the binary:
  `md5 -q pkg/plxnative` vs `ssh root@TV 'md5sum .../plxnative'`.
- **Standby kills reverse SSH tunnels** (`ssh -R`) and can leave the TV-side
  dropbear holding the stale listen port — the next `-R` fails with
  "remote port forwarding failed". Kill the stale per-connection dropbear on the
  TV (`netstat -tlnp | grep <port>` → kill that pid), then reconnect.
- **Standby closes the app**, so the capture stream port (:8910), the remote FIFO,
  and any luna-send `-i` launch subscription are gone — relaunch the app after waking.
- **SSH auth: key first, `sshpass` fallback.** This machine authenticates with an
  installed key, which is why the driver uses `BatchMode=yes`. The `Makefile`'s
  `sshpass` path is the fallback for a machine without the key — both work; the key
  simply wins when present.
- **This webOS 4.5 build's power method is `power/powerOff`** —
  `power/turnOff` (newer webOS docs) returns `Unknown method`.
- **`luna-send` silently no-ops without a controlling TTY** — on-TV calls are wrapped
  in `script -qc "…" /dev/null` (the house pattern; plain `ssh` stays binary-safe).
- **The powerOff ssh session hangs** without keepalives: the link drops while the
  session is open and TCP waits minutes. The driver uses `ServerAliveInterval=3`
  so it errors out in ~6 s after the call has already fired; the poll loop is the
  real confirmation.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `TV did not answer within 180s` | TV may be hard-off at the mains or on a different network segment. Check it's plugged in; retry once (`WAKE_TIMEOUT=300`). |
| `Permission denied (publickey,password)` | You're on a machine whose key isn't on the TV. Add your pubkey to `/home/root/.ssh/authorized_keys` from a trusted machine (or rely on the Makefile's `sshpass` fallback). |
| `no MAC for the magic packet` | First run against a sleeping TV with no cache. Wake it by hand once and run `wake-tv.sh status` to learn+cache the MAC, or set `TV_MAC=` explicitly once. |
| Wake works but `make deploy` still fails | The deploy raced the wake-up services; retry the deploy, then md5-compare (see Gotchas). |
