---
name: tv-lock
description: >
  Take, hold and hand back the ONE dev television, so two jobs never drive it at once. Use before
  ANY device work — "deploy to the TV", "run the test suite", "screenshot the TV", "reproduce it
  on device", "measure the frame rate", "capture the video plane" — and whenever a command is
  refused with "the television is held by another lane", "BLOCKED: this lane does not hold the
  television's lock", or a tool says somebody else is on the set. Also covers running SEVERAL
  agents at once against one TV ("fan this out", "run these in parallel", "another agent is using
  the TV"), what to do while you wait, and when it is safe to break a lease. There is one set, one
  app instance, and no OS-level mutex: two jobs on it do not fail cleanly, they produce plausible
  WRONG data that reads exactly like a real regression.
---

# The television is a mutex — this is how you hold it

There is exactly one dev set, one app instance on it, and webOS enforces nothing. Two
`tests/run.py` runs, or a run plus a `make deploy`, or a capture session plus either, **kill each
other's app**. The damage is never a clean failure. It is:

- a `timeline_climb` that failed because the other lane closed the app mid-case;
- an fps number measured while somebody else's binary was landing under it;
- a capture of a screen the other job navigated to;
- an event log graded as yours, written by their session.

None of those can be told from a real regression by reading them. That is the whole reason this
exists: sequencing by hand only works while one person is doing the sequencing, and "you own the
television exclusively" is a sentence that is true when written and false the moment a second lane
starts.

## The loop

```bash
tools/tv-lock.sh status                             # who has it, and is anybody on it unlocked
tools/tv-lock.sh acquire --why "verify HUD change"  # take it (add --wait 540 to queue for it)
  … device work: tools/tv-session.sh up, ./tests/run.py, make deploy, captures …
tools/tv-session.sh down                            # hand the APP back (interactive boot)
tools/tv-lock.sh release                            # hand the TELEVISION back
```

One-shot jobs get the whole thing in a single command, released even on Ctrl-C:

```bash
tools/tv-lock.sh with --why "fps suite" -- ./tests/run.py --fps
```

**Take a lease for the SESSION, not per command.** Every TV-facing tool already takes a short
implicit lease when nobody holds the set, so a lone `make deploy` cannot collide — but that lease
ends with the command, and *the gap between two of your own commands is exactly where another lane
lands*. If you are going to touch the set more than once, acquire first.

| command | what it does |
|---|---|
| `status` | holder, how long held, how long left, why — plus the unlocked-user pre-flight |
| `acquire [--why T] [--wait S] [--ttl MIN] [--as LABEL]` | take it; `--wait` polls instead of failing |
| `renew [--ttl MIN]` | extend a lease you hold (long sessions; `with` does it for you) |
| `release` | hand it back |
| `require [--quiet] [--advisory]` | assert + renew; what the tools call, rarely typed by hand |
| `with [opts] -- CMD…` | acquire → run → release, through Ctrl-C, SIGTERM and a crash |
| `break [--yes]` | steal a lease; names the holder first and refuses a LIVE one without `--yes` |
| `selftest` | exercises the entire protocol against a temp dir — **no television involved** |

Default lease: **45 minutes**, renewed automatically whenever a tool uses the set, so a real
session never ages out under itself. The implicit one is **10 minutes**.

## It is enforced, not advisory

Four layers, so there is no "I forgot" path:

1. **`tools/tv-session.sh`** — `up`, `key`, `click`, `shot` and `down` require the lock. `log` and
   `status` are read-only: they take nothing and refuse nothing, but they name the holder, because
   reading a log another lane's app is writing is how a session gets graded as your own.
2. **The Makefile** — `deploy`, `run`, `run-stream`, `kill`, `install` and `uninstall` take
   `tv-lock-require` as a prerequisite (last in the list for `deploy`, so a two-minute FFmpeg build
   does not happen while holding the set).
3. **`tests/run.py`** — acquires where it commits to driving the TV and releases in the same
   `finally` as the teardown; it *inherits* a lease this lane already holds rather than re-taking
   it, so `with -- ./tests/run.py` works unchanged. `tools/capture-screen.sh` requires it too.
4. **A `PreToolUse` hook** (`.claude/hooks/tv-lock-guard.py`) — refuses any Bash command that
   drives the set without a lease: a raw `ssh root@…`, an `scp` into the app directory, a
   `sshpass` one-liner, `make deploy`, `tests/run.py`. It reads one local file, never the network,
   and blocks nothing host-side.

## When it refuses

**Do not work around it.** A refusal means a real job is on the set; going around it corrupts
*both* results, and yours will look like a regression rather than an accident.

1. **Look**: `tools/tv-lock.sh status` — it prints who, from which checkout, since when, why, and
   how long is left.
2. **Queue**: `tools/tv-lock.sh acquire --wait 540 --why "…"` polls every 5 s. Keep it under ~9
   minutes so it fits inside one tool call; re-run it if it times out.
3. **Meanwhile, do the host half.** Most work does not need a television:
   - `make check` — the host unit suite, sub-second;
   - **`make sim`** — the real app core on macOS against the real PMS, screenshotting itself, and
     **N instances run at once**. Layout, focus, navigation, every screen and the whole Plex data
     layer are answerable there. See the **`ui-sim`** skill. It cannot answer frame rate, text
     rasterization, or anything about video — those, and only those, need the set.
4. **Break it only when it is a corpse**: `tools/tv-lock.sh break` refuses a live lease and tells
   you how long it has left; `break --yes` takes it anyway. An expired lease needs no break at all
   — the next `acquire` takes it and says whose it was.

## Two things the lock cannot see

- **A human watching television.** The lock knows about jobs, not about the household. `status`
  therefore also runs the old pre-flight — `fuser` on **both** installs' own binaries (inode-scoped;
  `pidof plxnative` matches both, since both binaries carry that name) and a count of ssh sessions
  on the set. `N ssh sessions (one is mine)` is the check that actually fires, and it sees machines
  whose processes you cannot: read the warning, do not dismiss it as your own connection.
