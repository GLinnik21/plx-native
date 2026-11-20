//! H7's shader bisect and link variants: why does ONE vertex shader fail on a driver that compiles all its
//! siblings, and what does the driver want of a pair?
//!
//! The first emulator run (YaGL over the host's Mesa) compiled 13 of the renderer's 14 required shader
//! pairs; the 14th, `init_gl ambient`, died in the vertex stage with `0:37(9): error: syntax error,
//! unexpected NEW_IDENTIFIER` (composed line 37 is `varying highp vec2 v_dither_nc;`), while the very same
//! declaration in `vs_src.vert` compiles. Nothing at a desk says why, so this compiles the failing source in
//! a ladder of variants, each changing ONE thing, and the answer is which rungs pass.
//!
//! The second run found `#version 100` fixes the COMPILE but not the LINK ("all shaders must use same
//! shading language version"), and the working hypothesis became `vs_ambient.vert`'s `uniform vec4 u_atl,
//! u_atr, u_abr, u_abl;`, the only statement with several declarators in the renderer's shaders. That
//! statement is split into four declarations on this branch, and what is measured now is BEFORE and AFTER:
//!
//! * **The ladder** (`a0`, `a` ... `o`; compile only) is built from the source `plx_gfx` compiles now, the
//!   SPLIT `vs_ambient.vert`. The one exception is `a0`: a frozen, literal copy of the ORIGINAL unsplit
//!   source ([`Sources::original`]), compiled as `a` is, so `a0` against `a` is the before and after.
//! * **The link variants** (`L1` ... `i2`) compile a vertex and a fragment source and LINK them, logging both
//!   stages' status and every info log in full: `L1`/`L2` the split source without and with `#version 100`
//!   on both stages, `L3`/`L4` the original without and with, `L5`/`L5v` a cross-link, `i0`/`i2` variant `i`
//!   (the shortest failing cut) on the original and on the split source.
//!
//! Nothing here is ever kept: every variant is a throwaway object, deleted at once, never seen by `plx_gfx`.
//! It runs on the render thread in the preflight's frame, right after the pair check, whether or not that
//! passed (on a driver that accepts everything each line is one short sentence).
//!
//! Each variant is logged in full: the source's byte length and line count, the compile (and link) status,
//! what `glGetShaderSource` and `GL_SHADER_SOURCE_LENGTH` say about the text the driver kept (does it
//! truncate?), the driver's whole info logs, and the source line a compile failure blames. The panel gets
//! one summary line each (`a0- a+ b+ ...`, `L1+ L2+ L3:vs- ...`). The letters are the coordinator's: `j` is
//! not a compile but the byte check (CR, TAB, BOM) of the source as passed.

use crate::gfx_probe::{LinkTrial, LinkVerdict, ShaderGl, Trial, ES_PREAMBLE, VERSION_100};
use crate::log::{plog, plog_full};

/// A `main` that writes every output the dithered vertex source declares, for variant `i`.
const TRIVIAL_MAIN: &str = "void main(){ gl_Position = vec4(a_pos,0.,1.); v_col = vec4(1.); v_dither_nc = a_pos; }\n";

/// The texts everything here is built from, each a complete shader source as `plx_gfx` composes it (a NUL
/// at the end is ignored).
#[derive(Clone, Copy, Debug)]
pub struct Sources<'a> {
    /// `VS_AMBIENT_DITHERED`: three `#define`s in front of the repository's `vs_ambient.vert`, which has
    /// its four corner uniforms declared one to a statement. What `plx_gfx` compiles.
    pub full: &'a str,
    /// The same, from the frozen copy of `vs_ambient.vert` as it was before the split (`u_atl, u_atr,
    /// u_abr, u_abl` in one statement).
    pub original: &'a str,
    /// The sibling that compiled on the first run (`VS_SRC_DITHERED`).
    pub control: &'a str,
    /// The fragment source the ambient programs link against (`FS_AMBIENT`).
    pub fs: &'a str,
}

fn text(s: &str) -> &str {
    s.strip_suffix('\0').unwrap_or(s)
}

/// One source to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub id: &'static str,
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

/// The ladder, compile only: the split source `full` and its changes, `a0` the original, `l` the sibling.
pub fn variants(src: &Sources) -> Vec<Variant> {
    let (full, original, control) = (text(src.full), text(src.original), text(src.control));
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
    // Every variant but `m` is handed over the way `plx_gfx` hands its sources: two strings, `ES_PREAMBLE`
    // (empty) first.
    let v = |id: &'static str, what: &'static str, source: String| Variant { id, what, source, preamble: Some(ES_PREAMBLE) };
    vec![
        v("a0", "BEFORE: the ORIGINAL source, `uniform vec4 u_atl, u_atr, u_abr, u_abl;` in one statement, behind plx_gfx's own two-string call (an empty first string): `a` as the first two emulator runs measured it", original.to_string()),
        v("a", "AFTER: as is, the repository's SPLIT source (one declarator to a statement) behind plx_gfx's own two-string call: what plx_gfx compiles now", full.to_string()),
        v("b", "every // comment line removed", without_comment_lines(full)),
        v("c", "`highp` dropped from the v_dither_nc line only", drop_highp_on_dither_line(full)),
        v("d", "only #define PLX_DITHER_NC", with(&["PLX_DITHER_NC"])),
        v("e", "PLX_DITHER_NC + PLX_WASH_INK", with(&["PLX_DITHER_NC", "PLX_WASH_INK"])),
        v("f", "PLX_DITHER_NC + PLX_SCRIM", with(&["PLX_DITHER_NC", "PLX_SCRIM"])),
        v("g", "`#version 100` as the first line of the source (the compile-only twin of L2's vertex stage)", format!("{VERSION_100}{full}")),
        v("h", "the text after #ifdef resolution (no directive left)", preprocess(full)),
        v("i", "through the v_dither_nc block, then a trivial main", format!("{}{TRIVIAL_MAIN}", through_dither_block(full))),
        v("k", "all comment text blanked, every line kept (line numbers unchanged)", blank_comments(full)),
        v("l", "control: VS_SRC_DITHERED, the sibling that compiled", control.to_string()),
        Variant { id: "m", what: "as a but ONE source string (what the first run's preflight did)", source: full.to_string(), preamble: None },
        v("n", "the v_dither_nc declaration moved to the top", move_dither_decl_to_top(full)),
        v("o", "every precision qualifier removed", strip_precision(full)),
    ]
}

