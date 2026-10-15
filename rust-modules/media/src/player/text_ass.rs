//! Text subtitles as ASS: the pure half of rendering every text track through libass (plan
//! `libass-text-subs` §1.3, §1.4, §1.6, §2.2).
//!
//! What lives here, all of it pure (no libass, no font, no clock, no I/O):
//!
//! - [`convert`]: one cue's text, in one of the [`TextDialect`]s, to the body of an ASS event plus
//!   the cue's numpad alignment. It ports the semantics of the `sub_text` the bespoke caption draw
//!   used (kept in `tests.rs` as the oracle) and then escapes the result for libass, which reads `\N`, `\n`, `\h`, `\{` and `\}` as
//!   escapes, so a literal backslash must never be allowed to form one by accident.
//! - [`header`]: the synthesized `[Script Info]`/`[V4+ Styles]` for the viewer's size and position
//!   ([`TextStyle`](super::text_style::TextStyle)), one style per numpad alignment so the position
//!   of a cue is a property of its Style field.
//! - [`script_from_srt`] / [`script_from_vtt`]: a whole sidecar file to ASS event lines, with the
//!   newest-wins clip applied ([`clip_newest_wins`]) and times written as `H:MM:SS.cc`.
//! - [`event_payload`]: the Matroska packet an embedded cue becomes (what `ass_source::push`
//!   takes).
//!
//! Both switches are wired: `ass_source::push_text` converts every embedded text cue here, and
//! `sidecar::decode` converts a sidecar file with [`script_from_srt`] / [`script_from_vtt`].

use super::text_style::{self, TextStyle};

/// The family every synthesized style names. The shipped Inter Bold answers to it (plan §3.2); a
/// libass without that alias would fall back to the default family, "Inter", and pick Inter Regular.
pub const STYLE_FONT: &str = "PlxSubs";

/// A cue's numpad alignment, 1-9 (2 is the default, bottom centre).
pub type Align = u8;

/// The alignment of a cue that asks for none.
pub const DEFAULT_ALIGN: Align = 2;

/// Where a cue sits vertically. The newest-wins clip only compares cues of one class, and the
/// screen lifts only the bottom class while the transport HUD is up (plan §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlignClass {
    /// Numpad 1-3.
    Bottom,
    /// Numpad 4-6.
    Middle,
    /// Numpad 7-9.
    Top,
}

impl AlignClass {
    /// The class of a numpad alignment; anything outside 1-9 is the default, bottom.
    pub fn of_align(align: Align) -> Self {
        match align {
            4..=6 => Self::Middle,
            7..=9 => Self::Top,
            _ => Self::Bottom,
        }
    }

    /// The class of a Style field, as `Default`/`An1`..`An9` the header defines. Another name
    /// (an authored style) has no class.
    pub fn of_style(name: &str) -> Option<Self> {
        if name == "Default" {
            return Some(Self::Bottom);
        }
        let digit = name.strip_prefix("An")?;
        match digit.as_bytes() {
            [d @ b'1'..=b'9'] => Some(Self::of_align(d - b'0')),
            _ => None,
        }
    }
}

/// The name of the header style for a numpad alignment. `Default` is alignment 2.
pub fn style_name(align: Align) -> &'static str {
    match align {
        1 => "An1",
        3 => "An3",
        4 => "An4",
        5 => "An5",
        6 => "An6",
        7 => "An7",
        8 => "An8",
        9 => "An9",
        _ => "Default",
    }
}

/// The grammar a text track's cues are written in: what the cue text means beyond the common floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextDialect {
    Srt,
    WebVtt,
    MovText,
    Sami,
    RealText,
    MicroDvd,
    Mpl2,
    /// Every other text codec: the common floor only.
    Plain,
}

impl TextDialect {
    /// The dialect of an FFmpeg subtitle codec name, or `None` for a codec that is not text
    /// (`ass`/`ssa` have a renderer of their own; PGS/VobSub/DVB are images).
    pub fn from_codec_name(name: &str) -> Option<Self> {
        Some(match name {
            "subrip" | "srt" => Self::Srt,
            "webvtt" => Self::WebVtt,
            "mov_text" => Self::MovText,
            "sami" => Self::Sami,
            "realtext" => Self::RealText,
            "microdvd" => Self::MicroDvd,
            "mpl2" => Self::Mpl2,
            "text" | "vplayer" | "pjs" | "jacosub" | "subviewer" | "subviewer1" | "stl" => Self::Plain,
            _ => return None,
        })
    }

