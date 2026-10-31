// `anchor_panel`: the ONE placer of a panel beside the control that opened it (HUD-plugin plan, D9).
use super::*;
use crate::consts::{SAFE, SCR_H, SCR_W};

const GAP: f32 = 28.0;

fn menu(anchor: Rect, w: f32, h: f32, need_h: f32) -> Anchored {
    anchor_panel(anchor, (w, h), need_h, GAP, Placement::PLAYER_MENU)
}

fn bottom(r: Rect) -> f32 {
    r.y + r.h
}

fn right(r: Rect) -> f32 {
    r.x + r.w
}

fn inside(r: Rect, keep: Rect) -> bool {
    r.x >= keep.x && r.y >= keep.y && right(r) <= right(keep) && bottom(r) <= bottom(keep)
}

fn apart(a: Rect, b: Rect) -> bool {
    right(a) <= b.x || right(b) <= a.x || bottom(a) <= b.y || bottom(b) <= a.y
}

/// The pre-`anchor_panel` body of `under_or_above`, kept as the model its rects are held to.
fn legacy_under_or_above(a: Rect, content_w: f32, content_h: f32) -> Rect {
    let width = content_w.clamp(crate::table::MENU_MIN_W, crate::table::MENU_MAX_W);
    let h = content_h.clamp(120.0, SCR_H - 2.0 * PANEL_EDGE);
    let below = a.y + a.h + PANEL_GAP;
    let y = if below + h <= SCR_H - PANEL_EDGE { below } else { (a.y - PANEL_GAP - h).max(PANEL_EDGE) };
    let x = a.x.clamp(PANEL_EDGE_X, (SCR_W - PANEL_EDGE_X - width).max(PANEL_EDGE_X));
    Rect::new(x, y, width, h)
}

/// **`under_or_above` is a call of the shared placer and moves nothing.** Every anchor on a grid
/// that crosses both vertical branches, the clamps at both ends of the width and of the height, and
/// both screen edges answers the rect the old body did, bit for bit.
#[test]
fn under_or_above_answers_the_rects_it_always_did() {
    let mut n = 0;
    for ay in [0.0, 54.0, 146.0, 400.0, 540.0, 700.0, 880.0, 1000.0, 1040.0] {
        for ax in [0.0, 60.0, 96.0, 700.0, 1500.0, 1700.0, 1900.0] {
            for (aw, ah) in [(40.0, 40.0), (230.0, 64.0), (400.0, 300.0)] {
                for cw in [0.0, 300.0, 420.0, 650.0, 900.0] {
                    for ch in [0.0, 120.0, 400.0, 700.0, 952.0, 2000.0] {
                        let a = Rect::new(ax, ay, aw, ah);
                        assert_eq!(under_or_above(a, cw, ch), legacy_under_or_above(a, cw, ch), "anchor {a:?} content {cw}x{ch}");
                        n += 1;
                    }
                }
            }
        }
    }
    assert_eq!(n, 9 * 7 * 3 * 5 * 6, "the grid ran whole");
}

/// The side a panel takes is the one facing the screen's centre: an opener in the lower half opens
/// ABOVE, one in the upper half BELOW, and the panel keeps the gap between them.
#[test]
fn the_panel_opens_on_the_side_facing_the_screen_centre() {
    let low = Rect::new(1400.0, 792.0, 424.0, 64.0);
    let r = menu(low, 440.0, 500.0, 500.0);
    assert_eq!(r.side, Side::Above, "lower half: above");
    assert_eq!(bottom(r.rect), low.y - GAP, "the gap is the whole distance to the opener");
    assert_eq!(r.rect.h, 500.0);

    let high = Rect::new(1400.0, 120.0, 424.0, 64.0);
    let r = menu(high, 440.0, 500.0, 500.0);
    assert_eq!(r.side, Side::Below, "upper half: below");
    assert_eq!(r.rect.y, bottom(high) + GAP);
    assert_eq!(r.rect.h, 500.0);
}

