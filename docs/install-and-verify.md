# Installing PlxNative, and checking what you downloaded

This page does not change between releases. A release note tells you what is new in one version; this tells you how to install any of them, how to check that the file you have is the file that was published, and what the app does on your television once it is there.

Per-release facts — the hash, the sizes, the payload, what was tested on which set — are in that version's [technical audit](https://github.com/GLinnik21/plx-native/tree/main/docs/release-audits).

## Which file to download

A release attaches five files. **You need the first one.**

| File | What it is |
|---|---|
| `com.beb.plxnative_X.Y.Z_arm.ipk` | The app. |
| `com.beb.plxnative.manifest.json` | The Homebrew Channel's manifest — how the Channel finds and verifies the update. |
| `ipk.sha256` | The checksum, for `sha256sum -c`. |
| `ffmpeg-9.0.tar.xz` | The pristine upstream FFmpeg source, published because we are obliged to. |
| `build-ffmpeg.sh` | The complete configure invocation that produced the bundled FFmpeg libraries. |

## Installing it

Two routes, and one of them is better:

- **The [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel)** — install any `.ipk` you point it at, whether or not it is in the catalogue. **Prefer this.**
- **[dev-manager-desktop](https://github.com/webosbrew/dev-manager-desktop)**, over LG Developer Mode.

**Developer Mode expires.** LG ends a Dev Mode session after about 1000 hours and *uninstalls the apps installed through it* when it does. dev-manager-desktop can renew the session before that happens; the Homebrew Channel has no expiry at all. This is the whole reason for the ranking.

**You do not need a rooted television.** The app runs in LG's normal sandbox: an unprivileged uid, chrooted, under the stock jail profile, with no capabilities and no `requiredPermissions` declared. Root matters only for the development loop, not for using it.

## Checking what you downloaded

Nothing anywhere in this distribution chain is signed — there is no code signing in the webosbrew path at all — so the sha256 in the release note is what tells you the file you have is the file that was published there.

```sh
shasum -a 256 com.beb.plxnative_X.Y.Z_arm.ipk        # macOS, Linux
sha256sum -c ipk.sha256                              # with the checksum asset beside it
certutil -hashfile com.beb.plxnative_X.Y.Z_arm.ipk SHA256   # Windows
```

**If the Homebrew Channel installs the update for you from its catalogue, you have nothing to do.** It fetches that release's `com.beb.plxnative.manifest.json`, hashes the download on the television, and refuses to install a package that does not match. Pointing the Channel at a bare `.ipk` yourself skips that check, so do it yourself.

**On rebuilding it to compare.** Two builds of one commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible across machines yet — the bundled FFmpeg records the toolchain paths it was built against — so a hash from your own rebuild will differ, and that is not tampering. Each audit's `Reproducibility evidence` section shows exactly which paths a given package carries.

Every release is built and uploaded by GitHub Actions from the tag. If a release's assets were uploaded by a person rather than by `github-actions[bot]`, the build and verification gates did not run — the audit records the uploader for exactly this reason.

## What the app does on your television

Invariant across releases. Where a release changes one of these, its note says so and its audit measures it.

**What it writes, all mode 0600:**

- `/tmp/plxnative-events.log`, `/tmp/plxnative-stderr.log` and `/tmp/plxnative-crash.log` — the first two truncated each launch, the crash log append-only so it survives a restart. Every line is scrubbed **before it is written**: tokens, header and query credentials, hostnames (including the `plex.direct` names that encode your LAN address), bare addresses, Plex GUIDs, search queries and your server and profile names are rewritten, and media titles, search terms and subtitle text are never written at all. What remains is ratingKeys — server-local item numbers, which are what a playback bug is diagnosed from. Someone with access to the same server could map one back to an item, so still think before posting a log publicly. [`PRIVACY.md`](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md) is the full contract.
- Your signed-in session, as `<id>-auth.json` under `/media/developer` or `/media/internal` — one access token per server your account can reach.
- Where you were when you last closed it, as `<id>-lastplace.json` beside the session file: the page, your profile id and one server and item id.

A crash writes no core file.

**What it reads outside its own directory:** the television's own codec table at `/etc/umediaserver/device_codec_capability_config.json`, and its firmware identity at `/var/run/nyx/os_info.json` and `/var/run/nyx/device_info.json`. All three are published by the platform, read once at boot, never written.

**What it reaches:** `plex.tv` and `discover.provider.plex.tv` over TLS, and the Plex Media Servers your account can reach — your own and any shared with you — at a LAN address where one answers and a public one otherwise. **No analytics, no telemetry, no crash upload.** Nothing is sent anywhere else.

**What listens:** nothing. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO and the TCP capture listener that exist in a development build. Each audit measures this on the shipped bytes rather than asserting it.

## Scope

Movies and TV shows, from a Plex Media Server your account can reach. No music, no photos, no live TV, no DVR. There is deliberately nowhere on the television to type a server address — configure servers on a phone or a PC, and the app offers what your account already knows about.

## The bundled FFmpeg

The package contains three FFmpeg shared libraries — `libavformat-plx.so.63`, `libavcodec-plx.so.63` and `libavutil-plx.so.61` — built from **FFmpeg 9.0**, unmodified, and licensed **LGPL-2.1-or-later**. Demuxers, parsers, bitstream filters and subtitle decoders only: video and audio are decoded by the television's own hardware.

The complete corresponding source accompanies every release, as LGPL-2.1 §6 requires and not as a courtesy: `ffmpeg-9.0.tar.xz` is the pristine upstream tarball with no patches applied, and `build-ffmpeg.sh` is the complete configure invocation that produced the libraries. It is built with `--disable-everything` plus an explicit component list, and **without** `--enable-gpl`, `--enable-version3` or `--enable-nonfree`, so no GPL or non-free component is present. Each audit quotes the configure string recorded inside `libavutil` itself, which is the primary evidence for that.

They are ordinary shared libraries, `dlopen`ed by absolute path out of the app's own directory under exactly those names, so they can neither shadow nor be shadowed by the television's own FFmpeg — and a build of your own with the same names replaces ours. Full licence text travels inside the package, in `THIRD-PARTY-NOTICES.md` and `licenses/`.

The bundled build is configured `--disable-network` with `file` as its only protocol, so it cannot open a URL at all; everything it demuxes arrives through the app's own transport.