    fn decodes_entities(self) -> bool {
        matches!(self, Self::WebVtt | Self::Sami | Self::RealText)
    }
}

/// A converted cue: the ASS event text and the numpad alignment it asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Converted {
    pub body: String,
    pub align: Align,
}

impl Converted {
    pub fn class(&self) -> AlignClass {
        AlignClass::of_align(self.align)
    }
}

/// Convert one cue's text. `None` when nothing visible is left, so the caller drops the cue as the
/// bespoke draw skipped an empty one.
pub fn convert(text: &str, dialect: TextDialect) -> Option<Converted> {
    convert_aligned(text, dialect, None)
}

/// [`convert`] with the alignment a cue gets when its own text names none (WebVTT's `line:0`).
pub fn convert_aligned(text: &str, dialect: TextDialect, default_align: Option<Align>) -> Option<Converted> {
    let parsed = Parser::run(text, dialect);
    let pieces = normalize(parsed.pieces)?;
    let align = parsed.align.or(default_align).filter(|a| (1..=9).contains(a)).unwrap_or(DEFAULT_ALIGN);
    Some(Converted { body: render(&pieces), align })
}

/// The Matroska packet an embedded cue becomes: `ReadOrder,Layer,Style,Name,MarginL,MarginR,
/// MarginV,Effect,Text`. Every event margin is 0, so the style's own margins apply.
pub fn event_payload(read_order: u32, align: Align, body: &str) -> String {
    format!("{read_order},0,{},,0,0,0,,{body}", style_name(align))
}

/// How many characters of text a converted body shows: override blocks (`{...}`) count nothing, an
/// escape (`\N`, `\h`, `\{`, `\}`) counts as the one thing it stands for, and the ZWSP that keeps
/// a literal backslash apart from the next letter (see [`convert`]) counts nothing. This is the
/// `len=` of the `sub cue` log line: a length, never the dialogue.
pub fn visible_chars(body: &str) -> usize {
    let mut n = 0;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                }
            }
            '\\' if matches!(chars.peek(), Some('N' | 'n' | 'h' | '{' | '}')) => {
                chars.next();
                n += 1;
            }
            '\u{200B}' => {}
            _ => n += 1,
        }
    }
    n
}

// ---- parsing ----------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Ch(char),
    /// A line break (`\N`).
    Break,
    /// A non-breaking space (`\h`).
    Nbsp,
    /// The inside of an override block, without braces: `\i1`.
    Tag(String),
}

struct Parsed {
    pieces: Vec<Piece>,
    /// The first `\anN` of the cue.
    align: Option<Align>,
}

struct Parser {
    dialect: TextDialect,
    out: Parsed,
    /// Inside a WebVTT `<rt>…</rt>`: ruby text is dropped.
    in_rt: bool,
    /// Nothing but a break or the start of the cue precedes the cursor on its line.
    line_start: bool,
    /// Italic until the end of this line: an MPL2 line that began with `/`, or MicroDVD `{y:i}`
    /// (uppercase `{Y:i}` covers the cue).
    line_italic: bool,
}

