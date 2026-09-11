# PR #84: Wayland handles and background presentation

## Scope and evidence

[PR #84](https://github.com/GLinnik21/plx-native/pull/84) proposes clearing borrowed Wayland
handles on background and reacquiring them on DID foreground.

The Sentry issue PLX-NATIVE-K contains one event from `plxnative@0.6.3` on webOS 4.10.0,
K5LP. Its exception has one frame, `wl_proxy_marshal`; the saved return address maps into
`libmali.so.1.0` at offset `0xee83d`, not the app. There are no lifecycle breadcrumbs or
caller frames establishing that `clear_opaque_region` caused it. The dev TV's Wayland
library has a different build ID, so its instructions cannot symbolize that fault offset.
The exact reported crash was **not reproduced**, and this change does not establish its cause.

Two defects can be demonstrated independently:

- A failed SDL window-info query retained the previous borrowed handles. A host regression
  failed against that behavior and passed after the query began revoking the previous borrow.
- The PR's pointer invalidation alone still allowed background rendering through SDL/EGL.
  During a real OS app switch, after the first partial heartbeat interval, the dev TV reported
  background `fps` values of `1, 41, 58, 7, 1, 0, 1, 0, 1`. Nulling our handles cannot guard
  the driver's own Wayland calls.

## Change

`WindowActivity` is owned by the app loop and gates the combined presentation demand, outside
idle/noidle, video-plane and queued-work decisions. WILL or DID background closes the gate;
WILL foreground leaves it closed; DID foreground reopens it. Event polling and playback
suspend/reload processing continue. Foreground explicitly invalidates the UI.

Background also clears our borrowed display/surface pointers. Window-info refresh rejects
failure, a foreign SDL backend, or incomplete handles; pointer reads are unaligned because the
oversized byte buffer does not promise pointer alignment. The opaque-region cache resets on
revocation. No TV-library imports or existing calling conventions were changed. The diagnostics
extension below adds a C wrapper over the already bundled Sentry SDK, keeping its value union
on the C side of the ABI.

## Verification on main

Based on `804b1174`. The baseline survived one UI and one playback OS switch. The pointer-only
version survived three playback switches and one UI switch, but failed the background-present
assertion above. The final version produced nine complete background heartbeat intervals with
`loop=59–60 fps=0`, then reacquired a non-null surface and resumed presentation. A separate
UI-only cycle passed the same zero-present assertion over nine complete intervals. The surface and
display addresses remained unchanged on this TV; surface destruction was not observed.

Playback used the configured managed test account. DISPLAY capture after foreground showed
hardware-decoded video and the UI together. The switch target was the existing stable app,
not the webOS 4 Home ribbon (which does not background this app). Only the debug install was
replaced. Screenshots and raw logs remain local because they contain household content.

Host regressions cover failed refresh, foreign/incomplete window info, repeated revocation,
replacement handles, opaque-cache reset, and presentation requests across repeated lifecycle
pairs including a standalone DID background. `make check`, the shipping-feature cargo check,
`make`, `make sim`, and `tools/fwcompat.py --min-release 4.4.2 pkg/plxnative` passed. The firmware
matrix establishes loader compatibility from 4.4.2 through 11.2.0, not runtime behavior on those
other sets. The host suite's native Linux comparison is skipped on macOS.

To repeat the device check, acquire the TV lock, use `tv-session.sh up --guest --screen
player=<fixture ratingKey>`, and launch the existing stable app through SAM while keeping the
luna subscription open. Hold it foreground for ten seconds. Require a new background event and
at least three complete heartbeats with a live loop and zero presents. Launch the debug app
again without closing it; require a fresh window-info log, resumed presentation, and a DISPLAY
capture showing video. Repeat after leaving playback. Hand back the interactive app and lock.

## v0.6 backport

Based on `0523129e` (the v0.6.5 maintenance branch). The older `app.rs` loop keeps its own
`WindowActivity`, and leaves decoded poster uploads queued while backgrounded. Playback and
UI-only OS switches each produced nine complete intervals at `loop=61 fps=0`, followed by
window reacquisition and resumed presentation. DISPLAY capture showed resumed video.
The current main TV driver and guest resolver were used against the v0.6 build because the
old branch's `--guest` option falls back to the owner. The installed stable app was only the
switch target; it was not replaced.

## Diagnostics, without claiming the original crash is fixed

The native SDK previously requested zero breadcrumbs and the importer discarded that field.
Native crash events now retain at most 16 closed window breadcrumbs: WILL/DID background and
foreground, WM query start/result, and the first frame's start/successful swap after boot or
foreground. The query reports only SDL version bytes and handle-presence booleans. Lifecycle
and frame steps may say whether playback was active. No pointer addresses, content, routes,
or free-text errors are added. Strictly formatted UTC step timestamps allow correlation with the
crash time. Ordinary frames produce no breadcrumb.

These are fields on the existing consent-gated crash report, not separate usage or error events.
`PRIVACY.md` and the native report preview disclose the fields. The v0.6 notice revision advances
without expanding the crash-report purpose or re-asking consent. The sanitizer reconstructs
only this schema, rejecting other breadcrumb categories, messages and invalid field types.

`ci/window-breadcrumb-probe.c` is an offline ARM crash-capture proof. It links the same
`libsentry.a`, `libunwind.a` and `src/sentry_context.c` as the app, uses a disposable database,
`transport=none`, and an `example.invalid` DSN. It emits 43 sparse steps, then deliberately
raises SIGSEGV in its own process. The deployed Sentry daemon recovered exactly the newest
16 steps; the last three were `did_foreground`, `wm_ready`, `first_frame`. No report was sent.
The app's actual envelope importer also retained all 16 while retaining the SDK's UTC step timestamps and stripping local paths:

```sh
PLX_WINDOW_PROBE_ENVELOPE=/path/to/probe.envelope CARGO_INCREMENTAL=0 \
  cargo +nightly test --manifest-path rust-modules/Cargo.toml --lib \
  telemetry::native::tests::device_window_breadcrumbs_survive_the_real_importer -- --ignored
```

This test is deliberately ignored in the ordinary host suite because its input must come from
the real native daemon. Default host tests cover schema filtering/caps and consent withdrawal
before capture. The native proof uses its own database and never reads or changes app consent.

The completed v0.6 backport passed `make check`, the shipping-feature check, ARM and simulator
builds, and the supported-firmware load matrix. One full hostsim run initially failed the
unchanged timeline-routing test; that test passed in isolation and the subsequent complete
`make check` passed. The final diagnostics build also passed a fresh real-TV background/resume
check and DISPLAY capture. The original Sentry issue remains unresolved rather than being
labelled fixed by these defensive changes.
