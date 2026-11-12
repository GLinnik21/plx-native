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

fonts-over-budget.mkv (issue #1328; `--fonts-over-budget`, built alone because it is ~410 MB):
120 s of 1080p23.976 H.264 High that really carries bitrate (a moving test card under temporal
noise, ~22 Mbit/s mean, VBV-capped near the report's 30 Mbit/s peaks and ~166 KB frames) plus AAC,
ONE default ASS track (spa): ~26 s of ordinary dialogue, then a 90 s karaoke opening of ~2400
events (up to ~57 active at once: `\\k`/`\\kf` syllables, `\\t` scale and blur, `\\move`, `\\clip`,
`\\fad`, `\\bord`, `\\blur`, `\\be`, glow and echo layers, per-syllable pops, rings, shines and star
bursts, `\\p1` drawings, a 14/s sparkle field; invented lyrics: kanji/kana, romaji, Spanish). It
attaches 17 fonts, 83.4 MB, sized and ordered like the report's: the first seven (32 MB) fill what was
the 32 MiB `ass::MAX_FONT_BYTES` cap (removed by #1328) with faces no style names, the five the
opening's styles name (Antique, Brush Mincho, Dot, Pop, Heavy Gothic: each a different face from the
app's Inter and Noto Sans CJK KR fallback) come next and were all dropped by it, and small ones after
them (the dialogue face among them) still fit. The layout stays as the report's shape: libass is now
handed only the faces a style or `\\fn` names (`ass/font_names.rs`). Fonts, chosen over a hand-drawn set because the glyph SHAPES
must differ from the fallback: the five display faces are real OFL fonts from google/fonts
(Zen Antique, Yuji Syuku, DotGothic16, Mochiy Pop One, Dela Gothic One), pinned by commit and
sha256 like the Noto zips; the fillers and the dialogue face are pkg/ fonts. Every attachment is
`rename_font`ed in pure Python (no fontTools: the `name` table is rewritten to a distinct family,
and a private `PLXP` table of seeded random bytes pads it to the report's exact size; unknown
tables are legal sfnt and FreeType skips them, `fc-scan` and libass read them), so the file is
reproducible from pinned inputs with no pip install. `verify` asserts the attachment count and
bytes (> 32 MiB), distinct families, that every family a style names ends past the budget in
container order and that exactly the opening's five would have been dropped by a mirror of the removed `FontBudget`, that
each face has the glyphs its styles draw, one packet per event, >= 40 events at once, and a video
mean of at least 15 Mbit/s. A libass host render of the opening differs frame for frame between
the embedded faces and the fallback, so a dropped font is visible.
The per-syllable effects (pops, rings, shines, star bursts) are placed from the real glyph advances
of the Japanese face (`libass_advances`), so they sit on their glyphs.

Needs host ffmpeg and ffprobe only (no mkvmerge); curl is used for downloads when present: the A/V is encoded once per resolution and every
fixture remuxes it with `-c copy`, so the subtitle track is never subject to the encoder's sparse
subtitle scheduling; the packet count is checked against the script's event count anyway.
"""
import argparse
import hashlib
import json
import pathlib
import random
import re
import struct
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


# ---- fonts-over-budget (issue #1328): a dense opening, 17 attached fonts, 83 MB -----------------

OP_DURATION = 120
OP_START = 28.0          # the dialogue runs 0..26 s; the karaoke opening 28..118 s
OP_LINE = 5.0            # eighteen lyric lines of five seconds
BUDGET = 32 * 1024 * 1024    # the removed `MAX_FONT_BYTES` (#1328): the layout's history, not a live constant
FONT_OVERHEAD = 1024         # the removed `FONT_OVERHEAD_BYTES`, likewise
VIDEO_FLOOR_MBPS = 15.0      # the report's opening peaked near 30 Mbit/s
# The opening's libass cost knobs (host: ~3.4 ms per 1080p frame at these values, p95 3.9 ms; the
# television's per-job figure for this file is not measured): background sparkles per second and
# star particles thrown off each syllable. Both scale the events active at once (~45 average).
SPARKLES_PER_S = 14
BURST_STARS = 3

# Display faces from google/fonts (OFL), pinned by commit and sha256 like the Noto zips above.
GF_COMMIT = "c30b221beb0c1da59d563feb91ec8656a1fa972a"
GF_FONTS = {  # key -> (path under ofl/, sha256)
    "antique": ("zenantique/ZenAntique-Regular.ttf",
                "8c5cf7a136837ee705d06bbc133ea18ac06b7dd284f38aace91f5de36725c315"),
    "brush": ("yujisyuku/YujiSyuku-Regular.ttf",
              "82728ebafc8c97391e2dab633414a806f344b8e4e2227d307179f07b548fca61"),
    "dot": ("dotgothic16/DotGothic16-Regular.ttf",
            "3ad9af88726d42b40f7f365f0dcac785af73cf20ea6f1d5b44e57cc21150b8f1"),
    "pop": ("mochiypopone/MochiyPopOne-Regular.ttf",
            "9e009430e1316c271a5f34759c6b65fc343c4e806f193042528887e7235a92c6"),
    "gothic": ("delagothicone/DelaGothicOne-Regular.ttf",
               "4ff87a0965f1b0505e5a2c58424bc6ad3cff27e56a82f21c2fc9d6b0e3857ee2"),
}
LOCAL_FONTS = {  # key -> path in the checkout
    "inter": "pkg/appfont.ttf", "inter-bold": "pkg/appfont-bold.ttf",
    "hebrew": "pkg/appfont-hebrew.ttf", "hebrew-bold": "pkg/appfont-hebrew-bold.ttf",
    "arabic": "pkg/appfont-arabic.ttf", "arabic-bold": "pkg/appfont-arabic-bold.ttf",
}
FAMILY = {"dialogue": "Plx Dialogue Sans", "gothic": "PlxOP Heavy Gothic", "pop": "PlxOP Pop",
          "antique": "PlxOP Antique", "brush": "PlxOP Brush Mincho", "dot": "PlxOP Dot"}
# The attachments in container order, sized and ordered like the report's 17 (83 MB): the first
# seven spend a 32 MiB cap on faces no style names, the five the opening's styles name come
# next, and the small ones after them still fit (twelve were held in all, as in the report). The dialogue face is
# natural-sized (Inter Bold is larger than the report's Roboto slot). (file, family, base, style,
# padded-to bytes or None).
SLOTS = [
    ("filler-01.ttf", "Plx Filler 01", "inter", "Regular", 6_220_620),
    ("filler-02.ttf", "Plx Filler 02", "hebrew", "Regular", 32_904),
    ("filler-03.ttf", "Plx Filler 03", "inter", "Regular", 11_229_304),
    ("filler-04.ttf", "Plx Filler 04", "hebrew", "Regular", 37_300),
    ("filler-05.ttf", "Plx Filler 05", "hebrew", "Regular", 43_840),
    ("filler-06.ttf", "Plx Filler 06", "inter", "Regular", 9_883_924),
    ("filler-07.ttf", "Plx Filler 07", "inter", "Regular", 4_808_652),
    ("op-antique.ttf", FAMILY["antique"], "antique", "Regular", 7_477_668),
    ("op-brush-mincho.ttf", FAMILY["brush"], "brush", "Regular", 13_188_132),
    ("op-dot.ttf", FAMILY["dot"], "dot", "Regular", 6_102_048),
    ("op-pop.ttf", FAMILY["pop"], "pop", "Regular", 9_705_124),
    ("op-heavy-gothic.ttf", FAMILY["gothic"], "gothic", "Regular", 13_715_688),
    ("filler-13.ttf", "Plx Filler 13", "hebrew", "Regular", 47_816),
    ("filler-14.ttf", "Plx Filler 14", "arabic", "Regular", 245_796),
    ("filler-15.ttf", "Plx Filler 15", "arabic-bold", "Bold", 262_448),
    ("dialogue.ttf", FAMILY["dialogue"], "inter-bold", "Bold", None),
    ("filler-17.ttf", "Plx Filler 17", "hebrew-bold", "Bold", 32_972),
]


def _u16(b, o):
    return struct.unpack_from(">H", b, o)[0]


def _u32(b, o):
    return struct.unpack_from(">I", b, o)[0]


def sfnt_tables(data):
    """(sfnt version tag, {table tag: bytes}) of a single-font TrueType/OpenType file."""
    assert data[:4] in (b"\x00\x01\x00\x00", b"OTTO", b"true"), "not a plain sfnt (a TTC?)"
    out = {}
    for i in range(_u16(data, 4)):
        tag, _sum, off, length = struct.unpack_from(">4sIII", data, 12 + 16 * i)
        out[tag] = data[off:off + length]
    return data[:4], out


def table_sum(b):
    b = b + b"\0" * (-len(b) % 4)
    return sum(struct.unpack(f">{len(b) // 4}I", b)) & 0xFFFFFFFF


def sfnt_build(version, tables):
    """Serialize `tables` into a font: sorted directory, 4-byte aligned tables, real checksums."""
    tables = dict(tables)
    tags = sorted(tables)
    n = len(tags)
    entry = n.bit_length() - 1
    search = 16 << entry
    head = bytearray(tables[b"head"])
    head[8:12] = b"\0\0\0\0"                      # checkSumAdjustment is summed as zero
    tables[b"head"] = bytes(head)
    directory, body, head_at = b"", b"", 0
    for tag in tags:
        t = tables[tag]
        at = 12 + 16 * n + len(body)
        if tag == b"head":
            head_at = at
        directory += struct.pack(">4sIII", tag, table_sum(t), at, len(t))
        body += t + b"\0" * (-len(t) % 4)
    font = bytearray(struct.pack(">4sHHHH", version, n, search, entry, 16 * n - search)
                     + directory + body)
    struct.pack_into(">I", font, head_at + 8, (0xB1B0AFBA - table_sum(bytes(font))) & 0xFFFFFFFF)
    return bytes(font)


# Name ids copied from the base font (copyright, version, trademark, designer, licence ...); the
# family-bearing ones (1 2 3 4 6, and the typographic 16 17) are rewritten.
KEEP_NAME_IDS = {0, 5, 7, 8, 9, 10, 11, 12, 13, 14}


def name_records(table):
    count, store = _u16(table, 2), _u16(table, 4)
    for i in range(count):
        pid, eid, lid, nid, length, off = struct.unpack_from(">6H", table, 6 + 12 * i)
        yield pid, eid, lid, nid, table[store + off:store + off + length]


def font_family(data):
    """The Windows English family name (name id 1) of a font."""
    _, tables = sfnt_tables(data)
    for pid, eid, lid, nid, raw in name_records(tables[b"name"]):
        if (pid, eid, lid, nid) == (3, 1, 0x409, 1):
            return raw.decode("utf-16-be")
    raise ValueError("font has no Windows family name")


def renamed_name_table(old, family, style):
    kept = [(nid, raw) for pid, eid, lid, nid, raw in name_records(old)
            if (pid, eid, lid) == (3, 1, 0x409) and nid in KEEP_NAME_IDS]
    full = family if style == "Regular" else f"{family} {style}"
    new = {1: family, 2: style, 3: f"{full};plx-ass-stress", 4: full,
           6: f"{family.replace(' ', '')}-{style}"}
    rows = sorted(kept + [(nid, s.encode("utf-16-be")) for nid, s in new.items()])
    head, store = b"", b""
    for nid, raw in rows:
        head += struct.pack(">6H", 3, 1, 0x409, nid, len(raw), len(store))
        store += raw
    return struct.pack(">3H", 0, len(rows), 6 + 12 * len(rows)) + head + store


def rename_font(data, family, style, pad_to=None):
    """`data` with its family renamed, and (when `pad_to` is given) a private `PLXP` table of
    seeded random bytes appended so the file is exactly `pad_to` bytes. Unknown tables are legal
    sfnt and FreeType skips them; every glyph table is untouched."""
    version, tables = sfnt_tables(data)
    tables[b"name"] = renamed_name_table(tables[b"name"], family, style)
    font = sfnt_build(version, tables)
    if pad_to is not None:
        assert pad_to % 4 == 0 and pad_to > len(font) + 16, f"{family}: base is larger than {pad_to}"
        pad = random.Random(family).randbytes(pad_to - len(font) - 16)
        tables[b"PLXP"] = pad
        font = sfnt_build(version, tables)
        assert len(font) == pad_to, f"{family}: built {len(font)} bytes, wanted {pad_to}"
    return font


def font_cmap(data):
    """{Unicode code point: glyph id} a font maps to a real glyph (cmap formats 4 and 12)."""
    cmap = sfnt_tables(data)[1][b"cmap"]
    found = {}
    for i in range(_u16(cmap, 2)):
        pid, eid, off = struct.unpack_from(">HHI", cmap, 4 + 8 * i)
        if (pid, eid) not in ((0, 3), (0, 4), (3, 1), (3, 10)):
            continue
        fmt = _u16(cmap, off)
        if fmt == 4:
            segs = _u16(cmap, off + 6) // 2
            ends, starts = off + 14, off + 16 + 2 * segs
            deltas, ranges = starts + 2 * segs, starts + 4 * segs
            for s in range(segs):
                end, start = _u16(cmap, ends + 2 * s), _u16(cmap, starts + 2 * s)
                delta, ro = _u16(cmap, deltas + 2 * s), _u16(cmap, ranges + 2 * s)
                for c in range(start, min(end, 0xFFFE) + 1):
                    g = (c + delta) & 0xFFFF if ro == 0 else \
                        _u16(cmap, ranges + 2 * s + ro + 2 * (c - start))
                    if ro and g:
                        g = (g + delta) & 0xFFFF
                    if g:
                        found[c] = g
        elif fmt == 12:
            for g in range(_u32(cmap, off + 12)):
                start, end, glyph = struct.unpack_from(">III", cmap, off + 16 + 12 * g)
                found.update((c, glyph + c - start) for c in range(start, end + 1) if glyph + c - start)
    return found


def font_codepoints(data):
    """The Unicode code points a font maps to a real glyph."""
    return set(font_cmap(data))


def libass_advances(data, text, size):
    """The horizontal advance in px of each character of `text` set in the font `data` at font size
    `size` (a style's Fontsize at 100% scale, no \\fsp). libass sizes a face with
    FT_Request_Size(FT_SIZE_REQUEST_TYPE_REAL_DIM) (`ass_face_set_size`), which maps the face's
    ascender - descender, NOT its em (`head.unitsPerEm` takes no part), onto `size`; so a glyph is
    advance / (ascender - descender) of `size` wide. The ascender and descender are the ones libass
    leaves on the face (`set_font_metrics`, mimicking GDI): OS/2 usWinAscent and usWinDescent (read
    as signed) when they do not sum to zero; else FreeType's (sfnt/sfobjs.c: OS/2 sTypo* when
    fsSelection bit 7, USE_TYPO_METRICS, is set, else `hhea`); else OS/2 sTypo* when those differ,
    else the `head` bounding box. Dela Gothic One's are 1160 and 288 units for a 1000 unit em: a
    full-width glyph is 1000 / 1448 = 0.69 of `size` wide. Not modelled, and absent from the
    opening's text: kerning (a host render of every line of the opening agrees with these advances
    to 2 px). The style's Outline is symmetric and does not move a glyph's centre."""
    _, tables = sfnt_tables(data)
    asc, desc = struct.unpack_from(">hh", tables[b"hhea"], 4)
    os2 = tables.get(b"OS/2")
    typo = (0, 0)
    if os2 and len(os2) >= 78:
        typo = struct.unpack_from(">hh", os2, 68)
        win_asc, win_desc = struct.unpack_from(">hh", os2, 74)
        if win_asc + win_desc:
            asc, desc = win_asc, -win_desc
        elif _u16(os2, 62) & 128:
            asc, desc = typo
    if asc - desc == 0:
        if typo[0] - typo[1]:
            asc, desc = typo
        else:
            _, desc, _, asc = struct.unpack_from(">hhhh", tables[b"head"], 36)  # xMin yMin xMax yMax
    assert asc - desc > 0, "font has no height"
    metrics, count = tables[b"hmtx"], _u16(tables[b"hhea"], 34)
    cmap = font_cmap(data)
    return [_u16(metrics, 4 * min(cmap[ord(ch)], count - 1)) * size / (asc - desc) for ch in text]


# ---- the opening's script ----------------------------------------------------------------------
# Invented lyrics (so no real song is reproduced). Words are split by "/", the items of a word by
# a space, an item is `kanji-or-kana:romaji`; one `\k` syllable per item.
LYRICS = [
    "夜:yo 明:a け:ke/の:no/空:sora/へ:e/駆:ka け:ke 出:da す:su",
    "星:hoshi 屑:kuzu/の:no/海:umi/を:o/越:ko え:e て:te",
    "君:kimi/の:no/声:koe/が:ga/響:hibi い:i て:te/い:i る:ru",
    "未:mi 来:rai/を:o/掴:tsuka も:mo う:u/今:ima/す:su ぐ:gu に:ni",
    "風:kaze/に:ni/舞:ma う:u/花:hana び:bi ら:ra/た:ta ち:chi/よ:yo",
    "消:ki え:e な:na い:i/光:hikari/を:o/抱:da い:i て:te",
    "走:hashi れ:re/走:hashi れ:re/永:to 遠:wa/の:no/彼:kana 方:ta",
    "運:un 命:mei/の:no/扉:tobira/を:o/開:a け:ke ろ:ro",
    "約:yaku 束:soku/の:no/場:ba 所:sho/で:de/会:a お:o う:u",
]
SPANISH = [
    "Hacia el cielo del amanecer, salimos corriendo",
    "Cruzando el mar de polvo de estrellas",
    "Tu voz está resonando dentro de mí",
    "Atrapemos el futuro ahora mismo",
    "Pétalos que bailan con el viento",
    "Abrazando la luz que no se apaga",
    "Corre, corre, hasta el confín de la eternidad",
    "¡Abre la puerta del destino!",
    "Nos veremos en el lugar prometido",
]
DIALOGUE = [
    "Todo está listo para la partida.", "¿Seguro que quieres ir tan lejos?",
    "No tengo miedo. Ya no.", "Entonces salimos al amanecer.",
    "Dicen que más allá del mar hay una ciudad de luz.", "Y que allí nadie tiene que esconderse.",
    "Pues vamos a comprobarlo.", "Prepara las velas. Yo me encargo del mapa.",
    "¡Mira! Ya empieza a clarear.",
]
JP_SIZE = 88            # the Japanese line's font size; its glyphs are narrower (`libass_advances`)
JP_Y = 980              # its bottom edge (\an2: baseline plus descent); the glyphs' middle is ~40 px above
STAR = "m 0 -20 l 5 -6 20 -6 8 3 12 18 0 9 -12 18 -8 3 -20 -6 -5 -6"
RING = ("m -20 0 b -20 -11 -11 -20 0 -20 b 11 -20 20 -11 20 0 "
        "b 20 11 11 20 0 20 b -11 20 -20 11 -20 0")
PALETTE = ["&HFFE8D0&", "&H80E0FF&", "&HC0A0FF&", "&H70FFC0&", "&HFFFFFF&", "&HA0C8FF&"]


def style_line(name, family, size, primary, secondary, outline, bold, border, align, margin_v):
    return (f"Style: {name},{family},{size},{primary},{secondary},{outline},&H80000000,{bold},0,0,0,"
            f"100,100,0,0,1,{border},0,{align},60,60,{margin_v},1")


def opening_events(jp_font):
    """[(start_s, end_s, layer, style, text)] of the dialogue stretch and the 90 s opening.
    `jp_font` is the bytes of the face the OPJp style names: the per-syllable effects are placed
    from its real glyph advances.

    The effects are placed for the Japanese line's SETTLED layout (every glyph at 100%): the line's
    opening `\\fscx110` (animated to 100 over 0.35 s, before the first syllable) and each syllable's
    `\\fscx118` pop are transient, and a pop's scale is inherited by every later glyph until that
    glyph's own block, so the glyphs shift while a syllable pops (~15 px at a pop's start for the
    line's first glyphs, none for its last; more at the pop's peak). A host render of all 18 lines
    puts every pop within 2 px of its glyph's settled position."""
    rnd = random.Random(1328)
    ev = []
    for i, line in enumerate(DIALOGUE):
        ev.append((1 + i * 2.8, 1 + i * 2.8 + 2.5, 0, "Default", line))
    ev.append((118.2, 119.8, 0, "Default", "Continuará…"))
    for n in range(2 * len(LYRICS)):
        t0 = OP_START + n * OP_LINE
        words = [[tuple(item.split(":")) for item in w.split()] for w in LYRICS[n % len(LYRICS)].split("/")]
        items = [it for w in words for it in w]
        count = len(items)
        weights = [(10, 8, 6, 8, 12, 6, 8, 8, 10, 14)[j % 10] for j in range(count)]
        total_cs = 380
        durs = [total_cs * w // sum(weights) for w in weights]
        durs[-1] += total_cs - sum(durs)
        starts_ms = [400 + 10 * sum(durs[:j]) for j in range(count)]
        k = "kf" if n % 2 else "k"
        life_ms = int(OP_LINE * 1000)
        # Each item's centre at 100% scale, from the font's real advances, as libass centres a
        # `\an2\pos(960,_)` line (its width is the sum of the advances, no \fsp): so the per-syllable
        # effects sit on their glyph.
        widths = [sum(libass_advances(jp_font, jp, JP_SIZE)) for jp, _ in items]
        centres, x = [], 960 - sum(widths) / 2
        for width in widths:
            centres.append(x + width / 2)
            x += width
        kara = "".join(
            f"{{\\{k}{d}\\t({s},{s + 140},\\fscx118\\fscy118)\\t({s + 140},{s + 360},\\fscx100\\fscy100)}}{jp}"
            for (jp, _), d, s in zip(items, durs, starts_ms))
        glow = "".join(
            f"{{\\k{d}\\t({s},{s + 180},\\blur14)\\t({s + 180},{s + 420},\\blur9)}}{jp}"
            for (jp, _), d, s in zip(items, durs, starts_ms))
        romaji = ""
        j = 0
        for w in words:
            for _, ro in w:
                romaji += f"{{\\kf{durs[j]}}}{ro}"
                j += 1
            romaji += " "
        pos = f"\\an2\\pos(960,{JP_Y})"
        ev += [
            (t0 + 0.2, t0 + OP_LINE, 1, "OPEs",
             f"{{\\an2\\pos(960,1045)\\fad(300,300)\\bord3\\blur1\\fscx96\\t(0,500,\\fscx100)}}{SPANISH[n % 9]}"),
            (t0, t0 + OP_LINE, 2, "OPRomaji",
             f"{{\\an8\\pos(960,70)\\fad(200,300)\\bord4\\blur0.8\\fscx125\\fscy125\\t(0,400,\\fscx100\\fscy100)}}{romaji.rstrip()}"),
            (t0, t0 + OP_LINE, 3, "OPJp",
             f"{{{pos}\\fad(250,300)\\bord5\\blur0.6\\be1\\fscx110\\fscy110\\t(0,350,\\fscx100\\fscy100)}}{kara}"),
            (t0, t0 + OP_LINE, 2, "OPJp",
             f"{{{pos}\\fad(250,300)\\bord12\\blur9\\1a&HFF&\\3c&HFFB060&\\3a&H60&}}{glow}"),
            (t0, t0 + OP_LINE, 1, "OPJp",
             f"{{\\an2\\move(955,{JP_Y + 6},975,{JP_Y - 6},0,{life_ms})\\fad(300,300)\\alpha&HA0&\\bord0\\blur3"
             f"\\t(0,{life_ms},\\frz3\\fscx108)}}{''.join(jp for jp, _ in items)}"),
            (t0, t0 + OP_LINE, 0, "OPBrush",
             f"{{\\an5\\pos(960,470)\\fad(500,500)\\blur14\\bord0\\alpha&H90&\\1c&HFFE8D0&"
             f"\\t(0,{life_ms},\\fscx125\\fscy125\\frz5)}}{items[0][0]}"),
            (t0, t0 + OP_LINE, 0, "OPFx",
             f"{{\\an7\\pos(0,0)\\p1\\1c&H301810&\\1a&H60&\\bord0\\blur18\\clip(0,860,0,1040)"
             f"\\t(0,600,\\clip(0,860,1920,1040))\\fad(0,300)}}m 0 860 l 1920 860 1920 1040 0 1040"),
        ]
        for (jp, _), cx, s in zip(items, centres, starts_ms):
            a = t0 + s / 1000
            cy = JP_Y - JP_SIZE // 2
            c1, c2 = rnd.choice(PALETTE), rnd.choice(PALETTE)
            ev += [
                (a, a + 0.6, 5, "OPFx",
                 f"{{\\an5\\move({cx:.0f},{cy},{cx:.0f},{cy - 80},0,600)\\bord3\\blur1.5\\1c{c1}\\3c{c2}"
                 f"\\t(0,600,\\fscx210\\fscy210\\alpha&HFF&\\blur6)}}{jp}"),
                # \an7, not \an5: libass anchors a drawing at its coordinate origin, so under \an5 a ring
                # drawn about (0,0) drifts up-left by its own radius as it grows (host render: 18 px
                # at +0.15 s, 44 px at +0.45 s); \an7 keeps its centre on pos.
                (a, a + 0.55, 4, "OPFx",
                 f"{{\\an7\\pos({cx:.0f},{cy})\\p1\\bord4\\blur2\\1a&HFF&\\3c{c2}\\fscx30\\fscy30"
                 f"\\t(0,550,\\fscx260\\fscy260\\3a&HFF&)}}{RING}"),
                (a, a + 0.35, 6, "OPFx",
                 f"{{\\an5\\pos({cx:.0f},{cy})\\bord2\\blur0.5\\1c&HFFFFFF&\\3c&HFFFFFF&"
                 f"\\clip({cx - 50:.0f},{cy - 70},{cx - 50:.0f},{cy + 70})"
                 f"\\t(0,350,\\clip({cx - 50:.0f},{cy - 70},{cx + 50:.0f},{cy + 70}))}}{jp}"),
            ]
            for _ in range(BURST_STARS):
                dx, dy = rnd.randint(-110, 110), rnd.randint(-130, -30)
                ev.append((a, a + 0.8, 7, "OPFx",
                           f"{{\\an5\\move({cx:.0f},{cy},{cx + dx:.0f},{cy + dy},0,800)\\p1"
                           f"\\fscx{rnd.randint(40, 90)}\\fscy{rnd.randint(40, 90)}\\bord1\\blur1"
                           f"\\1c{rnd.choice(PALETTE)}\\3c&HFFFFFF&\\t(0,800,\\frz{rnd.choice((-1, 1)) * 240}"
                           f"\\alpha&HFF&)}}{STAR}"))
    steps = int(2 * len(LYRICS) * OP_LINE * SPARKLES_PER_S)
    for i in range(steps):
        a = OP_START + i / SPARKLES_PER_S
        life = rnd.uniform(1.6, 2.8)
        x1, y1 = rnd.randint(0, 1920), rnd.randint(40, 1040)
        x2, y2 = x1 + rnd.randint(-260, 260), y1 + rnd.randint(-200, 200)
        s = rnd.randint(60, 200)
        ev.append((a, a + life, rnd.choice((1, 3)), "OPFx",
                   f"{{\\an5\\move({x1},{y1},{x2},{y2},0,{int(life * 1000)})\\p1\\fscx{s}\\fscy{s}"
                   f"\\bord{rnd.choice((1, 2))}\\blur{rnd.choice((0.8, 1.5, 2.5))}\\1c{rnd.choice(PALETTE)}"
                   f"\\3c{rnd.choice(PALETTE)}\\fad(150,300)"
                   f"\\t(0,{int(life * 1000)},\\frz{rnd.choice((-1, 1)) * rnd.randint(120, 360)})}}{STAR}"))
    return ev


def opening_script(events):
    head = ["[Script Info]", "ScriptType: v4.00+", "PlayResX: 1920", "PlayResY: 1080",
            "ScaledBorderAndShadow: yes", "WrapStyle: 2", "", "[V4+ Styles]",
            "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, "
            "BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, "
            "BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
            style_line("Default", FAMILY["dialogue"], 48, "&H00FFFFFF", "&H0000FFFF", "&H26000000", -1, 2, 2, 100),
            style_line("OPJp", FAMILY["gothic"], JP_SIZE, "&H00FFFFFF", "&H0070A0FF", "&H00402010", 0, 5, 2, 100),
            style_line("OPRomaji", FAMILY["pop"], 52, "&H00FFFFFF", "&H00FFC0A0", "&H00402010", 0, 4, 8, 70),
            style_line("OPEs", FAMILY["antique"], 54, "&H00F0FFFF", "&H0000FFFF", "&H00301020", 0, 3, 2, 40),
            style_line("OPBrush", FAMILY["brush"], 380, "&H00FFFFFF", "&H00FFFFFF", "&H00000000", 0, 0, 5, 0),
            style_line("OPFx", FAMILY["dot"], JP_SIZE, "&H00FFFFFF", "&H00FFFFFF", "&H00FFB060", 0, 2, 5, 0),
            "", "[Events]", "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"]
    body = [f"Dialogue: {layer},{ts(a)},{ts(b)},{style},,0,0,0,,{text}"
            for a, b, layer, style, text in events]
    return "\n".join(head + body) + "\n"


def max_simultaneous(events):
    edges = sorted([(round(a * 100), 1) for a, *_ in events] + [(round(e[1] * 100), -1) for e in events],
                   key=lambda p: (p[0], p[1]))
    live = peak = 0
    for _, d in edges:
        live += d
        peak = max(peak, live)
    return peak


# ---- build and verify ---------------------------------------------------------------------------

def download(url, target):
    """curl when the host has it (it falls back from an unreachable IPv6 address at once, where
    urllib waits out the connect timeout of each address, minutes per file), else urllib."""
    part = target.with_name(target.name + ".part")
    try:
        subprocess.run(["curl", "-fsSL", "--max-time", "600", "-o", str(part), url], check=True)
    except FileNotFoundError:
        urllib.request.urlretrieve(url, part)
    part.rename(target)


def fetch_display_fonts(cache):
    """The pinned google/fonts TTFs, downloaded into `cache` and checked against their sha256."""
    got = {}
    for key, (path, digest) in GF_FONTS.items():
        target = cache / pathlib.Path(path).name
        if not target.exists():
            download(f"https://raw.githubusercontent.com/google/fonts/{GF_COMMIT}/ofl/{path}", target)
        have = hashlib.sha256(target.read_bytes()).hexdigest()
        if have != digest:
            target.unlink()
            sys.exit(f"{path}: sha256 {have} != pinned {digest}")
        got[key] = target
    return got


def build_attachments(out):
    """Write the 17 renamed, padded fonts of `SLOTS` under <out>/attach-1328; [(path, family)]."""
    cache = out / "fonts-1328"
    cache.mkdir(exist_ok=True)
    base = {key: ROOT / rel for key, rel in LOCAL_FONTS.items()}
    base.update(fetch_display_fonts(cache))
    attach = out / "attach-1328"
    attach.mkdir(exist_ok=True)
    built = []
    for name, family, key, style, size in SLOTS:
        path = attach / name
        path.write_bytes(rename_font(base[key].read_bytes(), family, style, size))
        built.append((path, family))
    return built


def encode_av_hibr(out):
    """1080p23.976 High-profile H.264 that cannot compress (moving test card under temporal noise)
    at 22 Mbit/s capped at 30, plus stereo AAC, OP_DURATION seconds: the decoder feed has real work.
    The small VBV buffer and ipratio 1.0 keep every frame, keyframes included, near the report's
    (frames up to ~166 KB; a default buffer made the first IDR 775 KB)."""
    path = out / "av-1080p23976-hibr.mkv"
    if not path.exists():
        part = out / "av-1080p23976-hibr.part"
        run(["ffmpeg", "-v", "error", "-y",
             "-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=24000/1001,noise=alls=28:allf=t+u",
             "-f", "lavfi", "-i", "anoisesrc=color=pink:amplitude=0.05:sample_rate=48000",
             "-t", str(OP_DURATION), "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high",
             "-level", "4.1", "-b:v", "22M", "-maxrate", "30M", "-bufsize", "3M", "-g", "48",
             "-x264-params", "ipratio=1.0", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "160k", "-ac", "2", "-f", "matroska", str(part)])
        part.rename(path)
    return path


def mux_fonts(out, av, name, ass_text, attachments):
    ass = out / f"{name}.ass"
    ass.write_text(ass_text, encoding="utf-8")
    cmd = ["ffmpeg", "-v", "error", "-y", "-i", str(av), "-i", str(ass)]
    for path, _ in attachments:
        cmd += ["-attach", str(path)]
    cmd += ["-map", "0:v", "-map", "0:a", "-map", "1:s", "-c", "copy", "-disposition:s:0", "default",
            "-metadata:s:s:0", "language=spa", "-t", str(OP_DURATION)]
    for i, (path, _) in enumerate(attachments):
        cmd += [f"-metadata:s:t:{i}", "mimetype=font/ttf", f"-metadata:s:t:{i}", f"filename={path.name}"]
    mkv = out / f"{name}.mkv"
    run(cmd + [str(mkv)])
    return mkv


def admitted(sizes):
    """The set of indexes the removed `ass::FontBudget::admit` kept: container order, each font charged its
    bytes plus FONT_OVERHEAD, one that does not fit skipped while a later smaller one may fit."""
    used, kept = 0, set()
    for i, size in enumerate(sizes):
        if size and used + size + FONT_OVERHEAD <= BUDGET:
            used += size + FONT_OVERHEAD
            kept.add(i)
    return kept


def verify_fonts_over_budget(mkv, events, ass_text, attachments):
    info = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json",
                           str(mkv)]))
    by_type = lambda t: [s for s in info["streams"] if s["codec_type"] == t]
    name = mkv.name
    (video,), (audio,), (sub,), atts = (by_type("video"), by_type("audio"), by_type("subtitle"),
                                        by_type("attachment"))
    assert (video["codec_name"], video["profile"], video["width"], video["height"]) == \
        ("h264", "High", 1920, 1080), f"{name}: want 1080p H.264 High"
    assert video["r_frame_rate"] == "24000/1001", f"{name}: frame rate {video['r_frame_rate']}"
    assert audio["codec_name"] == "aac" and audio["channels"] == 2, f"{name}: want stereo AAC"
    assert sub["codec_name"] == "ass" and sub["disposition"]["default"] == 1, f"{name}: ASS not default"
    assert sub["tags"].get("language") == "spa", f"{name}: subtitle language"
    duration = float(info["format"]["duration"])
    assert abs(duration - OP_DURATION) < 1, f"{name}: {duration} s"

    # The attachments: count, bytes, order, and that they are the files this script wrote.
    assert len(atts) == len(attachments) >= 17, f"{name}: {len(atts)} attachments"
    sizes = [int(a["extradata_size"]) for a in atts]
    assert [(a["tags"]["filename"], n) for a, n in zip(atts, sizes)] == \
        [(p.name, p.stat().st_size) for p, _ in attachments], f"{name}: attachments differ from the files"
    assert sum(sizes) > BUDGET, f"{name}: {sum(sizes)} font bytes do not exceed {BUDGET}"
    fonts = [p.read_bytes() for p, _ in attachments]
    families = [font_family(f) for f in fonts]
    assert families == [fam for _, fam in attachments] and len(set(families)) == len(families), \
        f"{name}: families are not distinct"

    # The styles: every family they name is in one attachment that ends past the budget in
    # container order, and the opening's are ones the budget rejects today.
    styled = {m[1]: m[2] for m in (re.match(r"Style: ([^,]+),([^,]+),", l) for l in ass_text.split("\n"))
              if m}
    kept = admitted(sizes)
    ends = [sum(sizes[:i + 1]) for i in range(len(sizes))]
    for style, family in styled.items():
        i = families.index(family)
        assert ends[i] > BUDGET, f"{name}: {style}'s {family} ends before the budget is spent"
        assert (i in kept) == (style == "Default"), \
            f"{name}: {style}'s {family}: the budget {'keeps' if i in kept else 'drops'} it"
        used = "".join(re.sub(r"\{[^}]*\}", "", t).replace("\\N", "") for _, _, _, s, t in events
                       if s == style and "\\p1" not in t)
        missing = {c for c in used if not c.isspace() and ord(c) not in font_codepoints(fonts[i])}
        assert not missing, f"{name}: {family} lacks {''.join(sorted(missing))}"
    dropped = [families[i] for i in range(len(sizes)) if i not in kept]
    assert set(dropped) == {f for st, f in styled.items() if st != "Default"}, \
        f"{name}: the budget drops {dropped}, not exactly the opening's faces"

    packets = run(["ffprobe", "-v", "error", "-select_streams", str(sub["index"]),
                   "-show_entries", "packet=pts_time", "-of", "csv=p=0", str(mkv)]).split()
    assert len(packets) == len(events), f"{name}: {len(packets)} packets != {len(events)} events"
    peak = max_simultaneous(events)
    assert peak >= 40, f"{name}: only {peak} events at once"

    frames = [tuple(map(float, p.split(","))) for p in run(
        ["ffprobe", "-v", "error", "-select_streams", str(video["index"]), "-show_entries",
         "packet=pts_time,size", "-of", "csv=p=0", str(mkv)]).split()]
    mbps = sum(s for _, s in frames) * 8 / duration / 1e6
    second = {}
    for t, s in frames:
        second[int(t)] = second.get(int(t), 0) + s * 8 / 1e6
    assert mbps >= VIDEO_FLOOR_MBPS, f"{name}: video {mbps:.1f} Mbit/s < {VIDEO_FLOOR_MBPS}"
    return {"duration": duration, "attachments": len(atts), "font_bytes": sum(sizes),
            "dropped": dropped, "events": len(events), "peak_events": peak, "video_mbps": mbps,
            "video_peak_mbps": max(second.values()), "max_frame_bytes": int(max(s for _, s in frames))}


