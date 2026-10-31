//! Sort & Filter state, summary, the glass menu's morph wiring, the header and the read-outs.
use super::super::header::{self, Count};
use super::super::readout::{self, Readout};
use super::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_machine::machine::Measure as _;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference, SHIPPED};
use plx_ui::fit::HEADROOM;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::page_header::HeaderSwap;
use plx_ui::table::MENU_MAX_W;
use plx_ui::widgets::{StatusKind, StatusOverlay};
use std::ffi::CString;

const LANGUAGES: [Preference; SHIPPED.len()] = SHIPPED;

fn default_state() -> WatchlistOptionsState {
    WatchlistOptionsState::default()
}

/// **The Type section's first row is the other two rows' words joined** (design review N-e): "Movies &
/// Shows" sat above a "TV Shows" row. In every shipped language it names both by the word their own
/// row wears.
#[test]
fn the_all_row_of_the_type_section_uses_the_words_of_the_other_two_rows() {
    for language in LANGUAGES {
        let _g = language_on_this_thread_for_test(language);
        let all = msg::watchlist_filter_type_all().to_lowercase();
        for row in [msg::watchlist_filter_type_movies(), msg::watchlist_filter_type_shows()] {
            assert!(all.contains(&row.to_lowercase()), "{}: {all:?} does not name the {row:?} row", language.tag());
        }
    }
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
    assert_eq!(s, default_state(), "back on Date added the value is the default again");
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
fn sections_are_named_as_the_librarys_and_the_checked_sort_says_its_direction_in_a_word() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let mut s = default_state();
    let secs = sections(&s);
    assert_eq!(secs.len(), 3);
    assert_eq!(secs.iter().map(|x| x.rows.len()).collect::<Vec<_>>(), [3, 4, 3]);
    assert_eq!(secs.iter().map(|x| x.header.as_str()).collect::<Vec<_>>(), ["Show", "Sort", "Filter"],
        "the Library's section words, not Type / Sort by / Show");
    assert_eq!(secs[1].rows[0].value.as_deref(), Some("Newest first"), "Date added descending");
    assert!(secs[1].rows.iter().all(|r| r.ticon.is_none()), "no chevron inside a row: it reads as opens");
    assert!(secs[1].rows.iter().skip(1).all(|r| r.value.is_none()));
    s.descending = false;
    assert_eq!(sections(&s)[1].rows[0].value.as_deref(), Some("Oldest first"));
    OptionsRow::Sort(SortKey::Title).apply(&mut s);
    assert_eq!(sections(&s)[1].rows[1].value.as_deref(), Some("A\u{2013}Z"));
    assert!(sections(&s).iter().all(|x| x.rows.iter().filter(|r| r.checked).count() == 1));
}

// ---- summary ----

#[test]
fn the_summary_is_the_sort_and_only_the_sort() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let mut s = default_state();
    assert_eq!(s.summary(), "Date added", "at rest the pill names the sort the panel ticks, never \"Default\"");
    OptionsRow::Type(TypeFilter::Movies).apply(&mut s);
    OptionsRow::Show(ShowFilter::Released).apply(&mut s);
    assert_eq!(s.summary(), "Date added", "a filter shows as the N of M read-out, never in the pill");
    OptionsRow::Sort(SortKey::Title).apply(&mut s);
    OptionsRow::Sort(SortKey::Title).apply(&mut s);
    assert_eq!(s.summary(), "Title", "the direction is the panel's checked-row value, not the pill's");
}

