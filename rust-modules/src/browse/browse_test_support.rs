//! Shared fixtures and helpers for the `browse` module's test files split out below.

use super::*;

pub(super) struct TestBrowse {
    pub(super) state: BrowseState,
    pub(super) adapter: Arc<BrowseAdapter>,
}
impl Default for TestBrowse {
    fn default() -> Self {
        Self {
            state: BrowseState::default(),
            adapter: Arc::new(BrowseAdapter::default()),
        }
    }
}
impl TestBrowse {
    pub(super) fn reset(&mut self) {
        self.state.reset_owned(&self.adapter);
    }

    pub(super) fn pump(&mut self) -> crate::stores::StoreOutcome {
        let roster = self.state.sync_roster_owned();
        if roster.retire_adapter {
            self.adapter = Arc::new(BrowseAdapter::default());
        }
        let mut outcome = self.state.pump_owned(&self.adapter);
        outcome.changed |= roster.changed;
        outcome
    }

    pub(super) fn discover_pump(&mut self) -> crate::stores::StoreOutcome {
        self.state.discover_pump_owned(&self.adapter)
    }

    pub(super) fn sync_roster(&mut self) {
        if self.state.sync_roster_owned().retire_adapter {
            self.adapter = Arc::new(BrowseAdapter::default());
        }
    }

    pub(super) fn seed_sources(&mut self, sources: Vec<BrowseSource>) {
        self.reset();
        self.state.sources = sources;
    }

    pub(super) fn append_sections(&mut self, source: usize, sections: Vec<(i64, String, SecKind)>) {
        self.state.append_sections_with(source, sections, None);
    }

    pub(super) fn section_title(&self, index: usize) -> &str {
        self.state.sections.get(index).map_or("", |section| section.title.as_str())
    }

    pub(super) fn section_count(&self) -> usize {
        self.state.sections.len()
    }

    pub(super) fn pinned(&self, index: usize) -> bool {
        self.state.pinned_for_test(index)
    }

    pub(super) fn source_rows(&self) -> Vec<SrcRow> {
        let Some(kind) = self.state.section_kind(self.state.cur()) else {
            return Vec::new();
        };
        self.state.rows_where(|section| section.kind == kind && section.pinned)
    }

    pub(super) fn kind_position(&self, index: usize) -> Option<(usize, usize)> {
        let kind = self.state.section_kind(index)?;
        if !self.pinned(index) {
            return None;
        }
        let (mut position, mut count) = (0, 0);
        for (candidate, section) in self.state.sections.iter().enumerate() {
            if section.kind != kind || !section.pinned {
                continue;
            }
            if candidate == index {
                position = count;
            }
            count += 1;
        }
        (count > 1).then_some((position + 1, count))
    }

    pub(super) fn tab_count(&self) -> usize {
        self.state.tab_kinds().count()
    }

    pub(super) fn tab_title(&self, tab: usize) -> &str {
        match self.state.tab_kinds().nth(tab) {
            Some(SecKind::Movie) => "Movies",
            Some(SecKind::Show) => "TV Shows",
            None => "",
        }
    }

    pub(super) fn tab_of_section(&self, section: usize) -> Option<usize> {
        self.state.section_kind(section).and_then(|kind| self.state.tab_of_kind(kind))
    }

    pub(super) fn loading_initial(&self) -> bool {
        self.state.cur_state().is_some_and(|state| {
            state.total < 0 && matches!(state.fetch, SecFetch::Loading)
        })
    }

    pub(super) fn fetch_state(&self) -> SecFetch {
        self.state.cur_state().map_or(SecFetch::Loading, |state| state.fetch)
    }

