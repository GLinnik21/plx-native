//! Settings > Libraries > Under posters: the rows, the write door, the policy mapping, the picture,
//! the sentences each row states and the fit of every string in every shipped language.
use super::*;
use super::super::test_support::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_machine::present::Present;

/// Every value the page can show: both title states over every details line (a show holding the
/// movie line's twin, never Video quality).
fn every_value() -> Vec<UnderPosters> {
    let mut all = Vec::new();
    for title in [false, true] {
        for details in UnderPostersDetails::LADDER {
            all.push(UnderPosters { title, details, show_details: details.for_shows(), sort_value: false });
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
        page.form.index_of_key(DetailsId(*d).key()).is_some_and(|i| marks[i])
    }).collect()
}

/// The lines a picker lists, in order.
fn listed(page: &DetailsPage) -> Vec<UnderPostersDetails> {
    UnderPostersDetails::LADDER.into_iter().filter(|d| page.form.index_of_key(DetailsId(*d).key()).is_some()).collect()
}

fn rows_of(page: &DetailsPage) -> Vec<(String, Option<String>)> {
    page.form.table.sections.iter().flat_map(|s| s.rows.iter()).map(|r| (r.label.to_string(), r.value.clone())).collect()
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

fn poster(title: &str, year: i32, genre: &str) -> PmsMovie {
    PmsMovie { title: title.into(), year, genre: Some(genre.into()), thumb: format!("/library/metadata/{title}/thumb"), ..Default::default() }
}

fn show(title: &str, year: i32, genre: &str) -> PmsMovie {
    PmsMovie { kind: 1, ..poster(title, year, genre) }
}

const DOT: char = '\u{b7}';

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
fn each_details_row_reads_its_kinds_line_and_opens_its_own_picker() {
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

#[test]
fn the_rows_read_each_kinds_stored_line_independently() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { details: UnderPostersDetails::Video, show_details: UnderPostersDetails::AgeRatingScore, ..UnderPosters::DEFAULT });
    let page = UnderPostersPage::new(EntryId(0));
    let value_of = |id: UpId| {
        let i = page.form.index_of_key(id.key()).unwrap();
        page.form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i).and_then(|r| r.value.clone())
    };
    assert_eq!(value_of(UpId::MovieDetails).as_deref(), Some("Video"));
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

#[test]
fn the_movie_picker_lists_nothing_first_each_line_with_its_sample_and_checks_the_current() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    assert_eq!(rows_of(&page), vec![
        ("Nothing".to_string(), None),
        (format!("Year {DOT} Length"), Some(format!("2024 {DOT} 1h 52m"))),
        ("Genre".to_string(), Some("Drama".to_string())),
        ("Video".to_string(), Some(format!("4K {DOT} HDR"))),
        (format!("Age rating {DOT} Score"), Some(format!("PG-13 {DOT} 7.8"))),
    ]);
    assert_eq!(checked(&page), vec![UnderPostersDetails::YearLength]);
}

#[test]
fn the_show_picker_never_lists_video_quality_and_words_its_year_line_with_seasons() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let page = DetailsPage::new(EntryId(0), PosterKind::Show);
    assert_eq!(rows_of(&page), vec![
        ("Nothing".to_string(), None),
        (format!("Year {DOT} Seasons"), Some(format!("2019 {DOT} 6 seasons"))),
        ("Genre".to_string(), Some("Drama".to_string())),
        (format!("Age rating {DOT} Score"), Some(format!("TV-14 {DOT} 8.4"))),
    ]);
    assert!(!listed(&page).contains(&UnderPostersDetails::Video));
    assert_eq!(checked(&page), vec![UnderPostersDetails::YearLength]);
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
    assert_eq!(sample(&movies, &format!("Year {DOT} Length")), Some(format!("2022 {DOT} 6m")));
    let shows = mount(PosterKind::Show);
    assert_eq!(sample(&shows, "Genre").as_deref(), Some("Comedy"), "the first show");
    // …and each sample is what the picture's tile of that kind wears under the same choice
    let posters = preview_posters(&c);
    for (page, kind) in [(&movies, PosterKind::Movie), (&shows, PosterKind::Show)] {
        for d in kind.picker_order() {
            let value = rows_of(page).into_iter().find(|(l, _)| *l == details_label(kind, d)).and_then(|(_, v)| v);
            let v = UnderPosters { title: false, ..kind.with_details(UnderPosters::DEFAULT, d) };
            assert_eq!(value, picture_lines(v, kind, poster_of(kind, &posters), false)[1], "{kind:?} {d:?}");
        }
    }
}

