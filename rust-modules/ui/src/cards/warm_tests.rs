// A shelf's artwork warm-ahead (`Shelf::warm_ahead`), over a spy [`Source`] that keeps the
// contract the real poster store keeps (`poster.rs`, `warm_admissible`): a lookahead claim is
// refused while a drawn card's request is outstanding, and at most one speculative fetch is out
// at a time. What the store may EVICT for a claim is graded there
// (`a_lookahead_claim_never_takes_a_slot_the_frame_drew_or_the_newest_ordinary_ones`); here:
// WHICH cards a shelf asks for, WHEN, and how much per tick.
use std::cell::RefCell;
use std::collections::HashSet;

use super::shelf::WARM_AHEAD_CARDS;
use super::{CardSource, Shelf};
use crate::card_row::{RowStyle, TileLabel};
use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
use crate::screen::{By, ScreenEvent};
use crate::tex::{self, Source, Warm};
use crate::widgets::{self, Art};
use plx_machine::machine::{
    Cx, Effects, EntryId, FocusKey, FocusRead, InputOwner, InstanceId, MachineId, PosterKey, PressRead, Tick,
};
use plx_machine::present::Present;

const ENTRY: EntryId = EntryId(9);
const STYLE: RowStyle = RowStyle::HOME;

#[derive(Default)]
struct Log {
    /// every `warm_ahead` call, in order, with the card number it named
    asked: Vec<usize>,
    /// the cards the source holds (claimed by a warm or a draw)
    held: HashSet<usize>,
    /// cards a draw asked for that are still queued
    visible_out: HashSet<usize>,
    /// the speculative fetch in flight, if any
    spec_out: Option<usize>,
    /// cards the spy admitted as lookahead claims, in order
    claims: Vec<usize>,
    /// claims attempted while a drawn request was outstanding (the store refuses these)
    refused: usize,
    /// each card the first time a draw asked for it, and whether it was already held then
    first_draw_held: Vec<(usize, bool)>,
}

thread_local! {
    static LOG: RefCell<Log> = RefCell::new(Log::default());
}

fn number(path: &str) -> usize {
    path.strip_prefix("/thumb/").unwrap().parse().unwrap()
}

struct Spy;
impl Source for Spy {
    fn probe(&self, _: u16, path: &str, _: i32, _: i32, _: bool) -> Option<PosterKey> {
        let n = number(path);
        LOG.with(|l| {
            let mut l = l.borrow_mut();
            if !l.first_draw_held.iter().any(|&(c, _)| c == n) {
                let was = l.held.contains(&n);
                l.first_draw_held.push((n, was));
            }
            if l.held.insert(n) {
                l.visible_out.insert(n);
            }
        });
        None
    }
    fn warm(&self, _: u16, _: &str, _: i32, _: i32, _: bool) -> Warm {
        Warm::Known
    }
    fn warm_ahead(&self, _: u16, path: &str, _: i32, _: i32, _: bool) -> Warm {
        let n = number(path);
        LOG.with(|l| {
            let mut l = l.borrow_mut();
            l.asked.push(n);
            if l.held.contains(&n) {
                return Warm::Known;
            }
            if !l.visible_out.is_empty() {
                l.refused += 1;
                return Warm::Full;
            }
            if l.spec_out.is_some() {
                return Warm::Full;
            }
            l.held.insert(n);
            l.spec_out = Some(n);
            l.claims.push(n);
            Warm::Claimed
        })
    }
    fn logo(&self, _: u16, _: &str) -> Option<PosterKey> {
        None
    }
    fn logo_warm(&self, _: u16, _: &str) -> Warm {
        Warm::Known
    }
    fn unresident(&self, _: PosterKey, _: bool) {}
    fn idle(&self) -> bool {
        true
    }
}

struct Cards {
    paths: Vec<String>,
}

impl Cards {
    fn new(n: usize) -> Self {
        Self { paths: (0..n).map(|i| format!("/thumb/{i}")).collect() }
    }
}

