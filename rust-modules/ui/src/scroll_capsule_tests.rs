use super::*;

struct Fixed;
impl plx_machine::machine::Measure for Fixed {
    fn width(&self, s: &std::ffi::CStr, size: i32, _: bool) -> f32 { s.to_bytes().len() as f32 * size as f32 * 0.5 }
    fn cap_h(&self, _: i32) -> f32 { 20.0 }
    fn line_h(&self, _: i32) -> f32 { 36.0 }
}

fn runs(name: &str, count: &str) -> (Vec<crate::draw_census::Run>, f32) {
    let (n, c) = (std::ffi::CString::new(name).unwrap(), std::ffi::CString::new(count).unwrap());
    (crate::draw_census::capture_runs(|| draw(Painter::recording(), &n, &c, 1.0, &Fixed)), width(&n, &c, &Fixed))
}

#[test]
fn the_capsule_stands_at_96_54_64_tall_with_28_on_the_left() {
    let (n, c) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    let r = rect(&n, &c, &Fixed);
    assert_eq!((r.x, r.y, r.h), (96.0, 54.0, 64.0));
    assert_eq!(r.w, width(&n, &c, &Fixed));
    assert_eq!(RADIUS, 32.0);
    assert_eq!(PAD_L, 28.0);
}

#[test]
fn a_very_long_name_is_never_cut_the_capsule_grows_to_hold_it() {
    let _g = plx_base::testlock::serial();
    let long = "Documentaries and Concerts of the Century, Remastered";
    let (runs, w) = runs(long, "12,345");
    assert!(runs.iter().any(|r| r.text == long), "the whole name is drawn, no ellipsis");
    assert!(runs.iter().all(|r| !r.text.contains('\u{2026}')));
    assert_eq!(w, PAD_L + long.len() as f32 * 14.0 + NAME_COUNT_GAP + 6.0 * 12.0 + PAD_R);
    assert!(w > 320.0, "wider than the old 320 cap: {w}");
}

#[test]
fn the_capsule_is_as_wide_as_its_text_with_28_on_the_left() {
    let _g = plx_base::testlock::serial();
    let (n, c) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    let w = width(&n, &c, &Fixed);
    assert_eq!(w, PAD_L + 5.0 * 14.0 + NAME_COUNT_GAP + 5.0 * 12.0 + PAD_R, "padding, name, gap, count, padding");
    assert!(w < 320.0, "a short name leaves a short capsule");
    let (runs, _) = runs("Films", "1,284");
    let name = runs.iter().find(|r| r.text == "Films").unwrap();
    assert_eq!(name.at.x, X + PAD_L, "the text starts 28 in");
}

#[test]
fn the_count_is_caption_size_regular_and_secondary_while_the_name_stays_body_bold() {
    let _g = plx_base::testlock::serial();
    let (runs, _) = runs("Films", "1,284");
    let name = runs.iter().find(|r| r.text == "Films").unwrap();
    let count = runs.iter().find(|r| r.text == "1,284").unwrap();
    assert_eq!((name.size, name.bold, name.ink), (theme::size::BODY, true, theme::TEXT_PRIMARY));
    assert_eq!((count.size, count.bold, count.ink), (theme::size::CAPTION, false, theme::TEXT_SECONDARY));
    assert_eq!(count.at.x, name.at.x + name.at.w + NAME_COUNT_GAP);
}

const GROUND: [f32; 3] = [0.2, 0.3, 0.4];

fn capsule_rect() -> Rect {
    let (n, c) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    rect(&n, &c, &Fixed)
}

/// The fade's alpha at each strip edge, top to bottom, with the strips checked to abut as it goes.
fn edge_alphas(capsule: Option<(Rect, f32)>) -> Vec<(f32, f32)> {
    let mut edges: Vec<(f32, f32)> = Vec::new();
    for (y, h, top, bottom) in fade_strips(GROUND, capsule) {
        assert_eq!(&top[..3], &GROUND, "the ground colour at the top edge of the strip at {y}");
        assert_eq!(&bottom[..3], &GROUND, "and at its bottom edge: no grey band through black");
        match edges.last() {
            Some(&(prev_y, prev_a)) => assert_eq!((prev_y, prev_a), (y, top[3]), "strips abut and share their stop"),
            None => edges.push((y, top[3])),
        }
        edges.push((y + h, bottom[3]));
    }
    edges
}

