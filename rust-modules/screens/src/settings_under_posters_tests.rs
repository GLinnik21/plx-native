//! Settings > Libraries > Under posters: the rows, the write door, the policy mapping, the picture,
//! the sentences each row states, the shared disabled state and the fit of every string in every
//! shipped language.
use super::*;
use super::super::test_support::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_plex::plex::session::UnderPosters;
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_machine::present::Present;
use DetailPart as P;

fn parts(ps: &[DetailPart]) -> DetailParts {
    DetailParts::of(ps)
}

/// A value from plain parts (the sort value Off, so the pages draw the caption).
fn up(title: bool, details: &[DetailPart], show_details: &[DetailPart]) -> UnderPosters {
    UnderPosters { title, details: parts(details), show_details: parts(show_details), sort_value: false }
}

/// Every set of up to four of the parts a kind offers (what the picker can produce): 57 for a
/// movie, 31 for a show.
fn subsets(offered: &[DetailPart]) -> Vec<DetailParts> {
    (0u32..(1 << offered.len()))
        .map(|m| parts(&offered.iter().enumerate().filter(|(i, _)| m >> i & 1 == 1).map(|(_, p)| *p).collect::<Vec<_>>()))
        .filter(|s| s.len() <= DetailParts::MAX)
        .collect()
}

/// **Every value the page can show**: both title states over EVERY set of up to four parts of each
/// kind. A row's text depends on its own kind's set alone (the root row on the movie's, each details
/// row on its own, a picker on its kind's), so the movie and show sets are walked side by side, the
/// shorter list wrapping, rather than as a 57 x 31 product: every set of both kinds appears in
/// every title state, and no combination of a row's own set is skipped.
fn every_value() -> Vec<UnderPosters> {
    let (movies, shows) = (subsets(PosterKind::Movie.offered()), subsets(PosterKind::Show.offered()));
    let mut all = Vec::new();
    for title in [false, true] {
        for i in 0..movies.len().max(shows.len()) {
            all.push(UnderPosters { title, details: movies[i % movies.len()], show_details: shows[i % shows.len()], sort_value: false });
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

/// The parts whose row wears the check mark, read off the picker's table, in the line's order.
fn checked(page: &DetailsPage) -> Vec<DetailPart> {
    let marks: Vec<bool> = page.form.table.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.checked)).collect();
    page.kind.offered().iter().copied().filter(|p| page.form.index_of_key(PartId(*p).key()).is_some_and(|i| marks[i])).collect()
}

/// The parts a picker lists, in order.
fn listed(page: &DetailsPage) -> Vec<DetailPart> {
    DetailPart::MOVIE.iter().chain(DetailPart::SHOW.iter()).copied().fold(Vec::new(), |mut all, p| {
        if !all.contains(&p) && page.form.index_of_key(PartId(p).key()).is_some() {
            all.push(p);
        }
        all
    })
}

fn rows_of(page: &DetailsPage) -> Vec<(String, Option<String>)> {
    page.form.table.sections.iter().flat_map(|s| s.rows.iter()).map(|r| (r.label.to_string(), r.value.clone())).collect()
}

/// Whether each part's row is disabled, in the picker's order.
fn disabled_rows(page: &DetailsPage) -> Vec<(DetailPart, bool)> {
    let rows: Vec<bool> = page.form.table.sections.iter().flat_map(|s| s.rows.iter().map(|r| r.disabled)).collect();
    page.kind.offered().iter().copied().map(|p| (p, page.form.index_of_key(PartId(p).key()).is_some_and(|i| rows[i]))).collect()
}

fn pick(page: &mut DetailsPage, part: DetailPart) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.activate(PartId(part).key().0, &mut fx);
    out
}

fn pops(effects: &[Stamped<InnerHost>]) -> bool {
    effects.iter().any(|e| matches!(e.fx, Fx::Nav(plx_machine::machine::NavOp::Pop)))
}

fn poster(title: &str, year: i32, genre: &str) -> PmsMovie {
    PmsMovie { title: title.into(), year, genre: Some(genre.into()), thumb: format!("/library/metadata/{title}/thumb"), ..Default::default() }
}

fn show(title: &str, year: i32, genre: &str) -> PmsMovie {
    PmsMovie { kind: 1, ..poster(title, year, genre) }
}

const DOT: char = '\u{b7}';

/// Move focus on `page` to `key`, as the engine would.
fn focus(page: &mut impl Machine<InnerHost, Ev = ScreenEvent<InnerHost>>, entry: EntryId, key: RowKey) {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    let to = FocusKey { entry, elem: key.0 };
    page.step(&ScreenEvent::FocusMoved { from: None, to, by: plx_ui::screen::By::Dir }, &cx(None), &mut fx);
}

// ---- the page --------------------------------------------------------------------------------

#[test]
fn the_page_has_a_title_switch_two_details_rows_a_sort_value_switch_and_reset_with_distinct_keys() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = UnderPostersPage::new(EntryId(0));
    let ids = [UpId::Title, UpId::MovieDetails, UpId::ShowDetails, UpId::SortValue, UpId::Reset];
    let keys: Vec<u32> = ids.iter().map(|id| id.key().0).collect();
    let mut unique = keys.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 5, "each row has its own key");
    assert!(keys.iter().all(|k| *k < crate::registry::BAND));
    let rows: Vec<usize> = keys.iter().map(|k| page.form.index_of_key(RowKey(*k)).expect("the row is in the form")).collect();
    assert_eq!(rows, vec![0, 1, 2, 3, 4], "Title, Movie details, TV show details, Sort value, Reset in that order");
}

/// The left column's sentence for each row of the page, as the design words it.
#[test]
fn each_page_row_states_the_designs_sentence() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let said = |id| sentence_for(Some(&id));
    assert_eq!(said(UpId::Title), "Shows each title under its poster.");
    assert_eq!(said(UpId::MovieDetails), "Choose what appears under movie posters.");
    assert_eq!(said(UpId::ShowDetails), "Choose what appears under TV show posters.");
    assert_eq!(said(UpId::SortValue), "Replaces the details line while a library is sorted.");
    assert_eq!(said(UpId::Reset), "Puts Title, Movie details, TV show details and Sort value back to their defaults.");
}

#[test]
fn no_row_carries_a_sub_line_on_either_page() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = UnderPostersPage::new(EntryId(0));
    assert!(page.form.table.sections.iter().flat_map(|s| s.rows.iter()).all(|r| r.detail.is_empty()),
        "Settings v2: the sentence is stated beside the table, not under the row");
    for kind in [PosterKind::Movie, PosterKind::Show] {
        let picker = DetailsPage::new(EntryId(0), kind);
        assert!(picker.form.table.sections.iter().flat_map(|s| s.rows.iter()).all(|r| r.detail.is_empty()), "{kind:?}");
    }
}

#[test]
fn each_details_row_reads_its_kinds_names_and_opens_its_own_picker() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    for (id, kind, label, want) in [
        (UpId::MovieDetails, PosterKind::Movie, "Movie details", format!("Year {DOT} Length")),
        (UpId::ShowDetails, PosterKind::Show, "TV show details", format!("Year {DOT} Seasons")),
    ] {
        let i = page.form.index_of_key(id.key()).unwrap();
        let row = page.form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i).unwrap();
        assert_eq!((row.label.as_str(), row.value.as_deref()), (label, Some(want.as_str())));
        assert_eq!(page.form.activate(i), Some(plx_ui::form::Activation::Push(SettingsPage::UnderPostersDetails(kind))));
        let out = activate(&mut page, id.clone());
        assert!(out.iter().any(|e| matches!(e.fx, Fx::Nav(plx_machine::machine::NavOp::Push(SettingsPage::UnderPostersDetails(k))) if k == kind)));
        assert!(write_request(out).is_none(), "opening a picker writes nothing");
    }
}

/// **A details row's value**: the ticked names joined with the middle dot in the line's order,
/// "Nothing" for none, and a count ("4 details") once the joined names run past the row's budget.
#[test]
fn a_details_value_is_the_names_nothing_or_a_count() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    assert_eq!(names_value(DetailParts::NONE), "Nothing");
    assert_eq!(names_value(parts(&[P::Year])), "Year");
    assert_eq!(names_value(parts(&[P::Length, P::Year])), format!("Year {DOT} Length"), "the line's order, not the order ticked");
    assert_eq!(names_value(parts(&[P::Year, P::Length, P::Genre])), format!("Year {DOT} Length {DOT} Genre"));
    assert_eq!(names_value(parts(&[P::Year, P::Length, P::Genre, P::Score])), "4 details");
    assert_eq!(names_value(parts(&[P::Year, P::Length, P::VideoQuality])), "3 details", "names past the budget read their count");
}

