//! H7's shader bisect: why does ONE vertex shader fail on a driver that compiles all its siblings?
//!
//! The first emulator run (YaGL over the host's Mesa) compiled 13 of the renderer's 14 required shader
//! pairs; the 14th, `init_gl ambient`, died in the vertex stage with `0:37(9): error: syntax error,
//! unexpected NEW_IDENTIFIER` (composed line 37 is `varying highp vec2 v_dither_nc;`), while the very same
//! declaration in `vs_src.vert` compiles. Nothing at a desk says why, so this compiles the failing source in
//! a ladder of variants, each changing ONE thing, and the answer is which rungs pass.
//!
//! Compile only: every variant is a throwaway shader object, deleted at once, never linked and never seen by
//! `plx_gfx`. It runs on the render thread in the preflight's frame, right after the pair check, whether or
//! not that passed (on a driver that accepts everything the line is one short sentence).
//!
//! Each variant is logged ONE LINE EACH, in full: the source's byte length and line count, the compile
//! status, what `glGetShaderSource` and `GL_SHADER_SOURCE_LENGTH` say about the text the driver kept (does
//! it truncate?), the driver's whole info log, and the source line the log blames. The panel gets one
//! summary line (`a- b- c+ ...`). The letters are the coordinator's: `j` is not a compile but the byte check
//! (CR, TAB, BOM) of the source as passed.

use crate::gfx_probe::{ShaderGl, Trial, BISECT_PREAMBLE, ES_PREAMBLE};
use crate::log::{plog, plog_full};

/// A `main` that writes every output the dithered vertex source declares, for variant `i`.
const TRIVIAL_MAIN: &str = "void main(){ gl_Position = vec4(a_pos,0.,1.); v_col = vec4(1.); v_dither_nc = a_pos; }\n";

/// One source to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub id: char,
    /// What was changed, for the log and the README.
    pub what: &'static str,
    /// The GLSL, without a terminating NUL.
    pub source: String,
    /// The first of the two source strings `plx_gfx` hands GL (its preamble); `None` hands ONE string, the
    /// source alone.
    pub preamble: Option<&'static str>,
}

impl Variant {
    /// The strings handed to `glShaderSource`.
    pub fn parts(&self) -> Vec<&str> {
        match self.preamble {
            Some(p) => vec![p, self.source.as_str()],
            None => vec![self.source.as_str()],
        }
    }

    /// The text the driver composes from [`Self::parts`]: what an error's line number counts in.
    pub fn composed(&self) -> String {
        self.parts().concat()
    }
}

/// The ladder, from the composed vertex source `full` (`VS_AMBIENT_DITHERED`) and the sibling `control`
/// (`VS_SRC_DITHERED`, which compiled on the emulator).
pub fn variants(full: &str, control: &str) -> Vec<Variant> {
    let (defines, rest) = split_defines(full);
    let with = |wanted: &[&str]| -> String {
        let mut s = String::new();
        for d in defines.iter().filter(|d| wanted.iter().any(|w| d.trim() == format!("#define {w}"))) {
            s.push_str(d);
            s.push('\n');
        }
        s.push_str(rest);
        s
    };
    // Every variant but `m` and `p` is handed over after the OLD, empty preamble string (what `plx_gfx` did
    // before it added its `#version 100` line), so the matrix is the one the first emulator run measured.
    let v = |id: char, what: &'static str, source: String| Variant { id, what, source, preamble: Some(BISECT_PREAMBLE) };
    vec![
        v('a', "as is, behind the OLD preamble (an empty first string, then the source: what plx_gfx handed over before it added #version 100)", full.to_string()),
        v('b', "every // comment line removed", without_comment_lines(full)),
        v('c', "`highp` dropped from the v_dither_nc line only", drop_highp_on_dither_line(full)),
        v('d', "only #define PLX_DITHER_NC", with(&["PLX_DITHER_NC"])),
        v('e', "PLX_DITHER_NC + PLX_WASH_INK", with(&["PLX_DITHER_NC", "PLX_WASH_INK"])),
        v('f', "PLX_DITHER_NC + PLX_SCRIM", with(&["PLX_DITHER_NC", "PLX_SCRIM"])),
        v('g', "`#version 100` as the first line of the source (the baseline: plx_gfx now sends this line)", format!("#version 100\n{full}")),
        v('h', "the text after #ifdef resolution (no directive left)", preprocess(full)),
        v('i', "through the v_dither_nc block, then a trivial main", format!("{}{TRIVIAL_MAIN}", through_dither_block(full))),
        v('k', "all comment text blanked, every line kept (line numbers unchanged)", blank_comments(full)),
        v('l', "control: VS_SRC_DITHERED, the sibling that compiled", control.to_string()),
        Variant { id: 'm', what: "as a but ONE source string (what the first run's preflight did)", source: full.to_string(), preamble: None },
        v('n', "the v_dither_nc declaration moved to the top", move_dither_decl_to_top(full)),
        v('o', "every precision qualifier removed", strip_precision(full)),
        Variant {
            id: 'p',
            what: "the source behind the CURRENT preamble: plx_gfx's own two strings, `#version 100` then the source (the call the renderer makes now)",
            source: full.to_string(),
            preamble: Some(ES_PREAMBLE),
        },
    ]
}

