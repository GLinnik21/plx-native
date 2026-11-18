use super::super::synth_fonts::{self as synth, NameRecord};
use super::*;
use std::sync::Arc;

fn shipped(file: &str) -> Vec<u8> {
    std::fs::read(super::super::asset_dir().join(file)).expect("a shipped face in pkg")
}

/// The folded names of one face, sorted, for an order-free comparison.
fn names(face: &Face) -> Vec<&str> {
    let mut all: Vec<&str> = face.names.iter().map(|n| std::str::from_utf8(n).unwrap()).collect();
    all.sort_unstable();
    all
}

fn font(file: &str, family: &str, style: &str) -> Font {
    // A tiny CFF face: nothing here is drawn, only named.
    let ps = format!("{}-{style}", family.replace(' ', ""));
    Font { name: file.into(), data: Arc::from(synth::cff_font(&synth::windows_names(family, style), &ps)) }
}

fn garbage(file: &str, fill: u8) -> Font {
    Font { name: file.into(), data: Arc::from(vec![fill; 64]) }
}

const STYLES: &str = "[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Zed Sans,24\n\n[Events]\n";

fn header_naming(fontname: &str) -> Vec<u8> {
    format!("[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,{fontname},24\n\n[Events]\n").into_bytes()
}

// ---- the name-table reader --------------------------------------------------------------------

#[test]
fn a_shipped_face_answers_to_its_family_full_and_postscript_names() {
    let faces = faces(&shipped("appfont.ttf")).expect("Inter parses");
    assert_eq!(faces.len(), 1);
    assert_eq!(names(&faces[0]), ["inter", "inter regular", "inter-regular"]);
    assert_eq!(faces[0].families, [b"inter".to_vec()]);
}

#[test]
fn a_face_with_two_families_answers_to_both() {
    // Inter Bold also answers to "PlxSubs" (a Windows record under another language id).
    let faces = faces(&shipped("appfont-bold.ttf")).unwrap();
    let mut families = faces[0].families.clone();
    families.sort();
    assert_eq!(families, [b"inter".to_vec(), b"plxsubs".to_vec()]);
    assert!(names(&faces[0]).contains(&"inter bold"));
}

#[test]
fn a_renamed_real_face_is_named_by_its_new_family_and_nothing_old() {
    let renamed = synth::rename(&shipped("appfont.ttf"), "Plx Karaoke", "Bold", None);
    let faces = faces(&renamed).unwrap();
    assert_eq!(names(&faces[0]), ["plx karaoke", "plx karaoke bold", "plxkaraoke-bold"]);
}

#[test]
fn a_collection_names_every_face() {
    let one = synth::rename(&shipped("appfont.ttf"), "Plx One", "Regular", None);
    let two = synth::rename(&shipped("appfont.ttf"), "Plx Two", "Regular", None);
    let faces = faces(&synth::collection(&[one, two])).expect("a TTC parses");
    assert_eq!(faces.len(), 2);
    assert_eq!(faces[0].families, [b"plx one".to_vec()]);
    assert_eq!(faces[1].families, [b"plx two".to_vec()]);
}

#[test]
fn a_collection_with_one_unreadable_face_is_unknown() {
    let one = synth::rename(&shipped("appfont.ttf"), "Plx One", "Regular", None);
    let mut broken = synth::collection(&[one.clone(), one]);
    broken[16..20].copy_from_slice(&0xFFFF_FF00u32.to_be_bytes());
    assert_eq!(faces(&broken), None, "a face whose offset table is off the file cannot be named");
}

#[test]
fn a_mac_only_name_table_is_read_as_latin_1() {
    let records: Vec<NameRecord> = vec![
        (1, 0, 1, b"Mac Family".to_vec()),
        (1, 0, 4, b"Mac Family Bold".to_vec()),
        (1, 0, 6, b"MacFamily-Bold".to_vec()),
    ];
    let face = &faces(&synth::cff_font(&records, "Zzz")).unwrap()[0];
    assert_eq!(names(face), ["mac family", "mac family bold", "macfamily-bold", "zzz"]);
}

