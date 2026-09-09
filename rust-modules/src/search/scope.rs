//! Retained Search source facts.
//!
//! The Search screen searches the granted server roster, not the favourite-library projection.
//! This module snapshots the small source description that an owned screen needs while keeping
//! the registry and browse tables off the render path. The cache is main-thread state, just like
//! the other Search publications; its key is fixed-size and allocation-free.

use crate::plex::{ServerId, MAX_SERVERS};
use std::ptr::addr_of_mut;
use std::sync::Arc;

/// The retained facts for one granted Search source.
#[derive(Clone)]
pub(crate) struct ScopeSource {
    pub(crate) sid: ServerId,
    pub(crate) name: String,
    pub(crate) libraries: Vec<String>,
    pub(crate) handle: String,
    pub(crate) owned: bool,
    pub(crate) live: bool,
}

/// A Search source publication retained by a frame snapshot.
#[derive(Clone)]
pub(crate) struct SourceScopeSnapshot {
    sources: Arc<Vec<ScopeSource>>,
}

impl Default for SourceScopeSnapshot {
    fn default() -> Self {
        Self {
            sources: Arc::new(Vec::new()),
        }
    }
}

impl SourceScopeSnapshot {
    pub(crate) fn sources(&self) -> &[ScopeSource] {
        self.sources.as_slice()
    }

    pub(crate) fn same_publication(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.sources, &other.sources)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    /// The generation catches ordinary roster changes; the exact ids catch an equal-sized
    /// replacement even if a test or a future registry path reuses the generation.
    roster_gen: u32,
    roster_len: usize,
    roster: [ServerId; MAX_SERVERS],
    /// `facts_gen` covers authoritative descriptions. The pointer fingerprint also covers the
    /// server's self-description path, which merges a name without advancing that revision.
    facts_gen: u32,
    facts: [usize; MAX_SERVERS],
    /// `sections_gen` covers library title/table landings; `source_list_gen` covers reachability
    /// and other source facts maintained by Browse.
    sections_gen: u32,
    source_list_gen: u32,
    profile_gen: u32,
}

struct Cache {
    key: Key,
    publication: SourceScopeSnapshot,
}

static mut CACHE: Option<Cache> = None;

/// Read every input used by the legacy Search source projection without allocating.
fn read_key() -> Key {
    let mut roster = [ServerId::UNSET; MAX_SERVERS];
    let mut facts = [0; MAX_SERVERS];
    let mut roster_len = 0;
    for (i, sid) in crate::plex::server_ids().enumerate().take(MAX_SERVERS) {
        roster[i] = sid;
        facts[i] = crate::plex::server_facts(sid).map_or(0, |f| std::ptr::from_ref(f) as usize);
        roster_len += 1;
    }
    Key {
        roster_gen: crate::plex::server_roster_gen(),
        roster_len,
        roster,
        facts_gen: crate::plex::server_facts_gen(),
        facts,
        sections_gen: crate::browse::sections_gen(),
        source_list_gen: crate::browse::source_list_gen(),
        profile_gen: crate::plex::session::current_gen(),
    }
}

/// Capture the current source facts, rebuilding only when a cheap semantic input moves.
pub(crate) fn snapshot() -> SourceScopeSnapshot {
    let key = read_key();
    // SAFETY: called by the main-thread Search publication boundary. The retained Arc keeps old
    // source facts alive after this cache replaces its current publication.
    let cache = unsafe { &mut *addr_of_mut!(CACHE) };
    if cache.as_ref().map(|c| c.key != key).unwrap_or(true) {
        *cache = Some(Cache {
            key,
            publication: build(),
        });
    }
    cache
        .as_ref()
        .expect("source scope cache was just built")
        .publication
        .clone()
}

fn build() -> SourceScopeSnapshot {
    let first = crate::plex::server_ids().next();
    let browse_sources = crate::browse::sources();
    let sources = crate::plex::server_ids()
        .map(|sid| {
            let facts = crate::plex::server_facts(sid);
            ScopeSource {
                sid,
                name: facts.map(|f| f.name.clone()).unwrap_or_default(),
                libraries: crate::browse::library_titles(sid)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                handle: facts.map(|f| f.handle.clone()).unwrap_or_default(),
                // Registration order is the only honest ownership answer before the roster has
                // described a slot; the session server is registered first.
                owned: facts.map(|f| f.owned).unwrap_or(Some(sid) == first),
                // A browse source that has not been adopted has not failed yet.
                live: browse_sources
                    .iter()
                    .find(|source| source.sid == sid)
                    .map(|source| source.reachable())
                    .unwrap_or(true),
            }
        })
        .collect();
    SourceScopeSnapshot {
        sources: Arc::new(sources),
    }
}

