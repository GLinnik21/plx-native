//! Home's hero scrim (owner's CW-13 v2 board): the three stacked terms of `theme::home_scrim`, the
//! one-pass ground that carries them, and the legibility contract graded at the new text positions.
//!
//! Pure math, no GL: the field the GPU's vertex shader evaluates is `GroundScrim::at`
//! (`gfx/scrim.rs`), which is what these read, and the mesh it is drawn on is walked for real
//! (`GroundScrim::mesh_error`).

use super::*;
use crate::consts::{MARGIN_X, SCR_H, SCR_W};
use super::test_support::*;

/// A `(t, alpha)` table at `t`, written independently of the shader's sum-of-ramps form: find the
/// segment and interpolate, flat outside.
fn table_at(stops: &[(f32, f32)], t: f32) -> f32 {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let ((t0, a0), (t1, a1)) = (w[0], w[1]);
        if t <= t1 {
            return a0 + (a1 - a0) * (t - t0) / (t1 - t0);
        }
    }
    stops[stops.len() - 1].1
}

/// The board's three terms at `(x, y)`, from the stop tables, stacked: the closed form of the design
/// itself, with none of the encoding the shader uses.
fn board_a(x: f32, y: f32, k: f32) -> f32 {
    use theme::home_scrim as h;
    let top = k * table_at(&h::TOP, y / h::TOP_H);
    let (qx, qy) = ((x - h::RADIAL_CENTRE.0) / h::RADIAL_RADII.0, (y - h::RADIAL_CENTRE.1) / h::RADIAL_RADII.1);
    let radial = k * table_at(&h::RADIAL, (qx * qx + qy * qy).sqrt());
    let bottom = k * table_at(&h::BOTTOM, (y - h::BOTTOM_Y) / h::BOTTOM_H);
    1.0 - (1.0 - top) * (1.0 - radial) * (1.0 - bottom)
}

/// **The scrim is the board's three tables at the board's stops.** Each term alone at its own knots
/// (the numbers the owner drew), and the stack where they overlap.
#[test]
fn the_scrim_is_the_boards_stops_at_the_boards_sample_points() {
    let a = |x: f32, y: f32| home_scrim_a(x, y, 1.0);
    let near = |got: f32, want: f32, what: &str| assert!((got - want).abs() < 1e-5, "{what}: {got} vs {want}");
    // the top band alone, down the right edge (x = 1900 is past the radial's reach, 560 + 1020, and
    // the bottom band has not begun)
    near(a(1900.0, 0.0), 0.50, "top band at the panel's top");
    near(a(1900.0, 0.18 * 240.0), 0.40, "top band at 18%");
    near(a(1900.0, 0.40 * 240.0), 0.26, "top band at 40%");
    near(a(1900.0, 0.65 * 240.0), 0.12, "top band at 65%");
    near(a(1900.0, 0.85 * 240.0), 0.03, "top band at 85%");
    assert_eq!(a(1900.0, 240.0), 0.0, "…and nothing at its foot");
    // the radial alone, on its centre row (above the bottom band, below the top one)
    near(a(560.0, 590.0), 0.78, "radial at its centre");
    near(a(560.0 + 0.30 * 1020.0, 590.0), 0.70, "radial at .30");
    near(a(560.0 + 0.55 * 1020.0, 590.0), 0.48, "radial at .55");
    near(a(560.0 + 0.78 * 1020.0, 590.0), 0.22, "radial at .78");
    near(a(560.0 + 0.92 * 1020.0, 590.0), 0.06, "radial at .92");
    assert_eq!(a(560.0 + 1020.0, 590.0), 0.0, "…and nothing at its edge");
    near(a(560.0, 590.0 + 0.55 * 460.0), 0.48 + (1.0 - 0.48) * table_at(&theme::home_scrim::BOTTOM, (843.0 - 640.0) / 440.0), "the ellipse's other axis, with the bottom band under it");
    // the bottom band alone, down the right edge
    near(a(1900.0, 640.0), 0.0, "bottom band at its top");
    near(a(1900.0, 640.0 + 0.12 * 440.0), 0.02, "bottom band at 12%");
    near(a(1900.0, 640.0 + 0.25 * 440.0), 0.08, "bottom band at 25%");
    near(a(1900.0, 640.0 + 0.40 * 440.0), 0.24, "bottom band at 40%");
    near(a(1900.0, 640.0 + 0.55 * 440.0), 0.46, "bottom band at 55%");
    near(a(1900.0, 640.0 + 0.70 * 440.0), 0.68, "bottom band at 70%");
    near(a(1900.0, 640.0 + 0.85 * 440.0), 0.84, "bottom band at 85%");
    near(a(1900.0, 1080.0), 0.93, "bottom band at the foot");
    // stacked: all three terms alive at once is not reachable (the top band ends at 240, the bottom
    // starts at 640), so the pairs that are: top + radial, radial + bottom
    near(a(700.0, 200.0), board_a(700.0, 200.0, 1.0), "top over radial");
    near(a(560.0, 760.0), board_a(560.0, 760.0, 1.0), "radial under the bottom band");
    // every point of the panel, against the table form
    for iy in 0..=54 {
        for ix in 0..=96 {
            let (x, y) = (ix as f32 * 20.0, iy as f32 * 20.0);
            near(a(x, y), board_a(x, y, 1.0), &format!("({x},{y})"));
        }
    }
}

