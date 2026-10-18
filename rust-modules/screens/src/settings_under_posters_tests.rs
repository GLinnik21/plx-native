//! Settings > Libraries > Under posters: the rows, the write door, the policy mapping, the preview
//! and the fit of every string in every shipped language.
use super::*;
use super::super::test_support::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_machine::machine::Measure;
use plx_machine::present::Present;

/// Every value the page can show: both title states over every details line.
fn every_value() -> Vec<UnderPosters> {
    let mut all = Vec::new();
    for title in [false, true] {
        for details in UnderPostersDetails::LADDER {
            all.push(UnderPosters { title, details });
        }
    }
    all
}

/// Holds the live cell on `value` for the test and puts the default back after it.
struct Live;
impl Live {
    fn set(value: UnderPosters) -> Self {
        plx_media::route::restore_under_posters(value);
        Live
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        plx_media::route::restore_under_posters(UnderPosters::DEFAULT);
    }
}

fn activate(page: &mut UnderPostersPage, id: UpId) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.activate(id.key().0, &mut fx);
    out
}

fn write_request(effects: Vec<Stamped<InnerHost>>) -> Option<(UnderPosters, std::sync::mpsc::Sender<bool>)> {
    let mut found = None;
    for event in effects {
        if let Fx::App(AppFx::Preferences(crate::registry::PreferenceCmd::Choose(
            plx_media::route::Choice::UnderPosters(v), reply))) = event.fx {
            assert!(found.is_none(), "one activation submits one write");
            found = Some((v, reply));
        }
    }
    found
}

/// The details lines whose row wears the checkmark, read off the table the page draws.
fn checked(page: &UnderPostersPage) -> Vec<UnderPostersDetails> {
    let marks: Vec<bool> = page.form.table.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.checked)).collect();
    UnderPostersDetails::LADDER.into_iter().filter(|d| {
        let i = page.form.index_of_key(UpId::Details(*d).key()).expect("every details row exists");
        marks[i]
    }).collect()
}

#[test]
fn the_form_has_a_title_switch_five_details_and_reset_with_distinct_keys() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = UnderPostersPage::new(EntryId(0));
    let mut keys = vec![UpId::Title.key().0, UpId::Reset.key().0];
    keys.extend(UnderPostersDetails::LADDER.map(|d| UpId::Details(d).key().0));
    let mut unique = keys.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 7, "title + five details + reset, each its own key");
    assert!(keys.iter().all(|k| *k < crate::registry::BAND));
    for key in keys {
        assert!(page.form.index_of_key(RowKey(key)).is_some(), "row {key} is in the form");
    }
    assert_eq!(checked(&page), vec![UnderPostersDetails::YearLength], "the default is Year · Length");
}

#[test]
fn the_title_switch_asks_the_write_door_and_waits_for_the_receipt() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let before = page.state.hash();
    let (asked, reply) = write_request(activate(&mut page, UpId::Title)).expect("a title write");
    assert_eq!(asked, UnderPosters { title: true, details: UnderPostersDetails::YearLength });
    assert!(page.state.busy && page.state.value.title, "the row shows the pick at once");
    assert_ne!(page.state.hash(), before, "a pending write belongs to replay state");
    assert!(!page.poll(), "waiting never blocks the frame");
    // The app persists, publishes, then replies.
    plx_media::route::restore_under_posters(asked);
    reply.send(true).unwrap();
    assert!(page.poll());
    assert!(!page.state.busy && page.state.value == asked);
}

#[test]
fn a_refused_write_puts_the_row_back_on_the_stored_value() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let (_, reply) = write_request(activate(&mut page, UpId::Details(UnderPostersDetails::Genre))).unwrap();
    assert_eq!(checked(&page), vec![UnderPostersDetails::Genre]);
    reply.send(false).unwrap();
    assert!(page.poll());
    assert_eq!(page.state.value, UnderPosters::DEFAULT);
    assert_eq!(checked(&page), vec![UnderPostersDetails::YearLength]);
}

#[test]
fn a_dropped_receipt_is_a_refusal() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let request = write_request(activate(&mut page, UpId::Title)).unwrap();
    drop(request);
    assert!(page.poll());
    assert_eq!(page.state.value, UnderPosters::DEFAULT);
}

