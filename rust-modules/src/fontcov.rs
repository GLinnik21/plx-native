//! Which codepoints a font file can actually draw — read straight out of its `cmap`.
//!
//! Two callers, for the same reason. [`text.rs`](crate::text) asks it per character to decide
//! which link of the fallback chain renders a run; the test module at the bottom of THIS file
//! asks it of the shipped `pkg/appfont*.ttf` and asserts a **declared codepoint set**, inside
//! `make check`, in milliseconds, with no television and no GL context.
//!
//! ## Why the gate exists
//!
//! `pkg/appfont.ttf` covers 2853 codepoints — Latin, Cyrillic, Greek — and **zero** Hangul, Kana
//! or Han. A Plex library with Korean, Japanese or Chinese titles rendered 100 % tofu, and every
//! test tier this project has was structurally blind to it: the host suite draws nothing, the
//! simulator renders through a different rasterizer, and the device FPS scenes grade `loop=`.
//! Coverage is not a rendering question at all — it is a property of a file on disk — so it
//! belongs in the cheapest tier, and this is it.
//!
//! The same defect class already shipped once, visibly: subtitle convention wraps a sung line in
//! U+2669–U+266C, Inter carries none of them, and a panel capture of the Family Guy theme read
//! `▯NO GLYPH▯ It seems today`. `tools/cut-inter.py` synthesizes those four; nothing asserted it
//! until now.
//!
//! ## Why the cmap and not `TTF_GlyphIsProvided`
//!
//! SDL2_ttf does expose a coverage query, and it is the wrong tool twice over. `TTF_GlyphIsProvided`
//! takes a **`Uint16`**, so it cannot be asked about anything above U+FFFF; and the 32-bit
//! `TTF_GlyphIsProvided32` is declared in the NDK's header, exported by the NDK's
//! `libSDL2_ttf-2.0.so.0.18.0`, and present on **none of the 14 firmware inventories** — the NDK is
//! 2.0.18 and every television ships 2.0.10 or 2.0.14
//! (`tools/fwcompat.py --lib libSDL2_ttf-2.0.so.0 --grep '32$'` returns zero matches on all 14).
//! It would therefore have compiled, linked and passed `make check`, then failed at the first
//! coverage query on every set. `text.rs`'s `extern` block carries the general form of that trap.
//!
//! Either one also needs an open `TTF_Font` — a size, a GL-adjacent init and a device — which is
//! precisely what a host gate must not need. Parsing the cmap answers the same question from the
//! file alone.
//!
//! ## What it reads
//!
//! Only the sfnt table directory and the `cmap` table, by seek — never the whole file. That
//! matters: `pkg/appfont-cjk.ttf` is 21 MB and its cmap is 233 KB. Unicode subtable formats 4, 6,
//! 12 and 13 are decoded and **unioned**; a mapping to glyph 0 is not coverage and is excluded,
//! which is the difference between "the cmap has an entry" and "the font can draw it".

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// A font's codepoint coverage: a flat bitset for the BMP, sorted ranges above it.
///
/// The BMP half is the hot one — `text.rs` asks per character on every cache-missing string — and
/// 8 KB of bitset makes that a shift and a mask instead of a binary search. Astral codepoints
/// (emoji, CJK ext-B) are rare enough in a Plex library to be worth a `partition_point`.
pub(crate) struct Coverage {
    bmp: Box<[u64; 1024]>,
    sup: Vec<(u32, u32)>,
    count: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CovErr {
    Io(String),
    /// The bytes are not an sfnt we recognise, or the table we need is missing/malformed. The
    /// string names WHICH, because "the font is bad" is not an actionable log line.
    Malformed(&'static str),
}

impl std::fmt::Display for CovErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CovErr::Io(e) => write!(f, "{e}"),
            CovErr::Malformed(w) => write!(f, "malformed font: {w}"),
        }
    }
}

impl Coverage {
    fn new() -> Coverage {
        Coverage {
            bmp: Box::new([0u64; 1024]),
            sup: Vec::new(),
            count: 0,
        }
    }

    pub(crate) fn contains(&self, cp: u32) -> bool {
        if cp <= 0xFFFF {
            let i = cp as usize;
            return self.bmp[i >> 6] & (1u64 << (i & 63)) != 0;
        }
        // ranges are sorted and disjoint: the last one starting at or before `cp` is the only
        // candidate. `partition_point` is the standard spelling of "how many start <= cp".
        let k = self.sup.partition_point(|&(s, _)| s <= cp);
        k > 0 && self.sup[k - 1].1 >= cp
    }

    /// How many codepoints this font maps to a real glyph. The gate quotes it, and `text.rs` logs
    /// it once per face so an event log says what the chain actually loaded.
    pub(crate) fn len(&self) -> u32 {
        self.count
    }

    fn add(&mut self, cp: u32) {
        if cp > 0x10FFFF {
            return;
        }
        if cp <= 0xFFFF {
            let i = cp as usize;
            let (w, b) = (i >> 6, 1u64 << (i & 63));
            if self.bmp[w] & b == 0 {
                self.bmp[w] |= b;
                self.count += 1;
            }
        } else {
            self.sup.push((cp, cp));
        }
    }

