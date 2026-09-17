# Installing PlxNative, and checking what you downloaded

PlxNative is not yet available in the LG Content Store, so it needs to be installed as a developer app.

**You do not need to root your TV.** If you are starting with a normal LG TV and have never installed a homebrew app before, follow the Developer Mode path below. If you already have Homebrew Channel, installation is much shorter.

## Already have Homebrew Channel?

Open **Homebrew Channel** on your TV, find **PlxNative** in the catalogue, and select **Install**.

That's it. You do not need to download an `.ipk` manually.

If you do not have Homebrew Channel installed, continue below.

## Starting from a normal LG TV

You will need:

- an LG webOS TV supported by PlxNative;
- a Plex account and Plex Media Server;
- a Mac, Windows PC, or Linux computer;
- the TV and computer connected to the same network.

The setup has two parts:

1. Enable LG Developer Mode and connect your TV to webOS Dev Manager.
2. Choose whether to install PlxNative directly or install Homebrew Channel first.

No command line, LG SDK, or IDE is required.

### 1. Enable Developer Mode

LG provides an official **Developer Mode** app for webOS TVs.

Follow [LG's official Developer Mode guide](https://webostv.developer.lge.com/develop/getting-started/developer-mode-app) to:

1. create or sign in to an LG Developer account;
2. install and open the **Developer Mode** app on the TV;
3. turn **Dev Mode Status** on and let the TV restart;
4. open Developer Mode again and turn **Key Server** on.

LG's guide continues into its own developer tooling after that. You do not need to install webOS Studio or use the command line for PlxNative.

### 2. Install webOS Dev Manager

Install [webOS Dev Manager](https://github.com/webosbrew/dev-manager-desktop) on your computer. It is available for macOS, Windows, and Linux and can add a Developer Mode TV without the LG SDK or command-line tools.

Open Dev Manager, add your television, and follow its connection flow. You will use the Key Server you enabled in the previous step.

Once the TV appears as connected, choose one of the two installation methods below.

### Option A — install PlxNative directly

**Choose this if you only want PlxNative.**

1. Open the [latest PlxNative release](https://github.com/GLinnik21/plx-native/releases/latest).
2. Download `com.beb.plxnative_X.Y.Z_arm.ipk`.
3. In webOS Dev Manager, open the Apps view for your TV and choose **Install**.
4. Select the downloaded `.ipk`.
5. Wait for installation to finish.

PlxNative should now appear in the TV's app launcher. Launch it and follow the QR-code sign-in flow to connect your Plex account.

When a new PlxNative release is available, installing the newer `.ipk` through Dev Manager upgrades the existing installation.

### Option B — install Homebrew Channel first

**Choose this if you want an app catalogue on the TV or expect to use other webOS homebrew apps.**

1. Use webOS Dev Manager to install **Homebrew Channel**.
2. Launch Homebrew Channel on your TV.
3. Find **PlxNative** in the catalogue.
4. Select **Install**.

Future PlxNative releases can then be installed from Homebrew Channel instead of downloading `.ipk` files manually.

Installing Homebrew Channel does **not** require rooting the TV. However, if Homebrew Channel itself was installed through Developer Mode, it and the apps installed through that Developer Mode environment are still subject to Developer Mode expiry.

## Important: Developer Mode expires

LG enables Developer Mode for a limited session. The Developer Mode app shows the remaining time in **Remain Session**.

Before it expires, connect the TV to the network, open Developer Mode, and select **EXTEND**. LG does not allow an already-expired session to be extended.

If the Developer Mode session expires, Developer Mode apps are removed. This also matters when you installed Homebrew Channel through Developer Mode.

A rooted Homebrew setup is different, but **root is not required to use PlxNative**.

## Which route should I use?

If you are starting from scratch and only want PlxNative:

**Developer Mode → webOS Dev Manager → PlxNative `.ipk`**

If you also want the webOS homebrew catalogue:

**Developer Mode → webOS Dev Manager → Homebrew Channel → PlxNative**

If Homebrew Channel is already installed:

**Homebrew Channel → PlxNative**

---

# Checking what you downloaded

The rest of this page covers the release files, package verification, provenance, and what the app reads, writes, and connects to once it is installed.

Per-release facts — the hash, sizes, payload, and what was tested on which set — are in that version's [technical audit](https://github.com/GLinnik21/plx-native/tree/main/docs/release-audits).

## Which file to download

A release attaches five files. **For a direct installation, you need the first one.**

| File | What it is |
|---|---|
| `com.beb.plxnative_X.Y.Z_arm.ipk` | The app. |
| `com.beb.plxnative.manifest.json` | The Homebrew Channel manifest — how the Channel finds and verifies the update. |
| `ipk.sha256` | The checksum, for `sha256sum -c`. |
| `ffmpeg-9.0.tar.xz` | The pristine upstream FFmpeg source, published because we are obliged to. |
| `build-ffmpeg.sh` | The complete configure invocation that produced the bundled FFmpeg libraries. |

## Verifying the package

Nothing in this distribution chain is code-signed, so the SHA-256 published with the release is what tells you that the file you downloaded is the file that was published there.

```sh
shasum -a 256 com.beb.plxnative_X.Y.Z_arm.ipk              # macOS
sha256sum -c ipk.sha256                                    # Linux, with the checksum asset beside it
certutil -hashfile com.beb.plxnative_X.Y.Z_arm.ipk SHA256 # Windows
```

**If Homebrew Channel installs PlxNative from its catalogue, you have nothing to verify manually.** It fetches that release's `com.beb.plxnative.manifest.json`, hashes the download on the television, and refuses to install a package that does not match.

If you point Homebrew Channel at a bare `.ipk` yourself instead of installing the catalogue entry, that catalogue verification path is bypassed, so verify the package yourself.

### Rebuilding it to compare

Two builds of one commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible across machines yet — the bundled FFmpeg records the toolchain paths it was built against — so a hash from your own rebuild will differ, and that is not evidence of tampering. Each audit's **Reproducibility evidence** section shows exactly which paths a given package carries.

Every release is built and uploaded by GitHub Actions from the tag. If a release's assets were uploaded by a person rather than by `github-actions[bot]`, the build and verification gates did not run — the audit records the uploader for exactly this reason.

# What the app does on your television

This section is invariant across releases. Where a release changes one of these behaviours, its release note says so and its audit measures it.

## What it writes

All of the following are created mode `0600`:

- `/tmp/plxnative-events.log`, `/tmp/plxnative-stderr.log` and `/tmp/plxnative-crash.log` — the first two are truncated each launch, while the crash log is append-only so it survives a restart. Every line is scrubbed **before it is written**: tokens, header and query credentials, hostnames (including `plex.direct` names that encode your LAN address), bare addresses, Plex GUIDs, search queries, and your server and profile names are rewritten. Media titles, search terms, and subtitle text are never written at all. What remains includes ratingKeys — server-local item numbers used to diagnose playback bugs. Someone with access to the same server could map one back to an item, so still think before posting a log publicly. [`PRIVACY.md`](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md) is the full contract.
- Your signed-in session, as `<id>-auth.json` under `/media/developer` or `/media/internal` — one access token per server your account can reach. PlxNative capability-probes the documented `com.webos.service.keymanager3` service (TV 24+) and uses its AES-GCM operation when LS2 policy permits it. The older `com.palm.keymanager` AES-CFB service is deliberately not used because it cannot authenticate ciphertext. webOS TV 4.10.2 has neither usable service, so the compatible result there is an atomically replaced, app-owned mode-0600 file. A protected file is never silently downgraded during a temporary service failure. The probe is an ordinary application-service call and the fallback needs no root service or root-only HAL API; store entitlement for Key Manager is still capability-tested at runtime rather than assumed from the OS version.

The app does not persist the last screen: an authenticated cold launch starts on Home. Upgrades remove the retired `<id>-lastplace.json` bookmark written by older builds.

A crash writes no core file.

## What it reads outside its own directory

The television's codec table at `/etc/umediaserver/device_codec_capability_config.json`, and its firmware identity at `/var/run/nyx/os_info.json` and `/var/run/nyx/device_info.json`. All three are published by the platform, read once at boot, and never written.

## What it reaches

`plex.tv` and `discover.provider.plex.tv` over TLS, and the Plex Media Servers your account can reach — your own and any shared with you — over HTTPS whenever a token is present.

Stable builds refuse token-bearing plaintext HTTP; developer-trigger builds may enable it for a local lab and log that exception.

**Only if you switch them on**, the app also reaches Sentry and PostHog in the European Union. They have separate switches, both are off by default, and both are reversible. [`PRIVACY.md`](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md) describes them in full.

Nothing is sent anywhere else. A build carries an endpoint only if one was compiled into it, so `strings` on the binary answers the question directly, and each release audit reports what it found there.

## What listens

Nothing. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO, and the TCP capture listener that exist in a development build. Each audit measures this on the shipped bytes rather than asserting it.

# Scope

Movies and TV shows from a Plex Media Server your account can reach. No music, no photos, no live TV, no DVR.

There is deliberately nowhere on the television to type a server address — configure servers on a phone or PC, and the app offers what your Plex account already knows about.

# The bundled FFmpeg

The package contains three FFmpeg shared libraries — `libavformat-plx.so.63`, `libavcodec-plx.so.63`, and `libavutil-plx.so.61` — built from **FFmpeg 9.0**, unmodified, and licensed **LGPL-2.1-or-later**.

They contain demuxers, parsers, bitstream filters, and subtitle decoders only: video and audio are decoded by the television's own hardware.

The complete corresponding source accompanies every release, as LGPL-2.1 §6 requires and not as a courtesy: `ffmpeg-9.0.tar.xz` is the pristine upstream tarball with no patches applied, and `build-ffmpeg.sh` is the complete configure invocation that produced the libraries.

It is built with `--disable-everything` plus an explicit component list, and **without** `--enable-gpl`, `--enable-version3`, or `--enable-nonfree`, so no GPL or non-free FFmpeg component is present. Each audit quotes the configure string recorded inside `libavutil` itself, which is the primary evidence for that.

They are ordinary shared libraries, `dlopen`ed by absolute path out of the app's own directory under exactly those names, so they can neither shadow nor be shadowed by the television's own FFmpeg — and a build of your own with the same names replaces ours.

Full licence text travels inside the package, in `THIRD-PARTY-NOTICES.md` and `licenses/`.

The bundled build is configured with `--disable-network` and `file` as its only protocol, so it cannot open a URL at all; everything it demuxes arrives through the app's own transport.
