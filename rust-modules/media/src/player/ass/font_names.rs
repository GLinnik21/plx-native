//! Which attached fonts a track asks for (issue #1328). Pure: bytes in, indices out.
//!
//! A file may attach far more font bytes than any renderer needs: an anime release carries a face
//! for every sign and karaoke style, 17 of them and 83 MB in the report. libass here runs with no
//! font provider (`src/ass.c`), so `ass_font_select` tries the requested family, then "Inter", then
//! the CJK file, and **an attached face that no requested name matches is never selected**: the bytes
//! it costs libass draw nothing. So the renderer hands libass the faces the track NAMES and nothing
//! else, and no byte count decides what is shown. A track names a face
//!
//!   * in its header: the Fontname of every `Style:` line (a sidecar script: the whole script);
//!   * or in an event: a `\fn` override, which [`Selection::wanted`] reads when the event is appended.
//!
//! **The rule is a superset of libass's matching**, so it can never hide text: a face is wanted when
//! a requested name equals (ASCII case folded, which is all `ass_strcasecmp` folds) its name table's
//! family (ID 1), full name (ID 4) or PostScript name (ID 6), on any platform and not only the
//! Microsoft one libass reads, the typographic and WWS family (ID 16, 21: libass counts only a
//! Microsoft-platform ID 1, and a face with none gets FreeType's `family_name`, which prefers these
//! two), or the CFF font name. "Inter" and "Arial" are always requested (`family_default` and libass's default Fontname). Wanting a face wants every
//! attachment that shares a family with it, added together in container order, so libass's weight
//! scoring and its add-order tie break between the members of a family are what they would be with
//! every font loaded. A face this module cannot read is WANTED: unknown means needed, because FreeType
//! may still name it. A byte-identical second copy is never added: libass would pick the first anyway.
//! Over-matching costs memory only; under-matching is what the tests pin.

use super::Font;
use std::collections::HashMap;

/// libass cuts a name at its own 1024-byte buffer (`get_font_info`); so does this.
const NAME_RECORD_BYTES: usize = 1024;
/// The names every renderer requests whatever the track says: libass's `family_default` and the
/// Fontname a style gets when it sets none.
const ALWAYS: [&[u8]; 2] = [b"inter", b"arial"];

const NAME_FAMILY: u16 = 1;
const NAME_FULL: u16 = 4;
const NAME_POSTSCRIPT: u16 = 6;
const NAME_TYPOGRAPHIC_FAMILY: u16 = 16;
const NAME_WWS_FAMILY: u16 = 21;

/// What one face answers to, normalized by [`fold`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Face {
    /// Family (ID 1, 16, 21), full and PostScript names.
    pub names: Vec<Vec<u8>>,
    /// The family names alone: what the family closure joins faces by.
    pub families: Vec<Vec<u8>>,
}

/// A name as it is compared: ASCII-lowercased, whitespace trimmed, one leading `@` (libass's
/// vertical-text marker) dropped. `None` for what can never name a face: nothing at all, or "0",
/// which `\fn0` reads as "the style's own font".
pub(super) fn fold(raw: &[u8]) -> Option<Vec<u8>> {
    let raw = trim(raw);
    let raw = trim(raw.strip_prefix(b"@").unwrap_or(raw));
    if raw.is_empty() || raw == b"0" {
        return None;
    }
    let raw = &raw[..raw.len().min(NAME_RECORD_BYTES)];
    Some(raw.to_ascii_lowercase())
}

fn trim(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if !matches!(first, b' ' | b'\t' | b'\r' | b'\n') {
            break;
        }
        bytes = rest;
    }
    while let [rest @ .., last] = bytes {
        if !matches!(last, b' ' | b'\t' | b'\r' | b'\n') {
            break;
        }
        bytes = rest;
    }
    bytes
}

fn be16(bytes: &[u8], at: usize) -> Option<usize> {
    let end = at.checked_add(2)?;
    Some(u16::from_be_bytes(bytes.get(at..end)?.try_into().ok()?) as usize)
}

fn be32(bytes: &[u8], at: usize) -> Option<usize> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(bytes.get(at..end)?.try_into().ok()?) as usize)
}

