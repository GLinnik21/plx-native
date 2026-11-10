//! Home's hero shade and the billboard's pager: that the shade stands behind the rects the page
//! really draws (and moves with them), and that the pager stands in the action row after the Info
//! disc, at the spacing of the owner's board (CW-13), whatever the buttons before it are.

use super::*;
use plx_ui::hero_shade::Role;
use plx_ui::widgets::CONTROL_GAP;

const HEX_A: [u8; 12] = [0xa1; 12];

/// Three heroes (rk 1 and 2 movies, rk 3 an episode) over the seed's Continue Watching row. With
/// `disc`, the first movie has a portable id, so its action row carries the Watchlist disc (one
/// control wider than the same row without it).
fn seeded(disc: bool) -> plx_data::pms::HubsSnapshot {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    plx_data::pms::edit_test_item(&mut state, "1", &|m| {
        m.kind = 0;
        m.guid = disc.then_some(HEX_A);
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

/// What the page's shade is built from beyond the heroes: the pager of the selected hero `hero`, then
/// the footer band of the Continue Watching row.
fn tail(s: &HomeScreen, view: HubsView<'_>, m: &dyn Measure, hero: &PmsMovie) -> [(Rect, f32); 2] {
    let footer = s.continue_footer_band(view).expect("the seed's first row is the deck");
    [(pager_block(view.hero_count(), info_right(m, hero)), Role::Controls.peak()), (footer, Role::Footer.peak())]
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
    assert_eq!((ba.len(), bb.len()), (4, 4), "copy, action row, pager, footer");
    assert!(ba[1].0.w > bb[1].0.w, "the premise: the disc makes the row wider ({} vs {})", ba[1].0.w, bb[1].0.w);
    let (ha, hb) = (a.selected_hero(with.view()).unwrap().item, b.selected_hero(without.view()).unwrap().item);
    assert_eq!(ba[2], (pager_block(with.view().hero_count(), info_right(m, ha)), Role::Controls.peak()));
    assert_eq!(bb[2], (pager_block(without.view().hero_count(), info_right(m, hb)), Role::Controls.peak()));
    assert!((ba[2].0.x - bb[2].0.x - (HERO_CTRL_D + CONTROL_GAP)).abs() < 1e-3, "the disc and its gap carry the pager along");
    assert!((ba[2].0.w - bb[2].0.w).abs() < 1e-3, "the group itself is the same size");
}

/// **Home's shade stands behind exactly the rects the page draws**: the copy column
/// ([`hero_copy_rect`]), the action row Play-to-Info ([`hero_rects`]) and the pager
/// ([`hero_pager`]) and the Continue Watching footers' band, each at its role's darkness, in that order.
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
    let [pager, footer] = tail(&s, view, m, hero);
    let want = [
        (hero_copy_rect(hero, m), Role::Copy.peak()),
        (row_rect(&controls), Role::Controls.peak()),
        pager,
        footer,
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
    let [pager, footer] = tail(&s, view, m, new);
    let want = [
        (at(hero_copy_rect(old, m), out_x), Role::Copy.peak()),
        (at(row(old), out_x), Role::Controls.peak()),
        (at(hero_copy_rect(new, m), in_x), Role::Copy.peak()),
        (at(row(new), in_x), Role::Controls.peak()),
        pager,
        footer,
    ];
    let got = s.hero_shade(view, m);
    assert_eq!(got.blocks(), want, "each hero's blocks ride its own page");
    assert_eq!(got.blocks()[4], pager, "the pager does not slide, whatever the heroes' offsets");
    assert_eq!(got.blocks().len(), plx_gfx::gfx::scrim::MAX_BLOCKS, "the busiest frame fills the uniform array exactly");
}

/// **Under the Continue Watching row the shade stands behind the footer band**, in screen space, one
/// footer line tall, a drop under the tiles (the geometry the row's footers are drawn at), and
/// it is the LAST block (the budget: two heroes' copy and row, the pager, then this one).
#[test]
fn the_shade_stands_behind_the_continue_watching_footers() {
    let _guard = plx_base::testlock::serial();
    let (_state, snapshot, s) = scroll_home();
    let view = snapshot.view();
    let m = &plx_ui::fixture::FixtureMeasure;
    let band = s.continue_footer_band(view).expect("the first row is the deck");
    let tile_foot = PEEK_Y + CARD_DY + RowStyle::STILL.h;
    assert_eq!((band.y, band.h), (tile_foot + theme::STILL_FOOTER_DROP, plx_ui::still_footer::FOOTER_LINE_H));
    assert!(band.w > 0.0 && band.x >= 0.0 && band.x + band.w <= SCR_W, "the band is on the panel: {band:?}");
    let shade = s.hero_shade(view, m);
    let last = *shade.blocks().last().expect("a footer block");
    assert_eq!(last, (band, Role::Footer.peak()));
    assert!(shade.blocks().len() <= plx_gfx::gfx::scrim::MAX_BLOCKS);
}