/// The leading `#define` lines of `src` (without their newlines) and the rest of the text.
fn split_defines(src: &str) -> (Vec<&str>, &str) {
    let mut defines = Vec::new();
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        if !line.starts_with("#define ") {
            break;
        }
        defines.push(line.trim_end_matches(['\n', '\r']));
        at += line.len();
    }
    (defines, &src[at..])
}

/// Is this line a comment line (blank-or-indent, then `//`)?
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Every comment line deleted, the rest as it was.
fn without_comment_lines(src: &str) -> String {
    src.split_inclusive('\n').filter(|l| !is_comment_line(l)).collect()
}

/// Comment text blanked, every line kept: a comment line becomes empty, a trailing `// ...` is cut off.
fn blank_comments(src: &str) -> String {
    src.split_inclusive('\n')
        .map(|l| {
            let nl = if l.ends_with('\n') { "\n" } else { "" };
            if is_comment_line(l) {
                return nl.to_string();
            }
            match l.find("//") {
                Some(i) => format!("{}{nl}", l[..i].trim_end()),
                None => l.to_string(),
            }
        })
        .collect()
}

/// The index of the line that declares `v_dither_nc` with a precision qualifier.
fn dither_decl_line(src: &str) -> Option<usize> {
    src.lines().position(|l| l.contains("v_dither_nc") && l.contains("highp") && !is_comment_line(l))
}

/// `highp ` taken out of the `v_dither_nc` declaration, nothing else touched.
fn drop_highp_on_dither_line(src: &str) -> String {
    let Some(at) = dither_decl_line(src) else { return src.to_string() };
    src.split_inclusive('\n')
        .enumerate()
        .map(|(i, l)| if i == at { l.replacen("highp ", "", 1) } else { l.to_string() })
        .collect()
}

/// The source up to and including the `#endif` that closes the block holding the `v_dither_nc` declaration.
/// (The coordinator's "first 35 lines" would stop before the declaration: the composed text has three
/// `#define` lines in front, and the declaration is line 37.)
fn through_dither_block(src: &str) -> String {
    let Some(at) = dither_decl_line(src) else { return src.to_string() };
    let mut out = String::new();
    for (i, l) in src.split_inclusive('\n').enumerate() {
        out.push_str(l);
        if i > at && l.trim() == "#endif" {
            break;
        }
    }
    out
}

/// The declaration moved to a plain line straight after the leading `#define`s, ahead of every comment.
fn move_dither_decl_to_top(src: &str) -> String {
    let Some(at) = dither_decl_line(src) else { return src.to_string() };
    let decl = format!("{}\n", src.lines().nth(at).unwrap_or("").trim());
    let defines = split_defines(src).0.len();
    let mut out = String::new();
    for (i, l) in src.split_inclusive('\n').enumerate() {
        if i == at {
            continue;
        }
        out.push_str(l);
        if i + 1 == defines {
            out.push_str(&decl);
        }
    }
    if defines == 0 {
        out.insert_str(0, &decl);
    }
    out
}

