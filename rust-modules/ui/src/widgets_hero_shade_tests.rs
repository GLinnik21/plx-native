//! The hero shade over content rects (`hero_shade`, `gfx::scrim`): its shape, that it follows a rect
//! that moves, that it is zero away from content, the one-pass ground that carries it, and the
//! legibility contract graded at the positions Home's ladder puts its copy.
//!
//! Pure math, no GL: the field the GPU's vertex shader evaluates is `GroundShade::at`
//! (`gfx/scrim.rs`), which is what these read, and the mesh it is drawn on is walked for real
//! (`GroundShade::mesh_error`). Detail's half of the contract is in `widgets_hero_scrim_tests.rs`.

use super::*;
use super::test_support::*;
use crate::consts::{MARGIN_X, SCR_H, SCR_W};
use crate::hero_shade::{HeroShade, Role};
use theme::shade as t;

/// A three-block fixture with different positions and role weights.
fn home_like() -> HeroShade {
    HeroShade::new()
        .add(Rect::new(MARGIN_X, 470.0, 943.0, 168.0), Role::Copy, 1.0)
        .add(Rect::new(MARGIN_X, 668.0, 420.0, 60.0), Role::Controls, 1.0)
        .add(Rect::new(MARGIN_X + 460.0, 668.0, 215.0, 60.0), Role::Controls, 1.0)
}

/// **A copy block is as wide as the widest line drawn in its column, not as wide as the column**: a
/// 943 px column holding lines that end at 300, 720 and 410 shades to 720 and is bare art a feather
/// past it; a line wider than the column is held inside it, and nothing drawn adds nothing.
#[test]
fn a_copy_block_stands_behind_the_widest_line_and_not_the_column() {
    let column = Rect::new(MARGIN_X, 500.0, 943.0, 120.0);
    let at = |w: f32| (MARGIN_X, MARGIN_X + w);
    let shade = HeroShade::new().add_ink(column, [at(300.0), at(720.0), at(410.0)], Role::Copy, 1.0);
    assert_eq!(shade.blocks(), [(Rect::new(MARGIN_X, 500.0, 720.0, 120.0), t::COPY_A)]);
    let ink_foot = MARGIN_X + 720.0;
    assert!(shade.at(ink_foot, 560.0, 1.0) >= t::COPY_A - 1e-4, "the last letter stands on the full darkness");
    assert_eq!(shade.at(ink_foot + t::PAD + t::FEATHER + 4.0, 560.0, 1.0), 0.0, "past the feather is bare art, not a slab");
    let held = HeroShade::new().add_ink(column, [(0.0, 5000.0)], Role::Copy, 1.0);
    assert_eq!(held.blocks()[0].0, column, "a line cannot widen its column's block");
    assert!(HeroShade::new().add_ink(column, [], Role::Copy, 1.0).blocks().is_empty(), "nothing drawn, nothing shaded");
    assert!(HeroShade::new().add_ink(column, [at(0.0)], Role::Copy, 1.0).blocks().is_empty(), "an empty line is not ink");
}

/// **A block holds its role's darkness behind its content and feathers to exactly nothing**
/// [`theme::shade::FEATHER`] px beyond the box ([`theme::shade::PAD`] past the content).
#[test]
fn a_block_holds_its_darkness_behind_content_and_feathers_to_nothing() {
    let r = Rect::new(400.0, 300.0, 500.0, 100.0);
    for (role, peak) in [(Role::Copy, t::COPY_A), (Role::Controls, t::CONTROLS_A)] {
        let s = HeroShade::new().add(r, role, 1.0);
        assert_eq!(role.peak(), peak);
        assert_eq!(s.at(650.0, 350.0, 1.0), peak, "the content's centre");
        assert_eq!(s.at(r.x, r.y, 1.0), peak, "the content's own corner is inside the padded box");
        assert_eq!(s.at(r.x - t::PAD, 350.0, 1.0), peak, "the padded box's edge");
        let edge = r.x - t::PAD;
        assert!((s.at(edge - t::FEATHER * 0.5, 350.0, 1.0) - peak * 0.5).abs() < 1e-5, "half the feather out is half the darkness");
        assert_eq!(s.at(edge - t::FEATHER, 350.0, 1.0), 0.0, "the feather's end is exactly nothing");
    }
}