    fn add_range(&mut self, lo: u32, hi: u32) {
        if lo > hi {
            return;
        }
        // The BMP part goes bit by bit (bounded by 65536 iterations); anything above is stored as
        // a range, so a font mapping all of plane 2 costs one entry rather than 43 000.
        let bmp_hi = hi.min(0xFFFF);
        for cp in lo..=bmp_hi {
            self.add(cp);
        }
        if hi > 0xFFFF {
            let lo = lo.max(0x10000);
            let hi = hi.min(0x10FFFF);
            if lo <= hi {
                self.sup.push((lo, hi));
            }
        }
    }

    /// Sort and coalesce the supplementary ranges, and only then count them. Subtables are unioned,
    /// so the same astral codepoint can arrive from two of them; counting before the merge would
    /// double it, and `contains` needs the sorted invariant `partition_point` assumes.
    fn seal(&mut self) {
        self.sup.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(self.sup.len());
        for &(lo, hi) in &self.sup {
            match merged.last_mut() {
                // `lo <= last.1 + 1` also coalesces ADJACENT ranges, which is what keeps a
                // per-codepoint format-4 walk from leaving thousands of one-wide entries.
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        for &(lo, hi) in &merged {
            self.count += hi - lo + 1;
        }
        self.sup = merged;
    }
}

// --- sfnt / cmap decoding -----------------------------------------------------------------------

fn be16(b: &[u8], off: usize) -> Option<u32> {
    Some(u16::from_be_bytes(b.get(off..off + 2)?.try_into().ok()?) as u32)
}
fn be32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn read_at<R: Read + Seek>(r: &mut R, off: u64, len: usize) -> Result<Vec<u8>, CovErr> {
    // A malformed directory can claim a table of any length; refuse absurd ones rather than
    // trying to allocate them. No real cmap is anywhere near 32 MB (Noto Sans CJK's is 233 KB).
    if len > 32 << 20 {
        return Err(CovErr::Malformed("table length is implausible"));
    }
    r.seek(SeekFrom::Start(off))
        .map_err(|e| CovErr::Io(e.to_string()))?;
    let mut v = vec![0u8; len];
    r.read_exact(&mut v)
        .map_err(|e| CovErr::Io(e.to_string()))?;
    Ok(v)
}

/// Locate `cmap` in the sfnt directory and return its bytes. Handles a TrueType **collection**
/// (`ttcf`) by taking font 0 — the shipped faces are not collections, but the upstream Noto CJK
/// release ships `.ttc` files beside the `.ttf` we pin, and reading one as a plain sfnt would
/// otherwise fail with a misleading "not an sfnt".
fn cmap_bytes<R: Read + Seek>(r: &mut R) -> Result<Vec<u8>, CovErr> {
    let mut base = 0u64;
    let head = read_at(r, 0, 12)?;
    if &head[0..4] == b"ttcf" {
        let n = be32(&head, 8).ok_or(CovErr::Malformed("short ttc header"))?;
        if n == 0 {
            return Err(CovErr::Malformed("ttc with no fonts"));
        }
        let dir = read_at(r, 12, 4)?;
        base = be32(&dir, 0).ok_or(CovErr::Malformed("short ttc directory"))? as u64;
    }
    let head = read_at(r, base, 12)?;
    let tag = be32(&head, 0).ok_or(CovErr::Malformed("short sfnt header"))?;
    // 0x00010000 TrueType outlines, 'OTTO' CFF outlines, 'true' the old Apple spelling. The cmap
    // is in the same place in all three, which is the whole point of asking the directory.
    if tag != 0x0001_0000 && tag != 0x4F54_544F && tag != 0x7472_7565 {
        return Err(CovErr::Malformed("not an sfnt (bad version tag)"));
    }
    let num = be16(&head, 4).ok_or(CovErr::Malformed("short sfnt header"))? as usize;
    let dir = read_at(
        r,
        base + 12,
        num.checked_mul(16)
            .ok_or(CovErr::Malformed("absurd table count"))?,
    )?;
    for i in 0..num {
        let rec = &dir[i * 16..];
        if &rec[0..4] == b"cmap" {
            let off = be32(rec, 8).ok_or(CovErr::Malformed("short cmap record"))?;
            let len = be32(rec, 12).ok_or(CovErr::Malformed("short cmap record"))?;
            return read_at(r, base + off as u64, len as usize);
        }
    }
    Err(CovErr::Malformed("no cmap table"))
}

/// Is this (platform, encoding) pair a **Unicode** cmap? Platform 0 is Unicode by definition;
/// platform 3 (Windows) is Unicode only at encoding 1 (BMP) and 10 (full repertoire). Encoding
/// 3/0 is "symbol" — a private-use remapping that would report coverage the font cannot really
/// draw at the codepoints claimed — and platform 1 is Mac Roman. Both are skipped.
fn is_unicode(platform: u32, encoding: u32) -> bool {
    platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10))
}