#[test]
fn a_pick_keeps_the_rest_writes_once_and_pops_when_the_receipt_is_durable() {
    let _guard = plx_base::testlock::serial();
    let start = UnderPosters { title: true, details: UnderPostersDetails::Video, show_details: UnderPostersDetails::AgeRatingScore, sort_value: false };
    let _live = Live::set(start);
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let effects = pick(&mut page, UnderPostersDetails::Genre);
    assert!(!pops(&effects), "the page waits for the receipt");
    let (asked, reply) = write_request(effects).expect("a write");
    assert_eq!(asked, UnderPosters { details: UnderPostersDetails::Genre, ..start }, "the show's line and the title are kept");
    assert_eq!(checked(&page), vec![UnderPostersDetails::Genre], "the row shows the pick at once");
    assert!(!polled(&mut page).0, "waiting never blocks the frame");
    plx_media::route::restore_under_posters(asked);
    reply.send(true).unwrap();
    let (settled, out) = polled(&mut page);
    assert!(settled && pops(&out), "a durable write pops the picker");
}

#[test]
fn a_show_pick_changes_only_the_show_line() {
    let _guard = plx_base::testlock::serial();
    let start = UnderPosters { title: true, details: UnderPostersDetails::Video, show_details: UnderPostersDetails::YearLength, sort_value: true };
    let _live = Live::set(start);
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Show);
    let (asked, _reply) = write_request(pick(&mut page, UnderPostersDetails::Genre)).expect("a write");
    assert_eq!(asked, UnderPosters { show_details: UnderPostersDetails::Genre, ..start }, "the movie line stays Video quality");
}

#[test]
fn a_refused_pick_puts_the_checkmark_back_and_stays() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Show);
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
    let _live = Live::set(UnderPosters { details: UnderPostersDetails::Video, ..UnderPosters::DEFAULT });
    let mut page = DetailsPage::new(EntryId(0), PosterKind::Movie);
    let effects = pick(&mut page, UnderPostersDetails::Video);
    assert!(pops(&effects));
    assert!(write_request(effects).is_none());
}

#[test]
fn a_show_can_never_be_written_video_quality() {
    let _guard = plx_base::testlock::serial();
    for d in UnderPostersDetails::LADDER {
        let v = PosterKind::Show.with_details(UnderPosters::DEFAULT, d);
        assert_ne!(v.show_details, UnderPostersDetails::Video, "{d:?}");
    }
    assert_eq!(PosterKind::Show.with_details(UnderPosters::DEFAULT, UnderPostersDetails::Video).show_details, UnderPostersDetails::YearLength);
    assert_eq!(PosterKind::Movie.with_details(UnderPosters::DEFAULT, UnderPostersDetails::Video).details, UnderPostersDetails::Video);
}

#[test]
fn the_pickers_picture_follows_the_focused_choice_of_its_kind() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters::DEFAULT);
    for kind in [PosterKind::Movie, PosterKind::Show] {
        let mut page = DetailsPage::new(EntryId(0), kind);
        assert_eq!(page.state.focused(), UnderPostersDetails::YearLength);
        page.state.sel = DetailsId(UnderPostersDetails::Genre).key();
        assert_eq!(page.state.focused(), UnderPostersDetails::Genre);
        assert_eq!(kind.details(page.state.value), UnderPostersDetails::YearLength, "focus is not a pick");
    }
}

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

