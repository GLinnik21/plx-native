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
Pause the synthetic clip before this core protocol to pin its HUD during capture. The successful
post-seek pause receipt and stationary samples below also verify that scrubbing preserves pause.

Run the following device commands with the lock held. The delays are between input frames; do
not combine them into one FIFO write. Save captures immediately and inspect them after the input
sequence. At session handback, release the lease promptly when device work pauses.

```sh
tools/tv-session.sh key pause
sleep 2
tools/tv-session.sh key pm:1400,870
sleep 1
tools/tv-session.sh shot /tmp/player-pointer-hud.png
tools/tv-session.sh key ck:1400,870
sleep 5
tools/tv-session.sh key pm:1400,890
sleep 1
tools/tv-session.sh key ck:1400,890
sleep 5
tools/tv-session.sh key pd:900,870
sleep 1
tools/tv-session.sh key pm:1200,700
sleep 1
tools/tv-session.sh key pm:700,700
sleep 1
tools/tv-session.sh shot /tmp/player-pointer-held.png
tools/tv-session.sh key pu:700,700
sleep 5
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
After each native seek it also requires `seek: paused frame restored ns=…`, emitted only after
the existing successful pause boundary closes the seek's temporary feed override, followed by at
least two heartbeat `pos=…s` samples whose spread is at most one second (integer quantization).
This proves an accepted native pause plus an observed stationary playhead; it is not a firmware
state callback. If five seconds does not collect the receipt and two samples, wait for them before
sending the next phase's token. A missing witness is a failure, not an assumed pause.

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
premature commits, missing commits, duplicate commits, wrong native seek targets and missing or
moving paused-state samples. `make check` runs this self-test. The script is offline and does not
acquire or drive the TV.

## SDL ABI and firmware review

The tracked `include/SDL2/SDL_events.h` mouse-button struct and the NDK sysroot's matching
declaration contain four `Uint32` fields, four `Uint8` fields, then two `Sint32` coordinates.
`SDL_stdinc.h` defines those through `uint32_t`, `uint8_t`, and `int32_t`. Therefore on ARM32,
button is at 16, state at 17, x at 20 and y at 24; the motion struct's x/y offsets also match.
`SDL_mouse.h` defines left-button as 1; `SDL_events.h` defines pressed/released as 1/0.
The NDK GCC was run with its explicit `--sysroot`, `-std=c11 -fsyntax-only -Iinclude` and
`_Static_assert`s for all six offsets, the button/state constants, and `sizeof(void *) == 4`:
all passed without producing an object or building the app. This source/target-compiler proof is
separate from the host SDL queue test.

Equivalent manual firmware-compatibility review found no new symbol, `extern`, link directive,
`dynlib!` declaration, library candidate, calling convention or `DT_NEEDED` change. The existing
`SDL_PushEvent` signature is unchanged. This is a loader-seam review; native gesture and pause
behavior still require the device protocol above.