/// **The hero's fade scales each term, then they stack**: at half strength the radial's heart is
/// `0.39`, not half of `0.78`'s stack with anything else, and at none there is no scrim at all.
#[test]
fn the_hero_fade_scales_each_term() {
    for k in [0.0f32, 0.25, 0.5, 1.0] {
        for (x, y) in [(0.0, 0.0), (560.0, 590.0), (700.0, 760.0), (1500.0, 1000.0)] {
            assert!((home_scrim_a(x, y, k) - board_a(x, y, k)).abs() < 1e-5, "k={k} ({x},{y})");
        }
    }
    assert!((home_scrim_a(560.0, 590.0, 0.5) - 0.39).abs() < 1e-5);
    assert_eq!(home_scrim_a(560.0, 590.0, 0.0), 0.0);
    assert_eq!(home_scrim_a(560.0, 590.0, -1.0), 0.0, "a negative fade is nothing, never an inverted scrim");
    assert_eq!(home_scrim_a(560.0, 590.0, 3.0), home_scrim_a(560.0, 590.0, 1.0), "…and it saturates at the hero's own full strength");
}

/// **The painted field is within a stated bound of the closed form.** The GPU evaluates the scrim at
/// the vertices of a 16 x 16 mesh and interpolates linearly between them; the ground cuts the panel
/// into 480 x 270 cells so a mesh cell is 30 x 17 px. This walks the REAL mesh at that cut, at full
/// strength and mid-fade, and holds the worst distance to one percent of alpha (2.55 codes of ink
/// alpha; over a bright photograph under 2 levels of 8-bit luminance, and at the radial's centre,
/// where it is largest, the painted field is the LOWER of the two) and shows what the uncut panel
/// would have been.
#[test]
fn the_painted_field_stays_within_one_percent_of_the_closed_form() {
    let s = home_scrim(1.0).field;
    let panel = [0.0, 0.0, SCR_W, SCR_H];
    let cells = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).count();
    let (cols, rows) = ((SCR_W / SCRIM_CELL_W).ceil() as usize, (SCR_H / SCRIM_CELL_H).ceil() as usize);
    assert_eq!(cells, cols * rows);
    let uncut = s.mesh_error(panel, 1, 1);
    let cut = s.mesh_error(panel, cols, rows);
    let half = home_scrim(0.5).field.mesh_error(panel, cols, rows);
    eprintln!("scrim mesh error: uncut {uncut:.4}, cut {cols}x{rows} {cut:.4} ({:.2} codes), half strength {half:.4}", cut * 255.0);
    assert!(cut <= 0.01, "the cut field is {:.3} codes of alpha from the closed form", cut * 255.0);
    assert!(half <= cut, "a fade only scales the error down");
    assert!(uncut > 4.0 * cut, "the cut is what buys the bound: {uncut} -> {cut}");
}

