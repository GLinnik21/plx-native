// The Sort value line on a TV library's SEASONS and EPISODES views (the owner's photo, 2026-10-31: the
// Episodes view sorted by Audience score drew nothing under its landscape cards): the gate, the row
// pitch and the focused row's open gap, what the focused card says, what the setting's Off restores,
// and the values' fit on the real card widths. The text per sort term is `registry::sort_info`'s own
// tests; the gate's table is `sort_values_show_on_the_four_listings...` in `library/mod.rs`.
use super::*;
use plx_data::browse::LibraryType;
use plx_data::pms::{PmsMovie, SortInfo, KIND_EPISODE, KIND_SEASON};
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use super::grid_motion_tests::settle;

const SHOW_ORDER_SEASONS: &str = "show.titleSort,index";
const SHOW_ORDER_EPISODES: &str = "show.titleSort,season.index:nullsLast,episode.index:nullsLast,\
    episode.originallyAvailableAt:nullsLast,episode.titleSort,episode.id";
/// What the audience score of the fixture's rows reads as (TMDB's percentage, a no-break space).
const SCORE: &str = "TMDB\u{a0}84%";

/// The setting a test runs under, put back when it ends (a panic included).
struct Setting;
impl Setting {
    fn set(setting: UnderPosters) -> Self {
        crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.set(Some(setting)));
        Self
    }
}
impl Drop for Setting {
    fn drop(&mut self) { crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.set(None)); }
}
fn off() -> UnderPosters { UnderPosters { sort_value: false, ..UnderPosters::DEFAULT } }

/// A Television section listed as `listed` (Seasons or Episodes), `n` rows sorted by `sort`: each
/// row carries what a `type=3` / `type=4` listing sends.
fn tv_values(listed: LibraryType, n: usize, sort: &str) -> Fixture {
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let kind = if listed == LibraryType::Episodes { KIND_EPISODE } else { KIND_SEASON };
    let rows = (0..n).map(|i| Some(PmsMovie {
        sid, rk: (i + 1).to_string(), kind, title: format!("Episode {i}"), show_title: "Series".into(),
        season_index: 1, ep_index: i as std::os::raw::c_int + 1, aired: "2014-05-12".into(),
        audience_rating: Some(8.4), added_at: Some(1_793_275_200), still: format!("/still/{i}"),
        sort_info: Some(SortInfo { audience_image: "themoviedb://image.rating".into(), last_viewed_at: Some(1_793_188_800),
            view_count: Some(2), ..Default::default() }),
        ..Default::default()
    })).collect();
    let mut fixture = Fixture::new();
    fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid, rows, vec![("A".into(), n as i64)])
        .with_library_type(listed).with_sort_key(sort);
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Show,
            row: plx_data::browse::SrcRow { section: 0, title: "Television".into(), pinned: true, current: true, ..Default::default() } },
    ]);
    fixture
}

fn tv_screen(fixture: &Fixture) -> LibraryScreen {
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Show);
    page.sync(&fixture.cx(None));
    page.initial = false;
    page
}

/// The page with grid card `index` focused and everything settled.
fn focused(fixture: &Fixture, index: usize) -> (LibraryScreen, FocusEngine<u32>) {
    let mut page = tv_screen(fixture);
    let mut engine = FocusEngine::new();
    let key = page.key(page.pair.detail.elem_at(index).unwrap());
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, fixture, ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore });
    settle(&mut page, &mut engine, fixture, 0);
    (page, engine)
}

/// Every text run of one frame of the page.
fn drawn(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>) -> Vec<plx_ui::draw_census::Run> {
    let cx = fixture.cx(engine.current(OWNER));
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    let _walk = plx_ui::frame::backdrop::discover(sources.clone());
    let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
    plx_ui::draw_census::capture_runs(|| page.draw(&mut frame))
}