#[test]
fn the_rows_read_each_kinds_stored_parts_independently() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { details: parts(&[P::VideoQuality]), show_details: parts(&[P::AgeRating, P::Score]), ..UnderPosters::DEFAULT });
    let page = UnderPostersPage::new(EntryId(0));
    let value_of = |id: UpId| {
        let i = page.form.index_of_key(id.key()).unwrap();
        page.form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i).and_then(|r| r.value.clone())
    };
    assert_eq!(value_of(UpId::MovieDetails).as_deref(), Some("Video quality"));
    assert_eq!(value_of(UpId::ShowDetails), Some(format!("Age rating {DOT} Score")));
}

#[test]
fn the_title_switch_asks_the_write_door_and_waits_for_the_receipt() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let before = page.state.hash();
    let (asked, reply) = write_request(activate(&mut page, UpId::Title)).expect("a title write");
    assert_eq!(asked, UnderPosters { title: true, ..UnderPosters::DEFAULT });
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

// ---- the pickers -----------------------------------------------------------------------------

#[test]
fn the_movie_picker_lists_six_parts_each_with_its_sample_and_checks_the_current() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    assert_eq!(rows_of(&page), vec![
        ("Year".to_string(), Some("2024".to_string())),
        ("Length".to_string(), Some("1h 52m".to_string())),
        ("Genre".to_string(), Some("Drama".to_string())),
        ("Video quality".to_string(), Some(format!("4K {DOT} HDR"))),
        ("Age rating".to_string(), Some("PG-13".to_string())),
        ("Score".to_string(), Some("7.8".to_string())),
    ]);
    assert_eq!(page.form.table.sections.len(), 1, "one section");
    assert_eq!(page.form.table.sections[0].header, "Show under posters");
    assert_eq!(checked(&page), vec![P::Year, P::Length]);
    assert!(disabled_rows(&page).iter().all(|(_, d)| !d), "two ticked: every row is usable");
}

#[test]
fn the_show_picker_lists_five_parts_with_seasons_and_no_length_or_video_quality() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = DetailsPage::new(EntryId(0), PosterKind::Show);
    assert_eq!(rows_of(&page), vec![
        ("Year".to_string(), Some("2019".to_string())),
        ("Seasons".to_string(), Some("6 seasons".to_string())),
        ("Genre".to_string(), Some("Drama".to_string())),
        ("Age rating".to_string(), Some("TV-14".to_string())),
        ("Score".to_string(), Some("8.4".to_string())),
    ]);
    let listed = listed(&page);
    assert!(!listed.contains(&P::Length) && !listed.contains(&P::VideoQuality));
    assert_eq!(checked(&page), vec![P::Year, P::Seasons]);
}

/// **Every sample on a picker comes from ONE source: the picture's tile of its kind.** The movie
/// picker words its samples from the library's first MOVIE, the show picker from its first SHOW, and
/// a kind the library has none of from the sample of that kind.
#[test]
fn each_pickers_samples_are_worded_from_the_first_poster_of_its_own_kind() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let sitcom = PmsMovie { child_count: 3, ..show("Sitcom", 2011, "Comedy") };
    let mut western = poster("Bravo", 2022, "Western");
    western.rating = "PG".into();
    western.dur_ns = 6 * 60_000_000_000;
    let items = vec![sitcom, western, poster("Charlie", 2001, "Noir")];
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(items);
    let c = cx_with(None, snapshot.view());
    let mount = |kind| {
        let mut page = DetailsPage::new(EntryId(0), kind);
        let mut out = Vec::new();
        let mut present = Present::new();
        page.step(&ScreenEvent::Mount, &c, &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
        page
    };
    let movies = mount(PosterKind::Movie);
    let sample = |page: &DetailsPage, label: &str| rows_of(page).into_iter().find(|(l, _)| l == label).and_then(|(_, v)| v);
    assert_eq!(sample(&movies, "Genre").as_deref(), Some("Western"), "the first movie, not the show before it");
    assert_eq!(sample(&movies, "Year").as_deref(), Some("2022"));
    assert_eq!(sample(&movies, "Length").as_deref(), Some("6m"));
    assert_eq!(sample(&movies, "Age rating").as_deref(), Some("PG"));
    let shows = mount(PosterKind::Show);
    assert_eq!(sample(&shows, "Genre").as_deref(), Some("Comedy"), "the first show");
    assert_eq!(sample(&shows, "Seasons").as_deref(), Some("3 seasons"));
    // …and each sample is what the picture's tile of that kind wears under the same single part
    let posters = preview_posters(&c);
    for (page, kind) in [(&movies, PosterKind::Movie), (&shows, PosterKind::Show)] {
        for part in [P::Year, P::Length, P::Seasons, P::Genre].into_iter().filter(|p| kind.offered().contains(p)) {
            let value = rows_of(page).into_iter().find(|(l, _)| *l == part_label(part)).and_then(|(_, v)| v);
            let v = UnderPosters { title: false, ..kind.with_parts(UnderPosters::DEFAULT, parts(&[part])) };
            assert_eq!(value, picture_lines(v, kind, poster_of(kind, &posters), false)[1], "{kind:?} {part:?}");
        }
    }
}

/// **OK on a part ticks it and STAYS**: the new set is one write through the door, the row shows it
/// at once, the rest of the setting is kept, and the page does not pop when the receipt answers.
#[test]
fn ok_ticks_a_part_writes_once_keeps_the_rest_and_stays() {
    let _guard = plx_base::testlock::serial();
    let start = UnderPosters { title: true, details: parts(&[P::Genre]), show_details: parts(&[P::AgeRating, P::Score]), sort_value: false };
    let _live = Live::set(start);
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let effects = pick(&mut page, P::Length);
    assert!(!pops(&effects), "a tick never leaves the page");
    let (asked, reply) = write_request(effects).expect("a write");
    assert_eq!(asked, UnderPosters { details: parts(&[P::Length, P::Genre]), ..start }, "the show's parts and the title are kept");
    assert_eq!(checked(&page), vec![P::Length, P::Genre], "the row shows the tick at once, in the line's order");
    assert!(!page.poll(), "waiting never blocks the frame");
    plx_media::route::restore_under_posters(asked);
    reply.send(true).unwrap();
    assert!(page.poll(), "the receipt settles the page");
    assert_eq!(checked(&page), vec![P::Length, P::Genre]);
    assert!(!page.state.busy);
}

#[test]
fn a_second_ok_on_a_ticked_part_unticks_it_and_the_last_one_leaves_nothing() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(up(false, &[P::Year], &[P::Year, P::Seasons]));
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let (asked, _reply) = write_request(pick(&mut page, P::Year)).expect("a write");
    assert_eq!(asked.details, DetailParts::NONE, "the empty set is a choice: no details line");
    assert!(checked(&page).is_empty());
}

#[test]
fn a_show_tick_changes_only_the_show_parts() {
    let _guard = plx_base::testlock::serial();
    let start = up(true, &[P::VideoQuality], &[P::Year, P::Seasons]);
    let _live = Live::set(start);
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Show);
    let (asked, _reply) = write_request(pick(&mut page, P::Genre)).expect("a write");
    assert_eq!(asked, UnderPosters { show_details: parts(&[P::Year, P::Seasons, P::Genre]), ..start }, "the movie parts stay");
}

#[test]
fn a_refused_tick_puts_the_check_mark_back_and_stays() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Show);
    let (_, reply) = write_request(pick(&mut page, P::Genre)).unwrap();
    assert_eq!(checked(&page), vec![P::Year, P::Seasons, P::Genre]);
    reply.send(false).unwrap();
    assert!(page.poll());
    assert_eq!(page.state.value, UnderPosters::DEFAULT);
    assert_eq!(checked(&page), vec![P::Year, P::Seasons]);
}

