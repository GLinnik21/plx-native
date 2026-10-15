#!/usr/bin/env python3
"""Build text-subtitle stress fixtures outside the checkout: `make_text_stress.py <output-dir>`.

The fixtures plan PR 5 of `libass-text-subs` lists: every text codec the player renders through
libass (SubRip, WebVTT, mov_text) with the script cases of the ASS stress set, the overlap cases
the newest-wins clip (D10) exists for, and SRT/VTT sidecars.

This is its own script because `make_ass_stress.py` (the ASS-script stress set, on the
`ass-perf-probe` branch) is not on main yet. Its text pools are copied verbatim, so once it lands
the two can share them; the cases here are the SRT/VTT/mov_text twins of its `latin2 cjk2 thai2
deva2 arab2 rapid long`, with the same names and timing, so a result can be compared across the
ASS and the text path.

Embedded (MKV unless noted; 60 s of 1080p24 h264 still image + silent AAC, ONE subtitle track):
  srt-latin2 srt-cjk2 srt-thai2 srt-deva2 srt-arab2 srt-rapid srt-long
                 SubRip, a two-line cue every 2 s (rapid: every 0.1 s; long: 300 characters / 10 s)
  overlap-1frame        each cue runs one frame (42 ms) into the next
  overlap-nodur         cues whose packets carry NO duration (the player defaults the end to +4 s)
  overlap-samestart     two cues with the same start, different text
  overlap-topbottom     a `{\\an8}` sign for 10 s over bottom dialogue every 2 s (both must show)
  webvtt                WebVTT in MKV
  movtext.mp4           mov_text in MP4
Sidecars: `tests/mock_pms.py --extra-media <video>` exposes the sibling `<video stem>.srt` and
`.vtt` as external subtitle streams, so a sidecar needs a video beside it:
  sidecar-latin2.mkv + .srt + .vtt     sidecar-thai2.mkv + .srt
  sidecar-topbottom.mkv + .vtt         (the first cue, a sign, is placed at the top by `line:0`)
  sidecar-thai-tis620.mkv + .srt       the Thai cues as TIS-620 (cp874) bytes, not UTF-8: the charset
                                       step of the sidecar reader must pick them up
  The sources of the embedded cases above sit beside their containers under the same stem, so
  `--extra-media srt-latin2.mkv` also offers the same cues as an external stream.
Samples FFmpeg cannot mux into a container (no muxer or Matroska codec id for them), written
by hand and checked only to demux with the right codec name:
  sample.smi (SAMI)   sample.sub (MicroDVD)

Needs host ffmpeg and ffprobe only. Every output is checked: codec name, packet count equal to
the cue count, and (overlap-nodur) the absence of a packet duration.
"""
import argparse
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
DURATION = 60
FRAME = 1 / 24  # one frame of the 24 fps A/V

# The pools of make_ass_stress.py, copied verbatim.
LATIN = [
    "I never thought we would actually make it this far.",
    "We have to leave very early tomorrow morning.",
    "Could you tell me how to get to the train station?",
    "I am grateful to everyone who helped us along the way.",
    "The weather is lovely today, let us go for a walk.",
    "We should get home before the rain starts to fall.",
]
CJK = [
    "我从来没有想过我们真的能走到这一步",
    "明天早上我们必须很早就出发赶火车",
    "请问你能告诉我去火车站怎么走吗",
    "我非常感谢一路上帮助过我们的每一个人",
    "今天天气很好，很适合到公园里散散步",
    "我们应该在下雨之前赶紧回家去",
]
THAI = [
    "ฉันไม่เคยคิดเลยว่าเราจะมาถึงที่นี่ได้จริง ๆ",
    "พรุ่งนี้เช้าเราต้องออกเดินทางตั้งแต่ตีห้า",
    "คุณช่วยบอกทางไปสถานีรถไฟให้หน่อยได้ไหม",
    "ผมรู้สึกขอบคุณทุกคนที่คอยช่วยเหลือมาตลอด",
    "อากาศวันนี้ดีมาก เหมาะกับการไปเดินเล่น",
    "เราควรกลับไปที่บ้านก่อนที่ฝนจะตกลงมา",
]
DEVA = [
    "मुझे नहीं लगा था कि हम यहाँ तक पहुँच पाएँगे",
    "कल सुबह हमें बहुत जल्दी निकलना होगा",
    "क्या आप मुझे रेलवे स्टेशन का रास्ता बताएँगे",
    "मैं उन सभी का आभारी हूँ जिन्होंने मदद की",
    "आज मौसम बहुत अच्छा है, टहलने चलते हैं",
    "बारिश शुरू होने से पहले हमें घर लौटना चाहिए",
]
ARAB = [
    "لم أكن أتخيل أننا سنصل إلى هنا يوما ما",
    "علينا أن نغادر مبكرا صباح الغد من فضلك",
    "هل يمكنك أن تدلني على طريق محطة القطار",
    "أنا ممتن لكل من ساعدنا طوال هذه الرحلة",
    "الطقس جميل اليوم ويناسب المشي في الحديقة",
    "يجب أن نعود إلى البيت قبل أن يبدأ المطر",
]