#[test]
fn the_fade_runs_from_opaque_ground_to_clear_in_the_ground_colour() {
    let edges = edge_alphas(None);
    assert_eq!(edges.first(), Some(&(0.0, 1.0)), "opaque at the very edge");
    assert_eq!(edges.last(), Some(&(FADE_H, 0.0)), "clear at FADE_H down, and nothing drawn past it");
    assert_eq!(FADE_H, 72.0);
    assert!(edges.windows(2).all(|w| w[1].1 <= w[0].1), "it only ever falls");
}

/// **The ramp is eased, not linear**: a straight line from opaque to clear breaks slope at its end,
/// and over saturated art that break reads as a bright band. The first and last strips fall by less
/// than half a linear step, the middle ones by more.
#[test]
fn the_fade_eases_in_and_out_instead_of_ending_in_a_slope_break() {
    let edges = edge_alphas(None);
    let linear = FADE_STEP / FADE_H;
    let drops: Vec<f32> = edges.windows(2).map(|w| w[0].1 - w[1].1).collect();
    assert_eq!(drops.len(), 12);
    assert!(drops[0] < linear * 0.5 && drops[11] < linear * 0.5, "ends are flat: {drops:?}");
    assert!(drops[5] > linear * 1.3 && drops[6] > linear * 1.3, "the middle is steeper than a line: {drops:?}");
    for (y, a) in &edges {
        let t = y / FADE_H;
        let exact = 1.0 - t * t * (3.0 - 2.0 * t);
        assert!((a - exact).abs() < 1e-6, "the stop at {y} is the smoothstep exactly");
    }
}

/// **The fade reaches through the capsule, and the capsule's rect is where it learns how far.**
/// 12 px below the capsule's bottom edge, not a second literal: a capsule that stood lower would
/// pull the fade down with it.
#[test]
fn the_fade_reaches_through_the_capsule_band_to_its_measured_bottom_plus_a_pad() {
    let r = capsule_rect();
    assert_eq!(fade_reach(r), r.y + r.h + FADE_BELOW_CAPSULE);
    assert_eq!(fade_reach(r), 130.0);
    assert_eq!(fade_reach(Rect::new(r.x, r.y + 20.0, r.w, r.h)), 150.0, "taken from the rect");
    assert_eq!(fade_reach(Rect::new(0.0, 0.0, 10.0, 10.0)), FADE_H, "never shallower than the plain fade");
    let edges = edge_alphas(Some((r, 1.0)));
    assert_eq!(edges.first(), Some(&(0.0, 1.0)));
    assert_eq!(edges.last(), Some(&(130.0, 0.0)), "clear at the reach");
    assert!(edges.windows(2).all(|w| w[1].1 <= w[0].1), "still only falling");
    let at = |y: f32| edges.iter().find(|e| e.0 >= y).unwrap().1;
    assert!(at(FADE_H + FADE_STEP) > 0.1, "the veil is still there just past the plain fade's end");
    assert!(at(r.y + r.h) > 0.0, "the capsule's lower edge is not on bare art");
}

/// **The deeper fade arrives with the capsule's alpha and the strips stand still**: only their alpha
/// moves, so a capsule fading in does not slide a gradient edge down the page.
#[test]
fn the_capsule_alpha_moves_the_alpha_of_the_strips_not_their_geometry() {
    let r = capsule_rect();
    let ys = |a: f32| fade_strips(GROUND, Some((r, a))).map(|(y, h, ..)| (y, h)).collect::<Vec<_>>();
    assert_eq!(ys(0.25), ys(1.0));
    assert_eq!(edge_alphas(Some((r, 0.0))), edge_alphas(None), "a capsule at alpha 0 is the plain fade");
    let (plain, half, full) = (edge_alphas(None), edge_alphas(Some((r, 0.5))), edge_alphas(Some((r, 1.0))));
    let deep = |e: &[(f32, f32)]| e.iter().find(|e| e.0 >= 100.0).map_or(0.0, |e| e.1);
    assert!(deep(&plain) == 0.0 && deep(&half) > 0.0 && deep(&full) > deep(&half), "alpha grows with the capsule's");
}

