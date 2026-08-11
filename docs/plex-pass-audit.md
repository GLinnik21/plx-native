# The Plex Pass dependency audit — what breaks on a free server, and what was done

**2026-08-11.** Issue #22's root cause was a Plex Pass server feature (HEVC encoding) silently
assumed by the client, invisible in development because the dev server has a Pass. This audit
walks *every* Pass-gated feature against every place the app touches it, so that class of bug is
enumerated rather than rediscovered one reviewer at a time.

Method: the Pass feature list from Plex's own support articles ([Plex Pass
overview](https://support.plex.tv/articles/201751006-plex-pass-feature-overview/),
[Transcoder](https://support.plex.tv/articles/transcoder/),
[Hardware-accelerated streaming](https://support.plex.tv/articles/115002178853-using-hardware-accelerated-streaming/),
[Credits detection](https://support.plex.tv/articles/credits-detection/)); a grep of every
touchpoint in this codebase; live decisions against a real PMS; and device tests for the two
findings that mattered. The one thing this audit cannot do is run against a genuinely
subscription-free server — the dev server's Pass cannot be suspended — so the free-server column
is derived from PMS's documented behavior plus the reviewer's logs in issue #22, and the reviewer
is the final verification for the fixes.

## The table

| Pass feature (server side) | App touchpoint | On a free server | Verdict |
|---|---|---|---|
| **HEVC encoding** | the transcode target in `plex/transcoder.rs` | was: no usable target → PMS **drops the video track**, audio-only stream, "playback failed" for every transcoded item | **FIXED** `d3d1d122` — target is a fallback chain `hevc,h264` / `ac3,eac3,aac`; h264 encoding is free everywhere |
| *(amplified by)* mkv-only direct play | `route.rs` sent every mp4 to the transcoder | no mp4 played at all | **FIXED** `2754a349` — mp4/m4v direct-play; free servers mostly never transcode now |
| **Credits detection** | Up Next tile + auto-advance armed *only* off a credits marker | tile could never appear; every episode ended by dropping to the detail page — no binge chain | **FIXED** `e9a4b9dd` — the last 30 s stand in as the credits segment when the server never said where they are; a real marker still outranks it |
| **Intro detection** | Skip Intro pill | no marker → no pill; nothing dangles | graceful, nothing to do |
| **Hardware transcoding** | transcode throughput | software transcode: a weak free server re-encoding 4K will buffer | documented; nothing client-side to do — direct-play-first already minimizes exposure (after the mp4 fix, only genuinely undecodable sources transcode) |
| **HDR tone mapping** | HDR source that must re-encode on a free server | h264 8-bit without tone mapping → washed-out colors | documented; **cannot be fixed client-side.** Narrow in practice: this panel decodes HEVC/HDR natively, so only undecodable HDR (e.g. AV1 on this SoC) hits it |
| **Plex Home (managed users)** | boot who's-watching picker | free account = roster of 1 → `app.rs` skips the picker (`home_users.len() > 1`) | graceful by construction |
| **Video preview thumbnails (BIF)** | chapter card thumbnails (`chapters_panel.rs`) | `thumb` empty → placeholder card; chapters themselves (embedded) still work | graceful |
| **Trailers / extras** | `Extras` appears in an include-list string (`plex/library.rs`) and is never rendered | server returns none; nothing requests or draws them | no-op |
| Live TV / DVR, music (sonic analysis, lyrics), photos, downloads/sync | not in scope of this app | — | n/a |

## The pattern this audit exists to kill

All three real findings are one bug wearing three coats: **a claim true on the development
environment, asserted as universal.** Plex Pass on the dev server (HEVC target, Up Next), an
old measurement never re-taken (mkv-only gate). The standing mitigations:

- the transcoder test pins the fallback chains so no list quietly becomes single-entry again;
- the reviewer's environment (free server, webOS 6.5.2) is now part of the verification loop;
- the diagnostics panel names server-side failures in words ("server sent audio only — it found
  no usable video transcode target") instead of "playback failed".

The last member of the class was the *decode capability* assertion — the app told every server
it could handle HEVC/4K/10-bit because the author's television can. The device publishes its own
decoder table (`/etc/umediaserver/device_codec_capability_config.json` — verified present on
webOS 4.5, listing per-codec max resolution/framerate/bitrate); the profile now derives from it
(`rust-modules/src/devcaps.rs`, read once at boot): HEVC on/off and its resolution ceiling, the
direct-play audio list, and `route.rs`'s local decode gate all read one caps snapshot, with the
49SM9000PLA constants kept only as the loudly-logged fallback for an unreadable table. The one
claim the table cannot express is bit depth, so `bitDepth=10` remains a commented constant — and
`getSystemInfo`'s `UHD` flag / the config service's HDR keys stay unused until a panel that
disagrees with its codec table is actually observed.

## What only the reviewer can confirm

Playing an H.264+AAC MP4 on a subscription-free server should now log
`stream: … /library/parts/…` (direct play, no transcoder involved), and an episode should offer
Up Next in its last 30 seconds with no markers on the server. Both are one line of
`/tmp/plxnative-events.log` or one photograph of the panel.