/// Neither vertical side holds the panel, so it flips sideways: toward the centre, which for an
/// opener in the right half is START (to the left of the opener, centred on it).
#[test]
fn a_panel_too_tall_for_above_and_below_opens_on_the_side_facing_the_centre() {
    let opener = Rect::new(1700.0, 560.0, 64.0, 64.0);
    let r = menu(opener, 440.0, 600.0, 600.0);
    assert_eq!(r.side, Side::Start, "right half: to the left of the opener");
    assert_eq!(right(r.rect), opener.x - GAP);
    assert_eq!((r.rect.w, r.rect.h), (440.0, 600.0));
    assert_eq!(r.rect.y + r.rect.h * 0.5, opener.cy(), "centred on the opener when the frame has the room");

    let left = Rect::new(120.0, 560.0, 64.0, 64.0);
    let r = menu(left, 440.0, 600.0, 600.0);
    assert_eq!(r.side, Side::End, "left half: to the right of the opener");
    assert_eq!(r.rect.x, right(left) + GAP);
}

/// A caller's own side order is tried in order, and the first side whose room holds the panel wins:
/// START with no width falls through to END.
#[test]
fn a_fixed_order_flips_to_the_next_side_that_holds_the_panel() {
    let keep = Rect::new(0.0, 0.0, 1000.0, 1000.0);
    let how = |order| Placement { order: SideOrder::Fixed(order), cross: Cross::Leading, overflow: Overflow::ClampToRoom, keep };
    let top = Rect::new(300.0, 10.0, 100.0, 40.0);
    let r = anchor_panel(top, (300.0, 400.0), 400.0, 20.0, how(&[Side::Above, Side::Below]));
    assert_eq!(r.side, Side::Below, "no room above the top edge");
    assert_eq!(r.rect.y, 70.0);

    let near_left = Rect::new(100.0, 400.0, 100.0, 40.0);
    let r = anchor_panel(near_left, (300.0, 200.0), 200.0, 20.0, how(&[Side::Start, Side::End]));
    assert_eq!(r.side, Side::End, "100 px to the left cannot hold 300 + 20");
    assert_eq!(r.rect.x, 220.0);
}

/// The panel always lies inside `keep`: across the axis it is pulled in at the edge, and a
/// sideways panel centred on an opener near the bottom slides up until its bottom is `keep`'s.
#[test]
fn a_panel_is_clamped_inside_the_safe_frame() {
    let near_right = Rect::new(1800.0, 792.0, 64.0, 64.0);
    let r = menu(near_right, 440.0, 400.0, 400.0);
    assert_eq!(r.side, Side::Above);
    assert_eq!(right(r.rect), SAFE.x + SAFE.w, "the hang from the opener's right edge, held inside SAFE");
    assert!(inside(r.rect, SAFE));

    let near_left = Rect::new(20.0, 120.0, 64.0, 64.0);
    let r = menu(near_left, 440.0, 400.0, 400.0);
    assert_eq!(r.side, Side::Below);
    assert_eq!(r.rect.x, SAFE.x, "a left-half opener hangs from its left edge, pulled in to SAFE");

    let low_right = Rect::new(1700.0, 900.0, 64.0, 64.0);
    let r = menu(low_right, 440.0, 900.0, 900.0);
    assert_eq!(r.side, Side::Start);
    assert_eq!(bottom(r.rect), bottom(SAFE), "centred would overflow, so it rests on the frame");
    assert!(inside(r.rect, SAFE));
}

/// **No side holds the panel**: the vertical side with the most room wins, and the panel shrinks to
/// that room, so it never covers its opener (the list inside scrolls). `under_or_above`'s
/// `ClampTop` is the other rule and is pinned by `alt_sources_tests`.
#[test]
fn when_no_side_holds_the_panel_the_most_room_wins_and_the_height_is_clamped_to_it() {
    let keep = Rect::new(0.0, 0.0, 1000.0, 1000.0);
    let how = Placement { order: SideOrder::Facing, cross: Cross::Hang, overflow: Overflow::ClampToRoom, keep };
    let opener = Rect::new(100.0, 600.0, 100.0, 100.0);
    // room above 600 - 20 = 580, below 1000 - 720 = 280; END has 780 wide but only 600 fit, and the frame is 1000 tall: ask 900
    let r = anchor_panel(opener, (900.0, 900.0), 900.0, 20.0, how);
    assert_eq!(r.side, Side::Above, "the larger vertical room");
    assert_eq!(r.rect.h, 580.0, "height clamped to the room");
    assert_eq!(bottom(r.rect), opener.y - 20.0);
    assert!(apart(r.rect, opener), "never over the opener");
    assert!(inside(r.rect, keep));

    let low = Rect::new(100.0, 100.0, 100.0, 100.0);
    let r = anchor_panel(low, (900.0, 950.0), 950.0, 20.0, how);
    assert_eq!(r.side, Side::Below);
    assert_eq!(r.rect.h, 1000.0 - 220.0);
    assert!(apart(r.rect, low));
}

