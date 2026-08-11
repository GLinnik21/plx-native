# Release notes: the standard

**This is binding.** Every release note from v0.3.0 on is written from the template in §2, passes
the checklist in §6, and is committed to this directory as `docs/release-notes/vX.Y.Z.md` before
the release is published. It is not a style guide — the fixed blocks are fixed wording, and the
checklist is a gate.

Written 2026-08-10 after auditing v0.2.0 and v0.2.1 line by line. Everything asserted here about
those two releases was re-measured against the published assets, not recalled.

## 0. Who reads these, and what each of them needs

| | Reader | Their one question | What they can do |
|---|---|---|---|
| 1 | **A TV owner** browsing an app catalogue with a remote | *Will this work on my television, and do I have to do anything?* | Press buttons. No shell, no commands. |
| 2 | **The webosbrew reviewer** | *Can I put this unsigned binary in front of other people's televisions?* | Read the repo, run `webosbrew-ipk-verify`, install it on his own sets. |
| 3 | **Us, later** | *What did this version actually claim, and what did we know when?* | Read the note and the commit log. |

Three facts about this project shape every rule below.

- **Nothing in the chain is signed.** The sha256 in the note is the only tamper check a user has.
- **We can test almost none of the hardware.** The binary starts on every firmware image webosbrew
  has at webOS 4.0 or newer. Playback is verified by a human on exactly one television. The notes
  have to carry that gap without either overclaiming or scaring people off.
- **We ship someone else's LGPL code.** The FFmpeg tarball and build script are attached to every
  release because LGPL-2.1 §6 requires it, not as a courtesy.

And one piece of history that earns the checklist: **these notes have overclaimed before.** An
early draft asserted support for a "webOS 26" that does not exist, and quoted an uncompressed size
as a download size. "Is every claim true and checkable" is the first-class concern here.

---

## 1. The verdict on v0.2.0 and v0.2.1

### What they got right — keep all of it

These are better than every comparable webosbrew release note, which are almost uniformly GitHub's
auto-generated PR-title list. Specifically:

1. **Prose with headed sections, addressed to a person.** It is the only form that can carry a
   compatibility claim, a defect boundary and a request for help. Keep it.
2. **The lede states the release's purpose, not its diff.** v0.2.1: *"This release exists to make a
   bug report possible."* That is the best sentence in either note. Every release gets one.
3. **Mechanism stated in one checkable clause.** v0.2.0: *"The app told SDL it was built against an
   older version than it was."* Re-verified — `sys_grab_wayland` declared 2.0.4, SDL ≥ 2.0.6
   rejects that, the dev TV runs 2.0.5, so "latent" is exactly the right word.
4. **The privacy promise on the diagnostics panel.** v0.2.1: *"It deliberately contains no server
   name, no address, no account details and no media title, so it is safe to post publicly."* This
   is structurally enforced — `player::Diag` is numbers, booleans and enums — and it is what makes
   asking a stranger for a bug report safe. Keep the sentence; §4 narrows only its edge.
5. **The sha256, and saying plainly that nothing is signed.** Both published hashes are correct: I
   downloaded both `.ipk`s and they match the notes *and* the manifest the television verifies.
6. **The LGPL section exists at all**, with the pristine tarball and the real configure script
   attached. No comparable app does this.
7. **A credential leak was disclosed in the release that fixed it**, with an instruction to act.
   The content was right. §4 fixes where it sat and how it was scoped.
8. **No emoji, no marketing verbs, no "New Contributors", no auto-generated body.** Keep.

### What must change

Nine items. Each is a defect in the artifact, not a matter of taste.

**1. The reproducibility claim is false, and it is the load-bearing one.** Both notes say *"Builds
are reproducible — the same commit and toolchain give a byte-identical `.ipk`."* Measured: the
three bundled FFmpeg libraries embed their build directory. v0.2.0's `libavutil-plx.so.61` carries
`--prefix=/home/runner/work/plx-native/plx-native/vendor/ffmpeg-prefix`; v0.2.1's carries
`--prefix=/Users/gleblinnik/Developer/plex/plex-native-poc/vendor/ffmpeg-prefix`. All three `.so`
files differ between the two releases. `ci/check-elf.sh` cannot catch it — it scans `pkg/plxnative`
and nothing else. So a reviewer who follows our own instruction gets a different hash and cannot
distinguish "different build directory" from "tampered". **Until this is fixed the claim is
banned**; §2 has the interim wording, and §6 item 5 is the gate.

**2. v0.2.1 was published by hand, and no gate ran.** Release run `31403652035` shows
`build + verify`, `redistributable assets` and `publish release` all skipped — a hand-pushed tag
skips `prepare`, and GitHub propagates that skip through `needs`. Every v0.2.1 asset has
`uploader=GLinnik21`; every v0.2.0 asset has `uploader=github-actions[bot]`. That is how the
maintainer's home directory shipped inside a public package. Every note now carries a provenance
line, and §6 item 1 refuses to publish without one.

