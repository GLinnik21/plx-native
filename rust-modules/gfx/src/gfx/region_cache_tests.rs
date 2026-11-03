//! The host pins of the region cache (`gfx::region_paint`): the slot step (`RegionSlot::serve`, the
//! very function the renderer calls, here with a counter for a texture) and the extent arithmetic
//! need no GL, so the policy "the same inputs are one render, each changed input is another" is
//! stated here without a context. The GL halves (the negative-origin target, the premultiplied quad)
//! are the simulator's and the television's to prove.

use super::*;

/// What `serve` is lent in place of the texture: how many times it was filled and drawn.
#[derive(Default)]
struct Texture {
    fills: u32,
    draws: u32,
}

fn ask(slot: &mut RegionSlot, tex: &mut Texture, key: u64, lands: bool) -> bool {
    slot.serve(
        tex,
        key,
        |t| {
            t.fills += 1;
            lands
        },
        |t| {
            let _ = t;
            true
        },
    )
}

/// Draw count as the renderer sees it: every ask that is served draws once.
fn ask_counted(slot: &mut RegionSlot, tex: &mut Texture, key: u64) {
    let drawn = slot.serve(tex, key, |t| { t.fills += 1; true }, |_| true);
    if drawn {
        tex.draws += 1;
    }
}

/// The same key is one fill, however many frames and however many pages ask for it.
#[test]
fn the_same_key_is_one_fill_and_every_later_ask_is_a_quad() {
    let (mut slot, mut tex) = (RegionSlot::new(), Texture::default());
    for _ in 0..50 {
        ask_counted(&mut slot, &mut tex, 7);
    }
    assert_eq!((tex.fills, tex.draws), (1, 50), "one fill, fifty quads");
}

/// A push or a pop draws two previews a frame, and both ask for the SAME picture: the one slot serves
/// both, so thirty frames of two pages are one fill.
#[test]
fn two_pages_asking_for_the_same_picture_share_the_one_fill() {
    let (mut slot, mut tex) = (RegionSlot::new(), Texture::default());
    for _frame in 0..30 {
        for _page in 0..2 {
            ask_counted(&mut slot, &mut tex, 11);
        }
    }
    assert_eq!((tex.fills, tex.draws), (1, 60));
}

/// Each changed input is a new key and one more fill, and the picture before it is not kept.
#[test]
fn a_new_key_is_one_more_fill() {
    let (mut slot, mut tex) = (RegionSlot::new(), Texture::default());
    ask_counted(&mut slot, &mut tex, 1);
    ask_counted(&mut slot, &mut tex, 1);
    ask_counted(&mut slot, &mut tex, 2);
    ask_counted(&mut slot, &mut tex, 2);
    assert_eq!(tex.fills, 2);
    ask_counted(&mut slot, &mut tex, 1);
    assert_eq!(tex.fills, 3, "the one slot holds one picture: going back is a fill");
}

/// A fill that did not land (the target declined) leaves the slot empty and draws nothing: the next
/// ask fills again rather than serving a texture that holds something else.
#[test]
fn a_fill_that_never_landed_is_asked_again_and_draws_nothing() {
    let (mut slot, mut tex) = (RegionSlot::new(), Texture::default());
    assert!(!ask(&mut slot, &mut tex, 5, false));
    assert!(!ask(&mut slot, &mut tex, 5, false), "never landed, never cached");
    assert_eq!(tex.fills, 2);
    assert!(ask(&mut slot, &mut tex, 5, true), "it lands, and is drawn");
    assert!(ask(&mut slot, &mut tex, 5, false), "and then it is held: the fill is not called");
    assert_eq!(tex.fills, 3);
}

/// A texture given back holds nothing: the key it had is not served from it again.
#[test]
fn a_forgotten_slot_fills_again() {
    let (mut slot, mut tex) = (RegionSlot::new(), Texture::default());
    ask_counted(&mut slot, &mut tex, 3);
    slot.forget();
    ask_counted(&mut slot, &mut tex, 3);
    assert_eq!(tex.fills, 2);
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
/// live draw is the only picture, and no texture is held. An unkeyed frame (a picture that is not
/// settled) is the same, and still counts as the page asking.
#[test]
fn with_no_gl_or_no_key_the_region_declines_and_paint_does_not_run() {
    let mut ran = false;
    let drawn = region_paint(Some(1), [0.0, 0.0, 10.0, 10.0], [0.0; 2], [1.0; 4], &mut || ran = true);
    assert!(!drawn && !ran);
    assert!(!region_paint(None, [0.0, 0.0, 10.0, 10.0], [0.0; 2], [1.0; 4], &mut || ran = true) && !ran);
    assert_eq!(region_resident_bytes(), 0, "nothing was made");
    region_frame_end();
    assert_eq!(region_resident_bytes(), 0);
}
