//! The Watchlist page's tests: every shipped sentence fits its slot in every shipped language, the
//! caption and mark vocabulary, what a press and a hold ask for, the removal landing, the read-out
//! selection, the ground's fallback chain, key stability and the Tier 2 card conformance run.
use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::watchlist::model::TileState;
use plx_machine::machine::Measure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference, SHIPPED};
use plx_ui::fit::HEADROOM;
use plx_ui::theme;

use super::cards_harness::{copy, tile, tile_in, tiles, Harness};
use super::caption;
use super::header::Count;
use super::options::WatchlistOptionsState;
use super::page::{ListState, MatchedFacts, WatchlistIn, WatchlistOut, WatchlistScreen, PILL_ELEM};
use super::readout::Readout;
use super::tiles as marks;

const LANGUAGES: [Preference; SHIPPED.len()] = SHIPPED;

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
    assert_eq!(caption::caption(&row, TileState::NotOnAnyServer), "Not on your servers");
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
    assert_eq!(marks::mark(TileState::NotOnAnyServer), Some(Icon::CircleSlash));
    assert!(marks::owns_corner(TileState::NotOnAnyServer));
    assert_eq!(marks::wash(TileState::NotOnAnyServer, false), 0.55);
    assert_eq!(marks::wash(TileState::NotOnAnyServer, true), 0.25);
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

/// **OK on the strip is a request too.** The strip is a `Control` group, so the remote's OK arms a
/// press and the page hears `PressCommit` with the strip key focused — never `Activate`. A page that
/// answered only a card's commit swallowed every strip press, and the Watchlist could only be left
/// with BACK (owner, on the TV: "I can't leave Watchlist").
#[test]
fn a_committed_press_on_the_strip_is_a_tab_request() {
    use crate::registry::HomeTab;
    use plx_machine::machine::PressId;
    use plx_ui::screen::By;
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST};
    let mut h = Harness::new(tiles(3));
    for (key, want) in [
        (TAB_KEY_HOME, vec![WatchlistOut::Tab(HomeTab::Home)]),
        (TAB_KEY_MOVIES, vec![WatchlistOut::Tab(HomeTab::Movies)]),
        (TAB_KEY_SHOWS, vec![WatchlistOut::Tab(HomeTab::Shows)]),
        (TAB_KEY_SEARCH, vec![WatchlistOut::Tab(HomeTab::Search)]),
        (TAB_KEY_CHIP, vec![WatchlistOut::Account]),
        (TAB_KEY_WATCHLIST, vec![]),
    ] {
        h.focus_elem(key, By::Dir);
        let (fx, _) = h.step_fx(plx_ui::screen::ScreenEvent::PressCommit(PressId(1)));
        assert_eq!(requests(fx), want, "{key:#x}");
    }
    // A tile's commit is still the tile's, once: the card path, not the focus fallback as well.
    h.focus_index(0);
    let (fx, _) = h.step_fx(plx_ui::screen::ScreenEvent::PressCommit(PressId(2)));
    let pushes = fx.iter().filter(|e| matches!(&e.fx,
        plx_machine::machine::Fx::App(crate::registry::AppFx::Content(crate::registry::ContentReq::Push(_))))).count();
    assert_eq!((pushes, requests(fx)), (1, vec![]), "the matched tile opens its Detail once, and nothing else");
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
    assert_eq!(s.count(), Some(Count::All(2)));
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