impl Parser {
    fn run(text: &str, dialect: TextDialect) -> Parsed {
        let mut p = Parser {
            dialect,
            out: Parsed { pieces: Vec::new(), align: None },
            in_rt: false,
            line_start: true,
            line_italic: false,
        };
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match c {
                '\r' => i += 1,
                '\n' => {
                    p.line_break();
                    i += 1;
                }
                '\\' => match chars.get(i + 1) {
                    Some('N') | Some('n') => {
                        p.line_break();
                        i += 2;
                    }
                    Some('h') => {
                        p.push(Piece::Nbsp);
                        i += 2;
                    }
                    _ => {
                        p.push(Piece::Ch('\\'));
                        i += 1;
                    }
                },
                '{' => match chars[i + 1..].iter().position(|&x| x == '}') {
                    Some(k) => {
                        let inner: String = chars[i + 1..i + 1 + k].iter().collect();
                        p.block(&inner);
                        i += k + 2;
                    }
                    // Unmatched: shown, not swallowed with the rest of the cue.
                    None => {
                        p.push(Piece::Ch('{'));
                        i += 1;
                    }
                },
                '<' => match chars[i + 1..].iter().position(|&x| x == '>') {
                    Some(k) => {
                        let inner: String = chars[i + 1..i + 1 + k].iter().collect();
                        p.tag(&inner);
                        i += k + 2;
                    }
                    // Unmatched: hides the rest of the cue, as the bespoke draw did in every dialect.
                    None => i = chars.len(),
                },
                '&' if dialect.decodes_entities() => {
                    let (piece, used) = entity(&chars[i..]);
                    p.push(piece);
                    i += used;
                }
                '|' if matches!(dialect, TextDialect::MicroDvd | TextDialect::Mpl2) => {
                    p.line_break();
                    i += 1;
                }
                '/' if dialect == TextDialect::Mpl2 && p.line_start => {
                    p.line_italic = true;
                    p.push(Piece::Tag("\\i1".into()));
                    i += 1;
                }
                _ => {
                    p.push(Piece::Ch(c));
                    i += 1;
                }
            }
        }
        p.out
    }

    fn push(&mut self, piece: Piece) {
        if self.in_rt {
            return;
        }
        if !matches!(piece, Piece::Tag(_)) {
            self.line_start = false;
        }
        self.out.pieces.push(piece);
    }

    fn line_break(&mut self) {
        if self.line_italic {
            self.line_italic = false;
            self.push(Piece::Tag("\\i0".into()));
        }
        self.push(Piece::Break);
        if !self.in_rt {
            self.line_start = true;
        }
    }

    /// A matched `{…}` block: an override block, or (MicroDVD) a control code. Only the whitelisted
    /// tags survive, so a text track can never ask for animation or arbitrary placement.
    fn block(&mut self, inner: &str) {
        if self.dialect == TextDialect::MicroDvd {
            if let Some(rest) = inner.strip_prefix("y:").or_else(|| inner.strip_prefix("Y:")) {
                if rest.contains(['i', 'I']) {
                    self.line_italic = inner.starts_with('y');
                    self.push(Piece::Tag("\\i1".into()));
                }
                return;
            }
        }
        for tag in override_tags(inner) {
            if let Some(digit) = tag.strip_prefix("an") {
                if let [d @ b'1'..=b'9'] = digit.as_bytes() {
                    self.out.align.get_or_insert(d - b'0');
                }
                continue;
            }
            if matches!(tag, "i0" | "i1" | "b0" | "b1" | "u0" | "u1" | "s0" | "s1") {
                self.push(Piece::Tag(format!("\\{tag}")));
            }
        }
    }

    /// A matched `<…>`.
    fn tag(&mut self, inner: &str) {
        let inner = inner.trim();
        let closing = inner.starts_with('/');
        let body = inner.trim_start_matches('/').trim();
        let name: String = body
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '/')
            .collect::<String>()
            .to_ascii_lowercase();
        match self.dialect {
            TextDialect::Srt | TextDialect::WebVtt => match name.as_str() {
                "i" | "b" | "u" => self.style_tag(&name, closing),
                "s" if self.dialect == TextDialect::Srt => self.style_tag(&name, closing),
                "font" if self.dialect == TextDialect::Srt => {
                    if closing {
                        self.push(Piece::Tag("\\c".into()));
                    } else if let Some(bgr) = font_color(body) {
                        self.push(Piece::Tag(format!("\\c&H{bgr}&")));
                    }
                }
                "rt" if self.dialect == TextDialect::WebVtt => self.in_rt = !closing,
                _ => {}
            },
            TextDialect::Sami | TextDialect::RealText => {
                if name == "br" {
                    self.line_break();
                }
            }
            _ => {}
        }
    }

    fn style_tag(&mut self, name: &str, closing: bool) {
        self.push(Piece::Tag(format!("\\{name}{}", if closing { 0 } else { 1 })));
    }
}

