//! The Watchlist page's tests: every shipped sentence fits its slot in every shipped language, the
//! caption and mark vocabulary, what a press and a hold ask for, the removal landing, the read-out
//! selection, the ground's fallback chain, key stability and the Tier 2 card conformance run.
use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::watchlist::model::TileState;
use plx_machine::machine::Measure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference};
use plx_ui::fit::HEADROOM;
use plx_ui::theme;

use super::cards_harness::{copy, mount, tile, tile_in, tiles, Harness};
use super::caption;
use super::options::WatchlistOptionsState;
use super::page::{ListState, MatchedFacts, WatchlistIn, WatchlistOut, WatchlistScreen, PILL_ELEM};
use super::readout::Readout;
use super::tiles as marks;

const LANGUAGES: [Preference; 4] = [Preference::En, Preference::Es, Preference::Be, Preference::Fr];

const UNPLAYABLE: [TileState; 5] = [
    TileState::ComingSoon,
    TileState::ComingSoonMatched,
    TileState::NotOnAnyServer,
    TileState::ServerOffline,
    TileState::Unknown,
];

fn screen(n: usize) -> WatchlistScreen {
    WatchlistScreen::new(tiles(n), WatchlistOptionsState::default())
}

/// **Every state sentence fits the focused caption, in every language, on the longest date.** The
/// focused tile's label may widen to two tile widths and the gap between them; a sentence wider
/// than that ends in an ellipsis. Every month is graded on day 28, measured with the device's
/// own advances at CAPTION.
#[test]
fn every_state_sentence_fits_the_focus_caption_in_every_language() {
    let budget = (2.0 * plx_ui::cards::GRID_STYLE.w + 2.0 * plx_ui::cards::GRID_STYLE.gap) * HEADROOM;
    let mut over = Vec::new();
    for language in LANGUAGES {
        let _guard = language_on_this_thread_for_test(language);
        for month in 1..=12 {
            let release = format!("2027-{month:02}-28");
            for state in UNPLAYABLE {
                let Some(text) = caption::state_sentence(state, Some(&release)) else { continue };
                let w = ShippedMeasure.width_str(&text, theme::size::CAPTION, false);
                if w > budget {
                    over.push(format!("{} {state:?} {release}: {w}px of {budget}px: {text:?}", language.tag()));
                }
            }
        }
    }
    assert!(over.is_empty(), "sentences the caption would ellipsize:\n  {}", over.join("\n  "));
}

/// **The verdicts, the page title and the pill text fit their slots in every language.**
#[test]
fn the_verdicts_title_and_pill_fit_in_every_language() {
    let column = plx_ui::consts::SAFE.w * HEADROOM;
    let mut over = Vec::new();
    for language in LANGUAGES {
        let _guard = language_on_this_thread_for_test(language);
        let mut lines: Vec<(&str, String, i32)> = vec![
            ("title", msg::watchlist_page_title().into(), theme::size::DISPLAY),
            ("pill", msg::watchlist_page_sort_filter_summary("Movies & Shows, Release Date"), theme::size::LABEL),
        ];
        for r in [Readout::Empty, Readout::Filtered, Readout::NoToken] {
            lines.push(("verdict", r.verdict().into(), theme::size::HEADLINE));
        }
        for (what, text, size) in lines {
            let w = ShippedMeasure.width_str(&text, size, false);
            if w > column {
                over.push(format!("{} {what}: {w}px of {column}px: {text:?}", language.tag()));
            }
        }
    }
    assert!(over.is_empty(), "lines wider than the safe area:\n  {}", over.join("\n  "));
}

/// **A caption is the state's sentence, else the year, else nothing.**
#[test]
fn captions_carry_the_state_sentence_with_the_month_date() {
    let _guard = language_on_this_thread_for_test(Preference::En);
    let mut row = super::cards_harness::row(1);
    row.release = Some("2027-03-12".into());
    assert_eq!(caption::caption(&row, TileState::ComingSoon), "Releasing 12 Mar");
    // Q6: coming soon AND matched keeps the clock, so it keeps the date.
    assert_eq!(caption::caption(&row, TileState::ComingSoonMatched), "Releasing 12 Mar");
    assert_eq!(caption::caption(&row, TileState::NotOnAnyServer), "On no server");
    assert_eq!(caption::caption(&row, TileState::ServerOffline), "Server offline");
    assert_eq!(caption::caption(&row, TileState::Available), "2001");
    assert_eq!(caption::caption(&row, TileState::Unknown), "2001");
    row.release = None;
    assert_eq!(caption::caption(&row, TileState::ComingSoon), "Coming soon");
    row.release = Some("soon".into());
    assert_eq!(caption::caption(&row, TileState::ComingSoon), "Coming soon");
    row.year = None;
    assert_eq!(caption::caption(&row, TileState::Available), "");
}

