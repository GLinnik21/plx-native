//! **Every Settings row's title and sub-line fits its row, in every shipped language.**
//!
//! A table row elides both lines to its label column (`TableView::label_width`), so a translation
//! that is a few characters too long does not break anything a unit test would notice — it just
//! ends in `…` on the television. The Belarusian root shipped exactly that way ("Неабавязковыя
//! справаздачы, звесткі пра прыватнасць і лака…"), invisible in the simulator because its newer
//! SDL_ttf sums fractional advances while the device rounds each glyph to a whole pixel.
//! [`crate::fontcov::advances::ShippedMeasure`] measures the shipped faces the device's way, so
//! these assertions are about the television, not the Mac. The Legal notices and Privacy & data
//! pages carry the same guard in their own modules (`legal.rs`, `consent_text_fit_tests.rs`).

use super::*;
use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
use crate::i18n::{language_on_this_thread_for_test, msg, Preference};
use crate::ui::route_screen::RouteLayout;
use crate::ui::table::{Row, TableView};

const LANGUAGES: [Preference; 3] = [Preference::En, Preference::Es, Preference::Be];

/// Every row of `table` whose title or sub-line would be elided at the Settings column's width.
fn overflowing(page: &str, table: &TableView, out: &mut Vec<String>) {
    let frame_w = RouteLayout::screen().sectioned_table().w;
    out.extend(table.elided_rows(frame_w, &ShippedMeasure, HEADROOM).into_iter().map(|e| format!("{page}: {e}")));
}

/// The root rows only a signed-in (and multi-user) account sees, built exactly as
/// `RootPage::rebuild` builds them; the signed-out rows come from a real `RootPage`.
fn signed_in_root_rows() -> TableView {
    let mut table = TableView::new();
    table.compact = false;
    let rows = [
        Row::new(msg::settings_libraries_title()).detail(msg::settings_libraries_detail())
            .value(msg::settings_libraries_count(88)).chevron(true),
        Row::new(msg::settings_auto_sign_in_title()).detail(msg::settings_auto_sign_in_detail()).toggle(false),
        Row::new(msg::settings_auto_sign_in_title()).detail(msg::settings_auto_sign_in_detail()).toggle(true),
        Row::new(msg::settings_trailers_title()).detail(msg::settings_trailers_detail()).toggle(false),
        Row::new(msg::settings_trailers_title()).detail(msg::settings_trailers_detail()).toggle(true),
        Row::new(msg::settings_audio_title()).detail(msg::settings_audio_detail()).chevron(true),
    ];
    table.set_sections(vec![rows.into_iter().fold(Section::new(""), Section::row)], 0, false);
    table
}

#[test]
fn every_settings_row_fits_its_column_in_every_language() {
    let mut out = Vec::new();
    for language in LANGUAGES {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        overflowing(&format!("{tag} root"), &RootPage::new(EntryId(0), test_support::cx(None).views).table, &mut out);
        overflowing(&format!("{tag} root (signed in)"), &signed_in_root_rows(), &mut out);
        overflowing(&format!("{tag} language"), &LanguagePage::new(EntryId(0)).table, &mut out);
    }
    assert!(out.is_empty(), "rows the television would end in an ellipsis:\n  {}", out.join("\n  "));
}

/// Owner report (Belarusian UI): a long VALUE squeezed the primary label of the Video Playback
/// page's Direct Play row — the label column was whatever the unelided value left over. The row's
/// label is the primary read and keeps its natural width; the value gives way first, ending in
/// an ellipsis. Built exactly as `preferences::PreferencesPage::rebuild` builds its field rows,
/// with every Direct Play value the picker can set.
#[test]
fn a_long_value_elides_before_the_settings_label_it_trails() {
    use crate::fontcov::advances::ShippedMeasure as M;
    use crate::ui::machine::Measure;
    let frame_w = RouteLayout::screen().sectioned_table().w;
    let mut out = Vec::new();
    let mut squeezed_values = 0;
    for language in LANGUAGES {
        let _guard = language_on_this_thread_for_test(language);
        for value in [msg::settings_playback_auto(), msg::settings_playback_forced(), msg::settings_playback_disabled()] {
            let mut table = TableView::new();
            table.compact = false;
            let rows = [
                Row::new(msg::settings_playback_quality()).value(msg::settings_audio_not_set()).chevron(true),
                Row::new(msg::settings_playback_direct_play()).value(value).chevron(true),
            ];
            table.set_sections(vec![rows.into_iter().fold(Section::new(""), Section::row)], 0, false);
            overflowing(&format!("{} playback ({value})", language.tag()), &table, &mut out);
            for (i, row) in table.sections[0].rows.iter().enumerate() {
                let cols = table.row_columns(row, frame_w, &M);
                let natural = M.width_str(value, theme::size::LABEL, true);
                if i == 1 && cols.value_w < natural { squeezed_values += 1; }
                assert!(cols.value_w > 0.0, "{}: a value keeps a visible slot", language.tag());
            }
        }
    }
    assert!(out.is_empty(), "labels a long value squeezed into an ellipsis:\n  {}", out.join("\n  "));
    assert!(squeezed_values > 0, "the premise: Belarusian's Force value does not fit beside its label");
}

/// The measure itself: whole pixels per glyph from each shipped face's own metrics, and a longer
/// string never measures narrower.
#[test]
fn the_shipped_measure_sums_whole_pixel_advances_like_the_device() {
    use crate::ui::machine::Measure;
    let m = ShippedMeasure;
    let a = m.width_str("Privacy & data", theme::size::CAPTION, false);
    let b = m.width_str("Privacy & data, and more", theme::size::CAPTION, false);
    assert!(a > 0.0 && b > a);
    assert_eq!(a.fract(), 0.0, "whole pixels per glyph");
    assert!(m.width_str("Прыватнасць", theme::size::CAPTION, false) > 0.0, "Cyrillic is mapped");
    assert!(m.width_str("Settings", theme::size::HEADLINE, true) > m.width_str("Settings", theme::size::HEADLINE, false),
        "the bold face is its own metrics");
}
