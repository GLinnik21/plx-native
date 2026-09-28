//! **Every Privacy & data row fits its column, in every shipped language.** The same guard as
//! `settings_text_fit_tests.rs` for the Settings root, owned here because a screen never names a
//! sibling screen: rows elide to `TableView::label_width`, and the device rounds each glyph to a
//! whole pixel, so [`crate::fontcov::advances::ShippedMeasure`] measures what the television draws.

use super::*;
use super::test_support::*;
use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
use crate::i18n::{language_on_this_thread_for_test, Preference};
use crate::ui::route_screen::RouteLayout;

#[test]
fn every_privacy_row_fits_its_column_in_every_language() {
    let frame_w = RouteLayout::screen().sectioned_table().w;
    let m = FixtureMeasure;
    let mut out = Vec::new();
    for language in [Preference::En, Preference::Es, Preference::Be] {
        let _guard = language_on_this_thread_for_test(language);
        let cx = test_cx(&m);
        let (mut sink_out, mut present) = sink();
        let page = ConsentPage::settings(EntryId(0), &cx, &mut mk_fx(&mut sink_out, &mut present));
        let tag = language.tag();
        out.extend(page.table.elided_rows(frame_w, &ShippedMeasure, HEADROOM).into_iter().map(|e| format!("{tag}: {e}")));
    }
    assert!(out.is_empty(), "rows the television would end in an ellipsis:\n  {}", out.join("\n  "));
}
