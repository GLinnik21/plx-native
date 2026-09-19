//! Retained Search source facts.
//!
//! The Search screen searches the granted server roster, not the favourite-library projection.
//! This module snapshots the small source description that an owned screen needs while keeping
//! the registry and browse tables off the render path. The cache is main-thread state, just like
//! the other Search publications. Registry counters are the cheap first gate; the cached exact
//! Browse projection distinguishes independent owners whose local generations happen to match.

use crate::plex::{ServerId, MAX_SERVERS};
use std::cell::RefCell;
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
    directory: Option<DirectoryInput>,
    publication: SourceScopeSnapshot,
}

/// Exact Browse semantics consumed by [`build_with_directory`]. Owner-local counters are only an
/// inexpensive first gate: two independent Browse stores legitimately begin at the same values.
/// Keeping the small source/library projection here prevents one owner's retained publication
/// from being returned for another without introducing a process-global owner identity.
#[derive(PartialEq, Eq)]
struct DirectoryInput {
    sources: Vec<(ServerId, bool)>,
    libraries: Vec<(ServerId, String)>,
}

impl DirectoryInput {
    fn capture(directory: crate::stores::browse::DirectoryView<'_>) -> Self {
        Self {
            sources: directory.sources().iter()
                .map(|(sid, source)| (*sid, source.reachable()))
                .collect(),
            libraries: directory.sections().iter().filter_map(|section| {
                Some((section.sid?, section.row.title.clone()))
            }).collect(),
        }
    }

    fn matches(&self, directory: crate::stores::browse::DirectoryView<'_>) -> bool {
        self.sources.len() == directory.sources().len()
            && self.sources.iter().zip(directory.sources()).all(
                |((cached_sid, cached_live), (sid, source))| {
                    cached_sid == sid && *cached_live == source.reachable()
                })
            && self.libraries.len() == directory.sections().iter()
                .filter(|section| section.sid.is_some()).count()
            && self.libraries.iter().zip(
                directory.sections().iter().filter_map(|section| {
                    Some((section.sid?, section.row.title.as_str()))
                })
            ).all(|((cached_sid, cached_title), (sid, title))| {
                *cached_sid == sid && cached_title == title
            })
    }
}

/// Per-owner memo of the last built [`SourceScopeSnapshot`]. Lives on `SearchState` so each
/// `Bridge`'s Search owner memoizes its own scope; two owners in one process never share a cache
/// entry. The production callers only ever reach this through `&self`/`&mut self` on the owning
/// `SearchState`, so the cell is interior-mutable rather than a plain field.
#[derive(Default)]
pub(crate) struct ScopeCache {
    cache: RefCell<Option<Cache>>,
}

/// Read every registry input used by a standalone Search fixture. Production adds the retained
/// Browse directory generations through [`read_key_with_directory`].
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
        sections_gen: 0,
        source_list_gen: 0,
        profile_gen: crate::plex::session::current_gen(),
    }
}

fn read_key_with_directory(directory: crate::stores::browse::DirectoryView<'_>) -> Key {
    let mut key = read_registry_key();
    key.sections_gen = directory.sections_gen();
    key.source_list_gen = directory.source_list_gen();
    key
}

fn read_registry_key() -> Key {
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
        sections_gen: 0,
        source_list_gen: 0,
        profile_gen: crate::plex::session::current_gen(),
    }
}

impl ScopeCache {
    /// Capture the current source facts, rebuilding only when a cheap semantic input moves.
    pub(crate) fn snapshot(&self) -> SourceScopeSnapshot {
        let key = read_key();
        let mut cache = self.cache.borrow_mut();
        let matches = cache.as_ref().is_some_and(|cached| {
            cached.key == key && cached.directory.is_none()
        });
        if !matches {
            *cache = Some(Cache {
                key,
                directory: None,
                publication: build(),
            });
        }
        cache
            .as_ref()
            .expect("source scope cache was just built")
            .publication
            .clone()
    }

