//! The hero corner scrim (`hero_scrim`) wedge shape/seam and its legibility contract.

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
        while x <= crate::ui::consts::SCR_W {
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
            hero_scrim_a(crate::ui::consts::SCR_W, s),
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
        theme::SCRIM_TEXT_A * crate::ui::landing_hero::PREVIEW_FIELD,
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
        crate::ui::consts::SCR_H,
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
    let highest = detail_title_cap_top().min(home_title_cap_top());
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

/// **The prize: "is the hero readable" as an assertion instead of a judgement on a television.**
///
/// Every line of hero copy either screen draws over artwork, at the WORST point of its run (the
/// right end, where the wedge is weakest), graded as
/// `1 − (1 − base_scrim_a) · (1 − hero_scrim_a) · (1 − hero_scrim_right_a)` against two
/// backdrops: a bright one (encoded 0.85 — a blown highlight, brighter than essentially any
/// fanart pixel) and a blown-white one (1.0, the degenerate case the design only promises a
/// floor for).
///
/// The ys are READ from the screens' own layout arithmetic — `hero_chain` for detail, the
/// bottom-anchored stack for home — so a layout change updates this contract rather than
/// silently invalidating it. The floors are the measured achieved ratios: the point is that
/// this fires the day someone brightens `SCRIM_INK`, dims a text token, moves
/// `HERO_TEXT_BOTTOM` or trims `HERO_SCRIM_W`, none of which is visible in a diff.
///
/// The two TITLE rows grade the TEXT fallback, at its cap top — a clearLogo occupies that band
/// instead on most items and paints higher than the knee, which is art riding the feather rather
/// than an anchor this closed form can speak for (see [`HERO_SCRIM_KNEE`]).
///
/// The design floor is 3:1 over bright art. ONE row misses it and is listed anyway rather than
/// hidden: detail's **facts line** is `TEXT_TERTIARY` (L=0.317), which needs α≈0.79 for 4.5:1 —
/// an essentially black corner. That is an INK decision, not a scrim one, and is deliberately
/// deferred; when the facts line moves to `TEXT_SECONDARY` over artwork its floor becomes 3.0
/// like the rest.
///
/// The people column was the second such row for exactly one day. Bottom-anchoring it moved its
/// worst case 74px up the frame, where the composite ground is ≈0.59 instead of ≈0.71, and at
/// tertiary that is 2.37:1 — so the row's floor was written down as 2.35 to match. **A contract
/// is not a measurement**: the entry below asserts the ordinary 3.0/2.5 again, and
/// `detail::PEOPLE_INK` is what meets it (one ink step, no scrim retune — the arithmetic for
/// why that is the cheap half of the trade is on that const).
#[test]
fn the_hero_text_reads_over_bright_artwork() {
    use crate::ui::consts::{MARGIN_X, SCR_W};
    let hc = crate::ui::detail_layout::hero_chain(
        // a two-line blurb — the shape `hero_chain`'s own doc is tuned on. Measuring is the one
        // thing the host cannot do, so the height is quoted, not computed.
        76.0, true,
    );
    let band = crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero);
    let home_col_r = MARGIN_X + crate::ui::landing_hero::COL_W; // 750 — the column's right end
    let det_col_r = MARGIN_X + crate::ui::detail_layout::HERO_TEXT_W; // 990 — the synopsis' wrap edge,
                                                               // and since nit 2 the title band's column too: a very wide wordmark runs the same 990.
    let people_r = SCR_W - MARGIN_X; // 1830 — the right-aligned people column's own edge

    // (label, x, y, ink, right wedge?, floor over BRIGHT art, floor over BLOWN WHITE)
    let rows: [(&str, f32, f32, [f32; 4], bool, f32, f32); 9] = [
        (
            "home title (right end)",
            home_col_r,
            home_title_cap_top(),
            theme::TEXT_PRIMARY,
            false,
            3.0,
            2.5,
        ),
        (
            "home title (at the margin)",
            MARGIN_X,
            home_title_cap_top(),
            theme::TEXT_PRIMARY,
            false,
            7.0,
            6.0,
        ),
        (
            "home kicker",
            500.0,
            home_title_band_top() + band + theme::space::MD,
            theme::TEXT_SECONDARY,
            false,
            3.0,
            2.5,
        ),
        // the blurb's FIRST line, at the top of its block — the weakest ground the run sees.
        // Both its ink and its block height are read from the shared hero synopsis.
        (
            "home synopsis",
            home_col_r,
            crate::ui::landing_hero::TEXT_BOTTOM - home_syn_h(),
            theme::TEXT_READING,
            false,
            3.0,
            2.5,
        ),
        (
            "detail title",
            det_col_r,
            detail_title_cap_top(),
            theme::TEXT_PRIMARY,
            true,
            3.0,
            2.5,
        ),
        (
            "detail meta",
            700.0,
            hc.meta_y,
            theme::TEXT_SECONDARY,
            true,
            3.0,
            2.5,
        ),
        (
            "detail synopsis",
            det_col_r,
            hc.syn_y,
            theme::TEXT_READING,
            true,
            3.0,
            2.5,
        ),
        // ⚠ the deferred ink decision — see the doc above
        (
            "detail facts",
            1270.0,
            hc.facts_y,
            theme::TEXT_TERTIARY,
            true,
            2.6,
            2.1,
        ),
        // The people column at its WORST case: the top line of the tallest block it can produce
        // (a wrapped credit over a wrapped cast list), `PEOPLE_MAX_LINES` above the buttons —
        // 74px higher than the old top-anchored block, where the right wedge is feathering in
        // from y=702 and supplies about half the alpha it did (0.092 against 0.178). It clears
        // the ordinary floor because its ink is `detail::PEOPLE_INK`, which is read from the
        // screen rather than restated here, so an ink change here is a test failure and not a
        // silent one.
        (
            "detail people (top line)",
            people_r,
            crate::ui::detail_layout::people_top(hc.btn_y, crate::ui::detail_layout::PEOPLE_MAX_LINES),
            crate::ui::detail_layout::PEOPLE_INK,
            true,
            3.0,
            2.5,
        ),
    ];

    for (label, x, y, ink, right, min_bright, min_white) in rows {
        let base = if label.starts_with("home") {
            crate::ui::landing_hero::base_scrim_a(y, 1.0)
        } else {
            crate::ui::detail_layout::base_scrim_a(y, 1.0)
        };
        let wedge = hero_scrim_a(x, 1.0);
        let rw = if right {
            hero_scrim_right_a(x, y, 1.0)
        } else {
            0.0
        };
        let total = 1.0 - (1.0 - base) * (1.0 - wedge) * (1.0 - rw);
        for (art, floor) in [(0.85f32, min_bright), (1.0, min_white)] {
            let before = contrast_over_art(ink, art, base);
            let after = contrast_over_art(ink, art, total);
            assert!(
                after >= floor,
                "{label} at ({x},{y}) over art {art}: {after:.2}:1, under its {floor}:1 floor (base {base:.3} + wedge {wedge:.3} + right {rw:.3})"
            );
            assert!(
                after > before,
                "{label} over art {art}: the wedge made it WORSE ({before:.2} → {after:.2})"
            );
        }
    }
    // Video-bound row. The title stays; prose has receded. Same curve, raised strength.
    {
        let x = det_col_r;
        let y = detail_title_cap_top();
        let strength = crate::ui::landing_hero::PREVIEW_FIELD;
        let base = crate::ui::detail_layout::base_scrim_a(y, 1.0);
        let wedge = hero_scrim_a(x, strength);
        let rw = hero_scrim_right_a(x, y, strength);
        let still = 1.0
            - (1.0 - base) * (1.0 - hero_scrim_a(x, 1.0)) * (1.0 - hero_scrim_right_a(x, y, 1.0));
        let total = 1.0 - (1.0 - base) * (1.0 - wedge) * (1.0 - rw);
        let after = contrast_over_art(theme::TEXT_PRIMARY, 0.85, total);
        let still_after = contrast_over_art(theme::TEXT_PRIMARY, 0.85, still);
        assert!(
            after >= 3.0,
            "detail title (video-bound) over bright art: {after:.2}:1, under 3:1"
        );
        assert!(
            after >= still_after,
            "the raised field must not weaken the title ({still_after:.2} → {after:.2})"
        );
    }
}
