---
name: cut-release
description: >
  Cut and publish a release of this app — bump the version, build the .ipk, write the release
  note, publish through CI, and update the webosbrew listing and PR. Use this whenever the user
  asks to release, ship, publish, cut, tag or bump a version ("cut 0.2.2", "ship a release",
  "tag a new version", "put out a build", "release notes for the next one"), and also when they
  ask to CHECK or FIX a release that already went out. Reach for it even when the request sounds
  like only one step ("bump the version", "write the notes", "why is the hash wrong") — the steps
  are coupled, and the ways this project's releases have actually broken were all invisible from
  inside a single step.
---

# cut-release — publishing this app without shipping a lie

A release here is not a tag. It is a **claim made to strangers about an unsigned binary** they are
about to install on a television. The notes assert a hash, a compatibility range and a licence
position; the channel manifest asserts a size; the `.ipk` asserts what it contains. Anyone can check
all of it, and when one of those claims is wrong there is no update mechanism to correct it — the
notes are the only channel back to someone who already installed.

**Since 2026-08-29 that claim is made in TWO documents, and knowing which is which is the first
thing to get right.** `docs/release-notes/vX.Y.Z.md` is the body CI publishes, written for a
television owner: what is new, what was fixed, whether they must act, whether it works on their
set, what is still broken. `docs/release-audits/vX.Y.Z.md` is the evidence — package facts,
hashes, `DT_NEEDED`, the payload inventory, provenance, the firmware matrix, the LGPL position —
and its measurable half is **generated from the artifact** by `ci/gen-release-audit.py` during the
release run rather than written by anyone. Both directories carry their own standard as a README.
Nothing was dropped in the split: every gate below still runs, and the numbers that used to be
typed into a note are now read out of the `.ipk`.

That is why this skill exists and why it is mostly about verification. Every rule below is here
because it was violated in a real release of this project, not because it is good practice.

## The three that actually went wrong

Read these first; the rest of the procedure assumes them.

**1. Releases are published BY CI. A hand-published release skips every gate.**
v0.2.1 was cut with `gh release create` from a laptop. The gates that would have caught the rest —
`redistributable assets`, `build + verify`, `publish release` — never ran, and the maintainer's home
directory shipped inside all three bundled FFmpeg libraries as a result. CI builds at a path that is
identical on every runner, which is the *only* reason the artifacts are reproducible at all.

**A bare `git push --tags` is not enough and fails silently.** `release.yml`'s `prepare` job is
gated on `workflow_dispatch`, and GitHub propagates that skip down the whole chain — a tag push runs
`tag / version agreement` and nothing else, leaving a tag with no release. Use **Actions → Release →
Run workflow**, or `gh workflow run release.yml`, with `version:` or `bump:`. To re-publish an
existing tag, use the `rebuild_tag` input.

**2. `RELEASE=1` must be on every single invocation, and you verify by hash, not by belief.**
`make RELEASE=1 && make deploy` ships a dev build. Worse, ANY make without `RELEASE=1` — including
`make check` — deletes the release artifacts at parse time, so a later step silently operates on a
dev build or on nothing. This bit during the very session that wrote this skill: a check for "does
the release build open a `/tmp` FIFO?" reported yes and was wrong, because the TV was still holding
a dev binary. The md5 comparison is what caught it. `release-guard` now turns that exact
split-invocation mistake into a message *for the stable id* — see the next section — but only
there: everywhere else `RELEASE=1` is still yours to remember.

The one exception, and it is safe to lean on: a **pure query** — any of the
`print-*` goals, and only those (`QUERY_GOALS` in the Makefile is the source; transcribing the list
here is how it went one short the first time it was written) is exempted from that
parse-time deletion, precisely so that asking the Makefile a question mid-release cannot discard the
binary you are about to publish. Mixing a query with a real goal is not a query and does stamp.

Whenever you assert something about "the release build", prove the bytes first:

```sh
md5 -q pkg/plxnative
sshpass -p alpine ssh root@$(cat .tv-host) \
  "md5sum /media/developer/apps/usr/palm/applications/com.beb.plxnative/plxnative"
```

**That id is spelled out on purpose, and it is the STABLE one.** This is the most-copied "prove the
bytes" idiom in the repo, and there is now a second install on the same television —
`com.beb.plxnative.debug`, the developer build, which is what an unflavoured `make deploy` targets.
Pasted into a debug context this command silently compares against the wrong app and reports a
mismatch (or, worse, a match) about a binary nobody is releasing. **A release is always the stable
id**, so leave the literal alone here; anywhere else, get the path from
`make -s print-appdir FLAVOR=<f>`. And note the hash is now weaker evidence than it reads as:
`pkg/plxnative` is a path every flavour and both configurations write, so a match proves the bytes
and not the install. The **first line of the event log** is the witness that names both —
`install: id=com.beb.plxnative flavour=- … features=release` — where `features=release` is the
direct answer to "is this the shipped configuration?". (The stable install prints `flavour=-`, not
`flavour=stable`: the field is derived by stripping the stable id off the running one, so the app
users get has nothing left to name.)

