//! Detail's continuous shade and text contrast over bright and white artwork.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn the_trailer_shade_clears_the_space_left_by_the_title() {
    use crate::landing_hero::{detail_shade, PREVIEW_FIELD};

    let still = detail_shade(1.0, 1.0, 1.0, 0.0);
    assert!(still.alpha(400.0, 480.0) > 0.5);
    let mut previous = detail_shade(1.0, PREVIEW_FIELD, 1.0, 0.0).alpha(400.0, 480.0);
    for progress in [0.25, 0.5, 0.75, 1.0] {
        let alpha = detail_shade(1.0, PREVIEW_FIELD, 1.0, progress).alpha(400.0, 480.0);
        assert!(alpha <= previous, "the vacated title area should lighten as the logo moves");
        previous = alpha;
    }
    let preview = detail_shade(1.0, PREVIEW_FIELD, 1.0, 1.0);
    for y in [260.0, 360.0, 480.0, 520.0] {
        assert_eq!(preview.alpha(400.0, y), 0.0, "the gap under the compact logo at y={y}");
    }
    let logo_shade = preview.alpha(300.0, 85.0);
    for x in [0.0, 576.0, 960.0, 1920.0] {
        assert_eq!(preview.alpha(x, 85.0), logo_shade, "the top fade has no patch behind the logo");
    }
    assert!((0.4..0.55).contains(&logo_shade), "the shade supports the compact logo without a dark patch");
    assert!(preview.alpha(300.0, 175.0) < logo_shade);
    assert!(preview.alpha(1200.0, 700.0) > 0.6, "the lower text keeps its shade");
}

/// Weakest contrast over a text box, sampled through the same shade the page uses.
fn detail_worst(
    shade: &WashScrim,
    ink: [f32; 4],
    art: f32,
    (x0, x1): (f32, f32),
    (y0, y1): (f32, f32),
) -> (f32, f32, f32, f32) {
    let mut worst = (f32::INFINITY, x0, y0, 0.0);
    for ix in 0..=16 {
        for iy in 0..=4 {
            let (x, y) = (x0 + (x1 - x0) * ix as f32 / 16.0, y0 + (y1 - y0) * iy as f32 / 4.0);
            let total = shade.alpha(x, y);
            let c = contrast_over_art(ink, art, total);
            if c < worst.0 {
                worst = (c, x, y, total);
            }
        }
    }
    worst
}

/// Check the full width of each text box, including the highest and widest credits.
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
    let shade = crate::landing_hero::detail_shade(1.0, 1.0, 1.0, 0.0);

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
            let (after, x, y, total) = detail_worst(&shade, ink, art, xs, ys);
            let before = contrast_over_art(ink, art, crate::detail_layout::base_scrim_a(y, 1.0));
            eprintln!("{label:<26} art {art:.2}: {after:5.2}:1 (floor {floor}) at ({x:.0},{y:.0}) total alpha {total:.3}; ramp alone {before:.2}:1");
            assert!(
                after >= floor,
                "{label} at ({x:.0},{y:.0}) over art {art}: {after:.2}:1, under its {floor}:1 floor (total alpha {total:.3})"
            );
            assert!(after >= before, "{label} over art {art}: the shade made it WORSE ({before:.2} → {after:.2})");
        }
    }
    // Video-bound rows. The compact logo and the synopsis still need contrast, including when
    // no ratings row pushes the synopsis down.
    let ceiling = crate::landing_hero::PREVIEW_FIELD;
    let preview = crate::landing_hero::detail_shade(1.0, ceiling, 1.0, 1.0);
    let synopsis = crate::detail_layout::hero_chain(34.0, false, &crate::fixture::FixtureMeasure).syn_y;
    let corner_run = (crate::detail_layout::PREVIEW_LOGO_X, crate::detail_layout::PREVIEW_LOGO_X + crate::detail_layout::PREVIEW_LOGO_MAX_W);
    let corner_span = (crate::detail_layout::PREVIEW_LOGO_Y, crate::detail_layout::PREVIEW_LOGO_Y + crate::hero_logo::band_h(crate::hero_logo::LogoRung::Compact));
    for (art, floor) in [(0.85f32, 3.0f32), (1.0, 2.5)] {
        let (c, x, y, total) = detail_worst(&preview, theme::TEXT_PRIMARY, art, corner_run, corner_span);
        eprintln!("detail logo in the trailer corner art {art:.2}: {c:5.2}:1 (floor {floor}) at ({x:.0},{y:.0}) total alpha {total:.3}");
        assert!(c >= floor, "the risen logo over art {art}: {c:.2}:1, under its {floor}:1 floor");
        let (c, ..) = detail_worst(&preview, theme::TEXT_READING, art, (MARGIN_X, det_col_r), (synopsis, synopsis + 34.0));
        assert!(c >= floor, "the preview synopsis over art {art}: {c:.2}:1, under its {floor}:1 floor");
    }
}
