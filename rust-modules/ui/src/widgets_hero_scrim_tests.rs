//! Detail's hero legibility contract: the atmospheric base ramp plus the content-following shade
//! (`hero_shade`), graded line by line over bright and blown-white artwork.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ── Detail's hero legibility: the atmospheric ramp under the content shade ───────────────────
//
// All pure math over `theme` tokens and the screens' own layout arithmetic — no GL, no globals, so
// these are ordinary parallel tests. The screens' contributions come from
// `ui::landing_hero::base_scrim_a` / `detail::base_scrim_a` / `detail::hero_chain`, which is the whole
// point of those being pure: the contract below reads the SAME numbers the draw does.

/// The worst contrast `ink` gets over `art` across the run `x0..=x1` and the span `y0..=y1` of a
/// text box on the Detail hero: the page's atmospheric base ramp (`base_scrim_a`, a function of `y`)
/// under the hero shade at `strength`, stacked.
fn detail_worst(
    shade: &crate::hero_shade::HeroShade,
    strength: f32,
    ink: [f32; 4],
    art: f32,
    (x0, x1): (f32, f32),
    (y0, y1): (f32, f32),
) -> (f32, f32, f32, f32) {
    let mut worst = (f32::INFINITY, x0, y0, 0.0);
    for ix in 0..=16 {
        for iy in 0..=4 {
            let (x, y) = (x0 + (x1 - x0) * ix as f32 / 16.0, y0 + (y1 - y0) * iy as f32 / 4.0);
            let total = 1.0 - (1.0 - crate::detail_layout::base_scrim_a(y, 1.0)) * (1.0 - shade.at(x, y, strength));
            let c = contrast_over_art(ink, art, total);
            if c < worst.0 {
                worst = (c, x, y, total);
            }
        }
    }
    worst
}

/// The blocks Detail's hero shade is built from, at their WIDEST (`DetailScreen::hero_shade` in
/// `screens/src/detail/mod.rs`): the title's band at its resting spot, the identity / ratings /
/// synopsis column from `meta_y` down to the facts line, the facts line, the credits column
/// (conservatively, at its tallest) and the action row, each as wide as the box its content is laid
/// into. The screen's own blocks are as wide as the widest line drawn in each box (`add_ink`), so they
/// are never wider than these and stand exactly behind the text; grading the text at the widest it can
/// stand, in the widest box, is the contract's worst case. The positions are read from
/// `detail_layout::hero_chain`, so a layout change updates the contract rather than invalidating it;
/// that Detail's screen builds blocks of exactly the drawn width is held where the screen is
/// (`detail::tests::the_detail_hero_shade_stands_behind_its_own_content`).
fn detail_shade(hc: &crate::detail_layout::HeroChain) -> crate::hero_shade::HeroShade {
    use crate::consts::{MARGIN_X, SCR_W};
    use crate::detail_layout as dl;
    use crate::hero_shade::{HeroShade, Role};
    // the facts line and the controls stop at the credits column's left edge less one small gap
    // (`appkit::hero_layout::FACTS_R`, which this layer may not name)
    let facts_r = SCR_W - MARGIN_X - dl::PEOPLE_W - theme::space::SM;
    let people_top = dl::people_top(hc.btn_y, dl::PEOPLE_MAX_LINES, theme::size::CAPTION as f32);
    HeroShade::new()
        .add(Rect::new(MARGIN_X, detail_title_cap_top(), dl::HERO_TEXT_W, HERO_CAP_H), Role::Copy, 1.0)
        .add(Rect::new(MARGIN_X, hc.meta_y, dl::HERO_TEXT_W, hc.facts_y - hc.meta_y), Role::Copy, 1.0)
        .add(Rect::new(MARGIN_X, hc.facts_y, facts_r - MARGIN_X, theme::hero::FACTS_H), Role::Copy, 1.0)
        .add(Rect::new(SCR_W - MARGIN_X - dl::PEOPLE_W, people_top, dl::PEOPLE_W, dl::PEOPLE_MAX_LINES as f32 * dl::PEOPLE_LEAD), Role::Copy, 1.0)
        .add(Rect::new(MARGIN_X, hc.btn_y, facts_r - MARGIN_X, 60.0), Role::Controls, 1.0)
}

