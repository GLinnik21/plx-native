//! Home's pager placement and the continuous shade behind its artwork.

use super::*;
use plx_ui::widgets::CONTROL_GAP;

fn chevron_ink(chevron: Rect) -> (f32, f32) {
    let (l, r) = plx_ui::icons::ink_x(Icon::Chevron);
    (chevron.x + l * chevron.w, chevron.x + r * chevron.w)
}

/// **The pager stands in the action row, after the Info disc, at the board's spacing.** The board
/// (CW-13) makes the row a flex with `gap = CONTROL_GAP`; the dots group has an extra 20 px of
/// margin-left after the Info disc, and the chevron is a 60 px box at margin-left -24, so it sits
/// tight after the dots. Pinned here to the row for rows of different widths (two labels, with and
/// without the disc) and for every dot count: the dots' left is the Info disc's right + `CONTROL_GAP` +
/// 20; the chevron box built the board's way puts its glyph's ink where the page puts it; the group is
/// centred on the row's centre line; and the pager moves with the row (a wider row, a pager further
/// right), because it is IN it.
#[test]
fn the_pager_stands_in_the_row_after_info_at_the_boards_spacing() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let item = &PmsMovie { guid: Some([0xa1; 12]), ..PmsMovie::default() };
    let long = std::ffi::CString::new("Reprendre la lecture depuis le debut de l'episode precedent").unwrap();
    let labels = [std::ffi::CString::new("Play").unwrap(), long];
    let infos: Vec<Rect> = labels.iter().map(|l| hero_rects(m, item, l, false).info).collect();
    assert!(infos[0].x < infos[1].x, "the premise: two rows of two widths ({:?})", infos.iter().map(|r| r.x).collect::<Vec<_>>());
    assert_eq!(HERO_PAGER_CY, 698.0, "the action row's centre line");
    assert_eq!(HERO_PAGER_CY, HERO_ROW_Y + HERO_CTRL_D * 0.5);
    let mut last_chevron_x = [f32::NEG_INFINITY; 2];
    for n in 2..=9usize {
        let dots_w = PageDots::new(n).width();
        for (i, info) in infos.iter().enumerate() {
            let info_right = info.x + info.w;
            let pager = hero_pager(info_right, dots_w);
            // the dots: the row's gap, then the group's own 20 px of margin-left
            assert_eq!(pager.dots_x, info_right + CONTROL_GAP + 20.0, "n={n} label {i}: the dots' left");
            // the chevron: the board's 60 px box at margin-left -24, one gap after the dots' right
            let box_left = pager.dots_x + dots_w + CONTROL_GAP - 24.0;
            let glyph_left = box_left + (HERO_CTRL_D - HERO_PAGER_D) * 0.5;
            let board_ink_left = glyph_left + plx_ui::icons::ink_x(Icon::Chevron).0 * HERO_PAGER_D;
            let (ink_l, _) = chevron_ink(pager.chevron);
            assert!((ink_l - board_ink_left).abs() < 0.1, "n={n} label {i}: the chevron's ink begins at {ink_l}, the board's at {board_ink_left}");
            assert!((ink_l - (pager.dots_x + dots_w) - CONTROL_GAP).abs() < 1e-3, "n={n} label {i}: one gap between the dots' ink and the chevron's");
            assert!((pager.chevron.y + pager.chevron.h * 0.5 - HERO_PAGER_CY).abs() < 1e-3, "n={n} label {i}: the chevron is on the centre line");
            assert!(pager.chevron.x > last_chevron_x[i], "n={n} label {i}: more dots grow the group to the right");
            last_chevron_x[i] = pager.chevron.x;
        }
        let (near, far) = (hero_pager(infos[0].x + infos[0].w, dots_w), hero_pager(infos[1].x + infos[1].w, dots_w));
        assert!(near.dots_x < far.dots_x, "n={n}: a wider row carries the pager further right");
    }
}

#[test]
fn the_shelf_fade_starts_at_the_first_shelfs_live_tile_top() {
    let _guard = plx_base::testlock::serial();
    let (_state, snapshot, mut s) = scroll_home();
    let _ = snapshot;
    assert_eq!(s.shelf_fade(1.0).stops[0].0, plx_ui::landing_hero::SHELF_TOP, "at rest");
    s.snap.jump(0.5);
    s.layout_grid();
    let tiles_top = s.grid.base_y(0) + CARD_DY;
    assert!(tiles_top < plx_ui::landing_hero::SHELF_TOP, "the premise: the dive has lifted the shelf");
    assert_eq!(s.shelf_fade(1.0).stops[0].0, tiles_top, "the fade is where the shelf is");
    s.grid.shelves.clear();
    assert_eq!(s.shelf_fade(1.0).stops[0].0, plx_ui::landing_hero::SHELF_TOP, "no shelf: the rest place");
}


