use super::*;
use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};

const ZW: char = '\u{200B}';

fn body(text: &str, dialect: TextDialect) -> String {
    convert(text, dialect).unwrap_or_else(|| panic!("{text:?} converts to nothing")).body
}

fn plain(text: &str) -> String {
    body(text, TextDialect::Plain)
}

fn assert_rows(dialect: TextDialect, rows: &[(&str, &str)]) {
    for (input, want) in rows {
        assert_eq!(&body(input, dialect), want, "{dialect:?} {input:?}");
    }
}

// ---- the common floor -------------------------------------------------------------------------

#[test]
fn line_breaks_in_every_spelling_become_one_ass_break() {
    assert_eq!(plain("a\\Nb"), "a\\Nb");
    // A literal `\n` must not pass through: WrapStyle 1 reads it as a space, not a break.
    assert_eq!(plain("a\\nb"), "a\\Nb");
    assert_eq!(plain("a\nb"), "a\\Nb");
    assert_eq!(plain("a\r\nb\r\n"), "a\\Nb", "CRLF is one break and \\r is dropped");
    assert_eq!(plain("C:\\new"), "C:\\New", "`\\n` is a break, as the bespoke draw read it");
}

#[test]
fn hard_space_stays_a_hard_space_and_other_backslashes_are_literal() {
    assert_eq!(plain("a\\hb"), "a\\hb");
    assert_eq!(plain("C:\\temp"), "C:\\temp");
    assert_eq!(plain("a\\ b"), "a\\ b");
    assert_eq!(plain("a\\"), "a\\");
    assert_eq!(plain("a\\\\Nb"), "a\\\\Nb", "a literal backslash, then a break");
}

#[test]
fn a_literal_backslash_never_forms_an_escape_with_what_follows() {
    // `\` + `N`/`n`/`h` would be a break, a space and a hard space; `\{` a literal brace.
    assert_eq!(plain("\\x"), "\\x");
    assert_eq!(plain("a\\{\\i1}b"), format!("a\\{ZW}{{\\i1}}b"), "`\\{{` is an escape for a brace");
    assert_eq!(plain("\\{\\i1}x"), format!("\\{ZW}{{\\i1}}x"));
    assert_eq!(plain("C:\\{}"), "C:\\", "the matched empty block is dropped, as the bespoke draw did");
    assert_eq!(plain("C:\\{}n"), format!("C:\\{ZW}n"), "an empty block between them does not hide the pair");
}

#[test]
fn braces_show_unless_they_form_an_override_block() {
    assert_eq!(plain("a{b"), "a\\{b", "an unmatched brace is shown, not swallowed with the cue");
    assert_eq!(plain("a}b"), "a\\}b");
    assert_eq!(plain("{\\pos(1,2)}x"), "x");
    assert_eq!(plain("{a note}x"), "x");
    assert_eq!(plain("\\{}"), "\\", "a backslash then an empty block");
}

#[test]
fn edges_are_trimmed_and_empty_lines_collapse() {
    assert_eq!(plain("  hello  \n\n\n  world  "), "hello\\Nworld");
    assert_eq!(plain("\n\nhello\n"), "hello");
    assert_eq!(plain("a   b"), "a   b", "interior spacing is kept");
    for empty in ["", "   ", "\n\n", "\\N", "{\\i1}", "{}", "\\N \\n", "<i></i>"] {
        assert_eq!(convert(empty, TextDialect::Srt), None, "{empty:?}");
        assert_eq!(convert(empty, TextDialect::Plain), None, "{empty:?}");
    }
    assert_eq!(plain("\\h").as_str(), "\\h", "a whole cue of one hard space stays an event");
    assert_eq!(plain("a\\N\\h\\Nb"), "a\\Nb", "a line of only hard spaces is an empty line");
    assert_eq!(plain("a\\hb"), "a\\hb", "an interior hard space is kept");
}

#[test]
fn plain_and_mov_text_hide_angle_markup_as_the_bespoke_draw_did() {
    for dialect in [TextDialect::Plain, TextDialect::MovText] {
        assert_rows(dialect, &[
            ("<i>x</i>", "x"),
            ("<b>Hi</b> there", "Hi there"),
            ("a <b", "a"),
            ("a < b > c", "a  c"),
            ("{\\an8}x", "x"),
            ("plain", "plain"),
        ]);
    }
}

// ---- overrides and alignment ------------------------------------------------------------------

#[test]
fn only_the_whitelisted_override_tags_survive() {
    assert_eq!(plain("{\\i1}x{\\i0} y"), "{\\i1}x{\\i0} y");
    assert_eq!(plain("{\\i1\\b1}x"), "{\\i1\\b1}x");
    assert_eq!(plain("{\\u1}x {\\s1}y"), "{\\u1}x {\\s1}y");
    assert_eq!(plain("{\\fad(100,200)\\i1}x"), "{\\i1}x");
    assert_eq!(plain("{\\t(0,100,\\i1)}x"), "x", "a tag nested in parentheses is not a top-level tag");
    assert_eq!(plain("{\\move(0,0,5,5)\\k20\\blur3\\c&HFF&\\b700\\i2}x"), "x");
    assert_eq!(plain("{\\p1}m 0 0 l 1 1{\\p0}"), "m 0 0 l 1 1");
}