fn parse_format4(t: &[u8], cov: &mut Coverage) -> Option<()> {
    let seg2 = be16(t, 6)? as usize;
    let segs = seg2 / 2;
    let (end_at, start_at, delta_at, ro_at) = (14, 16 + seg2, 16 + 2 * seg2, 16 + 3 * seg2);
    for i in 0..segs {
        let end = be16(t, end_at + 2 * i)?;
        let start = be16(t, start_at + 2 * i)?;
        let delta = be16(t, delta_at + 2 * i)?;
        let ro = be16(t, ro_at + 2 * i)?;
        if start > end {
            continue;
        }
        for cp in start..=end {
            if cp == 0xFFFF {
                continue; // the mandatory terminating segment, never real coverage
            }
            let g = if ro == 0 {
                (cp.wrapping_add(delta)) & 0xFFFF
            } else {
                // The spec's pointer arithmetic: idRangeOffset is a byte offset FROM ITS OWN slot.
                let at = ro_at + 2 * i + ro as usize + 2 * (cp - start) as usize;
                match be16(t, at) {
                    Some(0) | None => 0,
                    Some(g) => (g.wrapping_add(delta)) & 0xFFFF,
                }
            };
            if g != 0 {
                cov.add(cp);
            }
        }
    }
    Some(())
}

fn parse_format6(t: &[u8], cov: &mut Coverage) -> Option<()> {
    let first = be16(t, 6)?;
    let n = be16(t, 8)? as usize;
    for i in 0..n {
        if be16(t, 10 + 2 * i)? != 0 {
            cov.add(first + i as u32);
        }
    }
    Some(())
}

/// Formats 12 (segmented) and 13 (many-to-one). Identical headers; they differ only in whether
/// `startGlyphID` advances across the group, which does not change WHICH codepoints are covered.
fn parse_format12_13(t: &[u8], many_to_one: bool, cov: &mut Coverage) -> Option<()> {
    let n = be32(t, 12)? as usize;
    // The bounds check is done in u64, and that is load-bearing rather than tidy: **`usize` is
    // 32 BITS on `arm-unknown-linux-gnueabi`**, the only target that ships. A `numGroups` of
    // 357_913_941 makes `n * 12` exactly 0xFFFF_FFFC, so `16 + n * 12` WRAPS to 12 — which is
    // ≤ `t.len()` — and the indexing below then panics out of the SDL main thread on the second
    // group. Release builds have overflow checks off, so it wraps silently rather than aborting.
    // No host test can catch this: `cargo test` runs on a 64-bit Mac where the same expression is
    // simply true. In u64 the product cannot overflow for any u32 count, on either width.
    if 16u64 + n as u64 * 12 > t.len() as u64 {
        return None;
    }
    for i in 0..n {
        let g = &t[16 + i * 12..];
        let (lo, hi, gid) = (be32(g, 0)?, be32(g, 4)?, be32(g, 8)?);
        if lo > hi || hi > 0x10FFFF {
            continue;
        }
        if gid == 0 {
            // Format 13 maps the whole group to one glyph, so gid 0 means the group is empty;
            // format 12 walks the glyph id, so only the first codepoint lands on notdef.
            if !many_to_one && lo < hi {
                cov.add_range(lo + 1, hi);
            }
            continue;
        }
        cov.add_range(lo, hi);
    }
    Some(())
}

/// Coverage of the font in `r`, unioned across every Unicode cmap subtable it carries. The test
/// seam: it takes any `Read + Seek`, which is what lets the decoder be driven from a `Cursor` over
/// a synthetic sfnt — shapes no shipped font has, like a cmap entry pointing at glyph 0.
/// Production reads a path and goes through [`of_file`], which closes the file sooner.
#[cfg(test)]
pub(crate) fn of_reader<R: Read + Seek>(r: &mut R) -> Result<Coverage, CovErr> {
    decode(&cmap_bytes(r)?)
}

