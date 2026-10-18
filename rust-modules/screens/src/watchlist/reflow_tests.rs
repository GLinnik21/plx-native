//! What the Watchlist grid DRAWS while its cards reflow (a Sort & Filter change, a reorder, a count
//! change, a Remove): the first frame of a change is the frame before it, no two cards share a cell
//! at rest or in flight, and the back-to-front order is ghosts, leaving copies, still cards, cards fading in, moving survivors (the
//! longest path on top), the focused card.
//!
//! These read `WatchlistScreen::drawn_probe`, which is `Grid::draw`'s own plan with the rects it
//! paints; the Harness feeds the page BEFORE it ticks it, the order the app's frame now keeps
//! (`app::bridge::frame_ingest`).
use plx_ui::cards::DrawRole;
use plx_ui::Rect;

use super::cards_harness::{tile, tiles, Harness};
use super::header::Count;
use super::options::{TypeFilter, WatchlistOptionsState};
use super::page::{Drawn, WatchTile, WatchlistIn};

/// What a viewer sees: each visible thing as `(guid, rect x100, alpha x1000)`, sorted. The role is
/// not part of it (a leaving copy IS the card it was), and a thing at alpha 0 is not there, nor one
/// below the screen's bottom edge (the landing animates only what can be seen).
fn visible(h: &Harness) -> Vec<(String, [i32; 4], i32)> {
    let mut v: Vec<_> = h
        .screen
        .drawn_probe(h.focus.map(|k| k.elem))
        .into_iter()
        .filter(|d| d.alpha > 0.001 && d.rect.y < plx_ui::consts::SCR_H)
        .map(|d| (d.guid, rect_key(d.rect), (d.alpha * 1000.0).round() as i32))
        .collect();
    v.sort();
    v
}

fn rect_key(r: Rect) -> [i32; 4] {
    [(r.x * 100.0).round() as i32, (r.y * 100.0).round() as i32, (r.w * 100.0).round() as i32, (r.h * 100.0).round() as i32]
}

fn cards(h: &Harness) -> Vec<Drawn> {
    h.screen
        .drawn_probe(h.focus.map(|k| k.elem))
        .into_iter()
        .filter(|d| matches!(d.role, Some(DrawRole::Still | DrawRole::Arriving | DrawRole::Moving | DrawRole::Focus)))
        .collect()
}

fn overlap(a: Rect, b: Rect) -> f32 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w > 1.0 && h > 1.0 { w * h } else { 0.0 }
}

/// Two drawn cards sharing a cell: `(a, b)` of the first pair whose rects overlap while both can be
/// seen. A card fading in is not counted: it is drawn under the cards in flight and begins once its
/// cell is half clear (`a_new_card_starts_to_fade_in_once_its_cell_is_half_clear`).
fn sharing(h: &Harness) -> Option<(String, String)> {
    let cs: Vec<Drawn> = cards(h).into_iter().filter(|d| d.alpha > 0.01 && d.role != Some(DrawRole::Arriving)).collect();
    for (i, a) in cs.iter().enumerate() {
        for b in &cs[i + 1..] {
            if overlap(a.rect, b.rect) > 0.0 {
                return Some((format!("{} {:?} a={:.2}", a.guid, a.rect, a.alpha), format!("{} {:?} a={:.2}", b.guid, b.rect, b.alpha)));
            }
        }
    }
    None
}

fn reversed(rows: &[WatchTile]) -> Vec<WatchTile> {
    rows.iter().rev().cloned().collect()
}

/// Run until nothing moves, asserting `check` on every frame.
fn play(h: &mut Harness, frames: u32, mut check: impl FnMut(&Harness, u32)) {
    for f in 0..frames {
        h.run(1);
        h.reconcile();
        check(h, f);
    }
}