/// **With four ticked, every other row is DISABLED by the shared state**, still focusable, inert to
/// OK; unticking one makes them all usable again at once.
#[test]
fn with_four_ticked_the_other_rows_are_disabled_by_the_shared_state_and_ok_does_nothing() {
    let _guard = plx_base::testlock::serial();
    let four = parts(&[P::Year, P::Length, P::Genre, P::VideoQuality]);
    let _live = Live::set(UnderPosters { details: four, ..UnderPosters::DEFAULT });
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    assert_eq!(disabled_rows(&page), vec![
        (P::Year, false), (P::Length, false), (P::Genre, false), (P::VideoQuality, false), (P::AgeRating, true), (P::Score, true),
    ], "only the unticked rows are disabled");
    let i = page.form.index_of_key(PartId(P::AgeRating).key()).unwrap();
    assert_eq!(page.form.activate(i), None, "a disabled row answers nothing");
    assert!(write_request(pick(&mut page, P::Score)).is_none(), "OK on a disabled row writes nothing");
    assert_eq!(checked(&page), vec![P::Year, P::Length, P::Genre, P::VideoQuality]);
    // unticking one makes room
    let (asked, _reply) = write_request(pick(&mut page, P::Genre)).expect("an untick");
    assert_eq!(asked.details, parts(&[P::Year, P::Length, P::VideoQuality]));
    assert!(disabled_rows(&page).iter().all(|(_, d)| !d), "three ticked: every row is usable again");
    // a show offers four of five: the same rule
    let _live = Live::set(UnderPosters { show_details: parts(&[P::Year, P::Seasons, P::Genre, P::AgeRating]), ..UnderPosters::DEFAULT });
    let shows = DetailsPage::new(EntryId(0), PosterKind::Show);
    assert_eq!(disabled_rows(&shows).iter().filter(|(_, d)| *d).map(|(p, _)| *p).collect::<Vec<_>>(), vec![P::Score]);
}

/// **The unfocused kind's whole column is the design's `dim` (0.45), the focused one (or both) 1**:
/// on the Under posters page the kind of the OTHER details row is dimmed, both are at full strength on
/// Title, Sort value and Reset; in a picker the other kind is. The number is
/// [`plx_ui::follow_copy::DIM`], not the disabled control state a blocked row takes.
#[test]
fn the_unfocused_kind_is_dimmed_to_the_designs_value_per_focus() {
    use plx_ui::follow_copy::DIM;
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let entry = page.entry;
    let settled = |page: &mut UnderPostersPage| {
        for _ in 0..600 {
            page.dims.update(1.0 / 60.0);
        }
        page.dims.opacity()
    };
    assert_eq!(page.dims.opacity(), [1.0, 1.0], "opens on Title: both usable, no motion");
    focus(&mut page, entry, UpId::MovieDetails.key());
    assert_eq!(settled(&mut page), [1.0, DIM], "Movie details focused: the show column is dimmed");
    focus(&mut page, entry, UpId::ShowDetails.key());
    assert_eq!(settled(&mut page), [DIM, 1.0], "TV show details focused: the movie column is dimmed");
    for row in [UpId::SortValue, UpId::Reset, UpId::Title] {
        focus(&mut page, entry, row.key());
        assert_eq!(settled(&mut page), [1.0, 1.0], "{row:?}");
    }
    assert_eq!(DetailsPage::new(EntryId(0), PosterKind::Movie).picture_opacity(), [1.0, DIM], "the Movie picker dims the show");
    assert_eq!(DetailsPage::new(EntryId(0), PosterKind::Show).picture_opacity(), [DIM, 1.0], "the Show picker dims the movie");
    // The picker's blocked ROWS are another thing: the shared disabled control state.
    let four = parts(&[P::Year, P::Length, P::Genre, P::VideoQuality]);
    let _live = Live::set(UnderPosters { details: four, ..UnderPosters::DEFAULT });
    let picker = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let blocked = picker.form.table.sections.iter().flat_map(|s| s.rows.iter()).find(|r| r.disabled).expect("a blocked row");
    assert!(blocked.disabled);
}

#[test]
fn the_picture_draws_each_kinds_whole_column_at_its_opacity() {
    use plx_ui::follow_copy::DIM;
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, ..UnderPosters::DEFAULT });
    let slot = RouteLayout::settings_page().picture_slot();
    let alphas = |opacity: [f32; 2]| {
        let picture = RootPicture { value: UnderPosters { title: true, ..UnderPosters::DEFAULT }, posters: &[], sorted: false, opacity };
        plx_ui::draw_census::capture_runs(|| {
            plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), slot, &ShippedMeasure));
        })
    };
    let alpha = |runs: &[plx_ui::draw_census::Run], text: &str| runs.iter().find(|r| r.text == text).unwrap_or_else(|| panic!("{text:?} not drawn")).ink[3];
    let full = alphas([1.0, 1.0]);
    let dimmed = alphas([1.0, DIM]);
    for (text, rest) in [("MOVIE", theme::TEXT_READING), ("Movie", theme::TEXT_PRIMARY), (&*format!("2024 {DOT} 1h 52m"), theme::CAPTION_DETAIL_INK)] {
        assert_eq!(alpha(&full, text), rest[3], "{text}: full strength is its own ink");
        assert_eq!(alpha(&dimmed, text), rest[3], "{text}: the chosen column is untouched");
    }
    for (text, rest) in [("TV SHOW", theme::TEXT_READING), ("Show", theme::TEXT_PRIMARY), (&*format!("2019 {DOT} 6 seasons"), theme::CAPTION_DETAIL_INK)] {
        assert!((alpha(&dimmed, text) - rest[3] * DIM).abs() < 1e-4, "{text}: caption, title and line of the other column are at {DIM}: {} vs {}", alpha(&dimmed, text), rest[3]);
    }
}

#[test]
fn a_pickers_focus_states_the_part_or_why_it_is_blocked() {
    use plx_platform::i18n::msg;
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let four = parts(&[P::Year, P::Length, P::Genre, P::VideoQuality]);
    let _live = Live::set(UnderPosters { details: four, ..UnderPosters::DEFAULT });
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let entry = page.entry;
    assert_eq!(page.follow.text(), "Shows the release year, like 2024.", "it opens on the first part's sentence");
    focus(&mut page, entry, PartId(P::AgeRating).key());
    assert_eq!(page.follow.text(), msg::under_posters_blocked_age_rating());
    assert_eq!(page.follow.text(), "Up to four details fit under a poster. Remove one to add the age rating.");
    focus(&mut page, entry, PartId(P::Length).key());
    assert_eq!(page.follow.text(), "Shows the running time, like 1h 52m.", "a ticked part is never blocked");
}

/// Each part's sentence names what it shows with the real example, and each blocked one says why.
#[test]
fn every_part_has_its_own_sentence_with_a_real_example_and_its_own_blocked_one() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let two = DetailParts::MOVIE_DEFAULT;
    let four = parts(&[P::Year, P::Length, P::Genre, P::VideoQuality]);
    let sentence = |kind, set, part, example: &str| picker_sentence(kind, set, part, example);
    assert_eq!(sentence(PosterKind::Movie, two, P::Year, "2024"), "Shows the release year, like 2024.");
    assert_eq!(sentence(PosterKind::Show, two, P::Year, "2019"), "Shows the year it first aired, like 2019.");
    assert_eq!(sentence(PosterKind::Movie, two, P::Length, "1h 52m"), "Shows the running time, like 1h 52m.");
    assert_eq!(sentence(PosterKind::Show, two, P::Seasons, "6 seasons"), "Shows how many seasons it has, like 6 seasons.");
    assert_eq!(sentence(PosterKind::Movie, two, P::Genre, "Drama"), "Shows the genre, like Drama.");
    assert_eq!(sentence(PosterKind::Movie, two, P::VideoQuality, "4K \u{b7} HDR"), "Shows the video quality, like 4K \u{b7} HDR.");
    assert_eq!(sentence(PosterKind::Movie, two, P::AgeRating, "PG-13"), "Shows the age rating, like PG-13.");
    assert_eq!(sentence(PosterKind::Movie, two, P::Score, "7.8"), "Shows the audience score, like 7.8.");
    let mut blocked = Vec::new();
    for part in [P::AgeRating, P::Score] {
        let text = sentence(PosterKind::Movie, four, part, "ignored");
        assert!(text.starts_with("Up to four details fit under a poster."), "{text}");
        assert!(!text.contains("ignored"), "a blocked sentence has no example: {text}");
        blocked.push(text);
    }
    blocked.sort_unstable();
    blocked.dedup();
    assert_eq!(blocked.len(), 2, "each part is named in its own blocked sentence");
    // A ticked part at the maximum is not blocked.
    assert_eq!(sentence(PosterKind::Movie, four, P::Genre, "Drama"), "Shows the genre, like Drama.");
    // Every part of both kinds words a sentence, none empty.
    for kind in [PosterKind::Movie, PosterKind::Show] {
        for part in kind.offered().iter().copied() {
            assert!(!sentence(kind, two, part, "x").is_empty() && !sentence(kind, parts(&[P::Year, P::Genre, P::AgeRating, P::Score]), part, "x").is_empty());
        }
    }
}

