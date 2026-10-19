use super::scroll_chrome::*;
use plx_ui::scroll_capsule;

const PITCH: f32 = 480.0;

#[test]
fn the_tuck_is_zero_at_rest_and_one_once_the_first_row_has_gone() {
    assert_eq!(tuck(0.0, PITCH), 0.0);
    assert_eq!(tuck(PITCH - TUCK_RAMP, PITCH), 0.0, "the ramp starts exactly one ramp before");
    assert_eq!(tuck(PITCH, PITCH), 1.0);
    assert_eq!(tuck(PITCH * 5.0, PITCH), 1.0);
    let mid = tuck(PITCH - TUCK_RAMP * 0.5, PITCH);
    assert!(mid > 0.0 && mid < 1.0);
    let mut last = 0.0;
    for s in 0..=PITCH as i32 {
        let t = tuck(s as f32, PITCH);
        assert!(t >= last, "monotonic in scroll");
        last = t;
    }
}

#[test]
fn a_short_pitch_still_leaves_scroll_zero_untucked() {
    assert_eq!(tuck(0.0, 40.0), 0.0);
    assert_eq!(tuck(0.0, 0.0), 0.0);
}

#[test]
fn a_resting_page_has_a_constant_tuck_so_it_asks_for_no_frames() {
    let (a, moving) = plx_machine::idle::scoped_motion(|| tuck(PITCH * 3.0, PITCH));
    assert_eq!(a, 1.0);
    assert!(!moving, "the tuck is derived from the scroll spring; it animates nothing itself");
}

#[test]
fn rows_settle_at_120_or_lower_and_the_clamp_never_pushes_down() {
    assert_eq!(settle_scroll(500.0, 130.0), 500.0, "already clear");
    assert_eq!(settle_scroll(500.0, 120.0), 500.0);
    assert_eq!(settle_scroll(500.0, 100.0), 480.0, "pulled back by the 20 px deficit");
    assert_eq!(settle_scroll(10.0, -50.0), 0.0, "never below the page top");
    assert!(SETTLE_MIN_Y >= scroll_capsule::Y + scroll_capsule::H, "clear of the capsule");
}

#[test]
fn the_capsule_runs_are_the_whole_name_then_the_count() {
    let (name, count) = capsule_parts("Films", 1284);
    assert_eq!(name.to_str().unwrap(), "Films");
    assert!(count.to_str().unwrap().contains("284"), "{count:?}");
    let long = "Documentaries and Concerts of the Century, Remastered In Full";
    let (name, count) = capsule_parts(long, 1284);
    assert_eq!(name.to_str().unwrap(), long, "never shortened, no ellipsis");
    assert!(count.to_str().unwrap().contains('1'));
}

#[test]
fn the_capsule_shows_only_where_the_header_names_libraries() {
    assert!(!capsule_shown(0), "a one-library header row names none: nothing to restate");
    assert!(capsule_shown(2));
    assert!(capsule_shown(8));
}

#[test]
fn the_right_edge_is_the_last_column_of_whatever_the_grid_has() {
    assert!(!on_last_column(None, 6));
    assert!(!on_last_column(Some(0), 6));
    assert!(on_last_column(Some(5), 6));
    assert!(on_last_column(Some(11), 6));
    assert!(on_last_column(Some(3), 4), "the episode grid has four columns");
    assert!(!on_last_column(Some(3), 0));
}

#[test]
fn every_grid_row_rests_clear_of_the_capsule() {
    use super::layout::Layout;
    let lay = Layout::new(false, 400);
    for row in 0..lay.rows {
        let rest = lay.row_y(row, lay.row_reveal(row));
        assert!(rest >= SETTLE_MIN_Y - 0.01 || lay.row_reveal(row) == 0.0, "row {row} rests at {rest}");
    }
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
    let is_fade = |p: &std::rc::Rc<plx_ui::frame::backdrop::Paint>| {
        p.kind() == "rect" && p.placed() == [0.0, 0.0, super::SCR_W, scroll_capsule::FADE_H]
    };

    let at_rest = declared_by_a_draw(&fixture, &mut page);
    assert!(!at_rest.iter().any(is_fade), "no fade at the top of the page");

    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    let tucked = declared_by_a_draw(&fixture, &mut page);
    let at = tucked.iter().position(is_fade).expect("a tucked page draws the top fade");
    assert_eq!(tucked.iter().filter(|p| is_fade(p)).count(), 1, "exactly one fade");
    assert!(tucked.iter().all(|p| p.kind() != "glass"), "a one-library page has no header names, so no capsule: only the fade stays");
    assert!(tucked[..at].iter().any(|p| p.placed()[1] > 0.0 && p.placed()[3] > 0.0),
        "and over the grid: the page's own paints come before it");
}
