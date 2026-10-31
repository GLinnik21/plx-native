//! Settings > Libraries > Under posters: the rows, the write door, the policy mapping, the preview
//! and the fit of every string in every shipped language.
use super::*;
use super::super::test_support::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_machine::present::Present;

/// Every value the page can show: both title states over every details line.
fn every_value() -> Vec<UnderPosters> {
    let mut all = Vec::new();
    for title in [false, true] {
        for details in UnderPostersDetails::LADDER {
            all.push(UnderPosters { title, details, sort_value: false });
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

/// The details lines whose row wears the checkmark, read off the picker's table.
fn checked(page: &DetailsPage) -> Vec<UnderPostersDetails> {
    let marks: Vec<bool> = page.form.table.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.checked)).collect();
    UnderPostersDetails::LADDER.into_iter().filter(|d| {
        let i = page.form.index_of_key(DetailsId(*d).key()).expect("every details row exists");
        marks[i]
    }).collect()
}

fn pick(page: &mut DetailsPage, d: UnderPostersDetails) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.activate(DetailsId(d).key().0, &mut fx);
    out
}

fn polled(page: &mut DetailsPage) -> (bool, Vec<Stamped<InnerHost>>) {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let settled = page.poll(&mut fx);
    (settled, out)
}

fn pops(effects: &[Stamped<InnerHost>]) -> bool {
    effects.iter().any(|e| matches!(e.fx, Fx::Nav(plx_machine::machine::NavOp::Pop)))
}

#[test]
fn the_page_has_a_title_switch_a_details_row_and_reset_with_distinct_keys() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = UnderPostersPage::new(EntryId(0));
    let keys = [UpId::Title.key().0, UpId::Details.key().0, UpId::SortValue.key().0, UpId::Reset.key().0];
    let mut unique = keys.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 4, "each row has its own key");
    assert!(keys.iter().all(|k| *k < crate::registry::BAND));
    let rows: Vec<usize> = keys.iter().map(|k| page.form.index_of_key(RowKey(*k)).expect("the row is in the form")).collect();
    assert_eq!(rows, vec![0, 1, 2, 3], "Title, Details, Sort value, Reset in that order");
}

#[test]
fn the_details_row_reads_the_current_line_and_opens_the_picker() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let i = page.form.index_of_key(UpId::Details.key()).unwrap();
    let value = page.form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i).and_then(|r| r.value.clone());
    assert_eq!(value.as_deref(), Some("Year \u{b7} Length"));
    assert_eq!(page.form.activate(i), Some(plx_ui::form::Activation::Push(SettingsPage::UnderPostersDetails)));
    let out = activate(&mut page, UpId::Details);
    assert!(out.iter().any(|e| matches!(e.fx, Fx::Nav(plx_machine::machine::NavOp::Push(SettingsPage::UnderPostersDetails)))));
    assert!(write_request(out).is_none(), "opening the picker writes nothing");
}

#[test]
fn the_title_switch_asks_the_write_door_and_waits_for_the_receipt() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let before = page.state.hash();
    let (asked, reply) = write_request(activate(&mut page, UpId::Title)).expect("a title write");
    assert_eq!(asked, UnderPosters { title: true, details: UnderPostersDetails::YearLength, sort_value: true });
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
fn the_picker_lists_nothing_first_each_line_with_its_sample_and_checks_the_current() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = DetailsPage::new(EntryId(0));
    let rows: Vec<(String, Option<String>)> = page.form.table.sections.iter().flat_map(|s| s.rows.iter())
        .map(|r| (r.label.to_string(), r.value.clone())).collect();
    let dot = '\u{b7}';
    assert_eq!(rows, vec![
        ("Nothing".to_string(), None),
        (format!("Year {dot} Length"), Some(format!("2019 {dot} 2h 14m"))),
        ("Genre".to_string(), Some("Drama".to_string())),
        ("Video".to_string(), Some(format!("4K {dot} HDR"))),
        (format!("Age rating {dot} Score"), Some(format!("PG-13 {dot} 7.8"))),
    ]);
    assert_eq!(checked(&page), vec![UnderPostersDetails::YearLength]);
}