#[test]
fn a_picker_opens_on_its_first_part_with_the_stored_ticks() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters { title: true, details: parts(&[P::AgeRating, P::Score]), show_details: parts(&[P::Genre]), sort_value: false });
    let movies = DetailsPage::new(EntryId(0), PosterKind::Movie);
    assert_eq!(checked(&movies), vec![P::AgeRating, P::Score]);
    assert_eq!(movies.state.sel, PartId(P::Year).key(), "a list of switches opens on its first row");
    let shows = DetailsPage::new(EntryId(0), PosterKind::Show);
    assert_eq!(checked(&shows), vec![P::Genre]);
    assert_eq!(shows.state.sel, PartId(P::Year).key());
}

// ---- the pictures ----------------------------------------------------------------------------

/// Three posters whose genre and length say different things, then a show: a title/details caption
/// words the first movie's genre ("Noir"), a sort value words its year.
fn captioned_posters() -> Vec<PmsMovie> {
    let minutes = |n: i64| n * 60_000_000_000;
    vec![
        PmsMovie { dur_ns: minutes(98), ..poster("Alpha", 1999, "Noir") },
        PmsMovie { dur_ns: minutes(134), ..poster("Bravo", 2004, "Western") },
        show("Sitcom", 2011, "Comedy"),
    ]
}

/// Every text one draw of a page puts on screen, with the posters above in the library.
fn drawn_texts<S: Screen<InnerHost>>(page: &mut S) -> Vec<String> {
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(captioned_posters());
    let c = cx_with(None, snapshot.view());
    let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
    plx_ui::draw_census::capture_runs(|| page.draw(&mut f)).into_iter().filter(|run| run.ink[3] > 0.2).map(|run| run.text).collect()
}

/// A picker's picture is the two tiles of the page, the ticks on its own kind worded live.
#[test]
fn a_pickers_picture_shows_both_kinds_and_words_the_ticks_of_its_own() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: parts(&[P::Genre]), show_details: parts(&[P::Genre]), sort_value: true });
    let mut movies = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let texts = drawn_texts(&mut movies);
    for want in ["MOVIE", "TV SHOW", "Alpha", "Noir", "Sitcom", "Comedy"] {
        assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
    }
    assert!(!texts.iter().any(|t| t == "1999"), "the sort value is not the picker's picture: {texts:?}");
    // untick the movie's only part: the movie tile words no details line, its title stays
    let (asked, _reply) = write_request(pick(&mut movies, P::Genre)).unwrap();
    assert_eq!(asked.details, DetailParts::NONE);
    let none = drawn_texts(&mut movies);
    assert!(!none.iter().any(|t| t == "Noir"), "{none:?}");
    assert!(none.iter().any(|t| t == "Alpha"), "{none:?}");
    assert!(none.iter().any(|t| t == "Comedy"), "the show tile keeps its own parts: {none:?}");
}

#[test]
fn the_pages_picture_draws_a_tile_of_each_kind_from_the_library() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: parts(&[P::Genre]), show_details: parts(&[P::Genre]), sort_value: true });
    let mut page = UnderPostersPage::new(EntryId(0));
    for row in [UpId::Title, UpId::MovieDetails, UpId::ShowDetails, UpId::Reset] {
        page.state.sel = row.key();
        let texts = drawn_texts(&mut page);
        for want in ["MOVIE", "TV SHOW", "Alpha", "Noir", "Sitcom"] {
            assert!(texts.iter().any(|t| t == want), "{row:?} focused: {want:?} in {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "1999" || t == "2011"), "{row:?} focused: no year lines: {texts:?}");
    }
}

#[test]
fn the_picture_shows_each_posters_year_only_while_the_sort_value_row_has_focus() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: parts(&[P::Genre]), sort_value: true, ..UnderPosters::DEFAULT });
    let mut page = UnderPostersPage::new(EntryId(0));
    page.state.sel = UpId::SortValue.key();
    let texts = drawn_texts(&mut page);
    assert!(texts.iter().any(|t| t == "1999") && texts.iter().any(|t| t == "2011"), "a year under each tile: {texts:?}");
    assert!(!texts.iter().any(|t| t == "Noir" || t == "Alpha"), "not the Title/Details caption: {texts:?}");
    // the switch Off: the row previews the caption, there is no value to show
    page.state.value = UnderPosters { sort_value: false, ..page.state.value };
    let off = drawn_texts(&mut page);
    assert!(off.iter().any(|t| t == "Noir") && !off.iter().any(|t| t == "1999"), "{off:?}");
}

#[test]
fn reset_restores_the_default_and_is_inert_at_the_default() {
    let _guard = plx_base::testlock::serial();
    let reset_disabled = |page: &UnderPostersPage| page.form.table.sections.last().unwrap().rows[0].disabled;
    {
        let _live = Live::set(UnderPosters { title: true, details: parts(&[P::Genre]), show_details: DetailParts::NONE, sort_value: false });
        let mut page = UnderPostersPage::new(EntryId(0));
        assert!(!reset_disabled(&page), "a changed value can be reset");
        let (asked, _reply) = write_request(activate(&mut page, UpId::Reset)).expect("reset writes");
        assert_eq!(asked, UnderPosters::DEFAULT);
        assert!(reset_disabled(&page), "the reset action is unavailable as soon as it shows the defaults");
    }
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    assert!(reset_disabled(&page), "the default value presents the shared disabled row");
    assert!(write_request(activate(&mut page, UpId::Reset)).is_none(), "nothing to reset");
    assert!(!page.state.busy);
}

#[test]
fn the_show_preview_supplies_examples_for_missing_facts() {
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let tv = PmsMovie {
        kind: 1,
        title: "Family Stories".into(),
        child_count: 2,
        leaf_count: Some(12),
        viewed_leaf_count: Some(5),
        ..Default::default()
    };
    let value = up(true, &[], &[P::Year, P::Seasons, P::Genre, P::AgeRating]);
    assert_eq!(picture_lines(value, PosterKind::Show, Some(&tv), false),
        [Some("Family Stories".into()), Some("2019 · 2 seasons · Drama · TV-14".into())]);
}

#[test]
fn the_policy_mapping_is_total_and_keeps_the_default() {
    assert_eq!(caption_policy_of(UnderPosters::DEFAULT), CaptionPolicy::DEFAULT);
    for v in every_value() {
        assert_eq!(caption_policy_of(v), CaptionPolicy { title: v.title, details: v.details, show_details: v.show_details });
    }
}

#[test]
fn the_band_follows_either_kinds_parts() {
    let h = |title, details: &[DetailPart], show_details: &[DetailPart]| plx_ui::caption_policy::band_h(&caption_policy_of(up(title, details, show_details)));
    assert_eq!(h(false, &[], &[]), 0.0);
    assert_eq!(h(false, &[P::Year, P::Length], &[P::Year, P::Seasons]), 80.0);
    assert_eq!(h(true, &[], &[]), 80.0);
    assert_eq!(h(true, &[P::Genre], &[P::Genre]), 112.0);
    assert_eq!(h(false, &[], &[P::Genre]), 80.0, "a band opens when only the show parts have words");
    assert_eq!(h(false, &[P::Genre], &[]), 80.0, "a band opens when only the movie parts have words");
}

#[test]
fn the_picture_is_captioned_by_the_same_policy() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let lines = |v, kind| picture_lines(v, kind, None, false);
    for kind in [PosterKind::Movie, PosterKind::Show] {
        assert_eq!(lines(up(false, &[], &[]), kind), [None, None]);
    }
    let default = lines(UnderPosters::DEFAULT, PosterKind::Movie);
    assert_eq!(default[0], None, "the default shows no title");
    assert_eq!(default[1].as_deref(), Some("2024 \u{b7} 1h 52m"));
    assert_eq!(lines(UnderPosters::DEFAULT, PosterKind::Show)[1].as_deref(), Some("2019 \u{b7} 6 seasons"));
    let both = lines(up(true, &[P::Genre], &[P::Genre]), PosterKind::Movie);
    assert_eq!(both[0].as_deref(), Some("Movie"));
    assert_eq!(both[1].as_deref(), Some("Drama"));
    let video = UnderPosters { details: parts(&[P::VideoQuality]), ..UnderPosters::DEFAULT };
    assert_eq!(lines(video, PosterKind::Movie)[1].as_deref(), Some("4K \u{b7} HDR"));
    assert_eq!(lines(video, PosterKind::Show)[1].as_deref(), Some("2019 \u{b7} 6 seasons"), "a show's own parts, not the movie's");
    let age = up(false, &[P::AgeRating, P::Score], &[P::AgeRating, P::Score]);
    assert_eq!(lines(age, PosterKind::Movie)[1].as_deref(), Some("PG-13 \u{b7} 7.8"));
    assert_eq!(lines(age, PosterKind::Show)[1].as_deref(), Some("TV-14 \u{b7} 8.4"));
    // four parts, the line's fixed order, joined by the middle dot
    let four = up(false, &[P::Score, P::VideoQuality, P::Genre, P::Year], &[]);
    assert_eq!(lines(four, PosterKind::Movie)[1].as_deref(), Some("2024 \u{b7} Drama \u{b7} 4K \u{b7} HDR \u{b7} 7.8"));
}