#[test]
fn the_pickers_picture_words_its_own_kinds_tile_with_the_focused_choice() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, sort_value: true, ..UnderPosters::DEFAULT });
    let mut movies = DetailsPage::new(EntryId(0), PosterKind::Movie);
    movies.state.sel = DetailsId(UnderPostersDetails::Genre).key();
    let texts = drawn_texts(&mut movies);
    assert!(texts.iter().any(|t| t == "Noir"), "the focused Genre choice is what the movie tile words: {texts:?}");
    assert!(!texts.iter().any(|t| t == "Comedy" || t == "Sitcom"), "a movie picker draws no show tile: {texts:?}");
    assert!(!texts.iter().any(|t| t == "1999"), "the sort value is not the picker's picture: {texts:?}");
    movies.state.sel = DetailsId(UnderPostersDetails::None).key();
    let none = drawn_texts(&mut movies);
    assert!(!none.iter().any(|t| t == "Noir"), "the Nothing choice words no details line: {none:?}");
    assert!(none.iter().any(|t| t == "Alpha"), "the Title switch still shows the title: {none:?}");
    let mut shows = DetailsPage::new(EntryId(0), PosterKind::Show);
    shows.state.sel = DetailsId(UnderPostersDetails::Genre).key();
    let texts = drawn_texts(&mut shows);
    assert!(texts.iter().any(|t| t == "Comedy") && texts.iter().any(|t| t == "Sitcom"), "the show tile: {texts:?}");
    assert!(!texts.iter().any(|t| t == "Noir"), "a show picker draws no movie tile: {texts:?}");
}

#[test]
fn the_pages_picture_draws_a_tile_of_each_kind_from_the_library() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, show_details: UnderPostersDetails::Genre, sort_value: true });
    let mut page = UnderPostersPage::new(EntryId(0));
    for row in [UpId::Title, UpId::MovieDetails, UpId::ShowDetails, UpId::Reset] {
        page.state.sel = row.key();
        let texts = drawn_texts(&mut page);
        for want in ["Alpha", "Noir", "Sitcom"] {
            assert!(texts.iter().any(|t| t == want), "{row:?} focused: {want:?} in {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "1999" || t == "2011"), "{row:?} focused: no year lines: {texts:?}");
    }
}

#[test]
fn the_picture_shows_each_posters_year_only_while_the_sort_value_row_has_focus() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: true, ..UnderPosters::DEFAULT });
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
    {
        let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::Genre, show_details: UnderPostersDetails::None, sort_value: false });
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
fn a_picker_opens_on_its_kinds_stored_value() {
    let _guard = plx_base::testlock::serial();
    let _live = Live::set(UnderPosters { title: true, details: UnderPostersDetails::AgeRatingScore, show_details: UnderPostersDetails::Genre, sort_value: false });
    let movies = DetailsPage::new(EntryId(0), PosterKind::Movie);
    assert_eq!(checked(&movies), vec![UnderPostersDetails::AgeRatingScore]);
    assert_eq!(movies.state.sel, DetailsId(UnderPostersDetails::AgeRatingScore).key(), "the cursor starts on the checked line");
    let shows = DetailsPage::new(EntryId(0), PosterKind::Show);
    assert_eq!(checked(&shows), vec![UnderPostersDetails::Genre]);
    assert_eq!(shows.state.sel, DetailsId(UnderPostersDetails::Genre).key());
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
            let v = UnderPosters { title, details: stored, show_details: UnderPostersDetails::Genre, sort_value: false };
            assert_eq!(caption_policy_of(v), CaptionPolicy { title, details: kind, show_details: DetailsKind::Genre },
                "the movie line maps on its own");
            let v = UnderPosters { title, details: UnderPostersDetails::Genre, show_details: stored, sort_value: false };
            assert_eq!(caption_policy_of(v), CaptionPolicy { title, details: DetailsKind::Genre, show_details: kind },
                "the show line maps on its own");
        }
    }
}

