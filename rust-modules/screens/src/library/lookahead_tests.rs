// The Library's at-rest poster lookahead (`lookahead.rs`, `GridPart::prepare_ahead`), over a
// spy [`Source`]. The store half (what a lookahead claim may evict) is graded in
// `app/adapters/poster.rs`; here: WHICH cards are asked for, WHEN, and that the asking ends.
use super::*;
use plx_machine::machine::PosterKey;
use plx_ui::tex::{Source, Warm};
use std::cell::RefCell;
use std::collections::HashSet;

type Req = (u16, String, i32, i32);

#[derive(Default)]
struct Log {
    probes: Vec<Req>,
    warms: usize,
    asked: Vec<Req>,
    held: HashSet<Req>,
    claims: Vec<Req>,
    /// How many claims the spy still accepts before answering `Full`.
    budget: usize,
}

thread_local! {
    static LOG: RefCell<Log> = RefCell::new(Log::default());
}

struct Spy;
impl Source for Spy {
    fn probe(&self, srv: u16, path: &str, w: i32, h: i32, _: bool) -> Option<PosterKey> {
        LOG.with(|l| l.borrow_mut().probes.push((srv, path.into(), w, h)));
        None
    }
    fn warm(&self, _: u16, _: &str, _: i32, _: i32, _: bool) -> Warm {
        LOG.with(|l| l.borrow_mut().warms += 1);
        Warm::Claimed
    }
    fn warm_ahead(&self, srv: u16, path: &str, w: i32, h: i32, _: bool) -> Warm {
        LOG.with(|l| {
            let mut l = l.borrow_mut();
            let req = (srv, path.to_string(), w, h);
            l.asked.push(req.clone());
            if l.held.contains(&req) { return Warm::Known; }
            if l.budget == 0 { return Warm::Full; }
            l.budget -= 1;
            l.held.insert(req.clone());
            l.claims.push(req);
            Warm::Claimed
        })
    }
    fn logo(&self, _: u16, _: &str) -> Option<PosterKey> { None }
    fn logo_warm(&self, _: u16, _: &str) -> Warm { Warm::Known }
    fn unresident(&self, _: PosterKey, _: bool) {}
    fn idle(&self) -> bool { true }
}

fn reset(budget: usize) {
    plx_ui::tex::install(&Spy);
    LOG.with(|l| *l.borrow_mut() = Log { budget, ..Default::default() });
}

/// The rows a card draw would paint at `scroll`.
fn visible_rows(layout: &Layout, scroll: f32) -> (usize, usize) {
    let (lo, hi) = layout.visible_rows(scroll);
    let on = |row: usize| plx_ui::cards::paint_visible(plx_ui::Painter::root(), Rect::new(
        layout.cell_x(0), layout.row_y(row, scroll), layout.card_w(), layout.card_h()), 1.0, false);
    ((lo..hi).find(|&r| on(r)).unwrap(), (lo..hi).rev().find(|&r| on(r)).unwrap())
}

fn claimed_rows(cols: usize, prefix: &str) -> Vec<usize> {
    LOG.with(|l| l.borrow().claims.iter()
        .map(|(_, path, ..)| path.strip_prefix(prefix).unwrap().parse::<usize>().unwrap() / cols)
        .collect())
}

fn frames(page: &mut LibraryScreen, fixture: &Fixture, n: usize, ahead: usize) {
    for _ in 0..n { page.pair.detail.prepare_ahead(ahead, &fixture.cx(None)); }
}

#[test]
fn with_no_trigger_lookahead_is_three_rows_and_prepare_reaches_the_source() {
    let _guard = plx_base::testlock::serial();
    reset(usize::MAX);
    assert_eq!(lookahead::rows_for(None), 3, "no plxnative-lookahead trigger: the default depth");
    assert_eq!(lookahead::rows(), lookahead::rows_for(None), "unit tests never read the host's trigger file");
    let fixture = Fixture::art_listing(false);
    let mut page = fixture.screen();
    let scroll = page.layout.row_reveal(20);
    page.pair.detail.set_geometry(page.layout, scroll, page.layout, scroll);
    let mut budget = plx_ui::frame::Budget::new();
    for _ in 0..60 {
        <parts::GridPart as plx_ui::screen::Part<HostFixture>>::prepare(&mut page.pair.detail, &mut budget, &fixture.cx(None));
    }
    let cols = page.layout.cols();
    let rows = claimed_rows(cols, "/poster/");
    assert_eq!(rows.len(), 4 * cols, "three rows ahead plus one behind, with no trigger at all");
}

