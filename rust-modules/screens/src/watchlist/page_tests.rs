//! The Watchlist page's tests: every shipped sentence fits its slot in every shipped language, the
//! caption and mark vocabulary, what a press and a hold ask for, the removal landing, the read-out
//! selection, the ground's fallback chain, key stability and the Tier 2 card conformance run.
use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::watchlist::model::TileState;
use plx_machine::machine::Measure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference};
use plx_ui::fit::HEADROOM;
use plx_ui::theme;

use super::cards_harness::{copy, tile, tile_in, tiles, Harness};
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
        // The verdicts are measured in the face `draw_readout` paints them in (`verdict_face`: bold
        // TITLE), not in a stand-in.
        let (verdict_sz, verdict_bold, _) = plx_ui::widgets::StatusOverlay::verdict_face(plx_ui::widgets::StatusKind::Failed);
        let mut lines: Vec<(&str, String, i32, bool)> = vec![
            ("title", msg::watchlist_page_title().into(), theme::size::DISPLAY, false),
            ("pill", msg::watchlist_page_sort_filter_summary("Movies & Shows, Release Date"), theme::size::LABEL, false),
        ];
        for r in [Readout::Empty, Readout::Filtered, Readout::NoToken, Readout::Unreachable] {
            lines.push(("verdict", r.verdict().into(), verdict_sz, verdict_bold));
        }
        for (what, text, size, bold) in lines {
            let w = ShippedMeasure.width_str(&text, size, bold);
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

/// **The pill's focus ring, hit target and menu anchor are the pill the header draws, and it ends
/// on the grid's right edge** (not on the screen edge, not 150 px left of it).
#[test]
fn the_pill_is_one_rect_for_paint_focus_and_anchor() {
    use super::header;
    use plx_ui::screen::{At, Focusable};
    let h = Harness::new(tiles(3));
    let cx = h.cx();
    let focus = Focusable::<super::cards_harness::WatchHost>::place(&h.screen, &PILL_ELEM, &cx, At::Drawn)
        .expect("the pill is a focus stop")
        .rect;
    let style = plx_ui::cards::GRID_STYLE;
    let grid_right = plx_ui::consts::MARGIN_X + 6.0 * style.w + 5.0 * style.gap;
    let drawn = header::pill_rect(header::header_frame(grid_right), &plx_ui::fixture::FixtureMeasure, &h.screen.options().summary());
    assert_eq!(focus, drawn, "the focus rect is the drawn pill");
    assert_eq!(h.screen.pill_rect(&plx_ui::fixture::FixtureMeasure), drawn, "so is the menu's anchor");
    assert_eq!(focus.x + focus.w, grid_right, "right-aligned to the grid, not the screen");
    assert!(focus.x + focus.w < plx_ui::consts::SCR_W);
}

/// **A removed tile is kept as a ghost for the landing to fade (DS4)**: the grid names it and the
/// page can paint it.
#[test]
fn a_removed_tile_is_a_ghost_the_page_can_paint() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    let mut h = Harness::new(tiles(12));
    h.focus_index(2);
    assert!(h.screen.ghost_probe().0.is_empty());
    h.landing(Landing::RemoveFocused).unwrap();
    let (named, kept) = h.screen.ghost_probe();
    assert!(!named.is_empty(), "the grid is fading a card out");
    for key in &named {
        assert!(kept.contains(key), "the page kept the card the grid names ({key})");
    }
    h.run(600);
    let (named, kept) = h.screen.ghost_probe();
    assert!(named.is_empty() && kept.is_empty(), "the ghosts are let go once nothing fades");
}

/// **The page draws the ghost it kept**: drawing the screen a few frames into the landing asks for
/// the removed tile's poster, so a `StackView::paint` that skips `paint_ghosts` (or a page that
/// paints none) fails here.
#[test]
fn a_removed_tile_is_drawn_while_it_fades() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    let mut h = Harness::new(tiles(12));
    h.focus_index(2);
    h.landing(Landing::RemoveFocused).unwrap();
    h.run(3);
    assert!(!h.screen.ghost_probe().0.is_empty(), "still fading");
    let before = h.screen.ghost_draws();
    h.draw();
    assert!(h.screen.ghost_draws() > before, "the removed tile's poster was drawn");
}

/// **Cells past the loaded tail are reserved** (plan §1.3): the grid is as long as the list, and
/// a cell past the rows is a skeleton that opens nothing.
#[test]
fn cells_past_the_loaded_tail_are_reserved_skeletons() {
    use plx_ui::cards::CardSource;
    type H = super::cards_harness::WatchHost;
    let mut s = screen(100);
    s.input(WatchlistIn::List(ListState::Ready { total: 250 }));
    let src = s.cards_for_test();
    assert_eq!(CardSource::<H>::len(&src), 250);
    let loaded = |i| CardSource::<H>::loaded(&src, i);
    assert!(loaded(99) && loaded(100) && loaded(249), "a reserved cell is painted, as a skeleton");
    let unread = |i| CardSource::<H>::unread(&src, i);
    assert!(!unread(150), "the list worker reads every page itself: the grid has nothing to ask");
    let elem = |i| CardSource::<H>::elem(&src, i);
    assert_eq!(CardSource::<H>::index_of(&src, &elem(150)), Some(150));
    assert_eq!(s.press_at(150), None, "a reserved cell opens nothing");
    assert_eq!(s.hold_at(150), None);
}

