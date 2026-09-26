# Installing PlxNative

PlxNative is not yet available in the LG Content Store. You can install it on a regular LG webOS TV using LG's Developer Mode and a computer. **No root is required.**

## Already have Homebrew Channel?

Open **Homebrew Channel** on your TV, find [**PlxNative**](https://repo.webosbrew.org/apps/com.beb.plxnative/), and select **Install**. Then [open PlxNative and sign in](#4-open-plxnative-and-sign-in).

You can skip the Developer Mode and computer setup below. If your existing Homebrew Channel installation uses Developer Mode, keep renewing that session as usual.

## Before you start

You will need:

- an LG TV running **webOS 4.0 or newer**; check the [compatibility notes](../README.md#will-it-work-on-my-television), since playback support varies by model;
- a Plex account with access to a **Plex Media Server**, either your own or one shared with you;
- a Mac, Windows PC, or Linux computer for the setup described here;
- the TV and computer on the **same local network**.

The route is **set up the TV → connect from your computer → install PlxNative**. No LG SDK or IDE is required.

> **Important:** Developer Mode needs periodic renewal. If it expires and LG disables Developer Mode, apps installed through it are removed. This applies to both installation options below, including Homebrew Channel installed through Developer Mode. [How to keep the apps installed](#important-developer-mode-expires).

## 1. Enable Developer Mode on the TV

1. **On your computer**, visit [LG's developer website](https://webostv.developer.lge.com/) and select **Sign In**. Sign in or create an account, completing any account verification LG requests.
2. **On your TV**, open **LG Content Store / Apps**, search for **Developer Mode**, and install it.
3. Open the **Developer Mode** app and sign in with that LG account.
4. Turn **Dev Mode Status** on. The TV will restart.
5. Open **Developer Mode** again, confirm **Dev Mode Status** is on, and turn **Key Server** on.
6. Keep this screen open. You will need the TV's **IP address** and the **Passphrase** displayed here to connect from your computer. The IP address is also available in the TV's network settings.

[LG's official guide](https://webostv.developer.lge.com/develop/getting-started/developer-mode-app) has screenshots of these controls. **Do not continue into its CLI or webOS Studio setup** for this installation; use Dev Manager in the next step instead.

**Ready to continue:** Developer Mode is on, Key Server is on, and you have the TV's IP address and Passphrase.

## 2. Connect your computer to the TV

**webOS Dev Manager is the app on your computer.** It connects to the Developer Mode app on your TV and installs packages for you.

1. [Download the latest webOS Dev Manager](https://github.com/webosbrew/dev-manager-desktop/releases/latest). In **Assets**, choose the installer for your computer: `.dmg` for macOS, `.msi` for Windows, or the appropriate Linux package. The project's [download table](https://github.com/webosbrew/dev-manager-desktop#download) explains the architecture choices. Do not download **Source code**.
2. Install and open Dev Manager. Start its setup wizard to add a TV and select **Use Developer Mode**. Do not select the Homebrew Channel SSH option; that is for an existing rooted setup.
3. The wizard also lists the TV preparation steps. Since you completed them above, mark them complete and continue to the connection form.
4. Fill in the connection details:

   | Field | What to enter |
   |---|---|
   | **Device Name** | A name you choose, such as `living-room-tv`. |
   | **Address** | Your **TV's local IP address**, not your Plex server's address. |
   | **Passphrase / authentication info** | The **Passphrase shown in the TV's Developer Mode app**, with the same capitalisation. This is **not your LG account password**. |

   Keep the Developer Mode defaults: **Username** `prisoner`, **Port** `9922`, and **Authentication** `Dev Mode`. The wizard sets these for you.
5. Finish adding the TV. In Dev Manager, open **Apps** and check that the **Installed** tab can load the TV's apps.

**Ready to continue:** Dev Manager can display the TV's installed apps. If it cannot connect, see [Connection problems](#connection-problems) before downloading PlxNative.

## 3. Choose how to install PlxNative

Both options install PlxNative; neither unlocks extra PlxNative features. The difference is how you install and manage updates.

| | Direct installation | Through Homebrew Channel |
|---|---|---|
| **What you add to the TV** | PlxNative only. | Homebrew Channel, then PlxNative. |
| **How you update** | Install a newer `.ipk` from your computer using Dev Manager. | Open Homebrew Channel on the TV and select **Update** when available. |
| **Why choose it** | Fewer steps and no extra catalogue app. | Browse other homebrew apps and install updates with the TV remote. |

**Both options use the Developer Mode setup above and need the same session renewal.** Installing Homebrew Channel does not remove that requirement. Its catalogue can also contain apps with their own requirements; not every homebrew app works on every TV.

### Option A — install PlxNative directly

**The shorter route if you only want PlxNative.**

1. On your computer, open the [latest PlxNative release](https://github.com/GLinnik21/plx-native/releases/latest).
2. Under **Assets**, download **`com.beb.plxnative_X.Y.Z_arm.ipk`**. `X.Y.Z` is the release's version number. The `.ipk` is the TV app, not something to open on your computer; the manifest and source archives are not installers.
3. For a manual download, [check the package against the release checksum](#verifying-the-package) before installing it.
4. In Dev Manager, select your TV, open **Apps**, and click **Install**. Choose the downloaded `.ipk`.
5. Wait for installation to finish and check that **PlxNative** appears under **Installed**.

Continue to [step 4](#4-open-plxnative-and-sign-in). You do not need to install Homebrew Channel as well.

For future updates, repeat this with the newer `.ipk`. Install over the existing app rather than uninstalling it first.

### Option B — install Homebrew Channel, then PlxNative

**Choose this for a catalogue and app updates on the TV.** Homebrew Channel is a separate TV app; you still use your computer for this initial setup.

1. In **Dev Manager on your computer**, select your TV and open **Apps → Available**. This is the webOS Homebrew catalogue.
2. Find **Homebrew Channel**, open its entry, and select **Install**.
3. Wait for installation to finish. **On your TV**, open **Homebrew Channel** from the app launcher.
4. Find **PlxNative**, open its entry, and select **Install**.
5. Wait for installation to finish, then continue to [step 4](#4-open-plxnative-and-sign-in).

For future updates, open PlxNative's entry in Homebrew Channel and select **Update** when offered. Updates are installed when you select them, not automatically.

## 4. Open PlxNative and sign in

Open **PlxNative** from the TV's app launcher. Scan the on-screen QR code and sign in to the Plex account that has access to your server or shared libraries. Choose your Plex Home profile if prompted.

**Installation is complete.** You can now use PlxNative from the TV launcher. The remaining setup responsibility is keeping Developer Mode active, as described below.

## Important: Developer Mode expires

For installations made through LG Developer Mode, including Homebrew Channel installed that way:

1. While the TV is online, open the **Developer Mode** app and check **Remain Session**.
2. **Before the remaining time runs out**, select **EXTEND**.
3. Check that the remaining time has increased. Repeat before the next expiry; a calendar reminder can help.

LG does not let you extend an already-expired session. It documents that restarting the TV after expiry disables Developer Mode and removes apps installed through it. Re-enable Developer Mode and reinstall the apps if this happens.

Do not turn off Developer Mode after installation. **Key Server is different:** it is needed when initially adding the TV to Dev Manager, not for everyday playback, and it may turn off after a restart.

See [LG's session guidance](https://webostv.developer.lge.com/develop/getting-started/developer-mode-app#extending-developer-mode-time) and [webOSbrew's Developer Mode notes](https://www.webosbrew.org/devmode/).

## Connection problems

**Dev Manager cannot reach the TV:** check that the TV is on, both devices are on the same local network, and you entered the TV's current IP address. Guest Wi-Fi, device isolation, or a VPN can prevent a local connection.

**Adding the TV fails at authentication:** reopen Developer Mode on the TV, enable **Key Server**, and copy the current **Passphrase** exactly. In Dev Manager, use **Use Developer Mode**, not the rooted-TV SSH option or password authentication.

**The apps disappeared:** check **Dev Mode Status** and **Remain Session**. If Developer Mode has been disabled, enable it again and reinstall the apps using the same route.

**PlxNative opens but your libraries are missing:** make sure you signed in to a Plex account with access to a server and that the server is reachable. PlxNative does not include a media library or set up Plex Media Server for you.

For more connection help, see [webOSbrew's troubleshooting guide](https://www.webosbrew.org/devmode/#troubleshooting). To [report a PlxNative problem](https://github.com/GLinnik21/plx-native/issues), include your TV model, webOS version, PlxNative version, installation method, and the exact error. Do not post passwords, Passphrase, Plex tokens, or screenshots containing them.

---

## Checking what you downloaded

The installation walkthrough ends above. The sections below are a technical reference for package verification, release provenance, and what the app reads, writes, and connects to.

The app runs in LG's normal sandbox: an unprivileged uid, chrooted, under the stock jail profile. `appinfo.json` declares two ACGs, `database.operation` and `securitykey.operation` — both are the storage helper's, for the local DB8 keystore and the platform key manager it uses to keep your sign-in off plain disk; neither reaches the network or anything outside this app's own data.

Per-release facts — the hash, sizes, payload, and what was tested on which set — are in that version's [technical audit](https://github.com/GLinnik21/plx-native/tree/main/docs/release-audits).

### Which file to download

A release attaches five files. **For a direct installation, you need the first one.**

| File | What it is |
|---|---|
| `com.beb.plxnative_X.Y.Z_arm.ipk` | The app. |
| `com.beb.plxnative.manifest.json` | The Homebrew Channel manifest — how the Channel finds and verifies the update. |
| `ipk.sha256` | The checksum, for `sha256sum -c`. |
| `ffmpeg-9.0.tar.xz` | The pristine upstream FFmpeg source, published because we are obliged to. |
| `build-ffmpeg.sh` | The complete configure invocation that produced the bundled FFmpeg libraries. |

### Verifying the package

Nothing in this distribution chain is code-signed, so the SHA-256 published with the release is what tells you that the file you downloaded is the file that was published there.

Download `ipk.sha256` from the same release as the `.ipk` and open a terminal in the download folder. Replace `X.Y.Z` with the version you downloaded. On macOS and Windows, compare the printed hash with the one in `ipk.sha256`; on Linux, the check below should report `OK`. **Do not install the package if the hashes do not match.**

```sh
shasum -a 256 com.beb.plxnative_X.Y.Z_arm.ipk              # macOS
sha256sum -c ipk.sha256                                    # Linux, with the checksum asset beside it
certutil -hashfile com.beb.plxnative_X.Y.Z_arm.ipk SHA256 # Windows
```

[Return to direct installation](#option-a--install-plxnative-directly) after checking the file.

**If Homebrew Channel installs PlxNative from its catalogue, you have nothing to verify manually.** It fetches that release's `com.beb.plxnative.manifest.json`, hashes the download on the television, and refuses to install a package that does not match.

If you point Homebrew Channel at a bare `.ipk` yourself instead of installing the catalogue entry, that catalogue verification path is bypassed, so verify the package yourself.

#### Rebuilding it to compare

Two builds of one commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible across machines yet — the bundled FFmpeg records the toolchain paths it was built against — so a hash from your own rebuild will differ, and that is not evidence of tampering. Each audit's **Reproducibility evidence** section shows exactly which paths a given package carries.

Every release is built and uploaded by GitHub Actions from the tag. If a release's assets were uploaded by a person rather than by `github-actions[bot]`, the build and verification gates did not run — the audit records the uploader for exactly this reason.

## What the app does on your television

This section is invariant across releases. Where a release changes one of these behaviours, its release note says so and its audit measures it.

### What it writes

All of the following are created mode `0600`:

- `/tmp/plxnative-events.log`, `/tmp/plxnative-stderr.log` and `/tmp/plxnative-crash.log` — the first two are truncated each launch, while the crash log is append-only so it survives a restart. Every line is scrubbed **before it is written**: tokens, header and query credentials, hostnames (including `plex.direct` names that encode your LAN address), bare addresses, Plex GUIDs, search queries, and your server and profile names are rewritten. Media titles, search terms, and subtitle text are never written at all. What remains includes ratingKeys — server-local item numbers used to diagnose playback bugs. Someone with access to the same server could map one back to an item, so still think before posting a log publicly. [`PRIVACY.md`](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md) is the full contract.
- Your signed-in session, as `<id>-auth.json` under `/media/developer` or `/media/internal` — one access token per server your account can reach. PlxNative capability-probes the documented `com.webos.service.keymanager3` service (TV 24+) and uses its AES-GCM operation when LS2 policy permits it. The older `com.palm.keymanager` AES-CFB service is deliberately not used because it cannot authenticate ciphertext. webOS TV 4.10.2 has neither usable service, so the compatible result there is an atomically replaced, app-owned mode-0600 file. A protected file is never silently downgraded during a temporary service failure. The probe is an ordinary application-service call and the fallback needs no root service or root-only HAL API; store entitlement for Key Manager is still capability-tested at runtime rather than assumed from the OS version.

The app does not persist the last screen: an authenticated cold launch starts on Home. Upgrades remove the retired `<id>-lastplace.json` bookmark written by older builds.

A crash writes no core file.

### What it reads outside its own directory

The television's codec table at `/etc/umediaserver/device_codec_capability_config.json`, and its firmware identity at `/var/run/nyx/os_info.json` and `/var/run/nyx/device_info.json`. All three are published by the platform, read once at boot, and never written.

### What it reaches

`plex.tv` and `discover.provider.plex.tv` over TLS, and the Plex Media Servers your account can reach — your own and any shared with you — over HTTPS whenever a token is present, except the home-network exception below.

Stable builds refuse token-bearing plaintext HTTP, with one exception that is yours to make: when a server answers only unencrypted on your home network — every secure route to it, Plex's relay included, failed — the app asks **Connect without encryption?**: on the sign-in screen, on the Home or library read-out that reports the failure, or when you switch the server on in Settings → **Unencrypted connections**. It is offered only for a numeric private address plex.tv lists as local and on the same network as the television, never for a remote address or the relay, and never for a server set to require secure connections. The answer is remembered per server for the Plex account that gave it, so another account signing in is asked again. The permission it grants is never stored: it holds only while each fresh check of that server still reaches the same address, and it ends when you sign in or out, switch profile, or bring the app back from the background — the app cannot see the television change network, so those are the points where it proves eligibility again. While it is in use the app keeps retrying HTTPS and switches back as soon as a secure route verifies, and Settings → **Unencrypted connections** turns it off at once. Developer-trigger builds may enable plaintext for a local lab and log that exception.

**Only if you switch them on**, the app also reaches Sentry and PostHog in the European Union. They have separate switches, both are off by default, and both are reversible. [`PRIVACY.md`](https://github.com/GLinnik21/plx-native/blob/main/PRIVACY.md) describes them in full.

Nothing is sent anywhere else. A build carries an endpoint only if one was compiled into it, so `strings` on the binary answers the question directly, and each release audit reports what it found there.

### What listens

Nothing. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO, and the TCP capture listener that exist in a development build. Each audit measures this on the shipped bytes rather than asserting it.

## Scope

Movies and TV shows from a Plex Media Server your account can reach. No music, no photos, no live TV, no DVR.

There is deliberately nowhere on the television to type a server address — configure servers on a phone or PC, and the app offers what your Plex account already knows about.

## The bundled FFmpeg

The package contains three FFmpeg shared libraries — `libavformat-plx.so.63`, `libavcodec-plx.so.63`, and `libavutil-plx.so.61` — built from **FFmpeg 9.0**, unmodified, and licensed **LGPL-2.1-or-later**.

They contain demuxers, parsers, bitstream filters, and subtitle decoders only: video and audio are decoded by the television's own hardware.

The complete corresponding source accompanies every release, as LGPL-2.1 §6 requires and not as a courtesy: `ffmpeg-9.0.tar.xz` is the pristine upstream tarball with no patches applied, and `build-ffmpeg.sh` is the complete configure invocation that produced the libraries.

It is built with `--disable-everything` plus an explicit component list, and **without** `--enable-gpl`, `--enable-version3`, or `--enable-nonfree`, so no GPL or non-free FFmpeg component is present. Each audit quotes the configure string recorded inside `libavutil` itself, which is the primary evidence for that.

They are ordinary shared libraries, `dlopen`ed by absolute path out of the app's own directory under exactly those names, so they can neither shadow nor be shadowed by the television's own FFmpeg — and a build of your own with the same names replaces ours.

Full licence text travels inside the package, in `THIRD-PARTY-NOTICES.md` and `licenses/`.

The bundled build is configured with `--disable-network` and `file` as its only protocol, so it cannot open a URL at all; everything it demuxes arrives through the app's own transport.