/// **Every sample on the picker comes from ONE source: the preview's middle poster** (design review
/// C1). The rows said "2019 · 2h 14m" and "PG-13 · 7.8" (invented) beside a preview that read
/// "2022 · 6m" from the library's own poster; a kind the poster has no fact for samples what the
/// tile says instead (its identity line), the same words the preview draws.
#[test]
fn the_pickers_samples_are_worded_from_the_previews_middle_poster() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut middle = poster("Bravo", 2022, "Western");
    middle.rating = "PG".into();
    middle.dur_ns = 6 * 60_000_000_000;
    let items = vec![poster("Alpha", 1999, "Noir"), middle, poster("Charlie", 2011, "Comedy")];
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(items);
    let c = cx_with(None, snapshot.view());
    let mut page = DetailsPage::new(EntryId(0));
    let mut out = Vec::new();
    let mut present = Present::new();
    page.step(&ScreenEvent::Mount, &c, &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    let rows: Vec<(String, Option<String>)> = page.form.table.sections.iter().flat_map(|s| s.rows.iter())
        .map(|r| (r.label.to_string(), r.value.clone())).collect();
    let dot = '\u{b7}';
    assert_eq!(rows, vec![
        ("Nothing".to_string(), None),
        (format!("Year {dot} Length"), Some(format!("2022 {dot} 6m"))),
        ("Genre".to_string(), Some("Western".to_string())),
        ("Video".to_string(), Some("2022".to_string())),
        (format!("Age rating {dot} Score"), Some("PG".to_string())),
    ], "the middle poster's own facts, and for a kind it has none for the identity line the tile says instead (its year), never an invented fact");
    // …and each sample is what the preview captions that poster with under the same choice
    let posters = preview_posters(&c);
    for d in picker_order() {
        let value = page.form.table.sections.iter().flat_map(|s| s.rows.iter())
            .find(|r| r.label == details_label(d)).and_then(|r| r.value.clone());
        assert_eq!(value, preview_lines(UnderPosters { title: false, details: d, sort_value: false }, lifted_poster(&posters))[1], "{d:?}");
    }
}

#[test]
fn a_pick_keeps_the_title_writes_once_and_pops_when_the_receipt_is_durable() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Video, sort_value: false });
    let mut page = DetailsPage::new(EntryId(0));
    let effects = pick(&mut page, UnderPostersDetails::Genre);
    assert!(!pops(&effects), "the page waits for the receipt");
    let (asked, reply) = write_request(effects).expect("a write");
    assert_eq!(asked, UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false });
    assert_eq!(checked(&page), vec![UnderPostersDetails::Genre], "the row shows the pick at once");
    assert!(!polled(&mut page).0, "waiting never blocks the frame");
    plx_media::route::restore_under_posters(asked);
    reply.send(true).unwrap();
    let (settled, out) = polled(&mut page);
    assert!(settled && pops(&out), "a durable write pops the picker");
}

#[test]
fn a_refused_pick_puts_the_checkmark_back_and_stays() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = DetailsPage::new(EntryId(0));
    let (_, reply) = write_request(pick(&mut page, UnderPostersDetails::Genre)).unwrap();
    reply.send(false).unwrap();
    let (settled, out) = polled(&mut page);
    assert!(settled && !pops(&out));
    assert_eq!(page.state.value, UnderPosters::DEFAULT);
    assert_eq!(checked(&page), vec![UnderPostersDetails::YearLength]);
}

#[test]
fn choosing_the_checked_line_pops_without_a_write() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters { title: false, details: UnderPostersDetails::Video, sort_value: false });
    let mut page = DetailsPage::new(EntryId(0));
    let effects = pick(&mut page, UnderPostersDetails::Video);
    assert!(pops(&effects));
    assert!(write_request(effects).is_none());
}

#[test]
fn the_pickers_preview_follows_the_focused_choice() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = DetailsPage::new(EntryId(0));
    assert_eq!(page.state.focused(), UnderPostersDetails::YearLength);
    page.state.sel = DetailsId(UnderPostersDetails::Genre).key();
    assert_eq!(page.state.focused(), UnderPostersDetails::Genre);
    assert_eq!(page.state.value.details, UnderPostersDetails::YearLength, "focus is not a pick");
}