def build_fonts_over_budget(out):
    attachments = build_attachments(out)
    av = encode_av_hibr(out)
    jp_font = next(p for p, family in attachments if family == FAMILY["gothic"]).read_bytes()
    events = opening_events(jp_font)
    ass_text = opening_script(events)
    mkv = mux_fonts(out, av, "fonts-over-budget", ass_text, attachments)
    m = verify_fonts_over_budget(mkv, events, ass_text, attachments)
    print(f"{mkv.name}: {m['duration']:.1f} s, {m['events']} events (up to {m['peak_events']} at once), "
          f"{m['attachments']} fonts = {m['font_bytes']} bytes (old cap {BUDGET})")
    print(f"  video {m['video_mbps']:.1f} Mbit/s mean, {m['video_peak_mbps']:.1f} peak second, "
          f"largest frame {m['max_frame_bytes']} bytes")
    print("  the budget drops: " + ", ".join(m["dropped"]))
    print(f"\noutput: {out}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("output", type=pathlib.Path, help="directory for fonts, scripts and MKVs")
    ap.add_argument("--fonts-over-budget", action="store_true",
                    help="build only fonts-over-budget.mkv (120 s, ~410 MB, 17 fonts / 83 MB; issue #1328)")
    args = ap.parse_args()
    out = args.output.resolve()
    if ROOT in out.parents or out == ROOT:
        sys.exit("refusing to write into the repository checkout")
    out.mkdir(parents=True, exist_ok=True)
    if args.fonts_over_budget:
        build_fonts_over_budget(out)
        return
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