/// **The control is the Library's sort control**: the sort icon, the sort in force and a quiet down
/// chevron, with "Sort & Filter" its name, carried and never drawn: no label, no separator, no
/// "Sort & Filter · ".
#[test]
fn the_control_draws_the_criterion_alone_and_never_its_name() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let row = header::Row::new(&ShippedMeasure, "Date added", None);
    let runs = plx_ui::draw_census::capture_runs(|| row.draw(plx_ui::Painter::recording(), 0.0, &plx_ui::widgets::TabStrip::new(), 1.0, false, &HeaderSwap::new()));
    let words: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(words, [msg::watchlist_page_title(), "Date added"], "the title and the criterion: no name, no separator");
    assert_eq!(msg::watchlist_page_sort_filter(), "Sort & Filter", "the name is still the catalog's");
    let item = &row.sort_item();
    assert_eq!(item.label, msg::watchlist_page_sort_filter(), "the item carries the name (the identity the row's swap knows it by)");
    assert!(item.value_only && item.fold_glyph && item.fold_quiet && item.glyph == Some(plx_ui::icons::Icon::Sort) && item.focusable);
    assert_eq!(item.value.as_deref(), Some("Date added"));
}

/// **The control is the Library's, pixel for pixel in its parts**: the icon box leads, the criterion
/// follows one glyph gap past it, and the quiet chevron ends the text edge, x 1771; the criterion's
/// ink is the one run the control draws.
#[test]
fn the_control_ends_on_the_text_edge_with_its_icon_criterion_and_quiet_chevron() {
    use plx_ui::page_header::{GLYPH_GAP, GLYPH_SIZE, QUIET_FOLD_GAP, QUIET_FOLD_SIZE};
    let _g = language_on_this_thread_for_test(Preference::En);
    // The painter that records the census measures with the legacy advances; the row is laid out
    // with the same ones so that what is drawn is compared with what was placed.
    let row = header::Row::new(&plx_ui::widgets::LegacyMeasure, "Date added", None);
    let at = row.items()[1].text;
    assert!((at.x + at.w - header::GRID_RIGHT).abs() < 0.01, "the control ends on the right text edge: {at:?}");
    let runs = plx_ui::draw_census::capture_runs(|| row.draw(plx_ui::Painter::recording(), 0.0, &plx_ui::widgets::TabStrip::new(), 1.0, false, &HeaderSwap::new()));
    let criterion = runs.iter().find(|r| r.text == "Date added").expect("the criterion is drawn");
    assert_eq!(criterion.at.x, at.x + GLYPH_SIZE + GLYPH_GAP, "the criterion starts one gap past the icon's box");
    assert_eq!(criterion.at.x + criterion.at.w + QUIET_FOLD_GAP + QUIET_FOLD_SIZE, at.x + at.w, "the quiet chevron closes the text edge");
}

/// **The criterion has no slot, in any shipped language**: a longer sort name widens the control
/// to the LEFT of the text edge, and the read-out in front of it keeps a cluster gap clear of the
/// title (`the_menu_pill_and_readouts_fit_in_every_shipped_language`).
#[test]
fn the_control_keeps_its_right_edge_whatever_the_criterion_is() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let edge = |summary: &str| {
        let row = header::Row::new(&ShippedMeasure, summary, Some(Count::Of { shown: 6, total: 14 }));
        let control = row.items()[2].text;
        (control.x, control.x + control.w)
    };
    let (short, long) = (edge("Title"), edge("Date added"));
    assert!((short.1 - long.1).abs() < 0.01 && (short.1 - 1771.0).abs() < 0.01, "the edge never moves: {short:?} {long:?}");
    assert!(long.0 < short.0, "the control grows leftwards with its criterion");
}

// ---- the menu: glass ground, morph, geometry ----

fn open_menu() -> (OptionsMenu, Rect, WatchlistOptionsState) {
    let state = default_state();
    let pill = header::Row::new(&ShippedMeasure, &state.summary(), None).pill();
    let mut m = OptionsMenu::new();
    m.open(pill, &state);
    (m, pill, state)
}

/// **The panel is never narrower than the pill it grows out of** (owner: the Sort & Filter panel
/// was 337 under a 365 pill). One rule for every header control's popover
/// (`TableView::menu_panel_width_from`), the same one the Library's menu hangs by.
#[test]
fn the_panel_is_never_narrower_than_the_pill_it_grows_out_of() {
    let (m, pill, _) = open_menu();
    let panel = m.panel_rect(&ShippedMeasure);
    assert!(panel.w >= pill.w, "the panel {} is narrower than its pill {}", panel.w, pill.w);
    assert!((panel.x + panel.w - (pill.x + pill.w)).abs() < 0.01, "right edges stay aligned");
}