#[test]
fn the_first_an_tag_wins_and_every_an_is_removed() {
    let cases: [(&str, Align, &str); 8] = [
        ("{\\an8}Top", 8, "Top"),
        ("Top{\\an5}", 5, "Top"),
        ("{\\an8}{\\an2}x", 8, "x"),
        ("{\\an1}a{\\an9}b", 1, "ab"),
        ("{\\an0}x", 2, "x"),
        ("{\\an10}x", 2, "x"),
        ("{\\an7\\i1}x", 7, "{\\i1}x"),
        ("x", 2, "x"),
    ];
    for (input, align, want) in cases {
        let c = convert(input, TextDialect::Srt).unwrap();
        assert_eq!((c.align, c.body.as_str()), (align, want), "{input:?}");
        assert!(!c.body.contains("an"), "no \\an is emitted: {:?}", c.body);
    }
}

#[test]
fn convert_aligned_uses_the_default_only_when_the_text_names_none() {
    let c = convert_aligned("x", TextDialect::WebVtt, Some(8)).unwrap();
    assert_eq!(c.align, 8);
    let c = convert_aligned("{\\an4}x", TextDialect::WebVtt, Some(8)).unwrap();
    assert_eq!(c.align, 4);
    let c = convert_aligned("x", TextDialect::WebVtt, Some(0)).unwrap();
    assert_eq!(c.align, 2, "an out-of-range default is ignored");
}

#[test]
fn alignment_classes() {
    use AlignClass::*;
    let classes: Vec<_> = (1..=9).map(AlignClass::of_align).collect();
    assert_eq!(classes, [Bottom, Bottom, Bottom, Middle, Middle, Middle, Top, Top, Top]);
    assert_eq!(AlignClass::of_style("Default"), Some(Bottom));
    for align in 1..=9 {
        assert_eq!(AlignClass::of_style(style_name(align)), Some(AlignClass::of_align(align)), "{align}");
    }
    assert_eq!(AlignClass::of_style("Sign"), None);
    assert_eq!(AlignClass::of_style("An0"), None);
    assert_eq!(AlignClass::of_style("An10"), None);
    assert_eq!(style_name(2), "Default");
    assert_eq!(style_name(0), "Default");
    assert_eq!(style_name(12), "Default");
}

// ---- dialects ---------------------------------------------------------------------------------

#[test]
fn subrip_markup() {
    assert_rows(TextDialect::Srt, &[
        ("<i>Hello</i> world", "{\\i1}Hello{\\i0} world"),
        ("<I>Hello</I>", "{\\i1}Hello"),
        ("<b>a</b> <u>b</u> <s>c</s>", "{\\b1}a{\\b0} {\\u1}b{\\u0} {\\s1}c"),
        ("<i>nested <b>both</b></i>", "{\\i1}nested {\\b1}both"),
        ("<font color=\"#FF8000\">x</font> y", "{\\c&H0080FF&}x{\\c} y"),
        ("<font color=red>x", "{\\c&H0000FF&}x"),
        ("<font color='#0f0'>x", "{\\c&H00FF00&}x"),
        ("<FONT COLOR=\"#ff0000\" size=3>x</FONT>", "{\\c&H0000FF&}x"),
        ("<font color=\"FFFF00\">x", "{\\c&H00FFFF&}x"),
        ("<font face=\"Arial\" size=\"20\">x</font>", "x"),
        ("<font color=\"nonsense\">x", "x"),
        ("<unknown>x</unknown>", "x"),
        ("<i>one\ntwo</i>", "{\\i1}one\\Ntwo"),
        ("<i>\nfoo", "{\\i1}foo"),
        ("a\n<i>b", "a\\N{\\i1}b"),
        ("a <i>b", "a {\\i1}b"),
        ("a <", "a"),
        ("1 < 2", "1"),
        ("x<3 y", "x"),
        ("&amp;", "&amp;"),
    ]);
}

#[test]
fn subrip_braces_and_html_together() {
    assert_rows(TextDialect::Srt, &[
        ("{\\an8}<i>Top</i>", "{\\i1}Top"),
        ("<i>{\\b1}x", "{\\i1\\b1}x"),
        ("{\\i1}a<i>b", "{\\i1}a{\\i1}b"),
    ]);
    let c = convert("{\\an8}<i>Top</i>", TextDialect::Srt).unwrap();
    assert_eq!(c.align, 8);
}