#[test]
fn the_home_shade_is_anchored_to_the_edges_and_joins_without_a_dark_band() {
    let shelf = shelf_fade(SHELF_TOP, 1.0);
    let shade = home_shade(shelf, 1.0);
    assert_eq!(shade.alpha(96.0, 0.0), 0.0);
    assert_eq!(shade.alpha(96.0, 600.0), theme::shade::HOME_SIDE_A);
    assert!(shade.alpha(600.0, 600.0) > 0.0);
    assert_eq!(shade.alpha(SCR_W, 600.0), 0.0);
    for y in 1..=1080 {
        let y = y as f32;
        assert!(shade.alpha(96.0, y) >= shade.alpha(96.0, y - 1.0) - 1e-6);
        assert!((shade.alpha(SCR_W, y) - shelf.alpha(y)).abs() < 1e-6);
    }
    assert_eq!(shade.alpha(96.0, 1050.0), shade.alpha(SCR_W, 1050.0));
    assert_eq!(home_shade(shelf_fade(SHELF_TOP, 0.0), 0.0).alpha(96.0, 600.0), 0.0);
}

/// **The pager is part of the page it belongs to and rides that page's slide.** (Owner, v0.10.0 rc1:
/// "page indicator still above the page — I wanted it to be in the page and scroll with the page
/// instead of jumping".) Both pages of a flip are drawn at their own slide offset, hero content and
/// action row alike; the dots and chevron were drawn once, untranslated, so they stayed fixed while
/// the page slid under them and then jumped to the next hero's row.
#[test]
fn the_pager_slides_with_the_page_it_belongs_to() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);
    let chevrons = |s: &HomeScreen| -> Vec<Rect> {
        let env = s.env(0.0);
        let hero = s.selected_hero(view).unwrap();
        plx_ui::draw_census::capture_icons(|| {
            s.draw_pagers(view, hero.item, &env, Painter::recording(), &plx_ui::fixture::FixtureMeasure)
        })
        .into_iter()
        .filter(|(icon, _)| *icon == Icon::Chevron)
        .map(|(_, r)| r)
        .collect()
    };
    let rest = chevrons(&s);
    assert_eq!(rest.len(), 1, "a settled billboard draws one pager");
    assert!(s.flip(view, 1));
    for slide in [0.25, 0.5, 0.75] {
        s.hero_slide.jump(slide);
        let (out_x, in_x) = s.slide_offsets().unwrap();
        let mut got: Vec<f32> = chevrons(&s).iter().map(|r| r.x - rest[0].x).collect();
        got.sort_by(f32::total_cmp);
        let mut want = [out_x, in_x];
        want.sort_by(f32::total_cmp);
        assert_eq!(got.len(), 2, "slide {slide}: each page draws its own pager");
        for (got, want) in got.iter().zip(want) {
            assert!((got - want).abs() < 0.5, "slide {slide}: a pager stands at its page's offset {want}, got {got}");
        }
    }
}

/// **The hero controls' tint follows the incoming page, in step with the ground.** (Owner, v0.10.0
/// rc1: "when page flips play button tint falls behind the actual page color and has the tint of the
/// previous page".) Three parts: the row is read on the page that is arriving, where it is drawn,
/// rather than only once the slide has come to rest; the key dissolves toward a new reading instead
/// of popping to it; and the page sliding away keeps its own key instead of following the new one.
#[test]
fn the_hero_row_is_read_on_the_arriving_page_not_after_the_slide() {
    let row = |x: f32| Rect::new(x, 640.0, 470.0, 116.0);
    assert!(may_read_hero_ground(true, row(96.0), 1.0), "a settled page");
    assert!(may_read_hero_ground(true, row(96.0 + 0.5 * SCR_W), 1.0), "an arriving page, wholly on screen, is read where it is drawn");
    assert!(!may_read_hero_ground(true, row(SCR_W - 200.0), 1.0), "…not while its row is still partly off screen");
    assert!(!may_read_hero_ground(true, row(96.0 - 0.9 * SCR_W), 1.0));
    assert!(!may_read_hero_ground(false, row(96.0 - 0.5 * SCR_W), 1.0), "a page leaving reads nothing");
    assert!(!may_read_hero_ground(true, row(96.0), 0.5), "a route in flight reads nothing");
}

#[test]
fn the_hero_key_dissolves_to_a_new_reading_and_a_leaving_page_keeps_its_own() {
    let _guard = plx_base::testlock::serial();
    let (old, new) = ([0.10, 0.20, 0.60], [0.50, 0.20, 0.10]);
    let mut t = HeroTint::default();
    assert!(t.key().is_none() && t.for_page(true).is_none(), "nothing read yet");
    t.read.set(Some(old));
    t.step(0.016);
    assert_eq!(t.key(), Some(old), "the first reading is taken whole");
    // a flip: the page that leaves keeps `old`; a new reading lands for the page that arrives
    t.leave();
    t.read.set(Some(new));
    let mut last = old[0];
    for frame in 0..6 {
        t.step(0.016);
        let key = t.key().unwrap();
        assert!(key[0] > last && key[0] < new[0], "frame {frame}: the live key moves toward the new reading, never popping to it ({key:?})");
        last = key[0];
        assert_eq!(t.for_page(false), Some(old), "frame {frame}: the page sliding away keeps the key it left with");
    }
    for _ in 0..240 {
        t.step(0.016);
    }
    assert_eq!(t.key(), Some(new), "it lands on the reading");
    t.arrived();
    assert_eq!(t.for_page(false), Some(new), "with no page leaving, every page reads the one key");
}
