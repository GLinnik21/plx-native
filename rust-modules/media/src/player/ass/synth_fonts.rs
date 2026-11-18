//! Test-only font synthesis: a real shipped face (`pkg/appfont.ttf`) given another name, padded to
//! any size, or a name table written record by record, so a test can prove that a face was selected
//! by its name (it draws differently from the fallback) without committing a font. A Rust port of
//! `tests/fixtures/make_ass_stress.py::rename_font` (`sfnt_build`, `renamed_name_table`), plus the
//! shapes that script never needs: a Mac-only or family-less name table, a TrueType Collection and
//! an OpenType/CFF face.

use std::collections::BTreeMap;

pub(super) type Tag = [u8; 4];
pub(super) type Tables = BTreeMap<Tag, Vec<u8>>;

/// One name record: (platform, encoding, name id, raw bytes). The language is always English.
pub(super) type NameRecord = (u16, u16, u16, Vec<u8>);

fn u16_at(bytes: &[u8], at: usize) -> usize {
    u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap()) as usize
}

fn u32_at(bytes: &[u8], at: usize) -> usize {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}

/// The version tag and every table of a single-face font file.
pub(super) fn parse(data: &[u8]) -> (Tag, Tables) {
    let tables = (0..u16_at(data, 4))
        .map(|i| {
            let record = 12 + 16 * i;
            let offset = u32_at(data, record + 8);
            let length = u32_at(data, record + 12);
            (data[record..record + 4].try_into().unwrap(), data[offset..offset + length].to_vec())
        })
        .collect();
    (data[..4].try_into().unwrap(), tables)
}

fn sum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// A font file for `tables`, its tables placed from absolute offset `base` (non-zero inside a
/// collection): sorted directory, 4-byte aligned tables, real checksums.
pub(super) fn build_at(version: Tag, tables: &Tables, base: usize) -> Vec<u8> {
    let count = tables.len();
    let entry = usize::BITS as usize - 1 - count.leading_zeros() as usize;
    let search = 16 << entry;
    let mut directory = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    let mut head_at = None;
    for (tag, table) in tables {
        let mut table = table.clone();
        if tag == b"head" {
            head_at = Some(base + 12 + 16 * count + body.len() + 8);
            table[8..12].fill(0); // checkSumAdjustment is summed as zero
        }
        directory.extend_from_slice(tag);
        directory.extend_from_slice(&sum(&table).to_be_bytes());
        directory.extend_from_slice(&((base + 12 + 16 * count + body.len()) as u32).to_be_bytes());
        directory.extend_from_slice(&(table.len() as u32).to_be_bytes());
        body.extend_from_slice(&table);
        body.resize(body.len().next_multiple_of(4), 0);
    }
    let mut font = version.to_vec();
    for field in [count, search, entry, 16 * count - search] {
        font.extend_from_slice(&(field as u16).to_be_bytes());
    }
    font.extend_from_slice(&directory);
    font.extend_from_slice(&body);
    if let Some(at) = head_at {
        let at = at - base;
        let adjustment = 0xB1B0_AFBAu32.wrapping_sub(sum(&font));
        font[at..at + 4].copy_from_slice(&adjustment.to_be_bytes());
    }
    font
}

pub(super) fn build(version: Tag, tables: &Tables) -> Vec<u8> {
    build_at(version, tables, 0)
}

/// A format-0 `name` table of `records`.
pub(super) fn name_table(records: &[NameRecord]) -> Vec<u8> {
    let mut sorted: Vec<&NameRecord> = records.iter().collect();
    sorted.sort_by_key(|(platform, encoding, id, _)| (*platform, *encoding, *id));
    let mut head = Vec::new();
    let mut store: Vec<u8> = Vec::new();
    for (platform, encoding, id, raw) in sorted {
        for field in [*platform, *encoding, 0x409, *id, raw.len() as u16, store.len() as u16] {
            head.extend_from_slice(&field.to_be_bytes());
        }
        store.extend_from_slice(raw);
    }
    let mut table = Vec::new();
    for field in [0u16, (head.len() / 12) as u16, (6 + head.len()) as u16] {
        table.extend_from_slice(&field.to_be_bytes());
    }
    table.extend_from_slice(&head);
    table.extend_from_slice(&store);
    table
}

pub(super) fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// The Windows records of a face named `family`, `style`: family, subfamily, unique, full and
/// PostScript names (ids 1, 2, 3, 4, 6).
pub(super) fn windows_names(family: &str, style: &str) -> Vec<NameRecord> {
    let full = if style == "Regular" { family.to_string() } else { format!("{family} {style}") };
    let postscript = format!("{}-{style}", family.replace(' ', ""));
    [(1, family), (2, style), (3, "plx-ass-test"), (4, &full), (6, &postscript)]
        .into_iter()
        .map(|(id, text)| (3, 1, id, utf16(text)))
        .collect()
}

/// `data`, a single-face font, renamed to `family`, `style`; with `pad_to` the file is exactly that
/// many bytes, the difference a private `PLXP` table of unread bytes (legal sfnt, which FreeType
/// skips). The glyph tables are untouched, so the face draws as `data` does.
pub(crate) fn rename(data: &[u8], family: &str, style: &str, pad_to: Option<usize>) -> Vec<u8> {
    let (version, mut tables) = parse(data);
    tables.insert(*b"name", name_table(&windows_names(family, style)));
    let font = build(version, &tables);
    let Some(pad_to) = pad_to else { return font };
    assert!(pad_to % 4 == 0 && pad_to > font.len() + 16, "{family}: base is larger than {pad_to}");
    let mut state = family.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |s, b| (s ^ u64::from(b)).wrapping_mul(0x100_0000_01B3));
    let mut pad: Vec<u8> = Vec::with_capacity(pad_to);
    while pad.len() < pad_to - font.len() - 16 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let take = (pad_to - font.len() - 16 - pad.len()).min(8);
        pad.extend_from_slice(&state.to_le_bytes()[..take]);
    }
    tables.insert(*b"PLXP", pad);
    let padded = build(version, &tables);
    assert_eq!(padded.len(), pad_to);
    padded
}

/// A TrueType Collection of single-face fonts: each one's tables are placed after the shared
/// `ttcf` header, and the header lists their offset tables.
pub(super) fn collection(faces: &[Vec<u8>]) -> Vec<u8> {
    let header = 12 + 4 * faces.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(faces.len() as u32).to_be_bytes());
    let mut body: Vec<u8> = Vec::new();
    let mut offsets = Vec::new();
    for face in faces {
        let (version, tables) = parse(face);
        let at = header + body.len();
        offsets.push(at as u32);
        body.extend_from_slice(&build_at(version, &tables, at));
    }
    for offset in offsets {
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out.extend_from_slice(&body);
    out
}

/// An OpenType/CFF face with only the two tables a name read needs: `name` from `records` and a
/// `CFF ` table whose Name INDEX holds `cff_name`.
pub(super) fn cff_font(records: &[NameRecord], cff_name: &str) -> Vec<u8> {
    let mut cff = vec![1, 0, 4, 1]; // major, minor, hdrSize, offSize
    cff.extend_from_slice(&1u16.to_be_bytes()); // one name
    cff.push(1); // offSize
    cff.extend_from_slice(&[1, 1 + cff_name.len() as u8]);
    cff.extend_from_slice(cff_name.as_bytes());
    let mut tables = Tables::new();
    tables.insert(*b"name", name_table(records));
    tables.insert(*b"CFF ", cff);
    build(*b"OTTO", &tables)
}
