//! Sort & Filter state, summary, the glass menu's morph wiring, the header and the read-outs.
use super::super::header::{self, Count, HeaderFace};
use super::super::readout::{self, Readout};
use super::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference, SHIPPED};
use plx_ui::fit::HEADROOM;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::table::MENU_MAX_W;
use plx_ui::widgets::{StatusKind, StatusOverlay};
use std::ffi::CString;

const LANGUAGES: [Preference; SHIPPED.len()] = SHIPPED;

fn default_state() -> WatchlistOptionsState {
    WatchlistOptionsState::default()
}

// ---- state transitions ----

#[test]
fn a_type_or_show_press_sets_the_choice_and_a_repeat_changes_nothing() {
    let mut s = default_state();
    assert!(OptionsRow::Type(TypeFilter::Shows).apply(&mut s));
    assert_eq!(s.type_, TypeFilter::Shows);
    assert!(!OptionsRow::Type(TypeFilter::Shows).apply(&mut s), "already chosen: no change");
    assert!(OptionsRow::Show(ShowFilter::OnYourServers).apply(&mut s));
    assert_eq!(s.show, ShowFilter::OnYourServers);
    assert_eq!((s.sort, s.descending), (SortKey::DateAdded, true), "the other sections stay");
}

#[test]
fn a_new_sort_key_lands_on_its_natural_direction_and_the_active_key_flips() {
    let mut s = default_state();
    assert!(OptionsRow::Sort(SortKey::Title).apply(&mut s));
    assert_eq!((s.sort, s.descending), (SortKey::Title, false), "Title reads A to Z");
    assert!(OptionsRow::Sort(SortKey::Title).apply(&mut s), "the active key flips direction");
    assert!(s.descending);
    assert!(OptionsRow::Sort(SortKey::Rating).apply(&mut s));
    assert_eq!((s.sort, s.descending), (SortKey::Rating, true), "Rating is highest first");
    assert!(OptionsRow::Sort(SortKey::DateAdded).apply(&mut s));
    assert_eq!(s, default_state(), "back on Date Added the value is the default again");
}

#[test]
fn every_row_is_in_all_once_and_exactly_one_per_section_is_chosen() {
    let s = default_state();
    let chosen = |f: fn(&OptionsRow) -> bool| OptionsRow::ALL.iter().filter(|r| f(r) && r.is_chosen(&s)).count();
    assert_eq!(chosen(|r| matches!(r, OptionsRow::Type(_))), 1);
    assert_eq!(chosen(|r| matches!(r, OptionsRow::Sort(_))), 1);
    assert_eq!(chosen(|r| matches!(r, OptionsRow::Show(_))), 1);
    assert_eq!(OptionsRow::ALL.len(), 10);
}

#[test]
fn sections_are_type_sort_show_with_the_direction_chevron_on_the_active_key() {
    use plx_ui::icons::Icon;
    let mut s = default_state();
    let secs = sections(&s);
    assert_eq!(secs.len(), 3);
    assert_eq!(secs.iter().map(|x| x.rows.len()).collect::<Vec<_>>(), [3, 4, 3]);
    assert_eq!(secs[1].rows[0].ticon, Some(Icon::ChevronDown), "Date Added descending");
    assert!(secs[1].rows.iter().skip(1).all(|r| r.ticon.is_none()));
    s.descending = false;
    assert_eq!(sections(&s)[1].rows[0].ticon, Some(Icon::ChevronUp));
    assert!(sections(&s).iter().all(|x| x.rows.iter().filter(|r| r.checked).count() == 1));
}

// ---- summary ----

#[test]
fn the_summary_is_default_until_something_changes_then_lists_the_changes_in_section_order() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let mut s = default_state();
    assert_eq!(s.summary(), "Default");
    OptionsRow::Type(TypeFilter::Movies).apply(&mut s);
    assert_eq!(s.summary(), "Movies");
    OptionsRow::Sort(SortKey::Title).apply(&mut s);
    OptionsRow::Sort(SortKey::Title).apply(&mut s);
    assert_eq!(s.summary(), "Movies, Title", "the direction is the menu's chevron, not the pill's");
    OptionsRow::Show(ShowFilter::Released).apply(&mut s);
    assert_eq!(s.summary(), "Movies, Title, Released");
    let mut d = default_state();
    d.descending = false;
    assert_eq!(d.summary(), "Date Added", "a flipped default key is still a changed sort");
}

