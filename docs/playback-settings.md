# Playback settings

Settings has **Video & playback** and **Audio & subtitles** pages. **Video & playback** holds
Video (quality, Direct Play, Dolby Vision, Playback engine), Player (skip interval, D-pad seeking, HDR overlay,
intro and credits skip, Player HUD) and Up next (next episode, Continue Watching). Subtitle size,
position and color live in the Look group of **Audio & subtitles**: this app's own settings, kept per profile
and never sent to the account. **Subtitle color** is White (the default, the look of every earlier
build), Yellow (a bright yellow for readability) or one of five grays that only dim the caption
for HDR; it is the same pick as the Color section of the player's Subtitles menu. A color
(yellow) is the default color of text subtitles (SubRip, WebVTT and the other text codecs), so a
cue's own inline colors still win (except an explicit white `<font color="#ffffff">`, which many SubRip files
wrap every cue in: that is the file's default ink spelled out, so the pick still applies); styled ASS/SSA scripts keep their own colors, except that text in a style whose color is pure white (the default ink) follows the pick (signs drawn in white too), and picture subtitles (PGS, VobSub) never take a color, only a gray dims them. This row has no subtitle on screen to judge, so it offers every color; the player's Subtitles menu, which knows the active track, shows the colors that track cannot take dimmed with the reason (the stored pick stays). The dark outline stays under every color. These settings are saved on this television, **per profile**: each
Plex Home profile keeps its own picks, and a profile that has not chosen yet starts from the value
the television held before profiles had their own. The page follows a profile switch (Settings itself closes on one). Each row states its own
explanation beside the table; the page's fallback sentence ("Playback settings for Maya.", naming
the seated profile) shows only while the Player HUD row, or another row with no sentence of its
own, has focus, or while nobody is seated. Settings is reached
through a profile, so a normal session always has one seated; where none is (a developer-token
run or a controlled replay; `Seat` in `plex/session.rs` lists the unseated states) the rows show
those starting values read-only, with the line "Choose a profile to change this", because
nothing can be saved for nobody.
Language preferences are saved to the active Plex profile and are shared with other Plex
clients.

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
restart the app and return to Auto. An enabled-state warning remains visible in Settings
(on the Video & playback page the warning follows the page's fallback sentence in the same
paragraph); while Force is on, Default quality reads "Overridden" and its sentence says the saved
quality is overridden. Returning to Auto or
Disabled needs no confirmation.

Audio passthrough remains the television's Digital Sound Output setting; if Atmos or E-AC3 audio
comes out slow or deep on a soundbar, see [the troubleshooting note](troubleshooting.md#audio-is-slow-deep-or-drifting-out-of-sync-on-a-soundbar).

## Playback engine

**Playback engine** (Video group) chooses which engine fetches and decodes direct-play videos, per
profile: **Standard** (the default) is this app's own player, which feeds the stream to the TV;
**Experimental** is an opt-in that hands the file's URL to the TV's own media pipeline (the URI engine,
`docs/uri-first-migration.md`). This app's own player screen and controls stay in both: only fetching
and decoding move. The opt-in is the viewer's own qualification of a URI source for that profile only:
nobody who has not turned it on is routed to URI, and the row is visible to everyone in every shipped
language.

With Experimental on, only a direct play of the original file can go to the TV's pipeline, and only
one Standard would itself direct-play on this television. Standard's own verdicts decide that
(`StandardDirectPlay`, carried on `OriginalRequest`): a video whose codec or size the set cannot show,
a Dolby Vision file the set and the Dolby Vision setting refuse (a Profile 5 file on a set without Dolby
Vision, with the setting on Auto), an audio track the set's own decoder cannot play (also one this
app would decode itself) and a container Standard has the server remux (anything but .mkv, .mp4 and .m4v)
stay in Standard, where the server converts what the TV cannot show or play. The TV's pipeline would play
such a file with wrong colours or no sound and report no failure, so the handoff to Standard would never
fire. Those are Standard's verdicts and no more: the server's own decision is not one of them, because the
URI offer is made before the server is asked, so a file that clears these checks but that the server would
still convert can go to the TV's pipeline. A video your Plex server converts (remux, transcode or HLS), any file
that carries a subtitle stream (whether or not a subtitle is switched on) or a burned or client-drawn
subtitle, a trailer preview, a non-1x speed, a relay link and Auto quality away from the LAN also still
play in Standard. On webOS 5+, originals with multiple audio tracks also stay in Standard
because the TV service can order tracks differently from Plex. Which sets reach the TV's pipeline at all
is decided per firmware major: webOS 4.x through ACB, and webOS 5.x through the TV's exported window (its
PRODUCT run is the 5.6.2 row of `docs/uri-mode.md`); a later major (6 and up) plays Standard until `EXPORTED_STRATA` admits it under the rule in `docs/uri-mode.md`. An Experimental failure takes at most one handoff per play: the video restarts in Standard
(`route::backend_policy`). On webOS 4.x the Experimental engine binds the screen through ACB and waits for each
release's completion callback before the next title; when that never arrives the plane stays
quarantined until the app restarts, the next title's Experimental bind is refused and that title takes the
handoff to Standard. When a play stays on Standard with Experimental on, the event log names the reason
in one line, `uri: original declined (<reason>)` (`docs/uri-mode.md`, "Why a play stayed in Standard"); that line
is local and nothing about this setting is sent to telemetry or diagnostics.

