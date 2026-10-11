//! A listing's page directory: the rows of each 60-item page, loaded or evicted, with a kept-row
//! count per page that outlives eviction.
//!
//! This generalises the `SecItems` table in `browse/mod.rs` (one slot per page, a page allocated
//! when it lands, never dropped) so a long listing holds only a few pages of rows while its indices
//! stay absolute. For an unfiltered listing index `i` is `(i / PAGE, i % PAGE)`. A listing filtered
//! client-side keeps the number of rows each visited page held; `locate` walks those counts, so an
//! index past a short page moves by the rows that page lost. An unvisited page counts as full.
//!
//! Eviction drops rows, never the directory: an evicted page reads exactly like a page that never
//! loaded, and its kept count is still the count the screen saved.

use std::ops::Range;
use std::sync::Arc;

/// Items per page, the one size the section listings (`browse`) and the episode list share. Two grid
/// screens' worth (10 rows x 6): big enough that a full-screen scroll rarely waits, small enough
/// that a page parse stays invisible on-frame.
pub const PAGE: usize = 60;

/// Pages of rows kept while more than this many are loaded. Eviction runs only above it.
pub const MAX_LOADED: usize = 8;

/// What the screen needs right now, in item indices. [`PageCache::evict`] keeps the pages covering
/// these.
#[derive(Clone, Debug)]
pub struct Keep {
    /// The items on screen, plus whatever the screen is about to read.
    pub wanted: Range<usize>,
    /// The focused item. Its page stays so a re-anchor on identity can read it.
    pub focus: Option<usize>,
    /// The target of a pending restore. Its page stays until the restore seats.
    pub restore: Option<usize>,
}

/// Which of `pages` directory pages the keep rule holds: page 0, the pages covering `keep.wanted`
/// with two either side, and the pages of the focus and restore targets. `page_of` maps an index
/// to its page and gives `pages` for an index past the listing, so a target past the end keeps
/// nothing. Shared with the unfiltered `SecItems` table in `browse`, which must not drift from it.
pub(crate) fn kept_pages(pages: usize, keep: &Keep, page_of: impl Fn(usize) -> usize) -> Vec<bool> {
    let mut kept = vec![false; pages];
    if let Some(first) = kept.first_mut() {
        *first = true;
    }
    let first = if keep.wanted.is_empty() { pages } else { page_of(keep.wanted.start) };
    if first < pages {
        let last = page_of(keep.wanted.end - 1);
        let hi = (last + 3).min(pages);
        let lo = first.saturating_sub(2).min(hi);
        kept[lo..hi].fill(true);
    }
    for i in [keep.focus, keep.restore].into_iter().flatten() {
        if let Some(slot) = kept.get_mut(page_of(i)) {
            *slot = true;
        }
    }
    kept
}

#[derive(Clone, Debug)]
struct Page<T> {
    rows: Option<Arc<Vec<T>>>,
    /// Rows the page holds once loaded. It survives eviction, so indices do not move when rows go.
    kept: u8,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self { rows: None, kept: FULL }
    }
}

const FULL: u8 = PAGE as u8;

#[derive(Clone, Debug)]
pub struct PageCache<T> {
    total: usize,
    pages: Vec<Page<T>>,
    /// Pages whose kept count is below `PAGE`. Zero means every index is plain division.
    short: usize,
}

impl<T> Default for PageCache<T> {
    fn default() -> Self {
        Self { total: 0, pages: Vec::new(), short: 0 }
    }
}

impl<T> PageCache<T> {
    /// Number of items by index, as far as the listing is known.
    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The row at index `i`, or `None` for a hole: a page never loaded, evicted, or past the end.
    pub fn get(&self, i: usize) -> Option<&T> {
        let (p, offset) = self.locate(i)?;
        self.pages.get(p)?.rows.as_ref()?.get(offset)
    }