#[test]
fn a_face_with_no_family_record_answers_to_its_typographic_family() {
    // FreeType's family_name fallback reads ID 16 and 21.
    let bare: Vec<NameRecord> = vec![
        (3, 1, 4, synth::utf16("Only Full")),
        (3, 1, 16, synth::utf16("Typo Family")),
        (3, 1, 21, synth::utf16("Wws Family")),
    ];
    let face = &faces(&synth::cff_font(&bare, "Cff Name")).unwrap()[0];
    assert_eq!(names(face), ["cff name", "only full", "typo family", "wws family"]);
    assert_eq!(face.families, [b"typo family".to_vec(), b"wws family".to_vec()]);
}

#[test]
fn the_typographic_family_is_a_name_even_beside_a_family_record() {
    // libass counts only a Microsoft-platform ID 1, so a face whose ID 1 sits on another platform
    // is named by FreeType's family_name, which prefers ID 16: ID 16 and 21 are always names.
    let with_family: Vec<NameRecord> = vec![(3, 1, 1, synth::utf16("Fam")), (3, 1, 16, synth::utf16("Typo"))];
    let face = &faces(&synth::cff_font(&with_family, "Cff")).unwrap()[0];
    assert_eq!(names(face), ["cff", "fam", "typo"]);
    assert_eq!(face.families, [b"fam".to_vec(), b"typo".to_vec()]);
}

#[test]
fn a_style_naming_the_typographic_family_selects_a_face_whose_only_id_1_is_a_mac_record() {
    // The Mac-platform ID 1 "Foo" is invisible to libass, which then falls back to FreeType's
    // family_name: the preferred family, ID 16 "Foo Pro". A style naming "Foo Pro" must select it.
    let records: Vec<NameRecord> = vec![(1, 0, 1, b"Foo".to_vec()), (1, 0, 16, b"Foo Pro".to_vec())];
    let fonts = [
        Font { name: "foo.otf".into(), data: Arc::from(synth::cff_font(&records, "Zzz")) },
        font("b.otf", "Unrelated", "Regular"),
    ];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(&header_naming("Foo Pro")), [0]);
    let mut by_mac_family = Selection::new(&fonts);
    assert_eq!(by_mac_family.seed(&header_naming("Foo")), [0], "the Mac family stays a name too");
}

#[test]
fn the_cff_font_name_is_a_postscript_name() {
    let records: Vec<NameRecord> = vec![(3, 1, 1, synth::utf16("Fam"))];
    let face = &faces(&synth::cff_font(&records, "Fam-Heavy")).unwrap()[0];
    assert_eq!(names(face), ["fam", "fam-heavy"]);
}

#[test]
fn names_are_ascii_folded_only_and_trimmed() {
    assert_eq!(fold(b"  @Arial Black \t").unwrap(), b"arial black");
    assert_eq!(fold("ÜNÏ".as_bytes()).unwrap(), "ÜnÏ".as_bytes(), "only ASCII letters fold, as ass_strcasecmp does");
    assert_eq!(fold(b"@"), None);
    assert_eq!(fold(b"  "), None);
    assert_eq!(fold(b"0"), None, "\\fn0 is the style's own font");
    assert_eq!(fold(&vec![b'a'; 5000]).unwrap().len(), NAME_RECORD_BYTES);
}

#[test]
fn utf16_names_with_non_bmp_characters_are_decoded() {
    let records: Vec<NameRecord> = vec![(3, 10, 1, synth::utf16("Font \u{1F600} Name"))];
    let face = &faces(&synth::cff_font(&records, "N")).unwrap()[0];
    assert!(names(face).contains(&"font \u{1F600} name"));
}

