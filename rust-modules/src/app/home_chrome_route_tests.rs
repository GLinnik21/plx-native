//! **Home's shelf headings and the shared top chrome** (FRAMES-8 item 5): the account chip and the
//! nav pills are drawn over the page, and a heading that stands in their band reads as torn by them.
//! On the Under posters settings that open the focused row's caption band LESS (80 px, or none) the
//! heading of the shelf above the focused one rested at y 61 and 101, under the chip, because Home
//! rested a focused row by the caption band it opens (`row_reveal_band`'s `lo`).
//!
//! Key-driven on a real `Dispatcher<AppHost>` (`bridge_test_support::World`): what a person reads is
//! what the page DREW (`World::drawn`, the text runs with the alpha the cascade left on each).

use super::*;
use super::test_support::World;
use super::under_posters_shelf_route_tests::{set, Cell};
use plx_plex::plex::session::UnderPostersDetails as Details;
use plx_ui::widgets::{CHIP_FRAME, TAB_TRACK_PAD, TOP_BAR_BOTTOM, TOP_BAR_Y};
use plx_ui::Rect;

const TITLES: [&str; 6] = ["Shelf Alpha", "Shelf Bravo", "Shelf Charlie", "Shelf Delta", "Shelf Echo", "Shelf Foxtrot"];

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
    let mut w = World::home_with_disc(name, [7; 12]);
    let ids: Vec<String> = (0..TITLES.len()).map(|i| format!("test.row.{i}")).collect();
    let rows: Vec<(&str, &str, &str)> = ids.iter().zip(TITLES).map(|(id, title)| (id.as_str(), "", title)).collect();
    let adapter = w.bridge.stores.hubs.adapter_for_test();
    plx_data::pms::seed_named_hubs_for_test(w.bridge.stores.hubs.state_mut(), &adapter, 3, &rows);
    w.frames(30);
    w.press(Key::Down);
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
struct Heading {
    title: &'static str,
    rect: Rect,
    alpha: f32,
}

fn headings(w: &mut World) -> Vec<Heading> {
    let line = plx_ui::theme::size::HEADLINE as f32;
    w.drawn().runs.into_iter().filter_map(|run| {
        let title = *TITLES.iter().find(|title| **title == run.text)?;
        Some(Heading { title, rect: Rect { h: run.at.h.max(line), ..run.at }, alpha: run.ink[3] })
    }).collect()
}

/// A heading is DRAWN once it has any ink worth the name: the fade takes it to nothing, and a run at
/// alpha 0 paints no pixel.
fn drawn(h: &Heading) -> bool {
    h.alpha > 0.02
}

/// Focus on the first poster of shelf `n` (0-based), the way a person gets there: Down, a row at a time.
fn to_shelf(w: &mut World, n: usize) {
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
    // A heading is judged at its place on the page, without the focus pop's lift (a few pixels that
    // let it clear the popped tile), so the heading of the shelf focus is leaving can stand up to
    // that lift lower than the place it fades by. Its glyphs start below the line's top, so this
    // bound is the most ink a run can leave on the chip's last rows, not a measure of any.
    let lift = plx_ui::cards::heading_lift_max(&plx_ui::cards::RowStyle::HOME);
    let most = plx_ui::scroll_chrome::clearance_under_shown_chrome(CHIP_FRAME.y + CHIP_FRAME.h + lift);
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
                        assert!(h.alpha <= most + 1e-3,
                            "{setting}, Down {press} frame {frame}: the heading {:?} at {:?} is drawn at alpha {} over the chip (at most {most})", h.title, h.rect, h.alpha);
                    }
                }
            }
        }
        assert!(crossed > 0, "{setting}: premise: a heading crosses the chip's rows while the page scrolls");
    }
}
