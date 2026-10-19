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
    let w = World::watchlist("route-account-chip-rect");
    let rest = w.bridge.bar_member_rect(TAB_KEY_CHIP, NavPresentation::default()).expect("the chip is a bar member");
    assert_eq!(rest.x, plx_ui::widgets::CHIP_FRAME.x - plx_ui::widgets::TAB_TRACK_PAD, "the capsule outsets the avatar");
    assert_eq!(rest.y, plx_ui::widgets::CHIP_FRAME.y - plx_ui::widgets::TAB_TRACK_PAD);
    let tucked = NavPresentation::default().tucked(0.5);
    let moved = w.bridge.bar_member_rect(TAB_KEY_CHIP, tucked).expect("tucked, still the chip");
    assert_eq!(moved, plx_ui::Rect { y: rest.y - plx_ui::screen::chrome_lift(0.5), ..rest }, "the tuck moves the rect the bar draws");
    assert_eq!(w.bridge.bar_member_rect(plx_ui::widgets::TAB_KEY_HOME, NavPresentation::default()), None, "only the chip is a lifted bar member");
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

/// **One clock: the dim and the glass morph begin on the same frame and reverse on the same frame**
/// (N-b). They are two springs of different stiffness (the dim ~16 frames, the morph ~5) that must
/// never be two clocks: the dim is visible exactly while the morph is, from the frame the menu is
/// first seen (a held surface ticks both with no time), and on a close both turn round on the frame
/// the surface goes `Closing`, not one frame apart.
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
    for _ in 0..60 {
        w.step(vec![]);
        open.push(dim_and_morph(&w));
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
    for _ in 0..60 {
        w.step(vec![]);
        close.push(dim_and_morph(&w));
    }
    let turned = |pick: fn(&(f32, f32)) -> f32| close.iter().position(|reading| pick(reading) < 0.999).expect("it falls");
    assert_eq!(turned(|reading| reading.0), turned(|reading| reading.1), "the dim and the morph turn round on the same frame: {close:?}");
    let gone = |pick: fn(&(f32, f32)) -> f32| close.iter().position(|reading| pick(reading) <= 0.0);
    assert!(gone(|reading| reading.1).is_some(), "the glass is back on the chip: {close:?}");
}

/// **The morph begins and ends on the chip AS THE BAR DRAWS IT, on every frame the menu is up**
/// (N-b, judgment 8): opened over the Watchlist, the popover's opener is the rect the lit chip is
/// drawn at (`bar_member_rect` through the host page's tuck; the tuck itself is graded by
/// `the_chips_drawn_rect_follows_the_bars_unfurl_and_tuck`), frame by frame through the open and the
/// close, and the morph at its first and last frame is that rect with a circle's corner (the chip at
/// rest is a round surround, radius = half its height).
#[test]
fn the_morph_begins_and_ends_on_the_chip_as_the_bar_draws_it() {
    use plx_ui::dispatch::Rig;
    let mut w = World::watchlist("route-account-morph-rect");
    w.land_watchlist();
    w.settle(30);
    let drawn = |w: &World| {
        let presented = w.bridge.navigation_presentation().tucked(w.d.top_screen().expect("a page").chrome_tuck());
        w.bridge.bar_member_rect(TAB_KEY_CHIP, presented).expect("the chip is a bar member")
    };
    crate::app::bridge::open_account_menu(&mut w.d);
    let mut seen = [0, 0];
    let watch = |w: &mut World, seen: &mut [u32; 2]| {
        w.drawn(); // the dispatcher hands the surface the rect in its draw
        let Some(menu) = crate::app::bridge::account_menu(&w.d) else { return };
        let Some(from) = menu.popover().opener() else { return };
        let rect = drawn(w);
        assert_eq!(from, rect, "frame {}: the morph's opener is the chip as drawn", w.frame);
        let f = menu.popover().morph_frame(Some(from), plx_ui::Rect::new(from.x, from.y + from.h + 14.0, 316.0, 400.0), 26.0).unwrap();
        if f.progress == 0.0 {
            seen[1] += 1;
            assert_eq!(f.rect, rect, "frame {}: at progress 0 the glass IS the chip's rect", w.frame);
            assert_eq!(f.radius, rect.h * 0.5, "frame {}: and its corner the chip's round surround", w.frame);
        }
        seen[0] += 1;
    };
    for _ in 0..80 { w.step(vec![]); watch(&mut w, &mut seen); }
    assert!(crate::app::bridge::account_menu_up(&w.d), "premise: the menu is up");
    w.tap(Key::Back, 0);
    for _ in 0..80 { w.step(vec![]); watch(&mut w, &mut seen); }
    assert!(!crate::app::bridge::account_menu_up(&w.d), "premise: it closed");
    assert!(seen[0] > 20 && seen[1] > 0, "the opener was read on {} frames, {} of them at progress 0", seen[0], seen[1]);
}
