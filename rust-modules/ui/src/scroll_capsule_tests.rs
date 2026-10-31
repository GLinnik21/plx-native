use super::*;

struct Fixed;
impl plx_machine::machine::Measure for Fixed {
    fn width(&self, s: &std::ffi::CStr, size: i32, _: bool) -> f32 { s.to_bytes().len() as f32 * size as f32 * 0.5 }
    fn cap_h(&self, _: i32) -> f32 { 20.0 }
    fn line_h(&self, _: i32) -> f32 { 36.0 }
}

/// A capsule measured and painted in one go, the way a caller that has no use for its rectangle does.
fn draw(p: Painter, name: &CStr, count: &CStr, criterion: Option<&CStr>, alpha: f32, measure: &dyn Measure) {
    Capsule::new(name, count, criterion, measure).draw(p, alpha);
}

/// **A capsule is measured once**: the page asks for its rectangle (the top fade reaches through it)
/// and then draws it, and both read the runs measured when the capsule was built. It measured them
/// three times a frame (the rectangle, the draw's own rectangle, the draw's runs).
#[test]
fn a_capsule_measures_each_run_once_whatever_is_asked_of_it() {
    use std::cell::Cell;
    struct Counting(Cell<usize>);
    impl Measure for Counting {
        fn width(&self, s: &CStr, size: i32, bold: bool) -> f32 { self.0.set(self.0.get() + 1); Fixed.width(s, size, bold) }
        fn cap_h(&self, size: i32) -> f32 { Fixed.cap_h(size) }
        fn line_h(&self, size: i32) -> f32 { Fixed.line_h(size) }
    }
    let _g = plx_base::testlock::serial();
    let (name, count, criterion) = (c"Films", c"1,284", c"Date viewed");
    for (criterion, runs) in [(None, 2), (Some(criterion), 4)] {
        let measure = Counting(Cell::new(0));
        let capsule = Capsule::new(name, count, criterion, &measure);
        let rect = capsule.rect();
        capsule.draw(Painter::recording(), 1.0);
        capsule.draw(Painter::recording(), 0.5);
        assert_eq!(measure.0.get(), runs, "{criterion:?}: every run measured once, by the one build");
        assert_eq!(rect, super::rect(name, count, criterion, &Fixed), "the rectangle is the free function's");
        assert_eq!(rect.w, width(name, count, criterion, &Fixed));
    }
}

fn runs(name: &str, count: &str) -> (Vec<crate::draw_census::Run>, f32) {
    let (n, c) = (std::ffi::CString::new(name).unwrap(), std::ffi::CString::new(count).unwrap());
    (crate::draw_census::capture_runs(|| draw(Painter::recording(), &n, &c, None, 1.0, &Fixed)), width(&n, &c, None, &Fixed))
}

/// The runs and the width of a capsule that carries the sort `criterion`.
fn sorted_runs(name: &str, count: &str, criterion: &str) -> (Vec<crate::draw_census::Run>, f32) {
    let (n, c, k) = (std::ffi::CString::new(name).unwrap(), std::ffi::CString::new(count).unwrap(), std::ffi::CString::new(criterion).unwrap());
    (
        crate::draw_census::capture_runs(|| draw(Painter::recording(), &n, &c, Some(&k), 1.0, &Fixed)),
        width(&n, &c, Some(&k), &Fixed),
    )
}

#[test]
fn the_capsule_stands_at_96_54_64_tall_with_28_on_the_left() {
    let (n, c) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    let r = rect(&n, &c, None, &Fixed);
    assert_eq!((r.x, r.y, r.h), (96.0, 54.0, 64.0));
    assert_eq!(r.w, width(&n, &c, None, &Fixed));
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
    let w = width(&n, &c, None, &Fixed);
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

/// **The capsule carries the sort** ("Films 1,284 · Date viewed"): four pieces, each drawn once, in
/// order, [`PIECE_GAP`] apart, the first 28 in from the capsule's left end.
#[test]
fn a_capsule_that_carries_the_sort_draws_the_name_the_count_a_dot_and_the_criterion_once_each() {
    let _g = plx_base::testlock::serial();
    let (runs, _) = sorted_runs("Films", "1,284", "Date viewed");
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["Films", "1,284", "\u{b7}", "Date viewed"], "each piece exactly once, left to right");
    assert_eq!(runs[0].at.x, X + PAD_L, "the text starts 28 in");
    for pair in runs.windows(2) {
        assert_eq!(pair[1].at.x, pair[0].at.x + pair[0].at.w + PIECE_GAP, "{} to {}: the board's 12 between pieces", pair[0].text, pair[1].text);
    }
    assert_eq!(PIECE_GAP, 12.0);
    let (plain, _) = runs_of_plain();
    assert_eq!(plain.len(), 2, "a page that passes no criterion draws the name and the count, nothing more");
}