/// **While Sort & Filter is up the pill is the menu's, not the page's** (DS7: "the pill's glass
/// becomes the panel"). The menu names the pill as its opener, so the dispatcher lifts it out of
/// the page pass and the menu's own copy (`OptionsMenu::draw_face`, the morph's opener face) is the
/// only pill drawn. Red when the page kept drawing its pill under the scrim: the
/// settled menu, slid up over the pill because it is taller than the room below it, refracted
/// that pill's label into its glass rim (`GlassRim::Menu`'s sharp band) — a ghost "Sort & Filter
/// · Default" along the panel's top edge.
#[test]
fn the_open_menu_lifts_the_pill_out_of_the_page() {
    use super::cards_harness::{WatchHost, ENTRY};
    use super::menu::WatchlistMenuScreen;
    use plx_machine::machine::{EntryId, FocusKey};
    use plx_ui::screen::Screen;
    let mut h = Harness::new(tiles(3));
    let pill = h.screen.pill_rect(&plx_ui::fixture::FixtureMeasure);
    let arg = crate::registry::WatchlistMenuArg {
        host: ENTRY,
        options: WatchlistOptionsState::default(),
        anchor: [pill.x, pill.y, pill.w, pill.h].map(f32::to_bits),
    };
    let menu = WatchlistMenuScreen::new(EntryId(ENTRY.0 + 1), arg);
    let key = FocusKey { entry: ENTRY, elem: PILL_ELEM };
    assert_eq!(Screen::<WatchHost>::opener(&menu), Some((ENTRY, Some(key))), "the menu's opener is the page's pill");
    let label_in_pill = |log: &[(u64, plx_ui::Rect)]| {
        log.iter().filter(|(tag, r)| *tag == 100 && r.cx() > pill.x && r.cx() < pill.x + pill.w && r.cy() > pill.y && r.cy() < pill.y + pill.h).count()
    };
    assert!(label_in_pill(&h.draw_census(None)) > 0, "premise: the page draws the pill's label");
    let lifted = h.draw_census(Some(key));
    assert_eq!(label_in_pill(&lifted), 0, "a lifted pill is not drawn by the page");
    assert!(lifted.iter().any(|(tag, r)| *tag == 100 && r.x < pill.x), "the title still is");
}