/// The tags of an override block, each without its leading backslash. A backslash inside
/// parentheses (`\t(\i1)`) belongs to the enclosing tag.
fn override_tags(inner: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (at, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '\\' if depth <= 0 => {
                if let Some(s) = start {
                    tags.push(&inner[s..at]);
                }
                start = Some(at + 1);
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        tags.push(&inner[s..]);
    }
    tags.into_iter().map(str::trim).collect()
}

/// One `&name;` entity at the start of `chars`: the piece and how many characters it used. An
/// unknown or unterminated one is a literal `&`.
fn entity(chars: &[char]) -> (Piece, usize) {
    let end = chars.iter().take(8).position(|&c| c == ';');
    if let Some(end) = end {
        let name: String = chars[1..end].iter().collect();
        let piece = match name.as_str() {
            "amp" => Some(Piece::Ch('&')),
            "lt" => Some(Piece::Ch('<')),
            "gt" => Some(Piece::Ch('>')),
            "quot" => Some(Piece::Ch('"')),
            "apos" => Some(Piece::Ch('\'')),
            "nbsp" => Some(Piece::Nbsp),
            _ => None,
        };
        if let Some(piece) = piece {
            return (piece, end + 1);
        }
    }
    (Piece::Ch('&'), 1)
}

/// The `BBGGRR` of a `<font color=…>` attribute, if it names a colour.
fn font_color(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let after = lower[lower.find("color")? + "color".len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let value = match after.chars().next()? {
        q @ ('"' | '\'') => after[1..].split(q).next()?,
        _ => after.split(|c: char| c.is_whitespace() || c == '>').next()?,
    };
    let value = value.trim();
    let hex = value.strip_prefix('#').unwrap_or(value);
    let rgb: u32 = if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        u32::from_str_radix(hex, 16).ok()?
    } else if hex.len() == 3 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let n = u32::from_str_radix(hex, 16).ok()?;
        let (r, g, b) = ((n >> 8) & 0xF, (n >> 4) & 0xF, n & 0xF);
        (r * 17) << 16 | (g * 17) << 8 | b * 17
    } else {
        named_color(value)?
    };
    Some(format!("{:02X}{:02X}{:02X}", rgb & 0xFF, (rgb >> 8) & 0xFF, rgb >> 16))
}

/// The HTML colour keywords SubRip files use, as `0xRRGGBB`.
fn named_color(name: &str) -> Option<u32> {
    Some(match name {
        "white" => 0xFFFFFF,
        "black" => 0x000000,
        "red" => 0xFF0000,
        "lime" | "green" => 0x00FF00,
        "blue" => 0x0000FF,
        "yellow" => 0xFFFF00,
        "cyan" | "aqua" => 0x00FFFF,
        "magenta" | "fuchsia" => 0xFF00FF,
        "gray" | "grey" => 0x808080,
        "silver" => 0xC0C0C0,
        "maroon" => 0x800000,
        "navy" => 0x000080,
        "olive" => 0x808000,
        "purple" => 0x800080,
        "teal" => 0x008080,
        "orange" => 0xFFA500,
        _ => return None,
    })
}

// ---- normalisation and rendering --------------------------------------------------------------

