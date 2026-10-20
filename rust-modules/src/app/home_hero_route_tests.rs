//! **Home's billboard and its shelves: what the page DRAWS while focus crosses the fold** (the
//! owner's two reports of 2026-10-19, `HOME-BACK`).
//!
//! 1. *"On home when I scroll to the bottom, press BACK and then from hero down, the animation
//!    briefly jumps to where I was before BACK and then to the first shelf."* Home rested the page
//!    by the PICTURE's focus (the billboard stays "focused" in the picture until the dive is half
//!    way), so after the fold the deep scroll target outlived the BACK and the first Down drew the
//!    deep shelves for a few frames. The rest comes from the ENGINE's focus now, through the same
//!    shared rule the Library rests by (`ui::cards::grid_rest_scroll`): a billboard focus rests the
//!    page at the first shelf.
//! 2. *"When we go to hero, all shelves are scrolled to the beginning and when we scroll down they
//!    scroll back."* Home drew every shelf at its offset times the dive (`CardSource::sweep`), so a
//!    shelf slid to its first card as focus reached the billboard and slid back on Down. A shelf
//!    draws at its real offset everywhere, and keeps its place.
//!
//! Key-driven on a real `Dispatcher<AppHost>` (`bridge_test_support::World`): what a person sees
//! is what the page DREW (`World::drawn`) and where `place(.., At::Drawn)` puts a card, every frame.

use super::*;
use plx_ui::screen::Focusable;
use super::test_support::World;
use super::home_chrome_route_tests::{drawn, headings, home_hero_of, home_shelves_of, to_shelf, Heading, TITLES};

/// The dive's frames: the Down key's own frame, then this many more (the dive and the lift settle in
/// well under this).
const DIVE_FRAMES: u32 = 45;

/// Press `key` and keep every frame it takes `frames` frames to play: the headings each drew.
fn watch_headings(w: &mut World, key: Key, frames: u32) -> Vec<Vec<Heading>> {
    w.tap(key, 0);
    let mut seen = vec![headings(w)];
    for _ in 0..frames {
        w.step(vec![]);
        seen.push(headings(w));
    }
    seen
}

/// The y shelf `title`'s heading is drawn at in a frame, if it is drawn at all.
fn y_of(frame: &[Heading], title: &str) -> Option<f32> {
    frame.iter().find(|h| h.title == title && drawn(h)).map(|h| h.rect.y)
}

/// The heading of any shelf from Charlie down that this frame draws on the screen, with its y: the
/// first Down from the billboard may show the first two shelves and no others.
fn deep_headings(frame: &[Heading]) -> Vec<(&'static str, f32)> {
    frame.iter()
        .filter(|h| drawn(h) && h.rect.y > 0.0 && h.rect.y < plx_ui::consts::SCR_H)
        .filter(|h| TITLES.iter().position(|t| *t == h.title).is_some_and(|i| i >= 2))
        .map(|h| (h.title, h.rect.y))
        .collect()
}

/// A dive from the billboard shows the first shelf and its neighbour and no deeper shelf, and the
/// first shelf's heading only ever rises to its rest. A failure prints the headings every frame drew.
fn assert_dives_to_the_first_shelf_only(what: &str, frames: &[Vec<Heading>]) {
    let mut problems: Vec<String> = Vec::new();
    for (n, frame) in frames.iter().enumerate() {
        let deep = deep_headings(frame);
        if !deep.is_empty() {
            problems.push(format!("{what}, frame {n}: the dive draws deeper shelves {deep:?}"));
        }
    }
    let ys: Vec<(usize, f32)> = frames.iter().enumerate().filter_map(|(n, f)| y_of(f, TITLES[0]).map(|y| (n, y))).collect();
    if ys.len() <= 10 {
        problems.push(format!("{what}: the first shelf's heading is drawn in only {} frames of the dive", ys.len()));
    }
    for pair in ys.windows(2) {
        if pair[1].1 > pair[0].1 + 0.5 {
            problems.push(format!("{what}: the first shelf's heading goes DOWN from y {:.1} (frame {}) to {:.1} (frame {})", pair[0].1, pair[0].0, pair[1].1, pair[1].0));
        }
    }
    if !problems.is_empty() {
        let table: Vec<String> = frames.iter().enumerate()
            .map(|(n, f)| format!("  {n:>2}: {:?}", f.iter().filter(|h| drawn(h)).map(|h| (h.title.trim_start_matches("Shelf "), h.rect.y.round())).collect::<Vec<_>>()))
            .collect();
        panic!("{}\nthe headings each frame drew (title, y):\n{}", problems.join("\n"), table.join("\n"));
    }
}