/// **The Sort & Filter surface begins and ends as the page's own pill, and is never nothing** (spec
/// (a), (b), (d); frame dump at 572f46d07). Driven through the surface itself, as the dispatcher
/// drives it: Mount, the held face (`Screen::draw_held`), the open's frames, BACK, and the close
/// run out and past. Graded on the paints each frame DECLARES (a discovery walk):
///
/// - the held face and the last close frame are the page's focused pill, primitive for primitive
///   (red when the face was the pill drawn on the container's appear: on the held frames nothing,
///   at the end of the close a pill faded to nothing, then the page's popping in);
/// - every frame between has the opener's plate or the glass (red on the n334–351 empty frames).
#[test]
fn the_menu_surface_begins_and_ends_as_the_pages_pill_and_is_never_nothing() {
    use super::cards_harness::{WatchHost, ENTRY};
    use super::menu::WatchlistMenuScreen;
    use plx_machine::machine::{
        Cx, Edge, Effects, EntryId, FocusKey, FocusRead, InputEvent, InputKind, InputOwner, Key, Machine, PressRead, Tick,
    };
    use plx_ui::screen::{By, DrawFrame, Screen, ScreenEvent};
    use std::{cell::RefCell, rc::Rc};
    let _g = plx_base::testlock::serial();
    let mut h = Harness::new(tiles(3));
    h.focus_elem(PILL_ELEM, By::Dir);
    let pill = h.screen.pill_rect(&plx_ui::fixture::FixtureMeasure);
    let inside = |r: plx_ui::Rect| {
        let (cx, cy) = (r.cx(), r.cy());
        cx > pill.x && cx < pill.x + pill.w && cy > pill.y && cy < pill.y + pill.h
    };
    type Census = Vec<(u64, plx_ui::Rect)>;
    let shape = |log: &Census| -> Vec<(u64, [i32; 4])> {
        log.iter().filter(|(_, r)| inside(*r)).map(|(t, r)| (*t, [r.x, r.y, r.w, r.h].map(|v| v.round() as i32))).collect()
    };
    let page_pill = shape(&h.draw_census(None));
    assert!(page_pill.iter().any(|(t, r)| *t != 100 && r[2] == pill.w.round() as i32), "premise: the page draws its focused pill's plate: {page_pill:?}");

    let arg = crate::registry::WatchlistMenuArg {
        host: ENTRY,
        options: WatchlistOptionsState::default(),
        anchor: [pill.x, pill.y, pill.w, pill.h].map(f32::to_bits),
    };
    let entry = EntryId(ENTRY.0 + 1);
    let mut menu = WatchlistMenuScreen::new(entry, arg);
    let mut ms = 0;
    let step = |menu: &mut WatchlistMenuScreen, ev: ScreenEvent<WatchHost>, ms: u32| {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let cx = Cx {
            views: (),
            tick: Tick { ms, dt_us: 16_667 },
            measure: &plx_ui::fixture::FixtureMeasure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(entry),
        };
        let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(10)), &mut present);
        Machine::<WatchHost>::step(menu, &ev, &cx, &mut fx);
    };
    let draw = |menu: &mut WatchlistMenuScreen, held: bool| -> Census {
        let sources = Rc::new(RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        let cx = Cx {
            views: (),
            tick: Tick { ms: 0, dt_us: 16_667 },
            measure: &plx_ui::fixture::FixtureMeasure,
            press: PressRead::default(),
            focus: FocusRead { current: Some(FocusKey { entry, elem: 0 }), ..Default::default() },
            owner: InputOwner::Entry(entry),
        };
        plx_ui::draw_census::capture(|| {
            let _discovery = plx_ui::frame::backdrop::discover(sources.clone());
            let mut f = DrawFrame::new(&cx, plx_ui::Painter::recording());
            if held {
                Screen::<WatchHost>::draw_held(menu, &mut f);
            } else {
                Screen::<WatchHost>::draw(menu, &mut f);
            }
        })
    };
    // The plate or the glass: a primitive at least the pill's height and twice as wide (the
    // surface narrows toward the panel's width, which may be less than the pill's; text runs are
    // the label's and the rows').
    let something = |log: &Census| log.iter().any(|(t, r)| *t != 100 && r.h >= pill.h - 1.0 && r.w >= 2.0 * pill.h);

    step(&mut menu, ScreenEvent::Mount, ms);
    assert_eq!(shape(&draw(&mut menu, true)), page_pill, "held: the surface is the page's pill");
    assert_eq!(shape(&draw(&mut menu, false)), page_pill, "frame 0: still the page's pill");
    for _ in 0..40 {
        ms += 16;
        step(&mut menu, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }), ms);
        let log = draw(&mut menu, false);
        assert!(something(&log), "an open frame with neither plate nor glass: {log:?}");
    }
    let back = InputEvent {
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        at: Tick::default(),
        source: plx_machine::machine::Source::Sdl,
    };
    step(&mut menu, ScreenEvent::Input(back), ms);
    let mut last = Vec::new();
    for _ in 0..60 {
        ms += 16;
        step(&mut menu, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }), ms);
        last = draw(&mut menu, false);
        assert!(something(&last), "a close frame with neither plate nor glass");
    }
    assert_eq!(shape(&last), page_pill, "the close lands on the page's own pill: nothing to pop in");
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

/// **The menu's lift carries a removed tile's ghost above the dim** (Remove from Watchlist): the page
/// says the opener is departing while its ghost fades, the page pass leaves that ghost out when it
/// is lifted, and the lift's own frame draws it, so it is drawn once and never drops into the dim
/// on the OK frame.
#[test]
fn the_menu_lift_carries_a_removed_tile_ghost_above_the_dim() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    use plx_ui::screen::Screen;
    type H = super::cards_harness::WatchHost;
    let mut h = Harness::new(tiles(12));
    h.focus_index(2);
    let gone = h.focus.expect("focused");
    assert!(!Screen::<H>::opener_departing(&h.screen, gone), "a card still on the page is not departing");
    h.landing(Landing::RemoveFocused).unwrap();
    assert_ne!(h.focus, Some(gone), "premise: focus reconciled off the removed tile");
    assert!(Screen::<H>::opener_departing(&h.screen, gone), "the removed tile is departing while its ghost fades");
    let before = h.screen.ghost_draws();
    h.draw_census(Some(gone));
    assert_eq!(h.screen.ghost_draws(), before, "the page pass leaves the lifted ghost out");
    h.redraw_lift(Some(gone));
    assert!(h.screen.ghost_draws() > before, "the lift draws the ghost");
    h.run(600);
    assert!(!Screen::<H>::opener_departing(&h.screen, gone), "and lets it go once it has faded");
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

