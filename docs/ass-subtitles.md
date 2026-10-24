# Native ASS and SSA subtitles

Direct playback preserves the authored script: styles, positioned signs, overlapping dialogue,
drawings, font attachments, movement and karaoke. A re-encode still burns the
selected subtitle on the server; over a remux the app draws a sidecar, and an embedded track when
the link is local and the Part is 1..=30000 kbps, and otherwise the server burns it. ASS/SSA retains its authored layout.
The viewer's existing tone and timing controls still apply to every subtitle.

Embedded TEXT tracks (SubRip, WebVTT, mov_text, SAMI and the other text codecs) render through the same
libass worker. They have no header in the file, so `player::ass_source` synthesizes one
(`player::text_ass::header`) from the viewer's size and position picks and keeps each cue as it arrived;
`player::text_ass::convert` turns the cue's text into an ASS event body in the track's dialect (italic,
bold, underline, colour and `{\an}` placement honoured, everything else shown as plain text). Such a
source has `Origin::Text`: it renders into the full 1920x1080 logical screen, not the video viewport,
and the screen lifts its bottom-anchored cues clear of the transport. A change of the size or position
pick re-synthesizes the header of the selected track lazily (the revision bumps once; the renderer swaps
the track in place). The track's events are a newest-wins view of the cues: a cue's end is clipped to
the start of the next cue of the same alignment class, so an overlap never leaves the old text up beside
the new one. External SRT/VTT files take the same road: `player::sidecar` charset-decodes the file,
`text_ass::script_from_srt`/`script_from_vtt` convert it to one script (WebVTT `line:0` puts a cue at the
top), and `sidecar::ass_source` serves it as an `Origin::Text` source, restyled lazily like an embedded
track. A long or large converted script is fed to libass a window at a time (`ass_script`: at most 8192
lines and 8 MiB per window, except a single line larger than that, which the renderer refuses); only a script loaded whole is bound by 8 MiB. The one thing the sidecar still
draws itself is the failure notice (`sidecar::notice()`, drawn by `draw_subtitle_message`).

`ci/build-libass.py` builds a pinned libass with private FreeType, FriBidi, HarfBuzz and
libunibreak (Unicode line breaking, so a line with no spaces, as in Chinese or Japanese, can
still wrap). libass carries one local patch, `ci/patches/libass-0001-grow-font-faces.patch`,
which lets each font hold more than the upstream ten faces; it is pinned by digest in
`ci/dependencies.json` and the build fails unless it applied. The
application loads its own `libass-plx` by absolute path. Only the versioned `plx_ass_*` facade is
exported; dependency symbols cannot collide with the firmware's font stack. `src/ass.c` reads
libass structures through the same headers used to compile that library. No firmware libass or
new Starfish API is required. Pins, checksums, license notices and corresponding source archives
are part of the normal package/source-bundle workflow.

