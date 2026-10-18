//! T1: the Collections page's own rules: Sort visibility, the section-sync guard, the crumb's
//! geometry and focus order, the tile caption (Belarusian plurals 1/2/5/21), the tile grid.
use super::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{self, language_on_this_thread_for_test, Language, Preference};

fn films(n: i64) -> TileFacts<'static> {
    TileFacts { title: "Starfall Saga", subtype: Some("movie"), child_count: n, smart: false }
}

fn lines(tag: &str, stored: CaptionPolicy, f: &TileFacts<'_>) -> [Option<String>; 2] {
    let _g = language_on_this_thread_for_test(Preference::from_tag(tag));
    caption(stored, f, i18n::current())
}

#[test]
fn sort_is_hidden_unless_the_server_declares_two_or_more() {
    assert!(!sort_visible(0));
    assert!(!sort_visible(1), "PMS declares only titleSort for type=18");
    assert!(sort_visible(2));
    assert!(sort_visible(5));
    let m = ShippedMeasure;
    assert!(header(SecKind::Movie, 1, "Title", 1920.0, &m).sort.is_none());
    let h = header(SecKind::Movie, 2, "Title", 1920.0, &m);
    assert!(h.sort.is_some());
    assert_eq!(h.stops(), [Stop::Root, Stop::Sort]);
    assert_eq!(header(SecKind::Movie, 1, "Title", 1920.0, &m).stops(), [Stop::Root], "the root link is the only stop");
}

#[test]
fn the_sort_control_ends_at_the_header_row_edge() {
    let h = header(SecKind::Movie, 2, "Title", 1920.0, &ShippedMeasure);
    let sort = h.sort.unwrap();
    assert!(sort.focusable);
    // Text edge 1771 less the fold glyph (10 gap + 22): the run's own end is the row's edge.
    assert!((sort.text.x + sort.text.w - 1771.0).abs() < 0.01);
}

#[test]
fn the_section_sync_does_not_run_on_the_collections_page() {
    assert!(syncs_section(Listing::All));
    assert!(syncs_section(Listing::Recommended));
    assert!(!syncs_section(Listing::Collections));
}

#[test]
fn the_crumb_reads_films_then_collections_and_shows_for_a_show_library() {
    let _g = language_on_this_thread_for_test(Preference::from_tag("en"));
    let m = ShippedMeasure;
    let h = header(SecKind::Movie, 1, "Title", 1920.0, &m);
    assert_eq!((h.crumb.root.as_str(), h.crumb.page.as_str()), ("Films", "Collections"));
    assert_eq!(header(SecKind::Show, 1, "Title", 1920.0, &m).crumb.root, "Shows");
    assert_eq!(h.crumb_layout.root_text.x, 96.0);
    assert!(h.crumb_layout.page_plate.x > h.crumb_layout.root_plate.x + h.crumb_layout.root_plate.w - 8.0);
}

#[test]
fn the_crumb_and_the_sort_control_never_meet_in_any_shipped_language() {
    let m = ShippedMeasure;
    let mut bad = Vec::new();
    for l in Language::SHIPPED {
        let _g = language_on_this_thread_for_test(Preference::from_tag(l.tag()));
        for kind in [SecKind::Movie, SecKind::Show] {
            let h = header(kind, 2, "Title added (newest first)", 1920.0, &m);
            let gap = h.sort.unwrap().plate.x - h.crumb_layout.right();
            if gap < page_header::CLUSTER_GAP { bad.push(format!("{}: {gap:.0}px between crumb and Sort", l.tag())); }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn the_grid_geometry_is_the_librarys() {
    assert_eq!(GRID_TOP + TILE_H + 40.0, ROW2_TOP);
    assert_eq!(TILE_W + 35.0, TILE_PITCH);
    assert_eq!(GRID_TOP, super::super::layout::HEADER_Y + super::super::layout::HEADER_BLOCK_H);
}

#[test]
fn the_default_caption_is_the_count_alone_and_title_on_puts_the_name_above() {
    let en = "en";
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(24)), [None, Some("24 films".into())]);
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(1)), [None, Some("1 film".into())]);
    let titled = CaptionPolicy { title: true, details: DetailsKind::YearLength };
    assert_eq!(lines(en, titled, &films(24)), [Some("Starfall Saga".into()), Some("24 films".into())]);
    let shows = TileFacts { subtype: Some("show"), ..films(3) };
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &shows)[1].as_deref(), Some("3 shows"));
    let other = TileFacts { subtype: None, ..films(3) };
    assert!(lines(en, CaptionPolicy::DEFAULT, &other)[1].is_some(), "an unknown subtype still counts");
}