/// `highp`, `mediump` and `lowp` removed as whole words (and the space after each).
fn strip_precision(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < bytes.len() {
        let hit = ["highp", "mediump", "lowp"].iter().find(|w| {
            let w = w.as_bytes();
            bytes[i..].starts_with(w)
                && (i == 0 || !is_word(bytes[i - 1]))
                && bytes.get(i + w.len()).map_or(true, |c| !is_word(*c))
        });
        match hit {
            Some(w) => {
                i += w.len();
                if bytes.get(i) == Some(&b' ') {
                    i += 1;
                }
            }
            None => {
                let ch = src[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The text after `#ifdef` / `#ifndef` / `#else` / `#endif` resolution: the directives are gone, the lines
/// of an inactive branch are gone, a flag-only `#define NAME` is consumed (it is what the conditions read).
/// Everything else (`#version`, `#extension`, a `#define` with a value) is kept, though this source has
/// none. Enough for one file; it is not a C preprocessor.
pub fn preprocess(src: &str) -> String {
    struct Frame {
        parent: bool,
        taken: bool,
        active: bool,
    }
    let mut defined: Vec<String> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut out = String::new();
    for line in src.split_inclusive('\n') {
        let active = stack.last().map_or(true, |f| f.active);
        if let Some(rest) = line.trim().strip_prefix('#') {
            let rest = rest.trim_start();
            let (word, arg) = match rest.split_once(char::is_whitespace) {
                Some((w, a)) => (w, a.trim()),
                None => (rest, ""),
            };
            let name = arg.split_whitespace().next().unwrap_or("");
            match word {
                "ifdef" | "ifndef" => {
                    let cond = defined.iter().any(|d| d == name) == (word == "ifdef");
                    stack.push(Frame { parent: active, taken: cond && active, active: cond && active });
                    continue;
                }
                "else" => {
                    if let Some(f) = stack.last_mut() {
                        f.active = f.parent && !f.taken;
                        f.taken |= f.active;
                    }
                    continue;
                }
                "endif" => {
                    stack.pop();
                    continue;
                }
                "define" if active => {
                    defined.push(name.to_string());
                    if arg.split_whitespace().count() <= 1 {
                        continue;
                    }
                }
                _ => {}
            }
        }
        if active {
            out.push_str(line);
        }
    }
    out
}

/// What a source's bytes are made of (variant `j`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inspection {
    pub bytes: usize,
    pub lines: usize,
    pub cr: usize,
    pub tab: usize,
    pub nul: usize,
    pub bom: bool,
    pub non_ascii: usize,
    pub longest_line: usize,
}

pub fn inspect(src: &str) -> Inspection {
    let count = |b: u8| src.bytes().filter(|c| *c == b).count();
    Inspection {
        bytes: src.len(),
        lines: src.matches('\n').count() + usize::from(!src.is_empty() && !src.ends_with('\n')),
        cr: count(b'\r'),
        tab: count(b'\t'),
        nul: count(0),
        bom: src.starts_with('\u{feff}'),
        non_ascii: src.bytes().filter(|c| *c >= 0x80).count(),
        longest_line: src.lines().map(str::len).max().unwrap_or(0),
    }
}

impl Inspection {
    /// What the coordinator asked to hear about, if there is any: a CR, a TAB, a BOM (or a NUL). Non-ASCII
    /// bytes are ordinary here (the comments have dashes), so they are counted in [`Self::describe`] only.
    pub fn odd(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.cr > 0 {
            parts.push(format!("{} CR", self.cr));
        }
        if self.tab > 0 {
            parts.push(format!("{} TAB", self.tab));
        }
        if self.bom {
            parts.push("a leading BOM".to_string());
        }
        if self.nul > 0 {
            parts.push(format!("{} NUL", self.nul));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    pub fn describe(&self) -> String {
        format!(
            "{} bytes, {} lines, longest line {} bytes; CR {}, TAB {}, NUL {}, leading BOM {}, non-ASCII bytes {}",
            self.bytes,
            self.lines,
            self.longest_line,
            self.cr,
            self.tab,
            self.nul,
            if self.bom { "YES" } else { "no" },
            self.non_ascii
        )
    }
}

/// The line number a driver's info log blames: the second number of the first `N:N` in its first line
/// (`0:37(9): error: ...` from Mesa, `ERROR: 0:37: ...` from ANGLE, NVIDIA and Mali).
pub fn blamed_line(log: &str) -> Option<usize> {
    let first = log.lines().map(str::trim).find(|l| !l.is_empty())?;
    let b = first.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_digit()) {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if b.get(j) == Some(&b':') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
                let mut k = j + 1;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                return first[j + 1..k].parse().ok().filter(|n| *n > 0);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// How one variant went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub id: char,
    pub trial: Trial,
}

/// The whole bisect: one outcome per variant and the byte check of the source as passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BisectReport {
    pub outcomes: Vec<Outcome>,
    pub bytes: Inspection,
}

impl BisectReport {
    /// The panel's one line. `+` compiles, `-` fails; `j` is the byte check; a driver that keeps a
    /// different text than it was given is named.
    pub fn summary(&self) -> String {
        let total = self.outcomes.len();
        let mut s = if self.outcomes.iter().all(|o| o.trial.compiled) {
            format!("shader bisect: all {total} variants compile")
        } else {
            let marks: Vec<String> =
                self.outcomes.iter().map(|o| format!("{}{}", o.id, if o.trial.compiled { '+' } else { '-' })).collect();
            format!("shader bisect (+ compiles, - fails): {}", marks.join(" "))
        };
        match self.bytes.odd() {
            Some(odd) => s.push_str(&format!("; j bytes: {odd}")),
            None => s.push_str("; j bytes clean"),
        }
        // A read-back that is a stub (YaGL returns 8 bytes for every source) is not "different text": it is
        // no answer, and the log says so per variant. Only a trustworthy read-back that differs is named.
        let differ: Vec<String> =
            self.outcomes.iter().filter(|o| o.trial.readback_differs()).map(|o| o.id.to_string()).collect();
        if !differ.is_empty() {
            s.push_str(&format!("; the driver's copy of the source differs for {}", differ.join(" ")));
        }
        s
    }
}

/// Write each line of `text` as its own log line behind `prefix`, in full (`log::long`).
pub fn log_block(prefix: &str, text: &str) {
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        plog_full!("{prefix}{l}");
    }
}

/// What the driver says it kept of the text: `GL_SHADER_SOURCE_LENGTH` and `glGetShaderSource` against the
/// bytes passed. A driver that truncates its copy shows up here, not in a compile status.
fn describe_readback(t: &Trial) -> String {
    let length = match t.source_length {
        Some(n) => format!("GL_SHADER_SOURCE_LENGTH {n}"),
        None => "GL_SHADER_SOURCE_LENGTH not read".to_string(),
    };
    let got = match &t.readback {
        None => "glGetShaderSource not bound here".to_string(),
        Some(r) if t.readback_unsupported() => format!(
            "glGetShaderSource UNSUPPORTED here (returned {} bytes for {} passed: a stub, so it says nothing about the kept text)",
            r.returned, t.passed_len
        ),
        Some(r) => format!(
            "glGetShaderSource returned {} bytes, {}",
            r.returned,
            match r.first_diff {
                None if r.same => "identical to what was passed".to_string(),
                None => "DIFFERENT from what was passed".to_string(),
                Some(at) => format!("DIFFERENT from what was passed (first difference at byte {at})"),
            }
        ),
    };
    format!("passed {} bytes + NUL; {length}; {got}", t.passed_len)
}

/// Compile every variant of `full` (and the `control`) on `gl`, logging each in full as it goes, so a
/// crash in the middle leaves the log's last line naming the variant.
pub fn run(gl: &mut dyn ShaderGl, full: &str, control: &str) -> BisectReport {
    let full = full.strip_suffix('\0').unwrap_or(full);
    let control = control.strip_suffix('\0').unwrap_or(control);
    let bytes = inspect(full);
    plog!("H7 bisect j: VS_AMBIENT_DITHERED as passed: {}", bytes.describe());
    match bytes.odd() {
        Some(odd) => plog!("H7 bisect j: ODD BYTES: {odd}"),
        None => plog!("H7 bisect j: no CR, TAB, BOM or NUL"),
    }
    let mut outcomes = Vec::new();
    for v in variants(full, control) {
        let parts = v.parts();
        plog!("H7 bisect {}: compiling ({}): {}", v.id, if v.preamble.is_none() { "one string" } else { "two strings" }, v.what);
        let trial = gl.trial(false, &parts);
        let info = inspect(&v.source);
        plog!(
            "H7 bisect {}: {} bytes, {} lines -> {}; {}",
            v.id,
            info.bytes,
            info.lines,
            if trial.compiled { "COMPILED" } else { "FAILED" },
            describe_readback(&trial)
        );
        if !trial.log.is_empty() {
            log_block(&format!("H7 bisect {} info log: ", v.id), &trial.log);
        }
        if !trial.compiled {
            if let Some(n) = blamed_line(&trial.log) {
                // The driver counts in the text it composed from the strings: the preamble's lines (none for
                // the empty one, one for `#version 100`) come first.
                let composed = v.composed();
                match composed.lines().nth(n - 1) {
                    Some(text) => plog_full!("H7 bisect {}: the log blames line {n}: {text}", v.id),
                    None => plog!(
                        "H7 bisect {}: the log blames line {n}, past the end of the {}-line text it was given",
                        v.id,
                        composed.lines().count()
                    ),
                }
            }
        }
        outcomes.push(Outcome { id: v.id, trial });
    }
    let report = BisectReport { outcomes, bytes };
    plog_full!("H7 bisect: {}", report.summary());
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx_probe::Readback;

    const FULL: &str = include_str!("../../../../rust-modules/gfx/src/shaders/vs_ambient.vert");
    const SRC: &str = include_str!("../../../../rust-modules/gfx/src/shaders/vs_src.vert");

    fn composed() -> String {
        format!("#define PLX_DITHER_NC\n#define PLX_WASH_INK\n#define PLX_SCRIM\n{FULL}")
    }

    fn control() -> String {
        format!("#define PLX_DITHER_NC\n{SRC}")
    }

    fn by_id(vs: &[Variant], id: char) -> &Variant {
        vs.iter().find(|v| v.id == id).unwrap_or_else(|| panic!("variant {id}"))
    }

    fn has_token(src: &str, word: &str) -> bool {
        src.lines().filter(|l| !is_comment_line(l)).any(|l| l.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|t| t == word))
    }

    #[test]
    fn the_ladder_has_every_rung_and_each_one_changes_what_it_says() {
        let full = composed();
        let vs = variants(&full, &control());
        assert_eq!(vs.iter().map(|v| v.id).collect::<String>(), "abcdefghiklmnop", "j is the byte check, not a compile");
        let a = by_id(&vs, 'a');
        assert_eq!(a.source, full);
        assert_eq!(a.preamble, Some(BISECT_PREAMBLE), "a keeps the OLD empty preamble, so the matrix compares with the first run");
        assert_eq!(BISECT_PREAMBLE, "");
        assert_eq!(by_id(&vs, 'm').preamble, None, "m is the one-string twin of a");
        assert_eq!(by_id(&vs, 'm').source, full);
        // p is plx_gfx's own call now: its `#version 100` line, then the source, as two strings. It is the
        // same composed text as g's, which carries the line inside the source.
        let p = by_id(&vs, 'p');
        assert_eq!((p.preamble, p.source.as_str()), (Some(ES_PREAMBLE), full.as_str()));
        assert_eq!(p.parts(), vec!["#version 100\n", full.as_str()]);
        assert_eq!(p.composed(), by_id(&vs, 'g').composed());
        assert_eq!(a.parts(), vec!["", full.as_str()]);

        // The failing line really is line 37 of the composed source, as the emulator's log said.
        assert_eq!(full.lines().nth(36).map(str::trim), Some("varying highp vec2 v_dither_nc;"));
        assert_eq!(dither_decl_line(&full), Some(36));

        let b = &by_id(&vs, 'b').source;
        assert!(b.len() < full.len() && !b.lines().any(is_comment_line) && b.contains("varying highp vec2 v_dither_nc;"));
        assert_eq!(full.lines().filter(|l| !is_comment_line(l)).count(), b.lines().count(), "only comment lines went");

        let c = &by_id(&vs, 'c').source;
        assert!(c.contains("varying vec2 v_dither_nc;") && !c.contains("varying highp vec2 v_dither_nc;"));
        assert_eq!(c.lines().count(), full.lines().count());
        assert_eq!(c.replacen("varying vec2 v_dither_nc;", "varying highp vec2 v_dither_nc;", 1), full, "nothing else touched");

        for (id, defines) in [('d', 1usize), ('e', 2), ('f', 2)] {
            let s = &by_id(&vs, id).source;
            assert_eq!(s.lines().take_while(|l| l.starts_with("#define ")).count(), defines, "{id}");
            assert!(s.starts_with("#define PLX_DITHER_NC\n"), "{id}");
            assert!(s.ends_with(FULL), "{id}: the file itself is untouched");
        }
        assert!(by_id(&vs, 'e').source.contains("#define PLX_WASH_INK\n") && !by_id(&vs, 'e').source.contains("#define PLX_SCRIM\n"));
        assert!(by_id(&vs, 'f').source.contains("#define PLX_SCRIM\n") && !by_id(&vs, 'f').source.contains("#define PLX_WASH_INK\n"));

        assert_eq!(by_id(&vs, 'g').source, format!("#version 100\n{full}"));

        let h = &by_id(&vs, 'h').source;
        assert!(!h.lines().any(|l| l.trim_start().starts_with("#ifdef") || l.trim_start().starts_with("#endif") || l.trim_start().starts_with("#define")));
        assert!(h.contains("varying highp vec2 v_dither_nc;") && h.contains("float scrim_alpha(vec2 px)"));
        assert!(!h.contains("v_cuv") && !h.contains("u_art"), "PLX_ART_WASH is not defined, so its block is gone");

        let i = &by_id(&vs, 'i').source;
        assert!(i.ends_with(TRIVIAL_MAIN) && i.contains("varying highp vec2 v_dither_nc;") && i.contains("attribute vec2 a_pos;"));
        assert!(!i.contains("u_sc[12]") && i.trim_end_matches(TRIVIAL_MAIN).trim_end().ends_with("#endif"), "cut after the block's #endif");
        assert_eq!(i.lines().count(), 3 + 35 + 1, "the three defines, the file's 35 lines through its #endif, then one line of main");

        let k = &by_id(&vs, 'k').source;
        assert_eq!(k.lines().count(), full.lines().count(), "line numbers do not move");
        assert!(!k.contains("//") && k.contains("varying highp vec2 v_dither_nc;"));
        assert_eq!(k.lines().nth(36).map(str::trim), Some("varying highp vec2 v_dither_nc;"), "the blamed line is still line 37");

        assert_eq!(by_id(&vs, 'l').source, control());

        let n = &by_id(&vs, 'n').source;
        assert_eq!(n.lines().nth(3), Some("varying highp vec2 v_dither_nc;"), "right after the three defines");
        assert_eq!(n.lines().count(), full.lines().count(), "moved, not copied");
        assert_eq!(n.matches("v_dither_nc").count(), full.matches("v_dither_nc").count());

        let o = &by_id(&vs, 'o').source;
        assert!(!has_token(o, "highp") && !has_token(o, "mediump") && !has_token(o, "lowp"));
        assert!(!o.contains("uniform  ") && o.contains("varying vec2 v_dither_nc;") && o.contains("uniform vec4 u_art;"));
    }

    #[test]
    fn a_variant_never_equals_a_unless_it_is_a_twin_so_no_rung_is_a_silent_no_op() {
        let full = composed();
        for v in variants(&full, &control()) {
            if !matches!(v.id, 'a' | 'm' | 'p') {
                assert_ne!(v.source, full, "variant {} changed nothing", v.id);
            }
        }
    }

    #[test]
    fn the_ladder_works_on_a_source_without_the_lines_it_looks_for_and_changes_nothing_it_cannot_find() {
        let vs = variants("void main(){}\n", "void main(){}\n");
        assert_eq!(vs.len(), 15);
        assert_eq!(by_id(&vs, 'c').source, "void main(){}\n");
        assert_eq!(by_id(&vs, 'i').source, format!("void main(){{}}\n{TRIVIAL_MAIN}"));
    }

    #[test]
    fn the_preprocessor_resolves_nesting_else_and_ifndef() {
        let src = "#define A\n#define B 1.0\nx0\n#ifdef A\nx1\n#ifdef Z\nx2\n#else\nx3\n#endif\n#ifndef A\nx4\n#else\nx5\n#endif\n#endif\n#ifdef Z\nx6\n#else\n#ifndef B\nx7\n#endif\nx8\n#endif\nx9\n";
        let out = preprocess(src);
        assert_eq!(out.lines().collect::<Vec<_>>(), vec!["#define B 1.0", "x0", "x1", "x3", "x5", "x8", "x9"]);
        // A flag-only define is consumed; one with a value stays (and still counts as defined).
        assert!(!out.contains("#define A"));
    }

    #[test]
    fn a_define_inside_an_inactive_block_defines_nothing() {
        assert_eq!(preprocess("#ifdef NOPE\n#define X\n#endif\n#ifdef X\nbad\n#endif\nok\n"), "ok\n");
    }

    #[test]
    fn the_real_source_resolves_to_the_branches_the_three_defines_select() {
        let h = preprocess(&composed());
        for kept in ["varying highp vec2 v_dither_nc;", "uniform vec3 u_ink;", "uniform vec2 u_inka;", "uniform vec4 u_sc[12];", "v_dither_nc = px * (1.0 / 256.0);", "inka = mix(inka, 1.0, scrim_alpha(px));", "v_col.rgb = mix(v_col.rgb, u_ink, inka);", "v_col = mix(mix(u_atl"] {
            assert!(h.contains(kept), "{kept}");
        }
        for gone in ["v_inka", "u_uvrect", "v_col = vec4(u_ink, scrim_alpha(px));"] {
            assert!(!h.contains(gone), "{gone}");
        }
        // The scrim-layer sibling takes the other branch of the `#else`.
        let layer = preprocess(&format!("#define PLX_SCRIM\n#define PLX_SCRIM_LAYER\n{FULL}"));
        assert!(layer.contains("v_col = vec4(u_ink, scrim_alpha(px));") && !layer.contains("v_col = mix(mix(u_atl"));
        assert!(layer.contains("uniform vec3 u_ink;") && !layer.contains("v_dither_nc"));
    }

    #[test]
    fn the_byte_check_counts_cr_tab_bom_and_nul() {
        let clean = inspect(&composed());
        assert_eq!((clean.cr, clean.tab, clean.nul, clean.bom), (0, 0, 0, false), "{clean:?}");
        assert_eq!(clean.odd(), None);
        assert!(clean.bytes > 6000 && clean.lines > 100 && clean.non_ascii > 0, "the comments have dashes: {clean:?}");
        let dirty = inspect("\u{feff}a\r\nb\tc\t\r\n\0");
        assert_eq!((dirty.cr, dirty.tab, dirty.nul, dirty.bom, dirty.lines), (2, 2, 1, true, 3));
        assert_eq!(dirty.odd().as_deref(), Some("2 CR, 2 TAB, a leading BOM, 1 NUL"));
        assert!(dirty.describe().contains("leading BOM YES") && clean.describe().contains("leading BOM no"));
        assert_eq!(inspect(""), Inspection::default());
    }

    #[test]
    fn the_blamed_line_is_read_from_mesa_angle_and_nvidia_logs() {
        assert_eq!(blamed_line("0:37(9): error: syntax error, unexpected NEW_IDENTIFIER"), Some(37));
        assert_eq!(blamed_line("ERROR: 0:12: 'gl_LastFragColorARM' : undeclared identifier"), Some(12));
        assert_eq!(blamed_line("\n  1:200(3): warning: x\n0:4: later"), Some(200), "the first line with content");
        assert_eq!(blamed_line("(0) : error C0000: syntax error"), None, "no N:N");
        assert_eq!(blamed_line(""), None);
        assert_eq!(blamed_line("0:0: nothing at line zero"), None);
    }

    // --- a fake driver ------------------------------------------------------------------------------

    /// A desktop GLSL 1.10 front end: `highp`, `mediump`, `lowp` are not keywords there (no `#version 100`),
    /// so one on any active line is the error the emulator printed. What the first emulator run guessed at.
    struct Desktop110 {
        /// How many source strings each trial was handed.
        trials: Vec<usize>,
        keep_different_copy: bool,
    }

    impl ShaderGl for Desktop110 {
        fn compile(&mut self, _fragment: bool, _src: &str) -> Result<u32, String> {
            unreachable!("the bisect only trials")
        }
        fn link(&mut self, _vs: u32, _fs: u32) -> Result<u32, String> {
            unreachable!("the bisect never links")
        }
        fn delete(&mut self, _vs: u32, _fs: u32, _program: Option<u32>) {}
        fn trial(&mut self, _fragment: bool, parts: &[&str]) -> Trial {
            let joined = parts.concat();
            let resolved = preprocess(&joined);
            let bad = if joined.starts_with("#version 100") {
                None
            } else {
                resolved.lines().position(|l| !is_comment_line(l) && l.split(|c: char| !c.is_alphanumeric()).any(|t| matches!(t, "highp" | "mediump" | "lowp")))
            };
            self.trials.push(parts.len());
            Trial {
                compiled: bad.is_none(),
                log: bad.map(|n| format!("0:{}(9): error: syntax error, unexpected NEW_IDENTIFIER\n0:{}(9): error: a second line", n + 1, n + 1)).unwrap_or_default(),
                passed_len: joined.len(),
                source_length: Some(joined.len() as i32 + 1),
                readback: Some(Readback {
                    returned: joined.len() as i32 - i32::from(self.keep_different_copy),
                    same: !self.keep_different_copy,
                    first_diff: self.keep_different_copy.then_some(joined.len() - 1),
                }),
            }
        }
    }

    #[test]
    fn on_a_desktop_110_driver_only_the_rungs_that_remove_the_precision_qualifier_pass() {
        let _g = crate::testutil::log_serial();
        let mut gl = Desktop110 { trials: Vec::new(), keep_different_copy: false };
        let report = run(&mut gl, &format!("{}\0", composed()), &format!("{}\0", control()));
        assert_eq!(gl.trials.len(), 15);
        assert_eq!(gl.trials.iter().filter(|parts| **parts == 1).count(), 1, "only m is a one-string call");
        assert_eq!(
            report.summary(),
            "shader bisect (+ compiles, - fails): a- b- c+ d- e- f- g+ h- i- k- l- m- n- o+ p+; j bytes clean"
        );
        let a = &report.outcomes[0];
        assert!(!a.trial.compiled && a.trial.passed_len == composed().len(), "{a:?}");
        assert!(a.trial.log.starts_with("0:") && a.trial.log.contains("NEW_IDENTIFIER"), "{}", a.trial.log);
    }

    #[test]
    fn a_driver_that_accepts_everything_gets_one_short_sentence() {
        struct Accepting;
        impl ShaderGl for Accepting {
            fn compile(&mut self, _: bool, _: &str) -> Result<u32, String> {
                Ok(1)
            }
            fn link(&mut self, _: u32, _: u32) -> Result<u32, String> {
                Ok(1)
            }
            fn delete(&mut self, _: u32, _: u32, _: Option<u32>) {}
            fn trial(&mut self, _: bool, parts: &[&str]) -> Trial {
                Trial { compiled: true, passed_len: parts.concat().len(), ..Trial::default() }
            }
        }
        let _g = crate::testutil::log_serial();
        let report = run(&mut Accepting, &composed(), &control());
        assert_eq!(report.summary(), "shader bisect: all 15 variants compile; j bytes clean");
        assert!(report.summary().chars().count() < 70);
    }

    #[test]
    fn a_driver_that_keeps_a_different_copy_of_the_source_is_named_in_the_summary() {
        let _g = crate::testutil::log_serial();
        let mut gl = Desktop110 { trials: Vec::new(), keep_different_copy: true };
        let s = run(&mut gl, &composed(), &control()).summary();
        assert!(s.ends_with("; the driver's copy of the source differs for a b c d e f g h i k l m n o p"), "{s}");
    }

    /// YaGL's `glGetShaderSource` returned 8 bytes for every source, the compiling ones included: a stub.
    /// That is no evidence of an altered copy, so the summary does not name it and the log says why.
    #[test]
    fn a_stub_glgetshadersource_is_labelled_unsupported_not_reported_as_a_different_copy() {
        let stub = |compiled: bool| Trial {
            compiled,
            passed_len: 1500,
            source_length: Some(1501),
            readback: Some(Readback { returned: 8, same: false, first_diff: Some(8) }),
            ..Trial::default()
        };
        assert!(stub(true).readback_unsupported() && !stub(true).readback_differs());
        let report = BisectReport {
            outcomes: vec![Outcome { id: 'a', trial: stub(false) }, Outcome { id: 'g', trial: stub(true) }],
            bytes: inspect("x\n"),
        };
        let s = report.summary();
        assert_eq!(s, "shader bisect (+ compiles, - fails): a- g+; j bytes clean");
        let described = describe_readback(&stub(true));
        assert!(described.contains("glGetShaderSource UNSUPPORTED here (returned 8 bytes for 1500 passed"), "{described}");
        assert!(!described.contains("DIFFERENT"), "{described}");

        // A real read-back that differs is still named; so is a short source whose copy is all there is.
        let real = Trial { passed_len: 1500, readback: Some(Readback { returned: 1400, same: false, first_diff: Some(1400) }), ..Trial::default() };
        assert!(!real.readback_unsupported() && real.readback_differs());
        let tiny = Trial { passed_len: 8, readback: Some(Readback { returned: 8, same: true, first_diff: None }), ..Trial::default() };
        assert!(!tiny.readback_unsupported() && !tiny.readback_differs());
        assert!(describe_readback(&real).contains("DIFFERENT from what was passed (first difference at byte 1400)"));
    }

    #[test]
    fn the_panel_summary_of_the_worst_case_is_one_short_line() {
        let _g = crate::testutil::log_serial();
        let mut gl = Desktop110 { trials: Vec::new(), keep_different_copy: false };
        let s = run(&mut gl, &composed(), &control()).summary();
        assert!(s.chars().count() <= 110, "{} chars: {s}", s.chars().count());
    }
}
