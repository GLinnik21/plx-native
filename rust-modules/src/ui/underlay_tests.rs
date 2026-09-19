//! The underlay field's contract, all of it host-gradeable: the pipeline is pure arithmetic over
//! arrays and the one GL call is behind a `cfg(not(test))` seam, so nothing here needs a context.
//!
//! What these cases are FOR, in one line each: the grade may not drift from the four-corner wash's;
//! the low-pass may not invent or lose light; the reconstruction may not move a cell, overshoot a
//! cell pair, or step; a corner envelope must still produce the bilinear it replaces; a latch is
//! once; the field must be SPATIAL; and a dim at weight zero must be the scrim the app already drew.

use super::*;
use crate::ui::consts::{SCR_H, SCR_W};

fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps)
}

/// The corner sources a ground has to survive — the same hostile set
/// `widgets_ambient_ground_tests.rs` grades the wash against.
fn hostile() -> Vec<[f32; 3]> {
    vec![
        [1.0, 1.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.5, 0.5, 0.5],
        [0.12, 0.34, 0.56],
    ]
}

/// A field whose cells are whatever the caller says, latched, with no texture — every case below
/// that needs a live `UnderlayField` needs exactly this much of one.
fn latched(cells: [[f32; 3]; N]) -> UnderlayField {
    let mut f = UnderlayField::new();
    f.cells = cells;
    f.latched = true;
    f
}

/// **The grade is the wash's grade, cell for cell.**
///
/// This is the test that makes stage B's migration a change of SHAPE and not of palette: a ground
/// built from 120 cells and a ground built from four corners must answer the same colour for the
/// same source, or the same page would change hue the day it switched mechanism. It is exact, not
/// approximate, because both sides call `AmbientWash::keyed_one` — a near-miss here would mean
/// somebody re-typed the cap or the lean.
#[test]
fn the_ground_grade_is_the_washs_own_grade_cell_for_cell() {
    for c in hostile() {
        let mine = graded(c.map(crate::gfx::lin), Grade::Ground).map(crate::gfx::enc);
        let wash = AmbientWash::keyed([c; 4], [AmbientWash::GROUND_W; 4])[0];
        assert!(
            close(mine, [wash[0], wash[1], wash[2]], 1e-5),
            "grade drifted from AmbientWash::keyed for {c:?}: {mine:?} vs {wash:?}"
        );
    }
}

/// `Grade::Dim` is the IDENTITY, and that is a decision rather than an omission: a dim is drawn
/// over the page it sampled, so capping and leaning it would grade the same light twice.
#[test]
fn the_dim_grade_leaves_the_sampled_light_alone() {
    for c in hostile() {
        let lin = c.map(crate::gfx::lin);
        assert_eq!(graded(lin, Grade::Dim), lin, "Dim must not touch {c:?}");
    }
}

/// **The kernel's literal table is the Gaussian at [`SIGMA`].** It is written out so the render
/// path makes no `exp` call (`ci/allow/libm.txt`); this makes sure the literals say what the
/// constant says, so retuning SIGMA without regenerating the table fails here.
#[test]
fn the_kernel_table_is_the_gaussian_at_sigma() {
    for (d, &w) in GAUSS.iter().enumerate() {
        let want = (-((d * d) as f64) / (2.0 * f64::from(SIGMA) * f64::from(SIGMA))).exp();
        assert!((f64::from(w) - want).abs() < 1e-6, "tap {d}: {w} vs {want}");
    }
    let k = kernel();
    assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    assert_eq!(k[0], k[2 * RADIUS]);
}

