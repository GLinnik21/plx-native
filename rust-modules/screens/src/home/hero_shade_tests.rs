//! Home's hero shade, the shelf fade and the billboard's pager: that the shade stands behind the
//! rects the page really draws (and moves with them), that the Continue Watching captions have no
//! block of it but the shelf fade under them, and that the pager stands in the action row after the
//! Info disc, at the spacing of the owner's board (CW-13), whatever the buttons before it are.

use super::*;
use plx_ui::hero_shade::Role;
use plx_ui::widgets::CONTROL_GAP;

const HEX_A: [u8; 12] = [0xa1; 12];

/// Three heroes (rk 1 and 2 movies, rk 3 an episode) over the seed's Continue Watching row. With
/// `disc`, the first movie has a portable id, so its action row carries the Watchlist disc (one
/// control wider than the same row without it).
fn seeded(disc: bool) -> plx_data::pms::HubsSnapshot {
    seeded_resuming(disc, false)
}

/// [`seeded`], with the first movie half watched when `resuming`: its hero's Play reads "Resume".
fn seeded_resuming(disc: bool, resuming: bool) -> plx_data::pms::HubsSnapshot {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    plx_data::pms::edit_test_item(&mut state, "1", &|m| {
        m.kind = 0;
        m.guid = disc.then_some(HEX_A);
        if resuming {
            (m.resume_ms, m.dur_ns) = (3_600_000, 7_200_000 * 1_000_000);
        }
    });
    plx_data::pms::edit_test_item(&mut state, "2", &|m| m.kind = 0);
    // an episode names itself with its one bold line, so its ladder has a block a bare movie's lacks
    plx_data::pms::edit_test_item(&mut state, "3", &|m| {
        m.kind = 3;
        m.season_index = 1;
        m.ep_index = 2;
    });
    plx_data::pms::hubs_snapshot(&state)
}

fn tick(s: &mut HomeScreen, view: HubsView<'_>, frames: u32) {
    for frame in 0..frames {
        step(s, view, None, &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 }));
    }
}

/// The chevron's inked left and right edges, from its drawn box.
fn chevron_ink(chevron: Rect) -> (f32, f32) {
    let (l, r) = plx_ui::icons::ink_x(Icon::Chevron);
    (chevron.x + l * chevron.w, chevron.x + r * chevron.w)
}

/// The action row's rect the shade stands behind: Play to Info, one control tall.
fn row_rect(c: &HeroRects) -> Rect {
    Rect::new(c.play.x, HERO_ROW_Y, c.info.x + c.info.w - c.play.x, HERO_CTRL_D)
}

/// What the page's shade is built from beyond the heroes: the pager of the selected hero `hero`.
fn tail(view: HubsView<'_>, m: &dyn Measure, hero: &PmsMovie) -> (Rect, f32) {
    (pager_block(view.hero_count(), info_right(m, hero)), Role::Controls.peak())
}

/// Where a hero's Info disc ends, as the page lays its row out.
fn info_right(m: &dyn Measure, hero: &PmsMovie) -> f32 {
    let resumes = plx_data::metadata::resume_ns(hero.resume_ms, hero.dur_ns / 1_000_000) > 0;
    let info = hero_rects(m, hero, plx_appkit::hero_layout::play_label(resumes)).info;
    info.x + info.w
}

/// The pager's block in the shade, ink to ink: the dots' left to the chevron's right, one control tall.
fn pager_block(n: usize, info_right: f32) -> Rect {
    let pager = hero_pager(info_right, PageDots::new(n).width());
    Rect::new(pager.dots_x, HERO_ROW_Y, chevron_ink(pager.chevron).1 - pager.dots_x, HERO_CTRL_D)
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
    let snapshot = seeded(true);
    let view = snapshot.view();
    let item = view.hero(0).unwrap().item;
    let long = std::ffi::CString::new("Reprendre la lecture depuis le debut de l'episode precedent").unwrap();
    let labels = [std::ffi::CString::new("Play").unwrap(), long];
    let infos: Vec<Rect> = labels.iter().map(|l| hero_rects(m, item, l).info).collect();
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

/// **The shade's pager block is the pager the page draws**, for each hero's own row: the same screen
/// with a Watchlist disc (a wider row) hands the shade a block further right, by exactly the disc and
/// its gap, and the block is the pager's ink from the dots to the chevron.
#[test]
fn the_shades_pager_block_follows_the_row_it_stands_in() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let with = seeded(true);
    let without = seeded(false);
    let (mut a, mut b) = (screen(with.view()), screen(without.view()));
    tick(&mut a, with.view(), 1);
    tick(&mut b, without.view(), 1);
    let (sa, sb) = (a.hero_shade(with.view(), m), b.hero_shade(without.view(), m));
    let (ba, bb) = (sa.blocks(), sb.blocks());
    assert_eq!((ba.len(), bb.len()), (3, 3), "copy, action row, pager: nothing under the captions");
    assert!(ba[1].0.w > bb[1].0.w, "the premise: the disc makes the row wider ({} vs {})", ba[1].0.w, bb[1].0.w);
    let (ha, hb) = (a.selected_hero(with.view()).unwrap().item, b.selected_hero(without.view()).unwrap().item);
    assert_eq!(ba[2], (pager_block(with.view().hero_count(), info_right(m, ha)), Role::Controls.peak()));
    assert_eq!(bb[2], (pager_block(without.view().hero_count(), info_right(m, hb)), Role::Controls.peak()));
    assert!((ba[2].0.x - bb[2].0.x - (HERO_CTRL_D + CONTROL_GAP)).abs() < 1e-3, "the disc and its gap carry the pager along");
    assert!((ba[2].0.w - bb[2].0.w).abs() < 1e-3, "the group itself is the same size");
}

