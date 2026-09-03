---
name: fleet-plan
description: >
  Plan and launch parallel agent work in this repo — several agents, several git worktrees, one
  physical television. Use when the ask is "fan this out", "run these in parallel", "use several
  agents", "spin up a fleet", "split this across worktrees", "can multiple agents work on this at
  once", and equally when nobody said any of that but you are about to launch a second worker
  yourself. Covers who gets the TV (at most one lane), the shared stash stack that hands one lane
  another lane's work, what a second build tree costs on disk, cutting a worktree from the right
  base, which gitignored files a lane has to be seeded with, and the block to paste into every
  worker prompt. Holding the television is the `tv-lock` skill's job; this one decides which lane
  is allowed to ask for it. Workflow agents are unreachable mid-run, so everything a lane needs
  must be in its prompt at launch — which is exactly what gets forgotten at the moment somebody
  decides to parallelise.
---

# fleet-plan — N agents, N worktrees, ONE television

Four things go wrong here, all four have gone wrong here, and all four are decided **before the
first worker starts** — after that you cannot reach a workflow agent to correct it. The single
most useful thing in this file is [the worker-prompt block](#the-worker-prompt-block): paste it
into every lane, with its three blanks filled in.

**This file does not document the lock.** `tools/tv-lock.sh`'s subcommands, its lease semantics,
when a lease may be broken and what the `PreToolUse` hook refuses are the **`tv-lock`** skill, and
keeping a second copy of them here is how both copies rot. This one answers the question the lock
cannot: *which lane is allowed to want the television at all.*

## First: should this be a fleet at all?

Fan out only when **every** line is yes. Otherwise do it yourself — a fleet on the wrong shape of
work is slower *and* riskier, because it adds worktree setup, disk, an integration merge, and four
hazards, to buy parallelism that isn't there.

| | fan out | do it yourself |
|---|---|---|
| files touched | lanes touch **disjoint files** | one file, or one module everyone edits |
| dependencies | lanes compile independently | B needs a symbol A is writing (see [base](#3-cut-each-worktree-from-a-named-base)) |
| verification | host — `make check`, the simulator | **all of it needs the television** |
| shape known | the partition is obvious now | still exploring; you'd be guessing the split |

That third row is the one people talk themselves past. The TV is a mutex, so N lanes that all need
it finish **no faster than one** — they queue on `tools/tv-lock.sh` — while each still pays a
worktree, a build tree and a merge. Three lanes of genuinely independent, host-verifiable work is
where this starts paying.

## 1. Give the television to AT MOST ONE lane

**Telling two prompts "you own the television exclusively" is not a mutex.** Each sentence is true
when it is written and false the moment the second lane starts. That exact mistake was made on
**2026-08-21** — a blur measurement and a Dolby capture running at once — and it was caught by luck
rather than by anything failing loudly. `tools/tv-lock.sh` came out of a second collision on
**2026-08-22** and turns the second lane into a refusal instead of a corrupt measurement. But **the
lock schedules; it does not plan.** Two lanes that both want the set still run in series, and that
queue is invisible in the plan you wrote.

- **A LANE IS A CHECKOUT** — `tools/tv-lock.sh:62`, `LANE="${PLX_TV_LOCK_LANE:-$REPO}"`. What that
  means for planning: **a second worktree on the same Mac is a second lane**, however the prompt
  describes it. Do not set `PLX_TV_LOCK_LANE` to make two worktrees share one lease — that is
  spelling "we are one lane" at the mechanism whose entire job is to disagree.
- **`FLAVOR` does not buy you a second lane.** Two installs live on one set, but
  `docs/two-installs.md` §3.2: *"One hardware video plane and one decoder… Two installs cannot play
  at once."* The lease is one directory on the television, with no flavour in it — deliberately.
- **Everyone else goes to the simulator** — the **`ui-sim`** skill. N instances run at once, and it
  answers layout, focus, navigation, every screen and the whole Plex data layer. Give each lane
  **its own `SIM_DIR`**: it defaults to `/tmp/plxnative-sim` (`Makefile:845`) and is passed straight
  through as that instance's `PLXNATIVE_RUNTIME_DIR`, so two lanes on the default share one token
  file, one remote FIFO and one event log.
- Say the assignment **out loud in every prompt**, including the lanes that don't get it. "No
  device access" is information a worker acts on; silence is a worker that tries `make deploy`,
  gets refused by `.claude/hooks/tv-lock-guard.py`, and reaches for a raw `ssh root@…`.

## 2. Never let a lane `git stash`

**The stash stack is shared across worktrees, and a pop takes whatever is on top — including
another lane's work.** `refs/stash` is a plain repo-wide ref; it is not one of git's per-worktree
refs, so both lanes see one list. Reproduced end to end on **git 2.50.1, 2026-08-23**, in two
throwaway worktrees:

```
lane A: git stash push -m lane-a     # stash@{0} = lane-a
lane B: git stash push -m lane-b     # stash@{0} = lane-b, lane-a is now @{1}
lane A: git stash pop                # -> lane A's tree now contains lane B's change, and THAT
                                     #    entry is dropped. A's own work is still on the stack;
                                     #    B's next pop takes it.
```

**Prevention — commit, don't stash.** A commit on the lane branch is private to the lane, survives
a crash, and is what the integrator is looking for anyway:

```sh
git add -A && git commit -m "wip"    # -A, because `git stash create` below captures TRACKED files only
```

**Recovery, if a lane stashed anyway** — pin the entry to a ref of its own and get it off the
shared stack, before another lane pops it:

```sh
git update-ref refs/rescue/lane-a "$(git rev-parse stash@{0})" && git stash drop
git stash apply refs/rescue/lane-a         # restores, from any worktree in the family
```

To stash *safely* in the first place, skip the stack entirely — `git stash create` returns a commit
object it stores nowhere:

```sh
git update-ref refs/rescue/lane-a "$(git stash create 'lane-a wip')"
```

Both forms verified the same day: `git stash list` stays empty throughout, and `apply` restores the
right tree in a *different* worktree from the one that made it. What `git stash create` does not
capture is **untracked files** — a new module or a new test file is not in that commit — which is
why `git add -A && git commit` is the default advice and this is the fallback.

**`refs/rescue/*` is a shared namespace too, and nothing prunes it.** Name every pin after the lane
that made it, and delete pins **by name**. Do not sweep the namespace: on **2026-08-23** a
`for-each-ref refs/rescue | update-ref -d` cleanup written to remove two refs from the current
session removed five older ones with it (`unit-b-wip`, `unit-c-wip`, `unit-e-wip`,
`pre-dv-scrub`, `main-wip-2026-08-22`), and a rescue ref is the *only* thing holding its commit —
delete it and the object is unreachable, recoverable only by matching `git fsck --unreachable`
output against commit subjects, and gone for good once `git gc` prunes it (two weeks after the
object was written, by default).

## 3. Cut each worktree from a named base

Recorded **2026-08-21**: a lane was branched from an unrelated `backdrop-blur` commit instead of the
integration branch, so the symbol its task depended on did not exist.

```sh
MAIN=/Users/gleblinnik/Developer/plex/plex-native-poc
git -C "$MAIN" fetch origin                              # `git log --all` reads only refs you HOLD
BASE=$(git -C "$MAIN" rev-parse origin/main)             # or the integration branch
git -C "$MAIN" worktree add -b fleet/<lane> "$MAIN/.claude/worktrees/<lane>" "$BASE"
```

`.claude/worktrees/` is gitignored (`.gitignore:31`) and is where this project's agent worktrees
already live — three of them on 2026-08-23, beside the main checkout. Verify **from inside the
lane**, before any work; the worker prompt makes this the first command:

```sh
git log --oneline -1                                     # must be the base the prompt names
git merge-base --is-ancestor <BASE> HEAD && echo "base ok"
```

**The symptom of a wrong base is a compile error naming something the prompt promised exists** —
`cannot find function … in this scope`, an unresolved import, a `make check` failure in tests the
lane never touched. The expensive reaction is the natural one: the agent writes the missing symbol
itself, and the integrator gets two definitions of it. Hence "refuse to start", not "work around
it".

## 4. `make check` only — a full build fills the disk

Every worktree gets its own cargo target dir per feature set — `RUST_TDIR` = `target` /
`target-release`, `SIM_TDIR` = `target-sim`, `MACAPP_TDIR` = `target-macapp`, all under
`rust-modules/` — and its own `vendor/ffmpeg-prefix`, though since 2026-09-03 the expensive half
of that last one is shared (below). **`make disk` is the current number for every checkout at
once; take it from there rather than from the table below**, which is a record of one afternoon
and has already been overtaken twice.

Measured on this Mac, **2026-08-23**:

| | |
|---|---|
| `du -sh .` at the repo root | **27 GB** — and read the next two rows before believing it |
| ⤷ of which `.claude/worktrees/`, the three existing lanes | 9.2 GB |
| ⤷ the main checkout's own content | 17 GB |
| `rust-modules/target` (`debug/incremental` 6.2 G + `debug/deps` 5.9 G) | 14 GB |
| `target-sim` / `target-release` / `target-macapp` | 2.1 GB / 321 MB / 125 MB |
| `vendor/` (`ffmpeg-build` 379 M + `ffmpeg-prefix` 3.9 M) | 383 MB |
| the three existing lanes, individually | 1.2 / 3.3 / 4.8 GB |
| ⤷ what the 3.3 GB one is: `target` 2.5 G + `target-sim` 589 M + `vendor` 141 M | |
| free on this volume | 48 GiB |

Two of those rows are counter-intuitive. **A fleet worktree under `.claude/worktrees/` is inside
the main checkout**, so `du -sh .` at the root bills you for every other lane as well — 9.2 of that
27 GB is three lanes, and it comes back only when they are removed. And the 14 GB is months of
accumulated host *and* cross artefacts in one tree, not the price of one `make check`: **the
incremental cache alone is 6.2 GB** and grows without bound. The honest per-lane figure is the
1.2–4.8 GB row. The recorded failure was **7 lanes × ~10 GB against a 72 GB margin**; the margin
today is 48 GiB.

**Measured again 2026-09-03, twelve lanes in, and the shape had changed enough to act on: 45 GB
across the family, on a volume with 3.2 GiB free.** The breakdown is the part worth carrying,
because it contradicts the thing everyone reaches for first — **FFmpeg was 2.6 GB of it, 6%**,
while `target*/debug/incremental` alone was **24 GB, 53%**, at 1.2 to 4.0 GB per lane. A compile
cache, sized larger than the object code beside it, in trees that exist for one task each.

Three things came out of that measurement and they are the current state of this section:

- **`make disk`** (`tools/build-gc.sh`) reports every checkout's derived trees, the external lane
  trees under `$PLX_FLEET_DIR`, and the free space, in one table; `tools/build-gc.sh
  --orphans | --incremental | --lanes | --all` reclaims. Nothing it deletes is anything but `make`
  output. Run it when a lane starts failing for space, before launching a fleet, and `--orphans`
  after tearing one down.
- **A linked worktree no longer writes an incremental cache at all** — the Makefile sets
  `CARGO_INCREMENTAL=0` when `.git` is a file rather than a directory, so a lane pays object code
  and nothing else. The main checkout keeps its cache. `CARGO_INCREMENTAL=1 make check` in a lane
  overrides it, which is the right call only for a lane genuinely doing long iterative work.
- **The FFmpeg build tree is machine-wide and keyed by its configure flags**, under
  `$PLX_BUILD_CACHE` (default `~/.cache/plxnative`). See the vendor bullet below: the manual
  symlink this skill used to prescribe is no longer needed, and the hazard it carried is gone.

**The rule: workers run `make check` and nothing that cross-compiles. ONE integrator does the
cross-build, once, at the end.** `make check` is `make lint` (three named clippy lints) plus
`cargo test --lib`, `ci/flavor.py --selftest` and `tests/test_harness.py` — all four invoke their
tool directly, so none of them enters `ci/build-ffmpeg.sh`. The FFmpeg build is reached down exactly
one chain — `pkg/plxnative` → the Rust staticlib → `pkg/.ffabi-ok` → the header rule at
`Makefile:416` — which means a bare `make`, `make all`, `make deploy`, `make ipk` and `make test`
all build it and `make check` cannot. (`make macapp` does **not**: `ci/mkmacapp.py` never mentions
FFmpeg. It costs a fourth cargo target dir, 125 MB, and nothing else.)

Keep each lane's build trees **outside** the worktree:

```sh
export CARGO_TARGET_DIR=$HOME/plx-fleet/<lane>/target       # governs `make check` — it passes no --target-dir
export SIM_TDIR=$HOME/plx-fleet/<lane>/target-sim           # `make sim` DOES pass --target-dir, which wins
```

Give each lane its **own** path: one shared dir makes concurrent cargo runs block on the target
lock and re-fingerprint each other's sources.

**That does not save disk** — the bytes move, they do not vanish. What it buys is that
**`git worktree remove` stays meaningful.**

**And moving them is how they become permanent, which is the failure this advice caused.** A tree
under `$HOME/plx-fleet/<lane>` outlives its worktree by construction: remove the lane and the
gigabytes stay, owned by nobody, named for a branch that no longer exists. Found 2026-09-03 by
teaching `tools/build-gc.sh` to look there — **36 GB across ten dead lanes** (`cards`, `deploy`,
`detail`, `glass`, `integ`, `labels`, `person`, `routes`, `search`, `settings`), every single one
an orphan, none of them visible to a `du` at the repo root or to any earlier version of that
script. So: **`tools/build-gc.sh --orphans` after every fleet**, which deletes exactly the
external trees whose worktree is gone and touches no live lane. `make disk` lists them with an
ORPHAN marker. Build output is
untracked, so a lane with a target dir inside it always needs `--force` — and `--force` deletes
uncommitted *source* changes just as happily (verified 2026-08-23: a plain `remove` refuses with
`contains modified or untracked files`; `--force` took the tree, modified tracked file and all).
With the build trees elsewhere, a bare `git worktree remove` is a free assertion that the lane
committed everything. `Makefile:850` has a second reason: a checkout on a network or external
volume cannot be a cargo target dir at all, because those filesystems have no `flock`.

## Seed each lane with the gitignored files it needs

Only two are worth copying, and the list is shorter than it looks because the tooling already
reaches back to the main checkout:

```sh
WT=$MAIN/.claude/worktrees/<lane>
cp "$MAIN/src/config.local.h"        "$WT/src/"          # PMS host + token
cp "$MAIN/tests/manifest.local.json" "$WT/tests/"        # only for ./tests/run.py --server
```

- **`src/config.local.h`** is the one a host-only lane still needs. `make sim-run` and
  `make sim-shot` read `PMS_HOST` out of it (`Makefile:843`) and die `no PMS host` without it —
  `make sim` alone only builds, so the failure arrives one command later than you expect;
  `make sim-token`, `tools/tv-session.sh` and `tests/run.py` read `PMS_TOKEN` from it. **So every
  lane carries its own copy of a real X-Plex-Token**, which is exactly why `.gitignore` names it in
  the *tracked* file — read the comment there: it used to be held out by `.git/info/exclude`, which
  is local-only and *"does not apply in a fresh worktree"*, i.e. one `git add -A` from a live
  credential in a public repo's history. `git worktree remove` the lanes when the fleet is done and
  the copies go with them.
- **`tests/manifest.local.json`** only for `--server`. The default (synthetic) tier of
  `tests/run.py` runs with no overlay at all.
- **Do NOT copy `.tv-host`.** The Makefile's `TV` (`Makefile:52`) and `tools/tv-lock.sh:96` both
  fall back to the main checkout's copy via `git rev-parse --git-common-dir`, and `wake-tv.sh` /
  `tools/tv-session.sh` ask `make -s print-tv`. The one gap is `tests/run.py`, which reads
  `REPO_ROOT/.tv-host` with no such fallback (`tests/run.py:85`) — the device lane either copies it
  or passes `--tv`.
- **Do NOT copy `.tv-mac`.** It is a cache; `wake-tv.sh` re-derives it from the ARP table.
- **Do NOT `cp -R "$MAIN/vendor" "$WT/vendor"`.** The destination exists, so that writes
  `vendor/vendor/` — and doing it while seeding a fleet once put **30,247 build-artefact files
  (280 MB, plus the builder's MAC addresses and home path in FFmpeg's configure logs) into a branch
  bound for a public repository**. The `.gitignore` entry that now catches it says so. **You no
  longer need to do anything at all**: since 2026-09-03 `ci/build-ffmpeg.sh` puts the 122 MB source
  and object tree in a machine-wide cache under `$PLX_BUILD_CACHE` (default `~/.cache/plxnative`),
  keyed by the configure flags, so a lane that cross-builds compiles nothing and copies out a
  3.8 MB prefix. Measured the day it landed: a cold worktree's `make pkg/.ffabi-ok` went from
  ~2 minutes and 122 MB to **3 seconds and 3.8 MB**.

  The `ln -s "$MAIN/vendor/ffmpeg-prefix" "$WT/vendor/ffmpeg-prefix"` recipe this skill used to
  give is therefore obsolete — and it is worth knowing WHY it was never quite safe, because the
  cache is keyed precisely to fix it. A symlinked prefix is shared across configurations, and
  `RELEASE=1` drops swscale and the mpeg1/mpegts pair: a release lane rebuilding through the link
  silently replaced a dev lane's libraries, and the Makefile's configuration stamp — which deletes
  a header *inside* the prefix to force a rebuild — reached through the link into the other lane's
  tree to do it. Different flags now hash to different cache keys, and each checkout keeps its own
  real prefix directory, so neither half can happen.

## The worker-prompt block

Fill in `<BASE>`, `<lane>` and the device line, paste verbatim into **every** lane. Workflow agents
cannot be reached once they are running — a hazard you meant to mention is a hazard that ships.

```markdown
## Fleet rules for this lane — before your first command

1. BASE. Run `git log --oneline -1`. If it is not `<BASE>`, STOP and say so; do not start, and do
   not write a missing symbol yourself — the worktree was cut wrong.
2. THE TELEVISION: <this lane has NO device access | this lane owns the TV>. With no access: no
   ssh/scp/sshpass, no `make deploy|run|test|kill|install`, no `tests/run.py`, no
   `tools/tv-session.sh`, no `tools/capture-screen.sh` (a PreToolUse hook refuses them). Verify on
   the simulator — the `ui-sim` skill — with your own root: `make sim-shot SIM_DIR=/tmp/sim-<lane>`.
3. DISK. `make check` ONLY. Never bare `make`, `make all`, `make deploy`, `make ipk` — each
   cross-builds FFmpeg into this worktree (a built lane here measures 1.2–4.8 GB). First:
   `export CARGO_TARGET_DIR=$HOME/plx-fleet/<lane>/target SIM_TDIR=$HOME/plx-fleet/<lane>/target-sim`
4. NEVER `git stash` — the stack is SHARED across worktrees and another lane's pop takes your work.
   Use `git add -A && git commit -m wip`. If you already stashed, pin it and drop it:
   `git update-ref refs/rescue/<lane> "$(git rev-parse stash@{0})" && git stash drop`
5. COMMIT ON THE LANE BRANCH, not a detached HEAD. Touch only the files this prompt names — other
   lanes are editing the rest right now. End your final message with `git log --oneline -3` and
   `git status --short`.
6. You cannot be reached mid-run. If this prompt is wrong or blocked, stop and report it; do not
   improvise around it.
```

## If two lanes collide anyway

1. **Stop ONE job** (`TaskStop`) and let the other finish. Do not stop both, and do not salvage the
   stopped one's half-collected numbers.
2. **Re-run the stopped lane from scratch.**
3. **Treat everything measured during the overlap as contaminated** — the other lane's, too —
   whether or not it looks fine. That is the whole point: it looks fine.
4. Clean the set, holding the lock (the **`tv-lock`** skill):

   ```sh
   tools/tv-lock.sh acquire --why "clean up after a collision"
   tools/tv-session.sh down                # closes the app, clears every plxnative-* trigger, relaunches
   tools/tv-lock.sh release
   ```

   `down` resolves ONE install's runtime root, so if the two lanes were on different flavours run it
   for each (`tools/tv-session.sh --flavor stable down`). Then check for stray ssh clients:
   `pgrep -fl "ssh .*$(make -s print-tv)"` — **`make -s print-tv`, not `cat .tv-host`**, because a
   lane is a worktree and a worktree has no `.tv-host`. Read those pids' argv; do not just count
   them. A count explained away as "my own grep pipeline" is precisely how the 2026-08-22 collision
   happened.

## Collecting the work

Merging the lane branch tips is **not sufficient**. A review agent's commit has twice ended up
orphaned on a detached HEAD instead of on its lane branch (`e0ade211`, `65d4d847`, one session on
2026-08-21; both eventually reached `main` through `review/auth-pin` and `review/menu-watch-pair`).
Before removing any worktree, ask each one's HEAD whether it already landed:

```sh
git worktree list --porcelain | awk '/^worktree /{w=substr($0,10)} /^HEAD /{print $2, w}' |
while read -r sha wt; do
  git merge-base --is-ancestor "$sha" <integration> || echo "UNMERGED: $sha  $wt"
done
git worktree remove "$WT" && git worktree prune   # NO --force: see §4 on what --force also deletes
```

(`substr($0,10)` rather than `$2` because a worktree path may contain spaces; the sha is printed
first so `read -r sha wt` still puts the whole tail in `wt`. Verified 2026-08-23, including that
the `UNMERGED:` branch actually fires.)

Then the integrator — and only the integrator — does the one cross-build, and takes the television
for whatever the fleet could not verify on the host.

**And what reaches `main` is ONE squash commit**, never the integration branch's history
(`AGENTS.md`, Working rules): on the main checkout, `git merge --squash <integration>` and a single
commit whose message is the fleet's own account. Lane merges into the integration branch are the
fleet's business; a trunk of lane commits and merge commits is not.
