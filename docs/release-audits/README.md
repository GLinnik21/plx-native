# Release audits: the standard

A release of this app is **a claim made to strangers about an unsigned binary** they are about to install on a television. The notes assert a hash and a compatibility range; the channel manifest asserts a size; the `.ipk` asserts what it contains. Anyone can check all of it, and when one of those claims is wrong there is no update mechanism to correct it.

`docs/release-notes/vX.Y.Z.md` is what the television owner reads. **This directory is where the claim is actually made good.** One audit per release, `docs/release-audits/vX.Y.Z.md`, holding the evidence: what was built, from what, by whom, containing what, verified how.

Introduced 2026-08-29, splitting the two apart. Nothing here is new rigour — every gate below already ran. It used to be printed in front of the reader, which made a release note twelve thousand characters long and buried its own compatibility claim under a package-facts table.

## Who reads an audit

| Reader | Their question |
|---|---|
| The **webosbrew reviewer** | Can I put this unsigned binary in front of other people's televisions? What does it open, write and reach? |
| A **contributor auditing an old release** | What was in v0.3.0, and how do I tell whether the file I have is it? |
| **Us, later** | What did this version claim, what was actually shipped, and what did we know at the time? |
| Anyone checking our **LGPL position** | Which libraries, built how, with which corresponding source? |

None of them is reading it on a television. Density is fine here; prose is not the point.

## Two halves, and the line between them is real

An audit file has an **authored** half and a **generated** half, separated by markers:

```
<!-- BEGIN GENERATED — ci/gen-release-audit.py. Do not edit by hand. -->
…
<!-- END GENERATED -->
```

**Generated — read out of the artifact by `ci/gen-release-audit.py`.** Identity and provenance, package identity and hashes, hash agreement across the four places it appears, the ar container's layout, the control file, the build configuration as measured on the bytes, declared permissions and memory, host- and path-shaped strings in the shipped binary, the build-machine-path scan, the complete payload inventory with per-file hashes, `DT_NEEDED`, the bundled libraries and their SONAMEs, FFmpeg's recorded configure invocation, the licence files in the payload, the CI gate verdicts, the static firmware matrix, and the published-asset verification.

**Authored — written by a person before the release, because no command can produce it.** Device-test evidence (which suites ran, on which television, against what, with what result), external compatibility reports known at release time, the reasoning behind any compatibility tier that moved, the provenance of a known issue, and anything the release deliberately did not do.

**That distinction is not tidiness, it is honesty.** A device test is a human watching a television; a third-party report is somebody else's sentence; a decision not to take a fix is a judgement. Generating those would be inventing them. Everything a machine *can* measure is measured, and the two are visibly separated so a reader always knows which they are looking at.

The authored half is written and committed **before** the release runs — it is reviewable in the same diff as the version bump, and `ci/check-package.py` refuses to build a stable package whose audit file is missing or has lost its markers.

## The pipeline

```
a person writes docs/release-notes/vX.Y.Z.md      (what the user needs to know)
a person writes docs/release-audits/vX.Y.Z.md     (the authored half: device evidence, reports)
        ↓  Actions → Release → Run workflow
CI bumps, tags, and gates the version files        (prepare, guard)
CI builds the artifact                             (build + verify — RELEASE=1, FLAVOR=stable)
CI verifies it                                     (check-elf.sh, check-package.py, ipk-verify)
CI generates the audit's evidence half             (gen-release-audit.py, from the real artifact)
CI publishes the release                           (body = the note, with __IPK_SHA256__ filled)
CI re-derives every claim from the published files (verify-published.sh)
CI commits the completed audit to main
```

The audit is generated **before** the release is published, from the artifact that is about to be published; the one section that cannot be is `What the public can download`, which needs a live release to look at, and it is appended in the same job immediately after.

## Regenerating one, by hand

The generator takes artifacts, not a build tree, so it works against a release that already exists — which is how the whole design was validated, against the published v0.5.0:

```sh
gh release download v0.5.0 -D /tmp/v0.5.0
ci/gen-release-audit.py --tag v0.5.0 --dist /tmp/v0.5.0 --out /tmp/facts.md
```