/// One vertex source and one fragment source to compile and link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkVariant {
    pub id: &'static str,
    /// What was changed, for the log and the README.
    pub what: &'static str,
    pub vs: String,
    pub fs: String,
    /// The first string handed ahead of BOTH sources ([`ES_PREAMBLE`] or [`VERSION_100`]).
    pub preamble: &'static str,
}

impl LinkVariant {
    pub fn vs_parts(&self) -> Vec<&str> {
        vec![self.preamble, self.vs.as_str()]
    }

    pub fn fs_parts(&self) -> Vec<&str> {
        vec![self.preamble, self.fs.as_str()]
    }
}

/// `src` with `varying vec4 v_col;` declared ahead of the first non-`#define` line and `v_col` written first
/// thing in `main`: the sibling's vertex stage made complete for `fs_ambient.frag`, which reads `v_col`.
fn with_v_col(src: &str) -> String {
    let (defines, rest) = split_defines(src);
    let mut s = String::new();
    for d in defines {
        s.push_str(d);
        s.push('\n');
    }
    s.push_str("varying vec4 v_col;\n");
    s.push_str(&rest.replacen("void main(){", "void main(){ v_col = vec4(1.0);", 1));
    s
}

/// The link variants, in the order they are run.
pub fn link_variants(src: &Sources) -> Vec<LinkVariant> {
    let (full, original, control, fs) = (text(src.full), text(src.original), text(src.control), text(src.fs));
    let l = |id: &'static str, what: &'static str, vs: String, preamble: &'static str| LinkVariant { id, what, vs, fs: fs.to_string(), preamble };
    vec![
        l("L1", "the SPLIT VS_AMBIENT_DITHERED + FS_AMBIENT, an empty first string on both stages: what plx_gfx does now", full.to_string(), ES_PREAMBLE),
        l("L2", "L1 with `#version 100` as the first string of BOTH stages", full.to_string(), VERSION_100),
        l("L3", "the ORIGINAL unsplit VS_AMBIENT_DITHERED + FS_AMBIENT, an empty first string (expected: the vertex stage does not compile)", original.to_string(), ES_PREAMBLE),
        l("L4", "the ORIGINAL unsplit source with `#version 100` on both stages (expected: both compile, the link fails)", original.to_string(), VERSION_100),
        l("L5", "cross-link: VS_SRC_DITHERED + FS_AMBIENT, an empty first string (NB vs_src.vert declares no v_col, which fs_ambient.frag reads: a link failure here only counts if the log names versions, not v_col)", control.to_string(), ES_PREAMBLE),
        l("L5v", "L5 with `varying vec4 v_col;` declared and written by the vertex source, so the pair is complete and a link failure is not the missing varying", with_v_col(control), ES_PREAMBLE),
        l("i0", "variant i (through the v_dither_nc block, then a trivial main) cut from the ORIGINAL source + FS_AMBIENT, an empty first string", format!("{}{TRIVIAL_MAIN}", through_dither_block(original)), ES_PREAMBLE),
        l("i2", "variant i cut from the SPLIT source + FS_AMBIENT, an empty first string: the shortest failing source with the split applied", format!("{}{TRIVIAL_MAIN}", through_dither_block(full)), ES_PREAMBLE),
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
    pub id: &'static str,
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

/// The non-empty lines of `text`, each behind `prefix`: one log line each.
pub fn block_lines(prefix: &str, text: &str) -> Vec<String> {
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("{prefix}{l}")).collect()
}

/// Write each line of `text` as its own log line behind `prefix`, in full (`log::long`).
pub fn log_block(prefix: &str, text: &str) {
    for l in block_lines(prefix, text) {
        plog_full!("{l}");
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

/// Log the source line a compile failure blames. The driver counts in the text it composed from the strings:
/// the preamble's lines (none for the empty one, one for `#version 100`) come first.
fn blamed_note(tag: &str, composed: &str, log: &str) -> Option<String> {
    let n = blamed_line(log)?;
    Some(match composed.lines().nth(n - 1) {
        Some(text) => format!("{tag}: the log blames line {n}: {text}"),
        None => format!("{tag}: the log blames line {n}, past the end of the {}-line text it was given", composed.lines().count()),
    })
}

/// Compile every variant of the ladder on `gl`, logging each in full as it goes, so a crash in the middle
/// leaves the log's last line naming the variant.
pub fn run(gl: &mut dyn ShaderGl, src: &Sources) -> BisectReport {
    let bytes = inspect(text(src.full));
    plog!("H7 bisect j: VS_AMBIENT_DITHERED as passed: {}", bytes.describe());
    match bytes.odd() {
        Some(odd) => plog!("H7 bisect j: ODD BYTES: {odd}"),
        None => plog!("H7 bisect j: no CR, TAB, BOM or NUL"),
    }
    let mut outcomes = Vec::new();
    for v in variants(src) {
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
            if let Some(note) = blamed_note(&format!("H7 bisect {}", v.id), &v.composed(), &trial.log) {
                plog_full!("{note}");
            }
        }
        outcomes.push(Outcome { id: v.id, trial });
    }
    let report = BisectReport { outcomes, bytes };
    plog_full!("H7 bisect: {}", report.summary());
    report
}

/// How one link variant went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkOutcome {
    pub id: &'static str,
    pub trial: LinkTrial,
}

/// All the link variants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkReport {
    pub outcomes: Vec<LinkOutcome>,
}