/// Trim every line, drop empty ones, and collapse runs of breaks, as `caption_wrap` did when it
/// skipped empty segments. Tags are never lost to the trimming: they wait and are emitted before
/// the next visible character. `None` when no visible character remains.
fn normalize(pieces: Vec<Piece>) -> Option<Vec<Piece>> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    let mut deferred: Vec<Piece> = Vec::new();
    let mut pending_break = false;
    let mut line_visible = false;
    let mut any_visible = false;
    let mut any_nbsp = false;
    for piece in pieces {
        match piece {
            Piece::Tag(_) => deferred.push(piece),
            Piece::Ch(c) if c.is_whitespace() => deferred.push(piece),
            Piece::Nbsp => {
                any_nbsp = true;
                deferred.push(piece);
            }
            Piece::Break => {
                if line_visible {
                    // The tags that came before the break belong to the line it ends.
                    out.extend(deferred.drain(..).filter(|d| matches!(d, Piece::Tag(_))));
                    pending_break = true;
                    line_visible = false;
                }
                deferred.retain(|d| matches!(d, Piece::Tag(_)));
            }
            visible => {
                if pending_break {
                    out.push(Piece::Break);
                    pending_break = false;
                }
                for d in deferred.drain(..) {
                    if matches!(d, Piece::Tag(_)) || line_visible {
                        out.push(d);
                    }
                }
                out.push(visible);
                line_visible = true;
                any_visible = true;
            }
        }
    }
    if any_visible {
        Some(out)
    } else {
        // A cue that is only hard spaces is still an event (SAMI's `&nbsp;` clear packets).
        any_nbsp.then(|| vec![Piece::Nbsp])
    }
}

/// Pieces to ASS text. A literal backslash that would form an escape with what follows it (`N`,
/// `n`, `h`, or the `{` of an override block) is followed by U+200B, which is zero-width, present
/// in Inter Bold, and only a break opportunity under libunibreak.
fn render(pieces: &[Piece]) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < pieces.len() {
        match &pieces[i] {
            Piece::Ch('{') => s.push_str("\\{"),
            Piece::Ch('}') => s.push_str("\\}"),
            Piece::Ch('\\') => {
                s.push('\\');
                if matches!(pieces.get(i + 1), Some(Piece::Ch('N' | 'n' | 'h')) | Some(Piece::Tag(_))) {
                    s.push('\u{200B}');
                }
            }
            Piece::Ch(c) => s.push(*c),
            Piece::Break => s.push_str("\\N"),
            Piece::Nbsp => s.push_str("\\h"),
            Piece::Tag(_) => {
                s.push('{');
                while let Some(Piece::Tag(t)) = pieces.get(i) {
                    s.push_str(t);
                    i += 1;
                }
                s.push('}');
                continue;
            }
        }
        i += 1;
    }
    s
}

// ---- header -----------------------------------------------------------------------------------

/// The numpad alignments the header defines a style for, in order, with the style `MarginV`.
fn style_margins(style: &TextStyle) -> [(Align, u32); 9] {
    let bottom = text_style::bottom_margin_v(style.position);
    let top = text_style::TOP_MARGIN_V.round() as u32;
    [(2, bottom), (1, bottom), (3, bottom), (4, 0), (5, 0), (6, 0), (7, top), (8, top), (9, top)]
}