## Plex account preferences

The screen loads and saves the active profile using its own plex.tv credential. A save sends
only changed keys to `PUT https://clients.plex.tv/api/v2/user/profile`, with query parameters and
an empty body. Saving does not create a television-only language preference. Confirmed values
remain authoritative if a save fails; the screen provides Retry. Responses for an old profile
or an obsolete preference revision cannot replace newer settings.

Audio **Original** clears the preferred language while enabling automatic selection. Subtitle
mode offers manual selection, "when audio isn't in the subtitle language", and always; **No preference** clears only the
subtitle language. The "when audio isn't in the subtitle language" mode shows subtitles when the audio that will play is not in the subtitle language (`route/plan.rs` compares the two).
The Subtitles menu's second row, **Forced only**, is a third remembered state: the next episode shows its forced subtitle in the
language of the audio that plays (audio that names none uses the language it was chosen in), or
nothing (never a full track), and switching the audio switches
the forced track with it. It sits in the order below exactly where a remembered Off does, and it is
not remembered again by an OK on the row that already holds it.
Order: this playback's own pick, then (subtitles only) a subtitle file the
server has already selected, then what the profile remembered for the show (episodes only), then
existing PMS item selections, then explicit show preferences, then account defaults. The remembered
pick is written by the track menu: choosing an audio or subtitle track (or Off) while an episode
plays remembers it for that show under the seated profile, and the next episode of the show starts on
the same pick, matched by language, forced/SDH and the like rather than by stream id. A remembered
external subtitle (a sidecar or downloaded file) cannot be reused as that file: the next episode has
its own, so the pick means that episode's text sidecar of the same language and forced-ness, drawn by
the app wherever it can draw a sidecar and burned on a video re-encode. With no such sidecar the
episode's embedded track of that language and forced-ness is used if it has one; with neither, the
memory steps aside and the order continues (it never falls to Off). When a fresh
play of an episode applies a remembered pick on a direct play, the app also writes the applied tracks
to the server once, on the first decoded frame, so the server's selection for that viewer matches
the screen (a forced track that only the container's own track names reveal, found on that frame,
is the subtitle written, in that one write); a declined or fall-through pick, a retry and a
transcode write nothing, a Part with a
downloaded subtitle gets no subtitle write (a forced track found on that frame included), and a
pick the viewer makes first carries its own write (after an audio pick, the forced track found on that
frame is then written on its own, as any subtitle change during playback is, unless the Part carries a
downloaded subtitle; a subtitle pick has no track left to find and writes nothing more). A movie's pick, a pick the route refused (Force Direct Play), an automatic choice and an OK on the subtitle already
showing are not remembered. A remembered Off and a remembered Forced only beat the account's rule
and the server's embedded selection, and unlike a remembered track neither falls through to them
(Forced only with no forced track shows nothing). A subtitle file the server has selected for the
item, and that the app can draw, still shows first, whichever of the three was remembered. A selected
subtitle the app cannot draw (a PGS or VobSub file, say) does not outrank them.
Account defaults do not introduce subtitle burning when playback starts as a transcode. A remembered
pick is burned only where the video is being re-encoded anyway (a fixed quality, a relay, adaptive
streaming); on a copy remux or a direct play the app draws it if it can, and otherwise it is not
shown rather than forcing a re-encode (the audio enhancements' own burn rule is a separate matter); the resolve's single `track-memory:` line says `not shown (copy remux) subtitle N` for it (applied halves lead that line, so search for the substring), and the Subtitles menu says so with one line under its first group (shown only for that case).

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
this change does not advertise all DTS-HD streams as directly playable. PMS reports DTS as
`dca`; Auto and Force treat it as `dts`, and the profile sends `dca`. Force skips the channel
ceiling for DTS and, where the device table lists no DTS decoder (webOS 10+) or the install learned that
the set's decoder refuses a layout, decodes the DTS core of a track with one or two channels in the app instead
(plain stereo, no downmix); a set whose table lists DTS keeps decoding it itself. The app never mixes a source
of more than two channels down to stereo by default (owner decision, 2026-10-28), so Force refuses a DTS 5.1 track the set
cannot decode, with the same read-out as any unplayable track, instead of playing it in stereo. Auto never decodes
in the app on its own except as the very last rung, after the server refused to convert a direct play's audio and,
for a track of more than two channels, after the manual encode was refused too (`open recovery: decoding dts in
the app`); a surround track played that way is marked "played in stereo" in the audio menu. One exception, inside
a single request: once that rung has put DTS in force for a track of one or two channels, DTS stays decoded in the
app for the rest of that request, so a later pick in the audio menu of a surround DTS track switches without asking
the server first and plays as stereo, labelled the same way. The next request starts from the set's own decoder again.

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

## Intro and credits skipping

Requested in [discussion #826](https://github.com/GLinnik21/plx-native/discussions/826).

Playback settings offers **Skip intros** and **Skip credits** separately for a viewer whose
Plex account feature list grants the corresponding capability. These are per-profile choices:
**Manual** is the default, **Disabled** hides the skip button, and **Automatic** offers a
pair of buttons with a five-second countdown fill, and both timed prompts follow one rule: the
**filled** button is the action the countdown will perform, pressing it performs it now, and the
unfilled **Watch** button to its left cancels the countdown. For an intro that is **Watch Intro** +
**Skip Intro** (filled); for credits with a mid-item marker **Watch Credits** + **Skip Credits**; where the
existing player offers the next episode, **Watch Credits** + **Next Episode** (filled). Initial focus is the filled
button. Watch cancels the countdown, and for the pair leaves Next Episode available to press. BACK dismisses the
offer; choosing the Watch button of any Automatic skip prompt dismisses that offer too.

Only time with an actionable, presented Watch button and advancing playback counts. Pause,
buffering freeze the remaining opportunity. The pair retains its existing cancellation behavior:
moving away from Next Episode or opening a panel cancels its countdown. Moving the ring to the Watch button of
any Automatic skip prompt cancels its countdown; a panel covering it freezes its countdown. Seeking or leaving the
marker resets it. Intro and intermediate credits expiry seek to the marker end; final credits
use the existing playback-finished path, respecting the Next episode preference. Automatic final
credits use five presented seconds on the Up Next controls wherever that pair is available; the
other final-credits offers (no queued successor, or Next episode set to After credits or Off) are the
same Watch + filled Skip pair as an intro: no Automatic skip prompt has a standalone Watch control. Disabled and Manual retain
Next episode's independent policy, and synthetic end-of-item tails never auto-skip.

The app checks the **viewer’s** `intro-markers` and `credits-markers` grants, and `pass` in the same
viewer-credential list (the Plex Pass plex.tv reports for the viewer) grants both; a shared server
owner's subscription is not evidence of viewer eligibility. **One exception, by the owner's
decision (2026-11-11):** a seated Plex Home profile that is not the administrator (a managed
profile) has no Plex Pass of its own, so when its own list leaves either grant out, its Home
administrator's feature list is read as well and what it grants is added; the administrator's
`pass` therefore covers the household's profiles. Nothing else about the household's account is
used. A seat with no answer yet, or whose
latest landed answer lacks the grant, hides the new settings and uses the existing manual controls; a
stale answer, or a re-read that fails, keeps the last settled grants, so a row never drops away while the
viewer sits on it. Saved choices remain intact.
No Plex viewing-history write is needed to change these local settings.