/// **Zero away from content**, exactly: every point of the panel farther than the feather from every
/// padded box has no shade at all, so the photograph is the photograph.
#[test]
fn the_shade_is_exactly_zero_far_from_the_content() {
    let s = home_like();
    let far = t::PAD + t::FEATHER;
    let boxes: Vec<Rect> = s.blocks().iter().map(|&(r, _)| r).collect();
    let mut tested = 0;
    for iy in 0..=54 {
        for ix in 0..=96 {
            let (x, y) = (ix as f32 * 20.0, iy as f32 * 20.0);
            let clear = boxes.iter().all(|r| {
                let (dx, dy) = ((r.x - x).max(x - (r.x + r.w)).max(0.0), (r.y - y).max(y - (r.y + r.h)).max(0.0));
                (dx * dx + dy * dy).sqrt() >= far
            });
            if clear {
                assert_eq!(s.at(x, y, 1.0), 0.0, "({x},{y}) is clear of every box");
                tested += 1;
            }
        }
    }
    assert!(tested > 1000, "the premise: most of the panel is clear ({tested} points)");
    assert_eq!(HeroShade::new().at(960.0, 540.0, 1.0), 0.0, "a hero with no content has no shade");
}

/// **The shade follows a rect that moves.** Detail's trailer: the hero logo rises from its title spot
/// into the top-left corner, and the shade that stood behind the title is gone from there and stands
/// behind the logo in the corner. (The screen feeds its live logo rect; `detail`'s own test holds that.)
#[test]
fn the_shade_follows_a_logo_that_rises() {
    let hero = Rect::new(MARGIN_X, crate::detail_layout::TITLE_BOTTOM - 120.0, 480.0, 120.0);
    let corner = Rect::new(crate::detail_layout::PREVIEW_LOGO_X, crate::detail_layout::PREVIEW_LOGO_Y, 480.0, 54.0);
    let at_rest = HeroShade::new().add(hero, Role::Copy, 1.0);
    let risen = HeroShade::new().add(corner, Role::Copy, 1.0);
    let (hero_c, corner_c) = ((hero.x + 100.0, hero.y + 60.0), (corner.x + 100.0, corner.y + 27.0));
    assert_eq!(at_rest.at(hero_c.0, hero_c.1, 1.0), t::COPY_A);
    assert!(at_rest.at(corner_c.0, corner_c.1, 1.0) < 0.01, "at rest there is no shade up in the corner");
    assert_eq!(risen.at(corner_c.0, corner_c.1, 1.0), t::COPY_A);
    assert!(risen.at(hero_c.0, hero_c.1, 1.0) < 0.01, "…and once risen none is left behind at the title");
    // in flight the block is in between: as the logo travels, the shade at the title spot only
    // falls and the shade at the corner only rises, and the block's own centre is held throughout
    let mut prev = (f32::INFINITY, f32::NEG_INFINITY);
    for k in 0..=8 {
        let t = k as f32 / 8.0;
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        let mid = Rect::new(lerp(hero.x, corner.x), lerp(hero.y, corner.y), lerp(hero.w, corner.w), lerp(hero.h, corner.h));
        let part = HeroShade::new().add(mid, Role::Copy, 1.0);
        assert_eq!(part.at(mid.x + mid.w * 0.5, mid.y + mid.h * 0.5, 1.0), t::COPY_A, "t={t}: the block's own centre");
        let here = (part.at(hero_c.0, hero_c.1, 1.0), part.at(corner_c.0, corner_c.1, 1.0));
        assert!(here.0 <= prev.0 && here.1 >= prev.1, "t={t}: the title spot's shade {} then {}, the corner's {} then {}", prev.0, here.0, prev.1, here.1);
        prev = here;
    }
}