/// The cells tile the panel exactly once: no crack for the clear colour, no row painted twice, and
/// the edges are the same float on both sides.
#[test]
fn the_scrim_cells_tile_the_panel_exactly() {
    let cells: Vec<Rect> = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).collect();
    let area: f32 = cells.iter().map(|c| c.w * c.h).sum();
    assert_eq!(area, SCR_W * SCR_H);
    assert!(cells.iter().all(|c| c.w <= SCRIM_CELL_W && c.h <= SCRIM_CELL_H && c.w > 0.0 && c.h > 0.0));
    for a in &cells {
        for b in &cells {
            if a.x + a.w == b.x && a.y == b.y {
                assert_eq!(a.h, b.h, "neighbours in a row are as tall");
            }
        }
    }
    // an odd rect, for the edge arithmetic: the far edge is the rect's own
    let r = Rect::new(13.5, 7.25, 1000.0, 333.0);
    let c: Vec<Rect> = cut_cells(r, 480.0, 270.0).collect();
    assert_eq!(c.len(), 3 * 2);
    assert_eq!(c.last().map(|c| (c.x + c.w, c.y + c.h)), Some((r.x + r.w, r.y + r.h)));
    assert_eq!(cut_cells(r, f32::INFINITY, f32::INFINITY).count(), 1, "no scrim: the rect whole");
}

/// What a recording painter was handed while `f` ran: `(command tag, screen rect)` in draw order.
fn handed(f: impl FnOnce(Painter)) -> Vec<(u64, Rect)> {
    crate::draw_census::capture(|| f(Painter::recording()))
}

/// **The layered scrim is the ground's own cells**, so a frame that cannot carry it in the pass (a
/// hero slide) draws the same field in the same pieces, as the scrim layer (tag 26).
#[test]
fn the_layered_scrim_is_the_grounds_cells() {
    let got = handed(|p| scrim_layer(p, home_scrim(1.0)));
    let want: Vec<(u64, Rect)> = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).map(|c| (26, c)).collect();
    assert_eq!(got, want);
}

/// **`draw_ground` hands the scrim to the ground in cells exactly when the ground can carry it**
/// (`wash_ink_ok`, as its ramp always was), and otherwise hands it back for the caller's layer. No
/// scrim and no ramp is the plain wash in one piece, as before.
#[test]
fn draw_ground_carries_the_scrim_in_cells_or_hands_it_back() {
    let mut w = AmbientWash::flat([0.0; 4]);
    w.jump([[0.2, 0.3, 0.6, 1.0], [0.5, 0.2, 0.3, 1.0], [0.1, 0.4, 0.2, 1.0], [0.3, 0.3, 0.3, 1.0]]);
    let inked = plx_gfx::gfx::wash_ink_ok();
    let mut took = !inked;
    let got = handed(|p| took = w.draw_ground(p, Rect::FULL, None, None, Some(home_scrim(1.0))));
    let cells: Vec<Rect> = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).collect();
    if inked {
        assert_eq!(got, cells.iter().map(|&c| (23, c)).collect::<Vec<_>>(), "one inked mesh a cell");
    } else {
        assert_eq!(got, [(16, Rect::FULL)], "no program: the plain wash, the scrim handed back");
    }
    assert_eq!(took, inked, "taken exactly when the wash can carry it");
    let mut took = true;
    assert_eq!(handed(|p| took = w.draw_ground(p, Rect::FULL, None, None, None)), [(16, Rect::FULL)]);
    assert!(!took, "nothing to carry, nothing taken");
}

// ── Legibility at the new text positions ────────────────────────────────────────────────────────

/// The worst contrast `ink` gets over `art` along the horizontal run `x0..=x1` and the vertical span
/// `y0..=y1` of a text box, under the scrim at full strength (the scrim is smooth, so a grid of
/// points finds its weakest corner).
fn worst_over(ink: [f32; 4], art: f32, (x0, x1): (f32, f32), (y0, y1): (f32, f32)) -> (f32, f32, f32) {
    let mut worst = (f32::INFINITY, x0, y0);
    for ix in 0..=16 {
        for iy in 0..=4 {
            let (x, y) = (x0 + (x1 - x0) * ix as f32 / 16.0, y0 + (y1 - y0) * iy as f32 / 4.0);
            let c = contrast_over_art(ink, art, home_scrim_a(x, y, 1.0));
            if c < worst.0 {
                worst = (c, x, y);
            }
        }
    }
    worst
}