#[test]
fn webvtt_markup() {
    assert_rows(TextDialect::WebVtt, &[
        ("<v Bob>Hi</v>", "Hi"),
        ("<c.yellow.bg_blue>Hi</c>", "Hi"),
        ("<lang en>Hi</lang>", "Hi"),
        ("a <00:00:01.000>b", "a b"),
        ("<ruby>\u{6f22}<rt>kan</rt></ruby>", "\u{6f22}"),
        ("<ruby>a<rt>one\ntwo</rt>b</ruby>", "ab"),
        ("&amp; &lt;i&gt; &quot;q&quot; &apos; &nbsp;x", "& <i> \"q\" ' \\hx"),
        ("&unknown; & x &amp", "&unknown; & x &amp"),
        ("<i>it</i> <b>bd</b> <u>ul</u>", "{\\i1}it{\\i0} {\\b1}bd{\\b0} {\\u1}ul"),
        ("<s>x</s>", "x"),
        ("<font color=\"red\">x</font>", "x"),
        ("one\ntwo", "one\\Ntwo"),
    ]);
}

#[test]
fn sami_and_realtext_honour_br() {
    for dialect in [TextDialect::Sami, TextDialect::RealText] {
        assert_rows(dialect, &[
            ("Line1<br>Line2", "Line1\\NLine2"),
            ("a<BR/>b", "a\\Nb"),
            ("a<br />b", "a\\Nb"),
            ("a<br><br>b", "a\\Nb"),
            ("<P Class=ENCC>x&nbsp;y</P>", "x\\hy"),
            ("<font color=red>x</font>", "x"),
            ("<i>x</i>", "x"),
            ("&lt;b&gt;", "<b>"),
            ("a <", "a"),
        ]);
    }
}

#[test]
fn microdvd_and_mpl2_pipes_and_italics() {
    assert_rows(TextDialect::MicroDvd, &[
        ("one|two", "one\\Ntwo"),
        ("{y:i}text", "{\\i1}text"),
        ("{Y:i}a|b", "{\\i1}a\\Nb"),
        ("{y:i}a|b", "{\\i1}a{\\i0}\\Nb"),
        ("{y:b}x", "x"),
        ("{c:$0000FF}x", "x"),
        ("{f:Arial}{s:20}x", "x"),
        ("a\nb|c", "a\\Nb\\Nc"),
        ("<i>x</i>", "x"),
    ]);
    assert_rows(TextDialect::Mpl2, &[
        ("/italic line|plain", "{\\i1}italic line{\\i0}\\Nplain"),
        ("plain|/italic", "plain\\N{\\i1}italic"),
        ("/a|/b", "{\\i1}a{\\i0}\\N{\\i1}b"),
        ("a/b", "a/b"),
        ("one|two", "one\\Ntwo"),
    ]);
    // `|` is plain text for every other dialect.
    assert_eq!(body("a|b", TextDialect::Srt), "a|b");
    assert_eq!(body("/a", TextDialect::Srt), "/a");
}

#[test]
fn codec_names_map_to_dialects() {
    use TextDialect::*;
    let table = [
        ("subrip", Srt), ("srt", Srt), ("webvtt", WebVtt), ("mov_text", MovText), ("sami", Sami),
        ("realtext", RealText), ("microdvd", MicroDvd), ("mpl2", Mpl2), ("text", Plain),
        ("vplayer", Plain), ("pjs", Plain), ("jacosub", Plain), ("subviewer", Plain),
        ("subviewer1", Plain), ("stl", Plain),
    ];
    for (name, dialect) in table {
        assert_eq!(TextDialect::from_codec_name(name), Some(dialect), "{name}");
    }
    for not_text in ["ass", "ssa", "hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "dvb_teletext", ""] {
        assert_eq!(TextDialect::from_codec_name(not_text), None, "{not_text}");
    }
}

#[test]
fn non_latin_text_passes_through() {
    let thai = "\u{e09}\u{e31}\u{e19}\u{e44}\u{e21}\u{e48}\u{e40}\u{e04}\u{e22}";
    assert_eq!(plain(thai), thai);
    let arabic = "\u{644}\u{645} \u{623}\u{643}\u{646}";
    assert_eq!(plain(arabic), arabic);
    assert_eq!(plain("\u{6211}\u{4ece}\\N\u{6765}"), "\u{6211}\u{4ece}\\N\u{6765}");
}

/// What libass 0.17.5 would draw for `body`, as a model of `ass_get_next_char` and the override
/// parser: `\N` a break, `\n` a SPACE (WrapStyle 1), `\h` a hard space, `\{` and `\}` literal
/// braces, `{…}` an override block, U+200B nothing.
fn libass_reads(body: &str) -> Vec<char> {
    const BREAK: char = '\u{1}';
    const NBSP: char = '\u{a0}';
    let chars: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match (chars[i], chars.get(i + 1)) {
            ('\\', Some('N')) => (out.push(BREAK), i += 2).1,
            ('\\', Some('n')) => (out.push(' '), i += 2).1,
            ('\\', Some('h')) => (out.push(NBSP), i += 2).1,
            ('\\', Some('{')) => (out.push('{'), i += 2).1,
            ('\\', Some('}')) => (out.push('}'), i += 2).1,
            ('{', _) => {
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                i += 1;
            }
            ('\u{200B}', _) => i += 1,
            (c, _) => (out.push(c), i += 1).1,
        }
    }
    out
}

