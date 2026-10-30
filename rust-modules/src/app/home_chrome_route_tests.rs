//! **Home's shelf headings and the shared top chrome** (FRAMES-8 item 5): the account chip and the
//! nav pills are drawn over the page, and a heading that stands in their band reads as torn by them.
//! On the Under posters settings that open the focused row's caption band LESS (80 px, or none) the
//! heading of the shelf above the focused one rested at y 61 and 101, under the chip, because Home
//! rested a focused row by the caption band it opens (`row_reveal_band`'s `lo`).
//!
//! Key-driven on a real `Dispatcher<AppHost>` (`bridge_test_support::World`): what a person reads is
//! what the page DREW (`World::drawn`, the text runs with the alpha the cascade left on each).

use super::*;
use super::test_support::{Drawn, World};
use super::under_posters_shelf_route_tests::{describe_cards, set, Cell};
use plx_plex::plex::session::UnderPostersDetails as Details;
use plx_ui::widgets::{CHIP_FRAME, TAB_TRACK_PAD, TOP_BAR_BOTTOM, TOP_BAR_Y};
use plx_ui::Rect;

pub(super) const TITLES: [&str; 6] = ["Shelf Alpha", "Shelf Bravo", "Shelf Charlie", "Shelf Delta", "Shelf Echo", "Shelf Foxtrot"];

/// The three band heights the Under posters setting opens under the focused poster:
/// `(name, title on, details, band)`.
const SETTINGS: [(&str, bool, Details, f32); 3] = [
    ("112 (title and a detail line)", true, Details::Genre, 112.0),
    ("80 (one line)", false, Details::YearLength, 80.0),
    ("0 (nothing)", false, Details::None, 0.0),
];

/// Home over six named shelves of three posters, the hero's Down already taken: focus is on the first
/// poster of the first shelf.
fn home_shelves(name: &str) -> World {
    home_shelves_of(name, 3)
}

/// [`home_shelves`] with `cols` posters on every shelf (rating keys `1..=cols`, the same on every
/// shelf), for a walk along a shelf that needs room to scroll.
pub(super) fn home_shelves_of(name: &str, cols: usize) -> World {
    let mut w = home_hero_of(name, cols);
    w.press(Key::Down);
    w
}

/// [`home_shelves_of`] before the hero's Down: the six shelves are published and settled, focus is
/// still on the billboard.
pub(super) fn home_hero_of(name: &str, cols: usize) -> World {
    let mut w = World::home_with_disc(name, [7; 12]);
    let ids: Vec<String> = (0..TITLES.len()).map(|i| format!("test.row.{i}")).collect();
    let rows: Vec<(&str, &str, &str)> = ids.iter().zip(TITLES).map(|(id, title)| (id.as_str(), "", title)).collect();
    let adapter = w.bridge.stores.hubs.adapter_for_test();
    plx_data::pms::seed_named_hubs_for_test(w.bridge.stores.hubs.state_mut(), &adapter, cols, &rows);
    w.frames(30);
    w
}