/// **A change shows the frame before it first.** The page is fed, then ticked: the tick starts the
/// landing and the frame drawn from that tick is exactly the previous one (survivors where they
/// stood, the leaving copies and the ghost at full alpha, the arrivals not yet there). A step on the
/// tick that began the landing moved everything a frame's worth, which read as a snap.
#[test]
fn the_first_frame_of_a_reorder_is_the_frame_before_it() {
    let mut h = Harness::new(tiles(14));
    h.run(60);
    let before = visible(&h);
    assert!(before.len() >= 12, "premise: the first rows are on show");
    h.screen.input(WatchlistIn::Rows(reversed(h.screen.tiles())));
    h.run(1);
    assert_eq!(visible(&h), before, "the first frame of a reorder is the frame before it");
}

#[test]
fn the_first_frame_of_a_filter_is_the_frame_before_it() {
    let mut h = Harness::new(tiles(14));
    h.run(60);
    let before = visible(&h);
    let kept: Vec<WatchTile> = h.screen.tiles().iter().enumerate().filter(|(i, _)| i % 3 != 1).map(|(_, t)| t.clone()).collect();
    h.screen.input(WatchlistIn::Rows(kept));
    h.run(1);
    // the survivors stand where they stood, the filtered-out cards are ghosts at full alpha where
    // they stood: the multiset of what is seen is the one before
    assert_eq!(visible(&h), before, "the first frame of a filter is the frame before it");
}

#[test]
fn the_first_frame_of_a_remove_is_the_frame_before_it() {
    let mut h = Harness::new(tiles(14));
    h.run(60);
    let before = visible(&h);
    let gone = h.screen.tiles()[5].row.guid.clone();
    h.screen.input(WatchlistIn::Removed(gone));
    h.run(1);
    assert_eq!(visible(&h), before, "the first frame of a Remove is the frame before it");
}

/// **No two cards share a cell at rest, whatever the change was.**
#[test]
fn no_two_cards_share_a_cell_at_rest() {
    for (what, make) in [
        ("two to four", Box::new(|_: &[WatchTile]| tiles(4)) as Box<dyn Fn(&[WatchTile]) -> Vec<WatchTile>>),
        ("reversed", Box::new(reversed)),
        ("filtered", Box::new(|r: &[WatchTile]| r.iter().step_by(2).cloned().collect())),
    ] {
        let start = if what == "two to four" { 2 } else { 14 };
        let mut h = Harness::new(tiles(start));
        h.focus_index(0);
        h.run(60);
        let next = make(h.screen.tiles());
        h.screen.input(WatchlistIn::Rows(next));
        play(&mut h, 400, |_, _| {});
        assert_eq!(sharing(&h), None, "{what}: two cards share a cell at rest");
        assert!(cards(&h).iter().all(|d| d.alpha > 0.999), "{what}: every card is whole at rest");
    }
}

/// **A card that arrives does not share a cell with a card at rest.** Two titles become four, the
/// two new ones in front: the old two slide from cells 0 and 1 to 2 and 3, and the new two fade in
/// at 0 and 1, under the movers: no two cards share a cell but a fading one with the cards over it.
#[test]
fn no_cards_share_a_cell_in_flight_but_one_fading_in_under_the_movers() {
    let mut h = Harness::new(tiles(2));
    h.focus_index(0);
    h.run(60);
    let mut four: Vec<WatchTile> = vec![tile(100), tile(101)];
    four.extend(h.screen.tiles().iter().cloned());
    h.screen.input(WatchlistIn::Rows(four));
    let mut released = None;
    play(&mut h, 300, |h, f| {
        if let Some((a, b)) = sharing(h) {
            panic!("frame {f}: two cards share a cell: {a} | {b}");
        }
        let new = cards(h).into_iter().filter(|d| d.guid.ends_with("g100") || d.guid.ends_with("g101")).collect::<Vec<_>>();
        if released.is_none() && new.iter().any(|d| d.alpha > 0.01) {
            released = Some(f);
        }
    });
    assert!(released.is_some(), "the new cards do arrive");
    assert!(cards(&h).iter().all(|d| d.alpha > 0.999), "and are whole at rest");
}