/// **The low-pass may not invent or lose light.** A flat field is preserved exactly (the kernel is
/// normalised) and a ramp keeps its mean (edge REPLICATION biases the two ends in opposite
/// directions by the same amount, so the bias cancels over the grid).
#[test]
fn the_low_pass_preserves_a_flat_field_and_the_grids_mean() {
    let flat = [[0.30f32, 0.41, 0.52]; N];
    for (a, b) in low_pass(&flat).iter().zip(flat.iter()) {
        assert!(close(*a, *b, 1e-6), "a flat field moved: {a:?} vs {b:?}");
    }

    let ramp: [[f32; 3]; N] = std::array::from_fn(|i| {
        let (col, row) = (i % W, i / W);
        let u = col as f32 / (W - 1) as f32;
        let v = row as f32 / (H - 1) as f32;
        [u, v, 0.5 * (u + v)]
    });
    let mean = |g: &[[f32; 3]; N]| -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        for c in g {
            for (a, v) in acc.iter_mut().zip(c) {
                *a += v / N as f32;
            }
        }
        acc
    };
    let (before, after) = (mean(&ramp), mean(&low_pass(&ramp)));
    assert!(
        close(before, after, 1e-5),
        "the low-pass moved the grid's mean: {before:?} -> {after:?}"
    );
}

/// **Reconstruction is INTERPOLATION**: at a cell's own centre it returns that cell, untouched.
/// A reconstruction that smooths its own knots is a second low-pass nobody asked for, and it would
/// pull every colour in the field toward the page's mean.
#[test]
fn the_reconstruction_interpolates_the_cell_centres() {
    let cells: [[f32; 3]; N] = std::array::from_fn(|i| {
        let (col, row) = (i % W, i / W);
        [
            (col as f32 * 0.061).fract(),
            (row as f32 * 0.137 + 0.2).fract(),
            ((col * H + row) as f32 * 0.029).fract(),
        ]
    });
    for i in 0..N {
        let (col, row) = (i % W, i / W);
        let u = (col as f32 + 0.5) / W as f32;
        let v = (row as f32 + 0.5) / H as f32;
        let got = reconstruct(&cells, u, v);
        assert!(
            close(got, cells[i], 1e-4),
            "cell {col},{row} moved under reconstruction: {got:?} vs {:?}",
            cells[i]
        );
    }
}

/// **A cubic may not invent a colour the page does not contain.** Catmull-Rom overshoots a step,
/// and an overshoot here is not ringing you squint at — it is a 128px band of a colour that is not
/// in the frame. The bracket clamp is what stops it, and this is the case that would notice if it
/// were removed (it did, when the clamp was taken out to check).
///
/// The input is a HARD EDGE in both axes — quadrants — and not a checkerboard, which is the
/// obvious worst case and the wrong one: alternating knots give every Catmull-Rom tangent a zero
/// centred difference, and a Hermite segment with zero tangents is monotone. The overshoot lives
/// in the flat segment NEXT TO a step, where one tangent sees the step and the other does not.
#[test]
fn the_reconstruction_cannot_overshoot_the_cells_that_bracket_it() {
    let cells: [[f32; 3]; N] = std::array::from_fn(|i| {
        let (col, row) = (i % W, i / W);
        let v = if (col < W / 2) ^ (row < H / 2) { 0.95 } else { 0.05 };
        [v, 1.0 - v, v]
    });
    let at = |col: usize, row: usize, ch: usize| cells[row * W + col][ch];
    let steps = 7;
    for row in 0..H - 1 {
        for col in 0..W - 1 {
            for s in 1..steps {
                for t in 1..steps {
                    let fx = s as f32 / steps as f32;
                    let fy = t as f32 / steps as f32;
                    let u = (col as f32 + 0.5 + fx) / W as f32;
                    let v = (row as f32 + 0.5 + fy) / H as f32;
                    let got = reconstruct(&cells, u, v);
                    for (ch, g) in got.iter().enumerate() {
                        let quad = [
                            at(col, row, ch),
                            at(col + 1, row, ch),
                            at(col, row + 1, ch),
                            at(col + 1, row + 1, ch),
                        ];
                        let lo = quad.iter().cloned().fold(f32::INFINITY, f32::min);
                        let hi = quad.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                        assert!(
                            *g >= lo - 1e-5 && *g <= hi + 1e-5,
                            "overshoot at ({u},{v}) ch{ch}: {g} outside {lo}..{hi}"
                        );
                    }
                }
            }
        }
    }
}