/// The value mode is decided per listing through the real screen: the Episodes and the Seasons views
/// of a TV library join the Movies and TV Shows grids, a sort the kind of row carries no fact for (and
/// the Show order, Random, a later server's sort) keeps 40 px rows, and the switch's Off restores them.
#[test]
fn seasons_and_episodes_views_space_their_rows_for_a_value_only_when_the_sort_has_one() {
    let _guard = plx_base::testlock::serial();
    let _setting = Setting::set(UnderPosters::DEFAULT);
    let table = [
        (LibraryType::Episodes, "audienceRating", true), (LibraryType::Episodes, "rating", true),
        (LibraryType::Episodes, "originallyAvailableAt", true), (LibraryType::Episodes, "addedAt", true),
        (LibraryType::Episodes, "lastViewedAt", true),
        (LibraryType::Episodes, SHOW_ORDER_EPISODES, false), (LibraryType::Episodes, "random", false),
        (LibraryType::Episodes, "titleSort", false), (LibraryType::Episodes, "futureServerSort", false),
        (LibraryType::Episodes, "unviewedLeafCount", false),
        (LibraryType::Seasons, "addedAt", true), (LibraryType::Seasons, "lastViewedAt", true),
        (LibraryType::Seasons, "unviewedLeafCount", true),
        (LibraryType::Seasons, SHOW_ORDER_SEASONS, false), (LibraryType::Seasons, "random", false),
        (LibraryType::Seasons, "audienceRating", false), (LibraryType::Seasons, "futureServerSort", false),
    ];
    for (listed, sort, shows) in table {
        let page = tv_screen(&tv_values(listed, 36, sort));
        assert_eq!(page.layout.value_line(), shows, "{listed:?} {sort}");
        assert_eq!(page.layout.grid_pitch() - page.layout.card_h(), if shows { 60.0 } else { 40.0 }, "{listed:?} {sort}");
        assert_eq!(page.layout.cols(), if listed == LibraryType::Episodes { 4 } else { 6 }, "{listed:?}: the columns are the view's own");
        assert_eq!(page.layout.is_episodes(), listed == LibraryType::Episodes);
    }
    // The setting Off: every view keeps the old geometry, a sort with a value included.
    let _off = Setting::set(off());
    for listed in [LibraryType::Episodes, LibraryType::Seasons] {
        let page = tv_screen(&tv_values(listed, 36, "addedAt"));
        assert!(!page.layout.value_line(), "{listed:?}");
        assert_eq!(page.layout.grid_pitch() - page.layout.card_h(), 40.0, "{listed:?}");
    }
}

/// The focused row opens 26 px past its 60 px rest while values show, whatever the Title switch says (a
/// value replaces the title and the details line); with the switch Off the Episodes view opens its legacy
/// 114 px band (74 past the rest) and the Seasons view the Under posters policy's band.
#[test]
fn the_focused_row_opens_for_the_value_and_the_setting_off_restores_the_old_band() {
    let _guard = plx_base::testlock::serial();
    let opened = |listed: LibraryType, setting: UnderPosters| {
        let _setting = Setting::set(setting);
        let fixture = tv_values(listed, 60, "addedAt");
        let cols = tv_screen(&fixture).layout.cols();
        let rest = tv_screen(&fixture).layout.row_y(4, 0.0);
        let (page, _) = focused(&fixture, 3 * cols);
        page.layout.row_y(4, 0.0) - rest
    };
    let title_on = UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: true };
    for listed in [LibraryType::Episodes, LibraryType::Seasons] {
        assert!((opened(listed, UnderPosters::DEFAULT) - 26.0).abs() < 0.01, "{listed:?} opens 26 px under a value");
        assert!((opened(listed, title_on) - 26.0).abs() < 0.01, "{listed:?}: the Title switch does not widen a value's band");
    }
    assert!((opened(LibraryType::Episodes, off()) - 74.0).abs() < 0.01, "Off: the Episodes view's legacy 114 px band");
    assert!((opened(LibraryType::Seasons, off()) - 40.0).abs() < 0.01, "Off: the Seasons view's one-line policy band (80 px)");
}

/// What a card says: with a value the focused episode's caption IS the value (bold, once, under the
/// lifted card) and the card carries no title or air-date line besides it; every other visible card
/// shows its own value regular; with the switch Off the old two-line caption (title, air date) is back.
/// A sort that IS the air date draws the date once: the value replaces the date caption, it does not
/// repeat it.
#[test]
fn the_focused_episode_shows_its_value_and_not_the_air_date_twice() {
    let _guard = plx_base::testlock::serial();
    let _setting = Setting::set(UnderPosters::DEFAULT);
    let fixture = tv_values(LibraryType::Episodes, 36, "audienceRating");
    let (mut page, engine) = focused(&fixture, 5);
    let runs = drawn(&fixture, &mut page, &engine);
    let values: Vec<_> = runs.iter().filter(|run| run.text == SCORE && run.ink[3] > 0.5).collect();
    assert!(values.len() >= 4, "a value under every visible episode: {}", values.len());
    assert!(values.iter().all(|run| run.size == plx_ui::theme::size::CAPTION), "CAPTION 24 whatever the card");
    assert_eq!(values.iter().filter(|run| run.bold).count(), 1, "exactly the focused card's value is bold");
    assert!(runs.iter().all(|run| run.text != "Episode 5"), "the episode's title is not drawn under the card");
    // Under the lifted card: the one value and nothing else.
    let cx = fixture.cx(engine.current(OWNER));
    let card = page.pair.detail.rect_at(&cx, 5);
    let under = |runs: &[plx_ui::draw_census::Run]| {
        let mut texts: Vec<String> = runs.iter().filter(|run| run.ink[3] > 0.01 && run.at.cx() > card.x && run.at.cx() < card.x + card.w
            && run.at.y >= card.y + card.h && run.at.y < card.y + card.h + 40.0).map(|run| run.text.clone()).collect();
        texts.sort();
        texts.dedup();
        texts
    };
    assert_eq!(under(&runs), [SCORE.to_owned()], "the focused episode's caption is its value");
    // Sorted by the air date, the date is the value and is drawn once, never a caption on top of it.
    let by_date = tv_values(LibraryType::Episodes, 36, "originallyAvailableAt");
    let (mut page, engine) = focused(&by_date, 5);
    let runs = drawn(&by_date, &mut page, &engine);
    assert_eq!(under(&runs), ["May\u{a0}2014".to_owned()], "one date line under the focused episode");
    // The switch Off: the old caption (title, then the air date line) and no value line anywhere.
    let _off = Setting::set(off());
    let (mut page, engine) = focused(&fixture, 5);
    let runs = drawn(&fixture, &mut page, &engine);
    assert!(runs.iter().all(|run| run.text != SCORE), "no value line with the switch Off");
    assert!(runs.iter().any(|run| run.text == "Episode 5"), "the legacy caption's title is back");
}

