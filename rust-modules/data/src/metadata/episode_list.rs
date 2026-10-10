//! The selected season's episodes as a page directory of 60-row pages.
//!
//! A season is still read whole, so a list is normally *complete*: every page loaded, each page
//! holding exactly the rows its span of the listing covers. A complete list serialises as the plain
//! array a season has always been recorded as, so recorded sessions do not change. A page that has
//! not landed is a *hole*: its indices read as `None` and `iter_loaded` skips them, so a later
//! paged read can fill a long season in without the readers changing shape.

use super::Episode;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Episodes per page. Index `i` lives on page `i / PAGE`, row `i % PAGE`.
pub const PAGE: usize = 60;

/// Source of [`EpisodeList::id`]. Process-wide, so no two lists ever share an id.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct EpisodeList {
    total: usize,
    /// One slot per page of the listing; `None` is a page that has not landed.
    pages: Vec<Option<Vec<Episode>>>,
    /// Names this list for layout caches: a new list is a new id, and an in-place edit bumps `rev`.
    id: u64,
    /// Bumped by every write to a page, so `(id, rev)` names one exact content.
    rev: u64,
}

impl EpisodeList {
    /// A list of `total` episodes with every page a hole.
    pub fn with_total(total: usize) -> Self {
        Self { total, pages: vec![None; total.div_ceil(PAGE)], id: next_id(), rev: 0 }
    }

    /// The number of episodes in the season, loaded or not.
    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The episode at index `i`, or `None` for a hole or an index past the season.
    pub fn get(&self, i: usize) -> Option<&Episode> {
        if i >= self.total {
            return None;
        }
        self.pages.get(i / PAGE)?.as_ref()?.get(i % PAGE)
    }

    /// Every loaded episode with its index, in order. Holes are skipped, not counted.
    pub fn iter_loaded(&self) -> impl Iterator<Item = (usize, &Episode)> + '_ {
        self.pages.iter().enumerate().flat_map(|(p, page)| {
            page.iter().flatten().enumerate().map(move |(offset, e)| (p * PAGE + offset, e))
        })
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
    pub fn nearest_loaded(&self, i: usize) -> Option<usize> {
        let held = |p: usize| self.pages.get(p).and_then(|rows| rows.as_ref()).filter(|rows| !rows.is_empty());
        let home = (i / PAGE).min(self.pages.len().checked_sub(1)?);
        let below = (0..=home).rev().find_map(|p| held(p).map(|rows| (p * PAGE + rows.len() - 1).min(i)));
        let above = (home..self.pages.len()).find_map(|p| held(p).map(|_| (p * PAGE).max(i)));
        match (below, above) {
            (Some(lo), Some(hi)) => Some(if i - lo <= hi - i { lo } else { hi }),
            (lo, hi) => lo.or(hi),
        }
    }

    /// The pages covering the index range `lo..hi` that have not landed.
    pub fn missing(&self, lo: usize, hi: usize) -> impl Iterator<Item = usize> + '_ {
        let end = self.pages.len();
        let first = (lo / PAGE).min(end);
        let last = if hi > lo { hi.div_ceil(PAGE).min(end) } else { first };
        (first..last.max(first)).filter(move |&p| self.pages[p].is_none())
    }

    /// Lands `rows` as page `page`. A page past the directory is ignored, and rows past the
    /// season's end are dropped, so a page never reads past `len()`.
    pub fn set_page(&mut self, page: usize, mut rows: Vec<Episode>) {
        let span = self.span(page);
        let Some(slot) = self.pages.get_mut(page) else {
            return;
        };
        rows.truncate(span);
        *slot = Some(rows);
        self.rev += 1;
    }

    /// Mutates the loaded episode with this rating key in place (a watched flip, say). Returns
    /// `false` when no loaded episode has it, and then nothing changes.
    pub fn edit(&mut self, rating_key: &str, f: impl FnOnce(&mut Episode)) -> bool {
        let Some(i) = self.position(rating_key) else {
            return false;
        };
        let Some(episode) = self.pages.get_mut(i / PAGE).and_then(Option::as_mut).and_then(|rows| rows.get_mut(i % PAGE)) else {
            return false;
        };
        f(episode);
        self.rev += 1;
        true
    }

    /// Every page has landed in full, so the list is the season in order.
    pub fn is_complete(&self) -> bool {
        self.pages.iter().enumerate().all(|(p, rows)| rows.as_ref().is_some_and(|rows| rows.len() == self.span(p)))
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// How many episodes page `page` covers: a full page, or the short last page, or none past it.
    fn span(&self, page: usize) -> usize {
        self.total.saturating_sub(page * PAGE).min(PAGE)
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
        Self { total: self.total, pages: self.pages.clone(), id: next_id(), rev: self.rev }
    }
}

/// A whole season, every page loaded: the list every caller built before pages existed.
impl From<Vec<Episode>> for EpisodeList {
    fn from(rows: Vec<Episode>) -> Self {
        let total = rows.len();
        let mut rows = rows.into_iter().peekable();
        let mut pages = Vec::with_capacity(total.div_ceil(PAGE));
        while rows.peek().is_some() {
            pages.push(Some(rows.by_ref().take(PAGE).collect()));
        }
        Self { total, pages, id: next_id(), rev: 0 }
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
            total: self.total,
            pages: self
                .pages
                .iter()
                .enumerate()
                .filter_map(|(p, rows)| Some(PageRef { start: p * PAGE, rows: rows.as_deref()? }))
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
        let Some(slot) = list.pages.get_mut(p) else {
            return Err("episode page starts past the season");
        };
        if slot.is_some() {
            return Err("episode page appears twice");
        }
        *slot = Some(page.rows);
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