/// **No STAIRCASE.** The defect this whole module exists to avoid is a field that reads as flat
/// plateaus with creases between them, so a ramp must come out of the reconstruction advancing by
/// roughly the same amount at every one of the 60 output texels. A plateau-and-jump reconstruction
/// fails this immediately: its steps are 0 and then several times the mean.
#[test]
fn a_ramp_reconstructs_without_a_step() {
    let dark = [0.10f32, 0.10, 0.10];
    let light = [0.60f32, 0.60, 0.60];
    let cells = cells_from_corners([dark, light, light, dark], Grade::Dim);
    let row: Vec<f32> = (0..TEX_W)
        .map(|i| {
            let u = (i as f32 + 0.5) / TEX_W as f32;
            crate::gfx::enc(reconstruct(&cells, u, 0.5)[0])
        })
        .collect();
    let ideal = (row[TEX_W - 1] - row[0]) / (TEX_W - 1) as f32;
    assert!(ideal > 0.0, "the ramp must rise: {row:?}");
    for (i, w) in row.windows(2).enumerate() {
        let step = w[1] - w[0];
        assert!(
            step >= 0.5 * ideal && step <= 2.0 * ideal,
            "texel {i}: step {step} is not within 0.5..2 of the uniform {ideal}"
        );
    }
}

/// **A corner envelope still produces the bilinear it stands in for.** This is the compatibility
/// contract for the CPU source: the video-plane and pre-Home cases hand four corners to a field
/// where they used to hand them to an `AmbientWash`, and the picture must not change.
///
/// The epsilon is not slop, it is two named quantities: the cells store LINEAR light, so a cubic
/// through them reconstructs a curve where the wash interpolates a straight line in display codes,
/// and the outer half-cell is reached by linear extrapolation rather than by a knot. Together they
/// measured under a TENTH of an 8-bit code across this grid of points (2026-09-19); the assertion
/// holds them to a quarter of one, well inside the 1/255 a panel can show.
#[test]
fn a_corner_envelope_reproduces_the_bilinear_wash_it_stands_in_for() {
    let corners = [
        [0.90, 0.20, 0.10],
        [0.10, 0.80, 0.20],
        [0.20, 0.30, 0.90],
        [0.70, 0.70, 0.10],
    ];
    let mut field = UnderlayField::new();
    field.latch_from_corners(corners, Grade::Ground);

    let mut wash = AmbientWash::flat(theme::SURFACE_APP);
    wash.jump(AmbientWash::keyed(corners, [AmbientWash::GROUND_W; 4]));

    for &u in &[0.05f32, 0.25, 0.5, 0.75, 0.95] {
        for &v in &[0.05f32, 0.25, 0.5, 0.75, 0.95] {
            let (x, y) = (u * SCR_W, v * SCR_H);
            let mine = field.sample(x, y);
            let theirs = wash.sample(Rect::FULL, x, y);
            assert!(
                close(mine, theirs, 0.25 / 255.0),
                "({u},{v}): field {mine:?} vs wash {theirs:?}"
            );
        }
    }
}

/// **A latch is once, and `reset` is the only way back.** The reason is that `latch_from_frame` is
/// called from a `draw` that runs every frame: without idempotence the ground would re-sample
/// itself compositing over itself, and a route's atmosphere would drift a little darker every
/// frame it was up.
#[test]
fn a_latch_is_idempotent_and_reset_re_arms_it() {
    let a = [[0.9, 0.1, 0.1], [0.9, 0.1, 0.1], [0.9, 0.1, 0.1], [0.9, 0.1, 0.1]];
    let b = [[0.1, 0.1, 0.9], [0.1, 0.1, 0.9], [0.1, 0.1, 0.9], [0.1, 0.1, 0.9]];

    let mut f = UnderlayField::new();
    assert!(!f.is_latched());
    f.latch_from_corners(a, Grade::Dim);
    assert!(f.is_latched());
    let first = f.cells;

    f.latch_from_corners(b, Grade::Dim);
    assert_eq!(f.cells, first, "a second latch must not move a latched field");
    // And the frame path takes the same early exit — which is also why this case can call it at
    // all: `gfx::sample_underlay_field` is never reached, so no GL context is needed.
    assert!(f.latch_from_frame(Grade::Ground), "a latched field is already an answer");
    assert_eq!(f.cells, first);

    f.reset();
    assert!(!f.is_latched());
    f.latch_from_corners(b, Grade::Dim);
    assert!(f.is_latched());
    assert_ne!(f.cells, first, "after a reset the next latch is the one that takes");
}

