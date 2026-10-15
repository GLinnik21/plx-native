//! The shipped subtitle faces, checked as files (no libass): the add order, the manifest against
//! the bytes, what each face covers and what FreeType and libass will make of its names. The
//! native renders of the same faces are in `ass/tests.rs`.

use super::*;
use plx_base::fontcov::{self, Coverage};
use plx_base::sha256::sha256;

fn dir() -> &'static Path {
    super::super::asset_dir()
}

fn manifest() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir().join(SUBS_DIR).join(MANIFEST)).unwrap()).unwrap()
}

fn faces() -> Vec<serde_json::Value> {
    manifest()["faces"].as_array().unwrap().clone()
}

fn cov(name: &str) -> Coverage {
    fontcov::of_file(&dir().join(name)).unwrap()
}

fn subs_cov(face: &serde_json::Value) -> Coverage {
    fontcov::of_file(&dir().join(SUBS_DIR).join(face["file"].as_str().unwrap())).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn be16(b: &[u8], at: usize) -> usize {
    u16::from_be_bytes([b[at], b[at + 1]]) as usize
}

fn be32(b: &[u8], at: usize) -> usize {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}

fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    (0..be16(font, 4)).find_map(|i| {
        let rec = 12 + i * 16;
        (&font[rec..rec + 4] == tag).then(|| &font[be32(font, rec + 8)..][..be32(font, rec + 12)])
    })
}

/// (platformID, languageID, nameID, text) of every Windows Unicode record of the `name` table.
fn names(font: &[u8]) -> Vec<(usize, usize, usize, String)> {
    let t = table(font, b"name").expect("a name table");
    let (count, strings) = (be16(t, 2), be16(t, 4));
    (0..count)
        .filter_map(|i| {
            let r = 6 + i * 12;
            let (platform, encoding, lang, id) = (be16(t, r), be16(t, r + 2), be16(t, r + 4), be16(t, r + 6));
            if platform != 3 || encoding != 1 {
                return None;
            }
            let units: Vec<u16> = t[strings + be16(t, r + 10)..][..be16(t, r + 8)]
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            Some((platform, lang, id, String::from_utf16(&units).unwrap()))
        })
        .collect()
}

/// FreeType's `tt_face_get_name` (sfobjs.c) for the family: the first Windows record, replaced by
/// any later one whose language is an English variant. `strict` is the newer rule, English-US only.
fn freetype_family(font: &[u8], strict: bool) -> String {
    let mut found: Option<(usize, String)> = None;
    for (_, lang, id, text) in names(font) {
        if id != 1 {
            continue;
        }
        let english = if strict { lang == 0x409 } else { lang & 0x3FF == 0x009 };
        match &found {
            None => found = Some((lang, text)),
            Some(_) if english => found = Some((lang, text)),
            _ => {}
        }
    }
    found.expect("a family record").1
}

fn family_records(font: &[u8], id: usize) -> Vec<(usize, String)> {
    names(font).into_iter().filter(|r| r.2 == id).map(|r| (r.1, r.3)).collect()
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap()
}

fn read_sub(face: &serde_json::Value) -> Vec<u8> {
    std::fs::read(dir().join(SUBS_DIR).join(face["file"].as_str().unwrap())).unwrap()
}

/// Inter first, Inter Bold second, then the Noto faces in manifest order: libass breaks ties among
/// members of a family by the order they were added.
#[test]
fn the_bundled_faces_go_in_inter_first_then_the_manifest_order() {
    let files = bundled(dir()).unwrap();
    let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
    assert_eq!(&names[..2], ["appfont.ttf", "appfont-bold.ttf"]);
    let listed: Vec<String> = faces().iter().map(|f| f["file"].as_str().unwrap().to_string()).collect();
    assert_eq!(&names[2..], &listed[..]);
    assert!(listed.len() >= 20, "{} faces", listed.len());
    for f in &files {
        assert!(f.is_file(), "{f:?} is not shipped");
    }
}

