//! **A card the focus leaves lands at EXACT rest, on the routes a person walks with keys** (#861).
//!
//! `gfx::spring` never lands a released spring on its target, so a card the walk had visited used to
//! settle one ulp above scale 1.0, which `ui::widgets::pop_factor` turns into a pop factor of about
//! 1e-6, which sends the card to the heavier focus program and out of the resting split for as long as
//! the page stays up (`Spring::land`, called by `CardRow::update` for every cell the focus is not on).
//! `ui::card_row`'s own test pins the row; these two walk a real `Dispatcher<AppHost>` along a shelf
//! with keys, at a held remote's cadence, and read the pop of every card the page would draw.
//! Search and Detail's Cast shelf run the same `cards::Shelf` but have no World mount: the conformance case
//! `released_card_lands_at_rest` (`plx_ui::cards::conformance`, run over every card screen by the screens
//! crate's `cards_conformance_tests`) walks them at the screens tier.

use super::*;
use super::test_support::World;

/// After a walk of `steps` cards to the right, only the focused card is off rest, and it is popped;
/// the cards the walk released draw with the plain program.
fn walk_right_then_rest(w: &mut World, steps: usize, what: &str) {
    for _ in 0..steps { w.tap(Key::Right, 2); }
    w.frames(120);
    let (_, card) = w.sole_card_off_rest(&format!("{what}, after {steps} cards of a held walk"));
    assert!(card >= steps, "{what}: the walk reached card {steps} of its shelf, got card {card}");
}

#[test]
fn a_home_shelf_walked_with_a_held_key_leaves_every_released_card_at_exact_rest() {
    let mut w = World::home_grid("route-rest-home", 3, 12);
    w.press(Key::Down);
    w.sole_card_off_rest("premise: the first shelf's first card holds focus and is the only one popped");
    walk_right_then_rest(&mut w, 8, "Home");
}

#[test]
fn a_recommended_shelf_walked_with_a_held_key_leaves_every_released_card_at_exact_rest() {
    let mut w = World::recommended("route-rest-recommended");
    // the world's shelves hold three cards; a walk that leaves several behind (as Home's does) needs wider ones
    w.bridge.stores.browse.borrow_mut().seed_shelves_for_test(0, &["One", "Two", "Three", "Four", "Five", "Six"], 12);
    w.frames(60);
    w.sole_card_off_rest("premise: the page arrives in its first card, the only one popped");
    walk_right_then_rest(&mut w, 8, "Recommended");
}