/// Two titles become four, the new two in front: the survivors slide two cells right under the new
/// cards' cells. Returns the harness with the page fed, before its first tick.
fn two_become_four() -> Harness {
    let mut h = Harness::new(tiles(2));
    h.focus_index(0);
    h.run(60);
    let mut four: Vec<WatchTile> = vec![tile(100), tile(101)];
    four.extend(h.screen.tiles().iter().cloned());
    h.screen.input(WatchlistIn::Rows(four));
    h
}

fn is_new(d: &Drawn) -> bool {
    d.guid.ends_with("g100") || d.guid.ends_with("g101")
}

/// The share of `cell` the `movers` cover (they do not overlap one another here).
fn covered(cell: Rect, movers: &[Rect]) -> f32 {
    movers.iter().map(|m| overlap(cell, *m)).sum::<f32>() / (cell.w * cell.h)
}

/// **A new card starts to appear once its cell is half clear.** It waits at alpha 0 while the cards
/// in flight cover half its cell or more, and its fade begins on the first frame they cover less,
/// not when the last of them has gone (a large re-toggle showed a dozen frames of empty cells).
#[test]
fn a_new_card_starts_to_fade_in_once_its_cell_is_half_clear() {
    let mut h = two_become_four();
    let (mut began, mut half_clear): ([Option<u32>; 2], [Option<u32>; 2]) = ([None; 2], [None; 2]);
    play(&mut h, 300, |h, f| {
        let probe = cards(h);
        let movers: Vec<Rect> = probe.iter().filter(|d| matches!(d.role, Some(DrawRole::Moving | DrawRole::Focus)) && !is_new(d)).map(|d| d.rect).collect();
        for d in probe.iter().filter(|d| is_new(d)) {
            let k = usize::from(d.guid.ends_with("g101"));
            if half_clear[k].is_none() && covered(d.rect, &movers) < 0.5 {
                half_clear[k] = Some(f);
            }
            if began[k].is_none() && d.alpha > 0.0 {
                began[k] = Some(f);
            }
        }
    });
    assert!(half_clear.iter().all(|f| f.is_some_and(|f| f > 0)), "premise: the cells start covered and clear later: {half_clear:?}");
    assert_eq!(began, half_clear, "each new card begins to fade on the first frame its cell is under half covered");
}

/// **A fading-in card is under every card in flight.** Back to front: ghosts, leaving copies, still
/// cards, the cards fading in, the movers (longest path last), the focused card; the new card that is
/// partly covered is drawn before the card that covers it.
#[test]
fn a_card_fading_in_is_drawn_under_the_cards_in_flight() {
    let mut h = two_become_four();
    let mut stacked = false;
    play(&mut h, 300, |h, f| {
        let probe = h.screen.drawn_probe(h.focus.map(|k| k.elem));
        let at = |pred: &dyn Fn(&Drawn) -> bool| probe.iter().position(pred);
        for (i, new) in probe.iter().enumerate().filter(|(_, d)| is_new(d) && d.alpha > 0.0 && d.alpha < 1.0) {
            assert_eq!(new.role, Some(DrawRole::Arriving), "frame {f}: a fading card is drawn as arriving: {new:?}");
            for (j, over) in probe.iter().enumerate().filter(|(_, d)| matches!(d.role, Some(DrawRole::Moving | DrawRole::Focus))) {
                if overlap(new.rect, over.rect) > 0.0 {
                    assert!(i < j, "frame {f}: {} fading in is drawn over {} in flight", new.guid, over.guid);
                    stacked = true;
                }
            }
        }
        let last_new = probe.iter().rposition(|d| d.role == Some(DrawRole::Arriving));
        let first_move = at(&|d| matches!(d.role, Some(DrawRole::Moving)));
        if let (Some(a), Some(m)) = (last_new, first_move) {
            assert!(a < m, "frame {f}: an arriving card is drawn after a mover");
        }
        let still = probe.iter().rposition(|d| d.role == Some(DrawRole::Still));
        let first_arriving = at(&|d| d.role == Some(DrawRole::Arriving));
        if let (Some(s), Some(a)) = (still, first_arriving) {
            assert!(s < a, "frame {f}: a still card is drawn after an arriving one");
        }
    });
    assert!(stacked, "premise: a card fading in was partly under a card in flight");
}

