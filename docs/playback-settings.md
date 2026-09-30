# Playback settings

Settings has **Video & playback** and **Audio & subtitles** pages. Quality and Direct Play
are saved on this television; language preferences are saved to the active Plex profile and
are shared with other Plex clients.

## Quality and Direct Play

Default quality uses the same ladder and saved preference as the player's More menu. Settings
changes apply to the next playback; the player menu retains its existing current-playback behavior.

Direct Play defaults to **Auto**, which uses device capabilities and normal routing.
**Disabled** excludes original-file playback while allowing a remux or transcode.
**Force Direct Play (advanced)** attempts original playback through supported engine paths,
overrides the saved quality and disables automatic remux/transcode fallback. It does not add
codec implementations or a TrueHD path. It cannot guarantee that the TV accepts a stream.

Entering Force requires an explicit acknowledgement, initially focused on Cancel. The warning
names missing sound, incorrect pictures, freezes, crashes and the possibility that the app
cannot recover gracefully. It instructs the user to enable Force only if they know how to
restart the app and return to Auto. An enabled-state warning remains visible in Settings;
the quality controls explain that the saved quality is overridden. Returning to Auto or
Disabled needs no confirmation.

Audio passthrough remains the television's Digital Sound Output setting.

## Plex account preferences

The screen loads and saves the active profile using its own plex.tv credential. A save sends
only changed keys to `PUT https://clients.plex.tv/api/v2/user/profile`, with query parameters and
an empty body. Saving does not create a television-only language preference. Confirmed values
remain authoritative if a save fails; the screen provides Retry. Responses for an old profile
or an obsolete preference revision cannot replace newer settings.

Audio **Original** clears the preferred language while enabling automatic selection. Subtitle
mode offers manual selection, "when audio isn't in the subtitle language", and always; **No preference** clears only the
subtitle language. Existing PMS item selections take precedence, followed by explicit show
preferences and then account defaults. That mode shows subtitles when the audio that will play is not in the subtitle language (`route/plan.rs` compares the two).
Account defaults do not introduce subtitle burning when playback starts as a transcode.

PMS can keep using an earlier account preference until its own copy updates. The UI explains
this delay, and the resolver does not guess that an old-language selection was automatic.

### Language catalog provenance

Both pickers bundle the complete account-picker catalog verified in **Plex Web 4.160.0**,
build **75ddd7b**: 190 entries with native display names and exact language codes. This data is
local; the application does not download JavaScript to populate the menu. The choices are
independent of which languages happen to occur in the current library or item.

Sources inspected:

- [Language catalog, module 12060](https://app.plex.tv/desktop/js/main-8792-79c4e04db2360a62e838-plex-4.160.0-75ddd7b.js).
- [Audio & Subtitle Settings picker](https://app.plex.tv/desktop/js/chunk-4030-108232e3535fafc4f3ac-plex-4.160.0-75ddd7b.js).

Plex sorts native names with `localeCompare`, excludes `xx` and `xn`, and restricts this picker
to codes of length two or five. Its broader catalog includes `es-419`, but that value is not
an offered account-picker option. Existing account values outside the offered list remain
visible and are preserved. Regional codes are not rewritten on save.

## Verification boundary

Mock profile writes cover development and UI verification. Real account preferences must not be
changed as part of these tests. Issue #223's live PMS preference-propagation measurement requires
a throwaway account shared to the dev server and remains a separate completion gate.

Host tests cannot establish DTS sound, synchronization, resource allocation or DTS-HD core
acceptance. Those require the synthetic DTS fixture and server-backed playback on the television.
A muted device run proves pipeline behavior, not audible output.

Auto keeps the measured DTS channel ceiling: DTS-HD sources reporting more channels than that
ceiling (including 7.1 on the six-channel dev set), or no usable channel count, still convert on
the server. Stripping an HD extension alone does not prove the remaining core fits the decoder;
this change does not advertise all DTS-HD streams as directly playable.

### Implementation verification — 2026-09-27

- `make check` passed: default and hostsim Rust suites, feature checks, native helpers and the
  Python harness. After the final initial-load focus correction, all ten composed Settings
  tests passed, including load/retry focus, save focus, and Force confirmation/cancellation.
- Shipping-feature check (`--no-default-features`, `CARGO_INCREMENTAL=0`), ARM cross-build,
  ELF assertions, and the supported firmware loader matrix passed. No new FFI was introduced.
- All three committed replay anchors were freshly recorded after the state-shape change and
  passed both Targets and Resolve: six runs, every difference counter zero. The alphabet adds
  only source-backed Direct Play enum values and fixed Settings copy.
- Simulator: full account language picker, initial focus, regional-code save to the mock,
  warning with Cancel selected, and persistent Force/quality-override presentation inspected.
- LG webOS 4.10.2 debug install: Force warning and persistent enabled-state copy captured and
  inspected. A server-backed H.264/AC3 case passed while Force was persisted, using the managed
  Guest test identity. Direct Play was then returned to Auto.
- Native H.264/DTS 5.1 synthetic playback passed all pipeline assertions, including declaration,
  video binding, clock progress, displayed frames and in-place seeking.
- DTS-HD MA 5.1 core playback passed the same assertions using a temporary interoperability
  fixture. The public source sample's truncated tail was removed before looping its valid first
  3.4 seconds; every one of the resulting 5,532 audio packets contained a complete core. The
  malformed packet guard remains intact. The sample media is not distributed in this repository.

The television remained muted with its panel off. These results do not claim audible output
verification, and no real Plex account language preferences were written. The throwaway-account
PMS propagation measurement is still outstanding.
