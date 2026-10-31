//! T1: the Collections page's own rules: Sort visibility, the section-sync guard, the crumb's
//! geometry and focus order, the tile caption (Belarusian plurals 1/2/5/21).
use super::*;
use plx_ui::caption_policy::{CaptionFacts, CaptionPolicy, DetailsKind};
use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{self, language_on_this_thread_for_test, Language, Preference};
use plx_ui::page_header::{self, HeaderLayout, ItemRect, PageHeader, Slot};
use plx_machine::machine::Measure;

/// The header row the page declares, laid out at the screen's width by the shared widget.
fn row(library: &str, kind: SecKind, declared_sorts: usize, sort_value: &str, measure: &dyn Measure) -> (PageHeaderSpec, HeaderLayout) {
    let page = PageHeader::new(header_spec(library, kind, declared_sorts, sort_value), &[], measure);
    let layout = page.layout(1920.0, measure);
    (page.spec().clone(), layout)
}

fn part(layout: &HeaderLayout, slot: Slot) -> Option<ItemRect> {
    layout.items.iter().copied().find(|item| item.slot == slot)
}

/// A movie collection's facts as the one builder reads them (`registry::tile_facts::caption_facts`).
fn films(n: u32) -> CaptionFacts {
    CaptionFacts { title: Some("Starfall Saga".into()), subtype: Some("movie".into()), child_count: Some(n).filter(|n| *n > 0), ..Default::default() }
}

fn lines(tag: &str, stored: CaptionPolicy, f: &CaptionFacts) -> [Option<String>; 2] {
    let _g = language_on_this_thread_for_test(Preference::from_tag(tag));
    plx_ui::caption_policy::caption_lines(&stored, f, i18n::current())
}

#[test]
fn sort_is_hidden_unless_the_server_declares_two_or_more() {
    assert!(!sort_visible(0));
    assert!(!sort_visible(1), "PMS declares only titleSort for type=18");
    assert!(sort_visible(2));
    assert!(sort_visible(5));
    let m = ShippedMeasure;
    assert!(part(&row("Movies", SecKind::Movie, 1, "Title", &m).1, Slot::Right(0)).is_none());
    assert!(part(&row("Movies", SecKind::Movie, 2, "Title", &m).1, Slot::Right(0)).is_some());
}

#[test]
fn the_sort_control_ends_at_the_header_row_edge() {
    let sort = part(&row("Movies", SecKind::Movie, 2, "Title", &ShippedMeasure).1, Slot::Right(0)).unwrap();
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
fn the_crumb_root_is_the_librarys_own_name_then_collections() {
    let _g = language_on_this_thread_for_test(Preference::from_tag("en"));
    let m = ShippedMeasure;
    let (spec, layout) = row("Movies", SecKind::Movie, 1, "Title", &m);
    let crumb = spec.crumb.expect("the Collections page's row is a crumb");
    assert_eq!((crumb.root.as_str(), crumb.page.as_str()), ("Movies", "Collections"));
    let root_of = |library, kind| header_spec(library, kind, 1, "Title").crumb.unwrap().root;
    assert_eq!(root_of("Anime Films", SecKind::Show), "Anime Films");
    // Only a directory with no section to name falls back to what the library holds.
    assert_eq!(root_of("", SecKind::Movie), "Films");
    assert_eq!(root_of("", SecKind::Show), "Shows");
    let (root, name) = (part(&layout, Slot::CrumbRoot).unwrap(), part(&layout, Slot::CrumbPage).unwrap());
    assert_eq!(root.text.x, 96.0);
    assert!(name.plate.x > root.plate.x + root.plate.w - 8.0);
}

#[test]
fn the_crumb_and_the_sort_control_never_meet_in_any_shipped_language() {
    let m = ShippedMeasure;
    let mut bad = Vec::new();
    for l in Language::SHIPPED {
        let _g = language_on_this_thread_for_test(Preference::from_tag(l.tag()));
        for kind in [SecKind::Movie, SecKind::Show] {
            let (_, layout) = row("Movies", kind, 2, "Title added (newest first)", &m);
            let name = part(&layout, Slot::CrumbPage).unwrap().plate;
            let gap = part(&layout, Slot::Right(0)).unwrap().plate.x - (name.x + name.w);
            if gap < page_header::CLUSTER_GAP { bad.push(format!("{}: {gap:.0}px between crumb and Sort", l.tag())); }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn the_default_caption_is_the_count_alone_and_title_on_puts_the_name_above() {
    let en = "en";
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(24)), [None, Some("24 films".into())]);
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(1)), [None, Some("1 film".into())]);
    let titled = CaptionPolicy { title: true, details: DetailsKind::YearLength };
    assert_eq!(lines(en, titled, &films(24)), [Some("Starfall Saga".into()), Some("24 films".into())]);
    let shows = CaptionFacts { subtype: Some("show".into()), ..films(3) };
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &shows)[1].as_deref(), Some("3 shows"));
    let other = CaptionFacts { subtype: Some(String::new()), ..films(3) };
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
}

