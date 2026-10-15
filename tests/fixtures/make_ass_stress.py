#!/usr/bin/env python3
"""Build libass stress fixtures outside the checkout: `make_ass_stress.py <output-dir>`.

Each MKV is 60 s of 1080p24 h264 (still image) + silent AAC with ONE ASS track styled like the
app's SRT look (PlayRes 1920x1080, Bold, 36 pt, outline 2, no shadow, bottom centre, MarginV 100).
Scripted fonts are attached (mimetype font/ttf) so libass loads them as embedded fonts; Inter comes
from pkg/appfont-bold.ttf. Noto Sans Thai/Devanagari/Arabic are downloaded from the notofonts
GitHub releases, PINNED by URL and sha256 (OFL), into <output-dir>/fonts.

Fixtures: latin2 cjk2 thai2 deva2 arab2 rapid karaoke long, plus rapid-4k60 (3840x2160, 60 fps).
Serve one with `tests/mock_pms.py --extra-media <output-dir>/<name>.mkv` (docs/ass-subtitles.md)
and read the `assperf:` event-log lines (rust-modules/media/src/player/ass_perf.rs).

TEXT fixtures (the same A/V, ONE text track each, rendered by libass through the converter in
rust-modules/media/src/player/text_ass.rs; plan libass-text-subs section 7):

  srt-latin2 srt-cjk2 srt-thai2 srt-deva2 srt-arab2 srt-rapid srt-long   SRT (subrip) in MKV, the
                                  text of the ASS case of the same name
  srt-markup.mkv                  <i> <b> <u> <font color>, {\\an8} {\\an5} {\\an1} {\\an3}, three
                                  lines, a literal backslash and an unmatched brace
  srt-overlap.mkv                 a 1-frame overlap, two cues with one start, a top sign over bottom
                                  dialogue
  mov_text-mixed.mp4              mov_text in MP4: Latin, CJK, Thai, Devanagari and Arabic in turn
  webvtt-mixed.mkv                WebVTT in MKV, the same cues plus <v>/<c> tags, an entity and a
                                  `line:0` setting (embedded settings are ignored: plan Q4)

Every generated subtitle track (these and the ASS ones) carries the DEFAULT disposition, so the
player selects it without a menu pick; `verify` checks it with ffprobe, and checks that the markup
survived the mux by dumping the track's packets. SAMI (and MicroDVD, MPL2, RealText ...) cannot be
muxed: the bundled FFmpeg demuxes only matroska, mov and mpegts, and matroska/mp4/mpegts cannot
carry a sami codec id ("Subtitle codec sami is not supported" from the matroska muxer, "Could not
find tag for codec sami" from mp4). Those dialects are covered by the converter's host tests only.
Matroska blocks without a duration (the "no-duration packet" case, start + 4 s) cannot be written
by ffmpeg either: its muxer always writes BlockDuration for a subtitle packet.

Needs host ffmpeg and ffprobe only (no mkvmerge): the A/V is encoded once per resolution and every
fixture remuxes it with `-c copy`, so the subtitle track is never subject to the encoder's sparse
subtitle scheduling; the packet count is checked against the script's event count anyway.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import sys
import urllib.request
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
DURATION = 60

# (family, zip URL, sha256 of the zip, member to extract)
NOTO = {
    "Noto Sans Thai": (
        "https://github.com/notofonts/thai/releases/download/NotoSansThai-v2.002/NotoSansThai-v2.002.zip",
        "af889cc673fc714060ce5e4e088fbad32aa4c0571a19958efeaff128a22da485",
        "NotoSansThai/unhinted/ttf/NotoSansThai-Bold.ttf",
    ),
    "Noto Sans Devanagari": (
        "https://github.com/notofonts/devanagari/releases/download/NotoSansDevanagari-v2.007/NotoSansDevanagari-v2.007.zip",
        "820c7da45b1e63562cb41c0a8cac5d9a4202312043a3a040ed1325857ef469b1",
        "NotoSansDevanagari/unhinted/ttf/NotoSansDevanagari-Bold.ttf",
    ),
    "Noto Sans Arabic": (
        "https://github.com/notofonts/arabic/releases/download/NotoSansArabic-v2.013/NotoSansArabic-v2.013.zip",
        "1301aceaea84c501cf2e6dcfb3182e2328c8eae5725817fcb239672bda7154f1",
        "NotoSansArabic/unhinted/ttf/NotoSansArabic-Bold.ttf",
    ),
}

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
SYLLABLES = ("Twin kle twin kle lit tle star how I won der what you are "
             "up a bove the world so high").split()


def ts(seconds):
    cs = round(seconds * 100)
    return f"{cs // 360000}:{cs // 6000 % 60:02d}:{cs // 100 % 60:02d}.{cs % 100:02d}"


def script(font, events, wrap=None):
    """`events` are (start_s, end_s, text); the text carries ASS `\\N` line breaks itself."""
    head = ["[Script Info]", "ScriptType: v4.00+", "PlayResX: 1920", "PlayResY: 1080",
            "ScaledBorderAndShadow: yes"]
    if wrap is not None:
        head.append(f"WrapStyle: {wrap}")
    head += ["", "[V4+ Styles]",
             "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, "
             "BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, "
             "BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
             f"Style: Default,{font},36,&H00FFFFFF,&H0000FFFF,&H26000000,&H00000000,-1,0,0,0,"
             "100,100,0,0,1,2,0,2,60,60,100,-1",
             "", "[Events]", "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, "
             "Effect, Text"]
    body = [f"Dialogue: 0,{ts(a)},{ts(b)},Default,,0,0,0,,{t}" for a, b, t in events]
    return "\n".join(head + body) + "\n"


def two_line(pool, step, offset=0):
    out = []
    for i in range(int(DURATION / step)):
        a, b = pool[(2 * i + offset) % len(pool)], pool[(2 * i + 1 + offset) % len(pool)]
        out.append((i * step, (i + 1) * step, f"{a}\\N{b}"))
    return out


def rapid_events():
    out = []
    for i in range(DURATION * 10):
        out.append((i / 10, (i + 1) / 10,
                    f"{i:04d} {LATIN[i % len(LATIN)][:34]}\\N{CJK[i % len(CJK)]}"))
    return out


def karaoke_events():
    line = "".join(f"{{\\k300}}{s} " for s in SYLLABLES[:20]).rstrip()
    return [(0, DURATION, line)]


def long_events():
    """Six 10 s cues of exactly 300 Latin characters each, rotated so no two match."""
    words = " ".join(LATIN * 6).split()
    out = []
    for i in range(6):
        line = " ".join(words[i * 7:] + words)[:300].rstrip()
        line += "." * (300 - len(line))
        out.append((i * 10, (i + 1) * 10, line))
    return out


# ---- text (SRT / WebVTT / mov_text) fixtures ---------------------------------------------------

LANG = {"latin": "eng", "cjk": "chi", "thai": "tha", "deva": "hin", "arab": "ara"}
FRAME = 1 / 24


def text_of(events):
    """The ASS-case events as plain cue text: ASS `\\N` becomes a real line break."""
    return [(a, b, t.replace("\\N", "\n")) for a, b, t in events]


def srt_time(seconds):
    ms = round(seconds * 1000)
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d},{ms % 1000:03d}"


def vtt_time(seconds):
    return srt_time(seconds).replace(",", ".")


def srt_file(events):
    return "".join(f"{i}\n{srt_time(a)} --> {srt_time(b)}\n{t}\n\n"
                   for i, (a, b, t) in enumerate(events, 1))


def vtt_file(events):
    """`events` are (start, end, text) or (start, end, text, settings)."""
    out = ["WEBVTT\n\n"]
    for e in events:
        a, b, t = e[:3]
        settings = f" {e[3]}" if len(e) > 3 else ""
        out.append(f"{vtt_time(a)} --> {vtt_time(b)}{settings}\n{t}\n\n")
    return "".join(out)


def markup_events():
    """Every override the converter honours, and the cases that must show literally."""
    return [
        (0, 4, '<i>Italic</i>, <b>bold</b>, <u>underline</u>\n'
               '<font color="#ff8800">orange</font> and <font color="red">red</font> text'),
        (4, 8, "{\\an8}Top-aligned cue (an8)"),
        (8, 12, "Bottom line one\nBottom line two\nBottom line three"),
        (12, 16, "{\\an5}Middle-aligned cue (an5)"),
        (16, 20, "{\\an1}Bottom-left (an1)"),
        (20, 24, "{\\an3}Bottom-right (an3)"),
        (24, 28, "<i>ital</i> <font color=\"#00ccff\">cyan</font> {\\i1}brace italic{\\i0} plain"),
        (28, 32, "C:\\temp\\new, a{b unmatched, 1 } 2, <unknown>tag</unknown>"),
        (32, 36, "{\\an8}<i>Top italic</i>\nsecond top line"),
    ]


def overlap_events():
    return [
        (0, 4, "A: runs to 4.000 s"),
        (4 - FRAME, 8, "B: starts ONE FRAME before A ends"),
        (10, 14, "C: starts at 10 s, 4 s long"),
        (10, 13, "D: the same start as C, shorter (newest wins)"),
        (16, 24, "E: bottom dialogue, 8 s long"),
        (18, 22, "{\\an8}F: a top sign over E (both show)"),
        (26, 30, "G: a cue that is overlapped at its END"),
        (29, 33, "H: begins before G ends (G is clipped)"),
    ]


def mixed_events():
    """Latin, CJK, Thai, Devanagari and Arabic in turn, two cues each, 4 s apart."""
    out = []
    for i, pool in enumerate((LATIN, CJK, THAI, DEVA, ARAB)):
        for j in range(2):
            n = 2 * i + j
            out.append((n * 4, n * 4 + 3.5, f"{pool[2 * j]}\n{pool[2 * j + 1]}"))
    return out


def vtt_mixed_events():
    out = mixed_events()
    t = out[-1][1] + 0.5
    out.append((t, t + 3.5, "<v Narrator>Speaker tag, <c.red>class tag</c> and <i>italic</i> &amp; &lt;an entity&gt;"))
    out.append((t + 4, t + 7.5, "Cue setting line:0 (embedded settings are ignored)", "line:0"))
    return out


def mux_text(out, av, name, sub_path, container, codec, lang):
    """Remux `av` with the text file `sub_path` as its one subtitle track, default disposition."""
    mux = out / f"{name}.{container}"
    run(["ffmpeg", "-v", "error", "-y", "-i", str(av), "-i", str(sub_path),
         "-map", "0:v", "-map", "0:a", "-map", "1:s", "-c:v", "copy", "-c:a", "copy",
         "-c:s", codec, "-disposition:s:0", "default", "-metadata:s:s:0", f"language={lang}",
         "-t", str(DURATION), str(mux)])
    return mux


def packet_blob(path, index):
    """The raw bytes of every packet of stream `index`, concatenated."""
    r = subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-map", f"0:{index}", "-c", "copy",
                        "-f", "data", "-"], capture_output=True)
    if r.returncode:
        sys.exit(f"{path.name}: packet dump failed:\n{r.stderr.decode(errors='replace')[-1000:]}")
    return r.stdout


def verify_text(path, codec, events, markers=()):
    """One subtitle stream of `codec`, the default disposition, one packet per cue, and every byte
    string in `markers` still in the packets (the mux did not rewrite the markup)."""
    info = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json",
                           str(path)]))
    subs = [s for s in info["streams"] if s["codec_type"] == "subtitle"]
    assert len(subs) == 1 and subs[0]["codec_name"] == codec, f"{path.name}: want one {codec} stream"
    assert subs[0]["disposition"]["default"] == 1, f"{path.name}: not the default track"
    packets = run(["ffprobe", "-v", "error", "-select_streams", f"{subs[0]['index']}",
                   "-show_entries", "packet=pts_time,size", "-of", "csv=p=0", str(path)]).split()
    if codec == "mov_text":
        # MP4 has no gaps: the muxer fills each one with an empty sample (just the 2-byte length
        # prefix), which the player skips as a cue with no text.
        packets = [p for p in packets if int(p.split(",")[1]) > 2]
    assert len(packets) == events, f"{path.name}: {len(packets)} packets != {events} cues"
    blob = packet_blob(path, subs[0]["index"])
    for m in markers:
        assert m.encode() in blob, f"{path.name}: {m!r} did not survive the mux"
    return float(info["format"]["duration"])


def text_fixtures(out, av):
    """Build every text fixture; returns the table rows (file, duration, cues, codec)."""
    ascii_markers = {"srt-markup": ["<i>Italic</i>", "<b>bold</b>", '<font color="#ff8800">',
                                    "{\\an8}", "{\\an5}", "{\\an1}", "{\\an3}", "{\\i1}"],
                     "srt-overlap": ["{\\an8}"]}
    srt_cases = [
        ("srt-latin2", "latin", text_of(two_line(LATIN, 2))),
        ("srt-cjk2", "cjk", text_of(two_line(CJK, 2))),
        ("srt-thai2", "thai", text_of(two_line(THAI, 2))),
        ("srt-deva2", "deva", text_of(two_line(DEVA, 2))),
        ("srt-arab2", "arab", text_of(two_line(ARAB, 2))),
        ("srt-rapid", "latin", text_of(rapid_events())),
        ("srt-long", "latin", text_of(long_events())),
        ("srt-markup", "latin", markup_events()),
        ("srt-overlap", "latin", overlap_events()),
    ]
    rows = []
    for name, script_key, events in srt_cases:
        srt = out / f"{name}.srt"
        srt.write_text(srt_file(events), encoding="utf-8")
        mkv = mux_text(out, av, name, srt, "mkv", "copy", LANG[script_key])
        rows.append((mkv.name, verify_text(mkv, "subrip", len(events), ascii_markers.get(name, ())),
                     len(events), "subrip"))
    mixed = mixed_events()
    srt = out / "movtext-mixed.srt"
    srt.write_text(srt_file(mixed), encoding="utf-8")
    mp4 = mux_text(out, av, "mov_text-mixed", srt, "mp4", "mov_text", "eng")
    rows.append((mp4.name, verify_text(mp4, "mov_text", len(mixed), [CJK[0], THAI[0], DEVA[0], ARAB[0]]),
                 len(mixed), "mov_text"))
    vtt_events = vtt_mixed_events()
    vtt = out / "webvtt-mixed.vtt"
    vtt.write_text(vtt_file(vtt_events), encoding="utf-8")
    mkv = mux_text(out, av, "webvtt-mixed", vtt, "mkv", "copy", "eng")
    rows.append((mkv.name, verify_text(mkv, "webvtt", len(vtt_events), ["<v Narrator>", "<c.red>", "&amp;"]),
                 len(vtt_events), "webvtt"))
    return rows


def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"{' '.join(map(str, cmd[:3]))} failed:\n{r.stderr[-2000:]}")
    return r.stdout


def fetch_fonts(out):
    fonts = {"Inter": ROOT / "pkg/appfont-bold.ttf"}
    cache = out / "fonts"
    cache.mkdir(exist_ok=True)
    for family, (url, digest, member) in NOTO.items():
        zpath = cache / pathlib.Path(url).name
        if not zpath.exists():
            urllib.request.urlretrieve(url, zpath)
        got = hashlib.sha256(zpath.read_bytes()).hexdigest()
        if got != digest:
            zpath.unlink()
            sys.exit(f"{url}: sha256 {got} != pinned {digest}")
        target = cache / pathlib.Path(member).name
        with zipfile.ZipFile(zpath) as z:
            target.write_bytes(z.read(member))
        fonts[family] = target
    return fonts


def encode_av(out, name, size, fps):
    path = out / name
    if not path.exists():
        run(["ffmpeg", "-v", "error", "-y",
             "-f", "lavfi", "-i", f"color=c=0x2a3340:s={size}:r={fps}",
             "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
             "-t", str(DURATION), "-c:v", "libx264", "-preset", "veryfast", "-tune", "stillimage",
             "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "64k", str(path)])
    return path


def mux(out, av, name, ass_text, font):
    ass = out / f"{name}.ass"
    ass.write_text(ass_text, encoding="utf-8")
    cmd = ["ffmpeg", "-v", "error", "-y", "-i", str(av), "-i", str(ass)]
    if font:
        cmd += ["-attach", str(font)]
    cmd += ["-map", "0:v", "-map", "0:a", "-map", "1:s", "-c", "copy",
            "-disposition:s:0", "default", "-t", str(DURATION)]
    if font:
        cmd += ["-metadata:s:t:0", "mimetype=font/ttf",
                "-metadata:s:t:0", f"filename={pathlib.Path(font).name}"]
    mkv = out / f"{name}.mkv"
    run(cmd + [str(mkv)])
    return mkv


def verify(mkv, events, attached):
    info = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-show_format",
                           "-of", "json", str(mkv)]))
    streams = info["streams"]
    subs = [s for s in streams if s["codec_type"] == "subtitle"]
    atts = [s for s in streams if s["codec_type"] == "attachment"]
    assert len(subs) == 1 and subs[0]["codec_name"] == "ass", f"{mkv.name}: ASS stream missing"
    assert subs[0]["disposition"]["default"] == 1, f"{mkv.name}: not the default track"
    if attached:
        assert len(atts) == 1 and atts[0]["tags"].get("mimetype") == "font/ttf", \
            f"{mkv.name}: font attachment missing"
    else:
        assert not atts, f"{mkv.name}: unexpected attachment"
    packets = run(["ffprobe", "-v", "error", "-select_streams", f"{subs[0]['index']}",
                   "-show_entries", "packet=pts_time", "-of", "csv=p=0", str(mkv)]).split()
    assert len(packets) == events, f"{mkv.name}: {len(packets)} packets != {events} events"
    return float(info["format"]["duration"])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("output", type=pathlib.Path, help="directory for fonts, scripts and MKVs")
    out = ap.parse_args().output.resolve()
    if ROOT in out.parents or out == ROOT:
        sys.exit("refusing to write into the repository checkout")
    out.mkdir(parents=True, exist_ok=True)
    fonts = fetch_fonts(out)
    av = encode_av(out, "av-1080p24.mkv", "1920x1080", 24)
    av4k = encode_av(out, "av-2160p60.mkv", "3840x2160", 60)
    cases = [  # name, font family, events, wrap style, A/V
        ("latin2", "Inter", two_line(LATIN, 2), None, av),
        ("cjk2", "Noto Sans CJK KR", two_line(CJK, 2), None, av),
        ("thai2", "Noto Sans Thai", two_line(THAI, 2), None, av),
        ("deva2", "Noto Sans Devanagari", two_line(DEVA, 2), None, av),
        ("arab2", "Noto Sans Arabic", two_line(ARAB, 2), None, av),
        ("rapid", "Inter", rapid_events(), None, av),
        ("karaoke", "Inter", karaoke_events(), None, av),
        ("long", "Inter", long_events(), 0, av),
        ("rapid-4k60", "Inter", rapid_events(), None, av4k),
    ]
    rows = []
    for name, family, events, wrap, source in cases:
        font = fonts.get(family)
        mkv = mux(out, source, name, script(family, events, wrap), font)
        duration = verify(mkv, len(events), font is not None)
        rows.append((mkv.name, duration, len(events), family))
    text_rows = text_fixtures(out, av)
    print(f"{'file':<22}{'duration_s':>11}{'events':>8}  font / codec")
    for r in rows + text_rows:
        print(f"{r[0]:<22}{r[1]:>11.1f}{r[2]:>8}  {r[3]}")
    print(f"\noutput: {out}")


if __name__ == "__main__":
    main()