/// **Home's copy reads over bright artwork, at the positions the ladder puts it.** Every line the
/// hero draws on the artwork (the episode label, the two synopsis lines, the title's fallback type and the
/// Continue Watching footer; the top navigation stands on its own glass and is not graded here), at its WORST point (the whole box: the weakest corner of the run, not the
/// margin), over a bright backdrop (encoded 0.85: a blown highlight) and blown white (1.0, where the
/// design only promises a floor). The positions are read from the ladder itself (`home_rows`), so a
/// layout change updates this contract rather than invalidating it.
///
/// The floors are the ones the old rows were graded to (3.0 over bright art, 2.5 over blown white).
/// **If the board's curve fails one, the answer is not to bend the board's numbers or the floor:** the
/// failing position and alpha are what this prints.
#[test]
fn home_copy_reads_over_bright_artwork_at_the_ladders_positions() {
    use crate::landing_hero::{home_rows, COL_W};
    let band = crate::hero_logo::band_h(crate::hero_logo::LogoRung::Hero);
    let line_h = crate::hero_syn_h(1);
    let col = (MARGIN_X, MARGIN_X + COL_W);
    let syn = (MARGIN_X, MARGIN_X + theme::hero::HOME_SYNOPSIS_W);
    // the three block shapes a hero has: an episode's one bold line, a synopsis of two lines, of one
    let episode = home_rows(band, Some(line_h));
    let two = home_rows(band, Some(crate::hero_syn_h(2)));
    let one = home_rows(band, Some(line_h));
    // the title's text fallback is baselined on its band's foot (`hero_logo::place`); a clearLogo
    // paints higher, which is art riding the scrim rather than an anchor this closed form can speak
    // for. Two display lines: the first cap top through the second line's baseline.
    let title_foot = |s: &crate::landing_hero::HomeStack| s.title.y + s.title.h;
    let title_cap_top = |s: &crate::landing_hero::HomeStack| title_foot(s) - HERO_CAP_H;
    let block = |s: &crate::landing_hero::HomeStack| s.block.expect("a block").y;
    let tile_foot = crate::consts::PEEK_Y + crate::consts::CARD_DY + crate::card_row::RowStyle::STILL.h;
    // (label, x run, y span, ink, floor over BRIGHT art, floor over BLOWN WHITE)
    let rows: [(&str, (f32, f32), (f32, f32), [f32; 4], f32, f32); 7] = [
        ("episode label", col, (block(&episode), block(&episode) + line_h), theme::TEXT_PRIMARY, 3.0, 2.5),
        ("synopsis, two lines", syn, (block(&two), block(&two) + crate::hero_syn_h(2)), theme::TEXT_READING, 3.0, 2.5),
        ("synopsis, one line", syn, (block(&one), block(&one) + line_h), theme::TEXT_READING, 3.0, 2.5),
        ("title over an episode label", col, (title_cap_top(&episode), title_foot(&episode)), theme::TEXT_PRIMARY, 3.0, 2.5),
        ("title over two synopsis lines", col, (title_cap_top(&two), title_foot(&two)), theme::TEXT_PRIMARY, 3.0, 2.5),
        ("title over one synopsis line", col, (title_cap_top(&one), title_foot(&one)), theme::TEXT_PRIMARY, 3.0, 2.5),
        // the Continue Watching row at rest: its footer line stands under the art, on the bottom band
        (
            "continue footer",
            (MARGIN_X, MARGIN_X + crate::still_footer::FOOTER_BELOW_INNER_W),
            (tile_foot + theme::STILL_FOOTER_DROP, tile_foot + theme::STILL_FOOTER_DROP + crate::still_footer::FOOTER_LINE_H),
            theme::TEXT_SECONDARY,
            3.0,
            2.5,
        ),
    ];
    let mut failed = Vec::new();
    for (label, xs, ys, ink, min_bright, min_white) in rows {
        for (art, floor) in [(0.85f32, min_bright), (1.0, min_white)] {
            let (c, x, y) = worst_over(ink, art, xs, ys);
            let before = contrast_over_art(ink, art, 0.0);
            eprintln!("{label:<32} art {art:.2}: {c:5.2}:1 (floor {floor}) at ({x:.0},{y:.0}) alpha {:.3}; unscrimmed {before:.2}:1", home_scrim_a(x, y, 1.0));
            if c < floor {
                failed.push(format!("{label} over art {art}: {c:.2}:1 under its {floor}:1 floor at ({x:.0},{y:.0}), scrim alpha {:.3}", home_scrim_a(x, y, 1.0)));
            }
            assert!(c > before, "{label} over art {art}: the scrim made it WORSE ({before:.2} -> {c:.2})");
        }
    }
    assert!(failed.is_empty(), "the board's curve fails the floor:\n{}", failed.join("\n"));
}