#[test]
fn a_missing_or_hostile_manifest_is_fonts_unavailable() {
    let root = std::env::temp_dir().join(format!("plx-subs-manifest-{}", std::process::id()));
    let subs = root.join(SUBS_DIR);
    std::fs::create_dir_all(&subs).unwrap();
    assert!(matches!(bundled(&root), Err(Fault::FontsUnavailable)), "no manifest");
    for body in [
        "not json",
        r#"{"faces":[]}"#,
        r#"{"faces":[{"file":"../appfont.ttf"}]}"#,
        r#"{"faces":[{"file":"a/b.ttf"}]}"#,
        r#"{"faces":[{"file":".hidden.ttf"}]}"#,
        r#"{"faces":[{"file":"x.otf"}]}"#,
    ] {
        std::fs::write(subs.join(MANIFEST), body).unwrap();
        assert!(matches!(bundled(&root), Err(Fault::FontsUnavailable)), "{body}");
    }
    std::fs::write(subs.join(MANIFEST), r#"{"faces":[{"file":"a.ttf"}]}"#).unwrap();
    assert_eq!(bundled(&root).unwrap().len(), 3);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_manifest_matches_the_shipped_bytes() {
    for f in faces() {
        let bytes = read_sub(&f);
        assert_eq!(hex(&sha256(&bytes)), f["sha256"].as_str().unwrap(), "{}", f["file"]);
        assert_eq!(bytes.len() as u64, f["bytes"].as_u64().unwrap(), "{}", f["file"]);
    }
    let on_disk = std::fs::read_dir(dir().join(SUBS_DIR))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "ttf"))
        .count();
    assert_eq!(on_disk, faces().len(), "a face on disk the manifest does not list, or the reverse");
}

/// The oldest FreeType in the fleet (webOS 3) refuses more than 65534 glyphs and a `loca` past
/// 256 KiB is a sign the cut kept far more than a subtitle needs.
#[test]
fn every_face_opens_under_old_freetype_bounds() {
    for f in faces() {
        let bytes = read_sub(&f);
        let maxp = table(&bytes, b"maxp").unwrap();
        assert!(be16(maxp, 4) <= 65534, "{}", f["file"]);
        assert_eq!(be16(maxp, 4) as u64, f["glyphs"].as_u64().unwrap());
        assert!(table(&bytes, b"loca").unwrap().len() < 0x40000, "{}", f["file"]);
        assert!(table(&bytes, b"glyf").is_some(), "{}: TrueType outlines only", f["file"]);
        assert!(subs_cov(&f).len() > 0);
    }
}

#[test]
fn freetype_keeps_reporting_the_real_family_under_either_rule() {
    let bold = read("appfont-bold.ttf");
    for strict in [false, true] {
        assert_eq!(freetype_family(&bold, strict), "Inter");
        assert_eq!(freetype_family(&read("appfont.ttf"), strict), "Inter");
        for f in faces() {
            assert_eq!(freetype_family(&read_sub(&f), strict), f["family"].as_str().unwrap(), "{}", f["file"]);
        }
    }
}

#[test]
fn the_aliases_are_the_records_libass_looks_for() {
    let bold = read("appfont-bold.ttf");
    for id in [1, 4] {
        let rec = family_records(&bold, id);
        assert!(rec.contains(&(0x407, "PlxSubs".into())), "Inter Bold id {id}: {rec:?}");
    }
    for id in [1, 4] {
        assert!(
            !family_records(&read("appfont.ttf"), id).iter().any(|r| r.1 == "PlxSubs"),
            "only Bold answers to PlxSubs"
        );
    }
    for f in faces() {
        let bytes = read_sub(&f);
        let own = f["family"].as_str().unwrap();
        let rec = family_records(&bytes, 1);
        assert!(rec.contains(&(0x409, own.into())), "{own}: {rec:?}");
        assert!(rec.contains(&(0x407, "PlxSubs".into())), "{own}: {rec:?}");
        assert!(rec.contains(&(0x40C, "Inter".into())), "{own}: {rec:?}");
        let full = family_records(&bytes, 4);
        assert!(full.contains(&(0x407, "PlxSubs".into())), "{own}: full name {full:?}");
        assert!(full.contains(&(0x40C, "Inter".into())), "{own}: full name {full:?}");
        for (lang, _) in rec.iter().filter(|(_, t)| t == "PlxSubs" || t == "Inter") {
            assert_ne!(lang & 0x3FF, 0x009, "{own}: an alias under an English-variant language");
        }
    }
}