/// Three posters whose genre and length say different things: a title/details caption words the
/// middle one's genre ("Western"), a sort value words its length ("2h 14m").
fn captioned_posters() -> Vec<PmsMovie> {
    let minutes = |n: i64| n * 60_000_000_000;
    vec![
        PmsMovie { dur_ns: minutes(98), ..poster("Alpha", 1999, "Noir") },
        PmsMovie { dur_ns: minutes(134), ..poster("Bravo", 2004, "Western") },
        PmsMovie { dur_ns: minutes(121), ..poster("Charlie", 2011, "Comedy") },
    ]
}

/// The lines the page's preview captions wear, read off the label blocks of one draw of the page:
/// the page's own rows are not label blocks, so a row's sample cannot stand for the preview.
fn previewed_lines<S: Screen<InnerHost>>(page: &mut S) -> Vec<String> {
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(captioned_posters());
    let c = cx_with(None, snapshot.view());
    let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
    let runs = plx_ui::draw_census::capture_runs(|| page.draw(&mut f));
    plx_ui::draw_census::label_lines(&runs)
}

#[test]
fn the_pickers_preview_follows_the_focused_choice_with_the_sort_value_on() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::YearLength, sort_value: true });
    let mut page = DetailsPage::new(EntryId(0));
    page.state.sel = DetailsId(UnderPostersDetails::Genre).key();
    let genre = previewed_lines(&mut page);
    assert!(genre.iter().any(|l| l == "Western"), "the focused Genre choice is what the preview words: {genre:?}");
    assert!(previewed_values(&mut page).is_empty(), "the value line is not the picker's preview");
    page.state.sel = DetailsId(UnderPostersDetails::None).key();
    let none = previewed_lines(&mut page);
    assert!(!none.iter().any(|l| l == "Western"), "the None choice words no details line: {none:?}");
    assert!(none.iter().any(|l| l == "Bravo"), "the Title switch still shows the title: {none:?}");
}

/// The sort value lines one draw of the page puts on screen, `(text, bold)`: what the Sort value
/// row's preview shows under the posters (the page's own rows are not at the caption rung's weight pair).
fn previewed_values<S: Screen<InnerHost>>(page: &mut S) -> Vec<(String, bool)> {
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(captioned_posters());
    let c = cx_with(None, snapshot.view());
    let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
    let runs = plx_ui::draw_census::capture_runs(|| page.draw(&mut f));
    let wanted: Vec<String> = captioned_posters().iter()
        .map(|m| crate::registry::sort_info::label(m, "year", None).value.unwrap().text).collect();
    runs.into_iter().filter(|run| wanted.contains(&run.text) && run.size == plx_ui::theme::size::CAPTION && run.ink[3] > 0.2)
        .map(|run| (run.text, run.bold)).collect()
}

#[test]
fn the_pages_preview_shows_the_value_line_only_while_the_sort_value_row_has_focus() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: true });
    let mut page = UnderPostersPage::new(EntryId(0));
    for row in [UpId::Title, UpId::Details, UpId::Reset] {
        page.state.sel = row.key();
        let lines = previewed_lines(&mut page);
        assert!(lines.iter().any(|l| l == "Western"), "{row:?} focused: the Title/Details caption: {lines:?}");
        assert!(previewed_values(&mut page).is_empty(), "{row:?} focused: no value line");
    }
    page.state.sel = UpId::SortValue.key();
    let values = previewed_values(&mut page);
    assert!(values.contains(&("2004".to_owned(), true)), "the lifted poster wears its year bold: {values:?}");
    assert!(values.contains(&("1999".to_owned(), false)) && values.contains(&("2011".to_owned(), false)),
        "the others wear theirs regular: {values:?}");
    let lines = previewed_lines(&mut page);
    assert!(!lines.iter().any(|l| l == "Western"), "the Sort value row focused: not the Title/Details caption: {lines:?}");
}