#[test]
fn the_pill_text_is_the_catalogs_sentence_split_at_the_label() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let frame = header::header_frame(1771.0);
    let r = header::pill_rect(frame, &FixtureMeasure, "Default");
    // "Sort & Filter" + " · Default" measured as the chip measures them.
    let w = plx_ui::value_chip::ValueChip::width(&FixtureMeasure, c"Sort & Filter", c" · Default", None);
    assert_eq!(r.w, w);
}

// ---- the menu: glass ground, morph, geometry ----

fn open_menu() -> (OptionsMenu, Rect, WatchlistOptionsState) {
    let state = default_state();
    let frame = header::header_frame(1771.0);
    let pill = header::pill_rect(frame, &ShippedMeasure, &state.summary());
    let mut m = OptionsMenu::new();
    m.open(pill, &state);
    (m, pill, state)
}

#[test]
fn the_menu_is_a_morphing_popover_that_grows_out_of_the_pill_and_ends_on_the_panel_radius() {
    let (mut m, pill, _) = open_menu();
    assert_eq!(m.popover().open_style(), plx_ui::popover::OpenStyle::Morph);
    let panel = m.panel_rect(&ShippedMeasure);
    let f0 = m.popover().morph_frame(Some(pill), panel, PANEL_RAD).unwrap();
    assert!(f0.rect.w <= panel.w && f0.rect.h < panel.h, "frame 0 is the pill, not the panel");
    for _ in 0..120 {
        m.update(1.0 / 60.0, &ShippedMeasure);
    }
    let f = m.popover().morph_frame(Some(pill), panel, PANEL_RAD).unwrap();
    assert!((f.radius - PANEL_RAD).abs() < 0.05, "the morph ends on the panel's radius");
    assert!((f.rect.h - panel.h).abs() < 0.5 && (f.rect.w - panel.w).abs() < 0.5);
    assert!(m.is_open());
    m.dismiss();
    assert!(!m.is_open());
}

#[test]
fn the_panel_hangs_under_the_pill_right_aligned_inside_the_safe_frame() {
    let (m, pill, _) = open_menu();
    let panel = m.panel_rect(&ShippedMeasure);
    let safe = plx_ui::consts::SAFE;
    assert!((panel.x + panel.w - (pill.x + pill.w)).abs() < 0.5, "right edges align");
    let hung = pill.y + pill.h + theme::space::SM;
    assert_eq!(panel.y, hung.min(safe.y + safe.h - panel.h), "under the pill, or slid up to stay whole");
    assert!(panel.y <= hung);
    assert!(panel.x >= safe.x && panel.y + panel.h <= safe.y + safe.h + 0.01);
    assert!(panel.w >= plx_ui::table::MENU_MIN_W && panel.w <= MENU_MAX_W);
}

/// **Every row of the ten-row menu lies inside the panel, in every language** (the panel used to
/// run past the safe frame and clip its last rows).
#[test]
fn every_row_lies_inside_the_panel_in_every_language() {
    let safe = plx_ui::consts::SAFE;
    for lang in LANGUAGES {
        let _g = language_on_this_thread_for_test(lang);
        let (m, _, _) = open_menu();
        let panel = m.panel_rect(&ShippedMeasure);
        assert!(panel.y >= safe.y - 0.01 && panel.y + panel.h <= safe.y + safe.h + 0.01, "{}: panel in the safe frame", lang.tag());
        for i in 0..OptionsRow::ALL.len() {
            let r = m.row_frame(i, &ShippedMeasure).expect("row frame");
            assert!(r.y >= panel.y - 0.01 && r.y + r.h <= panel.y + panel.h + 0.01, "{}: row {i} inside the panel", lang.tag());
        }
    }
}