/// The decode half, split from the I/O half so [`of_file`] can CLOSE the file before doing the
/// slow part. The walk over a pan-CJK format-4 subtable is milliseconds and the read is
/// microseconds; holding a descriptor across the difference is free to avoid and not free to keep
/// (`stream.rs`'s fd-leak assertion grades the process-wide count, and the host suite reads these
/// fonts from several tests at once).
fn decode(t: &[u8]) -> Result<Coverage, CovErr> {
    let n = be16(t, 2).ok_or(CovErr::Malformed("short cmap header"))? as usize;
    let mut cov = Coverage::new();
    let mut seen = 0usize;
    for i in 0..n {
        let rec = 4 + 8 * i;
        let (Some(plat), Some(enc), Some(off)) =
            (be16(&t, rec), be16(&t, rec + 2), be32(&t, rec + 4))
        else {
            break;
        };
        if !is_unicode(plat, enc) {
            continue;
        }
        let Some(sub) = t.get(off as usize..) else {
            continue;
        };
        let Some(format) = be16(sub, 0) else { continue };
        // Formats 0 (byte), 2 (high-byte CJK legacy) and 14 (variation selectors) are `None`, and
        // that is the point: `seen` counts subtables this decoder ACTUALLY READ, never ones it
        // skipped. Counting a skip made a font whose only Unicode subtables are unsupported decode
        // as `Ok(<empty coverage>)` instead of `Err`, and the two are not interchangeable
        // downstream — `text.rs::link_covers` falls back to "the base face covers everything" only
        // on `Err`, and takes an empty `Ok` as authoritative. The result would have been that every
        // accented Latin, Cyrillic and Greek character failed Inter and resolved to the CJK
        // fallback, which carries a full Latin set: the whole interface silently in the wrong
        // typeface, with no error anywhere.
        let decoded = match format {
            4 => parse_format4(sub, &mut cov),
            6 => parse_format6(sub, &mut cov),
            12 => parse_format12_13(sub, false, &mut cov),
            13 => parse_format12_13(sub, true, &mut cov),
            _ => None,
        };
        if decoded.is_some() {
            seen += 1;
        }
    }
    if seen == 0 {
        return Err(CovErr::Malformed("no usable Unicode cmap subtable"));
    }
    cov.seal();
    Ok(cov)
}

/// The errors do NOT name `path` — every caller has it and prefixes its own message with it, and
/// a doubled path reads as a bug in the error plumbing rather than as the missing file it is.
pub(crate) fn of_file(path: &Path) -> Result<Coverage, CovErr> {
    let cmap = {
        let mut f = File::open(path).map_err(|e| CovErr::Io(e.to_string()))?;
        cmap_bytes(&mut f)? // `f` is dropped HERE, before `decode` — see `decode`'s doc
    };
    decode(&cmap)
}

/// The shipped faces' coverage, parsed ONCE for the whole test binary.
///
/// Two reasons, both measured on the way in. The fallback's cmap is 233 KB and its format-4 walk
/// is the most expensive thing this module does; and every `of_file` holds a descriptor, so
/// fourteen tests reading three fonts in parallel moved the PROCESS-wide descriptor count enough
/// to trip `stream.rs`'s fd-leak assertion — whose slack is +8 over whatever the rest of the suite
/// happens to be holding, and which then fails naming a leak that does not exist. Parsing once
/// turns fourteen concurrent opens into three sequential ones.
#[cfg(test)]
pub(crate) fn shipped(name: &str) -> &'static Result<Coverage, String> {
    use std::sync::OnceLock;
    static REG: OnceLock<Result<Coverage, String>> = OnceLock::new();
    static BOLD: OnceLock<Result<Coverage, String>> = OnceLock::new();
    static CJK: OnceLock<Result<Coverage, String>> = OnceLock::new();
    let slot = match name {
        "appfont.ttf" => &REG,
        "appfont-bold.ttf" => &BOLD,
        "appfont-cjk.ttf" => &CJK,
        other => panic!("no shipped-font slot for {other:?} — add one beside the other three"),
    };
    slot.get_or_init(|| {
        // The fonts are repository payload, not crate data — `pkg/` sits beside `rust-modules/`.
        let p = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../pkg")).join(name);
        of_file(&p).map_err(|e| format!("{}: {e}", p.display()))
    })
}