impl LinkOutcome {
    /// The token the panel shows for it: `L1+` linked, `L3:vs-` the vertex stage did not compile, `:fs-` the
    /// fragment stage, `:both-` both, `L4:link-` both compiled and the link failed.
    pub fn mark(&self) -> String {
        match self.trial.verdict() {
            LinkVerdict::Linked => format!("{}+", self.id),
            LinkVerdict::VertexFailed => format!("{}:vs-", self.id),
            LinkVerdict::FragmentFailed => format!("{}:fs-", self.id),
            LinkVerdict::BothFailed => format!("{}:both-", self.id),
            LinkVerdict::LinkFailed => format!("{}:link-", self.id),
        }
    }
}

impl LinkReport {
    /// The panel's one line: every variant's [`LinkOutcome::mark`], or one short sentence when all linked.
    /// (`vs-` / `fs-` is a stage that did not compile, `link-` a pair that compiled and did not link; the
    /// log has the driver's words for each.)
    pub fn summary(&self) -> String {
        if self.outcomes.iter().all(|o| o.trial.verdict() == LinkVerdict::Linked) {
            return format!("shader link: all {} variants compile and link", self.outcomes.len());
        }
        let marks: Vec<String> = self.outcomes.iter().map(LinkOutcome::mark).collect();
        format!("shader link: {}", marks.join(" "))
    }
}

/// A stage's compile status, as the log words it.
fn stage_word(ok: bool) -> &'static str {
    if ok {
        "COMPILED"
    } else {
        "FAILED"
    }
}

/// How a link variant ended, in words for the log.
fn verdict_words(v: LinkVerdict) -> &'static str {
    match v {
        LinkVerdict::Linked => "linked",
        LinkVerdict::VertexFailed => "the vertex stage does not compile",
        LinkVerdict::FragmentFailed => "the fragment stage does not compile",
        LinkVerdict::BothFailed => "neither stage compiles",
        LinkVerdict::LinkFailed => "both stages compile, the link fails",
    }
}

/// The log lines for one finished link variant, in order: the result, then every info log (read on success
/// too, and a stage or link that failed with nothing to say is named), each line of it whole, then the
/// source line each failed compile blames.
pub fn link_log_lines(v: &LinkVariant, trial: &LinkTrial) -> Vec<String> {
    let (vs, fs) = (v.vs_parts().concat(), v.fs_parts().concat());
    let link = match &trial.link {
        None => "not attempted (a stage did not compile)",
        Some(l) if l.ok => "LINKED",
        Some(_) => "FAILED",
    };
    let mut out = vec![format!(
        "H7 link {}: vertex {} ({} bytes), fragment {} ({} bytes), link {link} => {}",
        v.id,
        stage_word(trial.vs.ok),
        vs.len(),
        stage_word(trial.fs.ok),
        fs.len(),
        verdict_words(trial.verdict())
    )];
    let stages = [("vertex", trial.vs.ok, &trial.vs.log), ("fragment", trial.fs.ok, &trial.fs.log)];
    for (stage, ok, log) in stages.into_iter().chain(trial.link.iter().map(|l| ("link", l.ok, &l.log))) {
        if !log.is_empty() {
            out.extend(block_lines(&format!("H7 link {} {stage} info log: ", v.id), log));
        } else if !ok {
            out.push(format!("H7 link {} {stage} info log: (the driver gave no info log)", v.id));
        }
    }
    for (stage, ok, log, composed) in [("vertex", trial.vs.ok, &trial.vs.log, &vs), ("fragment", trial.fs.ok, &trial.fs.log, &fs)] {
        if !ok {
            out.extend(blamed_note(&format!("H7 link {} {stage}", v.id), composed, log));
        }
    }
    out
}