#[test]
fn bytes_that_are_not_a_font_or_are_cut_short_never_panic_and_name_nothing() {
    assert_eq!(faces(b""), None);
    assert_eq!(faces(b"abc"), None);
    assert_eq!(faces(&[0u8; 64]), None);
    assert_eq!(faces(&vec![0xFFu8; 4096]), None);
    let real = shipped("appfont.ttf");
    for cut in (0..200).chain([1000, 4000]) {
        let _ = faces(&real[..cut]);
    }
    // Cut inside the name table: its records run past the end of the file.
    let tiny = synth::cff_font(&synth::windows_names("Fam", "Regular"), "Fam-Regular");
    for cut in 0..tiny.len() {
        let _ = faces(&tiny[..cut]);
    }
    assert!(faces(&tiny).is_some());
    assert_eq!(faces(&tiny[..tiny.len() - 30]), None, "a truncated name table is unknown, not partly read");
}

#[test]
fn a_font_with_a_corrupted_byte_anywhere_never_panics() {
    let tiny = synth::cff_font(&synth::windows_names("Fam", "Regular"), "Fam-Regular");
    for at in 0..tiny.len() {
        for value in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
            let mut bytes = tiny.clone();
            bytes[at] = value;
            let _ = faces(&bytes);
        }
    }
    let ttc = synth::collection(&[tiny.clone(), tiny]);
    for at in 0..64 {
        for value in [0x00, 0xFF, 0x80] {
            let mut bytes = ttc.clone();
            bytes[at] = value;
            let _ = faces(&bytes);
        }
    }
    // A collection claiming four billion faces reads only the offsets that exist.
    let mut huge = ttc;
    huge[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    let _ = faces(&huge);
}

// ---- the names a track asks for -----------------------------------------------------------------

fn texts(found: Vec<Vec<u8>>) -> Vec<String> {
    found.into_iter().map(|n| String::from_utf8(n).unwrap()).collect()
}

#[test]
fn a_style_line_names_the_fontname_column_the_format_line_gives() {
    let header = "[V4+ Styles]\nFormat: Name, Fontsize, Fontname, Bold\nStyle: A,20,Gothic One,0\nStyle: B,20,@Vertical Mincho,0\n";
    assert_eq!(texts(requested(header.as_bytes(), true)), ["gothic one", "vertical mincho"]);
    let sloppy = "[V4 Styles]\nStyle: Default,Old Style,16,&H00FFFFFF\r\n";
    assert_eq!(texts(requested(sloppy.as_bytes(), true)), ["old style"], "no Format line: column 1");
    let other = "[V4+ Styles]\nFormat: Fontname, Name\nStyle: Zed Font,Default\n";
    assert_eq!(texts(requested(other.as_bytes(), true)), ["zed font"], "Fontname in the first column");
}

#[test]
fn only_a_styles_section_has_style_lines() {
    let script = "[Events]\nFormat: Layer, Start\nStyle: Nope,Not A Font\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: S,A Font\n";
    assert_eq!(texts(requested(script.as_bytes(), true)), ["a font"]);
}

#[test]
fn fn_overrides_are_read_in_a_header_a_script_and_an_event() {
    let event = br"0,0,Default,,0,0,0,,{\an8\fn Karaoke Sans \b1\fn@Tall One}text\Nmore{\fn(Paren Font)}";
    assert_eq!(texts(requested(event, false)), ["karaoke sans", "tall one", "(paren font)", "paren font"]);
    let dialogue = b"[Events]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,{\\fnScript Face}x\n";
    assert_eq!(texts(requested(dialogue, true)), ["script face"]);
}

#[test]
fn a_fn_with_a_parenthesis_keeps_both_readings() {
    // libass ends the name at a `(`; a block that spells it into the name keeps it. Both are asked.
    assert_eq!(texts(requested(br"{\fnNoto Sans (Bold)}", false)), ["noto sans (bold)", "noto sans", "bold"]);
}