/// **Home's shade stands behind exactly the rects the page draws**: the copy column
/// ([`hero_copy_rect`]), the action row Play-to-Info ([`hero_rects`]) and the pager
/// ([`hero_pager`]), each at its role's darkness, in that order.
#[test]
fn the_home_hero_shade_stands_behind_the_content_it_draws() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let snapshot = seeded(true);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    assert!(s.slide_offsets().is_none(), "the premise: a hero at rest");
    let hero = s.selected_hero(view).expect("a hero").item;
    let controls = hero_rects(m, hero, plx_appkit::hero_layout::play_label(false));
    let want = [
        (hero_copy_rect(hero, m), Role::Copy.peak()),
        (row_rect(&controls), Role::Controls.peak()),
        tail(view, m, hero),
    ];
    assert_eq!(s.hero_shade(view, m).blocks(), want);
    // and a hero with another kind of copy shades its own column: the episode's one bold line is
    // not the movie's synopsis (the ladder, and with it the rect, differs)
    let episode = view.hero(2).unwrap().item;
    assert_eq!(episode.kind, 3);
    assert_ne!(hero_copy_rect(episode, m), hero_copy_rect(hero, m), "the premise: the two ladders differ");
}

/// **The shade moves with a hero that slides**: during a flip the outgoing and incoming heroes each
/// bring their copy and action row at their own slide offset, and the pager (the selected hero's, after
/// its Info) does not slide.
#[test]
fn the_home_hero_shade_follows_a_slide() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let snapshot = seeded(false);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    let old = s.selected_hero(view).expect("a hero").item;
    assert!(s.flip(view, 1));
    tick(&mut s, view, 1);
    let (out_x, in_x) = s.slide_offsets().expect("the premise: a slide in flight");
    assert!(out_x != 0.0 && in_x != 0.0);
    let new = s.selected_hero(view).expect("a hero").item;
    assert!(!std::ptr::eq(old, new), "the premise: another hero is selected");
    let at = |r: Rect, dx: f32| Rect::new(r.x + dx, r.y, r.w, r.h);
    let row = |h: &PmsMovie| {
        let resumes = plx_data::metadata::resume_ns(h.resume_ms, h.dur_ns / 1_000_000) > 0;
        row_rect(&hero_rects(m, h, plx_appkit::hero_layout::play_label(resumes)))
    };
    let pager = tail(view, m, new);
    let want = [
        (at(hero_copy_rect(old, m), out_x), Role::Copy.peak()),
        (at(row(old), out_x), Role::Controls.peak()),
        (at(hero_copy_rect(new, m), in_x), Role::Copy.peak()),
        (at(row(new), in_x), Role::Controls.peak()),
        pager,
    ];
    let got = s.hero_shade(view, m);
    assert_eq!(got.blocks(), want, "each hero's blocks ride its own page");
    assert_eq!(got.blocks()[4], pager, "the pager does not slide, whatever the heroes' offsets");
    assert!(got.blocks().len() <= plx_gfx::gfx::scrim::MAX_BLOCKS, "the busiest frame fits the uniform array");
}