/// **A block fades with its own content, the hero's fade scales every block and then they stack**, a
/// content alpha of 0 adds no block at all, and neither a negative nor an excessive input inverts or
/// overdrives the shade.
#[test]
fn a_blocks_own_alpha_and_the_heros_fade_scale_each_block_then_stack() {
    let (a, b) = (Rect::new(100.0, 400.0, 600.0, 80.0), Rect::new(100.0, 500.0, 600.0, 80.0));
    let both = HeroShade::new().add(a, Role::Copy, 1.0).add(b, Role::Controls, 1.0);
    let (pa, pb) = (t::COPY_A, t::CONTROLS_A);
    let stack = |ka: f32, kb: f32| 1.0 - (1.0 - pa * ka) * (1.0 - pb * kb);
    // a point inside both padded boxes
    let (x, y) = (300.0, 490.0);
    assert!((both.at(x, y, 1.0) - stack(1.0, 1.0)).abs() < 1e-6);
    assert!((both.at(x, y, 0.5) - stack(0.5, 0.5)).abs() < 1e-6, "the fade scales each, then they stack");
    let faded = HeroShade::new().add(a, Role::Copy, 0.5).add(b, Role::Controls, 1.0);
    assert!((faded.at(x, y, 1.0) - stack(0.5, 1.0)).abs() < 1e-6, "one block fades by itself");
    assert_eq!(HeroShade::new().add(a, Role::Copy, 0.0).blocks().len(), 0, "no content alpha, no block");
    assert_eq!(HeroShade::new().add(Rect::new(0.0, 0.0, 0.0, 50.0), Role::Copy, 1.0).blocks().len(), 0, "an empty rect is no block");
    assert_eq!(both.at(x, y, 0.0), 0.0);
    assert_eq!(both.at(x, y, -1.0), 0.0, "a negative fade is nothing, never an inverted shade");
    assert_eq!(both.at(x, y, 9.0), both.at(x, y, crate::landing_hero::PREVIEW_FIELD), "…and it saturates at the video-bound ceiling");
    assert!(both.at(x, y, crate::landing_hero::PREVIEW_FIELD) < 1.0);
}

/// More content than the shader's uniform array holds folds into the last block (its box widened to
/// hold both, the darker peak kept): a limit of the program, never a cap on what a screen may draw.
#[test]
fn content_past_the_uniform_limit_folds_into_the_last_block() {
    let mut s = HeroShade::new();
    for i in 0..plx_gfx::gfx::scrim::MAX_BLOCKS + 3 {
        s = s.add(Rect::new(100.0 + 200.0 * i as f32, 300.0, 100.0, 60.0), Role::Copy, 1.0);
    }
    assert_eq!(s.blocks().len(), plx_gfx::gfx::scrim::MAX_BLOCKS);
    let last = s.blocks()[plx_gfx::gfx::scrim::MAX_BLOCKS - 1].0;
    let (x0, x1) = (100.0 + 200.0 * (plx_gfx::gfx::scrim::MAX_BLOCKS - 1) as f32, 100.0 + 200.0 * (plx_gfx::gfx::scrim::MAX_BLOCKS + 2) as f32 + 100.0);
    assert_eq!((last.x, last.x + last.w), (x0, x1), "the last block holds every surplus rect");
    assert_eq!(s.field().blocks().count(), plx_gfx::gfx::scrim::MAX_BLOCKS);
}

