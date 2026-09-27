//! Retained Search publication. Capturing a frame clones handles, not queries or result items.
use super::{Arc, Shelf, State};

#[derive(Clone)]
pub(crate) struct SearchSnapshot {
    query: Option<Arc<str>>,
    shelves: Option<Arc<Vec<Shelf>>>,
    state: State,
    query_gen: u32,
    recents: super::recents::RecentsSnapshot,
    scope: super::scope::SourceScopeSnapshot,
}

impl Default for SearchSnapshot {
    fn default() -> Self {
        Self {
            query: None,
            shelves: None,
            state: State::Idle,
            query_gen: 0,
            recents: Default::default(),
            scope: Default::default(),
        }
    }
}

impl SearchSnapshot {
    /// Built by `SearchState::snapshot`/`snapshot_with_directory` — the only callers, since the
    /// fields here are otherwise private to this module.
    pub(crate) fn from_parts(
        query: Option<Arc<str>>,
        shelves: Option<Arc<Vec<Shelf>>>,
        state: State,
        query_gen: u32,
        recents: super::recents::RecentsSnapshot,
        scope: super::scope::SourceScopeSnapshot,
    ) -> Self {
        Self { query, shelves, state, query_gen, recents, scope }
    }

    pub(crate) fn view(&self) -> SearchView<'_> { SearchView(self) }

    /// Publication identity is a process-local change detector, never a serialized pointer.
    /// Raw text can change without changing the trimmed-query epoch; result edits can also
    /// change a publication inside one query, so the epoch alone is insufficient.
    pub(crate) fn same_publication(&self, other: &Self) -> bool {
        fn same<T: ?Sized>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        }
        self.state == other.state && self.query_gen == other.query_gen
            && same(&self.query, &other.query) && same(&self.shelves, &other.shelves)
            && self.recents.same_publication(&other.recents)
            && self.scope.same_publication(&other.scope)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SearchView<'a>(&'a SearchSnapshot);

