use super::*;
use crate::captured_store_fixture::{key, roster};

#[test]
fn empty_captured_home_pins_do_not_republish_on_directory_generation_alone() {
    let keys = ClientKeys::default();
    let scope = |sections_gen| BrowseScope { sections_gen, pins: &[],
        clients: std::borrow::Cow::Borrowed(&keys) };
    let mut state = PmsState::default();
    sync_roster_with_scope(&mut state, &scope(0));
    let before = state.catalog_gen;
    sync_roster_with_scope(&mut state, &scope(1));
    assert_eq!(state.catalog_gen, before,
        "an empty pin table has no new Home membership to publish");
    assert!(!browse_scope_moved(&mut state, &scope(2)));
}

#[test]
fn captured_home_pin_content_changes_are_seen_even_when_directory_generation_repeats() {
    let keys = ClientKeys::default();
    let a = [(ServerId::from_raw(7), 11, true)];
    let b = [(ServerId::from_raw(7), 11, false)];
    let scope = |pins| BrowseScope { sections_gen: 3, pins,
        clients: std::borrow::Cow::Borrowed(&keys) };
    let mut state = PmsState::default();
    sync_roster_with_scope(&mut state, &scope(&a));
    let before = state.catalog_gen;
    sync_roster_with_scope(&mut state, &scope(&a));
    assert_eq!(state.catalog_gen, before);
    sync_roster_with_scope(&mut state, &scope(&b));
    assert_ne!(state.catalog_gen, before);
    assert_eq!(state.scope_pins, b);
}

#[test]
fn captured_home_roster_preserves_key_eligibility_partial_directory_and_same_generation_facts() {
    let _serial = plx_base::testlock::serial();
    let a = roster(&[(61, Some(key(1)), "friend"), (101, None, "unkeyed")], 3, 4);
    let directory = crate::stores::browse::DirectorySnapshot::default();
    let scope = BrowseScope::retained(directory.view(), &a);
    assert_eq!(roster_with_scope(&scope), [(ServerId::from_raw(61), "friend".into())], "unknown library membership is allowed, missing keys are not");
    let mut state = PmsState::default();
    sync_roster_with_scope(&mut state, &scope);
    assert_eq!(state.srcs.len(), 1);
    assert_eq!(state.srcs[0].key, Some(key(1)));
    let b = roster(&[(61, Some(key(1)), "corrected")], 3, 4);
    sync_roster_with_scope(&mut state, &BrowseScope::retained(directory.view(), &b));
    assert_eq!(state.srcs[0].handle, "corrected", "facts content matters even when generation hints repeat");
    let c = roster(&[(61, Some(key(2)), "corrected")], 3, 4);
    sync_roster_with_scope(&mut state, &BrowseScope::retained(directory.view(), &c));
    assert_eq!(state.srcs[0].key, Some(key(2)), "full credential key is checked independently of roster generation");
    let d = roster(&[(101, Some(key(3)), "next")], 3, 4);
    sync_roster_with_scope(&mut state, &BrowseScope::retained(directory.view(), &d));
    assert_eq!(state.srcs[0].sid.raw(), 101, "same-size same-generation roster replacement is not aliased");
    let adapter = Arc::new(PmsAdapter::default());
    reset_with_scope(&mut state, &adapter, &BrowseScope::retained(directory.view(), &d));
    assert!(state.srcs.is_empty() && state.jobs.is_empty());
}