/// Latin, digits and punctuation are Inter Bold's, so no Noto face may carry any of what Inter
/// Bold has, and nothing under U+0250 (nor combining marks, general punctuation, currency).
#[test]
fn latin_resolves_to_inter_bold_and_no_noto_face_competes_for_it() {
    let inter = cov("appfont-bold.ttf");
    for f in faces() {
        let c = subs_cov(&f);
        for cp in (0..=0x24F).chain(0x300..=0x36F).chain(0x2000..=0x206F).chain(0x20A0..=0x20CF) {
            let kept = matches!(cp, 0x200C..=0x200F);
            assert!(!c.contains(cp) || kept, "{} carries U+{cp:04X}", f["file"]);
        }
        for cp in 0..=0xFFFFu32 {
            if inter.contains(cp) && c.contains(cp) {
                assert!(matches!(cp, 0x200C | 0x200D | 0x25CC), "{} duplicates Inter U+{cp:04X}", f["file"]);
            }
        }
    }
    for ch in "AZaz09éßŁ".chars() {
        assert!(inter.contains(ch as u32), "{ch}");
    }
}

#[test]
fn symbols_subtitles_use_are_drawn_by_inter_bold_or_the_cjk_default() {
    let (inter, cjk) = (cov("appfont-bold.ttf"), cov("appfont-cjk.ttf"));
    for ch in "♪♫…–—“”‘’«»¿¡•©®™".chars() {
        assert!(inter.contains(ch as u32) || cjk.contains(ch as u32), "U+{:04X} {ch}", ch as u32);
    }
}

#[test]
fn each_script_resolves_to_its_noto_face_and_not_to_inter() {
    let inter = cov("appfont-bold.ttf");
    for f in faces() {
        let c = subs_cov(&f);
        let sample = f["sample"].as_str().unwrap();
        assert!(sample.chars().count() >= 4, "{}", f["file"]);
        for ch in sample.chars() {
            assert!(c.contains(ch as u32), "{} lacks its sample U+{:04X}", f["file"], ch as u32);
            assert!(!inter.contains(ch as u32), "Inter Bold covers {}'s sample U+{:04X}", f["file"], ch as u32);
        }
        let ranges = f["ranges"].as_array().unwrap();
        let mut counted = 0;
        for r in ranges {
            let (lo, hi) = (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() as u32);
            for cp in lo..=hi {
                assert!(c.contains(cp), "{}: range U+{cp:04X} not in the cmap", f["file"]);
                counted += 1;
            }
        }
        assert_eq!(counted, c.len(), "{}: the manifest ranges are the cmap", f["file"]);
        assert_eq!(counted as u64, f["codepoints"].as_u64().unwrap());
    }
}

