//! **Home's placeholder count** (`plx_ui::placeholder`): the hero backdrop whose art has not
//! arrived. The count is at the DRAW, never at the resolve — `bind` records the pending path and
//! `Backdrop::draw` counts it, and only when the hero layer is on screen.

use super::*;
use plx_ui::placeholder::{self, Reason};

fn hero_with_art(art: &str) -> PmsMovie {
    PmsMovie { art: art.into(), ..Default::default() }
}

fn bound(art: &str, snap: f32) -> Backdrop {
    let movie = hero_with_art(art);
    let mut b = Backdrop::new();
    b.bind(Some(HeroRef { item: &movie, source: "" }), None, None, snap);
    b
}

fn env(sp: f32, hero_a: f32) -> Env {
    let mut env = Env::inert();
    env.sp = sp;
    env.hero_a = hero_a;
    env
}

fn drawn(b: &Backdrop, env: &Env) -> placeholder::Frame {
    placeholder::capture_declared(|| b.draw(Painter::root(), env, None)).1
}

#[test]
fn a_hero_whose_art_has_not_arrived_counts_one_backdrop_at_the_draw() {
    let b = bound("/library/metadata/5/art/1", 0.0);
    assert_eq!(b.pending_art, "/library/metadata/5/art/1");
    let f = drawn(&b, &env(0.0, 1.0));
    assert_eq!((f.count, f.of(Reason::HomeBackdrop)), (1, 1), "{f:?}");
    assert_eq!(f.entries[0].key, "/library/metadata/5/art/1");
}

/// `bind` resolves; it is the DRAW that counts. Binding alone, with no draw, counts nothing.
#[test]
fn binding_the_art_is_not_a_placeholder_draw() {
    let f = placeholder::capture_declared(|| bound("/library/metadata/5/art/1", 0.0)).1;
    assert_eq!(f.count, 0, "{f:?}");
}

#[test]
fn a_hero_with_no_art_to_fetch_is_absence() {
    let b = bound("", 0.0);
    assert!(b.pending_art.is_empty());
    assert_eq!(drawn(&b, &env(0.0, 1.0)).count, 0);
}

#[test]
fn a_hero_layer_that_is_not_on_screen_counts_nothing() {
    // Scrolled into the grid: the hero art is culled and `bind` records nothing...
    let b = bound("/library/metadata/5/art/1", 1.0);
    assert!(b.pending_art.is_empty());
    assert_eq!(drawn(&b, &env(1.0, 0.0)).count, 0);
    // ...and a hero that has faded out is not drawn even when a path is still pending.
    let b = bound("/library/metadata/5/art/1", 0.0);
    assert_eq!(drawn(&b, &env(0.0, 0.0)).count, 0);
}

#[test]
fn a_recording_pass_over_the_backdrop_counts_nothing() {
    let b = bound("/library/metadata/5/art/1", 0.0);
    let f = placeholder::capture_declared(|| b.draw(Painter::recording(), &env(0.0, 1.0), None)).1;
    assert_eq!(f.count, 0, "{f:?}");
}