    pub(super) fn first_run_asks(&self) -> bool {
        let session = crate::plex::session::peek();
        crate::plex::pins::asks(
            self.state.sources.len(),
            session.pins_for(&crate::plex::session::current_profile_key()),
        )
    }
}
pub(super) struct RegisteredCleanup;
impl Drop for RegisteredCleanup {
    fn drop(&mut self) {
        crate::plex::reset_servers_for_test();
    }
}
pub(super) fn registered_source(
) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client) {
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "old", "cid");
    assert!(crate::plex::set_current(sid));
    let mut source = a_source("original", "", true);
    source.sid = sid;
    source.sections_done = false;
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![source]);
    (
        RegisteredCleanup,
        browse,
        sid,
        crate::plex::client_for(sid).unwrap(),
    )
}
pub(super) fn registered_page_source(
) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client) {
    let (cleanup, mut browse, sid, client) = registered_source();
    if let Some(source) = browse.state.source_mut(0) {
        source.sections_done = true;
        source.counts_done = true;
    }
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    (cleanup, browse, sid, client)
}
pub(super) fn registered_resident_page_source(
) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client) {
    let (cleanup, mut browse, sid, client) = registered_page_source();
    let state = browse.state.state_mut(0).unwrap();
    state.fetch = SecFetch::Ready;
    state.total = 1;
    state.items = SecItems::from_vec(vec![Some(PmsMovie::default())]);
    (cleanup, browse, sid, client)
}
pub(super) fn registered_directory_source(
) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client)
{
    let (cleanup, mut browse, sid, client) = registered_page_source();
    let state = browse.state.state_mut(0).unwrap();
    state.genres = Arc::new(vec![GenreEntry {
        id: "new".into(),
        title: "New Genre".into(),
    }]);
    state.letters = Arc::new(vec![("N".into(), 7)]);
    state.genres_done = false;
    state.letters_done = false;
    (cleanup, browse, sid, client)
}
pub(super) fn queue_success_from(
    browse: &mut TestBrowse,
    client: &'static crate::plex::Client,
    token_gen: u32,
) {
    let landing = SrcLanding {
        client,
        token_gen,
        name: "stale-name".into(),
        what: SrcWhat::Sections(Some(vec![(99, "Stale Library".into(), SecKind::Movie)])),
    };
    *browse.adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((browse.state.table_epoch(), 0, landing));
    let _ = browse.state.land_discovery_owned(&browse.adapter);
}
pub(super) fn queue_page_from(
    browse: &mut TestBrowse,
    client: &'static crate::plex::Client,
    token_gen: u32,
) {
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(PageResult {
        client,
        token_gen,
        gen: browse.state.query_gen(),
        sec: 0,
        start: 0,
        items: Vec::new(),
        total: 0,
        sorts: None,
    });
    browse.adapter.fetching.store(true, Ordering::SeqCst);
    let _outcome = browse.pump();
}
pub(super) fn queue_directories_from(
    browse: &mut TestBrowse,
    client: &'static crate::plex::Client,
    token_gen: u32,
) {
    let epoch = browse.state.table_epoch();
    *browse.adapter.genre_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(DirectoryResult {
        epoch,
        sec: 0,
        client,
        token_gen,
        list: vec![GenreEntry {
            id: "stale".into(),
            title: "Stale Genre".into(),
        }],
    });
    *browse.adapter.letter_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(DirectoryResult {
        epoch,
        sec: 0,
        client,
        token_gen,
        list: vec![("S".into(), 99)],
    });
    browse.adapter.genre_fetching.store(true, Ordering::SeqCst);
    browse.adapter.letters_fetching.store(true, Ordering::SeqCst);
    browse.state.land_directory_owned(
        &browse.adapter.genre_fetching, &browse.adapter.genre_result, |st, list| {
        st.genres_done = true;
        st.genres = Arc::new(list);
    });
    browse.state.land_directory_owned(
        &browse.adapter.letters_fetching, &browse.adapter.letter_result, |st, list| {
        st.letters_done = true;
        st.letters = Arc::new(list);
    });
}
pub(super) fn assert_new_directories_survive(browse: &TestBrowse) {
    let state = browse.state.states().first().unwrap();
    assert_eq!(state.genres.len(), 1);
    assert_eq!(state.genres[0].id, "new");
    assert_eq!(state.genres[0].title, "New Genre");
    assert_eq!(*state.letters, vec![("N".into(), 7)]);
    assert!(!state.genres_done);
    assert!(!state.letters_done);
}
// ---- the three-state fetch machine ---------------------------------------------------------
//
// These drive the same owned pump core that reports to `ui::idle`'s process-global flag — the
// exact obligation `ui/xfade.rs` inherited when its `tick` started doing the same — so they
// take the CRATE-wide serial lock, not a module-local one. Their local state has one default
// row and no sections; `maybe_spawn` returns before it can reach the network, so nothing here
// spawns a worker.

