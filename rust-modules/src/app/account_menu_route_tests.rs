//! **The account menu by keys** (routes.md R2, design review ACCT), on the one route rig ([`World`]):
//! the profile chip in the top bar, reached the way a person reaches it, opened with OK and closed with
//! BACK. The Library's "+N" and Sort & Filter (`library_route_tests`) and the Watchlist's Sort & Filter
//! (`watchlist_disc_route_tests`) have their own rows. The Collections page has no Sort control to
//! open today (PMS declares only `titleSort` for `type=18`, `collections_page::sort_visible`), so its
//! header row is the crumb alone and there is no opener on it to press.

use super::*;
use super::test_support::World;
use plx_ui::widgets::TAB_KEY_CHIP;

/// **The profile chip opens the account menu by keys, and BACK puts focus back on the chip.** Up from
/// the grid reaches the header, Up again the bar's pill, Left along the bar the chip; OK presents
/// the menu over the page (the page stays on top and the heartbeat still says "library"; the menu is
/// the surface on top, wearing the "account" word), and BACK closes it without moving the page, focus
/// back on the chip that opened it. Its panel's growth out of the chip and the lit chip are paint
/// (`account_menu::the_panel_grows_out_of_the_chip_and_settles_on_its_frame`, the capture sheet).
#[test]
fn the_profile_chip_opens_the_account_menu_by_keys_and_back_returns_focus_to_it() {
    let mut w = World::movies_library("route-account-menu", false);
    let page = w.d.nav.top_page().map(|e| e.id);
    w.climb_into_header();
    w.press(Key::Up);
    assert!(w.on_nav(), "premise: Up from the header reaches the bar");
    for _ in 0..8 {
        if w.elem() == Some(TAB_KEY_CHIP) { break; }
        w.press(Key::Left);
    }
    assert_eq!(w.elem(), Some(TAB_KEY_CHIP), "Left along the bar reaches the profile chip");
    assert!(!crate::app::bridge::account_menu_up(&w.d), "premise: focus alone opens nothing");

    w.press(Key::Ok);
    assert!(crate::app::bridge::account_menu_up(&w.d), "OK on the chip presents the account menu");
    assert_eq!(w.d.nav.top_page().map(|e| e.id), page, "a surface over the page, not a replacement of it");
    assert!(w.on(&AppArg::Library));
    assert_eq!(crate::app::bridge::overlay_word(&w.d), Some("account"), "and it is the account menu that is on top");
    // N-a: the chip is the menu's opener on the dispatcher's ONE lift decision, like a header pill
    // (`opener_route_tests`); there is no second path that lifts it
    let chip = plx_machine::machine::FocusKey { entry: page.expect("a page"), elem: TAB_KEY_CHIP };
    assert_eq!(w.d.lifted_opener(), Some((chip.entry, Some(chip))), "the chip is the lifted opener while its menu is up");

    w.press(Key::Back);
    // The menu eases out over more frames than a press settles in.
    w.settle(60);
    assert!(!crate::app::bridge::account_menu_up(&w.d), "BACK closes the menu");
    assert_eq!(w.d.nav.top_page().map(|e| e.id), page, "and nothing else");
    assert_eq!(w.elem(), Some(TAB_KEY_CHIP), "focus is back on the chip that opened it");
    assert_eq!(w.d.lifted_opener(), None, "and the lift stands down with the menu");
}

