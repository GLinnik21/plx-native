//! **The SVG subset a HUD asset may use**, checked by a Rust tokenizer BEFORE the bytes reach nanosvg:
//! `src/svg.c` is a C parser and a package is attacker-controlled input.
//!
//! An asset is a MASK (`ui/src/icons.rs:9-10`: only alpha survives; the app tints it per state), so the
//! subset is the icon contract and nothing more: `<svg viewBox>` holding `<path d>`.
//!
//! **Policy** (the module's contract; [`check_asset`] enforces it and reports every violation it finds):
//! * Elements: `svg` and `path` only. Anything else is `HUD-E0701`; a gradient element
//!   (`linearGradient`, `radialGradient`, `stop`) carries the hint "HUD assets are alpha masks tinted by
//!   the app; a gradient would be discarded, so draw the mark in one colour".
//! * Attributes: `viewBox` (required), `xmlns` (exactly `http://www.w3.org/2000/svg`) and `fill` (the
//!   mask white `#ffffff` only) on `svg`; `d` (required), `fill` (mask white only) and `fill-rule` on
//!   `path`. Any other attribute, and any other `fill`, is `HUD-E0702` (a colour other than mask-white,
//!   `stroke*`, `opacity`, `style`, `class`, `transform`) with the hint "assets are masks tinted per
//!   state, so colour would be dropped". Mask-white is accepted because every icon under
//!   `assets/icons` carries `fill='#ffffff'`.
//! * Syntax the subset never admits is `HUD-E0703`: entities (`&`), `<!DOCTYPE`, CDATA, processing
//!   instructions, `href`/`xlink`, `url(`, `data:`, `://`, `<text>`'s and `<style>`'s content cannot be
//!   reached because those elements are already `HUD-E0701`.
//! * A malformed document (unbalanced tags, no `<svg viewBox>`, a `viewBox` that is not four finite
//!   numbers with positive width and height, a `d` that is not path data) is `HUD-E0704`.
//! * More than [`MAX_ASSET_BYTES`] bytes is `HUD-E0705`; more than [`MAX_PATHS_PER_ASSET`] `<path>`
//!   elements is `HUD-E0706`.
//!
//! Every finding's pointer is the asset string's own, the `pointer` argument (`/assets/<id>`).
//!
//! ## How the tokenizer applies it
//!
//! * **Order.** The size bound is checked first and alone: an over-long source yields exactly one
//!   `HUD-E0705` and is never scanned. Otherwise the document is scanned once, left to right, and every
//!   `E0701`/`E0702` is collected. `E0703` and `E0704` are SYNTAX findings: the ones that leave the
//!   scanner unsure what comes next (unterminated tag, mismatched close, DOCTYPE) stop the scan, so a
//!   later finding is never a guess. A finding that names a bad VALUE (an entity, a bad `d`) does not.
//! * **Prolog and comments.** An optional BOM and whitespace, an optional leading `<?xml ... ?>`
//!   declaration, and XML comments anywhere (real icons carry long ones) are skipped. A comment ends
//!   at the first `-->`; `<!-->` and `<!--->` are refused, because HTML-style parsers close those at
//!   once, and so is a comment with a `<` after a `>` in its body: nanosvg ends any `<!` construct at
//!   the first `>` and reads the rest as content, so that `<` starts an element the checker never saw
//!   (a `>` alone, as in prose that mentions `<path>`, leaves only text). A scanner that disagrees about
//!   where a comment ends is how an element gets smuggled.
//!   Any other `<?` is a processing instruction, any other `<!` a declaration: `HUD-E0703`.
//! * **Quotes.** Attribute values may use either quote; an unquoted value, a repeated attribute or a
//!   missing separator is `HUD-E0704`. A NUL byte is `HUD-E0704` (a C string would end there).
//! * **Elements that are already refused** (`HUD-E0701`) are not checked against the attribute
//!   allow-list, only for the `HUD-E0703` references: one finding per mistake, not five for a `<rect>`.
//! * **`fill`.** A paint attribute that is not mask-white (`none`, a colour, `url(#g)`) is `HUD-E0702`
//!   even when it is a reference: the author's mistake is the colour. A reference in any other
//!   attribute (`clip-path="url(#c)"`, `href`) is `HUD-E0703`.
//! * **No path at all** is `HUD-E0704`, except when an `HUD-E0701` was already reported (the author is
//!   about to convert those shapes, and "no path" would only repeat it).
//! * **Repeats.** The same finding about the same thing is reported once, at its first byte offset
//!   (which the message carries); two paths with two different bad fills are two findings.
//!
//! The scanner works on bytes. Every delimiter it stops at is ASCII, and an ASCII byte is never a UTF-8
//! continuation byte, so every slice it takes is on a character boundary; there is no recursion and no
//! indexing that is not guarded, so no input panics.

use std::collections::BTreeSet;

use super::{codes, Code, Finding};

/// The most bytes one asset may carry. Real icons are under 3 KiB; a published outline of a numeral set
/// is larger, and this bounds the tokenizer's work per asset.
pub const MAX_ASSET_BYTES: usize = 16 * 1024;
/// The most `<path>` elements one asset may carry (the real icons use one to four).
pub const MAX_PATHS_PER_ASSET: usize = 8;

/// What a checked asset is: its `viewBox` as `[min_x, min_y, width, height]` and its `<path>` count.
#[derive(Clone, Debug, PartialEq)]
pub struct SvgInfo {
    pub view_box: [f32; 4],
    pub paths: usize,
}

/// The only `xmlns` an asset may declare.
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
/// How many characters of a name or value a finding's message quotes.
const ECHO_CHARS: usize = 24;

const HINT_GRADIENT: &str = "HUD assets are alpha masks tinted by the app; a gradient would be discarded, so draw the mark in one colour";
const HINT_COLOUR: &str = "assets are masks tinted per state, so colour would be dropped; colour comes from the face's ink, so remove the attribute (a white fill is accepted; stroked marks must be drawn as filled outlines)";
const HINT_ATTRIBUTE: &str = "an asset is a mask that holds only <svg viewBox> and <path d>; remove the attribute (bake a transform into the path data)";
const HINT_REFERENCE: &str = "an asset is a self-contained mask: entities, external references and linked resources are not allowed; remove the attribute";
const HINT_ENTITY: &str = "entities and external references are not allowed in an asset; write the character itself, or remove it";
const HINT_SYNTAX: &str = "write the asset as plain XML: <svg viewBox='0 0 24 24'><path d='M...'/></svg>, with no declarations, instructions or CDATA";
const HINT_STRUCTURE: &str = "an asset is one <svg viewBox> holding <path d> elements, each closed, and nothing else";

/// Check one asset against the subset. `pointer` is the asset string's JSON pointer (`/assets/gear`),
/// the pointer of every finding returned. `Ok` carries what the compiler needs to place the mask; `Err`
/// carries EVERY violation found (never empty).
pub fn check_asset(pointer: &str, svg: &str) -> Result<SvgInfo, Vec<Finding>> {
    if svg.len() > MAX_ASSET_BYTES {
        return Err(vec![Finding::new(
            &codes::E0705,
            pointer,
            format!(
                "the asset is {} bytes; the most allowed is {MAX_ASSET_BYTES}",
                svg.len()
            ),
            "a mask needs a few hundred bytes: simplify the outline or merge its subpaths",
        )]);
    }
    let mut checker = Checker {
        pointer,
        src: svg,
        pos: 0,
        out: Vec::new(),
        seen: BTreeSet::new(),
        stack: Vec::new(),
        root_seen: false,
        root_done: false,
        root_at: 0,
        view_box: None,
        paths: 0,
        excess_at: 0,
    };
    if checker.scan().is_ok() {
        checker.finish();
    }
    checker.conclude()
}

/// The scan cannot go on: the finding that says why is already reported.
struct Halt;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Svg,
    Path,
    /// An element the subset refuses (`HUD-E0701`), or a nested `<svg>`.
    Other,
}

struct Attr<'a> {
    name: &'a str,
    value: &'a str,
    /// Byte offset of the name and of the first byte of the value.
    at: usize,
    value_at: usize,
}

struct Open<'a> {
    name: &'a str,
    attrs: Vec<Attr<'a>>,
    self_closing: bool,
    at: usize,
}