#[test]
fn a_press_edits_the_value_and_keeps_the_highlight_on_its_row() {
    let (mut m, _, mut state) = open_menu();
    assert_eq!(m.selected(), Some(OptionsRow::Type(TypeFilter::All)));
    m.move_sel(1);
    assert_eq!(m.selected(), Some(OptionsRow::Type(TypeFilter::Movies)));
    assert!(m.press(&mut state));
    assert_eq!(state.type_, TypeFilter::Movies);
    assert_eq!(m.selected(), Some(OptionsRow::Type(TypeFilter::Movies)), "rebuilt in place");
    assert!(!m.press(&mut state), "the chosen row again is not a change");
    m.move_sel(3);
    assert_eq!(m.selected(), Some(OptionsRow::Sort(SortKey::Title)));
    assert!(m.press(&mut state));
    assert_eq!((state.sort, state.descending), (SortKey::Title, false));
}

/// **The morph ends in the pill AS IT WILL REST.** Focus back on the pill: the accent plate all
/// the way. Focus gone elsewhere: the plate is the idle fill from the first close frame, so the
/// glass never passes through the accent.
#[test]
fn a_close_lands_in_the_pill_as_it_will_rest() {
    use plx_ui::value_chip::ValueChip;
    let (mut m, _, _) = open_menu();
    for _ in 0..120 {
        m.update(1.0 / 60.0, &ShippedMeasure);
    }
    assert_eq!(m.plate_fill(), ValueChip::fill(true), "open from the focused pill");
    m.dismiss();
    for _ in 0..60 {
        m.update(1.0 / 60.0, &ShippedMeasure);
        assert_eq!(m.plate_fill(), ValueChip::fill(true), "focus returns to the pill: accent throughout");
    }
    let (mut m, _, _) = open_menu();
    for _ in 0..120 {
        m.update(1.0 / 60.0, &ShippedMeasure);
    }
    m.dismiss_resting(false);
    assert_ne!(ValueChip::fill(false), ValueChip::fill(true), "premise: the two fills differ");
    for _ in 0..60 {
        m.update(1.0 / 60.0, &ShippedMeasure);
        assert_eq!(m.plate_fill(), ValueChip::fill(false), "focus went elsewhere: idle fill, never the accent");
    }
}

#[test]
fn the_pill_face_still_records_its_label() {
    let (m, _, state) = open_menu();
    let log = plx_ui::draw_census::capture(|| m.draw_face(plx_ui::Painter::recording(), &ShippedMeasure, &state));
    assert!(log.iter().any(|(tag, _)| *tag == 100), "the pill's label is a text run");
    // The surface's whole morph (plate, glass, label, rows) is graded on `Popover` in `plx_ui`
    // and on the menu surface in `page_tests`.
}

// ---- header ----

#[test]
fn the_pill_is_right_aligned_and_centred_on_the_title_line() {
    let frame = header::header_frame(1771.0);
    let r = header::pill_rect(frame, &FixtureMeasure, "Default");
    assert_eq!(r.x + r.w, frame.x + frame.w);
    assert_eq!(r.y + r.h * 0.5, header::HEADER_CY);
    assert_eq!(frame.x, plx_ui::consts::MARGIN_X);
    assert_eq!(frame.y, 172.0, "board: title top 172");
    assert_eq!(header::HEADER_CY, 196.0);
}

