---
name: upstream-pr
description: >
  Prepare a FinNative change for a pull request to PlxNative (GLinnik21/plx-native) so the
  maintainer can merge it as it is, instead of reworking it first. Use whenever a generic fix is
  about to go upstream ("open the upstream PR", "send this to plx-native", "make it upstream-able",
  "port this fix to PlxNative"), and before asking for review on a PR already open there. It is the
  maintainer's own rework of our merged PRs, written down: what he changed on #839, #937, #1018 and
  #1034, and how to check a branch for the same things before he has to.
---

# Upstream PR

FinNative's generic fixes land on our `main` first and go upstream as one commit on a branch of
the fork cut from upstream's `main` (`AGENTS.md`, "Upstream is merged in"). The maintainer merges
good fixes, but on every PR of ours he has pushed "Simplify:" commits first. Behaviour stayed the
same; the shape changed. Each item below is one he made, and each is cheaper for us to make
before he reads the branch.

## Before opening (or before asking for review)

1. **Merge upstream's `main` into the branch** and run the changed crates' tests on the result. He
   merged 62 commits into #937 himself, and fixed a call that `main` had changed (#921's `version`
   argument to `request_play`).
2. **Read upstream's recent commits in the files you touch** (`git log upstream/main -20 --
   <paths>`). A helper a PR just added is the one he expects you to call: #1034's failure sites had
   to go through #937's `fail_engine_for_good`, merged the day before.
3. **Run `/simplify` on the diff**, then check the list below by hand.
4. **Run the `doc-claim-auditor` agent.** He removed a "Dolby Vision turns off" note that #839 made
   false, and recorded #839's server measurement in `docs/pms-api.md`.

## What he changes

- **A new behaviour rides an existing path.** #1034's native-audio retry restarted the play from
  the app loop; he moved it onto the pre-picture recovery flight every failed open already uses
  (`open_failure_action`), so the viewer never sees the error screen and the frame thread never
  waits on the server. Before adding a retry, a loop or a second route, find the one that exists.
- **One owner for each piece of state.** #1034 kept learned refusals in a preference, a static and
  per-rebuild `Box::leak` snapshots; he made the preference the only list, changed through one
  method (`Session::with_native_audio_refusal`), with the capability value derived from it. No new
  statics, no leaks, no second copy.
- **Pass values, don't read globals.** Upstream is moving every planner and machine to values it is
  handed (#1054: one capability value per playback request; `ci/allow/globals/` only shrinks). A new
  `devcaps::caps()` or other process-wide read inside a machine is a step backwards.
- **A repeated sequence becomes one named step.** #937 and #1018 each open-coded a three-call
  sequence in two places; he named it once (`fail_engine_for_good`, `claim_parked_foreground`).
- **No redundant guards.** #1018's `awaiting_load()` check was already implied by
  `claimable_clock().is_none()`. Check each condition against the others before keeping it.
- **Plain Rust.** Early returns over a second check inside a combined guard; `(left > 1)
  .then_some(..)` over an `if`; build a value only on the branch that uses it.
- **A recovery records itself like its siblings.** #1034's conversion reopened the engine without
  a Recovery attempt in the ledger, which the other recovery flights record.
- **Comments sit with the code they describe.** He moved #1018's call out of a comment it had been
  spliced into, and gave it its own line.

## What he keeps

Measurements, evidence tables, tests and the reasoning in commit messages. He thanked us for the
C1 packet-size measurement and the per-firmware table on #1034. Keep doing those.

## The PR itself

- One commit, titled `Area: what changes for the viewer`, with the account in the body, as the
  rest of upstream's history reads.
- Say what was run on a television and what was not. He asks for a C1 check when he could not run
  one.
- Credit work ported from elsewhere in the commit (a `Co-Authored-By:` line and the source commit).
- Name the upstream PR in FinNative's copy of the commit, so the next merge from upstream shows
  which fixes came back.