struct Checker<'a> {
    pointer: &'a str,
    src: &'a str,
    /// Always on a character boundary: it only ever moves to just past an ASCII delimiter, or to the end.
    pos: usize,
    out: Vec<Finding>,
    /// (code, what) of every finding reported, so a repeat is dropped.
    seen: BTreeSet<(&'static str, String)>,
    stack: Vec<(&'a str, Kind)>,
    root_seen: bool,
    root_done: bool,
    root_at: usize,
    view_box: Option<[f32; 4]>,
    paths: usize,
    excess_at: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':'
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-')
}

/// A name or value as a message quotes it: the first [`ECHO_CHARS`] characters.
fn clip(s: &str) -> String {
    match s.char_indices().nth(ECHO_CHARS) {
        Some((cut, _)) => format!("{}...", &s[..cut]),
        None => s.to_owned(),
    }
}

/// The paint and style attributes: a finding about one of these is about COLOUR.
fn is_paint(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "fill"
        || n == "style"
        || n == "class"
        || n.starts_with("stroke")
        || n.ends_with("opacity")
        || n.ends_with("color")
}

/// The contract's own white: the one `fill` an asset may carry (every icon under `assets/icons` does).
fn is_mask_white(value: &str) -> bool {
    let v = value.trim_matches(|c: char| c.is_ascii_whitespace());
    ["#fff", "#ffffff", "white"]
        .iter()
        .any(|white| v.eq_ignore_ascii_case(white))
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_digit() || c.is_ascii_whitespace() || "MmLlHhVvCcSsQqTtAaZz,+-.eE".contains(c)
}

/// What to tell the author about an element outside the subset. Every hint says why: an asset is a mask.
fn element_hint(name: &str) -> &'static str {
    match name {
        "linearGradient" | "radialGradient" | "stop" | "pattern" => HINT_GRADIENT,
        "text" | "tspan" => "text is not a mask: convert the glyph outlines to a <path> (an SVG editor's \"convert text to outlines\")",
        "style" => "a style sheet sets colour and is not a mask: remove <style> and draw the mark as one filled <path>",
        "image" | "use" | "foreignObject" | "script" => "a mask asset is self-contained and runs nothing: draw the mark as inline <path> data, not as a reference to or an embedding of something else",
        "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" => "a mask is one path of subpaths: express the shape as a <path> (a stroked line becomes a filled outline)",
        "filter" | "mask" | "clipPath" => "the rasteriser does not support filters, masks or clip paths: bake the effect into the path outlines",
        "g" | "defs" | "symbol" | "switch" => "grouping is not part of the mask contract: put the <path> elements directly inside the <svg>",
        _ => "an asset is a mask: one <svg viewBox> holding <path d> elements; draw the mark as path data",
    }
}

/// Parse `viewBox`: four finite numbers separated by whitespace and/or commas, width and height above
/// zero. The aspect must stay finite too, so the compiler can divide by either side.
fn parse_view_box(value: &str) -> Result<[f32; 4], &'static str> {
    let mut numbers: Vec<f32> = Vec::new();
    for token in value
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|t| !t.is_empty())
    {
        if !token
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
        {
            return Err("it holds something that is not a number");
        }
        let n: f32 = token
            .parse()
            .map_err(|_| "it holds something that is not a number")?;
        if !n.is_finite() {
            return Err("it holds a number that is not finite");
        }
        numbers.push(n);
    }
    match numbers[..] {
        [min_x, min_y, w, h] => {
            if !(w > 0.0 && h > 0.0) {
                Err("its width and height must be above zero")
            } else if !((w / h).is_finite() && (h / w).is_finite()) {
                Err("its width and height are too far apart in size")
            } else {
                Ok([min_x, min_y, w, h])
            }
        }
        _ => Err("it must hold exactly four numbers: min-x min-y width height"),
    }
}

impl<'a> Checker<'a> {
    /// Record a finding at byte `at`, unless the same thing was already reported. `what` names the
    /// mistake without its offset (it is the repeat key); the message is `what` plus the offset.
    fn report(&mut self, code: &Code, at: usize, what: String, hint: &str) {
        if !self.seen.insert((code.id, what.clone())) {
            return;
        }
        self.out.push(Finding::new(
            code,
            self.pointer,
            format!("{what} (byte {at})"),
            hint,
        ));
    }

    /// Report, and give back the stop signal for `Err(...)?`.
    fn halt(&mut self, code: &Code, at: usize, what: String, hint: &str) -> Halt {
        self.report(code, at, what, hint);
        Halt
    }

    fn scan(&mut self) -> Result<(), Halt> {
        let src = self.src;
        if let Some(at) = src.find('\0') {
            return Err(self.halt(
                &codes::E0704,
                at,
                "a NUL byte".to_owned(),
                "remove the NUL byte: a C string ends there, so the parser would see less than was checked",
            ));
        }
        if src.starts_with('\u{feff}') {
            self.pos = '\u{feff}'.len_utf8();
        }
        while src.as_bytes().get(self.pos).is_some_and(|&b| is_ws(b)) {
            self.pos += 1;
        }
        self.declaration()?;
        loop {
            let at = self.pos;
            let next = src[at..].find('<');
            let end = next.map_or(src.len(), |i| at + i);
            self.text(&src[at..end], at)?;
            self.pos = end;
            if next.is_none() {
                break;
            }
            self.markup()?;
        }
        if let Some(&(name, _)) = self.stack.last() {
            return Err(self.halt(
                &codes::E0704,
                src.len(),
                format!("<{}> is never closed", clip(name)),
                HINT_STRUCTURE,
            ));
        }
        if !self.root_seen {
            return Err(self.halt(
                &codes::E0704,
                0,
                "the asset has no <svg> element".to_owned(),
                HINT_STRUCTURE,
            ));
        }
        Ok(())
    }

    /// The end-of-document findings: the path bounds.
    fn finish(&mut self) {
        if self.paths > MAX_PATHS_PER_ASSET {
            let what = format!(
                "the asset has {} <path> elements; the most allowed is {MAX_PATHS_PER_ASSET}",
                self.paths
            );
            let at = self.excess_at;
            self.report(
                &codes::E0706,
                at,
                what,
                "merge the marks into ONE <path> of subpaths (M...z M...z)",
            );
        } else if self.paths == 0 && !self.out.iter().any(|f| f.code == codes::E0701.id) {
            let at = self.root_at;
            self.report(
                &codes::E0704,
                at,
                "the asset has no <path>".to_owned(),
                "an asset draws its mark with <path d='...'/> inside the <svg>",
            );
        }
    }

    fn conclude(self) -> Result<SvgInfo, Vec<Finding>> {
        if self.out.is_empty() {
            if let Some(view_box) = self.view_box {
                return Ok(SvgInfo {
                    view_box,
                    paths: self.paths,
                });
            }
            // A missing viewBox is reported when <svg> opens, so this is not reachable; an `Err` is
            // still never empty.
            return Err(vec![Finding::new(
                &codes::E0704,
                self.pointer,
                "the asset has no usable viewBox",
                "give the <svg> a viewBox='0 0 24 24'",
            )]);
        }
        Err(self.out)
    }

    /// `<?xml ... ?>` at the very start. Anything else starting `<?` is a processing instruction, which
    /// [`Self::markup`] refuses.
    fn declaration(&mut self) -> Result<(), Halt> {
        let src = self.src;
        let at = self.pos;
        let Some(after) = src[at..].strip_prefix("<?xml") else {
            return Ok(());
        };
        if !after.starts_with(|c: char| c.is_ascii_whitespace()) {
            return Ok(());
        }
        let Some(close) = after.find("?>") else {
            return Err(self.halt(
                &codes::E0704,
                at,
                "the <?xml declaration is never closed".to_owned(),
                HINT_SYNTAX,
            ));
        };
        if after[..close].contains(['&', '<']) {
            return Err(self.halt(
                &codes::E0703,
                at,
                "the <?xml declaration holds markup or an entity".to_owned(),
                HINT_SYNTAX,
            ));
        }
        self.pos = at + "<?xml".len() + close + "?>".len();
        Ok(())
    }

    /// The text between two tags. Entities are `HUD-E0703` anywhere; other text is only allowed as
    /// whitespace, except inside an element that is already refused.
    fn text(&mut self, text: &str, at: usize) -> Result<(), Halt> {
        if let Some(i) = text.find('&') {
            self.report(
                &codes::E0703,
                at + i,
                "an entity reference".to_owned(),
                HINT_ENTITY,
            );
            return Ok(());
        }
        if text.bytes().all(is_ws) {
            return Ok(());
        }
        match self.stack.last().map(|&(_, kind)| kind) {
            Some(Kind::Other) => Ok(()),
            Some(Kind::Svg | Kind::Path) => {
                self.report(
                    &codes::E0704,
                    at,
                    "text between elements".to_owned(),
                    HINT_STRUCTURE,
                );
                Ok(())
            }
            None => {
                let what = if self.root_done {
                    "content after the closing </svg>"
                } else {
                    "content before the <svg> element"
                };
                Err(self.halt(&codes::E0704, at, what.to_owned(), HINT_STRUCTURE))
            }
        }
    }

    /// One construct that starts with `<`, at `self.pos`.
    fn markup(&mut self) -> Result<(), Halt> {
        let src = self.src;
        let at = self.pos;
        let rest = &src[at..];
        if rest.starts_with("<!--") {
            return self.comment();
        }
        if rest.starts_with("<![CDATA[") {
            self.report(&codes::E0703, at, "a CDATA section".to_owned(), HINT_SYNTAX);
            return self.skip_past("]]>", at, "the CDATA section");
        }
        if rest.starts_with("<?") {
            self.report(
                &codes::E0703,
                at,
                "a processing instruction".to_owned(),
                HINT_SYNTAX,
            );
            return self.skip_past("?>", at, "the processing instruction");
        }
        if rest.starts_with("<!") {
            return Err(self.halt(
                &codes::E0703,
                at,
                "a DOCTYPE or other declaration (entities and external references are not allowed)"
                    .to_owned(),
                HINT_SYNTAX,
            ));
        }
        if rest.starts_with("</") {
            return self.close_tag();
        }
        let tag = self.open_tag()?;
        self.open(tag)
    }