**3. The verification step does not run.** `ipk.sha256` reads
`019b4e14…  pkg/com.beb.plxnative_0.2.1_arm.ipk`, so `shasum -a 256 -c ipk.sha256` fails with *"No
such file or directory"* in the directory a user actually downloads into. Cause is one line —
`Makefile:469`. Neither note prints a command at all.

**4. The compatibility statement is buried and unusable.** v0.2.1's *"Playback is device-verified
only on webOS 4.10"* is at word 549 of 579, below the FFmpeg licence text — and "4.10" is a number
no owner can see. The same television is called webOS 4.5 in `README.md`, in `CLAUDE.md` and in
`rust-modules/src/webos.rs`'s own module doc, while the set itself reports release `4.10.2`. Two
numbering systems with neither named is the exact mechanism that produced "webOS 26". §3 settles it.

**5. v0.2.0 has never been amended, and it is now wrong.** It still says playback *"behaves the
same on every television"* and predicts *"sound and a black picture"*. Three days later real webOS
6 and 10 hardware came back **stuck in buffering**, and the report is the headline of
`docs/webos5-port.md`. Its `.ipk` is still being downloaded. §8 makes a dated erratum mandatory.

**6. v0.2.1 asserts a failure it may have fixed, and never cites the evidence.** The lede says
*"Playback still fails on webOS 5 and newer"* — a status observed on **v0.2.0**, on two of seven
releases, restated as current fact about a build nobody has run there. Commit `afedac6b` is inside
`v0.2.0..v0.2.1` and its own message says it *"fixes the placement bug that most likely IS the
symptom"*. Pessimistic overclaiming is still overclaiming, and it costs us the one report that
would settle the question. It also never mentions that webosbrew's own reviewer got the UI, sign-in
and library working on webOS 6 and 10 — the strongest evidence this project has ever had.

**7. The credential disclosure is filed under "Fixed" and scoped too narrowly.** Every other item
in that list is complete when you install; this one is not — installing stops future writes and
does nothing about a token already in a file. And *"If you have sent anyone a
`/tmp/plxnative-events.log`"* excludes the worst-affected population: at v0.1.0 that file was
`fopen(…, "w")` → mode 0644 in a `/tmp` this repo documents as the shared system `/tmp`, mode 1777
in both jail profiles, with the token-bearing line already present. Nothing had to be sent to
anyone. §4 fixes the placement, the scope and the missing remedy route.

**8. "Every line now passes a scrub" is wider than the code.** `redact_tokens` matches the literal
`X-Plex-Token=` and guards `crate::log` only. `src/starfish.c` writes to the same file with
`fprintf(elogf, …)` at ten sites, and `src/main.c`'s crash tracer at more. Nothing token-bearing
reaches those today — but the sentence claims a class is closed, and the class is closed on one of
two sinks by a matcher for one parameter name.

**9. Numbers typed by a human.** *"This adds about 0.6 MB to the download (4.7 MB to 5.3 MB)."* The
delta is right. The endpoints are not: v0.1.0's `.ipk` is 4,861,038 bytes and v0.2.0's is 5,519,534
— 4.6 → 5.3 MiB, or 4.9 → 5.5 MB. "4.7" is neither, and no unit is named. This is the same class as
the original overclaim. §2 forbids any measurable number that CI did not produce.

---

## 2. The template

Copy `docs/release-notes/TEMPLATE.md`, or copy from here. Sections appear in this order, always.
**Blocks marked FIXED WORDING are not to be improvised** — change them only by changing this
standard.

| # | Section | What goes in it | Present |
|---|---|---|---|
| 1 | Title | `vX.Y.Z — <what this release is for>` | always |
| 2 | Lede | One paragraph, ≤ 4 lines: the release's purpose, not its diff | always |
| 3 | `## If you used …` | Safety disclosure — §4 owns the shape and the trigger | conditional |
| 4 | `## Which televisions this works on` | §3's block, verbatim, edited only by §3's rule | always |
| 5 | `## Help wanted` | A named ask when we need a report from hardware we do not own | conditional |
| 6 | Narrative sections | What changed, ≤ 3 `##` sections, effects before mechanisms | always |
| 7 | `## Fixed` | Bold lead-in clause + what the reader would have seen | conditional |
| 8 | `## Installing` | Which file, both routes ranked, Dev Mode expiry, no root | always |
| 9 | `## Checking what you downloaded` | FIXED WORDING — hash, command, provenance | always |
| 10 | `## Source for the bundled FFmpeg` | FIXED WORDING — LGPL obligation | always while bundled |
| 11 | `## Package facts` | Generated table for audience 2 | always |
| 12 | `## Still the same scope` | The never-built list. Byte-identical between releases | always |
| 13 | `## Known not to work` | The defect boundary. Expected to shrink | always while non-empty |
| 14 | `**Full Changelog**: …` | GitHub appends it | always |
| 15 | `## Updates to this note` | Dated errata, appended after publication — §8 | conditional |

