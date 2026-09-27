#!/usr/bin/env python3
"""Build and verify the styled ASS fixture outside the checkout.

Requires host ffmpeg, ffprobe and mkvmerge (MKVToolNix). The two embedded tracks
have distinct styles at the same timestamps; the first also lives beside the
video as a sidecar. Fonts and full timed events are muxed by mkvmerge after A/V
encoding: FFmpeg's shortest/sparse-subtitle scheduling can silently omit these
long, overlapping events or park them at EOF.

Use --source-video to repeat an existing A/V clip to the script's duration (for
example, the 4K60 performance fixture) without re-encoding it. Every output is
checked for complete events, timestamps, attachment metadata and interleaving.
The PLX73 attachment is a renamed bold face; its otherwise regular-style caption
looks different from fallback. styled-ass-no-font.mkv contains the identical
tracks without attachments, so comparing the caption proves font selection.
Serve with tests/mock_pms.py --extra-media <output>/styled-ass.mkv.
"""
import argparse
from collections import Counter
from decimal import Decimal
import json
import pathlib
import shutil
import struct
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
# Matroska audio lacing and B-frame reordering can place a few future A/V packets
# ahead of a subtitle. Half a second allows ordinary muxing lookahead, while
# rejecting cues that will not be demuxed until many seconds after their start.
MAX_INTERLEAVE_LAG_MS = 500
FONT_NAMES = {"Inter.ttf", "PLX73.ttf"}


def font_checksum(data):
    padded = data + bytes((-len(data)) % 4)
    return sum(word[0] for word in struct.iter_unpack(">I", padded)) & 0xFFFFFFFF


def fixture_font():
    """Rename the shipped bold face without changing SFNT table sizes.

    Mirrors player/ass/tests.rs:fixture_font. A unique family makes attachment
    selection visible, while using the bundled font avoids another dependency.
    """
    font = bytearray((ROOT / "pkg/appfont-bold.ttf").read_bytes())
    count = struct.unpack_from(">H", font, 4)[0]
    records = {bytes(font[at:at + 4]): at for at in range(12, 12 + count * 16, 16)}
    name_record, head_record = records[b"name"], records[b"head"]
    name_start, name_length = struct.unpack_from(">II", font, name_record + 8)
    head_start, head_length = struct.unpack_from(">II", font, head_record + 8)
    names = font[name_start:name_start + name_length]
    renamed = names.replace(b"Inter", b"PLX73").replace(
        "Inter".encode("utf-16-be"), "PLX73".encode("utf-16-be")
    )
    if renamed == names or len(renamed) != name_length:
        raise ValueError("bundled font no longer supports the fixed-size PLX73 family rename")
    font[name_start:name_start + name_length] = renamed
    struct.pack_into(">I", font, head_start + 8, 0)
    for record, start, length in ((name_record, name_start, name_length),
                                   (head_record, head_start, head_length)):
        struct.pack_into(">I", font, record + 4, font_checksum(font[start:start + length]))
    adjustment = (0xB1B0AFBA - font_checksum(font)) & 0xFFFFFFFF
    struct.pack_into(">I", font, head_start + 8, adjustment)
    if font_checksum(font) != 0xB1B0AFBA:
        raise ValueError("renamed font has an invalid SFNT checksum")
    return bytes(font)


def ass_time_ms(value):
    hours, minutes, seconds = value.strip().split(":")
    return (int(hours) * 3600 + int(minutes) * 60) * 1000 + int(
        Decimal(seconds) * 1000
    )


def script_events(script):
    events = []
    for line in script.splitlines():
        if line.startswith("Dialogue:"):
            fields = line.split(",", 9)
            start, end = ass_time_ms(fields[1]), ass_time_ms(fields[2])
            events.append((start, end - start))
    if not events:
        raise ValueError("ASS script contains no dialogue events")
    return events


def packet_ms(packet, field):
    return int(Decimal(packet[field]) * 1000)