/// **The page lays down the app's ground itself, before its own** (a read-out after Sort & Filter
/// closed left the menu's last frames on screen): `PageGround::draw` skips a ground resolved to
/// the app surface because the frame clear already painted that colour, so a page that does not
/// clear shows whatever the back buffer held. Home, the Library, Search and Detail all clear first.
#[test]
fn the_page_clears_the_frame_under_a_flat_ground() {
    let mut h = Harness::new(vec![]);
    h.screen.input(WatchlistIn::List(ListState::Ready { total: 3 }));
    assert_eq!(h.screen.readout(), Some(Readout::Filtered));
    assert_eq!(h.screen.ground_goal(), [theme::SURFACE_APP; 4], "the read-out's ground is flat");
    let sources = h.draw_discovered();
    assert!(!sources.borrow().paints.is_empty(), "the page's draw laid down no frame clear");
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
    assert_eq!(s.count(), Some(Count::All(250)), "100 loaded of 250");
    s.input(WatchlistIn::Options(WatchlistOptionsState {
        type_: super::options::TypeFilter::Movies,
        ..Default::default()
    }));
    assert_eq!(s.count(), Some(Count::Of { shown: 100, total: 250 }), "a narrowed list counts what is shown of the whole");
}

/// **Sort alone does not narrow**, and a filter that leaves nothing still counts "0 of N" beside the
/// pill (board WatchlistFiltered) while the empty list and the failed reads show no count.
#[test]
fn the_count_is_all_or_shown_of_total_and_absent_off_the_landed_list() {
    let mut s = screen(3);
    s.input(WatchlistIn::Options(WatchlistOptionsState { sort: super::options::SortKey::Title, ..Default::default() }));
    assert_eq!(s.count(), Some(Count::All(3)), "sort alone does not narrow");
    s.input(WatchlistIn::Options(WatchlistOptionsState { show: super::options::ShowFilter::Released, ..Default::default() }));
    s.input(WatchlistIn::Rows(vec![]));
    assert_eq!(s.readout(), Some(Readout::Filtered));
    assert_eq!(s.count(), Some(Count::Of { shown: 0, total: 3 }));
    s.input(WatchlistIn::List(ListState::Ready { total: 0 }));
    assert_eq!((s.readout(), s.count()), (Some(Readout::Empty), None));
    for l in [ListState::Loading, ListState::Unavailable, ListState::Failed] {
        s.input(WatchlistIn::List(l));
        assert_eq!(s.count(), None);
    }
}

/// **A Remove updates the count when the grid's landing STARTS**, narrowed or not: not at the commit
/// (the hold menu is still up), not under the menu's veil, but on the frame
/// the tiles begin to move, so the number and the grid change together.
#[test]
fn a_remove_lowers_the_count_when_the_landing_starts() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    use plx_ui::containers::modal::{set_veil, CLOSING_GONE};
    let mut h = Harness::new(tiles(12));
    h.focus_index(2);
    assert_eq!(h.screen.count(), Some(Count::All(12)));
    set_veil(1.0);
    h.landing(Landing::RemoveFocused).unwrap();
    assert_eq!(h.screen.tiles().len(), 11, "premise: committed");
    for frame in 0..30 {
        h.run(1);
        assert_eq!(h.screen.count(), Some(Count::All(12)), "frame {frame}: the menu is still up");
    }
    set_veil(CLOSING_GONE * 0.5);
    let mut lowered_at_start = false;
    for frame in 0..40 {
        h.run(1);
        let started = h.screen.landing_started();
        let lowered = h.screen.count() == Some(Count::All(11));
        assert_eq!(lowered, started, "frame {frame}: the count moves with the landing's first step");
        if lowered {
            lowered_at_start = true;
            break;
        }
    }
    assert!(lowered_at_start, "the landing began and the count followed");
    set_veil(0.0);
}

