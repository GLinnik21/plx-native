# Issue 76: application-local state fallback

Implementation verified locally and on the development TV; no release implied.

## Why this location

The [reporter's two native-probe photographs](https://github.com/GLinnik21/plx-native/issues/76#issuecomment-5636176129)
on webOS TV Lite 11.2.0 show `0755 root:root` denying create with errno 13,
while `0775 root:5000` permits write, file fsync, close, rename and directory
fsync. A new process reads the prior mode-0600 marker. The numeric uid is
6586, gid 5000; the two process IDs differ. `0777` also worked but is unnecessary.

The actual probe package and source are in
[Actions run 34609012631](https://github.com/GLinnik21/plx-native/actions/runs/34609012631),
commit `b2ed2649be13cd10b6cbf541c33cb1b2508d3155`.

## Upgrade check on the development television

Before choosing the implementation, the installed probe was upgraded through
`com.webos.appInstallService/dev/install`, from 1.0.0 to 1.0.1. The service
reported `update:true` and final `state:installed`. App-created marker files
under both writable test directories survived with their prior timestamp,
uid 5956 / gid 5000 and mode 0600. The upgraded process (PID 25598) displayed
`PRIOR FOUND` and successful replacement steps. Its screenshot was inspected:
`/tmp/plx-storage-probe-after-upgrade-tv.png`.

The locally built upgrade probe's IPK SHA-256 was
`71b642133b1b5c9d2f0409fb3c78b1537b634dba9e497916097e93b0c007f32d`.
It was only an isolated diagnostic upgrade; the user's stable app was not
replaced. The probe was closed, the stable app foregrounded, and the TV lock
released afterwards. This checks a normal update on the older development
firmware, not uninstall/reinstall, power loss or upgrade on the reporter's TV.

## Implementation contract

- Synthesize an empty per-install `state/` in the IPK, uid 0 / gid 5000 /
  mode 0775; reject runtime contents and symlinks during packaging.
- Keep external session/consent paths first, then `state/auth.json` and
  `state/telemetry.json`, then the older app-root fallback. No forced migration
  of an existing external session.
- Keep individual files app-owned and 0600, the existing secure-envelope
  policy, and the unknown-format refusal. Sign-out removes candidate files,
  not the packaged directory.
- Do not report a lower-candidate write as persistent when an older readable
  higher-priority file could not be removed and would shadow it on restart.
  The regression was observed failing before the guard was implemented.
- Shared-group namespace replacement/replay remains a limitation. These
  permissions are not proof of isolation from other Developer Mode apps.

## Implementation verification

- `make check`: lint and the full host suites passed (2504 default-feature,
  2533 hostsim tests), including state fallback, legacy migration, sign-out,
  unknown-envelope preservation and both session/consent shadow regressions.
- Seven packaging tests passed, including all flavours, deterministic state
  metadata, symlink/nonempty-state rejection, and wrong archive mode/group.
- Shipping feature set: `CARGO_INCREMENTAL=0 cargo +nightly check --lib
  --no-default-features` passed. Host dev/test debug information was disabled
  to avoid exhausting disk; this did not change feature selection or tests.
- `make FLAVOR=debug RELEASE=1 ipk` passed the ARM build and all actual-IPK
  assertions. Loader-symbol inventories passed for supported firmware entries
  from 4.4.2 through 11.2.0, not for unsupported earlier entries.

The actual application package was then installed as the **debug** app, with
shipping features (`features=release`). Before testing, its previous installed
tree, runtime files and external state were saved to a private TV backup.
Only the debug app's external auth/consent/spool filenames were temporarily
occupied by directories to force the old-path failure. The firmware's internal
mount was not changed, nor were system or application-directory permissions.

First boot (PID 26180) created `state/auth.json` as uid 6085 / gid 5000 / 0600
and reported `persisted_plaintext`. A new process (PID 26332) selected
`app_dir:plaintext`; the entire initial client-identity file was unchanged.

Next, the existing private debug session and consent were copied into the
state directory as test fixtures, retaining their ownership/mode. This was
**not** a fresh QR authorization. The real app read account/PMS/server/local
eligibility as all true, restored its two-server roster, and performed normal
session saves into state. Credential-field fingerprints before/after that
real save matched. Another process (PID 26655), with no further fixture copy,
again selected state and restored the same eligibility and roster.

All test-created state and obstructions were moved into the private backup;
the original debug snapshot was restored and `tar --compare` passed. The
stable app was foregrounded, not reinstalled, and the TV lock released.
A private backup containing credentials was retained on the development TV;
its path is recorded in the task handoff, not here. No user data was deleted.

## Field confirmation

The full application from commit `190c88aff87be2706e66a486aa75e3c8a22993b5`
passed [CI run 34616399003](https://github.com/GLinnik21/plx-native/actions/runs/34616399003).
The downloaded debug IPK was checked for its distinct app ID and empty
`state/` directory with uid 0 / gid 5000 / mode 0775. Its SHA-256 is
`47adeda1d687ffdb9c0953554eaef256edf907d9059e23eb63ca1b6089e4cdd4`.

In response to the request to sign into that build, close it and reopen it,
the original Lite 11.2 reporter [confirmed that it works](https://github.com/GLinnik21/plx-native/issues/76#issuecomment-5637356525).
This is field confirmation of the requested sign-in/restart test, in addition
to the earlier instrumented filesystem photographs. It is a user report,
not an additional device trace collected by us.

Application upgrade on the reporter's TV, power-loss durability and cross-app
isolation were not established by that reply. No release is authorized or
implied by this document.
