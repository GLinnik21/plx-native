//! **The Watchlist's opening** (kept by the owner through the Library redesign): a page that mounts
//! with nothing landed holds its arrival seat on the header's pill, and when the first tiles land the
//! seat follows them with an ANIMATED seat on the first tile (the first tile's focus pop rises as it
//! arrives). The grid's first landing itself plays no fade (measured). This pins that choreography so
//! a header or geometry change cannot quietly drop it.
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

#[test]
fn a_fresh_page_seats_the_pill_and_then_the_first_tile_with_an_animated_seat() {
    let mut h = loading();
    let (fx, _) = h.step_fx(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GRID) }));
    assert_eq!(seats(&fx), vec![HEADER], "nothing has landed: the pill holds the arrival seat");
    // the engine's seat comes back, then the rows land
    h.step(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroupAnimated(HEADER) }));
    h.screen.input(WatchlistIn::List(ListState::Ready { total: 12 }));
    h.screen.input(WatchlistIn::Rows(tiles(12)));
    let (fx, _) = h.step_fx(ScreenEvent::Tick(plx_machine::machine::Tick { ms: 100, dt_us: 16_667 }));
    assert_eq!(seats(&fx), vec![GRID], "the first tiles landed: the arrival follows them");
}
