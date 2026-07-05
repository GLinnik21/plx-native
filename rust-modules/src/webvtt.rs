//! webvtt — a self-contained, dependency-free (pure `std`) **streaming** WebVTT
//! parser. It exists to turn the body of Plex's
//! `GET /video/:/transcode/universal/subtitles?…&subtitles=auto` endpoint
//! (`text/vtt`, delivered progressively in lock-step with the video transcode) into
//! [`VttCue`]s that the player pushes into the existing `SHARED.sub_cues` store, so
//! the on-screen renderer (`ui::player_hud::draw_subtitles`) is reused unchanged.
//!
//! Two properties make it fit the streaming socket in `player::threads`:
//!   * **Incremental.** [`VttParser::push`] accepts arbitrary byte chunks (as they
//!     arrive from `http_read`) and returns each cue the moment it is *completed*
//!     (terminated by a blank line). Partial lines/cues are buffered internally.
//!   * **UTF-8-boundary safe.** Chunks are buffered as raw bytes and only split on
//!     `\n` (0x0A, which can never appear inside a multi-byte UTF-8 sequence), so a
//!     multi-byte character split across a chunk boundary is never corrupted.
//!
//! Times are returned in **nanoseconds relative to the stream's zero** (i.e. the
//! transcode session's `offset`), matching the units of `player::SubCue`. The caller
//! adds `SHARED.disp_base` to rebase onto the content-time clock `playpos_ns()` uses
//! (see `docs/soft-subs-plan.md`, "Timeline alignment").
//!
//! Scope: the WebVTT subset PMS emits — `WEBVTT` header, cue blocks
//! `HH:MM:SS.mmm --> HH:MM:SS.mmm [settings]` + text lines, `NOTE`/`STYLE`/`REGION`
//! blocks (ignored), optional cue-identifier lines (ignored). Inline cue tags
//! (`<i>`, `<c.class>`, `<v Speaker>`, `<00:00:01.000>`…) are stripped and the common
//! HTML entities decoded, yielding tag-free UTF-8 with `\n`-joined lines.
#![allow(dead_code)]

/// One parsed cue: times in **nanoseconds** (relative to the stream's zero) plus the
/// display text (tag-stripped, entity-decoded, multiple text lines joined with `\n`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VttCue {
    pub start_ns: i64,
    pub end_ns: i64,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// consuming the `WEBVTT` signature + header metadata, until the first blank line
    Header,
    /// between blocks: expecting a cue id, a timing line, or a NOTE/STYLE/REGION keyword
    Idle,
    /// inside a NOTE/STYLE/REGION block: skip everything until the next blank line
    Ignore,
    /// saw a cue-identifier line; the next line is expected to be the timing line
    AfterId,
    /// inside a cue's text: collect lines until the next blank line
    Cue,
}

struct Pending {
    start_ns: i64,
    end_ns: i64,
    lines: Vec<String>,
}

/// Incremental WebVTT parser. Create with [`VttParser::new`], feed body bytes with
/// [`VttParser::push`] (which returns any cues completed by that chunk), and call
/// [`VttParser::finish`] at EOF to flush a trailing cue not followed by a blank line.
pub struct VttParser {
    buf: Vec<u8>,
    bom_checked: bool,
    mode: Mode,
    cur: Option<Pending>,
}

impl Default for VttParser {
    fn default() -> Self {
        Self::new()
    }
}

impl VttParser {
    pub fn new() -> Self {
        VttParser { buf: Vec::new(), bom_checked: false, mode: Mode::Header, cur: None }
    }

