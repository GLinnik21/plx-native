<!--
Copy to docs/release-audits/vX.Y.Z.md, fill in the AUTHORED half, and commit it with the release
note and the version bump. Delete these comments. The standard is README.md in this directory.

You are writing the half no command can produce: what a human saw on a television, what somebody
else reported, and what this release deliberately did not do. Everything measurable is generated
into the block at the bottom by `ci/gen-release-audit.py` during the release run — do not type a
hash, a size, a file list, a DT_NEEDED entry or a firmware verdict anywhere in this file.

Preview the generated half against a previous release at any time:
    gh release download vA.B.C -D /tmp/vA.B.C
    ci/gen-release-audit.py --tag vA.B.C --dist /tmp/vA.B.C
-->

# Release audit — vX.Y.Z

The evidence behind [the vX.Y.Z release note](https://github.com/GLinnik21/plx-native/blob/main/docs/release-notes/vX.Y.Z.md). Written for a webosbrew reviewer, a contributor auditing an old release, and us in a year. The section at the bottom is generated from the artifact that was published; everything above it was written by a person before the release ran.

## Device test evidence

<The only grade of whether video plays, and the only thing here a machine cannot produce.>

| | |
|---|---|
| television | <model, platform release as the set reports it, what LG markets the generation as> |
| server | <what the suite ran against> |
| suite | `./tests/run.py --server` — <n> cases, <result>, <date> |
| suite | `./tests/run.py` (synthetic pipeline tier, no Plex) — <n> cases, <result>, <date> |
| perf | `./tests/run.py --fps` — <result>, or why it was not run |
| by hand | <anything watched by a person that the suites do not cover, with what was seen> |

<One paragraph on anything the runs do not settle: a case that was skipped and why, a shape the library does not contain, a tier that is structurally blind to what this release changed.>

## External compatibility reports known at release time

<Every third-party report this release's compatibility claim rests on: @who, which set, which platform release, which version they ran, the date, a link, and what they actually saw. Never generalise one to its neighbours. If there are none, say so — an empty section here is itself a fact about how much is known.>

## Compatibility tiers, and what moved

<Which tier each firmware sits in, and — for anything that changed since the previous release — the single piece of evidence that moved it. One television moves one line. A static check never moves a playback line.>

## Known issues at release, with provenance

<Each defect the note lists, with where it was measured, on what, and whether this release changes it. This is where the measurement lives; the note carries the sentence a user needs.>

## Deliberately not done

<Anything a reviewer might expect and not find: a fix that was proven and not taken, a check that does not run, a claim that was available and was not made. State it here rather than letting it be discovered.>

## Release configuration

| | |
|---|---|
| flavour | `com.beb.plxnative` (stable) — the id users install; `FLAVOR: stable` is pinned in the release workflow's build job |
| cargo features | `RELEASE=1`, which drops `devtools` and `devtriggers` |
| built by | GitHub Actions, from the tag, at a path identical on every runner |
| toolchain | webOS NDK <version if it moved>, Rust nightly pinned by date in the `RUST_NIGHTLY` repository variable |

<Anything unusual about this build: a toolchain bump, a dependency change, a new payload file, a new outbound host, a new file written outside the app's own directory. If none, say none — the sentence is what makes the absence checkable.>

<!-- BEGIN GENERATED — ci/gen-release-audit.py. Do not edit by hand. -->

*(This block is written by the release workflow from the published artifact. A release audit whose generated half is still this placeholder did not go through `.github/workflows/release.yml`, which is itself worth knowing.)*

<!-- END GENERATED -->
