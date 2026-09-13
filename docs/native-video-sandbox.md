# Native video sandbox and Repair

On a machine name starting with `k5lp` or `k3lp`, an unreadable `/dev/rtkmem` blocks native
playback before an Engine is installed. The failure code is `jail_missing_rtkmem`. This is a
community finding carried from final 0.6.6, not a diagnosis of every native playback failure.
The cached device fact lasts until process exit; leaving playback clears only that playback's
refusal.

The failure screen offers **OK to review sandbox repair**. The confirmation starts on Cancel
and explains that Homebrew Channel root access will update this app's sandbox through LG's
native jailer profile. Only the explicit Repair answer submits the fixed command. Boot and
ordinary Play never submit it.

One accepted attempt is allowed for the entire process, including worker failure and timeout.
Leaving or recreating the player page does not reset it. A timeout means the remote outcome is
unknown: cancelling the LS2 wait cannot promise that the root command stopped.

Success requires the exact result marker and a fresh readability check from this app's jail.
Close PlxNative completely and reopen it before playing. The cached preflight is deliberately
not cleared by success. The repair does not root a TV, install a service, download a file, or
send Plex credentials to Homebrew Channel.

Source evidence: v0.6.6 (`48866094e73493eb57a045723cdb6e195d9f4361`),
[issue #74](https://github.com/GLinnik21/plx-native/issues/74#issuecomment-5634518876), and
[Homebrew Channel PR #202](https://github.com/webosbrew/webos-homebrew-channel/pull/202).
The source records one reporter's k5lp 43UM7400PLB playback confirmation after a root-shell
jailer repair. This port has host evidence only. It does not newly establish Repair execution,
node persistence, or playback on an affected chassis. A healthy older TV cannot prove those
claims. Native playback, final dialog pixels, and Repair presentation still need the integration
candidate's simulator/device acceptance.
