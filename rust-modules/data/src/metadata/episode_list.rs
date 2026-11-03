//! The selected season's episodes as a page directory of 60-row pages.
//!
//! A season lands as page 0; a short one is then *complete* (every page loaded, each holding
//! exactly the rows its span of the listing covers) and serialises as the plain array a season has
//! always been recorded as, so recorded sessions do not change. A longer one has *holes*: a page
//! that has not landed (or was evicted) reads as `None` at its indices and `iter_loaded` skips it,
//! and `MetadataCmd::WantEpisodes` fetches the pages the screen reaches and evicts the ones it left.

use super::Episode;
/// Episodes per page, the shared page size: index `i` lives on page `i / PAGE`, row `i % PAGE`.
pub use crate::stores::page_cache::PAGE;
use crate::stores::page_cache::{Keep, PageCache};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Source of [`EpisodeList::id`]. Process-wide, so no two lists ever share an id.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The page of index `i` in a list of `pages` pages. Episodes are never filtered, so an index in
/// the last page's slack past the total still belongs to that page; only `pages * PAGE` and
/// beyond is past the end.
fn page_of(pages: usize) -> impl Fn(usize) -> usize {
    move |i| (i / PAGE).min(pages)
}

pub struct EpisodeList {
    /// The page directory. Episodes are never filtered, so every page keeps its full count and an
    /// index is plain division; a landed page is clipped to the season, not to a filter.
    cache: PageCache<Episode>,
    /// Names this list for layout caches: a new list is a new id, and an in-place edit bumps `rev`.
    id: u64,
    /// Bumped by every write to a page, so `(id, rev)` names one exact content.
    rev: u64,
}

impl EpisodeList {
    /// A list of `total` episodes with every page a hole.
    pub fn with_total(total: usize) -> Self {
        let mut cache = PageCache::default();
        cache.set_total(total);
        Self { cache, id: next_id(), rev: 0 }
    }

    /// The number of episodes in the season, loaded or not.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// The episode at index `i`, or `None` for a hole or an index past the season.
    pub fn get(&self, i: usize) -> Option<&Episode> {
        self.cache.get(i)
    }

    /// Every loaded episode with its index, in order. Holes are skipped, not counted.
    pub fn iter_loaded(&self) -> impl Iterator<Item = (usize, &Episode)> + '_ {
        self.cache.loaded_rows()
    }

    /// Index 0. Page 0 is always loaded first, so this is a hole only before the season lands.
    pub fn first(&self) -> Option<&Episode> {
        self.get(0)
    }

    /// The index of the first loaded episode with this rating key.
    pub fn position(&self, rating_key: &str) -> Option<usize> {
        self.iter_loaded().find(|(_, e)| e.rk == rating_key).map(|(i, _)| i)
    }

    /// The loaded index closest to `i`; a tie goes to the lower. `None` when nothing has landed.
    /// An index past a page's last row is not on it, so the answer is always a loaded index.
    pub fn nearest_loaded(&self, i: usize) -> Option<usize> {
        let pages = self.cache.page_count();
        let held = |p: usize| self.cache.page_rows(p).filter(|rows| !rows.is_empty());
        let home = (i / PAGE).min(pages.checked_sub(1)?);
        let below = (0..=home).rev().find_map(|p| held(p).map(|rows| (p * PAGE + rows.len() - 1).min(i)));
        let above = (home..pages).find_map(|p| held(p).filter(|rows| i < p * PAGE + rows.len()).map(|_| (p * PAGE).max(i)));
        match (below, above) {
            (Some(lo), Some(hi)) => Some(if i - lo <= hi - i { lo } else { hi }),
            (lo, hi) => lo.or(hi),
        }
    }

    /// The pages covering the index range `lo..hi` that have not landed.
    pub fn missing(&self, lo: usize, hi: usize) -> impl Iterator<Item = usize> + '_ {
        self.cache.missing_by(lo, hi, page_of(self.cache.page_count()))
    }

    /// Lands `rows` as page `page`. A page past the directory is ignored, and rows past the
    /// season's end are dropped, so a page never reads past `len()`.
    pub fn set_page(&mut self, page: usize, mut rows: Vec<Episode>) {
        rows.truncate(self.span(page));
        if self.cache.set_rows(page, rows) {
            self.rev += 1;
        }
    }

    /// Puts the pages `old` holds, and this list lacks, onto this one. For a fresh read of the SAME
    /// season (the caller has checked that): the read brings page 0 alone, and the pages the strip
    /// had reached stay in place so the episode under the focus is still there. A list of another
    /// length is another listing, and carries nothing.
    pub fn carry_pages_from(&mut self, old: &EpisodeList) {
        if old.len() != self.len() {
            return;
        }
        for page in 0..self.cache.page_count() {
            if self.cache.page_rows(page).is_none() {
                if let Some(rows) = old.cache.page_rows(page) {
                    self.set_page(page, rows.to_vec());
                }
            }
        }
    }

    /// How many pages have landed.
    pub fn loaded_pages(&self) -> usize {
        self.cache.loaded_pages()
    }

    /// Drops the rows of every loaded page the screen no longer needs (`kept_pages`: page 0, the
    /// wanted range with two pages either side, the focus and a pending restore target). Does
    /// nothing while `MAX_LOADED` or fewer pages are loaded, so a season that fits stays whole.
    /// An evicted page reads as a hole again and is fetched when the screen asks for it.
    pub fn evict(&mut self, keep: &Keep) {
        if self.cache.evict_by(keep, page_of(self.cache.page_count())) {
            self.rev += 1;
        }
    }

    /// Mutates the loaded episode with this rating key in place (a watched flip, say). Returns
    /// `false` when no loaded episode has it, and then nothing changes.
    pub fn edit(&mut self, rating_key: &str, f: impl FnOnce(&mut Episode)) -> bool {
        let Some(i) = self.position(rating_key) else {
            return false;
        };
        let edited = self.cache.edit(i, f);
        if edited {
            self.rev += 1;
        }
        edited
    }

    /// Every page has landed in full, so the list is the season in order.
    pub fn is_complete(&self) -> bool {
        (0..self.cache.page_count()).all(|p| self.cache.page_rows(p).is_some_and(|rows| rows.len() == self.span(p)))
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// How many episodes page `page` covers: a full page, or the short last page, or none past it.
    fn span(&self, page: usize) -> usize {
        self.len().saturating_sub(page * PAGE).min(PAGE)
    }
}

