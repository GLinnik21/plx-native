//! Retained listing read contract for the owned Library screen. No borrowed global data:
//! a frame can keep this publication across page arrivals, re-queries and account resets.
//! Source rosters and section hubs are separate publications, not implied by this view.

use std::ops::Range;

use super::{Arc, GenreEntry, SecFetch, SecItems, ServerId, SortEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ListingId {
    pub(crate) epoch: u32,
    pub(crate) query: u32,
    pub(crate) sid: ServerId,
    pub(crate) section: i64,
}

#[derive(Clone)]
pub(crate) struct ListingSnapshot {
    data: Option<ListingData>,
}

#[derive(Clone)]
struct ListingData {
    id: ListingId,
    total: i64,
    fetch: SecFetch,
    items: SecItems,
    sorts: Arc<Vec<SortEntry>>,
    genres: Arc<Vec<GenreEntry>>,
    letters: Arc<Vec<(String, i64)>>,
    sort_idx: usize,
    sort_desc: bool,
    genre: Option<Arc<GenreEntry>>,
    unwatched: bool,
    cursor: Option<Arc<super::Cursor>>,
}

impl ListingSnapshot {
    pub(crate) fn empty() -> Self {
        Self { data: None }
    }
    #[cfg(test)]
    pub(crate) fn empty_for_test() -> Self {
        Self::empty()
    }

    #[cfg(test)]
    pub(crate) fn with_cursor(mut self, cursor: super::Cursor) -> Self {
        if let Some(data) = &mut self.data {
            data.cursor = Some(Arc::new(cursor));
        }
        self
    }
    #[cfg(test)]
    pub(crate) fn with_fetch(mut self, fetch: SecFetch, total: i64) -> Self {
        if let Some(data) = &mut self.data {
            data.fetch = fetch;
            data.total = total;
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_section(mut self, epoch: u32, section: i64) -> Self {
        if let Some(data) = &mut self.data {
            data.id.epoch = epoch;
            data.id.section = section;
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_total(mut self, total: usize) -> Self {
        if let Some(data) = &mut self.data {
            data.total = total as i64;
            data.items.resize(total);
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_page(mut self, start: usize, items: Vec<crate::pms::PmsMovie>) -> Self {
        if let Some(data) = &mut self.data {
            for (offset, item) in items.into_iter().enumerate() {
                data.items.set(start + offset, item);
            }
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn absent() -> Self {
        Self { data: None }
    }

    pub(crate) fn view(&self) -> ListingView<'_> {
        ListingView(self)
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        sid: ServerId,
        items: Vec<Option<crate::pms::PmsMovie>>,
        letters: Vec<(String, i64)>,
    ) -> Self {
        Self {
            data: Some(ListingData {
                id: ListingId {
                    epoch: 1,
                    query: 1,
                    sid,
                    section: 1,
                },
                total: items.len() as i64,
                fetch: SecFetch::Ready,
                items: SecItems::from_vec(items),
                sorts: Arc::new(vec![SortEntry {
                    key: "titleSort".into(),
                    title: "Title".into(),
                    default_desc: false,
                }]),
                genres: Arc::new(Vec::new()),
                letters: Arc::new(letters),
                sort_idx: 0,
                sort_desc: false,
                genre: None,
                unwatched: false,
                cursor: None,
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ListingView<'a>(&'a ListingSnapshot);

impl<'a> ListingView<'a> {
    pub(crate) fn retain(self) -> ListingSnapshot {
        self.0.clone()
    }

    /// Immutable tier-three bookmark; a live entry's engine memory takes precedence.
    pub(crate) fn cursor(self) -> Option<&'a super::Cursor> {
        self.0.data.as_ref()?.cursor.as_deref()
    }

    /// Placement keys need rebuilding only when item membership or listing identity changes.
    pub(crate) fn same_items(self, other: ListingView<'_>) -> bool {
        match (&self.0.data, &other.0.data) {
            (Some(a), Some(b)) => {
                a.id == b.id && a.total == b.total && Arc::ptr_eq(&a.items.pages, &b.items.pages)
            }
            (None, None) => true,
            _ => false,
        }
    }
    /// Changed immutable page handles for the same listing identity and total. Comparing the
    /// table is O(total / PAGE); visiting the returned ranges is O(changed page slots).
    pub(crate) fn changed_page_ranges<'b>(
        self,
        other: ListingView<'b>,
    ) -> Option<ChangedPageRanges<'a, 'b>> {
        let (current, previous) = (self.0.data.as_ref()?, other.0.data.as_ref()?);
        (current.id == previous.id && current.total == previous.total).then_some(
            ChangedPageRanges {
                current: &current.items,
                previous: &previous.items,
                page: 0,
                total: current.total.max(0) as usize,
            },
        )
    }
    pub(crate) fn id(self) -> Option<ListingId> {
        self.0.data.as_ref().map(|s| s.id)
    }
    /// -1 means the first page has not answered; zero is a known empty listing.
    pub(crate) fn total(self) -> i64 {
        self.0.data.as_ref().map_or(-1, |s| s.total)
    }
    pub(crate) fn fetch(self) -> SecFetch {
        self.0.data.as_ref().map_or(SecFetch::Loading, |s| s.fetch)
    }
    /// Missing pages and out-of-range indices are None. The reference is bounded by the
    /// retained snapshot, never a fictitious 'static lifetime ending at the next pump.
    pub(crate) fn item(self, index: usize) -> Option<&'a crate::pms::PmsMovie> {
        self.0.data.as_ref()?.items.get(index)
    }
    pub(crate) fn sorts(self) -> &'a [SortEntry] {
        self.0.data.as_ref().map_or(&[], |s| s.sorts.as_slice())
    }
    pub(crate) fn genres(self) -> &'a [GenreEntry] {
        self.0.data.as_ref().map_or(&[], |s| s.genres.as_slice())
    }
    pub(crate) fn letters(self) -> &'a [(String, i64)] {
        self.0.data.as_ref().map_or(&[], |s| s.letters.as_slice())
    }
    pub(crate) fn sort_index(self) -> usize {
        self.0.data.as_ref().map_or(0, |s| s.sort_idx)
    }
    pub(crate) fn sort_desc(self) -> bool {
        self.0.data.as_ref().is_some_and(|s| s.sort_desc)
    }
    pub(crate) fn genre(self) -> Option<&'a GenreEntry> {
        self.0.data.as_ref()?.genre.as_deref()
    }
    pub(crate) fn unwatched(self) -> bool {
        self.0.data.as_ref().is_some_and(|s| s.unwatched)
    }
    pub(crate) fn rail_available(self) -> bool {
        self.id().is_some()
            && self
                .sorts()
                .get(self.sort_index())
                .is_none_or(|s| s.key == "titleSort" && !self.sort_desc())
            && !self.unwatched()
            && self.genre().is_none()
            && self.letters().len() > 1
    }
    pub(crate) fn letter_start(self, index: usize) -> usize {
        self.letters()
            .iter()
            .take(index)
            .map(|(_, n)| (*n).max(0) as usize)
            .fold(0usize, usize::saturating_add)
    }
}

pub(crate) struct ChangedPageRanges<'a, 'b> {
    current: &'a SecItems,
    previous: &'b SecItems,
    page: usize,
    total: usize,
}

impl Iterator for ChangedPageRanges<'_, '_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let pages = self.total.div_ceil(super::PAGE);
        while self.page < pages {
            let page = self.page;
            self.page += 1;
            let current = self.current.pages.get(page).and_then(Option::as_ref);
            let previous = self.previous.pages.get(page).and_then(Option::as_ref);
            let same = match (current, previous) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if !same {
                let start = page * super::PAGE;
                return Some(start..(start + super::PAGE).min(self.total));
            }
        }
        None
    }
}

/// Main-thread capture, once per frame. O(1), including arbitrarily large loaded listings.
impl super::BrowseState {
    pub(crate) fn listing_snapshot(&self) -> ListingSnapshot {
        let sec = self.cur();
        let id = self.sections().get(sec).and_then(|section| {
            Some(ListingId {
                epoch: self.table_epoch(),
                query: self.query_gen(),
                sid: self.section_sid(sec)?,
                section: section.key,
            })
        });
        ListingSnapshot {
            // No empty Arc allocations on Login or before discovery has produced a section.
            data: id
                .zip(self.states().get(sec))
                .map(|(id, state)| ListingData {
                    id,
                    total: state.total,
                    fetch: state.fetch,
                    items: state.items.clone(),
                    sorts: state.sorts.clone(),
                    genres: state.genres.clone(),
                    letters: state.letters.clone(),
                    sort_idx: state.sort_idx,
                    sort_desc: state.sort_desc,
                    genre: state.genre.clone(),
                    unwatched: state.unwatched,
                    cursor: state.cursor.clone(),
                }),
        }
    }
}

pub(crate) fn snapshot() -> ListingSnapshot {
    super::legacy().listing_snapshot()
}

/// The source/section table in registration order. Section indices are meaningful only
/// inside `epoch`; stable identities always include the server and its own section key.
#[derive(Clone)]
pub(crate) struct SectionView {
    pub(crate) borrowed: bool,
    pub(crate) sid: Option<ServerId>,
    pub(crate) key: i64,
    pub(crate) kind: super::SecKind,
    pub(crate) row: super::SrcRow,
}

#[derive(Default)]
struct DirectoryData {
    sources: Vec<(ServerId, super::SrcGroup)>,
    sections: Vec<SectionView>,
}

/// Owned by the frame bridge (later BrowseStore), never a new global. Source prose is rebuilt
/// only when the table, source facts or chosen section changes, not at every prepare/draw split.
#[derive(Clone)]
pub(crate) struct DirectorySnapshot {
    preferred: [Option<usize>; 2],
    kind_fetch: [SecFetch; 2],
    stamp: Option<(u32, u32, usize, usize)>,
    data: Arc<DirectoryData>,
    source: Option<usize>,
    source_fetch: SecFetch,
    discovery: SecFetch,
}

impl Default for DirectorySnapshot {
    fn default() -> Self {
        Self {
            preferred: [None; 2],
            kind_fetch: [SecFetch::Loading; 2],
            stamp: None,
            data: Arc::default(),
            source: None,
            source_fetch: SecFetch::Loading,
            discovery: SecFetch::Loading,
        }
    }
}

impl DirectorySnapshot {
    #[cfg(test)]
    pub(crate) fn fixture_source(
        epoch: u32,
        sid: ServerId,
        source: super::SrcGroup,
        fetch: SecFetch,
    ) -> Self {
        Self {
            preferred: [None; 2],
            kind_fetch: [fetch; 2],
            stamp: Some((epoch, 0, 0, 1)),
            data: Arc::new(DirectoryData {
                sources: vec![(sid, source)],
                sections: Vec::new(),
            }),
            source: Some(0),
            source_fetch: fetch,
            discovery: fetch,
        }
    }

    pub(crate) fn same_publication(&self, other: &Self) -> bool {
        self.stamp == other.stamp
            && self.source == other.source
            && self.source_fetch == other.source_fetch
            && self.discovery == other.discovery
            && self.preferred == other.preferred
            && self.kind_fetch == other.kind_fetch
    }
    #[cfg(test)]
    pub(crate) fn fixture(epoch: u32, current: usize, sections: Vec<SectionView>) -> Self {
        let preferred = [super::SecKind::Movie, super::SecKind::Show]
            .map(|kind| sections.iter().position(|s| s.kind == kind && s.row.pinned));
        Self {
            preferred,
            kind_fetch: [SecFetch::Ready; 2],
            stamp: Some((epoch, 0, current, 0)),
            data: Arc::new(DirectoryData {
                sources: Vec::new(),
                sections,
            }),
            source: None,
            source_fetch: SecFetch::Ready,
            discovery: SecFetch::Ready,
        }
    }
    /// Main-thread capture. The existing source-list generation covers names, reachability,
    /// counts and pins. Current section is separate because its tick can move without a landing;
    /// source count also catches a newly granted source before it has any sections to append.
    pub(crate) fn capture(&mut self) {
        let stamp = (
            super::table_epoch(),
            super::source_list_gen(),
            super::cur(),
            super::sources().len(),
        );
        if self.stamp != Some(stamp) {
            self.data = Arc::new(DirectoryData {
                sources: super::sources()
                    .iter()
                    .map(|s| s.sid)
                    .zip(super::source_groups())
                    .collect(),
                sections: super::sections()
                    .iter()
                    .zip(super::all_source_rows())
                    .map(|(s, row)| SectionView {
                        borrowed: super::section_sid_is_borrowed(row.section),
                        sid: super::sources().get(s.src).map(|source| source.sid),
                        key: s.key,
                        kind: s.kind,
                        row,
                    })
                    .collect(),
            });
            self.stamp = Some(stamp);
        }
        // These can change without changing the directory's prose (for example a retry).
        self.source = super::cur_source_idx();
        self.source_fetch = super::cur_source_state();
        self.discovery = super::discovery_state();
        for (i, kind) in [super::SecKind::Movie, super::SecKind::Show]
            .into_iter()
            .enumerate()
        {
            self.preferred[i] = super::tab_of_kind(kind).and_then(super::tab_section);
            self.kind_fetch[i] = super::kind_state(kind);
        }
    }

    pub(crate) fn view(&self) -> DirectoryView<'_> {
        DirectoryView(self)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DirectoryView<'a>(&'a DirectorySnapshot);

impl<'a> DirectoryView<'a> {
    pub(crate) fn preferred(self, kind: super::SecKind) -> Option<usize> {
        self.0.preferred[match kind {
            super::SecKind::Movie => 0,
            super::SecKind::Show => 1,
        }]
    }
    pub(crate) fn kind_fetch(self, kind: super::SecKind) -> SecFetch {
        self.0.kind_fetch[match kind {
            super::SecKind::Movie => 0,
            super::SecKind::Show => 1,
        }]
    }
    pub(crate) fn epoch(self) -> Option<u32> {
        self.0.stamp.map(|s| s.0)
    }
    pub(crate) fn current(self) -> Option<usize> {
        self.0
            .stamp
            .map(|s| s.2)
            .filter(|&i| i < self.0.data.sections.len())
    }
    pub(crate) fn sources(self) -> &'a [(ServerId, super::SrcGroup)] {
        &self.0.data.sources
    }
    pub(crate) fn sections(self) -> &'a [SectionView] {
        &self.0.data.sections
    }
    pub(crate) fn source(self) -> Option<&'a (ServerId, super::SrcGroup)> {
        self.sources().get(self.0.source?)
    }
    pub(crate) fn source_fetch(self) -> SecFetch {
        self.0.source_fetch
    }
    pub(crate) fn discovery(self) -> SecFetch {
        self.0.discovery
    }
    /// Press-time section, not necessarily the committed one during a page fade.
    pub(crate) fn rows_for(self, section: usize) -> impl Iterator<Item = &'a super::SrcRow> {
        let kind = self.sections().get(section).map(|s| s.kind);
        self.sections()
            .iter()
            .filter(move |s| Some(s.kind) == kind && s.row.pinned)
            .map(|s| &s.row)
    }

    /// Favourite libraries represented by the current type pill, with their table indices.
    pub(crate) fn favorite_sections_for(
        self,
        section: usize,
    ) -> impl Iterator<Item = (usize, &'a SectionView)> {
        self.rows_for(section)
            .filter_map(move |row| self.sections().get(row.section).map(|s| (row.section, s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_page_delta_names_only_the_replaced_immutable_page() {
        let sid = ServerId::from_raw(0);
        let movie = |i| crate::pms::PmsMovie {
            sid,
            rk: format!("{i}"),
            ..Default::default()
        };
        let first = ListingSnapshot::fixture(
            sid,
            (0..super::super::PAGE).map(|i| Some(movie(i))).collect(),
            Vec::new(),
        )
        .with_total(10_000);
        let second = first.clone().with_page(
            super::super::PAGE,
            (super::super::PAGE..super::super::PAGE * 2)
                .map(movie)
                .collect(),
        );
        assert_eq!(
            second
                .view()
                .changed_page_ranges(first.view())
                .unwrap()
                .collect::<Vec<_>>(),
            vec![super::super::PAGE..super::super::PAGE * 2]
        );
        assert!(second
            .view()
            .changed_page_ranges(second.view())
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn directory_retains_identity_and_refreshes_prose_only_on_change() {
        let _guard = crate::testlock::serial();
        crate::browse::seed_two_source_table_for_test();
        super::super::source_mut(0).unwrap().sid = ServerId::from_raw(0);
        super::super::source_mut(1).unwrap().sid = ServerId::from_raw(1);
        crate::browse::set_cur(0);
        let mut directory = DirectorySnapshot::default();
        directory.capture();
        let old = directory.clone();
        directory.capture();
        assert!(Arc::ptr_eq(&old.data, &directory.data));
        let view = old.view();
        assert_eq!(view.sections()[0].key, view.sections()[2].key);
        assert_ne!(view.sections()[0].sid, view.sections()[2].sid);
        assert_eq!(view.sources().len(), 2);
        assert_eq!(view.current(), Some(0));
        assert_eq!(view.source().unwrap().0, view.sections()[0].sid.unwrap());
        assert_eq!(view.source_fetch(), crate::browse::cur_source_state());
        assert_eq!(view.discovery(), crate::browse::discovery_state());
        assert_eq!(
            view.rows_for(0).cloned().collect::<Vec<_>>(),
            crate::browse::source_rows_for(0)
        );
        assert_eq!(
            view.rows_for(1).cloned().collect::<Vec<_>>(),
            crate::browse::source_rows_for(1)
        );
        assert_eq!(view.rows_for(999).count(), 0);
        super::super::source_mut(0).unwrap().name = "Changed".into();
        super::super::legacy_mut().bump_source_facts_gen();
        directory.capture();
        assert_eq!(directory.view().sources()[0].1.name, "Changed");
        assert_ne!(old.view().sources()[0].1.name, "Changed");
        crate::browse::set_cur(1);
        directory.capture();
        assert_eq!(directory.view().current(), Some(1));
        assert!(directory.view().sections()[1].row.current);
        assert!(!directory.view().sections()[0].row.current);
        crate::browse::reset();
        directory.capture();
        assert_ne!(directory.view().epoch(), old.view().epoch());
        assert!(directory.view().sections().is_empty());
        assert!(directory.view().current().is_none());
        assert_eq!(old.view().sections().len(), 4);
    }

    #[test]
    fn query_and_menus_are_one_retained_publication() {
        let _guard = crate::testlock::serial();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::set_cur(0);
        let state = super::super::state_mut(0).unwrap();
        state.sorts = Arc::new(vec![SortEntry {
            key: "titleSort".into(),
            title: "Title".into(),
            default_desc: false,
        }]);
        state.genres = Arc::new(vec![GenreEntry {
            id: "7".into(),
            title: "Drama".into(),
        }]);
        state.letters = Arc::new(vec![("A".into(), 3), ("B".into(), 4)]);
        let initial = snapshot();
        assert!(Arc::ptr_eq(
            &initial.data.as_ref().unwrap().sorts,
            &state.sorts
        ));
        assert!(Arc::ptr_eq(
            &initial.data.as_ref().unwrap().genres,
            &state.genres
        ));
        assert!(Arc::ptr_eq(
            &initial.data.as_ref().unwrap().letters,
            &state.letters
        ));
        assert_eq!(initial.view().sort_index(), 0);
        assert!(!initial.view().sort_desc());
        assert_eq!(initial.view().sorts()[0].key, "titleSort");
        assert_eq!(initial.view().genres()[0].id, "7");
        assert!(initial.view().rail_available());
        assert_eq!(initial.view().letter_start(1), 3);
        assert_eq!(initial.view().letter_start(99), 7);
        crate::browse::set_genre_by_id(Some("7"));
        crate::browse::set_unwatched(true);
        crate::browse::set_sort_by_key("titleSort", true);
        let filtered = snapshot();
        assert!(filtered.view().unwatched());
        assert!(filtered.view().sort_desc());
        assert_eq!(filtered.view().genre().unwrap().id, "7");
        assert!(!filtered.view().rail_available());
        assert!(initial.view().genre().is_none());
        assert!(!initial.view().unwatched());
        assert!(initial.view().rail_available());
        assert!(Arc::ptr_eq(
            &initial.data.as_ref().unwrap().sorts,
            &filtered.data.as_ref().unwrap().sorts
        ));
        assert_eq!(
            filtered.view().rail_available(),
            crate::browse::rail_available()
        );
        crate::browse::reset();
        assert_eq!(filtered.view().genre().unwrap().title, "Drama");
        assert!(!snapshot().view().rail_available());
    }

    #[test]
    fn retained_listing_survives_edit_requery_and_reset() {
        let _guard = crate::testlock::serial();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(2);
        let first = snapshot();
        let id = first.view().id().unwrap();
        let before = first.view().item(0).unwrap().clone();
        let retained = &first.data.as_ref().unwrap().items.pages;
        assert!(std::sync::Arc::ptr_eq(
            retained,
            &super::super::cur_state().unwrap().items.pages
        ));
        crate::browse::set_watched_local(before.sid, &before.rk, false);
        assert!(snapshot().view().item(0).unwrap().unwatched);
        assert_eq!(first.view().item(0).unwrap().unwatched, before.unwatched);
        super::super::requery();
        assert_ne!(snapshot().view().id().unwrap().query, id.query);
        assert_eq!(first.view().total(), 2);
        assert_eq!(first.view().fetch(), SecFetch::Ready);
        crate::browse::reset();
        assert!(snapshot().view().id().is_none());
        assert_eq!(snapshot().view().total(), -1);
        assert_eq!(first.view().id(), Some(id));
        assert_eq!(first.view().item(0).unwrap().rk, before.rk);
    }
}