/// **THE POINT OF THE WHOLE MECHANISM: the field is SPATIAL.** A four-corner envelope can say a
/// page is greenish; only this can say the green is at the bottom LEFT. If this case ever passes
/// for a field that has collapsed to its own mean, the module has become an expensive way to store
/// one colour.
#[test]
fn the_field_samples_greener_where_the_green_is() {
    let raw: [[f32; 3]; N] = std::array::from_fn(|i| {
        let (col, row) = (i % W, i / W);
        let u = col as f32 / (W - 1) as f32;
        let v = row as f32 / (H - 1) as f32;
        // green at the bottom-left (u=0, v=1), red at the top-right (u=1, v=0)
        let green = (1.0 - u) * v;
        let red = u * (1.0 - v);
        [0.05 + 0.85 * red, 0.05 + 0.85 * green, 0.05]
    });
    let f = latched(cells_from_frame(&raw, Grade::Dim));

    let bl = f.sample(SCR_W * 0.12, SCR_H * 0.88);
    let tr = f.sample(SCR_W * 0.88, SCR_H * 0.12);
    assert!(
        bl[1] - bl[0] > 0.2,
        "the bottom-left corner must read GREEN, got {bl:?}"
    );
    assert!(
        tr[0] - tr[1] > 0.2,
        "the top-right corner must read RED, got {tr:?}"
    );
    assert!(
        bl[1] > tr[1] && tr[0] > bl[0],
        "the two corners must not have swapped: bl {bl:?}, tr {tr:?}"
    );
}

/// **A dim at weight zero is the scrim the app already draws, to the bit.**
///
/// Not "close enough": a field tinted to black still runs `plx_dither`, which would put ±1 LSB of
/// noise on a surface whose entire content is one code — visible as grain on a full-screen scrim,
/// and bought for nothing. So weight 0 is a different DRAW, and so is an unlatched field, which is
/// what every one of these surfaces is on its first frame.
#[test]
fn a_dim_at_weight_zero_is_todays_flat_scrim_to_the_bit() {
    for &a in &[0.0f32, 0.25, 0.58, 1.0] {
        assert_eq!(
            plan(true, 0.0, a),
            Draw::Flat(theme::scrim_black(a)),
            "weight 0 must be the flat scrim at alpha {a}"
        );
        assert_eq!(
            plan(false, 0.7, a),
            Draw::Flat(theme::scrim_black(a)),
            "an unlatched field has nothing to show at alpha {a}"
        );
        assert_eq!(
            plan(true, 0.7, a),
            Draw::Field([0.7, 0.7, 0.7, a]),
            "a weighted dim is mix(SCRIM_BLACK_INK, field, w), i.e. one multiply"
        );
    }
    // The weight is a fraction of the ink, so it clamps rather than amplifying the field.
    assert_eq!(plan(true, 2.0, 1.0), Draw::Field([1.0, 1.0, 1.0, 1.0]));
    assert_eq!(plan(true, -1.0, 1.0), Draw::Flat(theme::scrim_black(1.0)));
}

