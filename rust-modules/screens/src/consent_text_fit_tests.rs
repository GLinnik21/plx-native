//! **Every Privacy & data row fits its column, in every shipped language.** The same guard as
//! `settings_text_fit_tests.rs` for the Settings root, owned here because a screen never names a
//! sibling screen: rows elide to `TableView::label_width`, and the device rounds each glyph to a
//! whole pixel, so [`plx_base::fontcov::advances::ShippedMeasure`] measures what the television draws.

use super::*;
use super::test_support::*;
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_ui::route_screen::RouteLayout;

/// Every privacy row, built by the real `ConsentPage::settings`, through `TableView::fit_report`.
#[test]
fn every_privacy_row_fits_its_column_in_every_language() {
    let frame_w = RouteLayout::screen().sectioned_table().w;
    let m = FixtureMeasure;
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let cx = test_cx(&m);
        let (mut sink_out, mut present) = sink();
        let page = ConsentPage::settings(EntryId(0), &cx, &mut mk_fx(&mut sink_out, &mut present));
        let tag = language.tag();
        out.extend(page.form.table.app_fit_failures(frame_w, tag));
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

/// A title may wrap between words, but a word wider than the narrative column is split in the
/// middle by `narrative_title`. The proposed Russian "Конфиденциальность" did exactly that.
#[test]
fn every_privacy_title_word_fits_the_narrative_column_in_every_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_machine::machine::Measure;
    let column = RouteLayout::screen().narrative.w;
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        for word in plx_platform::i18n::msg::settings_privacy_title().split_whitespace() {
            let width = ShippedMeasure.width_str(word, plx_ui::theme::size::HERO, true);
            assert!(width <= column,
                "{}: privacy title word {word:?} is {width}px in a {column}px column", language.tag());
        }
    }
}

#[test]
fn russian_privacy_document_titles_fit_without_splitting_words() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_machine::machine::Measure;
    let _guard = language_on_this_thread_for_test(plx_platform::i18n::Preference::Ru);
    let column = RouteLayout::screen().narrative.w;
    // The policy opens through both Legal and Privacy & data; both reader titles must fit.
    for title in [plx_platform::i18n::msg::settings_legal_privacy_title(),
        plx_platform::i18n::msg::settings_consent_policy()] {
        for word in title.split_whitespace() {
            let width = ShippedMeasure.width_str(word, plx_ui::theme::size::HERO, true);
            assert!(width <= column,
                "Russian privacy document title word {word:?} is {width}px in a {column}px column");
        }
    }
}