# A cue is (start_s, end_s, text); the text carries real line breaks.


def two_line(pool, step, offset=0):
    out = []
    for i in range(int(DURATION / step)):
        a, b = pool[(2 * i + offset) % len(pool)], pool[(2 * i + 1 + offset) % len(pool)]
        out.append((i * step, (i + 1) * step, f"{a}\n{b}"))
    return out


def rapid_cues():
    return [(i / 10, (i + 1) / 10, f"{i:04d} {LATIN[i % len(LATIN)][:34]}\n{CJK[i % len(CJK)]}")
            for i in range(DURATION * 10)]


def long_cues():
    """Six 10 s cues of exactly 300 Latin characters each, rotated so no two match."""
    words = " ".join(LATIN * 6).split()
    out = []
    for i in range(6):
        line = " ".join(words[i * 7:] + words)[:300].rstrip()
        line += "." * (300 - len(line))
        out.append((i * 10, (i + 1) * 10, line))
    return out


def overlap_1frame():
    return [(i * 2, i * 2 + 2 + FRAME, f"Cue {i:02d} runs a frame into the next\n{LATIN[i % len(LATIN)]}")
            for i in range(DURATION // 2)]


def overlap_nodur():
    # start == end: FFmpeg's matroska muxer writes such a packet without a BlockDuration.
    return [(i * 3, i * 3, f"Cue {i:02d} has no duration\n{LATIN[i % len(LATIN)]}")
            for i in range(DURATION // 3)]


def overlap_samestart():
    out = []
    for i in range(DURATION // 4):
        out.append((i * 4, i * 4 + 3, f"Cue {i:02d} OLD: this one must never show"))
        out.append((i * 4, i * 4 + 2, f"Cue {i:02d} NEW: the newer cue wins"))
    return out


def topbottom_cues(top_tag):
    out = [(0, 10, f"{top_tag}SIGN: a top line that outlasts the dialogue")]
    out += [(i * 2, i * 2 + 2, LATIN[i % len(LATIN)]) for i in range(1, 5)]
    return out


def srt_time(seconds, sep=","):
    ms = round(seconds * 1000)
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d}{sep}{ms % 1000:03d}"


def write_srt(path, cues, encoding="utf-8"):
    path.write_text("".join(
        f"{n}\n{srt_time(a)} --> {srt_time(b)}\n{t}\n\n" for n, (a, b, t) in enumerate(cues, 1)
    ), encoding=encoding)


def write_vtt(path, cues, settings=None):
    settings = settings or {}
    path.write_text("WEBVTT\n\n" + "".join(
        f"{srt_time(a, '.')} --> {srt_time(b, '.')}{settings.get(n, '')}\n{t}\n\n"
        for n, (a, b, t) in enumerate(cues)
    ), encoding="utf-8")


def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"{' '.join(map(str, cmd[:3]))} failed:\n{r.stderr[-2000:]}")
    return r.stdout


def encode_av(out):
    path = out / "av-1080p24.mkv"
    if not path.exists():
        run(["ffmpeg", "-v", "error", "-y",
             "-f", "lavfi", "-i", "color=c=0x2a3340:s=1920x1080:r=24",
             "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
             "-t", str(DURATION), "-c:v", "libx264", "-preset", "veryfast", "-tune", "stillimage",
             "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "64k", str(path)])
    return path


def mux(out, av, name, sub_file, codec, ext="mkv"):
    target = out / f"{name}.{ext}"
    run(["ffmpeg", "-v", "error", "-y", "-i", str(av), "-i", str(sub_file),
         "-map", "0:v", "-map", "0:a", "-map", "1:s", "-c", "copy", "-c:s", codec,
         "-disposition:s:0", "default", "-t", str(DURATION), str(target)])
    return target


def probe_subtitle(path):
    """(codec name, [packet duration or None, ...]) of the one subtitle stream."""
    info = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(path)]))
    subs = [s for s in info["streams"] if s["codec_type"] == "subtitle"]
    assert len(subs) == 1, f"{path.name}: {len(subs)} subtitle streams"
    packets = json.loads(run(["ffprobe", "-v", "error", "-select_streams", str(subs[0]["index"]),
                              "-show_entries", "packet=duration_time", "-of", "json", str(path)]))
    return subs[0]["codec_name"], [p.get("duration_time") for p in packets["packets"]]


def probe_streams(path):
    info = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(path)]))
    return {s["codec_type"] for s in info["streams"]}