impl<'a> SearchView<'a> {
    pub(crate) fn snapshot(self) -> SearchSnapshot { self.0.clone() }
    pub(crate) fn same_publication(self, other: &SearchSnapshot) -> bool { self.0.same_publication(other) }
    pub(crate) fn query(self) -> &'a str { self.0.query.as_deref().unwrap_or("") }
    pub(crate) fn shelves(self) -> &'a [Shelf] {
        self.0.shelves.as_deref().map(Vec::as_slice).unwrap_or(&[])
    }
    pub(crate) fn state(self) -> State { self.0.state }
    pub(crate) fn query_gen(self) -> u32 { self.0.query_gen }
    pub(crate) fn recents(self) -> &'a super::recents::RecentsSnapshot { &self.0.recents }
    pub(crate) fn scope(self) -> &'a super::scope::SourceScopeSnapshot { &self.0.scope }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Item, Kind, SearchAdapter, SearchState};

    /// A standalone owned Search fixture — the whole point being that a second one built beside it
    /// shares nothing: no query, no shelves, no landing, no notice.
    struct Fixture {
        state: SearchState,
        adapter: Arc<SearchAdapter>,
    }

    impl Fixture {
        fn new() -> Self {
            Self { state: SearchState::default(), adapter: Arc::new(SearchAdapter::default()) }
        }
        fn set_query(&mut self, q: &str) {
            crate::search::set_query_for_test(&mut self.state, &self.adapter, q);
        }
        fn snapshot(&self) -> SearchSnapshot {
            self.state.snapshot()
        }
    }

    fn publish_fixture() -> (Fixture, SearchSnapshot) {
        let mut fx = Fixture::new();
        fx.set_query("wallace");
        fx.state.publish_shelves_for_test(vec![Shelf {
            kind: Kind::Movie,
            items: vec![Item::Media(crate::pms::PmsMovie {
                sid: crate::plex::ServerId::UNSET, rk: "retained-search".into(),
                title: "Synthetic result".into(), unwatched: true, ..Default::default()
            })],
        }]);
        let snap = fx.snapshot();
        (fx, snap)
    }

    #[test]
    fn a_query_slice_is_consumed_before_replacing_its_publication() {
        let _guard = crate::testlock::serial();
        let mut fx = Fixture::new();
        fx.set_query("wallace");
        let prefix = fx.state.query()[..2].to_string();
        fx.set_query(&prefix);
        assert_eq!(fx.snapshot().view().query(), "wa");
        assert_eq!(fx.snapshot().view().state(), State::Searching);
    }

    #[test]
    fn captures_share_published_buffers_and_whitespace_preserves_the_result_identity() {
        let _guard = crate::testlock::serial();
        let (mut fx, old) = publish_fixture();
        let another = fx.snapshot();
        assert!(old.same_publication(&another));
        assert!(Arc::ptr_eq(old.query.as_ref().unwrap(), another.query.as_ref().unwrap()));
        assert!(Arc::ptr_eq(old.shelves.as_ref().unwrap(), another.shelves.as_ref().unwrap()));
        assert!(old.view().scope().same_publication(another.view().scope()));
        fx.set_query("wallace  ");
        let spaced = fx.snapshot();
        assert!(!old.same_publication(&spaced), "raw field edits are a publication change");
        assert_eq!(old.view().query(), "wallace");
        assert_eq!(spaced.view().query(), "wallace  ");
        assert_eq!(old.view().query_gen(), spaced.view().query_gen());
        assert_eq!(spaced.view().state(), State::Ready);
        assert!(Arc::ptr_eq(old.shelves.as_ref().unwrap(), spaced.shelves.as_ref().unwrap()));
        assert!(!crate::search::set_watched_local_for_test(&mut fx.state, crate::plex::ServerId::UNSET, "absent", true));
        assert!(Arc::ptr_eq(spaced.shelves.as_ref().unwrap(), fx.snapshot().shelves.as_ref().unwrap()));
    }

    #[test]
    fn query_replacement_and_profile_reset_cannot_rewrite_a_retained_view() {
        let _guard = crate::testlock::serial();
        let (mut fx, old) = publish_fixture();
        fx.set_query("gromit");
        let next = fx.snapshot();
        assert_ne!(next.view().query_gen(), old.view().query_gen());
        assert_eq!(next.view().state(), State::Searching);
        assert!(next.view().shelves().is_empty());
        crate::search::reset_for_test(&mut fx.state, &fx.adapter);
        let reset = fx.snapshot();
        assert_ne!(reset.view().query_gen(), next.view().query_gen());
        assert_eq!(reset.view().query(), "");
        assert_eq!(reset.view().state(), State::Idle);
        assert!(reset.view().shelves().is_empty());
        assert_eq!(old.view().query(), "wallace");
        assert_eq!(old.view().state(), State::Ready);
        assert_eq!(old.view().shelves()[0].kind, Kind::Movie);
        assert_eq!(old.view().shelves()[0].items.len(), 1);
        assert_eq!(next.view().query(), "gromit");
    }

    #[test]
    fn optimistic_edits_copy_on_write_and_preserve_the_old_item() {
        let _guard = crate::testlock::serial();
        let (mut fx, old) = publish_fixture();
        let watched = |snapshot: &SearchSnapshot| match &snapshot.view().shelves()[0].items[0] {
            Item::Media(item) => item.watched,
            _ => panic!("fixture must be a media item"),
        };
        assert!(!watched(&old));
        assert!(!crate::search::set_watched_local_for_test(&mut fx.state, crate::plex::ServerId::from_raw(9), "retained-search", true));
        assert!(Arc::ptr_eq(old.shelves.as_ref().unwrap(), fx.snapshot().shelves.as_ref().unwrap()));
        assert!(crate::search::set_watched_local_for_test(&mut fx.state, crate::plex::ServerId::UNSET, "retained-search", true));
        let changed = fx.snapshot();
        assert!(!old.same_publication(&changed), "optimistic edits do not change the query epoch");
        assert!(!watched(&old));
        assert!(watched(&changed));
        assert!(!Arc::ptr_eq(old.shelves.as_ref().unwrap(), changed.shelves.as_ref().unwrap()));
        assert!(Arc::ptr_eq(changed.shelves.as_ref().unwrap(), fx.snapshot().shelves.as_ref().unwrap()));
    }

    #[test]
    fn rebuilding_a_source_answer_replaces_only_the_new_publication() {
        let _guard = crate::testlock::serial();
        struct RegistryReset;
        impl Drop for RegistryReset { fn drop(&mut self) { crate::plex::reset_servers_for_test(); } }
        let _registry = RegistryReset;
        crate::plex::reset_servers_for_test();
        let (mut fx, old) = publish_fixture();
        let sid = crate::plex::register_for_test("search-view", "127.0.0.1", 9, "synthetic", "fixture");
        let mut answer = [const { Vec::new() }; super::super::NKIND];
        answer[0].push(Item::Media(crate::pms::PmsMovie {
            sid, rk: "new-answer".into(), title: "Replacement result".into(), ..Default::default()
        }));
        crate::search::record_for_test(&mut fx.state, sid.raw() as usize, Some(answer));
        crate::search::rebuild_for_test(&mut fx.state);
        let next = fx.snapshot();
        assert_eq!(old.view().shelves()[0].items[0].title(), "Synthetic result");
        assert_eq!(next.view().shelves()[0].items[0].title(), "Replacement result");
        assert_eq!(next.view().shelves()[0].items[0].sid(), sid);
        assert!(Arc::ptr_eq(old.query.as_ref().unwrap(), next.query.as_ref().unwrap()));
        assert!(!Arc::ptr_eq(old.shelves.as_ref().unwrap(), next.shelves.as_ref().unwrap()));
    }
}
