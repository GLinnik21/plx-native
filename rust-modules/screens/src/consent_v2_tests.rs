//! Settings v2, stage 3: Privacy & data's anatomy (rows without sub-lines, the focused row's
//! sentence in the left column, the Settings page title, the line beside Done) and the first-run
//! question, which keeps its look.

use super::*;
use super::test_support::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_machine::machine::Measure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, SHIPPED};
use plx_ui::route_screen::TitleStyle;

fn settings_page(m: &FixtureMeasure) -> ConsentPage {
    let c = test_cx(m);
    let (mut out, mut present) = sink();
    ConsentPage::settings(EntryId(1), &c, &mut mk_fx(&mut out, &mut present))
}

fn focus(page: &mut ConsentPage, id: RowId) {
    let m = FixtureMeasure;
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let to = FocusKey { entry: EntryId(1), elem: page.key_of(id) };
    page.step(&ScreenEvent::FocusMoved { from: None, to, by: plx_ui::screen::By::Dir }, &c, &mut mk_fx(&mut out, &mut present));
}

/// The board (Sv2s-V1): Reporting (2), Information (5), On this TV (1) - eight rows, none with
/// a sub-line, because the sentence is said beside the table.
#[test]
fn privacy_is_eight_rows_in_three_groups_without_sub_lines() {
    let _guard = plx_base::testlock::serial();
    let m = FixtureMeasure;
    let page = settings_page(&m);
    assert_eq!(page.row_ids(), vec![
        RowId::Errors, RowId::Usage,
        RowId::PreviewCrash, RowId::PreviewUsage, RowId::Policy, RowId::ErrorsId, RowId::AnalyticsId,
        RowId::Delete,
    ]);
    let sections: Vec<usize> = page.form.table.sections.iter().map(|s| s.rows.len()).collect();
    assert_eq!(sections, vec![2, 5, 1]);
    for section in &page.form.table.sections {
        for row in &section.rows {
            assert!(row.detail.is_empty(), "{:?} still carries a sub-line", row.label);
        }
    }
}

/// All eight rows fit the list without scrolling, even with focus on the last.
#[test]
fn all_eight_privacy_rows_fit_without_scrolling() {
    let _guard = plx_base::testlock::serial();
    let m = FixtureMeasure;
    let mut page = settings_page(&m);
    focus(&mut page, RowId::Delete);
    for _ in 0..600 {
        page.form.table.update(1.0 / 60.0, page.list_frame().h);
    }
    assert_eq!(page.form.table.scroll_pos(), 0.0, "the last row is already in view");
}

/// The sentence is a pure function of the focused row, and each is the old sub-line.
#[test]
fn the_sentence_follows_the_focused_row() {
    let _guard = plx_base::testlock::serial();
    let m = FixtureMeasure;
    let mut page = settings_page(&m);
    assert_eq!(page.follow.text(), msg::settings_consent_crash_detail(), "opens on the first row's sentence");
    assert_eq!(page.follow.arrival(), 1.0, "nothing fades on open");
    for (id, want) in [
        (RowId::Usage, msg::settings_consent_usage_detail()),
        (RowId::PreviewCrash, msg::settings_consent_preview_crash()),
        (RowId::PreviewUsage, msg::settings_consent_preview_usage()),
        (RowId::Policy, msg::settings_consent_preview_policy()),
        (RowId::ErrorsId, msg::settings_consent_preview_errors_id()),
        (RowId::AnalyticsId, msg::settings_consent_preview_analytics_id()),
        (RowId::Delete, msg::settings_consent_delete_detail()),
        (RowId::Errors, msg::settings_consent_crash_detail()),
    ] {
        focus(&mut page, id);
        assert_eq!(page.follow.text(), want, "{id:?}");
    }
    assert_eq!(settings_sentence(None), msg::settings_consent_settings_copy(), "no focused row: the page's");
}

/// Flipping a switch keeps the sentence (the same row stays focused): nothing fades.
#[test]
fn flipping_a_switch_does_not_restart_the_sentence() {
    let _guard = plx_base::testlock::serial();
    let m = FixtureMeasure;
    let mut page = settings_page(&m);
    let (mut out, mut present) = sink();
    for _ in 0..60 { page.follow.update(1.0 / 60.0); }
    page.row_commit(page.key_of(RowId::Errors), &mut mk_fx(&mut out, &mut present));
    assert_eq!(page.follow.arrival(), 1.0);
}

/// Privacy & data wears the Settings page title; the first-run question keeps the hero (the board
/// does not move it).
#[test]
fn only_privacy_moves_to_the_page_title_and_first_run_keeps_the_hero() {
    let _guard = plx_base::testlock::serial();
    let m = FixtureMeasure;
    assert_eq!(settings_page(&m).layout.title_style, TitleStyle::Page);
    let c = test_cx(&m);
    let (mut out, mut present) = sink();
    let first = ConsentPage::first_run(EntryId(1), 0, &c, &mut mk_fx(&mut out, &mut present));
    assert_eq!(first.layout.title_style, TitleStyle::Hero);
}