/// **One mark per tile, and the no-server wash is quieter under focus** (TileStates board).
#[test]
fn marks_and_washes_follow_the_state() {
    use plx_ui::icons::Icon;
    assert_eq!(marks::mark(TileState::ComingSoon), Some(Icon::Clock));
    assert_eq!(marks::mark(TileState::ComingSoonMatched), Some(Icon::Clock));
    assert_eq!(marks::mark(TileState::ServerOffline), Some(Icon::ServerSlash));
    assert_eq!(marks::mark(TileState::Available), None);
    assert_eq!(marks::mark(TileState::NotOnAnyServer), None);
    assert!(marks::wash(TileState::NotOnAnyServer, false) > marks::wash(TileState::NotOnAnyServer, true));
    assert_eq!(marks::wash(TileState::Available, false), 0.0);
}

/// **OK opens the matched copy when there is a playable one, else the catalog page.** Q6 default:
/// coming soon plus matched keeps the clock and stays playable.
#[test]
fn a_press_opens_the_copy_or_the_catalog() {
    let rows = vec![
        tile_in(0, TileState::Available),
        tile_in(1, TileState::ComingSoonMatched),
        tile_in(2, TileState::ComingSoon),
        tile_in(3, TileState::NotOnAnyServer),
        tile_in(4, TileState::ServerOffline),
        tile_in(5, TileState::Unknown),
    ];
    let s = WatchlistScreen::new(rows, WatchlistOptionsState::default());
    assert_eq!(s.press_at(0), Some(WatchlistOut::OpenDetail(copy(0))));
    assert_eq!(s.press_at(1), Some(WatchlistOut::OpenDetail(copy(1))));
    for i in 2..6 {
        assert_eq!(s.press_at(i), Some(WatchlistOut::OpenCatalog(format!("plex://movie/g{i}"))), "{i}");
    }
    assert_eq!(s.press_at(6), None);
    match s.hold_at(3) {
        Some(WatchlistOut::Hold { index: 3, guid, state: TileState::NotOnAnyServer, copy: None }) => {
            assert_eq!(guid, "plex://movie/g3")
        }
        other => panic!("{other:?}"),
    }
}

/// The Watchlist requests a step queued, oldest first.
fn requests(fx: Vec<plx_machine::machine::Stamped<super::cards_harness::WatchHost>>) -> Vec<WatchlistOut> {
    use plx_machine::machine::Fx;
    fx.into_iter()
        .filter_map(|e| match e.fx {
            Fx::App(crate::registry::AppFx::Watchlist(out)) => Some(out),
            _ => None,
        })
        .collect()
}

/// **The pill asks for the Sort & Filter menu as a Watchlist request.**
#[test]
fn the_pill_opens_the_options() {
    let mut h = Harness::new(tiles(3));
    let (fx, _) = h.step_fx(plx_ui::screen::ScreenEvent::Activate(PILL_ELEM));
    assert_eq!(requests(fx), vec![WatchlistOut::OpenOptions]);
}

/// **The strip's other tabs and the chip are requests; the Watchlist mark asks for nothing.**
#[test]
fn a_strip_press_is_a_tab_request() {
    use crate::registry::HomeTab;
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_SEARCH, TAB_KEY_WATCHLIST};
    let mut h = Harness::new(tiles(3));
    for (key, want) in [
        (TAB_KEY_HOME, vec![WatchlistOut::Tab(HomeTab::Home)]),
        (TAB_KEY_SEARCH, vec![WatchlistOut::Tab(HomeTab::Search)]),
        (TAB_KEY_CHIP, vec![WatchlistOut::Account]),
        (TAB_KEY_WATCHLIST, vec![]),
    ] {
        let (fx, _) = h.step_fx(plx_ui::screen::ScreenEvent::Activate(key));
        assert_eq!(requests(fx), want, "{key:#x}");
    }
}