impl CardSource<FixtureHost> for Cards {
    fn len(&self) -> usize {
        self.paths.len()
    }
    fn elem(&self, i: usize) -> u32 {
        100 + i as u32
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        e.checked_sub(100).map(|i| i as usize).filter(|&i| i < self.paths.len())
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Thumb { sid: 1, key: &self.paths[i], res: (300, 450) }
    }
    fn label(&self, _: usize) -> TileLabel {
        TileLabel::default()
    }
}

struct Rig {
    shelf: Shelf,
    src: Cards,
    view: FixtureView,
    focus: Option<FocusKey<u32>>,
    ms: u32,
}

impl Rig {
    fn new(n: usize) -> Self {
        tex::install(&Spy);
        LOG.with(|l| *l.borrow_mut() = Log::default());
        Self { shelf: Shelf::new(ENTRY, &STYLE), src: Cards::new(n), view: FixtureView::default(), focus: None, ms: 0 }
    }

    fn feed(&mut self, ev: ScreenEvent<FixtureHost>) {
        let mut present = Present::new();
        let mut out = Vec::new();
        let cx = Cx {
            views: FixtureViews { store: &self.view },
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: 1.0, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        };
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
        self.shelf.on(&ev, &cx, &self.src, &mut fx);
    }

    fn focus_on(&mut self, i: usize, by: By) {
        let from = self.focus;
        let to = FocusKey { entry: ENTRY, elem: 100 + i as u32 };
        self.focus = Some(to);
        self.feed(ScreenEvent::FocusMoved { from, to, by });
    }

