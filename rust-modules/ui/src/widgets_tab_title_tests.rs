//! A library's own title on the Other Videos pill: the cleaning, the budget the strip leaves it,
//! the refusals (a collision with another pill, no room) and the elision. Widths are the shipped
//! bold face's whole-pixel advances (`ShippedMeasure`), the model the budget is derived from, not a
//! television measurement; `FixtureMeasure` (14 px a byte at this size) builds the synthetic strips
//! whose room is chosen by hand.

use super::*;
use crate::fixture::FixtureMeasure;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, SHIPPED};

const ELLIPSIS: char = '\u{2026}';
/// Where the Other Videos pill sits in the strip a household with all three types draws.
const VIDEO_AT: usize = 3;

/// The strip of a household with all three types in the current UI language: Home, Movies,
/// TV Shows, the Other Videos pill (`video`), the Watchlist mark and Search.
fn strip(video: &str) -> Vec<TabFace> {
    vec![
        TabFace::Label(msg::browse_chrome_home().into()),
        TabFace::Label(msg::browse_kind_movies().into()),
        TabFace::Label(msg::browse_kind_tv_shows().into()),
        TabFace::Label(video.into()),
        TabFace::Icon(crate::icons::Icon::Bookmark),
        TabFace::Icon(crate::icons::Icon::Search),
    ]
}

fn translated_strip() -> Vec<TabFace> {
    strip(msg::browse_kind_other_videos())
}

fn text_of(face: &TabFace) -> &str {
    match face {
        TabFace::Label(text) => text,
        TabFace::Icon(icon) => panic!("a mark, not a label: {icon:?}"),
    }
}

use plx_machine::machine::Measure;

fn text_w(text: &str) -> f32 {
    ShippedMeasure.width_str(text, theme::size::BODY, true)
}

/// What the other pills of `faces` take, as `fit_title_face` counts them.
fn others_w(faces: &[TabFace], index: usize, measure: &dyn plx_machine::machine::Measure) -> f32 {
    tab_widths(faces, measure).iter().enumerate().filter(|&(i, _)| i != index).map(|(_, w)| *w).sum()
}

#[test]
fn the_room_a_title_may_take_is_derived_from_the_chip_room() {
    assert_eq!(TAB_VIEW_FOR_ROOM, 912.0, "1920 - 2 * (480 + 16 + 8): the capsule of a 270 px name, the air, the pad");
    // the same width, asked of the chip's own function: a track of this content leaves the room
    assert_eq!(chip_name_room(TAB_VIEW_FOR_ROOM + 2.0 * TAB_TRACK_PAD), TAB_TITLE_ROOM);
    // raw 247 is capped, then it falls one px per px, then the floor, then nothing
    assert_eq!(title_budget(549.0, 6), Some(TAB_TITLE_MAX));
    assert_eq!(title_budget(586.0, 6), Some(210.0));
    assert_eq!(title_budget(736.0, 6), Some(TAB_TITLE_FLOOR), "exactly the floor is usable");
    assert_eq!(title_budget(736.5, 6), None, "half a pixel under the floor is not");
    for hopeless in [760.0, 912.0, 5000.0] {
        assert_eq!(title_budget(hopeless, 6), None, "{hopeless}: never a non-positive budget");
    }
    assert_eq!(title_budget(0.0, 1), Some(TAB_TITLE_MAX), "a lone pill still has the cap");
}

/// The table of the addendum, per shipped language: the text room the other pills leave (the
/// translated Other Videos label is not one of them), and that the translated label itself always
/// fits it, so the type word is never elided.
#[test]
fn fit_title_face_budget_per_language() {
    let expected = [("en", 240.0), ("de", 240.0), ("es", 240.0), ("fr", 240.0), ("it", 240.0),
        ("pt", 240.0), ("hu", 217.0), ("ru", 210.0), ("be", 197.0), ("pl", 240.0), ("nl", 240.0)];
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let faces = translated_strip();
        let others = others_w(&faces, VIDEO_AT, &ShippedMeasure);
        let budget = title_budget(others, faces.len()).unwrap_or_else(|| panic!("{tag}: no room at all"));
        let want = expected.iter().find(|(t, _)| *t == tag).unwrap_or_else(|| panic!("{tag} is not in the table")).1;
        assert_eq!(budget, want, "{tag}: others take {others}");
        let word = msg::browse_kind_other_videos();
        assert!(text_w(word) <= budget, "{tag}: the type word {word:?} is {} px of a {budget} px room", text_w(word));
    }
    assert_eq!(expected.len(), SHIPPED.len(), "every shipped language is in the table");
}