#[test]
fn what_libass_reads_is_what_the_cue_said() {
    // Every string over a backslash-heavy alphabet, against a model of the cue's meaning: `\N`
    // and `\n` are breaks, `\h` a hard space, any other backslash a literal one.
    const BREAK: char = '\u{1}';
    let alphabet = ['\\', 'N', 'n', 'h', 'x'];
    let mut checked = 0;
    for len in 1..=6usize {
        for n in 0..alphabet.len().pow(len as u32) {
            let input: String = (0..len).map(|k| alphabet[n / alphabet.len().pow(k as u32) % alphabet.len()]).collect();
            let chars: Vec<char> = input.chars().collect();
            let mut want: Vec<char> = Vec::new();
            let mut i = 0;
            while i < chars.len() {
                match (chars[i], chars.get(i + 1)) {
                    ('\\', Some('N' | 'n')) => (want.push(BREAK), i += 2).1,
                    ('\\', Some('h')) => (want.push('\u{a0}'), i += 2).1,
                    (c, _) => (want.push(c), i += 1).1,
                }
            }
            while want.first() == Some(&BREAK) {
                want.remove(0);
            }
            while want.last() == Some(&BREAK) {
                want.pop();
            }
            want.dedup_by(|a, b| *a == BREAK && *b == BREAK);
            // A hard space at a line's edge is edge whitespace; a cue of nothing else is one event.
            let had_nbsp = want.contains(&'\u{a0}');
            let lines: Vec<Vec<char>> = want
                .split(|&c| c == BREAK)
                .map(|l| {
                    let a = l.iter().position(|&c| c != '\u{a0}').unwrap_or(l.len());
                    let b = l.iter().rposition(|&c| c != '\u{a0}').map_or(a, |k| k + 1);
                    l[a..b].to_vec()
                })
                .filter(|l| !l.is_empty())
                .collect();
            want = lines.join(&BREAK);
            if want.is_empty() && had_nbsp {
                want = vec!['\u{a0}'];
            }
            let got = convert(&input, TextDialect::Plain);
            if want.is_empty() {
                assert_eq!(got, None, "{input:?}");
                continue;
            }
            let got = got.unwrap_or_else(|| panic!("{input:?} converted to nothing"));
            assert_eq!(libass_reads(&got.body), want, "{input:?} -> {:?}", got.body);
            checked += 1;
        }
    }
    assert!(checked > 10_000, "{checked}");
}

#[test]
fn braces_survive_libass_reading() {
    for (input, want) in [("a{b", "a{b"), ("a}b", "a}b"), ("{{x}", "")] {
        let got = convert(input, TextDialect::Plain).map(|c| libass_reads(&c.body).into_iter().collect::<String>());
        assert_eq!(got.as_deref().unwrap_or(""), want, "{input:?}");
    }
}

#[test]
fn plain_text_matches_the_bespoke_sub_text_over_every_small_string() {
    // `sub_text` (the draw this replaced, kept below as the oracle) is the reference for the floor: every string over its
    // special characters must read the same once the ASS escapes are undone and each line is
    // trimmed. Strings with an unmatched `{` are skipped: showing it is the deliberate change.
    fn unmatched_brace(chars: &[char]) -> bool {
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '{' {
                match chars[i + 1..].iter().position(|&c| c == '}') {
                    Some(k) => i += k + 1,
                    None => return true,
                }
            }
            i += 1;
        }
        false
    }
    fn lines(s: &str) -> Vec<String> {
        s.split('\n').map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
    }
    fn undo(body: &str) -> String {
        libass_reads(body).into_iter().map(|c| match c {
            '\u{1}' => '\n',
            '\u{a0}' => ' ',
            c => c,
        }).collect()
    }
    let alphabet = ['\\', 'N', 'n', 'h', 'x', ' ', '<', '>', '{', '}', '\n'];
    let mut compared = 0;
    for len in 1..=5usize {
        for n in 0..alphabet.len().pow(len as u32) {
            let chars: Vec<char> = (0..len).map(|k| alphabet[n / alphabet.len().pow(k as u32) % alphabet.len()]).collect();
            if unmatched_brace(&chars) {
                continue;
            }
            let input: String = chars.iter().collect();
            let want = lines(&sub_text(input.as_bytes()));
            let got = convert(&input, TextDialect::Plain).map(|c| lines(&undo(&c.body))).unwrap_or_default();
            assert_eq!(got, want, "{input:?}");
            compared += 1;
        }
    }
    assert!(compared > 100_000, "{compared}");
}

