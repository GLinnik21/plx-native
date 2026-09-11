# Issue #74: evidence for a non-root Developer Mode option

Investigated 2026-09-11. User instructions are in the
[non-root Developer Mode guide](../non-root-video-sandbox.md).

## What the firmware establishes

The inspected `/usr/bin/jailer` came from the development TV on webOS 4.10.2 (`m16p3`),
not the affected reporter's webOS 4.10.0 k5lp set. The harvested executable's SHA-256 is
`5648a718c6f0a0e6db6ae77c5a9c0198f87b77a6aaf6ae83ddb867c32ddc3ae4`. No firmware executable or decompiled code is redistributed here.

Offline Ghidra analysis established this path:

- `LunaJail::CJail::setup` at `0x22f94` calls `verifyAndRereadConf` after reading the base
  configuration, when not in passthrough mode.
- `verifyAndRereadConf` at `0x1e6b0` looks for `jail_app.conf` and `jail_app.conf.sig` under
  the selected application directory. An absent config or signature leaves the base profile.
- With both present, it calls `verify_sig` with `/etc/ssl/certs/appsigning-bundle.crt`.
  Invalid signatures throw a jailer exception; a valid pair replaces the selected configuration
  and is read before jail setup continues.
- `verify_sig` at `0x24820` loads the certificate store and the PEM PKCS7 signature, then
  calls `PKCS7_verify`. This is signed configuration selection by the privileged launcher;
  an ordinary application does not need permission to execute the root-only jailer binary.

This proves support for the mechanism on the inspected firmware. It does not prove that every
TV accepts the currently downloaded pair or lets its Developer Mode SSH user write the app folder.

## LG download checked

Both files were downloaded directly over HTTPS from LG with `sdkVersion=4.10.0`:

| File | Bytes | SHA-256 observed on 2026-09-11 |
| --- | ---: | --- |
| `jail_app.conf` | 12711 | `7a3711f4df5afaaef554ba99758c1f425cd0accdb62d4fcdceb8a1c0baa7f331` |
| `jail_app.conf.sig` | 1631 | `6b695c0e7791cae3a9d7e06feaf8b1f3feb25e311953501441ebaa70fc1cee49` |

The configuration's `rtk == true` block specifies creation of `/dev/rtkmem`, ownership
`0:5000`, and mode `0660`. That is directly relevant to the reported missing/unreadable device.
These are observations of LG's file, not instructions to create a node or change permissions
by hand. Future downloads may differ; these hashes are evidence of this investigation, not a
permanent pin for every firmware.

A detached OpenSSL verification of these two files succeeded; a copy with one changed byte was
rejected. The check used `-noverify`, so this
establishes signature/content consistency **only**. It did not validate LG's trust chain against
a target TV's certificate store, and is not a substitute for the firmware's verification.

## Non-root transfer route

Current [webOS Dev Manager](https://github.com/webosbrew/dev-manager-desktop) source establishes:

- Developer Mode connections use the `prisoner` account; Files transfers use that SSH session.
- Info's **webOS version** comes from `webos_release`, falling back to `sdkVersion` if the
  former query fails. This differs from the separately displayed firmware version.
- Files provides **Download**, **Delete**, and **+ → Upload**. It does not bypass permissions.
  There is no Files Rename command or Apps Close button.

The guide links the exact source locations and LG's supported optional `ares-launch --close`
command. It does not use `ares-push` or `ares-shell`, which the current unified CLI marks as
unsupported for the TV profile. Copying into PlxNative's own app directory matters: the
[Kodi add-on](https://github.com/mariotaku/kodi.addon.webos-jailer-fix/blob/main/kodi.addon.webos-jailer-fix/script.py)
writes its pair under Kodi's HOME, so merely running that add-on does not establish a fix for
PlxNative's separate jail.

The guide backs up originals to a separate computer folder, removes an existing config before
its signature, uploads the new signature before the config, and keeps the app closed throughout.
An invalid signature can prevent app launch, so recovery uses Dev Manager's independent Files
connection to remove the pair or restore the originals. A full power cycle follows the Kodi
author's instruction; no claim is made that it is necessary on every firmware.

## What still needs a TV test

No file was installed or changed on a TV during this investigation. On an affected non-rooted
set, verify upload permissions, normal launch with the signed pair, app-side `rtkmem=ok`
where its log is accessible, and actual playback. Reboot, app-update, reinstall, and firmware-update
persistence are unmeasured. Developer Mode expiry/removal deletes the developer-installed app.

## Candidate product follow-up

An app-side download of the same official, signed pair could make this repair available without
Homebrew Channel. Before implementing it, prove app-directory writability under the real app UID
and acceptance of the pair on affected firmware. The implementation would need bounded HTTPS
requests without Plex credentials, firmware-specific selection, validation against the TV's
trusted signer before activation, preserved originals and a recoverable two-file update, and
clear full-restart instructions. It must target this app only, never replace the shared
`/media/developer/jail_app.conf` or bypass signature checks. The root-based repair remains a
separate, explicitly confirmed option; neither route should silently run at startup.
