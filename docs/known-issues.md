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

**Correction (2026-09-15, from a `/plan-eng-review` pass on `feature-trailers`): the ancestor check
below names the wrong commit, and the fix IS present on `main`/`feature-trailers` today.** The
paragraph and command this replaces said the fix lived only on `release/v0.6` as commit `ac305265`
and was "never merged back." That is only half true: `ac305265`'s exact content — confirmed
byte-identical, `diff <(git show ac305265:src/starfish.c) <(git show 6f3486d0:src/starfish.c)` and
the same for `rust-modules/src/webos.rs` both produce no output — was independently re-applied to
`main` as a **different commit, `6f3486d0`**, same author, same commit message, same timestamp
(2026-09-10 02:44:43 +0300 on both). `6f3486d0` **is** an ancestor of current `feature-trailers`
(`git merge-base --is-ancestor 6f3486d0 HEAD` → yes), and the live `src/starfish.c` on this branch
today has the full `g_load_returned`/`LOAD_RETURNED()`/`SET_LOAD_RETURNED()` gate, and
`rust-modules/src/webos.rs` has the full `devjail`/`rtkmem` probe — both read directly off the
current tree, not inferred. The `ac305265`-only ancestor check below is why an eng-review pass on
`feature-trailers` almost re-applied this fix a second time (`git cherry-pick -n ac305265` produced
conflicts across 20+ files entirely because of unrelated drift, not because the fix was missing —
aborted once this was found).

**This directly contradicts the "Empirically confirmed 2026-09-14" paragraph below, and that
contradiction is unresolved, not silently overwritten.** `6f3486d0` predates that device session in
the branch's own history (it sits before `1f82193e`, the commit that introduced this very file), so
a device build of "the current `HEAD` of `main`" on 2026-09-14 should already have carried the fix.
Either that device session built something other than what it intended to (a stale local checkout,
a different flavour/branch), or the fix is textually present but not actually effective on real
hardware for a reason not yet understood, or the empirical claim itself is mistaken. **Whoever picks
this up next: re-run the device reproduction (`tests/run.py --pipeline --only pipe_finish_eos` or a
real PMS Play press) against a genuinely fresh `feature-trailers`/`main` checkout before trusting
either paragraph** — the code-level evidence above is solid, but it has not been re-confirmed on the
television this file's whole premise is about.

**Why it's not on `main` (historical — the fix WAS eventually ported, see the correction above):**
`v0.6.1` (and every patch through `v0.6.6`) was cut from the `release/v0.6` maintenance line, not
from `main` — see the `cut-release` skill's `line: release/vX.Y` dispatch input. That line diverged
from `main` at `b074943f` and its 37 commits (issue #74's fix among them, plus #75/#76 sign-in
fixes, IPv6 redaction, sandbox-repair docs, DB8 persistence) were not merged back as a batch — issue
#74's fix specifically was re-applied to `main` separately, under commit `6f3486d0`, not by merging
or cherry-picking `release/v0.6` wholesale.

**Update, 2026-09-24: all 38 `release/v0.6` commits have been audited against `main`, and every
real fix is there.** #74's fix is `6f3486d0` as above; the 0.6.x persistence/sign-in/consent line
was independently re-implemented in #105. The audit found three real gaps, all ported in this
change: ambiguous bare-IPv6 redaction (`d1aeb238`), hardware context (television model, SoC,
hardware revision, webOS release, `rtkmem`/install facts) on handled playback-error events
(`c48770c7` — deliberately not extended to incident reports, since the consent-free one-press
report promises it identifies "only that one report — not you or this television"), and the
crash-log mark's identity binding plus bounded read (`e0a9fb36` — its event-id identity mixing was
not ported).

**Before you spend a device session on a k5lp/k3lp crash shaped like this one**, check whether your
branch already has the fix — check BOTH possible commits, since a re-applied fix gets a new hash:

```sh
for c in 6f3486d0 ac305265; do
  git merge-base --is-ancestor "$c" HEAD 2>/dev/null && echo "fix present via $c" && break
done || echo "fix MISSING under either known hash — this may be issue #74 for real, or a new bug; verify content, not just these two hashes, since a future re-application would get a third hash"
```

If missing under both: **do not** re-run `decompile-tv-lib` against `libpf`/`libplayerAPIs` to
indict LG's GStreamer bindings — that path was walked once already and produced a real Ghidra
finding (a `GenericPipeline::deepElementRemovedCallback` double-`gst_object_unref` on dispose) that
was true but was a *symptom* of the race, reachable only because of it, not an independent
unpatchable defect. Either merge or cherry-pick the fix (and ideally the rest of `release/v0.6` — it
is 37 commits of real fixes sitting nowhere but a tag) onto the branch you're testing, or restrict
k5lp/k3lp device testing to a build already past that commit. `devjail: soc=<name>
rtkmem=ok|missing|n/a`, on the `webos:` line near the top of the event log, says which chassis
you're on and whether the jail grants `/dev/rtkmem` — check it before assuming a crash is generic.

**Empirically confirmed 2026-09-14 (status: contradicted by the correction above, not yet
re-verified):** building the `v0.6.6` tag in an isolated worktree and deploying it to the `debug`
flavour on the affected television reproduced no crash — the event log shows a clean
`start_bufferfeed: refusing — this sandbox does not give the app /dev/rtkmem on this chassis` and
the app declines to play, instead of dying. This paragraph originally claimed the current `HEAD` of
`main` (and of `feature-trailers`, cut from it) still crashed on the identical scenario — see the
correction above for why that claim needs a fresh device session before being trusted either way.