/// **A Remove holds the colour wash until the grid's landing ends.** The wash eases one 8-bit step
/// a frame (at the shared rate) toward the newly focused title, which invalidates the tab strip's glass on every
/// landing frame (the page is drawn twice for it); during the landing the target stays at the
/// pre-Remove colours, and afterwards eases to the title that took the removed one's place.
#[test]
fn a_remove_holds_the_wash_target_until_the_landing_ends() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    use plx_ui::containers::modal::set_veil;
    set_veil(0.0);
    let mut rows = tiles(12);
    for (i, t) in rows.iter_mut().enumerate() {
        let c = 20 + 15 * i as u8;
        t.copy.as_mut().unwrap().blur = Some([[c, 255 - c, c / 2]; 4]);
    }
    let mut h = Harness::new(rows);
    h.focus_index(2);
    h.run(5);
    let before = h.screen.ground_goal();
    assert_ne!(before, [theme::SURFACE_APP; 4], "premise: the removed title keys a colour");
    h.landing(Landing::RemoveFocused).unwrap();
    let mut held = 0;
    for frame in 0..200 {
        h.run(1);
        h.reconcile();
        if !h.screen.landing_in_flight() {
            break;
        }
        assert_eq!(h.screen.ground_goal(), before, "frame {frame}: the landing is in flight");
        held += 1;
    }
    assert!(held > 0, "premise: the landing ran for at least a frame");
    assert!(!h.screen.landing_in_flight(), "premise: the landing ended");
    h.run(2);
    let after = h.screen.ground_goal();
    assert_ne!(after, before, "after the landing the wash goes to the new title");
    assert_ne!(after, [theme::SURFACE_APP; 4]);
}

/// **The ease after a Remove's landing changes the drawn wash on about half the frames of an
/// ordinary dissolve.** Each frame the drawn 8-bit code changes re-blurs the tab strip's glass (the page is drawn twice), so
/// the wash that was held through the landing catches up at a faster, Watchlist-only rate; every
/// other ground keeps `AmbientWash::K`.
#[test]
fn the_ease_after_a_remove_is_about_half_the_ordinary_one() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    use plx_ui::containers::modal::set_veil;
    use plx_ui::widgets::PageGround;
    set_veil(0.0);
    let mut rows = tiles(12);
    for (i, t) in rows.iter_mut().enumerate() {
        let c = if i % 2 == 0 { 10 } else { 245 };
        t.copy.as_mut().unwrap().blur = Some([[c, 255 - c, 128]; 4]);
    }
    let mut h = Harness::new(rows);
    h.focus_index(2);
    h.run(5);
    let before = h.screen.ground_goal();
    h.landing(Landing::RemoveFocused).unwrap();
    for _ in 0..200 {
        h.run(1);
        h.reconcile();
        if !h.screen.landing_in_flight() {
            break;
        }
    }
    // The frames after the landing's end on which a drawn code changes: each is a double-drawn one.
    let (mut last, mut frames, mut quiet) = (h.screen.ground_codes(), 0, 0);
    for _ in 1..400 {
        h.run(1);
        let now = h.screen.ground_codes();
        if now != last {
            frames += 1;
            quiet = 0;
            last = now;
        } else {
            quiet += 1;
            if quiet > 30 {
                break;
            }
        }
    }
    let after = h.screen.ground_goal();
    // The ordinary dissolve over the same colours.
    let code = |g: &PageGround| g.corners().iter().flatten().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as i32).collect::<Vec<_>>();
    let mut g = PageGround::new();
    g.jump_target(before);
    let (mut glast, mut gframes, mut gquiet) = (code(&g), 0, 0);
    for _ in 1..400 {
        g.key_target(after, 16_667e-6);
        let now = code(&g);
        if now != glast {
            gframes += 1;
            gquiet = 0;
            glast = now;
        } else {
            gquiet += 1;
            if gquiet > 30 {
                break;
            }
        }
    }
    assert!(gframes >= 10, "premise: the ordinary dissolve is long ({gframes})");
    assert!(frames * 10 <= gframes * 6, "the post-Remove ease changed the drawn wash on {frames} frames against the ordinary {gframes}");
    assert!(frames * 10 >= gframes * 2, "and not a snap: {frames} against {gframes}");
}