/// **The Continue Watching captions have no block behind them: the shelf fade stands under them.**
/// The shade's blocks are the hero's own (copy, action row, pager), every one of them above the tiles'
/// foot, and the fade the ground carries ([`HomeScreen::shelf_fade`]) is the full-width ramp from the
/// tiles' top to near-black at the caption line's top, scaled by the hero's fade.
#[test]
fn the_continue_watching_captions_stand_on_the_shelf_fade_not_on_a_block() {
    let _guard = plx_base::testlock::serial();
    let (_state, snapshot, s) = scroll_home();
    let view = snapshot.view();
    let m = &plx_ui::fixture::FixtureMeasure;
    let tiles = Rect::new(0.0, PEEK_Y + CARD_DY, SCR_W, RowStyle::STILL.h);
    let line = plx_ui::still_footer::Place::Below.line_top(tiles);
    let caption = Rect::new(0.0, line, SCR_W, plx_ui::still_footer::FOOTER_LINE_H);
    for (rect, _) in s.hero_shade(view, m).blocks() {
        assert!(rect.y + rect.h <= tiles.y + tiles.h, "{rect:?}: no block reaches the Continue Watching footer line");
    }
    // the shade's own darkness over the caption line is the feather's tail at most: none of its
    // blocks is near enough to darken it (the fade does that)
    let shade = s.hero_shade(view, m).scrim(1.0);
    assert_eq!(shade.alpha(SCR_W * 0.5, caption.y + caption.h * 0.5), 0.0, "the shade does not reach the caption line");
    let fade = s.shelf_fade(1.0);
    assert_eq!(fade.stops[0].0, tiles.y, "it begins at the tiles' top, 768 at rest");
    assert_eq!((tiles.y, line), (768.0, 1012.0));
    assert_eq!(fade.alpha(tiles.y), 0.0);
    assert_eq!(fade.alpha(caption.y), theme::shade::SHELF_FOOT_A, "near-black at the caption line");
    assert_eq!(fade.alpha(caption.y + caption.h), theme::shade::SHELF_FOOT_A, "…the whole line, to its foot");
    assert_eq!(s.shelf_fade(0.5).alpha(caption.y), theme::shade::SHELF_FOOT_A * 0.5, "it fades with the hero");
}

/// **The shelf fade rides the page**: it starts at the FIRST shelf's live tile top, so a page that
/// has scrolled the shelf up (the dive) takes the fade with it, and a page with no shelf yet
/// stands it at the rest place.
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

/// **The shade layer draws the cells under the copy, not the panel.** A resting hero's shade is the
/// layer over its opaque art (`Backdrop::draw`: no ground pass is drawn once the art covers the wash),
/// 16 cells of 1536 vertices each running the six-block distance; the renderer drops a cell the
/// shade cannot reach and, of one it reaches, the mesh rows above and below the part it does
/// (`gfx::draw_scrim_layer`). This counts what a RESUMING hero's real blocks (copy column, action row
/// with the Watchlist disc, pager) leave drawn at full strength, and holds that everything left out is
/// exactly zero, so the skipped draws were the ones that painted nothing.
#[test]
fn a_resuming_heros_shade_reaches_only_the_cells_under_its_content() {
    use plx_ui::widgets::{cut_cells, SCRIM_CELL_H, SCRIM_CELL_W};
    const ROWS: usize = 16; // `gfx::FIELD_N`: the mesh is ROWS rows of ROWS squares of 6 vertices
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let snapshot = seeded_resuming(true, true);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    assert!(s.slide_offsets().is_none(), "the premise: a hero at rest");
    let hero = s.selected_hero(view).expect("a hero").item;
    assert!(hero_resumes(hero), "the premise: a Resume hero");
    let shade = s.hero_shade(view, m).scrim(1.0);
    let cells: Vec<Rect> = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).collect();
    let rect = |c: &Rect| [c.x, c.y, c.w, c.h];
    let reached: Vec<&Rect> = cells.iter().filter(|c| shade.field.reaches(rect(c))).collect();
    let drawn_rows: usize = cells.iter().filter_map(|c| shade.field.reached_rows(rect(c), ROWS)).map(|r| r.len()).sum();
    let verts = |rows: usize| rows * ROWS * 6;
    eprintln!(
        "resume hero shade: {} of {} cells reached ({} of {} vertices), {} of {} mesh rows drawn ({} vertices): {:?}",
        reached.len(),
        cells.len(),
        verts(reached.len() * ROWS),
        verts(cells.len() * ROWS),
        drawn_rows,
        cells.len() * ROWS,
        verts(drawn_rows),
        reached.iter().map(|c| (c.x as i32, c.y as i32)).collect::<Vec<_>>()
    );
    assert!(reached.len() < cells.len(), "the shade does not cover the panel");
    assert!(reached.len() * 2 <= cells.len() + 2, "…a little over half of it at most: {} of {}", reached.len(), cells.len());
    assert!(drawn_rows < reached.len() * ROWS, "…and the cells it reaches are drawn only in part: {drawn_rows} rows");
    for c in cells.iter().filter(|c| !shade.field.reaches(rect(c))) {
        for iy in 0..=18 {
            for ix in 0..=32 {
                let (x, y) = (c.x + c.w * ix as f32 / 32.0, c.y + c.h * iy as f32 / 18.0);
                assert_eq!(shade.alpha(x, y), 0.0, "({x},{y}) in the skipped cell {c:?}");
            }
        }
    }
    // …and every cell holding a block's own rect is reached
    for (rect, _) in s.hero_shade(view, m).blocks() {
        let (cx, cy) = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let home = cells.iter().find(|c| cx >= c.x && cx < c.x + c.w && cy >= c.y && cy < c.y + c.h).expect("on the panel");
        assert!(shade.field.reaches([home.x, home.y, home.w, home.h]), "{rect:?}'s cell {home:?}");
    }
}
