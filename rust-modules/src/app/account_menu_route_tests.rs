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
