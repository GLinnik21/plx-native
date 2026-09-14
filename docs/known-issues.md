# Known issues — check here before you decompile firmware or blame LG

This file exists because one already-fixed bug got re-investigated from scratch on 2026-09-14: a
real device session, a Ghidra decompilation of three LG libraries, and a written conclusion that a
crash needed "an LG firmware fix or a different TV" — when the actual fix had shipped five days
earlier, on a branch nobody checked. Read this file's one entry before opening `decompile-tv-lib`
or writing off a crash as a firmware defect. Add a new entry here whenever that would have saved a
device session.

## `main` is missing 37 commits that shipped on `release/v0.6`, including a crash fix (issue #74)

**Symptom:** the app crashes reliably and immediately on every real playback attempt, on a
**Realtek k5lp or k3lp chassis** (`board=K5LP_ATSC` and similar). Two variants seen, both right
after the event log's `SMP loadCompleted (priming before Play)` line, before any
`setMediaVideoData sent` / video bind:

- SIGABRT, `*** SIGNAL 6`, immediately preceded in stderr by
  `GStreamer-CRITICAL: Trying to dispose object "omxh264dec", but it still has a parent "registry0"`
  (and the same for `dvovideosink`).
- SIGSEGV, `*** SIGNAL 11 addr=0x0`, registers `r0=0x0`.

It reproduces through `tests/run.py --pipeline --only pipe_finish_eos` (an ordinary H.264/AC3 mkv,
no Dolby Vision declared) just as reliably as through real PMS-driven Play presses, and it is
**debug-flavour-only in the field** — a `features=release` install on the same television plays
the same content without incident.

**This is not an LG firmware bug, and the fix already exists.** It is
[issue #74](https://github.com/GLinnik21/plx-native/issues/74), fixed in commit `ac305265`
(`fix(player): never drive Starfish while Load is still in flight, and refuse to Load under a k5lp
jail with no /dev/rtkmem`), released as **v0.6.1**. Root cause, from that commit's own account:
`src/starfish.c` marked the native session ready *before* the synchronous `Load` call returned, so
every main-thread verb (`isLoadCompleted`, `Play`, `Feed`, the ACB bind) was free to race a `Load`
still running on its own thread. On most sets the race window is harmless. On k5lp/k3lp, Developer
Mode's jail does not grant `/dev/rtkmem`
([webosbrew/webos-homebrew-channel#202](https://github.com/webosbrew/webos-homebrew-channel/issues/202)),
so `Load` hangs inside video-output init, stretching that window wide open — which is exactly when
the crash lands. The fix gates on a `devjail: soc=k5lp rtkmem=missing` probe and refuses to `Load`
at all in that case, plus closes the race for every other set.

**Why it's not on `main`:** `v0.6.1` (and every patch through `v0.6.6`) was cut from the
`release/v0.6` maintenance line, not from `main` — see the `cut-release` skill's `line:
release/vX.Y` dispatch input. That line diverged from `main` at `b074943f` and its 37 commits
(issue #74's fix among them, plus #75/#76 sign-in fixes, IPv6 redaction, sandbox-repair docs, DB8
persistence) were **never merged back**. `main`, and every branch cut from `main` since, still
carries the original race. Verified directly:

```sh
git merge-base --is-ancestor ac305265 main   && echo "on main" || echo "NOT on main"    # NOT on main
git merge-base --is-ancestor ac305265 v0.6.6 && echo "on v0.6.6" || echo "not on v0.6.6" # on v0.6.6
git log main..v0.6.6 --oneline | wc -l                                                   # 37
```

**Before you spend a device session on a k5lp/k3lp crash shaped like this one**, check whether your
branch already has the fix:

```sh
git merge-base --is-ancestor ac305265 HEAD && echo "fix present — this is a different bug" \
  || echo "fix MISSING — this is issue #74, known and already fixed elsewhere"
```

If missing: **do not** re-run `decompile-tv-lib` against `libpf`/`libplayerAPIs` to indict LG's
GStreamer bindings — that path was walked once already and produced a real Ghidra finding (a
`GenericPipeline::deepElementRemovedCallback` double-`gst_object_unref` on dispose) that was true
but was a *symptom* of the race, reachable only because of it, not an independent unpatchable
defect. Either merge or cherry-pick `ac305265` (and ideally the rest of `release/v0.6` — it is 37
commits of real fixes sitting nowhere but a tag) onto the branch you're testing, or restrict k5lp/
k3lp device testing to a build already past that commit. `devjail: soc=<name>
rtkmem=ok|missing|n/a`, on the `webos:` line near the top of the event log, says which chassis
you're on and whether the jail grants `/dev/rtkmem` — check it before assuming a crash is generic.

**Empirically confirmed 2026-09-14:** building the `v0.6.6` tag in an isolated worktree and
deploying it to the `debug` flavour on the affected television reproduced no crash — the event log
shows a clean `start_bufferfeed: refusing — this sandbox does not give the app /dev/rtkmem on this
chassis` and the app declines to play, instead of dying. The current `HEAD` of `main` (and of
`feature-trailers`, cut from it) still crashes on the identical scenario.
