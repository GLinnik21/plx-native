//! `held_keys`: the cards in the rows Home holds (layer-2 L2-5), which a re-read of the page asks the
//! server about. The window is `state.hold` as the store keeps it, so it is checked against the
//! ring's own bound and against the rows the ring released.

use super::*;
use crate::stores::HeldKeys;

const ROWS: usize = 40;
const CARDS: usize = 3;

/// A ring of `ROWS` rows of `CARDS` cards whose rating keys name their row (`7-<row>-<card>`), the
/// rows outside `hold` given up to placeholders.
fn ring(hold: (usize, usize)) -> (PmsState, ServerId) {
    let mut state = PmsState::default();
    let adapter = Arc::new(PmsAdapter::default());
    seed_ring_for_test(&mut state, &adapter, ROWS, CARDS, &|_| 7, hold);
    let sid = state.srcs[0].sid;
    (state, sid)
}

fn row_keys(sid: ServerId, rows: std::ops::RangeInclusive<usize>) -> HeldKeys {
    rows.flat_map(|row| (0..CARDS).map(move |c| (sid, format!("7-{row}-{c}")))).collect()
}

/// Catalog row `k` is a source shelf only when no deck row leads: this fixture has none.
#[test]
fn held_keys_match_hold_window() {
    let _guard = plx_base::testlock::serial();
    let (state, sid) = ring((10, 12));
    assert_eq!(held_keys(&state), row_keys(sid, 10..=12),
        "exactly the cards of the rows held, in catalog order, none of the released rows' placeholders");
    let (head, sid) = ring((0, 1));
    assert_eq!(held_keys(&head), row_keys(sid, 0..=1));
}

/// A hold wider than the ring's bound is centred on the middle, the way the store keeps it, and
/// lists no more than the bound's rows.
#[test]
fn held_keys_follow_the_clamped_hold() {
    let _guard = plx_base::testlock::serial();
    let (state, sid) = ring((0, ROWS - 1));
    let (lo, hi) = clamp_hold((0, ROWS - 1));
    assert_eq!(hi - lo + 1, HOLD_ROWS);
    assert_eq!(held_keys(&state), row_keys(sid, lo..=hi));
}

#[test]
fn an_empty_home_holds_nothing() {
    assert!(held_keys(&PmsState::default()).is_empty());
}

#[test]
fn source_ids_are_the_sources_in_display_order() {
    let _guard = plx_base::testlock::serial();
    let (state, sid) = ring((0, 1));
    assert_eq!(source_ids(&state), vec![sid]);
    assert!(source_ids(&PmsState::default()).is_empty());
}