#[test]
fn a_film_describing_details_choice_falls_back_to_the_count_and_none_stays_none() {
    let en = "en";
    for d in [DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore] {
        let p = CaptionPolicy { title: false, details: d };
        assert_eq!(lines(en, p, &films(4)), [None, Some("4 films".into())], "{d:?}");
    }
    let none = CaptionPolicy { title: true, details: DetailsKind::None };
    assert_eq!(lines(en, none, &films(4)), [Some("Starfall Saga".into()), None]);
    assert_eq!(policy(CaptionPolicy { title: false, details: DetailsKind::None }).max_lines(), 0);
}

#[test]
fn an_unknown_count_leaves_no_count_line() {
    let en = "en";
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(0)), [None, None]);
}

#[test]
fn belarusian_counts_take_the_right_plural_for_1_2_5_21() {
    let be = "be";
    let line = |n| lines(be, CaptionPolicy::DEFAULT, &films(n))[1].clone().unwrap();
    let (one, few, many, twenty_one) = (line(1), line(2), line(5), line(21));
    assert!(one.starts_with("1 "), "{one}");
    assert_ne!(one, many);
    assert_ne!(few, many, "few ({few}) is not many ({many})");
    assert_ne!(few, one);
    assert_eq!(twenty_one.split_once(' ').unwrap().1, one.split_once(' ').unwrap().1, "21 takes the 'one' form");
    let shows = TileFacts { subtype: Some("show"), ..films(2) };
    assert!(lines(be, CaptionPolicy::DEFAULT, &shows)[1].as_deref().is_some_and(|s| s.starts_with("2 ")));
}

#[test]
fn every_shipped_language_words_the_count_without_english() {
    let en = lines("en", CaptionPolicy::DEFAULT, &films(7))[1].clone().unwrap();
    for l in Language::SHIPPED {
        if l.tag() == "en" { continue; }
        let s = lines(l.tag(), CaptionPolicy::DEFAULT, &films(7))[1].clone().unwrap();
        assert!(s.starts_with("7 ") || s.contains('7'), "{}: {s}", l.tag());
        // French spells the plural of "film" as English does.
        if l.tag() != "fr" { assert_ne!(s, en, "{}: caption is English", l.tag()); }
    }
}

#[test]
fn the_empty_sentence_and_smart_label_exist_in_every_language_and_fit_a_centred_line() {
    let m = ShippedMeasure;
    let en = { let _g = language_on_this_thread_for_test(Preference::from_tag("en")); (empty_text("Films"), smart_label().to_owned()) };
    for l in Language::SHIPPED {
        let _g = language_on_this_thread_for_test(Preference::from_tag(l.tag()));
        let e = empty_text("Documentaries & Concerts");
        assert!(e.contains("Documentaries & Concerts"), "{}: {e}", l.tag());
        let w = std::ffi::CString::new(e.clone()).map_or(0.0, |c| m.width(&c, plx_ui::theme::size::BODY, false));
        assert!(w < 1200.0, "{}: {e} is {w}px", l.tag());
        if l.tag() != "en" {
            assert_ne!(empty_text("Films"), en.0, "{}", l.tag());
            assert_ne!(smart_label(), en.1, "{}", l.tag());
        }
    }
}