Add `--write` to splice the result into `docs/release-audits/v0.5.0.md` between its markers, and `--commit`, `--run-url`, `--uploader`, `--published-at`, `--build-config`, `--gates`, `--fwcompat`, `--published` for the values that come from the run rather than from the package. It is stdlib-only Python with no network and no NDK: the ELF is parsed in-process, so it runs on the runner, on a Mac, and on a machine that cannot build this project at all.

An audit regenerated from published assets should reproduce the committed one field for field. That is the check worth running when a hash is disputed.

## Auditing a release by hand

Every item below is automated — `ci/check-package.py` before the build, `ci/gen-release-audit.py` at publish time, `ci/verify-published.sh` after it — and this is how you re-run them against a release that already exists, including one cut before any of it existed. This is the old pre-publish checklist; it did not go away, it stopped being something a person had to remember.

```sh
V=0.5.0; PREV=0.4.1; REPO=GLinnik21/plx-native
D=$(mktemp -d) && cd "$D" && gh release download "v$V" --repo "$REPO"
```

**1 — CI built this, not a laptop.** (v0.2.1 fails.)

```sh
gh api repos/$REPO/releases/tags/v$V --jq '.assets[]|"\(.uploader.login)  \(.name)"'
gh run view <run-id> --repo $REPO --json jobs --jq '.jobs[]|"\(.name): \(.conclusion)"'
```

Every uploader must read `github-actions[bot]`, and `build + verify`, `redistributable assets` and `publish release` must all say `success`. A `skipped` means the release did not happen the way it claims.

**2 to 5 — the whole artifact, in one command.** Hashes in every place they appear, `shasum -c` working where a user stands, sizes from the manifest with their units, the payload inventory, and the build-machine-path scan:

```sh
ci/gen-release-audit.py --tag "v$V" --dist . --out /tmp/audit.md && less /tmp/audit.md
ci/verify-published.sh "v$V"
```

**6 — the payload gained nothing unannounced.** Two audits diff cleanly, which is what the per-file inventory is for:

```sh
diff <(sed -n '/^### Payload inventory/,/^### /p' docs/release-audits/v$PREV.md) \
     <(sed -n '/^### Payload inventory/,/^### /p' docs/release-audits/v$V.md)
```

**7 — the LGPL position.** The generated `FFmpeg and the LGPL position` section names the libraries actually in the package, the attached tarball's sha256 against the one pinned in the build script, and the absence of `--enable-gpl`, `--enable-version3` and `--enable-nonfree` with comments excluded from the scan. `ci/check-package.py` separately asserts that `THIRD-PARTY-NOTICES.md` names exactly the distributed libraries — it once listed a fourth that release builds do not ship.

**8 — compatibility was regenerated, not retyped.** The `Static firmware compatibility` section is `webosbrew-ipk-verify`'s own output. Reproduce it offline from the published binary:

```sh
ci/gen-release-audit.py --tag "v$V" --dist .   # extracts nothing; to grade the binary yourself:
tools/fwcompat.py <the plxnative extracted from the .ipk>
```

**9 — every version number in the note exists.** `ci/check-package.py` gates it on every build; this is the same question asked of a published body:

```sh
gh release view "v$V" --repo $REPO --json body -q .body | grep -oE 'webOS [0-9]+(\.[0-9]+)*|v[0-9]+\.[0-9]+\.[0-9]+' | sort -u
git tag --list 'v*'
```

**10 — links resolve.** Nothing gates a 404:

```sh
gh release view "v$V" --repo $REPO --json body -q .body | grep -oE 'https?://[^ )]+' \
  | xargs -n1 -I{} sh -c 'printf "%s " {}; curl -o /dev/null -s -w "%{http_code}\n" -L {}'
```

**11 — the published body is the committed file, with only the sentinel filled.**

```sh
diff <(gh release view "v$V" --repo $REPO --json body -q .body) \
     <(sed "s|__IPK_SHA256__|$(shasum -a 256 com.beb.plxnative_${V}_arm.ipk | cut -d' ' -f1)|" \
       docs/release-notes/v$V.md)
# only GitHub's appended "What's Changed" / "Full Changelog" tail may differ
```

## What each field is doing there

Every one of these is in the audit because a release of this project got it wrong, or because a reviewer would have to ask.