/// **The painted field against the closed form.** The GPU evaluates the shade at the vertices of a
/// 16 x 16 mesh and interpolates linearly between them; the ground cuts the panel into 480 x 270
/// cells so a mesh cell is 30 x 17 px (the same cut the ground has always had: no new vertices). This
/// walks the REAL mesh at that cut, for a Home-like hero at full strength and mid-fade.
///
/// Two properties, and the second is the one the legibility contract needs:
///
/// - **Anywhere, the painted field is within `MESH_BOUND` of alpha of the closed form.** The worst
///   case is mid-feather, where a smoothstep has its sharpest bend (a 140 px feather against a 30 px
///   step): `peak * 6 * h^2 / (8 * feather^2)` = 0.0207 at the copy's peak. That is a gentle gradient
///   moving by 5 codes of alpha, never a plateau or an edge. Halving the peak halves it.
/// - **Under the content it is the closed form, or darker.** The content stands PAD px inside the
///   plateau and the smoothstep is flat at its start, so the mesh triangles under every glyph carry the
///   held alpha: at no point of any content rect does the painted field stand LIGHTER than the closed
///   form by more than `UNDER_CONTENT` (the direction that could cost legibility; the other one only
///   adds shade). The residue is a NEIGHBOUR's feather crossing a rect (the copy block's tail over the
///   action row), where the stack is already far past the floor. That is what lets the contract be
///   graded on the closed form.
#[test]
fn the_painted_field_is_the_closed_form_under_content_and_close_elsewhere() {
    // the largest the painted field may stand from the closed form anywhere, in alpha (~6.4 codes)
    const MESH_BOUND: f32 = 0.025;
    // …and the most it may stand LIGHTER under the content (one 8-bit code of alpha is 0.0039)
    const UNDER_CONTENT: f32 = 0.005;
    let hero = home_like();
    let s = hero.scrim(1.0).field;
    let panel = [0.0, 0.0, SCR_W, SCR_H];
    let cells = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).count();
    let (cols, rows) = ((SCR_W / SCRIM_CELL_W).ceil() as usize, (SCR_H / SCRIM_CELL_H).ceil() as usize);
    assert_eq!(cells, cols * rows);
    let uncut = s.mesh_error(panel, 1, 1);
    let cut = s.mesh_error(panel, cols, rows);
    let half = hero.scrim(0.5).field.mesh_error(panel, cols, rows);
    eprintln!("shade mesh error: uncut {uncut:.4}, cut {cols}x{rows} {cut:.4} ({:.2} codes), half strength {half:.4}", cut * 255.0);
    assert!(cut <= MESH_BOUND, "the cut field is {:.2} codes of alpha from the closed form", cut * 255.0);
    assert!(half <= cut * 0.55, "a fade scales the error down with it ({half} vs {cut})");
    assert!(uncut > 2.0 * cut, "the cut is what buys the bound: {uncut} -> {cut}");

    let mut worst = (0.0f32, 0.0f32, 0.0f32);
    let mut tested = 0;
    for &(r, _) in hero.blocks() {
        for iy in 0..=8 {
            for ix in 0..=40 {
                let (x, y) = (r.x + r.w * ix as f32 / 40.0, r.y + r.h * iy as f32 / 8.0);
                let gap = s.at(x, y) - s.painted_at(panel, cols, rows, x, y);
                if gap > worst.0 {
                    worst = (gap, x, y);
                }
                tested += 1;
            }
        }
    }
    eprintln!("under content ({tested} points): worst painted-vs-closed gap {:.5} at ({:.0},{:.0})", worst.0, worst.1, worst.2);
    assert!(worst.0 <= UNDER_CONTENT, "under content the painted field stands {:.4} lighter than the closed form at ({:.0},{:.0})", worst.0, worst.1, worst.2);
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

/// **The layered shade is the ground's own cells**, so a frame that cannot carry it in the pass (a
/// hero slide) draws the same field in the same pieces, as the scrim layer (tag 26).
#[test]
fn the_layered_shade_is_the_grounds_cells() {
    let got = handed(|p| scrim_layer(p, home_like().scrim(1.0)));
    let want: Vec<(u64, Rect)> = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).map(|c| (26, c)).collect();
    assert_eq!(got, want);
}

