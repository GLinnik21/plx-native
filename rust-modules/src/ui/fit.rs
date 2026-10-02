//! One declared-priority primitive for a line with two competing runs — [`two_runs`].
//!
//! A settings row's label beside its trailing value and a section header beside its trailing
//! accessory share one shape: two text runs on one line, of which ONE is the PRIMARY (the thing the
//! line is about) and the other a SECONDARY read-out that gives way first. `ui/table.rs` calls this
//! for both (`row_columns`, `header_columns`).

/// The two natural widths [`two_runs`] resolves, plus the primary's guaranteed share.
pub(crate) struct Pair {
    /// The primary's measured width, already hugged by the caller if it wants headroom.
    pub primary_nat: f32,
    /// The secondary's measured width, already capped by the caller if it has a cap.
    pub secondary_nat: f32,
    /// The share of the span the primary keeps (up to its natural width) once both no longer fit.
    /// Below `1.0` so the secondary is never squeezed to an empty column by a merely long primary.
    pub primary_share: f32,
}

/// The share of a row's or a section header's span the primary is guaranteed.
pub(crate) const ROW_PRIMARY_SHARE: f32 = 0.6;

/// The margin a primary run is measured with before it reaches [`two_runs`]: a `Measure` models
/// whole-pixel advances but not the device's kerning/hinting, so a run kept at exactly its measured
/// width can still end in an ellipsis. This is the text-fit tests' 2% headroom
/// ([`HEADROOM`]) plus a hair. It is not part of `two_runs` because the
/// header/accessory caller measures the drawn string directly.
pub(crate) const HUG_MARGIN: f32 = 1.025;

/// The share of a column a line may fill under the host measure
/// (`fontcov::advances::ShippedMeasure`, which the fit tests name this constant beside): the
/// measure models whole-pixel advances but not the device's kerning or hinting, so a line that
/// clears its column by one pixel there (731 of 732 was a real Belarusian Settings candidate) is
/// left no margin at all on the set. A popover panel is sized for `natural / HEADROOM`
/// (`TableView::measured_width`) and the fit tests grade against the same figure.
pub(crate) const HEADROOM: f32 = 0.98;

/// Resolve a primary/secondary pair onto a `span`-wide line separated by `gap`, returning
/// `(primary_w, secondary_w)`.
///
/// If both natural widths plus `gap` fit, the secondary takes its natural width and the primary
/// every pixel left. Otherwise the primary gets `max(span - secondary_nat - gap, min(primary_nat,
/// span * primary_share))` — its natural width up to its share, never less than a fully-elided
/// secondary would leave it — and the secondary gets what remains after the primary and the gap,
/// floored at `0` for a span too small to hold both.
pub(crate) fn two_runs(span: f32, gap: f32, pair: Pair) -> (f32, f32) {
    let slot = pair.secondary_nat + gap;
    if pair.primary_nat + slot <= span {
        return (span - slot, pair.secondary_nat);
    }
    let primary_w = (span - slot).max(pair.primary_nat.min(span * pair.primary_share));
    (primary_w, (span - primary_w - gap).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAP: f32 = 16.0;
    const SPANS: [f32; 6] = [0.0, 50.0, 200.0, 480.0, 900.0, 1200.0];
    const WIDTHS: [f32; 8] = [0.0, 10.0, 60.0, 150.0, 300.0, 500.0, 900.0, 10_000.0];

    fn resolve(span: f32, primary_nat: f32, secondary_nat: f32) -> (f32, f32) {
        two_runs(span, GAP, Pair { primary_nat, secondary_nat, primary_share: ROW_PRIMARY_SHARE })
    }

    #[test]
    fn a_pair_that_fits_leaves_the_secondary_at_its_natural_width() {
        let (primary_w, secondary_w) = resolve(400.0, 100.0, 60.0);
        assert_eq!(secondary_w, 60.0);
        assert_eq!(primary_w, 400.0 - 60.0 - GAP);
    }

    /// The primary keeps `min(natural, share * span)`, and a non-empty secondary never overruns.
    #[test]
    fn the_primary_keeps_its_share_and_the_columns_never_overrun() {
        for span in SPANS {
            for primary_nat in WIDTHS {
                for secondary_nat in WIDTHS {
                    let (primary_w, secondary_w) = resolve(span, primary_nat, secondary_nat);
                    assert!(primary_w >= primary_nat.min(span * ROW_PRIMARY_SHARE), "{span}/{primary_nat}/{secondary_nat}");
                    assert!(secondary_w == 0.0 || primary_w + GAP + secondary_w <= span + 0.01, "{span}/{primary_nat}/{secondary_nat}");
                }
            }
        }
    }

    #[test]
    fn a_long_primary_leaves_the_secondary_the_rest_of_the_span() {
        let (_, secondary_w) = resolve(640.0, 10_000.0, 500.0);
        assert_eq!(secondary_w, 640.0 - 640.0 * ROW_PRIMARY_SHARE - GAP);
    }

    /// #301's formula, worked by hand at `GAP = 16` and a `0.6` share, so a refactor cannot move it.
    fn assert_columns(got: (f32, f32), want: (f32, f32), case: &str) {
        assert!((got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3, "{case}: got {got:?}, want {want:?}");
    }

    /// Fitting branch: `primary + secondary + gap <= span` gives the secondary its natural width and
    /// the primary the rest, up to and including the exact boundary.
    #[test]
    fn the_fitting_branch_gives_the_secondary_its_natural_width_and_the_primary_the_rest() {
        assert_columns(resolve(400.0, 100.0, 60.0), (324.0, 60.0), "roomy");
        assert_columns(resolve(176.0, 100.0, 60.0), (100.0, 60.0), "exactly full");
    }

    /// Overflowing branch, primary OVER its share: it is capped at `0.6 * span` and the secondary
    /// takes the remainder: span 640 -> primary 384, secondary 640 - 384 - 16 = 240.
    #[test]
    fn an_overflowing_primary_over_its_share_is_capped_at_the_share() {
        assert_columns(resolve(640.0, 10_000.0, 500.0), (384.0, 240.0), "over share");
    }

    /// Overflowing branch, primary UNDER its share: it keeps its natural width (100 < 0.6 * 400)
    /// and the secondary gets 400 - 100 - 16 = 284, elided from its 350.
    #[test]
    fn an_overflowing_primary_under_its_share_keeps_its_natural_width() {
        assert_columns(resolve(400.0, 100.0, 350.0), (100.0, 284.0), "under share");
    }

    /// Overflowing branch where a small secondary is worth more than the share cut: the primary
    /// takes `span - secondary - gap` = 334 (> 0.6 * 400 = 240) and the secondary stays whole.
    #[test]
    fn a_small_secondary_keeps_its_natural_width_when_the_primary_overflows() {
        assert_columns(resolve(400.0, 380.0, 50.0), (334.0, 50.0), "small secondary");
    }

    /// A span too small for the gap: the primary still gets its share, the secondary floors at 0.
    #[test]
    fn a_tiny_span_floors_the_secondary_at_zero() {
        assert_columns(resolve(20.0, 100.0, 100.0), (12.0, 0.0), "tiny span");
    }
}
