# Native ASS and SSA subtitles

Direct playback preserves the authored script: styles, positioned signs, overlapping dialogue,
drawings, font attachments, movement and karaoke. A re-encode still burns the
selected subtitle on the server; over a remux the app draws a sidecar, and an embedded track when
the link is local and the Part is 1..=30000 kbps, and otherwise the server burns it. ASS/SSA retains its authored layout.
The viewer's timing control applies to every subtitle, and so does a gray tone (Settings > Audio &
subtitles > Subtitle color, or the Color section of the player's Subtitles menu), which only dims
the drawn bitmaps. A COLOR tone (Yellow) is different: it is the default colour of a synthesized
text track (below), and in an authored ASS/SSA script (embedded, or a sidecar file, including a
converted SubRip that reaches the app as an ASS track) it replaces the PrimaryColour of every style
whose primary is exactly pure white (`&H00FFFFFF`, in any spelling libass reads: `&h` or `0x` hex, or decimal; alpha 0): white is the default
ink, so it follows the pick. `player::ass_ink` does the rewrite on the header that reaches libass
(the file's own bytes stay the reopen identity), through `SubtitleTone::text_rgb8`, the one palette.
A style whose primary is any other colour, an inline `\c` override (explicit white included), the
outline and shadow colours and the karaoke SecondaryColour (recoloured only when it is itself pure
white) are left alone, and the grays never rewrite a header (they stay a draw-time tint). A colour
change restyles the live track once, in place (`ass::restyles`). Cost, known: signs and typesetting
drawn in white take the colour too. Picture subtitles (PGS, VobSub) are never recoloured. Whether the pick reaches the active track is one fact,
`player::subtitle_takes_colour` (an ASS/SSA track: its header has a white-primary style, judged on the file's
own bytes; delivered to the session's readout once a tick by `player::deliver_subtitle_ink`), which the
Subtitles menu's Color page reads to dim the colours that would do nothing.

Embedded TEXT tracks (SubRip, WebVTT, mov_text, SAMI and the other text codecs) render through the same
libass worker. They have no header in the file, so `player::ass_source` synthesizes one
(`player::text_ass::header`) from the viewer's size, position and colour picks and keeps each cue as it arrived;
`player::text_ass::convert` turns the cue's text into an ASS event body in the track's dialect (italic,
bold, underline, colour and `{\an}` placement honoured, everything else shown as plain text; a SubRip
`<font color>` that is plain white is the file's default ink spelled out, so it is dropped and the viewer's colour pick applies). Such a
source has `Origin::Text`: it renders into the full 1920x1080 logical screen, not the video viewport,
and the screen lifts its bottom-anchored cues clear of the transport. A change of the size, position or colour
pick (a gray tone is a draw-time tint, so only a colour counts) re-synthesizes the header of the selected track lazily (the revision bumps once; the renderer swaps
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

The facade's ABI is 5 (`plx_ass_abi_version()`; `include/ass.h`). Two callers check it and refuse any
other number: `player/ass.rs` for the subtitles, and `plx_gfx::rtl` for the UI's Arabic and Hebrew
text through the `plx_ass_text_*` calls ABI 4 added (`src/ass_text.c`). ABI 5 let `plx_ass_add_font`
run after `plx_ass_load` and lifted its 64 MiB limit (issue #1328, "Attached fonts" below), and
`plx_ass_add_font_borrowed` hands libass a font by reference instead of copying it. Besides the load/render calls
it sets track features (`plx_ass_set_features`, Unicode line breaking), the libass cache bounds
(`plx_ass_set_cache_limits`) and extra line spacing (`plx_ass_set_line_spacing`), reports counters and the last render's phase timing
(`plx_ass_stats`), and replaces the loaded track in place (`plx_ass_replace_track`). The renderer
scans its fonts (`ass_set_fonts`) only on its first load; a replaced track keeps every opened face
and does not extract its own `[Fonts]` section again, and a font added after the load is opened
alone at the next frame (libass's `ass_update_embedded_fonts`), so `font_scans` stays 1. The engine uses the replace for two
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

Attached fonts. A file may attach far more font bytes than a renderer needs (the report in #1328: 17
fonts, 83 MB; the opening's five karaoke faces sat after ~32 MB of faces no style names and before
small faces that still fit, so the old 32 MiB cap dropped exactly the five the opening needed and
kept twelve it did not), and no byte cap
decides what a track can draw: the demuxer takes every font attachment out of the format as it opens (`detach_fonts`, which also frees
libavformat's `codecpar` copy) and hands them over with the headers (`collect_manifest`), the
source store keeps the file's list whole (a same-file reopen keeps the `Arc` it holds, so the
renderer's same-fonts check is a pointer compare), and the renderer hands libass the faces the track
NAMES. With no font provider libass selects a face by the requested family, then "Inter", then the
CJK file, so an attached face that no requested name matches is never drawn and its bytes would be
dead weight. `player/ass/font_names.rs` (pure) reads each attachment's sfnt `name` table (family 1,
typographic family 16, WWS family 21, full 4, PostScript 6, on every platform, plus the CFF font name,
TrueType Collections included) and selects a face when a requested name equals one of them under ASCII
case folding, the rule being a superset of libass's own matching so it cannot hide text. The requested names are the Fontname of every `Style:` line of the
header (a sidecar script: its whole text, windowed or not), the always-requested "Inter" and "Arial",
and every `\fn` override, which `Native::append` reads in each event packet before `plx_ass_chunk` and
adds the face for then. A face shares its family with others: all of them are added together, in
container order, so libass's weight scoring and add-order tie break are what loading every font would
give. A font that cannot be read is added (unknown means needed); a byte-identical second copy is not.
A restyle of a text track reads the new header the same way. The event log says how many the renderer
holds, numbers only: `ass: embedded ... fonts=17 font_bytes=...` when the file's inventory is read,
and `ass: renderer fonts=5/17 font_bytes=...` for each renderer (again when a later event adds a face).
What is resident is the file's attachments as libavformat holds them (the Matroska demuxer's own
element buffer, one copy per attachment for as long as the file is open, and with the side reader,
which does not run `avformat_find_stream_info`, the stream's internal codec context's too; both are
private to libavformat) and the store's copy of them, one `Arc<[u8]>` per font. libass is handed no
copy: `Native::font_shared` gives it the store's buffer by reference (`plx_ass_add_font_borrowed`, which
the renderer keeps alive until after `plx_ass_destroy`), so a face a track names is resident once
however many renderers draw with it. The demuxer
frees its own `codecpar->extradata` copy of every attachment the moment `avformat_open_input` returns
(`detach_fonts`). What libass still copies per renderer are the bundled faces (`fonts::bundled`) and a
font the script's own `[Fonts]` section carries (libass extracts those itself). Measured on the host with
a 6 x 8 MiB fixture in a fresh process (`make check-ffmpeg`, `ff_font_memory_tests.rs`; `make check-ass`,
`player/ass/tests.rs`), the main demuxer holds the fonts 1x beside the store's 1x (3x and 1x before) and
a renderer over them holds 9 MiB (57 MiB before).

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

`player::ass` owns the native objects on its workers: one primary, started by the first request, and
up to two helpers. A published frame carries the window of
playheads it stays right for, so a UI tick inside the window enqueues nothing; the one pending
slot holds either a Normal job (the screen needs a frame) or an Ahead job (the next boundary,
rendered early and promoted by the first tick at or after it). A completed frame is accepted only
for the current source/selection epoch.

A MOVING cue (karaoke, `\t`, `\move`, `\fad`, an Effect) is right for one playhead only, so a frame
asked for when the screen needs it is shown one render late (50-150 ms on the television, issue
#1328). While the player is running the mailbox instead PLANS the frames (`player::ass::mailbox`):
it hands out `Pipe` jobs for future playheads on a grid of one `step` (what a render costs over the
workers, scaled by the playback speed, never under one video frame or 33 ms, never over 250 ms), the
first a `lead` ahead of the playhead (a render, a quarter more, and a UI tick), holds each frame
that lands, and a tick shows the held frame with the greatest playhead not after its own: never
early, at most a step old. An event boundary inside a moving stretch (the stress fixture's opening has
one every ~19 ms) does not end a frame or take the plan off its grid; the plan resolves it at its own
step, and only the boundary that ends the stretch is rendered at its own playhead (a frame the worker
reports as `moves_on` stands until a newer one lands or its age runs out). Nothing is planned past the horizon (the lead and a step per worker), so
a worker with nothing to do parks. The pace (`running`, the applied speed, the video frame period)
and the tick interval reach the mailbox as the values of `ass::request`'s `Pace`
(`player::subtitle_pace`), never as a clock or a registry read inside it. A revision that only
APPENDED events keeps every planned frame for a playhead before the first event it adds
(`earliest_change_ms`, built on `append_from`) and replans from there; a revision that reaches the
playhead, a seek, a new source or size, a jump of the clock, a pause and a worker so slow that
nothing lands in time (a hole) each throw the plan away and ask for one Normal job, as a still cue does.
The cost the plan paces by is the worker-reported time of the moving frames (a job that opened or
re-synced the renderer is no sample), a running mean capped at 500 ms a sample, kept as the cost at
FULL size (a frame drawn smaller is taken back to it, see "A heavy moving stretch is drawn smaller").

One worker cannot make thirty frames a second from an 80 ms render, so the plan asks for the workers
that can (`workers_wanted`: the cost, at the size the stretch is drawn at, over the 33 ms floor) and the runtime starts helpers up to what the
device allows: the primary and a helper per other core, at most `MAX_ASS_RENDERERS` (3), and no more
helpers than a quarter of the memory available holds (the primary is already open when the verdict is
asked, so it is not charged again; `pool::decide`: each renderer is the fonts it copies, the
bundled faces (the attached ones are the store's, shared), plus a bitmap cache; `/proc/meminfo`'s `MemAvailable` is read
once for a selection, the first time a pipeline drawing at full size would want a helper (`pool_wanted`), and the verdict is logged as
`ass: pool renderers=N of M font_bytes=.. per_renderer=.. mem_available=.. budget=..`; a heavy file on
a short television runs the primary alone, and the primary always runs). Every worker
owns a whole libass instance (`Engine`: its own `ASS_Library`, renderer and track, which libass does not
let two threads share), fed the same `Source` snapshot, so a helper costs the bundled faces, the caches
and the events again, but not the attached fonts, which it references; it is started from the same `task::spawn` site as the primary, only while a moving cue is
planned, counted in the plan's pacing only once it has rendered a steady frame (its first job opens the
source and lands late), and gives its renderer back after 10 s with nothing to do. Each job carries the
newest frame that had landed (`Job::reference`) and the renderer shares the allocation of every region
that draws identically to one of that frame's (before looking at its own last frame), so a static sign
rendered by another engine is not uploaded again by the screen: the texture cache keys on the
allocation. The UI never
waits for font parsing or rasterization. Native change detection avoids work for unchanged output. Overlapping libass images are composed
in order into disjoint regions, with a maximum of 64 regions and one bounded pixel arena.
Consecutive libass image identities also retain native region pixels, so a moving or karaoke
cue does not repeatedly blend unrelated static signs. A bounded image-key table tracks that reuse;
changed geometry, image order, clipping or content invalidates the affected regions.
Unchanged region pixels retain their shared allocation, including across translations, so the
player screen’s `ass_subtitles` cache reuses their GL uploads. Changed regions recycle the
remaining textures; gaps, Off and unmount release them. The cache reports all retained texture
bytes to the shared render accounting.

The pixels of a region are PREMULTIPLIED RGBA, published as composed (#1328): the facade blends
every layer in premultiplied space and does no straight-alpha conversion pass, which was about 45% of
the composite's time on the host and walked mostly transparent pixels. The screen draws them through
`Painter::tex_premultiplied` (`gfx::draw_tex_premultiplied`: `ONE, ONE_MINUS_SRC_ALPHA`, the tint's
colour scaled by its alpha after the painter's cascade), so the ink rung and a fade dim them as they
dim a straight texture. Image subtitles (PGS, VobSub) and glyph textures are uploaded straight and
stay on `Painter::tex`.

## A heavy moving stretch is drawn smaller

Helpers are limited by the set's cores and memory, so a stretch that one renderer cannot make thirty
frames a second of (the opening of the issue's file is ~85 ms a frame at 1080p on the television) is
drawn at 2/3 (720p) or 1/2 (540p) of the output size instead (#1328, `player::ass::scale`). libass is told
the smaller frame size and the SAME storage size (the facade's `ass_set_frame_size` takes the render
size, `ass_set_storage_size` the unchanged one, and the script's PlayRes is untouched), so the script
lays out identically and only the number of pixels
it draws changes; the regions come back in the smaller canvas and the screen draws each up to the
rectangle it covers on the output with the linear filter its upload sets (`Frame::output_edges`,
`ass_subtitles::region_rect`: both edges of a region are mapped, so regions that abut still abut).

What it buys, measured on the host with the storage size fixed: libass takes 0.64x the time at 720p
and 0.49x at 540p, the composite 0.49x and 0.30x, and the bytes published 0.54x and 0.37x. Weighted by
the television's split of an 85 ms frame (65 ms in libass, 20 ms in the composite) a frame costs about
0.60 and 0.44 of one at full size, the constants `COST_RATIO_TWO_THIRDS` and `COST_RATIO_HALF`, rounded
down so a cost taken back to full size is never overstated.

The choice is a pure `Governor` the mailbox owns, never a clock or a registry read. The mailbox keeps
the cost at FULL size (each sample is divided by its rung's ratio) and, on each tick of the running
pipeline, asks which rung the stretch should be drawn at: the largest whose projected step (cost x
ratio x playback speed / renderers) meets the target, 33 ms or one video frame, whichever is longer.
The renderers are the pool's verdict for the selection (`Runtime::renderers_decided`): nothing is
decided before the pool has said, and a stretch that full size would not want a helper for never
consults it and stays at full size. The first steady frame of a stretch decides at once (what the last
stretch cost says nothing of this one); inside one, a rung steps down when its projection misses the
target for 600 ms, and back up one rung only when the larger rung projects under 75% of the target for
3 s (both holds scale with the playback speed, and count only after six steady frames of the rung). A
step down within 10 s of a step up bars the next step up for 30 s, so a model that is wrong costs one
flap per half minute at most.

Only the pipeline's `Pipe` jobs are drawn smaller, and the Normal job that fills a hole inside a reduced
stretch (a frame of another size would empty libass's caches, and the next one would pay for them
again). Any other Normal or Ahead job, the first frame of a stretch, a still cue and a paused player are
drawn at full size, and the still picture a reduced stretch hands over to is drawn again at full size by
one Normal job (the reduced frame stands until it lands, and is not withdrawn as "covered"). The rung
belongs to the selection it was decided for: a new epoch, source or size starts at full size again. The scale is not part of the `Key`, so a move never blanks the screen: the frame
on screen stays until a frame at the new scale lands, and each `Frame` says which scale it was drawn at.
libass empties its caches when the frame size changes, so a render at a new size is cold and is no cost
sample. Each move is logged once (`ass: render scale 1.00 -> 0.50 cost=.. renderers=.. rate=.. target=..`)
and every `assperf` line carries `scale=`.

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
newest-wins for overlapping cues of the same alignment, the outline, tone (white and the grays) and the timing offset. Authored ASS is never
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
  --lib --no-default-features --features play-uri,play-feed
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
`player::ass_perf` writes an `assperf: w=.. window_ms=.. calls=.. changed=.. p95_us=.. max_us=.. job_p95_us=..
job_max_us=.. pub_bytes=.. up_bytes=.. jobs=N+A ahead_hit=.. shown=.. late=.. stale=.. nwait_p95_us=..
nwait_max_us=.. lib_p95_us=.. comp_p95_us=.. lib_cpu_p95_us=.. comp_cpu_p95_us=.. images_p95=..
img_kpx_p95=.. scale=..` line per worker (`w` is the worker, 0 the primary) when a job
arrives at least a second after that worker's previous line (`window_ms` is the span its counts cover,
including any idle stretch before it; read the counts as rates over it) (FFI render time, whole-job time including source load, append, flush-and-reload and window
slide, `changed` and `pub_bytes` count frames published or held ahead whose pixels differ from the one on screen, bytes uploaded; `jobs` counts the worker's Normal and Ahead jobs
separately (a frame the pipeline planned is an Ahead one), `ahead_hit` the boundary frames of still cues the UI thread
promoted, `shown` the frames of the moving cue's pipeline it promoted (updates a second) and `late` how many of those came more than a tick after
their playhead, `stale` the frames this worker rendered that were thrown away when they landed, `nwait` how long a Normal job
waited behind an Ahead job already rendering; the partial window is written when the source is
deselected). `up_bytes`, `ahead_hit`, `shown` and `late` are counted by the UI thread and read by the
primary's line alone (a helper's line has zeros there). The last six fields split `p95_us` between the two things `plx_ass_render` does, timed by
the facade itself with `clock_gettime` (`src/ass.c`, read back through `PlxAssStats.last_*`): `lib_p95_us`
is libass's own `ass_render_frame`, `comp_p95_us` is everything after it (bounds, regions and our
per-pixel composite (premultiplied RGBA, published as composed; eight pixels at a time with NEON, `src/ass_composite.h`, scalar on a target without it), 0 for a call that found the frame unchanged), both wall time over
the same calls as `p95_us`; `lib_cpu_p95_us` and `comp_cpu_p95_us` are those two phases on the worker
thread's CPU clock (`CLOCK_THREAD_CPUTIME_ID`), so a phase whose wall figure is far above its CPU one was
waiting for the CPU (descheduled, or blocked), not computing; `images_p95` and `img_kpx_p95` are the `ASS_Image`s libass produced for a composed frame and
the sum of their width x height in thousands of pixels, over the composed frames only; `scale` is the
smallest size a job of the window was drawn at (`1.00`, `0.67`, `0.50`: the side over the output's), so a
line is read at its own scale. Attribute a slow
render with these before optimising either half. The worker also writes
one `assperf: worker w=<n> tid=<n> nice=<n>` line when it starts, which `tools/tv-sched-trace.sh --tid <n>`
follows. Workers run below the frame thread: the first at niceness +5, a helper at +10
(`plx_base::task::lower_priority`, a per-thread `setpriority`, so only the worker is touched; the
line has no `nice=` where the platform has none or the kernel refused it). The UI-thread texture upload is the `assup` span on `FRAMEDROP spans=` (spans print only on
`FRAMEDROP` lines, with `/tmp/plxnative-framedrop` armed); each changed region uploads with a full
`glTexImage2D` (a bucketed `glTexSubImage2D` variant that reused the texture was measured on the TV and gave no gain, #735). Nothing is
logged while no styled source is selected, and a render costs one clock pair in Rust and six clock reads in the facade (three per clock). The worker renders
only when the screen needs a frame (a new key, a playhead outside the published frame's validity
window) or plans one (an animated cue, while playing: one frame per step, ahead of its playhead) and
renders the next boundary of a still cue ahead of its time, so a steady cue costs no
job at all between boundaries (no line is written then), not one per UI frame. `python3
tests/fixtures/make_ass_stress.py <dir>` builds the stress MKVs (Latin, CJK, Thai, Devanagari,
Arabic, rapid, karaoke, long, 4K60; host `ffmpeg`/`ffprobe` only) to serve with `mock_pms.py
--extra-media`, and the text-codec ones: `srt-*` (the scripts, rapid and long as SubRip in MKV,
plus `srt-markup` and `srt-overlap`), `mov_text-mixed.mp4` and `webvtt-mixed.mkv`. `--fonts-over-budget` builds
only `fonts-over-budget.mkv` (~410 MB, issue #1328): a dense 90 s karaoke opening with 17 attached fonts
(83 MB) whose opening faces all come after the first 32 MiB of font bytes (the cap the renderer
used to apply, removed by that issue), on 22 Mbit/s video. A text cue logs
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