/// A Remove no menu veiled starts its landing at once; the frame it begins in draws the frame before
/// it, so the count follows on the frame after, with the first step.
#[test]
fn an_unveiled_remove_lowers_the_count_with_its_first_step() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    use plx_ui::containers::modal::set_veil;
    set_veil(0.0);
    let mut h = Harness::new(tiles(12));
    h.focus_index(2);
    h.landing(Landing::RemoveFocused).unwrap();
    assert_eq!(h.screen.count(), Some(Count::All(12)), "the first frame is the frame before the Remove");
    h.run(1);
    assert_eq!(h.screen.count(), Some(Count::All(11)));
}

/// **`SHAPE` lists the fields in the order `write` emits them**, and the options in the canonical
/// form the menu uses (the sort a number, never a `Debug` string).
#[test]
fn the_shape_follows_the_write_order() {
    assert!(WatchlistScreen::SHAPE.starts_with("WatchlistScreen{entry:u32,next_elem:u32,list:{tag:u32,total:u64},options:{type:u32,sort:u32,"));
    assert!(!WatchlistScreen::SHAPE.contains("sort:str"));
}

/// **Nothing on the grid changes between a Remove's commit and the landing's start**: under the
/// hold menu's veil every survivor keeps the slot and the scale it was
/// drawn at, the new focus rests (no pop, no caption move), and the first frame of the landing is
/// the only frame anything moves on.
#[test]
fn a_remove_changes_nothing_on_the_grid_until_the_landing_starts() {
    use plx_ui::cards::conformance::CardHarness;
    use plx_ui::containers::modal::{set_veil, CLOSING_GONE};
    let mut h = Harness::new(tiles(14));
    h.focus_index(2);
    h.run(60);
    let before: Vec<(u32, plx_ui::Rect, f32)> =
        h.cards().into_iter().map(|e| (e, h.drawn_rect(e, 1.0).unwrap(), h.scale(e).unwrap())).collect();
    let removed = h.focused().unwrap();
    // every word the frame draws and where: the focus caption (title and year line) among them
    let words_before = h.draw_texts();
    assert!(words_before.iter().any(|(w, _)| w == "2002"), "premise: the removed title's caption is drawn: {words_before:?}");
    set_veil(1.0);
    let guid = h.screen.tiles()[h.screen.index_of(removed).unwrap()].row.guid.clone();
    h.screen.input(WatchlistIn::Removed(guid));
    for frame in 0..30 {
        h.run(1);
        h.reconcile();
        let now = h.focused().unwrap();
        assert_ne!(now, removed, "premise: focus reconciled off the removed tile");
        assert_eq!(h.draw_texts(), words_before, "frame {frame}: the caption (text and anchor) is as drawn before the commit");
        let leaving = h.screen.leaving_probe();
        for (e, rect, scale) in &before {
            if *e == removed || rect.y > 1080.0 {
                // (a card below the screen snaps: nothing of it is on screen to animate)
                continue;
            }
            let (mut r, s) = (h.drawn_rect(*e, 1.0).unwrap(), h.scale(*e).unwrap());
            // a card that changed row stands as its leaving copy, at full alpha, where it was drawn
            if let Some((_, copy, alpha)) = h.screen.index_of(*e).and_then(|i| leaving.iter().find(|l| l.0 == i)) {
                assert!((alpha - 1.0).abs() < 1e-3, "frame {frame}: card {e}'s copy is not whole: {alpha}");
                r = *copy;
            }
            let want = if *e == now { 1.0 } else { *scale };
            assert!(
                *e == now // the focused card's stop carries the full pop; its scale is checked below
                    || (r.x - rect.x).abs() < 0.5 && (r.y - rect.y).abs() < 0.5 && (r.w - rect.w).abs() < 0.5 && (r.h - rect.h).abs() < 0.5,
                "frame {frame}: card {e} moved under the menu: {rect:?} -> {r:?}"
            );
            assert!((s - want).abs() < 1e-3, "frame {frame}: card {e} rescaled under the menu: {want} -> {s}");
        }
    }
    set_veil(CLOSING_GONE * 0.5);
    let now = h.focused().unwrap();
    let mut last = 1.0f32;
    let mut rose = false;
    let mut moved = false;
    for frame in 0..120 {
        h.run(1);
        moved |= h.draw_texts().iter().any(|(w, _)| w == "2003");
        let s = h.scale(now).unwrap();
        assert!(s + 1e-4 >= last, "frame {frame}: the new focus collapsed from {last} to {s}");
        rose |= s > 1.001;
        last = s;
    }
    assert!(rose && (last - h.focus_scale()).abs() < 1e-3, "the new focus rose once to the pop and rests there");
    assert!(moved, "the landing's start moved the caption to the new focus");
    set_veil(0.0);
}

