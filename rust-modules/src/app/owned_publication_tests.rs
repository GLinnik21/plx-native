//! Owned publication gates are independent; unchanged observations do not repaint.
use super::*;

#[test]
fn session_cache_landing_damages_only_its_owner_once() {
    let _g = plx_base::testlock::serial();
    let mut bridge = Bridge::for_test(|| 0);
    let mut owner = Dispatcher::<AppHost>::new();
    let other = Dispatcher::<AppHost>::new();
    let other_epoch = other.present.damage_epoch();
    let other_page = other.present.page_damage_epoch();
    let before = owner.present.damage_epoch();
    let page_before = owner.present.page_damage_epoch();
    owner.present.set_scope(plx_machine::present::Scope::Surface);
    bridge.session_cache_generation = plx_plex::plex::session::visible_generation().wrapping_sub(1);
    bridge.land_session_cache(&mut owner);
    assert_eq!(owner.present.damage_epoch(), before + 1);
    assert_eq!(owner.present.page_damage_epoch(), page_before + 1);
    bridge.land_session_cache(&mut owner);
    assert_eq!(owner.present.damage_epoch(), before + 1, "one visible generation, one landing");
    assert_eq!(other.present.damage_epoch(), other_epoch);
    assert_eq!(other.present.page_damage_epoch(), other_page);
}

#[test]
fn unmounted_watchlist_commands_have_no_page_authority() {
    let _g = plx_base::testlock::serial();
    let mut bridge = Bridge::for_test(|| 0);
    let mut owner = Dispatcher::<AppHost>::new();
    let other = Dispatcher::<AppHost>::new();
    let before = owner.present.damage_epoch();
    let other_before = other.present.damage_epoch();
    bridge.watchlist_reread();
    bridge.watchlist_feed(&mut owner);
    bridge.catalog_feed(&mut owner);
    bridge.watchlist_feed(&mut owner);
    assert_eq!(owner.present.damage_epoch(), before, "unmounted scalar/store changes publish no page");
    assert_eq!(other.present.damage_epoch(), other_before);
}