| Field | Why |
|---|---|
| asset uploader | v0.2.1 was published from a laptop with `gh release create`. `prepare` is gated on `workflow_dispatch`, GitHub propagates that skip down the whole chain, and `build + verify`, `redistributable assets` and `publish release` all silently did not run. Every asset's uploader was a person. **`github-actions[bot]` on every asset is the single strongest fact in the audit.** |
| hash agreement | The hash appears in four places — the artifact, `ipk.sha256`, the Homebrew Channel manifest and the release body. The manifest's copy is enforced *on the television* at install time and by nothing in CI. |
| `shasum -c` verifying beside the `.ipk` | `ipk.sha256` carried a `pkg/` prefix through v0.2.1, so the command the notes told people to run failed with "No such file or directory" in the directory they had actually downloaded into. |
| build-machine paths, per payload file | v0.2.1 shipped the maintainer's home directory inside all three FFmpeg libraries, and with it a reproducibility claim that could not be true. `check-elf.sh` scanned `pkg/plxnative` and nothing beside it. |
| dev-trigger witnesses | `RELEASE=1` must be on *every* make invocation, and any make without it deletes the release artifacts at parse time. A dev-featured binary under the released id ships a `/tmp` trigger surface, a world-writable FIFO that can drive the UI, and an unauthenticated TCP capture listener. Measured on the bytes, so no stamp and no command line has to be believed. |
| FFmpeg's recorded configure invocation | It is simultaneously the licence evidence (no `--enable-gpl`, `--enable-version3` or `--enable-nonfree`), the reproducibility evidence (it is where the build directory leaked), and the answer to "does this FFmpeg have network" (it does not — `--disable-network`, `--enable-protocol=file` only). |
| payload inventory with hashes | The package once shipped without its fonts, and once nearly shipped a host `.dylib`. A diff against the previous release's inventory is how "the payload gained nothing unannounced" is answered in one command. |
| `DT_NEEDED` | A name the device lacks kills the process at `exec()`, before `main`, before the event log exists. Two families are deliberately absent and `dlopen`ed instead, and the audit says so where a reviewer will look for it. |
| declared permissions, `requiredMemory` | What the reviewer's own checklist asks. A knowingly wrong number is disclosed rather than omitted: `requiredMemory` shipped as 60 against a measured 152 MiB peak, and saying so cost nothing while the reviewer finding it would have cost the release. |
| host- and path-shaped strings | The reviewer will run `strings`. Better that the audit shows what they will find, including the three `/tmp/plxnative-*` literals that are log messages rather than paths the binary opens. |
| static firmware matrix | The note carries one line of it. The evidence is fourteen rows and belongs here. |
| device-test evidence | The only thing that grades whether video plays, and the only thing on this list a machine cannot produce. |

## Rules

1. **Prefer a measured fact to a written one.** If a field can be derived from the artifact, derive it — do not type it and do not let an agent type it. If it cannot, attribute it: who, which set, which version, what date.
2. **Never let a static check become a playback claim.** `webosbrew-ipk-verify` grades whether the process starts. It cannot see a video plane.
3. **State a knowingly wrong number rather than omitting it**, with what it should be and when it will move.
4. **An audit is a record of one artifact**, not living documentation. It gains a dated erratum if it turns out to be wrong about itself; it does not gain later discoveries. A firmware that turns out to work in December belongs in December's release, not in August's audit.
5. **Invariant product facts are not audit fields.** What the app writes, reads and reaches *by design* is documented once, in [`docs/install-and-verify.md`](../install-and-verify.md); the audit records what was *measured in this artifact*. A description and a measurement are different things and they have different lifetimes.
6. **An audit whose generated half is missing means the release did not go through the workflow.** That is worth as much as anything in the file.

## What is enforced

| Gate | Where |
|---|---|
| the audit file exists for this version | `ci/check-package.py` |
| it carries the BEGIN/END generated markers | `ci/check-package.py` |
| its authored half carries the required sections | `ci/check-package.py` |
| every `webOS N` its authored half names has evidence in this repo | `ci/check-package.py` |
| the generated half is produced from the built artifact, and generation failing fails the release | `.github/workflows/release.yml` |
| the completed audit reaches `main` | `.github/workflows/release.yml` |

The audits themselves are not gated on being *right* about a television — nothing can do that. They are gated on being *derived*, which is the part a machine can hold.