- **Work started before the lock existed**, or from a checkout without these tools. Same warning,
  same rule.

## Fleets

When farming device work out to several agents, **the television is the scheduling constraint, not
a detail**. Give exactly one lane device access at a time and say so in the other prompts; run the
rest host-only or on the simulator. The lock now makes the second lane *fail* instead of silently
corrupting the first, but a fleet that all queue on one set is still a fleet running in series —
plan the work so only one lane needs the device.

A lane is a **checkout**: the lease belongs to the worktree, so every Bash call, `make` and nested
tool inside it inherits the same lease, and a second worktree on the same Mac is a different lane.

## Under the hood (enough to debug it)

- The lock is a **directory on the television**, `/tmp/plx-tv.lock`, holding one `owner` file.
  `mkdir` is the atomic create-if-absent primitive that works on the set's busybox userland; the
  whole decision is taken on the TV in one round trip, never as a read here and a write back.
- It lives on the **device** because the device is the resource — a host-side file cannot see the
  second worktree, the second Mac, or the colleague on the sofa. Under `/tmp` because a TV reboot
  is the one event that also makes every holder's session meaningless.
- The name deliberately does **not** start with `plxnative-`: for the stable install the app's
  runtime root *is* `/tmp`, and any file there with that prefix marks the boot as automated and
  suppresses the who's-watching picker (`dev::any_trigger_present`). It is also outside the glob
  `make run` and `tests/run.py` clear, so a teardown cannot drop somebody's lease.
- **Every timestamp is the host's.** pmlog's wall clock on this set runs ~3 h off, so a lease
  minted from the TV's own `date` would expire in the past or three hours late.
- Each lane keeps a local mirror of its lease under `~/.plxnative/tv-lock/`, which is what makes
  the hook free and what lets `require` skip the round trip for a minute. The television always
  holds the authority; the tools reconcile the two on every use.
- `tools/tv-lock.sh selftest` runs the real protocol text under a local `sh` against a temp
  directory: who wins a contended acquire, that a live lease is not stealable, that an expired one
  is, that release is owner-scoped. Run it after touching the tool — it needs no television.
- `python3 .claude/hooks/tv-lock-guard-test.py` grades the hook's classifier the same way, and
  **half its cases are false positives** — `pgrep -fl "…|make deploy"`, a heredoc that documents
  the lock, a commit message that mentions it. That is the guard's real failure mode: refusing
  work that never touches the set teaches the reader to reach for the bypass. Add a case there
  before widening what the guard matches.

The escape hatch is `PLX_TV_LOCK_BYPASS=1 <command>`, which both the tools and the hook honour. It
is for a human who knows the set is theirs. Reaching for it because a lock said no is the one move
this whole mechanism exists to prevent.