/// libass sizes a line from `usWinAscent + usWinDescent` per em, and splits it at the ascent. A face
/// whose total differs draws its script larger or smaller than the Latin beside it (Thai was 1.511
/// em against Inter Bold's 1.430), and one whose split differs draws it higher or lower. Both are
/// held, each in its own unit: the total in em (read from `head`), the split as a fraction.
#[test]
fn every_face_has_inter_bolds_win_metrics() {
    let metrics = |font: &[u8]| {
        let os2 = table(font, b"OS/2").unwrap();
        let upem = be16(table(font, b"head").unwrap(), 18) as f64;
        let (asc, desc) = (be16(os2, 74) as f64, be16(os2, 76) as f64);
        ((asc + desc) / upem, asc / (asc + desc))
    };
    let (inter_em, inter_split) = metrics(&read("appfont-bold.ttf"));
    assert!((inter_em - 1.430).abs() < 0.001, "Inter Bold's own total is {inter_em} em");
    for f in faces() {
        let (em, split) = metrics(&read_sub(&f));
        assert!((em - inter_em).abs() < 0.001, "{}: {em} em vs Inter's {inter_em}", f["file"]);
        assert!((split - inter_split).abs() < 0.001, "{}: split {split} vs Inter's {inter_split}", f["file"]);
    }
}

#[test]
fn shaped_scripts_keep_their_layout_tables() {
    for f in faces() {
        let bytes = read_sub(&f);
        assert!(table(&bytes, b"GSUB").is_some() || table(&bytes, b"GPOS").is_some(), "{}", f["file"]);
        if matches!(f["script"].as_str().unwrap(), "Thai" | "Devanagari" | "Arabic" | "Khmer" | "Myanmar") {
            assert!(table(&bytes, b"GSUB").is_some(), "{} lost GSUB", f["file"]);
            assert!(table(&bytes, b"GPOS").is_some(), "{} lost GPOS", f["file"]);
        }
    }
}

/// What the picker offers and what is left out is derived, not remembered: every language of
/// `languages.rs` is in exactly one bucket (a face's `languages`, `uncovered`, the manifest's `cjk`
/// or `inter` list), and each claim is checked against the shipped cmaps. A language added to the
/// picker without a decision here fails this test.
#[test]
fn every_language_has_exactly_one_bucket_and_the_claim_holds() {
    use plx_plex::plex::languages::LANGUAGES;
    let m = manifest();
    let codes = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect()
    };
    let (inter, cjk) = (cov("appfont-bold.ttf"), cov("appfont-cjk.ttf"));
    let mut seen: Vec<String> = Vec::new();
    let mut claim = |bucket: &str, listed: Vec<String>, covered: &dyn Fn(u32) -> bool| {
        for code in listed {
            let lang = LANGUAGES.iter().find(|l| l.code == code).unwrap_or_else(|| panic!("{bucket}: unknown language {code}"));
            let missing: Vec<char> = lang.name.chars().filter(|c| !c.is_whitespace() && !covered(*c as u32)).collect();
            assert!(missing.is_empty(), "{bucket}/{code}: {:?} lacks {missing:?}", lang.name);
            seen.push(code);
        }
    };
    claim("inter", codes(&m["buckets"]["inter"]), &|cp| inter.contains(cp));
    claim("cjk", codes(&m["buckets"]["cjk"]), &|cp| cjk.contains(cp));
    for f in faces() {
        let c = subs_cov(&f);
        claim(f["script"].as_str().unwrap(), codes(&f["languages"]), &|cp| inter.contains(cp) || c.contains(cp));
    }
    for u in m["uncovered"].as_array().unwrap() {
        assert!(u["reason"].as_str().unwrap().len() > 10, "{}: say why", u["script"]);
        claim("uncovered", codes(&u["languages"]), &|_| true);
    }
    seen.sort();
    let mut every: Vec<String> = LANGUAGES.iter().map(|l| l.code.to_string()).collect();
    every.sort();
    assert_eq!(seen, every, "a language in no bucket, or in two");
    // A script left uncovered stays so: no shipped face carries a codepoint of Yi, Avestan or
    // Mongolian (a face that gains one means the row is stale).
    for cp in [0xA288u32, 0x10B00, 0x1820] {
        assert!(!inter.contains(cp), "U+{cp:04X}");
        assert!(faces().iter().all(|f| !subs_cov(f).contains(cp)), "a face covers U+{cp:04X}; update `uncovered`");
    }
}
