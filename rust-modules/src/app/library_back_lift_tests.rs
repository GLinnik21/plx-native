//! **BACK from a Detail onto the Library grid leaves its rows where they stood.** Owner, on the TV:
//! "when entering a movie details and going back the row below the selected card bounces". The
//! frames are the product's: the route dip's own image decision (`PageImage::plan`) after every
//! World frame, as `recommended_enter_tests` runs it, and the grid's settled geometry read back
//! (`World::poster_cell`, the caption bands folded in).

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_ui::containers::transition::{PageDip, PageImage, PagePaint};

struct ImageHeld;
impl Drop for ImageHeld {
    fn drop(&mut self) { plx_ui::containers::transition::set_page_image_held(false); }
}

/// One World frame and the dispatcher's image decision on it, stated for the next tick.
fn step(w: &mut World, image: &mut PageImage, valid: &mut bool, inputs: Vec<InputEvent<u32>>) {
    w.step(inputs);
    let quiescent = !plx_machine::idle::page_layout_moving();
    let transition = &w.d.nav.tabs.stack.transition;
    let (alpha, flight) = (transition.page_alpha(), transition.in_flight());
    let Some(entry) = w.d.nav.top_page().map(|e| e.id) else { return };
    match image.plan(entry, flight, alpha, w.frame * 16, *valid, quiescent) {
        PagePaint::Capture => { image.captured(entry); *valid = true; plx_ui::containers::transition::set_page_image_held(true); }
        PagePaint::ReplacementCapture => { image.replacement_captured(entry); *valid = true; plx_ui::containers::transition::set_page_image_held(true); }
        PagePaint::Live => { *image = PageImage::default(); *valid = false; plx_ui::containers::transition::set_page_image_held(false); }
        PagePaint::Held(_) => plx_ui::containers::transition::set_page_image_held(true),
    }
}

#[test]
fn back_from_a_detail_leaves_the_library_grid_rows_where_they_stood() {
    let _held = ImageHeld;
    let mut w = World::movies_library("library-back-lift", false);
    w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    let (mut image, mut valid) = (PageImage::default(), false);
    for _ in 0..90 { step(&mut w, &mut image, &mut valid, vec![]); }
    assert_eq!(w.grid_cell(), Some((0, 0)), "premise: the library opens on its first poster");
    // the first cell of the row under the focused one
    let below = |w: &World| {
        let first = w.poster_cell(0);
        (1..64).map(|i| w.poster_cell(i)).find(|cell| cell.y > first.y + 1.0).expect("premise: a second row")
    };
    let settled = below(&w).y;
    let t = tick(w.frame + 1);
    step(&mut w, &mut image, &mut valid, script_key(Key::Ok, t));
    for _ in 0..90 { step(&mut w, &mut image, &mut valid, vec![]); }
    assert!(!w.on(&AppArg::Library), "premise: OK on the poster opened its Detail");
    let t = tick(w.frame + 1);
    step(&mut w, &mut image, &mut valid, script_key(Key::Back, t));
    let mut ys = Vec::new();
    for _ in 0..90 {
        step(&mut w, &mut image, &mut valid, vec![]);
        if w.on(&AppArg::Library) { ys.push(below(&w).y); }
    }
    assert!(!ys.is_empty(), "premise: BACK returned to the Library");
    assert!(ys.iter().all(|y| (y - settled).abs() < 0.5),
        "the row under the focused poster moved on the way back (settled at {settled}): {ys:?}");
}