// ---- the root row ----------------------------------------------------------------------------

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
        trace: plx_platform::playback_trace::Status::Off,
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

/// The Under posters row reads its setting like its siblings do: the MOVIE details value the page's
/// Movie details row reads, and "Title" when the title is all there is (never "Nothing" beside a
/// poster that wears its name). The show parts are one level down.
#[test]
fn the_root_row_reads_the_setting() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let read = |title, details: &[DetailPart]| root_row_text(&RootInputs { under_posters: UnderPosters { title, details: parts(details), ..UnderPosters::DEFAULT }, ..text_fit_inputs() });
    let (value, sub) = read(false, &[P::Year, P::Length]);
    assert_eq!(value.as_deref(), Some("Year \u{b7} Length"), "the default reads its names");
    assert_eq!(sub, "", "Settings v2: the root row has no sub-line");
    assert_eq!(super::super::sentence_for(Some(&RootId::UnderPosters)), "Choose what appears under posters, for movies and TV shows alike.",
        "the sentence moved beside the table");
    assert_eq!(read(true, &[P::Year, P::Length]).0.as_deref(), Some("Year \u{b7} Length"), "the title is the page's own row");
    assert_eq!(read(true, &[P::Genre]).0.as_deref(), Some("Genre"));
    assert_eq!(read(false, &[P::VideoQuality]).0.as_deref(), Some("Video quality"));
    assert_eq!(read(true, &[]).0.as_deref(), Some("Title"));
    assert_eq!(read(false, &[]).0.as_deref(), Some("Nothing"));
    assert_eq!(read(false, &[P::Year, P::Length, P::Genre, P::Score]).0.as_deref(), Some("4 details"));
    let shows_only = UnderPosters { details: parts(&[P::Genre]), show_details: parts(&[P::AgeRating, P::Score]), ..UnderPosters::DEFAULT };
    assert_eq!(root_row_text(&RootInputs { under_posters: shows_only, ..text_fit_inputs() }).0.as_deref(), Some("Genre"), "the movie names, not the show's");
    for value in every_value() {
        let text = root_row_text(&RootInputs { under_posters: value, ..text_fit_inputs() }).0;
        assert!(text.as_deref().is_some_and(|t| !t.is_empty()), "{value:?} reads nothing");
    }
}

/// The value is read from the STORED setting when the root is built, so a return from the page (or
/// from a picker) shows the pick.
#[test]
fn the_root_page_reads_the_stored_setting_into_its_row() {
    let _guard = plx_base::testlock::serial();
    let _sess = seated_scratch_session("root-reads-under-posters");
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let stored = UnderPosters { title: true, details: parts(&[P::Genre]), show_details: DetailParts::NONE, sort_value: false };
    let _live = Live::set(stored);
    let page = RootPage::new(EntryId(0), test_support::cx(None).views.directory, &crate::registry::capture_fixture());
    assert_eq!(page.state.under_posters, stored);
}

// ---- the sentences each row states ---------------------------------------------------------

#[test]
fn every_row_states_its_own_sentence_and_the_page_its_own() {
    use plx_platform::i18n::msg;
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    assert_eq!(sentence_for(Some(&UpId::Title)), msg::under_posters_title_detail());
    assert_eq!(sentence_for(Some(&UpId::MovieDetails)), msg::under_posters_movie_details_detail());
    assert_eq!(sentence_for(Some(&UpId::ShowDetails)), msg::under_posters_show_details_detail());
    assert_eq!(sentence_for(Some(&UpId::SortValue)), msg::under_posters_sort_value_detail());
    assert_eq!(sentence_for(Some(&UpId::Reset)), msg::under_posters_reset_detail());
    assert_eq!(sentence_for(None), msg::under_posters_copy(), "no focused row, the page's sentence");
    let all = [UpId::Title, UpId::MovieDetails, UpId::ShowDetails, UpId::SortValue, UpId::Reset];
    let mut sentences: Vec<&str> = all.iter().map(|id| sentence_for(Some(id))).collect();
    assert!(sentences.iter().all(|s| !s.is_empty()));
    sentences.sort_unstable();
    sentences.dedup();
    assert_eq!(sentences.len(), all.len(), "no two rows say the same thing");
}

#[test]
fn the_page_opens_on_its_first_rows_sentence_and_follows_focus_and_the_dimming_with_it() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    assert_eq!(page.follow.text(), sentence_for(Some(&UpId::Title)), "no fade on arrival");
    assert_eq!(page.dims.opacity(), [1.0, 1.0], "both kinds are usable on Title: no dimming on arrival");
    let entry = page.entry;
    focus(&mut page, entry, UpId::ShowDetails.key());
    assert_eq!(page.follow.text(), sentence_for(Some(&UpId::ShowDetails)));
    assert!(page.follow.arrival() < 1.0, "a new sentence fades in");
    assert!(page.dims.is_dimmed(0) && !page.dims.is_dimmed(1), "the movie column is the unfocused one");
    // the dimming is a spring: it moves over frames and lands
    page.dims.update(1.0 / 60.0);
    let mid = page.dims.opacity()[0];
    assert!(mid < 1.0 && mid > plx_ui::follow_copy::DIM, "it moves, it does not jump: {mid}");
    for _ in 0..240 {
        page.dims.update(1.0 / 60.0);
    }
    assert_eq!(page.dims.opacity(), [plx_ui::follow_copy::DIM, 1.0]);
}

// ---- fit ------------------------------------------------------------------------------------