/// **The account menu's morph starts on the chip as the BAR draws it** (judgment 8): the rig's
/// `bar_member_rect` is `chip_cap` at the strip's live unfurl, through the host page's tuck, so a chip
/// the Watchlist or the Library has tucked is where the lit copy and the morph's seed are. Red against
/// the constant `widgets::CHIP_PILL` (the avatar's circle at no unfurl, never moved by the tuck).
#[test]
fn the_chips_drawn_rect_follows_the_bars_unfurl_and_tuck() {
    use plx_ui::dispatch::Rig;
    use plx_ui::screen::NavPresentation;
    let mut w = World::watchlist("route-account-chip-rect");
    let rest = w.bridge.bridge().bar_member_rect(TAB_KEY_CHIP, NavPresentation::default()).expect("the chip is a bar member");
    assert_eq!(rest.x, plx_ui::widgets::CHIP_FRAME.x - plx_ui::widgets::TAB_TRACK_PAD, "the capsule outsets the avatar");
    assert_eq!(rest.y, plx_ui::widgets::CHIP_FRAME.y - plx_ui::widgets::TAB_TRACK_PAD);
    let tucked = NavPresentation::default().tucked(0.5);
    let moved = w.bridge.bridge().bar_member_rect(TAB_KEY_CHIP, tucked).expect("tucked, still the chip");
    assert_eq!(moved, plx_ui::Rect { y: rest.y - plx_ui::screen::chrome_lift(0.5), ..rest }, "the tuck moves the rect the bar draws");
    assert_eq!(w.bridge.bridge().bar_member_rect(plx_ui::widgets::TAB_KEY_HOME, NavPresentation::default()), None, "only the chip is a lifted bar member");
}

/// The overlay's two readings this frame: the modal dim's alpha (`ModalStack::scrims`, the appear
/// spring the stack steps) and the account panel's morph progress (the popover's springs the screen
/// steps on its Tick).
fn dim_and_morph(w: &World) -> (f32, f32) {
    // as a share of the dim's peak, so it reads on the morph's 0..1
    let dim = w.d.nav.modals.scrims(1.0).first().map_or(0.0, |(_, alpha)| *alpha / plx_ui::theme::underlay::DIM_COMPACT);
    let morph = crate::app::bridge::account_menu(&w.d).and_then(|menu| menu.popover().morph_progress()).unwrap_or(0.0);
    (dim, morph)
}

/// What the account panel's glass is DRAWN with this frame, as the surface's own morph states it:
/// `(ground_alpha, dim, content_alpha)`. The glass's `ground_alpha` is 0 on the closed frame (before
/// the first step of an open), whatever the progress says: the slower rect spring is no longer "where
/// the glass is" (the open's width waits [`plx_ui::popover_morph::WIDTH_LAG`] while the height already
/// extends below the chip, and a close's width is back at the chip's by ~133 ms while the slab is
/// still draining under it). A close's closed frame is never read off the surface: the surface is
/// retired on the frame the droplet is handed back to the chip (`Screen::handed_back`), so from that
/// frame there is no surface and all three read 0. The opener rect only picks the droplet's geometry,
/// which none of these three read, so the chip's resting circle stands in for the one the rig has not
/// drawn yet.
fn glass_and_own_dim(w: &World) -> (f32, f32, f32) {
    let chip = plx_ui::widgets::chip_cap(0.0, 0.0);
    crate::app::bridge::account_menu(&w.d)
        .and_then(|menu| menu.popover().morph_frame(Some(chip), plx_ui::Rect::FULL, 0.0))
        .map_or((0.0, 0.0, 0.0), |f| (f.ground_alpha, f.dim, f.content_alpha))
}