/// `51.49` or `40`: at most two decimals, no trailing zeros.
fn number(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The synthesized header of a text-origin script for the viewer's picks (plan §2.2): PlayRes
/// 1920x1080, `WrapStyle 1`, one style per numpad alignment, ending after the `[Events]` format
/// line so events can be appended.
pub fn header(style: &TextStyle) -> String {
    let scale = style.scale();
    let fs = number(text_style::font_size(scale));
    let margin = text_style::side_margin(scale);
    let mut h = String::from(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 1\n\
         ScaledBorderAndShadow: yes\nYCbCr Matrix: None\n\n[V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
         BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
         BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    );
    for (align, margin_v) in style_margins(style) {
        h.push_str(&format!(
            "Style: {name},{STYLE_FONT},{fs},&H00FFFFFF,&H00FFFFFF,&H26000000,&HFF000000,\
             0,0,0,0,100,100,0,0,1,2,0,{align},{margin},{margin},{margin_v},-1\n",
            name = style_name(align),
        ));
    }
    h.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    h
}

// ---- newest wins ------------------------------------------------------------------------------

/// One cue as the clip sees it: a span in any one unit (ms for embedded cues, ns for a sidecar)
/// and the class it sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipCue {
    pub start: i64,
    pub end: i64,
    pub class: AlignClass,
    /// Arrival order (a Matroska ReadOrder, a sidecar's file index). Cues with one start are
    /// ordered by it, never by their place in the slice, so a caller may build the slice from a
    /// map ordered by end time and still get the newest cue.
    pub seq: u32,
}

/// The newest-wins view of a set of cues (D10): each cue's end is clipped to the start of the next
/// cue of the same class, so a 1-frame overlap, or an end defaulted to start + 4 s, no longer
/// leaves the old text up beside the new. Cues of different classes do not clip each other, so a
/// top sign over bottom dialogue both show.
///
/// The result has one entry per input cue, in input order: the clipped `(start, end)`, or `None`
/// for a cue that is dropped, never emitted, because its clipped duration is not positive (two
/// cues with one start: the one with the higher `seq` is the newer and wins) or it had none to begin
/// with. `cues` may be in any order; equal starts are ordered by `seq`.
pub fn clip_newest_wins(cues: &[ClipCue]) -> Vec<Option<(i64, i64)>> {
    let mut order: Vec<usize> = (0..cues.len()).filter(|&i| cues[i].end > cues[i].start).collect();
    order.sort_by_key(|&i| (cues[i].start, cues[i].seq, i));
    let mut next_start: [Option<i64>; 3] = [None; 3];
    let slot = |c: AlignClass| match c {
        AlignClass::Bottom => 0,
        AlignClass::Middle => 1,
        AlignClass::Top => 2,
    };
    let mut out = vec![None; cues.len()];
    for &i in order.iter().rev() {
        let cue = cues[i];
        let end = next_start[slot(cue.class)].map_or(cue.end, |s| cue.end.min(s));
        if end > cue.start {
            out[i] = Some((cue.start, end));
        }
        next_start[slot(cue.class)] = Some(cue.start);
    }
    out
}

// ---- sidecar files ----------------------------------------------------------------------------

/// `00:01:02,500 --> 00:01:05,000` (SubRip), `01:02.500 --> 01:05.000 line:0` (WebVTT).
pub fn timing_line(line: &str) -> Option<(i64, i64)> {
    timing_line_settings(line).map(|(start, end, _)| (start, end))
}

/// [`timing_line`] and the cue settings that follow the end time (empty for SubRip).
pub fn timing_line_settings(line: &str) -> Option<(i64, i64, &str)> {
    let (a, b) = line.split_once("-->")?;
    let start = timestamp(a.trim())?;
    let b = b.trim_start();
    let end_text = b.split_whitespace().next()?;
    let end = timestamp(end_text)?;
    Some((start, end, b[end_text.len()..].trim()))
}

/// `[H+:]MM:SS[,.]fff` → ns. The fraction is read as written (`.5` is half a second, `.50` too).
pub fn timestamp(s: &str) -> Option<i64> {
    let (clock, frac) = match s.rsplit_once([',', '.']) {
        Some((c, f)) => (c, f),
        None => (s, ""),
    };
    let mut secs: i64 = 0;
    let mut fields = 0;
    for part in clock.split(':') {
        let v: i64 = part.trim().parse().ok().filter(|v| *v >= 0)?;
        secs = secs.checked_mul(60)?.checked_add(v)?;
        fields += 1;
    }
    if !(2..=3).contains(&fields) {
        return None;
    }
    let mut frac_ns: i64 = 0;
    let mut scale: i64 = 100_000_000;
    for ch in frac.chars().take(9) {
        frac_ns += ch.to_digit(10)? as i64 * scale;
        scale /= 10;
    }
    secs.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

/// One cue of a sidecar file before conversion.
struct FileCue {
    start_ns: i64,
    end_ns: i64,
    settings: String,
    text: String,
}

/// Read the cues of a SubRip or WebVTT file: a timing line (one containing `-->`) opens a cue, its
/// text is every line up to the next blank line or timing line, and anything else outside a cue
/// (the counter line, `WEBVTT`, a cue identifier) is not text. A WebVTT NOTE, STYLE or REGION
/// block is skipped whole.
fn file_cues(text: &str) -> Vec<FileCue> {
    let text = text.trim_start_matches('\u{feff}');
    let mut out = Vec::new();
    let mut cur: Option<FileCue> = None;
    let mut skipping = false;
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r');
        let blank = line.trim().is_empty();
        if blank {
            skipping = false;
            out.extend(cur.take().filter(|c| !c.text.is_empty()));
            continue;
        }
        if skipping {
            continue;
        }
        if let Some((start_ns, end_ns, settings)) = timing_line_settings(line) {
            out.extend(cur.take().filter(|c| !c.text.is_empty()));
            cur = Some(FileCue { start_ns, end_ns, settings: settings.to_string(), text: String::new() });
        } else if let Some(c) = cur.as_mut() {
            if !c.text.is_empty() {
                c.text.push('\n');
            }
            c.text.push_str(line);
        } else if ["NOTE", "STYLE", "REGION"].iter().any(|k| line == *k || line.starts_with(&format!("{k} ")) || line.starts_with(&format!("{k}\t"))) {
            skipping = true;
        }
    }
    out.extend(cur.take().filter(|c| !c.text.is_empty()));
    out
}

/// Whether WebVTT cue settings ask for the top of the frame: `line:0`, or a `line:` of 10 % or
/// less. A negative line counts from the bottom and an `align:` is not a placement, so neither is.
fn vtt_line_is_top(settings: &str) -> bool {
    let Some(value) = settings.split_whitespace().find_map(|t| t.strip_prefix("line:")) else {
        return false;
    };
    let value = value.split(',').next().unwrap_or("");
    match value.strip_suffix('%') {
        Some(percent) => percent.parse::<f64>().is_ok_and(|p| (0.0..=10.0).contains(&p)),
        None => value.parse::<f64>().is_ok_and(|n| n == 0.0),
    }
}

/// A sidecar file as ASS: the `Dialogue:` lines, without a header, so a change of the viewer's
/// size or position re-assembles the script without converting the file again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextScript {
    /// Zero or more `Dialogue:` lines, each ending in a newline, in start order.
    pub events: String,
    /// How many cues survived (empty, zero-length and clipped-away cues do not).
    pub cues: usize,
}

