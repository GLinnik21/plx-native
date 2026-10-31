//! The strip's control keys: one list, all distinct, and no key declared beside it.
//!
//! The distinctness is also a compile-time assert next to the keys. The list is built by the same
//! `strip_keys!` lines that declare the constants, so the failure this file guards is the one the
//! macro cannot see: a key written as a plain `pub const` that never joins the list.

use super::*;

/// The values are part of the focus vocabulary a recording carries (`focus` elements are
/// `STRIP_BASE + offset`), so they are pinned, not derived: a key never moves.
#[test]
fn the_strip_keys_keep_their_offsets() {
    use crate::dispatch::STRIP_BASE;
    assert_eq!(
        [TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_SHOWS, TAB_KEY_SEARCH, TAB_KEY_CHIP, TAB_KEY_WATCHLIST, TAB_KEY_VIDEOS],
        [STRIP_BASE, STRIP_BASE + 1, STRIP_BASE + 2, STRIP_BASE + 3, STRIP_BASE + 4, STRIP_BASE + 5, STRIP_BASE + 7],
    );
    // +6 is left open on purpose: the Live TV pull request (#993) takes it, and the build refuses
    // two destinations on one key whichever of the two lands first.
    assert!(!TAB_KEYS.contains(&(STRIP_BASE + 6)));
}

#[test]
fn all_strip_keys_are_distinct_and_listed_once() {
    let mut seen = std::collections::BTreeSet::new();
    for key in TAB_KEYS {
        assert!(*key >= crate::dispatch::STRIP_BASE, "{key:#x} is outside the strip namespace");
        assert!(seen.insert(*key), "{key:#x} is listed twice");
    }
    assert!(TAB_KEYS.contains(&TAB_KEY_CHIP) && TAB_KEYS.contains(&TAB_KEY_WATCHLIST));
}

/// A `pub const TAB_KEY_*` outside `strip_keys!` would be a key the list and the distinctness assert
/// cannot see (two open pull requests each adding "the next free offset" is how that happens).
/// The macro's own lines are `NAME = offset;`, so every such line must be one of the list's keys and
/// no plain declaration may exist.
#[test]
fn no_strip_key_is_declared_beside_the_list() {
    let source = include_str!("widgets.rs");
    let plain: Vec<&str> = source
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("pub const TAB_KEY_") || line.starts_with("const TAB_KEY_"))
        .collect();
    assert!(plain.is_empty(), "declare strip keys inside strip_keys!, not as constants: {plain:?}");
    let declared = source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("TAB_KEY_") && line.ends_with(';') && line.contains(" = "))
        .count();
    assert_eq!(declared, TAB_KEYS.len(), "each strip_keys! line is one entry of TAB_KEYS");
}