/// Compile and link every link variant on `gl`, logging each in full as it goes.
pub fn run_links(gl: &mut dyn ShaderGl, src: &Sources) -> LinkReport {
    let mut outcomes = Vec::new();
    for v in link_variants(src) {
        let words = if v.preamble.is_empty() { "an empty first string on both stages" } else { "`#version 100` as the first string of both stages" };
        plog!("H7 link {}: compiling both stages and linking ({words}): {}", v.id, v.what);
        let trial = gl.link_trial(&v.vs_parts(), &v.fs_parts());
        for line in link_log_lines(&v, &trial) {
            plog_full!("{line}");
        }
        outcomes.push(LinkOutcome { id: v.id, trial });
    }
    let report = LinkReport { outcomes };
    plog_full!("H7 link: {}", report.summary());
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx_probe::{Readback, Status};

    /// The repository's `vs_ambient.vert`: the corner uniforms one to a statement.
    const FULL: &str = include_str!("../../../../rust-modules/gfx/src/shaders/vs_ambient.vert");
    /// The frozen copy of it from before the split.
    const UNSPLIT: &str = include_str!("probe_shaders/vs_ambient_unsplit.vert");
    const SRC: &str = include_str!("../../../../rust-modules/gfx/src/shaders/vs_src.vert");
    const DITHER: &str = include_str!("../../../../rust-modules/gfx/src/shaders/dither.glsl");
    const FS_AMBIENT: &str = include_str!("../../../../rust-modules/gfx/src/shaders/fs_ambient.frag");

    const DEFINES: &str = "#define PLX_DITHER_NC\n#define PLX_WASH_INK\n#define PLX_SCRIM\n";
    const FOUR: &str = "uniform vec4 u_atl;\nuniform vec4 u_atr;\nuniform vec4 u_abr;\nuniform vec4 u_abl;\n";
    const ONE: &str = "uniform vec4 u_atl, u_atr, u_abr, u_abl;\n";

    fn composed() -> String {
        format!("{DEFINES}{FULL}")
    }

    fn original() -> String {
        format!("{DEFINES}{UNSPLIT}")
    }

    fn control() -> String {
        format!("#define PLX_DITHER_NC\n{SRC}")
    }

    fn fs() -> String {
        format!("{DITHER}{FS_AMBIENT}")
    }

    /// The four texts the bisect is built from, owned.
    struct Texts {
        full: String,
        original: String,
        control: String,
        fs: String,
    }

    impl Texts {
        fn real() -> Texts {
            Texts { full: composed(), original: original(), control: control(), fs: fs() }
        }

        fn sources(&self) -> Sources<'_> {
            Sources { full: &self.full, original: &self.original, control: &self.control, fs: &self.fs }
        }
    }

    fn by_id<'a>(vs: &'a [Variant], id: &str) -> &'a Variant {
        vs.iter().find(|v| v.id == id).unwrap_or_else(|| panic!("variant {id}"))
    }

    fn link_by_id<'a>(vs: &'a [LinkVariant], id: &str) -> &'a LinkVariant {
        vs.iter().find(|v| v.id == id).unwrap_or_else(|| panic!("link variant {id}"))
    }

    fn has_token(src: &str, word: &str) -> bool {
        src.lines().filter(|l| !is_comment_line(l)).any(|l| l.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|t| t == word))
    }

    /// A statement that declares several variables at once: a top-level `uniform`, `varying`, `attribute`,
    /// `const` or type-led line with a comma before its `;` and no call in between. What the split removed.
    fn multi_declarator_lines(src: &str) -> Vec<String> {
        const LEAD: [&str; 18] = [
            "uniform", "varying", "attribute", "const", "highp", "mediump", "lowp", "float", "int", "bool", "vec2", "vec3", "vec4",
            "mat2", "mat3", "mat4", "sampler2D", "ivec2",
        ];
        src.lines()
            .map(|l| l.split("//").next().unwrap_or("").trim())
            .filter(|l| LEAD.iter().any(|w| l.split_whitespace().next() == Some(*w)))
            .filter(|l| {
                let stmt = l.split(';').next().unwrap_or("");
                l.contains(';') && stmt.contains(',') && !stmt.contains('(')
            })
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_frozen_original_is_the_repository_file_with_the_four_corner_uniforms_joined_again() {
        assert_eq!(FULL.matches(FOUR).count(), 1, "the repository's file declares the four corner uniforms one to a statement");
        assert_eq!(FULL.replacen(FOUR, ONE, 1), UNSPLIT, "the frozen copy differs from the repository's file by that one statement and nothing else");
        assert!(!FULL.contains(ONE) && UNSPLIT.contains(ONE) && !UNSPLIT.contains(FOUR));
        assert_eq!(UNSPLIT.lines().count() + 3, FULL.lines().count(), "one statement became four lines");
    }

    /// The hypothesis behind the split: `uniform vec4 u_atl, u_atr, u_abr, u_abl;` is the ONLY statement of the
    /// renderer's shaders with several declarators. If another one is ever added the probe is blind to it.
    #[test]
    fn the_corner_uniforms_were_the_only_multi_declarator_statement_in_the_renderers_shaders() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../rust-modules/gfx/src/shaders");
        let mut found = Vec::new();
        let mut files = 0;
        for entry in std::fs::read_dir(dir).expect("the renderer's shader directory") {
            let path = entry.unwrap().path();
            files += 1;
            let src = std::fs::read_to_string(&path).unwrap();
            for l in multi_declarator_lines(&src) {
                found.push(format!("{}: {l}", path.file_name().unwrap().to_string_lossy()));
            }
        }
        assert!(files > 20, "{files} shader files read");
        assert_eq!(found, Vec::<String>::new(), "a statement with several declarators is back in the renderer's shaders");
        assert_eq!(multi_declarator_lines(UNSPLIT), vec!["uniform vec4 u_atl, u_atr, u_abr, u_abl;".to_string()], "the original had exactly one");
        assert_eq!(multi_declarator_lines("void main(){ float a = max(1.0, 2.0); }\nvec2 d = vec2(1.0, 2.0);\n"), Vec::<String>::new());
    }

    #[test]
    fn the_ladder_has_every_rung_and_each_one_changes_what_it_says() {
        let t = Texts::real();
        let full = composed();
        let vs = variants(&t.sources());
        assert_eq!(
            vs.iter().map(|v| v.id).collect::<Vec<_>>(),
            vec!["a0", "a", "b", "c", "d", "e", "f", "g", "h", "i", "k", "l", "m", "n", "o"],
            "j is the byte check, not a compile"
        );
        // The rule: the plain letters are built from the SPLIT source (what plx_gfx compiles now); a0 is the
        // frozen ORIGINAL, compiled the way a is.
        let a = by_id(&vs, "a");
        assert_eq!(a.source, full);
        assert_eq!(a.preamble, Some(ES_PREAMBLE), "a is plx_gfx's own call: an empty first string, then the source");
        assert_eq!(ES_PREAMBLE, "");
        let a0 = by_id(&vs, "a0");
        assert_eq!((a0.source.as_str(), a0.preamble), (original().as_str(), Some(ES_PREAMBLE)));
        assert!(a0.source.contains(ONE) && !a.source.contains(ONE) && a.source.contains(FOUR));
        assert_eq!(by_id(&vs, "m").preamble, None, "m is the one-string twin of a");
        assert_eq!(by_id(&vs, "m").source, full);
        assert_eq!(a.parts(), vec!["", full.as_str()]);

        // The failing line of the emulator's log, `0:37(9)`, is line 37 of the ORIGINAL composed source; the
        // split moved it down by the three lines the statement grew by.
        let orig = original();
        assert_eq!(orig.lines().nth(36).map(str::trim), Some("varying highp vec2 v_dither_nc;"));
        assert_eq!(dither_decl_line(&orig), Some(36));
        assert_eq!(full.lines().nth(39).map(str::trim), Some("varying highp vec2 v_dither_nc;"));
        assert_eq!(dither_decl_line(&full), Some(39));

        let b = &by_id(&vs, "b").source;
        assert!(b.len() < full.len() && !b.lines().any(is_comment_line) && b.contains("varying highp vec2 v_dither_nc;"));
        assert_eq!(full.lines().filter(|l| !is_comment_line(l)).count(), b.lines().count(), "only comment lines went");

        let c = &by_id(&vs, "c").source;
        assert!(c.contains("varying vec2 v_dither_nc;") && !c.contains("varying highp vec2 v_dither_nc;"));
        assert_eq!(c.lines().count(), full.lines().count());
        assert_eq!(c.replacen("varying vec2 v_dither_nc;", "varying highp vec2 v_dither_nc;", 1), full, "nothing else touched");

        for (id, defines) in [("d", 1usize), ("e", 2), ("f", 2)] {
            let s = &by_id(&vs, id).source;
            assert_eq!(s.lines().take_while(|l| l.starts_with("#define ")).count(), defines, "{id}");
            assert!(s.starts_with("#define PLX_DITHER_NC\n"), "{id}");
            assert!(s.ends_with(FULL), "{id}: the file itself is untouched");
        }
        assert!(by_id(&vs, "e").source.contains("#define PLX_WASH_INK\n") && !by_id(&vs, "e").source.contains("#define PLX_SCRIM\n"));
        assert!(by_id(&vs, "f").source.contains("#define PLX_SCRIM\n") && !by_id(&vs, "f").source.contains("#define PLX_WASH_INK\n"));

        assert_eq!(by_id(&vs, "g").source, format!("#version 100\n{full}"));
        assert_eq!(VERSION_100, "#version 100\n", "g, L2 and L4 hand over a literal line, whatever plx_gfx's preamble is");

        let h = &by_id(&vs, "h").source;
        assert!(!h.lines().any(|l| l.trim_start().starts_with("#ifdef") || l.trim_start().starts_with("#endif") || l.trim_start().starts_with("#define")));
        assert!(h.contains("varying highp vec2 v_dither_nc;") && h.contains("float scrim_alpha(vec2 px)"));
        assert!(!h.contains("v_cuv") && !h.contains("u_art"), "PLX_ART_WASH is not defined, so its block is gone");

        let i = &by_id(&vs, "i").source;
        assert!(i.ends_with(TRIVIAL_MAIN) && i.contains("varying highp vec2 v_dither_nc;") && i.contains("attribute vec2 a_pos;"));
        assert!(!i.contains("u_sc[12]") && i.trim_end_matches(TRIVIAL_MAIN).trim_end().ends_with("#endif"), "cut after the block's #endif");
        assert!(i.contains(FOUR), "i is cut from the split source");
        assert_eq!(i.lines().count(), 3 + 38 + 1, "the three defines, the file's 38 lines through its #endif, then one line of main");

        let k = &by_id(&vs, "k").source;
        assert_eq!(k.lines().count(), full.lines().count(), "line numbers do not move");
        assert!(!k.contains("//") && k.contains("varying highp vec2 v_dither_nc;"));
        assert_eq!(k.lines().nth(39).map(str::trim), Some("varying highp vec2 v_dither_nc;"), "the declaration is still on line 40");

        assert_eq!(by_id(&vs, "l").source, control());

        let n = &by_id(&vs, "n").source;
        assert_eq!(n.lines().nth(3), Some("varying highp vec2 v_dither_nc;"), "right after the three defines");
        assert_eq!(n.lines().count(), full.lines().count(), "moved, not copied");
        assert_eq!(n.matches("v_dither_nc").count(), full.matches("v_dither_nc").count());

        let o = &by_id(&vs, "o").source;
        assert!(!has_token(o, "highp") && !has_token(o, "mediump") && !has_token(o, "lowp"));
        assert!(!o.contains("uniform  ") && o.contains("varying vec2 v_dither_nc;") && o.contains("uniform vec4 u_art;"));
    }

    #[test]
    fn a_variant_never_equals_a_unless_it_is_a_twin_so_no_rung_is_a_silent_no_op() {
        let t = Texts::real();
        for v in variants(&t.sources()) {
            if !matches!(v.id, "a" | "m") {
                assert_ne!(v.source, t.full, "variant {} changed nothing", v.id);
            }
        }
    }

    #[test]
    fn the_ladder_works_on_a_source_without_the_lines_it_looks_for_and_changes_nothing_it_cannot_find() {
        let src = "void main(){}\n".to_string();
        let t = Texts { full: src.clone(), original: src.clone(), control: src.clone(), fs: src.clone() };
        let vs = variants(&t.sources());
        assert_eq!(vs.len(), 15);
        assert_eq!(by_id(&vs, "c").source, "void main(){}\n");
        assert_eq!(by_id(&vs, "i").source, format!("void main(){{}}\n{TRIVIAL_MAIN}"));
        assert_eq!(link_variants(&t.sources()).len(), 8);
    }

    // --- the link variants --------------------------------------------------------------------------------

    #[test]
    fn the_link_variants_pair_the_original_and_the_split_source_with_and_without_a_version_line() {
        let t = Texts::real();
        let vs = link_variants(&t.sources());
        assert_eq!(vs.iter().map(|v| v.id).collect::<Vec<_>>(), vec!["L1", "L2", "L3", "L4", "L5", "L5v", "i0", "i2"]);
        let (l1, l2, l3, l4) = (link_by_id(&vs, "L1"), link_by_id(&vs, "L2"), link_by_id(&vs, "L3"), link_by_id(&vs, "L4"));
        // L1 is exactly what plx_gfx does for the ambient pair now.
        assert_eq!((l1.vs.as_str(), l1.fs.as_str(), l1.preamble), (t.full.as_str(), t.fs.as_str(), ES_PREAMBLE));
        assert_eq!(l1.vs_parts(), vec!["", t.full.as_str()]);
        assert_eq!(l1.fs_parts(), vec!["", t.fs.as_str()]);
        // L2 is L1 with a version line on BOTH stages; L3/L4 the same two with the original vertex source.
        assert_eq!((l2.vs.as_str(), l2.fs.as_str(), l2.preamble), (l1.vs.as_str(), l1.fs.as_str(), VERSION_100));
        assert_eq!(l2.vs_parts()[0], "#version 100\n");
        assert_eq!(l2.fs_parts()[0], "#version 100\n");
        assert_eq!((l3.vs.as_str(), l3.fs.as_str(), l3.preamble), (t.original.as_str(), t.fs.as_str(), ES_PREAMBLE));
        assert_eq!((l4.vs.as_str(), l4.preamble), (t.original.as_str(), VERSION_100));
        assert!(l3.vs.contains(ONE) && !l1.vs.contains(ONE));
        // The cross-link is the sibling's vertex source; vs_src.vert declares no v_col, fs_ambient.frag reads it.
        let (l5, l5v) = (link_by_id(&vs, "L5"), link_by_id(&vs, "L5v"));
        assert_eq!((l5.vs.as_str(), l5.fs.as_str()), (t.control.as_str(), t.fs.as_str()));
        assert!(!l5.vs.contains("v_col") && t.fs.contains("v_col"), "so L5 alone cannot tell a version failure from a missing varying");
        assert!(l5.what.contains("v_col"), "and says so");
        assert!(l5v.vs.contains("varying vec4 v_col;") && l5v.vs.contains("void main(){ v_col = vec4(1.0);"), "{}", l5v.vs);
        assert!(l5v.vs.starts_with("#define PLX_DITHER_NC\nvarying vec4 v_col;\n"));
        assert_eq!(l5v.vs.replacen("varying vec4 v_col;\n", "", 1).replacen(" v_col = vec4(1.0);", "", 1), l5.vs, "nothing else changed");
        // i0 / i2: the shortest failing cut on the original and on the split source.
        let (i0, i2) = (link_by_id(&vs, "i0"), link_by_id(&vs, "i2"));
        assert!(i0.vs.contains(ONE) && i2.vs.contains(FOUR) && !i2.vs.contains(ONE));
        assert!(i0.vs.ends_with(TRIVIAL_MAIN) && i2.vs.ends_with(TRIVIAL_MAIN));
        assert_eq!(i0.vs.replacen(ONE, FOUR, 1), i2.vs, "the two differ by the split and nothing else");
        assert_eq!(i2.vs, by_id(&variants(&t.sources()), "i").source, "i2 is variant i, linked");
        assert_eq!((i0.preamble, i2.preamble, i0.fs.as_str()), (ES_PREAMBLE, ES_PREAMBLE, t.fs.as_str()));
        // Every one of them says what it is.
        assert!(vs.iter().all(|v| v.what.len() > 20));
    }

    #[test]
    fn the_trivial_main_writes_every_varying_the_ambient_fragment_reads() {
        assert!(FS_AMBIENT.contains("varying vec4 v_col;") && DITHER.contains("varying highp vec2 v_dither_nc;"));
        assert!(TRIVIAL_MAIN.contains("v_col =") && TRIVIAL_MAIN.contains("v_dither_nc ="), "so i0 and i2 are complete pairs for FS_AMBIENT");
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

    // --- fake drivers -------------------------------------------------------------------------------

    /// A desktop GLSL 1.10 front end finds a precision qualifier on any active line unless the source begins
    /// with `#version 100`: the position of the first such line in the text after `#ifdef` resolution.
    fn highp_line(joined: &str) -> Option<usize> {
        if joined.starts_with("#version 100") {
            return None;
        }
        preprocess(joined)
            .lines()
            .position(|l| !is_comment_line(l) && l.split(|c: char| !c.is_alphanumeric()).any(|t| matches!(t, "highp" | "mediump" | "lowp")))
    }

    fn ok_status() -> Status {
        Status { ok: true, log: String::new() }
    }

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
            let bad = highp_line(&joined);
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
        fn link_trial(&mut self, vs: &[&str], fs: &[&str]) -> LinkTrial {
            let (v, f) = (self.trial(false, vs), self.trial(true, fs));
            let link = (v.compiled && f.compiled).then(ok_status);
            LinkTrial { vs: Status { ok: v.compiled, log: v.log }, fs: Status { ok: f.compiled, log: f.log }, link }
        }
    }

    /// The working hypothesis about YaGL as a model: a source with a statement of several declarators is
    /// passed through RAW (desktop GLSL 1.10, so a precision qualifier is an error, unless the source begins
    /// with `#version 100`, which makes it ES 1.00), every other source is translated and compiles whatever
    /// it says. A link then wants every stage on one language version, and every varying the fragment reads
    /// written by the vertex stage.
    struct YaglGuess;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Front {
        Translated,
        RawEs,
        RawDesktop,
    }

    impl YaglGuess {
        fn stage(parts: &[&str]) -> (Status, Front) {
            let joined = parts.concat();
            if multi_declarator_lines(&joined).is_empty() {
                return (ok_status(), Front::Translated);
            }
            if joined.starts_with("#version 100") {
                return (ok_status(), Front::RawEs);
            }
            match highp_line(&joined) {
                Some(n) => (Status { ok: false, log: format!("0:{}(9): error: syntax error, unexpected NEW_IDENTIFIER", n + 1) }, Front::RawDesktop),
                None => (ok_status(), Front::RawDesktop),
            }
        }
    }

    impl ShaderGl for YaglGuess {
        fn compile(&mut self, _fragment: bool, _src: &str) -> Result<u32, String> {
            unreachable!("the bisect only trials")
        }
        fn link(&mut self, _vs: u32, _fs: u32) -> Result<u32, String> {
            unreachable!("the bisect never links")
        }
        fn delete(&mut self, _vs: u32, _fs: u32, _program: Option<u32>) {}
        fn trial(&mut self, _fragment: bool, parts: &[&str]) -> Trial {
            let joined = parts.concat();
            let (s, _) = Self::stage(parts);
            // YaGL's glGetShaderSource returned 8 bytes for every source: a stub.
            Trial {
                compiled: s.ok,
                log: s.log,
                passed_len: joined.len(),
                source_length: Some(joined.len() as i32 + 1),
                readback: Some(Readback { returned: 8, same: false, first_diff: Some(8) }),
            }
        }
        fn link_trial(&mut self, vs: &[&str], fs: &[&str]) -> LinkTrial {
            let ((v, vk), (f, fk)) = (Self::stage(vs), Self::stage(fs));
            let link = (v.ok && f.ok).then(|| {
                if (vk == Front::RawEs) != (fk == Front::RawEs) {
                    Status { ok: false, log: "error: all shaders must use same shading language version".to_string() }
                } else if fs.concat().contains("v_col") && !vs.concat().contains("v_col") {
                    Status { ok: false, log: "error: fragment shader input `v_col' is not written by the vertex shader".to_string() }
                } else {
                    ok_status()
                }
            });
            LinkTrial { vs: v, fs: f, link }
        }
    }

    #[test]
    fn on_a_desktop_110_driver_only_the_rungs_that_remove_the_precision_qualifier_pass() {
        let _g = crate::testutil::log_serial();
        let mut gl = Desktop110 { trials: Vec::new(), keep_different_copy: false };
        let t = Texts::real();
        let report = run(&mut gl, &t.sources());
        assert_eq!(gl.trials.len(), 15);
        assert_eq!(gl.trials.iter().filter(|parts| **parts == 1).count(), 1, "only m is a one-string call");
        assert_eq!(
            report.summary(),
            "shader bisect (+ compiles, - fails): a0- a- b- c+ d- e- f- g+ h- i- k- l- m- n- o+; j bytes clean"
        );
        let a0 = &report.outcomes[0];
        assert_eq!(a0.id, "a0");
        assert!(!a0.trial.compiled && a0.trial.passed_len == original().len(), "{a0:?}");
        assert!(a0.trial.log.starts_with("0:") && a0.trial.log.contains("NEW_IDENTIFIER"), "{}", a0.trial.log);
    }

    /// What the hypothesis predicts for the ladder and for the link variants, on the model of it above: the
    /// original fails to compile, everything built from the split source compiles, and the one pair that
    /// differs only by a version line links with it (L2) or not (L4) depending on the split.
    #[test]
    fn on_the_hypothesis_model_the_split_fixes_the_compile_and_the_version_line_breaks_only_the_unsplit_link() {
        let _g = crate::testutil::log_serial();
        let t = Texts::real();
        let bisect = run(&mut YaglGuess, &t.sources());
        assert_eq!(
            bisect.summary(),
            "shader bisect (+ compiles, - fails): a0- a+ b+ c+ d+ e+ f+ g+ h+ i+ k+ l+ m+ n+ o+; j bytes clean",
            "a0 before, a after: the same call on the original and on the split source"
        );
        assert!(bisect.outcomes.iter().all(|o| !o.trial.readback_differs()), "a stub read-back is never reported as a different copy");
        let links = run_links(&mut YaglGuess, &t.sources());
        assert_eq!(links.summary(), "shader link: L1+ L2+ L3:vs- L4:link- L5:link- L5v+ i0:vs- i2+");
        let l4 = &links.outcomes.iter().find(|o| o.id == "L4").unwrap().trial;
        assert_eq!(l4.link.as_ref().unwrap().log, "error: all shaders must use same shading language version");
        let l5 = &links.outcomes.iter().find(|o| o.id == "L5").unwrap().trial;
        assert!(l5.link.as_ref().unwrap().log.contains("v_col"), "L5's failure is the missing varying, which L5v shows by linking");
        assert_eq!((l4.verdict(), links.outcomes[0].trial.verdict()), (LinkVerdict::LinkFailed, LinkVerdict::Linked));
    }

    #[test]
    fn on_a_desktop_110_driver_the_link_variants_with_a_version_line_are_the_ones_that_link() {
        let _g = crate::testutil::log_serial();
        let mut gl = Desktop110 { trials: Vec::new(), keep_different_copy: false };
        let links = run_links(&mut gl, &Texts::real().sources());
        assert_eq!(
            links.summary(),
            "shader link: L1:both- L2+ L3:both- L4+ L5:both- L5v:both- i0:both- i2:both-",
            "the fragment source has a highp varying of its own, so both stages fail without the line"
        );
        assert!(links.summary().chars().count() <= 100, "{}", links.summary());
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
            fn link_trial(&mut self, _: &[&str], _: &[&str]) -> LinkTrial {
                LinkTrial { vs: ok_status(), fs: ok_status(), link: Some(ok_status()) }
            }
        }
        let _g = crate::testutil::log_serial();
        let t = Texts::real();
        let report = run(&mut Accepting, &t.sources());
        assert_eq!(report.summary(), "shader bisect: all 15 variants compile; j bytes clean");
        assert!(report.summary().chars().count() < 70);
        let links = run_links(&mut Accepting, &t.sources());
        assert_eq!(links.summary(), "shader link: all 8 variants compile and link");
    }

    #[test]
    fn a_driver_that_keeps_a_different_copy_of_the_source_is_named_in_the_summary() {
        let _g = crate::testutil::log_serial();
        let mut gl = Desktop110 { trials: Vec::new(), keep_different_copy: true };
        let s = run(&mut gl, &Texts::real().sources()).summary();
        assert!(s.ends_with("; the driver's copy of the source differs for a0 a b c d e f g h i k l m n o"), "{s}");
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
            outcomes: vec![Outcome { id: "a", trial: stub(false) }, Outcome { id: "g", trial: stub(true) }],
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
        let s = run(&mut gl, &Texts::real().sources()).summary();
        assert!(s.chars().count() <= 110, "{} chars: {s}", s.chars().count());
    }

    // --- the link variants' log and summary -----------------------------------------------------------

    fn link_variant() -> LinkVariant {
        LinkVariant { id: "L9", what: "test", vs: "void main(){}\n".into(), fs: "void main(){}\n".into(), preamble: VERSION_100 }
    }

    fn st(ok: bool, log: &str) -> Status {
        Status { ok, log: log.to_string() }
    }

    #[test]
    fn a_link_that_failed_is_logged_with_both_stages_status_and_every_line_of_the_drivers_words() {
        let t = LinkTrial {
            vs: st(true, ""),
            fs: st(true, "WARNING: 0:3: something harmless"),
            link: Some(st(false, "error: all shaders must use same shading language version\n\nsecond line\n")),
        };
        let lines = link_log_lines(&link_variant(), &t);
        assert_eq!(
            lines,
            vec![
                "H7 link L9: vertex COMPILED (27 bytes), fragment COMPILED (27 bytes), link FAILED => both stages compile, the link fails".to_string(),
                "H7 link L9 fragment info log: WARNING: 0:3: something harmless".to_string(),
                "H7 link L9 link info log: error: all shaders must use same shading language version".to_string(),
                "H7 link L9 link info log: second line".to_string(),
            ],
            "the success's warning is kept, the blank line is not a log line, and a vertex stage with nothing to say says nothing"
        );
        assert_eq!(t.verdict(), LinkVerdict::LinkFailed);
    }

    #[test]
    fn a_stage_that_did_not_compile_blames_its_line_and_the_link_is_not_attempted() {
        let mut v = link_variant();
        v.preamble = ES_PREAMBLE;
        v.vs = "a\nb\nvarying highp vec2 x;\n".into();
        let t = LinkTrial { vs: st(false, "0:3(9): error: syntax error, unexpected NEW_IDENTIFIER"), fs: st(true, ""), link: None };
        let lines = link_log_lines(&v, &t);
        assert_eq!(lines[0], "H7 link L9: vertex FAILED (26 bytes), fragment COMPILED (14 bytes), link not attempted (a stage did not compile) => the vertex stage does not compile");
        assert_eq!(lines[1], "H7 link L9 vertex info log: 0:3(9): error: syntax error, unexpected NEW_IDENTIFIER");
        assert_eq!(lines[2], "H7 link L9 vertex: the log blames line 3: varying highp vec2 x;");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(t.verdict(), LinkVerdict::VertexFailed);

        // A failure with no info log is still named, in the stage that failed.
        let silent = LinkTrial { vs: st(true, ""), fs: st(false, ""), link: None };
        let lines = link_log_lines(&v, &silent);
        assert_eq!(lines[1], "H7 link L9 fragment info log: (the driver gave no info log)");
        assert_eq!(silent.verdict(), LinkVerdict::FragmentFailed);
        let both = LinkTrial { vs: st(false, ""), fs: st(false, ""), link: None };
        assert_eq!(both.verdict(), LinkVerdict::BothFailed);
        assert_eq!(link_log_lines(&v, &both).len(), 3, "the result and one 'no info log' line per stage");
    }

    #[test]
    fn the_link_summary_marks_each_variant_by_how_it_ended() {
        let trial = |vs: bool, fs: bool, link: Option<bool>| LinkTrial { vs: st(vs, ""), fs: st(fs, ""), link: link.map(|ok| st(ok, "")) };
        let report = LinkReport {
            outcomes: vec![
                LinkOutcome { id: "L1", trial: trial(true, true, Some(true)) },
                LinkOutcome { id: "L3", trial: trial(false, true, None) },
                LinkOutcome { id: "L4", trial: trial(true, true, Some(false)) },
                LinkOutcome { id: "L5", trial: trial(true, false, None) },
                LinkOutcome { id: "i0", trial: trial(false, false, None) },
            ],
        };
        assert_eq!(report.summary(), "shader link: L1+ L3:vs- L4:link- L5:fs- i0:both-");
        // A pair that compiled but whose link was never recorded is not a pass.
        assert_eq!(trial(true, true, None).verdict(), LinkVerdict::LinkFailed);
        assert_eq!(LinkReport::default().summary(), "shader link: all 0 variants compile and link");
    }

    #[test]
    fn the_v_col_twin_declares_the_varying_after_the_defines_and_writes_it_first_in_main() {
        assert_eq!(with_v_col("#define A\n#define B\nx\nvoid main(){\n  y;\n}\n"), "#define A\n#define B\nvarying vec4 v_col;\nx\nvoid main(){ v_col = vec4(1.0);\n  y;\n}\n");
        assert_eq!(with_v_col("x\n"), "varying vec4 v_col;\nx\n", "no main to write it in: only the declaration");
    }
}