#[test]
fn the_menu_is_a_morphing_popover_that_grows_out_of_the_pill_and_ends_on_the_panel_radius() {
    let (mut m, pill, _) = open_menu();
    assert_eq!(m.popover().open_style(), plx_ui::popover::OpenStyle::Morph);
    let panel = m.panel_rect(&ShippedMeasure);
    let f0 = m.popover().morph_frame(Some(pill), panel, PANEL_RAD).unwrap();
    assert_eq!(f0.rect, pill, "frame 0 is the focused pill (the plate and its outset), not the panel");
    assert!(f0.rect.h < panel.h);
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

/// **The pill is never under the panel's glass** (S12): the opener lift draws the pill lit above the
/// panel, so the settled panel must hang clear of it, in every shipped language. A panel that
/// slid up over the pill (taller than the room below) would put an accent pill across its top edge.
#[test]
fn the_panel_hangs_clear_of_the_lit_pill_in_every_language() {
    for lang in LANGUAGES {
        let _g = language_on_this_thread_for_test(lang);
        let (m, pill, _) = open_menu();
        let panel = m.panel_rect(&ShippedMeasure);
        assert!(panel.y >= pill.y + pill.h, "{}: the panel {panel:?} covers the pill {pill:?}", lang.tag());
    }
}

// ---- header ----

/// **The row is the Library's**: container left 76 / right 129, top 148, 56 tall, text edges x 96
/// and x 1771; the control is right-aligned on the text edge, the title stands on the left edge.
#[test]
fn the_row_is_the_librarys_geometry() {
    let row = header::Row::new(&FixtureMeasure, "Date added", None);
    let title = row.title_box();
    assert_eq!((title.x, title.y, title.h), (76.0, 148.0, 56.0), "the title's box: text at x 96, the room a plate would take overhangs 20");
    let plate = row.plate();
    assert_eq!((plate.y, plate.h), (148.0, 56.0));
    assert_eq!(plate.x + plate.w, 1771.0 + 20.0, "the control's text ends on x 1771; its plate overhangs");
    let pill = row.pill();
    assert_eq!(pill.cy(), plate.cy());
    assert!((pill.w - plate.w - 2.0 * plx_ui::widgets::PILL_FOCUS_OUTSET).abs() < 1.0e-4, "the focused pill is the plate and its outset");
    assert_eq!(header::GRID_RIGHT, 1771.0);
    assert_eq!(header::HEADER_TOP + header::BLOCK_H, 244.0);
}

/// **S10: the Watchlist's title is plain text.** The selection plate means "current among
/// siblings" and the white pill means "focusable", and the title is neither: it is the page's name,
/// so it stands at full white on the bare ground with no plate under it, whatever the focus does
/// (the design review's S10: "Watchlist" sat on a plate that no sibling shared).
#[test]
fn the_title_is_plain_text_with_no_plate_under_it() {
    use plx_machine::machine::EntryId;
    use plx_ui::widgets::{CtlPop, TabStrip};
    let _g = plx_base::testlock::serial();
    let row = header::Row::new(&ShippedMeasure, "Default", None);
    let (mut capsules, mut pop) = (TabStrip::new(), CtlPop::<1>::new());
    for _ in 0..180 { row.step(EntryId(1), None, &mut capsules, &mut pop, 1.0 / 60.0); }
    let title = row.title_box();
    let plates = plx_ui::draw_census::capture(|| row.draw(plx_ui::Painter::recording(), 0.0, &capsules, 1.0, false, &HeaderSwap::new()));
    let under_title: Vec<_> = plates.iter().filter(|(tag, r)| *tag == 2 && r.x <= title.x + title.w && r.x + r.w >= title.x).collect();
    assert!(under_title.is_empty(), "a plate is drawn under the title: {under_title:?}");
    let runs = plx_ui::draw_census::capture_runs(|| row.draw(plx_ui::Painter::recording(), 0.0, &capsules, 1.0, false, &HeaderSwap::new()));
    let name = runs.iter().find(|r| r.text == msg::watchlist_page_title()).expect("the title is drawn");
    assert_eq!(name.ink, [1.0, 1.0, 1.0, 1.0], "the title is full white, not a resting control's 65%");
}

/// **A count is a filter read-out and nothing else** (the Library's rule): "6 of 14" while a filter
/// narrows the list, one row gap ahead of the control, and no "14 titles" at rest.
#[test]
fn the_row_carries_a_readout_only_while_a_filter_narrows_the_list() {
    let plain = header::Row::new(&FixtureMeasure, "Default", Some(Count::All(14)));
    assert_eq!(plain.items().len(), 2, "the title and the control: nothing else at rest");
    let narrowed = header::Row::new(&FixtureMeasure, "Default", Some(Count::Of { shown: 6, total: 14 }));
    assert_eq!(narrowed.items().len(), 3);
    let readout = narrowed.items()[1];
    let control = narrowed.items()[2];
    assert_eq!(control.text.x - (readout.text.x + readout.text.w), plx_ui::page_header::GAP);
    assert!(!readout.focusable && control.focusable);
    assert_eq!(narrowed.plate(), plain.plate(), "the control does not move for the read-out");
    let log = plx_ui::draw_census::capture(|| narrowed.draw(plx_ui::Painter::recording(), 0.0, &plx_ui::widgets::TabStrip::new(), 1.0, false, &HeaderSwap::new()));
    assert_eq!(log.iter().filter(|(t, _)| *t == 100).count(), 3, "the title, the read-out and the criterion");
}

/// A lifted control (the Sort & Filter menu is its owner now) is left out of the page's pass, and
/// the row's scroll offset moves everything with the page.
#[test]
fn a_lifted_control_is_left_out_and_the_row_rides_the_scroll() {
    let row = header::Row::new(&FixtureMeasure, "Date added", None);
    let at = |dy: f32, lifted: bool| plx_ui::draw_census::capture(|| row.draw(plx_ui::Painter::recording(), dy, &plx_ui::widgets::TabStrip::new(), 1.0, lifted, &HeaderSwap::new()));
    assert_eq!(at(0.0, true).iter().filter(|(t, _)| *t == 100).count(), 1, "the title alone");
    let (rest, scrolled) = (at(0.0, false), at(-100.0, false));
    for (a, b) in rest.iter().zip(&scrolled) {
        assert!((a.1.y - 100.0 - b.1.y).abs() < 1.0e-3);
    }
}

/// **Copy**: "{shown} of {total}" is the Library's sentence, in every language, grouped as it groups.
#[test]
fn the_readout_is_the_librarys_sentence_in_every_language() {
    for lang in SHIPPED {
        let _g = language_on_this_thread_for_test(lang);
        let locale = plx_platform::i18n::current();
        let want = msg::library_header_filtered_of(&locale.number(1284), &locale.number(2500));
        assert_eq!(Count::Of { shown: 1284, total: 2500 }.readout(), Some(want), "{lang:?}");
        assert_eq!(Count::All(14).readout(), None);
    }
    let _g = language_on_this_thread_for_test(Preference::En);
    assert_eq!(Count::Of { shown: 6, total: 14 }.readout().as_deref(), Some("6 of 14"));
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

/// Q2: the read-out's verdict and reason sit exactly where `StatusOverlay` puts a page-placed
/// `Failed` read-out's, for a one-line AND a two-line reason: the painted runs' positions do not
/// depend on the reason box's height (it is top-anchored), and the read-out's bands ARE the overlay's.
#[test]
fn the_readout_paints_the_same_runs_for_a_one_line_and_a_two_line_reason() {
    let _g = language_on_this_thread_for_test(Preference::En);
    let measure = FixtureMeasure;
    let (cap, reason) = readout::bands(Rect::FULL, &measure);
    let (verdict_sz, ..) = StatusOverlay::verdict_face(StatusKind::Failed);
    assert_eq!(cap.y, 540.0);
    assert_eq!(cap.h, measure.line_h(verdict_sz));
    assert_eq!(reason.y, 540.0 + cap.h + theme::space::SM, "the reason hangs SM under the verdict's box");
    // The reserved slot: one line pitch plus the last line's box, whatever the reason says.
    let (reason_sz, _) = StatusOverlay::reason_face(StatusKind::Failed);
    let pitch = plx_ui::text_view::TextView::new("", reason_sz, theme::TEXT_SECONDARY).line_h();
    assert_eq!(reason.h, pitch + measure.line_h(reason_sz));
    // Painted (the read-out draws with the device's `LegacyMeasure`): the reason's first run starts
    // at the reason band's top, one-line body or two.
    let (_, painted) = readout::bands(Rect::FULL, &plx_ui::widgets::LegacyMeasure);
    let runs = |r: Readout| {
        plx_ui::draw_census::capture_runs(|| readout::draw_readout(r, plx_ui::Painter::recording(), Rect::FULL))
    };
    for r in [Readout::Empty, Readout::Filtered] {
        let runs = runs(r);
        assert!(runs.len() >= 2, "{r:?}: a verdict and at least one reason line");
        let body = CString::new(r.body().unwrap()).unwrap();
        let (reason_sz, ink) = StatusOverlay::reason_face(StatusKind::Failed);
        let want = plx_ui::draw_census::capture_runs(|| {
            plx_ui::text_view::TextView::new(body.to_str().unwrap(), reason_sz, ink)
                .h(plx_ui::label::HAlign::Center)
                .max_lines(2)
                .with_measure(&plx_ui::widgets::LegacyMeasure)
                .draw(plx_ui::Painter::recording(), Rect::new(painted.x, painted.y, painted.w, 0.0));
        });
        assert!((runs[1].at.y - want[0].at.y).abs() < 0.5, "{r:?}: the reason starts at {}, want {}", runs[1].at.y, want[0].at.y);
    }
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
        // The widest menu state: every section's longest label, the active key's direction word.
        for (sort, descending) in [SortKey::DateAdded, SortKey::Title, SortKey::Release, SortKey::Rating]
            .into_iter().flat_map(|sort| [(sort, false), (sort, true)])
        {
            let mut state = default_state();
            (state.sort, state.descending) = (sort, descending);
            let mut table = plx_ui::table::TableView::new();
            table.compact = true;
            table.open_sections(sections(&state));
            if let Some(f) = table.menu_cap_failure(&ShippedMeasure, &format!("{tag} sort & filter menu, {sort:?} desc {descending}")) {
                bad.push(f);
            }
        }
        // The pill with the longest summary the menu can compose: the longest sort label.
        // The title's plate must end a cluster gap before the right cluster's first text edge, for
        // every sort's name with the longest read-out in front of it.
        for sort in [SortKey::DateAdded, SortKey::Title, SortKey::Release, SortKey::Rating] {
            let mut long = default_state();
            long.sort = sort;
            for count in [Count::All(88888), Count::Of { shown: 88888, total: 88888 }] {
                let row = header::Row::new(&ShippedMeasure, &long.summary(), Some(count));
                let (title_end, cluster) = (row.title_box().x + row.title_box().w, row.cluster_left());
                if cluster < title_end + plx_ui::page_header::CLUSTER_GAP {
                    bad.push(format!("{tag} {sort:?} {count:?}: the cluster starts at x {cluster:.0}, the title's plate ends {title_end:.0}"));
                }
                // The control has no slot: whatever the sort is called, it ends on the text edge.
                let control = row.items().last().expect("the control").text;
                if (control.x + control.w - header::GRID_RIGHT).abs() > 0.01 {
                    bad.push(format!("{tag} {sort:?} {count:?}: the control ends at x {:.1}, not {}", control.x + control.w, header::GRID_RIGHT));
                }
            }
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
