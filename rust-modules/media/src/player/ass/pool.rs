//! How many renderers the memory can hold (issue #1328). Pure: numbers in, a verdict out.
//!
//! Every renderer is a whole libass instance with its own copy of the fonts (`ass_add_font` copies
//! the bytes: the bundled faces and the attached ones the track names) and its own bitmap cache, so
//! a helper costs what the primary does. A file whose karaoke styles name five display faces puts
//! tens of megabytes in each renderer, and a television has little to spare; a helper that
//! starts where the memory is not there trades a late subtitle frame for the process. The runner
//! (`Runtime::renderers_for`) reads `MemAvailable` and the primary's font bytes and asks here once
//! per selection; nothing here reads a file or a clock.
//!
//! The number is a bound on helpers only: the primary always runs, so no subtitle is ever hidden by
//! it, a pipeline merely has fewer workers to share its frames over.

/// SCAFFOLD: the share of `MemAvailable` the renderers may take together is not decided yet.
#[allow(dead_code)]
pub(super) const MEMORY_SHARE_DIVISOR: u64 = 1;

/// What the pool may hold, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // SCAFFOLD: the runner reads these when the gate lands
pub(super) struct Verdict {
    /// How many renderers, the primary included: at least 1, at most the ceiling asked for.
    pub renderers: usize,
    /// What one renderer holds: its fonts and its bitmap cache.
    pub per_renderer: u64,
    /// What the renderers may take together; `None` when the memory available is not known.
    pub budget: Option<u64>,
}

/// SCAFFOLD: allows the ceiling whatever the memory.
#[allow(dead_code)]
pub(super) fn decide(font_bytes: u64, mem_available: Option<u64>, ceiling: usize) -> Verdict {
    let _ = (font_bytes, mem_available);
    Verdict { renderers: ceiling.max(1), per_renderer: 0, budget: None }
}

/// SCAFFOLD: reads nothing.
#[allow(dead_code)]
pub(super) fn mem_available_bytes(meminfo: &str) -> Option<u64> {
    let _ = meminfo;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn mem_available_is_read_from_the_meminfo_line_in_bytes() {
        let meminfo = "MemTotal:        1892864 kB\nMemFree:          102400 kB\nMemAvailable:     612345 kB\nBuffers: 1 kB\n";
        assert_eq!(mem_available_bytes(meminfo), Some(612_345 * 1024));
        assert_eq!(mem_available_bytes("MemTotal: 5 kB\nMemFree: 4 kB\n"), None, "an old kernel has no such line");
        assert_eq!(mem_available_bytes("MemAvailable: lots kB\n"), None);
        assert_eq!(mem_available_bytes(""), None);
    }

    #[test]
    fn a_renderer_costs_its_fonts_and_its_bitmap_cache() {
        let verdict = decide(10 * MIB, Some(4096 * MIB), 3);
        assert_eq!(verdict.per_renderer, 10 * MIB + u64::from(super::super::CACHE_BITMAP_MIB.unsigned_abs()) * MIB);
    }

    #[test]
    fn light_fonts_get_every_renderer_the_device_has_cores_for() {
        let verdict = decide(2 * MIB, Some(800 * MIB), 3);
        assert_eq!(verdict.renderers, 3, "a quarter of 800 MiB holds seven renderers of 26 MiB");
        assert_eq!(decide(2 * MIB, Some(800 * MIB), 2).renderers, 2, "and never more than the cores allow");
    }

    #[test]
    fn heavy_fonts_leave_the_pipeline_one_renderer_where_memory_is_short() {
        // the fixture: the opening's five display faces and the bundled ones, ~40 MiB in each renderer
        let verdict = decide(40 * MIB, Some(400 * MIB), 3);
        assert_eq!(verdict.renderers, 1, "a quarter of 400 MiB is 100 MiB: one renderer of 64 MiB, not two");
        assert_eq!(decide(40 * MIB, Some(800 * MIB), 3).renderers, 3, "the same fonts fit where there is memory");
        assert_eq!(decide(40 * MIB, Some(560 * MIB), 3).renderers, 2, "140 MiB holds two of 64");
    }

    #[test]
    fn the_primary_always_runs_and_an_unknown_memory_does_not_stop_a_helper() {
        assert_eq!(decide(500 * MIB, Some(64 * MIB), 3).renderers, 1, "never below the primary");
        assert_eq!(decide(500 * MIB, Some(0), 3).renderers, 1);
        let unknown = decide(40 * MIB, None, 3);
        assert_eq!((unknown.renderers, unknown.budget), (3, None), "no number, no gate: the cores bound it");
        assert_eq!(decide(2 * MIB, Some(800 * MIB), 0).renderers, 1, "a ceiling of nothing is still the primary");
    }

    #[test]
    fn the_budget_is_a_quarter_of_what_is_available() {
        assert_eq!(decide(MIB, Some(400 * MIB), 3).budget, Some(100 * MIB));
    }
}