#[test]
fn a_zero_depth_is_off_and_asks_for_nothing() {
    let _guard = plx_base::testlock::serial();
    reset(usize::MAX);
    let fixture = Fixture::art_listing(false);
    let mut page = fixture.screen();
    let scroll = page.layout.row_reveal(20);
    page.pair.detail.set_geometry(page.layout, scroll, page.layout, scroll);
    frames(&mut page, &fixture, 20, 0);
    LOG.with(|l| {
        let l = l.borrow();
        assert!(l.asked.is_empty() && l.claims.is_empty() && l.warms == 0 && l.probes.is_empty());
    });
}

#[test]
fn an_at_rest_grid_warms_a_bounded_set_nearest_row_first_then_goes_quiet() {
    let _guard = plx_base::testlock::serial();
    for episodes in [false, true] {
        reset(usize::MAX);
        let fixture = Fixture::art_listing(episodes);
        let mut page = fixture.screen();
        let layout = page.layout;
        let cols = layout.cols();
        let scroll = layout.row_reveal(20);
        page.pair.detail.set_geometry(layout, scroll, layout, scroll);
        let (first, last) = visible_rows(&layout, scroll);
        let prefix = if episodes { "/still/" } else { "/poster/" };

        frames(&mut page, &fixture, 2, 2);
        LOG.with(|l| assert!(l.borrow().asked.is_empty(),
            "the cards on screen get the first frames to claim their own art"));
        frames(&mut page, &fixture, 1, 2);
        assert_eq!(claimed_rows(cols, prefix), vec![last + 1], "one card per frame, nearest row first");
        frames(&mut page, &fixture, 100, 2);

        let rows = claimed_rows(cols, prefix);
        assert_eq!(rows.len(), 3 * cols, "two rows ahead plus one behind, no more");
        assert!(rows.len() <= plx_ui::tex::AHEAD_SET_MAX);
        assert_eq!(rows[..cols], vec![last + 1; cols][..], "the nearest row completes before the next starts");
        let set: HashSet<_> = rows.iter().copied().collect();
        assert_eq!(set, HashSet::from([last + 1, last + 2, first - 1]), "ahead in the travel direction, one behind");
        assert!(rows.iter().all(|&r| r > last || r < first), "never a card the grid is drawing");
        // After the set is held, further frames claim nothing more: the page can go idle.
        let held = LOG.with(|l| l.borrow().claims.len());
        frames(&mut page, &fixture, 50, 2);
        assert_eq!(LOG.with(|l| l.borrow().claims.len()), held);
        // The spy never saw a Draw or a plain Warm: the lookahead is its own door.
        LOG.with(|l| assert!(l.borrow().probes.is_empty() && l.borrow().warms == 0));
    }
}

#[test]
fn a_moving_grid_asks_for_nothing_and_the_travel_direction_picks_the_side() {
    let _guard = plx_base::testlock::serial();
    reset(usize::MAX);
    let fixture = Fixture::art_listing(false);
    let mut page = fixture.screen();
    let layout = page.layout;
    let cols = layout.cols();
    let base = layout.row_reveal(20);
    // Held key: the target moves every frame, and the spring trails it.
    for i in 0..60 {
        let target = base - i as f32 * 40.0;
        page.pair.detail.set_geometry(layout, target + 25.0, layout, target);
        frames(&mut page, &fixture, 1, 2);
    }
    // The spring still on its way to a target that has stopped.
    let target = base - 60.0 * 40.0;
    for _ in 0..30 {
        page.pair.detail.set_geometry(layout, target + 40.0, layout, target);
        frames(&mut page, &fixture, 1, 2);
    }
    LOG.with(|l| assert!(l.borrow().asked.is_empty(), "nothing is asked while the grid moves"));
    // It arrives: travelling UP, so the rows ahead are above the window and one is kept below.
    page.pair.detail.set_geometry(layout, target, layout, target);
    frames(&mut page, &fixture, 100, 2);
    let (first, last) = visible_rows(&layout, target);
    let rows = claimed_rows(cols, "/poster/");
    assert_eq!(rows[0], first - 1, "the nearest row is the one in the direction of travel");
    assert_eq!(rows.iter().copied().collect::<HashSet<_>>(), HashSet::from([first - 1, first - 2, last + 1]));
}