#[cfg(test)]
fn reset_for_test() {
    unsafe {
        *addr_of_mut!(CACHE) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            reset_for_test();
            crate::browse::reset();
            crate::plex::reset_servers_for_test();
            crate::plex::session::set_current(None);
        }
    }

    fn fixture() -> (ServerId, ServerId) {
        crate::plex::reset_servers_for_test();
        let own = crate::plex::register_for_test("own-machine", "127.0.0.1", 1, "own", "scope");
        let share =
            crate::plex::register_for_test("share-machine", "127.0.0.1", 2, "share", "scope");
        crate::browse::seed_registered_table_for_test([own, share]);
        (own, share)
    }

    #[test]
    fn retained_publication_survives_facts_and_browse_changes() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        let (own, share) = fixture();
        let old = snapshot();
        let same = snapshot();
        assert!(old.same_publication(&same));
        assert!(Arc::ptr_eq(&old.sources, &same.sources));
        assert_eq!(old.sources()[0].sid, own);
        assert_eq!(old.sources()[0].name, "mac-mini");
        assert_eq!(old.sources()[1].libraries, ["Film Club", "Film Club"]);
        assert!(old.sources()[0].owned);
        assert!(!old.sources()[1].owned);
        assert!(old.sources()[0].live && old.sources()[1].live);

        crate::plex::describe_server(share, "renamed-share", "new-friend", false);
        crate::browse::append_section_for_test(1, 9, "Archive", crate::browse::SecKind::Movie);
        let changed = snapshot();
        assert!(!old.same_publication(&changed));
        assert_eq!(old.sources()[1].name, "nas-home");
        assert_eq!(old.sources()[1].handle, "friend");
        assert_eq!(old.sources()[1].libraries, ["Film Club", "Film Club"]);
        assert_eq!(changed.sources()[1].name, "renamed-share");
        assert_eq!(changed.sources()[1].handle, "new-friend");
        assert_eq!(
            changed.sources()[1].libraries,
            ["Film Club", "Film Club", "Archive"]
        );
        assert!(!Arc::ptr_eq(&old.sources, &changed.sources));

        crate::browse::seed_sources_for_test(2, false);
        let unreachable = snapshot();
        assert!(old.sources()[0].live && old.sources()[1].live);
        assert!(!unreachable.sources()[0].live && !unreachable.sources()[1].live);
    }

    #[test]
    fn source_addition_publishes_a_new_roster_projection() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        crate::plex::reset_servers_for_test();
        let own = crate::plex::register_for_test("own-machine", "127.0.0.1", 1, "own", "scope");
        crate::browse::seed_sources_for_test(1, true);
        let old = snapshot();

        let share =
            crate::plex::register_for_test("share-machine", "127.0.0.1", 2, "share", "scope");
        let next = snapshot();

        assert_eq!(old.sources().len(), 1);
        assert_eq!(old.sources()[0].sid, own);
        assert_eq!(next.sources().len(), 2);
        assert_eq!(next.sources()[1].sid, share);
        assert!(
            next.sources()[1].live,
            "an unadopted source is optimistically live"
        );
        assert!(!old.same_publication(&next));
    }

    #[test]
    fn equal_sized_roster_replacement_publishes_new_sources() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        let (_, old_share) = fixture();
        let old = snapshot();

        crate::plex::reset_servers_for_test();
        let replacement =
            crate::plex::register_for_test("replacement", "127.0.0.1", 3, "replacement", "scope");
        let other = crate::plex::register_for_test("other", "127.0.0.1", 4, "other", "scope");
        assert_ne!(old_share, replacement);
        crate::browse::reset();
        let next = snapshot();

        assert_eq!(old.sources().len(), 2);
        assert_eq!(next.sources().len(), 2);
        assert_eq!(next.sources()[0].sid, replacement);
        assert_eq!(next.sources()[1].sid, other);
        assert!(!old.same_publication(&next));
    }
}