    /// Feed a chunk of the streamed response body. Returns every cue **completed** by
    /// this chunk, in document order. Incomplete lines/cues are retained for the next
    /// call. Safe to call with any chunk sizes, including 1 byte at a time.
    pub fn push(&mut self, data: &[u8]) -> Vec<VttCue> {
        self.buf.extend_from_slice(data);
        self.strip_bom();
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=nl).collect();
            let mut line = &line_bytes[..line_bytes.len() - 1]; // drop '\n'
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1]; // drop '\r' (CRLF)
            }
            let s = String::from_utf8_lossy(line).into_owned();
            self.process_line(&s, &mut out);
        }
        out
    }

    /// Flush a trailing cue whose block was not terminated by a blank line (EOF or
    /// connection close). Also emits a cue held only in the internal line buffer if
    /// the last line lacked a newline. Idempotent.
    pub fn finish(&mut self) -> Vec<VttCue> {
        let mut out = Vec::new();
        // process any final line that never got a trailing '\n'
        if !self.buf.is_empty() {
            let mut line = &self.buf[..];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let s = String::from_utf8_lossy(line).into_owned();
            self.buf.clear();
            if !s.is_empty() {
                self.process_line(&s, &mut out);
            }
        }
        if self.mode == Mode::Cue {
            self.flush_cur(&mut out);
            self.mode = Mode::Idle;
        }
        out
    }

    fn strip_bom(&mut self) {
        if self.bom_checked || self.buf.len() < 3 {
            return;
        }
        self.bom_checked = true;
        if self.buf[..3] == [0xEF, 0xBB, 0xBF] {
            self.buf.drain(..3);
        }
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<VttCue>) {
        match self.mode {
            Mode::Header => {
                if line.trim().is_empty() {
                    self.mode = Mode::Idle;
                } else if line.contains("-->") {
                    // tolerate a header with no blank line before the first cue
                    self.mode = Mode::Idle;
                    self.process_idle(line, out);
                }
                // else: header metadata (e.g. "WEBVTT", "Kind: captions") — ignore
            }
            Mode::Ignore => {
                if line.trim().is_empty() {
                    self.mode = Mode::Idle;
                }
            }
            Mode::Idle => self.process_idle(line, out),
            Mode::AfterId => {
                if line.contains("-->") {
                    self.start_cue(line);
                } else {
                    // the previous line was not a cue id after all — re-evaluate this one
                    self.mode = Mode::Idle;
                    self.process_idle(line, out);
                }
            }
            Mode::Cue => {
                if line.trim().is_empty() {
                    self.flush_cur(out);
                    self.mode = Mode::Idle;
                } else if let Some(c) = self.cur.as_mut() {
                    c.lines.push(line.to_string());
                }
            }
        }
    }

    fn process_idle(&mut self, line: &str, _out: &mut Vec<VttCue>) {
        let t = line.trim();
        if t.is_empty() {
            return; // stay Idle
        }
        if line.contains("-->") {
            self.start_cue(line);
            return;
        }
        if is_block_keyword(t, "NOTE") || is_block_keyword(t, "STYLE") || is_block_keyword(t, "REGION") {
            self.mode = Mode::Ignore;
            return;
        }
        // a cue-identifier line: the next line should be the timing line
        self.mode = Mode::AfterId;
    }

    fn start_cue(&mut self, line: &str) {
        match parse_timing(line) {
            Some((s, e)) => {
                self.cur = Some(Pending { start_ns: s, end_ns: e, lines: Vec::new() });
                self.mode = Mode::Cue;
            }
            None => {
                // malformed timing line — skip the rest of this block
                self.mode = Mode::Ignore;
            }
        }
    }

    fn flush_cur(&mut self, out: &mut Vec<VttCue>) {
        if let Some(p) = self.cur.take() {
            let text = clean_text(&p.lines.join("\n"));
            if !text.is_empty() {
                out.push(VttCue { start_ns: p.start_ns, end_ns: p.end_ns, text });
            }
        }
    }
}

/// `true` if `line` (already trimmed) is a `kw` block header: exactly `kw`, or `kw`
/// followed by a space/tab (e.g. `NOTE this is a comment`).
fn is_block_keyword(line: &str, kw: &str) -> bool {
    line == kw
        || line.strip_prefix(kw).map(|r| r.starts_with(' ') || r.starts_with('\t')).unwrap_or(false)
}

/// Parse a `START --> END [settings]` timing line into `(start_ns, end_ns)`.
/// Cue settings after the end timestamp are ignored. Returns `None` if malformed.
pub fn parse_timing(line: &str) -> Option<(i64, i64)> {
    let mut it = line.splitn(2, "-->");
    let left = it.next()?.trim();
    let right = it.next()?.trim();
    let start = parse_timestamp(left)?;
    // the end timestamp is the first whitespace-delimited token; the rest are settings
    let end_tok = right.split_whitespace().next()?;
    let end = parse_timestamp(end_tok)?;
    Some((start, end))
}

/// Parse a WebVTT timestamp (`HH:MM:SS.mmm` or `MM:SS.mmm`, `.` or `,` decimal) into
/// nanoseconds. Returns `None` if the shape is not a valid timestamp.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // split off the fractional part (WebVTT uses '.', SRT '.' too but be lenient re ',')
    let (hms, frac) = match s.find(['.', ',']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [hh, mm, ss] => (parse_u(hh)?, parse_u(mm)?, parse_u(ss)?),
        [mm, ss] => (0i64, parse_u(mm)?, parse_u(ss)?),
        _ => return None,
    };
    if m >= 60 || sec >= 60 {
        return None;
    }
    // fractional part -> milliseconds: take up to 3 digits, pad right to 3 (".5" == 500ms)
    let ms = if frac.is_empty() {
        0
    } else {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut f = frac.to_string();
        f.truncate(3);
        while f.len() < 3 {
            f.push('0');
        }
        f.parse::<i64>().ok()?
    };
    let total_ms = ((h * 60 + m) * 60 + sec) * 1000 + ms;
    Some(total_ms * 1_000_000)
}

