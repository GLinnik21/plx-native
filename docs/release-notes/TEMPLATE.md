<!--
Copy to docs/release-notes/vX.Y.Z.md, fill in, delete the sections that do not apply, and delete
these comments. The standard is README.md in this directory; read it once, then work from here.

Two rules this file cannot show you and `ci/check-package.py` will fail you for:
  * DO NOT HARD-WRAP. One source line per paragraph or list item, however long. GitHub wraps it.
  * Links must be ABSOLUTE. A release body does not resolve repository-relative paths.

Everything technical — package facts, DT_NEEDED, sizes, provenance, the payload inventory, the
CI gate verdicts, the firmware matrix, the LGPL detail — belongs in docs/release-audits/vX.Y.Z.md,
which is written from its own template and completed by CI from the real build. It is not gone;
it is not here.

The `# vX.Y.Z — <theme>` line below becomes the GitHub release's own title, not the body's first
line — release.yml strips it before posting, since GitHub already renders it as its own header
next to the tag. Write it anyway; it is the only source for that title and it is what makes this
file read as a complete document on its own.
-->

# vX.Y.Z — <short human-readable theme>

<One short paragraph, usually 1-3 sentences: why this release matters to someone who already has the app. Not a summary of the diff.>

<!-- CONDITIONAL, AND ABOVE EVERYTHING ELSE WHEN IT FIRES. Titled with the reader's ACTION, never
     with our defect. See README.md — "When the reader has to act, that comes first". -->
## If you used vA.B.C or earlier: <the action, in the reader's words>

<What was exposed and what someone holding it can do, in the user's terms. One sentence.>

<Which released versions are affected, by number, and which are not.>

**What to do:** <the exact place a person with a remote and a phone can reach> — <and what it costs them>.

**What is fixed, and what is not:** <the fix, scoped to the sink and the shape it covers> — <and the residual>.

<!-- CONDITIONAL. Same placement rule: above the features. -->
## Breaking: <what stops working, and for whom>

<What changes, who it affects, and what they can do instead.>

<!-- USUALLY PRESENT. Omit for a pure bug-fix release. -->
## What's new

- **<A user-visible capability.>** <What it means in practice — the effect, not the implementation.>

<!-- CONDITIONAL. -->
## Fixed

- **<What the reader would have observed.>** <Optionally one clause of mechanism.>

<!-- USUALLY PRESENT. One line per tier; drop a tier that has no evidence this release.
     The verbs are fixed: verified / reported / statically checked. A static check is NOT a
     playback claim. See README.md — "Compatibility, which is the hard part". -->
## Compatibility

- **Plays video, verified by watching it — <model year> LG sets.** <The one television: model, platform release, what LG markets it as, and what was run on it before this release.>
- **Plays video, reported by someone else — <model year>.** <who reported it as a PROFILE LINK — `[name](https://github.com/name)`, never a GitHub mention — which set, which platform release, which version they ran, the date, and a link to the report.>
- **Starts, and nothing further is known — every other firmware from webOS <N> up.** The loader resolves this binary's libraries and symbols against all <n> firmware images webosbrew has at <N> or newer. That grades startup and says nothing about whether video plays; the matrix is in the [technical audit](https://github.com/GLinnik21/plx-native/blob/main/docs/release-audits/vX.Y.Z.md).
- **Does not start — webOS <M> and older.** Symbols the app needs are missing there, so the process is killed before anything appears.

If your set is in the middle two groups, [tell us what happened](https://github.com/GLinnik21/plx-native/issues). It working is as useful a report as it failing.

<!-- CONDITIONAL. Actual defects in THIS version, with a trajectory. Not product decisions —
     "no music library" is scope and lives in docs/install-and-verify.md, not here. -->
## Known issues

- **<What a reader would hit.>** <Where it was measured or how it is known, and whether this release changes it.>

<!-- CONDITIONAL. Only when a report from hardware or a network nobody here has would change what
     we know. Name the ask, not the wish. -->
## Help test this release

**<If you have X, try Y.>** <What changed on that path, and that nobody has run it there.> [Open an issue](https://github.com/GLinnik21/plx-native/issues). It working is as useful a report as it failing.

<!-- ALWAYS. Fixed shape — the invariant half lives in docs/install-and-verify.md. -->
## Installing

Download **com.beb.plxnative_X.Y.Z_arm.ipk** and install it with the [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel) or [dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop). You do **not** need a rooted TV.

Nothing in this distribution chain is signed, so this sha256 is what tells you the file you have is the file published here:

```
__IPK_SHA256__  com.beb.plxnative_X.Y.Z_arm.ipk
```

[Installing and checking a download](https://github.com/GLinnik21/plx-native/blob/main/docs/install-and-verify.md) covers the other assets, how to check the hash on each platform, and the Developer Mode expiry that uninstalls your apps. This package bundles FFmpeg under LGPL-2.1-or-later and its complete corresponding source is attached to this release. Exactly what was built, verified and shipped is in the [technical audit for vX.Y.Z](https://github.com/GLinnik21/plx-native/blob/main/docs/release-audits/vX.Y.Z.md).

<!-- GitHub appends "**Full Changelog**: vA.B.C...vX.Y.Z" after this file. Never type it yourself. -->