#[test]
fn a_fn_inside_a_t_tag_ends_at_the_tags_closing_parenthesis() {
    // libass hands the nested parse the end of the `\t` argument, so the name stops at its `)`.
    let asked = texts(requested(br"{\t(0,500,\fnFoo)}text", false));
    assert!(asked.contains(&"foo".to_string()), "{asked:?}");
    let asked = texts(requested(br"0,0,Default,,0,0,0,,{\t(\fn Bar Sans )\b1}x", false));
    assert!(asked.contains(&"bar sans".to_string()), "{asked:?}");
    // A tail after the `)`, another tag inside, and a second `\t` in the same block.
    let asked = texts(requested(br"{\t(0,500,1,\fnFoo\fs20)\t(0,9,\fnBaz)\bord2}", false));
    assert!(asked.contains(&"foo".to_string()) && asked.contains(&"baz".to_string()), "{asked:?}");
    // An unclosed `\t(` reads to the end of its block, as before.
    let asked = texts(requested(br"{\t(0,500,\fnQux}", false));
    assert!(asked.contains(&"qux".to_string()), "{asked:?}");
    // Selection: the face the nested `\fn` names is added when its event arrives.
    let fonts = [font("a.otf", "Foo", "Regular"), font("b.otf", "Other", "Regular")];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.wanted(br"1,0,Default,,0,0,0,,{\t(0,500,\fnFoo)}x"), [0]);
}

#[test]
fn fn0_an_empty_fn_and_other_tags_ask_for_nothing() {
    assert_eq!(texts(requested(br"{\fscx90\fsp2\frz4\fad(1,2)}{\f}{\fr10}", false)), Vec::<String>::new());
    assert_eq!(texts(requested(br"{\fn0}{\fn}{\fs20\fscx90}", false)), Vec::<String>::new());
    // A backslash that ends the text, and a tag cut off at the end.
    assert_eq!(texts(requested(b"{\\fn", false)), Vec::<String>::new());
    assert_eq!(texts(requested(b"\\", false)), Vec::<String>::new());
    assert_eq!(texts(requested(b"{\\ fnSpaced}", false)), ["spaced"], "libass skips spaces after the backslash");
}

#[test]
fn the_fonts_and_graphics_sections_are_not_read_for_tags() {
    // uuencoded data holds backslashes; a `\fn` in it is not a tag.
    let script = "[Fonts]\nfontname: a.ttf\n!!\\fnNot A Tag!!\n\n[Graphics]\n\\fnAlso Not\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: S,Real\n";
    assert_eq!(texts(requested(script.as_bytes(), true)), ["real"]);
}

// ---- the selection -------------------------------------------------------------------------------

#[test]
fn a_style_selects_the_face_it_names_and_no_other() {
    let fonts = [font("a.otf", "Zed Sans", "Regular"), font("b.otf", "Other One", "Regular"), font("c.otf", "Third", "Bold")];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(STYLES.as_bytes()), [0]);
    assert_eq!(selection.seed(STYLES.as_bytes()), Vec::<usize>::new(), "once held, never added twice");
}

#[test]
fn inter_and_arial_are_always_requested() {
    let fonts = [font("a.otf", "Other", "Regular"), font("i.otf", "Inter", "Regular"), font("r.otf", "ARIAL", "Bold")];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(&header_naming("Nothing Matches")), [1, 2]);
}

#[test]
fn a_full_name_or_postscript_name_selects_its_face() {
    let fonts = [font("a.otf", "Fam", "Heavy"), font("b.otf", "Unrelated", "Regular")];
    let mut by_full = Selection::new(&fonts);
    assert_eq!(by_full.seed(&header_naming("fam heavy")), [0]);
    let mut by_postscript = Selection::new(&fonts);
    assert_eq!(by_postscript.seed(&header_naming("Fam-Heavy")), [0]);
}

#[test]
fn a_matched_face_brings_every_member_of_its_family_in_container_order() {
    let fonts = [
        font("regular.otf", "Fam", "Regular"),
        font("other.otf", "Other", "Regular"),
        font("bold.otf", "Fam", "Bold"),
        font("italic.otf", "Fam", "Italic"),
    ];
    let mut by_family = Selection::new(&fonts);
    assert_eq!(by_family.seed(&header_naming("FAM")), [0, 2, 3]);
    // Naming the Bold face by its full name still brings the whole family: libass scores its
    // members by weight and slant, and an add-order tie break that needs them all present.
    let mut by_full = Selection::new(&fonts);
    assert_eq!(by_full.seed(&header_naming("Fam Bold")), [0, 2, 3]);
}

