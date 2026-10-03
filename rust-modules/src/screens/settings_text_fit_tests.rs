//! **Every Settings row's title and sub-line fits its row, in every shipped language.**
//!
//! A table row elides both lines to its label column (`TableView::label_width`), so a translation
//! that is a few characters too long does not break anything a unit test would notice — it just
//! ends in `…` on the television. The Belarusian root shipped exactly that way ("Неабавязковыя
//! справаздачы, звесткі пра прыватнасць і лака…"), invisible in the simulator because its newer
//! SDL_ttf sums fractional advances while the device rounds each glyph to a whole pixel.
//! [`plx_base::fontcov::advances::ShippedMeasure`] measures the shipped faces the device's way, so
//! these assertions are about the television, not the Mac. The Legal notices and Privacy & data
//! pages carry the same guard in their own modules (`legal.rs`, `consent_text_fit_tests.rs`).

use super::*;
use plx_base::fontcov::advances::ShippedMeasure;
use crate::i18n::{language_on_this_thread_for_test, SHIPPED};
use crate::ui::route_screen::RouteLayout;
use crate::ui::table::TableView;

/// The app-owned findings for `table` at the Settings column's width.
fn overflowing(page: &str, table: &TableView, out: &mut Vec<String>) {
    let frame_w = RouteLayout::screen().sectioned_table().w;
    out.extend(table.app_fit_failures(frame_w, page));
}

/// The baseline `RootInputs` each variant below changes one field of (a sum over fields, not a
/// product).
fn base_root_inputs() -> RootInputs {
    RootInputs {
        signed_in: true, multi_user: false, library_count: 0,
        auto_sign_in: false, trailer_autoplay: true,
        language: crate::i18n::Preference::System, plaintext: Vec::new(),
    }
}

/// Build `inputs` through the real [`root_form`] and collect the app-owned findings.
fn overflowing_root(tag: &str, inputs: &RootInputs, out: &mut Vec<String>) {
    let mut form = FormTable::<RootId, Action, SettingsPage>::new(super::super::registry::BAND);
    form.table.compact = false;
    form.set(root_form(inputs), None);
    overflowing(tag, &form.table, out);
}

/// The Settings root in every `RootInputs` shape, and the Language page, through the real builders.
#[test]
fn every_settings_row_fits_its_column_in_every_language() {
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        overflowing(&format!("{tag} root (signed out)"), &RootPage::new(EntryId(0), test_support::cx(None).views).form.table, &mut out);
        overflowing(&format!("{tag} language"), &LanguagePage::new(EntryId(0)).form.table, &mut out);

        overflowing_root(&format!("{tag} root (signed in)"), &base_root_inputs(), &mut out);
        for signed_in in [true, false] {
            overflowing_root(&format!("{tag} signed_in={signed_in}"),
                &RootInputs { signed_in, ..base_root_inputs() }, &mut out);
        }
        for multi_user in [true, false] {
            overflowing_root(&format!("{tag} multi_user={multi_user}"),
                &RootInputs { multi_user, ..base_root_inputs() }, &mut out);
        }
        for auto_sign_in in [true, false] {
            overflowing_root(&format!("{tag} auto_sign_in={auto_sign_in}"),
                &RootInputs { auto_sign_in, ..base_root_inputs() }, &mut out);
        }
        for trailer_autoplay in [true, false] {
            overflowing_root(&format!("{tag} trailer_autoplay={trailer_autoplay}"),
                &RootInputs { trailer_autoplay, ..base_root_inputs() }, &mut out);
        }
        for &library_count in &[0i64, 1, 88] {
            overflowing_root(&format!("{tag} library_count={library_count}"),
                &RootInputs { library_count, ..base_root_inputs() }, &mut out);
        }
        for language_pref in LANGUAGES {
            overflowing_root(&format!("{tag} language={language_pref:?}"),
                &RootInputs { language: language_pref, ..base_root_inputs() }, &mut out);
        }
        // A machine name is server text (exempt); the fallback "Plex server" and the detail and
        // toggle word beside either are app text.
        for named in [true, false] {
            for on in [true, false] {
                for connected in [true, false] {
                    overflowing_root(&format!("{tag} plaintext named={named} on={on} connected={connected}"),
                        &RootInputs { plaintext: vec![PlaintextRowInput {
                            machine: ServerMachineId("machine".into()),
                            name: if named { "some-server-machine-name-that-is-very-long".into() }
                                  else { crate::i18n::msg::settings_plaintext_server().into() },
                            named, on, connected,
                        }], ..base_root_inputs() }, &mut out);
                }
            }
        }
    }
    crate::ui::table::assert_no_fit_failures(&out);
}

/// The measure itself: whole pixels per glyph from each shipped face's own metrics, and a longer
/// string never measures narrower.
#[test]
fn the_shipped_measure_sums_whole_pixel_advances_like_the_device() {
    use plx_machine::machine::Measure;
    let m = ShippedMeasure;
    let a = m.width_str("Privacy & data", theme::size::CAPTION, false);
    let b = m.width_str("Privacy & data, and more", theme::size::CAPTION, false);
    assert!(a > 0.0 && b > a);
    assert_eq!(a.fract(), 0.0, "whole pixels per glyph");
    assert!(m.width_str("Прыватнасць", theme::size::CAPTION, false) > 0.0, "Cyrillic is mapped");
    assert!(m.width_str("Settings", theme::size::HEADLINE, true) > m.width_str("Settings", theme::size::HEADLINE, false),
        "the bold face is its own metrics");
}
