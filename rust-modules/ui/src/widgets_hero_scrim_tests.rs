//! Detail's hero legibility contract (the atmospheric base ramp plus the content-following shade,
//! `hero_shade`), and the retired corner wedge's (`hero_scrim`) shape and seam.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ── The hero corner scrim: the wedge's shape, its seam, and the legibility it promises ──────
//
// All pure math over `theme` tokens and the two screens' own layout arithmetic — no GL, no
// globals, so these are ordinary parallel tests. The screens' contributions come from
// `ui::landing_hero::base_scrim_a` / `detail::base_scrim_a` / `detail::hero_chain`, which is the whole
// point of those three being pure: the contract below reads the SAME numbers the draw does.

/// The bilinear field a `(rect, [tl, tr, br, bl])` quad actually rasterizes, at absolute
/// `(x, y)` — the shader's own `mix(mix(tl,tr,u), mix(bl,br,u), v)`, in alpha. The anti-drift
/// yardstick: the closed-form [`hero_scrim_a`]/[`hero_scrim_right_a`] the contract is graded on
/// and the corner colours that are actually drawn must agree, or the promise is about a field
/// nobody paints.
fn bilerp_a(q: (Rect, [[f32; 4]; 4]), x: f32, y: f32) -> f32 {
    let (r, k) = q;
    let u = ((x - r.x) / r.w).clamp(0.0, 1.0);
    let v = ((y - r.y) / r.h).clamp(0.0, 1.0);
    let top = k[0][3] + (k[1][3] - k[0][3]) * u;
    let bot = k[3][3] + (k[2][3] - k[3][3]) * u;
    top + (bot - top) * v
}

/// The wedge is a DARKENER that peaks in the corner the text is in: monotone non-increasing in
/// x, at full [`theme::SCRIM_TEXT_A`] at the margin, and exactly 0 from [`HERO_SCRIM_W`] out —
/// a non-zero value there would draw a vertical line across the hero where the quads end. Also
/// pins that every out-of-range input clamps rather than going negative or past 1: `x` and
/// `strength` both come from live animation state, and a negative alpha here would BRIGHTEN
/// the artwork under the copy.
#[test]
fn the_wedge_never_brightens_toward_the_text() {
    for &s in &[0.25f32, 0.5, 1.0] {
        assert!(
            (hero_scrim_a(0.0, s) - theme::SCRIM_TEXT_A * s).abs() < 1e-6,
            "the margin is the peak"
        );
        let mut prev = f32::INFINITY;
        let mut x = 0.0f32;
        while x <= crate::consts::SCR_W {
            let a = hero_scrim_a(x, s);
            assert!(
                a <= prev + 1e-6,
                "s={s}: alpha rose from {prev} to {a} at x={x}"
            );
            assert!(
                (0.0..=1.0).contains(&a),
                "s={s} x={x}: alpha {a} outside 0..=1"
            );
            prev = a;
            x += 8.0;
        }
        assert_eq!(
            hero_scrim_a(HERO_SCRIM_W, s),
            0.0,
            "the wedge must reach exactly nothing at its end"
        );
        assert_eq!(
            hero_scrim_a(crate::consts::SCR_W, s),
            0.0,
            "…and stay there"
        );
        assert_eq!(
            hero_scrim_a(-40.0, s),
            theme::SCRIM_TEXT_A * s,
            "x left of the frame is the peak, not more"
        );
    }
    assert_eq!(
        hero_scrim_a(0.0, -0.5),
        0.0,
        "a negative strength is nothing, never an inverted wedge"
    );
    assert_eq!(
        hero_scrim_a(0.0, 4.0),
        theme::SCRIM_TEXT_A * crate::landing_hero::PREVIEW_FIELD,
        "strength saturates at the video-bound ceiling, not past it"
    );
}

/// **The seam** — the one structural bug this component can have, and invisible on the host in
/// every other form. The two left quads must share an EXACT float y and an identical colour
/// pair along it: a gap leaves one row of unscrimmed artwork (BRIGHT) straight across the
/// hero, an overlap composites two scrims on one row (BLACK). Neither is subtle and both look
/// like a renderer bug rather than a layout one.
///
/// `art_scrim`'s hairline had a different cause (an integer-truncated scissor meeting a float
/// fill) and a different fix (`gfx::snap`). This pair is fill-to-fill: do not "fix" it by
/// snapping, which would move the seam off the shared float and CREATE the bug.
#[test]
fn the_wedges_two_quads_abut_with_no_step() {
    let (q, n) = hero_scrim_quads(1.0, false);
    assert_eq!(n, 2);
    let (r0, k0) = q[0];
    let (r1, k1) = q[1];
    assert_eq!(r0.y + r0.h, r1.y, "the seam is not one float");
    assert_eq!(r0.x, r1.x, "the two quads must be the same column");
    assert_eq!(r0.w, r1.w);
    assert_eq!(
        k0[3], k1[0],
        "quad 0's bottom-left must be quad 1's top-left"
    );
    assert_eq!(
        k0[2], k1[1],
        "quad 0's bottom-right must be quad 1's top-right"
    );
    assert_eq!(
        r1.y + r1.h,
        crate::consts::SCR_H,
        "the wedge runs to the foot of the panel"
    );
    // …and the field is continuous ACROSS the seam, not merely the same colour at its ends:
    // sample both quads' own bilinear along it.
    for x in [0.0f32, 300.0, 900.0, HERO_SCRIM_W] {
        let below = bilerp_a(q[1], x, r1.y);
        assert!(
            (bilerp_a(q[0], x, r0.y + r0.h) - below).abs() < 1e-6,
            "step at x={x}"
        );
        // The quad's CORNERS are `hero_scrim_a` by construction now, so what is left to grade in
        // the interior is that the closed form is AFFINE in x — a curve there would be a field
        // `grad4`'s straight interpolation cannot reproduce, and the contract would be about a
        // shape nobody paints.
        assert!(
            (below - hero_scrim_a(x, 1.0)).abs() < 1e-6,
            "the drawn field disagrees with hero_scrim_a at x={x}"
        );
    }
}

