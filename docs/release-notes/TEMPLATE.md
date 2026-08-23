<!--
Copy to docs/release-notes/vX.Y.Z.md and fill in. The standard is README.md in this directory;
the blocks marked FIXED are not to be reworded here. Delete the conditional sections that do not
apply, and delete these comments.

Order is fixed: lede -> safety -> compatibility -> help wanted -> what changed -> fixed ->
installing -> hash -> LGPL -> package facts -> scope -> known not to work.
-->

# vX.Y.Z — <what this release is for>

<One paragraph, four lines at most. What this release is FOR, not a summary of the diff.>

<!-- CONDITIONAL — see README.md §4 for the trigger. Delete the whole section if it does not fire.
     Titled with the reader's ACTION, never with our defect. Above the features, always. -->
## If you used vA.B.C or earlier: <the action, in the reader's words>

<What was exposed and what someone holding it can do, in the user's terms — one sentence.>
<Which released versions are affected, by number, and which are not. If we ever asked users to send
us the affected artefact, say so.>

**What to do:** <the exact place a person with a remote and a phone can reach> — <and what it costs
them>.

**What is fixed, and what is not:** <the fix, scoped to the sink and the shape it covers> — <what
that file or panel still contains>.

<!-- ALWAYS. FIXED — regenerate the static line from CI's ipk-verify output; edit the tiers only by
     README.md §3's rule. -->
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

<!-- CONDITIONAL — when this release needs a report from hardware nobody here owns. -->
## Help wanted

**If you have a <set>, <the specific thing to try>.** <What changed on that path in this release.>
Nobody has been able to run it there. <Where to send the result.> It working is as useful a report
as it failing.

<!-- ALWAYS — up to three sections. A heading may never claim more than the evidence under it.
     State the effect first; mechanism gets one clause, or goes in the commit message. -->
## <What changed>

<Prose.>

<!-- CONDITIONAL — bold lead-in carrying the whole claim, as an owner would have seen it. -->
## Fixed

- **<The effect.>** <One clause of mechanism.>

<!-- ALWAYS -->
## Installing

Download **com.beb.plxnative_X.Y.Z_arm.ipk** — that is the app. The other four files are the
Homebrew Channel's manifest, the checksum, and the FFmpeg source we are obliged to publish.

Install it with the [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel) or
[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop). **Prefer the Homebrew
Channel:** LG expires a Developer Mode session after about 1000 hours and *uninstalls the apps
installed through it* when it does. You do **not** need a rooted TV — the app runs in LG's normal
sandbox, unprivileged, with no special permissions.

<!-- ALWAYS. FIXED WORDING — substitute only the hash, the version, the commit and the run URL. -->
## Checking what you downloaded

Nothing anywhere in this distribution chain is signed — there is no code signing in the webosbrew
path at all — so this sha256 is what tells you the file you have is the file that was published
here.

```
<sha256>  com.beb.plxnative_X.Y.Z_arm.ipk
```

Check it with `shasum -a 256 com.beb.plxnative_X.Y.Z_arm.ipk` on macOS or Linux, or
`certutil -hashfile com.beb.plxnative_X.Y.Z_arm.ipk SHA256` on Windows. With the `ipk.sha256`
asset beside the file, `sha256sum -c ipk.sha256` does the same in one step.

If the Homebrew Channel installs this for you from its catalogue, it fetches this release's
`com.beb.plxnative.manifest.json`, hashes the download on the television and refuses to install a
package that does not match — you have nothing to do. Pointing the Channel at a bare `.ipk`
yourself skips that check, so check it yourself.

Two builds of this commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible
across machines yet — the bundled FFmpeg records the directory it was built in — so a hash from
your own rebuild will differ, and that is not tampering.

Built by GitHub Actions from commit `<commit>`,
[run <id>](https://github.com/GLinnik21/plx-native/actions/runs/<id>). The assets attached here are
that run's artifacts, unmodified — nothing was rebuilt or re-uploaded by hand.

<!-- ALWAYS while FFmpeg is bundled. FIXED WORDING — read the library names out of the package. -->
## Source for the bundled FFmpeg

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

<!-- ALWAYS — every row derived, never remembered. -->
## Package facts

| | |
|---|---|
| id · version · type | `com.beb.plxnative` · `X.Y.Z` · native |
| download · installed | `<ipkSize>` bytes · `<installedSize>` KiB (both read from the attached manifest) |
| root required | **no** — device-verified: runs as an unprivileged uid with `CapEff: 0`, chrooted, under LG's stock jail profile |
| declared permissions | none — `appinfo.json` declares no `requiredPermissions` |
| `DT_NEEDED` | `<n>` entries, `<unchanged since vA.B.C>` (asserted in CI against `ci/expected-dt-needed.txt`) |
| payload | `<+0 files since vA.B.C>` |
| listening sockets | none. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO and the TCP capture listener. (`strings` on the binary also shows `/tmp/plxnative-url` — that is a log message, not a path it opens.) |
| written outside its own directory | `/tmp/plxnative-{events,stderr,crash}.log`, mode 0600; the signed-in session at `/media/developer/com.beb.plxnative-auth.json` or `/media/internal/.com.beb.plxnative-auth.json`, mode 0600; and where you were when you last closed it, at `/media/developer/com.beb.plxnative-lastplace.json` or `/media/internal/.com.beb.plxnative-lastplace.json`, mode 0600 — the page, your Plex Home profile id and one server/item id, with no token and no playback position in it. A crash writes no core file. |
| outbound hosts | your Plex Media Server, `plex.tv`, `discover.provider.plex.tv`. No analytics, telemetry or crash upload. |
| declared `requiredMemory` | 160 MB, against a peak of 152 MiB measured on the dev set across boot, browsing and 4K HDR playback. Was 60, which was below the 120 MB webOS substitutes when an app declares nothing. |

<!-- ALWAYS — byte-identical between releases. Changes only when the code changes. -->
## Still the same scope

Movies and TV shows, from a Plex Media Server on your own network. No music, no photos, no live TV
or DVR, and no way to type in a server address — the app connects to a server it finds on your
network, or to nothing.

<!-- ALWAYS while non-empty. Defects with a trajectory, not product decisions. -->
## Known not to work

- Playback on webOS 5 and newer (above).
- Remote-only servers, Plex Relay, servers that require secure connections, and servers reached by
  hostname or IPv6 — the app finds a server on your own network by numeric address, and there is
  nowhere to type one in.
- The app tells your server it can decode HEVC, 4K and 10-bit without asking the television,
  because that is what the developer's set does. On a lower-end webOS 4.x panel that is wrong, and
  so is the fallback.
