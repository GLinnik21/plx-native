# Release notes: the standard

A release note is what a television owner reads when their Homebrew Channel offers them an update. It is not the record of what was built, verified and shipped — that is [`docs/release-audits/`](../release-audits/README.md), and the split between the two is the point of this standard.

**This is binding for the note's content**, and parts of it are enforced by `ci/check-package.py` rather than by review. Every note is committed to this directory as `docs/release-notes/vX.Y.Z.md` before the release is published; `.github/workflows/release.yml` publishes that file as the release body.

Rewritten 2026-08-29. The version before it was written after auditing v0.2.0 and v0.2.1 line by line, and it fixed the right defects — but it fixed them by putting the whole audit in front of the reader. A v0.5.0 note that opens with a new feature and reaches its known issues 12 000 characters later is not serving the person holding the remote. The verification did not get weaker; it moved.

## Who reads this, and what they need

One reader: **someone who already uses PlxNative, or is deciding whether to install it.** They have a remote and possibly a phone. They cannot run a command.

A note answers their six questions, quickly:

1. What is new?
2. What was fixed?
3. Is there anything I need to do?
4. Is there a breaking change or a security issue I need to know about?
5. Is this expected to work on my television?
6. What is still broken in this version?

Two other readers exist — a webosbrew reviewer deciding whether to put an unsigned binary in front of other people's televisions, and us in a year asking what a version actually claimed. **They are served by the audit, not by this note.** Sending them to a second document is what lets this one stay readable.

## The shape

A small standard, not a wall of mandatory sections. Sections that would add nothing are omitted; the order below is the default, and questions 3 and 4 override it.

```markdown
# vX.Y.Z — <short human-readable theme>

<One short paragraph, usually 1-3 sentences, on why this release matters. Not a summary of the diff.>

## What's new

- **A user-visible capability.** What it means in practice.

## Fixed

- **What the reader would have observed.** Optionally one clause of mechanism.

## Compatibility

<A short evidence-aware snapshot — the tiers below.>

## Known issues

<Actual defects in this version. Omit when there are none worth a reader's time.>

## Help test this release

<Only when a specific report from hardware or a network nobody here has would change what we know.>

## Installing

<Two sentences, the sha256 line, and the links. Fixed shape — see below.>
```

`**Full Changelog**` is appended by GitHub, from the compare range, and must never be typed into the file. A hand-written compare link is exactly the class of measurable fact this project has got wrong before.

**The `# vX.Y.Z — <theme>` line never reaches the published body.** `release.yml` extracts it as
the release's own `name` — GitHub already renders that next to the tag, above the body, so leaving
it in as the body's first line too put the version on the page twice (a v0.6.0 defect, fixed the
same day it published). Keep writing it: it is still what makes the committed `.md` file read as a
complete, titled document on its own, and it is the only source the workflow has for the release's
display name. Just do not expect to see it a second time on the release page — the body CI posts
starts at the paragraph underneath it.

### When the reader has to act, that comes first

**A security disclosure, a migration step, or anything else the reader must do goes in its own `##` section immediately after the lede, above every feature.** Never inside `Fixed`: the test is mechanical — *if installing the release does not complete the item, it does not belong in a list of things installing completes.* Leave one cross-reference line in `Fixed` so a changelog reader is routed rather than told twice.

Title it with **the reader's action**, not our defect: `## If you ran v0.1.0 or v0.2.0: rotate your Plex token`, not `## The event log printed credentials`.

**Breaking changes are equally prominent**, under a heading that says what stops working and for whom.

A release carries an action-required section when it changes, or reveals that an earlier release had, any of: a credential reaching a file, a log, a screen or the network; **who can read** a file the app writes; what the app writes **outside its own directories**; what leaves the network; or a listener, a FIFO or any other inbound surface. That list is deliberately wider than "credential" — a 0644 file in a 1777 `/tmp`, an unauthenticated listener and a per-binary device identifier are all in scope, and every one of them is invisible to `webosbrew-ipk-verify`, which grades only whether the app starts.

Five rules decide the wording, all of them from an actual disclosure this project got wrong:

1. **Unconditional whenever the condition is one the reader cannot evaluate.** "If you have sent anyone a log file" asks a user to know something they do not, and it excluded the population whose file was world-readable in a shared `/tmp` and had never been sent anywhere.
2. **Name the versions.** "An earlier version" is not actionable; "v0.1.0 and v0.2.0 are affected, v0.2.1 is not" is, and it is checkable.
3. **Never claim a class is closed more widely than the code closes it.** Name the sink and the shape, and say what still writes to the same file unscrubbed.
4. **Always state the residual.** A disclosure that stops at "fixed" invites the reader to treat the artefact as safe, which is how the next incident starts.
5. **Whenever we ask the reader to send us anything, the same paragraph says what is in it.**

Never a reassurance we cannot support: there is no update push here, and telemetry is opt-in and carries no account identity (a crash report's random Crash report ID counts uninterrupted opt-ins, nothing more — it is destroyed by switching the category off and by signing out), so *"no evidence of misuse"* and *"few users affected"* stay unsupportable — an opt-in "users affected" figure is a sample of the people who opted in, which is not a population and must never be quoted as one. The honest sentence is that we cannot tell whether this happened to you, which is why the instruction has no conditions on it.

No CVSS, no CVE, no severity label, no "we take security seriously", no root-cause essay. Nothing consumes this as a dependency and a score we cannot compute is theatre.

## Compatibility, which is the hard part

Hardware coverage here is unusually uncertain: playback is verified by a human on one television, and everything else is a third-party report or a static check. The note has to carry that gap without either overclaiming or frightening people off — and this project has overclaimed before, in a published note, about a "webOS 26" that does not exist.

**Four tiers, and the distinction between them is the whole content.** Keep the note's version to one line each.

| Tier | What it means | The verb |
|---|---|---|
| Plays video, verified | A human watched it, on a named set, with a firmware and a date | **verified** |
| Plays video, reported | A named third party on a named set, with a date and the version they ran | **reported** |
| Starts — nothing further known | A tool resolved libraries and symbols against firmware inventories | **statically checked** |
| Does not start | The same tool says a symbol the app needs is absent | — |

Three rules produce the block:

1. **Model year first; the platform release in parentheses.** An owner can see the year on the box. They cannot see `4.10.2`, and it collides with the "webOS 4.5" LG markets the same set as. Never print a bare release number as the primary key, and never mix LG's marketing numbering with webosbrew's platform numbering without saying which is which.
2. **Only those verbs.** Never "supports", never "works on", never "compatible with".
3. **A heading or a tier may never claim more than the evidence under it.** In particular: **a static loader check is not a playback claim.** It says the process starts. It says nothing at all about whether a picture appears.

Updating it:

- **One television moves one line, and only the line it belongs to.** A verified set does not promote a range, a release or a model year it did not sit in.
- **A third-party report is added with the reporter's name as a profile link, the date and the version they ran.** Never generalise a report from one release to its neighbours, and never restate someone's hedge as a verdict.
- **When a release changes something on a path that is known broken, do not restate the old failure as present fact.** Say what changed and that nobody has run it there. That is what `Help test this release` is for, and it is the only way this project ever gets the report it needs.
- **The static tier is regenerated from the release run's `webosbrew-ipk-verify` output, never retyped.** The full matrix lives in the audit; the note carries the one-line summary of it.
- **A compatibility or scope change is a two-artifact change**: this note *and* a PR to `webosbrew/apps-repo` updating the package description, which is the only text an owner reads before installing. Sync `docs/webosbrew-package.yml` in the same commit.

## Installing — fixed shape

The invariant half lives in [`docs/install-and-verify.md`](../install-and-verify.md) and is not repeated per release. The note carries only this, with the sha256 as a sentinel CI fills:

````markdown
## Installing

Download **com.beb.plxnative_X.Y.Z_arm.ipk** and install it with the [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel) or [dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop). You do **not** need a rooted TV.

Nothing in this distribution chain is signed, so this sha256 is what tells you the file you have is the file published here:

```
__IPK_SHA256__  com.beb.plxnative_X.Y.Z_arm.ipk
```

[Installing and checking a download](https://github.com/GLinnik21/plx-native/blob/main/docs/install-and-verify.md) covers the other assets, how to check the hash on each platform, and the Developer Mode expiry that uninstalls your apps. This package bundles FFmpeg under LGPL-2.1-or-later and its complete corresponding source is attached to this release. Exactly what was built, verified and shipped is in the [technical audit for vX.Y.Z](https://github.com/GLinnik21/plx-native/blob/main/docs/release-audits/vX.Y.Z.md).
````

`__IPK_SHA256__` is the one value in the file CI substitutes. It cannot be committed — it does not exist until the release run builds the artifact — and a hash nobody types cannot be the wrong hash. `ci/check-package.py` refuses a note that lost the sentinel and refuses one carrying a literal 64-hex string beside it, which would be either a stale hash from a previous release or a typed one.

The other four sentinels (`__COMMIT__`, `__RUN_URL__`, `__IPK_SIZE__`, `__INSTALLED_SIZE__`) are still substituted if a note uses them, but they belong in the audit now and a note should not need them.

## Writing style

Optimise for GitHub's renderer and for someone reading on a television or a phone.

- **Do not hard-wrap prose.** No 80-column, no 100-column. **One source line per paragraph or list item**, and let GitHub wrap it. This is enforced: `ci/check-package.py` fails a note whose paragraphs are split across source lines. The rest of this repository hard-wraps; release notes and this directory's documents deliberately do not, because they are read rendered, in a browser, at a width nobody controls.
- **Effects before mechanisms.** What the reader would have seen comes first; the mechanism gets one clause, or goes in the commit message.
- **Semantic limits, not line counts.** "One short paragraph, usually 1-3 sentences" — never "four lines", which means nothing once the text is not hard-wrapped.
- **Links must be absolute.** A release body is not rendered relative to the repository, so `[audit](docs/release-audits/v0.5.0.md)` is a dead link for every reader. Use `https://github.com/GLinnik21/plx-native/blob/main/…`. Enforced.
- **No GitHub @mentions — link the profile instead.** `[name](https://github.com/name)`, not `@name`. Enforced. A mention in a release body makes GitHub list that person as a **contributor to the release**, which is wrong for the people these notes actually name: a tester who reported a firmware result and a reviewer who filed a bug contributed neither code nor the release. It also attributes our claims to them on their own profile. Five published releases had done this before it was noticed.
- **No emoji.** Enforced. The most important sentences here are a compatibility claim and sometimes a credential-rotation instruction, being read by someone deciding whether to trust an unsigned binary. Nothing is signed and one television is tested, so the prose is the warranty.
- **No marketing verbs** — "blazing", "massively improved", "now fully supports".
- **No auto-generated PR-title dump as the body.** It cannot express a compatibility claim, a defect boundary or an ask, which are three of the six things a note is for. GitHub's generated list is appended after the note, where it is a free second record.
- **No measurable number a human typed.** Sizes, hashes, firmware counts, test counts. This project has published a wrong size, a wrong hash instruction and a firmware that does not exist; all three were typed.
- **No pasted log excerpt, sample URL or screenshot** containing a server name, LAN address, `machineIdentifier`, media title, profile name or token.
- **No comparative claims about the official Plex app.** Fine as positioning in the README; in a release note it is an unverifiable assertion about a third party that a channel reviewer would have to defend or strip.
- **No prerelease, no draft, no `-rc`, no four-component versions.** All four fail silently rather than loudly: a draft or prerelease drops out of `releases/latest`, which is the URL the Homebrew Channel resolves the manifest through, and LG will not install a version that is not three integers.
- **No `CHANGELOG.md`.** This directory published as the release body is the same record with one copy.

## What is NOT in a note any more

All of it moved to [`docs/release-audits/`](../release-audits/README.md) — none of it was deleted, and every CI gate that produced it still runs.

Package facts tables · `DT_NEEDED` · the payload inventory · runtime filesystem paths · listening sockets · permissions and capabilities · the outbound-host inventory · per-platform SHA verification commands · build provenance and the run URL · reproducibility evidence · the FFmpeg configure invocation and the full LGPL explanation · CI gate verdicts · the firmware compatibility matrix · install instructions that do not change between releases · the unchanged product scope.

If you find yourself wanting to add one of those to a note, add it to the audit template instead — or, if it is invariant, to `docs/install-and-verify.md`, which is current documentation rather than a per-release record.

## Versions

`ci/bump-version.py` refuses anything but three integers. **Which level is decided by the line you
are cutting from before it is decided by what changed** — development here is trunk-based:

- **minor** — what `main` cuts, and the answer for nearly every release: fixes, diagnostics and
  internals as much as something a user can see is new or a change in which televisions are
  supported. The same app working better is a minor here, because it came off trunk.
- **major** — reserved. `1.0.0` means playback is device-verified on more than one platform generation.
- **patch** — a maintenance release on an existing minor's own line, for a fix that must reach
  people already on that version without shipping trunk. The release workflow refuses one from
  `main`, and no maintenance line exists yet, so no release so far has been one.

A release that changes only documentation or CI does not need a version.

## Historical integrity

**A published note is a record of what was known and relevant at publication.** It is not living documentation, and it should not accumulate today's understanding.

- **Do not add newly discovered compatibility information to an old note.** A set that turned out to work in October does not belong in an August release body.
- **Do not add later fixes to an old note.** The release that fixed it has its own note.
- **For a security problem discovered later in an old version**, the mechanism is a [GitHub security advisory](https://github.com/GLinnik21/plx-native/security/advisories) plus a fixing release whose note carries the action-required section — not an edit to every old body. An advisory reaches people the old page never will.
- **When a note is factually wrong about itself** — it claims a hash that is not the artifact's, or a build path that is not what happened — append a dated `## Updates to this note`. Errata correct the record; they do not extend it.

Rewriting a pre-standard note to the current template is a deliberate, separate exercise, and when it happens two things survive: a safety disclosure is never dropped, and anything a reader could have acted on stays true or stays stated. The archive of what a note used to say is `git log` on these files.

**That exercise was carried out once, on 2026-08-29**, when v0.1.0 through v0.5.0 were rewritten to this standard in a single dated pass. Three rules governed it and are the precedent for any future one:

- **Every safety disclosure moved across intact** — the token rotation in v0.1.0, v0.2.0 and v0.2.1, and both of v0.4.0's, the PIN bypass and the first release that reaches a server over the internet.
- **Nothing a reader could act on was dropped.** v0.1.0, v0.2.0 and v0.2.1 still say that their `ipk.sha256` records a path rather than a bare filename, so `sha256sum -c` will not find the download — because that instruction is one a reader follows. v0.2.1 still says it was built and published by hand.
- **No later knowledge was folded backwards.** v0.2.0 does not carry the webOS 6 and 10 report that arrived three days after it shipped; v0.2.1 does not carry [mariotaku](https://github.com/mariotaku)'s 6.5.2 success from the day after; none of them carries the webOS 10.3.1 transcode failure found in late August. Each note says what was known when it was published, and the evidence for that boundary is the dated record in each release's audit.

Because every note now conforms, `ci/check-package.py` applies the whole standard to all of them with no version exemption.

## Two things to run before announcing it

Neither can be a gate, because both end in a judgement.

**Was the action-required section owed?** The trigger list above is wide, so evaluate it explicitly rather than by feel, and record the decision — including "no hits" — in the notes file's commit message:

```sh
git diff "vA.B.C".."vX.Y.Z" -- rust-modules/src src ci \
  | grep -nE '^\+.*(token|X-Plex-Token|mkfifo|bind\(|listen\(|O_CREAT|fopen|0o?6[0-9]{2}|/tmp/|/media/)'
```

**Is anything claimed more widely than the evidence?** These phrases are not banned outright — "not reproducible across machines" is the honest sentence, and it contains two of them — but each one is a place where a note has overclaimed before, so read every hit:

```sh
grep -niE 'reproducible|byte-identical|every firmware|fully support|works on|guarantee|no known issues' docs/release-notes/vX.Y.Z.md
```

## What is enforced, and where

| Gate | Where |
|---|---|
| the note exists for this version | `ci/check-package.py` |
| it carries `__IPK_SHA256__`, and no unknown `__SENTINEL__` | `ci/check-package.py` |
| no hand-typed package hash | `ci/check-package.py` |
| every `webOS N` it names has evidence in this repo | `ci/check-package.py` |
| no hard-wrapped prose | `ci/check-package.py` |
| no relative links, no emoji, no @mentions | `ci/check-package.py` |
| it does not carry the audit's sections | `ci/check-package.py` |
| the published body is this file with the sentinel filled | `.github/workflows/release.yml` |
| the published body quotes the artifact's real hash | `ci/verify-published.sh` |

Everything a command can decide is a gate, never a checklist item. That is the lesson of v0.2.1, which was published by hand and thereby skipped every gate at once: a checklist is only as good as the person following it, and the release that skipped them skipped them *because* it was done by hand.