/// **One clock: the dim and the glass morph begin on the same frame, reverse on the same frame and
/// end together** (N-b). They are two springs of different stiffness (the dim ~16 frames, the morph
/// ~5) that must never be two clocks: the dim is visible exactly while the morph is, from the frame
/// the menu is first seen (a held surface ticks both with no time), and on a close the scrim IS the
/// morph's own dim on every frame, so it turns round with the glass and is never left on the page
/// after the droplet is gone.
///
/// The droplet close (`popover_morph`: the bulge, the width and height on separate springs, the slab
/// draining into the opener) replaced two readings this test took from the earlier close. The rect's
/// progress is not "where the glass is": it was 0 on the open's first glass frame (the height leads,
/// the width waits its lag) and is 0 from ~150 ms of a close while the slab still drains for ~100 ms
/// more. The glass is gone on the frame its `ground_alpha` is, the closed frame the hand-back lands
/// on. And the dim follows the HEIGHT out, which the bulge holds for about two frames, so it no
/// longer turns "within the content phase" of the rect: a window of a few frames around the rect's
/// turn would have pinned the bulge, not the clock. The clock is pinned instead by equality, on every
/// frame of both legs, with the morph's own dim.
#[test]
fn the_dim_and_the_glass_morph_open_and_close_on_one_clock() {
    let mut w = World::movies_library("route-account-one-clock", false);
    w.climb_into_header();
    w.press(Key::Up);
    for _ in 0..8 {
        if w.elem() == Some(TAB_KEY_CHIP) { break; }
        w.press(Key::Left);
    }
    assert_eq!(w.elem(), Some(TAB_KEY_CHIP), "premise: the chip holds focus");
    w.settle(30);

    // OPEN: frame by frame from the press
    w.tap(Key::Ok, 0);
    let mut open = vec![dim_and_morph(&w)];
    let mut open_glass = vec![glass_and_own_dim(&w)];
    for _ in 0..60 {
        w.step(vec![]);
        open.push(dim_and_morph(&w));
        open_glass.push(glass_and_own_dim(&w));
    }
    assert!(crate::app::bridge::account_menu_up(&w.d), "premise: the menu is up");
    let first_dim = open.iter().position(|(dim, _)| *dim > 0.0).expect("the dim ramps");
    let first_morph = open.iter().position(|(_, morph)| *morph > 0.0).expect("the glass morphs");
    assert_eq!(first_dim, first_morph, "the dim and the morph begin on the same frame: {open:?}");
    for (n, (dim, morph)) in open.iter().enumerate() {
        assert_eq!(*dim > 0.0, *morph > 0.0, "open frame {n}: the dim is visible iff the morph is ({dim}, {morph})");
    }
    assert!(open.last().is_some_and(|(dim, morph)| *dim > 0.99 && *morph > 0.99), "both landed: {:?}", open.last());

    // CLOSE: both turn round on the frame the surface goes Closing
    w.tap(Key::Back, 0);
    let mut close = vec![dim_and_morph(&w)];
    let mut close_glass = vec![glass_and_own_dim(&w)];
    for _ in 0..60 {
        w.step(vec![]);
        close.push(dim_and_morph(&w));
        close_glass.push(glass_and_own_dim(&w));
    }

    // ONE clock: on every frame of both legs the scrim the container draws IS the dim the morph states
    for (leg, readings, glass) in [("open", &open, &open_glass), ("close", &close, &close_glass)] {
        for (n, ((dim, _), (_, own, _))) in readings.iter().zip(glass).enumerate() {
            assert!((dim - own).abs() < 1e-4, "{leg} frame {n}: the scrim ({dim}) is not the morph's own dim ({own})");
        }
    }
    // the dim is never on the page without the glass: on every frame of both legs
    for (leg, readings, glass) in [("open", &open, &open_glass), ("close", &close, &close_glass)] {
        for (n, ((dim, _), (ground, _, _))) in readings.iter().zip(glass).enumerate() {
            assert!(*dim <= 0.0 || *ground > 0.0, "{leg} frame {n}: a dim ({dim}) with no glass drawn");
        }
    }
    // The close leads with the rows and the glass: on the first frame after the BACK the rows are
    // leaving and the slab has turned round (the dim, one clock with them, follows the height the
    // bulge holds for about two frames).
    assert!(close_glass[1].2 < 1.0, "the rows start leaving on the first close frame: {close_glass:?}");
    assert!(close[1].1 < 1.0, "the glass's slab turns round on the first close frame: {close:?}");
    assert!(close.iter().any(|(dim, _)| *dim < 0.5), "the dim falls: {close:?}");
    // The glass is HANDED BACK to the chip on the first close frame its `ground_alpha` is 0: the
    // closed frame, after the slab has drained into it, which is also the frame the surface is
    // retired on (nothing of it stands to read, so the readings are 0). The rect's progress is back
    // at 0 before that (the slab drains on), and the dim is out no later than the glass: it follows
    // the slab's height, which the drain takes to nothing a frame or two before the fold lands, so a
    // dim left on the page after the droplet is gone, or one outliving it by more than the last
    // sliver, fails here.
    let handed_back = close_glass.iter().position(|(ground, _, _)| *ground <= 0.0).expect("the droplet is handed back to the chip");
    let dim_gone = close.iter().position(|(dim, _)| *dim <= 0.0).expect("the dim is gone");
    assert!(dim_gone <= handed_back && handed_back - dim_gone <= 2, "the dim ({dim_gone}) is out no later than the glass ({handed_back}) and within its last sliver: {close:?}");
    assert!(close[handed_back..].iter().all(|(dim, morph)| *dim <= 0.0 && *morph <= 0.0), "after the hand-back nothing of the morph is left: {close:?}");
    let progress_gone = close.iter().position(|(_, morph)| *morph <= 0.0).expect("the glass's rect is back at the opener's size");
    assert!(progress_gone <= handed_back, "the rect is back before the hand-back, which waits for the slab to drain: {close:?}");
}