/// Every value an episode or a season can say fits its card at CAPTION 24 bold in every shipped
/// language, the mark's hang included, so the fit ladder (drop +1, then the mark) is never reached and
/// nothing is cut with an ellipsis. The Episodes view's cards are wider than a poster; the Seasons
/// view's are posters.
#[test]
fn every_episode_and_season_value_fits_its_card_in_every_shipped_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_machine::machine::Measure;
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
    let _guard = plx_base::testlock::serial();
    let episode_w = layout::Layout::new(true, 10).with_episodes(true).card_w();
    let season_w = layout::Layout::new(true, 10).card_w();
    assert!(episode_w > season_w, "an episode's card is wider than a poster ({episode_w} vs {season_w}): no new truncation risk");
    let keys = ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating", "duration", "viewOffset",
        "viewCount", "addedAt", "lastViewedAt", "mediaHeight", "mediaBitrate", "year", "unviewedLeafCount"];
    let mut too_wide = Vec::new();
    for preference in SHIPPED {
        let _locale = language_on_this_thread_for_test(preference);
        for (kind, card_w) in [(KIND_EPISODE, episode_w), (KIND_SEASON, season_w)] {
            // The widest facts either kind can carry: a three-digit count, ten hours, IMDb's mark and wording.
            let mut item = PmsMovie { kind, rating: "NC-17".into(), dur_ns: (10 * 60 + 59) * 60_000_000_000, year: 2014,
                aired: "2014-05-28".into(), video_resolution: Some("1080".into()), audience_rating: Some(10.0),
                added_at: Some(1_793_275_200), leaf_count: Some(999), viewed_leaf_count: Some(0), unwatched: true,
                sort_info: Some(SortInfo { rating: Some(10.0), rating_image: "imdb://image.rating".into(),
                    audience_image: "imdb://image.rating".into(), user_rating: Some(10.0), view_count: Some(999),
                    last_viewed_at: Some(1_793_188_800), bitrate_kbps: Some(99_900), media_count: 2, ..Default::default() }),
                ..Default::default() };
            let mut values = Vec::new();
            for count in [1u32, 2, 5, 21, 22, 999] {
                item.leaf_count = Some(count);
                item.sort_info.as_mut().unwrap().view_count = Some(i64::from(count));
                for key in keys {
                    if let Some(value) = crate::registry::sort_info::label(&item, key, Some([2026, 10, 30])).value { values.push(value); }
                }
            }
            // An item nobody played, and a date on the last day the calendar words ("Yesterday").
            item.sort_info.as_mut().unwrap().view_count = Some(0);
            item.sort_info.as_mut().unwrap().last_viewed_at = None;
            for key in ["viewCount", "lastViewedAt"] {
                values.extend(crate::registry::sort_info::label(&item, key, Some([2026, 10, 30])).value);
            }
            assert!(values.len() >= 6, "{kind}: the rows say something");
            for value in values {
                let full = if value.plus { format!("{}{}", value.text, plx_ui::cards::SORT_VALUE_PLUS) } else { value.text.clone() };
                let width = ShippedMeasure.width_str(&full, plx_ui::theme::size::CAPTION, true);
                // The mark hangs left of the number (20 px + 6 px, twice for the centred number).
                let hang = if value.mark.is_empty() { 0.0 } else { 52.0 };
                if width + hang > card_w { too_wide.push(format!("{preference:?} kind {kind}: {full:?} is {width}px + {hang}px in a {card_w}px card")); }
            }
        }
    }
    assert!(too_wide.is_empty(), "{too_wide:#?}");
}
