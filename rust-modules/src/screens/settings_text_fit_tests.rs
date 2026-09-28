//! **Every Settings row's title and sub-line fits its row, in every shipped language.**
//!
//! A table row elides both lines to its label column (`TableView::label_width`), so a translation
//! that is a few characters too long does not break anything a unit test would notice — it just
//! ends in `…` on the television. The Belarusian root shipped exactly that way ("Неабавязковыя
//! справаздачы, звесткі пра прыватнасць і лака…"), invisible in the simulator because its newer
//! SDL_ttf sums fractional advances while the device rounds each glyph to a whole pixel.
//! [`crate::fontcov::advances::ShippedMeasure`] measures the shipped faces the device's way, so
//! these assertions are about the television, not the Mac.

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
        overflowing(&format!("{tag} legal"), crate::screens::legal::LegalIndex::new(EntryId(0)).table_for_test(), &mut out);
        let cx = test_support::cx(None);
        let (mut sink, mut present) = (Vec::new(), crate::ui::present::Present::new());
        let mut fx = Effects::new(&mut sink, MachineId::Instance(InstanceId(0)), &mut present);
        let privacy = crate::screens::consent::ConsentPage::settings(EntryId(0), &cx, &mut fx);
        overflowing(&format!("{tag} privacy"), privacy.table_for_test(), &mut out);
    }
    assert!(out.is_empty(), "rows the television would end in an ellipsis:\n  {}", out.join("\n  "));
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