/// The sidecar reader's own cases (`sidecar.rs` tests), through the new converter.
#[test]
fn the_sidecar_readers_cases_survive_the_conversion() {
    let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Hello</i>\r\nthere\r\n\r\n\
               2\r\n00:01:00,250 --> 00:01:02,000\r\n{\\an8}Second line\r\n";
    assert_eq!(
        script_from_srt(srt).events,
        "Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,{\\i1}Hello{\\i0}\\Nthere\n\
         Dialogue: 0,0:01:00.25,0:01:02.00,An8,,0,0,0,,Second line\n"
    );
    let vtt = "WEBVTT\n\nNOTE made by hand\n\n01:02.500 --> 01:05.000 line:0 position:50%\nHi\n\n\
               1:00:00.000 --> 1:00:01.000\nAn hour in\n";
    assert_eq!(
        script_from_vtt(vtt).events,
        "Dialogue: 0,0:01:02.50,0:01:05.00,An8,,0,0,0,,Hi\n\
         Dialogue: 0,1:00:00.00,1:00:01.00,Default,,0,0,0,,An hour in\n"
    );
    let unsorted = "2\n00:00:10,000 --> 00:00:11,000\nlater\n\n1\n00:00:01,000 --> 00:00:02,000\nsooner\n\n\
                    3\n00:00:05,000 --> 00:00:04,000\nbackwards\n\n4\n00:00:20,000 --> 00:00:21,000\n<i></i>\n";
    let script = script_from_srt(unsorted);
    let texts: Vec<&str> = script.events.lines().map(|l| l.rsplit(",,").next().unwrap()).collect();
    assert_eq!(texts, ["sooner", "later"]);
    for garbage in ["<html><body>501 Not Implemented</body></html>", "", "1\n99:99:99,999 --> nonsense\ntext\n"] {
        assert_eq!(script_from_srt(garbage).cues, 0, "{garbage:?}");
        assert_eq!(script_from_vtt(garbage).cues, 0, "{garbage:?}");
    }
}

// ---- header -----------------------------------------------------------------------------------

fn style_lines(header: &str) -> Vec<&str> {
    header.lines().filter(|l| l.starts_with("Style: ")).collect()
}

#[test]
fn header_matches_the_plan() {
    let h = header(&TextStyle::default());
    assert!(h.starts_with("[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 1\nScaledBorderAndShadow: yes\n"));
    assert!(h.ends_with("[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"));
    let lines = style_lines(&h);
    assert_eq!(lines.len(), 9);
    assert_eq!(
        lines[0],
        "Style: Default,PlxSubs,51.49,&H00FFFFFF,&H00FFFFFF,&H26000000,&HFF000000,0,0,0,0,100,100,0,0,1,2,0,2,580,580,97,-1"
    );
    assert!(h.contains("Style: An8,PlxSubs,51.49,&H00FFFFFF,&H00FFFFFF,&H26000000,&HFF000000,0,0,0,0,100,100,0,0,1,2,0,8,580,580,100,-1"));
    assert!(h.contains(",0,0,1,2,0,5,580,580,0,-1\n"), "Middle ignores MarginV");
}

#[test]
fn every_style_name_a_cue_can_name_exists_in_the_header() {
    for style in [TextStyle::default(), TextStyle { size: SubtitleSize::ExtraLarge, position: SubtitlePosition::High }] {
        let h = header(&style);
        for align in 1..=9 {
            let name = style_name(align);
            let line = h
                .lines()
                .find(|l| l.starts_with(&format!("Style: {name},")))
                .unwrap_or_else(|| panic!("libass would fall back to Default for {name}"));
            let fields: Vec<&str> = line.trim_start_matches("Style: ").split(',').collect();
            assert_eq!(fields[18], align.to_string(), "{name} Alignment");
        }
        let format = h.lines().find(|l| l.starts_with("Format: Name")).unwrap();
        for line in style_lines(&h) {
            assert_eq!(line.trim_start_matches("Style: ").split(',').count(), format.split(',').count(), "{line}");
        }
    }
}

#[test]
fn header_follows_the_viewers_size_and_position() {
    let h = header(&TextStyle { size: SubtitleSize::ExtraLarge, position: SubtitlePosition::High });
    let default = style_lines(&h)[0].trim_start_matches("Style: ").split(',').map(str::to_string).collect::<Vec<_>>();
    assert_eq!(default[2], "77.23", "36 * 1.5 * 1.4302");
    assert_eq!((default[19].as_str(), default[20].as_str(), default[21].as_str()), ("390", "390", "417"));
    let an8 = h.lines().find(|l| l.starts_with("Style: An8,")).unwrap();
    assert!(an8.contains(",8,390,390,100,-1"), "the top margin is independent of Position: {an8}");
    let an1 = h.lines().find(|l| l.starts_with("Style: An1,")).unwrap();
    assert!(an1.contains(",1,390,390,417,-1"), "{an1}");
    let small = header(&TextStyle { size: SubtitleSize::Small, position: SubtitlePosition::Middle });
    assert!(style_lines(&small)[0].contains(",40.16,"), "{}", style_lines(&small)[0]);
    assert!(style_lines(&small)[0].contains(",2,664,664,257,-1"));
}