#[test]
fn the_band_follows_either_kinds_line() {
    let h = |title, details, show_details| plx_ui::caption_policy::band_h(&caption_policy_of(UnderPosters { title, details, show_details, sort_value: false }));
    let none = UnderPostersDetails::None;
    assert_eq!(h(false, none, none), 0.0);
    assert_eq!(h(false, UnderPostersDetails::YearLength, UnderPostersDetails::YearLength), 80.0);
    assert_eq!(h(true, none, none), 80.0);
    assert_eq!(h(true, UnderPostersDetails::Genre, UnderPostersDetails::Genre), 112.0);
    assert_eq!(h(false, none, UnderPostersDetails::Genre), 80.0, "a band opens when only the show line has words");
    assert_eq!(h(false, UnderPostersDetails::Genre, none), 80.0, "a band opens when only the movie line has words");
}

#[test]
fn the_picture_is_captioned_by_the_same_policy() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let none = UnderPostersDetails::None;
    let lines = |v, kind| picture_lines(v, kind, None, false);
    for kind in [PosterKind::Movie, PosterKind::Show] {
        assert_eq!(lines(UnderPosters { title: false, details: none, show_details: none, sort_value: false }, kind), [None, None]);
    }
    let default = lines(UnderPosters::DEFAULT, PosterKind::Movie);
    assert_eq!(default[0], None, "the default shows no title");
    assert_eq!(default[1].as_deref(), Some("2024 \u{b7} 1h 52m"));
    assert_eq!(lines(UnderPosters::DEFAULT, PosterKind::Show)[1].as_deref(), Some("2019 \u{b7} 6 seasons"));
    let both = lines(UnderPosters { title: true, details: UnderPostersDetails::Genre, show_details: UnderPostersDetails::Genre, sort_value: false }, PosterKind::Movie);
    assert_eq!(both[0].as_deref(), Some("Movie"));
    assert_eq!(both[1].as_deref(), Some("Drama"));
    let video = UnderPosters { details: UnderPostersDetails::Video, ..UnderPosters::DEFAULT };
    assert_eq!(lines(video, PosterKind::Movie)[1].as_deref(), Some("4K \u{b7} HDR"));
    assert_eq!(lines(video, PosterKind::Show)[1].as_deref(), Some("2019 \u{b7} 6 seasons"), "a show's own line, not the movie's");
    let age = UnderPosters { details: UnderPostersDetails::AgeRatingScore, show_details: UnderPostersDetails::AgeRatingScore, ..UnderPosters::DEFAULT };
    assert_eq!(lines(age, PosterKind::Movie)[1].as_deref(), Some("PG-13 \u{b7} 7.8"));
    assert_eq!(lines(age, PosterKind::Show)[1].as_deref(), Some("TV-14 \u{b7} 8.4"));
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

