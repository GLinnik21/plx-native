//! Detail's hero shade: that it stands behind the rects the page really draws, and that it MOVES with
//! them, above all the logo rising into the trailer corner (`plx_ui::hero_shade`). Detail's layout is
//! unchanged by the shade; these hold only what the shade is fed.

use super::*;
use plx_ui::hero_shade::Role;

fn titled(sid: ServerId) -> Detail {
    let mut d = detail(sid, "show");
    d.title = "A Test Title".into();
    d
}

/// The rect the hero's title paints at rest, read the way the draw reads it: the band bottom-anchored
/// on `TITLE_BOTTOM`, `HERO_TEXT_W` wide, through `HeroLogo::painted`.
fn title_rect(screen: &DetailScreen, m: &dyn plx_machine::machine::Measure) -> Rect {
    let band_h = plx_ui::hero_logo::band_h(LogoRung::Hero);
    let band = Rect::new(plx_ui::consts::MARGIN_X, plx_ui::detail_layout::TITLE_BOTTOM - band_h, plx_ui::detail_layout::HERO_TEXT_W, band_h);
    HeroLogo::new(screen.sid.raw(), &screen.rk, "A Test Title", LogoRung::Hero).painted(band, m)
}

/// **The shade stands behind exactly the rects the Detail hero draws**: the title (logo or typed), the
/// identity / ratings / synopsis column from `meta_y` down to the facts, the facts line, and the
/// action row, each at its role's darkness. The positions are the page's own layout (`hero_chain`,
/// `hero_row_rect`), so a layout change carries the shade with it.
#[test]
fn the_detail_hero_shade_stands_behind_its_own_content() {
    let sid = ServerId::UNSET;
    let guard = install(titled(sid));
    let screen = bare(&guard, sid, "show");
    let meta = test_store().view();
    let m = &plx_ui::fixture::FixtureMeasure;
    let d = screen.detail(meta);
    let chain = screen.hero_chain(m, meta);
    let set = screen.hero_set(meta);
    let widths = hero::hero_widths(m, set, set.restart, screen.disc_unfurl.map(|s| s.pos), screen.named_episode(meta));
    let x = plx_ui::consts::MARGIN_X;
    let want = [
        (title_rect(&screen, m), Role::Copy.peak()),
        (Rect::new(x, chain.meta_y, plx_ui::detail_layout::HERO_TEXT_W, chain.facts_y - chain.meta_y), Role::Copy.peak()),
        (Rect::new(x, chain.facts_y, plx_appkit::hero_layout::FACTS_R - x, theme::hero::FACTS_H), Role::Copy.peak()),
        (hero::hero_row_rect(set, chain.btn_y, widths), Role::Controls.peak()),
    ];
    let shade = screen.hero_shade(d, m, meta);
    assert_eq!(shade.blocks(), want);
    assert!(d.and_then(|d| hero::people_rect(d, chain.btn_y, m)).is_none(), "the premise: this item has no credits column");
    clear();
}

/// **The credits column brings its own block** when the item has one, bottom-anchored where
/// [`hero::draw_people`] draws it.
#[test]
fn the_credits_column_has_its_own_block() {
    let sid = ServerId::UNSET;
    let mut d = titled(sid);
    d.cast = vec![plx_data::metadata::Cast {
        tag: "An Actor".into(),
        role: "A Role".into(),
        thumb: String::new(),
        id: 0,
        tag_key: String::new(),
    }];
    let guard = install(d);
    let screen = bare(&guard, sid, "show");
    let meta = test_store().view();
    let m = &plx_ui::fixture::FixtureMeasure;
    let d = screen.detail(meta).expect("the item is installed");
    let chain = screen.hero_chain(m, meta);
    let people = hero::people_rect(d, chain.btn_y, m).expect("a cast line is a credits column");
    assert_eq!(people.x + people.w, plx_ui::consts::SCR_W - plx_ui::consts::MARGIN_X, "its right edge is the margin");
    assert!(people.h > 0.0 && people.y + people.h <= chain.btn_y, "it stands above the action row: {people:?}");
    let shade = screen.hero_shade(Some(d), m, meta);
    assert_eq!(*shade.blocks().last().expect("a block"), (people, Role::Copy.peak()));
    clear();
}