/// The band of the chrome the page is drawn under: the tab track's full width, from its top edge to
/// the bottom of the pills' band.
fn chrome_band() -> Rect {
    Rect::new(0.0, TOP_BAR_Y - TAB_TRACK_PAD, plx_ui::consts::SCR_W, TOP_BAR_BOTTOM - (TOP_BAR_Y - TAB_TRACK_PAD))
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// One drawn shelf heading: its title, the box its line stands in (the line is a `HEADLINE` tall),
/// and the alpha it was drawn at.
pub(super) struct Heading {
    pub(super) title: &'static str,
    pub(super) rect: Rect,
    pub(super) alpha: f32,
}

pub(super) fn headings(w: &mut World) -> Vec<Heading> {
    headings_in(&w.drawn())
}

/// [`headings`] over a frame already drawn, so one draw can be graded for its headings and its caption.
fn headings_in(drawn: &Drawn) -> Vec<Heading> {
    let line = plx_ui::theme::size::HEADLINE as f32;
    drawn.runs.iter().filter_map(|run| {
        let title = *TITLES.iter().find(|title| **title == run.text)?;
        Some(Heading { title, rect: Rect { h: run.at.h.max(line), ..run.at }, alpha: run.ink[3] })
    }).collect()
}

/// A heading is DRAWN once it has any ink worth the name: the fade takes it to nothing, and a run at
/// alpha 0 paints no pixel.
pub(super) fn drawn(h: &Heading) -> bool {
    h.alpha > 0.02
}

/// Focus on the first poster of shelf `n` (0-based), the way a person gets there: Down, a row at a time.
pub(super) fn to_shelf(w: &mut World, n: usize) {
    for _ in 0..n {
        w.press(Key::Down);
    }
}

#[test]
fn a_resting_heading_is_always_clear_of_the_chrome_and_rests_where_it_did_whatever_the_band() {
    let band = chrome_band();
    let mut problems: Vec<String> = Vec::new();
    let mut rested: Vec<(usize, &str, f32)> = Vec::new();
    for shelf in [1usize, 2] {
        for (setting, title, details, _) in SETTINGS {
            let mut w = home_shelves(&format!("hc-rest-{shelf}-{}", setting.split(' ').next().unwrap_or("")));
            let _cell = Cell::default();
            set(&mut w, title, details);
            to_shelf(&mut w, shelf);
            w.frames(40);
            let all = headings(&mut w);
            for h in all.iter().filter(|h| drawn(h)) {
                if overlaps(h.rect, CHIP_FRAME) {
                    problems.push(format!("{setting}, focus on shelf {shelf}: the heading {:?} at y {:.1} stands under the account chip", h.title, h.rect.y));
                } else if overlaps(h.rect, band) {
                    problems.push(format!("{setting}, focus on shelf {shelf}: the heading {:?} at y {:.1} stands in the chrome's band", h.title, h.rect.y));
                }
            }
            match all.iter().find(|h| h.title == TITLES[shelf] && drawn(h)) {
                Some(focused) => rested.push((shelf, setting, focused.rect.y)),
                None => problems.push(format!("{setting}, focus on shelf {shelf}: premise: the focused shelf's heading is not drawn")),
            }
        }
    }
    for shelf in [1usize, 2] {
        let ys: Vec<f32> = rested.iter().filter(|r| r.0 == shelf).map(|r| r.2).collect();
        if !ys.windows(2).all(|pair| (pair[0] - pair[1]).abs() < 0.5) {
            problems.push(format!("focus on shelf {shelf}: the focused heading rests at one y whatever the Under posters band, got {ys:?}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn no_heading_is_drawn_over_the_chip_while_the_page_scrolls() {
    // A heading fades by where it is DRAWN (the lift included), so no ink of it stands over the
    // chip's rows: over the chip it is gone, whatever the scroll is doing.
    for (setting, title, details, _) in SETTINGS {
        let mut w = home_shelves(&format!("hc-scroll-{}", setting.split(' ').next().unwrap_or("")));
        let _cell = Cell::default();
        set(&mut w, title, details);
        let mut crossed = 0;
        for press in 0..4 {
            w.tap(Key::Down, 0);
            for frame in 0..30 {
                w.step(vec![]);
                for h in headings(&mut w) {
                    if overlaps(h.rect, CHIP_FRAME) {
                        crossed += 1;
                        assert!(h.alpha <= 0.02,
                            "{setting}, Down {press} frame {frame}: the heading {:?} at {:?} is drawn at alpha {} over the chip (none allowed)", h.title, h.rect, h.alpha);
                    }
                }
            }
        }
        assert!(crossed > 0, "{setting}: premise: a heading crosses the chip's rows while the page scrolls");
    }
}


/// The y a shelf's heading is drawn at, if it is drawn at all.
fn heading_y(w: &mut World, shelf: usize) -> Option<f32> {
    headings(w).iter().find(|h| h.title == TITLES[shelf] && drawn(h)).map(|h| h.rect.y)
}

/// Present Home's pointer stops as its draw registers them, the way a frame's hit map is presented
/// (a host has no GL to run the page's full draw through the dispatcher).
fn present_home_stops(w: &mut World) {
    let stops = w.bridge.with_home(&w.d, |home, cx, _| {
        let mut draw = plx_ui::screen::DrawFrame::new(cx, plx_ui::Painter::root());
        home.record_stops::<AppHost>(&mut draw, cx.views.hubs);
        draw.into_stops()
    }).expect("Home is the top page");
    w.d.input.hit.fill(stops);
    w.d.input.hit.swap();
}

/// **A Magic Remote user can reach the shelves either side of the one the page rests on.** The page
/// rests the focused shelf at one fixed line, so the shelf above it stands off the top and the one
/// below it stands cut by the bottom margin. The wheel is the direction key it stands for
/// (`bridge::wheel_input`) and moves focus a shelf at a time; the pointer takes focus onto the
/// poster of the shelf below, which is in view, without moving the page (the pointer scrolls by the
/// screen's edges, `input::EdgeScroll`), and a click brings that shelf to the rest line.
#[test]
fn a_pointer_user_reaches_the_shelves_either_side_of_a_resting_shelf() {
    for (setting, title, details, _) in SETTINGS {
        let name = setting.split(' ').next().unwrap_or("");
        let mut w = home_shelves(&format!("hc-reach-{name}"));
        let _cell = Cell::default();
        set(&mut w, title, details);
        to_shelf(&mut w, 2);
        w.frames(150);
        let rest = heading_y(&mut w, 2).unwrap_or_else(|| panic!("{setting}: premise: shelf 3's heading is drawn"));
        // the wheel, one tick towards you, is Up: shelf 2 comes to the rest line
        let at = super::test_support::tick(w.frame + 1);
        w.step(super::wheel_input(1, at));
        w.frames(150);
        let up = heading_y(&mut w, 1).unwrap_or_else(|| panic!("{setting}: the wheel's Up brought no shelf 2 heading"));
        assert!((up - rest).abs() < 1.0, "{setting}: shelf 2 rests at {up}, the rest line is {rest}");
        // and one tick away from you is Down, back to shelf 3
        let at = super::test_support::tick(w.frame + 1);
        w.step(super::wheel_input(-1, at));
        w.frames(150);
        assert!((heading_y(&mut w, 2).unwrap_or(f32::NAN) - rest).abs() < 1.0, "{setting}: the wheel's Down is back on shelf 3 (rest {rest}, now {:?}, focus {:?})", heading_y(&mut w, 2), w.focus());
        // the pointer: a poster of shelf 4 (below shelf 3, cut by the bottom margin) takes focus
        present_home_stops(&mut w);
        let focused = w.focus().expect("a poster holds focus");
        let (focus_y, hit_map) = {
            let stops = w.d.input.hit.front();
            let own = stops.iter().find(|stop| stop.key == focused).unwrap_or_else(|| panic!("the focused poster {focused:?} is a stop of {:?}", stops.iter().map(|s| (s.key.entry.0, s.key.elem)).collect::<Vec<_>>()));
            (own.rest_rect.y, stops.to_vec())
        };
        let below = hit_map.iter()
            .filter(|stop| stop.key.entry == focused.entry && stop.rest_rect.y > focus_y + 300.0 && stop.rest_rect.y < 1000.0)
            .min_by(|a, b| a.rest_rect.x.total_cmp(&b.rest_rect.x))
            .unwrap_or_else(|| panic!("{setting}: shelf 4 registers a poster in view: {:?}", hit_map.iter().map(|s| s.rest_rect).collect::<Vec<_>>()));
        assert_eq!(below.hover, plx_ui::screen::Hover::Focus, "{setting}: the shelf below a resting shelf takes the pointer");
        let (x, y) = (below.rect.cx(), below.rect.y + below.rect.h * 0.25);
        for (px, py) in [(x - 40.0, y + 30.0), (x, y)] {
            let at = super::test_support::tick(w.frame + 1);
            w.step(vec![super::pointer_input(px, py, at)]);
        }
        w.frames(150);
        assert_eq!(w.focus().map(|k| k.elem), Some(below.key.elem), "{setting}: pointing at a poster of shelf 4 takes focus");
        // (its heading only gives up the lift a focused shelf's heading wears, `HEADING_LIFT_MAX`)
        assert!((heading_y(&mut w, 2).unwrap_or(f32::NAN) - rest).abs() <= HEADING_LIFT_MAX,
            "{setting}: a hover moves focus, not the page: shelf 3 stays on the rest line {rest} ({:?})", heading_y(&mut w, 2));
        // a click on it brings shelf 4 to the rest line
        let at = super::test_support::tick(w.frame + 1);
        w.step(vec![super::click_input(x, y, at)]);
        w.frames(150);
        assert!((heading_y(&mut w, 3).unwrap_or(f32::NAN) - rest).abs() < 1.0,
            "{setting}: clicking a poster of shelf 4 brought it to the rest line ({:?})", heading_y(&mut w, 3));
    }
}

/// A genre wider than the whole panel's room, so the focused details line is a marquee at every
/// card of the shelf (`card_row`'s glide: the line and its follower inside a clip window, after a rest
/// beat). A shorter one would still glide under the first cards, but a block that reaches back left of
/// a card near the right edge (`anchor_label`) holds a line that fits the room whole, at rest.
const LONG_GENRE: &str = "Psychological Courtroom Neo-Noir Crime Thriller Saga Of Many Unusual Genres No Poster Can Hold, \
And No Gap Beside It Can Spare, Not Even With The Caption Reaching Back Across The Panel To Its Left Edge";

/// The most the focused shelf's heading is lifted by while focus is in the row's first slots
/// (`card_row::heading_clearance`), which it gives up as a walk leaves them: a pixel of slack over it.
const HEADING_LIFT_MAX: f32 = 18.0;

/// The focused caption's long line as the frame drew it: the left edge of its leftmost run, and how many
/// runs of it there were (the line and its follower, each with its shadow). `None`: the line was not
/// drawn. (A host draw is a backdrop-discovery walk, which keeps the marquee's clip window to itself, so
/// the line's place is read against another frame of the same tile, and its motion against
/// `plx_machine::idle`'s local damage, which a marquee past its rest beat claims.)
fn long_line(drawn: &Drawn) -> Option<(f32, usize)> {
    let runs: Vec<_> = drawn.runs.iter().filter(|run| run.label && run.text == LONG_GENRE).collect();
    (!runs.is_empty()).then(|| (runs.iter().map(|run| run.at.x).fold(f32::INFINITY, f32::min), runs.len()))
}

/// Draw the frame, and say whether a marquee is moving in it (past its rest beat).
fn draw_and_glides(w: &mut World) -> (Drawn, bool) {
    let _ = plx_machine::idle::take_local_damage();
    let frame = w.drawn();
    (frame, plx_machine::idle::take_local_damage() > 0)
}

/// **A held Right along a Home shelf glides the focused caption while the shelf headings stay clear
/// of the chip.** The two were only ever graded apart: the glide DIRECT (`card_row`'s marquee tests),
/// the headings on a page that scrolls vertically. Here a person holds Right along a shelf whose
/// cards all carry a details line too long to hold, with the page resting on the third shelf: at every
/// step of the walk the focused block is the setting's lines, the long line is a marquee that keeps
/// gliding through the walk (the block's text is the same on every card, so its clock runs on), no
/// heading is drawn over the chip, and the focused shelf's heading rests at one line whatever band the
/// setting opens, only coming down from it by the lift it gives up as the walk leaves the first slots.
#[test]
fn a_held_right_along_a_home_shelf_glides_the_caption_without_a_heading_over_the_chip() {
    // (name, Title switch, Details picker): a two-line band, a one-line band, and no band at all.
    const WALKS: [(&str, bool, Details); 3] = [
        ("title-and-genre", true, Details::Genre),
        ("genre-alone", false, Details::Genre),
        ("nothing", false, Details::None),
    ];
    const STEPS: usize = 6;
    let mut rested: Vec<(&str, f32)> = Vec::new();
    for (name, title, details) in WALKS {
        let mut w = home_shelves_of(&format!("hc-walk-{name}"), 12);
        describe_cards(&mut w, 12, &|m, _| { m.title = "Heat".into(); m.genre = Some(LONG_GENRE.into()); });
        let _cell = Cell::default();
        set(&mut w, title, details);
        to_shelf(&mut w, 2);
        w.frames(40);
        let setting = format!("{name} (title {title}, details {details:?})");
        let want: Vec<&str> = title.then_some("Heat").into_iter().chain((details != Details::None).then_some(LONG_GENRE)).collect();
        // (the focused heading rises while focus is in the first slots: `heading_clearance`'s lift)
        let rest = heading_y(&mut w, 2).unwrap_or_else(|| panic!("{setting}: premise: the focused shelf's heading is drawn"));
        rested.push((name, rest));
        let mut heading = rest;
        // The block is drawn for the first time (that keys the marquee's clock), and its rest beat passes
        // (`marquee::TITLE`, one second), so the walk starts on a line that is already moving.
        let start = w.focus();
        let (first, _) = draw_and_glides(&mut w);
        assert_eq!(long_line(&first).is_some(), details != Details::None, "{setting}: premise: the long line is drawn exactly when the setting shows a details line");
        w.frames(120);
        // steps 0..=STEPS are the walk (0: before any Right); the two after it are the walk's end at rest,
        // a second apart, where the line has visibly moved on.
        let mut at_rest: Vec<f32> = Vec::new();
        for step in 0..STEPS + 3 {
            match step {
                0 => {}
                s if s <= STEPS => { w.tap(Key::Right, 2); }
                s if s == STEPS + 1 => w.frames(120),
                _ => w.frames(30),
            }
            let (frame, glides) = draw_and_glides(&mut w);
            assert_eq!(frame.caption_lines(), want, "{setting}, step {step}: the focused block is the setting's lines");
            for h in headings_in(&frame).iter().filter(|h| overlaps(h.rect, CHIP_FRAME)) {
                assert!(h.alpha <= 0.02, "{setting}, step {step}: the heading {:?} at {:?} is drawn at alpha {} over the chip (none allowed)", h.title, h.rect, h.alpha);
            }
            let focused = headings_in(&frame).into_iter().find(|h| h.title == TITLES[2] && drawn(h))
                .unwrap_or_else(|| panic!("{setting}, step {step}: the focused shelf's heading is drawn"));
            // It only ever comes down from the place it rested at, and by no more than the lift it gives up:
            // never up toward the chip, whatever the band or the glide under it.
            assert!(focused.rect.y >= heading - 0.01 && focused.rect.y <= rest + HEADING_LIFT_MAX,
                "{setting}, step {step}: the focused heading, resting at {rest}, only comes down by the lift ({heading} -> {})", focused.rect.y);
            heading = focused.rect.y;
            match long_line(&frame) {
                Some((x, runs)) => {
                    assert!(runs >= 2, "{setting}, step {step}: the long line is a marquee, a run and its follower, got {runs} runs");
                    assert!(glides, "{setting}, step {step}: the line is past its rest beat and gliding, yet the frame reports no motion");
                    if step > STEPS { at_rest.push(x); }
                }
                None => assert!(details == Details::None && !glides, "{setting}, step {step}: the long line is not drawn, and nothing glides in its place"),
            }
        }
        if let [first, second] = at_rest[..] {
            assert!(second < first - 5.0, "{setting}: a second on, the line has glided left on the card the walk landed on ({first} -> {second})");
        }
        assert_ne!(w.focus(), start, "{setting}: premise: the walk moved focus");
    }
    for pair in rested.windows(2) {
        assert!((pair[0].1 - pair[1].1).abs() < 0.5, "the focused heading rests at one y whatever the band: {rested:?}");
    }
}