/// The Under posters row reads its setting like its siblings do: the MOVIE details line the page's
/// Movie details row reads, and "Title" when the title is all there is (never "Nothing" beside a
/// poster that wears its name). The show line is one level down.
#[test]
fn the_root_row_reads_the_setting() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let read = |title, details| root_row_text(&RootInputs { under_posters: UnderPosters { title, details, ..UnderPosters::DEFAULT }, ..text_fit_inputs() });
    let (value, sub) = read(false, UnderPostersDetails::YearLength);
    assert_eq!(value.as_deref(), Some("Year \u{b7} Length"), "the default reads its details line");
    assert_eq!(sub, "", "Settings v2: the root row has no sub-line");
    assert_eq!(super::super::sentence_for(Some(&RootId::UnderPosters)), "Under posters, for movies and TV shows.",
        "the sentence moved beside the table");
    assert_eq!(read(true, UnderPostersDetails::YearLength).0.as_deref(), Some("Year \u{b7} Length"), "the title is the page's own row");
    assert_eq!(read(true, UnderPostersDetails::Genre).0.as_deref(), Some("Genre"));
    assert_eq!(read(false, UnderPostersDetails::Video).0.as_deref(), Some("Video"));
    assert_eq!(read(true, UnderPostersDetails::None).0.as_deref(), Some("Title"));
    assert_eq!(read(false, UnderPostersDetails::None).0.as_deref(), Some("Nothing"));
    let shows_only = UnderPosters { details: UnderPostersDetails::Genre, show_details: UnderPostersDetails::AgeRatingScore, ..UnderPosters::DEFAULT };
    assert_eq!(root_row_text(&RootInputs { under_posters: shows_only, ..text_fit_inputs() }).0.as_deref(), Some("Genre"), "the movie line, not the show's");
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
    let stored = UnderPosters { title: true, details: UnderPostersDetails::Genre, show_details: UnderPostersDetails::None, sort_value: false };
    let _live = Live::set(stored);
    let page = RootPage::new(EntryId(0), test_support::cx(None).views.directory);
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
fn the_page_opens_on_its_first_rows_sentence_and_follows_focus() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let mut page = UnderPostersPage::new(EntryId(0));
    assert_eq!(page.follow.text(), sentence_for(Some(&UpId::Title)), "no fade on arrival");
    let entry = page.entry;
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    let to = FocusKey { entry, elem: UpId::ShowDetails.key().0 };
    page.step(&ScreenEvent::FocusMoved { from: None, to, by: plx_ui::screen::By::Dir }, &cx(None), &mut fx);
    assert_eq!(page.follow.text(), sentence_for(Some(&UpId::ShowDetails)));
    assert!(page.follow.arrival() < 1.0, "a new sentence fades in");
}

// ---- fit ------------------------------------------------------------------------------------