/// **The shade follows the logo up into the trailer corner.** `preview_logo` is the spring that
/// carries the hero logo to the top-left compact spot; the shade is built from the logo's live
/// rect, so at rest it stands behind the title and once risen it stands in the corner, with none left
/// behind the title spot's own block. In flight the rect (and the shade with it) is in between.
#[test]
fn the_shade_rises_with_the_trailer_logo() {
    let sid = ServerId::UNSET;
    let guard = install(titled(sid));
    let mut screen = bare(&guard, sid, "show");
    let meta = test_store().view();
    let m = &plx_ui::fixture::FixtureMeasure;
    let (x, corner_y) = (plx_ui::detail_layout::PREVIEW_LOGO_X, plx_ui::detail_layout::PREVIEW_LOGO_Y);
    let logo_block = |screen: &DetailScreen| {
        let d = screen.detail(meta);
        screen.hero_shade(d, m, meta).blocks()[0].0
    };
    let rest = logo_block(&screen);
    screen.preview_logo = Spring::at(1.0);
    let risen = logo_block(&screen);
    assert!(rest.y > 300.0 && risen.y < 120.0, "the premise: the title is low and the corner is high ({} -> {})", rest.y, risen.y);
    assert!(risen.x >= x && risen.y >= corner_y - 0.5 && risen.w <= plx_ui::detail_layout::PREVIEW_LOGO_MAX_W + 0.5, "the risen block is in the corner: {risen:?}");
    // …and the SHADE, at a point in each place, is where the block is
    let probe = |screen: &DetailScreen, px: f32, py: f32| {
        let d = screen.detail(meta);
        screen.hero_shade(d, m, meta).at(px, py, 1.0)
    };
    let (title_pt, corner_pt) = ((rest.x + 60.0, rest.y + rest.h * 0.5), (risen.x + 60.0, risen.y + risen.h * 0.5));
    screen.preview_logo = Spring::at(0.0);
    let (rest_title, rest_corner) = (probe(&screen, title_pt.0, title_pt.1), probe(&screen, corner_pt.0, corner_pt.1));
    screen.preview_logo = Spring::at(1.0);
    let (risen_title, risen_corner) = (probe(&screen, title_pt.0, title_pt.1), probe(&screen, corner_pt.0, corner_pt.1));
    assert!(rest_title >= theme::shade::COPY_A && rest_corner < 0.01, "at rest: behind the title ({rest_title}), none in the corner ({rest_corner})");
    assert!(risen_corner >= theme::shade::COPY_A - 1e-4, "risen: behind the corner logo ({risen_corner})");
    assert!(risen_title < rest_title - 0.1, "…and the title spot lost the logo's block ({rest_title} -> {risen_title})");
    // in flight the rect is between the two, and keeps moving toward the corner
    let mut prev_y = f32::INFINITY;
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        screen.preview_logo = Spring::at(t);
        let r = logo_block(&screen);
        assert!(r.y <= prev_y, "t={t}: the block only rises ({prev_y} -> {})", r.y);
        prev_y = r.y;
    }
    clear();
}

/// **Each block fades with the content it stands behind**: the trailer's chrome fade takes the logo's,
/// the facts', the credits' and the action row's blocks with it, and the synopsis column's own fade
/// takes the column's; a hero with none of its content left has no shade at all.
#[test]
fn a_block_fades_with_its_own_content() {
    let sid = ServerId::UNSET;
    let guard = install(titled(sid));
    let mut screen = bare(&guard, sid, "show");
    let meta = test_store().view();
    let m = &plx_ui::fixture::FixtureMeasure;
    let blocks = |screen: &DetailScreen| screen.hero_shade(screen.detail(meta), m, meta).blocks().len();
    assert_eq!(blocks(&screen), 4);
    screen.preview_chrome = 0.0;
    assert_eq!(blocks(&screen), 1, "only the synopsis column, which has its own fade");
    screen.preview_synopsis = 0.0;
    screen.preview_prose = 0.0;
    assert_eq!(blocks(&screen), 0, "nothing left to stand behind");
    clear();
}
