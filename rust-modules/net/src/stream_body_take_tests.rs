//! A sized body over 4 GiB on the 32-bit television. `remaining` (an `i64`) used to be cast to
//! `usize` before the comparison, which on armv7 keeps only its low 32 bits: a 23,346,250,742-byte
//! film read as finished at file byte 1,871,414,262 (the size minus 5·2³²), and again every 4 GiB
//! after a seek — a premature end of the movie. On a 64-bit host the cast is lossless, so these
//! tests can only fail on a 32-bit target (they were observed failing in an armv7 container).

use super::body_take;

const REPORTED_SIZE: i64 = 23_346_250_742;
const FOUR_GIB: i64 = 1 << 32;

#[test]
fn a_remainder_that_is_a_multiple_of_4_gib_is_not_the_end() {
    assert_eq!(body_take(65_536, 5 * FOUR_GIB), 65_536);
}

#[test]
fn the_reported_film_is_not_finished_at_1_87_gb() {
    let cut = REPORTED_SIZE - 5 * FOUR_GIB;
    assert_eq!(
        cut, 1_871_414_262,
        "fixture: where the truncated count reached zero"
    );
    assert_eq!(body_take(65_536, REPORTED_SIZE - cut), 65_536);
}

#[test]
fn a_remainder_just_past_4_gib_is_not_a_short_read() {
    // Truncated, 4 GiB + 10 read as 10 bytes left.
    assert_eq!(body_take(65_536, FOUR_GIB + 10), 65_536);
}

#[test]
fn a_small_or_spent_remainder_still_bounds_the_read() {
    assert_eq!(body_take(65_536, 10), 10);
    assert_eq!(body_take(65_536, 0), 0);
    assert_eq!(body_take(65_536, -3), 0);
}