/// **Every string fits, in every shipped language**: both pages' rows, both pickers (with and
/// without four ticked), the root row beside every value it can read, every sentence a row or picker
/// states (each part's own and each blocked one) in the five lines the left column gives it (the
/// board's rule for German, French and Belarusian), clear of the picture, and the picture's own
/// words inside their boxes.
#[test]
fn every_under_posters_string_fits_in_every_language() {
    use plx_ui::text_view::TextView;
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let layout = RouteLayout::settings_page();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        let tag = language.tag();
        for value in every_value() {
            let mut page = FormTable::<UpId, UpAction, SettingsPage>::new(crate::registry::BAND);
            page.table.compact = false;
            page.table.board_metrics(true); // as the page draws it
            page.set(under_posters_form(value), None);
            out.extend(page.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} under posters {value:?}")));
            for kind in [PosterKind::Movie, PosterKind::Show] {
                let mut picker = FormTable::<PartId, DetailPart, SettingsPage>::new(crate::registry::BAND);
                picker.table.compact = false;
                picker.table.board_metrics(true);
                picker.set(details_form(kind, kind.parts(value), &picture_facts(kind, None)), None);
                out.extend(picker.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} {kind:?} details picker {value:?}")));
            }
            let mut root = FormTable::<RootId, Action, SettingsPage>::new(crate::registry::BAND);
            root.table.compact = false;
            root.table.board_metrics(true);
            root.set(root_form(&RootInputs { under_posters: value, ..text_fit_inputs() }), None);
            out.extend(root.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} settings root {value:?}")));
        }
        // each sentence ends above the picture slot, in at most the lines the column gives it
        let mut sentences: Vec<(String, &str, String)> = [UpId::Title, UpId::MovieDetails, UpId::ShowDetails, UpId::SortValue, UpId::Reset]
            .iter().map(|id| (format!("{id:?}"), plx_platform::i18n::msg::under_posters_title(), sentence_for(Some(id)).to_string())).collect();
        sentences.push(("page".into(), plx_platform::i18n::msg::under_posters_title(), plx_platform::i18n::msg::under_posters_copy().to_string()));
        let two = DetailParts::MOVIE_DEFAULT;
        let four = |kind: PosterKind| parts(kind.offered()).kept(&kind.offered()[..4]);
        for kind in [PosterKind::Movie, PosterKind::Show] {
            for part in kind.offered().iter().copied() {
                let example = part_example(kind, part, &picture_facts(kind, None));
                sentences.push((format!("{kind:?} {part:?}"), kind.row_label(), picker_sentence(kind, two, part, &example)));
                if !four(kind).contains(part) {
                    sentences.push((format!("{kind:?} {part:?} blocked"), kind.row_label(), picker_sentence(kind, four(kind), part, &example)));
                }
            }
        }
        for (what, title, copy) in sentences {
            let frame = layout.narrative_copy_frame(true, title, layout.action.y, &ShippedMeasure);
            let drop = layout.copy_cap_drop(theme::size::LABEL, &ShippedMeasure);
            let leading = theme::size::LABEL as f32 + theme::space::XS;
            let view = TextView::new(&copy, theme::size::LABEL, theme::TEXT_READING)
                .leading(leading)
                .max_lines(plx_ui::follow_copy::FOLLOW_LINES + 1)
                .with_measure(&ShippedMeasure);
            let lines = (view.measure_h(layout.narrative.w) / leading).round() as usize;
            if lines > plx_ui::follow_copy::FOLLOW_LINES {
                out.push(format!("{tag} {what}: {lines} lines"));
            }
            if frame.y + drop + lines as f32 * leading > layout.picture_slot().y {
                out.push(format!("{tag} {what}: runs into the picture slot"));
            }
        }
        // the picture's words: a kind caption fits its box un-elided (the lines have their own test)
        for kind in [PosterKind::Movie, PosterKind::Show] {
            let upper = kind.caption().to_uppercase();
            let natural = plx_machine::machine::Measure::width_str(&ShippedMeasure, &upper, theme::size::CAPTION, false)
                + plx_gfx::text::tracking_extra(&upper, plx_ui::table::BOARD_CAPS_TRACK_PX);
            if natural > PICTURE_LINE_W {
                out.push(format!("{tag} {kind:?} {upper:?}: {natural}px over a {PICTURE_LINE_W}px caption box"));
            }
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

// ---- the picture ----------------------------------------------------------------------------

#[test]
fn the_pages_picture_reads_the_first_movie_and_the_first_show_of_the_cache() {
    let _guard = plx_base::testlock::serial();
    let items: Vec<PmsMovie> = [show("Sitcom", 2011, "Comedy"), poster("Alpha", 2000, "Noir"), poster("Bravo", 2001, "Noir"), show("Drama", 2012, "Drama")].into();
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(items);
    let c = cx_with(None, snapshot.view());
    let titles: Vec<&str> = preview_posters(&c).iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Alpha", "Sitcom"], "the first movie, then the first show, no more");
    assert!(preview_posters(&cx(None)).is_empty(), "no library cached: no poster of either kind");
}

#[test]
fn both_kinds_are_sourced_when_the_last_opened_library_lists_only_one() {
    let _guard = plx_base::testlock::serial();
    // The last opened library is Movies. The show is in another library's retained shelf.
    let movies: Vec<PmsMovie> = [poster("Alpha", 2000, "Noir"), poster("Bravo", 2001, "Noir")].into();
    let elsewhere: Vec<PmsMovie> = [poster("Other film", 1990, "Noir"), show("Sitcom", 2011, "Comedy")].into();
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new()).with_sample(movies).with_shelf_sample(elsewhere);
    let c = cx_with(None, snapshot.view());
    let titles: Vec<&str> = preview_posters(&c).iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Alpha", "Sitcom"], "the current library's movie first, the show from the widest cache");
    // Only a shelf, no listing at all (the library has not landed): both kinds still come from it.
    let shelf_only = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new())
        .with_shelf_sample([show("Sitcom", 2011, "Comedy"), poster("Other film", 1990, "Noir")].into());
    let c = cx_with(None, shelf_only.view());
    let titles: Vec<&str> = preview_posters(&c).iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Other film", "Sitcom"]);
}

/// Both libraries listed, a show library never opened: the cache holds a movie and no show.
fn movies_only_directory() -> plx_data::stores::browse::DirectorySnapshot {
    use plx_data::browse::SecKind;
    use plx_data::stores::browse::{SectionView, SrcRow};
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let section = |key, kind| SectionView { sid: Some(sid), key, kind, row: SrcRow { pinned: true, ..Default::default() } };
    plx_data::stores::browse::DirectorySnapshot::fixture(7, 0, vec![section(1, SecKind::Movie), section(2, SecKind::Show)])
        .with_sample([poster("Alpha", 2000, "Noir")].into())
}

/// The Browse hub reads `effects` asks for, as the `(epoch, section key)` of each.
fn hub_asks(effects: Vec<Stamped<InnerHost>>) -> Vec<(u32, i64)> {
    use plx_data::stores::{StoreCmd, StoreId};
    use plx_data::stores::browse::{BrowseCmd, LibraryWork};
    effects.into_iter().filter_map(|e| match e.fx {
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            target, work: LibraryWork::Hubs { may_publish: true } }))) => Some((target.epoch, target.section)),
        _ => None,
    }).collect()
}

fn step_page(page: &mut UnderPostersPage, ev: ScreenEvent<InnerHost>, c: &Cx<'_, InnerHost>) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    page.step(&ev, c, &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    out
}

/// Does `effects` pump the Browse store (`StoreWork::Browse`)?
fn pumps_browse(effects: &[Stamped<InnerHost>]) -> bool {
    effects.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::StoreWork(plx_data::stores::StoreWork::Browse))))
}

fn tick_of(dt_ms: u32) -> ScreenEvent<InnerHost> {
    ScreenEvent::Tick(Tick { ms: 0, dt_us: dt_ms * 1000 })
}

/// **A show tile is never left art-less only because the show library was not opened yet**: a kind
/// the cache has no poster of is asked for as the hubs of that kind's library (a read of its shelves
/// that does not switch the library the user is on), with the store pumped, on every tick until the
/// poster is there (a landed read is staged and only a later `Hubs` ask commits it, and nothing
/// lands unpumped). A kind that is cached, or whose library's shelves the store has published, is
/// not asked for.
#[test]
fn a_kind_with_no_cached_poster_is_asked_for_until_it_arrives_or_its_shelves_are_published() {
    use plx_data::browse::SecKind;
    let _guard = plx_base::testlock::serial();
    let snapshot = movies_only_directory();
    let c = cx_with(None, snapshot.view());
    assert_eq!(preview_posters(&c).len(), 1, "premise: a movie and no show is cached");
    let mut page = UnderPostersPage::new(EntryId(0));
    let mount = step_page(&mut page, ScreenEvent::Mount, &c);
    assert!(pumps_browse(&mount), "nothing lands unless the store is pumped");
    assert_eq!(hub_asks(mount), vec![(7, 2)], "the show library's shelves, and only that kind");
    let tick = step_page(&mut page, tick_of(16), &c);
    assert_eq!(hub_asks(tick), vec![(7, 2)], "asked again: the landed read is committed by the next ask");
    // Once the show is cached: nothing asked, nothing pumped.
    let full = movies_only_directory().with_shelf_sample([show("Sitcom", 2011, "Comedy")].into());
    let c2 = cx_with(None, full.view());
    assert_eq!(preview_posters(&c2).len(), 2);
    let tick = step_page(&mut page, tick_of(16), &c2);
    assert!(!pumps_browse(&tick));
    assert!(hub_asks(tick).is_empty());
    // A library whose shelves are published and hold no show is answered: no more asking, however
    // long that took (there is no timer to run out before a slow server answers).
    let answered = movies_only_directory().with_kind_shelves_published(SecKind::Show);
    let c3 = cx_with(None, answered.view());
    let tick = step_page(&mut page, tick_of(16), &c3);
    assert!(!pumps_browse(&tick));
    assert!(hub_asks(tick).is_empty());
}

/// The Settings root draws the same picture while its Under posters row has focus, and asks for the
/// missing poster then and only then, and only while it is the page on top (the pushed Under
/// posters page asks for itself, and the store must not be pumped twice a frame).
#[test]
fn the_settings_root_asks_for_the_missing_poster_only_on_its_under_posters_row_and_on_top() {
    use plx_ui::form::FormId;
    let _guard = plx_base::testlock::serial();
    // The Libraries section (and the Under posters row in it) is there only for a signed-in account.
    let _session = multi_user_session("under-posters-root-ask");
    let snapshot = movies_only_directory();
    let mut root = RootPage::new(EntryId(0), snapshot.view(), &crate::registry::capture_fixture());
    let on = |elem: u32| Some(FocusKey { entry: EntryId(0), elem });
    let tick = |root: &mut RootPage, focus| {
        let c = cx_with(focus, snapshot.view());
        let mut out = Vec::new();
        let mut present = Present::new();
        root.step(&tick_of(16), &c, &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
        out
    };
    let under = RootId::UnderPosters.key().0;
    form_focus(&mut root.form, RootId::Favourites.key().0);
    assert!(hub_asks(tick(&mut root, on(RootId::Favourites.key().0))).is_empty(), "another row: nothing asked");
    form_focus(&mut root.form, under);
    assert_eq!(root.form.selected_id(), Some(&RootId::UnderPosters), "premise: the row is selected");
    let asked = tick(&mut root, on(under));
    assert!(pumps_browse(&asked));
    assert_eq!(hub_asks(asked), vec![(7, 2)], "the Under posters row asks for the show library's shelves");
    let buried = tick(&mut root, Some(FocusKey { entry: EntryId(1), elem: 0 }));
    assert!(hub_asks(buried).is_empty(), "a page pushed over the root asks for itself: the root stays quiet");
}

#[test]
fn a_title_with_no_poster_path_is_not_art_and_never_a_sample() {
    let _guard = plx_base::testlock::serial();
    let bare = PmsMovie { thumb: String::new(), ..show("Bare", 2011, "Comedy") };
    let snapshot = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new())
        .with_sample([poster("Alpha", 2000, "Noir"), bare].into())
        .with_shelf_sample([show("Sitcom", 2011, "Comedy")].into());
    let c = cx_with(None, snapshot.view());
    let titles: Vec<&str> = preview_posters(&c).iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Alpha", "Sitcom"], "the show with no thumb is passed over for one that has art");
}