fn runs_of_plain() -> (Vec<crate::draw_census::Run>, f32) {
    runs("Films", "1,284")
}

/// **The ink split**: the library and the criterion in the primary ink, the count and the dot in the
/// quiet ink (50% white, the board's: `COUNT_FILTERED_INK`, the header read-out's); all four BODY
/// regular. The plain capsule's count keeps `TEXT_SECONDARY`.
#[test]
fn the_library_and_the_criterion_are_primary_ink_and_the_count_and_the_dot_secondary() {
    let _g = plx_base::testlock::serial();
    let (runs, _) = sorted_runs("TV Shows", "312", "Unplayed");
    let ink = |text: &str| runs.iter().find(|r| r.text == text).map(|r| (r.size, r.bold, r.ink)).unwrap();
    assert_eq!(ink("TV Shows"), (theme::size::BODY, false, theme::TEXT_PRIMARY));
    assert_eq!(ink("312"), (theme::size::BODY, false, theme::COUNT_FILTERED_INK));
    assert_eq!(ink("\u{b7}"), (theme::size::BODY, false, theme::COUNT_FILTERED_INK));
    assert_eq!(theme::COUNT_FILTERED_INK, [1.0, 1.0, 1.0, 0.5], "the role is 50% white");
    assert_eq!(ink("Unplayed"), (theme::size::BODY, false, theme::TEXT_PRIMARY));
}

/// **The width follows the content**: the padding, the four runs and the three gaps; a longer
/// criterion makes a wider capsule, and the plain capsule is unchanged by the option existing.
#[test]
fn a_capsule_that_carries_the_sort_is_as_wide_as_its_four_pieces_and_its_paddings() {
    let _g = plx_base::testlock::serial();
    let dot = DOT.to_bytes().len() as f32;
    let run = |bytes: usize| bytes as f32 * theme::size::BODY as f32 * 0.5;
    let (_, w) = sorted_runs("Films", "1,284", "Date viewed");
    assert_eq!(w, PAD_L + run(5) + run(5) + run(dot as usize) + run(11) + 3.0 * PIECE_GAP + PAD_R);
    let (_, longer) = sorted_runs("Films", "1,284", "Last episode added");
    assert!(longer > w, "{longer} against {w}: the capsule grows with the criterion");
    let (n, c, k) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap(), std::ffi::CString::new("Date viewed").unwrap());
    let r = rect(&n, &c, Some(&k), &Fixed);
    assert_eq!((r.x, r.y, r.w, r.h), (X, Y, w, H), "the same stand as the plain capsule, only wider");
    assert_eq!(fade_reach(r), fade_reach(rect(&n, &c, None, &Fixed)), "the fade reaches the capsule's bottom, not its width");
    let (_, plain) = runs_of_plain();
    assert_eq!(plain, PAD_L + 5.0 * 14.0 + NAME_COUNT_GAP + 5.0 * 12.0 + PAD_R, "the plain capsule is as it was");
}

const GROUND: [f32; 3] = [0.2, 0.3, 0.4];

fn capsule_rect() -> Rect {
    let (n, c) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    rect(&n, &c, None, &Fixed)
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
        draw(Painter::root(), &name, &count, None, 1.0, &Fixed);
    }
    assert_eq!(sources.borrow().entries.len(), 1, "one live backdrop entry: the capsule's glass");
    let frozen = crate::popover_glass::tests::declared_all(|| draw(Painter::root(), &name, &count, None, 1.0, &Fixed));
    assert!(frozen.iter().all(|p| p.kind() != "glass_frozen"), "never the frozen ground");
}

/// A long name still prices inside the glass region budget a moving host carries at 60 fps.
#[test]
fn a_long_names_capsule_stays_inside_the_glass_region_budget() {
    let (name, count) = (std::ffi::CString::new("Documentaries and Concerts of the Century").unwrap(), std::ffi::CString::new("12,345").unwrap());
    let criterion = std::ffi::CString::new("Date viewed").unwrap();
    let r = rect(&name, &count, Some(&criterion), &Fixed);
    let reg = plx_gfx::gfx::blur_region(r.x, r.y, r.w, r.h);
    assert!(reg[2] * reg[3] <= plx_gfx::gfx::GLASS_REGION_BUDGET, "{reg:?}");
}

#[test]
fn a_zero_tuck_draws_nothing() {
    use crate::popover_glass::tests::declared_all;
    let _g = plx_base::testlock::serial();
    let (name, count) = (std::ffi::CString::new("Films").unwrap(), std::ffi::CString::new("1,284").unwrap());
    let all = declared_all(|| {
        draw(Painter::root(), &name, &count, None, 0.0, &Fixed);
        draw(Painter::root(), &name, &count, Some(c"Date viewed"), 0.0, &Fixed);
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