### The skeleton

````markdown
# vX.Y.Z — <what this release is for>

<One paragraph. What this release is FOR. Not a summary of the diff.>

## If you used vA.B.C or earlier: <the reader's action, in the reader's words>

<§4. Delete this whole section when §4's trigger does not fire.>

## Which televisions this works on

<§3's block, verbatim.>

## Help wanted

**If you have a <set>, <the specific thing to try>.** <What changed on that path in this release,
and that nobody has run it there.> <Where to send it.> It working is as useful a report as it
failing.

## <What changed — a heading that claims no more than the evidence under it>

<Prose. State the effect first; the mechanism belongs in one clause, or in the commit.>

## Fixed

- **<The effect, as a reader would have seen it.>** <One clause of mechanism.>

## Installing

Download **com.beb.plxnative_X.Y.Z_arm.ipk** — that is the app. The other four files are the
Homebrew Channel's manifest, the checksum, and the FFmpeg source we are obliged to publish.

Install it with the [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel) or
[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop). **Prefer the Homebrew
Channel:** LG expires a Developer Mode session after about 1000 hours and *uninstalls the apps
installed through it* when it does. You do **not** need a rooted TV — the app runs in LG's normal
sandbox, unprivileged, with no special permissions.

## Checking what you downloaded

<FIXED WORDING — below.>

## Source for the bundled FFmpeg

<FIXED WORDING — below.>

## Package facts

<Generated table — below.>

## Still the same scope

Movies and TV shows, from a Plex Media Server on your own network. No music, no photos, no live TV
or DVR, and no way to type in a server address — the app connects to a server it finds on your
network, or to nothing.

## Known not to work

- <Defect boundary, one line each. Expected to shrink release over release.>
````

### FIXED WORDING — the verification block

Substitute only `<sha256>`, `<X.Y.Z>`, `<commit>`, `<run-url>`.

````markdown
Nothing anywhere in this distribution chain is signed — there is no code signing in the webosbrew
path at all — so this sha256 is what tells you the file you have is the file that was published
here.

```
<sha256>  com.beb.plxnative_<X.Y.Z>_arm.ipk
```

Check it with `shasum -a 256 com.beb.plxnative_<X.Y.Z>_arm.ipk` on macOS or Linux, or
`certutil -hashfile com.beb.plxnative_<X.Y.Z>_arm.ipk SHA256` on Windows. With the `ipk.sha256`
asset beside the file, `sha256sum -c ipk.sha256` does the same in one step.

If the Homebrew Channel installs this for you from its catalogue, it fetches this release's
`com.beb.plxnative.manifest.json`, hashes the download on the television and refuses to install a
package that does not match — you have nothing to do. Pointing the Channel at a bare `.ipk`
yourself skips that check, so check it yourself.

Built by GitHub Actions from commit `<commit>`, [run <run-url>]. The assets attached here are that
run's artifacts, unmodified — nothing was rebuilt or re-uploaded by hand.
````

While defect §1.1 stands, add exactly this and nothing stronger:

````markdown
Two builds of this commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible
across machines yet — the bundled FFmpeg records the directory it was built in — so a hash from
your own rebuild will differ, and that is not tampering.
````

Delete that paragraph and restore *"Builds are reproducible — the same commit and toolchain give a
byte-identical `.ipk`, so you can rebuild with `make RELEASE=1 ipk` and compare"* only when §6
item 5 passes on a clean checkout on a second machine.

### FIXED WORDING — the LGPL block

The three library names are read out of the package, not typed (§6 item 7).

````markdown
This package contains three FFmpeg shared libraries — `libavformat-plx.so.63`,
`libavcodec-plx.so.63` and `libavutil-plx.so.61` — built from **FFmpeg 9.0**, unmodified, and
licensed **LGPL-2.1-or-later**. Demuxers, parsers, bitstream filters and subtitle decoders only:
video and audio are decoded by the television's own hardware.

The complete corresponding source is attached to this release:

- `ffmpeg-9.0.tar.xz` — the pristine upstream tarball, sha256
  `7f607a00dd0d28a729d5a4811205812eef01cf6ef6155025febb6f36a9062d52`, from
  <https://ffmpeg.org/releases/>. No patches are applied.