/// What one draw of the picture (both kinds) counts as placeholders, over `posters`.
fn placeholders_of(value: UnderPosters, posters: &[&PmsMovie]) -> plx_ui::placeholder::Frame {
    use plx_ui::follow_copy::PictureSlot;
    let picture = RootPicture { value, posters, sorted: false, opacity: [1.0, 1.0] };
    plx_ui::placeholder::capture_declared(|| picture.draw(plx_ui::Painter::root(), RouteLayout::settings_page().picture_slot(), &ShippedMeasure)).1
}

#[test]
fn the_picture_never_draws_a_skeleton_or_an_empty_art_key_card() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (movie, tv) = (poster("Alpha", 1999, "Noir"), show("Sitcom", 2011, "Comedy"));
    for value in every_value() {
        // neither kind has art anywhere: the finished art-less face, twice
        let none = placeholders_of(value, &[]);
        assert_eq!(none.count, 0, "{value:?}: no poster of either kind is not a placeholder: {none:?}");
        // one kind has art, the other has none
        let one = placeholders_of(value, &[&movie]);
        // only the movie's card waits (on its own poster, by its own key); the show wears the face
        assert_eq!(one.count, 1, "{value:?}: a show with no art is not a skeleton: {one:?}");
        assert_eq!(one.entries[0].key, movie.thumb, "{value:?}: the one card waiting is the movie's, by its art key: {one:?}");
        // a poster whose row has no poster path is no art either
        let bare = PmsMovie { thumb: String::new(), ..tv.clone() };
        let blank = placeholders_of(value, &[&bare]);
        assert_eq!(blank.of(plx_ui::placeholder::Reason::CardSkeleton), 0, "{value:?}: a row with no thumb draws the art-less face: {blank:?}");
        assert!(blank.entries.iter().all(|e| !e.key.is_empty()), "{blank:?}");
    }
}

#[test]
fn a_kind_with_art_still_draws_through_the_shared_card() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (movie, tv) = (poster("Alpha", 1999, "Noir"), show("Sitcom", 2011, "Comedy"));
    // With art, the card is the shared one: it waits on its poster like any tile (a counted skeleton
    // with its real key until the poster source answers), and never on an empty key.
    let seen = placeholders_of(UnderPosters::DEFAULT, &[&movie, &tv]);
    assert_eq!(seen.of(plx_ui::placeholder::Reason::CardSkeleton), 2, "two tiles of art, each still waiting on its poster: {seen:?}");
    assert!(seen.entries.iter().all(|e| !e.key.is_empty()), "every card names its art: {seen:?}");
}

#[test]
fn a_tile_takes_the_first_poster_of_its_kind_else_the_sample_of_that_kind() {
    let _guard = plx_base::testlock::serial();
    let (movie, tv) = (poster("Alpha", 1999, "Noir"), show("Sitcom", 2011, "Comedy"));
    let posters = [&tv, &movie];
    assert_eq!(poster_of(PosterKind::Movie, &posters).map(|p| p.title.as_str()), Some("Alpha"));
    assert_eq!(poster_of(PosterKind::Show, &posters).map(|p| p.title.as_str()), Some("Sitcom"));
    assert!(poster_of(PosterKind::Show, &[&movie]).is_none());
    assert!(picture_facts(PosterKind::Show, None).is_show && !picture_facts(PosterKind::Show, None).is_movie);
    assert!(picture_facts(PosterKind::Movie, None).is_movie && !picture_facts(PosterKind::Movie, None).is_show);
}

#[test]
fn a_line_the_setting_turns_on_is_never_drawn_empty_and_a_blank_setting_draws_no_word() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let off = up(false, &[], &[]);
    let words = |v: UnderPosters, posters: &[&PmsMovie]| -> Vec<String> {
        let picture = RootPicture { value: v, posters, sorted: false, opacity: [1.0, 1.0] };
        plx_ui::draw_census::capture_text(|| {
            plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), RouteLayout::settings_page().picture_slot(), &ShippedMeasure));
        }).into_iter().map(|(t, _)| t).collect()
    };
    assert_eq!(words(off, &[]), ["MOVIE", "TV SHOW"], "with no caption chosen the picture is faces under their kinds, and no line");
    let bare = PmsMovie { title: "Bare".into(), ..Default::default() };
    let year = up(false, &[P::Year], &[P::Year]);
    assert_eq!(words(year, &[&bare]).len(), 2 + 2, "a movie with not even a year still words a line, and so does the sample show (and each wears its kind)");
}

#[test]
fn both_pages_draw_for_every_value_without_a_painter() {
    let _guard = plx_base::testlock::serial();
    for value in every_value() {
        let _live = Live::set(value);
        let c = cx(None);
        let mut f = DrawFrame::new(&c, plx_ui::Painter::recording());
        <UnderPostersPage as Screen<InnerHost>>::draw(&mut UnderPostersPage::new(EntryId(0)), &mut f);
        for kind in [PosterKind::Movie, PosterKind::Show] {
            <DetailsPage as Screen<InnerHost>>::draw(&mut DetailsPage::new(EntryId(0), kind), &mut f);
        }
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
    assert!(probe.contains("show_details="), "the show parts are part of the probe: {probe}");
    let mut arg = String::new();
    SettingsPage::UnderPosters.probe(&mut arg);
    assert_eq!(arg, "under-posters");
    for kind in [PosterKind::Movie, PosterKind::Show] {
        let details = DetailsPage::new(EntryId(0), kind);
        let word = match kind { PosterKind::Movie => "under-posters-movie", PosterKind::Show => "under-posters-show" };
        assert_eq!(<DetailsPage as Screen<InnerHost>>::name(&details), word);
        let mut arg = String::new();
        SettingsPage::UnderPostersDetails(kind).probe(&mut arg);
        assert_eq!(arg, format!("under-posters-details[{}]", kind as u8));
    }
    let (mut movies, mut shows) = (String::new(), String::new());
    DetailsPage::new(EntryId(0), PosterKind::Movie).state.probe(&mut movies);
    DetailsPage::new(EntryId(0), PosterKind::Show).state.probe(&mut shows);
    assert_ne!(movies, shows, "the two pickers are told apart in the replay probe");
    // the replay digest reads the parts
    let mut a = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let before = a.state.hash();
    a.state.value = UnderPosters { details: parts(&[P::Genre]), ..a.state.value };
    assert_ne!(a.state.hash(), before, "a tick is part of the digest");
}

#[test]
fn the_sort_value_switch_uses_the_existing_confirmed_profile_write_and_reset() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    let (asked, reply) = write_request(activate(&mut page, UpId::SortValue)).unwrap();
    assert!(!asked.sort_value, "the default is On, so OK on the row asks for Off");
    assert_eq!(UnderPosters { sort_value: true, ..asked }, UnderPosters::DEFAULT, "nothing else changes");
    reply.send(false).unwrap(); page.poll();
    assert_eq!(page.state.value, UnderPosters::DEFAULT, "a refused save must not turn it off");
    plx_media::route::restore_under_posters(asked);
    page.poll();
    let (reset, _) = write_request(activate(&mut page, UpId::Reset)).unwrap();
    assert_eq!(reset, UnderPosters::DEFAULT);
}

