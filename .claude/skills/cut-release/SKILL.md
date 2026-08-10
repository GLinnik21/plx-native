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
a dev binary. The md5 comparison is what caught it.

Whenever you assert something about "the release build", prove the bytes first:

```sh
md5 -q pkg/plxnative
sshpass -p alpine ssh root@$(cat .tv-host) \
  "md5sum /media/developer/apps/usr/palm/applications/com.beb.plxnative/plxnative"
```

**3. Every number in the note is copied from an artifact, never typed.**
Past notes have claimed a "webOS 26" that does not exist and quoted an uncompressed size as a
download size. Sizes come from the manifest's `ipkSize`; hashes from `shasum` on the published
asset; firmware numbers must appear in `tools/fwcompat.py`'s matrix or `docs/webos5-port.md`'s
table. `scripts/preflight.sh` checks all of this.

## The procedure

### 1. Decide the version and bump it

The version lives in **four** places and `ci/check-package.py` asserts all four agree:
`pkg/appinfo.json` (the source), `ipkroot/ctl/control`, `rust-modules/Cargo.toml` (the diagnostics
panel prints this one on screen), and the built `.ipk` filename. Bump the first three; the fourth
follows from the build.

Semver as this project means it: **patch** for fixes and diagnostics, **minor** when a user gains a
capability, **major** is unused. A release that changes only docs or CI does not need a version.

### 2. Write the note before building

`docs/release-notes/TEMPLATE.md` is the skeleton and `docs/release-notes/README.md` is the standard
— read the standard the first time, then work from the template. Save the note to
`docs/release-notes/vX.Y.Z.md` and commit it in the same commit as the version bump, because CI
publishes the release body from that file.

Three blocks are **fixed wording** and must be pasted, not improvised: the verification block, the
LGPL block, and the compatibility statement. They are the sentences most likely to become false by
paraphrase, which is exactly why they are frozen.

The compatibility statement has one editing rule, in §3 of the standard: **one television verified
moves one line.** Do not widen it because a firmware "should" work.

### 3. Build and verify locally, then let CI do the real one

```sh
make RELEASE=1 ipk          # RELEASE=1 on every invocation, no exceptions
python3 ci/check-package.py # four version witnesses, ar layout, both descriptors, build paths
.claude/skills/cut-release/scripts/preflight.sh   # everything a command can decide
```

`preflight.sh` is the accumulated set of things that have gone wrong. Read its output rather than
its exit code alone — some items are advisory and say so.

A local build is for catching mistakes early. It is **not** what ships: CI builds the artifact,
because a local build embeds the local NDK path.

### 4. Publish through the workflow

```sh
gh workflow run release.yml -f version=X.Y.Z     # or -f bump=patch
gh run watch $(gh run list --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
```

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

Not what you built — what a stranger gets:

```sh
.claude/skills/cut-release/scripts/preflight.sh --published vX.Y.Z
```

This downloads the published `.ipk`, re-hashes it, checks the hash matches the note **and** the
manifest, runs `shasum -c` the way a user would, and scans every payload file for a build-machine
path.

### 6. Install it the way a user would

Deploying over ssh does not exercise the package. Install the real `.ipk` — this is how two
packaging bugs were found that `make deploy` could never have surfaced:

```sh
scp pkg/com.beb.plxnative_X.Y.Z_arm.ipk root@$(cat .tv-host):/tmp/
ssh root@$(cat .tv-host) "script -qc \"luna-send -i -a com.webos.appInstallService \
  luna://com.webos.appInstallService/dev/install \
  '{\\\"id\\\":\\\"com.beb.plxnative\\\",\\\"ipkUrl\\\":\\\"/tmp/com.beb.plxnative_X.Y.Z_arm.ipk\\\",\\\"subscribe\\\":true}'\" /dev/null"
```

Wake the TV first (`wake-tv` skill). Then launch it and read the event log: the boot line names the
firmware, and a release build must leave **only** the three `*.log` files in `/tmp` — no FIFO, no
`:8910` listener. That is the release build's whole premise and it is worth re-checking every time,
by hash.

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

If a release is wrong enough to withdraw, say so in the note and publish a fixed one. Never delete
a tag that has been public; the hash in someone's notes has to keep meaning something.