/// Done stays the only band control and appears with a pending draft; the line beside it says
/// what BACK does, sits to Done's right inside the slot, and is gone with Done. BACK still
/// discards (the surface pops) and Done still applies it.
#[test]
fn done_carries_one_line_beside_it_only_while_a_draft_is_pending() {
    let _guard = plx_base::testlock::serial();
    let saved = consent::current();
    let m = FixtureMeasure;
    let mut page = settings_page(&m);
    assert!(page.band_labels().is_empty() && page.band_note().is_none(), "no draft: no Done, no note");
    let (mut out, mut present) = sink();
    page.row_commit(page.key_of(RowId::Errors), &mut mk_fx(&mut out, &mut present));
    assert_eq!(page.band_labels(), vec![msg::settings_done_c()]);
    assert_eq!(page.band_note(), Some(msg::settings_consent_discard_note_c()));
    let labels = page.band_labels();
    let band = BandPart {
        layout: page.layout, labels: &labels, group: BAND_GROUP, entry: EntryId(1), uncommitted: true,
        scales: [1.0; 2], palette: palette(), danger: None, note: page.band_note(),
    };
    let done = band.rects(&m)[0];
    let note = band.note_rect(&m).expect("a note beside the lone control");
    assert!(note.x >= done.x + done.w, "to Done's right: {note:?} vs {done:?}");
    assert_eq!((note.y, note.h), (done.y, done.h), "on Done's band");
    assert!(note.x + note.w <= page.layout.action.x + page.layout.action.w, "inside the slot");
    // a pair of answers never takes a note
    let pair = [msg::settings_done_c(), msg::settings_cancel_c()];
    let paired = BandPart { labels: &pair, ..band };
    assert!(paired.note_rect(&m).is_none());
    // BACK is unchanged: the page declines it so the surface pops; the draft is not recorded
    assert_eq!(page.step(&key_down(Key::Back), &test_cx(&m), &mut mk_fx(&mut out, &mut present)), Handled::No);
    assert!(!out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::Consent(_)))), "BACK records nothing");
    // Done applies it and pops
    out.clear();
    page.band_commit(0, &mut mk_fx(&mut out, &mut present));
    assert!(out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::Consent(ConsentCmd::Record { errors: true, .. })))));
    assert!(out.iter().any(|s| matches!(s.fx, Fx::Nav(NavOp::Pop))));
    restore_consent_snapshot(saved);
}

/// The line beside Done is one line that fits the slot beside the pill, in every language
/// (measured with the shipped faces), and every followed sentence fits five lines.
#[test]
fn the_note_and_every_sentence_fit_in_every_language() {
    use plx_ui::text_view::TextView;
    let _guard = plx_base::testlock::serial();
    let layout = RouteLayout::settings_page();
    let ids = [RowId::Errors, RowId::Usage, RowId::PreviewCrash, RowId::PreviewUsage, RowId::Policy,
        RowId::ErrorsId, RowId::AnalyticsId, RowId::Delete];
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let labels = [msg::settings_done_c()];
        let band = BandPart {
            layout, labels: &labels, group: BAND_GROUP, entry: EntryId(1), uncommitted: true,
            scales: [1.0; 2], palette: palette(), danger: None, note: Some(msg::settings_consent_discard_note_c()),
        };
        let room = band.note_rect(&ShippedMeasure).map_or(0.0, |r| r.w);
        let width = ShippedMeasure.width(msg::settings_consent_discard_note_c(), theme::size::CAPTION, false);
        if width > room {
            out.push(format!("{tag}: the note is {width}px, the slot leaves {room}px"));
        }
        let frame = layout.narrative_copy_frame(true, msg::settings_privacy_title(), layout.action.y, &ShippedMeasure);
        let leading = theme::size::LABEL as f32 + theme::space::XS;
        let mut all: Vec<Option<RowId>> = ids.iter().copied().map(Some).collect();
        all.push(None);
        for id in all {
            let view = TextView::new(settings_sentence(id), theme::size::LABEL, theme::TEXT_READING)
                .leading(leading)
                .max_lines(plx_ui::follow_copy::FOLLOW_LINES + 1)
                .with_measure(&ShippedMeasure);
            let lines = (view.measure_h(layout.narrative.w) / leading).round() as usize;
            if lines > plx_ui::follow_copy::FOLLOW_LINES {
                out.push(format!("{tag} {id:?}: {lines} lines"));
            }
            if frame.y + lines as f32 * leading > layout.action.y {
                out.push(format!("{tag} {id:?}: runs into the action slot"));
            }
        }
        // the title (48, tracked) keeps whole words in the column, as the hero one had to
        for word in msg::settings_privacy_title().split_whitespace() {
            let w = ShippedMeasure.width_str(word, theme::size::DISPLAY, true);
            if w > layout.narrative.w {
                out.push(format!("{tag}: title word {word:?} is {w}px"));
            }
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}