#[test]
fn a_sort_value_row_with_the_switch_off_previews_the_title_and_details_caption() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false });
    let mut page = UnderPostersPage::new(EntryId(0));
    page.state.sel = UpId::SortValue.key();
    let lines = previewed_lines(&mut page);
    assert!(lines.iter().any(|l| l == "Western"), "{lines:?}");
    assert!(previewed_values(&mut page).is_empty());
}

#[test]
fn reset_restores_the_default_and_is_inert_at_the_default() {
    let _guard = plx_base::testlock::serial();
    {
        let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false });
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
fn the_picker_opens_on_the_stored_value() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::AgeRatingScore, sort_value: false });
    let page = DetailsPage::new(EntryId(0));
    assert_eq!(checked(&page), vec![UnderPostersDetails::AgeRatingScore]);
    assert_eq!(page.state.sel, DetailsId(UnderPostersDetails::AgeRatingScore).key(), "the cursor starts on the checked line");
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
            assert_eq!(caption_policy_of(UnderPosters { title, details: stored, sort_value: false }), CaptionPolicy { title, details: kind });
        }
    }
}

#[test]
fn the_band_follows_the_pick() {
    let h = |title, details| band_h(&caption_policy_of(UnderPosters { title, details, sort_value: false }));
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
    assert_eq!(preview_lines(UnderPosters { title: false, details: none, sort_value: false }, None), [None, None]);
    let default = preview_lines(UnderPosters::DEFAULT, None);
    assert_eq!(default[0], None, "the default shows no title");
    assert_eq!(default[1].as_deref(), Some("2019 \u{b7} 2h 14m"));
    let both = preview_lines(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false }, None);
    assert_eq!(both[0].as_deref(), Some("Movie"));
    assert_eq!(both[1].as_deref(), Some("Drama"));
    assert_eq!(preview_lines(UnderPosters { title: false, details: UnderPostersDetails::Video, sort_value: false }, None)[1].as_deref(), Some("4K \u{b7} HDR"));
    assert_eq!(preview_lines(UnderPosters { title: false, details: UnderPostersDetails::AgeRatingScore, sort_value: false }, None)[1].as_deref(),
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
        language: plx_platform::i18n::Preference::System,
        under_posters: plx_plex::plex::session::UnderPosters::DEFAULT, plaintext: Vec::new(),
    }
}

/// The Under posters row of the Settings root as a viewer reads it: `(value, sub-line)`.
fn root_row_text(inputs: &RootInputs) -> (Option<String>, String) {
    let mut form = FormTable::<RootId, Action, SettingsPage>::new(crate::registry::BAND);
    form.table.compact = false;
    form.set(root_form(inputs), None);
    let i = form.index_of_key(RootId::UnderPosters.key()).expect("the row is on a signed-in root");
    let row = form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i).expect("the row exists");
    (row.value.clone(), row.detail.clone())
}

/// **S18: the Under posters row reads its setting like its siblings do, and its sub-line says what
/// it governs.** Favorite libraries reads a count and Language its name; this row used to read
/// nothing, and "What shows under a poster." named no poster. One rule, `summary`, words the value
/// from the stored setting: the details line the page's Details row reads, and "Title" when the
/// title is all there is (never "Nothing" beside a poster that wears its name).
#[test]
fn the_root_row_reads_the_setting_and_names_the_poster_it_governs() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let read = |title, details| root_row_text(&RootInputs { under_posters: UnderPosters { title, details, sort_value: false }, ..text_fit_inputs() });
    let (value, sub) = read(false, UnderPostersDetails::YearLength);
    assert_eq!(value.as_deref(), Some("Year \u{b7} Length"), "the default reads its details line");
    assert_eq!(sub, "Under posters, for movies and TV shows.");
    assert_eq!(read(true, UnderPostersDetails::YearLength).0.as_deref(), Some("Year \u{b7} Length"), "the title is the page's own row");
    assert_eq!(read(true, UnderPostersDetails::Genre).0.as_deref(), Some("Genre"));
    assert_eq!(read(false, UnderPostersDetails::Video).0.as_deref(), Some("Video"));
    assert_eq!(read(true, UnderPostersDetails::None).0.as_deref(), Some("Title"));
    assert_eq!(read(false, UnderPostersDetails::None).0.as_deref(), Some("Nothing"));
    // every value the setting can hold reads something
    for value in every_value() {
        let text = root_row_text(&RootInputs { under_posters: value, ..text_fit_inputs() }).0;
        assert!(text.as_deref().is_some_and(|t| !t.is_empty()), "{value:?} reads nothing");
    }
}

