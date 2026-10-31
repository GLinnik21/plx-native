//! The host pins of the region cache (`gfx::region_paint`): the keyed slot table and the extent
//! arithmetic need no GL, so the policy "the same inputs are one render, each changed input is
//! another" is stated here without a context. The GL halves (the negative-origin target, the
//! premultiplied quad) are the simulator's and the television's to prove.

use super::*;

/// The slot table is the whole policy of the region cache that needs no GL: the same key is one
/// fill, however many frames and however many pages ask for it.
#[test]
fn the_same_key_is_one_render_and_every_later_ask_is_cached() {
    let mut s = KeyedSlots::<2>::new();
    let RegionPlan::Render(i) = s.plan(7) else { panic!("an empty table renders") };
    s.filled(i, 7);
    for _ in 0..50 {
        assert_eq!(s.plan(7), RegionPlan::Cached(i), "the key a slot holds is never rendered again");
    }
}

/// Each changed input is a new key: a fill, in another slot while one is free, and the old picture
/// stays cached for the page that still wants it.
#[test]
fn a_new_key_renders_into_a_free_slot_and_leaves_the_old_one_cached() {
    let mut s = KeyedSlots::<2>::new();
    let RegionPlan::Render(a) = s.plan(1) else { panic!() };
    s.filled(a, 1);
    let RegionPlan::Render(b) = s.plan(2) else { panic!("another key is another fill") };
    assert_ne!(a, b, "a free slot is taken before a held one is evicted");
    s.filled(b, 2);
    assert_eq!(s.plan(1), RegionPlan::Cached(a));
    assert_eq!(s.plan(2), RegionPlan::Cached(b));
}

/// A push or a pop draws two previews a frame whose keys differ (the picker's hovered caption is not
/// the stored one): alternating asks must not refill a slot each time.
#[test]
fn two_pages_with_two_keys_alternating_render_each_once() {
    let mut s = KeyedSlots::<2>::new();
    let mut fills = 0;
    for _frame in 0..30 {
        for key in [10u64, 20] {
            if let RegionPlan::Render(i) = s.plan(key) {
                fills += 1;
                s.filled(i, key);
            }
        }
    }
    assert_eq!(fills, 2, "one fill per distinct key over thirty frames of both pages");
}

/// A third key evicts the least recently used one, never the one a page just asked for.
#[test]
fn a_third_key_evicts_the_least_recently_used() {
    let mut s = KeyedSlots::<2>::new();
    for key in [1u64, 2] {
        let RegionPlan::Render(i) = s.plan(key) else { panic!() };
        s.filled(i, key);
    }
    assert!(matches!(s.plan(1), RegionPlan::Cached(_)), "touch 1: 2 is now the oldest");
    let RegionPlan::Render(i) = s.plan(3) else { panic!() };
    s.filled(i, 3);
    assert!(matches!(s.plan(1), RegionPlan::Cached(_)) && matches!(s.plan(3), RegionPlan::Cached(_)));
    assert!(matches!(s.plan(2), RegionPlan::Render(_)), "2 was evicted");
}

/// A fill that did not land (the target declined) leaves its slot empty: the next ask renders again
/// rather than serving a texture that holds something else.
#[test]
fn a_fill_that_never_landed_is_asked_again() {
    let mut s = KeyedSlots::<2>::new();
    assert!(matches!(s.plan(5), RegionPlan::Render(_)));
    assert!(matches!(s.plan(5), RegionPlan::Render(_)), "never `filled`, never cached");
}

/// The texture is the rect grown by the slack and rounded outward, so the AA bleed and a
/// fractional-position quad's bilinear taps read transparent texels.
#[test]
fn the_extent_grows_by_the_slack_and_rounds_outward() {
    assert_eq!(region_extent([96.0, 470.0, 660.0, 556.0]), [92, 466, 668, 564]);
    assert_eq!(region_extent([96.5, 470.25, 660.0, 556.0]), [92, 466, 669, 565]);
}

/// The extent is part of the key: the same picture at another place is another texture.
#[test]
fn the_same_key_at_another_extent_is_another_key() {
    let a = region_mix(9, [92, 466, 668, 564]);
    assert_ne!(a, region_mix(9, [93, 466, 668, 564]));
    assert_ne!(a, region_mix(9, [92, 466, 668, 565]));
    assert_eq!(a, region_mix(9, [92, 466, 668, 564]));
}

/// Without a GL context (a host test) the region is never drawn and `paint` never runs: the caller's
/// live draw is the only picture. Without a key (a picture that is not settled) the same.
#[test]
fn with_no_gl_or_no_key_the_region_declines_and_paint_does_not_run() {
    let mut ran = false;
    let drawn = region_paint(Some(1), [0.0, 0.0, 10.0, 10.0], [0.0; 2], [1.0; 4], &mut || ran = true);
    assert!(!drawn && !ran);
    assert!(!region_paint(None, [0.0, 0.0, 10.0, 10.0], [0.0; 2], [1.0; 4], &mut || ran = true) && !ran);
}
