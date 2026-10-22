//! **The Watchlist's opening** (kept by the owner through the Library redesign): a page that mounts
//! with its first read still on its way seats NOTHING (`PageRegions::pending`, the rule the Library
//! and Recommended pages follow: one request, `PageRegions::unseat`, naming the grid group it
//! declares empty), and when the first tiles land the seat is an ANIMATED seat on the first tile
//! (its focus pop rises as it arrives). A list that landed empty keeps the header's pill. The grid's
//! first landing itself plays no fade (measured). This pins that choreography so a header or
//! geometry change cannot quietly drop it.
use plx_machine::machine::{Delivery, Enter, Fx, FocusTarget, GroupId};
use plx_ui::screen::ScreenEvent;

use super::cards_harness::{tiles, Harness, ENTRY};
use super::page::{ListState, WatchlistIn, WatchlistScreen};

const HEADER: GroupId = GroupId(1);
const GRID: GroupId = GroupId(0);

fn seats(fx: &[plx_machine::machine::Stamped<super::cards_harness::WatchHost>]) -> Vec<GroupId> {
    fx.iter()
        .filter_map(|s| match &s.fx {
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroupAnimated(g) }))) => {
                Some(*g)
            }
            _ => None,
        })
        .collect()
}

fn loading() -> Harness {
    let mut h = Harness::new(Vec::new());
    h.screen = WatchlistScreen::loading(ENTRY);
    h.run(1);
    h
}

/// The fresh enters the page asked of the engine that name `FirstInGroup` (not animated): the unseat.
fn unseats(fx: &[plx_machine::machine::Stamped<super::cards_harness::WatchHost>]) -> Vec<GroupId> {
    fx.iter()
        .filter_map(|s| match &s.fx {
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(g) }))) => Some(*g),
            _ => None,
        })
        .collect()
}

#[test]
fn a_fresh_page_seats_nothing_and_then_the_first_tile_with_an_animated_seat() {
    let mut h = loading();
    let (fx, _) = h.step_fx(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GRID) }));
    assert_eq!(seats(&fx), vec![], "nothing has landed: no pill, no tile holds the arrival seat");
    assert_eq!(unseats(&fx), vec![GRID], "the one request names the pending grid, declared empty");
    // the engine's answer comes back, then the rows land
    h.step(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GRID) }));
    let (fx, _) = h.step_fx(ScreenEvent::Tick(plx_machine::machine::Tick { ms: 50, dt_us: 16_667 }));
    assert!(seats(&fx).is_empty() && unseats(&fx).is_empty(), "still loading: the page does not ask again");
    h.screen.input(WatchlistIn::List(ListState::Ready { total: 12 }));
    h.screen.input(WatchlistIn::Rows(tiles(12)));
    let (fx, _) = h.step_fx(ScreenEvent::Tick(plx_machine::machine::Tick { ms: 100, dt_us: 16_667 }));
    assert_eq!(seats(&fx), vec![GRID], "the first tiles landed: the arrival follows them");
}

#[test]
fn a_list_that_landed_empty_seats_the_pill() {
    let mut h = loading();
    h.step(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GRID) }));
    h.step(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GRID) }));
    h.screen.input(WatchlistIn::List(ListState::Ready { total: 0 }));
    let (fx, _) = h.step_fx(ScreenEvent::Tick(plx_machine::machine::Tick { ms: 100, dt_us: 16_667 }));
    assert_eq!(seats(&fx), vec![HEADER], "loaded and empty: the pill is the page's one control");
}
