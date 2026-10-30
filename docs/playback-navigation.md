# Playback navigation

**Previous/Next** are always part of the default control row for an episode with a neighbor
in its existing PlayQueue; there is no setting for them. Movies, extras and a queue without
either neighbor omit both, which leaves the three existing discs at exactly the same positions.

Settings → Video & playback has one per-profile option, **Instant D-pad seeking**, Off by
default. Off preserves reveal → target → OK for Left/Right. On makes a fresh Left/Right press
jump immediately by the configured Skip interval when the controls are hidden. With controls
visible, it still previews and waits for OK. Holding the D-pad in this optional
hidden-controls path does not start a scrub.

Dedicated **rewind/fast-forward** keys work independently of that option. A tap commits one
Skip interval jump on release, without opening the controls or requiring OK. Waiting for the
release lets a hold use the existing shared held-key mechanism without first seeking. A hold
raises the scrubber, moves its pending target continuously and waits for OK; Back cancels it.

Immediate jumps preserve pause. Fresh taps no more than 1.5 seconds apart accumulate from the
last clamped requested target, including when reversing direction. An earlier keyframe landing
therefore cannot erase the requested travel. After a longer gap, the next tap starts from the
current intended playhead. Another control, pointer interaction, a held scrub or a new item
ends the burst; a same-item seek reload preserves it. A dismissed immediate seek stays hidden
through the first new picture; ordinary rebuffering and failures retain their diagnostic read-out.

The default control row is **Previous, Next**, a wider gap, then **Subtitles, Audio, More**.
Previous restarts at five seconds or later and shows the restart arrow. Before five seconds it
plays the preceding queue item. Unavailable buttons are dimmed, have no pointer stop and are
skipped by keyboard focus. Entering the row focuses Next when enabled, otherwise its first
enabled button. Marker offers and Up Next retain their precedence over this row.

## Queue continuity and maintenance

The feature reuses the existing player screen, focus engine, transport widget, seek request and
stop/resolve/start transaction. It does not copy a player renderer or change native decoding.
Its main files are:

| File under `rust-modules/` | Responsibility |
| --- | --- |
| `screens/src/player/seek_jump.rs` | FF/REW tap policy and optional D-pad jumps |
| `appkit/src/player_hud/seek_jump.rs` | Short-burst requested-target accumulation |
| `appkit/src/player_hud/state.rs` | One default disc row and its availability |
| `appkit/src/player_hud/classic.rs` | The baseline row's shared HStack geometry (width and band); `render.rs` draws the row and its icons from the compiled `builtin:classic` |
| `media/src/route/queue_navigation.rs` | Queue neighbors, continuation and five-second rule |
| `plex/src/plex/session/playback_options.rs` | Independent preference types and defaults |
| `plex/src/plex/queue_window.rs` | Read the existing PlayQueue around a target item |
| `src/app/playback/queue_transport.rs` | Queue handoff shared with Up Next |

Queue handoffs preserve the PlayQueue ID, selected item ID and known rows through the shared
resolve environment. Creating a replacement queue from its remaining tail would lose Previous.
Each handoff reads the existing window around the target item ID using
`GET /playQueues/{playQueueId}` with `center`, `window`, `includeBefore`, `includeAfter` and
`own=0`, as specified in `docs/plex-openapi.json`. Expired, unavailable or mismatched windows
retain the known snapshot. The queue-window behavior is shared with automatic advance.

The D-pad option serves remotes without dedicated FF/REW keys. It can also make frequent jumps
through filler sections in anime more convenient while preserving the ordinary D-pad default.

## Synthetic regression

Use a fresh evidence directory for each run. No household account is needed:

```sh
make sim-macos
python3 tests/fixtures/make_fixtures.py --only mockverify --out /tmp/navigation-media
python3 tests/playback_navigation_sim.py --sim rust-modules/target-sim/debug/plxnative-sim \
  --media /tmp/navigation-media --out /tmp/navigation-evidence
```

This exercises queue continuity, restart after five seconds, an immediate hidden-HUD FF tap,
and visible-HUD D-pad confirmation. Add `--instant-dpad` to exercise the optional D-pad path.
For a repeated-keyframe regression, make a copy with 12-second keyframes (24 fps):

```sh
mkdir -p /tmp/navigation-long-gop
ffmpeg -y -i /tmp/navigation-media/mockverify-v1.mkv -t 60 -map 0 \
  -c:v libx264 -preset ultrafast -b:v 8M -minrate 8M -maxrate 8M -bufsize 16M \
  -x264-params nal-hrd=cbr -g 288 -keyint_min 288 -sc_threshold 0 \
  -c:a copy -c:s copy /tmp/navigation-long-gop/mockverify-v1.mkv
cp /tmp/navigation-media/mockverify-v2.mkv /tmp/navigation-media/mockverify-v2.eng.srt /tmp/navigation-long-gop/
python3 tests/playback_navigation_sim.py --sim rust-modules/target-sim/debug/plxnative-sim \
  --media /tmp/navigation-long-gop --out /tmp/navigation-burst --rapid-seeks
```

The burst regression uses FF taps with the D-pad option Off. Three requested targets advance by
exactly ten seconds while two adjacent taps land on the same keyframe and the sequence reaches
a later keyframe. Host tests also cover REW reversal, intervals, bounds, clock wrap, expiry,
reset, tap versus hold, disabled focus, row spacing and title fitting. Simulator checks do not
establish LG decoding, text rasterization or hardware video-plane behavior; use `tv-session`
for those before shipping.