/// **The morph begins and ends on the chip AS THE BAR DRAWS IT, on every frame the menu is up**
/// (N-b, judgment 8): opened over the Watchlist by keys (the chip holds focus, so it is the lifted
/// opener, as in `the_profile_chip_opens_the_account_menu_by_keys_and_back_returns_focus_to_it`), the
/// popover's opener is the rect the lit chip is drawn at (`bar_member_rect` through the host page's
/// tuck; the tuck itself is graded by `the_chips_drawn_rect_follows_the_bars_unfurl_and_tuck`), frame
/// by frame through the open and the close; and the CLOSED frame the open begins on, the one before the
/// glass first draws, is that rect with a circle's corner (the chip at rest is a round surround,
/// radius = half its height).
///
/// The closed frame is the frame whose `ground_alpha` is 0, not the frame whose `progress` is: the
/// open's height leads while the width waits its lag, so the glass is already a drop extending below
/// the chip (`ground_alpha` 0.5, 106 px tall) at progress 0.
///
/// The close has no closed frame to read, by design: the surface hands the chip back on the frame its
/// droplet is gone (`Screen::handed_back`, which `Surface::faded` reads), so the frame the morph would be
/// closed on is the frame the surface and the opener's lift leave on, and the chrome pass draws the chip.
/// Before that fix the surface stood ~17 frames more on closed frames, the lift painting the chip on the
/// stack's latched field (a lighter body) until the container's spring came to rest, and the chip snapped
/// darker in one frame (owner film, 2026-11-18). The close's end is read on both sides of that frame
/// instead. On the last frame glass is drawn the opener's re-formed blob (`Frame::merge`) is whole on
/// the chip's rect AS THE BAR DRAWS IT (centred, bottom edge on the chip's), the slab a sliver under its
/// centre and the opener's face back, and the lift still owns the chip. From the first frame after it
/// the surface and the lift are gone, and the bar draws the chip where it did before the menu opened.
/// Both ends are counted apart, since one counter was satisfied by the open alone and the close's end
/// was never read.
#[test]
fn the_morph_begins_and_ends_on_the_chip_as_the_bar_draws_it() {
    use plx_ui::dispatch::Rig;
    let mut w = World::watchlist("route-account-morph-rect");
    w.land_watchlist();
    w.settle(30);
    let page = w.d.nav.top_page().map(|e| e.id).expect("a page");
    let drawn = |d: &Dispatcher<AppHost>, owner: &mut crate::app::runtime_owners::Harness| {
        let bridge = owner.bridge();
        let presented = bridge.navigation_presentation(&d.nav).tucked(d.top_screen().expect("a page").chrome_tuck());
        bridge.bar_member_rect(TAB_KEY_CHIP, presented).expect("the chip is a bar member")
    };
    // by keys, as a person opens it: the chip holds focus, so it is the menu's opener on the lift decision
    walk_to_the_chip(&mut w);
    w.settle(30);
    let at_rest = drawn(&w.d, &mut w.bridge);
    // frames read, the closed ones on the way in and (there must be none) on the way out, the last
    // frame glass is drawn on the way out with the chip as drawn then, whether the lift still owned
    // the chip then, and the frame after BACK the surface left on
    let mut read = 0;
    let (mut closed_in, mut closed_out) = (0, 0);
    let mut last_glass = None;
    let mut lift_on_last = false;
    let mut left_after = None;
    // `back_at`: the frame of the BACK, once the close has begun
    let mut watch = |w: &mut World, back_at: Option<u32>| {
        w.drawn(); // the dispatcher hands the surface the rect in its draw
        let Some(menu) = crate::app::bridge::account_menu(&w.d) else {
            if let Some(back_at) = back_at {
                left_after.get_or_insert(w.frame - back_at);
                assert_eq!(w.d.lifted_opener(), None, "frame {}: the surface is gone, so is the lift: the bar draws the chip", w.frame);
            }
            return;
        };
        let Some(from) = menu.popover().opener() else { return };
        let rect = drawn(&w.d, &mut w.bridge);
        assert_eq!(from, rect, "frame {}: the morph's opener is the chip as drawn", w.frame);
        let panel = plx_ui::popover::cover_opener(from, (316.0, 400.0), plx_ui::consts::SAFE);
        let f = menu.popover().morph_frame(Some(from), panel, 26.0).unwrap();
        read += 1;
        if back_at.is_some() {
            assert!(!menu.popover().handed_back(), "frame {}: the droplet is handed back and the surface still stands", w.frame);
            last_glass = Some((rect, f));
            let chip = plx_machine::machine::FocusKey { entry: page, elem: TAB_KEY_CHIP };
            lift_on_last = w.d.lifted_opener() == Some((page, Some(chip)));
        }
        if f.ground_alpha == 0.0 {
            if back_at.is_some() { closed_out += 1 } else { closed_in += 1 }
            assert_eq!(f.rect, rect, "frame {}: with no glass drawn the frame IS the chip's rect", w.frame);
            assert_eq!(f.radius, rect.h * 0.5, "frame {}: and its corner the chip's round surround", w.frame);
        }
    };
    w.tap(Key::Ok, 0);
    watch(&mut w, None);
    for _ in 0..80 { w.step(vec![]); watch(&mut w, None); }
    assert!(crate::app::bridge::account_menu_up(&w.d), "premise: the menu is up");
    w.tap(Key::Back, 0);
    let back_at = w.frame;
    for _ in 0..80 { w.step(vec![]); watch(&mut w, Some(back_at)); }
    assert!(!crate::app::bridge::account_menu_up(&w.d), "premise: it closed");
    assert!(read > 20, "the opener was read on {read} frames");
    assert!(closed_in > 0, "the open began on the chip: {closed_in} closed frames before the glass first drew");
    assert_eq!(closed_out, 0, "the close ended with the surface: {closed_out} frames stood closed, the droplet handed back and the opener's lift still painting the chip");
    // the surface left with its droplet (about 15 frames after BACK), not with the container's appear
    // spring (a Sheet's rest, about 31)
    let left = left_after.expect("the surface left");
    assert!(left < 24, "the surface left {left} frames after BACK");
    // the close ENDED ON THE CHIP as the bar draws it: on the last frame glass is drawn, the opener's
    // re-formed blob is the chip's rect (anchored on its bottom edge, the one nearest the slab), whole,
    // with the opener's own face back and the slab a sliver on the chip's centre
    let (chip, last) = last_glass.expect("the close drew glass");
    let blob = last.merge.expect("the droplet drains into the opener's re-formed blob");
    assert!((blob.rect.cx() - chip.cx()).abs() < 0.01, "the blob is centred on the chip: {blob:?} on {chip:?}");
    assert!((blob.rect.y + blob.rect.h - (chip.y + chip.h)).abs() < 0.01, "the blob stands on the chip's bottom edge: {blob:?} on {chip:?}");
    assert!(blob.rect.w > chip.w * 0.9 && blob.rect.h > chip.h * 0.9, "the blob is the chip, whole: {blob:?} on {chip:?}");
    assert!(blob.tint > 0.95, "the blob wears the chip's own fill: {blob:?}");
    assert!(last.opener_alpha > 0.9, "the chip's own face is back: {last:?}");
    assert!((last.rect.cx() - chip.cx()).abs() < 0.01 && last.rect.w < chip.w * 0.2, "what is left of the slab is a sliver on the chip's centre: {:?} on {chip:?}", last.rect);
    assert!(lift_on_last, "the lift still owned the chip on the last frame the surface stood");
    // and from there the bar draws the chip where it did before the menu opened
    assert_eq!(drawn(&w.d, &mut w.bridge), at_rest, "the chip is back where the bar drew it");
}

