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
    cmd += ["-map", "0:v", "-map", "0:a", "-map", "1:s", "-c", "copy", "-t", str(DURATION)]
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
    print(f"{'file':<16}{'duration_s':>11}{'events':>8}  font")
    for r in rows:
        print(f"{r[0]:<16}{r[1]:>11.1f}{r[2]:>8}  {r[3]}")
    print(f"\noutput: {out}")


if __name__ == "__main__":
    main()
