# Install FinNative on an LG webOS TV

FinNative is an unofficial, open-source Jellyfin app for LG webOS TVs, built on PlxNative. It is not in the LG Content Store, but you can install it on a regular LG webOS TV using LG's Developer Mode and a computer. **No root is required.** If something goes wrong, see [troubleshooting](troubleshooting-finnative.md).

## Before you start

You will need:

- an LG TV running **webOS 4.0 or newer**; check the [compatibility notes](../README.md#will-it-work-on-my-television), since only one model has played video with FinNative so far;
- a **Jellyfin server** and a user on it, with the server reachable from the TV;
- a Mac, Windows PC, or Linux computer for the setup described here;
- the TV and computer on the **same local network**.

The route is **set up the TV → connect from your computer → install FinNative**. No LG SDK or IDE is required. FinNative is not in the Homebrew Channel catalogue yet, so this guide installs it directly.

> **Important:** Developer Mode needs periodic renewal. If it expires and LG disables Developer Mode, apps installed through it are removed. [How to keep the apps installed](#important-developer-mode-expires).

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
   | **Address** | Your **TV's local IP address**, not your Jellyfin server's address. |
   | **Passphrase / authentication info** | The **Passphrase shown in the TV's Developer Mode app**, with the same capitalisation. This is **not your LG account password**. |

   Keep the Developer Mode defaults: **Username** `prisoner`, **Port** `9922`, and **Authentication** `Dev Mode`. The wizard sets these for you.
5. Finish adding the TV. In Dev Manager, open **Apps** and check that the **Installed** tab can load the TV's apps.

**Ready to continue:** Dev Manager can display the TV's installed apps. If it cannot connect, see [Connection problems](#connection-problems) before downloading FinNative.

## 3. Install FinNative

1. On your computer, open the [latest FinNative release](https://github.com/tiper/fin-native/releases/latest).
2. Under **Assets**, download **`io.github.tiper.finnative_X.Y.Z_arm.ipk`**. `X.Y.Z` is the release's version number. The `.ipk` is the TV app, not something to open on your computer; the manifest and source archives are not installers.
3. [Check the package against the release checksum](#verifying-the-package) before installing it.
4. In Dev Manager, select your TV, open **Apps**, and click **Install**. Choose the downloaded `.ipk`.
5. Wait for installation to finish and check that **FinNative** appears under **Installed**.

For future updates, repeat this with the newer `.ipk`. Install over the existing app rather than uninstalling it first, so you stay signed in.

FinNative installs beside PlxNative and never replaces it, so you can keep both.

## 4. Open FinNative and sign in

Open **FinNative** from the TV's app launcher. It looks for Jellyfin servers on your home network and shows a **Quick Connect** code: on your phone or computer, open Jellyfin, go to **Quick Connect** in your user settings and enter the code, or scan the QR code to open that page.

To sign in with a password instead, or if your server is not found, choose **Manual sign-in** and enter the server's address (for example `192.168.1.20:8096` or `https://jellyfin.example.com`), then your username and password. You can also use Quick Connect from that form.

If the server says Quick Connect is off, an administrator can turn it on in the Jellyfin dashboard under **General**, or you can use Manual sign-in.

**Installation is complete.** You can now use FinNative from the TV launcher. The remaining setup responsibility is keeping Developer Mode active, as described below.

## Important: Developer Mode expires

FinNative is installed through LG Developer Mode, so its session has to be kept alive:

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

**FinNative opens but finds no server:** check that the Jellyfin server is running and on the same network as the TV, or use **Manual sign-in** and type its address. FinNative does not include a media library or set up a Jellyfin server for you.

For more connection help, see [webOSbrew's troubleshooting guide](https://www.webosbrew.org/devmode/#troubleshooting). To [report a FinNative problem](https://github.com/tiper/fin-native/issues), include your TV model, webOS version, FinNative version, Jellyfin server version, and the exact error. Do not post passwords, the Passphrase, access tokens, or screenshots containing them. Please report FinNative problems there, not to the PlxNative project.

If FinNative itself is installed and opens but something inside it doesn't work — sign-in, playback, or anything else — see [Troubleshooting](troubleshooting-finnative.md).

---

## Checking what you downloaded

The installation walkthrough ends above. The sections below are a technical reference for package verification, release provenance, and what the app reads, writes, and connects to.

The app runs in LG's normal sandbox: an unprivileged uid, chrooted, under the stock jail profile. `appinfo.json` declares two ACGs, `database.operation` and `securitykey.operation` — both are the storage helper's, for the local DB8 keystore and the platform key manager it uses to keep your sign-in off plain disk; neither reaches the network or anything outside this app's own data.

Per-release facts — the hash, sizes, payload, and what was tested on which set — are in that version's [technical audit](https://github.com/tiper/fin-native/tree/main/docs/release-audits). FinNative's is v0.1.0; every other audit in that folder is PlxNative's, a different application.

### Which file to download

A release attaches several files. **To install, you need the first one.**

| File | What it is |
|---|---|
| `io.github.tiper.finnative_X.Y.Z_arm.ipk` | The app. |
| `io.github.tiper.finnative.manifest.json` | The Homebrew Channel manifest — how the Channel finds and verifies an update. |
| `ipk.sha256` | The checksum, for `sha256sum -c`. |
| `ffmpeg-<version>.tar.xz` | The pristine upstream FFmpeg source, published because we are obliged to. |
| `build-ffmpeg.sh` | The complete configure invocation that produced the bundled FFmpeg libraries. |
| the source bundle (`.tar.gz`) and `source-rebuild.json` | The app's complete corresponding source, and the record of CI rebuilding the package from it. |

### Verifying the package

Nothing in this distribution chain is code-signed, so the SHA-256 published with the release is what tells you that the file you downloaded is the file that was published there.

Download `ipk.sha256` from the same release as the `.ipk` and open a terminal in the download folder. Replace `X.Y.Z` with the version you downloaded. On macOS and Windows, compare the printed hash with the one in `ipk.sha256`; on Linux, the check below should report `OK`. **Do not install the package if the hashes do not match.**

```sh
shasum -a 256 io.github.tiper.finnative_X.Y.Z_arm.ipk              # macOS
sha256sum -c ipk.sha256                                            # Linux, with the checksum asset beside it
certutil -hashfile io.github.tiper.finnative_X.Y.Z_arm.ipk SHA256 # Windows
```

[Return to the installation](#3-install-finnative) after checking the file.

FinNative is not in the Homebrew Channel catalogue yet. Once it is, the Channel will fetch that release's `io.github.tiper.finnative.manifest.json`, hash the download on the television, and refuse to install a package that does not match. If you point Homebrew Channel at a bare `.ipk` yourself, that verification path is bypassed, so verify the package yourself.

#### Rebuilding it to compare

Two builds of one commit on one machine produce a byte-identical `.ipk`. It is **not** reproducible across machines yet — the bundled FFmpeg records the toolchain paths it was built against — so a hash from your own rebuild will differ, and that is not evidence of tampering. Each audit's **Reproducibility evidence** section shows exactly which paths a given package carries.

Every release is built and uploaded by GitHub Actions from the tag. If a release's assets were uploaded by a person rather than by `github-actions[bot]`, the build and verification gates did not run — the audit records the uploader for exactly this reason.

## What the app does on your television

This section is invariant across releases. Where a release changes one of these behaviours, its release note says so and its audit measures it.

### What it writes

Both entries below are restricted to the app, but not at the same mode. The three logs are mode `0640` — owner read/write, the app's own group read, nothing for anyone else — so that on a television that is not rooted, Dev Manager should be able to fetch them and you can attach them to a bug report. The signed-in session is mode `0600`, owner only.

- `plxnative-events.log`, `plxnative-stderr.log` and `plxnative-crash.log` (mode `0640`) in `/tmp/io.github.tiper.finnative/` — the first two are truncated each launch, while the crash log is append-only so it survives a restart of the app. Because the mode includes group read, another app installed on the same television that runs in FinNative's group can read these three files too; the event log is scrubbed as described next, and so is the panic text the app appends to the crash log, but the crash tracer's own records (a faulting address, registers, one memory-map line) and the stderr log (whatever aborts and the television's own libraries print) are not. Every event-log line is scrubbed **before it is written**: tokens, header and query credentials, hostnames, bare addresses, search queries, and your server and profile names are rewritten. Media titles, search terms, and subtitle text are never written at all. What remains can include item identifiers from your server, used to diagnose playback bugs. Someone with access to the same server could map one back to an item, so still think before posting a log publicly. [`PRIVACY.md`](../PRIVACY.md) is the full policy.
- Your signed-in session (mode `0600`), as `<id>-auth.json` under `/media/developer` or `/media/internal` — the access token your Jellyfin server issued and the server's address, never your password. The app capability-probes the documented `com.webos.service.keymanager3` service (TV 24+) and uses its AES-GCM operation when LS2 policy permits it. The older `com.palm.keymanager` AES-CFB service is deliberately not used because it cannot authenticate ciphertext. webOS TV 4.10.2 has neither usable service, so the compatible result there is an atomically replaced, app-owned mode-0600 file. A protected file is never silently downgraded during a temporary service failure. The probe is an ordinary application-service call and the fallback needs no root service or root-only HAL API; store entitlement for Key Manager is still capability-tested at runtime rather than assumed from the OS version.

The app does not persist the last screen: an authenticated cold launch starts on Home. Upgrades remove the retired `<id>-lastplace.json` bookmark written by older builds.

A crash writes no core file.

### What it reads outside its own directory

The television's codec table at `/etc/umediaserver/device_codec_capability_config.json`, and its firmware identity at `/var/run/nyx/os_info.json` and `/var/run/nyx/device_info.json`. All three are published by the platform, read once at boot, and never written.

### What it reaches

The Jellyfin server you sign in to, and nothing else. While you sign in, the app also sends a discovery broadcast on your home network to find Jellyfin servers there.

Stable builds send your password or token over plaintext HTTP only to a server at a numeric private address on your home network, such as `http://192.168.1.20:8096`. A server typed as a plain `http://` name, or at a public address, is refused with a message saying so; use its `https://` address instead. The permission is checked again whenever the app starts from a saved sign-in or comes back from the background, because the app cannot see the television change network. Settings → **Unencrypted connections** lists the server once it has been allowed and turns it off at once. Developer-trigger builds may enable plaintext for a local lab and log that exception.

FinNative sends no crash reports or analytics. The reporting code it inherits from PlxNative is compiled without any destination in builds made from this repository, so it has nowhere to send to.

Nothing is sent anywhere else. A build carries an endpoint only if one was compiled into it, so `strings` on the binary answers the question directly, and each release audit reports what it found there.

### What listens

Nothing. A release build compiles out the whole `/tmp` trigger surface, the remote-control FIFO, and the TCP capture listener that exist in a development build. Each audit measures this on the shipped bytes rather than asserting it.

## Scope

Movies and TV shows from the Jellyfin server you sign in to. No music, no photos, no live TV, no DVR, and playlists can't be opened yet.

## The bundled FFmpeg

The package contains four FFmpeg shared libraries — `libavformat-plx.so.63`, `libavcodec-plx.so.63`, `libavutil-plx.so.61`, and `libswresample-plx.so.7` — built from **FFmpeg 9.0**, unmodified, and licensed **LGPL-2.1-or-later**.

They contain demuxers, parsers, bitstream filters, subtitle decoders, audio decoders and the AC3 encoder of Settings > Playback > Convert audio: video is decoded by the television's own hardware, and so is audio except FLAC, which is decoded in the app and fed to the television as PCM, any audio while a playback speed other than 1x is chosen, which the app decodes and time-stretches, and the codecs the television cannot decode while Convert audio is "on the TV", which the app decodes and re-encodes to AC3.

The complete corresponding source accompanies every release, as LGPL-2.1 §6 requires and not as a courtesy: `ffmpeg-<version>.tar.xz` (the tarball `ci/dependencies.json` pins) is the pristine upstream tarball with no patches applied, and `build-ffmpeg.sh` is the complete configure invocation that produced the libraries.

It is built with `--disable-everything` plus an explicit component list, and **without** `--enable-gpl`, `--enable-version3`, or `--enable-nonfree`, so no GPL or non-free FFmpeg component is present. Each audit quotes the configure string recorded inside `libavutil` itself, which is the primary evidence for that.

They are ordinary shared libraries, `dlopen`ed by absolute path out of the app's own directory under exactly those names, so they can neither shadow nor be shadowed by the television's own FFmpeg — and a build of your own with the same names replaces ours.

Full licence text travels inside the package, in `THIRD-PARTY-NOTICES.md` and `licenses/`.

The bundled build is configured with `--disable-network` and `file` as its only protocol, so it cannot open a URL at all; everything it demuxes arrives through the app's own transport.