/// The bytes of table `tag` of the sfnt whose offset table starts at `start`.
fn table<'a>(bytes: &'a [u8], start: usize, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let count = be16(bytes, start.checked_add(4)?)?;
    let directory = start.checked_add(12)?;
    for i in 0..count {
        let record = directory.checked_add(i.checked_mul(16)?)?;
        if bytes.get(record..record.checked_add(4)?)? == tag {
            let offset = be32(bytes, record + 8)?;
            let length = be32(bytes, record + 12)?;
            return bytes.get(offset..offset.checked_add(length)?);
        }
    }
    None
}

/// A name record's string as UTF-8: UTF-16BE for the Unicode and Windows platforms, Latin-1 for
/// Mac Roman (its bytes above 0x7F are not Latin-1, but no ASCII fold maps them to anything a style
/// would spell, and libass reads no Mac record at all).
fn decode(platform: usize, raw: &[u8]) -> Option<String> {
    match platform {
        0 | 3 => {
            let units = raw.chunks_exact(2).map(|u| u16::from_be_bytes([u[0], u[1]]));
            Some(char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect())
        }
        1 => Some(raw.iter().map(|&b| char::from(b)).collect()),
        _ => None,
    }
}

/// The names of the face whose offset table starts at `start`, or `None` when it names nothing
/// this module can read: no `name` table, one that is truncated, or one with no usable record.
fn face(bytes: &[u8], start: usize) -> Option<Face> {
    let names = table(bytes, start, b"name")?;
    let count = be16(names, 2)?;
    let strings = be16(names, 4)?;
    let mut found: Vec<(u16, Vec<u8>)> = Vec::new();
    for i in 0..count {
        let record = 6usize.checked_add(i.checked_mul(12)?)?;
        let (platform, id) = (be16(names, record)?, be16(names, record + 6)? as u16);
        if !matches!(id, NAME_FAMILY | NAME_FULL | NAME_POSTSCRIPT | NAME_TYPOGRAPHIC_FAMILY | NAME_WWS_FAMILY) {
            continue;
        }
        let (length, offset) = (be16(names, record + 8)?, be16(names, record + 10)?);
        let from = strings.checked_add(offset)?;
        let Some(raw) = names.get(from..from.checked_add(length)?) else { continue };
        let Some(text) = decode(platform, raw) else { continue };
        if let Some(name) = fold(text.as_bytes()) {
            found.push((id, name));
        }
    }
    // libass counts only a Microsoft-platform ID 1 as a family and otherwise takes FreeType's
    // `family_name`, which prefers ID 16 and 21; which of the two a face gets is decided by records
    // this reader does not keep apart, so ID 16 and 21 are families always (a superset).
    let mut out = Face::default();
    for (id, name) in found {
        let family = matches!(id, NAME_FAMILY | NAME_TYPOGRAPHIC_FAMILY | NAME_WWS_FAMILY);
        if family && !out.families.contains(&name) {
            out.families.push(name.clone());
        }
        if (family || matches!(id, NAME_FULL | NAME_POSTSCRIPT)) && !out.names.contains(&name) {
            out.names.push(name);
        }
    }
    // An OpenType/CFF face's PostScript name, as FreeType reports it, is the CFF font name.
    if let Some(name) = cff_name(bytes, start).and_then(|n| fold(&n)) {
        if !out.names.contains(&name) {
            out.names.push(name);
        }
    }
    (!out.names.is_empty()).then_some(out)
}

/// The first entry of a `CFF ` table's Name INDEX.
fn cff_name(bytes: &[u8], start: usize) -> Option<Vec<u8>> {
    let cff = table(bytes, start, b"CFF ")?;
    let index = *cff.get(2)? as usize;
    let count = be16(cff, index)?;
    let offset_size = *cff.get(index.checked_add(2)?)? as usize;
    if count == 0 || !(1..=4).contains(&offset_size) {
        return None;
    }
    let offsets = index.checked_add(3)?;
    let read = |n: usize| -> Option<usize> {
        let at = offsets.checked_add(n.checked_mul(offset_size)?)?;
        let raw = cff.get(at..at.checked_add(offset_size)?)?;
        Some(raw.iter().fold(0usize, |sum, &b| (sum << 8) | b as usize))
    };
    // Offsets count from the byte before the data, so the first entry is `[off0 - 1, off1 - 1)`.
    let data = offsets.checked_add(count.checked_add(1)?.checked_mul(offset_size)?)?.checked_sub(1)?;
    let (from, to) = (read(0)?, read(1)?);
    let name = cff.get(data.checked_add(from)?..data.checked_add(to)?)?;
    Some(name[..name.len().min(NAME_RECORD_BYTES)].to_vec())
}