/// The value is read from the STORED setting when the root is built, so a return from the page (or
/// from the picker) shows the pick: the root page reads the live cell, not a copy of the default.
#[test]
fn the_root_page_reads_the_stored_setting_into_its_row() {
    let _guard = plx_base::testlock::serial();
    let _sess = seated_scratch_session("root-reads-under-posters");
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false });
    let page = RootPage::new(EntryId(0), test_support::cx(None).views);
    assert_eq!(page.state.under_posters, UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false });
}

/// **Every string fits, in every shipped language**: both pages' rows, the root row beside every
/// value it can read, and the preview's frame clear of the copy above it on each page.
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
            let mut picker = FormTable::<DetailsId, UnderPostersDetails, SettingsPage>::new(crate::registry::BAND);
            picker.table.compact = false;
            picker.set(details_form(value.details, &sample_facts()), None);
            out.extend(picker.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} details picker {value:?}")));
            let mut root = FormTable::<RootId, Action, SettingsPage>::new(crate::registry::BAND);
            root.table.compact = false;
            root.set(root_form(&RootInputs { under_posters: value, ..text_fit_inputs() }), None);
            out.extend(root.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} settings root {value:?}")));
        }
        // each page's narrative copy ends above the preview frame
        for (title, copy) in [
            (plx_platform::i18n::msg::under_posters_title(), plx_platform::i18n::msg::under_posters_copy()),
            (plx_platform::i18n::msg::under_posters_details_row(), plx_platform::i18n::msg::under_posters_details_copy()),
        ] {
            let frame = layout.narrative_copy_frame(true, title, layout.action.y, &ShippedMeasure);
            let copy_h = TextView::new(copy, theme::size::LABEL, theme::TEXT_READING)
                .leading(theme::size::LABEL as f32 + theme::space::XS)
                .max_lines(12)
                .with_measure(&ShippedMeasure)
                .measure_h(layout.narrative.w);
            // The board's own clearance is 20 px (a five-line copy from 280 under a frame at 470), so the
            // air the copy must leave is the tight rung, not the default one.
            if frame.y + copy_h + theme::space::SM > preview_frame().y {
                out.push(format!("{tag}: the copy under {title:?} ends at {:.0}, under the preview at {:.0}", frame.y + copy_h, preview_frame().y));
            }
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

fn poster(title: &str, year: i32, genre: &str) -> PmsMovie {
    PmsMovie { title: title.into(), year, genre: Some(genre.into()), ..Default::default() }
}

#[test]
fn the_preview_is_the_boards_frame_with_three_library_cards_the_outer_two_cut_by_it() {
    let narrative = RouteLayout::screen().narrative;
    let frame = preview_frame();
    assert_eq!((frame.w, frame.h), (660.0, 556.0), "the board's frame");
    assert!(frame.x >= narrative.x && frame.x + frame.w <= narrative.x + narrative.w);
    assert!((frame.y + frame.h - (narrative.y + narrative.h)).abs() < 0.01, "it rests on the column's floor");
    let row = preview_row();
    assert_eq!((row.y - frame.y, row.h), (40.0, 375.0), "tiles 40 px under the frame's top, a Library card tall");
    let xs: Vec<f32> = (0..PREVIEW_TILES).map(|i| SampleRow::tile(&PREVIEW_STYLE, PREVIEW_TILES, i, row).x - frame.x).collect();
    for (got, want) in xs.iter().zip([-80.0, 205.0, 490.0]) {
        assert!((got - want).abs() < 1e-3, "tile x in the frame: {xs:?}");
    }
    assert_eq!(PREVIEW_STYLE.w, plx_ui::consts::CARD_W, "the Library's own card, not a private size");
    let lifted = SampleRow::lifted(&PREVIEW_STYLE, PREVIEW_TILES, PREVIEW_LIFTED, row);
    assert!(lifted.x >= frame.x && lifted.x + lifted.w <= frame.x + frame.w, "the lifted tile is whole: {lifted:?}");
    assert!(lifted.y >= frame.y && lifted.y + lifted.h <= frame.y + frame.h);
    assert_eq!(PREVIEW_REST_ALPHA, 0.35, "the others show at the board's 35%");
}

#[test]
fn the_preview_captions_only_the_lifted_tile_from_its_own_poster() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (a, b, c) = (poster("Alpha", 1999, "Noir"), poster("Bravo", 2004, "Western"), poster("Charlie", 2011, "Comedy"));
    let runs = plx_ui::draw_census::capture_runs(|| {
        draw_preview(plx_ui::Painter::recording(), UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false },
            &[&a, &b, &c], None, &ShippedMeasure);
    });
    let words: Vec<&str> = runs.iter().filter(|r| r.ink[3] > 0.5).map(|r| r.text.as_str()).collect();
    assert_eq!(words, vec!["Bravo", "Western"], "only the middle tile is captioned, with its own facts");
    let lifted = SampleRow::lifted(&PREVIEW_STYLE, PREVIEW_TILES, lifted_tile(3), preview_row());
    let caption = runs.iter().rfind(|r| r.text == "Western").unwrap();
    assert!(caption.at.y > lifted.y + lifted.h - 1.0, "the caption stands under the tile: {:?}", caption.at);
    // fewer posters than tiles: the lifted tile is the last real one, the rest are blank faces
    let two = plx_ui::draw_census::capture_runs(|| {
        draw_preview(plx_ui::Painter::recording(), UnderPosters { title: true, details: UnderPostersDetails::None, sort_value: false }, &[&a], None, &ShippedMeasure);
    });
    assert_eq!(two.iter().filter(|r| r.ink[3] > 0.5).map(|r| r.text.as_str()).collect::<Vec<_>>(), vec!["Alpha"]);
    assert_eq!(lifted_tile(0), PREVIEW_LIFTED, "no posters: the middle tile");
    assert_eq!(lifted_tile(1), 0, "one poster: the lifted tile is that poster");
}