def verify(path, codec, cues, nodur=False):
    got_codec, durations = probe_subtitle(path)
    assert got_codec == codec, f"{path.name}: codec {got_codec} != {codec}"
    assert len(durations) == len(cues), f"{path.name}: {len(durations)} packets != {len(cues)} cues"
    if nodur:
        assert all(d is None for d in durations), f"{path.name}: a packet kept a duration"
    return len(durations)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("output", type=pathlib.Path, help="directory for scripts and containers")
    out = ap.parse_args().output.resolve()
    if ROOT in out.parents or out == ROOT:
        sys.exit("refusing to write into the repository checkout")
    out.mkdir(parents=True, exist_ok=True)
    av = encode_av(out)

    embedded = [  # name, cues, source writer, container codec, container ext, expects no durations
        ("srt-latin2", two_line(LATIN, 2), write_srt, "srt", "mkv"),
        ("srt-cjk2", two_line(CJK, 2), write_srt, "srt", "mkv"),
        ("srt-thai2", two_line(THAI, 2), write_srt, "srt", "mkv"),
        ("srt-deva2", two_line(DEVA, 2), write_srt, "srt", "mkv"),
        ("srt-arab2", two_line(ARAB, 2), write_srt, "srt", "mkv"),
        ("srt-rapid", rapid_cues(), write_srt, "srt", "mkv"),
        ("srt-long", long_cues(), write_srt, "srt", "mkv"),
        ("overlap-1frame", overlap_1frame(), write_srt, "srt", "mkv"),
        ("overlap-nodur", overlap_nodur(), write_srt, "srt", "mkv"),
        ("overlap-samestart", overlap_samestart(), write_srt, "srt", "mkv"),
        ("overlap-topbottom", topbottom_cues("{\\an8}"), write_srt, "srt", "mkv"),
        ("webvtt", two_line(LATIN, 2), write_vtt, "webvtt", "mkv"),
        ("movtext", two_line(LATIN, 2), write_srt, "mov_text", "mp4"),
    ]
    rows = []
    for name, cues, writer, codec, ext in embedded:
        source = out / f"{name}.{'vtt' if writer is write_vtt else 'srt'}"
        writer(source, cues)
        container = mux(out, av, name, source, codec, ext)
        # FFmpeg names the Matroska codec `subrip`; the others keep their own name.
        want = {"srt": "subrip"}.get(codec, codec)
        n = verify(container, want, cues, nodur=name == "overlap-nodur")
        rows.append((container.name, want, n))

    sidecars = [  # video stem, [(extension, cues, writer, vtt settings)]
        ("sidecar-latin2", [("srt", two_line(LATIN, 2), write_srt, None),
                            ("vtt", two_line(LATIN, 2), write_vtt, None)]),
        ("sidecar-thai2", [("srt", two_line(THAI, 2), write_srt, None)]),
        ("sidecar-topbottom", [("vtt", topbottom_cues(""), write_vtt, {0: " line:0 align:center"})]),
        ("sidecar-thai-tis620", [("srt", two_line(THAI, 2), write_srt, "cp874")]),
    ]
    for stem, files in sidecars:
        video = out / f"{stem}.mkv"
        run(["ffmpeg", "-v", "error", "-y", "-i", str(av), "-map", "0:v", "-map", "0:a",
             "-c", "copy", "-t", str(DURATION), str(video)])
        assert probe_streams(video) == {"video", "audio"}, f"{video.name}: expected no subtitle"
        rows.append((video.name, "-", 0))
        for extension, cues, writer, settings in files:  # settings: VTT cue settings, or an SRT charset
            path = out / f"{stem}.{extension}"
            if writer is write_vtt:
                writer(path, cues, settings)
            elif settings:  # SubRip: the extra field names the file's charset
                writer(path, cues, settings)
            else:
                writer(path, cues)
            codec, durations = probe_subtitle(path)
            assert len(durations) == len(cues), f"{path.name}: {len(durations)} != {len(cues)} cues"
            rows.append((path.name, codec, len(durations)))

    # Not containable: no muxer (SAMI) and no Matroska codec id (both). Hand-written, demux-checked.
    (out / "sample.smi").write_text(
        "<SAMI><BODY>\n<SYNC Start=1000><P Class=ENCC>Line one<br>Line two\n"
        "<SYNC Start=3000><P Class=ENCC>&nbsp;\n<SYNC Start=4000><P Class=ENCC>"
        "<font color=red>Second</font> &amp; third\n<SYNC Start=6000><P Class=ENCC>&nbsp;\n"
        "</BODY></SAMI>\n", encoding="utf-8")
    (out / "sample.sub").write_text(
        "{1}{1}24.000\n{24}{72}{y:i}Italic|plain\n{96}{120}Second line|with a pipe\n", encoding="utf-8")
    for name, codec, want in (("sample.smi", "sami", 4), ("sample.sub", "microdvd", 2)):
        got, durations = probe_subtitle(out / name)
        assert got == codec and len(durations) == want, f"{name}: {got} {len(durations)}"
        rows.append((name, got, len(durations)))

    print(f"{'file':<22}{'codec':<10}{'packets':>8}")
    for name, codec, n in rows:
        print(f"{name:<22}{codec:<10}{n:>8}")
    print(f"\noutput: {out}")


if __name__ == "__main__":
    main()