/// **Back to front: ghosts, still cards, moving survivors (the longest path last), the focused
/// card.** The movers are stacked by how far each had to go, and the stacking does not change
/// mid-flight.
#[test]
fn the_draw_order_is_ghost_still_moving_by_path_length_focus() {
    let mut h = Harness::new(tiles(12));
    h.focus_index(0);
    h.run(60);
    let mut rows = h.screen.tiles().to_vec();
    // a swap that crosses, and a card removed so there is a ghost
    rows.swap(1, 4);
    rows.remove(8);
    h.screen.input(WatchlistIn::Rows(rows));
    let mut start: std::collections::HashMap<String, Rect> = Default::default();
    let mut orders: Vec<(u32, Vec<String>)> = Vec::new();
    play(&mut h, 40, |h, f| {
        let probe = h.screen.drawn_probe(h.focus.map(|k| k.elem));
        let roles: Vec<Option<DrawRole>> = probe.iter().map(|d| d.role).collect();
        let rank = |r: &Option<DrawRole>| match r {
            None => 0,
            Some(DrawRole::Leaving) => 1,
            Some(DrawRole::Still) => 2,
            Some(DrawRole::Arriving) => 3,
            Some(DrawRole::Moving) => 4,
            Some(DrawRole::Focus) => 5,
        };
        assert!(roles.windows(2).all(|w| rank(&w[0]) <= rank(&w[1])), "frame {f}: roles out of order: {roles:?}");
        assert_eq!(roles.last(), Some(&Some(DrawRole::Focus)), "frame {f}: the focused card is drawn last: {roles:?}");
        let moving: Vec<&Drawn> = probe.iter().filter(|d| d.role == Some(DrawRole::Moving)).collect();
        if f == 0 {
            start = moving.iter().map(|d| (d.guid.clone(), d.rect)).collect();
        }
        orders.push((f, moving.iter().map(|d| d.guid.clone()).collect()));
    });
    assert!(start.len() >= 2, "premise: at least two cards were moving at once ({} did)", start.len());
    let end: std::collections::HashMap<String, Rect> =
        h.screen.drawn_probe(None).into_iter().filter(|d| d.role.is_some()).map(|d| (d.guid, d.rect)).collect();
    let travel = |g: &String| {
        let (a, b) = (start[g], end[g]);
        (a.x - b.x).hypot(a.y - b.y)
    };
    for (f, order) in &orders {
        let known: Vec<&String> = order.iter().filter(|g| start.contains_key(*g)).collect();
        assert!(known.windows(2).all(|w| travel(w[0]) <= travel(w[1]) + 0.01),
            "frame {f}: the movers are not stacked by path length (longest last): {:?}",
            known.iter().map(|g| (g.as_str(), travel(g))).collect::<Vec<_>>());
    }
    let first = &orders[0].1;
    for (f, order) in &orders {
        let now: Vec<&String> = order.iter().filter(|g| first.contains(*g)).collect();
        let was: Vec<&String> = first.iter().filter(|g| order.contains(*g)).collect();
        assert_eq!(now, was, "frame {f}: the stacking of the movers changed mid-flight");
    }
}

