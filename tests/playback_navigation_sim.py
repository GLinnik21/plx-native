#!/usr/bin/env python3
"""Exercise playback navigation against synthetic Plex media, using the shared simulator driver.

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


def verify(binary, media, output, instant_dpad=False):
    output.mkdir(parents=True, exist_ok=False)
    (output / "auth.json").write_text(json.dumps({"instant_dpad_seeking": "on" if instant_dpad else "off", "trailer_autoplay": False}))
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
        # A metadata descriptor can arrive before the resolve installs the item's queue/kind.
        # Wait for that item's actual Playing heartbeat before exercising its queue controls.
        playing_at = sim.latest(lambda _, line: line.startswith("playing item: "))[0]
        wait(r"loop=.*route=player .*pos=\d+s", playing_at)
        # A hidden-HUD reveal can produce the same fingerprint as the previous visible frame.
        # A reveal may therefore need a second UP. Await each actual focus transition rather
        # than assuming a fixed render delay, especially while the host is also building.
        for _ in range(3):
            stamp = sim.key("up")
            wait(r"key type=0x301", stamp)
            if sim.wait_for(r"^focus route=player .*hud=1 f=1 (?:btn=1 .*pair=1 .*next=1|btn=0 .*pair=1 previous=1 next=0)", 1, after=stamp):
                return
        latest = sim.latest(lambda _, line: line.startswith("focus "))[1]
        raise AssertionError(f"control row not reached: {latest}")

    try:
        wait(r"boot: dev token")
        wait(rf"playing item: rk={mock_pms.V1_RATING_KEY} ")
        wait(r"^focus route=player .*slot=discs items=5")
        control_row()
        stamp = sim.key("ok")
        wait(rf"queue transport: Next rk={mock_pms.V3_RATING_KEY}", stamp)
        wait(rf"playing item: rk={mock_pms.V3_RATING_KEY} ", stamp)
        queue_posts = [path for path, _ in pms.requests if path.split("?")[0] == "/playQueues"]
        assert len(queue_posts) == 1, f"Next replaced the queue and lost its preceding rows: {queue_posts}"
        windows = [path for path, _ in pms.requests if path.startswith("/playQueues/1?")]
        assert any("center=2" in path and "own=0" in path for path in windows), windows
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
        stamp = sim.key("left")
        wait(r"^focus route=player .*f=1 btn=0", stamp)
        stamp = sim.key("ok")
        wait(r"seek\(in-place\): av_seek t=0 ", stamp)
        wait(r"loop=.*route=player .*pos=[12]s", stamp)
        print("PASS Previous restarts the current item after five seconds", flush=True)

        # UP from the control row explicitly dismisses the HUD. The focus probe uses the last
        # input time, so its timer-only sample cannot establish an automatic timeout by itself.
        stamp = sim.key("up")
        wait(r"^focus route=player .*hud=0 ", stamp)
        if not instant_dpad:
            stamp = sim.key("right")
            wait(r"^focus route=player .*hud=1 f=0", stamp)
            time.sleep(0.15)
            assert not any("seek(in-place): av_seek t=" in line for at, line in sim.snapshot() if at >= stamp)
            stamp = sim.key("back")
            wait(r"^focus route=player .*hud=0 ", stamp)
            print("PASS default hidden-HUD D-pad press reveals without seeking", flush=True)
        stamp = sim.key("right" if instant_dpad else "ff")
        wait(r"seek\(in-place\): av_seek t=[1-9][0-9]* ", stamp)
        wait(r"loop=.*route=player .*pos=1[1-9]s", stamp)
        flashes = [line for at, line in sim.snapshot() if at >= stamp
                   and line.startswith("focus route=player ") and "hud=1 " in line]
        assert not flashes, f"hidden seeking flashed the HUD during reprime: {flashes}"
        latest = sim.latest(lambda _, line: line.startswith("focus "))[1]
        assert "route=player " in latest and "hud=0 " in latest, latest
        stamp = sim.key("shot")
        wait(r"shot: wrote ", stamp)
        print("PASS hidden-HUD seek stays hidden through the first new picture", flush=True)

        stamp = sim.key("up")
        wait(r"^focus route=player .*hud=1 f=0", stamp)
        stamp = sim.key("right")
        time.sleep(0.3)  # allow the input frame; only OK may emit the next seek
        assert not any(re.search(r"seek\(in-place\):", line) for at, line in sim.snapshot() if at >= stamp)
        stamp = sim.key("ok")
        wait(r"seek\(in-place\): av_seek t=", stamp)
        print("PASS a visible-HUD seek waits for OK", flush=True)

        # Use the shared split-edge remote protocol: a held transport key must not send the
        # tap's jump first, or commit on release. Only OK takes the continuous preview.
        stamp = sim.key("holddown:ff")
        time.sleep(0.5)
        for _ in range(4):
            sim.key("holdrep:ff")
            time.sleep(0.08)
        sim.key("holdup:ff")
        wait(r"scrub: hold over .*pending", stamp)
        assert not any("seek(in-place): av_seek t=" in line for at, line in sim.snapshot() if at >= stamp)
        stamp = sim.key("ok")
        wait(r"seek\(in-place\): av_seek t=", stamp)
        print("PASS held FF previews without an initial jump and waits for OK", flush=True)
    finally:
        sim.close()
        server.shutdown()
        server.server_close()



def verify_rapid(binary, media, output):
    """Use a clip with keyframes more than one skip interval apart (e.g. 12s vs 10s)."""
    output.mkdir(parents=True, exist_ok=False)
    (output / "auth.json").write_text(json.dumps({"trailer_autoplay": False}))
    server, pms = mock_pms.serve(0, media=media, media_next_episode=True)
    server.media_kbit = 20_000  # stay ahead of playback without reading the whole clip to EOF
    port = server.server_address[1]
    url = f"http://127.0.0.1:{port}"
    sim = SimRun(binary.resolve(), port, {
        "plxnative-plextv": url, "plxnative-discover": url, "plxnative-clocksink": "",
        "plxnative-noidle": "", "plxnative-focus": "", "plxnative-play": str(mock_pms.V1_RATING_KEY),
    }, output)

    def wait(pattern, after=0):
        stamp = sim.wait_for(pattern, 20, after=after)
        assert stamp is not None, f"no {pattern!r}; evidence: {output}"
        return stamp

    try:
        wait("boot: dev token")
        wait(r"loop=.*route=player .*pos=[23]s")
        stamp = sim.key("up")
        wait(r"^focus route=player .*hud=1 f=1", stamp)
        stamp = sim.key("up")
        wait(r"^focus route=player .*hud=0 ", stamp)
        targets = []
        landings = []
        presses = []
        for _ in range(3):
            stamp = sim.key("ff")
            presses.append(stamp)
            if len(presses) > 1:
                assert presses[-1] - presses[-2] < 1.5, "host too slow to exercise one tap burst"
            wait(r"seek\(in-place\): av_seek t=", stamp)
            line = sim.latest(lambda at, line: at >= stamp and "seek(in-place): av_seek t=" in line)[1]
            targets.append(int(re.search(r"av_seek t=(\d+)", line)[1]))
            wait(r"in-place seek\(native\): Seek\(", stamp)
            line = sim.latest(lambda at, line: at >= stamp and "in-place seek(native): Seek(" in line)[1]
            landings.append(int(re.search(r"Seek\((\d+)\)", line)[1]))
            # A heartbeat with pos= is published only once playback is Playing again, so the
            # next tap exercises an actual earlier landing, not merely an in-flight target.
            wait(r"loop=.*route=player .*pos=\d+s", stamp)
        assert [b - a for a, b in zip(targets, targets[1:])] == [10_000_000_000] * 2, targets
        assert any(a == b for a, b in zip(landings, landings[1:])), (
            f"fixture did not produce a repeated keyframe: {landings}"
        )
        assert landings[-1] > landings[0], landings
        print(f"PASS rapid taps accumulate across repeated keyframes: targets={targets}, landings_ms={landings}", flush=True)
    finally:
        sim.close()
        server.shutdown()
        server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sim", type=Path, required=True)
    parser.add_argument("--media", type=Path, required=True)
    parser.add_argument("--out", type=Path, help="new directory for runtime, screenshots and logs")
    parser.add_argument("--instant-dpad", action="store_true", help="exercise the opt-in hidden-HUD D-pad path")
    parser.add_argument("--rapid-seeks", action="store_true", help="verify tap accumulation with a long-GOP fixture")
    args = parser.parse_args()
    def run(binary, media, output):
        if args.rapid_seeks:
            verify_rapid(binary, media, output)
        else:
            verify(binary, media, output, args.instant_dpad)
    if args.out:
        run(args.sim, args.media, args.out)
        print(f"Evidence: {args.out}")
    else:
        with tempfile.TemporaryDirectory(prefix="plx-playback-navigation-") as root:
            run(args.sim, args.media, Path(root) / "runtime")


if __name__ == "__main__":
    main()
