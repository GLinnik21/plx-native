//! **The Detail page's placeholder count** (`plx_ui::placeholder`): the backdrop whose art has not
//! arrived. The condition is a pure function so the hook is held by a test, not by a grep: `draw_backdrop`
//! itself reaches GL (scissor scopes, the ambient wash), which the host cannot run.

use super::backdrop_pending;

#[test]
fn art_that_is_drawn_and_has_no_texture_is_a_wait() {
    assert!(backdrop_pending(1.0, 0, "/library/metadata/5/art/1"));
    assert!(backdrop_pending(0.02, 0, "/library/metadata/5/art/1"), "just above the visibility floor");
}

#[test]
fn art_that_has_arrived_is_not_a_wait() {
    assert!(!backdrop_pending(1.0, 7, "/library/metadata/5/art/1"));
}

#[test]
fn an_item_with_no_artwork_is_absence() {
    assert!(!backdrop_pending(1.0, 0, ""), "nothing will ever arrive for an empty path");
}

#[test]
fn a_backdrop_that_is_not_drawn_waits_for_nothing() {
    // scrolled away, or the preview plane covers it: `art_alpha` is at or under the draw floor
    assert!(!backdrop_pending(0.0, 0, "/library/metadata/5/art/1"));
    assert!(!backdrop_pending(0.01, 0, "/library/metadata/5/art/1"));
}
