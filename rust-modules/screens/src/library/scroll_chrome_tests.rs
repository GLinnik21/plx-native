//! The Library's use of the shared scroll chrome (`plx_ui::scroll_chrome`) and of the shared rest
//! rule (`poster_grid::rest_scroll`, through its layout), and what a tucked page declares.
use plx_ui::scroll_capsule;

/// A focused row below the first settles with its poster top where the first row's is at rest
/// (the grid top, y 244): focus reads on one line at the top of a library and deep in it. At the
/// end of the list the document cannot scroll further, so the last row may sit lower.
#[test]
fn a_focused_row_settles_at_the_first_rows_resting_y() {
    use super::layout::{Layout, LIBRARY_GRID_TOP};
    let lay = Layout::new(true, 400);
    assert_eq!(lay.row_y(0, lay.row_reveal(0)), LIBRARY_GRID_TOP);
    assert_eq!(LIBRARY_GRID_TOP, 244.0);
    for row in [1, 2, 5, 40, 200] {
        let focused = lay.with_grid_focus(Some(row));
        assert_eq!(focused.row_y(row, focused.row_reveal(row)), LIBRARY_GRID_TOP, "row {row} rests at 244");
    }
    let last = lay.rows - 1;
    let focused = lay.with_grid_focus(Some(last));
    let y = focused.row_y(last, focused.row_reveal(last));
    assert!(y >= LIBRARY_GRID_TOP, "the last row never rests above the line: {y}");
    assert_eq!(focused.row_reveal(last), focused.max_scroll(), "the end clamps, it does not scroll past it");
    assert!(y > LIBRARY_GRID_TOP, "the last row of a long list sits lower: {y}");
}

#[test]
fn a_settled_row_is_clear_of_the_capsule() {
    assert!(super::layout::LIBRARY_GRID_TOP >= scroll_capsule::Y + scroll_capsule::H);
}

/// The paints one descriptive draw of `page` declares (`frame::backdrop::discover`: primitives are
/// recorded, GL is never reached), as `popover_glass::tests::declared_all` records a menu's.
fn declared_by_a_draw(fixture: &super::tests::Fixture, page: &mut crate::library::LibraryScreen)
    -> Vec<std::rc::Rc<plx_ui::frame::backdrop::Paint>>
{
    use plx_ui::screen::Screen;
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let cx = fixture.cx(None);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        page.draw(&mut frame);
    }
    let paints = sources.borrow().paints.clone();
    paints
}

/// **The top fade is drawn**: a tucked page lays the 72 px ground gradient over the top edge, after
/// the grid and under the capsule, and a resting page draws none. The unit test of `draw_top_fade`
/// alone could not see a page that never called it, or called it under the grid.
#[test]
fn a_tucked_page_draws_the_72px_top_fade_over_the_grid_and_under_the_capsule_and_a_resting_page_none() {
    let _g = plx_base::testlock::serial();
    let fixture = super::tests::Fixture::new();
    let mut page = fixture.screen();
    // the fade is abutting full-width strips (the eased ramp, `scroll_capsule::fade_strips`)
    let is_fade = |p: &std::rc::Rc<plx_ui::frame::backdrop::Paint>| {
        let r = p.placed();
        p.kind() == "rect" && r[0] == 0.0 && r[2] == super::SCR_W && r[3] == scroll_capsule::FADE_STEP && r[1] < scroll_capsule::FADE_H
    };

    let at_rest = declared_by_a_draw(&fixture, &mut page);
    assert!(!at_rest.iter().any(is_fade), "no fade at the top of the page");

    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    page.handoff.step(0.0, pitch, pitch, pitch, page.capsule_wanted()); // a jump lands on the decided state
    let tucked = declared_by_a_draw(&fixture, &mut page);
    let at = tucked.iter().position(is_fade).expect("a tucked page draws the top fade");
    let strips: Vec<[f32; 4]> = tucked.iter().filter(|p| is_fade(p)).map(|p| p.placed()).collect();
    assert_eq!(strips.len(), (scroll_capsule::FADE_H / scroll_capsule::FADE_STEP) as usize, "exactly one fade: its strips down to FADE_H");
    assert_eq!(strips[0][1], 0.0, "from the very top edge");
    assert!(strips.windows(2).all(|w| w[1][1] == w[0][1] + w[0][3]), "abutting");
    assert_eq!(strips.last().map(|r| r[1] + r[3]), Some(scroll_capsule::FADE_H));
    assert!(tucked.iter().all(|p| p.kind() != "glass"), "a one-library page has no header names, so no capsule: only the fade stays");
    assert!(tucked[..at].iter().any(|p| p.placed()[1] > 0.0 && p.placed()[3] > 0.0),
        "and over the grid: the page's own paints come before it");
}
