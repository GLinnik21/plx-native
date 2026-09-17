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

pub(super) use crate::plex::{Hub, MediaContainer, Metadata, Tag};

/// A standalone owned Search fixture for the split test modules below — the whole point being
/// that a second one built beside it shares nothing: no query, no shelves, no landing, no
/// notice. Fields are `pub(super)` so a sibling test module (a descendant of `search`, exactly
/// like this file) can reach into `owner.state`/`owner.adapter` the same way production's
/// `SearchStore` does, instead of re-deriving a facade method per private field.
pub(super) struct Owner {
    pub(super) state: SearchState,
    pub(super) adapter: Arc<SearchAdapter>,
}

impl Default for Owner {
    fn default() -> Self {
        Self { state: SearchState::default(), adapter: Arc::new(SearchAdapter::default()) }
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
    pub(super) fn pump(&mut self, dt: f32) -> bool {
        self.state.pump(&self.adapter, dt)
    }
    pub(super) fn pump_with_directory(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> bool {
        self.state.pump_with_directory(&self.adapter, dt, directory)
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
pub(super) struct Fresh(#[allow(dead_code)] crate::testlock::Serial);

impl Drop for Fresh {
    fn drop(&mut self) {
        crate::plex::reset_servers_for_test();
    }
}

pub(super) fn fresh() -> Fresh {
    let g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    Fresh(g)
}

/// Park every source's fetch, so no host test spawns a worker: one would dial a `Client` whose
/// port belongs to nobody, and a stray background thread also perturbs the process-wide fd
/// count `stream.rs`'s tests assert on. Call it before every `pump()`.
pub(super) fn hold_off(owner: &mut Owner) {
    for s in &mut owner.state.src {
        s.retry_cd = RETRY_FRAMES;
    }
    owner.state.armed = false;
}

/// A registered loopback slot. The port is never dialled — `hold_off` parks every spawn — so
/// this exists only to give [`slots`] a roster to fan out over. `register_for_test`, not the
/// public `register`: the latter mints and PERSISTS a device uuid.
pub(super) fn register(owner: &mut Owner, n: usize) {
    for i in 0..n {
        crate::plex::register_for_test(
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
    owner.state.visible = crate::plex::server_roster_gen();
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