- `build-ffmpeg.sh` — the complete configure invocation that produced the libraries. Built with
  `--disable-everything` plus an explicit component list, and **without** `--enable-gpl`,
  `--enable-version3` or `--enable-nonfree`, so no GPL or non-free component is present.

They are ordinary shared libraries, `dlopen`ed by absolute path from the app's own directory under
exactly those names — so they can neither shadow nor be shadowed by the television's own FFmpeg,
and a build of your own with the same names replaces ours. Full text in `THIRD-PARTY-NOTICES.md`
and `licenses/LGPL-2.1.txt`, both inside the package.
````

### The package-facts table

Every row is derived, not remembered. Rows whose value CI cannot produce do not go in this table.

````markdown
| | |
|---|---|
| id · version · type | `com.beb.plxnative` · `X.Y.Z` · native |
| download · installed | `<ipkSize>` bytes · `<installedSize>` KiB (both read from the attached manifest) |
| root required | **no** — device-verified: runs as an unprivileged uid with `CapEff: 0`, chrooted, under LG's stock jail profile |
| declared permissions | none — `appinfo.json` declares no `requiredPermissions` |
| `DT_NEEDED` | `<n>` entries, `<unchanged since vA.B.C / changed: …>` (asserted in CI against `ci/expected-dt-needed.txt`) |
| payload | `<+0 files since vA.B.C>` |
| listening sockets | none. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO and the TCP capture listener |
| written outside its own directory | `/tmp/plxnative-{events,stderr,crash}.log`, mode 0600; the signed-in session at `/media/developer/com.beb.plxnative-auth.json` or `/media/internal/.com.beb.plxnative-auth.json`, mode 0600. A crash writes no core file. |
| outbound hosts | your Plex Media Server, `plex.tv`, `discover.provider.plex.tv`. No analytics, telemetry or crash upload. |
| declared `requiredMemory` | 60 MB against a measured ~74 MB peak — under-declared; known, tracked in `docs/distribution.md` §6.10 |
````

Two notes on that table, both load-bearing.

- The listening-sockets row is the single most useful sentence we can give audience 2, because
  anyone reading the source finds a world-writable `/tmp` FIFO driving the UI and a `capture.rs`
  that binds `INADDR_ANY` with no authentication, and has no reason to know both are compiled out.
  State it in the shape the reader's own check produces: while `strings` on the shipped binary
  still prints `/tmp/plxnative-url`, add *"(`strings` also shows `/tmp/plxnative-url` — that is a
  log message, not a path it opens.)"* and open an issue to remove the literal. A property stated
  in a form that fails the reader's own check reads as a lie, not a rounding error.
- **A knowingly wrong number is disclosed, never omitted.** `requiredMemory: 60` against ~74 MB
  measured costs us nothing to admit and the whole release if the reviewer finds it himself.

---

## 3. The compatibility statement — settled

Three rules produce it, and they are the whole answer to the overclaiming history.

1. **Model year first; the platform release in parentheses.** An owner can see the year. They
   cannot see `4.10.2` — that lives in `/var/run/nyx/os_info.json` — and it collides with the
   "webOS 4.5" this repo uses for the same set. Never print a bare release number as the primary
   key, and never mix LG's marketing numbering with webosbrew's platform numbering without saying
   which is which.
2. **Every tier names its evidence class, and only these three verbs exist:**
   **verified** (a human watched it), **reported** (a named person on a named set, with a date and
   the version they ran), **statically checked** (a tool resolved symbols; it grades startup and
   nothing else). Never "supports", never "works on", never "compatible with".
3. **A heading or a tier may never claim more than the evidence under it.**

### The block — FIXED WORDING

````markdown
## Which televisions this works on

- **Plays video — 2019 LG sets.** Verified by watching it, on one television: an LG 49SM9000PLA,
  which reports platform release 4.10.2 (webosbrew's `goldilocks2` bucket; LG markets this
  generation as webOS 4.5). A 21-case suite runs on that set against a live Plex Media Server
  before every release.