#[test]
fn a_late_fn_adds_only_what_is_new_and_a_repeat_adds_nothing() {
    let fonts = [font("a.otf", "Alpha", "Regular"), font("b.otf", "Beta", "Regular"), font("c.otf", "Gamma", "Regular")];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(&header_naming("Alpha")), [0]);
    assert_eq!(selection.wanted(br"1,0,Default,,0,0,0,,{\fn@BETA}x"), [1]);
    assert_eq!(selection.wanted(br"2,0,Default,,0,0,0,,{\fnBeta\fnAlpha}y"), Vec::<usize>::new());
    assert_eq!(selection.wanted(br"3,0,Default,,0,0,0,,{\fnGamma\fnBeta}z"), [2]);
    assert_eq!(selection.wanted(b"4,0,Default,,0,0,0,,no override"), Vec::<usize>::new());
    assert_eq!(selection.wanted(br"5,0,Default,,0,0,0,,{\fnNot Attached}"), Vec::<usize>::new());
}

#[test]
fn a_byte_identical_copy_is_added_once_and_the_first_is_kept() {
    let first = font("first.otf", "Twin", "Regular");
    let copy = Font { name: "copy.otf".into(), data: first.data.clone() };
    let again = Font { name: "again.otf".into(), data: Arc::from(first.data.to_vec()) };
    let fonts = [first, copy, again, font("x.otf", "Twin", "Bold")];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(&header_naming("Twin")), [0, 3], "the copies never reach libass");
}

#[test]
fn a_face_that_cannot_be_read_is_wanted_once_at_the_first_seed() {
    let fonts = [garbage("a.pfb", 7), font("b.otf", "Named", "Regular"), garbage("c.pfb", 7), garbage("d.pfb", 9)];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(&header_naming("Elsewhere")), [0, 3], "unknown means wanted; the copy of a.pfb is not");
    assert_eq!(selection.seed(&header_naming("Named")), [1]);
    assert_eq!(selection.seed(&header_naming("Named")), Vec::<usize>::new());
}

#[test]
fn an_empty_list_selects_nothing_and_a_script_with_no_fonts_does_not_fail() {
    let mut selection = Selection::new(&[]);
    assert_eq!(selection.seed(b"[Script Info]\n"), Vec::<usize>::new());
    assert_eq!(selection.wanted(br"{\fnX}"), Vec::<usize>::new());
}

#[test]
fn a_collection_is_wanted_when_any_of_its_faces_is_named() {
    let one = synth::rename(&shipped("appfont.ttf"), "Plx One", "Regular", None);
    let two = synth::rename(&shipped("appfont.ttf"), "Plx Two", "Regular", None);
    let fonts = [
        Font { name: "pair.ttc".into(), data: Arc::from(synth::collection(&[one, two])) },
        font("x.otf", "Unrelated", "Regular"),
    ];
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(&header_naming("Plx Two")), [0]);
}

/// Issue #1328 in the report's own shape: seventeen attachments whose first twelve have no style
/// naming them, and five, last in the container, that the opening's karaoke styles do. The names
/// are the report's file names; the families are what the styles ask for.
#[test]
fn the_five_faces_a_karaoke_opening_names_are_selected_from_seventeen() {
    let families = [
        "Filler 0", "Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5", "Filler 6", "Filler 7",
        "Filler 8", "Filler 9", "Filler 10", "Filler 11", "Kiza Kinryou", "Mastro Sans", "Roboto", "Stylus", "Lehn",
    ];
    let fonts: Vec<Font> = families.iter().enumerate().map(|(i, f)| font(&format!("{i}.otf"), f, "Regular")).collect();
    let header = "[V4+ Styles]\nFormat: Name, Fontname\nStyle: Sign,Kiza Kinryou\nStyle: Op1,Mastro Sans\nStyle: Op2,Roboto\nStyle: Op3,Stylus\nStyle: Op4,Lehn\nStyle: Default,Arial\n";
    let mut selection = Selection::new(&fonts);
    assert_eq!(selection.seed(header.as_bytes()), [12, 13, 14, 15, 16]);
}
