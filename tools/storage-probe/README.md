# Storage probe — issue 76

This is a diagnostic, **not a PlxNative release or a sign-in fix**.
It installs as `com.beb.plxnative.storageprobe`, alongside PlxNative, with the
launcher title **Storage probe (test)**. It does not read Plex credentials,
contact Plex or Sentry, upload anything, or change system permissions.

## On the TV

1. Extract the downloaded Actions artifact ZIP and install its `.ipk` using
   webOS Dev Manager, like any other Developer Mode app. Do not uninstall PlxNative.
2. Open **Storage probe (test)** and photograph the results.
3. Press **BACK** to close the probe completely, then reopen it and photograph
   the results again. `PRIOR FOUND` means a previously written marker was read;
   `NEW` means none was present before this launch. Each launch also tries a new write.
4. Share both photos in issue 76, along with your webOS version. No SSH logs needed.

The probe accesses only three test directories inside its own install directory.
They are packaged with different Unix permissions: `0755 root:root`,
`0775 root:5000`, and `0777 root:root`. It displays the **actual installed**
owner, group and mode rather than assuming the installer preserves these.
The test marker is non-secret, created with mode `0600`. Existing unexpected
files and symlinks are refused rather than overwritten.

You can uninstall the probe afterwards; this removes its own test data, not
PlxNative's. Closing/reopening tests process-restart persistence, not power-loss
durability or preservation across an application update. Those need separate tests.
Successful writes do not prove isolation from other apps sharing the same jail.

## Reproduce the build

With the repository's webOS NDK available:

```sh
python3 tools/storage-probe/test_probe.py
python3 tools/storage-probe/build.py --out /tmp/storage-probe-build
python3 tools/fwcompat.py /tmp/storage-probe-build/storageprobe
```

The output directory must not already exist. No Rust/Plex application build is
performed. The package contains only this diagnostic, the existing Inter font
and its OFL license, a test icon, this document and the repository MIT license.
SDL2 and SDL2_ttf are dynamically linked from the TV, not bundled.

The host tests check file-handling safety and distinguish first/second launches.
They do not verify webOS installation, jail permissions, or the TV UI.