/// Every face of the font file `bytes` (one, or a TrueType Collection's several), or `None` when
/// the file is not one this module can name: the caller treats that as "wanted".
pub(super) fn faces(bytes: &[u8]) -> Option<Vec<Face>> {
    if bytes.get(..4)? == b"ttcf" {
        let count = be32(bytes, 8)?.min(bytes.len() / 4);
        let mut seen = std::collections::HashSet::new();
        let mut starts: Vec<usize> = Vec::new();
        for i in 0..count {
            let start = be32(bytes, 12 + i * 4)?;
            if seen.insert(start) {
                starts.push(start);
            }
        }
        // One unreadable face makes the whole file unknown: it may be the one a style names.
        starts.iter().map(|&start| face(bytes, start)).collect::<Option<Vec<_>>>().filter(|f| !f.is_empty())
    } else {
        Some(vec![face(bytes, 0)?])
    }
}

/// `text` is the bytes of an ASS header or script, or of one event's packet: every name it asks
/// for, folded. `script` is true for a header or a whole script, whose `Style:` lines name a
/// Fontname each, and whose `[Fonts]` and `[Graphics]` sections (uuencoded, so they hold backslashes
/// that are not tags) are not read; an event's packet holds no style lines and is read for `\fn` alone.
pub(super) fn requested(text: &[u8], script: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if !script {
        overrides(text, &mut out);
        return out;
    }
    let (mut styles, mut skip, mut column) = (false, false, None::<usize>);
    for line in text.split(|&b| b == b'\n') {
        let line = trim(line);
        if line.first() == Some(&b'[') {
            let section = line.to_ascii_lowercase();
            styles = section.windows(6).any(|w| w == b"styles");
            skip = section.starts_with(b"[fonts") || section.starts_with(b"[graphics");
            continue;
        }
        if skip {
            continue;
        }
        if styles {
            if let Some(rest) = strip_key(line, b"format:") {
                column = rest.split(|&b| b == b',').position(|f| trim(f).eq_ignore_ascii_case(b"fontname"));
            } else if let Some(rest) = strip_key(line, b"style:") {
                let field = rest.split(|&b| b == b',').nth(column.unwrap_or(1));
                out.extend(field.and_then(fold));
            }
        }
        overrides(line, &mut out);
    }
    out
}

fn strip_key<'a>(line: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    (line.len() >= key.len() && line[..key.len()].eq_ignore_ascii_case(key)).then(|| &line[key.len()..])
}

/// Every `\fn` argument in `text`. libass reads a tag after a backslash and any spaces, ends a bare
/// argument at the next `\`, and ends the tag name at a `(`; the argument is also read to the `}`
/// of its block, and both readings are kept (a `(` is part of the name in some files and the start
/// of a parenthesized argument in others; the extra name costs a face at most). An argument inside
/// `\t(..)` ends at that tag's `)` (`ass_parse_tags` is handed the end of the `\t` argument), so
/// `{\t(0,500,\fnFoo)}` asks for "Foo" and the name is also cut at the first `)` that precedes any
/// `(`.
fn overrides(text: &[u8], out: &mut Vec<Vec<u8>>) {
    let mut at = 0;
    while let Some(found) = text[at..].iter().position(|&b| b == b'\\') {
        at += found + 1;
        let mut tag = at;
        while text.get(tag) == Some(&b' ') {
            tag += 1;
        }
        if text.get(tag..tag + 2) != Some(b"fn".as_slice()) {
            continue;
        }
        let start = tag + 2;
        let end = start + text[start..].iter().position(|&b| matches!(b, b'\\' | b'}' | b'\n' | b'\r')).unwrap_or(text.len() - start);
        let argument = &text[start..end];
        out.extend(fold(argument));
        let open = argument.iter().position(|&b| b == b'(');
        if let Some(paren) = open {
            out.extend(fold(&argument[..paren]));
            let inner = &argument[paren + 1..];
            let close = inner.iter().position(|&b| b == b')').unwrap_or(inner.len());
            out.extend(fold(&inner[..close]));
        }
        // Inside `\t(..)` the name ends at the tag's own `)`; a `)` after a `(` closes that `(`.
        if let Some(close) = argument.iter().position(|&b| b == b')').filter(|&c| open.is_none_or(|o| c < o)) {
            out.extend(fold(&argument[..close]));
        }
        at = end;
    }
}

