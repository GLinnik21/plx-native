//! Original Library window assertions, now run against the production owned layout.
use super::layout::{library_window, MAX_LIBRARY_PILLS};

    #[test]
    fn a_row_that_fits_draws_every_library_and_no_overflow() {
        let (start, len) = library_window(&[300.0, 260.0, 240.0], 0, 1500.0, 8.0, 90.0, MAX_LIBRARY_PILLS);
        assert_eq!((start, len), (0, 3));
    }

    #[test]
    fn an_overflowing_row_always_keeps_the_selected_library() {
        let w = [400.0, 400.0, 400.0, 400.0, 400.0];
        // Wide enough for two pills plus the `+N`, so three of the five cannot be drawn.
        let (start, len) = library_window(&w, 4, 1000.0, 8.0, 90.0, MAX_LIBRARY_PILLS);
        assert!(len < w.len(), "the fixture must actually overflow, or this grades nothing");
        assert!(
            (start..start + len).contains(&4),
            "the window {start}..{} does not hold the selected library",
            start + len
        );
    }

    #[test]
    fn an_overflowing_row_starts_at_the_front_when_the_selection_is_there() {
        let w = [400.0, 400.0, 400.0, 400.0, 400.0];
        let (start, len) = library_window(&w, 0, 1000.0, 8.0, 90.0, MAX_LIBRARY_PILLS);
        assert_eq!(start, 0);
        assert!(len < w.len());
    }

    #[test]
    fn a_pill_wider_than_the_band_is_still_drawn() {
        let (start, len) = library_window(&[4000.0, 300.0], 0, 1000.0, 8.0, 90.0, MAX_LIBRARY_PILLS);
        assert_eq!((start, len), (0, 1));
    }

    #[test]
    fn more_libraries_than_the_array_holds_overflow_rather_than_being_truncated_away() {
        let w = [120.0; 9];
        let (start, len) = library_window(&w, 8, 1500.0, 8.0, 90.0, MAX_LIBRARY_PILLS);
        assert!(
            len < w.len(),
            "all {} were returned, so `hidden` is 0 and no `+N` is built — the tail is dropped by \
             the array instead",
            w.len()
        );
        assert!(
            len + 1 <= MAX_LIBRARY_PILLS,
            "the `+N` has to fit the array beside the {len} libraries, or it is the pill that gets \
             truncated away"
        );
        assert!(
            (start..start + len).contains(&8),
            "the window {start}..{} does not hold the selected library",
            start + len
        );
    }