    pub(crate) fn snapshot_with_directory(
        &self,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> SourceScopeSnapshot {
        let key = read_key_with_directory(directory);
        let mut cache = self.cache.borrow_mut();
        let matches = cache.as_ref().is_some_and(|cached| {
            cached.key == key
                && cached.directory.as_ref().is_some_and(|input| input.matches(directory))
        });
        if !matches {
            *cache = Some(Cache {
                key,
                directory: Some(DirectoryInput::capture(directory)),
                publication: build_with_directory(directory),
            });
        }
        cache.as_ref().expect("source scope cache was just built").publication.clone()
    }
}

fn build() -> SourceScopeSnapshot {
    let first = crate::plex::server_ids().next();
    let sources = crate::plex::server_ids()
        .map(|sid| {
            let facts = crate::plex::server_facts(sid);
            ScopeSource {
                sid,
                name: facts.map(|f| f.name.clone()).unwrap_or_default(),
                libraries: Vec::new(),
                handle: facts.map(|f| f.handle.clone()).unwrap_or_default(),
                // Registration order is the only honest ownership answer before the roster has
                // described a slot; the session server is registered first.
                owned: facts.map(|f| f.owned).unwrap_or(Some(sid) == first),
                // A browse source that has not been adopted has not failed yet.
                live: true,
            }
        })
        .collect();
    SourceScopeSnapshot {
        sources: Arc::new(sources),
    }
}

fn build_with_directory(
    directory: crate::stores::browse::DirectoryView<'_>,
) -> SourceScopeSnapshot {
    let first = crate::plex::server_ids().next();
    let sources = crate::plex::server_ids().map(|sid| {
        let facts = crate::plex::server_facts(sid);
        ScopeSource {
            sid,
            name: facts.map(|f| f.name.clone()).unwrap_or_default(),
            libraries: directory.library_titles(sid).map(str::to_owned).collect(),
            handle: facts.map(|f| f.handle.clone()).unwrap_or_default(),
            owned: facts.map(|f| f.owned).unwrap_or(Some(sid) == first),
            live: directory.sources().iter().find(|source| source.0 == sid)
                .map(|source| source.1.reachable()).unwrap_or(true),
        }
    }).collect();
    SourceScopeSnapshot { sources: Arc::new(sources) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_scope_uses_the_supplied_directory_instead_of_browse_globals() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "retained-machine", "127.0.0.1", 9, "retained", "scope");
        let directory = crate::stores::browse::DirectorySnapshot::fixture(3, 0, vec![
            crate::stores::browse::SectionView {
                borrowed: false,
                sid: Some(sid),
                key: 12,
                kind: crate::stores::browse::SecKind::Movie,
                row: crate::stores::browse::SrcRow {
                    section: 0,
                    title: "Retained Library".into(),
                    pinned: true,
                    current: true,
                    ..Default::default()
                },
            },
        ]);

        let cache = ScopeCache::default();
        let scope = cache.snapshot_with_directory(directory.view());

        assert_eq!(scope.sources()[0].libraries, ["Retained Library"]);
    }

