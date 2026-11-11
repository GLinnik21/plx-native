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