/// The texture is the grid magnified 4x per axis and OPAQUE — coverage is the tint's business, so
/// an alpha in here would be a second, silent one.
#[test]
fn the_texture_is_sixty_by_thirty_two_and_opaque() {
    assert_eq!((TEX_W, TEX_H), (60, 32));
    assert_eq!((W, H, N), (15, 8, 120));
    let px = texture_rgba(&[[0.25f32, 0.5, 0.75]; N]);
    assert_eq!(px.len(), TEX_W * TEX_H * 4);
    assert!(px.chunks_exact(4).all(|p| p[3] == 255), "every texel is opaque");
    // A flat field magnifies to a flat texture — the reconstruction adds no structure of its own.
    let first = &px[0..3];
    assert!(
        px.chunks_exact(4).all(|p| p[0..3] == *first),
        "a flat field must not acquire structure in the reconstruction"
    );
}

// ── The panel material (`draw_panel` / `panel_plan`) ─────────────────────────────────────────────

/// A field latched through the REAL adopt path — cells, texture bytes and the luma table the
/// panel's ceiling is solved against — from one flat display colour.
fn latched_flat(rgb: [f32; 3]) -> UnderlayField {
    let mut f = UnderlayField::new();
    f.latch_sampled(&[rgb; N], Grade::Dim);
    f
}

/// **A panel's window into the field is its SCREEN rect over the screen size** — the only UV rect
/// under which the green at the page's bottom-left is still at the panel's bottom-left. Graded
/// through a TRANSLATED painter as well, because the rect a panel is drawn at is the one its entry
/// slide has moved, and a window keyed to the rest rect would slide the field WITH the panel.
#[test]
fn a_panels_field_window_is_its_screen_rect_over_the_screen_size() {
    let f = latched_flat([0.2, 0.3, 0.4]);
    let r = Rect::new(630.0, 202.0, 660.0, 540.0);
    let want = [630.0 / SCR_W, 202.0 / SCR_H, 660.0 / SCR_W, 540.0 / SCR_H];
    assert_eq!(panel_uv(r), want);
    match f.panel_plan(r, theme::underlay::PANEL_TINT) {
        PanelDraw::Field { uv, .. } => assert_eq!(uv, want),
        PanelDraw::Flat => panic!("a latched field must draw the field"),
    }
    // Mid-slide: the painter carries the entry translate, and the window follows the DRAWN rect.
    let p = Painter::root().translate(0.0, 12.0);
    let (_, moved, _) = p.to_screen(r);
    let PanelDraw::Field { uv, .. } = f.panel_plan(moved, theme::underlay::PANEL_TINT) else {
        panic!("latched");
    };
    assert_eq!(uv, [630.0 / SCR_W, 214.0 / SCR_H, 660.0 / SCR_W, 540.0 / SCR_H]);
    // The full screen is the whole texture.
    assert_eq!(panel_uv(Rect::FULL), [0.0, 0.0, 1.0, 1.0]);
}

/// **Unlatched is the FLAT sheet, never a blank.** The first frame a panel is up may have no
/// field (the latch is taken inside that frame's page pass, and a video plane refuses it
/// outright), and a `false` from `draw_panel` is what makes `widgets::panel_ground` lay down
/// `PANEL_TOP`/`PANEL_BOT`.
#[test]
fn an_unlatched_field_draws_the_flat_panel_material() {
    let f = UnderlayField::new();
    let r = Rect::new(100.0, 100.0, 400.0, 300.0);
    assert_eq!(f.panel_plan(r, theme::underlay::PANEL_TINT), PanelDraw::Flat);
    assert!(
        !f.draw_panel(Painter::root(), r, 32.0, theme::underlay::PANEL_TINT),
        "no field — the caller must draw the flat sheet"
    );
    // And a reset field is unlatched again.
    let mut g = latched_flat([0.5, 0.5, 0.5]);
    g.reset();
    assert_eq!(g.panel_plan(r, theme::underlay::PANEL_TINT), PanelDraw::Flat);
}

