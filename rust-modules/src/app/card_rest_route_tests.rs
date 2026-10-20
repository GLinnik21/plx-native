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
/// the cards the walk released draw with the plain program (`pop_factor == 0.0`).
fn walk_right_then_rest(w: &mut World, steps: usize, what: &str) {
    // RowStyle::HOME's pop; any peak above 1.0 turns an exact 1.0 into 0.0 and one ulp above it into more.
    let peak = 1.09;
    for _ in 0..steps { w.tap(Key::Right, 2); }
    w.frames(120);
    let off = w.cards_off_rest();
    assert_eq!(off.len(), 1, "{what}: after {steps} cards of a held walk only the focused card is off rest, got {off:?}");
    assert!(off[0].2 > 1.0, "{what}: the focused card is popped: {off:?}");
    let pops = w.card_pops();
    let shelf = off[0].0;
    assert!(off[0].1 >= steps, "{what}: the walk reached card {steps} of its shelf: {off:?}");
    for released in 0..off[0].1 {
        assert_eq!(plx_ui::widgets::pop_factor(pops[shelf][released], peak), 0.0,
            "{what}: released card {released} still draws with the focus program (pop {})", pops[shelf][released]);
    }
}

#[test]
fn a_home_shelf_walked_with_a_held_key_leaves_every_released_card_at_exact_rest() {
    let mut w = World::home_grid("route-rest-home", 3, 12);
    w.press(Key::Down);
    assert_eq!(w.cards_off_rest().len(), 1, "premise: the first shelf's first card holds focus and is the only one popped");
    walk_right_then_rest(&mut w, 8, "Home");
}

#[test]
fn a_recommended_shelf_walked_with_a_held_key_leaves_every_released_card_at_exact_rest() {
    let mut w = World::recommended("route-rest-recommended");
    assert_eq!(w.cards_off_rest().len(), 1, "premise: the page arrives in its first card, the only one popped");
    walk_right_then_rest(&mut w, 2, "Recommended");
}
