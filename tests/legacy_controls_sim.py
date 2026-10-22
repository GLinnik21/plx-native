#!/usr/bin/env python3
"""Exercise legacy transport against synthetic Plex media, using the shared simulator driver.

Generate media with tests/fixtures/make_fixtures.py --only mockverify --out <directory>, then
run this with --sim rust-modules/target-sim/debug/plxnative-sim --media <directory>.
No television, household credentials, or external Plex account is used.
"""
import argparse
import json
from pathlib import Path
import re
import tempfile
import time

import mock_pms
from paging_link import SimRun


def verify(binary, media, output):
    output.mkdir(parents=True, exist_ok=False)
    (output / "auth.json").write_text(json.dumps({"playback_controls": "legacy", "trailer_autoplay": False}))
    server, pms = mock_pms.serve(0, media=media, media_next_episode=True)
    port = server.server_address[1]
    stand_in = f"http://127.0.0.1:{port}"
    sim = SimRun(binary.resolve(), port, {
        "plxnative-plextv": stand_in, "plxnative-discover": stand_in,
        "plxnative-clocksink": "", "plxnative-noidle": "", "plxnative-focus": "",
        "plxnative-play": str(mock_pms.V1_RATING_KEY),
    }, output)

    def wait(pattern, after=0, timeout=25):
        stamp = sim.wait_for(pattern, timeout, after=after)
        if stamp is None:
            raise AssertionError(f"timed out waiting for {pattern!r}; evidence: {output}")
        return stamp

    def control_row():
        # A hidden-HUD reveal can produce the same fingerprint as the previous visible frame.
        # Await the actual key-up instead of requiring a deduplicated focus line to repeat.
        for _ in range(3):
            stamp = sim.key("up")
            wait(r"key type=0x301", stamp)
            time.sleep(0.15)  # focus is published after the input batch, after the key-up log
            latest = sim.latest(lambda _, line: line.startswith("focus "))[1]
            if "hud=1 f=1 " in latest:
                assert "slot=legacy items=5" in latest
                return
        raise AssertionError(f"control row not reached: {latest}")

    try:
        wait(r"boot: dev token")
        wait(rf"playing item: rk={mock_pms.V1_RATING_KEY} ")
        wait(r"^focus route=player .*slot=legacy items=5")
        control_row()
        stamp = sim.key("right")
        wait(r"^focus route=player .*f=1 btn=1", stamp)
        stamp = sim.key("ok")
        wait(rf"queue transport: Next rk={mock_pms.V3_RATING_KEY}", stamp)
        wait(rf"playing item: rk={mock_pms.V3_RATING_KEY} ", stamp)
        queue_posts = [path for path, _ in pms.requests if path.split("?")[0] == "/playQueues"]
        assert len(queue_posts) == 1, f"Next replaced the queue and lost its preceding rows: {queue_posts}"
        control_row()
        stamp = sim.key("ok")
        wait(rf"queue transport: Previous rk={mock_pms.V1_RATING_KEY}", stamp)
        wait(rf"playing item: rk={mock_pms.V1_RATING_KEY} ", stamp)
        print("PASS Next followed by Previous retains the preceding episode", flush=True)

        # The common handoff must retain the original queue, not POST a new tail at each item.
        queue_posts = [path for path, _ in pms.requests if path.split("?")[0] == "/playQueues"]
        assert len(queue_posts) == 1, f"queue replaced during item navigation: {queue_posts}"

        wait(r"loop=.*route=player .*pos=(?:[5-9]|[1-9][0-9]+)s", stamp)
        control_row()
        stamp = sim.key("ok")
        wait(r"seek\(in-place\): av_seek t=0 ", stamp)
        wait(r"loop=.*route=player .*pos=[12]s", stamp)
        print("PASS Previous restarts the current item after five seconds", flush=True)

        # UP from the control row explicitly dismisses the HUD. The focus probe uses the last
        # input time, so its timer-only sample cannot establish an automatic timeout by itself.
        stamp = sim.key("up")
        wait(r"^focus route=player .*hud=0 ", stamp)
        stamp = sim.key("right")
        wait(r"seek\(in-place\): av_seek t=[1-9][0-9]* ", stamp)
        wait(r"loop=.*route=player .*pos=1[1-9]s", stamp)
        latest = sim.latest(lambda _, line: line.startswith("focus "))[1]
        assert "route=player " in latest and "hud=0 " in latest, latest
        stamp = sim.key("shot")
        wait(r"shot: wrote ", stamp)
        print("PASS hidden-HUD seek commits immediately and leaves the HUD dismissed", flush=True)

        stamp = sim.key("up")
        wait(r"^focus route=player .*hud=1 f=0", stamp)
        stamp = sim.key("right")
        time.sleep(0.3)  # allow the input frame; only OK may emit the next seek
        assert not any(re.search(r"seek\(in-place\):", line) for at, line in sim.snapshot() if at >= stamp)
        stamp = sim.key("ok")
        wait(r"seek\(in-place\): av_seek t=", stamp)
        print("PASS a visible-HUD seek waits for OK", flush=True)
    finally:
        sim.close()
        server.shutdown()
        server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sim", type=Path, required=True)
    parser.add_argument("--media", type=Path, required=True)
    parser.add_argument("--out", type=Path, help="new directory for runtime, screenshots and logs")
    args = parser.parse_args()
    if args.out:
        verify(args.sim, args.media, args.out)
        print(f"Evidence: {args.out}")
    else:
        with tempfile.TemporaryDirectory(prefix="plx-legacy-controls-") as root:
            verify(args.sim, args.media, Path(root) / "runtime")


if __name__ == "__main__":
    main()