#[test]
fn a_refused_write_draws_the_debug_cue_under_the_title_until_a_write_goes_out() {
    let cue = super::header::dev_blocked_text().to_str().unwrap();
    let mut h = Harness::new(tiles(6));
    assert!(!h.draw_texts().iter().any(|(t, _)| t == cue), "no cue before any refusal");
    h.screen.input(WatchlistIn::DevBlocked(true));
    let texts = h.draw_texts();
    let (_, at) = texts.iter().find(|(t, _)| t == cue).expect("the cue is drawn once a write was refused");
    let title = texts.iter().find(|(t, _)| t == msg::watchlist_page_title()).map(|(_, r)| *r).unwrap();
    assert!(at.y > title.y + title.h - 1.0 && at.y + at.h < super::page::GRID_TOP, "between the title and the grid: {at:?}");
    h.screen.input(WatchlistIn::DevBlocked(false));
    assert!(!h.draw_texts().iter().any(|(t, _)| t == cue), "a write that went out clears it");
}

#[test]
fn the_debug_cue_is_compiled_only_into_a_devtriggers_build() {
    // The shipping feature set (nightly, stable) must not be able to draw or feed it (Q8).
    for (file, src, anchor) in [
        ("header.rs", include_str!("header.rs"), "pub fn dev_blocked_text"),
        ("header.rs", include_str!("header.rs"), "pub fn draw_dev_blocked"),
        ("page.rs", include_str!("page.rs"), "if self.dev_blocked {"),
    ] {
        let at = src.find(anchor).unwrap_or_else(|| panic!("{file}: `{anchor}` moved"));
        let before: Vec<&str> = src[..at].lines().rev().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//")).take(2).collect();
        assert!(before.iter().any(|l| *l == "#[cfg(feature = \"devtriggers\")]"),
            "{file}: `{anchor}` must sit under #[cfg(feature = \"devtriggers\")], saw {before:?}");
    }
}