/// **The count sits just left of the pill**: 24 px clear of it, right-aligned there, centred on the
/// pill's line, LABEL regular; the pill does not move for it.
#[test]
fn the_count_hangs_24px_left_of_the_pill_on_its_line() {
    let frame = header::header_frame(1771.0);
    let pill = header::pill_rect(frame, &FixtureMeasure, "Default");
    let c = header::count_rect(frame, &FixtureMeasure, "Default", &Count::All(14).text());
    assert_eq!(c.x + c.w, pill.x - theme::space::MD);
    assert_eq!(c.cy(), pill.cy());
    let face = HeaderFace { title: "Watchlist", count: Some(Count::All(14)), summary: "Default", pill_focused: false };
    let log = plx_ui::draw_census::capture(|| header::draw_header(plx_ui::Painter::recording(), frame, &FixtureMeasure, &face));
    let runs = log.iter().filter(|(t, _)| *t == 100).count();
    assert_eq!(runs, 4, "title, count, and the pill's label and value");
    let bare = HeaderFace { count: None, ..face };
    let log = plx_ui::draw_census::capture(|| header::draw_header(plx_ui::Painter::recording(), frame, &FixtureMeasure, &bare));
    assert_eq!(log.iter().filter(|(t, _)| *t == 100).count(), 3);
    assert_eq!(header::pill_rect(frame, &FixtureMeasure, "Default"), pill, "the pill is where it was");
}

/// **Copy**: "{n} titles" at Default with real plural forms, "{shown} of {total}" when narrowed.
#[test]
fn the_count_reads_in_every_language_with_its_plural_forms() {
    let want = [
        ("en", [(1, "1 title"), (2, "2 titles"), (21, "21 titles")], "6 of 14"),
        ("es", [(1, "1 título"), (2, "2 títulos"), (21, "21 títulos")], "6 de 14"),
        ("fr", [(1, "1 titre"), (2, "2 titres"), (21, "21 titres")], "6 sur 14"),
        ("be", [(1, "1 назва"), (2, "2 назвы"), (21, "21 назва")], "6 з 14"),
        ("it", [(1, "1 titolo"), (2, "2 titoli"), (21, "21 titoli")], "6 di 14"),
        ("pt", [(1, "1 título"), (2, "2 títulos"), (21, "21 títulos")], "6 de 14"),
        ("de", [(1, "1 Titel"), (2, "2 Titel"), (21, "21 Titel")], "6 von 14"),
    ];
    for lang in SHIPPED {
        let (_, all, of) = want.iter().find(|row| row.0 == lang.tag()).unwrap_or_else(|| panic!("{lang:?}: no reviewed count row"));
        let _g = language_on_this_thread_for_test(lang);
        for (n, text) in all {
            assert_eq!(Count::All(*n).text(), *text, "{lang:?} {n}");
        }
        assert_eq!(Count::Of { shown: 6, total: 14 }.text(), *of);
    }
    let _g = language_on_this_thread_for_test(Preference::Be);
    assert_eq!(Count::All(3).text(), "3 назвы");
    assert_eq!(Count::All(5).text(), "5 назваў");
    assert_eq!(Count::All(11).text(), "11 назваў");
}

// ---- read-outs ----

#[test]
fn the_readout_hangs_its_verdict_from_540_with_the_reason_under_it() {
    let frame = Rect::FULL;
    let (cap, reason) = readout::bands(frame, &FixtureMeasure);
    assert_eq!(cap.y, 540.0);
    assert_eq!(cap.y, StatusOverlay::FULL_ANCHOR_TOP);
    assert_eq!(reason.y, cap.y + cap.h + theme::space::SM);
    assert!(reason.w <= StatusOverlay::REASON_W);
    assert!((reason.cx() - frame.cx()).abs() < 0.01, "centred on the page");
}