- **Starts, signs in and browses your library — but video does not start — webOS 6 and 10.**
  Reported on real hardware by @mariotaku of webosbrew on 2026-08-09, running v0.2.0
  ([apps-repo#224](https://github.com/webosbrew/apps-repo/pull/224)): the app opens and the library
  works, and pressing play leaves a spinner that never resolves. Nothing else there is known to be
  broken.
- **Starts — and nothing further is known — every other firmware from webOS 4.0 up.** The loader
  resolves this binary's libraries and symbols cleanly against all nine firmware images webosbrew
  has at 4.0 or newer, webOS 4.4.2 through 11.2.0. That is a static check against LG's own symbol
  tables: it says the process starts, and says nothing at all about whether video plays.
- **Does not start — webOS 3.9.2 and older.** Symbols the app needs are missing from those
  firmwares, so the loader kills the process before anything appears. Installing it there gets you
  a tile that does nothing.

If your set is in the middle two groups, tell us what happened —
[open an issue](https://github.com/GLinnik21/plx-native/issues). It working is as useful a report
as it failing.
````

### The rule for updating it

- **One television moves one line, and only the line it belongs to.** A verified set does not
  promote a range, a release, or a model year it did not sit in. "Plays video — 2019 sets" is
  written that way because 4.4.2 (2018) has been statically checked and never watched.
- **A third-party report is added with `@who`, the date and the version they ran.** Never
  generalise a report from one release to its neighbours, and never restate someone's hedge as a
  verdict.
- **When a release changes something on a path that is broken, do not restate the old failure as
  present fact.** Say what changed and that nobody has run it there. That is `Help wanted`, and it
  is the only way this project ever gets the report it needs.
- **The static line is regenerated from CI's `webosbrew-ipk-verify` output every release.** Never
  retyped. If the count or the range changes, it changes because the tool said so.
- **A compatibility or scope change is a two-artifact change**: this note *and* a PR to
  `webosbrew/apps-repo` updating the package description, which is the only text an owner reads
  before installing. Sync `docs/webosbrew-package.yml` in the same commit; it is already drifting
  from the submitted copy.

---

## 4. Safety disclosure

### What triggers the rule

A release carries a safety section when it changes, or reveals that an earlier release had, any of:

- a credential reaching a file, a log, a screen or the network;
- **who can read** a file the app writes (a mode change, a shared directory);
- what the app writes **outside its own directories**;
- what leaves the network — a new host, a new request;
- a listener, a FIFO, or any other inbound surface.

That list is deliberately wider than "credential". A 0644 file in a 1777 `/tmp`, a 209 MB core dump
onto a 615 MB partition shared with every app on the set, an unauthenticated `0.0.0.0:8910`
listener and a per-binary device identifier are all in scope — and every one of them is invisible
to `webosbrew-ipk-verify`, which grades only whether the app starts.

### Where it goes

**Its own `##` section, immediately after the lede, above every feature section.** Never inside
`Fixed`. The test is mechanical: *if installing the release does not complete the item, it does not
belong in a list of things installing completes.* Leave one cross-reference line in `Fixed` so a
changelog reader is routed rather than told twice.

Title it with **the reader's action**, not our defect: `## If you ran v0.1.0 or v0.2.0: rotate your
Plex token`, not `## The event log printed credentials`.

Cap it at about eight lines. Depth belongs in the commit and in `docs/`.

### The shape — FIXED

````markdown
## If you used vA.B.C or earlier: <the action, in the reader's words>

<What was exposed, and what someone holding it can do — in the user's terms, one sentence.
"That token is a password to your Plex server: anyone holding it can browse and stream your
library." Not "credentials".>

<Which released versions are affected, by number, and which are not. If we ever asked users to
send us the affected artefact, say so here.>

**What to do:** <the exact place a person with a remote and a phone can reach> — <and what it
costs them: "the television will ask you to sign in again with the QR code; that is expected">.

**What is fixed, and what is not:** <the fix, scoped to the sink and the shape it actually
covers> <— and the residual: what that file or panel still contains.>
````

### Five rules that decide the wording

1. **Unconditional whenever the condition is one the reader cannot evaluate.** "If you have sent
   anyone a log" asks a user to know something they do not, and it excluded the population whose
   file was world-readable in a shared `/tmp`. Where the action is cheap and the condition is
   uncertain, the action is unconditional.
2. **Name the versions.** There have been three. "An earlier version" is not actionable; "v0.1.0
   and v0.2.0 are affected, v0.2.1 is not" is, and it is checkable.
3. **Never claim a class is closed more widely than the code closes it.** Name the sink and the
   shape: *"every line the app's Rust logger writes has any `X-Plex-Token=` value stripped"* — and
   say what still writes to the same file unscrubbed.
4. **Always state the residual.** The event log still records the server's name, its LAN address,
   Plex Home profile names and episode titles. A disclosure that stops at "fixed" invites the
   reader to treat the file as safe to post, which is how the next incident starts.
5. **Whenever we ask the reader to send us anything, the same paragraph says what is in it.** The
   diagnostics panel is safe because `Diag` is numbers, booleans and enums — but the *photograph*
   is of their television, and our own transport draws the now-playing title over it. So: *"The
   panel shows no server name, no address, no account details and no title. The rest of the
   picture is your television, though — pause on something you don't mind posting, or crop to the
   panel."*

### What never appears

No CVSS, no CVE, no GitHub Security Advisory, no "critical"/"low-risk" label — nothing consumes
this as a dependency and a score we cannot compute is theatre. No "we take security seriously", no
apology paragraph, no root-cause essay, no process-improvement list: every sentence must help the
reader decide whether they are affected, tell them what to do, or let the reviewer judge the fix.
And never a reassurance we cannot support — there is no telemetry and no update push, so *"no
evidence of misuse"* and *"few users affected"* are unsupportable. The honest sentence is: *"the
app collects nothing, so there is no way for us to tell whether this happened to you — which is why
this instruction has no conditions on it."*

### Backport

When a fix reveals that a *published* release was exposed, that release's note gets a dated erratum
(§8) linking forward, on the same day. A user stranded on v0.1.0 never sees v0.2.1's page.

---

## 5. Versions, and what a bump means

The mechanics are enforced elsewhere and are not restated here: `ci/bump-version.py` refuses
anything but three integers (LG will not install `1.0.0-rc1`), and `.github/workflows/release.yml`
pins `draft: false` / `prerelease: false` with the reason inline — either one drops the release out
of `releases/latest`, which is the URL the Homebrew Channel manifest resolves through. **There is
no beta channel and there never will be.**

What is *not* written down anywhere, and belongs here because it decides what the note must carry:

- **patch** — fixes, diagnostics and internals. The same app, working better or explaining itself.
- **minor** — something a user can see is new, or a change in which televisions are supported.
- **major** — reserved. `1.0.0` means playback is device-verified on more than one platform
  generation.

---

## 6. The pre-publish checklist

Run this against the **published** release before announcing it anywhere, and paste the output into
the release PR or the notes file's commit message. Every item is a command. Items 1, 2, 3, 5 and 9
are the ones that would have caught what actually shipped.

```sh
V=0.3.0; PREV=0.2.1; REPO=GLinnik21/plx-native
D=$(mktemp -d) && cd "$D" && gh release download "v$V" --repo "$REPO"
```

**1 — CI built this, not a laptop.** (v0.2.1 fails.)

```sh
gh api repos/$REPO/releases/tags/v$V --jq '.assets[]|"\(.uploader.login)  \(.name)"'
# every line must read github-actions[bot]
gh run list --repo $REPO --workflow Release --limit 5
gh run view <run-id> --repo $REPO   # build + verify / redistributable assets / publish must be ✓, not "-"
```

**2 — the note names that commit and that run**, and the sha it names is the tag's commit:

```sh
git rev-parse "v$V"
gh release view "v$V" --repo $REPO --json body -q .body | grep -oE '\b[0-9a-f]{7,40}\b|actions/runs/[0-9]+'
```

**3 — four copies of the hash agree, and the checksum file verifies where a user stands.**
(The last line fails today: `Makefile:469` writes a `pkg/` prefix.)

```sh
shasum -a 256 com.beb.plxnative_${V}_arm.ipk
python3 -c "import json;print(json.load(open('com.beb.plxnative.manifest.json'))['ipkHash']['sha256'])"
cat ipk.sha256
gh release view "v$V" --repo $REPO --json body -q .body | grep -oE '[0-9a-f]{64}'
shasum -a 256 -c ipk.sha256          # must print OK — not "No such file or directory"
```

**4 — every size in the note came from the manifest, with its unit named.**

```sh
python3 -c "import json;m=json.load(open('com.beb.plxnative.manifest.json'));print(m['ipkSize'],'bytes download;',m['installedSize'],'KiB installed')"
```

`installedSize` is never called a download. If neither number earns its place, print neither.

**5 — no build-host path anywhere in the payload.** This is the reproducibility gate, and it is the
check `ci/check-elf.sh` does not perform (it scans `pkg/plxnative` only). Both published releases
fail it.

```sh
mkdir -p x && ar p com.beb.plxnative_${V}_arm.ipk data.tar.gz | tar xz -C x
for f in x/usr/palm/applications/com.beb.plxnative/*; do
  strings -a "$f" | grep -qE '(^|[^[:alnum:]/_.-])/(Users|home)/[a-z]' && echo "BUILD-HOST PATH: $f"
done
```

Any output → the reproducibility paragraph stays in its interim form (§2), and the fix is to build
FFmpeg under a fixed prefix and extend `check-elf.sh` over every staged ELF.

**6 — the payload gained nothing unannounced.**

```sh
gh release download "v$PREV" --repo $REPO -p '*.ipk' -D prev
mkdir -p xp && ar p prev/com.beb.plxnative_${PREV}_arm.ipk data.tar.gz | tar xz -C xp
diff <(cd xp && find . -type f | sort) <(cd x && find . -type f | sort)
```

**7 — the LGPL block names the libraries that are actually there, and the tarball is upstream.**
(`THIRD-PARTY-NOTICES.md` §1.1 currently lists a fourth, `libswscale-plx.so.10`, that release
builds do not ship — fix the notice, not the note.)

```sh
ls x/usr/palm/applications/com.beb.plxnative/*.so.*
shasum -a 256 ffmpeg-9.0.tar.xz    # 7f607a00dd0d28a729d5a4811205812eef01cf6ef6155025febb6f36a9062d52
grep -n '^SHA256=' build-ffmpeg.sh
grep -c -- '--enable-gpl\|--enable-nonfree\|--enable-version3' build-ffmpeg.sh   # must be 0
```

**8 — the compatibility block was regenerated, not retyped.**

```sh
tools/fwcompat.py                                   # or the run's ipk-verify job summary
gh run view <run-id> --repo $REPO --log | grep -A30 'Compatibility check'
```

Every firmware named in the note appears there with that verdict. Every "verified" or "reported"
tier names a set, a version, a date and a witness.

**9 — every version number in the note exists.** This is the "webOS 26" gate.

```sh
gh release view "v$V" --repo $REPO --json body -q .body \
  | grep -oE 'webOS [0-9]+(\.[0-9]+)*|v[0-9]+\.[0-9]+\.[0-9]+' | sort -u
git tag --list 'v*'
grep -nE '^\| \*{0,2}[0-9]+\.[0-9]+' docs/webos5-port.md   # the firmware ↔ release ↔ year table
```

Every `vX.Y.Z` must be a tag; every `webOS N` must appear in the firmware table or in
`fwcompat.py`'s matrix. Anything else is deleted, not softened.

**10 — the safety trigger was evaluated, not skipped.**

```sh
git diff "v$PREV".."v$V" -- rust-modules/src src ci \
  | grep -nE '^\+.*(token|X-Plex-Token|mkfifo|bind\(|listen\(|O_CREAT|fopen|0o?6[0-9]{2}|/tmp/|/media/)'
```

Any hit → decide explicitly whether §4's section is owed, and record the decision in the notes
file's commit message. "No hits" is also a decision worth recording.

**11 — the banned phrases are absent, or justified.**

```sh
gh release view "v$V" --repo $REPO --json body -q .body \
  | grep -niE 'reproducible|byte-identical|every firmware|fully support|works on|guarantee|no known issues'
```

**12 — links resolve.**

```sh
gh release view "v$V" --repo $REPO --json body -q .body | grep -oE 'https?://[^ )]+' \
  | xargs -n1 -I{} sh -c 'printf "%s " {}; curl -o /dev/null -s -w "%{http_code}\n" -L {}'
```

**13 — the published body is the committed file.**

```sh
diff <(gh release view "v$V" --repo $REPO --json body -q .body) docs/release-notes/v$V.md
# only GitHub's appended "**Full Changelog**" line may differ
```

---

## 7. What we do not do, and why

- **No `CHANGELOG.md`.** `docs/release-notes/vX.Y.Z.md` published as the release body is the same
  record with one copy. A second hand-maintained file in a repo with a documented drift problem
  (`CLAUDE.md` claimed 59 tests against a ~284-test suite for months) is a liability, not a record.
- **No Keep a Changelog buckets.** Added / Changed / Deprecated / Removed have never had a
  meaningful entry here, and naming empty sections is how a note starts padding itself. We take
  exactly one thing from that spec — a `Security` item is not a `Fixed` item — and its principle,
  that changelogs are for humans.
- **No auto-generated "What's Changed" PR list as the body.** It is the default across every
  comparable app and it cannot express a compatibility claim, a defect boundary or an ask, which
  are the three things these notes exist for. **Keep the `Full Changelog` compare link** — this
  repo's commit subjects are already written for people ("webos5: it runs on 6 and 10 — only
  playback is unfinished, so the ceiling comes off"), so it is a real second record for audience 3
  at zero cost.
- **No emoji, no `.github/release.yml` category config, no "New Contributors" block.** The
  neighbourhood's emoji headings exist to give an auto-generated dump some shape; these notes have
  shape. The most important sentences here are a credential-rotation instruction and a
  compatibility claim being read by someone deciding whether to trust an unsigned binary made with
  AI assistance. A 🎉 beside those spends credibility this project cannot re-buy: nothing is
  signed and one television is tested, so the prose *is* the warranty.
- **No prerelease, no draft, no `-rc`, no four-component versions.** All four fail silently rather
  than loudly. See §5.
- **No security-advisory apparatus** — no CVE, no CVSS, no severity labels. See §4.
- **No per-firmware table.** Fourteen uniform rows imply fourteen independent pieces of evidence.
  There is one television, one third-party report and one static tool. Tiers make the evidence
  visible; a table hides it.
- **No comparative claims about the official Plex app.** Fine as positioning in the README; in a
  release note it is an unverifiable assertion about a third party that a channel reviewer would
  have to defend or strip, and it buys the release nothing.
- **No marketing verbs** — "blazing", "massively improved", "now fully supports".
- **No pasted log excerpt, sample URL or screenshot** containing a server name, LAN address,
  `machineIdentifier`, media title, profile name or token. The diagnostics panel is structurally
  safe so that photographs can be posted; a release note must not undo that.
- **No number a reader can measure that a human typed.** Sizes, hashes, firmware counts, test
  counts. This project already proved the point twice.
- **No silent edit of a published note.** See §8.

---

## 8. Where this lives, and how it is written

**This file:** `docs/release-notes/README.md` — the standard. GitHub renders it when anyone opens
the directory, which is also where the notes are.

**Each release:** `docs/release-notes/vX.Y.Z.md`, committed in the same `release: X.Y.Z` commit
`ci/bump-version.py` already creates, and published with `body_path:` (plus `append_body: true` to
keep GitHub's compare-link tail). Today the prose is pasted onto an already-published release by
hand: it exists nowhere under review, `releases/latest` briefly carries a one-line body, and the
documented `rebuild_tag:` path re-publishes without it. Committing it makes the record reviewable
in a diff and identical on a rebuild — and removes the last argument for a `CHANGELOG.md`.

**Rewriting a published note.** Owner decision, 11 August 2026, overriding the errata-only rule
this section previously carried: when a note is wrong or predates this standard, **rewrite it to the
template** rather than appending a dated correction on top. The reasoning is that these notes have
exactly one job — telling a stranger what they are installing — and a reader arriving today is
served by a note that is simply correct, not by an archaeology of what was believed in August. A
correction stacked above a wrong paragraph leaves both on the page and makes the reader adjudicate.

Two things survive that decision, because they protect the reader rather than the record:

* **A safety disclosure is never dropped when a note is rewritten.** It moves into the template's
  own §3 (`## If you used vA.B.C or earlier: …`), which is where it belongs and where a reader
  scanning on a phone will actually meet it. v0.1.0 and v0.2.0 both carry the token disclosure for
  this reason even though neither mentioned it when published.
* **Anything a reader could have acted on stays true or stays stated.** If a rewrite changes what a
  release claims about itself — how it was built, whether a command works — the new text says so
  plainly in the section that owns it, rather than quietly dropping it. v0.2.1's note says in its
  verification section that it was published by hand.

The record of what a note used to say lives in `git log` for these files, which is a better archive
than a page of stacked corrections.

Editing a release body touches no asset: the manifest's `ipkHash` covers the `.ipk` only, and
`draft`/`prerelease` stay false, so `releases/latest` is unaffected. Verify rather than assume —
`ci/verify-published.sh vX.Y.Z` re-checks that the note still quotes the artifact's real hash.

### How this connects to what already exists

- **`docs/distribution.md` §7a ("Cutting a release")** gains one step in front of *"Actions →
  Release → Run workflow"*: write `docs/release-notes/vX.Y.Z.md` from this standard first, and
  pick the bump level by §5. §7a keeps owning the mechanics; this file owns the words.
- **`.github/workflows/release.yml`** publishes with `body_path`, and gains one assertion in the
  publish job: the body must contain the artifact's real sha256 and the current compatibility
  block, or the release fails. That is the same shape as `ci/check-package.py`'s four-file version
  check. Because the body is currently pasted after publication, the assertion must also fire on
  `release: edited`, or it grades the seed rather than the text.
- **`CLAUDE.md`** gains one line in *Key files*: `docs/release-notes/` — the per-version record and
  the standard the notes are written to.
- **`README.md`** already carries the Dev Mode expiry warning and the honest scope; the notes link
  to it rather than restating it.
- **`docs/webosbrew-package.yml` + apps-repo #224** are the second artifact of any compatibility or
  scope change (§3).

### The four repo fixes this standard depends on

1. `Makefile:469` — write a bare filename into `ipk.sha256` so `sha256sum -c` works where a user
   downloads. Nothing consumes the path form.
2. `.github/workflows/release.yml` — the `push: tags` path currently skips `build`, `legal-gate`
   and `publish` (a skipped `prepare` propagates through `needs`). Either make those jobs
   `if: always() && …` or refuse to publish from a run that did not build.
3. `ci/build-ffmpeg.sh` + `ci/check-elf.sh` — build FFmpeg under a fixed prefix, and scan every
   staged ELF for build-host paths. Until then the reproducibility claim stays banned (§2).
4. `THIRD-PARTY-NOTICES.md` §1.1 — its table lists `libswscale-plx.so.10`, which release builds do
   not ship. The notice must name the libraries in the package.