impl Default for EpisodeList {
    fn default() -> Self {
        Vec::new().into()
    }
}

impl Clone for EpisodeList {
    /// A clone gets a new id. Two clones edited apart would otherwise share `(id, rev)` for two
    /// different contents.
    fn clone(&self) -> Self {
        Self { cache: self.cache.clone(), id: next_id(), rev: self.rev }
    }
}

/// A whole season, every page loaded: the list every caller built before pages existed.
impl From<Vec<Episode>> for EpisodeList {
    fn from(rows: Vec<Episode>) -> Self {
        let mut cache = PageCache::default();
        cache.set_total(rows.len());
        let mut rows = rows.into_iter().peekable();
        let mut page = 0;
        while rows.peek().is_some() {
            cache.set_rows(page, rows.by_ref().take(PAGE).collect());
            page += 1;
        }
        Self { cache, id: next_id(), rev: 0 }
    }
}

impl FromIterator<Episode> for EpisodeList {
    fn from_iter<I: IntoIterator<Item = Episode>>(iter: I) -> Self {
        iter.into_iter().collect::<Vec<_>>().into()
    }
}

/// The paged form of an incomplete list. Only landed pages are written.
#[derive(Serialize)]
struct PagedRef<'a> {
    total: usize,
    pages: Vec<PageRef<'a>>,
}