/// **Every string fits, in every shipped language**: both pages' rows, both pickers, the root row
/// beside every value it can read, and every sentence a row or picker states in the five lines the
/// left column gives it (the board's rule for German, French and Belarusian), clear of the picture.
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
                let mut picker = FormTable::<DetailsId, UnderPostersDetails, SettingsPage>::new(crate::registry::BAND);
                picker.table.compact = false;
                picker.table.board_metrics(true);
                picker.set(details_form(kind, kind.details(value), &picture_facts(kind, None)), None);
                out.extend(picker.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} {kind:?} details picker {value:?}")));
            }
            let mut root = FormTable::<RootId, Action, SettingsPage>::new(crate::registry::BAND);
            root.table.compact = false;
            root.table.board_metrics(true);
            root.set(root_form(&RootInputs { under_posters: value, ..text_fit_inputs() }), None);
            out.extend(root.table.app_fit_failures(layout.sectioned_table().w, &format!("{tag} settings root {value:?}")));
        }
        // each sentence ends above the picture slot, in at most the lines the column gives it
        let mut sentences: Vec<(String, &str, &str)> = [UpId::Title, UpId::MovieDetails, UpId::ShowDetails, UpId::SortValue, UpId::Reset]
            .iter().map(|id| (format!("{id:?}"), plx_platform::i18n::msg::under_posters_title(), sentence_for(Some(id)))).collect();
        sentences.push(("page".into(), plx_platform::i18n::msg::under_posters_title(), plx_platform::i18n::msg::under_posters_copy()));
        for kind in [PosterKind::Movie, PosterKind::Show] {
            sentences.push((format!("{kind:?} picker"), kind.row_label(), kind.picker_copy()));
        }
        for (what, title, copy) in sentences {
            let frame = layout.narrative_copy_frame(true, title, layout.action.y, &ShippedMeasure);
            let drop = layout.copy_cap_drop(theme::size::LABEL, &ShippedMeasure);
            let leading = theme::size::LABEL as f32 + theme::space::XS;
            let view = TextView::new(copy, theme::size::LABEL, theme::TEXT_READING)
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
fn placeholders_of(value: UnderPosters, posters: &[&PmsMovie], kinds: &[PosterKind]) -> plx_ui::placeholder::Frame {
    use plx_ui::follow_copy::PictureSlot;
    let picture = RootPicture { value, posters, kinds, sorted: false };
    plx_ui::placeholder::capture_declared(|| picture.draw(plx_ui::Painter::root(), RouteLayout::settings_page().picture_slot(), &ShippedMeasure)).1
}

#[test]
fn the_picture_never_draws_a_skeleton_or_an_empty_art_key_card() {
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (movie, tv) = (poster("Alpha", 1999, "Noir"), show("Sitcom", 2011, "Comedy"));
    let kinds = [PosterKind::Movie, PosterKind::Show];
    for value in every_value() {
        // neither kind has art anywhere: the finished art-less face, twice
        let none = placeholders_of(value, &[], &kinds);
        assert_eq!(none.count, 0, "{value:?}: no poster of either kind is not a placeholder: {none:?}");
        // one kind has art, the other has none
        let one = placeholders_of(value, &[&movie], &kinds);
        // only the movie's card waits (on its own poster, by its own key); the show wears the face
        assert_eq!(one.count, 1, "{value:?}: a show with no art is not a skeleton: {one:?}");
        assert_eq!(one.entries[0].key, movie.thumb, "{value:?}: the one card waiting is the movie's, by its art key: {one:?}");
        // a poster whose row has no poster path is no art either
        let bare = PmsMovie { thumb: String::new(), ..tv.clone() };
        let blank = placeholders_of(value, &[&bare], &[PosterKind::Show]);
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
    let seen = placeholders_of(UnderPosters::DEFAULT, &[&movie, &tv], &[PosterKind::Movie, PosterKind::Show]);
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
    let none = UnderPostersDetails::None;
    let off = UnderPosters { title: false, details: none, show_details: none, sort_value: false };
    let words = |v: UnderPosters, posters: &[&PmsMovie]| -> Vec<String> {
        let picture = RootPicture { value: v, posters, kinds: &[PosterKind::Movie, PosterKind::Show], sorted: false };
        plx_ui::draw_census::capture_text(|| {
            plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), RouteLayout::settings_page().picture_slot(), &ShippedMeasure));
        }).into_iter().map(|(t, _)| t).collect()
    };
    assert!(words(off, &[]).is_empty(), "with no caption chosen the picture is faces only");
    let bare = PmsMovie { title: "Bare".into(), ..Default::default() };
    let genre = UnderPosters { details: UnderPostersDetails::Genre, show_details: UnderPostersDetails::Genre, ..off };
    assert_eq!(words(genre, &[&bare]).len(), 2, "a movie with no genre still words a line, and so does the sample show");
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
    assert!(probe.contains("show_details="), "the show line is part of the probe: {probe}");
    let mut arg = String::new();
    SettingsPage::UnderPosters.probe(&mut arg);
    assert_eq!(arg, "under-posters");
    for kind in [PosterKind::Movie, PosterKind::Show] {
        let details = DetailsPage::new(EntryId(0), kind);
        assert_eq!(<DetailsPage as Screen<InnerHost>>::name(&details), "under-posters-details");
        let mut arg = String::new();
        SettingsPage::UnderPostersDetails(kind).probe(&mut arg);
        assert_eq!(arg, format!("under-posters-details[{}]", kind as u8));
    }
    let (mut movies, mut shows) = (String::new(), String::new());
    DetailsPage::new(EntryId(0), PosterKind::Movie).state.probe(&mut movies);
    DetailsPage::new(EntryId(0), PosterKind::Show).state.probe(&mut shows);
    assert_ne!(movies, shows, "the two pickers are told apart in the replay probe");
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
    let value = UnderPosters { title: false, details: UnderPostersDetails::None, show_details: UnderPostersDetails::None, sort_value: true };
    let drawn = |posters: &[&PmsMovie]| -> Vec<String> {
        let picture = RootPicture { value, posters, kinds: &[PosterKind::Movie, PosterKind::Show], sorted: true };
        plx_ui::draw_census::capture_text(|| {
            plx_gfx::gfx::without_frame_clear(|| plx_ui::follow_copy::PictureSlot::draw(&picture, plx_ui::Painter::recording(), RouteLayout::settings_page().picture_slot(), &ShippedMeasure));
        }).into_iter().map(|(t, _)| t).collect()
    };
    let movie = poster("A", 1999, "Noir");
    let tv = PmsMovie { kind: KIND_SHOW, ..poster("B", 2004, "Noir") };
    assert_eq!(drawn(&[&movie, &tv]), ["1999", "2004"]);
    // a kind with no poster wears the sample's year
    assert_eq!(drawn(&[&movie]), ["1999", SAMPLE_YEAR.to_string().as_str()]);
    let nameless = PmsMovie { year: 0, ..poster("N", 0, "Noir") };
    assert!(drawn(&[&nameless]).iter().any(|t| t == "\u{2013}"), "a missing year is the en dash");
}