    /// One frame: the shelf's tick (which warms), then the draw's probes of the on-axis cards.
    fn frame(&mut self) {
        self.ms += 16;
        self.feed(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        let pitch = STYLE.w + STYLE.gap;
        for i in 0..self.src.len() {
            let x = STYLE.margin_x + i as f32 * pitch - self.shelf.scroll();
            if crate::on_axis(x, STYLE.w, crate::consts::SCR_W, 0.0) {
                let (srv, path, w, h) = widgets::card_art_request(&self.src.art(i)).unwrap();
                tex::resolve_wh_on(srv, path, w, h, false);
            }
        }
    }

    /// Every outstanding fetch lands (a fast server).
    fn land_all(&self) {
        LOG.with(|l| {
            let mut l = l.borrow_mut();
            l.visible_out.clear();
            l.spec_out = None;
        });
    }
}

fn log<R>(f: impl FnOnce(&Log) -> R) -> R {
    LOG.with(|l| f(&l.borrow()))
}

/// Hold Right for `steps` cards, one key repeat every `every` frames, fetches landing each frame.
fn hold_right(r: &mut Rig, steps: usize, every: usize) {
    r.focus_on(0, By::Restore);
    r.frame();
    r.land_all();
    for step in 1..=steps {
        r.focus_on(step, By::Dir);
        for _ in 0..every {
            r.frame();
            r.land_all();
        }
    }
}

#[test]
fn the_cards_ahead_of_a_held_right_are_requested_before_they_are_drawn() {
    let mut r = Rig::new(60);
    hold_right(&mut r, 25, 6);
    let late: Vec<usize> =
        log(|l| l.first_draw_held.iter().filter(|&&(c, was)| c >= 12 && !was).map(|&(c, _)| c).collect());
    assert!(late.is_empty(), "cards drawn for the first time with no request already made: {late:?}");
    assert!(log(|l| l.claims.iter().any(|&c| c >= 12)), "no card past the first screen was warmed at all");
}

#[test]
fn nothing_behind_the_direction_of_travel_is_warmed() {
    let mut r = Rig::new(60);
    hold_right(&mut r, 20, 6);
    let behind = log(|l| l.claims.iter().filter(|&&c| c < 3).count());
    assert_eq!(behind, 0, "a held Right warmed the cards behind it");
}

#[test]
fn a_visible_request_is_never_queued_behind_a_warm_ahead() {
    let mut r = Rig::new(60);
    r.focus_on(0, By::Restore);
    r.frame();
    r.land_all();
    // a drawn card's request is outstanding; the spy (like the store) refuses every warm meanwhile
    LOG.with(|l| {
        l.borrow_mut().visible_out.insert(999);
    });
    let before = log(|l| l.claims.len());
    for _ in 0..10 {
        r.frame();
    }
    assert_eq!(log(|l| l.claims.len()), before, "a lookahead claim was made while a drawn request was queued");
    assert!(log(|l| l.refused) > 0, "the shelf never asked, so the refusal went unexercised");
    // the drawn cards were still asked for, whatever the warm said
    assert!(log(|l| (0..7).all(|c| l.held.contains(&c))));
}

#[test]
fn the_work_per_tick_is_bounded() {
    let mut r = Rig::new(400);
    r.focus_on(0, By::Restore);
    for _ in 0..3 {
        r.frame();
        r.land_all();
    }
    let mut worst = 0;
    for step in 1..60 {
        r.focus_on(step, By::Dir);
        for _ in 0..3 {
            let asked = log(|l| l.asked.len());
            let claimed = log(|l| l.claims.len());
            r.frame();
            r.land_all();
            worst = worst.max(log(|l| l.asked.len()) - asked);
            assert!(log(|l| l.claims.len()) - claimed <= 1, "more than one claim in a tick");
        }
    }
    assert!(worst <= 2 * WARM_AHEAD_CARDS, "a tick scanned {worst} cards");
    assert!(
        log(|l| l.held.len()) < 60 + 40 + 3 * WARM_AHEAD_CARDS,
        "the held set grew past the walk plus its lookahead"
    );
}

#[test]
fn a_resting_shelf_with_everything_warm_asks_for_nothing() {
    let mut r = Rig::new(60);
    r.focus_on(0, By::Restore);
    for _ in 0..40 {
        r.frame();
        r.land_all();
    }
    let asked = log(|l| l.asked.len());
    for _ in 0..40 {
        r.frame();
        r.land_all();
    }
    assert_eq!(log(|l| l.asked.len()), asked, "an unchanged shelf kept scanning the source");
}

#[test]
fn a_window_slide_warms_the_cards_that_just_landed_ahead() {
    let mut r = Rig::new(24);
    r.focus_on(20, By::Restore);
    for _ in 0..40 {
        r.frame();
        r.land_all();
    }
    assert!(log(|l| l.held.contains(&23)), "focus near the tail left the tail cold");
    // the window slides: more cards land after the tail and the focus keeps its element
    r.src = Cards::new(48);
    for _ in 0..40 {
        r.frame();
        r.land_all();
    }
    assert!(log(|l| l.claims.iter().any(|&c| c >= 24)), "cards landed past the old tail were not warmed");
}

#[test]
fn a_shelf_that_is_not_focused_asks_for_nothing() {
    let mut r = Rig::new(60);
    for _ in 0..10 {
        r.frame();
        r.land_all();
    }
    assert!(log(|l| l.asked.is_empty()), "an unfocused shelf warmed {:?}", log(|l| l.asked.clone()));
}

#[test]
fn a_held_left_warms_the_cards_behind_it_and_none_ahead() {
    let mut r = Rig::new(60);
    r.focus_on(50, By::Restore);
    for _ in 0..40 {
        r.frame();
        r.land_all();
    }
    // the restore's own scroll sweep drew the cards on the way; only the travel left is graded
    LOG.with(|l| l.borrow_mut().first_draw_held.clear());
    for step in 1..=25 {
        r.focus_on(50 - step, By::Dir);
        for _ in 0..6 {
            r.frame();
            r.land_all();
        }
    }
    let late: Vec<usize> = log(|l| l.first_draw_held.iter().filter(|&&(c, was)| c < 40 && !was).map(|&(c, _)| c).collect());
    assert!(late.is_empty(), "cards drawn for the first time with no request already made: {late:?}");
}