#[test]
fn the_fade_paints_abutting_gradient_strips_across_the_page() {
    use crate::popover_glass::tests::declared_all;
    let _g = plx_base::testlock::serial();
    let all = declared_all(|| draw_top_fade(Painter::root(), 1920.0, GROUND, 1.0, None));
    let rects: Vec<[f32; 4]> = all.iter().filter(|p| p.kind() == "rect").map(|p| p.placed()).collect();
    assert_eq!(rects.len(), 12, "twelve strips of {FADE_STEP}");
    assert!(rects.iter().all(|r| r[0] == 0.0 && r[2] == 1920.0), "each the page's width");
    assert_eq!(rects[0][1], 0.0);
    assert!(rects.windows(2).all(|w| w[1][1] == w[0][1] + w[0][3]), "abutting, no seam");
    let last = rects[11];
    assert_eq!(last[1] + last[3], FADE_H);
}

/// **The capsule is the nav pill's LIVE glass**: one `backdrop` surface of the standing-container
/// kind, no frozen-ground glass. Red on the frozen popover glass it used to wear.
#[test]
fn the_capsule_is_live_nav_pill_glass_not_frozen_ground() {
    use crate::frame::backdrop::{self, Sources, Z};
    use std::{cell::RefCell, rc::Rc};
    let _g = plx_base::testlock::serial();
    let (name, count) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = backdrop::discover(sources.clone());
        let _page = backdrop::layer(Z::page(1), false);
        draw(Painter::root(), &name, &count, 1.0, &Fixed);
    }
    assert_eq!(sources.borrow().entries.len(), 1, "one live backdrop entry: the capsule's glass");
    let frozen = crate::popover_glass::tests::declared_all(|| draw(Painter::root(), &name, &count, 1.0, &Fixed));
    assert!(frozen.iter().all(|p| p.kind() != "glass_frozen"), "never the frozen ground");
}

/// A long name still prices inside the glass region budget a moving host carries at 60 fps.
#[test]
fn a_long_names_capsule_stays_inside_the_glass_region_budget() {
    let (name, count) = (std::ffi::CString::new("Documentaries and Concerts of the Century").unwrap(), std::ffi::CString::new("12,345").unwrap());
    let r = rect(&name, &count, &Fixed);
    let reg = plx_gfx::gfx::blur_region(r.x, r.y, r.w, r.h);
    assert!(reg[2] * reg[3] <= plx_gfx::gfx::GLASS_REGION_BUDGET, "{reg:?}");
}

#[test]
fn a_zero_tuck_draws_nothing() {
    use crate::popover_glass::tests::declared_all;
    let _g = plx_base::testlock::serial();
    let (name, count) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    let all = declared_all(|| {
        draw(Painter::root(), &name, &count, 0.0, &Fixed);
        draw_top_fade(Painter::root(), 1920.0, [0.2; 3], 0.0, None);
    });
    assert!(all.is_empty());
}

#[test]
fn the_tuck_fades_the_chrome_out() {
    use crate::screen::NavPresentation;
    let nav = NavPresentation { chrome_alpha: 0.8, ..Default::default() };
    assert_eq!(nav.tucked(0.0), nav, "an untucked page changes nothing");
    assert!((nav.tucked(0.25).chrome_alpha - 0.6).abs() < 1e-6);
    assert_eq!(nav.tucked(1.0).chrome_alpha, 0.0);
    assert_eq!(nav.tucked(7.0).chrome_alpha, 0.0, "clamped");
}

#[test]
fn the_tuck_lifts_the_chrome_as_well_as_fading_it() {
    use crate::screen::{chrome_lift, NavPresentation};
    let nav = NavPresentation::default();
    assert_eq!(nav.chrome_lift, 0.0, "every other page rests at offset 0");
    assert_eq!(nav.tucked(0.0).chrome_lift, 0.0);
    assert_eq!(nav.tucked(1.0).chrome_lift, crate::widgets::TOP_BAR_BOTTOM);
    assert_eq!(chrome_lift(0.5), crate::widgets::TOP_BAR_BOTTOM * 0.5);
    assert_eq!(nav.tucked(9.0).chrome_lift, crate::widgets::TOP_BAR_BOTTOM, "clamped");
}