## The stable id is a release-only id, and the Makefile enforces it

`com.beb.plxnative` is the id users install. There is now a second install on the same
television — `com.beb.plxnative.debug`, the developer build — and **that one is the Makefile's
default**, so every release command below has to say `FLAVOR=stable` out loud. A release is always
the stable id and always `RELEASE=1`; the other combination is refused:

```
$ make FLAVOR=stable ipk
refusing to put a DEV build on com.beb.plxnative — that id is what users install.
  release build:      make FLAVOR=stable RELEASE=1 ipk
  developer install:  make ipk          (FLAVOR=stable is not the default)
  really meant it:    make FLAVOR=stable ALLOW_DEV_ON_STABLE=1 ipk
```

(It echoes your goal back, so the three lines are always spelled for the target you just tried.
The parenthetical names the flavour you **asked for**, not the default — reading it as advice to
add `FLAVOR=stable` is backwards; the developer install is the one you get by typing nothing.)

This is §2 seen from the other side: a dev-featured binary under the shipped id carries the whole
`/tmp` trigger surface, the world-writable `plxnative-remote` FIFO and the `:8910` capture
listener (8910 is the *stable* install's port; a flavoured one defaults to 8911, which is exactly
why the shipped id carrying a listener **at all** is the thing being ruled out here). Before the
split that could only happen by publishing by hand, which is how v0.2.1's defects got out; now it
is one forgotten `RELEASE=1` on a machine that also has a television, so it gets a mechanism.

**`ci/check-package.py` grades the same rule on the packaged BYTES, and it now does so
unconditionally** — the check sits outside the `pkg/.build-config` branch, gated on nothing but
"this package is the stable id". That matters because the stamp cannot be trusted to be one of the
two shipped configurations: the Makefile documents a third (`RUST_FEATFLAGS="--no-default-features
--features devtriggers"`, the README-screenshot recipe), and while the check was nested it would
have printed "SKIP — neither shipped configuration" and packaged a dev-trigger binary under the
released id on a green run. So the gate genuinely holds against the two ways past the recipe: the
documented `ALLOW_DEV_ON_STABLE=1` hatch, and a third feature set that satisfies `release-guard`
without being a release. "Carries no dev-trigger surface" is a property of the bytes and needs no
stamp to grade.

**`ALLOW_DEV_ON_STABLE=1` is never part of a release.** It exists for exactly one job —
reproducing a user's report against the id they actually installed, with the dev trigger surface
on — and a build made that way must never be published, packaged into a release asset, or hashed
into a note. Put the real thing back with `make FLAVOR=stable RELEASE=1 deploy` when you are done,
because the id it left behind is the one the household watches with.

## The procedure

### 1. Decide the version and bump it

The version lives in **four** places and `ci/check-package.py` asserts all four agree:
`pkg/appinfo.json` (the source), `ipkroot/ctl/control`, `rust-modules/Cargo.toml`, and the built
`.ipk` filename. Bump the first three; the fourth follows from the build.

**What the app REPORTS is derived from `Cargo.toml`, not equal to it, and the difference shows up
on the surface you are most likely to read it from.** `rust-modules/build.rs` publishes that number
exactly for a `RELEASE=1` build and as the **next minor plus `-dev`** for every other one, so the
diagnostics panel, `X-Plex-Version` and the Sentry release all say `0.6.0-dev` on a developer build
of a tree that last published `0.5.0`. A `-dev` on a photographed panel therefore means *this is
not a release build* — it does not mean the binary is stale, and a release build showing anything
but the bare `X.Y.Z` means `RELEASE=1` did not take. Both the
`ALLOW_DEV_ON_STABLE=1` reproduction build below and any `make deploy` of the debug flavour print
the suffixed form.

Semver as this project means it, and **the first question is which LINE you are on, not what
changed**. Development is trunk-based: `main` cuts **minor** releases — for fixes, diagnostics and
new capability alike — and **major** when that is the deliberate call. A **patch** belongs to an
existing minor's own maintenance line and is not cut from trunk, which is why the workflow refuses
one on `main` rather than quietly publishing it (there is no maintenance-line support yet; adding it
is the prerequisite for ever cutting a patch). A release that changes only docs or CI does not need
a version at all.