/// **BACK leaves the page as a request** (the application takes it to Home), not a content pop.
#[test]
fn back_is_a_request() {
    use plx_machine::machine::{Edge, InputEvent, InputKind, Key, Source, Tick};
    let mut h = Harness::new(tiles(3));
    let (fx, _) = h.step_fx(plx_ui::screen::ScreenEvent::Input(InputEvent {
        at: Tick { ms: 0, dt_us: 0 },
        source: Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    }));
    assert_eq!(requests(fx), vec![WatchlistOut::Back]);
}

/// **The read-out follows the list's state, never the tile count alone.**
#[test]
fn the_readout_follows_the_list() {
    let mut s = screen(2);
    assert_eq!(s.readout(), None);
    assert_eq!(s.count(), Some(2));
    s.input(WatchlistIn::Rows(vec![]));
    assert_eq!(s.readout(), Some(Readout::Filtered), "narrowed to nothing, 2 on the list");
    s.input(WatchlistIn::List(ListState::Ready { total: 0 }));
    assert_eq!(s.readout(), Some(Readout::Empty));
    s.input(WatchlistIn::List(ListState::Loading));
    assert_eq!((s.readout(), s.count()), (None, None));
    s.input(WatchlistIn::List(ListState::Unavailable));
    assert_eq!(s.readout(), Some(Readout::NoToken));
}

/// **A tile keeps its element while it stays on the list, and a removed key is not reused.**
#[test]
fn elements_are_stable_across_reorders_and_never_reused() {
    let mut s = screen(4);
    let before: Vec<u32> = (0..4).map(|i| s.elem_at(i).unwrap()).collect();
    let mut rows = s.tiles().to_vec();
    rows.reverse();
    s.input(WatchlistIn::Rows(rows));
    let after: Vec<u32> = (0..4).map(|i| s.elem_at(i).unwrap()).collect();
    assert_eq!(after, before.iter().rev().copied().collect::<Vec<_>>());
    let gone = "plex://movie/g0".to_string();
    s.input(WatchlistIn::Removed(gone));
    assert_eq!(s.tiles().len(), 3);
    assert_eq!(s.list(), ListState::Ready { total: 3 });
    let mut rows = s.tiles().to_vec();
    rows.push(tile(0));
    s.input(WatchlistIn::Rows(rows));
    let again = s.elem_at(3).unwrap();
    assert!(!before.contains(&again), "a returning guid gets a fresh key");
}

/// **The ground follows the focused title's colours, else the flat app surface.**
#[test]
fn the_ground_falls_back_from_blur_to_flat() {
    let flat = [theme::SURFACE_APP; 4];
    let mut s = screen(2);
    s.set_focus_index(0);
    assert_eq!(s.ground_goal(), flat, "no matched colours");
    let blur = [[0.9, 0.1, 0.1], [0.1, 0.9, 0.1], [0.1, 0.1, 0.9], [0.5, 0.5, 0.5]];
    s.input(WatchlistIn::Facts(
        "plex://movie/g0".into(),
        MatchedFacts { blur: Some(blur), ..Default::default() },
    ));
    assert_ne!(s.ground_goal(), flat, "the focused title's colours");
    s.set_focus_index(1);
    assert_eq!(s.ground_goal(), flat, "the other title has none");
}

/// **A removal lands: the survivors stay, and focus is on a title that is still on the list.**
#[test]
fn removing_the_focused_title_leaves_focus_on_a_neighbour() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    let mut h = Harness::new(tiles(6));
    h.focus_index(2);
    let removed = h.focused().unwrap();
    h.landing(Landing::RemoveFocused).unwrap();
    assert_eq!(h.screen.tiles().len(), 5);
    let now = h.focused().expect("focus survives");
    assert_ne!(now, removed);
    assert!(h.screen.index_of(now).is_some());
}

/// **The Tier 2 card conformance cases pass for the Watchlist grid** (the screen's harness is not
/// yet in `cards_conformance_tests.rs`'s table; this runs the same cases on it). Memory is the one
/// case the page cannot take: `PageMemory` has no Watchlist arm.
#[test]
fn the_card_conformance_cases_pass() {
    use plx_ui::cards::conformance::{run_all, Mount, Outcome};
    let matrix = run_all(&[("watchlist", mount as Mount)]);
    let bad: Vec<String> = matrix
        .iter()
        .filter_map(|(s, c, o)| match o {
            Outcome::Fail(m) => Some(format!("{s} {c}: {m}")),
            _ => None,
        })
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