/// With the Sort value row focused the picture words each tile's own year (a library grid sorted by
/// Year), whatever kind of item the tile holds, and a year the server did not send is the dash.
#[test]
fn the_sort_value_picture_words_the_year_of_every_kind_of_item() {
    use plx_data::pms::KIND_SHOW;
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let value = UnderPosters { sort_value: true, ..up(false, &[], &[]) };
    let drawn = |posters: &[&PmsMovie]| -> Vec<String> {
        let picture = RootPicture { value, posters, sorted: true, opacity: [1.0, 1.0] };
        plx_ui::draw_census::capture_text(|| {
            plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), RouteLayout::settings_page().picture_slot(), &ShippedMeasure));
        }).into_iter().map(|(t, _)| t).collect()
    };
    let movie = poster("A", 1999, "Noir");
    let tv = PmsMovie { kind: KIND_SHOW, ..poster("B", 2004, "Noir") };
    assert_eq!(drawn(&[&movie, &tv]), ["MOVIE", "1999", "TV SHOW", "2004"]);
    // a kind with no poster wears the sample's year
    assert_eq!(drawn(&[&movie]), ["MOVIE", "1999", "TV SHOW", SAMPLE_YEAR.to_string().as_str()]);
    let nameless = PmsMovie { year: 0, ..poster("N", 0, "Noir") };
    assert!(drawn(&[&nameless]).iter().any(|t| t == "\u{2013}"), "a missing year is the en dash");
}

/// **The Under posters picture paints into the slot it is handed** (the v3 board): a movie poster on
/// the left and a TV show poster on the right, 410 px apart from the slot's left edge, each 250 x 375
/// with its kind centred over it and its lines (the bold title above the details, when Title is on)
/// centred under it in the slot's own box. Handed a different slot, the same picture moves with it.
#[test]
fn the_picture_paints_into_the_slot_it_is_handed() {
    use plx_ui::follow_copy::PictureSlot;
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    assert_eq!((PICTURE_TILE_W, PICTURE_TILE_H, PICTURE_TILE_STEP), (250.0, 375.0, 410.0), "the board's tiles");
    let picture = RootPicture { value: UnderPosters::DEFAULT, posters: &[], sorted: false, opacity: [1.0, 1.0] };
    let at = |slot: Rect| {
        plx_ui::draw_census::capture_text(|| {
            plx_gfx::gfx::without_frame_clear(|| picture.draw(plx_ui::Painter::recording(), slot, &ShippedMeasure));
        })
    };
    let slot = RouteLayout::settings_page().picture_slot();
    assert!(slot.w >= PICTURE_TILE_STEP + PICTURE_TILE_W, "the slot holds both tiles: {slot:?}");
    let runs = at(slot);
    let words: Vec<&str> = runs.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(words, ["MOVIE", "2024 · 1h 52m", "TV SHOW", "2019 · 6 seasons"], "each kind over its tile, then its details line under it");
    let tile_top = slot.y + PICTURE_TILE_TOP;
    let tile_bottom = tile_top + PICTURE_TILE_H;
    for (i, (kind, line)) in [(&runs[0], &runs[1]), (&runs[2], &runs[3])].into_iter().enumerate() {
        let tile_x = slot.x + PICTURE_TILE_STEP * i as f32;
        let tile_cx = tile_x + PICTURE_TILE_W * 0.5;
        // a recorded run's rect is its ANCHOR (the centre, for a centred run) and measured width
        assert!((line.1.x - tile_cx).abs() <= 1.0, "{:?} is centred under its tile: anchor {} vs {tile_cx}", line.0, line.1.x);
        assert!(line.1.y >= tile_bottom && line.1.y <= tile_bottom + PICTURE_LINE_GAP + PICTURE_LINE_H,
            "{:?} stands in the line box under the tile: {:?}", line.0, line.1);
        // the kind is centred over its tile, clear of it
        assert!((kind.1.x - tile_cx).abs() <= 1.0, "{:?} is centred over its tile: {:?}", kind.0, kind.1);
        assert!(kind.1.y >= slot.y - theme::size::CAPTION as f32 && kind.1.y + plx_ui::table::caps_band_h() <= tile_top,
            "{:?} is over the tile: {:?} (tile top {tile_top})", kind.0, kind.1);
    }
    let moved = Rect::new(slot.x + 123.0, slot.y + 45.0, slot.w, slot.h);
    for ((_, a), (_, b)) in runs.iter().zip(at(moved).iter()) {
        assert!((b.x - a.x - 123.0).abs() < 0.01 && (b.y - a.y - 45.0).abs() < 0.01, "the picture follows its slot: {a:?} -> {b:?}");
    }
    // The title plus the longest details block still ends inside the slot.
    let floor = tile_bottom + PICTURE_LINE_GAP + PICTURE_LINE_STEP * PICTURE_DETAIL_LINES as f32 + PICTURE_LINE_H;
    assert!(floor <= slot.y + slot.h, "preview ends at {floor}, the slot's floor is {}", slot.y + slot.h);
}

/// With Title on the title stands above the details, in bold, on each tile.
#[test]
fn with_title_on_the_bold_title_stands_above_the_details_line() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let slot = RouteLayout::settings_page().picture_slot();
    let picture = RootPicture { value: UnderPosters { title: true, ..UnderPosters::DEFAULT }, posters: &[], sorted: false, opacity: [1.0, 1.0] };
    let runs = plx_ui::draw_census::capture_runs(|| {
        plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), slot, &ShippedMeasure));
    });
    let words: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(words, ["MOVIE", "Movie", "2024 · 1h 52m", "TV SHOW", "Show", "2019 · 6 seasons"]);
    for (title, details) in [(&runs[1], &runs[2]), (&runs[4], &runs[5])] {
        assert!(title.bold && !details.bold, "{title:?} {details:?}");
        assert!(title.at.y < details.at.y, "the title is above the details: {title:?} {details:?}");
    }
}

/// **The kind captions are the section headers' own caps**: the table's header size, tracked,
/// upper-cased from the catalog's kind words (`Movie`, `TV Show`), whatever the poster art, the
/// setting or the Sort value preview, in the reading ink while their tile is usable.
#[test]
fn the_kind_captions_wear_the_section_header_look_under_every_setting() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (movie, tv) = (poster("Alpha", 1999, "Noir"), show("Sitcom", 2011, "Comedy"));
    let slot = RouteLayout::settings_page().picture_slot();
    for value in every_value() {
        for (sorted, posters) in [(false, vec![]), (false, vec![&movie, &tv]), (true, vec![&movie, &tv])] {
            let picture = RootPicture { value, posters: &posters, sorted, opacity: [1.0, 1.0] };
            let runs = plx_ui::draw_census::capture_runs(|| {
                plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), slot, &ShippedMeasure));
            });
            let kinds: Vec<_> = runs.iter().filter(|r| r.text == "MOVIE" || r.text == "TV SHOW").collect();
            assert_eq!(kinds.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["MOVIE", "TV SHOW"], "{value:?} sorted={sorted}");
            for r in kinds {
                assert_eq!((r.size, r.bold, r.ink), (theme::size::CAPTION, false, theme::TEXT_READING), "{r:?}");
            }
        }
    }
}

/// All combinations of the preview's examples fit across the two detail lines in every locale.
#[test]
fn the_preview_draws_every_selected_example_inside_its_caption_column() {
    use plx_machine::machine::Measure;
    let _guard = plx_base::testlock::serial();
    let slot = RouteLayout::settings_page().picture_slot();
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        for (i, kind) in [PosterKind::Movie, PosterKind::Show].into_iter().enumerate() {
            let centre = slot.x + PICTURE_TILE_STEP * i as f32 + PICTURE_TILE_W * 0.5;
            for set in subsets(kind.offered()) {
                let value = kind.with_parts(UnderPosters { title: true, ..UnderPosters::DEFAULT }, set);
                let [_, details] = plx_ui::caption_policy::caption_lines(
                    &caption_policy_of(value), &kind_sample(kind), plx_platform::i18n::current(),
                );
                let picture = RootPicture { value, posters: &[], sorted: false, opacity: [1.0, 1.0] };
                let runs = plx_ui::draw_census::capture_runs(|| {
                    plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(
                        &picture, plx_ui::Painter::recording(), slot, &ShippedMeasure,
                    ));
                });
                let lines: Vec<_> = runs.iter()
                    .filter(|r| r.ink == theme::CAPTION_DETAIL_INK && (r.at.x - centre).abs() < 1.0)
                    .map(|r| r.text.as_str()).collect();
                let fits = lines.len() <= PICTURE_DETAIL_LINES && lines.iter().all(|text|
                    ShippedMeasure.width_str(text, theme::size::CAPTION, false) <= PICTURE_LINE_W);
                if !fits || normalize(&lines.join(" ")) != normalize(details.as_deref().unwrap_or("")) {
                    out.push(format!("{} {kind:?} {set:?}: {details:?} drawn as {lines:?}", language.tag()));
                }
            }
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}