#[test]
fn the_pages_preview_reads_the_first_cached_posters_of_the_current_library() {
    let _guard = plx_base::testlock::serial();
    let items: Vec<PmsMovie> = ["Alpha", "Bravo", "Charlie", "Delta"].into_iter().map(|t| poster(t, 2000, "Noir")).collect();
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(items);
    let c = cx_with(None, snapshot.view());
    let titles: Vec<&str> = preview_posters(&c).iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Alpha", "Bravo", "Charlie"], "the first three, no more");
    assert!(preview_posters(&cx(None)).is_empty(), "no library cached: blank faces");
}

#[test]
fn a_blank_face_carries_no_word() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let runs = plx_ui::draw_census::capture_runs(|| {
        draw_preview(plx_ui::Painter::recording(), UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false }, &[], None, &ShippedMeasure);
    });
    let words: Vec<&str> = runs.iter().filter(|r| r.ink[3] > 0.5).map(|r| r.text.as_str()).collect();
    assert_eq!(words.iter().filter(|w| **w == "Movie").count(), 1, "the title is the caption alone, never also a word on a blank face: {words:?}");
    let off = plx_ui::draw_census::capture_runs(|| {
        draw_preview(plx_ui::Painter::recording(), UnderPosters { title: false, details: UnderPostersDetails::None, sort_value: false }, &[], None, &ShippedMeasure);
    });
    assert!(off.iter().all(|r| r.ink[3] <= 0.5), "with no caption chosen the preview is faces only: {:?}", off.iter().map(|r| &r.text).collect::<Vec<_>>());
}