    /// Edits the loaded row at index `i` in place, and reports whether there was one to edit. A
    /// page still shared with a reader is copied first, so the reader keeps the old row.
    pub fn edit(&mut self, i: usize, f: impl FnOnce(&mut T)) -> bool
    where
        T: Clone,
    {
        let Some((p, offset)) = self.locate(i) else { return false };
        let Some(rows) = self.pages.get_mut(p).and_then(|page| page.rows.as_mut()) else { return false };
        let Some(row) = Arc::make_mut(rows).get_mut(offset) else { return false };
        f(row);
        true
    }

    /// Every loaded row with its absolute index, in index order.
    pub fn loaded_rows(&self) -> impl Iterator<Item = (usize, &T)> + '_ {
        let mut first = 0;
        self.pages.iter().flat_map(move |page| {
            let start = first;
            first += usize::from(page.kept);
            page.rows.iter().flat_map(|rows| rows.iter()).enumerate().map(move |(k, row)| (start + k, row))
        })
    }

    pub fn is_loaded(&self, page: usize) -> bool {
        self.pages.get(page).is_some_and(|p| p.rows.is_some())
    }

    /// Page numbers covering the index range `lo..hi` that are not loaded.
    pub fn missing(&self, lo: usize, hi: usize) -> impl Iterator<Item = usize> + '_ {
        let end = self.pages.len();
        let first = self.page_of(lo).min(end);
        let last = if hi > lo { (self.page_of(hi - 1) + 1).min(end) } else { first };
        (first..last.max(first)).filter(move |&p| !self.is_loaded(p))
    }

    /// Pages in the directory: `total` items at `PAGE` each, the last one possibly short.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn loaded_pages(&self) -> usize {
        self.pages.iter().filter(|p| p.rows.is_some()).count()
    }

    pub fn set_total(&mut self, n: usize) {
        self.total = n;
        self.pages.resize_with(n.div_ceil(PAGE), Page::default);
        self.recount();
    }

    /// Lands `rows` as page `page`. Returns whether the kept count differs from the count the
    /// directory assumed (full, or the count the page last had). A page past the directory is
    /// ignored and reports no change. Rows past a page's `PAGE` are dropped: a page never holds more.
    pub fn set_page(&mut self, page: usize, mut rows: Vec<T>) -> bool {
        rows.truncate(PAGE);
        let kept = rows.len() as u8;
        let Some(slot) = self.pages.get_mut(page) else {
            return false;
        };
        let (was_short, changed) = (slot.kept < FULL, slot.kept != kept);
        slot.kept = kept;
        slot.rows = Some(Arc::new(rows));
        match (was_short, kept < FULL) {
            (false, true) => self.short += 1,
            (true, false) => self.short -= 1,
            _ => {}
        }
        changed
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The `(page, offset within page)` holding index `i`. `None` past the end of the listing.
    pub fn locate(&self, i: usize) -> Option<(usize, usize)> {
        if i >= self.total {
            return None;
        }
        if self.short == 0 {
            return Some((i / PAGE, i % PAGE));
        }
        let mut rest = i;
        for (p, page) in self.pages.iter().enumerate() {
            let kept = usize::from(page.kept);
            if rest < kept {
                return Some((p, rest));
            }
            rest -= kept;
        }
        None
    }

    /// The index of the first item of `page`.
    pub fn first_index_of(&self, page: usize) -> usize {
        if self.short == 0 {
            return page * PAGE;
        }
        let before: usize = self.pages.iter().take(page).map(|p| usize::from(p.kept)).sum();
        before + PAGE * page.saturating_sub(self.pages.len())
    }

    /// Drops the rows of every loaded page the screen no longer needs. Does nothing while
    /// `MAX_LOADED` or fewer pages are loaded.
    pub fn evict(&mut self, keep: &Keep) {
        if self.loaded_pages() <= MAX_LOADED {
            return;
        }
        let kept = kept_pages(self.pages.len(), keep, |i| self.page_of(i));
        for (page, keep) in self.pages.iter_mut().zip(kept) {
            if !keep {
                page.rows = None;
            }
        }
    }

    /// One kept count per page of the directory, for a screen to save with its position.
    pub fn kept_counts(&self) -> Vec<u8> {
        self.pages.iter().map(|p| p.kept).collect()
    }

    /// Replaces the directory with `counts` (one entry per page). No rows come back, so every page
    /// reads as a hole until it lands again. The caller sets the total.
    pub fn restore_kept_counts(&mut self, counts: &[u8]) {
        self.pages = counts.iter().map(|&kept| Page { rows: None, kept }).collect();
        self.recount();
    }

    /// The page holding index `i`, or the directory's length when `i` is past the listing.
    fn page_of(&self, i: usize) -> usize {
        self.locate(i).map_or(self.pages.len(), |(p, _)| p)
    }

    fn recount(&mut self) {
        self.short = self.pages.iter().filter(|p| p.kept < FULL).count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize, base: usize) -> Vec<usize> {
        (base..base + n).collect()
    }

    /// A listing of `total` items with every page landed, as a server that honours paging would
    /// return it.
    fn full(total: usize) -> PageCache<usize> {
        let mut c = PageCache::default();
        c.set_total(total);
        for p in 0..total.div_ceil(PAGE) {
            let n = PAGE.min(total - p * PAGE);
            c.set_page(p, rows(n, p * PAGE));
        }
        c
    }

    fn keep(wanted: Range<usize>) -> Keep {
        Keep { wanted, focus: None, restore: None }
    }

    fn loaded_set(c: &PageCache<usize>) -> Vec<usize> {
        (0..c.pages.len()).filter(|&p| c.is_loaded(p)).collect()
    }

    #[test]
    fn holes_read_as_none() {
        let mut c = PageCache::default();
        c.set_total(200);
        assert_eq!(c.len(), 200);
        assert_eq!(c.get(5), None);
        c.set_page(0, rows(60, 0));
        assert_eq!(c.get(5), Some(&5));
        assert_eq!(c.get(65), None, "page 1 was never loaded");
        assert_eq!(c.get(200), None, "past the listing");
        assert!(!c.is_loaded(1));
    }

    #[test]
    fn missing_lists_exactly_the_unloaded_pages_of_a_range() {
        let mut c = PageCache::default();
        c.set_total(20_000);
        c.set_page(0, rows(60, 0));
        c.set_page(2, rows(60, 120));
        assert_eq!(c.missing(0, 240).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(c.missing(0, 60).count(), 0);
        assert_eq!(c.missing(60, 61).collect::<Vec<_>>(), vec![1]);
        assert_eq!(c.missing(0, 0).count(), 0);
    }

    #[test]
    fn eviction_keeps_page_zero_focus_restore_and_the_wanted_range_plus_two_either_side() {
        let mut c = full(20_000);
        // Wanted is page 10, focus page 30, restore page 15.
        c.evict(&Keep { wanted: 600..660, focus: Some(1800), restore: Some(900) });
        assert_eq!(loaded_set(&c), vec![0, 8, 9, 10, 11, 12, 15, 30]);
    }

    #[test]
    fn eviction_never_leaves_more_than_eight_unless_the_kept_set_is_larger() {
        let mut c = full(20_000);
        // Wanted spans pages 10..=12, so the kept set is 8..=14, page 0 and page 30: nine pages.
        c.evict(&Keep { wanted: 600..780, focus: Some(1800), restore: None });
        assert_eq!(loaded_set(&c), vec![0, 8, 9, 10, 11, 12, 13, 14, 30]);
    }

    #[test]
    fn eviction_does_nothing_while_eight_or_fewer_pages_are_loaded() {
        let mut c = PageCache::default();
        c.set_total(20_000);
        let landed = [0, 40, 80, 120, 160, 200, 240, 280];
        for p in landed {
            c.set_page(p, rows(60, p * PAGE));
        }
        c.evict(&keep(0..60));
        assert_eq!(loaded_set(&c), landed.to_vec());

        c.set_page(320, rows(60, 320 * PAGE));
        c.evict(&keep(0..60));
        assert_eq!(loaded_set(&c), vec![0], "nine loaded: only the kept set stays");
    }

    #[test]
    fn a_20000_item_list_scrolled_end_to_end_never_holds_more_than_eight_pages() {
        let total = 20_000;
        let mut c = PageCache::default();
        c.set_total(total);
        let mut peak = 0;
        let mut start = 0;
        while start < total {
            // A 120-item window that moves 45 items a step, so it covers two or three pages.
            let window = start..(start + 120).min(total);
            for p in c.missing(window.start, window.end).collect::<Vec<_>>() {
                let n = PAGE.min(total - p * PAGE);
                c.set_page(p, rows(n, p * PAGE));
                c.evict(&Keep { wanted: window.clone(), focus: Some(window.start), restore: None });
                peak = peak.max(c.loaded_pages());
            }
            assert!(window.clone().all(|i| c.get(i) == Some(&i)), "window {window:?} reads");
            start += 45;
        }
        assert!(peak <= MAX_LOADED, "peak {peak} pages loaded");
    }

    #[test]
    fn kept_counts_survive_eviction_and_restore_round_trips() {
        let mut c = full(20_000);
        c.set_page(3, rows(57, 180));
        c.evict(&keep(600..660));
        assert!(!c.is_loaded(3));
        assert_eq!(c.kept_counts()[3], 57);

        let saved = c.kept_counts();
        assert_eq!(saved.len(), 334);
        let mut back = PageCache::<usize>::default();
        back.set_total(20_000);
        back.restore_kept_counts(&saved);
        assert_eq!(back.kept_counts(), saved);
        assert_eq!(back.loaded_pages(), 0, "no rows come back with the counts");
        assert_eq!(back.first_index_of(4), 4 * PAGE - 3);
    }

    #[test]
    fn locate_on_a_filtered_page_shifts_later_indices_by_three_and_leaves_earlier_ones_alone() {
        let mut c = PageCache::default();
        c.set_total(20_000);
        c.set_page(0, rows(60, 0));
        c.set_page(1, rows(57, 60));
        c.set_page(2, rows(60, 120));
        assert_eq!(c.locate(59), Some((0, 59)));
        assert_eq!(c.locate(60), Some((1, 0)));
        assert_eq!(c.locate(116), Some((1, 56)));
        assert_eq!(c.locate(117), Some((2, 0)), "unfiltered this was (1, 57)");
        assert_eq!(c.locate(120), Some((2, 3)));
        assert_eq!(c.first_index_of(2), 117);
        assert_eq!(c.get(117), Some(&120));

        let mut plain = PageCache::<usize>::default();
        plain.set_total(20_000);
        assert_eq!(plain.locate(12_345), Some((205, 45)));
        assert_eq!(plain.first_index_of(205), 12_300);
    }

    #[test]
    fn set_page_reports_a_changed_kept_count() {
        let mut c = PageCache::default();
        c.set_total(20_000);
        assert!(!c.set_page(0, rows(60, 0)), "full, as assumed");
        assert!(c.set_page(1, rows(57, 60)), "57 is not the 60 assumed");
        assert!(!c.set_page(1, rows(57, 60)), "same count again");
        assert!(c.set_page(1, rows(60, 60)), "back to full");
    }

    #[test]
    fn a_server_that_ignores_paging_leaves_only_the_kept_set_after_evict() {
        let mut c = PageCache::default();
        c.set_total(20_000);
        // The server answers every ask with the whole listing; the caller lands it page by page.
        for p in 0..=20 {
            c.set_page(p, rows(60, p * PAGE));
        }
        c.evict(&Keep { wanted: 600..660, focus: Some(1200), restore: None });
        assert_eq!(loaded_set(&c), vec![0, 8, 9, 10, 11, 12, 20]);
        assert_eq!(c.get(300), None, "page 5 was dropped");
    }
}