/// One default state with no section table, used by store-only tests that never land a page.
pub(super) fn seed_one_section(browse: &mut TestBrowse) {
    browse.reset();
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
    browse.state.states = vec![SecState::default()];
}
/// Land what a worker would post for the CURRENT query: `total < 0` is the failure sentinel.
pub(super) fn land_page(browse: &mut TestBrowse, total: i64, items: usize) {
    let client = crate::plex::client();
    let r = PageResult {
        client,
        token_gen: client.token_gen(),
        gen: browse.state.query_gen(),
        sec: 0,
        start: 0,
        items: (0..items).map(|_| PmsMovie::default()).collect(),
        total,
        sorts: None,
    };
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    let _outcome = browse.pump();
}
// ---- the SOURCE's own state, one layer up ---------------------------------------------------
//
// Same three states, one layer up, and graded through the per-source flags rather than through
// `ensure_sections`: the fetch half needs a server, and a host test that reached for one would
// be dialling whatever address another module's test had just registered.
//
// `cur_source_state` is a PROJECTION of `reachable`/`sections_done` — there is no fourth field
// to set, which is the point of resolving it that way: the flags the Sources list already dims
// a group by are the flags the read-out reads.

/// Seed one source in a chosen phase and make it the CURRENT server, so the empty-table
/// fallback in [`cur_source_state`] resolves to it rather than to whatever the registry was
/// left holding. Registration dials nothing — it publishes a slot.
pub(super) fn seed_one_source(
    browse: &mut TestBrowse,
    reachable: bool,
    sections_done: bool,
) -> usize {
    // The CURRENT server's id, registered or not — see `seed_sources_for_test` for why nothing
    // is registered here. It makes `cur_source_idx`'s empty-table fallback resolve to this row.
    browse.seed_sources(vec![BrowseSource {
        sid: crate::plex::current_server(),
        client_addr: 0,
        token_gen: 0,
        machine_id: "mach-0".into(),
        owned: true,
        name: "nas-home".into(),
        handle: "friend".into(),
        state: if reachable {
            SourceState::Reachable
        } else {
            SourceState::Unreachable
        },
        tier: None,
        sections_done,
        counts_done: true,
        retry_cd: 0,
    }]);
    0
}
// ---- the (source, section) table ------------------------------------------------------------
//
// These seed SOURCES directly and mark every phase done, so `maybe_discover` picks nothing and
// no worker is spawned — the same discipline as the fetch-machine tests above, one layer up.
// Their `sid` is `UNSET`, which resolves to no client, so even a spawn could reach no socket.

pub(super) fn a_source(name: &str, handle: &str, reachable: bool) -> BrowseSource {
    BrowseSource {
        sid: ServerId::UNSET,
        client_addr: 0,
        token_gen: 0,
        // the machine id doubles as the fixture's identity, and OWNERSHIP follows the handle
        // here (a fixture, not the product rule — `sync_roster` takes `owned` from the roster,
        // because a share whose `sourceTitle` plex.tv did not send is still a share)
        machine_id: name.to_string(),
        owned: handle.is_empty(),
        name: name.into(),
        handle: handle.into(),
        state: if reachable {
            SourceState::Reachable
        } else {
            SourceState::Unreachable
        },
        tier: None,
        sections_done: true,
        counts_done: true,
        retry_cd: 0,
    }
}
// ---- the Home selection: defaults, persistence, and one answer per PROFILE ------------------
//
// The RULES are `plex::pins` and are graded there, pure. What is graded here is the plumbing
// around them, which is where the failures actually live: does an answer reach the disk, does
// it come back, and does it come back to the person who gave it.

/// The session-isolation guard, under the name this module's tests have always called it.
/// It is `plex::session::TempSession` now — one guard rather than the three near-identical
/// copies that had grown here, in `ui::onboard` and in `auth`; the local alias is kept only so
/// the dozens of call sites below still read as pinning THIS module's per-profile answer.
pub(super) use crate::plex::session::TempSession as TempPins;
/// One account, two servers — seeded and discovered exactly as a boot does it.
pub(super) fn seed_two_servers(browse: &mut TestBrowse) {
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
}