/// **Focus can enter the reserved cells, sees a skeleton there, and keeps its cell when the row
/// lands**: DOWN from the last loaded row lands on a drawn skeleton, a reserved cell's key names
/// one absolute position (so a landing page never hands it to another cell), and when that
/// position's row lands, focus moves to the title now in the cell it was on.
#[test]
fn a_reserved_cell_keeps_its_place_when_its_row_lands() {
    use plx_ui::cards::conformance::{CardHarness, Nb};
    use plx_ui::cards::CardSource;
    use plx_ui::screen::{By, Dir};
    type H = super::cards_harness::WatchHost;
    let mut h = Harness::new(tiles(100));
    h.screen.input(WatchlistIn::List(ListState::Ready { total: 250 }));
    h.run(1);
    h.focus_index(99);
    let Nb::To(below) = h.neighbour(h.screen.elem_at(99).unwrap(), Dir::Down) else {
        panic!("DOWN from the last loaded row reaches the reserved cells")
    };
    let at = CardSource::<H>::index_of(&h.screen.cards_for_test(), &below);
    assert_eq!(at, Some(105), "DOWN goes one row down, into the reserved cells");
    h.focus_elem(below, By::Dir);
    h.run(60);
    h.screen.take_skeleton_draws();
    h.draw();
    assert!(h.screen.take_skeleton_draws().contains(&105), "the focused reserved cell is drawn as a skeleton");

    // A cell further down, scrolled to: its skeleton is drawn too.
    let far = CardSource::<H>::elem(&h.screen.cards_for_test(), 150);
    h.focus_elem(far, By::Dir);
    h.run(120);
    h.draw();
    assert!(h.screen.take_skeleton_draws().contains(&150), "a scrolled-to reserved cell is a skeleton");

    // The next page lands: 150 is now a title, and focus moves to it in the same cell.
    h.screen.input(WatchlistIn::Rows(tiles(200)));
    h.run(1);
    h.reconcile();
    let focused = h.focus.map(|k| k.elem).unwrap();
    assert_eq!(h.screen.index_of(focused), Some(150), "focus stays on the cell it was on");
    assert_eq!(h.screen.focused(), Some(150));
    assert_eq!(h.screen.tiles()[150].row.title, "150");
    // and the reserved keys past the new tail still name their own absolute positions
    let src = h.screen.cards_for_test();
    assert_eq!(CardSource::<H>::index_of(&src, &CardSource::<H>::elem(&src, 220)), Some(220));
    assert_eq!(CardSource::<H>::index_of(&src, &far), None, "the landed cell's reserved key is retired");
}

/// **Discover art needs a reachable server's photo proxy** (Q9): an unmatched title asks for its
/// poster through one, and draws the skeleton when none is reachable.
#[test]
fn an_unmatched_title_gets_discover_art_only_through_a_reachable_server() {
    let sid = plx_plex::plex::ServerId::from_raw(3);
    let mut unmatched = tile_in(0, TileState::ComingSoon);
    unmatched.row.thumb = "https://image.example/poster.jpg".into();
    let mut s = WatchlistScreen::new(vec![unmatched], WatchlistOptionsState::default());
    assert_eq!(s.art_of(0), Some((String::new(), 0)), "no server reachable: the skeleton");
    s.input(WatchlistIn::ArtServer(Some(sid)));
    assert_eq!(s.art_of(0), Some(("https://image.example/poster.jpg".to_string(), 3)));
    s.input(WatchlistIn::ArtServer(None));
    assert_eq!(s.art_of(0), Some((String::new(), 0)), "the server went: the skeleton again");
}

/// **A matched copy's UltraBlur colours key the ground** with no hand-fed facts.
#[test]
fn a_matched_copys_colours_key_the_ground() {
    let blur = [[200, 20, 20], [20, 200, 20], [20, 20, 200], [128, 128, 128]];
    let mut row = tile(0);
    row.copy.as_mut().unwrap().blur = Some(blur);
    let mut s = WatchlistScreen::new(vec![row, tile(1)], WatchlistOptionsState::default());
    s.set_focus_index(0);
    assert_ne!(s.ground_goal(), [theme::SURFACE_APP; 4]);
    s.set_focus_index(1);
    assert_eq!(s.ground_goal(), [theme::SURFACE_APP; 4]);
}

/// **A failed first read is a read-out with Try again**, not a blank page; Try again asks the
/// application to read the list again.
#[test]
fn a_failed_first_read_offers_try_again() {
    let mut h = Harness::new(vec![]);
    h.screen.input(WatchlistIn::List(ListState::Failed));
    assert_eq!(h.screen.readout(), Some(Readout::Unreachable));
    assert!(Readout::Unreachable.retries());
    let (fx, _) = h.step_fx(plx_ui::screen::ScreenEvent::Activate(super::page::RETRY_ELEM));
    assert_eq!(requests(fx), vec![WatchlistOut::Retry]);
}

/// **The count is the list's length while pages are still arriving**, and the shown titles under
/// a narrowing filter.
#[test]
fn the_count_reads_the_whole_list_while_the_tail_loads() {
    let mut s = screen(100);
    s.input(WatchlistIn::List(ListState::Ready { total: 250 }));
    assert_eq!(s.count(), Some(250), "100 loaded of 250");
    s.input(WatchlistIn::Options(WatchlistOptionsState {
        type_: super::options::TypeFilter::Movies,
        ..Default::default()
    }));
    assert_eq!(s.count(), Some(100), "a narrowed list counts what is shown");
}

/// **`SHAPE` lists the fields in the order `write` emits them**, and the options in the canonical
/// form the menu uses (the sort a number, never a `Debug` string).
#[test]
fn the_shape_follows_the_write_order() {
    assert!(WatchlistScreen::SHAPE.starts_with("WatchlistScreen{entry:u32,next_elem:u32,list:{tag:u32,total:u64},options:{type:u32,sort:u32,"));
    assert!(!WatchlistScreen::SHAPE.contains("sort:str"));
}