/// Walk focus to the profile chip by keys (Up to the bar, Left along it).
fn walk_to_the_chip(w: &mut World) {
    for _ in 0..14 {
        if w.elem() == Some(TAB_KEY_CHIP) { break; }
        w.press(if w.on_nav() { Key::Left } else { Key::Up });
    }
    assert_eq!(w.elem(), Some(TAB_KEY_CHIP), "premise: the chip holds focus");
}

/// Walk focus to the profile chip by keys and open the account menu.
fn open_account_menu_from_the_chip(w: &mut World) {
    walk_to_the_chip(w);
    w.press(Key::Ok);
    assert!(crate::app::bridge::account_menu_up(&w.d), "premise: the account menu is up");
    w.settle(40);
}

/// Page-scope damage raised from the frame of a BACK on the open account menu through the close,
/// counted on the owning `Present` (`page_damage_epoch` never resets, so a swap cannot hide it).
fn page_damage_on_close(w: &mut World) -> u64 {
    let before = w.d.present.page_damage_epoch();
    w.tap(Key::Back, 0);
    w.frames(40);
    assert!(!crate::app::bridge::account_menu_up(&w.d), "premise: BACK closed the menu");
    w.d.present.page_damage_epoch() - before
}

/// **Closing the account menu with BACK raises no page damage.** The menu is a `Style::Sheet`, which
/// freezes the page under it, and typed page damage is what retakes that snapshot on the close's first
/// frame: the page redrawn into it, the tab bar's blur re-sourced, the dim re-latched (14.9M GPU
/// cycles against an 11.2M budget on the TV, a dropped frame on every close). The dismissal re-enters
/// the covered page, which hears `FocusMoved { from: Some(chip), to: chip, by: Restore }` (a move that
/// moves nothing) after its `RestoreMemory` re-armed the saved-place jump; Home answered with `Input`
/// page damage, and the Watchlist's `Uncover`, `Enter` and `FocusMoved` each damaged unconditionally
/// (`watchlist/page.rs`); both now damage only for what changed. The page damage the close raises is
/// what this rig can see of the host's snapshot: it runs no GL, so `popover::host`'s retake itself is
/// read at the host level (`popover_host_tests`), and a close with no typed damage re-takes nothing.
#[test]
fn closing_the_account_menu_with_back_raises_no_page_damage() {
    let worlds: [(&str, fn() -> World); 6] = [
        ("home", || World::home_grid("route-account-quiet-home", 3, 5)),
        ("library", || World::movies_library("route-account-quiet-library", false)),
        ("recommended", || World::recommended("route-account-quiet-recommended")),
        ("collections", || World::collections("route-account-quiet-collections", false)),
        ("episodes", || World::episodes_library("route-account-quiet-episodes")),
        ("watchlist", || World::watchlist("route-account-quiet-watchlist")),
    ];
    let mut noisy = Vec::new();
    for (name, build) in worlds {
        let mut w = build();
        w.settle(30);
        open_account_menu_from_the_chip(&mut w);
        let damage = page_damage_on_close(&mut w);
        if damage != 0 { noisy.push((name, damage)); }
    }
    assert!(noisy.is_empty(), "BACK on the account menu damaged the page under it: {noisy:?}");
}