#[test]
fn each_details_row_keeps_the_title_and_sends_only_its_own_change() {
    let _guard = plx_base::testlock::serial();
    let start = UnderPosters { title: true, details: UnderPostersDetails::Video };
    let _live = Live::set(start);
    for details in UnderPostersDetails::LADDER {
        let mut page = UnderPostersPage::new(EntryId(0));
        let request = write_request(activate(&mut page, UpId::Details(details)));
        if details == start.details {
            assert!(request.is_none(), "choosing the current line writes nothing");
        } else {
            assert_eq!(request.unwrap().0, UnderPosters { title: true, details });
        }
    }
}

#[test]
fn reset_restores_the_default_and_is_inert_at_the_default() {
    let _guard = plx_base::testlock::serial();
    {
        let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::None });
        let mut page = UnderPostersPage::new(EntryId(0));
        let (asked, _reply) = write_request(activate(&mut page, UpId::Reset)).expect("reset writes");
        assert_eq!(asked, UnderPosters::DEFAULT);
    }
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    assert!(write_request(activate(&mut page, UpId::Reset)).is_none(), "nothing to reset");
    assert!(!page.state.busy);
}

#[test]
fn the_page_opens_on_the_stored_value() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::AgeRatingScore });
    let page = UnderPostersPage::new(EntryId(0));
    assert!(page.state.value.title);
    assert_eq!(checked(&page), vec![UnderPostersDetails::AgeRatingScore]);
}

#[test]
fn the_policy_mapping_is_total_and_keeps_the_default() {
    assert_eq!(caption_policy_of(UnderPosters::DEFAULT), CaptionPolicy::DEFAULT);
    let pairs = [
        (UnderPostersDetails::YearLength, DetailsKind::YearLength),
        (UnderPostersDetails::Genre, DetailsKind::Genre),
        (UnderPostersDetails::Video, DetailsKind::Video),
        (UnderPostersDetails::AgeRatingScore, DetailsKind::AgeRatingScore),
        (UnderPostersDetails::None, DetailsKind::None),
    ];
    assert_eq!(pairs.len(), UnderPostersDetails::LADDER.len());
    for (stored, kind) in pairs {
        for title in [false, true] {
            assert_eq!(caption_policy_of(UnderPosters { title, details: stored }), CaptionPolicy { title, details: kind });
        }
    }
}

#[test]
fn the_band_follows_the_pick() {
    let h = |title, details| band_h(&caption_policy_of(UnderPosters { title, details }));
    assert_eq!(h(false, UnderPostersDetails::None), 0.0);
    assert_eq!(h(false, UnderPostersDetails::YearLength), 80.0);
    assert_eq!(h(true, UnderPostersDetails::None), 80.0);
    assert_eq!(h(true, UnderPostersDetails::Genre), 112.0);
}

#[test]
fn the_preview_is_captioned_by_the_same_policy() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let none = UnderPostersDetails::None;
    assert_eq!(preview_lines(UnderPosters { title: false, details: none }), [None, None]);
    let default = preview_lines(UnderPosters::DEFAULT);
    assert_eq!(default[0], None, "the default shows no title");
    assert_eq!(default[1].as_deref(), Some("2019 \u{b7} 2h 14m"));
    let both = preview_lines(UnderPosters { title: true, details: UnderPostersDetails::Genre });
    assert_eq!(both[0].as_deref(), Some("Movie"));
    assert_eq!(both[1].as_deref(), Some("Drama"));
    assert_eq!(preview_lines(UnderPosters { title: false, details: UnderPostersDetails::Video })[1].as_deref(), Some("4K \u{b7} HDR"));
    assert_eq!(preview_lines(UnderPosters { title: false, details: UnderPostersDetails::AgeRatingScore })[1].as_deref(),
        Some("PG-13 \u{b7} 7.8"));
}

