# Native video blocked by the TV's sandbox

On some Realtek k5lp/k3lp televisions, PlxNative stops playback with
`jail_missing_rtkmem`. The app cannot read `/dev/rtkmem` inside its sandbox. It refuses to
start LG's native video pipeline because that condition has been associated with crashes.
Changing playback quality does not repair the sandbox.

For a TV without root access, see the [Developer Mode guide](non-root-video-sandbox.md).
It describes an app-local LG-signed configuration option whose affected-device result still
needs confirmation. The in-app Homebrew Channel repair below requires root.

## Repair from the failure screen

Choose **Repair sandbox**, then confirm the dialog explaining the use of root access.
PlxNative asks the installed Homebrew Channel service to run LG's jailer for this PlxNative
install. This needs a rooted TV with an elevated Homebrew Channel service. It does not root
the TV or install a privileged service.

If the app reports **Sandbox repaired**, close PlxNative completely and reopen it before
trying playback. The result means the repair command succeeded and this app can now read
the device node; it does not mean a video has been decoded yet. If the service is unavailable,
not elevated, or the repair fails, the failure screen says so. A timeout leaves the result
unknown because cancelling a service request does not necessarily stop its command.
PlxNative will not submit another repair in that process.

This operation uses the
[Homebrew Channel's documented local service](https://github.com/webosbrew/webos-homebrew-channel#luna-service).
It does not send a Plex token, media information, or account data to that service, and does
not download a configuration from the internet.

## Repair reported on the 43UM7400PLB

The [reporter of issue #74](https://github.com/GLinnik21/plx-native/issues/74#issuecomment-5634518876)
confirmed playback with PlxNative 0.6.3 after running this command in a **root shell on the TV**:

```sh
jailer -d -t native -p /media/developer/apps/usr/palm/applications/com.beb.plxnative -i com.beb.plxnative /bin/true
```

This path is the reporter's standard PlxNative install. Check your installed app's directory
before using it; it must match the app id passed with `-i`. Do not substitute another app's id.
The command asks LG's jailer to populate PlxNative's existing jail using its `native` profile.
The [attached jailer output](https://github.com/user-attachments/files/32104665/jailer.txt)
records creation of `/var/palm/jail/com.beb.plxnative/dev/rtkmem`.

Close PlxNative completely and reopen it after the repair. Returning to Home alone may leave
the process running. PlxNative caches its sandbox check at launch, so retrying in the old
process still uses the old result. A fresh event log should say
`devjail: soc=k5lp rtkmem=ok`; then test playback. A successful shell command alone does not
prove that the app can play.

The report covers this k5lp set on platform release 4.10.0. It does not establish success on
every Realtek television or after an app reinstall, firmware update, or full power cycle.
[The original Homebrew Channel change](https://github.com/webosbrew/webos-homebrew-channel/pull/202)
describes the created nodes as surviving reboot; that persistence has not been independently
tested by this project on the affected set.

## Why reinstalling Homebrew Channel does not fix it

Homebrew Channel briefly had an installer step that ran this jailer command as root.
[PR #211 reverted it](https://github.com/webosbrew/webos-homebrew-channel/pull/211) because it
did not solve other sandbox problems on some webOS 8 models. No tagged Homebrew Channel
release shipped that step. Reinstalling PlxNative through the Channel therefore does not
perform this repair.

The community also provides a [Kodi jailer-fix add-on](https://github.com/mariotaku/kodi.addon.webos-jailer-fix)
that downloads LG's signed configuration into **Kodi's own app directory** and asks for a full
power cycle. We have not established that running it for Kodi repairs PlxNative. It is not a
verified substitute for the PlxNative-specific command above.

If the [non-root Developer Mode procedure](non-root-video-sandbox.md) is unavailable or does
not restore playback, keep the failure screen's version, model, firmware, and failure code
when [reporting the problem](https://github.com/GLinnik21/plx-native/issues/74).
Do not disable the playback guard or create a device node by guessing its device numbers.