#[test]
fn header_numbers_have_no_trailing_zeros() {
    assert_eq!(number(40.0), "40");
    assert_eq!(number(51.486), "51.49");
    assert_eq!(number(77.5), "77.5");
}

#[test]
fn event_payload_is_a_matroska_packet() {
    assert_eq!(event_payload(7, 2, "Hi"), "7,0,Default,,0,0,0,,Hi");
    assert_eq!(event_payload(0, 8, "a\\Nb"), "0,0,An8,,0,0,0,,a\\Nb");
    // The text field may itself hold commas.
    assert_eq!(event_payload(1, 4, "a,b"), "1,0,An4,,0,0,0,,a,b");
}

// ---- newest wins ------------------------------------------------------------------------------

fn cue(start: i64, end: i64, class: AlignClass) -> ClipCue {
    ClipCue { start, end, class, seq: 0 }
}

#[test]
fn a_one_frame_overlap_is_clipped_to_the_next_start() {
    let b = AlignClass::Bottom;
    let got = clip_newest_wins(&[cue(0, 2000, b), cue(1960, 4000, b)]);
    assert_eq!(got, [Some((0, 1960)), Some((1960, 4000))]);
}

#[test]
fn a_defaulted_end_no_longer_outlives_the_next_cue() {
    let b = AlignClass::Bottom;
    // start + 4 s, with the next line arriving 3 s in.
    let got = clip_newest_wins(&[cue(0, 4000, b), cue(3000, 7000, b)]);
    assert_eq!(got, [Some((0, 3000)), Some((3000, 7000))]);
}

#[test]
fn cues_with_one_start_keep_only_the_newest() {
    let b = AlignClass::Bottom;
    assert_eq!(clip_newest_wins(&[cue(0, 5, b), cue(0, 3, b)]), [None, Some((0, 3))]);
    assert_eq!(clip_newest_wins(&[cue(0, 3, b), cue(0, 5, b)]), [None, Some((0, 5))]);
    // Three at one start: only the last survives.
    assert_eq!(clip_newest_wins(&[cue(1, 9, b), cue(1, 8, b), cue(1, 7, b)]), [None, None, Some((1, 7))]);
}

#[test]
fn different_classes_do_not_clip_each_other() {
    use AlignClass::*;
    let got = clip_newest_wins(&[cue(0, 10, Top), cue(2, 4, Bottom), cue(3, 8, Middle)]);
    assert_eq!(got, [Some((0, 10)), Some((2, 4)), Some((3, 8))]);
    // A cue of another class between two of one class changes nothing for them.
    let got = clip_newest_wins(&[cue(0, 10, Bottom), cue(2, 3, Top), cue(6, 9, Bottom)]);
    assert_eq!(got, [Some((0, 6)), Some((2, 3)), Some((6, 9))]);
}

#[test]
fn a_gap_leaves_the_end_alone_and_an_invalid_cue_clips_nothing() {
    let b = AlignClass::Bottom;
    assert_eq!(clip_newest_wins(&[cue(0, 2, b), cue(5, 6, b)]), [Some((0, 2)), Some((5, 6))]);
    let got = clip_newest_wins(&[cue(0, 10, b), cue(4, 4, b), cue(6, 3, b)]);
    assert_eq!(got, [Some((0, 10)), None, None], "a zero or negative cue is dropped and does not clip");
    assert_eq!(clip_newest_wins(&[]), Vec::<Option<(i64, i64)>>::new());
}

#[test]
fn clip_takes_cues_in_any_order_and_answers_in_input_order() {
    let b = AlignClass::Bottom;
    let got = clip_newest_wins(&[cue(3000, 7000, b), cue(0, 4000, b), cue(1000, 1500, b)]);
    assert_eq!(got, [Some((3000, 7000)), Some((0, 1000)), Some((1000, 1500))]);
}

#[test]
fn clip_ties_follow_seq_not_slice_position() {
    // A map keyed (start, end) hands the newer, shorter cue first; `seq` still says which arrived last.
    let b = AlignClass::Bottom;
    let old = ClipCue { start: 0, end: 3000, class: b, seq: 1 };
    let new = ClipCue { start: 0, end: 2000, class: b, seq: 2 };
    assert_eq!(clip_newest_wins(&[new, old]), [Some((0, 2000)), None]);
    assert_eq!(clip_newest_wins(&[old, new]), [None, Some((0, 2000))]);
}

// ---- files ------------------------------------------------------------------------------------

