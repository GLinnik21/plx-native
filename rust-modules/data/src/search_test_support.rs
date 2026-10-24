//! Shared fixtures and helpers for the `search` test modules split out below.

use super::*;

/// **No library is enumerated**, which by [`section_is_fav`]'s unknown rule makes every hit a
/// favourite — so every test below that does not care about ranking grades the round-robin
/// merge exactly as it did before favourites existed. The ranking tests build their own table.
pub(super) const NO_FAVS: &[(ServerId, i64, bool)] = &[];

/// [`merge`] with no favourite table, for the tests whose subject is the merge itself.
pub(super) fn merge_favs(sources: &[Source]) -> Vec<Shelf> {
    merge(sources, NO_FAVS)
}

pub(super) use plx_plex::plex::{Hub, MediaContainer, Metadata, Tag};

/// A standalone owned Search fixture for the split test modules below — the whole point being
/// that a second one built beside it shares nothing: no query, no shelves, no landing, no
/// notice. Fields are `pub(super)` so a sibling test module (a descendant of `search`, exactly
/// like this file) can reach into `owner.state`/`owner.adapter` the same way production's
/// `SearchStore` does, instead of re-deriving a facade method per private field.
pub(super) struct Owner {
    pub(super) state: SearchState,
    pub(super) adapter: Arc<SearchAdapter>,
    /// The fake server this fixture's runner answers every request with ([`Owner::serve`]). With
    /// none, a request waits on [`SearchState::jobs`] unanswered, as a fetch no worker has finished.
    pub(super) io: Option<Arc<Mutex<dyn tail::Io + Send>>>,
}

impl Default for Owner {
    fn default() -> Self {
        Self { state: SearchState::default(), adapter: Arc::new(SearchAdapter::default()), io: None }
    }
}

impl Owner {
    pub(super) fn set_query(&mut self, q: &str) {
        set_query(&mut self.state, &self.adapter, q);
    }
    pub(super) fn set_query_from_directory(
        &mut self,
        q: &str,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) {
        set_query_from_directory(&mut self.state, &self.adapter, q, directory);
    }
    pub(super) fn run_with_directory(
        &mut self,
        cmd: crate::stores::search::SearchCmd,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> bool {
        self.state.run_with_directory(&self.adapter, cmd, directory)
    }
    pub(super) fn reset(&mut self) {
        reset(&mut self.state, &self.adapter);
    }
    pub(super) fn rebuild(&mut self) {
        rebuild(&mut self.state);
    }
    pub(super) fn supersede(&mut self) {
        supersede(&mut self.state, &self.adapter);
    }
    pub(super) fn land(&self, i: usize, gen: u32, what: Option<Projection>) {
        land(&self.adapter, i, gen, what);
    }
    /// Answer every request from now on with `fake`, on the calling thread right after each pump, so
    /// a pump is deterministic: the fixture plays the runner. The returned handle reads and steers
    /// the fake.
    pub(super) fn serve(&mut self, fake: tail_tests::Fake) -> Arc<Mutex<tail_tests::Fake>> {
        let fake = Arc::new(Mutex::new(fake));
        self.io = Some(fake.clone());
        fake
    }
    /// Run the requests the last step emitted against the fake server ([`Owner::serve`]) and post
    /// their answers; returns how many ran. Without a fake they stay on the outbox.
    pub(super) fn run_jobs(&mut self) -> usize {
        let Some(io) = self.io.clone() else { return 0 };
        let jobs = std::mem::take(&mut self.state.jobs);
        let n = jobs.len();
        for job in jobs {
            let crate::stores::StoreJob::Search(job, port) = job else { panic!("not a search job") };
            let mut io = io.lock().unwrap_or_else(|e| e.into_inner());
            finish_with_io(&port, job.addr, &mut *io, job.req);
        }
        n
    }
    pub(super) fn pump(&mut self, dt: f32) -> bool {
        let changed = self.state.pump(&self.adapter, dt);
        self.run_jobs();
        changed
    }
    pub(super) fn pump_with_directory(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> bool {
        let changed = self.state.pump_with_directory(&self.adapter, dt, directory);
        self.run_jobs();
        changed
    }
    pub(super) fn favs(&self) -> Vec<(ServerId, i64, bool)> {
        favs(&self.state)
    }
}

/// Take the crate-wide serialization lock and empty the server registry, so each test starts
/// from a known table and leaves one behind. `route.rs`'s `fresh_registry` — the store's own
/// state now lives on the caller's own [`Owner`] rather than a process-wide global, so nothing
/// about IT needs resetting here.
///
/// It empties on the way OUT as well, `servers.rs`' own `Fresh` discipline and for a sharper
/// reason since [`slots`] became a window: a test that signs out leaves the registry's FLOOR
/// raised, and the next module to register a server without resetting first would find its own
/// slot numbering shifted under it.
pub(super) struct Fresh(#[allow(dead_code)] plx_base::testlock::Serial);

impl Drop for Fresh {
    fn drop(&mut self) {
        plx_plex::plex::reset_servers_for_test();
    }
}

pub(super) fn fresh() -> Fresh {
    let g = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    Fresh(g)
}

/// Park every source's fetch, so the pump emits no request: the tests that call it grade landings
/// they inject themselves, and a request on the outbox would be one more thing for them to account
/// for. Call it before every `pump()`.
pub(super) fn hold_off(owner: &mut Owner) {
    for i in slots() {
        source_mut(&mut owner.state, i).retry_cd = RETRY_FRAMES;
    }
    owner.state.armed = false;
}

/// A registered loopback slot. The port is never dialled — no request is run against it — so
/// this exists only to give [`slots`] a roster to fan out over. `register_for_test`, not the
/// public `register`: the latter mints and PERSISTS a device uuid.
pub(super) fn register(owner: &mut Owner, n: usize) {
    for i in 0..n {
        plx_plex::plex::register_for_test(
            &format!("search-test-{i}"),
            "127.0.0.1",
            1,
            "tok",
            "cid-search-test",
        );
    }
    // Test setup finishes before any seeded mailbox/status. Production learns this boundary
    // from its first pump; fixtures that inject a landing directly must mark the just-built
    // roster as already observed so that first pump grades the landing rather than setup.
    owner.state.visible = plx_plex::plex::server_roster_gen();
    assert_eq!(nsrc(), n);
}

pub(super) fn hub(id: &str, kind: &str) -> Hub {
    Hub {
        hub_identifier: id.to_string(),
        kind: kind.to_string(),
        ..Default::default()
    }
}

pub(super) fn meta(kind: &str, rk: &str, title: &str) -> Metadata {
    Metadata {
        kind: kind.to_string(),
        rating_key: rk.to_string(),
        title: title.to_string(),
        ..Default::default()
    }
}

pub(super) fn media(rk: &str) -> Item {
    Item::Media(PmsMovie {
        rk: rk.to_string(),
        title: rk.to_string(),
        ..Default::default()
    })
}

/// A source that answered, holding `items` on shelf `k`.
pub(super) fn answered(k: usize, items: Vec<Item>) -> Source {
    let mut s = Source {
        status: Status::Answered,
        ..Source::EMPTY
    };
    s.items[k] = items;
    s
}

/// A source whose attempt failed, with no backoff armed — [`hold_off`] is what parks spawns in
/// these tests, so a fixture must not also decide the retry timing it is being graded on.
pub(super) fn failed() -> Source {
    Source {
        status: Status::Failed,
        ..Source::EMPTY
    }
}

pub(super) fn titles(shelf: &Shelf) -> Vec<&str> {
    shelf.items.iter().map(|i| i.title()).collect()
}
