//! Detail's continuous shade and text contrast over bright and white artwork.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

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
    // Video-bound rows. The title stays (at rest, prose receded) and the logo shrinks to the corner:
    // the same shade at the raised strength must not weaken either.
    let ceiling = crate::landing_hero::PREVIEW_FIELD;
    let title = (MARGIN_X, det_col_r);
    let spans = (detail_title_cap_top(), crate::detail_layout::TITLE_BOTTOM);
    let (still, ..) = detail_worst(&shade, theme::TEXT_PRIMARY, 0.85, title, spans);
    let preview = crate::landing_hero::detail_shade(1.0, ceiling, 1.0, 1.0);
    let (bound, ..) = detail_worst(&preview, theme::TEXT_PRIMARY, 0.85, title, spans);
    assert!(bound >= 3.0, "detail title (video-bound) over bright art: {bound:.2}:1, under 3:1");
    assert!(bound >= still, "the raised field must not weaken the title ({still:.2} → {bound:.2})");
    let corner_run = (crate::detail_layout::PREVIEW_LOGO_X, crate::detail_layout::PREVIEW_LOGO_X + crate::detail_layout::PREVIEW_LOGO_MAX_W);
    let corner_span = (crate::detail_layout::PREVIEW_LOGO_Y, crate::detail_layout::PREVIEW_LOGO_Y + crate::hero_logo::band_h(crate::hero_logo::LogoRung::Compact));
    for (art, floor) in [(0.85f32, 3.0f32), (1.0, 2.5)] {
        let (c, x, y, total) = detail_worst(&preview, theme::TEXT_PRIMARY, art, corner_run, corner_span);
        eprintln!("detail logo in the trailer corner art {art:.2}: {c:5.2}:1 (floor {floor}) at ({x:.0},{y:.0}) total alpha {total:.3}");
        assert!(c >= floor, "the risen logo over art {art}: {c:.2}:1, under its {floor}:1 floor");
    }
}
