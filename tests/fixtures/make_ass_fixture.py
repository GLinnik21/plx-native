#!/usr/bin/env python3
"""Build the styled ASS device fixture outside the checkout (requires host ffmpeg).

The two embedded tracks differ at the same timestamps; the first also lives beside
the video as a sidecar. The attached font, vector sign, overlapping dialogue,
movement and karaoke all survive a round trip through an ordinary Matroska file.
Serve with tests/mock_pms.py --extra-media <output>/styled-ass.mkv.
"""
import argparse
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output.resolve()
    if output == ROOT or ROOT in output.parents:
        parser.error("generated media belongs outside the checkout")
    output.mkdir(parents=True, exist_ok=True)
    script = (ROOT / "tests/fixtures/styled-ass.ass").read_text()
    first = output / "styled-ass.ass"
    second = output / "second-track.ass"
    first.write_text(script)
    second.write_text(script.replace("GREEN TOP LEFT", "SECOND TRACK")
                      .replace("&H0000FF00", "&H00FF00FF"))
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", "color=c=0x202020:s=1920x1080:r=24:d=120",
        "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
        "-i", str(first), "-i", str(second),
        "-map", "0:v", "-map", "1:a", "-map", "2:s", "-map", "3:s",
        "-c:v", "libx264", "-preset", "ultrafast", "-crf", "24",
        "-c:a", "ac3", "-b:a", "192k", "-c:s", "copy",
        "-metadata:s:s:0", "language=eng", "-metadata:s:s:0", "title=Styled",
        "-metadata:s:s:1", "language=jpn", "-metadata:s:s:1", "title=Second",
        "-disposition:s:0", "default", "-disposition:s:1", "0",
        "-attach", str(ROOT / "pkg/appfont.ttf"),
        "-metadata:s:t:0", "mimetype=application/x-truetype-font",
        "-metadata:s:t:0", "filename=Inter.ttf", "-shortest",
        str(output / "styled-ass.mkv"),
    ], check=True)


if __name__ == "__main__":
    main()