/// **S23, under the one rule.** A name the tile's face cuts rides the details line when the Title
/// switch is off (the policy's own band, one line), is said once when it is on, and Nothing opens
/// nothing; a name the face sets whole follows the switch.
#[test]
fn a_name_the_face_cuts_rides_the_details_line_inside_the_policys_band() {
    let en = "en";
    let cut = CaptionFacts { name_cut: true, ..films(4) };
    let silent = CaptionPolicy { title: false, details: DetailsKind::None };
    assert_eq!(lines(en, silent, &cut), [None, None], "Nothing opens nothing");
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &cut), [None, Some("Starfall Saga \u{b7} 4 films".into())]);
    assert_eq!(lines(en, CaptionPolicy { title: true, ..CaptionPolicy::DEFAULT }, &cut), [Some("Starfall Saga".into()), Some("4 films".into())]);
    assert_eq!(lines(en, silent, &films(4)), [None, None], "a name set whole follows the switch");
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(4)), [None, Some("4 films".into())]);
}

#[test]
fn an_unknown_count_invents_no_count_and_says_what_the_tile_is() {
    let en = "en";
    assert_eq!(lines(en, CaptionPolicy::DEFAULT, &films(0)), [None, Some("Collection".into())]);
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
    let shows = CaptionFacts { subtype: Some("show".into()), ..films(2) };
    assert!(lines(be, CaptionPolicy::DEFAULT, &shows)[1].as_deref().is_some_and(|s| s.starts_with("2 ")));
}

#[test]
fn every_shipped_language_words_the_count_without_english() {
    let en = lines("en", CaptionPolicy::DEFAULT, &films(7))[1].clone().unwrap();
    for l in Language::SHIPPED {
        if l.tag() == "en" { continue; }
        let s = lines(l.tag(), CaptionPolicy::DEFAULT, &films(7))[1].clone().unwrap();
        assert!(s.starts_with("7 ") || s.contains('7'), "{}: {s}", l.tag());
        // French and Dutch spell the plural of "film" as English does.
        if !["fr", "nl"].contains(&l.tag()) { assert_ne!(s, en, "{}: caption is English", l.tag()); }
    }
}

#[test]
fn the_empty_sentence_exists_in_every_language_and_fit_a_centred_line() {
    let m = ShippedMeasure;
    let en = { let _g = language_on_this_thread_for_test(Preference::from_tag("en")); empty_text("Films") };
    for l in Language::SHIPPED {
        let _g = language_on_this_thread_for_test(Preference::from_tag(l.tag()));
        let e = empty_text("Documentaries & Concerts");
        assert!(e.contains("Documentaries & Concerts"), "{}: {e}", l.tag());
        let w = std::ffi::CString::new(e.clone()).map_or(0.0, |c| m.width(&c, plx_ui::theme::size::BODY, false));
        assert!(w < 1200.0, "{}: {e} is {w}px", l.tag());
        if l.tag() != "en" {
            assert_ne!(empty_text("Films"), en, "{}", l.tag());
        }
    }
}