/// **The ceiling is `ground_capped`'s rule, per panel**: a dark page keeps the full `PANEL_TINT`,
/// a white one is lowered until its brightest texel under the panel lands on `PANEL_LUMA_MAX` —
/// and only the texels UNDER the panel count, so a bright edge elsewhere on the page does not dim
/// a panel standing on dark ground.
#[test]
fn the_panel_tint_is_capped_by_the_brightest_texel_under_the_panel() {
    let w = theme::underlay::PANEL_TINT;
    let cap = theme::underlay::PANEL_LUMA_MAX;
    let r = Rect::new(630.0, 270.0, 660.0, 540.0);
    let tint = |f: &UnderlayField, r: Rect| match f.panel_plan(r, w) {
        PanelDraw::Field { tint, .. } => tint,
        PanelDraw::Flat => panic!("latched"),
    };

    let dark = tint(&latched_flat([0.05, 0.05, 0.08]), r);
    assert_eq!(dark, [w, w, w, 1.0], "a dark page is under the ceiling");

    let white = tint(&latched_flat([1.0, 1.0, 1.0]), r);
    assert!(white[0] < w, "a white page must be lowered, got {white:?}");
    assert!(
        (white[0] - cap).abs() < 0.01,
        "white × k lands on PANEL_LUMA_MAX: {} vs {cap}",
        white[0]
    );
    assert_eq!(white[3], 1.0, "the field is opaque; coverage is the painter's");

    // Bright only in the far LEFT column: a panel on the right half never sees it.
    let raw: [[f32; 3]; N] =
        std::array::from_fn(|i| if i % W == 0 { [1.0; 3] } else { [0.04; 3] });
    let mut edge = UnderlayField::new();
    edge.latch_sampled(&raw, Grade::Dim);
    let right = Rect::new(SCR_W * 0.55, 200.0, SCR_W * 0.35, 500.0);
    assert_eq!(
        tint(&edge, right),
        [w, w, w, 1.0],
        "light outside the panel is not its business"
    );
    let left = Rect::new(0.0, 200.0, 400.0, 500.0);
    assert!(tint(&edge, left)[0] < w, "light under the panel is");
}

/// WCAG relative luminance over display codes — `gfx::lin` is the same IEC transfer.
fn rel(c: [f32; 3]) -> f32 {
    0.2126 * crate::gfx::lin(c[0]) + 0.7152 * crate::gfx::lin(c[1]) + 0.0722 * crate::gfx::lin(c[2])
}
fn ratio(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y) = (rel(a), rel(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// **Text on a panel keeps its floors at both ends of the page's range.** The composite is what
/// the GPU blends, in display codes: the field × k (opaque), then the panel frost over it at the
/// panel material's density. Primary and secondary ink must clear 4.5:1 and tertiary 3:1 over a
/// WHITE page (where the ceiling does the work), a BLACK one (where the frost's own neutral does)
/// and a saturated yellow one, on both frost stops.
#[test]
fn text_contrast_floors_hold_over_a_bright_and_a_dark_underlay() {
    let a = theme::PANEL_MATERIAL.frost();
    let rgb = |c: [f32; 4]| [c[0], c[1], c[2]];
    for under in [[1.0f32, 1.0, 1.0], [0.0, 0.0, 0.0], [1.0, 0.85, 0.2]] {
        let f = latched_flat(under);
        let PanelDraw::Field { tint, .. } = f.panel_plan(
            Rect::new(630.0, 270.0, 660.0, 540.0),
            theme::underlay::PANEL_TINT,
        ) else {
            panic!("latched");
        };
        let field = [0, 1, 2].map(|i| under[i] * tint[i]);
        for stop in [theme::PANEL_FROST_TOP, theme::PANEL_FROST_BOT] {
            let ground = [0, 1, 2].map(|i| stop[i] * a + field[i] * (1.0 - a));
            for (ink, floor, name) in [
                (theme::TEXT_PRIMARY, 4.5, "primary"),
                (theme::TEXT_SECONDARY, 4.5, "secondary"),
                (theme::TEXT_TERTIARY, 3.0, "tertiary"),
            ] {
                let c = ratio(rgb(ink), ground);
                assert!(
                    c >= floor,
                    "{name} ink over {under:?} reads {c:.2}:1 < {floor}:1 (ground {ground:?})"
                );
            }
        }
    }
}