/// The top chrome owns its own legibility with its own dark capsule (`draw_tab_row`'s track).
/// A wedge creeping up into that band is two treatments fighting over one strip — and the
/// capsule was tuned assuming nothing else is under it.
#[test]
fn the_wedge_leaves_the_top_chrome_alone() {
    let (q, _) = hero_scrim_quads(1.0, true);
    let bar_bottom = TOP_BAR_Y + TAB_PILL_H + TAB_TRACK_PAD + theme::space::XS;
    for (i, (r, _)) in q.iter().enumerate() {
        assert!(
            r.y >= bar_bottom,
            "quad {i} starts at y={} — inside the top chrome's band ({bar_bottom})",
            r.y
        );
    }
    // The feather also has to start ABOVE the highest text anchor, which is what lets
    // `hero_scrim_a` be a function of x alone (see `HERO_SCRIM_KNEE`) and is what makes the
    // legibility table below sound — every row it grades is at or below this line. A clearLogo
    // paints higher than any of them, but that is ART riding the feather, not a graded anchor.
    let highest = detail_title_cap_top();
    assert!(
        HERO_SCRIM_KNEE <= highest,
        "the knee ({HERO_SCRIM_KNEE}) must be at or above the topmost hero line ({highest})"
    );
}

/// The right wedge is OPT-IN (home's hero has no right-aligned copy and must not pay for one)
/// and feathers in from both of its inner edges, so neither boundary can draw a line on the
/// picture. Its overlap with the left wedge's tail is intended, not an accident to "fix" by
/// moving an edge: the two fields multiply, and the facts line lives in the overlap.
#[test]
fn the_right_wedge_is_opt_in_and_starts_at_zero() {
    assert_eq!(
        hero_scrim_quads(1.0, false).1,
        2,
        "no right wedge unless asked for"
    );
    let (q, n) = hero_scrim_quads(1.0, true);
    assert_eq!(n, 3);
    let (r, k) = q[2];
    assert_eq!(
        [k[0][3], k[1][3], k[3][3]],
        [0.0, 0.0, 0.0],
        "only the bottom-right corner carries weight"
    );
    assert_eq!(k[2][3], HERO_SCRIM_R_A, "…and it carries exactly the token");
    for y in [r.y, r.y + r.h * 0.5, r.y + r.h] {
        assert_eq!(
            bilerp_a(q[2], r.x, y),
            0.0,
            "the left edge must not draw a line"
        );
    }
    for x in [r.x, r.x + r.w * 0.5, r.x + r.w] {
        assert_eq!(
            bilerp_a(q[2], x, r.y),
            0.0,
            "the top edge must not draw a line"
        );
    }
    // …and inside the quad the closed form must be the BILINEAR product `grad4` can actually
    // interpolate between those corners (u·v). The corners themselves are `hero_scrim_right_a`
    // by construction; this is the part of the shape that construction does not pin.
    for x in [1250.0f32, 1500.0, 1830.0, 1920.0] {
        for y in [750.0f32, 900.0, 1080.0] {
            let want = hero_scrim_right_a(x, y, 1.0);
            assert!(
                (bilerp_a(q[2], x, y) - want).abs() < 1e-6,
                "drawn field != hero_scrim_right_a at ({x},{y})"
            );
        }
    }
    assert!(
        r.x < HERO_SCRIM_W,
        "the two wedges are meant to overlap — the facts line sits in the overlap"
    );
}

/// The wedge belongs to the HERO, not to the frame: at strength 0 there is nothing to draw.
/// A residual wedge on the grid/shelf view would be a real regression and is invisible in a
/// still — the shelves need the flat ground their card drop-shadows are tuned against.
///
/// Graded on the closed forms alone. The quads' own corners USED to be swept here too, and that
/// sweep is now a tautology: [`hero_scrim_quads`] builds every corner by evaluating these two
/// functions, so it cannot report weight they do not have.
#[test]
fn the_wedge_leaves_with_the_hero() {
    assert_eq!(hero_scrim_a(0.0, 0.0), 0.0);
    assert_eq!(hero_scrim_a(HERO_SCRIM_W, 0.0), 0.0);
    assert_eq!(hero_scrim_right_a(1920.0, 1080.0, 0.0), 0.0);
}

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

/// The blocks Detail's hero shade is built from, in the SAME shapes `DetailScreen::hero_shade` feeds
/// (`screens/src/detail/mod.rs`): the title's band at its resting spot, the identity / ratings /
/// synopsis column from `meta_y` down to the facts line, the facts line, the credits column
/// (conservatively, at its tallest) and the action row. The positions are read from
/// `detail_layout::hero_chain`, so a layout change updates the contract rather than invalidating it;
/// that Detail's screen builds exactly these rects is held where the screen is
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
