#!/usr/bin/env python3
"""Build the terminated ASS header and packet fixture with generated captions."""
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
HEADER = ROOT / "ass-terminated-header.bin"
OUTPUT = ROOT / "ass-terminated-header.mkv"


def vint_width(data, pos):
    if pos >= len(data):
        raise ValueError("Truncated Matroska element")
    for width in range(1, 9):
        if data[pos] & (0x80 >> (width - 1)):
            if pos + width > len(data):
                raise ValueError("Truncated Matroska element")
            return width
    raise ValueError("Invalid Matroska element length")


def elements(data):
    pos = 0
    while pos < len(data):
        start = pos
        width = vint_width(data, pos)
        ident = data[pos:pos + width]
        pos += width
        width = vint_width(data, pos)
        size = int.from_bytes(data[pos:pos + width], "big") & ((1 << (7 * width)) - 1)
        pos += width
        if size == (1 << (7 * width)) - 1:
            size = len(data) - pos
        end = pos + size
        if end > len(data):
            raise ValueError("Truncated Matroska element")
        yield ident, data[pos:end], data[start:end]
        pos = end


def element(ident, body):
    width = next(n for n in range(1, 9) if len(body) < (1 << (7 * n)) - 1)
    size = ((1 << (7 * width)) | len(body)).to_bytes(width, "big")
    return ident + size + body


def replace_headers(data, header):
    tracks = []
    ordinal = 0
    terminated_track = None
    for ident, body, raw in elements(data):
        if ident == bytes.fromhex("bf"):
            continue
        if ident != bytes.fromhex("ae"):
            tracks.append(raw)
            continue
        children = list(elements(body))
        if not any(i == bytes.fromhex("86") and b == b"S_TEXT/ASS" for i, b, _ in children):
            tracks.append(raw)
            continue
        private = header if ordinal == 0 else header.rstrip(b"\0")
        if ordinal == 0:
            numbers = [int.from_bytes(b, "big") for i, b, _ in children if i == b"\xd7"]
            if len(numbers) != 1 or numbers[0] <= 0:
                raise ValueError("Expected one positive ASS track number")
            terminated_track = numbers[0]
        tracks.append(element(ident, b"".join(
            element(i, private) if i == bytes.fromhex("63a2") else r
            for i, _, r in children if i != bytes.fromhex("bf")
        )))
        ordinal += 1
    if ordinal != 2:
        raise ValueError("Expected two ASS tracks")
    return b"".join(tracks), terminated_track


def terminate_packets(data, track):
    output = []
    changed = 0
    for ident, body, raw in elements(data):
        if ident == bytes.fromhex("bf"):
            continue
        if ident == bytes.fromhex("a0"):
            blocks, count = terminate_packets(body, track)
            output.append(element(ident, blocks))
            changed += count
        elif ident in (bytes.fromhex("a1"), bytes.fromhex("a3")):
            width = vint_width(body, 0)
            if len(body) < width + 3:
                raise ValueError("Truncated Matroska block")
            number = int.from_bytes(body[:width], "big") & ((1 << (7 * width)) - 1)
            if number == track:
                output.append(element(ident, body + b"\0"))
                changed += 1
            else:
                output.append(raw)
        else:
            output.append(raw)
    return b"".join(output), changed


def main():
    header = HEADER.read_bytes()
    if len(header) != 521 or header[-1:] != b"\0" or b"\0" in header[:-1]:
        raise ValueError("Expected the supplied 521-byte header")
    with tempfile.TemporaryDirectory(prefix="plx-ass-header-") as temp:
        temp = Path(temp)
        script = temp / "captions.ass"
        script.write_bytes(header[:-1] + (
            "\r\n[Events]\r\n"
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\r\n"
            "Dialogue: 0,0:00:01.73,0:00:03.73,Default,,0,0,0,,FIRST TEST CAPTION\r\n"
            "Dialogue: 0,0:00:05.00,0:00:07.00,Default,,0,0,0,,SECOND TEST CAPTION\r\n"
        ).encode())
        source = temp / "source.mkv"
        subprocess.run([
            "ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i",
            "color=c=black:s=320x180:r=24:d=8", "-i", str(script),
            "-map", "0:v", "-map", "1:0", "-map", "1:0", "-c:v", "libx264",
            "-preset", "ultrafast", "-g", "24", "-pix_fmt", "yuv420p", "-c:s", "copy",
            "-fflags", "+bitexact", "-flags:v", "+bitexact", str(source),
        ], check=True)
        output = []
        changed = 0
        for ident, body, raw in elements(source.read_bytes()):
            if ident == bytes.fromhex("18538067"):
                segment = []
                children = list(elements(body))
                tracks = [b for i, b, _ in children if i == bytes.fromhex("1654ae6b")]
                if len(tracks) != 1:
                    raise ValueError("Expected one Matroska track inventory")
                headers, track = replace_headers(tracks[0], header)
                for i, b, r in children:
                    # These elements contain offsets or checksums of the original header.
                    if i in [bytes.fromhex(s) for s in ("114d9b74", "1c53bb6b", "bf")]:
                        continue
                    if i == bytes.fromhex("1654ae6b"):
                        segment.append(element(i, headers))
                    elif i == bytes.fromhex("1f43b675"):
                        packets, count = terminate_packets(b, track)
                        segment.append(element(i, packets))
                        changed += count
                    else:
                        segment.append(r)
                output.append(element(ident, b"".join(segment)))
            else:
                output.append(raw)
        if changed != 2:
            raise ValueError(f"Expected two terminated ASS packets, changed {changed}")
        OUTPUT.write_bytes(b"".join(output))
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-select_streams", "s", "-count_packets",
        "-show_entries", "stream=extradata_size,nb_read_packets", "-of", "json", str(OUTPUT),
    ]))
    if [(s["extradata_size"], s["nb_read_packets"]) for s in probe["streams"]] != [(521, "2"), (520, "2")]:
        raise ValueError("Expected two ASS tracks with the captured headers and two packets each")
    print(f"{OUTPUT.name}: terminated header and packets, unterminated control, two captions per track")


if __name__ == "__main__":
    main()
