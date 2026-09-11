# Issue #74: confirmed sandbox repair through Homebrew Channel

Measured 2026-09-11. Functional code: `e0893a72`, following backend `4ca54ea7`, on
`fix/issue74-sandbox-repair`. Later cleanup updates documentation and removes obsolete
dead-code allowances without changing the repair behavior.

## Failure and repair evidence

The [reporter’s latest comment](https://github.com/GLinnik21/plx-native/issues/74#issuecomment-5634518876)
includes the PlxNative 0.6.3 failure log and confirms that playback worked after a root-shell
`jailer -d -t native -p <PlxNative install directory> -i com.beb.plxnative /bin/true`.
The [event log](https://github.com/user-attachments/files/32090410/plxnative-events.log)
reports `devjail: soc=k5lp rtkmem=missing` at line 5 and refuses native startup at line 1471.
The [jailer transcript](https://github.com/user-attachments/files/32104665/jailer.txt)
records successful creation of the app jail’s `dev/rtkmem` character device at line 185.
The transcript establishes node creation; playback success is the reporter’s separate statement.
These are the baseline failure artifact and affected-device repair evidence, not a newly run
failing unit test.

[Homebrew Channel PR #202](https://github.com/webosbrew/webos-homebrew-channel/pull/202/files)
implemented the same command without the diagnostic `-d` flag in its root installer service.
[PR #211](https://github.com/webosbrew/webos-homebrew-channel/pull/211) reverted it because it
did not solve other sandbox problems on some webOS 8 sets. That does not contradict this k5lp
report, but it prevents claiming a universal sandbox fix.

## Implementation boundary

The failure screen offers a neutral, Cancel-default confirmation. Only confirmation submits
one fixed command to the existing Homebrew Channel `exec` service. The backend validates this
install’s id and exact path, requires an elevated service and a real host character device,
and checks both the command result and fresh app-side readability before reporting repair.
No repair runs at startup or on an ordinary playback attempt. Every accepted attempt is terminal
for that process, including timeout; a cancelled LS2 subscription does not prove the root
command stopped. The existing boot-time playback guard stays intact until relaunch.

No new native-library symbols, FFI signatures, or dependencies were introduced. The operation
reuses the existing LS2 client. Manual compatibility review covered these unchanged calls,
the feature gates, command/path construction, and callback lifetime inherited from that client.
An independent source review found no concrete backend security or correctness defects.

## Host and simulator checks

The parent reran `make check`: 2488 default Rust tests and 2517 hostsim Rust tests passed,
with no failures or ignored tests; the remaining repository gates passed too. The shipping
`--no-default-features` library check passed. The macOS simulator and ARM debug build passed. After the documentation cleanup, the
shipping library check passed again and `make FLAVOR=debug ipk` passed all packaging assertions.
The local test package is `com.beb.plxnative.debug_0.6.4_arm.ipk`, SHA-256
`f7383a91d773c0e0542fbcdaf952b69d8eeb5dedfa508060eb64b48701833b6b`. Its rebuilt ELF
build id is `a93dcd8748b724472d78fbe1a959b7910ec307c9`; the native checks below used the
pre-cleanup build. This is a development package, not a published release.

Simulator captures were opened at 1920×1080 for: idle, confirmation with Cancel selected,
cancellation back to the original failure, Repair selected, unsupported-device refusal,
and the display-only Running, Repaired, unavailable-service, non-root-service, and timeout
fixtures. The copy and controls fit. Confirming on the Mac reached Unsupported; the hardware
guard was not bypassed. Display fixtures cannot enable a backend operation.

## Native checks

The development TV is `m16p3`, platform release 4.10.2, and is **not** an affected Realtek set.
The test used only the debug install and a synthetic localhost URL without a Plex item or
watch-history descriptor. Audio was muted; the test did not change the backlight setting.
The stable install was not deployed, restarted, or used for playback.

The deployed debug binary was hash-verified by `tv-session.sh`; its ELF build id was
`2f70ee9c97e116c8a20fcf034609b88ece670077`. With no probe trigger, no repair/probe log appeared.
With the development-only fixed `id -u` probe armed, the app reported:

```text
devjail: soc=m16p3 rtkmem=n/a
jail-repair-probe: root
```

This proves the real jailed PlxNative process can reach an elevated Homebrew Channel service
on this TV. It does not execute the repair command. `/usr/bin/jailer` was separately confirmed
present and root-only on this firmware.

The following DISPLAY captures were opened and checked:

- [Failure and repair action](issue74-sandbox-repair/idle.png).
- [Confirmation opened by pointer, Cancel selected](issue74-sandbox-repair/confirm.png).
- [Unsupported-device refusal after pointer cancel, then D-pad confirmation](issue74-sandbox-repair/unsupported.png).
- [Ordinary failure still opens the quality picker](issue74-sandbox-repair/ordinary-quality.png).
- [Changing to the sandbox failure hides the stale quality picker](issue74-sandbox-repair/stale-more-hidden.png).

BACK returned from the last state to Home. The session driver cleared the test triggers and
relaunched debug interactively; the TV lock was released.

## Remaining evidence needed

The new **in-app** operation has not yet been run on an affected k5lp/k3lp set. It still needs
that owner to confirm the command succeeds through their Homebrew installation and playback
works after fully closing and reopening PlxNative. Reboot, reinstall, firmware-update, and
cryptofs-install persistence have not been measured here. These host, simulator, and m16p3
checks must not be presented as proof of those cases.