/// A server-length list is taller than `need_h`, which decides the SIDE, so the height past it is
/// the room's and the list scrolls; the side does not depend on how long the list is.
#[test]
fn the_side_follows_the_need_and_the_height_follows_the_room() {
    let opener = Rect::new(1400.0, 792.0, 424.0, 64.0);
    let short = menu(opener, 440.0, 300.0, 600.0);
    let long = menu(opener, 440.0, 2000.0, 600.0);
    assert_eq!((short.side, long.side), (Side::Above, Side::Above));
    assert_eq!(short.rect.h, 300.0);
    assert_eq!(long.rect.h, opener.y - GAP - SAFE.y, "clamped to the room above");
    assert_eq!(long.rect.y, SAFE.y);
}

/// **Openers in one segment share a panel edge.** The panel hangs from the anchor's outer edge, so
/// two anchors that end at the same edge (the segment's) give panels that end at the same edge, and
/// the panel's width does not move that edge.
#[test]
fn anchors_ending_at_one_segment_edge_share_the_panels_edge() {
    let edge = SAFE.x + SAFE.w;
    for w in [300.0, 440.0, 650.0] {
        for anchor_w in [64.0, 148.0, 424.0] {
            let a = Rect::new(edge - anchor_w, 792.0, anchor_w, 64.0);
            let r = menu(a, w, 400.0, 400.0);
            assert_eq!(right(r.rect), edge, "anchor {anchor_w} wide, panel {w} wide");
        }
    }
}

/// The placer is a pure function of the anchor and the sizes: the same inputs give the same rect
/// however often it is asked, and a different anchor gives a different one. This is the half of the
/// latch the placer owns (it reads nothing live); the other half, that a menu keeps the anchor it
/// was opened with, is structural (`with_anchor` sets a private field once) and is pinned where
/// the menus are, by `every_menu_arm_latches_the_anchor_it_is_handed` and the menus' own tests.
#[test]
fn the_same_anchor_and_size_place_the_same_panel_every_time() {
    let anchor = Rect::new(1400.0, 792.0, 424.0, 64.0);
    let first = menu(anchor, 440.0, 500.0, 600.0);
    for _ in 0..8 {
        assert_eq!(menu(anchor, 440.0, 500.0, 600.0), first);
    }
    let moved = menu(Rect::new(1400.0, 300.0, 424.0, 64.0), 440.0, 500.0, 600.0);
    assert_ne!(moved.rect, first.rect, "a different anchor is a different panel");
}

/// The rise comes from the opener's side: from below for a panel above its opener, from above for
/// one below it, sideways for START and END; and the vertical rise every existing caller passes is
/// unchanged, frame for frame.
#[test]
fn the_rise_starts_displaced_toward_the_openers_side() {
    let r = Popover::RISE;
    assert_eq!(Popover::rise_offset(Side::Above, r, 0.0), (0.0, r), "above: rises up from below");
    assert_eq!(Popover::rise_offset(Side::Below, r, 0.0), (0.0, -r), "below: drops down from above");
    assert_eq!(Popover::rise_offset(Side::Start, r, 0.0), (r, 0.0), "start: slides left from the opener on its right");
    assert_eq!(Popover::rise_offset(Side::End, r, 0.0), (-r, 0.0));
    for side in [Side::Above, Side::Below, Side::Start, Side::End] {
        assert_eq!(Popover::rise_offset(side, r, 1.0), (0.0, 0.0), "{side:?}: arrived");
    }
    for a in [0.0f32, 0.1, 0.37, 0.5, 0.99, 1.0] {
        assert_eq!(Popover::rise_offset(Side::Above, r, a), (0.0, r * (1.0 - a)), "the vertical rise is the old one at appear {a}");
    }
}