#[test]
fn timing_lines_and_timestamps() {
    const S: i64 = 1_000_000_000;
    assert_eq!(timing_line("00:00:01,000 --> 00:00:03,500"), Some((S, 3 * S + S / 2)));
    assert_eq!(timing_line("01:02.500 --> 01:05.000 line:0 align:start"), Some((62 * S + S / 2, 65 * S)));
    assert_eq!(timing_line("not a timing line"), None);
    assert_eq!(timing_line("1 --> 2"), None);
    assert_eq!(timing_line_settings("00:01.000 --> 00:02.000 line:0 align:start"), Some((S, 2 * S, "line:0 align:start")));
    assert_eq!(timing_line_settings("00:01.000 --> 00:02.000"), Some((S, 2 * S, "")));
    assert_eq!(timestamp("1:02:03.250"), Some(3723 * S + S / 4));
    assert_eq!(timestamp("12"), None);
    assert_eq!(timestamp("00:00:01,000"), Some(S));
    assert_eq!(timestamp("00:01.5"), Some(S + S / 2));
    assert_eq!(timestamp("a:b:c"), None);
    assert_eq!(timestamp("1:2:3:4"), None);
    assert_eq!(timestamp("-1:00:00,000"), None);
}

#[test]
fn srt_file_to_dialogue_lines() {
    let srt = "1\r\n00:00:01,000 --> 00:00:03,000\r\nHello\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,500\r\n<i>World</i>\r\nsecond line\r\n";
    let script = script_from_srt(srt);
    assert_eq!(script.cues, 2);
    assert_eq!(
        script.events,
        "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello\n\
         Dialogue: 0,0:00:04.00,0:00:05.50,Default,,0,0,0,,{\\i1}World{\\i0}\\Nsecond line\n"
    );
}

#[test]
fn srt_overlap_is_clipped_and_times_floor_to_the_centisecond() {
    let srt = "1\n00:00:01,000 --> 00:00:03,000\nHello\n\n2\n00:00:02,999 --> 00:00:04,000\nWorld\n";
    let script = script_from_srt(srt);
    assert_eq!(
        script.events,
        "Dialogue: 0,0:00:01.00,0:00:02.99,Default,,0,0,0,,Hello\n\
         Dialogue: 0,0:00:02.99,0:00:04.00,Default,,0,0,0,,World\n",
        "2.999 floors to 2.99 for both, so the clip and the new start agree"
    );
    let srt = "1\n01:02:03,459 --> 01:02:04,000\nHour\n";
    assert_eq!(script_from_srt(srt).events, "Dialogue: 0,1:02:03.45,1:02:04.00,Default,,0,0,0,,Hour\n");
}

#[test]
fn a_cue_rounded_or_clipped_to_nothing_is_dropped() {
    // Two cues starting within one 10 ms step: the first would last 4 ms.
    let srt = "1\n00:00:01,001 --> 00:00:03,000\nfirst\n\n2\n00:00:01,005 --> 00:00:03,000\nsecond\n";
    let script = script_from_srt(srt);
    assert_eq!(script.cues, 1);
    assert_eq!(script.events, "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,second\n");
    // A cue that lasts under a centisecond on its own.
    let srt = "1\n00:00:01,000 --> 00:00:01,009\nblink\n\n2\n00:00:02,000 --> 00:00:03,000\nok\n";
    assert_eq!(script_from_srt(srt).cues, 1);
    // Same start: the later one in the file wins, the earlier one is never emitted.
    let srt = "1\n00:00:01,000 --> 00:00:05,000\nold\n\n2\n00:00:01,000 --> 00:00:03,000\nnew\n";
    let script = script_from_srt(srt);
    assert_eq!(script.events, "Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,new\n");
}

#[test]
fn empty_and_backwards_cues_are_dropped() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\n<i></i>\n\n2\n00:00:05,000 --> 00:00:04,000\nbackwards\n\n3\n00:00:06,000 --> 00:00:07,000\nkept\n";
    let script = script_from_srt(srt);
    assert_eq!(script.cues, 1);
    assert!(script.events.contains("kept"));
    assert_eq!(script_from_srt("").cues, 0);
    assert_eq!(script_from_srt("not a subtitle file").events, "");
}

#[test]
fn a_byte_order_mark_and_unsorted_cues_are_handled() {
    let srt = "\u{feff}1\n00:00:05,000 --> 00:00:06,000\nlate\n\n2\n00:00:01,000 --> 00:00:02,000\nearly\n";
    let script = script_from_srt(srt);
    assert_eq!(script.cues, 2);
    let early = script.events.find("early").unwrap();
    let late = script.events.find("late").unwrap();
    assert!(early < late, "events are in start order");
}

#[test]
fn sidecar_alignments_pick_styles_and_both_classes_show() {
    let srt = "1\n00:00:01,000 --> 00:00:09,000\n{\\an8}SIGN\n\n2\n00:00:02,000 --> 00:00:04,000\nDialogue\n\n3\n00:00:03,000 --> 00:00:05,000\n{\\an5}Middle\n";
    let script = script_from_srt(srt);
    assert_eq!(
        script.events,
        "Dialogue: 0,0:00:01.00,0:00:09.00,An8,,0,0,0,,SIGN\n\
         Dialogue: 0,0:00:02.00,0:00:04.00,Default,,0,0,0,,Dialogue\n\
         Dialogue: 0,0:00:03.00,0:00:05.00,An5,,0,0,0,,Middle\n"
    );
}