#[test]
fn both_pages_draw_for_every_value_without_a_painter() {
    let _guard = plx_base::testlock::serial();
    for value in every_value() {
        let _live = Live::set(value);
        let c = cx(None);
        let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
        <UnderPostersPage as Screen<InnerHost>>::draw(&mut UnderPostersPage::new(EntryId(0)), &mut f);
        <DetailsPage as Screen<InnerHost>>::draw(&mut DetailsPage::new(EntryId(0)), &mut f);
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
    let details = DetailsPage::new(EntryId(0));
    assert_eq!(<DetailsPage as Screen<InnerHost>>::name(&details), "under-posters-details");
    SettingsPage::UnderPostersDetails.probe(&mut arg);
    assert!(arg.ends_with("under-posters-details"), "{arg}");
}

#[test]
fn the_sort_value_switch_uses_the_existing_confirmed_profile_write_and_reset() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let (asked, reply) = write_request(activate(&mut page, UpId::SortValue)).unwrap();
    assert!(!asked.sort_value, "the default is On, so OK on the row asks for Off");
    assert_eq!((asked.title, asked.details), (false, UnderPostersDetails::YearLength));
    reply.send(false).unwrap(); page.poll();
    assert_eq!(page.state.value, UnderPosters::DEFAULT, "a refused save must not turn it off");
    plx_media::route::restore_under_posters(asked);
    page.poll();
    let (reset, _) = write_request(activate(&mut page, UpId::Reset)).unwrap();
    assert_eq!(reset, UnderPosters::DEFAULT);
}

/// What one draw of a page puts on screen as `(text, ink alpha)`, for every run whose text is in `words`.
fn drawn_alphas<S: Screen<InnerHost>>(page: &mut S, words: &[&str]) -> Vec<(String, f32)> {
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(captioned_posters());
    let c = cx_with(None, snapshot.view());
    let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
    plx_ui::draw_census::capture_runs(|| page.draw(&mut f)).into_iter()
        .filter(|run| words.contains(&run.text.as_str())).map(|run| (run.text, run.ink[3])).collect()
}

fn alpha_of(runs: &[(String, f32)], text: &str) -> f32 {
    runs.iter().filter(|(t, _)| t == text).map(|(_, a)| *a).fold(0.0, f32::max)
}

/// **The Sort value state illustrates a library sorted by Year** and says so under the posters: the
/// values are the sample posters' own years (the one value formatter), the middle one whole and the
/// outer two dimmed with their tiles; Off has the Title/Details caption and no "Sorted by" line.
#[test]
fn the_sort_value_preview_shows_each_posters_year_dimmed_beside_the_lifted_one_and_says_sorted_by_year() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: true });
    let mut page = UnderPostersPage::new(EntryId(0));
    page.state.sel = UpId::SortValue.key();
    let runs = drawn_alphas(&mut page, &["1999", "2004", "2011", "Sorted by Year"]);
    assert!(alpha_of(&runs, "2004") > 0.9, "the lifted poster's year is whole: {runs:?}");
    for outer in ["1999", "2011"] {
        let a = alpha_of(&runs, outer);
        assert!(a > 0.0 && a <= PREVIEW_REST_ALPHA + 1e-3, "{outer} dims with its tile: {a}");
    }
    assert!(alpha_of(&runs, "Sorted by Year") > 0.9, "the caption under the frame reads whole: {runs:?}");
    page.state.value = UnderPosters { sort_value: false, ..page.state.value };
    let off = drawn_alphas(&mut page, &["1999", "2004", "2011", "Sorted by Year"]);
    assert!(off.is_empty(), "Off: no years and no 'Sorted by' caption: {off:?}");
}

#[test]
fn a_poster_with_no_year_wears_the_dash_in_the_sort_value_preview() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let nameless = PmsMovie { year: 0, ..poster("Nameless", 0, "Noir") };
    let runs = plx_ui::draw_census::capture_runs(|| {
        draw_preview(plx_ui::Painter::recording(), UnderPosters { title: false, details: UnderPostersDetails::None, sort_value: true },
            &[&nameless, &nameless, &nameless], None, &ShippedMeasure);
    });
    assert!(runs.iter().any(|r| r.text == "\u{2013}"), "a missing year is the en dash: {:?}", runs.iter().map(|r| &r.text).collect::<Vec<_>>());
}