    /// Move `pos` past the next `end` at or after `at`; halt when it never comes.
    fn skip_past(&mut self, end: &str, at: usize, what: &str) -> Result<(), Halt> {
        let src = self.src;
        match src[at..].find(end) {
            Some(i) => {
                self.pos = at + i + end.len();
                Ok(())
            }
            None => Err(self.halt(
                &codes::E0704,
                at,
                format!("{what} is never closed"),
                HINT_SYNTAX,
            )),
        }
    }

    fn comment(&mut self) -> Result<(), Halt> {
        let src = self.src;
        let at = self.pos;
        let body = at + "<!--".len();
        let rest = &src[body..];
        if rest.starts_with('>') || rest.starts_with("->") {
            return Err(self.halt(
                &codes::E0704,
                at,
                "a comment that some parsers close at once".to_owned(),
                "write comments as <!-- text -->",
            ));
        }
        match rest.find("-->") {
            Some(i) => {
                self.pos = body + i + "-->".len();
                if let Some((_, after)) = rest[..i].split_once('>') {
                    if after.contains('<') {
                        return Err(self.halt(
                            &codes::E0704,
                            at,
                            "a comment with markup after a `>` inside it".to_owned(),
                            "nanosvg ends a comment at the first `>` and reads what follows as markup: leave `<` out of a comment after a `>`, or write no `>` in it",
                        ));
                    }
                }
                Ok(())
            }
            None => Err(self.halt(
                &codes::E0704,
                at,
                "a comment is never closed".to_owned(),
                "close the comment with -->",
            )),
        }
    }

    fn close_tag(&mut self) -> Result<(), Halt> {
        let src = self.src;
        let bytes = src.as_bytes();
        let at = self.pos;
        let start = at + "</".len();
        let mut i = start;
        while bytes.get(i).is_some_and(|&b| is_name_char(b)) {
            i += 1;
        }
        let name = &src[start..i];
        while bytes.get(i).is_some_and(|&b| is_ws(b)) {
            i += 1;
        }
        if name.is_empty() || bytes.get(i) != Some(&b'>') {
            return Err(self.halt(
                &codes::E0704,
                at,
                "a closing tag that is not </name>".to_owned(),
                HINT_STRUCTURE,
            ));
        }
        self.pos = i + 1;
        match self.stack.pop() {
            None => {
                return Err(self.halt(
                    &codes::E0704,
                    at,
                    format!("</{}> closes nothing", clip(name)),
                    HINT_STRUCTURE,
                ));
            }
            Some((open, _)) if open != name => {
                return Err(self.halt(
                    &codes::E0704,
                    at,
                    format!("</{}> closes <{}>", clip(name), clip(open)),
                    HINT_STRUCTURE,
                ));
            }
            Some(_) => {}
        }
        if self.stack.is_empty() {
            self.root_done = true;
        }
        Ok(())
    }