/// **`draw_ground` hands the shade to the ground in cells exactly when the ground can carry it**
/// (`wash_ink_ok`, as its ramp always was), and otherwise hands it back for the caller's layer. No
/// shade and no ramp is the plain wash in one piece, as before.
#[test]
fn draw_ground_carries_the_shade_in_cells_or_hands_it_back() {
    let mut w = AmbientWash::flat([0.0; 4]);
    w.jump([[0.2, 0.3, 0.6, 1.0], [0.5, 0.2, 0.3, 1.0], [0.1, 0.4, 0.2, 1.0], [0.3, 0.3, 0.3, 1.0]]);
    let inked = plx_gfx::gfx::wash_ink_ok();
    let mut took = !inked;
    let got = handed(|p| took = w.draw_ground(p, Rect::FULL, None, None, Some(home_like().scrim(1.0))));
    let cells: Vec<Rect> = cut_cells(Rect::FULL, SCRIM_CELL_W, SCRIM_CELL_H).collect();
    if inked {
        assert_eq!(got, cells.iter().map(|&c| (23, c)).collect::<Vec<_>>(), "one inked mesh a cell");
    } else {
        assert_eq!(got, [(16, Rect::FULL)], "no program: the plain wash, the shade handed back");
    }
    assert_eq!(took, inked, "taken exactly when the wash can carry it");
    let mut took = true;
    assert_eq!(handed(|p| took = w.draw_ground(p, Rect::FULL, None, None, None)), [(16, Rect::FULL)]);
    assert!(!took, "nothing to carry, nothing taken");
}

// ── Legibility at the positions Home's ladder puts its copy ─────────────────────────────────────

/// Grade the furthest right the hero copy can reach and the full shelf caption strip.
#[test]
fn home_hero_and_shelf_copy_read_over_white_artwork() {
    use crate::landing_hero::{home_rows, home_shade, shelf_fade, COL_W, SHELF_TOP};
    let shelf = shelf_fade(SHELF_TOP, 1.0);
    let shade = home_shade(shelf, 1.0);
    let stack = home_rows(crate::hero_logo::band_h(crate::hero_logo::LogoRung::Hero),
        Some(crate::landing_hero::HOME_LABEL_H));
    let title_bottom = stack.title.y + stack.title.h;
    let line = stack.block.expect("one line");
    let footer_y = crate::still_footer::Place::Below.line_top(
        Rect::new(0.0, SHELF_TOP, SCR_W, crate::card_row::RowStyle::STILL.h));
    for (ink, x, y) in [
        (theme::TEXT_PRIMARY, MARGIN_X + COL_W, title_bottom - HERO_CAP_H),
        (theme::TEXT_PRIMARY, MARGIN_X + COL_W, line.y),
        (theme::TEXT_READING, MARGIN_X + COL_W, line.y),
        (theme::TEXT_SECONDARY, SCR_W, footer_y),
    ] {
        let alpha = shade.alpha(x, y);
        assert!(contrast_over_art(ink, 0.85, alpha) >= 3.0, "bright artwork at ({x}, {y}), shade {alpha}");
        assert!(contrast_over_art(ink, 1.0, alpha) >= 2.5, "white artwork at ({x}, {y}), shade {alpha}");
    }
}

// ── The shelf fade ──────────────────────────────────────────────────────────────────────────────

