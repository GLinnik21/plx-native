//! The band of rows a paged store keeps cards for, and the one cut it applies to what a screen asks
//! (Home's hub rows and a Library section's Recommended page).
//!
//! A screen tells its store which rows it wants held (`ui::cards::HoldBand` decides when to say
//! so); the store gives the cards of every other row up and keeps a descriptor in their place.
//! The store keeps at most [`HOLD_ROWS`] rows of what it was asked, cut by [`clamp_hold`]. A screen
//! that judges "is what I asked for still enough?" against its own request, and not against what
//! the store KEPT, believes in rows the store released: Recommended's BACK from rows 20..23 asked
//! for rows 0..27, the store kept 5..20, and rows 0..4 stayed placeholders with no read in flight.
//!
//! It is defined here, in `base`, and not in `data` or `ui`: the store (`data`) applies the cut and
//! the band (`ui`) must apply the same one to know what is held, and neither may name the other
//! (the same reason as [`tile`](crate::tile)). Both call this function, so there is one cut.

/// Rows of Home (and of a Library section's Recommended page) that hold cards at once: the rows in
/// view plus a margin. Every other row keeps its descriptor (identity, title, key, total, window
/// offset, ledger, the count of cards it last showed) and publishes that many placeholder slots, so
/// every row stays reachable and nothing is left off.
///
/// Bound on held cards: `HOLD_ROWS` x `MAX_SHELF_ITEMS` = 16 x 24 = 384, plus the merged
/// Continue Watching row (one window of the deck, at most `MAX_SHELF_ITEMS`) and at most
/// `HERO_MAX` retained heroes, however many rows the servers offer. Descriptors are a few hundred
/// bytes each; they are the directory.
///
/// **The rule a screen's band has to keep:** the rows in view plus `2 * (HOLD_MARGIN + HOLD_SLACK)`
/// (`ui::cards`) fit in `HOLD_ROWS`, or the store keeps less than the view needs.
/// `ui::cards` states it as a compile-time assertion against this constant.
pub const HOLD_ROWS: usize = 16;

/// The held range as the store keeps it: at most [`HOLD_ROWS`] rows, centred on what the screen
/// asked for when it asked for more.
pub fn clamp_hold((lo, hi): (usize, usize)) -> (usize, usize) {
    let hi = hi.max(lo);
    if hi - lo < HOLD_ROWS { return (lo, hi); }
    let lo = ((lo + hi) / 2).saturating_sub(HOLD_ROWS / 2);
    (lo, lo + HOLD_ROWS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_band_that_fits_is_kept_whole_and_a_taller_one_is_cut_to_the_rows_around_its_middle() {
        assert_eq!(clamp_hold((3, 3)), (3, 3));
        assert_eq!(clamp_hold((4, 4 + HOLD_ROWS - 1)), (4, 4 + HOLD_ROWS - 1), "exactly the bound fits");
        assert_eq!(clamp_hold((0, 27)), (5, 20), "the review's BACK: rows 0..27 asked, 5..20 kept");
        let (lo, hi) = clamp_hold((0, 100));
        assert_eq!(hi - lo + 1, HOLD_ROWS);
        assert_eq!(clamp_hold((9, 2)), (9, 9), "an inverted ask is its first row");
    }

    #[test]
    fn the_cut_is_idempotent() {
        for (lo, hi) in [(0, 27), (3, 90), (40, 41), (0, 15), (0, 16)] {
            let once = clamp_hold((lo, hi));
            assert_eq!(clamp_hold(once), once, "{lo}..{hi}");
        }
    }
}