/// **The mover with the longest path draws on top of the others.** A swap of cards 0 and 5 makes
/// their paths cross the four between; each of the four travels one cell, the swapped two five.
#[test]
fn the_longest_path_draws_last_among_the_movers() {
    let mut h = Harness::new(tiles(6));
    h.focus_index(2);
    h.run(60);
    let mut rows = h.screen.tiles().to_vec();
    rows.swap(0, 5);
    let swapped = [rows[0].row.guid.clone(), rows[5].row.guid.clone()];
    h.screen.input(WatchlistIn::Rows(rows));
    let mut checked = false;
    play(&mut h, 30, |h, f| {
        let probe = h.screen.drawn_probe(h.focus.map(|k| k.elem));
        let moving: Vec<&Drawn> = probe.iter().filter(|d| d.role == Some(DrawRole::Moving)).collect();
        if moving.len() == 2 {
            let last_two: Vec<&String> = moving.iter().map(|d| &d.guid).collect();
            assert!(last_two.iter().all(|g| swapped.contains(g)), "frame {f}: the two with the longest paths are drawn last: {last_two:?}");
            checked = true;
        }
    });
    assert!(checked, "premise: a frame with exactly the two long movers (the focused card keeps its place)");
}

/// **The back-to-front order is what the grid really asks for.** The posters the grid asks for in a
/// frame arrive ghosts first, and the focused card's last.
#[test]
fn the_grid_asks_for_ghosts_first_and_the_focused_card_last() {
    let mut h = Harness::new(tiles(12));
    h.focus_index(3);
    h.run(60);
    let gone = h.screen.tiles()[6].row.guid.clone();
    h.screen.input(WatchlistIn::Removed(gone));
    h.run(3);
    h.screen.take_art_log();
    h.draw();
    let log = h.screen.take_art_log();
    assert!(log.iter().any(|(ghost, _)| *ghost), "premise: a ghost was asked for");
    let first_card = log.iter().position(|(ghost, _)| !ghost).expect("cards were asked for");
    assert!(log[first_card..].iter().all(|(ghost, _)| !ghost), "a ghost was asked for after a card: {log:?}");
    let focus = h.screen.index_of(h.focus.unwrap().elem).unwrap();
    assert_eq!(log.last(), Some(&(false, focus)), "the focused card is the last poster asked for: {log:?}");
}

/// **A card that keeps its row moves; a card that changes row crossfades.** A reorder follows the
/// same rule as a filter.
#[test]
fn a_reorder_moves_within_a_row_and_crossfades_across_rows() {
    let mut h = Harness::new(tiles(14));
    h.run(60);
    let rest: std::collections::HashMap<String, Rect> = h.screen.drawn_probe(None).into_iter().map(|d| (d.guid, d.rect)).collect();
    h.screen.input(WatchlistIn::Rows(reversed(h.screen.tiles())));
    h.run(1);
    let probe = h.screen.drawn_probe(h.focus.map(|k| k.elem));
    let mut moved = 0;
    let mut crossed = 0;
    for d in probe.iter().filter(|d| d.role == Some(DrawRole::Moving)) {
        assert!((rest[&d.guid].y - d.rect.y).abs() < 0.5, "{} is moving but changes rows", d.guid);
        moved += 1;
    }
    for d in probe.iter().filter(|d| d.role == Some(DrawRole::Leaving)) {
        assert!((rest[&d.guid].y - d.rect.y).abs() < 0.5, "a leaving copy stands where the card stood");
        crossed += 1;
    }
    assert!(moved > 0 && crossed > 0, "premise: a reversal of three rows has both kinds ({moved} moving, {crossed} crossfading)");
}

/// Where each card stands on screen, by guid: the most opaque thing drawn for it (a card that
/// crossfades is its leaving copy until the new one is the stronger).
fn places(h: &Harness) -> std::collections::BTreeMap<String, (f32, f32)> {
    let mut best: std::collections::BTreeMap<String, (f32, (f32, f32))> = Default::default();
    for d in h.screen.drawn_probe(h.focus.map(|k| k.elem)) {
        let e = best.entry(d.guid).or_insert((-1.0, (0.0, 0.0)));
        if d.alpha > e.0 {
            *e = (d.alpha, (d.rect.x, d.rect.y));
        }
    }
    best.into_iter().map(|(g, (_, at))| (g, at)).collect()
}