/// **The shelf fade reaches near-black at the caption line and starts at the tiles' top.** Nothing at
/// the top of the tiles (and nothing above), its named foot ([`theme::shade::SHELF_FOOT_A`], 0.92 to
/// 0.95 of the ink) at the caption line's top and all along the line to the panel's foot, the ease in
/// between (the knee [`theme::shade::SHELF_MID_A`] at the tiles' mid-line), never decreasing, and
/// never stepping by more than two 8-bit codes from one pixel row to the next: no visible band edge.
#[test]
fn the_shelf_fade_reaches_near_black_at_the_caption_line() {
    use crate::landing_hero::{shelf_fade, SHELF_TOP};
    let ramp = shelf_fade(SHELF_TOP, 1.0);
    let tiles = Rect::new(0.0, SHELF_TOP, SCR_W, crate::card_row::RowStyle::STILL.h);
    let line = crate::still_footer::Place::Below.line_top(tiles);
    assert_eq!((SHELF_TOP, line), (768.0, 1012.0), "the tiles' top and the caption line of the board");
    assert!((0.92..=0.95).contains(&t::SHELF_FOOT_A), "near-black: 0.92 to 0.95 of the ink, not {}", t::SHELF_FOOT_A);
    assert_eq!(ramp.alpha(0.0), 0.0, "nothing up in the hero");
    assert_eq!(ramp.alpha(SHELF_TOP), 0.0, "nothing at the tiles' top");
    assert_eq!(ramp.alpha(tiles.y + tiles.h * 0.5), t::SHELF_MID_A, "the knee, at the tiles' mid-line");
    assert_eq!(ramp.alpha(line), t::SHELF_FOOT_A, "the caption line's top");
    assert_eq!(ramp.alpha(line + crate::still_footer::FOOTER_LINE_H), t::SHELF_FOOT_A, "…and its foot");
    assert_eq!(ramp.alpha(SCR_H), t::SHELF_FOOT_A, "…held to the panel's foot");
    let mut last = 0.0;
    for row in 0..=SCR_H as usize {
        let a = ramp.alpha(row as f32);
        assert!(a >= last, "row {row}: the fade never lightens ({last} -> {a})");
        assert!(a - last <= 2.0 / 255.0, "row {row}: a step of {:.2} codes is a visible edge", (a - last) * 255.0);
        last = a;
    }
    // it is the hero's: it fades with the hero, and never exceeds the foot
    assert!((shelf_fade(SHELF_TOP, 0.5).alpha(line) - t::SHELF_FOOT_A * 0.5).abs() < 1e-6, "half a hero, half the fade");
    assert_eq!(shelf_fade(SHELF_TOP, 0.0).alpha(line), 0.0, "no hero, no fade");
    assert_eq!(shelf_fade(SHELF_TOP, 7.0).alpha(line), t::SHELF_FOOT_A, "an excess fade saturates at the foot");
    // it rides the page: the shelf scrolled up 300 px takes the fade with it, curve unchanged
    let up = shelf_fade(SHELF_TOP - 300.0, 1.0);
    for y in [SHELF_TOP, 800.0, 900.0, line, SCR_H] {
        assert!((up.alpha(y - 300.0) - ramp.alpha(y)).abs() < 1e-6, "y {y}: the fade is the same curve, 300 px up");
    }
    // the ground carries it with the shade in ONE ink (`draw_ground` asserts it)
    assert_eq!(ramp.ink, home_like().scrim(1.0).ink);
}

/// **The ramp's layered form is the curve the ground carries**: the frames a ground cannot carry it in
/// (a hero slide) draw one full-width rect a segment from the first stop down, with the last stop's
/// alpha held to the panel's foot, so the strips tile `first stop..SCR_H` with no gap.
#[test]
fn a_ramp_drawn_as_layers_tiles_from_its_first_stop_to_the_panels_foot() {
    let ramp = crate::landing_hero::shelf_fade(crate::landing_hero::SHELF_TOP, 1.0);
    let got = handed(|p| ramp.layer(p));
    assert_eq!(got.len(), 3, "two segments and the held foot");
    let mut y = ramp.stops[0].0;
    for (_, r) in &got {
        assert_eq!((r.x, r.w, r.y), (0.0, SCR_W, y), "full width, strip to strip");
        y = r.y + r.h;
    }
    assert_eq!(y, SCR_H);
    // Detail's ramp ends on the panel's foot: no held strip
    let detail = WashRamp { ink: theme::scrim(1.0), stops: [(367.2, 0.0), (700.0, 0.4), (SCR_H, 0.8)] };
    assert_eq!(handed(|p| detail.layer(p)).len(), 2);
}