    /// `<name attr="value" ...>` or `.../>`, with the cursor on the `<`.
    fn open_tag(&mut self) -> Result<Open<'a>, Halt> {
        let src = self.src;
        let bytes = src.as_bytes();
        let at = self.pos;
        let start = at + 1;
        let mut i = start;
        if !bytes.get(i).is_some_and(|&b| is_name_start(b)) {
            return Err(self.halt(
                &codes::E0704,
                at,
                "a '<' that does not start an element".to_owned(),
                HINT_STRUCTURE,
            ));
        }
        while bytes.get(i).is_some_and(|&b| is_name_char(b)) {
            i += 1;
        }
        let name = &src[start..i];
        let mut attrs: Vec<Attr<'a>> = Vec::new();
        loop {
            let gap = i;
            while bytes.get(i).is_some_and(|&b| is_ws(b)) {
                i += 1;
            }
            match bytes.get(i) {
                None => {
                    return Err(self.halt(
                        &codes::E0704,
                        at,
                        format!("<{}> is never finished with '>'", clip(name)),
                        HINT_STRUCTURE,
                    ));
                }
                Some(b'>') => {
                    self.pos = i + 1;
                    return Ok(Open {
                        name,
                        attrs,
                        self_closing: false,
                        at,
                    });
                }
                Some(b'/') if bytes.get(i + 1) == Some(&b'>') => {
                    self.pos = i + 2;
                    return Ok(Open {
                        name,
                        attrs,
                        self_closing: true,
                        at,
                    });
                }
                Some(_) => {}
            }
            let name_at = i;
            if i == gap || !bytes.get(i).is_some_and(|&b| is_name_start(b)) {
                return Err(self.halt(
                    &codes::E0704,
                    i,
                    format!(
                        "<{}> has something that is not a whitespace-separated attribute",
                        clip(name)
                    ),
                    HINT_STRUCTURE,
                ));
            }
            while bytes.get(i).is_some_and(|&b| is_name_char(b)) {
                i += 1;
            }
            let attr_name = &src[name_at..i];
            while bytes.get(i).is_some_and(|&b| is_ws(b)) {
                i += 1;
            }
            if bytes.get(i) != Some(&b'=') {
                return Err(self.halt(
                    &codes::E0704,
                    name_at,
                    format!("attribute {} has no =value", clip(attr_name)),
                    HINT_STRUCTURE,
                ));
            }
            i += 1;
            while bytes.get(i).is_some_and(|&b| is_ws(b)) {
                i += 1;
            }
            let quote = match bytes.get(i) {
                Some(&q) if q == b'"' || q == b'\'' => q,
                _ => {
                    return Err(self.halt(
                        &codes::E0704,
                        name_at,
                        format!("attribute {} is not quoted", clip(attr_name)),
                        "quote the value with \" or '",
                    ));
                }
            };
            let value_at = i + 1;
            let Some(len) = bytes[value_at..].iter().position(|&b| b == quote) else {
                return Err(self.halt(
                    &codes::E0704,
                    name_at,
                    format!("the value of {} is never closed", clip(attr_name)),
                    "close the value with the quote that opened it",
                ));
            };
            let value = &src[value_at..value_at + len];
            i = value_at + len + 1;
            if let Some(k) = value.find('&') {
                self.report(
                    &codes::E0703,
                    value_at + k,
                    "an entity reference".to_owned(),
                    HINT_ENTITY,
                );
            }
            if attrs.iter().any(|a| a.name == attr_name) {
                self.report(
                    &codes::E0704,
                    name_at,
                    format!("attribute {} is written twice", clip(attr_name)),
                    "remove the repeat",
                );
                continue;
            }
            attrs.push(Attr {
                name: attr_name,
                value,
                at: name_at,
                value_at,
            });
        }
    }

    /// An element opens: place it, check its attributes, and push it unless it closed itself.
    fn open(&mut self, tag: Open<'a>) -> Result<(), Halt> {
        let parent = self.stack.last().map(|&(_, kind)| kind);
        if self.root_done {
            return Err(self.halt(
                &codes::E0704,
                tag.at,
                format!(
                    "a second element, <{}>, after the closing </svg>",
                    clip(tag.name)
                ),
                HINT_STRUCTURE,
            ));
        }
        let kind = match (tag.name, parent) {
            (name, None) => {
                if name != "svg" {
                    return Err(self.halt(
                        &codes::E0704,
                        tag.at,
                        format!("the root element is <{}>, not <svg>", clip(name)),
                        HINT_STRUCTURE,
                    ));
                }
                self.root_seen = true;
                self.root_at = tag.at;
                Kind::Svg
            }
            ("svg", Some(_)) => {
                self.report(
                    &codes::E0704,
                    tag.at,
                    "an <svg> inside the <svg>".to_owned(),
                    HINT_STRUCTURE,
                );
                Kind::Other
            }
            ("path", Some(parent)) => {
                if parent == Kind::Path {
                    self.report(
                        &codes::E0704,
                        tag.at,
                        "a <path> inside a <path>".to_owned(),
                        "a <path> has no children: write it as <path d='...'/>",
                    );
                }
                self.paths += 1;
                if self.paths == MAX_PATHS_PER_ASSET + 1 {
                    self.excess_at = tag.at;
                }
                Kind::Path
            }
            (name, Some(_)) => {
                self.report(
                    &codes::E0701,
                    tag.at,
                    format!("element <{}> is not part of the mask subset", clip(name)),
                    element_hint(name),
                );
                Kind::Other
            }
        };
        self.attributes(&tag, kind);
        if tag.self_closing {
            if parent.is_none() {
                self.root_done = true;
            }
        } else {
            self.stack.push((tag.name, kind));
        }
        Ok(())
    }

    /// Every attribute of one element, then the ones it must have.
    fn attributes(&mut self, tag: &Open<'a>, kind: Kind) {
        for attr in &tag.attrs {
            if self.reference(tag.name, attr, kind) || kind == Kind::Other {
                continue;
            }
            self.allowed(tag.name, attr, kind);
        }
        let missing = match kind {
            Kind::Svg => (!tag.attrs.iter().any(|a| a.name == "viewBox"))
                .then_some(("viewBox", "<svg viewBox='0 0 24 24'>")),
            Kind::Path => {
                (!tag.attrs.iter().any(|a| a.name == "d")).then_some(("d", "<path d='M...'/>"))
            }
            Kind::Other => None,
        };
        if let Some((name, example)) = missing {
            self.report(
                &codes::E0704,
                tag.at,
                format!("<{}> has no {name} attribute", tag.name),
                &format!("write {example}"),
            );
        }
    }

    /// The `HUD-E0703` references: `href`, `xlink`, `xmlns:*`, a foreign `xmlns`, and a value that
    /// points outside the asset. True when the attribute has been dealt with completely.
    fn reference(&mut self, element: &str, attr: &Attr<'a>, kind: Kind) -> bool {
        let lower = attr.name.to_ascii_lowercase();
        if lower == "href"
            || lower.ends_with(":href")
            || lower.starts_with("xlink:")
            || lower.starts_with("xmlns:")
        {
            self.report(
                &codes::E0703,
                attr.at,
                format!(
                    "attribute {} on <{}> is a reference",
                    clip(attr.name),
                    clip(element)
                ),
                HINT_REFERENCE,
            );
            return true;
        }
        if attr.name == "xmlns" {
            if attr.value == SVG_NAMESPACE {
                return false;
            }
            self.report(
                &codes::E0703,
                attr.value_at,
                format!("xmlns=\"{}\" is not the SVG namespace", clip(attr.value)),
                "write xmlns='http://www.w3.org/2000/svg' or leave the attribute out",
            );
            return true;
        }
        let value = attr.value.to_ascii_lowercase();
        let external = value.contains("://") || value.contains("data:");
        let paint = is_paint(attr.name);
        if external || (value.contains("url(") && !paint) {
            self.report(
                &codes::E0703,
                attr.value_at,
                format!(
                    "attribute {}=\"{}\" on <{}> points outside the asset",
                    clip(attr.name),
                    clip(attr.value),
                    clip(element)
                ),
                HINT_REFERENCE,
            );
            // A colour attribute that also points outside is still a colour mistake (`HUD-E0702`).
            return !paint || kind == Kind::Other;
        }
        false
    }

    /// One attribute of an `svg` or `path` element against the allow-list.
    fn allowed(&mut self, element: &str, attr: &Attr<'a>, kind: Kind) {
        match (kind, attr.name) {
            (Kind::Svg, "viewBox") => match parse_view_box(attr.value) {
                Ok(view_box) => self.view_box = Some(view_box),
                Err(why) => self.report(
                    &codes::E0704,
                    attr.value_at,
                    format!("viewBox=\"{}\": {why}", clip(attr.value)),
                    "write viewBox='0 0 24 24': min-x min-y width height",
                ),
            },
            (Kind::Svg, "xmlns") => {}
            (_, "fill") => {
                if !is_mask_white(attr.value) {
                    self.report(
                        &codes::E0702,
                        attr.value_at,
                        format!(
                            "fill=\"{}\" on <{element}> is not the mask white",
                            clip(attr.value)
                        ),
                        HINT_COLOUR,
                    );
                }
            }
            (Kind::Path, "d") => self.path_data(attr),
            (Kind::Path, "fill-rule") => {
                if attr.value != "nonzero" && attr.value != "evenodd" {
                    self.report(
                        &codes::E0702,
                        attr.value_at,
                        format!(
                            "fill-rule=\"{}\" is neither nonzero nor evenodd",
                            clip(attr.value)
                        ),
                        HINT_ATTRIBUTE,
                    );
                }
            }
            (_, name) => {
                let hint = if is_paint(name) {
                    HINT_COLOUR
                } else {
                    HINT_ATTRIBUTE
                };
                self.report(
                    &codes::E0702,
                    attr.at,
                    format!(
                        "attribute {} on <{element}> is not part of the mask subset",
                        clip(name)
                    ),
                    hint,
                );
            }
        }
    }

    fn path_data(&mut self, attr: &Attr<'a>) {
        if attr
            .value
            .trim_matches(|c: char| c.is_ascii_whitespace())
            .is_empty()
        {
            self.report(
                &codes::E0704,
                attr.value_at,
                "the path's d is empty".to_owned(),
                "give the <path> data: d='M6 4l14 8-14 8z'",
            );
        } else if let Some((i, c)) = attr.value.char_indices().find(|&(_, c)| !is_path_char(c)) {
            self.report(
                &codes::E0704,
                attr.value_at + i,
                format!("the path's d holds {c:?}, which is not path data"),
                "path data uses only the commands MmLlHhVvCcSsQqTtAaZz, numbers, spaces and commas",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "/assets/gear";

    fn codes_of(r: &Result<SvgInfo, Vec<Finding>>) -> Vec<&'static str> {
        match r {
            Ok(_) => Vec::new(),
            Err(v) => v.iter().map(|f| f.code).collect(),
        }
    }

    fn check(svg: &str) -> Result<SvgInfo, Vec<Finding>> {
        check_asset(P, svg)
    }

    /// The findings of a refused asset, after the shape every finding must have; `code` must be among them.
    fn rejects(svg: &str, code: &str) -> Vec<Finding> {
        match check(svg) {
            Ok(info) => panic!("{svg:?} was accepted as {info:?}, wanted {code}"),
            Err(found) => {
                assert!(!found.is_empty(), "an Err is never empty");
                assert!(
                    found.iter().any(|f| f.code == code),
                    "{svg:?}: wanted {code}, got {:?}",
                    found
                        .iter()
                        .map(|f| (f.code, &f.message))
                        .collect::<Vec<_>>()
                );
                for f in &found {
                    assert_eq!(f.pointer, P, "every finding carries the asset's pointer");
                    assert!(
                        f.message.contains("byte"),
                        "the offset is in the message: {}",
                        f.message
                    );
                    assert!(!f.hint.is_empty());
                    assert!(f.nearest.is_empty());
                }
                found
            }
        }
    }

    /// `rejects`, and `code` is the ONLY code reported.
    fn only(svg: &str, code: &str) {
        let found = rejects(svg, code);
        let all: Vec<_> = found.iter().map(|f| f.code).collect();
        assert_eq!(all, [code], "{svg:?}: {found:?}");
    }

    fn accepts(svg: &str) -> SvgInfo {
        match check(svg) {
            Ok(info) => info,
            Err(found) => panic!("{svg:?} was refused: {found:#?}"),
        }
    }

    const MARK: &str = "<svg viewBox='0 0 24 24'><path d='M6 4l14 8-14 8z'/></svg>";

    #[test]
    fn the_icon_contract_subset_is_accepted() {
        let info = accepts(MARK);
        assert_eq!(info.view_box, [0.0, 0.0, 24.0, 24.0]);
        assert_eq!(info.paths, 1);
        // Both quote styles, an empty <path></path>, a comment anywhere, a BOM, an xml declaration.
        accepts("<svg viewBox=\"0 0 24 24\"><path d=\"M0 0h1v1z\"></path></svg>");
        accepts("\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- a long -- comment, ünïcode — with <path> in it -->\n<svg viewBox=\"-1 1.6 26 26\" fill=\"#ffffff\" xmlns=\"http://www.w3.org/2000/svg\">\n  <!-- inside -->\n  <path fill-rule=\"evenodd\" d=\"M 0 0\n  L 1 1 z\"/>\n</svg>\n<!-- after -->\n");
        let info = accepts("<svg viewBox='-1 1.6 26 26'><path d='M0 0'/><path d='M1 1'/></svg>");
        assert_eq!(info.view_box, [-1.0, 1.6, 26.0, 26.0]);
        assert_eq!(info.paths, 2);
        // viewBox separators: commas, mixed, extra whitespace.
        accepts("<svg viewBox='0,0,24,24'><path d='M0 0'/></svg>");
        accepts("<svg viewBox=' 0 , 0 24\t24 '><path d='M0 0'/></svg>");
        // The mask white in the three spellings, in either case, on the root and on the path.
        for white in ["#fff", "#FFFFFF", "White", " #ffffff "] {
            accepts(&format!(
                "<svg viewBox='0 0 24 24' fill='{white}'><path fill='{white}' d='M0 0'/></svg>"
            ));
        }
        accepts("<svg viewBox='0 0 24 24' xmlns='http://www.w3.org/2000/svg'><path d='M0 0' fill-rule='nonzero'/></svg>");
        // Exactly the most paths is fine.
        let many = format!(
            "<svg viewBox='0 0 24 24'>{}</svg>",
            "<path d='M0 0'/>".repeat(MAX_PATHS_PER_ASSET)
        );
        assert_eq!(accepts(&many).paths, MAX_PATHS_PER_ASSET);
    }

    // Canonical vector 1: a gradient is an element refusal AND a colour refusal, and says why.
    #[test]
    fn a_gradient_is_refused_as_an_element_and_as_a_colour() {
        let svg = "<svg viewBox='0 0 24 24'><defs><linearGradient id='g'><stop offset='0' stop-color='red'/></linearGradient></defs><path fill='url(#g)' d='M0 0h1v1z'/></svg>";
        let found = rejects(svg, "HUD-E0701");
        let codes = codes_of(&check(svg));
        assert!(codes.contains(&"HUD-E0702"), "{codes:?}");
        let gradient: Vec<_> = found
            .iter()
            .filter(|f| f.message.contains("linearGradient") || f.message.contains("<stop>"))
            .collect();
        assert_eq!(gradient.len(), 2, "{found:#?}");
        for f in &found {
            assert!(f.hint.contains("mask"), "{}: {}", f.code, f.hint);
        }
        assert!(gradient.iter().all(|f| f
            .hint
            .contains("alpha masks tinted by the app")
            && f.hint.contains("a gradient would be discarded")
            && f.hint.contains("draw the mark in one colour")));
        let fill = found.iter().find(|f| f.code == "HUD-E0702").unwrap();
        assert!(fill
            .hint
            .contains("assets are masks tinted per state, so colour would be dropped"));
        assert!(fill.hint.contains("face's ink"));
        // The same for the radial gradient and the pattern.
        for tag in ["radialGradient", "pattern"] {
            let svg = format!("<svg viewBox='0 0 24 24'><{tag} id='g'/><path d='M0 0'/></svg>");
            for f in rejects(&svg, "HUD-E0701") {
                assert!(f.hint.contains("gradient would be discarded"));
            }
        }
    }

    // Canonical vector 2: two colours.
    #[test]
    fn multi_colour_content_is_refused_per_colour() {
        let svg = "<svg viewBox='0 0 24 24'><path fill='#ff0000' d='M0 0h1v1z'/><path fill='#00ff00' d='M2 2h1v1z'/></svg>";
        let found = rejects(svg, "HUD-E0702");
        let colours: Vec<_> = found.iter().filter(|f| f.code == "HUD-E0702").collect();
        assert_eq!(colours.len(), 2, "one finding per colour: {found:#?}");
        assert!(colours
            .iter()
            .all(|f| f.hint.contains("mask") && f.hint.contains("colour would be dropped")));
        assert!(colours[0].message.contains("#ff0000") && colours[1].message.contains("#00ff00"));
        // The same colour twice is one finding.
        let twice =
            "<svg viewBox='0 0 24 24'><path fill='red' d='M0 0'/><path fill='red' d='M1 1'/></svg>";
        assert_eq!(rejects(twice, "HUD-E0702").len(), 1);
    }

    // Vectors 3 and 4: text and style are elements the mask cannot hold.
    #[test]
    fn text_and_style_are_refused() {
        only(
            "<svg viewBox='0 0 24 24'><text x='1'>hi</text></svg>",
            "HUD-E0701",
        );
        only(
            "<svg viewBox='0 0 24 24'><style>.a{fill:red}</style><path d='M0 0'/></svg>",
            "HUD-E0701",
        );
        let f = rejects("<svg viewBox='0 0 24 24'><text>x</text></svg>", "HUD-E0701");
        assert!(f[0].hint.contains("outlines"));
        let f = rejects("<svg viewBox='0 0 24 24'><style>a{}</style></svg>", "HUD-E0701");
        assert!(f[0].hint.contains("style"));
    }

    // Vector 5: entities and a DOCTYPE with an ENTITY.
    #[test]
    fn entities_doctype_cdata_and_instructions_are_refused() {
        let f = rejects(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/>&amp;</svg>",
            "HUD-E0703",
        );
        assert!(f[0]
            .hint
            .contains("entities and external references are not allowed"));
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/>&#65;</svg>",
            "HUD-E0703",
        );
        let doctype =
            "<!DOCTYPE svg [ <!ENTITY x \"y\"> ]><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>";
        only(doctype, "HUD-E0703");
        only(
            "<!ENTITY x 'y'><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>",
            "HUD-E0703",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/><![CDATA[ x ]]></svg>",
            "HUD-E0703",
        );
        only(
            "<?xml-stylesheet href='a.css'?><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>",
            "HUD-E0703",
        );
        only(
            "<svg viewBox='0 0 24 24'><?php echo 1 ?><path d='M0 0'/></svg>",
            "HUD-E0703",
        );
        // An entity in a value is reported (and the value, here a bad fill, is judged on its own).
        rejects(
            "<svg viewBox='0 0 24 24'><path d='M0 0' fill='white&amp;'/></svg>",
            "HUD-E0703",
        );
        // A comment may hold an ampersand: the rule is for markup.
        accepts("<!-- a & b --><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>");
        // The xml declaration is the one allowed instruction, and only at the start.
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/><?xml version='1.0'?></svg>",
            "HUD-E0703",
        );
    }

    // Vector 6: an <image href> is an element AND a reference.
    #[test]
    fn an_image_and_its_href_are_two_findings() {
        let svg = "<svg viewBox='0 0 24 24'><image href='http://x/y.png'/></svg>";
        let found = rejects(svg, "HUD-E0701");
        assert_eq!(
            codes_of(&check(svg)),
            ["HUD-E0701", "HUD-E0703"],
            "{found:#?}"
        );
        // References by name and by value, on any element.
        for attr in [
            "xlink:href='a'",
            "HREF='a'",
            "xmlns:xlink='urn:x'",
            "clip-path='url(#c)'",
            "foo='data:x'",
            "foo='http://x/y'",
        ] {
            let svg = format!("<svg viewBox='0 0 24 24'><path {attr} d='M0 0'/></svg>");
            rejects(&svg, "HUD-E0703");
        }
        // A colour that points outside is both: the colour is the author's mistake.
        let codes = codes_of(&check(
            "<svg viewBox='0 0 24 24'><path fill='url(http://x/g)' d='M0 0'/></svg>",
        ));
        assert_eq!(codes, ["HUD-E0703", "HUD-E0702"]);
        // The wrong namespace is a reference; the right one is not.
        rejects(
            "<svg viewBox='0 0 24 24' xmlns='http://example.com/'><path d='M0 0'/></svg>",
            "HUD-E0703",
        );
        rejects(
            "<svg viewBox='0 0 24 24' xmlns='svg'><path d='M0 0'/></svg>",
            "HUD-E0703",
        );
        rejects(
            "<svg viewBox='0 0 24 24'><path xmlns='http://www.w3.org/2000/svg' d='M0 0'/></svg>",
            "HUD-E0702",
        );
        // The other referencing elements.
        for tag in ["use", "foreignObject", "script"] {
            let svg = format!("<svg viewBox='0 0 24 24'><{tag}/><path d='M0 0'/></svg>");
            rejects(&svg, "HUD-E0701");
        }
    }

    // Vector 7: size comes first and alone.
    #[test]
    fn an_over_long_asset_is_one_size_finding() {
        let big = format!(
            "<svg viewBox='0 0 24 24'><path fill='red' d='M0 0'/>{}</svg>",
            " ".repeat(MAX_ASSET_BYTES)
        );
        only(&big, "HUD-E0705");
        // At exactly the bound it is scanned.
        let head = "<svg viewBox='0 0 24 24'><path d='M0 0'/>";
        let tail = "</svg>";
        let fill = " ".repeat(MAX_ASSET_BYTES - head.len() - tail.len());
        let exact = format!("{head}{fill}{tail}");
        assert_eq!(exact.len(), MAX_ASSET_BYTES);
        accepts(&exact);
        only(&format!("{exact} "), "HUD-E0705");
    }

    // Vector 8: one past the most paths.
    #[test]
    fn too_many_paths_is_reported_with_the_rest() {
        let path = "<path d='M0 0'/>";
        let svg = format!(
            "<svg viewBox='0 0 24 24'>{}</svg>",
            path.repeat(MAX_PATHS_PER_ASSET + 1)
        );
        only(&svg, "HUD-E0706");
        let with_colour = format!(
            "<svg viewBox='0 0 24 24'>{}<path fill='red' d='M0 0'/></svg>",
            path.repeat(MAX_PATHS_PER_ASSET)
        );
        assert_eq!(
            codes_of(&check(&with_colour)),
            ["HUD-E0702", "HUD-E0706"]
        );
    }

    // Vector 9: a document that is not an icon at all.
    #[test]
    fn a_malformed_document_is_e0704() {
        // No viewBox, and every way a viewBox can be wrong.
        only("<svg><path d='M0 0'/></svg>", "HUD-E0704");
        for bad in [
            "",
            "0 0 24",
            "0 0 24 24 5",
            "0 0 0 24",
            "0 0 24 -1",
            "a b c d",
            "0 0 NaN 24",
            "0 0 inf 24",
            "0 0 1e999 24",
            "0x0 0 24 24",
            "0 0 3e38 1e-40",
        ] {
            let svg = format!("<svg viewBox='{bad}'><path d='M0 0'/></svg>");
            only(&svg, "HUD-E0704");
        }
        // No root at all, a wrong root, an unterminated tag, unbalanced tags.
        only("", "HUD-E0704");
        only("   ", "HUD-E0704");
        only("<!-- nothing -->", "HUD-E0704");
        only("<g viewBox='0 0 1 1'><path d='M0 0'/></g>", "HUD-E0704");
        only("<svg viewBox='0 0 24 24'><path d='M0 0'", "HUD-E0704");
        only("<svg viewBox='0 0 24 24'><path d='M0 0'/>", "HUD-E0704");
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/></g>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/></svg><svg viewBox='0 0 1 1'/>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/></svg>trailing",
            "HUD-E0704",
        );
        only(
            "junk<svg viewBox='0 0 24 24'><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only("</svg>", "HUD-E0704");
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/><</svg>",
            "HUD-E0704",
        );
        // Attribute syntax.
        only("<svg viewBox=0><path d='M0 0'/></svg>", "HUD-E0704");
        only("<svg viewBox><path d='M0 0'/></svg>", "HUD-E0704");
        only("<svg viewBox='0 0 24 24><path d='M0 0'/></svg>", "HUD-E0704");
        only(
            "<svg viewBox='0 0 24 24'viewBox='0 0 1 1'><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24' viewBox='0 0 1 1'><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        // Content that does not belong.
        only(
            "<svg viewBox='0 0 24 24'>hello<path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><svg viewBox='0 0 1 1'/><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'><path d='M1 1'/></path></svg>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'>text</path></svg>",
            "HUD-E0704",
        );
        // Comments that are not closed, or that close at once.
        only(
            "<svg viewBox='0 0 24 24'><!-- <path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only(
            "<!--><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only(
            "<!---><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        accepts("<!----><svg viewBox='0 0 24 24'><path d='M0 0'/></svg>");
        // No path.
        // nanosvg ends any `<!` construct at the first `>`, so a `>` inside a comment would hand the
        // C parser everything after it, gradients and colour included.
        let smuggled = "<svg viewBox='0 0 24 24'><path d='M0 0h1v1z'/><!-- > <linearGradient id='g'><stop stop-color='red'/></linearGradient><path fill='url(#g)' d='M0 0'/> --></svg>";
        only(smuggled, "HUD-E0704");
        only(
            "<svg viewBox='0 0 24 24'><!-- a > <b --><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/><!-- <path d='M1 1'/> <path d='M2 2'/> --></svg>",
            "HUD-E0704",
        );
        // A `>` with only text after it, or a `<` with no `>` before it, hides nothing from nanosvg.
        accepts("<svg viewBox='0 0 24 24'><!-- one <path> so a > b --><path d='M0 0'/></svg>");
        accepts("<svg viewBox='0 0 24 24'><!-- a < b --><path d='M0 0'/></svg>");
        accepts("<svg viewBox='0 0 24 24'><!-- a - b -- c --><path d='M0 0'/></svg>");
        only("<svg viewBox='0 0 24 24'></svg>", "HUD-E0704");
        only("<svg viewBox='0 0 24 24'/>", "HUD-E0704");
        // The xml declaration must close.
        only(
            "<?xml version='1.0' <svg viewBox='0 0 24 24'><path d='M0 0'/></svg>",
            "HUD-E0704",
        );
        // A NUL byte.
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/>\0</svg>",
            "HUD-E0704",
        );
    }

    #[test]
    fn path_data_is_checked_for_its_alphabet() {
        only("<svg viewBox='0 0 24 24'><path/></svg>", "HUD-E0704");
        only("<svg viewBox='0 0 24 24'><path d=''/></svg>", "HUD-E0704");
        only(
            "<svg viewBox='0 0 24 24'><path d='  \n '/></svg>",
            "HUD-E0704",
        );
        let f = rejects(
            "<svg viewBox='0 0 24 24'><path d='M0 0 B1'/></svg>",
            "HUD-E0704",
        );
        assert!(f[0].message.contains('B'), "{}", f[0].message);
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0 ü'/></svg>",
            "HUD-E0704",
        );
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0 (1)'/></svg>",
            "HUD-E0704",
        );
        accepts("<svg viewBox='0 0 24 24'><path d='MmLlHhVvCcSsQqTtAaZz 1,2 +3 -4 .5 1e5 2E-3'/></svg>");
    }

    #[test]
    fn colour_and_style_attributes_are_refused_with_the_mask_reason() {
        for attr in [
            "fill='none'",
            "fill='#ff0000'",
            "fill='currentColor'",
            "stroke='#fff'",
            "stroke-width='2'",
            "opacity='.5'",
            "fill-opacity='.5'",
            "stop-color='red'",
            "color='red'",
            "style='fill:red'",
            "class='a'",
        ] {
            let svg = format!("<svg viewBox='0 0 24 24'><path {attr} d='M0 0'/></svg>");
            let found = rejects(&svg, "HUD-E0702");
            assert!(
                found
                    .iter()
                    .all(|f| f.hint.contains("mask") && f.hint.contains("colour would be dropped")),
                "{attr}: {found:?}"
            );
        }
        // The root's attributes are held to the same list.
        let svg = "<svg viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='2'><path d='M0 0'/></svg>";
        assert_eq!(rejects(svg, "HUD-E0702").len(), 3);
        // Non-colour attributes outside the list are refused too, with a hint that still says "mask".
        for attr in [
            "transform='scale(2)'",
            "width='24'",
            "id='a'",
            "viewbox='0 0 1 1'",
            "fill-rule='odd'",
        ] {
            let svg = format!("<svg viewBox='0 0 24 24'><path {attr} d='M0 0'/></svg>");
            let found = rejects(&svg, "HUD-E0702");
            assert!(found.iter().all(|f| f.hint.contains("mask")), "{attr}: {found:?}");
        }
        only(
            "<svg viewBox='0 0 24 24' width='24'><path d='M0 0'/></svg>",
            "HUD-E0702",
        );
        // A refused element's own attributes are not listed on top of it.
        only(
            "<svg viewBox='0 0 24 24'><path d='M0 0'/><rect x='1' y='2' width='3' height='4' rx='1' fill='red'/></svg>",
            "HUD-E0701",
        );
    }

    #[test]
    fn every_refused_element_says_to_draw_a_mask() {
        for tag in ["rect", "circle", "ellipse", "line", "polyline", "polygon"] {
            let svg = format!("<svg viewBox='0 0 24 24'><{tag}/></svg>");
            let found = rejects(&svg, "HUD-E0701");
            assert!(found[0].hint.contains("<path>"), "{tag}");
        }
        for tag in [
            "filter",
            "mask",
            "clipPath",
            "g",
            "defs",
            "tspan",
            "symbol",
            "image",
            "use",
            "foreignObject",
            "script",
            "style",
            "text",
            "linearGradient",
            "radialGradient",
            "stop",
            "pattern",
            "whatever",
        ] {
            let svg = format!("<svg viewBox='0 0 24 24'><{tag}><path d='M0 0'/></{tag}></svg>");
            let found = rejects(&svg, "HUD-E0701");
            assert!(found.iter().all(|f| f.hint.contains("mask")), "{tag}: {found:?}");
            only(&svg, "HUD-E0701");
        }
        // Children of a refused element are looked at as well: a stop inside a gradient is its own finding.
        let svg = "<svg viewBox='0 0 24 24'><linearGradient><stop/></linearGradient></svg>";
        assert_eq!(rejects(svg, "HUD-E0701").len(), 2);
        // Text inside a refused element is not a second complaint.
        only("<svg viewBox='0 0 24 24'><text>hi</text></svg>", "HUD-E0701");
    }

    #[test]
    fn findings_carry_the_byte_offset_and_are_not_repeated() {
        let svg = "<svg viewBox='0 0 24 24'><rect/><path d='M0 0'/><rect/></svg>";
        let found = rejects(svg, "HUD-E0701");
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].message.ends_with("(byte 25)"),
            "{}",
            found[0].message
        );
        assert_eq!(found[0].severity, super::super::Severity::Error);
        assert_eq!(found[0].rule, "svg-element");
        assert!(found[0].message.contains("<rect>"));
        // A long name is clipped in the message.
        let long = "x".repeat(200);
        let found = rejects(
            &format!("<svg viewBox='0 0 24 24'><{long}/><path d='M0 0'/></svg>"),
            "HUD-E0701",
        );
        assert!(found[0].message.len() < 120, "{}", found[0].message);
    }

    #[test]
    fn multibyte_text_never_splits_a_character() {
        accepts("<svg viewBox='0 0 24 24'><path d='M0 0'/></svg>");
        for svg in [
            "<svg viewBox='0 0 24 24'>é<path d='M0 0'/></svg>",
            "<svg viewBox='0 0 24 24'><path d='M0 0' ü='1'/></svg>",
            "<svg viewBox='0 0 24 24'><é/></svg>",
            "<svg viewBox='é'><path d='M0 0'/></svg>",
            "<svg viewBox='0 0 24 24'><path fill='😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀' d='M0 0'/></svg>",
            "<😀 viewBox='0 0 24 24'/>",
            "😀",
            "<svg viewBox='0 0 24 24'><path d='M0 0'/></😀>",
        ] {
            assert!(check(svg).is_err(), "{svg}");
        }
    }

    // ---- the real icons -------------------------------------------------------------------------------

    macro_rules! icon {
        ($name:literal) => {
            include_str!(concat!("../../../../assets/icons/", $name, ".svg"))
        };
    }

    #[test]
    fn the_real_mask_icons_pass() {
        for (name, svg) in [
            ("play", icon!("play")),
            ("previous", icon!("previous")),
            ("next", icon!("next")),
            ("rewind", icon!("rewind")),
            ("fast-forward", icon!("fast-forward")),
            ("close", icon!("close")),
            ("more", icon!("more")),
            ("user", icon!("user")),
            ("trailer", icon!("trailer")),
            ("check-circle-fill", icon!("check-circle-fill")),
            ("bookmark", icon!("bookmark")),
            ("clock", icon!("clock")),
            ("tomato-calyx", icon!("tomato-calyx")),
            ("wifi-slash", icon!("wifi-slash")),
        ] {
            match check_asset(&format!("/assets/{name}"), svg) {
                Ok(info) => {
                    assert!(
                        info.paths >= 1 && info.paths <= MAX_PATHS_PER_ASSET,
                        "{name}"
                    );
                    assert!(info.view_box[2] > 0.0 && info.view_box[3] > 0.0, "{name}");
                }
                Err(found) => panic!("{name} is a real mask icon and was refused: {found:#?}"),
            }
        }
        assert_eq!(
            accepts(icon!("more")).paths,
            1,
            "a <path> named in a comment is not a path"
        );
        assert_eq!(
            accepts(icon!("tomato-calyx")).view_box,
            [-1.0, 1.6, 26.0, 26.0]
        );
    }

    #[test]
    fn the_real_icons_that_are_not_masks_are_refused() {
        // (name, svg, codes that must be among the findings)
        for (name, svg, wanted) in [
            ("pause", icon!("pause"), &["HUD-E0701"][..]),
            ("tomato", icon!("tomato"), &["HUD-E0701"][..]),
            ("info", icon!("info"), &["HUD-E0701", "HUD-E0702"][..]),
            ("cc", icon!("cc"), &["HUD-E0701", "HUD-E0702"][..]),
            ("chevron", icon!("chevron"), &["HUD-E0702"][..]),
            ("search", icon!("search"), &["HUD-E0702"][..]),
        ] {
            let found = match check_asset(&format!("/assets/{name}"), svg) {
                Ok(info) => panic!("{name} is not a mask icon but was accepted as {info:?}"),
                Err(found) => found,
            };
            for code in wanted {
                assert!(
                    found.iter().any(|f| f.code == *code),
                    "{name}: wanted {code} in {found:#?}"
                );
            }
        }
        // The stroked ones fail on the root's stroke attributes and say what to do.
        let chevron = check_asset("/assets/chevron", icon!("chevron")).unwrap_err();
        assert!(chevron
            .iter()
            .any(|f| f.message.contains("fill=\"none\"")));
        assert!(chevron.iter().any(|f| f.message.contains("stroke")));
        assert!(chevron.iter().all(|f| f.hint.contains("filled outlines")));
    }

    /// The six assets of `fixtures/reference.plxhud.json`, hand-copied; the test below proves the copies.
    const EXAMPLE_ASSETS: [(&str, &str); 6] = [
        (
            "subs-bubble",
            "<svg viewBox='0 0 24 24'><path d='M3 4h18v12H10l-5 4v-4H3z'/></svg>",
        ),
        (
            "list",
            "<svg viewBox='0 0 24 24'><path d='M4 6h16v2H4zM4 11h16v2H4zM4 16h16v2H4z'/></svg>",
        ),
        (
            "gear",
            "<svg viewBox='0 0 24 24'><path d='M10 2h4l1 3 3-1 2 3-2 2v2l2 2-2 3-3-1-1 3h-4l-1-3-3 1-2-3 2-2v-2L4 7l2-3 3 1z'/></svg>",
        ),
        (
            "eq",
            "<svg viewBox='0 0 24 24'><path d='M6 14h2v6H6zM11 8h2v12h-2zM16 11h2v9h-2z'/></svg>",
        ),
        (
            "back10",
            "<svg viewBox='0 0 24 24'><path d='M12 1v2a9 9 0 1 1-9 9h2a7 7 0 1 0 7-7v3l-4-4zM9 8h1.5v8h-1.5v-6.5l-1.5 1.5v-1.5zM13.5 8h2l1.5 1.5v5l-1.5 1.5h-2l-1.5-1.5v-5zM13.5 10v4l0.5 0.5h1l0.5-0.5v-4l-0.5-0.5h-1z'/></svg>",
        ),
        (
            "fwd30",
            "<svg viewBox='0 0 24 24'><path d='M12 1v2a9 9 0 1 0 9 9h-2a7 7 0 1 1-7-7v3l4-4zM8 8h2l1 1v6l-1 1h-2l-1-1v-0.5h2.5v-2h-2v-1.5h2v-1.5h-2.5v-0.5zM13.5 8h2l1.5 1.5v5l-1.5 1.5h-2l-1.5-1.5v-5zM13.5 10v4l0.5 0.5h1l0.5-0.5v-4l-0.5-0.5h-1z'/></svg>",
        ),
    ];

    #[test]
    fn the_worked_examples_assets_pass_and_match_the_fixture() {
        let fixture = include_str!("fixtures/reference.plxhud.json");
        for (id, svg) in EXAMPLE_ASSETS {
            assert!(
                fixture.contains(&format!("\"{id}\": \"{svg}\"")),
                "the hand-copy of {id} drifted from the fixture"
            );
            let info = accepts(svg);
            assert_eq!(info.view_box, [0.0, 0.0, 24.0, 24.0], "{id}");
            assert_eq!(info.paths, 1, "{id}");
        }
    }

    /// The numbers of a path's argument text (`"1.5-1.5"` is two), in order.
    fn path_numbers(text: &str) -> Vec<f32> {
        let mut out = Vec::new();
        let mut cur = String::new();
        for c in text.chars().chain(std::iter::once(' ')) {
            if (c == '-' || c.is_whitespace() || c == ',') && !cur.is_empty() {
                out.push(cur.parse().expect("a number"));
                cur.clear();
            }
            if c == '-' || c.is_ascii_digit() || c == '.' {
                cur.push(c);
            }
        }
        out
    }

    /// The vertices of one straight-sided sub-path: an absolute `M x y`, then relative `h`, `v`, `l` and a
    /// closing `z`, the only commands the numerals use.
    fn polygon(sub: &str) -> Vec<(f32, f32)> {
        let body = sub.strip_prefix('M').expect("a sub-path starts with M");
        let (start, mut rest) = body.split_at(body.find(|c: char| c.is_ascii_alphabetic()).expect("a command"));
        let at = path_numbers(start);
        let mut pts = vec![(at[0], at[1])];
        while !rest.is_empty() {
            let cmd = rest.chars().next().expect("a command");
            let end = rest[1..].find(|c: char| c.is_ascii_alphabetic()).map_or(rest.len(), |i| i + 1);
            let args = path_numbers(&rest[1..end]);
            let (x, y) = *pts.last().expect("a vertex");
            match (cmd, args.as_slice()) {
                ('h', [dx]) => pts.push((x + dx, y)),
                ('v', [dy]) => pts.push((x, y + dy)),
                ('l', [dx, dy]) => pts.push((x + dx, y + dy)),
                ('z', []) => {}
                _ => panic!("a numeral uses only h, v, l and z, got `{cmd}` with {args:?}"),
            }
            rest = &rest[end..];
        }
        pts
    }

    /// Twice the signed area (shoelace): positive for the turn the outer outlines take on screen.
    fn signed_area(p: &[(f32, f32)]) -> f32 {
        (0..p.len())
            .map(|i| {
                let (a, b) = (p[i], p[(i + 1) % p.len()]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum()
    }

    /// The box around a sub-path's vertices, as `(min x, min y, max x, max y)`.
    fn bounds(p: &[(f32, f32)]) -> (f32, f32, f32, f32) {
        p.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(x0, y0, x1, y1), &(x, y)| {
            (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
        })
    }

    /// **The 10 and 30 faces carry numerals, not bars.** The reference's two skip buttons once drew their
    /// digits as bare rectangles, which the TV showed as two vertical bars inside the ring. Each asset is
    /// the ring (its first sub-path, with curves) and then the numeral's own straight-sided outlines: no
    /// digit is a four-vertex rectangle, the 0's counter winds against its outline and lies inside it (the
    /// fill rule is nonzero, so a same-way sub-path would fill the hole), and every digit vertex stands a
    /// full unit inside the ring's inner edge (radius 7 around (12, 12), where both rings are centred), and the
    /// digit pair is centred on the ring's centre line.
    #[test]
    fn the_ten_and_thirty_faces_draw_numerals_clear_of_the_ring() {
        for (id, svg) in EXAMPLE_ASSETS.iter().filter(|(id, _)| matches!(*id, "back10" | "fwd30")) {
            let d = svg.split("d='").nth(1).and_then(|t| t.split('\'').next()).expect("a d attribute");
            let digits: Vec<Vec<(f32, f32)>> = d.split_inclusive('z').skip(1).map(polygon).collect();
            assert_eq!(digits.len(), 3, "{id}: the first digit, the 0's outline and the 0's counter");
            for (i, poly) in digits.iter().enumerate() {
                assert!(poly.len() > 4, "{id}: sub-path {i} is a bare rectangle: {poly:?}");
                for &(x, y) in poly {
                    let (dx, dy) = (x - 12.0, y - 12.0);
                    let r2 = dx * dx + dy * dy;
                    assert!(r2 <= 36.0, "{id}: ({x}, {y}) is {r2} (squared) from the ring's centre; its inner edge is 7, so 49");
                }
            }
            let (x0, y0, x1, y1) = digits.iter().map(|p| bounds(p)).fold(
                (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                |(a, b, c, d), (e, f, g, h)| (a.min(e), b.min(f), c.max(g), d.max(h)),
            );
            assert!((y0 + y1 - 24.0).abs() < 1e-3, "{id}: the digits span y {y0}..{y1}, not centred on the ring's 12");
            assert!((x0 + x1 - 24.0).abs() <= 0.5, "{id}: the digits span x {x0}..{x1}, not centred on the ring's 12");
            let (outline, counter) = (signed_area(&digits[1]), signed_area(&digits[2]));
            assert!(outline > 0.0 && counter < 0.0, "{id}: the counter winds against the outline ({outline}, {counter})");
            let (o, c) = (bounds(&digits[1]), bounds(&digits[2]));
            assert!(c.0 > o.0 && c.1 > o.1 && c.2 < o.2 && c.3 < o.3, "{id}: the counter lies inside the 0: {c:?} in {o:?}");
        }
    }

    /// One sub-path's commands with their numbers: `("a", [9, 9, 0, 1, 0, 9, 9])`.
    fn commands(sub: &str) -> Vec<(char, Vec<f32>)> {
        let mut out = Vec::new();
        let mut rest = sub;
        while let Some(cmd) = rest.chars().next() {
            let end = rest[1..].find(|c: char| c.is_ascii_alphabetic()).map_or(rest.len(), |i| i + 1);
            out.push((cmd, path_numbers(&rest[1..end])));
            rest = &rest[end..];
        }
        out
    }

    /// The ring-and-arrow sub-path of an example icon, as its commands.
    fn ring_of(id: &str) -> (String, Vec<(char, Vec<f32>)>) {
        let (_, svg) = EXAMPLE_ASSETS.iter().find(|(i, _)| *i == id).expect("an example asset");
        let d = svg.split("d='").nth(1).and_then(|t| t.split('\'').next()).expect("a d attribute");
        let ring = d.split_inclusive('z').next().expect("a ring");
        (ring.to_owned(), commands(ring))
    }

    /// **The back-10 arrow is the forward-30 arrow turned about the vertical centre line.** The owner saw
    /// `back10`'s outline close at the wrong point, so its arrowhead collapsed into a sliver, and its ring sat on
    /// (12, 13) while `fwd30`'s sat on (12, 12). Both rings now sit on (12, 12) with the outer radius 9 and the
    /// inner 7, and `back10`'s command list is `fwd30`'s mirrored about x = 12: the start point's x becomes
    /// 24 - x, every horizontal step changes sign and each arc's sweep flips.
    #[test]
    fn the_ten_arrow_mirrors_the_thirty_arrow() {
        let (fwd_text, fwd) = ring_of("fwd30");
        let (back_text, back) = ring_of("back10");
        assert_eq!(fwd_text, "M12 1v2a9 9 0 1 0 9 9h-2a7 7 0 1 1-7-7v3l4-4z", "fwd30's ring and arrow");
        assert_eq!(back_text, "M12 1v2a9 9 0 1 1-9 9h2a7 7 0 1 0 7-7v3l-4-4z", "back10's ring and arrow");
        let mirrored: Vec<(char, Vec<f32>)> = fwd
            .iter()
            .map(|(cmd, n)| match (*cmd, n.as_slice()) {
                ('M', [x, y]) => ('M', vec![24.0 - x, *y]),
                ('a', [rx, ry, rot, large, sweep, dx, dy]) => ('a', vec![*rx, *ry, *rot, *large, 1.0 - sweep, -dx, *dy]),
                ('h', [dx]) => ('h', vec![-dx]),
                ('l', [dx, dy]) => ('l', vec![-dx, *dy]),
                ('v', [dy]) => ('v', vec![*dy]),
                ('z', []) => ('z', vec![]),
                _ => panic!("fwd30's ring uses only M, a, h, l, v and z, got `{cmd}` with {n:?}"),
            })
            .collect();
        assert_eq!(back, mirrored, "back10's ring and arrow are not fwd30's mirror image");
    }

    // ---- seeded mutation -------------------------------------------------------------------------------

    /// A fixed-seed xorshift64: deterministic, no clock, no crate.
    struct Prng(u64);

    impl Prng {
        fn step(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            (self.step() % n.max(1) as u64) as usize
        }
    }

    const FRAGMENTS: [&str; 14] = [
        "<",
        ">",
        "&",
        "\"",
        "'",
        "/",
        "<!--",
        "-->",
        "<![CDATA[",
        "<?",
        "<!DOCTYPE",
        "fill=",
        "url(#g)",
        "href=",
    ];

    fn mutate(rng: &mut Prng, seed: &str) -> String {
        let mut bytes = seed.as_bytes().to_vec();
        for _ in 0..1 + rng.below(4) {
            if bytes.is_empty() {
                bytes.push(b'<');
            }
            match rng.below(6) {
                0 => {
                    let i = rng.below(bytes.len());
                    bytes[i] ^= 1 << rng.below(8);
                }
                1 => {
                    let from = rng.below(bytes.len());
                    let to = (from + 1 + rng.below(8)).min(bytes.len());
                    drop(bytes.drain(from..to));
                }
                2 => {
                    let at = rng.below(bytes.len() + 1);
                    let junk: Vec<u8> = (0..1 + rng.below(6)).map(|_| rng.step() as u8).collect();
                    drop(bytes.splice(at..at, junk));
                }
                3 => {
                    let at = rng.below(bytes.len() + 1);
                    let frag = FRAGMENTS[rng.below(FRAGMENTS.len())];
                    drop(bytes.splice(at..at, frag.bytes()));
                }
                4 => {
                    let keep = rng.below(bytes.len() + 1);
                    bytes.truncate(keep);
                }
                _ => {
                    // Duplicate a slice: repeats elements, attributes and quotes.
                    let from = rng.below(bytes.len());
                    let to = (from + 1 + rng.below(40)).min(bytes.len());
                    let copy = bytes[from..to].to_vec();
                    let at = rng.below(bytes.len() + 1);
                    drop(bytes.splice(at..at, copy));
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The accepted text with its comments removed (the one place `&` and the like are allowed).
    fn without_comments(svg: &str) -> String {
        let mut out = String::new();
        let mut rest = svg;
        while let Some(i) = rest.find("<!--") {
            out.push_str(&rest[..i]);
            match rest[i + 4..].find("-->") {
                Some(j) => rest = &rest[i + 4 + j + 3..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn seeded_mutations_never_panic_and_never_admit_what_the_policy_forbids() {
        let mut seeds: Vec<String> = [icon!("play"), icon!("more"), icon!("check-circle-fill")]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        seeds.extend(EXAMPLE_ASSETS.iter().map(|(_, svg)| (*svg).to_owned()));
        let mut rng = Prng(0x9e37_79b9_7f4a_7c15);
        let (mut accepted, mut refused) = (0usize, 0usize);
        for _ in 0..2000 {
            let seed = &seeds[rng.below(seeds.len())];
            let mutated = mutate(&mut rng, seed);
            match check_asset(P, &mutated) {
                Ok(info) => {
                    accepted += 1;
                    assert!(
                        info.paths >= 1 && info.paths <= MAX_PATHS_PER_ASSET,
                        "{mutated:?}"
                    );
                    assert!(info.view_box.iter().all(|v| v.is_finite()), "{mutated:?}");
                    assert!(
                        info.view_box[2] > 0.0 && info.view_box[3] > 0.0,
                        "{mutated:?}"
                    );
                    let text = without_comments(&mutated);
                    let lower = text.to_ascii_lowercase();
                    for forbidden in [
                        "&", "<!", "<?xml-", "href", "url(", "data:", "<script", "<style",
                        "<text", "<image", "stroke", "opacity",
                    ] {
                        assert!(
                            !lower.contains(forbidden),
                            "accepted {mutated:?}, which holds {forbidden:?}"
                        );
                    }
                    assert!(
                        !lower.replace(SVG_NAMESPACE, "").contains("://"),
                        "{mutated:?}"
                    );
                    assert!(text.matches("<?").count() <= 1, "{mutated:?}");
                }
                Err(found) => {
                    refused += 1;
                    assert!(!found.is_empty(), "{mutated:?}");
                    for f in &found {
                        assert!(codes::ALL.iter().any(|c| c.id == f.code), "{}", f.code);
                        assert!(f.code.starts_with("HUD-E07"), "{}", f.code);
                        assert_eq!(f.pointer, P);
                    }
                }
            }
        }
        // The mutations reach both outcomes, so the loop is exercising something.
        assert!(
            accepted > 20 && refused > 200,
            "accepted {accepted}, refused {refused}"
        );
    }

    #[test]
    fn deep_nesting_many_elements_and_many_entities_are_bounded_by_the_size_limit() {
        let deep = format!(
            "<svg viewBox='0 0 24 24'>{}",
            "<g>".repeat(MAX_ASSET_BYTES / 3 - 10)
        );
        assert!(codes_of(&check(&deep)).contains(&"HUD-E0704"));
        let wide = format!(
            "<svg viewBox='0 0 24 24'>{}</svg>",
            "<a/>".repeat(MAX_ASSET_BYTES / 4 - 10)
        );
        assert!(codes_of(&check(&wide)).contains(&"HUD-E0701"));
        let amps = format!(
            "<svg viewBox='0 0 24 24'>{}<path d='M0 0'/></svg>",
            "&".repeat(MAX_ASSET_BYTES - 100)
        );
        only(&amps, "HUD-E0703");
    }
}