def verify_fixture(path, script, *, fonts=True):
    """Reject empty streams, lost timings and subtitles parked behind later A/V."""
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_packets", "-show_streams",
        "-show_entries",
        "packet=stream_index,pts_time,duration_time,pos:"
        "stream=index,codec_name,codec_type:stream_tags=language,title,filename,mimetype",
        "-of", "json", str(path),
    ]))
    streams = probe["streams"]
    subtitles = [s for s in streams if s["codec_type"] == "subtitle"]
    if len(subtitles) != 2 or any(s["codec_name"] != "ass" for s in subtitles):
        raise ValueError("fixture must contain exactly two ASS streams")
    attachments = [s for s in streams if s["codec_type"] == "attachment"]
    expected_fonts = FONT_NAMES if fonts else set()
    actual_fonts = {s.get("tags", {}).get("filename") for s in attachments}
    if actual_fonts != expected_fonts or len(attachments) != len(expected_fonts):
        raise ValueError(f"fixture fonts differ: expected {expected_fonts}, found {actual_fonts}")
    if any(s["codec_name"] != "ttf" or
           s.get("tags", {}).get("mimetype") != "application/x-truetype-font"
           for s in attachments):
        raise ValueError("fixture font attachment metadata is incorrect")
    av_streams = {s["index"] for s in streams if s["codec_type"] in ("video", "audio")}
    expected = Counter(script_events(script))
    report = {"path": str(path), "fonts": sorted(actual_fonts), "tracks": []}
    for stream, language, title in zip(subtitles, ("eng", "jpn"), ("Styled", "Second")):
        tags = stream.get("tags", {})
        if tags.get("language") != language or tags.get("title") != title:
            raise ValueError(f"subtitle stream {stream['index']} has incorrect track metadata")
        packets = [p for p in probe["packets"] if p["stream_index"] == stream["index"]]
        actual = Counter((packet_ms(p, "pts_time"), packet_ms(p, "duration_time")) for p in packets)
        if actual != expected:
            raise ValueError(
                f"subtitle stream {stream['index']} has incomplete or mistimed events: "
                f"expected {sum(expected.values())}, found {sum(actual.values())}; "
                f"missing {dict(expected - actual)}, unexpected {dict(actual - expected)}"
            )
        report["tracks"].append({"index": stream["index"], "language": language,
                                 "packets": []})
    tracks = {t["index"]: t for t in report["tracks"]}
    av_time_ms = -1
    # ffprobe emits demux order; positions additionally expose late physical
    # placement in the saved report instead of inferring it from PTS alone.
    for packet in probe["packets"]:
        if packet["stream_index"] in av_streams:
            if "pts_time" in packet:
                av_time_ms = max(av_time_ms, packet_ms(packet, "pts_time"))
        elif packet["stream_index"] in tracks:
            start = packet_ms(packet, "pts_time")
            lag = max(0, av_time_ms - start)
            if lag > MAX_INTERLEAVE_LAG_MS:
                raise ValueError(
                    f"subtitle stream {packet['stream_index']} event at {start}ms "
                    f"is behind A/V at {av_time_ms}ms (file position {packet['pos']})"
                )
            tracks[packet["stream_index"]]["packets"].append({
                "start_ms": start, "duration_ms": packet_ms(packet, "duration_time"),
                "file_position": int(packet["pos"]), "av_ahead_ms": lag,
            })
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    parser.add_argument(
        "--source-video", type=pathlib.Path,
        help="repeat an existing A/V clip to the script duration without re-encoding",
    )
    args = parser.parse_args()
    output = args.output.resolve()
    if output == ROOT or ROOT in output.parents:
        parser.error("generated media belongs outside the checkout")
    for tool in ("ffmpeg", "ffprobe", "mkvmerge"):
        if shutil.which(tool) is None:
            parser.error(f"required host tool not found: {tool}")
    output.mkdir(parents=True, exist_ok=True)
    script = (ROOT / "tests/fixtures/styled-ass.ass").read_text()
    duration = max(start + length for start, length in script_events(script)) / 1000
    first = output / "styled-ass.ass"
    second = output / "second-track.ass"
    first.write_text(script)
    second.write_text(script.replace("GREEN TOP LEFT", "SECOND TRACK")
                      .replace("&H0000FF00", "&H00FF00FF"))
    unique_font = output / "PLX73.ttf"
    unique_font.write_bytes(fixture_font())
    with tempfile.TemporaryDirectory(prefix="ass-mux-", dir=output) as scratch:
        av = pathlib.Path(scratch) / "av.mkv"
        command = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
        if args.source_video:
            command += ["-stream_loop", "-1", "-i", str(args.source_video.resolve()),
                        "-map", "0:v:0", "-map", "0:a:0?", "-c", "copy"]
        else:
            command += [
                "-f", "lavfi", "-i", f"color=c=0x202020:s=1920x1080:r=24:d={duration}",
                "-f", "lavfi", "-i", f"anullsrc=r=48000:cl=stereo:d={duration}",
                "-map", "0:v", "-map", "1:a", "-c:v", "libx264",
                "-preset", "ultrafast", "-crf", "24", "-c:a", "ac3", "-b:a", "192k",
            ]
        subprocess.run(command + ["-t", str(duration), str(av)], check=True)
        candidate = pathlib.Path(scratch) / "styled-ass.mkv"
        subprocess.run([
            "mkvmerge", "-q", "-o", str(candidate), str(av),
            "--language", "0:eng", "--track-name", "0:Styled",
            "--default-track-flag", "0:yes", str(first),
            "--language", "0:jpn", "--track-name", "0:Second",
            "--default-track-flag", "0:no", str(second),
            "--attachment-mime-type", "application/x-truetype-font",
            "--attachment-name", "Inter.ttf", "--attach-file", str(ROOT / "pkg/appfont.ttf"),
            "--attachment-mime-type", "application/x-truetype-font",
            "--attachment-name", "PLX73.ttf", "--attach-file", str(unique_font),
        ], check=True)
        report = verify_fixture(candidate, script)
        no_font = pathlib.Path(scratch) / "styled-ass-no-font.mkv"
        subprocess.run([
            "mkvmerge", "-q", "-o", str(no_font), "--no-attachments", str(candidate),
        ], check=True)
        no_font_report = verify_fixture(no_font, script, fonts=False)
        target = output / "styled-ass.mkv"
        candidate.replace(target)
        report["path"] = str(target)
        no_font_target = output / "styled-ass-no-font.mkv"
        no_font.replace(no_font_target)
        no_font_report["path"] = str(no_font_target)
    (output / "styled-ass.verify.json").write_text(json.dumps(report, indent=2) + "\n")
    (output / "styled-ass-no-font.verify.json").write_text(
        json.dumps(no_font_report, indent=2) + "\n"
    )
    for track in report["tracks"]:
        lag = max(p["av_ahead_ms"] for p in track["packets"])
        print(f"Verified {track['language']}: {len(track['packets'])} ASS packets, "
              f"maximum A/V interleave lead {lag}ms")
    print(f"Created {target}")
    print(f"Created {no_font_target} without font attachments")


if __name__ == "__main__":
    main()
