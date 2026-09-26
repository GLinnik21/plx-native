# Native ASS and SSA subtitles

Direct playback preserves the authored script: styles, positioned signs, overlapping dialogue,
drawings, font attachments, movement and karaoke. Transcoded playback continues to burn the
selected subtitle on the server. The simple caption size/wrapping rules apply to plain text;
ASS/SSA retains its authored layout. The viewer's existing tone and timing controls still apply.

`ci/build-libass.py` builds a pinned libass with private FreeType, FriBidi and HarfBuzz. The
application loads its own `libass-plx` by absolute path. Only the versioned `plx_ass_*` facade is
exported; dependency symbols cannot collide with the firmware's font stack. `src/ass.c` reads
libass structures through the same headers used to compile that library. No firmware libass or
new Starfish API is required. Pins, checksums, license notices and corresponding source archives
are part of the normal package/source-bundle workflow.

The demuxer copies ASS headers, complete timed packets and font attachments out of FFmpeg.
`Shared` owns the ASS source store and renderer mailbox alongside the other playback transport
state. `player::ass_source` retains immutable snapshots and a bounded event window, including the
history needed for subtitle delay. It keeps all embedded ASS tracks so selecting another language
can use the already-read portion of the file. A seek changes source identities while retaining known
events, headers and fonts: Matroska does not resend earlier signs spanning the seek target.
Reread packets are deduplicated, and pruning follows a backward seek before the native clock
rebases; a new session discards the previous media's sources.
Reaching demux EOF does not discard subtitles while queued video still plays.

For an external ASS/SSA track, `player::sidecar` requests UTF-8 without converting to SubRip and
retains the entire script. It also incorporates the video's font attachments, including when
those arrive after the external file. Source reuse includes server identity, stream identity,
delivery key and codec. Backward seeks and Off→On do not re-download a loaded script.

`player::ass` owns the native objects on one worker. A single latest-request slot coalesces clock
updates; a completed frame is accepted only for the current source/selection epoch. The UI never
waits for font parsing or rasterization. Native change detection avoids work for unchanged output. Overlapping libass images are composed
in order into disjoint regions, with a maximum of 64 regions and one bounded pixel arena.
Unchanged region pixels retain their shared allocation, including across translations, so the
player screen’s `ass_subtitles` cache reuses their GL uploads. Changed regions recycle the
remaining textures; gaps, Off and unmount release them. The cache reports all retained texture
bytes to the shared render accounting.

The subtitle clock interpolates between the pipeline's sparse position callbacks, with at most
250 ms of extrapolation. Pause, seek and discontinuities re-anchor it. Output dimensions and the
original coded video dimensions are passed separately to libass. The output uses the player's
video-plane destination canvas; authored subtitles are not moved when the transport HUD appears.

The device fixture generator requires FFmpeg/ffprobe and MKVToolNix (`mkvmerge`). It verifies
subtitle packet counts, timestamps and mux interleaving before publishing a video.

Verification commands (set `MOCK_PMS_PORT` to an unused local port and configure the developer
build to reach that mock server):

```sh
make check
make check-ass
CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path rust-modules/Cargo.toml \
  --lib --no-default-features
make
python3 tests/fixtures/make_ass_fixture.py /tmp/ass-render-test
python3 tests/mock_pms.py --host 0.0.0.0 --port "$MOCK_PMS_PORT" \
  --extra-media /tmp/ass-render-test/styled-ass.mkv
```

The native host test checks actual libass pixels; ordinary host tests check source preservation,
selection/seek fencing, overlap, bounds and download policy. Neither proves TV performance.
Boot the fixture with `tools/tv-session.sh up --guest --mock --screen player=990001`.
`--mock` verifies the configured server’s synthetic identity and supplies a fabricated guest token;
it never resolves a real Plex account. Configure and build the developer app against that mock first.

Device checks must hold the TV lease and keep both the panel and sound off. Release the lease
while building or reviewing captures. Compare the unchanged
build and the candidate on the same media: styles and overlap, moving/karaoke cues, track changes,
Off, pause, backward/forward seek, external ASS and plain subtitles. Capture and inspect the
output, and compare the UI heartbeat and hardware displayed-frame counters with capture disabled
during performance measurement.