#[test]
fn the_root_row_opens_the_page() {
    let _guard = plx_base::testlock::serial();
    let mut form = FormTable::<RootId, Action, SettingsPage>::new(crate::registry::BAND);
    form.table.compact = false;
    form.set(root_form(&text_fit_inputs()), None);
    let i = form.index_of_key(RootId::UnderPosters.key()).expect("the row is on a signed-in root");
    assert_eq!(form.activate(i), Some(plx_ui::form::Activation::Push(SettingsPage::UnderPosters)));
    let mut signed_out = FormTable::<RootId, Action, SettingsPage>::new(crate::registry::BAND);
    signed_out.set(root_form(&RootInputs { signed_in: false, ..text_fit_inputs() }), None);
    assert!(signed_out.index_of_key(RootId::UnderPosters.key()).is_none(), "Libraries is a signed-in section");
}

fn text_fit_inputs() -> RootInputs {
    RootInputs {
        signed_in: true, multi_user: false, library_count: 0,
        auto_sign_in: false, trailer_autoplay: true, resolution: UiResolution::Auto, resolution_offered: false,
        language: plx_platform::i18n::Preference::System, plaintext: Vec::new(),
    }
}

/// **Every string fits, in every shipped language**: the page's rows, the root row beside every
/// value it can read, and the preview's block clear of the copy above it.
#[test]
fn every_under_posters_string_fits_in_every_language() {
    use plx_ui::text_view::TextView;
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let layout = RouteLayout::screen();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        let tag = language.tag();
        for value in every_value() {
            let mut page = FormTable::<UpId, UpAction, SettingsPage>::new(crate::registry::BAND);
            page.table.compact = false;
            page.set(under_posters_form(value), None);
            out.extend(page.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} under posters {value:?}")));
        }
        // the narrative copy ends above the preview
        let title = plx_platform::i18n::msg::under_posters_title();
        let frame = layout.narrative_copy_frame(true, title, layout.action.y, &ShippedMeasure);
        let copy_h = TextView::new(plx_platform::i18n::msg::under_posters_copy(), theme::size::LABEL, theme::TEXT_READING)
            .leading(theme::size::LABEL as f32 + theme::space::XS)
            .max_lines(12)
            .with_measure(&ShippedMeasure)
            .measure_h(layout.narrative.w);
        if frame.y + copy_h + theme::space::MD > preview_top() {
            out.push(format!("{tag}: the copy ends at {:.0}, under the preview at {:.0}", frame.y + copy_h, preview_top()));
        }
        // the sample title fits inside its placeholder on one line
        let w = ShippedMeasure.width_str(plx_platform::i18n::msg::under_posters_sample_title(), theme::size::CAPTION, true);
        if w > PREVIEW_W - 2.0 * theme::space::SM {
            out.push(format!("{tag}: the sample title is {w:.0}px, wider than its placeholder"));
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

#[test]
fn the_preview_stays_inside_the_narrative_column() {
    let narrative = RouteLayout::screen().narrative;
    let width = PREVIEW_TILES as f32 * PREVIEW_W + (PREVIEW_TILES - 1) as f32 * PREVIEW_GAP;
    assert!(width <= narrative.w);
    assert!(preview_top() + PREVIEW_H + PREVIEW_UNDER <= narrative.y + narrative.h);
    // the Library's proportions, not a private set
    let ratio = |w: f32, gap: f32| gap / w;
    assert!((ratio(PREVIEW_W, PREVIEW_GAP) - ratio(plx_ui::consts::CARD_W, plx_ui::consts::GAP)).abs() < 1e-4);
}

#[test]
fn the_page_draws_for_every_value_without_a_painter() {
    let _guard = plx_base::testlock::serial();
    for value in every_value() {
        let _live = Live::set(value);
        let mut page = UnderPostersPage::new(EntryId(0));
        let c = cx(None);
        let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
        <UnderPostersPage as Screen<InnerHost>>::draw(&mut page, &mut f);
    }
}

#[test]
fn the_page_has_its_own_screen_name_and_probe() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = UnderPostersPage::new(EntryId(0));
    assert_eq!(<UnderPostersPage as Screen<InnerHost>>::name(&page), "under-posters");
    let mut probe = String::new();
    page.state.probe(&mut probe);
    assert!(probe.starts_with("under_posters "), "{probe}");
    let mut arg = String::new();
    SettingsPage::UnderPosters.probe(&mut arg);
    assert_eq!(arg, "under-posters");
}