/// parse an all-ASCII-digit unsigned field, or None
fn parse_u(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// Strip WebVTT inline cue tags (`<…>`) and decode the common HTML entities, leaving
/// tag-free UTF-8. Interior `\n` (the line join) is preserved. Tags are stripped
/// **before** entities are decoded, so an escaped `&lt;` never looks like a tag.
pub fn clean_text(s: &str) -> String {
    // 1. drop <...> spans (covers <i>, <b>, <c.class>, <v Name>, <00:00:01.000>, </i>…)
    let mut no_tags = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => no_tags.push(c),
            _ => {}
        }
    }
    // 2. decode common entities (&amp; LAST so "&amp;lt;" -> "&lt;" literally)
    let decoded = no_tags
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&lrm;", "")
        .replace("&rlm;", "")
        .replace("&amp;", "&");
    decoded.trim().to_string()
}

/// Convenience: parse a complete WebVTT document in one shot. Equivalent to a single
/// [`VttParser::push`] followed by [`VttParser::finish`].
pub fn parse_all(input: &str) -> Vec<VttCue> {
    let mut p = VttParser::new();
    let mut cues = p.push(input.as_bytes());
    cues.extend(p.finish());
    cues
}

// ============================== tests ==============================
// Host-runnable with `rustc --test` (pure std, no socket, no crate deps). They are
// NOT reachable via `cargo test --lib` in this crate — the staticlib test executable
// can't resolve the SDL/Starfish extern "C" symbols the rest of the crate links to
// — so validate this file standalone:
//     rustc --test --edition 2021 rust-modules/src/webvtt.rs -o /tmp/webvtt_test && /tmp/webvtt_test
#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn ts_hh_mm_ss_mmm() {
        assert_eq!(parse_timestamp("00:00:01.000"), Some(1000 * MS));
        assert_eq!(parse_timestamp("01:02:03.500"), Some(((3600 + 120 + 3) * 1000 + 500) * MS));
        assert_eq!(parse_timestamp("00:00:00.000"), Some(0));
    }

    #[test]
    fn ts_mm_ss_mmm_no_hours() {
        assert_eq!(parse_timestamp("02:03.250"), Some(((120 + 3) * 1000 + 250) * MS));
        assert_eq!(parse_timestamp("00:05.000"), Some(5000 * MS));
    }

    #[test]
    fn ts_comma_and_short_frac() {
        assert_eq!(parse_timestamp("00:00:01,000"), Some(1000 * MS)); // SRT-style comma
        assert_eq!(parse_timestamp("00:00:01.5"), Some(1500 * MS)); // ".5" == 500ms
        assert_eq!(parse_timestamp("00:00:01"), Some(1000 * MS)); // no frac == .000
    }

    #[test]
    fn ts_rejects_garbage() {
        assert_eq!(parse_timestamp("hello"), None);
        assert_eq!(parse_timestamp(""), None);
        assert_eq!(parse_timestamp("00:99:00.000"), None); // minutes >= 60
        assert_eq!(parse_timestamp("00:00:75.000"), None); // seconds >= 60
        assert_eq!(parse_timestamp("00:00:01.abc"), None);
    }

    #[test]
    fn timing_ignores_cue_settings() {
        let (s, e) = parse_timing("00:00:01.000 --> 00:00:04.000 line:80% position:50% align:middle").unwrap();
        assert_eq!(s, 1000 * MS);
        assert_eq!(e, 4000 * MS);
    }

    #[test]
    fn simple_two_cues() {
        let doc = "WEBVTT\n\n\
                   00:00:01.000 --> 00:00:04.000\n\
                   Hello world\n\n\
                   00:00:05.500 --> 00:00:08.000\n\
                   Second cue\n";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], VttCue { start_ns: 1000 * MS, end_ns: 4000 * MS, text: "Hello world".into() });
        assert_eq!(cues[1], VttCue { start_ns: 5500 * MS, end_ns: 8000 * MS, text: "Second cue".into() });
    }

    #[test]
    fn multiline_text_joined_with_newline() {
        let doc = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nline one\nline two\n";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "line one\nline two");
    }

    #[test]
    fn note_style_region_and_ids_ignored() {
        let doc = "WEBVTT\n\
                   Kind: captions\n\
                   Language: en\n\n\
                   NOTE this is a comment\n\
                   spanning two lines\n\n\
                   STYLE\n\
                   ::cue { color: white }\n\n\
                   REGION\n\
                   id:r1 width:40%\n\n\
                   cue-identifier-42\n\
                   00:00:01.000 --> 00:00:02.000\n\
                   Real text\n\n\
                   NOTE trailing note\n";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Real text");
    }

    #[test]
    fn strips_tags() {
        let doc = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n\
                   <v Roger>Hi <i>there</i> <c.yellow>friend</c><00:00:01.500>!\n";
        let cues = parse_all(doc);
        assert_eq!(cues[0].text, "Hi there friend!");
    }

    #[test]
    fn decodes_entities_after_stripping_tags() {
        let doc = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n\
                   Tom &amp; Jerry &lt;3 &nbsp;done &amp;lt;keep&amp;gt;\n";
        let cues = parse_all(doc);
        // &amp;lt; must decode to the literal "&lt;", NOT be re-stripped as a tag
        assert_eq!(cues[0].text, "Tom & Jerry <3  done &lt;keep&gt;");
    }

    #[test]
    fn empty_text_cue_dropped() {
        let doc = "WEBVTT\n\n\
                   00:00:01.000 --> 00:00:02.000\n\
                   <i></i>\n\n\
                   00:00:03.000 --> 00:00:04.000\n\
                   kept\n";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "kept");
    }

    #[test]
    fn crlf_line_endings() {
        let doc = "WEBVTT\r\n\r\n00:00:01.000 --> 00:00:02.000\r\nwith CRLF\r\n";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "with CRLF");
    }

    #[test]
    fn bom_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhi\n");
        let mut p = VttParser::new();
        let mut cues = p.push(&bytes);
        cues.extend(p.finish());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "hi");
    }

    #[test]
    fn malformed_timing_block_skipped() {
        let doc = "WEBVTT\n\n\
                   00:00:01.000 -> 00:00:02.000\n\
                   bad arrow, no cue\n\n\
                   00:00:03.000 --> 00:00:04.000\n\
                   good\n";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "good");
    }

    #[test]
    fn finish_flushes_trailing_cue_without_blank_line() {
        // last cue has no trailing blank line and no trailing newline
        let doc = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nlast line no newline";
        let cues = parse_all(doc);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "last line no newline");
    }

    #[test]
    fn streaming_byte_by_byte_matches_oneshot() {
        let doc = "WEBVTT\n\
                   Kind: captions\n\n\
                   1\n\
                   00:00:01.000 --> 00:00:04.000\n\
                   <i>Hello</i> world\n\n\
                   NOTE skip me\n\n\
                   2\n\
                   00:00:05.500 --> 00:00:08.000 line:90%\n\
                   Second cue line A\n\
                   Second cue line B\n\n\
                   3\n\
                   00:01:00.000 --> 00:01:02.000\n\
                   Tom &amp; Jerry\n";
        let expect = parse_all(doc);
        assert_eq!(expect.len(), 3);

        // feed one byte at a time; the concatenation of push() results (+ finish) must match
        let mut p = VttParser::new();
        let mut got = Vec::new();
        for b in doc.as_bytes() {
            got.extend(p.push(&[*b]));
        }
        got.extend(p.finish());
        assert_eq!(got, expect);
    }

    #[test]
    fn streaming_arbitrary_chunks_match_oneshot() {
        let doc = "WEBVTT\n\n\
                   00:00:01.000 --> 00:00:02.000\nA\n\n\
                   00:00:03.000 --> 00:00:04.000\nB\nC\n\n\
                   00:00:05.000 --> 00:00:06.000\nD\n";
        let expect = parse_all(doc);
        let bytes = doc.as_bytes();
        for chunk in [1usize, 2, 3, 5, 7, 13, 64] {
            let mut p = VttParser::new();
            let mut got = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                let end = (i + chunk).min(bytes.len());
                got.extend(p.push(&bytes[i..end]));
                i = end;
            }
            got.extend(p.finish());
            assert_eq!(got, expect, "chunk size {chunk}");
        }
    }

    #[test]
    fn multibyte_utf8_split_across_chunks() {
        // a cue whose text contains multi-byte chars (é, — em dash, 世界), fed in a
        // way that splits the multi-byte sequences across push() boundaries
        let doc = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\ncafé — 世界\n";
        let expect = parse_all(doc);
        assert_eq!(expect[0].text, "café — 世界");
        let bytes = doc.as_bytes();
        let mut p = VttParser::new();
        let mut got = Vec::new();
        for b in bytes {
            got.extend(p.push(std::slice::from_ref(b)));
        }
        got.extend(p.finish());
        assert_eq!(got, expect);
    }
}
