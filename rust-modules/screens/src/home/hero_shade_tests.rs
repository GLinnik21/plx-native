//! Home's hero shade and the billboard's pager: that the shade stands behind the rects the page
//! really draws (and moves with them), and that the pager is ONE group at a fixed place on the action
//! row's centre line that no button label can move.

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

/// What the page's shade is built from beyond the heroes: the pager, then the footer band of the
/// Continue Watching row.
fn tail(s: &HomeScreen, view: HubsView<'_>) -> [(Rect, f32); 2] {
    let footer = s.continue_footer_band(view).expect("the seed's first row is the deck");
    [(pager_block(view.hero_count()), Role::Controls.peak()), (footer, Role::Footer.peak())]
}

/// The pager's block in the shade: the dots' left ink to the right margin, one control tall.
fn pager_block(n: usize) -> Rect {
    let pager = hero_pager(PageDots::new(n).width());
    Rect::new(pager.dots_x, HERO_ROW_Y, SCR_W - MARGIN_X - pager.dots_x, HERO_CTRL_D)
}

/// **The pager is one group: the chevron's ink ends at the right margin, the dots stand one control
/// gap before it between their ink, and the group is centred on the action row's centre line.** The
/// group grows to the LEFT as dots are added, so the chevron never moves.
#[test]
fn the_pager_is_one_group_ending_at_the_right_margin_on_the_rows_centre_line() {
    assert_eq!(SCR_W - MARGIN_X, 1824.0, "the design's right margin");
    assert_eq!(HERO_PAGER_CY, 698.0, "the action row's centre line");
    assert_eq!(HERO_PAGER_CY, HERO_ROW_Y + HERO_CTRL_D * 0.5);
    let mut last_dots_x = f32::INFINITY;
    let mut chevron_at = None;
    for n in 2..=9usize {
        let dots_w = PageDots::new(n).width();
        let pager = hero_pager(dots_w);
        let (ink_l, ink_r) = chevron_ink(pager.chevron);
        assert!((ink_r - (SCR_W - MARGIN_X)).abs() < 1e-3, "n={n}: the chevron's ink ends at {ink_r}, not the margin");
        assert!(
            (ink_l - (pager.dots_x + dots_w) - CONTROL_GAP).abs() < 1e-3,
            "n={n}: the dots' ink ends at {}, the chevron's begins at {ink_l}: not one gap",
            pager.dots_x + dots_w
        );
        assert!((pager.chevron.y + pager.chevron.h * 0.5 - HERO_PAGER_CY).abs() < 1e-3, "n={n}: the chevron is on the centre line");
        assert!(pager.dots_x < last_dots_x, "n={n}: more dots grow the group to the left");
        last_dots_x = pager.dots_x;
        assert_eq!(*chevron_at.get_or_insert(pager.chevron), pager.chevron, "n={n}: the chevron never moves");
    }
}

/// **The pager's place does not depend on the buttons** (their labels, their widths, their language):
/// whatever Play says, the row it ends is a different width and the pager is the same rect. Proved
/// two ways: the rects of rows of different widths (two labels; with and without the disc) do differ
/// while the pager does not, and clears them; and two SCREENS whose heroes' rows are different widths
/// hand the shade the same pager block.
#[test]
fn the_pager_stands_in_the_same_place_whatever_the_buttons_are() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let snapshot = seeded(true);
    let view = snapshot.view();
    let item = view.hero(0).unwrap().item;
    let long = std::ffi::CString::new("Reprendre la lecture depuis le debut de l'episode precedent").unwrap();
    let labels = [std::ffi::CString::new("Play").unwrap(), long];
    let rows: Vec<Rect> = labels.iter().map(|l| row_rect(&hero_rects(m, item, l))).collect();
    assert!(rows[0].w < rows[1].w, "the premise: two rows of two widths ({:?})", rows.iter().map(|r| r.w).collect::<Vec<_>>());
    let dots_w = PageDots::new(view.hero_count()).width();
    let pager = hero_pager(dots_w);
    for (i, row) in rows.iter().enumerate() {
        let again = hero_pager(dots_w);
        assert_eq!((again.dots_x, again.chevron), (pager.dots_x, pager.chevron), "label {i}: the pager does not read the row");
        assert!(row.x + row.w + CONTROL_GAP <= pager.dots_x, "label {i}: the row ({}) ends clear of the pager ({})", row.x + row.w, pager.dots_x);
    }

    // the same through the screen: a hero with the disc has a wider row than one without it
    let with = seeded(true);
    let without = seeded(false);
    let (mut a, mut b) = (screen(with.view()), screen(without.view()));
    tick(&mut a, with.view(), 1);
    tick(&mut b, without.view(), 1);
    let (sa, sb) = (a.hero_shade(with.view(), m), b.hero_shade(without.view(), m));
    let (ba, bb) = (sa.blocks(), sb.blocks());
    assert_eq!((ba.len(), bb.len()), (4, 4), "copy, action row, pager, footer");
    assert!(ba[1].0.w > bb[1].0.w, "the premise: the disc makes the row wider ({} vs {})", ba[1].0.w, bb[1].0.w);
    assert_eq!(ba[2], bb[2], "the pager block is the same for either row");
    assert_eq!(ba[2].0, pager_block(with.view().hero_count()));
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
    let [pager, footer] = tail(&s, view);
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
/// bring their copy and action row at their own slide offset, and the pager (fixed) does not slide.
#[test]
fn the_home_hero_shade_follows_a_slide() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let snapshot = seeded(false);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    let rest = s.hero_shade(view, m);
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
    let [pager, footer] = tail(&s, view);
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
    assert_eq!(got.blocks()[4], rest.blocks()[2], "the pager does not slide");
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