/// **The preview words each item's own year whatever kind the last listing held.** The preview
/// illustrates a library sorted by Year, and the library the reader last opened may be a TV show
/// library (shows), its Seasons or Episodes listing, or a Collections page: the year is the item's own
/// fact (`PmsMovie::year`) on every one of them, not a Movie-only fact, so none of them draws the dash
/// for data the server did send.
#[test]
fn the_sort_value_preview_words_the_year_of_every_kind_of_item() {
    use plx_data::pms::{KIND_COLLECTION, KIND_SHOW};
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let of_kind = |kind, year| PmsMovie { kind, ..poster("Title", year, "Noir") };
    let value = UnderPosters { title: false, details: UnderPostersDetails::None, sort_value: true };
    let drawn = |posters: &[&PmsMovie]| -> Vec<String> {
        plx_ui::draw_census::capture_runs(|| draw_preview(plx_ui::Painter::recording(), value, posters, None, &ShippedMeasure))
            .into_iter().map(|run| run.text).collect()
    };
    for (kinds, years) in [([0, KIND_SHOW, 2], ["1999", "2004", "2011"]), ([3, KIND_COLLECTION, 2], ["2015", "2021", "2023"])] {
        let items: Vec<PmsMovie> = kinds.iter().zip(years).map(|(kind, year)| of_kind(*kind, year.parse().unwrap())).collect();
        let texts = drawn(&[&items[0], &items[1], &items[2]]);
        for year in years {
            assert!(texts.iter().any(|t| t == year), "kind {kinds:?}: {year} is worded from the item's own year: {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "\u{2013}"), "kind {kinds:?}: a year the server sent is never the dash: {texts:?}");
    }
    // A year the server did not send is still the dash, of any kind.
    let nameless = of_kind(2, 0);
    assert!(drawn(&[&nameless, &nameless, &nameless]).iter().any(|t| t == "\u{2013}"));
}

/// **The picker's preview says where its posters come from**: the focused tile's own sample line and the
/// "The library you last opened" caption under the frame.
#[test]
fn the_pickers_preview_says_it_uses_the_library_last_opened() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: false, details: UnderPostersDetails::Genre, sort_value: true });
    let mut page = DetailsPage::new(EntryId(0));
    let runs = drawn_alphas(&mut page, &["The library you last opened", "Sorted by Year"]);
    assert!(alpha_of(&runs, "The library you last opened") > 0.9, "{runs:?}");
    assert!(alpha_of(&runs, "Sorted by Year") == 0.0, "never the Sort value state's caption: {runs:?}");
}

/// **The Video choice says what a TV show does instead**; no other choice carries a detail line.
#[test]
fn only_the_video_choice_carries_a_detail_line() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let mut form = FormTable::<DetailsId, UnderPostersDetails, SettingsPage>::new(crate::registry::BAND);
    form.table.compact = false;
    form.set(details_form(UnderPostersDetails::YearLength, &sample_facts()), None);
    let rows: Vec<(String, String)> = form.table.sections.iter().flat_map(|s| s.rows.iter())
        .map(|r| (r.label.clone(), r.detail.clone())).collect();
    assert_eq!(rows.len(), 5, "{rows:?}");
    for (label, detail) in &rows {
        if label == "Video" {
            assert_eq!(detail, "A TV show has no single video quality; it shows its year.");
        } else {
            assert!(detail.is_empty(), "{label}: {detail:?}");
        }
    }
}

/// **Both captions under the frame fit it, in every shipped language**: one line, uncut, inside the
/// frame less its sides.
#[test]
fn the_frames_captions_fit_in_every_language() {
    use plx_machine::machine::Measure;
    let _guard = plx_base::testlock::serial();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        for text in [plx_platform::i18n::msg::under_posters_sorted_by(), plx_platform::i18n::msg::under_posters_last_opened()] {
            let width = ShippedMeasure.width_str(text, theme::size::MICRO, false);
            if width > preview_frame().w - 2.0 * theme::space::MD {
                out.push(format!("{}: {text:?} is {width:.0}px under a {:.0}px frame", language.tag(), preview_frame().w));
            }
        }
    }
    assert!(out.is_empty(), "{out:#?}");
}