The level also decides what a working tree calls itself, since `rust-modules/build.rs` reports the
next minor: with `0.5.0` published, every developer build says `0.6.0-dev`, which is a true
pre-release of the version trunk is actually heading for.

### 2. Write BOTH documents before building

**The note** — `docs/release-notes/vX.Y.Z.md`, from `docs/release-notes/TEMPLATE.md`, to the
standard in that directory's README. It is what a television owner reads, and it is short. Two
rules a linter enforces and everyone forgets: **do not hard-wrap prose** (one source line per
paragraph — GitHub wraps it), and **links must be absolute**, because a release body resolves no
repository-relative path.

**The audit's authored half** — `docs/release-audits/vX.Y.Z.md`, from that directory's
`TEMPLATE.md`. This is the part no command can produce: which suites ran on which television with
what result, which third-party reports the compatibility claim rests on, where each known issue
was measured, and anything the release deliberately did not do. Everything measurable is left to
`ci/gen-release-audit.py`, which fills the file's generated block during the release run. **Do not
type a hash, a size, a file list, a `DT_NEEDED` entry or a firmware verdict into it.**

Commit both in the same commit as the version bump. CI publishes the body from the first and
completes the second.

Grade either one while you are writing it, with no package staged and before any bump:

```sh
python3 ci/check-package.py --lint-note  docs/release-notes/vX.Y.Z.md
python3 ci/check-package.py --lint-audit docs/release-audits/vX.Y.Z.md
```

Preview what the generated half will look like, against any release that already exists:

```sh
gh release download vA.B.C -D /tmp/vA.B.C
ci/gen-release-audit.py --tag vA.B.C --dist /tmp/vA.B.C
```

**The compatibility block has one editing rule and it has not changed: one television verified
moves one line.** Do not widen it because a firmware "should" work, and never let the static
loader check become a playback claim — it grades whether the process starts and cannot see a video
plane. The note carries one line per tier; the matrix belongs to the audit.

### 3. Build locally to catch mistakes early — CI builds what ships

```sh
make FLAVOR=stable RELEASE=1 ipk   # both, on every invocation, no exceptions
python3 ci/check-package.py        # the gate: versions, ar layout, descriptors, build paths, notes
```

`FLAVOR=stable` is what makes this the shipped id: unflavoured it packages
`com.beb.plxnative.debug`, whose `.ipk` filename, `appinfo.json` and staged directory all carry the
debug id — and `make ipk` only writes `pkg/ipk.sha256` for the stable flavour, so a debug package
cannot quietly overwrite the hash a release note quotes.

**`check-package.py` deliberately takes no `FLAVOR`, and that is not an omission to fix.** It reads
the flavour back out of the *staged* directory id (`ipkroot/data/usr/palm/applications/<id>`) and
grades whatever is actually there. Passing it an environment flavour would let the two disagree,
which is exactly the failure it exists to catch — so it prints `-- grading the <flavour> package:
<id>` and you read that line rather than trusting what you meant to build.

**Everything a command can decide is a gate in `ci/check-package.py`, not a step here.** That is
deliberate and it is the lesson of v0.2.1: a checklist is only as good as the person following it,
and the release that skipped every gate skipped them *because* it was done by hand. If you find
yourself wanting to add a check to this skill, add it to `check-package.py` instead — CI runs it on
every build and it cannot be forgotten.

A local build is for fast feedback. It is **not** what ships: CI builds the artifact, because a
local build embeds the local NDK path.

### 4. Publish through the workflow

```sh
gh workflow run release.yml -f version=X.Y.Z     # or -f bump=minor (trunk cuts minors)
gh run watch $(gh run list --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
```

**There is no flavour input here, and you do not want one.** `release.yml` pins `FLAVOR: stable` in
the build job's `env`, which is the right place for it: CI is the one context where the Makefile's
`FLAVOR ?= debug` default is always wrong, and a value nobody can forget to type beats one they
can. So the `FLAVOR=stable` you spell on every *local* command is already spelled for you in the
workflow — do not try to pass it as `-f`. If you ever need to confirm which id a run actually
packaged, read it off the artifact filename, which the workflow asserts, rather than from what the
job was meant to do.

Then confirm the gates actually ran — this is the check that would have caught v0.2.1:

```sh
gh run view <id> --json jobs --jq '.jobs[] | "\(.name): \(.conclusion)"'
```

`build + verify`, `redistributable assets` and `publish release` must all say `success`. If any says
`skipped`, the release did not happen the way the notes claim it did. Every asset's uploader should
be `github-actions[bot]`, never a person:

