# Player pointer regression (#131)

The FIFO and Lab Control accept authored 1920×1080 integer coordinates:

| Token | SDL event |
| --- | --- |
| `pm:X,Y` | One motion; becomes a drag while the button is held |
| `pd:X,Y` | Left-button press, held until release |
| `pu:X,Y` | Left-button release |
| `ck:X,Y` | Existing atomic click: two motions, press, release |

The independent primitives add no jitter, click or release of their own. Malformed or overflowing
coordinates are rejected. Off-canvas integers clamp to the canvas edge, preserving `ck:`'s prior
policy. Replay and all four tokens share the authored-to-window event encoder.

The full ingress regression also found a player defect: `bridge::release_input` sends the Input
machine's typed `Key::Ok`/`Edge::Up` with zero raw keycodes. `PlayerScreen` reclassified those
zeros as `Other`, leaving the scrub preview held forever without committing the seek. The shared
`classify_input` boundary now honors canonical navigation keys and reads raw fields only for
`Other`; player, player overlays, trailer and login-alert consumers use it. Earlier screen-only
tests sent raw `SDLK_RETURN` and therefore
never exercised the actual pointer release. This mismatch also exists in the session-8 tree.

`ck:` can reveal a hidden HUD without seeking: its events are ingested before another frame is
presented, and hit testing uses the **last presented frame**. That frame has no scrub stop while
the HUD is hidden, so the click belongs to the picture and toggles playback. Separate motion lets
the HUD present before pressing. The host regression reproduces this at both coordinates from
TV session 8, `(1400,870)` and `(1400,890)`, then resolves both to the real registered scrub stop
after presentation even while D-pad hover suppression remains armed. The session-8 log did not
record HUD visibility; this is a reproduced mechanism, not proof of that historical run's cause.

## Device protocol

Use `tv-lock` and `tv-session`, keeping the panel **off** and sound **off**. Run only against a
synthetic fixture URL or mock PMS; no household Plex playback. Boot with `--guest` and verify the
identity and synthetic URL in the log before input. Use the baseline
`pipe_h264_ac3_1080p.mkv` at its full fixture duration, without autoseek or pause triggers. Let the
video bind and advance before starting. Keep each click/drag on the same app instance; collect
the complete event log afterward. The driver lane owns boot, lock, fixture serving and teardown.

Run the following device commands with the lock held. The delays are between input frames; do
not combine them into one FIFO write. Save captures immediately, inspect them after the input
sequence, and release the lease before inspecting images or writing a report.

```sh
tools/tv-session.sh key pm:1400,870
sleep 1
tools/tv-session.sh shot /tmp/player-pointer-hud.png
tools/tv-session.sh key ck:1400,870
sleep 2
tools/tv-session.sh key pm:1400,890
sleep 1
tools/tv-session.sh key ck:1400,890
sleep 2
tools/tv-session.sh key pd:900,870
sleep 1
tools/tv-session.sh key pm:1200,700
sleep 1
tools/tv-session.sh key pm:700,700
sleep 1
tools/tv-session.sh shot /tmp/player-pointer-held.png
tools/tv-session.sh key pu:700,700
sleep 2
tools/tv-session.sh key pu:700,700
sleep 2
tools/tv-session.sh key pm:701,700
sleep 2
tools/tv-session.sh log > /tmp/player-pointer.log
python3 tests/player_pointer.py /tmp/player-pointer.log
```

The first capture must show the HUD and scrubber before the first click. The held capture must
show the preview moved left, with the pointer outside the scrub band's vertical range (810–970).
There must be exactly one `scrub: pointer commit ns=…` and one matching
`seek(in-place): av_seek t=…` for each click and for the release; none while held or after the
repeated release. The equal-x clicks must request the same position, and the final drag must seek
backward. The log grader checks those boundaries rather than counting total seeks alone.

For a HUD-reveal capture, first let the HUD expire while playing, capture the hidden state, then
send `pm:1400,870` and capture it again after one second. Do this in a separate run so it cannot
confuse the ordered markers above. Capture inspection proves presentation, while panel-off
testing cannot establish the physical pointer's subjective feel.

Run the baseline synthetic playback/FPS measurement on the base and candidate under identical
conditions. The event encoder does no per-frame work, and the new commit log runs once per
released scrub, but those facts do not replace a real-device FPS comparison.

## Host evidence

`app::run::lifecycle_regression_tests::remote_fifo_exposes_independent_pointer_edges` was observed
failing against the base's unmodified event dispatcher because `pd:900,870` was rejected. It
failed again after adding the tokens, with no `CommitSeek` on the actual zero-raw pointer release.
Both failures were observed before their respective fixes. It
drives SDL's actual event queue and app ingress, then the dispatcher/player, asserting held state,
preview without seek, one commit on release, and no commit on a repeated release. Screen tests
use production stop registration to distinguish HUD visibility from hover suppression at the
historical click coordinates. `make check` covers these Rust tests in its hostsim leg.

`python3 tests/player_pointer.py --selftest` grades saved-log fixtures with missing primitives,
premature commits, missing commits, duplicate commits and wrong native seek targets. This script
is offline and does not acquire or drive the TV.