/// **The Under posters picture paints into the slot it is handed** (board `Sv2-2`): a movie and a
/// show, 240 px apart from the slot's left edge, each with its details line centred under it in the
/// slot's own box. Handed a different slot, the same picture moves with it.
#[test]
fn the_picture_paints_into_the_slot_it_is_handed() {
    use plx_ui::follow_copy::PictureSlot;
    let _guard = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let _live = Live::set(UnderPosters::DEFAULT);
    let picture = RootPicture { value: UnderPosters::DEFAULT, posters: &[], kinds: &[PosterKind::Movie, PosterKind::Show], sorted: false };
    let at = |slot: Rect| {
        plx_ui::draw_census::capture_text(|| {
            plx_gfx::gfx::without_frame_clear(|| picture.draw(plx_ui::Painter::recording(), slot, &ShippedMeasure));
        })
    };
    let slot = RouteLayout::settings_page().picture_slot();
    let runs = at(slot);
    let words: Vec<&str> = runs.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(words, ["2024 · 1h 52m", "2019 · 6 seasons"], "the movie's and the show's details lines, and no label or title");
    for (i, (word, rect)) in runs.iter().enumerate() {
        let tile_cx = slot.x + PICTURE_TILE_STEP * i as f32 + PICTURE_TILE_W * 0.5;
        // a recorded run's rect is its ANCHOR (the centre, for a centred run) and measured width
        assert!((rect.x - tile_cx).abs() <= 1.0, "{word:?} is centred under its tile: anchor {} vs {tile_cx}", rect.x);
        assert!(rect.y >= slot.y + PICTURE_TILE_H && rect.y <= slot.y + PICTURE_LINE_TOP + PICTURE_LINE_H,
            "{word:?} stands in the line box under the tile: {rect:?}");
    }
    let moved = Rect::new(slot.x + 123.0, slot.y + 45.0, slot.w, slot.h);
    for ((_, a), (_, b)) in runs.iter().zip(at(moved).iter()) {
        assert!((b.x - a.x - 123.0).abs() < 0.01 && (b.y - a.y - 45.0).abs() < 0.01, "the picture follows its slot: {a:?} -> {b:?}");
    }
    // one kind, one tile: a picker draws only its own
    let one = RootPicture { kinds: &[PosterKind::Show], ..picture };
    let shows: Vec<String> = plx_ui::draw_census::capture_text(|| {
        plx_gfx::gfx::without_frame_clear(|| one.draw(plx_ui::Painter::recording(), slot, &ShippedMeasure));
    }).into_iter().map(|(t, _)| t).collect();
    assert_eq!(shows, ["2019 · 6 seasons"]);
}
