# Repair native video in Developer Mode without root

This procedure is for a TV that runs PlxNative through LG Developer Mode and reports blocked
access to `/dev/rtkmem`. It installs LG's signed jail configuration inside the
PlxNative application directory. It does not require root, Homebrew Channel, or a `jailer` command.

Firmware inspection supports this mechanism, and LG’s download contains the relevant device rule.
The complete procedure has not yet been tested on an affected, non-rooted TV, and Developer Mode file
permissions vary by firmware. Stop if webOS Dev Manager reports `permission denied`; do not try to
work around it with `chmod`, `chown`, or a root command.

## Before you start

Install PlxNative through Developer Mode, connect the TV in
[webOS Dev Manager](https://github.com/webosbrew/dev-manager-desktop), and keep the Developer Mode
session enabled and current. LG removes Developer Mode applications when that session expires or
Developer Mode is disabled; see LG's
[Developer Mode guide](https://webostv.developer.lge.com/develop/getting-started/developer-mode-app).

In Dev Manager, open **Info** and copy the value labeled **webOS version**. Dev Manager obtains this
from the TV's `webos_release`; it is not the firmware number shown in the TV settings. For example,
the affected reporter's **webOS version** was `4.10.0`, while Settings showed firmware `05.40.20`.
Dev Manager's implementation is visible in its
[device-info query](https://github.com/webosbrew/dev-manager-desktop/blob/67fd1b28b980aed672de5aa140660c304653ec40/src/app/core/services/device-manager.service.ts#L110-L130)
and [Info screen](https://github.com/webosbrew/dev-manager-desktop/blob/67fd1b28b980aed672de5aa140660c304653ec40/src/app/info/info.component.html#L18-L32).

Download the matching configuration and signature directly from LG. These two links are only for
the `4.10.0` example:

- [`jail_app.conf` for webOS 4.10.0](https://developer.lge.com/common/file/DownloadFile.dev?sdkVersion=4.10.0&fileType=conf)
- [`jail_app.conf.sig` for webOS 4.10.0](https://developer.lge.com/common/file/DownloadFile.dev?sdkVersion=4.10.0&fileType=sig)

For another webOS version, replace `4.10.0` in both URLs with the exact **webOS version** from Dev
Manager. Keep the filenames exactly `jail_app.conf` and `jail_app.conf.sig`. Do not edit either
file or convert its line endings: the signature covers the exact configuration bytes.

## Install the pair

1. Fully close PlxNative. If LG's CLI is already configured for a device named `myTV`, this
   supported command closes it:

   ```sh
   ares-launch --device myTV --close com.beb.plxnative
   ```

   Dev Manager does not automatically configure LG's CLI. If the command is not already set up,
   close PlxNative from the TV instead. Do not leave it running while changing the pair.

2. In Dev Manager, open **Files** and navigate to:

   ```text
   /media/developer/apps/usr/palm/applications/com.beb.plxnative
   ```

   A developer test IPK installed alongside the stable app instead uses:

   ```text
   /media/developer/apps/usr/palm/applications/com.beb.plxnative.debug
   ```

3. If `jail_app.conf` or `jail_app.conf.sig` already exists, select each one and click
   **Download** to save an original copy in a separate **backup folder** on the computer,
   retaining its exact filename. Keep the fresh LG downloads in a different folder. Record
   which files were absent. Do not delete an existing file unless its backup downloaded
   successfully. These copies are the rollback; Dev Manager has no Rename action.

4. Select the old `jail_app.conf`, if present, and click **Delete**. Then delete the old
   `jail_app.conf.sig`, if present. Delete only these two exact files.

5. Click **+**, choose **Upload**, and upload `jail_app.conf.sig` first. After that finishes,
   upload the matching `jail_app.conf` last. Keep PlxNative closed until both transfers complete.

Dev Manager's current Files UI exposes the
[Download, Delete, and Upload actions](https://github.com/webosbrew/dev-manager-desktop/blob/67fd1b28b980aed672de5aa140660c304653ec40/src/app/files/files.component.html#L59-L79).
Uploading the signature first minimizes the interval in which a new configuration could sit beside
an old signature; it is not a substitute for keeping a matching, unmodified pair.

If either delete or upload fails, stop before opening PlxNative. Delete any partially installed new
pair, then restore the originals by uploading the original signature first and original config
last. If both files were originally absent, leave both absent.

## Restart and verify

With Developer Mode still enabled, turn the TV fully off, unplug it for about 10 seconds, reconnect
power, and reopen PlxNative. This follows the restart instruction used by the community's
[Kodi jail-configuration repair](https://github.com/mariotaku/kodi.addon.webos-jailer-fix).
Do not disable and re-enable Developer Mode: that removes Developer Mode applications.

Try the same video that previously failed. Successful playback after reopening is the required
check. If you contact support, include a photo of the PlxNative support line. When Dev Manager can
read PlxNative's `/tmp` log, `devjail: ... rtkmem=ok` is useful supporting evidence, but the guide
does not require access to that log.

If PlxNative no longer launches, use Dev Manager **Files** to delete only `jail_app.conf` and
`jail_app.conf.sig`. Restore any originals, signature first and config last, then power-cycle and
reopen the app. A reinstall or data erase is not required for this rollback.

Check the pair again after every PlxNative update or reinstall, firmware update, or Developer Mode
expiry. Preservation across those events has not been verified. Rooted TVs can instead use the
[rooted-TV repair](native-video-sandbox.md), which avoids this manual Developer Mode workflow.

## Evidence and limits

The webOS **4.10.2** jailer we inspected looks for `jail_app.conf` and `jail_app.conf.sig` in
the application's own directory, verifies the pair against LG's app-signing certificate bundle,
and reads a valid app-local configuration. That binary came from the development TV, not the
reporter's affected 4.10.0 set. The current LG `4.10.0` download contains the conditional rule
that creates `/dev/rtkmem` for Realtek hardware. See the [firmware investigation](measurements/issue74-non-root-sandbox.md).

The downloaded `4.10.0` files were also checked as a detached pair with OpenSSL. That check proves
the two downloaded files match each other; it does not prove LG's trust chain or acceptance by a
particular TV. An invalid signature can make the launcher reject the app, which is why the backup,
upload order, and rollback steps above matter. The repository does not redistribute LG's files or
firmware code. The LG download pattern used here is also documented by the community's
[configuration-fetch script](https://gist.github.com/mariotaku/fab3ee34fae3415c8213d1a1639a6aff).