/// Rows `n` whose copies carry distinct UltraBlur colours, except the indexes in `bare`.
fn coloured(n: usize, bare: &[usize]) -> Vec<super::page::WatchTile> {
    let mut rows = tiles(n);
    for (i, t) in rows.iter_mut().enumerate().filter(|(i, _)| !bare.contains(i)) {
        let c = 20 + 15 * i as u8;
        t.copy.as_mut().unwrap().blur = Some([[c, 255 - c, c / 2]; 4]);
    }
    rows
}

/// **A Remove never takes the page colour through neutral.** The title that takes the removed
/// one's place has not had its colours loaded yet: the wash holds the old key through the landing and
/// after it, and eases to the new title once its colours arrive. Neutral is for a page that never had a key.
#[test]
fn a_remove_never_passes_the_wash_through_neutral() {
    use plx_ui::cards::conformance::{CardHarness, Landing};
    use plx_ui::containers::modal::set_veil;
    set_veil(0.0);
    let flat = [theme::SURFACE_APP; 4];
    let mut h = Harness::new(coloured(12, &[3]));
    h.focus_index(2);
    h.run(120);
    let (before, resting) = (h.screen.ground_goal(), h.screen.ground_codes());
    assert_ne!(before, flat, "premise: the removed title keys a colour");
    h.landing(Landing::RemoveFocused).unwrap();
    for frame in 0..200 {
        h.run(1);
        h.reconcile();
        assert_ne!(h.screen.ground_goal(), flat, "frame {frame}: the wash never aims at neutral");
        assert_eq!(h.screen.ground_goal(), before, "frame {frame}: the old key holds until the new title's colours arrive");
        assert_eq!(h.screen.ground_codes(), resting, "frame {frame}: and the drawn wash does not move");
    }
    assert!(!h.screen.landing_in_flight(), "premise: the landing ended");
    let blur = [[0.1, 0.8, 0.2]; 4];
    h.screen.input(WatchlistIn::Facts("plex://movie/g3".into(), MatchedFacts { blur: Some(blur), ..Default::default() }));
    h.run(2);
    let after = h.screen.ground_goal();
    assert!(after != before && after != flat, "the new title's colours key the wash once they are ready");
}

/// **A 9 to 14 filter never takes the page colour through neutral either.** The menu holds the
/// engine's focus while the cards reflow, so the page's mirror of it is an index the new list reads
/// as another title, one whose colours are not loaded. The wash follows the focused ELEMENT, not that index.
#[test]
fn a_filter_never_passes_the_wash_through_neutral_or_another_title() {
    use plx_machine::machine::{EntryId, FocusKey};
    use plx_ui::containers::modal::set_veil;
    set_veil(0.0);
    let flat = [theme::SURFACE_APP; 4];
    let mut h = Harness::new(coloured(9, &[]));
    h.focus_index(2);
    h.run(120);
    let (before, resting) = (h.screen.ground_goal(), h.screen.ground_codes());
    let focused = h.focus.unwrap().elem;
    assert_ne!(before, flat, "premise");
    // the Sort & Filter menu takes the engine's focus; the page keeps its own mirror
    h.focus = Some(FocusKey { entry: EntryId(77), elem: 1 });
    // 14 titles: five without colours arrive before the focused one, and the title now at its old index has none
    let mut rows: Vec<super::page::WatchTile> = (100..105).map(|i| tile(i)).collect();
    rows.extend(h.screen.tiles().iter().cloned());
    h.screen.input(WatchlistIn::Options(WatchlistOptionsState::default()));
    h.screen.input(WatchlistIn::Rows(rows));
    assert_ne!(h.screen.index_of(focused), Some(2), "premise: the focused title moved");
    for frame in 0..300 {
        h.run(1);
        assert_ne!(h.screen.ground_goal(), flat, "frame {frame}: the wash never aims at neutral");
        assert_eq!(h.screen.ground_goal(), before, "frame {frame}: it stays on the focused title's key");
        assert_eq!(h.screen.ground_codes(), resting, "frame {frame}: the drawn wash does not move");
    }
}