// ------------------------------------------------------------------------------------------------
// THE GATE. Everything below runs in `make check`.
// ------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn cov(name: &str) -> &'static Coverage {
        shipped(name).as_ref().unwrap_or_else(|e| panic!("{e}"))
    }

    /// Every codepoint in `lo..=hi` that the coverage is missing, capped so a totally absent
    /// block reports as a block rather than eleven thousand lines.
    fn missing(c: &Coverage, lo: u32, hi: u32) -> Vec<u32> {
        (lo..=hi).filter(|&cp| !c.contains(cp)).take(12).collect()
    }

    fn count(c: &Coverage, lo: u32, hi: u32) -> u32 {
        (lo..=hi).filter(|&cp| c.contains(cp)).count() as u32
    }

    // --- the declared set ------------------------------------------------------------------------
    //
    // LINK 1 — `appfont.ttf` / `appfont-bold.ttf` (Inter, cut by tools/cut-inter.py). Every
    // codepoint the product chrome itself can emit, plus the scripts the Plex libraries this
    // client is built for actually contain. Whole blocks, not samples: a partial block is how
    // "Cyrillic works" and "Ё is a box" coexist.
    const LINK1_BLOCKS: &[(&str, u32, u32)] = &[
        ("ASCII printable", 0x0020, 0x007E),
        ("Latin-1 letters", 0x00C0, 0x00FF),
        // Split around U+0149 (ŉ), which Inter does not carry and should not: Unicode DEPRECATED
        // it, no locale produces it, and it decomposes to U+02BC U+006E. Excluding it here is the
        // difference between a declared set and a wish list — every other position is asserted.
        ("Latin Extended-A (before U+0149)", 0x0100, 0x0148),
        ("Latin Extended-A (after U+0149)", 0x014A, 0x017E),
        // U+03A2 is a permanent hole in Unicode itself (reserved between Ρ and Σ), so the block
        // is split around it for the same reason as U+0149 above.
        ("Greek and Coptic (before U+03A2)", 0x0391, 0x03A1),
        ("Greek and Coptic (after U+03A2)", 0x03A3, 0x03CE),
        ("Cyrillic", 0x0400, 0x045F),
        // The regression that named this whole unit. Subtitle convention wraps a sung line in
        // these four; Inter has none of them, and `tools/cut-inter.py` synthesizes them. A panel
        // capture of the Family Guy theme read "▯NO GLYPH▯ It seems today" before it did.
        ("musical symbols (subtitle convention)", 0x2669, 0x266C),
    ];

    /// Individual codepoints link 1 must carry: the typographic punctuation the UI composes by
    /// hand (`elide` appends U+2026; headings join with U+00B7; PMS metadata is full of curly
    /// quotes and en/em dashes) and the marks drawn beside ratings.
    const LINK1_CHARS: &[(&str, char)] = &[
        ("ellipsis (elide appends it)", '\u{2026}'),
        ("middle dot (heading separator)", '\u{00B7}'),
        ("bullet", '\u{2022}'),
        ("en dash", '\u{2013}'),
        ("em dash", '\u{2014}'),
        ("left single quote", '\u{2018}'),
        ("right single quote / apostrophe", '\u{2019}'),
        ("left double quote", '\u{201C}'),
        ("right double quote", '\u{201D}'),
        ("multiplication sign", '\u{00D7}'),
        ("degree sign", '\u{00B0}'),
        ("no-break space", '\u{00A0}'),
        ("copyright", '\u{00A9}'),
        ("registered", '\u{00AE}'),
    ];

    // LINK 2 — `appfont-cjk.ttf` (Noto Sans CJK KR, cut by tools/cut-noto-cjk.py). Checklist #6
    // and #48 are unreachable without these: a Korean, Japanese or Chinese title is 100 % tofu
    // in link 1.
    const LINK2_BLOCKS: &[(&str, u32, u32)] = &[
        ("Hangul syllables", 0xAC00, 0xD7A3),
        ("Hangul Jamo", 0x1100, 0x11FF),
        ("Hangul Compatibility Jamo (letters)", 0x3131, 0x318E),
        ("Hiragana", 0x3041, 0x3096),
        ("Katakana", 0x30A1, 0x30FA),
        ("CJK symbols and punctuation", 0x3000, 0x303F),
    ];

    /// Han is not asserted as a whole block: Unicode leaves a handful of positions unassigned in
    /// every CJK range, and a floor is the honest shape of "this font draws Chinese". The numbers
    /// are the measured coverage of the pinned cut, so a subsetted or truncated replacement fails.
    const LINK2_FLOORS: &[(&str, u32, u32, u32)] = &[
        ("CJK unified ideographs", 0x4E00, 0x9FFF, 20_900),
        ("CJK unified ideographs ext-A", 0x3400, 0x4DBF, 6_500),
        ("halfwidth and fullwidth forms", 0xFF00, 0xFFEF, 200),
    ];

    /// Real titles, in the scripts the chain claims. A block assertion cannot catch a font whose
    /// cmap is fine and whose *combination* is not, and these read as evidence in a PR body.
    const SAMPLES: &[(&str, &str)] = &[
        ("Korean", "오징어 게임 · 기생충 · 부산행"),
        ("Japanese", "君の名は。 千と千尋の神隠し ドラえもん"),
        ("Chinese (simplified)", "流浪地球 · 让子弹飞"),
        ("Chinese (traditional)", "臥虎藏龍 · 東京物語"),
        ("Russian", "Ирония судьбы, или С лёгким паром!"),
        ("Greek", "Ο Θίασος"),
        ("Latin with diacritics", "Amélie · Das Boot · Coração"),
        ("subtitle sung line", "\u{266A} It seems today \u{266B}"),
    ];

    /// Report EVERY gap in one run, not the first. A fail-fast gate over eight blocks and
    /// fourteen characters makes a font swap an eight-iteration guessing game, which is how a
    /// re-cut ends up half-checked.
    fn report(font: &str, gaps: Vec<String>) {
        assert!(
            gaps.is_empty(),
            "pkg/{font} does not cover the declared set:\n  {}",
            gaps.join("\n  ")
        );
    }

    #[test]
    fn shipped_regular_font_covers_the_declared_latin_set() {
        let c = cov("appfont.ttf");
        let mut gaps = Vec::new();
        for &(name, lo, hi) in LINK1_BLOCKS {
            let m = missing(&c, lo, hi);
            if !m.is_empty() {
                gaps.push(format!(
                    "{name} (U+{lo:04X}..U+{hi:04X}) is missing {m:04X?}"
                ));
            }
        }
        for &(name, ch) in LINK1_CHARS {
            if !c.contains(ch as u32) {
                gaps.push(format!("{name} (U+{:04X})", ch as u32));
            }
        }
        report("appfont.ttf", gaps);
    }

    /// The bold face is a separate cut and can rot on its own. A bold-only hole is invisible in
    /// every other tier — the same string renders fine as body text and tofus as a heading.
    #[test]
    fn bold_face_covers_exactly_what_the_regular_one_does() {
        let (r, b) = (cov("appfont.ttf"), cov("appfont-bold.ttf"));
        assert_eq!(
            r.len(),
            b.len(),
            "appfont.ttf covers {} codepoints, appfont-bold.ttf {}",
            r.len(),
            b.len()
        );
        for &(name, lo, hi) in LINK1_BLOCKS {
            for cp in lo..=hi {
                assert_eq!(
                    r.contains(cp),
                    b.contains(cp),
                    "regular/bold disagree on U+{cp:04X} ({name})"
                );
            }
        }
        for &(name, ch) in LINK1_CHARS {
            assert!(
                b.contains(ch as u32),
                "pkg/appfont-bold.ttf is missing {name} (U+{:04X})",
                ch as u32
            );
        }
    }

    #[test]
    fn bundled_fallback_covers_the_declared_cjk_set() {
        let c = cov("appfont-cjk.ttf");
        let mut gaps = Vec::new();
        for &(name, lo, hi) in LINK2_BLOCKS {
            let m = missing(&c, lo, hi);
            if !m.is_empty() {
                gaps.push(format!(
                    "{name} (U+{lo:04X}..U+{hi:04X}) is missing {m:04X?}"
                ));
            }
        }
        for &(name, lo, hi, floor) in LINK2_FLOORS {
            let n = count(&c, lo, hi);
            if n < floor {
                gaps.push(format!("{name}: {n} codepoints, expected at least {floor}"));
            }
        }
        report("appfont-cjk.ttf", gaps);
    }

    /// Pins the cut itself. `tools/cut-noto-cjk.py` pins its INPUT by sha256; this pins its
    /// OUTPUT, so a re-cut that silently subsets (the `Subset/NotoSansKR-VF.ttf` mistake the
    /// script's docstring warns about drops ~5000 Han ideographs) fails here rather than on a
    /// reviewer's television.
    #[test]
    fn bundled_fallback_is_the_pinned_cut() {
        let c = cov("appfont-cjk.ttf");
        assert_eq!(
            c.len(), 44_810,
            "pkg/appfont-cjk.ttf covers {} codepoints, not the 44810 of Noto Sans CJK KR \
             (noto-cjk @ Sans2.004, Sans/Variable/TTF/NotoSansCJKkr-VF.ttf, instanced at wght 400). \
             If the pin moved on purpose, move this number and tools/cut-noto-cjk.py's together.",
            c.len()
        );
    }

    /// The whole point: the chain, not any one link. This is the assertion that would have
    /// failed on 2026-08-22 and passed on 2026-08-23.
    #[test]
    fn the_shipped_chain_can_draw_every_sample_string() {
        let links = [cov("appfont.ttf"), cov("appfont-cjk.ttf")];
        for &(script, s) in SAMPLES {
            let bad: Vec<char> = s
                .chars()
                .filter(|&ch| !links.iter().any(|c| c.contains(ch as u32)))
                .collect();
            assert!(
                bad.is_empty(),
                "{script}: the shipped chain cannot draw {bad:?} in {s:?}"
            );
        }
    }

    /// **RTL is out of scope, and this is where that is written down as a test rather than a
    /// comment.** Neither shipped face carries Hebrew or Arabic — and neither does the television's
    /// own `/usr/share/fonts/DroidSansFallback.ttf` (measured on webOS 4.10.0, 2026-08-23: 11172
    /// Hangul, 20902 Han, zero Hebrew, zero Arabic), so no link of the chain can draw them.
    ///
    /// Coverage would not be enough even if it existed. "The font contains the codepoint" is not
    /// "the renderer supports the script": Arabic needs joining and contextual shaping, Hebrew
    /// needs bidi reordering, and `text.rs` has neither — it hands byte runs to
    /// `TTF_RenderUTF8_Blended`, which in SDL2_ttf 2.0.x is a left-to-right advance loop with no
    /// shaper behind it. Adding a face here would turn tofu into *fluent-looking wrong text*,
    /// which is worse.
    ///
    /// So this test asserts the ABSENCE, deliberately: it fails the moment someone adds an RTL
    /// face, and the failure message is the reason not to. **Checklist #6 / #48 must not be marked
    /// Pass for RTL content on the strength of this unit.**
    #[test]
    fn rtl_is_out_of_scope_and_stays_that_way() {
        for name in ["appfont.ttf", "appfont-cjk.ttf"] {
            let c = cov(name);
            for (script, lo, hi) in [("Hebrew", 0x05D0u32, 0x05EAu32), ("Arabic", 0x0620, 0x064A)] {
                let n = count(&c, lo, hi);
                assert_eq!(
                    n, 0,
                    "pkg/{name} now carries {n} {script} codepoints. Coverage is NOT support: \
                     text.rs does no bidi and no shaping, so this would render fluent-looking \
                     wrong text instead of tofu. Ship a shaper before you ship the face, and do \
                     not mark checklist #6/#48 Pass for RTL until you have."
                );
            }
        }
    }

    // --- decoder unit tests, on synthetic sfnts -------------------------------------------------

    /// A minimal one-table sfnt wrapping `cmap`, so the decoder can be exercised on shapes no
    /// shipped font has (a notdef mapping, a format 12 group, a truncated table).
    fn sfnt(cmap: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes()); // numTables
        v.extend_from_slice(&[0; 6]); // searchRange/entrySelector/rangeShift
        v.extend_from_slice(b"cmap");
        v.extend_from_slice(&0u32.to_be_bytes()); // checksum
        v.extend_from_slice(&28u32.to_be_bytes()); // offset (12 + 16)
        v.extend_from_slice(&(cmap.len() as u32).to_be_bytes());
        v.extend_from_slice(cmap);
        v
    }

    /// cmap header + one subtable record pointing at `sub`.
    fn cmap1(platform: u16, encoding: u16, sub: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&platform.to_be_bytes());
        v.extend_from_slice(&encoding.to_be_bytes());
        v.extend_from_slice(&12u32.to_be_bytes());
        v.extend_from_slice(sub);
        v
    }

    fn format12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&12u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes()); // reserved
        v.extend_from_slice(&(16 + 12 * groups.len() as u32).to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes()); // language
        v.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for &(lo, hi, gid) in groups {
            v.extend_from_slice(&lo.to_be_bytes());
            v.extend_from_slice(&hi.to_be_bytes());
            v.extend_from_slice(&gid.to_be_bytes());
        }
        v
    }

    /// A cmap entry pointing at glyph 0 is a mapping to `.notdef` — the tofu box itself. Counting
    /// it as coverage is exactly the bug this module exists to prevent: the chain would resolve
    /// the run to a face that draws a box, and never consult the next link.
    #[test]
    fn a_mapping_to_glyph_zero_is_not_coverage() {
        let f = sfnt(&cmap1(
            3,
            10,
            &format12(&[(0x41, 0x43, 0), (0x61, 0x62, 7)]),
        ));
        let c = of_reader(&mut Cursor::new(f)).expect("decodes");
        assert!(!c.contains(0x41), "U+0041 maps to glyph 0");
        assert!(
            c.contains(0x42) && c.contains(0x43),
            "the rest of the group walks off glyph 1, 2"
        );
        assert!(c.contains(0x61) && c.contains(0x62));
        assert_eq!(c.len(), 4);
    }

    /// Astral coverage lives in a range list, not the bitset. `partition_point` has an off-by-one
    /// on both edges of every range, and the whole emoji/CJK-ext-B plane rides on it.
    #[test]
    fn supplementary_plane_ranges_answer_at_both_edges() {
        let f = sfnt(&cmap1(
            3,
            10,
            &format12(&[(0x2_0000, 0x2_A6DF, 1), (0x1_F600, 0x1_F64F, 9)]),
        ));
        let c = of_reader(&mut Cursor::new(f)).expect("decodes");
        for cp in [0x2_0000, 0x2_A6DF, 0x1_F600, 0x1_F64F, 0x2_5000] {
            assert!(c.contains(cp), "U+{cp:05X} is inside a declared group");
        }
        for cp in [0x1_FFFF, 0x2_A6E0, 0x1_F5FF, 0x1_F650] {
            assert!(
                !c.contains(cp),
                "U+{cp:05X} is outside every declared group"
            );
        }
        assert_eq!(
            c.len(),
            (0x2_A6DF - 0x2_0000 + 1) + (0x1_F64F - 0x1_F600 + 1)
        );
    }

    /// Format 4's second addressing mode (`idRangeOffset != 0`) indexes a trailing glyph array by
    /// pointer arithmetic relative to its own slot. It is the mode every real font uses for its
    /// sparse segments, and getting the base wrong silently reports a plausible WRONG set.
    #[test]
    fn format4_honours_the_id_range_offset_indirection() {
        // one real segment U+0041..U+0043 through glyphIdArray, plus the mandatory 0xFFFF segment
        let mut sub: Vec<u8> = Vec::new();
        let seg2 = 4u16;
        sub.extend_from_slice(&4u16.to_be_bytes()); // format
        sub.extend_from_slice(&0u16.to_be_bytes()); // length (unused by the decoder)
        sub.extend_from_slice(&0u16.to_be_bytes()); // language
        sub.extend_from_slice(&seg2.to_be_bytes());
        sub.extend_from_slice(&[0; 6]); // searchRange/entrySelector/rangeShift
        sub.extend_from_slice(&0x0043u16.to_be_bytes()); // endCode[0]
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes()); // endCode[1]
        sub.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        sub.extend_from_slice(&0x0041u16.to_be_bytes()); // startCode[0]
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes()); // startCode[1]
        sub.extend_from_slice(&0u16.to_be_bytes()); // idDelta[0]
        sub.extend_from_slice(&1u16.to_be_bytes()); // idDelta[1]
                                                    // idRangeOffset[0] must skip the rest of the array (2 entries * 2 bytes = 4) to reach
                                                    // glyphIdArray; entry 1 is the terminator and addresses nothing.
        sub.extend_from_slice(&4u16.to_be_bytes());
        sub.extend_from_slice(&0u16.to_be_bytes());
        sub.extend_from_slice(&5u16.to_be_bytes()); // glyphIdArray: U+0041 -> 5
        sub.extend_from_slice(&0u16.to_be_bytes()); //               U+0042 -> notdef
        sub.extend_from_slice(&6u16.to_be_bytes()); //               U+0043 -> 6
        let c = of_reader(&mut Cursor::new(sfnt(&cmap1(3, 1, &sub)))).expect("decodes");
        assert!(c.contains(0x41) && c.contains(0x43));
        assert!(!c.contains(0x42), "U+0042 indexes a zero in glyphIdArray");
        assert!(
            !c.contains(0xFFFF),
            "the terminating segment is not coverage"
        );
        assert_eq!(c.len(), 2);
    }

    /// A symbol cmap (3,0) remaps into the private-use area; treating it as Unicode would claim
    /// coverage of U+F020.. that no caller can ever ask for, and — worse — could make a garbage
    /// font look like it covers the declared set.
    #[test]
    fn non_unicode_subtables_are_ignored() {
        let f = sfnt(&cmap1(3, 0, &format12(&[(0xF020, 0xF0FF, 1)])));
        let err = of_reader(&mut Cursor::new(f))
            .err()
            .expect("a symbol-only cmap has no Unicode coverage");
        assert_eq!(err, CovErr::Malformed("no usable Unicode cmap subtable"));
    }

    /// Truncation must be an error, never a panic. `text.rs` calls this on a file the installer
    /// wrote, which is exactly where a short read comes from — and it runs on the SDL thread.
    #[test]
    fn a_truncated_font_is_an_error_not_a_panic() {
        let full = sfnt(&cmap1(3, 10, &format12(&[(0x41, 0x5A, 1)])));
        for cut in [0, 4, 11, 20, 30, full.len() - 1] {
            let r = of_reader(&mut Cursor::new(full[..cut].to_vec()));
            assert!(
                r.is_err(),
                "a font truncated to {cut} bytes decoded successfully"
            );
        }
        // ...and a group count the table cannot hold must not be trusted either. The last two are
        // the values that WRAP a 32-bit `usize`, which is the width the shipped ARM binary uses:
        // 12 × 357_913_941 is 0xFFFF_FFFC, so `16 + n * 12` becomes 12 in 32-bit arithmetic and
        // sails through a naive bounds check. This assertion cannot fail on the 64-bit dev Mac
        // whichever way `parse_format12_13` is written — it is here to pin the intent and to fail
        // for anyone who runs the suite on a 32-bit host. The guarantee itself comes from doing
        // the arithmetic in u64; see the comment at the check.
        for groups in [9999u32, 357_913_941, 357_913_942, u32::MAX] {
            let mut lying = format12(&[(0x41, 0x5A, 1)]);
            lying[12..16].copy_from_slice(&groups.to_be_bytes());
            let r = of_reader(&mut Cursor::new(sfnt(&cmap1(3, 10, &lying))));
            assert!(
                r.is_err(),
                "a format 12 claiming {groups} groups in 28 bytes decoded successfully"
            );
        }
    }

    /// A font whose only Unicode cmap subtable is one this decoder does not read must be an
    /// **error**, never an empty success. `text.rs::link_covers` falls back to "the base face
    /// covers everything" on `Err` and treats `Ok` as authoritative, so an empty `Ok` for
    /// `appfont.ttf` would push every accented Latin, Cyrillic and Greek character onto the CJK
    /// fallback — which has a full Latin set — and silently render the interface in the wrong
    /// typeface with nothing logged.
    #[test]
    fn a_font_with_only_unsupported_subtables_is_an_error_not_empty_coverage() {
        // format 0: a 256-byte byte-encoding table. Unicode platform, so it is not skipped as
        // non-Unicode — it is skipped because the decoder does not read format 0.
        let mut sub = vec![0u8; 262];
        sub[0..2].copy_from_slice(&0u16.to_be_bytes()); // format 0
        sub[2..4].copy_from_slice(&262u16.to_be_bytes());
        for (i, b) in sub.iter_mut().enumerate().skip(6) {
            *b = (i - 6) as u8; // every byte maps to a non-zero glyph
        }
        let err = of_reader(&mut Cursor::new(sfnt(&cmap1(3, 1, &sub))))
            .err()
            .expect("a format-0-only cmap is not coverage this decoder can vouch for");
        assert_eq!(err, CovErr::Malformed("no usable Unicode cmap subtable"));
    }
}