/// A 197 px title (German's own "Andere Videos") is whole in every shipped language: Belarusian, the
/// tightest, leaves exactly 197 px, the cost of keeping the chip's [`TAB_TITLE_ROOM`].
#[test]
fn a_197_px_title_fits_every_language() {
    let title = "Andere Videos";
    assert_eq!(text_w(title), 197.0, "the figure the addendum's table is about");
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let mut faces = translated_strip();
        assert!(fit_title_face(&mut faces, VIDEO_AT, title, &ShippedMeasure), "{tag}");
        assert_eq!(text_of(&faces[VIDEO_AT]), title, "{tag}: whole");
    }
}

/// Whatever the title, a pill that used it leaves the chip its room, in every language and the
/// expanded pseudo-locale (whose type words are far longer than any shipped one).
#[test]
fn a_used_title_never_takes_the_chip_room() {
    let long = "A very long family library name, 2019-2024";
    for title in [long, "Clips", "Andere Videos", &long.repeat(4)] {
        let mut languages: Vec<(String, plx_platform::i18n::ThreadLocaleGuard)> = Vec::new();
        for language in SHIPPED {
            languages.push((language.tag().to_owned(), language_on_this_thread_for_test(language)));
            check_room(title, &languages.last().unwrap().0);
            languages.pop();
        }
        let _pseudo = plx_platform::i18n::pseudo_on_this_thread_for_test();
        check_room(title, "qps-ploc");
    }
}

fn check_room(title: &str, tag: &str) {
    let mut faces = translated_strip();
    let used = fit_title_face(&mut faces, VIDEO_AT, title, &ShippedMeasure);
    let widths = tab_widths(&faces, &ShippedMeasure);
    let before = tab_widths(&translated_strip(), &ShippedMeasure);
    if used {
        assert!(tab_content_w(&widths) <= TAB_VIEW_FOR_ROOM,
            "{tag}: {title:?} makes the strip {} wide, past {TAB_VIEW_FOR_ROOM}", tab_content_w(&widths));
    } else {
        assert_eq!(widths, before, "{tag}: refused, the strip is the translated one");
    }
}

#[test]
fn a_title_budget_below_the_floor_uses_the_generic_label() {
    // 14 px a byte: a pill of n bytes is 14n + 36, and the strip below is [other, video], one gap.
    // raw = 912 - (14a + 36) - 16 - 36 = 824 - 14a: a = 54 leaves 68 px (a usable room), 55 leaves 54.
    let measure = FixtureMeasure;
    let cramped = |a: usize| vec![TabFace::Label("x".repeat(a)), TabFace::Label("Other Videos".into())];
    let mut faces = cramped(55);
    assert!(!fit_title_face(&mut faces, 1, "Holiday Clips", &measure), "54 px is under the floor");
    assert_eq!(faces, cramped(55), "…and the face keeps the type word, whole");

    let mut faces = cramped(54);
    assert!(fit_title_face(&mut faces, 1, "Holiday Clips", &measure), "a room over the floor is used");
    let face = text_of(&faces[1]);
    assert!(face.ends_with(ELLIPSIS) && measure.width_str(face, theme::size::BODY, true) <= 68.0,
        "the title is cut to its 68 px room: {face:?}");
}

#[test]
fn a_non_positive_budget_never_returns_the_unelided_title() {
    // elide_by answers its input unchanged for a budget <= 0; 60 bytes of other pill leave -24 px.
    let measure = FixtureMeasure;
    let mut faces = vec![TabFace::Label("x".repeat(60)), TabFace::Label("Other Videos".into())];
    let before = faces.clone();
    assert_eq!(title_budget(others_w(&faces, 1, &measure), 2), None);
    assert!(!fit_title_face(&mut faces, 1, "A title that is not short at all", &measure));
    assert_eq!(faces, before, "the generic label stays and the title is not drawn whole");
    // …and the trap itself, so the guard is known to be needed
    assert_eq!(plx_gfx::text::elide_by("A title", -24.0, false, |t| t.len() as f32), "A title");
}