#[derive(Serialize)]
struct PageRef<'a> {
    start: usize,
    rows: &'a [Episode],
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Wire {
    /// A complete list: the plain array every recorded session holds.
    Complete(Vec<Episode>),
    Paged(PagedWire),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PagedWire {
    total: usize,
    pages: Vec<PageWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageWire {
    start: usize,
    rows: Vec<Episode>,
}

impl Serialize for EpisodeList {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.is_complete() {
            return s.collect_seq(self.iter_loaded().map(|(_, e)| e));
        }
        PagedRef {
            total: self.len(),
            pages: (0..self.cache.page_count())
                .filter_map(|p| Some(PageRef { start: p * PAGE, rows: self.cache.page_rows(p)? }))
                .collect(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for EpisodeList {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Wire::deserialize(d)? {
            Wire::Complete(rows) => Ok(rows.into()),
            Wire::Paged(wire) => from_paged(wire).map_err(serde::de::Error::custom),
        }
    }
}

/// Rebuilds an incomplete list, refusing a page that is not on a page boundary, overruns the
/// season, or lands twice: such a page would put rows at indices the season does not have.
fn from_paged(wire: PagedWire) -> Result<EpisodeList, &'static str> {
    let mut list = EpisodeList::with_total(wire.total);
    for page in wire.pages {
        if page.start % PAGE != 0 {
            return Err("episode page does not start on a page boundary");
        }
        let p = page.start / PAGE;
        if page.rows.len() > list.span(p) {
            return Err("episode page runs past the season");
        }
        if p >= list.cache.page_count() {
            return Err("episode page starts past the season");
        }
        if list.cache.is_loaded(p) {
            return Err("episode page appears twice");
        }
        list.cache.set_rows(p, page.rows);
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(rk: &str) -> Episode {
        Episode { rk: rk.into(), ..Default::default() }
    }

    fn rows(lo: usize, hi: usize) -> Vec<Episode> {
        (lo..hi).map(|i| ep(&format!("e{i}"))).collect()
    }

    #[test]
    fn holes_read_as_none_and_len_is_the_total() {
        // Three pages: 60, 60 and 10. Page 1 never lands.
        let mut list = EpisodeList::with_total(130);
        list.set_page(0, rows(0, 60));
        list.set_page(2, rows(120, 130));
        assert_eq!(list.len(), 130);
        assert!(!list.is_complete());
        assert_eq!(list.get(59).map(|e| e.rk.as_str()), Some("e59"));
        assert!(list.get(60).is_none(), "a hole reads as None");
        assert!(list.get(119).is_none(), "the whole hole, not just its first row");
        assert_eq!(list.get(120).map(|e| e.rk.as_str()), Some("e120"));
        assert!(list.get(130).is_none(), "past the season is a hole too");
        assert_eq!(list.iter_loaded().count(), 70, "holes are skipped, not counted");
    }

    #[test]
    fn first_is_page_zero_row_zero_while_later_pages_are_holes() {
        let mut list = EpisodeList::with_total(130);
        assert!(list.first().is_none(), "nothing has landed yet");
        list.set_page(0, rows(0, 60));
        assert_eq!(list.first().map(|e| e.rk.as_str()), Some("e0"));
        assert!(list.get(60).is_none());
        assert!(EpisodeList::default().first().is_none());
    }

    #[test]
    fn position_finds_a_key_on_a_later_page() {
        let mut list = EpisodeList::with_total(130);
        list.set_page(1, rows(60, 120));
        assert_eq!(list.position("e75"), Some(75));
        assert_eq!(list.position("e0"), None, "page 0 has not landed");
        assert_eq!(list.position("e125"), None, "page 2 has not landed");
    }

    #[test]
    fn nearest_loaded_is_always_a_loaded_index() {
        // A short season is one partly filled page: an index past its last row is not loaded.
        let short = EpisodeList::from(rows(0, 3));
        assert_eq!(short.nearest_loaded(1), Some(1));
        assert_eq!(short.nearest_loaded(3), Some(2), "past the last row is the last row");
        assert_eq!(short.nearest_loaded(59), Some(2));
        // Pages 0 and 2 of three loaded, page 1 a hole: the closer edge wins, a tie goes lower.
        let mut holey = EpisodeList::with_total(130);
        holey.set_page(0, rows(0, 60));
        holey.set_page(2, rows(120, 130));
        assert_eq!(holey.nearest_loaded(65), Some(59));
        assert_eq!(holey.nearest_loaded(100), Some(120));
        assert_eq!(holey.nearest_loaded(89), Some(59), "30 from 59 and 31 from 120");
        assert_eq!(holey.nearest_loaded(90), Some(120), "31 from 59 and 30 from 120");
        assert_eq!(holey.nearest_loaded(500), Some(129), "past the season is the last row");
        assert_eq!(EpisodeList::with_total(130).nearest_loaded(5), None, "nothing has landed");
    }

    #[test]
    fn edit_changes_one_episode_and_bumps_rev() {
        let mut list = EpisodeList::from(rows(0, 3));
        let rev = list.rev();
        assert!(list.edit("e1", |e| e.watched = true));
        assert!(list.get(1).is_some_and(|e| e.watched));
        assert!(!list.get(0).is_some_and(|e| e.watched), "only the named episode changes");
        assert_eq!(list.rev(), rev + 1);
        assert!(!list.edit("nope", |e| e.watched = true));
        assert_eq!(list.rev(), rev + 1, "a miss writes nothing and bumps nothing");
    }

    #[test]
    fn a_range_in_the_last_pages_slack_still_asks_for_that_page() {
        // 121 episodes: pages 0..3, page 2 holds one row. A range computed against a longer list
        // can start past the total yet inside page 2's span.
        let list = EpisodeList::with_total(121);
        assert_eq!(list.missing(125, 140).collect::<Vec<_>>(), vec![2]);
        assert_eq!(list.missing(180, 200).count(), 0, "at or past pages * PAGE is past the end");
    }

    #[test]
    fn a_focus_in_the_last_pages_slack_keeps_that_page_on_eviction() {
        let mut big = EpisodeList::with_total(20_000);
        for p in 0..9 {
            big.set_page(p * 30, rows(p * 30 * PAGE, p * 30 * PAGE + PAGE));
        }
        // Nine pages loaded, over the budget; the focus sits in the slack of the last page.
        let total = 20_000usize;
        let last = total.div_ceil(PAGE) - 1;
        big.set_page(last, rows(last * PAGE, total));
        big.evict(&Keep { wanted: 0..60, focus: Some(total + 5), restore: None });
        assert!(big.get(last * PAGE).is_some(), "the focus' page is the last page and stays");
    }

    #[test]
    fn missing_lists_the_unloaded_pages_of_a_range() {
        // Pages 0..4 for 200 episodes: 60, 60, 60, 20. Page 1 has landed.
        let mut list = EpisodeList::with_total(200);
        list.set_page(1, rows(60, 120));
        assert_eq!(list.missing(0, 200).collect::<Vec<_>>(), vec![0, 2, 3]);
        assert_eq!(list.missing(70, 130).collect::<Vec<_>>(), vec![2]);
        assert_eq!(list.missing(190, 200).collect::<Vec<_>>(), vec![3]);
        assert_eq!(list.missing(5, 5).count(), 0, "an empty range needs nothing");
        assert_eq!(list.missing(250, 300).count(), 0, "past the directory needs nothing");
    }

    #[test]
    fn from_vec_is_complete_and_keeps_every_row_in_order() {
        let list = EpisodeList::from(rows(0, 130));
        assert_eq!(list.len(), 130);
        assert!(list.is_complete());
        assert_eq!(list.get(129).map(|e| e.rk.as_str()), Some("e129"));
        assert_eq!(list.iter_loaded().map(|(i, _)| i).collect::<Vec<_>>(), (0..130).collect::<Vec<_>>());
        assert_eq!(list.missing(0, 130).count(), 0);
        assert!(EpisodeList::from(Vec::new()).is_complete());
        assert!(EpisodeList::from(Vec::new()).is_empty());
    }

    #[test]
    fn from_iterator_builds_the_same_complete_list() {
        let list: EpisodeList = rows(0, 61).into_iter().collect();
        assert_eq!(list.len(), 61);
        assert!(list.is_complete());
        assert_eq!(list.get(60).map(|e| e.rk.as_str()), Some("e60"));
    }

    #[test]
    fn ids_are_unique_and_a_clone_gets_its_own() {
        let a = EpisodeList::default();
        let b = EpisodeList::default();
        assert_ne!(a.id(), b.id());
        assert_ne!(a.clone().id(), a.id());
    }

    #[test]
    fn a_complete_list_serialises_as_the_plain_array_it_always_was() {
        let plain = rows(0, 61);
        let list = EpisodeList::from(plain.clone());
        assert_eq!(serde_json::to_value(&list).unwrap(), serde_json::to_value(&plain).unwrap());
    }

    #[test]
    fn a_recorded_plain_array_loads_as_a_complete_list() {
        let recorded = serde_json::to_value(rows(0, 3)).unwrap();
        let list: EpisodeList = serde_json::from_value(recorded).unwrap();
        assert!(list.is_complete());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(2).map(|e| e.rk.as_str()), Some("e2"));
    }

    #[test]
    fn an_incomplete_list_round_trips_through_the_paged_shape() {
        let mut list = EpisodeList::with_total(130);
        list.set_page(2, rows(120, 130));
        let wire = serde_json::to_value(&list).unwrap();
        assert_eq!(wire["total"], 130);
        assert_eq!(wire["pages"].as_array().map(Vec::len), Some(1));
        assert_eq!(wire["pages"][0]["start"], 120);
        assert_eq!(wire["pages"][0]["rows"].as_array().map(Vec::len), Some(10));
        let back: EpisodeList = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), wire);
        assert_eq!(back.len(), 130);
        assert_eq!(back.get(125).map(|e| e.rk.as_str()), Some("e125"));
        assert!(back.get(0).is_none());
    }

    #[test]
    fn a_paged_shape_that_misaligns_overruns_or_repeats_a_page_is_refused() {
        let page = |start: usize, rows: Vec<Episode>| serde_json::json!({ "start": start, "rows": rows });
        let shape = |total: usize, pages: Vec<serde_json::Value>| serde_json::json!({ "total": total, "pages": pages });
        let misaligned = shape(130, vec![page(5, rows(5, 10))]);
        assert!(serde_json::from_value::<EpisodeList>(misaligned).is_err());
        let overrun = shape(10, vec![page(0, rows(0, 11))]);
        assert!(serde_json::from_value::<EpisodeList>(overrun).is_err());
        let past_end = shape(10, vec![page(60, Vec::new())]);
        assert!(serde_json::from_value::<EpisodeList>(past_end).is_err());
        let twice = shape(130, vec![page(0, rows(0, 60)), page(0, rows(0, 60))]);
        assert!(serde_json::from_value::<EpisodeList>(twice).is_err());
    }
}