```sh
gh api repos/GLinnik21/plx-native/releases/tags/vX.Y.Z --jq '.assets[] | "\(.name) \(.uploader.login)"'
```

### 5. Verify what the public can actually download

`ci/verify-published.sh` runs as the last step of the `publish` job, so this happens on its own:
it downloads the real assets and re-derives every claim from them — the hash agreeing in four
places, `shasum -c` working where a user stands, CI being the uploader, and no payload file
carrying a build machine's directory layout.

Run it by hand only to audit a release that already exists, including old ones:

```sh
ci/verify-published.sh vX.Y.Z
```

**Then check that the audit landed.** After the release is created and verified, the same job
regenerates `docs/release-audits/vX.Y.Z.md` from the published artifacts — now including the
`verify-published.sh` verdict — and pushes it to `main` as `release: audit for vX.Y.Z [skip ci]`.
Two things are worth reading rather than assuming:

```sh
git -C . fetch origin main && git show origin/main:docs/release-audits/vX.Y.Z.md | head -40
gh api repos/GLinnik21/plx-native/releases/tags/vX.Y.Z --jq '.assets[]|"\(.name) \(.uploader.login)"'
```

An audit whose generated block is still the template's placeholder means the release did not go
through `release.yml`, which is the same defect as v0.2.1 wearing different clothes.

### 6. Install it the way a user would

Deploying over ssh does not exercise the package. Install the real `.ipk` — this is how two
packaging bugs were found that `make deploy` could never have surfaced:

```sh
scp pkg/com.beb.plxnative_X.Y.Z_arm.ipk root@$(cat .tv-host):/tmp/
ssh root@$(cat .tv-host) "script -qc \"luna-send -i -a com.webos.appInstallService \
  luna://com.webos.appInstallService/dev/install \
  '{\\\"id\\\":\\\"com.beb.plxnative\\\",\\\"ipkUrl\\\":\\\"/tmp/com.beb.plxnative_X.Y.Z_arm.ipk\\\",\\\"subscribe\\\":true}'\" /dev/null"
```

**Both ids here are the stable one, deliberately** — this step is the release going onto the id
users install, and nothing about it is parameterised. **Do not substitute `make FLAVOR=stable
RELEASE=1 install`**, which issues the same luna call and then *deploys over it*: that is right for
bringing an install up, and wrong here, because it replaces the packaged binary with your local one
and the whole point of this step is that the **package** is what runs.

Wake the TV first (`wake-tv` skill). Then launch it and read the event log: its first line is
`install: id=com.beb.plxnative flavour=- … features=release` — check the id and `features=` there
rather than inferring them, since both builds' binaries are named `plxnative` — the next line names
the firmware, and a release build must leave **only** the three `*.log` files in the stable runtime
root, `/tmp`. No FIFO, no `:8910` listener. That is the release build's whole premise and it is
worth re-checking every time, by hash.

If the developer install is also on this television you will see a `/tmp/com.beb.plxnative.debug`
directory beside those logs: that is the *other* install's runtime root, not a leak from this one.
It is named for the app id — the reason the separator is a dot and not a hyphen — so it matches no
`plxnative-*` glob and cannot be mistaken for a trigger by the check or by the app.

### 7. Tell the people who are waiting

The listing and the submission are separate artifacts from the release and both go stale:

- **`webosbrew/apps-repo` PR** — the body argues for the requirements line. When compatibility
  changes, that argument changes. It has previously sat for weeks defending a ceiling that had
  already been removed.
- **`packages/com.beb.plxnative.yml`** — the description thousands of channel users read before
  installing. Its "please read before installing" section is where the honest limits live.
- **A comment on the PR** if the reviewer is waiting on something this release affects.

Comments and PR text go out **under the maintainer's name**. Write as them: state what changed, do
not apologise on their behalf for something an agent did, and match the terse register of the
thread rather than the house style of a commit message.

## After publishing

A note is a published claim, so it gets **errata, not edits**. If something in it turns out to be
wrong, append a dated correction under `## Updates to this note` and leave the original text
standing — someone may have acted on it. v0.2.1's reproducibility claim was corrected this way.

**A note is not living documentation.** Do not fold a later discovery into an old release body: a
firmware that turns out to work in October does not belong in August's note, and a fix has its own
release. For a security problem found later in a shipped version, the mechanism is a GitHub
security advisory plus a fixing release whose note carries the action-required section — an
advisory reaches people the old page never will. The same applies to an audit: it records one
artifact, gains a dated erratum if it is wrong about itself, and gains nothing else.

If a release is wrong enough to withdraw, say so in the note and publish a fixed one. Never delete
a tag that has been public; the hash in someone's notes has to keep meaning something.
