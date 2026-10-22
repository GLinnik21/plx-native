# Legacy Playback Controls

Enable **Settings → Playback → Legacy Playback Controls → On**. The preference is saved for
the current profile and defaults to Off. Missing or unrecognized stored values retain the
modern controls.

- With the HUD hidden, Left/Right immediately seeks by the existing Skip interval without
  opening the controls. Each fresh press is one jump; holding the key does not start a scrub.
- With the HUD visible, the existing preview/OK-to-confirm gesture is unchanged.
- Previous restarts the current item at five seconds or later. Before five seconds it plays
  the previous queue item. It is unavailable when there is no previous item and playback is
  still inside the first five seconds.
- Next plays the next queue item and is unavailable at the queue's end.
- Subtitles, Audio, More, Info, Chapters, Skip Intro/Credits and Up Next retain their existing
  behavior. Marker offers still temporarily occupy the control row.
- An immediate seek preserves a viewer's pause. Genuine buffering and failures can still show
  the normal diagnostic read-out.

## Carrying this feature in a fork

The implementation deliberately adds a variant to the existing HUD rather than copying the
player screen or its renderer. Shared playback fixes continue to apply to both modes.

The feature's main files are:

| File under `rust-modules/` | Responsibility |
| --- | --- |
| `screens/src/player/legacy_controls.rs` | Hidden-HUD key policy and regression tests |
| `appkit/src/player_hud/legacy_controls.rs` | Previous/Next row, actions and geometry |
| `media/src/route/legacy_controls.rs` | Queue neighbor selection and five-second rule |
| `plex/src/plex/session/playback_controls.rs` | Persisted preference type and default |
| `plex/src/plex/legacy_controls.rs` | Read the current PlayQueue window around a target item |
| `src/app/playback/queue_transport.rs` | Shared queue playback handoff, also used by Up Next |

Small integration hooks remain in the HUD slot, player key dispatcher, preference schema/live
setting registrations and Settings picker. Translations live in separate `legacy_controls.json`
catalog shards, and Previous/Next use separate SVG assets. Those hooks must be retained when
merging upstream updates; separate files reduce conflicts but cannot eliminate them.

Queue handoffs carry the existing PlayQueue ID, selected item ID and rows through the shared
resolve environment. Creating a fresh queue on Next can return only the remaining tail and lose
the preceding item; preserving the queue keeps Previous working after manual or automatic advance.
Each handoff refreshes the existing queue window around the target's item ID, so long queues
continue beyond the initially returned page. This read uses `GET /playQueues/{playQueueId}` with
`center`, `window`, `includeBefore`, `includeAfter` and `own=0`, as specified in the bundled
`docs/plex-openapi.json`. An expired, unavailable or mismatched window retains the known snapshot.

No native decoder, Starfish/ACB, demuxer or build configuration changes are needed.
Host tests and simulator checks do not establish LG decoder or video-plane behavior; finish
device verification through the repository's `tv-session` workflow before shipping.

The synthetic simulator regression exercises Next/Previous queue continuity, the five-second
restart rule, an immediate hidden-HUD seek and visible-HUD confirmation without household data:

```sh
make sim-macos
python3 tests/fixtures/make_fixtures.py --only mockverify --out /tmp/legacy-media
python3 tests/legacy_controls_sim.py --sim rust-modules/target-sim/debug/plxnative-sim \
  --media /tmp/legacy-media --out /tmp/legacy-controls-evidence
```

Use a new output directory for each run. A normal buffering read-out can briefly appear while
the simulator primes its clock sink after seeking; the HUD remains dismissed when playback resumes.