/// The page's own scroll spring may take its first step on the frame a landing begins in, which
/// moves every card the same way. What this asks is that the LAYOUT did not: each card moved by one
/// shared `(0, dy)`, `dy` a spring step, never a row's worth.
fn only_the_scroll_moved(before: &std::collections::BTreeMap<String, (f32, f32)>, after: &std::collections::BTreeMap<String, (f32, f32)>) -> Result<f32, String> {
    let mut dy = None;
    for (g, b) in before {
        let Some(a) = after.get(g) else { return Err(format!("{g} is not drawn after")) };
        if (a.0 - b.0).abs() > 0.02 {
            return Err(format!("{g} moved sideways: {b:?} -> {a:?}"));
        }
        let d = a.1 - b.1;
        match dy {
            None => dy = Some(d),
            Some(first) if (d - first).abs() > 0.02 => return Err(format!("{g} moved {d}px where another moved {first}px: {b:?} -> {a:?}")),
            _ => {}
        }
    }
    Ok(dy.unwrap_or(0.0))
}

/// The filter the caption tests share: the focused card (index 7, the second row) moves to index 4,
/// the first row, so its caption band leaves row 1 and opens on row 0. Returns the harness with the
/// page fed, before its first tick.
fn focus_changes_row() -> (Harness, std::collections::BTreeMap<String, (f32, f32)>) {
    let mut h = Harness::new(tiles(14));
    h.run(10);
    h.focus_index(7);
    h.run(90);
    h.reconcile();
    let before = places(&h);
    let kept: Vec<WatchTile> = h.screen.tiles().iter().skip(3).cloned().collect();
    h.screen.input(WatchlistIn::Rows(kept));
    (h, before)
}

/// **The caption band that follows the focus moves with the cards, not on the click.** The focused
/// card changes row, so its band closes on one row and opens on another; the cards between measured
/// their starting cell under the band the page had settled on, not the one it had drawn, so the
/// frame a landing begins in was not the frame before it.
#[test]
fn the_first_frame_of_a_filter_that_moves_the_focus_row_is_the_frame_before_it() {
    let (mut h, before) = focus_changes_row();
    h.run(1);
    if let Err(why) = only_the_scroll_moved(&before, &places(&h)) {
        panic!("the band that follows the focus moved on the click: {why}");
    }
}

/// **The header count moves with the grid.** A Sort & Filter change reaches the page as its options and
/// its rows on one frame; the count beside the pill keeps the old number until the cards are moving
/// (as a Remove's does), and shows the new one on that frame, not on the click.
#[test]
fn a_filter_changes_the_count_when_the_cards_start_to_move() {
    let mut h = Harness::new(tiles(14));
    h.run(60);
    assert_eq!(h.screen.count(), Some(Count::All(14)), "premise");
    let start = visible(&h);
    let kept: Vec<WatchTile> = h.screen.tiles().iter().take(9).cloned().collect();
    h.screen.input(WatchlistIn::Options(WatchlistOptionsState { type_: TypeFilter::Movies, ..Default::default() }));
    h.screen.input(WatchlistIn::Rows(kept));
    let mut changed = false;
    play(&mut h, 90, |h, f| {
        let moved = changed || visible(h) != start;
        changed = moved;
        let shows = h.screen.count();
        let want = if moved { Count::Of { shown: 9, total: 14 } } else { Count::All(14) };
        assert_eq!(shows, Some(want), "frame {f}: the count follows the cards (moving: {moved})");
    });
    assert!(changed, "premise: the cards moved");
}