    #[test]
    fn equal_generation_browse_owners_publish_their_own_search_scope() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "equal-generation-scope", "127.0.0.1", 9, "synthetic", "scope");
        let directory = |title: &str| crate::stores::browse::DirectorySnapshot::fixture(
            7,
            0,
            vec![crate::stores::browse::SectionView {
                borrowed: false,
                sid: Some(sid),
                key: 12,
                kind: crate::stores::browse::SecKind::Movie,
                row: crate::stores::browse::SrcRow {
                    section: 0,
                    title: title.into(),
                    pinned: true,
                    current: true,
                    ..Default::default()
                },
            }],
        );
        let alpha = directory("Alpha");
        let beta = directory("Beta");
        assert!(read_key_with_directory(alpha.view()) == read_key_with_directory(beta.view()),
            "the regression requires equal owner-local generations");

        let cache = ScopeCache::default();
        let first = cache.snapshot_with_directory(alpha.view());
        let second = cache.snapshot_with_directory(beta.view());

        assert_eq!(first.sources()[0].libraries, ["Alpha"]);
        assert_eq!(second.sources()[0].libraries, ["Beta"],
            "an independent owner cannot reuse another owner's publication");
        assert!(!first.same_publication(&second));
    }

    /// The two-owner proof for the memo's move off `static mut CACHE`. Two independent
    /// [`ScopeCache`]s (standing in for two `Bridge`s' `SearchState`s) build under the SAME
    /// registry-generation `Key` but DIFFERENT directory content — equal enough that only owner
    /// identity, not key or content, could ever distinguish them if the memo were shared. A
    /// content-identical scenario would pass trivially even on a single process-wide cache (the
    /// `DirectoryInput` comparison would keep matching), which is exactly the worthless shape the
    /// migration warns about, so owner B's content must differ from owner A's here. With a shared
    /// cache, owner B's differing-content call would evict owner A's cached entry, so owner A's
    /// following call — same key, same content as its first — would needlessly rebuild and lose
    /// `same_publication`'s Arc identity. Two per-owner caches must not do that.
    #[test]
    fn two_owners_do_not_share_the_scope_memo() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "shared-key-scope", "127.0.0.1", 9, "shared", "scope");
        let directory = |title: &str| crate::stores::browse::DirectorySnapshot::fixture(4, 0, vec![
            crate::stores::browse::SectionView {
                borrowed: false,
                sid: Some(sid),
                key: 12,
                kind: crate::stores::browse::SecKind::Movie,
                row: crate::stores::browse::SrcRow {
                    section: 0,
                    title: title.into(),
                    pinned: true,
                    current: true,
                    ..Default::default()
                },
            },
        ]);
        let dir_a = directory("Owner A's Library");
        let dir_b = directory("Owner B's Library");
        assert!(read_key_with_directory(dir_a.view()) == read_key_with_directory(dir_b.view()),
            "the regression requires equal owner-local generations");

        let owner_a = ScopeCache::default();
        let owner_b = ScopeCache::default();

        let a1 = owner_a.snapshot_with_directory(dir_a.view());
        // Owner B observes the same key but different content, interleaved between two of
        // owner A's calls.
        let b1 = owner_b.snapshot_with_directory(dir_b.view());
        let a2 = owner_a.snapshot_with_directory(dir_a.view());

        assert_eq!(b1.sources()[0].libraries, ["Owner B's Library"]);
        assert!(
            a1.same_publication(&a2),
            "owner A's own repeated call must hit its own memo, undisturbed by owner B's call \
             in between — a shared cache would have owner B's call evict owner A's entry"
        );
        assert!(Arc::ptr_eq(&a1.sources, &a2.sources));
    }

    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
            crate::plex::session::publish_profile_for_test(None,
                crate::plex::session::current_gen().wrapping_add(1));
        }
    }

    fn fixture() -> (
        crate::stores::Stores,
        crate::stores::browse::DirectorySnapshot,
        ServerId,
        ServerId,
    ) {
        crate::plex::reset_servers_for_test();
        let own = crate::plex::register_for_test("own-machine", "127.0.0.1", 1, "own", "scope");
        let share =
            crate::plex::register_for_test("share-machine", "127.0.0.1", 2, "share", "scope");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_registered_table_for_test([own, share]);
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        stores.capture_browse(&mut directory);
        (stores, directory, own, share)
    }

    #[test]
    fn retained_publication_survives_facts_and_browse_changes() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        let (stores, mut directory, own, share) = fixture();
        let cache = ScopeCache::default();
        let old = cache.snapshot_with_directory(directory.view());
        let same = cache.snapshot_with_directory(directory.view());
        assert!(old.same_publication(&same));
        assert!(Arc::ptr_eq(&old.sources, &same.sources));
        assert_eq!(old.sources()[0].sid, own);
        assert_eq!(old.sources()[0].name, "mac-mini");
        assert_eq!(old.sources()[1].libraries, ["Film Club", "Film Club"]);
        assert!(old.sources()[0].owned);
        assert!(!old.sources()[1].owned);
        assert!(old.sources()[0].live && old.sources()[1].live);

        crate::plex::describe_server(share, "renamed-share", "new-friend", false);
        stores.browse.borrow_mut().append_section_for_test(
            1, 9, "Archive", crate::browse::SecKind::Movie);
        stores.capture_browse(&mut directory);
        let changed = cache.snapshot_with_directory(directory.view());
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

        stores.browse.borrow_mut().seed_sources_for_test(2, false);
        stores.capture_browse(&mut directory);
        let unreachable = cache.snapshot_with_directory(directory.view());
        assert!(old.sources()[0].live && old.sources()[1].live);
        assert!(!unreachable.sources()[0].live && !unreachable.sources()[1].live);
    }

    #[test]
    fn source_addition_publishes_a_new_roster_projection() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        crate::plex::reset_servers_for_test();
        let own = crate::plex::register_for_test("own-machine", "127.0.0.1", 1, "own", "scope");
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_sources_for_test(1, true);
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        stores.capture_browse(&mut directory);
        let cache = ScopeCache::default();
        let old = cache.snapshot_with_directory(directory.view());

        let share =
            crate::plex::register_for_test("share-machine", "127.0.0.1", 2, "share", "scope");
        let next = cache.snapshot_with_directory(directory.view());

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

    /// Legacy `ui/search/field.rs`'s `the_memo_key_moves_when_a_source_goes_quiet_or_is_described`,
    /// on the publication the owned renderer memoises its scope sentence against
    /// (`render::Resources::prepare` rebuilds the line exactly when `same_publication` is false).
    ///
    /// The two inputs that move it are the ones no roster COUNT can see: a source being described
    /// (its name or the handle it is credited to) and a source going quiet. The counters that must
    /// NOT move for a pure description are asserted beside them, because a memo keyed on the roster
    /// alone would hold a sentence naming a machine by a name it no longer has.
    #[test]
    fn the_memo_key_moves_when_a_source_goes_quiet_or_is_described() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        let (stores, mut directory, _, share) = fixture();
        stores.browse.borrow_mut().seed_sources_for_test(2, true);
        stores.capture_browse(&mut directory);
        let cache = ScopeCache::default();
        let old = cache.snapshot_with_directory(directory.view());
        let before = read_key_with_directory(directory.view());
        assert!(old.sources().iter().all(|source| source.live));

        crate::plex::describe_server(share, "renamed-share", "new-friend", false);
        let described = cache.snapshot_with_directory(directory.view());
        let after = read_key_with_directory(directory.view());
        assert!(!old.same_publication(&described), "a described source is a new sentence");
        assert_eq!(described.sources()[1].handle, "new-friend");
        assert_eq!(
            (after.roster_gen, after.roster_len, after.roster, after.sections_gen, after.source_list_gen),
            (before.roster_gen, before.roster_len, before.roster, before.sections_gen, before.source_list_gen),
            "a pure description moves no roster, section or reachability counter"
        );
        assert_ne!(after.facts, before.facts, "…it moves the facts fingerprint, and that is enough");

        stores.browse.borrow_mut().seed_sources_for_test(2, false);
        stores.capture_browse(&mut directory);
        let quiet = cache.snapshot_with_directory(directory.view());
        assert!(!described.same_publication(&quiet), "a source going quiet is a new sentence");
        assert!(quiet.sources().iter().all(|source| !source.live));
        assert!(described.sources().iter().all(|source| source.live),
            "…and the retained publication keeps saying what it said");
        assert_ne!(read_key_with_directory(directory.view()).source_list_gen, after.source_list_gen);
    }

    #[test]
    fn equal_sized_roster_replacement_publishes_new_sources() {
        let _serial = crate::testlock::serial();
        let _reset = Reset;
        let (stores, mut directory, _, old_share) = fixture();
        let cache = ScopeCache::default();
        let old = cache.snapshot_with_directory(directory.view());

        crate::plex::reset_servers_for_test();
        let replacement =
            crate::plex::register_for_test("replacement", "127.0.0.1", 3, "replacement", "scope");
        let other = crate::plex::register_for_test("other", "127.0.0.1", 4, "other", "scope");
        assert_ne!(old_share, replacement);
        stores.browse_run(crate::stores::browse::BrowseCmd::Reset);
        stores.capture_browse(&mut directory);
        let next = cache.snapshot_with_directory(directory.view());

        assert_eq!(old.sources().len(), 2);
        assert_eq!(next.sources().len(), 2);
        assert_eq!(next.sources()[0].sid, replacement);
        assert_eq!(next.sources()[1].sid, other);
        assert!(!old.same_publication(&next));
    }
}