#[test]
fn a_source_with_no_slot_to_give_ends_the_asking_one_ask_per_frame() {
    let _guard = plx_base::testlock::serial();
    reset(5);
    let fixture = Fixture::art_listing(false);
    let mut page = fixture.screen();
    let scroll = page.layout.row_reveal(20);
    page.pair.detail.set_geometry(page.layout, scroll, page.layout, scroll);
    frames(&mut page, &fixture, 2, 3);
    let mut previous = 0;
    for _ in 0..40 {
        frames(&mut page, &fixture, 1, 3);
        let asked = LOG.with(|l| l.borrow().asked.len());
        let claims = LOG.with(|l| l.borrow().claims.len());
        assert!(claims <= 5);
        // A frame that is refused stops its walk at the refusal: it adds one ask, not a sweep.
        if claims == 5 { assert!(asked - previous <= 5 + 1, "a refused frame walks held cards then stops"); }
        previous = asked;
    }
    assert_eq!(LOG.with(|l| l.borrow().claims.len()), 5);
}

/// The lookahead warms exactly the key the card's draw resolves, or it fills a slot nothing hits.
#[test]
fn the_lookahead_asks_for_the_key_the_card_draw_resolves() {
    let _guard = plx_base::testlock::serial();
    for episodes in [false, true] {
        reset(usize::MAX);
        let fixture = Fixture::art_listing(episodes);
        let view = fixture.listing.view();
        for i in [0usize, 7, 431] {
            let art = parts::grid_art(view.item(i).unwrap());
            plx_ui::widgets::resolve_card_art(plx_ui::Painter::root(), Rect::new(0.0, 0.0, 250.0, 375.0), &art);
            let (srv, path, w, h) = plx_ui::widgets::card_art_request(&art).unwrap();
            let drawn = LOG.with(|l| l.borrow_mut().probes.pop().unwrap());
            assert_eq!(drawn, (srv, path.to_string(), w, h), "episodes={episodes} card {i}");
        }
    }
}

/// The default depth is a property of the rule, not of whatever trigger file the host holds.
#[test]
fn the_depth_is_three_rows_unless_a_trigger_that_parses_says_otherwise() {
    assert_eq!(lookahead::rows_for(None), 3);
    assert_eq!(lookahead::rows_for(Some("junk")), 3, "an unparsable trigger keeps the default");
    assert_eq!(lookahead::rows_for(Some("0")), 0, "0 turns the lookahead off");
    assert_eq!(lookahead::rows_for(Some("2")), 2);
}

/// The source protects the `plx_ui::tex::ON_SCREEN_ART_MAX` most recently used slots from a
/// lookahead claim (`poster.rs`, `AHEAD_PROTECT`) because the claim runs in `prepare`, before any
/// draw of the frame: this is the number of cards the grid can show at once, at every scroll
/// position, for both grid shapes.
#[test]
fn no_scroll_position_shows_more_cards_than_the_source_protects() {
    let _guard = plx_base::testlock::serial();
    for episodes in [false, true] {
        reset(usize::MAX);
        let fixture = Fixture::art_listing(episodes);
        let page = fixture.screen();
        let layout = page.layout;
        let (mut most, mut scroll) = (0, 0.0f32);
        while scroll <= layout.row_reveal(60) {
            let (lo, hi) = visible_rows(&layout, scroll);
            most = most.max((hi - lo + 1) * layout.cols());
            scroll += 7.0;
        }
        assert!(most > 0 && most <= plx_ui::tex::ON_SCREEN_ART_MAX,
            "episodes={episodes}: the grid shows {most} cards at once; the source protects {}", plx_ui::tex::ON_SCREEN_ART_MAX);
    }
}

#[test]
fn trigger_text_is_a_capped_row_count() {
    assert_eq!(lookahead::parse_rows("2"), Some(2));
    assert_eq!(lookahead::parse_rows(" 0\n"), Some(0));
    assert_eq!(lookahead::parse_rows("99"), Some(4));
    for bad in ["", "off", "-1", "1.5", "two"] { assert_eq!(lookahead::parse_rows(bad), None, "{bad:?}"); }
}