#[test]
fn webvtt_file_skips_blocks_and_places_line_zero_at_the_top() {
    let vtt = "\u{feff}WEBVTT - title\nKind: captions\n\nNOTE this is a comment\n00:00:09.000 --> 00:00:10.000\nnot a cue\n\nSTYLE\n::cue { color: red }\n\n\
        intro\n00:00:01.000 --> 00:00:02.000 line:0\nTop &amp; <b>bold</b>\n\n\
        00:00:03.000 --> 00:00:04.000 line:5% align:start\nstill top\n\n\
        00:00:05.000 --> 00:00:06.000 line:50%\nmiddle line\n\n\
        00:00:07.000 --> 00:00:08.000 line:-1\nbottom\n\n\
        00:01.000 --> 00:02.000\nshort form\n";
    let script = script_from_vtt(vtt);
    assert_eq!(
        script.events,
        "Dialogue: 0,0:00:01.00,0:00:02.00,An8,,0,0,0,,Top & {\\b1}bold\n\
         Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,short form\n\
         Dialogue: 0,0:00:03.00,0:00:04.00,An8,,0,0,0,,still top\n\
         Dialogue: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,middle line\n\
         Dialogue: 0,0:00:07.00,0:00:08.00,Default,,0,0,0,,bottom\n",
        "the NOTE and STYLE blocks are not cues and `line:0` is the top"
    );
    assert_eq!(script.cues, 5);
}

#[test]
fn vtt_line_settings() {
    for top in ["line:0", "line:0%", "line:10%", "line:5.5% align:start", "align:start line:0", "line:0,start"] {
        assert!(vtt_line_is_top(top), "{top}");
    }
    for not in ["", "line:1", "line:-1", "line:11%", "line:50%", "position:0", "align:start", "line:", "line:x"] {
        assert!(!vtt_line_is_top(not), "{not}");
    }
}

#[test]
fn srt_ignores_vtt_settings() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000 line:0\ntext\n";
    assert!(script_from_srt(srt).events.contains(",Default,"));
}

#[test]
fn assemble_is_header_then_events_and_follows_the_picks() {
    let script = script_from_srt("1\n00:00:01,000 --> 00:00:02,000\nHi\n");
    let medium = script.assemble(&TextStyle::default());
    assert!(medium.starts_with(&header(&TextStyle::default())));
    assert!(medium.ends_with("Hi\n"));
    let large = script.assemble(&TextStyle { size: SubtitleSize::Large, position: SubtitlePosition::High });
    assert_ne!(medium, large);
    assert!(large.ends_with(&script.events), "restyling re-uses the converted events");
}

#[test]
fn assembled_scripts_declare_each_dialogue_field() {
    let script = script_from_srt("1\n00:00:01,000 --> 00:00:02,000\nHi, there\n").assemble(&TextStyle::default());
    let format = script.lines().find(|l| l.starts_with("Format: Layer")).unwrap();
    let dialogue = script.lines().find(|l| l.starts_with("Dialogue: ")).unwrap();
    let fields = format.split(',').count();
    assert_eq!(dialogue.splitn(fields, ',').count(), fields);
    assert_eq!(dialogue.splitn(fields, ',').last(), Some("Hi, there"), "commas in the text stay in the text field");
}

// ---- visible_chars: the `len=` of the `sub cue` log line ---------------------------------------

#[test]
fn visible_chars_counts_what_the_cue_shows() {
    for (body, want) in [
        ("", 0),
        ("Hello", 5),
        ("{\\i1}Hello{\\i0}", 5),
        ("two\\Nlines", 9),
        ("a\\hb", 3),
        ("a\\{b\\}", 4),
        ("你好，世界", 5),
        ("{\\an8", 0),
    ] {
        assert_eq!(visible_chars(body), want, "{body:?}");
    }
    let escaped = body("C:\\new", TextDialect::Plain);
    assert!(visible_chars(&escaped) >= 3);
    assert_eq!(visible_chars(&body("a\\", TextDialect::Plain)), 2, "a trailing backslash shows");
    assert_eq!(visible_chars("a\\\u{200B}Nb"), 4, "the ZWSP that keeps `\\N` apart is not shown");
}

/// The plain-text reduction the bespoke caption draw used (markup stripped, line breaks
/// normalized), kept here only as the oracle the converter is graded against below.
fn sub_text(payload: &[u8]) -> String {
    let s = String::from_utf8_lossy(payload);
    let mut out = String::with_capacity(s.len());
    let mut ch = s.chars().peekable();
    while let Some(c) = ch.next() {
        match c {
            '<' => {
                while let Some(x) = ch.next() {
                    if x == '>' {
                        break;
                    }
                }
            } // <i></i>
            '{' => {
                while let Some(x) = ch.next() {
                    if x == '}' {
                        break;
                    }
                }
            } // {\an8}
            '\\' => match ch.peek() {
                Some('N') | Some('n') => {
                    ch.next();
                    out.push('\n');
                }
                Some('h') => {
                    ch.next();
                    out.push(' ');
                }
                _ => out.push('\\'),
            },
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}