/// **The control: the first Down of a fresh boot dives to the first shelf and shows no deeper one.**
#[test]
fn down_from_the_hero_on_a_fresh_boot_dives_to_the_first_shelf_only() {
    let mut w = home_hero_of("hh-fresh", 3);
    let frames = watch_headings(&mut w, Key::Down, DIVE_FRAMES);
    assert_dives_to_the_first_shelf_only("fresh boot", &frames);
}

/// **BACK from the last shelf folds to the billboard, and the next Down dives to the FIRST shelf
/// only.** The page was resting the last shelf (a scroll of some 4000 px); the fold used to leave that
/// target standing under the hidden shelves, and the first Down drew shelves 4-6 for seven frames
/// before the page travelled back up.
#[test]
fn down_from_the_hero_after_back_from_the_last_shelf_dives_to_the_first_shelf_only() {
    let mut w = home_shelves_of("hh-back", 3);
    to_shelf(&mut w, TITLES.len() - 1);
    let deep = headings(&mut w);
    assert!(y_of(&deep, TITLES[5]).is_some() && y_of(&deep, TITLES[0]).is_none(),
        "premise: the page rests on the last shelf (Alpha off, Foxtrot on): {:?}", deep.iter().map(|h| (h.title, h.rect.y, h.alpha)).collect::<Vec<_>>());
    w.press(Key::Back);
    w.frames(60);
    assert!(on_hero(&w), "premise: BACK folded focus to the billboard (grid {:?}, snap target {}, snap {}, focus {:?})", grid_at(&w), w.bridge.home_snap_target(&w.d), w.bridge.home_snap_pos(&w.d), w.focus());
    let frames = watch_headings(&mut w, Key::Down, DIVE_FRAMES);
    assert_dives_to_the_first_shelf_only("after BACK from the last shelf", &frames);
}

/// **The control: Up out of the first shelf to the billboard, from a page that had been resting deep,
/// and the next Down dives to the first shelf only.** It passes on the old code by design: the walk
/// down and back up leaves the page resting at the first shelf, so no deep target outlives the Up (only
/// the BACK fold, above, left one standing).
#[test]
fn down_from_the_hero_after_up_from_a_deep_walk_dives_to_the_first_shelf_only() {
    let mut w = home_shelves_of("hh-up", 3);
    to_shelf(&mut w, TITLES.len() - 1);
    for _ in 0..TITLES.len() - 1 {
        w.press(Key::Up);
    }
    w.press(Key::Up);
    assert!(on_hero(&w), "premise: Up from the first shelf reached the billboard");
    w.frames(30);
    let frames = watch_headings(&mut w, Key::Down, DIVE_FRAMES);
    assert_dives_to_the_first_shelf_only("after Up from a deep walk", &frames);
}

/// The x the card at `(row, col)` is DRAWN at (its centre: the focus pop grows it about that), if the
/// page places it.
fn card_cx(w: &World, row: usize, col: usize) -> f32 {
    w.bridge.with_home(&w.d, |home, cx, _| {
        let key = home.dev_focus_key::<AppHost>(row, col, cx).expect("the shelf holds the card");
        Focusable::<AppHost>::place(home, &key.elem, cx, At::Drawn).expect("the page places the card").rect.cx()
    }).expect("Home is on top")
}

