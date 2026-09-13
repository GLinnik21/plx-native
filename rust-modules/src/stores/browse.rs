//! The Library table and its per-section listing, as a machine over `crate::browse`
//! (`docs/stores-as-machines.md`). The vocabulary is [`BrowseCmd`]; [`BrowseStore`] owns the
//! main-thread state, worker adapter and notice while `crate::browse` retains the implementation
//! and the compatibility read publication.

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Machine};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::{StoreEv, StoreId};
#[cfg(test)]
use super::note;

pub(crate) use crate::browse::section_hubs::{HubsSnapshot, HubsView};
pub(crate) use crate::browse::view::{
    DirectorySnapshot, DirectoryView, ListingSnapshot, ListingView,
};

pub(crate) fn hubs_snapshot() -> HubsSnapshot {
    with_active(|store| store.hubs_snapshot()).unwrap_or_else(|| {
        crate::browse::section_hubs::snapshot(crate::browse::cur())
    })
}

/// Retain the current listing for one dispatcher frame, without copying its items.
pub(crate) fn listing_snapshot() -> ListingSnapshot {
    with_active(|store| store.listing_snapshot()).unwrap_or_else(crate::browse::view::snapshot)
}

/// Every mutation of the browse store a screen may ask for.
#[derive(Clone, Debug)]
pub(crate) enum BrowseCmd {
    Discovery(crate::browse::record::Result),
    RetrySource {
        epoch: u32,
        sid: ServerId,
    },
    /// Execute deferred Library work against the source and table epoch captured by the screen.
    Addressed {
        target: SectionAddress,
        work: LibraryWork,
    },
    /// Point the listing at section `i` (a pill or library-row press, committed at the fade floor).
    #[cfg(test)]
    SetCur(usize),
    RecheckShares,
    /// The Home editor's draft commit: one record for the whole session.
    ApplyPins(Vec<(usize, bool)>),
    RetryDiscovery,
    /// The profile/account switch: wipe everything and supersede everything in flight.
    Reset,
    HubsInvalidateAll,
    /// The optimistic half of a view-state write, on the grid and the shelves.
    SetWatchedLocal {
        sid: ServerId,
        rk: String,
        on: bool,
    },
    LeftTheDeck {
        sid: ServerId,
        rk: String,
    },
}

/// The production owner of Browse's main-thread state, worker transport and notice generation.
pub(crate) struct BrowseStore {
    state: crate::browse::BrowseState,
    adapter: Arc<crate::browse::BrowseAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
    #[cfg(test)]
    compatibility_publications: std::cell::Cell<u32>,
}

thread_local! {
    static ACTIVE: RefCell<Weak<RefCell<BrowseStore>>> = RefCell::new(Weak::new());
}

static BOOTSTRAP_AVAILABLE: AtomicBool = AtomicBool::new(true);

#[cfg(test)]
pub(crate) fn reset_bootstrap_for_test() {
    BOOTSTRAP_AVAILABLE.store(true, Ordering::SeqCst);
}

impl Default for BrowseStore {
    fn default() -> Self {
        #[cfg(test)]
        let take_legacy = active_owner().is_none() && crate::testlock::held();
        #[cfg(not(test))]
        let take_legacy = take_bootstrap_token();
        Self::new(take_legacy)
    }
}