#[test]
fn the_readout_wears_the_bright_verdict_faces() {
    let (sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
    assert_eq!((sz, bold, ink), (theme::size::TITLE, true, theme::TEXT_PRIMARY));
    let (rsz, rink) = StatusOverlay::reason_face(StatusKind::Failed);
    assert_eq!((rsz, rink), (theme::size::BODY, theme::TEXT_SECONDARY));
}

#[test]
fn each_readout_has_its_own_verdict_and_only_the_list_ones_have_a_body() {
    let _g = language_on_this_thread_for_test(Preference::En);
    assert_eq!(Readout::Empty.verdict(), "Your watchlist is empty");
    assert_eq!(Readout::Filtered.verdict(), "Nothing matches these filters");
    assert_eq!(Readout::NoToken.verdict(), "Watchlist isn’t available for this profile");
    assert_eq!(Readout::Unreachable.verdict(), "Can’t reach your Watchlist");
    assert!(Readout::Empty.body().is_some() && Readout::Filtered.body().is_some());
    assert!(Readout::NoToken.body().is_none() && Readout::Unreachable.body().is_none());
    let log = plx_ui::draw_census::capture(|| readout::draw_readout(Readout::Empty, plx_ui::Painter::recording(), Rect::FULL));
    assert_eq!(log.iter().filter(|(tag, _)| *tag == 100).count(), 2, "a verdict and a reason");
}

// ---- text fit, every shipped language, the device's own advances ----

#[test]
fn the_menu_pill_and_readouts_fit_in_every_shipped_language() {
    let mut bad = Vec::new();
    for language in LANGUAGES {
        let _g = language_on_this_thread_for_test(language);
        let tag = language.tag();
        // The widest menu state: every section's longest label, the active key's chevron.
        let mut state = default_state();
        state.sort = SortKey::Release;
        let mut table = plx_ui::table::TableView::new();
        table.compact = true;
        table.open_sections(sections(&state));
        if let Some(f) = table.menu_cap_failure(&ShippedMeasure, &format!("{tag} sort & filter menu")) {
            bad.push(f);
        }
        // The pill with the longest summary the menu can compose.
        let mut long = default_state();
        long.type_ = TypeFilter::Shows;
        long.sort = SortKey::Release;
        long.descending = false;
        long.show = ShowFilter::OnYourServers;
        let frame = header::header_frame(1771.0);
        let pill = header::pill_rect(frame, &ShippedMeasure, &long.summary());
        // The title (tracked), then the longest count (five digits, or "99999 of 99999") and its
        // 24 px gap to the pill, then the air the title keeps.
        let title = CString::new(msg::watchlist_page_title()).unwrap();
        let title_w = plx_machine::machine::Measure::width(&ShippedMeasure, &title, theme::size::DISPLAY, true)
            + theme::TRACKING_TITLE_TIGHT * title.as_bytes().len() as f32;
        let title_end = plx_ui::consts::MARGIN_X + title_w + theme::space::LG;
        let longest = [Count::All(88888), Count::All(11111), Count::All(22222), Count::Of { shown: 88888, total: 88888 }];
        for count in longest {
            let c = header::count_rect(frame, &ShippedMeasure, &long.summary(), &count.text());
            if c.x < title_end {
                bad.push(format!("{tag} count {:?} ({:.0}px) at x {:.0} runs into the title (needs >= {title_end:.0})", count.text(), c.w, c.x));
            }
            let def = header::count_rect(frame, &ShippedMeasure, &default_state().summary(), &count.text());
            if def.x < title_end {
                bad.push(format!("{tag} count {:?} at the Default pill is at x {:.0}, title ends {title_end:.0}", count.text(), def.x));
            }
        }
        if pill.x < title_end {
            bad.push(format!("{tag} pill {:.0}px wide runs into the title (needs x >= {title_end:.0}, is {:.0})", pill.w, pill.x));
        }
        // Verdicts: one line inside the page margins; bodies in the reason's two-line slot.
        for r in [Readout::Empty, Readout::Filtered, Readout::NoToken, Readout::Unreachable] {
            let c = CString::new(r.verdict()).unwrap();
            let w = plx_machine::machine::Measure::width(&ShippedMeasure, &c, theme::size::TITLE, true) / HEADROOM;
            if w > plx_ui::consts::SCR_W - 2.0 * plx_ui::consts::MARGIN_X {
                bad.push(format!("{tag} verdict {:?} is {w:.0}px", r.verdict()));
            }
            if let Some(b) = r.body() {
                let c = CString::new(b).unwrap();
                if StatusOverlay::failed_reason_truncates(&c, &ShippedMeasure, HEADROOM) {
                    bad.push(format!("{tag} body {b:?} would end in an ellipsis"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "does not fit:\n  {}", bad.join("\n  "));
}