#[test]
fn elide_keeps_a_base_letter_when_cut_before_a_combining_mark() {
    // 14 px a byte: "e" + U+0301 is three bytes (42 px). In a 68 px room "e…" (56) fits and
    // "é…" (84) does not, so the cut falls between the letter and its mark.
    let measure = FixtureMeasure;
    let mut faces = vec![TabFace::Label("x".repeat(54)), TabFace::Label("Other Videos".into())];
    assert!(fit_title_face(&mut faces, 1, "e\u{301}e\u{301}e\u{301}e\u{301}", &measure));
    assert_eq!(text_of(&faces[1]), "e\u{2026}", "a plain base letter and the ellipsis, never a broken byte sequence");
    // a 3-byte CJK scalar and a 4-byte emoji are cut whole or not at all
    for title in ["家族のビデオ家族のビデオ", "\u{1F468}\u{200D}\u{1F469}\u{1F468}\u{1F469}\u{1F467}\u{1F466}"] {
        let mut faces = vec![TabFace::Label("x".repeat(54)), TabFace::Label("Other Videos".into())];
        let used = fit_title_face(&mut faces, 1, title, &measure);
        assert!(!used || text_of(&faces[1]).chars().all(|c| c != char::REPLACEMENT_CHARACTER));
    }
}

#[test]
fn the_title_is_cleaned_before_it_is_measured() {
    assert_eq!(clean_title("  Home \t\n Movies  "), "Home Movies");
    assert_eq!(clean_title("Home\u{0}Movies"), "Home Movies", "a NUL is a space (CString would refuse it)");
    assert_eq!(clean_title("a\u{200B}b\u{200C}c\u{200D}d\u{200E}e\u{200F}f"), "abcdef");
    assert_eq!(clean_title("\u{202A}a\u{202B}b\u{202C}c\u{202D}d\u{202E}e"), "abcde");
    assert_eq!(clean_title("\u{2060}a\u{2064}b\u{2066}c\u{2069}d\u{206F}e\u{FEFF}f\u{AD}g"), "abcdefg");
    assert_eq!(clean_title("\u{200B}\u{202E}\u{7}\n"), "", "nothing visible");
    assert_eq!(clean_title("tab\u{A0}separated"), "tab separated", "a no-break space is a space");
    let long = "x".repeat(500);
    assert_eq!(clean_title(&long).chars().count(), TAB_TITLE_CHARS, "a hostile name is bounded before it is measured");
    assert_eq!(clean_title(&format!("{} {}", "x".repeat(127), "y")), "x".repeat(127), "…and a trailing space is not kept");
}

/// D9: a title equal to another pill's label is two identical pills. The comparison is Unicode
/// lowercase, so a capitalised Cyrillic word collides; an ASCII-only fold would let it through.
#[test]
fn the_collision_test_uses_unicode_lowercase() {
    let mut faces = vec![TabFace::Label("Главная".into()), TabFace::Label("Фильмы".into()),
        TabFace::Label("Другие видео".into())];
    let before = faces.clone();
    for title in ["ФИЛЬМЫ", "фильмы", "  Фильмы ", "ФИЛЬМЫ\u{200B}", "главная"] {
        assert!(!fit_title_face(&mut faces, 2, title, &ShippedMeasure), "{title:?}");
        assert_eq!(faces, before);
    }
    assert!("ФИЛЬМЫ".to_ascii_lowercase() != "Фильмы".to_ascii_lowercase(), "ASCII folding sees no collision");
    assert!(fit_title_face(&mut faces, 2, "Фильмы 2", &ShippedMeasure), "an equal label collides, a longer one does not");
    assert_eq!(text_of(&faces[2]), "Фильмы 2");
    // the Other Videos pill itself is not "another pill"
    let mut faces = before.clone();
    assert!(fit_title_face(&mut faces, 2, "ДРУГИЕ ВИДЕО", &ShippedMeasure));
}