/// Is focus on the billboard (not on a shelf, a heading or the top bar), and the dive headed there?
fn on_hero(w: &World) -> bool {
    grid_at(w).is_none() && w.bridge.home_snap_target(&w.d) == 0.0
}

/// Where focus stands on the shelves, `(shelf, card)`.
fn grid_at(w: &World) -> Option<(usize, usize)> {
    w.bridge.with_home(&w.d, |home, cx, focus| home.grid_position::<AppHost>(focus, cx)).flatten()
}

/// The first shelf walked Right to its eighth card: the shelf holds an offset, and its third card
/// stands where the first one would at rest. Returns that card's drawn centre x.
fn walked_first_shelf(name: &str) -> (World, f32) {
    let mut w = home_shelves_of(name, 12);
    for _ in 0..7 {
        w.press(Key::Right);
    }
    assert_eq!(grid_at(&w), Some((0, 7)), "premise: the walk Right reached the eighth card");
    // the scroll spring's tail is long: grade against the offset it LANDS at, not a few pixels short of it
    w.frames(150);
    let rest = card_cx(&w, 0, 2);
    let first = card_cx(&w, 0, 0);
    assert!(first < rest - 400.0, "premise: the shelf holds an offset, so its first card is off to the left of the third ({first} vs {rest})");
    (w, rest)
}

/// Play `key` and `frames` more, grading every frame: the third card of the first shelf is drawn
/// within half a pixel of where it rested. Says what drifted, if anything did.
fn third_card_drift(w: &mut World, key: Key, frames: u32, rest: f32, what: &str) -> Option<String> {
    let mut worst = (0.0f32, 0);
    w.tap(key, 0);
    for n in 0..=frames {
        let off = (card_cx(w, 0, 2) - rest).abs();
        if off > worst.0 {
            worst = (off, n);
        }
        w.step(vec![]);
    }
    (worst.0 > 0.5).then(|| format!("{what}: the first shelf's third card was drawn {:.1} px from where it rested (frame {} of {frames})", worst.0, worst.1))
}

/// **A shelf keeps its drawn offset when focus reaches the billboard by Up**, and keeps it on the
/// way back down: Down lands on the card the door rule names (the third, the one under the Play
/// button), which stands where it did.
#[test]
fn a_shelf_keeps_its_drawn_offset_when_focus_reaches_the_hero_by_up() {
    let (mut w, rest) = walked_first_shelf("hh-offset-up");
    let mut problems: Vec<String> = Vec::new();
    problems.extend(third_card_drift(&mut w, Key::Up, 40, rest, "Up to the billboard"));
    assert!(on_hero(&w), "premise: Up reached the billboard");
    problems.extend(third_card_drift(&mut w, Key::Down, DIVE_FRAMES, rest, "Down from the billboard"));
    if grid_at(&w) != Some((0, 2)) {
        problems.push(format!("Down from the billboard landed on {:?}, not the third card, where it stood", grid_at(&w)));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// **And by BACK from a deeper shelf**: the first shelf holds the offset it had been left with
/// through the fold, and again through the dive.
#[test]
fn a_shelf_keeps_its_drawn_offset_when_focus_reaches_the_hero_by_back() {
    let (mut w, rest) = walked_first_shelf("hh-offset-back");
    w.press(Key::Down);
    w.frames(150);
    assert_eq!(grid_at(&w).map(|(row, _)| row), Some(1), "premise: Down reached the second shelf");
    assert!((card_cx(&w, 0, 2) - rest).abs() <= 0.5, "premise: leaving the first shelf left its offset where it was");
    let mut problems: Vec<String> = Vec::new();
    problems.extend(third_card_drift(&mut w, Key::Back, 60, rest, "BACK to the billboard"));
    assert!(on_hero(&w), "premise: BACK reached the billboard");
    problems.extend(third_card_drift(&mut w, Key::Down, DIVE_FRAMES, rest, "Down from the billboard"));
    if grid_at(&w) != Some((0, 2)) {
        problems.push(format!("Down from the billboard landed on {:?}, not the third card, where it stood", grid_at(&w)));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
