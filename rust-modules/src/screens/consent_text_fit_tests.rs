//! **Every Privacy & data row fits its column, in every shipped language.** The same guard as
//! `settings_text_fit_tests.rs` for the Settings root, owned here because a screen never names a
//! sibling screen: rows elide to `TableView::label_width`, and the device rounds each glyph to a
//! whole pixel, so [`plx_base::fontcov::advances::ShippedMeasure`] measures what the television draws.

use super::*;
use super::test_support::*;
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use crate::ui::route_screen::RouteLayout;

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
    crate::ui::table::assert_no_fit_failures(&out);
}