The facade's ABI is 4 (`plx_ass_abi_version()`; `include/ass.h`). Two callers check it and refuse any
other number: `player/ass.rs` for the subtitles, and `plx_gfx::rtl` for the UI's Arabic and Hebrew
text through the `plx_ass_text_*` calls ABI 4 added (`src/ass_text.c`). Besides the load/render calls
it sets track features (`plx_ass_set_features`, Unicode line breaking), the libass cache bounds
(`plx_ass_set_cache_limits`) and extra line spacing (`plx_ass_set_line_spacing`), reports counters
(`plx_ass_stats`), and replaces the loaded track in place (`plx_ass_replace_track`). The renderer
scans its fonts (`ass_set_fonts`) only on its first load; a replaced track keeps every opened face
and does not extract its own `[Fonts]` section again. The engine uses the replace for two
cases: sliding the window of a long sidecar script, and a restyle (a Text-origin source whose
synthesized header or script changed with the viewer's text size or position, same fonts): the
track is replaced, the held events are appended again and the line spacing of the new header's size
is set. Any other change of source either re-feeds the
same renderer (the same embedded source with the same header and fonts) or opens a new one. A
Text-origin renderer, however it was reached, breaks lines by the Unicode rules and carries the line
spacing of the size its header was built for (`Origin::Text` holds those picks); an authored one
keeps libass's own pitch and breaking. `tests/fixtures/make_ass_faces.py` regenerates the 14 one-glyph test faces.

Fonts. libass runs with no font provider, so it finds a face by family name among the faces the
engine adds when a renderer is created (`ass/fonts.rs`): `appfont.ttf` and `appfont-bold.ttf`
(Inter; Bold also answers to the family "PlxSubs", which the synthesized style names), then the
22 `fonts/subs/NotoSans<Script>-Bold.ttf` faces listed in `fonts/subs/manifest.json`, in manifest
order. Each Noto face answers to "PlxSubs" and "Inter" too, carries nothing Inter Bold covers, and
has Inter Bold's win-metric proportions, so authored ASS naming an unknown family falls to a face
that has the script instead of the CJK default's missing-glyph boxes. Add order is load-bearing
(Inter first). The files are read at each create and not kept. `tools/cut-noto-subs.py` cuts them
and accounts for EVERY language of the picker (a face, the CJK default,
Inter, or uncovered with its reason), which a test checks against the shipped cmaps. Three scripts
are uncovered because Noto Sans ships them in Regular only: Yi (`ii`), Avestan (`ae`) and
traditional Mongolian (`mn`, whose Cyrillic form is Inter's). U+2060 is not carried by any face:
libass skips the default-ignorables (U+200B-200F, U+2060-206F) before it looks a glyph up.

The demuxer copies ASS headers, complete timed packets and font attachments out of FFmpeg.
The source store removes trailing zero terminators from headers and packets. A zero byte
inside a header makes the track invalid. The raw byte limits apply before trimming.
Direct playback and the subtitle side reader use the same source store.
A text track keeps its raw cues (the start, end and text as the demuxer delivered them) apart from the
events it publishes, each with a ReadOrder from a per-track counter that only increases: libass drops an
event whose ReadOrder it has seen, so a number is never reused after its cue is pruned. A packet the demuxer
delivers again after a seek or a demux reopen finds its raw cue and changes nothing (no revision bump),
and the raw cues are carried across a reopen of the same file (the exact delivery identity, the
track inventory and each track's dialect all match), so a long cue the reopened demuxer does not resend
stays up. The window follows the same floor as the other cue stores (the playhead less the longest
subtitle delay and 2 s) and the same event and byte bounds as the ASS tracks.
An empty normalized header or a missing header reports an invalid track. The source store drops
empty normalized packets and packets with an internal zero byte. These packets cannot prevent
valid captions on the same track from rendering. External ASS files retain their existing
validation rules.
`Shared` owns the ASS source store and renderer mailbox alongside the other playback transport
state. `player::ass_source` retains immutable snapshots and a bounded event window, including the
history needed for subtitle delay. It keeps all embedded ASS tracks so selecting another language
can use the already-read portion of the file. A seek changes source identities while retaining known
events, headers and fonts: Matroska does not resend earlier signs spanning the seek target.
Reread packets are deduplicated, and pruning follows a backward seek before the native clock
rebases; a new playback discards the previous media's sources.
Reload-based seeks and demux reopens also retain known events, after revalidating the exact
delivery identity and complete ASS header inventory. A different file or a new playback cannot
inherit them. This matters when an in-place seek falls back to a fresh native Load.
Reaching demux EOF does not discard subtitles while queued video still plays.

For an external ASS/SSA track, `player::sidecar` requests UTF-8 without converting to SubRip and
retains the entire script. It also incorporates the video's font attachments, including when
those arrive after the external file. Source reuse includes server identity, stream identity,
delivery key and codec. Backward seeks and Off→On do not re-download a loaded script.

`player::ass` owns the native objects on one worker. A published frame carries the window of
playheads it stays right for, so a UI tick inside the window enqueues nothing; the one pending
slot holds either a Normal job (the screen needs a frame) or an Ahead job (the next boundary,
rendered early and promoted by the first tick at or after it). A completed frame is accepted only
for the current source/selection epoch. The UI never
waits for font parsing or rasterization. Native change detection avoids work for unchanged output. Overlapping libass images are composed
in order into disjoint regions, with a maximum of 64 regions and one bounded pixel arena.
Consecutive libass image identities also retain native region pixels, so a moving or karaoke
cue does not repeatedly blend unrelated static signs. A bounded image-key table tracks that reuse;
changed geometry, image order, clipping or content invalidates the affected regions.
Unchanged region pixels retain their shared allocation, including across translations, so the
player screen’s `ass_subtitles` cache reuses their GL uploads. Changed regions recycle the
remaining textures; gaps, Off and unmount release them. The cache reports all retained texture
bytes to the shared render accounting.

The subtitle clock interpolates between the pipeline's sparse position callbacks, with at most
250 ms of extrapolation. Pause, seek and discontinuities re-anchor it. Output dimensions and the
original coded video dimensions are passed separately to libass. The output canvas fits the
picture's display aspect, including `sourceInfo`'s pixel aspect ratio, and is offset inside the
video window to account for pillarboxing or letterboxing. Authored subtitles are not moved when
the transport HUD appears.

The device fixture generator requires FFmpeg/ffprobe and MKVToolNix (`mkvmerge`). It verifies
subtitle packet counts, timestamps and mux interleaving before publishing a video.

## Look changes from the old renderer

Release notes in this repository (`docs/release-notes/vX.Y.Z.md`) are written when a release is cut
(`cut-release`); there is no running changelog, so the next release's note takes this list. Text
subtitles (SubRip, WebVTT, mov_text, SAMI and the other text codecs, embedded or sidecar) used to be
drawn by a private caption renderer and now go through libass, with these deliberate differences:

- Italic, underline, strike-through and colour in the subtitle are shown, and `{\an8}`/`{\an5}`
  places a cue at the top or the middle.
- The outline is a true 2 px stroke instead of four offset copies of the text.
- A cue is no longer split into pages or shrunk to fit. A tall cue grows upward, and may cover the Timing
  capsule.
- A lone brace is shown as written, `\h` is a non-breaking space, and the line breaks of SAMI, RealText,
  MicroDVD and MPL2 are honoured.
- Chinese, Japanese and other cues with no spaces wrap at a break the Unicode rules allow.
- ASS that names a font the app does not have ("Inter") now gets Bold Noto glyphs for the scripts Noto
  covers, instead of the CJK default's missing-glyph boxes.

Unchanged by design: the text size and the 48 px line pitch, the bottom baseline at every position and
size, the lift of bottom-anchored cues clear of the transport (200 px), the Timing capsule's position,
newest-wins for overlapping cues of the same alignment, tone and the timing offset. Authored ASS is never
lifted with the transport showing. Not done: dictionary line breaking for Thai, Lao, Khmer and Myanmar
(they break at spaces, as before), stacking of overlapping cues, and WebVTT cue settings other than
`line:0` on a sidecar file.

Verification commands (set `MOCK_PMS_PORT` to an unused local port and configure the developer
build to reach that mock server):

```sh
make check
make check-ffmpeg
make check-ass
CARGO_INCREMENTAL=0 cargo +$(make -s print-rust-toolchain) check --manifest-path rust-modules/Cargo.toml \
  --lib --no-default-features
make
python3 tests/fixtures/make_ass_fixture.py /tmp/ass-render-test
python3 tests/mock_pms.py --host 0.0.0.0 --port "$MOCK_PMS_PORT" \
  --extra-media /tmp/ass-render-test/styled-ass.mkv
```

The native host test checks actual libass pixels; ordinary host tests check source preservation,
selection/seek fencing, overlap, bounds and download policy. Neither proves TV performance.
`make check-ffmpeg` also builds the native ASS renderer and tests terminated headers and packets through
the shared subtitle ingestion path and the HTTP side reader. Both tests check caption pixels and
timing. `tests/fixtures/make_ass_header_fixture.py` regenerates their synthetic MKV from the
captured header and generated captions. Its second subtitle track has no terminators and is the control.
Boot the fixture with `tools/tv-session.sh up --guest --mock --screen player=990001`.
`--mock` verifies the configured server’s synthetic identity and supplies a fabricated guest token;
it never resolves a real Plex account. Configure and build the developer app against that mock first.

Libass cost on the TV is read from the event log, not guessed. While a styled source renders,
`player::ass_perf` writes an `assperf: window_ms=.. calls=.. changed=.. p95_us=.. max_us=.. job_p95_us=..
job_max_us=.. pub_bytes=.. up_bytes=.. jobs=N+A ahead_hit=.. nwait_p95_us=.. nwait_max_us=..` line when a job
arrives at least a second after the previous line (`window_ms` is the span its counts cover,
including any idle stretch before it; read the counts as rates over it) (FFI render time, whole-job time including source load, append, flush-and-reload and window
slide, `changed` and `pub_bytes` count frames published or held ahead whose pixels differ from the one on screen, bytes uploaded; `jobs` counts the worker's Normal and Ahead jobs
separately, `ahead_hit` the ahead frames the UI thread promoted, `nwait` how long a Normal job
waited behind an Ahead job already rendering; the partial window is written when the source is
deselected) and
one `assperf: worker tid=<n>` line when the worker starts, which `tools/tv-sched-trace.sh --tid <n>`
follows. The UI-thread texture upload is the `assup` span on `FRAMEDROP spans=` (spans print only on
`FRAMEDROP` lines, with `/tmp/plxnative-framedrop` armed); each changed region uploads with a full
`glTexImage2D` (a bucketed `glTexSubImage2D` variant that reused the texture was measured on the TV and gave no gain, #735). Nothing is
logged while no styled source is selected, and a render costs one clock pair. The worker renders
only when the screen needs a frame (a new key, a playhead outside the published frame's validity
window, an animated cue) and renders the next boundary ahead of its time, so a steady cue costs no
job at all between boundaries (no line is written then), not one per UI frame. `python3
tests/fixtures/make_ass_stress.py <dir>` builds the stress MKVs (Latin, CJK, Thai, Devanagari,
Arabic, rapid, karaoke, long, 4K60; host `ffmpeg`/`ffprobe` only) to serve with `mock_pms.py
--extra-media`, and the text-codec ones: `srt-*` (the scripts, rapid and long as SubRip in MKV,
plus `srt-markup` and `srt-overlap`), `mov_text-mixed.mp4` and `webvtt-mixed.mkv`. A text cue logs
`sub cue [a..bms] len=N` (the length, never the words) for the selected track.

Device checks must hold the TV lease and keep both the panel and sound off. Release the lease
while building or reviewing captures. Compare the unchanged
build and the candidate on the same media: styles and overlap, moving/karaoke cues, track changes,
Off, pause, backward/forward seek, external ASS and plain subtitles, and every text codec
(`tests/fixtures/make_ass_stress.py` also builds SubRip, mov_text and WebVTT fixtures, each with the
default subtitle disposition so the player selects the track by itself; SAMI and the other codecs the
bundled FFmpeg's demuxers cannot carry are covered by the converter's host tests). Capture and inspect the
output, and compare the UI heartbeat and hardware displayed-frame counters with capture disabled
during performance measurement.