fn take_bootstrap_token() -> bool {
    // Process-lifetime handoff: dropping or switching an owner never replenishes this token.
    BOOTSTRAP_AVAILABLE.compare_exchange(
        true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

impl BrowseStore {
    fn new(take_legacy: bool) -> Self {
        let (state, adapter, (notice_gen, notice_dirty)) = if take_legacy {
            (crate::browse::take_legacy_state(), crate::browse::take_legacy_adapter(),
                super::take_browse_notice_seed())
        } else {
            (Default::default(), Arc::new(Default::default()), (0, false))
        };
        Self {
            state,
            adapter,
            notice_gen: AtomicU32::new(notice_gen),
            notice_dirty: AtomicBool::new(notice_dirty),
            #[cfg(test)]
            compatibility_publications: std::cell::Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(super) fn production_bootstrap_for_test() -> Self {
        Self::new(take_bootstrap_token())
    }
}

fn with_active<R>(f: impl FnOnce(&mut BrowseStore) -> R) -> Option<R> {
    let owner = active_owner()?;
    let mut store = owner.try_borrow_mut()
        .expect("reentrant Browse compatibility call");
    Some(f(&mut store))
}

fn active_owner() -> Option<Rc<RefCell<BrowseStore>>> {
    ACTIVE.with(|active| active.borrow().upgrade())
}

pub(crate) fn activate(store: &Rc<RefCell<BrowseStore>>) {
    let next = Rc::downgrade(store);
    if ACTIVE.with(|active| Weak::ptr_eq(&active.borrow(), &next)) {
        return;
    }
    // Publish before committing the selector. A failed borrow or clone leaves the previous
    // compatibility owner selected instead of exposing a half-activated aggregate.
    store.borrow().publish_compatibility();
    ACTIVE.with(|active| *active.borrow_mut() = next);
}

#[cfg(test)]
fn with_activation<R>(store: &Rc<RefCell<BrowseStore>>, f: impl FnOnce() -> R) -> R {
    struct Restore {
        owner: Weak<RefCell<BrowseStore>>,
        publication: crate::browse::BrowseState,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(owner) = self.owner.upgrade() {
                if let Ok(owner) = owner.try_borrow() {
                    owner.publish_compatibility();
                } else {
                    crate::browse::publish_legacy(&self.publication);
                }
            } else {
                crate::browse::publish_legacy(&self.publication);
            }
            ACTIVE.with(|active| *active.borrow_mut() = self.owner.clone());
        }
    }
    let previous = ACTIVE.with(|active| active.borrow().clone());
    let publication = previous.upgrade().and_then(|owner| {
        owner.try_borrow().ok().map(|owner| owner.state.clone())
    }).unwrap_or_else(crate::browse::clone_legacy_state);
    let _restore = Restore { owner: previous, publication };
    activate(store);
    f()
}

impl BrowseStore {
    fn publish_compatibility(&self) {
        #[cfg(test)]
        if !crate::testlock::held() {
            return;
        }
        #[cfg(test)]
        self.compatibility_publications
            .set(self.compatibility_publications.get().wrapping_add(1));
        crate::browse::publish_legacy(&self.state);
    }

    fn bump(&self) -> u32 {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub(crate) fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    pub(crate) fn run(&mut self, cmd: BrowseCmd) -> bool {
        let save_cursor = matches!(&cmd, BrowseCmd::Addressed {
            work: LibraryWork::SaveCursor { .. }, ..
        });
        let quiet = matches!(&cmd, BrowseCmd::Addressed {
            work: LibraryWork::Want { .. } | LibraryWork::Letters | LibraryWork::Genres, ..
        });
        let roster_changed = if matches!(&cmd, BrowseCmd::RecheckShares) {
            self.sync_roster()
        } else {
            false
        };
        if matches!(&cmd, BrowseCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let discovery = matches!(&cmd, BrowseCmd::Discovery(_));
        let changed = self.state.run_owned(&self.adapter, cmd) || roster_changed;
        if !quiet && !save_cursor && changed {
            self.publish_compatibility();
        }
        if (save_cursor && changed) || (!quiet && !save_cursor && (!discovery || changed)) {
            self.bump();
        }
        changed
    }

    pub(crate) fn pump(&mut self) -> super::StoreOutcome {
        if !self.state.pump_needs_work(&self.adapter) {
            return Default::default();
        }
        let roster_changed = self.sync_roster();
        let source_gen = self.state.source_list_gen();
        let mut outcome = self.state.pump_owned(&self.adapter);
        outcome.changed |= roster_changed || source_gen != self.state.source_list_gen();
        if outcome.changed {
            self.publish_compatibility();
        }
        if outcome.changed {
            self.bump();
        }
        outcome
    }

    pub(crate) fn discover_pump(&mut self) -> super::StoreOutcome {
        if !self.state.discovery_needs_pump(&self.adapter) {
            return Default::default();
        }
        let roster_changed = self.sync_roster();
        let source_gen = self.state.source_list_gen();
        let mut outcome = self.state.discover_pump_owned(&self.adapter);
        outcome.changed |= roster_changed || source_gen != self.state.source_list_gen();
        if outcome.changed {
            self.publish_compatibility();
            self.bump();
        }
        outcome
    }

    pub(crate) fn listing_snapshot(&mut self) -> ListingSnapshot {
        self.state.listing_snapshot()
    }

    pub(crate) fn capture_directory(&mut self, snapshot: &mut DirectorySnapshot) {
        snapshot.capture_from(&mut self.state);
    }

    pub(crate) fn hubs_snapshot(&mut self) -> HubsSnapshot {
        self.state.hubs_snapshot(self.state.cur())
    }

    pub(crate) fn take_discovery(&mut self) -> Option<crate::browse::record::Result> {
        crate::browse::record::take_from(&self.adapter)
    }

    fn sync_roster(&mut self) -> bool {
        let sync = self.state.sync_roster_owned();
        if sync.retire_adapter {
            self.adapter = Arc::new(Default::default());
        }
        sync.changed
    }

    fn controlled_discover(
        &mut self,
        launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
    ) {
        let changed = self.sync_roster();
        self.state.controlled_discover_owned(&self.adapter, launch);
        if changed {
            self.publish_compatibility();
            self.bump();
        }
    }

    pub(crate) fn apply_discovery(
        &mut self,
        result: &crate::browse::record::Result,
        preferences: &crate::plex::session::Session,
    ) -> super::StoreOutcome {
        let outcome = crate::browse::record::apply_to(
            &mut self.state, &self.adapter, result, Some(preferences));
        if outcome.changed {
            self.publish_compatibility();
            self.bump();
        }
        outcome
    }

    #[cfg(test)]
    pub(crate) fn spawn_page_for_test(
        &mut self,
        client: &'static crate::plex::Client,
        title: &str,
    ) -> (std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>) {
        crate::browse::spawn_owned_page_for_test(&self.state, &self.adapter, client, title)
    }

    #[cfg(test)]
    fn seed_items_for_test(&mut self, count: usize) {
        crate::browse::seed_items_for_owner_test(&mut self.state, count);
        self.publish_compatibility();
    }

    #[cfg(test)]
    fn queue_discovery_for_test(
        &mut self,
        client: &'static crate::plex::Client,
        token_gen: u32,
        ok: bool,
    ) {
        crate::browse::queue_discovery_for_owner_test(
            &mut self.state, &self.adapter, client, token_gen, ok);
    }

    #[cfg(test)]
    pub(crate) fn prepare_page_for_test(&mut self, sid: ServerId) {
        crate::browse::prepare_page_for_owner_test(&mut self.state, sid);
    }

    #[cfg(test)]
    pub(crate) fn has_page_result_for_test(&self) -> bool {
        crate::browse::adapter_has_page_for_test(&self.adapter)
    }

    #[cfg(test)]
    fn queue_genre_for_test(&mut self, client: &'static crate::plex::Client) {
        crate::browse::queue_genre_for_owner_test(&mut self.state, &self.adapter, client);
    }

    #[cfg(test)]
    fn queue_page_failure_for_test(&mut self, client: &'static crate::plex::Client) {
        crate::browse::queue_page_failure_for_owner_test(&mut self.state, &self.adapter, client);
    }

    #[cfg(test)]
    fn source_list_gen_for_test(&self) -> u32 {
        self.state.source_list_gen()
    }
}

pub(crate) fn controlled_discover_active(
    launch: &mut dyn FnMut(crate::browse::DiscoveryRequest) -> bool,
) -> bool {
    with_active(|store| store.controlled_discover(launch)).is_some()
}

#[cfg(test)]
pub(crate) fn seed_items_active_for_test(count: usize) -> bool {
    with_active(|store| store.seed_items_for_test(count)).is_some()
}

#[cfg(test)]
pub(crate) fn queue_discovery_active_for_test(
    client: &'static crate::plex::Client,
    token_gen: u32,
    ok: bool,
) -> bool {
    with_active(|store| store.queue_discovery_for_test(client, token_gen, ok)).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SectionAddress {
    pub epoch: u32,
    pub sid: ServerId,
    pub section: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryEdit {
    Sort { key: String, desc: bool },
    Unwatched(bool),
    Genre(Option<String>),
}

#[derive(Clone, Debug)]
pub(crate) enum LibraryWork {
    SaveCursor {
        query: u32,
        cursor: crate::browse::Cursor,
    },
    /// Selection and query coexist and commit in this order, inside one store delivery.
    Commit {
        select: bool,
        choice: bool,
        query: Option<QueryEdit>,
    },
    Want {
        lo: usize,
        hi: usize,
    },
    Letters,
    Genres,
    Hubs {
        may_publish: bool,
    },
    Retry,
}

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: BrowseCmd) -> bool {
    super::apply(super::StoreCmd::Browse(cmd)).changed
}

/// The store's own step, reached only through [`super::apply`]. D3 moved the match itself
/// (`addressed` included) into `browse::run` — it called `pub(crate)` mutators across this
/// module boundary; those are private to `browse/mod.rs` now and this is their only door. The
/// bump-vs-note bookkeeping this function used to do inline moved with it (see `browse::run`'s
/// own doc for why that needed no help from `note`, which is private to this module).
pub(super) fn run(cmd: BrowseCmd) -> bool {
    // `crate::browse`'s temporary holder is reached from both `apply` above and
    // `crate::stores::apply(StoreCmd::Browse(..))` directly (some fixtures deliver a `StoreCmd`
    // without going through this module's `apply`) — guard the one point both funnel through. See
    // `lib.rs::testlock` and D5.
    #[cfg(test)]
    crate::testlock::assert_held("the browse store (apply)");
    let save_cursor = matches!(&cmd, BrowseCmd::Addressed {
        work: LibraryWork::SaveCursor { .. }, ..
    });
    with_active(|store| store.run(cmd.clone())).unwrap_or_else(|| {
        let changed = crate::browse::run(cmd);
        if !save_cursor || changed {
            super::bump(StoreId::Browse);
        }
        changed
    })
}

/// The landing pass the Library screen runs once a frame while it is up: pages, menu data, the
/// roster, the shelves. Answers `true` when the store changed.
#[cfg(test)]
pub(crate) fn pump() -> super::StoreOutcome {
    with_active(BrowseStore::pump).unwrap_or_else(|| {
        let outcome = crate::browse::pump();
        note(StoreId::Browse, outcome.changed);
        outcome
    })
}

/// The roster half alone — what Home and Search run to learn about a friend's libraries without
/// fetching any page.
pub(crate) fn discover_pump() -> super::EndpointRefreshSet {
    with_active(BrowseStore::discover_pump)
        .map(|outcome| outcome.endpoints).unwrap_or_else(crate::browse::discover_pump)
}

impl<H: super::StoreEffectHost> Machine<H> for BrowseStore {
    type Ev = StoreEv<BrowseCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                self.run(c.clone());
            }
            StoreEv::Pump { .. } => {
                self.pump().endpoints.emit(fx);
            }
        }
        Handled::Yes
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn bootstrap_adoption_is_permanently_one_shot_even_after_the_owner_drops() {
        let _guard = crate::testlock::serial();
        reset_bootstrap_for_test();
        apply(BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(1);

        let first = crate::stores::Stores::production_bootstrap_for_test();
        assert!(first.browse.borrow_mut().listing_snapshot().view().item(0).is_some());
        drop(first);

        let second = crate::stores::Stores::production_bootstrap_for_test();
        assert!(second.browse.borrow_mut().listing_snapshot().view().item(0).is_none(),
            "dropping the bootstrap owner must not mint another adoption token");
        drop(second);
        apply(BrowseCmd::Reset);
    }

    #[test]
    fn owned_notices_never_merge_the_retired_compatibility_generation() {
        let _guard = crate::testlock::serial();
        apply(BrowseCmd::Reset);
        let stores = crate::stores::Stores::default();
        let _ = stores.take_notices();

        super::super::bump(StoreId::Browse);
        assert!(stores.take_notices().iter().all(|(id, _)| *id != StoreId::Browse),
            "an owned aggregate must not import a later compatibility generation");

        drop(stores);
        apply(BrowseCmd::Reset);
    }

    #[test]
    fn snapshots_and_an_idle_pump_publish_no_compatibility_clone() {
        let _guard = crate::testlock::serial();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let stores = crate::stores::Stores::default();
        let before = stores.browse.borrow().compatibility_publications.get();
        let mut directory = DirectorySnapshot::default();
        {
            let mut browse = stores.browse.borrow_mut();
            let _ = browse.listing_snapshot();
            browse.capture_directory(&mut directory);
            let _ = browse.hubs_snapshot();
            assert_eq!(browse.pump(), super::super::StoreOutcome::default());
            assert_eq!(browse.discover_pump(), super::super::StoreOutcome::default());
        }
        assert_eq!(stores.browse.borrow().compatibility_publications.get(), before);
        drop(stores);
        apply(BrowseCmd::Reset);
    }

    #[test]
    fn compatibility_owner_is_drop_safe_and_reentrancy_refuses_without_corruption() {
        let _guard = crate::testlock::serial();
        reset_bootstrap_for_test();
        apply(BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();
        let first = crate::stores::Stores::default();
        let second = crate::stores::Stores::default();
        activate(&first.browse);
        assert_eq!(crate::browse::section_count(), 4);
        with_activation(&second.browse, || {
            assert_eq!(crate::browse::section_count(), 0,
                "the scoped owner must publish before the callback reads compatibility state");
        });
        assert_eq!(crate::browse::section_count(), 4,
            "normal scoped exit must restore the prior compatibility publication");
        let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_activation(&second.browse, || {
                assert_eq!(crate::browse::section_count(), 0);
                panic!("nested owner unwind");
            });
        }));
        assert!(nested.is_err());
        assert_eq!(crate::browse::section_count(), 4,
            "panic exit must restore the prior compatibility publication immediately");
        let first_gen = first.browse.borrow().gen();
        let held = first.browse.borrow_mut();
        let reentrant = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply(BrowseCmd::Reset);
        }));
        assert!(reentrant.is_err());
        drop(held);
        assert_eq!(first.browse.borrow().gen(), first_gen,
            "a refused same-owner reentry cannot partially mutate the owner");

        let held_second = second.browse.borrow_mut();
        let refused_switch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            activate(&second.browse);
        }));
        assert!(refused_switch.is_err());
        drop(held_second);
        apply(BrowseCmd::Reset);
        assert_eq!(first.browse.borrow().gen(), first_gen + 1,
            "a failed publication must leave the prior owner active");
        assert_eq!(second.browse.borrow().gen(), 0);

        let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_activation(&second.browse, || panic!("nested owner unwind after reset"));
        }));
        assert!(nested.is_err());
        apply(BrowseCmd::Reset);
        assert_eq!(first.browse.borrow().gen(), first_gen + 2,
            "the prior owner is restored when a nested owner unwinds");
        assert_eq!(second.browse.borrow().gen(), 0);

        drop(first);
        apply(BrowseCmd::Reset);
        assert!(crate::stores::take_notices().iter().any(|(id, _)| *id == StoreId::Browse),
            "a dead Weak falls back to the compatibility store instead of dangling");
        drop(second);
    }

    #[test]
    fn first_owner_atomically_adopts_prebridge_state_adapter_and_notice() {
        let _guard = crate::testlock::serial();
        reset_bootstrap_for_test();
        let session = crate::plex::session::TempSession::new("browse-preowner");
        session.watching("u-browse-preowner");
        crate::plex::reset_servers_for_test();
        apply(BrowseCmd::Reset);
        let sid = crate::plex::register_for_test(
            "browse-preowner", "127.0.0.1", 9, "synthetic", "fixture");
        let client = crate::plex::client_for(sid).unwrap();
        crate::browse::queue_discovery_for_test(client, client.token_gen(), false);
        let generation = crate::stores::gen(StoreId::Browse);

        let stores = crate::stores::Stores::default();
        assert_eq!(stores.browse.borrow().gen(), generation);
        let browse_notices: Vec<_> = stores.take_notices().into_iter()
            .filter(|(id, _)| *id == StoreId::Browse).collect();
        assert_eq!(browse_notices, [(StoreId::Browse, generation)],
            "the pre-owner dirty notice moves once rather than being merged twice");
        assert!(crate::stores::take_notices().iter().all(|(id, _)| *id != StoreId::Browse));
        let before_landing = stores.browse.borrow().gen();
        let endpoints = stores.browse.borrow_mut().discover_pump();
        assert_eq!(endpoints.endpoints.iter().map(|request| request.sid).collect::<Vec<_>>(), [sid]);
        assert_eq!(stores.browse.borrow().gen(), before_landing + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_landing + 1)]);
        assert!(stores.take_notices().is_empty(), "one discovery landing owes one notice");
        assert_eq!(stores.browse.borrow_mut().discover_pump().endpoints.iter().count(), 0,
            "the transferred adapter result is consumed exactly once");
        drop(stores);
        crate::stores::browse::apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn controlled_discovery_apply_and_directory_landing_each_bump_exactly_once() {
        let _guard = crate::testlock::serial();
        reset_bootstrap_for_test();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-owned-landings", "127.0.0.1", 9, "synthetic", "fixture");
        crate::browse::seed_registered_table_for_test([sid, sid]);
        let stores = crate::stores::Stores::default();
        let _ = stores.take_notices();
        let client = crate::plex::client_for(sid).unwrap();

        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), true);
        let result = stores.browse.borrow_mut().take_discovery().unwrap();
        let before_discovery = stores.browse.borrow().gen();
        let _ = stores.browse.borrow_mut().apply_discovery(
            &result, &crate::plex::session::Session::default());
        assert_eq!(stores.browse.borrow().gen(), before_discovery + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_discovery + 1)]);
        assert!(stores.take_notices().is_empty());

        stores.browse.borrow_mut().queue_genre_for_test(client);
        let before_directory = stores.browse.borrow().gen();
        assert!(stores.browse.borrow_mut().pump().changed,
            "a current directory landing is an observable Browse change");
        assert_eq!(stores.browse.borrow().gen(), before_directory + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_directory + 1)]);
        assert!(stores.take_notices().is_empty());

        drop(stores);
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn reset_rotates_the_adapter_away_from_a_late_old_worker() {
        let _guard = crate::testlock::serial();
        reset_bootstrap_for_test();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-reset-worker", "127.0.0.1", 9, "synthetic", "fixture");
        crate::browse::seed_registered_table_for_test([sid, sid]);
        let stores = crate::stores::Stores::default();
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let old = Arc::clone(&stores.browse.borrow().adapter);
        let (release, landed) = stores.browse.borrow_mut()
            .spawn_page_for_test(client, "retired-result");

        stores.browse.borrow_mut().run(BrowseCmd::Reset);
        let new = Arc::clone(&stores.browse.borrow().adapter);
        assert!(!Arc::ptr_eq(&old, &new), "reset must rotate worker transport identity");
        crate::browse::set_adapter_fetching_for_test(&new, true);
        release.send(()).unwrap();
        landed.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(crate::browse::adapter_has_page_for_test(&old));
        assert!(!crate::browse::adapter_has_page_for_test(&new),
            "a retired worker must not overwrite the replacement mailbox");
        assert!(crate::browse::adapter_fetching_for_test(&new),
            "a retired worker must not clear the replacement flight");

        drop(stores);
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn roster_removal_rotates_transport_away_from_a_held_old_worker() {
        let _guard = crate::testlock::serial();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-roster-retire", "127.0.0.1", 9, "synthetic", "fixture");
        crate::browse::seed_registered_table_for_test([sid, sid]);
        let stores = crate::stores::Stores::default();
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let old = Arc::clone(&stores.browse.borrow().adapter);
        let (release, landed) = stores.browse.borrow_mut()
            .spawn_page_for_test(client, "retired-by-roster");

        crate::plex::reset_servers_for_test();
        assert!(stores.browse.borrow_mut().pump().changed);
        let new = Arc::clone(&stores.browse.borrow().adapter);
        assert!(!Arc::ptr_eq(&old, &new),
            "source removal must retire the adapter before any replacement work is admitted");
        crate::browse::set_adapter_fetching_for_test(&new, true);
        release.send(()).unwrap();
        landed.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(crate::browse::adapter_has_page_for_test(&old));
        assert!(!crate::browse::adapter_has_page_for_test(&new));
        assert!(crate::browse::adapter_fetching_for_test(&new));

        drop(stores);
        apply(BrowseCmd::Reset);
    }

    #[test]
    fn extracted_discovery_result_cannot_clear_the_post_reset_source_flight() {
        let _guard = crate::testlock::serial();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-result-retire", "127.0.0.1", 9, "synthetic", "fixture");
        crate::browse::seed_registered_table_for_test([sid, sid]);
        let stores = crate::stores::Stores::default();
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().queue_discovery_for_test(
            client, client.token_gen(), true);
        let result = stores.browse.borrow_mut().take_discovery().unwrap();

        stores.browse.borrow_mut().run(BrowseCmd::Reset);
        let replacement = Arc::clone(&stores.browse.borrow().adapter);
        crate::browse::set_adapter_src_fetching_for_test(&replacement, true);
        let _ = stores.take_notices();
        let before = stores.browse.borrow().gen();
        let outcome = stores.browse.borrow_mut().apply_discovery(
            &result, &crate::plex::session::Session::default());
        assert!(!outcome.changed);
        assert_eq!(stores.browse.borrow().gen(), before);
        assert!(stores.take_notices().is_empty());
        assert!(crate::browse::adapter_src_fetching_for_test(&replacement),
            "a result extracted from the retired adapter must not release the new source flight");

        drop(stores);
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn controlled_roster_addition_is_one_published_change_even_when_spawn_is_refused() {
        let _guard = crate::testlock::serial();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let stores = crate::stores::Stores::default();
        let _ = stores.take_notices();
        let before_notice = stores.browse.borrow().gen();
        let before_sources = stores.browse.borrow().source_list_gen_for_test();
        crate::plex::register_for_test(
            "", "127.0.0.1", 9, "synthetic", "fixture");
        let mut launches = 0;
        assert!(controlled_discover_active(&mut |_| {
            launches += 1;
            false
        }));
        assert_eq!(launches, 1);
        let owned_sources = stores.browse.borrow().source_list_gen_for_test();
        assert_ne!(owned_sources, before_sources);
        assert_eq!(crate::browse::source_list_gen(), owned_sources,
            "the compatibility publication must immediately expose the owned roster");
        assert_eq!(stores.browse.borrow().gen(), before_notice + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before_notice + 1)]);
        assert!(stores.take_notices().is_empty());
        let _ = crate::ui::idle::take_local_damage();

        let settled_notice = stores.browse.borrow().gen();
        let settled_sources = stores.browse.borrow().source_list_gen_for_test();
        let settled_publications = stores.browse.borrow().compatibility_publications.get();
        let mut second_launches = 0;
        assert!(controlled_discover_active(&mut |_| {
            second_launches += 1;
            false
        }));
        assert_eq!(second_launches, 0, "the refused source is still in retry backoff");
        assert_eq!(stores.browse.borrow().gen(), settled_notice);
        assert_eq!(stores.browse.borrow().source_list_gen_for_test(), settled_sources);
        assert_eq!(crate::browse::source_list_gen(), settled_sources);
        assert_eq!(stores.browse.borrow().compatibility_publications.get(),
            settled_publications);
        assert!(stores.take_notices().is_empty());
        assert_eq!(crate::ui::idle::take_local_damage(), 0,
            "an empty-to-empty machine identity must not invalidate the settled frame");

        drop(stores);
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn isolated_current_page_failure_is_one_observable_change_and_notice() {
        let _guard = crate::testlock::serial();
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test(
            "browse-page-failure", "127.0.0.1", 9, "synthetic", "fixture");
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);
        crate::browse::seed_registered_table_for_test([sid, sid]);
        let stores = crate::stores::Stores::default();
        let client = crate::plex::client_for(sid).unwrap();
        stores.browse.borrow_mut().prepare_page_for_test(sid);
        let _ = stores.browse.borrow_mut().discover_pump();
        let _ = stores.take_notices();
        stores.browse.borrow_mut().queue_page_failure_for_test(client);
        let before = stores.browse.borrow().gen();

        let outcome = stores.browse.borrow_mut().pump();
        assert!(outcome.changed, "Loading to Failed is an observable listing publication");
        assert_eq!(stores.browse.borrow_mut().listing_snapshot().view().fetch(),
            crate::browse::SecFetch::Failed);
        assert_eq!(stores.browse.borrow().gen(), before + 1);
        assert_eq!(stores.take_notices(), [(StoreId::Browse, before + 1)]);
        assert!(stores.take_notices().is_empty());

        drop(stores);
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn library_switch_events_count_only_new_committed_choices() {
        let _guard = crate::testlock::serial();
        let session = crate::plex::session::TempSession::new("library-switch-events");
        session.watching("u-library-switch-events");
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                apply(BrowseCmd::Reset);
                crate::plex::reset_servers_for_test();
            }
        }
        let _cleanup = Cleanup;
        apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let own =
            crate::plex::register_for_test("switch-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = crate::plex::register_for_test(
            "switch-shared",
            "127.0.0.1",
            10,
            "synthetic",
            "fixture",
        );
        crate::browse::seed_registered_table_for_test([own, shared]);
        apply(BrowseCmd::SetCur(0));
        let a = SectionAddress {
            epoch: crate::browse::table_epoch(),
            sid: own,
            section: 1,
        };
        let b = SectionAddress { sid: shared, ..a };
        assert_eq!(
            crate::browse::resolve_section(b.epoch, b.sid, b.section),
            Some(2)
        );
        let commit = |target, select, choice| {
            apply(BrowseCmd::Addressed {
                target,
                work: LibraryWork::Commit {
                    select,
                    choice,
                    query: None,
                },
            })
        };
        let observe = |target, select, choice| {
            crate::diag::test_events::capture(|| commit(target, select, choice))
        };
        let switched = crate::diag::schema::DiagEvent::FeatureUsed {
            feature: crate::diag::schema::Feature::LibrarySwitch,
        };

        // Same-current includes the final A commit after a pending A→B→A was superseded.
        assert_eq!(observe(a, true, true), (true, vec![]));
        assert_eq!(
            observe(SectionAddress { section: 999, ..b }, true, true),
            (false, vec![])
        );
        assert_eq!(
            observe(
                SectionAddress {
                    epoch: b.epoch.wrapping_add(1),
                    ..b
                },
                true,
                true
            ),
            (false, vec![])
        );
        assert_eq!(
            observe(b, false, true),
            (false, vec![]),
            "foreign work without selection cannot count"
        );
        assert_eq!(crate::browse::cur(), 0);
        assert_eq!(
            observe(b, true, false),
            (true, vec![]),
            "boot/repoint is not a viewer choice"
        );
        assert_eq!(crate::browse::cur(), 2);
        assert_eq!(
            observe(b, true, true),
            (true, vec![]),
            "choosing the current library is quiet"
        );
        assert_eq!(observe(a, true, true), (true, vec![switched]));
        assert_eq!(crate::browse::cur(), 0);
        assert_eq!(
            observe(a, true, true),
            (true, vec![]),
            "repeated delivery cannot count twice"
        );
        assert_eq!(
            observe(b, true, true),
            (true, vec![switched]),
            "a later real switch counts once"
        );
        let (accepted, events) = crate::diag::test_events::capture(|| {
            apply(BrowseCmd::Addressed {
                target: b,
                work: LibraryWork::Commit {
                    select: false,
                    choice: false,
                    query: Some(QueryEdit::Unwatched(true)),
                },
            })
        });
        assert!(accepted);
        assert!(events.is_empty(), "query changes are not library switches");
    }
}