/// One attachment, as the selection knows it.
struct Entry {
    /// No name could be read: wanted at the first seed.
    unknown: bool,
    /// A byte-identical copy of an earlier attachment: never added.
    duplicate: bool,
    families: Vec<Vec<u8>>,
}

/// The attached fonts of one file and which of them the renderer has been given. Owned by the
/// renderer's `Native`: the font list is fixed for the renderer's life, so indices stay valid.
pub(super) struct Selection {
    entries: Vec<Entry>,
    by_name: HashMap<Vec<u8>, Vec<usize>>,
    by_family: HashMap<Vec<u8>, Vec<usize>>,
    added: Vec<bool>,
}

impl Selection {
    /// Read the names of every font once.
    pub(super) fn new(fonts: &[Font]) -> Self {
        let mut entries = Vec::with_capacity(fonts.len());
        let mut by_name: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        let mut by_family: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        // Same-length, equal-bytes copies: a length match is rare and a mismatch ends at the first
        // differing byte, so this reads a font in full only when it is a real duplicate.
        let mut firsts: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, font) in fonts.iter().enumerate() {
            let kept = firsts.entry(font.data.len()).or_default();
            let duplicate = kept.iter().any(|&i| fonts[i].data == font.data);
            if !duplicate {
                kept.push(index);
            }
            let faces = if duplicate { None } else { faces(&font.data) };
            let mut families = Vec::new();
            if let Some(faces) = &faces {
                for face in faces {
                    for name in &face.names {
                        let holders = by_name.entry(name.clone()).or_default();
                        if !holders.contains(&index) {
                            holders.push(index);
                        }
                    }
                    for family in &face.families {
                        let members = by_family.entry(family.clone()).or_default();
                        if !members.contains(&index) {
                            members.push(index);
                            families.push(family.clone());
                        }
                    }
                }
            }
            entries.push(Entry { unknown: !duplicate && faces.is_none(), duplicate, families });
        }
        let added = vec![false; fonts.len()];
        Self { entries, by_name, by_family, added }
    }

    /// The fonts a header (or a whole sidecar script) asks for that the renderer does not hold
    /// yet, in container order: its styles' Fontnames, every `\fn`, the always-requested names, and
    /// every font this module cannot read. Marks them held.
    pub(super) fn seed(&mut self, header: &[u8]) -> Vec<usize> {
        let mut names = requested(header, true);
        names.extend(ALWAYS.iter().map(|n| n.to_vec()));
        let mut wanted = self.take(&names);
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.unknown && !self.added[index] {
                wanted.push(index);
            }
        }
        self.mark(wanted)
    }

    /// The fonts the `\fn` overrides of one event's packet ask for that the renderer does not hold
    /// yet, in container order. Marks them held: the caller adds them before the event.
    pub(super) fn wanted(&mut self, event: &[u8]) -> Vec<usize> {
        let names = requested(event, false);
        if names.is_empty() {
            return Vec::new();
        }
        let wanted = self.take(&names);
        self.mark(wanted)
    }

    /// The indices holding any of `names`, joined by family, not yet added.
    fn take(&self, names: &[Vec<u8>]) -> Vec<usize> {
        let mut wanted = Vec::new();
        for name in names {
            for &index in self.by_name.get(name).into_iter().flatten() {
                wanted.push(index);
                for family in &self.entries[index].families {
                    wanted.extend(self.by_family[family].iter().copied());
                }
            }
        }
        wanted
    }

    fn mark(&mut self, mut wanted: Vec<usize>) -> Vec<usize> {
        wanted.sort_unstable();
        wanted.dedup();
        wanted.retain(|&i| !self.added[i] && !self.entries[i].duplicate);
        for &index in &wanted {
            self.added[index] = true;
        }
        wanted
    }
}

#[cfg(test)]
mod tests;