impl TextScript {
    /// The complete script for the viewer's picks.
    pub fn assemble(&self, style: &TextStyle) -> String {
        let mut script = header(style);
        script.push_str(&self.events);
        script
    }
}

/// A SubRip file as ASS events (already decoded to UTF-8).
pub fn script_from_srt(text: &str) -> TextScript {
    script_from(text, TextDialect::Srt)
}

/// A WebVTT file as ASS events (already decoded to UTF-8). `line:0` places a cue at the top.
pub fn script_from_vtt(text: &str) -> TextScript {
    script_from(text, TextDialect::WebVtt)
}

fn script_from(text: &str, dialect: TextDialect) -> TextScript {
    let mut cues = Vec::new();
    let mut converted = Vec::new();
    for cue in file_cues(text) {
        if cue.end_ns <= cue.start_ns {
            continue;
        }
        let top = dialect == TextDialect::WebVtt && vtt_line_is_top(&cue.settings);
        let Some(c) = convert_aligned(&cue.text, dialect, top.then_some(8)) else {
            continue;
        };
        cues.push(ClipCue { start: cue.start_ns, end: cue.end_ns, class: c.class(), seq: cues.len() as u32 });
        converted.push(c);
    }
    let clipped = clip_newest_wins(&cues);
    let mut lines: Vec<(i64, usize, i64, String)> = Vec::new();
    for (i, span) in clipped.iter().enumerate() {
        let Some((start, end)) = *span else { continue };
        // Floor to the centisecond (under a frame), then drop what rounding emptied.
        let (start_cs, end_cs) = (start / 10_000_000, end / 10_000_000);
        if end_cs <= start_cs {
            continue;
        }
        let line = format!(
            "Dialogue: 0,{},{},{},,0,0,0,,{}\n",
            centis(start_cs),
            centis(end_cs),
            style_name(converted[i].align),
            converted[i].body
        );
        lines.push((start_cs, i, end_cs, line));
    }
    lines.sort_by_key(|l| (l.0, l.1));
    let count = lines.len();
    TextScript { events: lines.into_iter().map(|l| l.3).collect(), cues: count }
}

/// `H:MM:SS.cc` from centiseconds.
fn centis(cs: i64) -> String {
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6_000 % 60, cs / 100 % 60, cs % 100)
}

#[cfg(test)]
mod tests;
