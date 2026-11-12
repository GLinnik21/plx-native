//! How many renderers the memory can hold (issue #1328). Pure: numbers in, a verdict out.
//!
//! Every renderer is a whole libass instance with its own copy of the bundled faces (`ass_add_font`
//! copies the bytes) and its own bitmap cache, so a helper costs what the primary does. The attached
//! fonts the track names are not in that cost: every renderer references the store's one buffer
//! (`plx_ass_add_font_borrowed`), so a file whose karaoke styles name five display faces puts those
//! tens of megabytes in the process once, not once per renderer. A television still has little to
//! spare; a helper that starts where the memory is not there trades a late subtitle frame for the
//! process. The runner (`Runtime::renderers_for`) reads `MemAvailable` and the primary's font bytes
//! and asks here once per selection; nothing here reads a file or a clock.
//!
//! The number is a bound on helpers only: the primary always runs, so no subtitle is ever hidden by
//! it, a pipeline merely has fewer workers to share its frames over. And the budget is spent on
//! helpers only: the verdict is asked once the primary is open, so what the primary holds is already
//! gone from `MemAvailable`. Charging it again counted it twice (TV, #1328: 164 MiB available, a
//! 41 MiB budget and 28 MiB a renderer answered one renderer where a helper fit).

/// The helpers may take this fraction (`1 / MEMORY_SHARE_DIVISOR`) of the memory available,
/// together: the rest is the video pipeline's, the UI's and the headroom a spike needs. A share,
/// not a number of megabytes, so a television with little free gets fewer helpers and one with
/// plenty gets them all; the event log line says what each verdict was made of.
pub(super) const MEMORY_SHARE_DIVISOR: u64 = 4;

/// What the pool may hold, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Verdict {
    /// How many renderers, the primary included: at least 1, at most the ceiling asked for.
    pub renderers: usize,
    /// What one renderer holds that another would not share: the fonts it copies and its bitmap cache.
    pub per_renderer: u64,
    /// What the helpers may take together; `None` when the memory available is not known.
    pub budget: Option<u64>,
}

/// What one renderer holds beyond its fonts: libass's bitmap cache, which grows to its cap
/// (`CACHE_BITMAP_MIB`) on a busy moving cue. The glyph outlines, FreeType's face state and the
/// parsed events are not counted: a floor on a renderer, not a ceiling.
fn cache_bytes() -> u64 {
    u64::from(super::CACHE_BITMAP_MIB.unsigned_abs()) << 20
}

/// How many renderers may run: `font_bytes` is what one copies in fonts (what the primary reports),
/// `mem_available` what the device has free with the primary already open (`None` when it could not
/// be read: nothing to gate on, the cores bound it), `ceiling` the most the cores and the pool's own
/// cap allow. The primary always runs and is not charged to the budget, so the answer is 1 plus the
/// helpers the budget holds, never below 1.
pub(super) fn decide(font_bytes: u64, mem_available: Option<u64>, ceiling: usize) -> Verdict {
    let per_renderer = font_bytes.saturating_add(cache_bytes());
    let ceiling = ceiling.max(1);
    let budget = mem_available.map(|available| available / MEMORY_SHARE_DIVISOR);
    let fits = budget.map_or(ceiling as u64, |budget| 1 + budget / per_renderer);
    let renderers = usize::try_from(fits).unwrap_or(usize::MAX).clamp(1, ceiling);
    Verdict { renderers, per_renderer, budget }
}

/// `MemAvailable` in bytes from the text of `/proc/meminfo` (the kernel's own estimate of what a
/// new allocation can take without swapping; absent before Linux 3.14).
pub(super) fn mem_available_bytes(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find_map(|line| line.strip_prefix("MemAvailable:"))?;
    let kib: u64 = line.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(1024)
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
        assert_eq!(verdict.renderers, 3, "a quarter of 800 MiB holds seven helpers of 26 MiB");
        assert_eq!(decide(2 * MIB, Some(800 * MIB), 2).renderers, 2, "and never more than the cores allow");
    }

    #[test]
    fn heavy_fonts_leave_the_pipeline_one_renderer_where_memory_is_short() {
        // the fixture: the opening's five display faces and the bundled ones, ~40 MiB in each renderer
        let verdict = decide(40 * MIB, Some(200 * MIB), 3);
        assert_eq!(verdict.renderers, 1, "a quarter of 200 MiB is 50 MiB: no helper of 64 MiB");
        assert_eq!(decide(40 * MIB, Some(800 * MIB), 3).renderers, 3, "the same fonts fit where there is memory");
        assert_eq!(decide(40 * MIB, Some(400 * MIB), 3).renderers, 2, "100 MiB holds one helper of 64");
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
    fn the_primary_already_open_is_not_charged_to_the_budget() {
        // measured on the TV (#1328): 164 MiB available with the primary open, 28 MiB a renderer
        let verdict = decide(3 * MIB, Some(157 * MIB), 3);
        assert_eq!(verdict.renderers, 2, "a 39 MiB budget holds one helper of 27 MiB beside the primary");
    }

    #[test]
    fn the_budget_is_a_quarter_of_what_is_available() {
        assert_eq!(decide(MIB, Some(400 * MIB), 3).budget, Some(100 * MIB));
    }
}