/// **The prize: "is the hero readable" as an assertion instead of a judgement on a television** —
/// DETAIL's half (Home's own shade, and its copy at the new ladder's positions, is graded in
/// `widgets_hero_shade_tests.rs`).
///
/// Every line of hero copy Detail draws over artwork, at the WORST point of its run, graded as
/// `1 − (1 − base_scrim_a(y)) · (1 − shade(x, y))`: the page's atmospheric ramp under the content
/// shade ([`crate::hero_shade`]) built from Detail's own blocks ([`detail_shade`]), against two
/// backdrops: a bright one (encoded 0.85, a blown highlight, brighter than essentially any fanart
/// pixel) and a blown-white one (1.0, the degenerate case the design only promises a floor for).
///
/// The ys are READ from the page's own layout arithmetic (`hero_chain`) so a layout change updates
/// this contract rather than silently invalidating it. The floors are the design's: 3:1 over bright
/// art, 2.5:1 over blown white. **They are not lowered to fit the shade**: a position that falls
/// under one is fixed with the shade's own parameters ([`theme::shade`]).
///
/// The TITLE row grades the TEXT fallback, across its cap box; a clearLogo occupies that band
/// instead on most items and paints inside the block the shade is built from (the block is the
/// painted rect itself, [`crate::hero_logo::HeroLogo::painted`]).
///
/// The people column grades at its tallest (four wrapped lines with the shortest reserved synopsis:
/// the highest People paint), and is shaded by its own block rather than by the left column's.
#[test]
fn the_hero_text_reads_over_bright_artwork() {
    use crate::consts::{MARGIN_X, SCR_W};
    let hc = crate::detail_layout::hero_chain(
        // a two-line blurb — the shape `hero_chain`'s own doc is tuned on. Measuring is the one
        // thing the host cannot do, so the height is quoted, not computed.
        34.0, true, &crate::fixture::FixtureMeasure,
    );
    let det_col_r = MARGIN_X + crate::detail_layout::HERO_TEXT_W; // the synopsis' wrap edge
    let people_l = SCR_W - MARGIN_X - crate::detail_layout::PEOPLE_W;
    let people_top = crate::detail_layout::people_top(hc.btn_y, crate::detail_layout::PEOPLE_MAX_LINES, theme::size::CAPTION as f32);
    let shade = detail_shade(&hc);

    // (label, x run, y span, ink, floor over BRIGHT art, floor over BLOWN WHITE)
    let rows: [(&str, (f32, f32), (f32, f32), [f32; 4], f32, f32); 5] = [
        ("detail title", (MARGIN_X, det_col_r), (detail_title_cap_top(), crate::detail_layout::TITLE_BOTTOM), theme::TEXT_PRIMARY, 3.0, 2.5),
        ("detail meta", (MARGIN_X, 700.0), (hc.meta_y, hc.meta_y + BADGE_H), theme::TEXT_SECONDARY, 3.0, 2.5),
        ("detail synopsis", (MARGIN_X, det_col_r), (hc.syn_y, hc.syn_y + 34.0), theme::TEXT_READING, 3.0, 2.5),
        ("detail facts", (MARGIN_X, 1270.0), (hc.facts_y, hc.facts_y + theme::hero::FACTS_H), crate::detail_layout::FACTS_INK, 3.0, 2.5),
        (
            "detail people (top line)",
            (people_l, SCR_W - MARGIN_X),
            (people_top, people_top + crate::detail_layout::PEOPLE_LEAD),
            crate::detail_layout::PEOPLE_INK,
            3.0,
            2.5,
        ),
    ];
    for (label, xs, ys, ink, min_bright, min_white) in rows {
        for (art, floor) in [(0.85f32, min_bright), (1.0, min_white)] {
            let (after, x, y, total) = detail_worst(&shade, 1.0, ink, art, xs, ys);
            let before = contrast_over_art(ink, art, crate::detail_layout::base_scrim_a(y, 1.0));
            eprintln!("{label:<26} art {art:.2}: {after:5.2}:1 (floor {floor}) at ({x:.0},{y:.0}) total alpha {total:.3}; ramp alone {before:.2}:1");
            assert!(
                after >= floor,
                "{label} at ({x:.0},{y:.0}) over art {art}: {after:.2}:1, under its {floor}:1 floor (total alpha {total:.3})"
            );
            assert!(after >= before, "{label} over art {art}: the shade made it WORSE ({before:.2} → {after:.2})");
        }
    }
    // Video-bound rows. The title stays (at rest, prose receded) and the logo shrinks to the corner:
    // the same shade at the raised strength must not weaken either.
    let ceiling = crate::landing_hero::PREVIEW_FIELD;
    let title = (MARGIN_X, det_col_r);
    let spans = (detail_title_cap_top(), crate::detail_layout::TITLE_BOTTOM);
    let (still, ..) = detail_worst(&shade, 1.0, theme::TEXT_PRIMARY, 0.85, title, spans);
    let (bound, ..) = detail_worst(&shade, ceiling, theme::TEXT_PRIMARY, 0.85, title, spans);
    assert!(bound >= 3.0, "detail title (video-bound) over bright art: {bound:.2}:1, under 3:1");
    assert!(bound >= still, "the raised field must not weaken the title ({still:.2} → {bound:.2})");
    // …and the logo risen into the trailer corner, where the ramp is nothing and the shade is all there is
    let corner = crate::hero_shade::HeroShade::new().add(
        Rect::new(
            crate::detail_layout::PREVIEW_LOGO_X,
            crate::detail_layout::PREVIEW_LOGO_Y,
            crate::detail_layout::PREVIEW_LOGO_MAX_W,
            crate::hero_logo::band_h(crate::hero_logo::LogoRung::Compact),
        ),
        crate::hero_shade::Role::Copy,
        1.0,
    );
    let corner_run = (crate::detail_layout::PREVIEW_LOGO_X, crate::detail_layout::PREVIEW_LOGO_X + crate::detail_layout::PREVIEW_LOGO_MAX_W);
    let corner_span = (crate::detail_layout::PREVIEW_LOGO_Y, crate::detail_layout::PREVIEW_LOGO_Y + crate::hero_logo::band_h(crate::hero_logo::LogoRung::Compact));
    for (art, floor) in [(0.85f32, 3.0f32), (1.0, 2.5)] {
        let (c, x, y, total) = detail_worst(&corner, ceiling, theme::TEXT_PRIMARY, art, corner_run, corner_span);
        eprintln!("detail logo in the trailer corner art {art:.2}: {c:5.2}:1 (floor {floor}) at ({x:.0},{y:.0}) total alpha {total:.3}");
        assert!(c >= floor, "the risen logo over art {art}: {c:.2}:1, under its {floor}:1 floor");
    }
}
